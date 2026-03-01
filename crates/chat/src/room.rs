use std::cmp::Ordering;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::time::Duration;

use anyhow::Error;
use common::EventUtils;
use gpui::{App, AppContext, Context, EventEmitter, SharedString, Task};
use itertools::Itertools;
use nostr_sdk::prelude::*;
use person::{Person, PersonRegistry};
use settings::{RoomConfig, SignerKind};
use state::{NostrRegistry, BOOTSTRAP_RELAYS, TIMEOUT};

use crate::NewMessage;

const NO_DEKEY: &str = "User hasn't set up a decoupled encryption key yet.";
const USER_NO_DEKEY: &str = "You haven't set up a decoupled encryption key or it's not available.";

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
            config: RoomConfig::new(),
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

    /// Updates the signer kind config for the room
    pub fn set_signer_kind(&mut self, kind: &SignerKind, cx: &mut Context<Self>) {
        self.config.set_signer_kind(kind);
        cx.notify();
    }

    /// Updates the backup config for the room
    pub fn set_backup(&mut self, cx: &mut Context<Self>) {
        self.config.toggle_backup();
        cx.notify();
    }

    /// Returns the config of the room
    pub fn config(&self) -> &RoomConfig {
        &self.config
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
        }
    }

    /// Emits a signal to reload the current room's messages.
    pub fn emit_refresh(&mut self, cx: &mut Context<Self>) {
        cx.emit(RoomEvent::Reload);
    }

    /// Get gossip relays for each member
    pub fn connect(&self, cx: &App) -> Task<Result<(), Error>> {
        let nostr = NostrRegistry::global(cx);
        let client = nostr.read(cx).client();

        let signer = nostr.read(cx).signer();
        let sender = signer.public_key();

        // Get room's id
        let id = self.id;

        // Get all members, excluding the sender
        let members: Vec<PublicKey> = self
            .members
            .iter()
            .filter(|public_key| Some(**public_key) != sender)
            .copied()
            .collect();

        cx.background_spawn(async move {
            let id = SubscriptionId::new(format!("room-{id}"));
            let opts = SubscribeAutoCloseOptions::default()
                .exit_policy(ReqExitPolicy::ExitOnEOSE)
                .timeout(Some(Duration::from_secs(TIMEOUT)));

            // Construct filters for each member
            let filters: Vec<Filter> = members
                .into_iter()
                .map(|public_key| {
                    Filter::new()
                        .author(public_key)
                        .kind(Kind::RelayList)
                        .limit(1)
                })
                .collect();

            // Construct target for subscription
            let target: HashMap<&str, Vec<Filter>> = BOOTSTRAP_RELAYS
                .into_iter()
                .map(|relay| (relay, filters.clone()))
                .collect();

            // Subscribe to the target
            client.subscribe(target).close_on(opts).with_id(id).await?;

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

        // Get all members, excluding the sender
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
        let config = self.config.clone();
        let persons = PersonRegistry::global(cx);
        let nostr = NostrRegistry::global(cx);
        let client = nostr.read(cx).client();
        let signer = nostr.read(cx).signer();

        // Get current user's public key
        let public_key = nostr.read(cx).signer().public_key()?;
        let sender = persons.read(cx).get(&public_key, cx);

        // Get all members (excluding sender)
        let members: Vec<Person> = self
            .members
            .iter()
            .filter(|public_key| public_key != &&sender.public_key())
            .map(|member| persons.read(cx).get(member, cx))
            .collect();

        Some(cx.background_spawn(async move {
            let signer_kind = config.signer_kind();
            let backup = config.backup();

            let user_signer = signer.get().await;
            let encryption_signer = signer.get_encryption_signer().await;

            let mut sents = 0;
            let mut reports = Vec::new();

            // Process each member
            for member in members {
                let relays = member.messaging_relays();
                let announcement = member.announcement();
                let public_key = member.public_key();

                if relays.is_empty() {
                    reports.push(SendReport::new(public_key).error("No messaging relays"));
                    continue;
                }

                // Handle encryption signer requirements
                if signer_kind.encryption() {
                    if announcement.is_none() {
                        reports.push(SendReport::new(public_key).error(NO_DEKEY));
                        continue;
                    }
                    if encryption_signer.is_none() {
                        reports.push(SendReport::new(sender.public_key()).error(USER_NO_DEKEY));
                        continue;
                    }
                }

                // Determine receiver and signer
                let (receiver, signer) = match signer_kind {
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
                        // Safe to unwrap due to earlier checks
                        (
                            announcement.unwrap().public_key(),
                            encryption_signer.as_ref().unwrap().clone(),
                        )
                    }
                    SignerKind::User => (member.public_key(), user_signer.clone()),
                };

                match send_gift_wrap(&client, &signer, &receiver, &rumor, relays, public_key).await
                {
                    Ok((report, _)) => {
                        reports.push(report);
                        sents += 1;
                    }
                    Err(report) => {
                        reports.push(report);
                    }
                }
            }

            // Send backup to current user if needed
            if backup && sents >= 1 {
                let relays = sender.messaging_relays();
                let public_key = sender.public_key();
                let signer = encryption_signer.as_ref().unwrap_or(&user_signer);

                match send_gift_wrap(&client, signer, &public_key, &rumor, relays, public_key).await
                {
                    Ok((report, _)) => reports.push(report),
                    Err(report) => reports.push(report),
                }
            }

            reports
        }))
    }
}

// Helper function to send a gift-wrapped event
async fn send_gift_wrap<T>(
    client: &Client,
    signer: &T,
    receiver: &PublicKey,
    rumor: &UnsignedEvent,
    relays: &[RelayUrl],
    public_key: PublicKey,
) -> Result<(SendReport, bool), SendReport>
where
    T: NostrSigner + 'static,
{
    // Ensure relay connections
    for url in relays {
        client.add_relay(url).and_connect().await.ok();
    }

    match EventBuilder::gift_wrap(signer, receiver, rumor.clone(), []).await {
        Ok(event) => {
            match client
                .send_event(&event)
                .to(relays)
                .ack_policy(AckPolicy::none())
                .await
            {
                Ok(output) => Ok((
                    SendReport::new(public_key)
                        .gift_wrap_id(event.id)
                        .output(output),
                    true,
                )),
                Err(e) => Err(SendReport::new(public_key).error(e.to_string())),
            }
        }
        Err(e) => Err(SendReport::new(public_key).error(e.to_string())),
    }
}
