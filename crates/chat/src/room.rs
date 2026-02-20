use std::cmp::Ordering;
use std::hash::{Hash, Hasher};
use std::time::Duration;

use anyhow::{Context as AnyhowContext, Error};
use common::EventUtils;
use gpui::{App, AppContext, Context, EventEmitter, SharedString, Task};
use itertools::Itertools;
use nostr_sdk::prelude::*;
use person::{Person, PersonRegistry};
use settings::{RoomConfig, SignerKind};
use state::{NostrRegistry, TIMEOUT};

use crate::{ChatRegistry, NewMessage};

#[derive(Debug, Clone)]
pub struct SendReport {
    pub receiver: PublicKey,
    pub gift_wrap_id: Option<EventId>,
    pub error: Option<SharedString>,
    pub output: Option<Output<EventId>>,
}

impl SendReport {
    pub fn new(receiver: PublicKey) -> Self {
        Self {
            receiver,
            gift_wrap_id: None,
            error: None,
            output: None,
        }
    }

    /// Set the gift wrap ID.
    pub fn gift_wrap_id(mut self, gift_wrap_id: EventId) -> Self {
        self.gift_wrap_id = Some(gift_wrap_id);
        self
    }

    /// Set the output.
    pub fn output(mut self, output: Output<EventId>) -> Self {
        self.output = Some(output);
        self
    }

    /// Set the error message.
    pub fn error<T>(mut self, error: T) -> Self
    where
        T: Into<SharedString>,
    {
        self.error = Some(error.into());
        self
    }

    /// Returns true if the send is pending.
    pub fn pending(&self) -> bool {
        self.output.is_none() && self.error.is_none()
    }

    /// Returns true if the send was successful.
    pub fn success(&self) -> bool {
        if let Some(output) = self.output.as_ref() {
            !output.failed.is_empty()
        } else {
            false
        }
    }
}

/// Room event.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum RoomEvent {
    /// Incoming message.
    Incoming(NewMessage),
    /// Reloads the current room's messages.
    Reload,
}

/// Room kind.
#[derive(Clone, Copy, Hash, Debug, PartialEq, Eq, PartialOrd, Ord, Default)]
pub enum RoomKind {
    #[default]
    Request,
    Ongoing,
}

#[derive(Debug, Clone)]
pub struct Room {
    /// Conversation ID
    pub id: u64,

    /// The timestamp of the last message in the room
    pub created_at: Timestamp,

    /// Subject of the room
    pub subject: Option<SharedString>,

    /// All members of the room
    pub(super) members: Vec<PublicKey>,

    /// Kind
    pub kind: RoomKind,

    /// Configuration
    config: RoomConfig,
}

impl Ord for Room {
    fn cmp(&self, other: &Self) -> Ordering {
        self.created_at.cmp(&other.created_at)
    }
}

impl PartialOrd for Room {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl PartialEq for Room {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}

impl Hash for Room {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.id.hash(state);
    }
}

impl Eq for Room {}

impl EventEmitter<RoomEvent> for Room {}

impl From<&UnsignedEvent> for Room {
    fn from(val: &UnsignedEvent) -> Self {
        let id = val.uniq_id();
        let created_at = val.created_at;
        let members = val.extract_public_keys();
        let subject = val
            .tags
            .find(TagKind::Subject)
            .and_then(|tag| tag.content().map(|s| s.to_owned().into()));

        Room {
            id,
            created_at,
            subject,
            members,
            kind: RoomKind::default(),
            config: RoomConfig::default(),
        }
    }
}

impl From<UnsignedEvent> for Room {
    fn from(val: UnsignedEvent) -> Self {
        Room::from(&val)
    }
}

impl Room {
    /// Constructs a new room with the given receiver and tags.
    pub fn new<T>(author: PublicKey, receivers: T) -> Self
    where
        T: IntoIterator<Item = PublicKey>,
    {
        // Map receiver public keys to tags
        let tags = Tags::from_list(receivers.into_iter().map(Tag::public_key).collect());

        // Construct an unsigned event for a direct message
        //
        // WARNING: never sign this event
        let mut event = EventBuilder::new(Kind::PrivateDirectMessage, "")
            .tags(tags)
            .build(author);

        // Ensure that the ID is set
        event.ensure_id();

        Room::from(&event)
    }

    /// Organizes the members of the room by moving the target member to the end.
    ///
    /// Always call this function to ensure the current user is at the end of the list.
    pub fn organize(mut self, target: &PublicKey) -> Self {
        if let Some(index) = self.members.iter().position(|member| member == target) {
            let member = self.members.remove(index);
            self.members.push(member);
        }
        self
    }

    /// Sets the kind of the room and returns the modified room
    pub fn kind(mut self, kind: RoomKind) -> Self {
        self.kind = kind;
        self
    }

    /// Sets this room is ongoing conversation
    pub fn set_ongoing(&mut self, cx: &mut Context<Self>) {
        if self.kind != RoomKind::Ongoing {
            self.kind = RoomKind::Ongoing;
            cx.notify();
        }
    }

    /// Updates the creation timestamp of the room
    pub fn set_created_at(&mut self, created_at: impl Into<Timestamp>, cx: &mut Context<Self>) {
        self.created_at = created_at.into();
        cx.notify();
    }

    /// Updates the subject of the room
    pub fn set_subject<T>(&mut self, subject: T, cx: &mut Context<Self>)
    where
        T: Into<SharedString>,
    {
        self.subject = Some(subject.into());
        cx.notify();
    }

    /// Returns the members of the room
    pub fn members(&self) -> Vec<PublicKey> {
        self.members.clone()
    }

    /// Checks if the room has more than two members (group)
    pub fn is_group(&self) -> bool {
        self.members.len() > 2
    }

    /// Gets the display name for the room
    pub fn display_name(&self, cx: &App) -> SharedString {
        if let Some(value) = self.subject.clone() {
            value
        } else {
            self.merged_name(cx)
        }
    }

    /// Gets the display image for the room
    pub fn display_image(&self, cx: &App) -> SharedString {
        if !self.is_group() {
            self.display_member(cx).avatar()
        } else {
            SharedString::from("brand/group.png")
        }
    }

    /// Get a member to represent the room
    ///
    /// Display member is always different from the current user.
    pub fn display_member(&self, cx: &App) -> Person {
        let persons = PersonRegistry::global(cx);
        persons.read(cx).get(&self.members[0], cx)
    }

    /// Merge the names of the first two members of the room.
    fn merged_name(&self, cx: &App) -> SharedString {
        let persons = PersonRegistry::global(cx);

        if self.is_group() {
            let profiles: Vec<Person> = self
                .members
                .iter()
                .map(|public_key| persons.read(cx).get(public_key, cx))
                .collect();

            let mut name = profiles
                .iter()
                .take(2)
                .map(|p| p.name())
                .collect::<Vec<_>>()
                .join(", ");

            if profiles.len() > 3 {
                name = format!("{}, +{}", name, profiles.len() - 2);
            }

            SharedString::from(name)
        } else {
            self.display_member(cx).name()
        }
    }

    /// Push a new message to the current room
    pub fn push_message(&mut self, message: NewMessage, cx: &mut Context<Self>) {
        let created_at = message.rumor.created_at;
        let new_message = created_at > self.created_at;

        // Emit the incoming message event
        cx.emit(RoomEvent::Incoming(message));

        if new_message {
            self.set_created_at(created_at, cx);
            // Sort chats after emitting a new message
            ChatRegistry::global(cx).update(cx, |this, cx| {
                this.sort(cx);
            });
        }
    }

    /// Emits a signal to reload the current room's messages.
    pub fn emit_refresh(&mut self, cx: &mut Context<Self>) {
        cx.emit(RoomEvent::Reload);
    }

    /// Get gossip relays for each member
    pub fn early_connect(&self, cx: &App) -> Task<Result<(), Error>> {
        let nostr = NostrRegistry::global(cx);
        let client = nostr.read(cx).client();

        let members = self.members();
        let subscription_id = SubscriptionId::new(format!("room-{}", self.id));

        cx.background_spawn(async move {
            let signer = client.signer().context("Signer not found")?;
            let public_key = signer.get_public_key().await?;

            for member in members.into_iter() {
                if member == public_key {
                    continue;
                };

                // Construct a filter for messaging relays
                let inbox = Filter::new()
                    .kind(Kind::InboxRelays)
                    .author(member)
                    .limit(1);

                // Construct a filter for announcement
                let announcement = Filter::new()
                    .kind(Kind::Custom(10044))
                    .author(member)
                    .limit(1);

                // Subscribe to get member's gossip relays
                client
                    .subscribe(vec![inbox, announcement])
                    .with_id(subscription_id.clone())
                    .close_on(
                        SubscribeAutoCloseOptions::default()
                            .timeout(Some(Duration::from_secs(TIMEOUT)))
                            .exit_policy(ReqExitPolicy::ExitOnEOSE),
                    )
                    .await?;
            }

            Ok(())
        })
    }

    /// Get all messages belonging to the room
    pub fn get_messages(&self, cx: &App) -> Task<Result<Vec<UnsignedEvent>, Error>> {
        let nostr = NostrRegistry::global(cx);
        let client = nostr.read(cx).client();
        let conversation_id = self.id.to_string();

        cx.background_spawn(async move {
            let filter = Filter::new()
                .kind(Kind::ApplicationSpecificData)
                .custom_tag(SingleLetterTag::lowercase(Alphabet::C), conversation_id);

            let messages = client
                .database()
                .query(filter)
                .await?
                .into_iter()
                .filter_map(|event| UnsignedEvent::from_json(&event.content).ok())
                .sorted_by_key(|message| message.created_at)
                .collect();

            Ok(messages)
        })
    }

    // Construct a rumor event for direct message
    pub fn rumor<S, I>(&self, content: S, replies: I, cx: &App) -> Option<UnsignedEvent>
    where
        S: Into<String>,
        I: IntoIterator<Item = EventId>,
    {
        let kind = Kind::PrivateDirectMessage;
        let content: String = content.into();
        let replies: Vec<EventId> = replies.into_iter().collect();

        let persons = PersonRegistry::global(cx);
        let nostr = NostrRegistry::global(cx);

        // Get current user's public key
        let sender = nostr.read(cx).signer().public_key()?;

        // Get all members
        let members: Vec<Person> = self
            .members
            .iter()
            .filter(|public_key| public_key != &&sender)
            .map(|member| persons.read(cx).get(member, cx))
            .collect();

        // Construct event's tags
        let mut tags = vec![];

        // Add subject tag if present
        if let Some(value) = self.subject.as_ref() {
            tags.push(Tag::from_standardized_without_cell(TagStandard::Subject(
                value.to_string(),
            )));
        }

        // Add all reply tags
        for id in replies.into_iter() {
            tags.push(Tag::event(id))
        }

        // Add all receiver tags
        for member in members.into_iter() {
            // Skip current user
            if member.public_key() == sender {
                continue;
            }

            tags.push(Tag::from_standardized_without_cell(
                TagStandard::PublicKey {
                    public_key: member.public_key(),
                    relay_url: member.messaging_relay_hint(),
                    alias: None,
                    uppercase: false,
                },
            ));
        }

        // Construct a direct message rumor event
        // WARNING: never sign and send this event to relays
        let mut event = EventBuilder::new(kind, content).tags(tags).build(sender);

        // Ensure that the ID is set
        event.ensure_id();

        Some(event)
    }

    /// Send rumor event to all members's messaging relays
    pub fn send(&self, rumor: UnsignedEvent, cx: &App) -> Option<Task<Vec<SendReport>>> {
        let persons = PersonRegistry::global(cx);
        let nostr = NostrRegistry::global(cx);
        let client = nostr.read(cx).client();
        let signer = nostr.read(cx).signer();

        // Get room's config
        let config = self.config.clone();

        // Get current user's public key
        let sender = nostr.read(cx).signer().public_key()?;

        // Get all members (excluding sender)
        let members: Vec<Person> = self
            .members
            .iter()
            .filter(|public_key| public_key != &&sender)
            .map(|member| persons.read(cx).get(member, cx))
            .collect();

        Some(cx.background_spawn(async move {
            let signer_kind = config.signer_kind();
            let user_signer = signer.get().await;
            let encryption_signer = signer.get_encryption_signer().await;

            let mut reports = Vec::new();

            for member in members {
                let relays = member.messaging_relays();
                let announcement = member.announcement();

                // Skip if member has no messaging relays
                if relays.is_empty() {
                    reports.push(SendReport::new(member.public_key()).error("No messaging relays"));
                    continue;
                }

                // Ensure relay connections
                for url in relays.iter() {
                    client
                        .add_relay(url)
                        .and_connect()
                        .capabilities(RelayCapabilities::GOSSIP)
                        .await
                        .ok();
                }

                // When forced to use encryption signer, skip if receiver has no announcement
                if signer_kind.encryption() && announcement.is_none() {
                    reports
                        .push(SendReport::new(member.public_key()).error("Encryption not found"));
                    continue;
                }

                // Determine receiver and signer based on signer kind
                let (receiver, signer_to_use) = match signer_kind {
                    SignerKind::Auto => {
                        if let Some(announcement) = announcement {
                            if let Some(enc_signer) = encryption_signer.as_ref() {
                                (announcement.public_key(), enc_signer.clone())
                            } else {
                                (member.public_key(), user_signer.clone())
                            }
                        } else {
                            (member.public_key(), user_signer.clone())
                        }
                    }
                    SignerKind::Encryption => {
                        let Some(encryption_signer) = encryption_signer.as_ref() else {
                            reports.push(
                                SendReport::new(member.public_key()).error("Encryption not found"),
                            );
                            continue;
                        };
                        let Some(announcement) = announcement else {
                            reports.push(
                                SendReport::new(member.public_key())
                                    .error("Announcement not found"),
                            );
                            continue;
                        };
                        (announcement.public_key(), encryption_signer.clone())
                    }
                    SignerKind::User => (member.public_key(), user_signer.clone()),
                };

                // Create and send gift-wrapped event
                match EventBuilder::gift_wrap(&signer_to_use, &receiver, rumor.clone(), []).await {
                    Ok(event) => {
                        match client
                            .send_event(&event)
                            .to(relays)
                            .ack_policy(AckPolicy::none())
                            .await
                        {
                            Ok(output) => {
                                reports.push(
                                    SendReport::new(member.public_key())
                                        .gift_wrap_id(event.id)
                                        .output(output),
                                );
                            }
                            Err(e) => {
                                reports.push(
                                    SendReport::new(member.public_key()).error(e.to_string()),
                                );
                            }
                        }
                    }
                    Err(e) => {
                        reports.push(SendReport::new(member.public_key()).error(e.to_string()));
                    }
                }
            }

            reports
        }))
    }

    /*
    * /// Create a new unsigned message event
    pub fn create_message(
        &self,
        content: &str,
        replies: Vec<EventId>,
        cx: &App,
    ) -> Task<Result<UnsignedEvent, Error>> {
        let nostr = NostrRegistry::global(cx);
        let client = nostr.read(cx).client();

        let subject = self.subject.clone();
        let content = content.to_string();

        let mut member_and_relay_hints = HashMap::new();

        // Populate the hashmap with member and relay hint tasks
        for member in self.members.iter() {
            let hint = nostr.read(cx).relay_hint(member, cx);
            member_and_relay_hints.insert(member.to_owned(), hint);
        }

        cx.background_spawn(async move {
            let signer = client.signer().context("Signer not found")?;
            let public_key = signer.get_public_key().await?;

            // List of event tags for each receiver
            let mut tags = vec![];

            for (member, task) in member_and_relay_hints.into_iter() {
                // Skip current user
                if member == public_key {
                    continue;
                }

                // Get relay hint if available
                let relay_url = task.await;

                // Construct a public key tag with relay hint
                let tag = TagStandard::PublicKey {
                    public_key: member,
                    relay_url,
                    alias: None,
                    uppercase: false,
                };

                tags.push(Tag::from_standardized_without_cell(tag));
            }

            // Add subject tag if present
            if let Some(value) = subject {
                tags.push(Tag::from_standardized_without_cell(TagStandard::Subject(
                    value.to_string(),
                )));
            }

            // Add all reply tags
            for id in replies {
                tags.push(Tag::event(id))
            }

            // Construct a direct message event
            //
            // WARNING: never sign and send this event to relays
            let mut event = EventBuilder::new(Kind::PrivateDirectMessage, content)
                .tags(tags)
                .build(public_key);

            // Ensure the event ID has been generated
            event.ensure_id();

            Ok(event)
        })
    }

    /// Create a task to send a message to all room members
    pub fn send_message(
        &self,
        rumor: &UnsignedEvent,
        cx: &App,
    ) -> Task<Result<Vec<SendReport>, Error>> {
        let nostr = NostrRegistry::global(cx);
        let client = nostr.read(cx).client();

        let mut members = self.members();
        let rumor = rumor.to_owned();

        cx.background_spawn(async move {
            let signer = client.signer().context("Signer not found")?;
            let current_user = signer.get_public_key().await?;

            // Remove the current user's public key from the list of receivers
            // the current user will be handled separately
            members.retain(|this| this != &current_user);

            // Collect the send reports
            let mut reports: Vec<SendReport> = vec![];

            for receiver in members.into_iter() {
                // Construct the gift wrap event
                let event =
                    EventBuilder::gift_wrap(signer, &receiver, rumor.clone(), vec![]).await?;

                // Send the gift wrap event to the messaging relays
                match client.send_event(&event).to_nip17().await {
                    Ok(output) => {
                        let id = output.id().to_owned();
                        let auth = output.failed.iter().any(|(_, s)| s.starts_with("auth-"));
                        let report = SendReport::new(receiver).status(output);
                        let tracker = tracker().read().await;

                        if auth {
                            // Wait for authenticated and resent event successfully
                            for attempt in 0..=SEND_RETRY {
                                // Check if event was successfully resent
                                if tracker.is_sent_by_coop(&id) {
                                    let output = Output::new(id);
                                    let report = SendReport::new(receiver).status(output);
                                    reports.push(report);
                                    break;
                                }

                                // Check if retry limit exceeded
                                if attempt == SEND_RETRY {
                                    reports.push(report);
                                    break;
                                }

                                smol::Timer::after(Duration::from_millis(1200)).await;
                            }
                        } else {
                            reports.push(report);
                        }
                    }
                    Err(e) => {
                        reports.push(SendReport::new(receiver).error(e.to_string()));
                    }
                }
            }

            // Construct the gift-wrapped event
            let event =
                EventBuilder::gift_wrap(signer, &current_user, rumor.clone(), vec![]).await?;

            // Only send a backup message to current user if sent successfully to others
            if reports.iter().all(|r| r.is_sent_success()) {
                // Send the event to the messaging relays
                match client.send_event(&event).to_nip17().await {
                    Ok(output) => {
                        reports.push(SendReport::new(current_user).status(output));
                    }
                    Err(e) => {
                        reports.push(SendReport::new(current_user).error(e.to_string()));
                    }
                }
            } else {
                reports.push(SendReport::new(current_user).on_hold(event));
            }

            Ok(reports)
        })
    }

    /// Create a task to resend a failed message
    pub fn resend_message(
        &self,
        reports: Vec<SendReport>,
        cx: &App,
    ) -> Task<Result<Vec<SendReport>, Error>> {
        let nostr = NostrRegistry::global(cx);
        let client = nostr.read(cx).client();

        cx.background_spawn(async move {
            let mut resend_reports = vec![];

            for report in reports.into_iter() {
                let receiver = report.receiver;

                // Process failed events
                if let Some(output) = report.status {
                    let id = output.id();
                    let urls: Vec<&RelayUrl> = output.failed.keys().collect();

                    if let Some(event) = client.database().event_by_id(id).await? {
                        for url in urls.into_iter() {
                            let relay = client.relay(url).await?.context("Relay not found")?;
                            let id = relay.send_event(&event).await?;

                            let resent: Output<EventId> = Output {
                                val: id,
                                success: HashSet::from([url.to_owned()]),
                                failed: HashMap::new(),
                            };

                            resend_reports.push(SendReport::new(receiver).status(resent));
                        }
                    }
                }

                // Process the on hold event if it exists
                if let Some(event) = report.on_hold {
                    // Send the event to the messaging relays
                    match client.send_event(&event).await {
                        Ok(output) => {
                            resend_reports.push(SendReport::new(receiver).status(output));
                        }
                        Err(e) => {
                            resend_reports.push(SendReport::new(receiver).error(e.to_string()));
                        }
                    }
                }
            }

            Ok(resend_reports)
        })
    }
    */
}
