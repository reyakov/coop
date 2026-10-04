use std::cmp::Ordering;
use std::hash::{Hash, Hasher};

use anyhow::{Error, anyhow};
use common::EventExt;
use gpui::{App, AppContext, Context, EventEmitter, SharedString, Task};
use instant::Duration;
use itertools::Itertools;
use nip4e::DeviceRegistry;
use nostr_sdk::prelude::*;
use person::{Person, PersonRegistry};
use settings::{RoomConfig, SignerKind};
use state::{NostrRegistry, TIMEOUT, UniversalSigner};

use crate::{FileAttachment, KIND_FILE_MESSAGE, NewMessage};

const NO_DEKEY: &str = "User hasn't set up a decoupled encryption key yet.";
const USER_NO_DEKEY: &str = "You haven't set up a decoupled encryption key yet. Set one up in the settings to message them.";

/// The outcome of delivering an encrypted rumor to a single receiver.
#[derive(Debug, Clone)]
pub struct SendReport {
    /// The receiver the rumor was addressed to.
    pub receiver: PublicKey,
    /// The per-relay send output, or the error that prevented the send.
    result: Result<Output<EventId, EventSendStatus>, SharedString>,
}

impl SendReport {
    /// The error that prevented the send, if any.
    pub fn error(&self) -> Option<&SharedString> {
        self.result.as_ref().err()
    }

    /// The per-relay send output, if the gift wrap was dispatched.
    pub fn output(&self) -> Option<&Output<EventId, EventSendStatus>> {
        self.result.as_ref().ok()
    }

    /// Whether at least one relay accepted the event.
    pub fn success(&self) -> bool {
        self.output()
            .is_some_and(|output| !output.success.is_empty())
    }

    /// Whether no relay accepted the event.
    pub fn failed(&self) -> bool {
        match &self.result {
            Err(_) => true,
            Ok(output) => output.success.is_empty(),
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
            .iter()
            .find(|tag| tag.kind() == "subject")
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
            .finalize_unsigned(author);

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
        self.kind = RoomKind::Ongoing;
        cx.notify();
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
    pub fn members(&self) -> &[PublicKey] {
        &self.members
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

    /// Gets the display picture for the room, if it has one
    pub fn display_image(&self, cx: &App) -> Option<SharedString> {
        if self.is_group() {
            None
        } else {
            self.display_member(cx).avatar()
        }
    }

    /// A stable seed for the room's generated avatar
    pub fn display_image_seed(&self, cx: &App) -> String {
        if self.is_group() {
            self.id.to_string()
        } else {
            self.display_member(cx).avatar_seed()
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
        let members = self.members().to_vec();

        cx.background_spawn(async move {
            let opts = SubscribeAutoCloseOptions::default()
                .exit_policy(ReqExitPolicy::ExitOnEOSE)
                .timeout(Some(Duration::from_secs(TIMEOUT)));

            let tasks: Vec<_> = members
                .into_iter()
                .map(|public_key| {
                    let client = client.clone();
                    async move {
                        let inbox = Filter::new()
                            .author(public_key)
                            .kind(Kind::InboxRelays)
                            .limit(1);

                        let announcement = Filter::new()
                            .author(public_key)
                            .kind(Kind::Custom(10044))
                            .limit(1);

                        client
                            .subscribe(vec![inbox, announcement])
                            .close_on(opts)
                            .await
                    }
                })
                .collect();

            for result in futures::future::join_all(tasks).await {
                result?;
            }

            Ok(())
        })
    }

    /// Get all messages belonging to the room
    pub fn get_messages(&self, cx: &App) -> Task<Result<Vec<UnsignedEvent>, Error>> {
        let nostr = NostrRegistry::global(cx);
        let client = nostr.read(cx).client();
        let room_id = self.id.to_string();

        cx.background_spawn(async move {
            let filter = Filter::new()
                .kind(Kind::ApplicationSpecificData)
                .custom_tag(SingleLetterTag::LOWERCASE_R, room_id);

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
    pub fn rumor<S, I>(
        &self,
        content: S,
        replies: I,
        reaction: bool,
        cx: &App,
    ) -> Option<UnsignedEvent>
    where
        S: Into<String>,
        I: IntoIterator<Item = EventId>,
    {
        let kind = if reaction {
            Kind::Reaction
        } else {
            Kind::PrivateDirectMessage
        };

        let content: String = content.into();
        let replies: Vec<EventId> = replies.into_iter().collect();

        // Get current user's public key
        let nostr = NostrRegistry::global(cx);
        let sender = nostr.read(cx).current_user()?;

        // Construct a direct message rumor event
        // WARNING: never sign and send this event to relays
        let mut event = EventBuilder::new(kind, content)
            .tags(self.conversation_tags(&replies, sender, cx))
            .finalize_unsigned(sender);

        // Ensure that the ID is set
        event.ensure_id();

        Some(event)
    }

    // Construct a rumor event for an encrypted file message (NIP-17 kind 15)
    pub fn file_rumor<I>(&self, file: FileAttachment, replies: I, cx: &App) -> Option<UnsignedEvent>
    where
        I: IntoIterator<Item = EventId>,
    {
        let replies: Vec<EventId> = replies.into_iter().collect();

        // Get current user's public key
        let nostr = NostrRegistry::global(cx);
        let sender = nostr.read(cx).current_user()?;

        let mut tags = self.conversation_tags(&replies, sender, cx);
        tags.extend(file.tags());

        // Construct a file message rumor event
        // WARNING: never sign and send this event to relays
        let mut event = EventBuilder::new(KIND_FILE_MESSAGE, file.url.to_string())
            .tags(tags)
            .finalize_unsigned(sender);

        // Ensure that the ID is set
        event.ensure_id();

        Some(event)
    }

    // Build the `subject` + reply `e` tags + receiver `p` tags (excluding `sender`)
    fn conversation_tags(&self, replies: &[EventId], sender: PublicKey, cx: &App) -> Vec<Tag> {
        let persons = PersonRegistry::global(cx);

        // Construct event's tags
        let mut tags = vec![];

        // Add subject tag if present
        if let Some(value) = self.subject.as_ref() {
            tags.push(Tag::custom("subject", vec![value.to_string()]));
        }

        // Add all reply tags
        for id in replies {
            tags.push(Tag::event(*id))
        }

        // Add all receiver tags (no intermediate allocation)
        for public_key in self.members.iter().filter(|pk| *pk != &sender) {
            let member = persons.read(cx).get(public_key, cx);
            tags.push(Tag::from(Nip01Tag::PublicKey {
                public_key: member.public_key(),
                relay_hint: member.messaging_relay_hint(),
            }));
        }

        tags
    }

    /// Send the rumor to every member's messaging relays.
    pub fn send(&self, rumor: UnsignedEvent, cx: &App) -> Option<Task<Vec<SendReport>>> {
        let config = self.config.clone();

        let device = DeviceRegistry::global(cx);
        let encryption_signer = device.read(cx).signer(cx);

        let nostr = NostrRegistry::global(cx);
        let client = nostr.read(cx).client();
        let user_signer = nostr.read(cx).signer();
        let current_user = nostr.read(cx).current_user()?;

        // Get sender's profile
        let persons = PersonRegistry::global(cx);
        let sender = persons.read(cx).get(&current_user, cx);

        // Get all members (excluding sender)
        let receivers: Vec<Person> = self
            .members
            .iter()
            .filter(|public_key| public_key != &&sender.public_key())
            .map(|member| persons.read(cx).get(member, cx))
            .collect();

        Some(cx.background_spawn(async move {
            let signer_kind = config.signer_kind();

            // Deliver to every receiver concurrently
            let sends = receivers.into_iter().map(|receiver| {
                deliver(
                    &client,
                    signer_kind,
                    receiver,
                    &rumor,
                    encryption_signer.as_ref(),
                    &user_signer,
                )
            });

            let mut reports: Vec<SendReport> = futures::future::join_all(sends).await;
            let minimum_success = reports.iter().any(SendReport::success);

            // Back up the message to ourselves once at least one receiver got it
            if config.backup() && minimum_success {
                reports.push(
                    deliver(
                        &client,
                        signer_kind,
                        sender,
                        &rumor,
                        encryption_signer.as_ref(),
                        &user_signer,
                    )
                    .await,
                );
            }

            reports
        }))
    }
}

/// Deliver a rumor to a single receiver.
async fn deliver<S>(
    client: &Client,
    signer_kind: &SignerKind,
    receiver: Person,
    rumor: &UnsignedEvent,
    encryption_signer: Option<&UniversalSigner>,
    user_signer: &S,
) -> SendReport
where
    S: AsyncGetPublicKey + AsyncSignEvent + AsyncNip44,
{
    let public_key = receiver.public_key();
    let announcement = receiver.announcement();

    if signer_kind.encryption() {
        // Receiver didn't set up a decoupled encryption key
        if announcement.is_none() {
            return SendReport {
                receiver: public_key,
                result: Err(NO_DEKEY.into()),
            };
        }

        // Sender didn't set up a decoupled encryption key
        if encryption_signer.is_none() {
            return SendReport {
                receiver: public_key,
                result: Err(USER_NO_DEKEY.into()),
            };
        }
    }

    match send_gift_wrap(
        client,
        &receiver,
        rumor,
        signer_kind,
        encryption_signer,
        user_signer,
    )
    .await
    {
        Ok(output) => SendReport {
            receiver: public_key,
            result: Ok(output),
        },
        Err(error) => SendReport {
            receiver: public_key,
            result: Err(error.to_string().into()),
        },
    }
}

/// Build the gift-wrapped event for a rumor and send it.
async fn send_gift_wrap<S>(
    client: &Client,
    receiver: &Person,
    rumor: &UnsignedEvent,
    config: &SignerKind,
    encryption_signer: Option<&UniversalSigner>,
    user_signer: &S,
) -> Result<Output<EventId, EventSendStatus>, Error>
where
    S: AsyncGetPublicKey + AsyncSignEvent + AsyncNip44,
{
    let mut extra_tags = vec![Tag::custom("k", [rumor.kind.to_string()])];

    // Determine the receiver public key based on the config
    let receiver_key = match config {
        SignerKind::Auto => match receiver.announcement().as_ref() {
            Some(announcement) => {
                extra_tags.push(Tag::public_key(receiver.public_key()));
                announcement.public_key()
            }
            None => receiver.public_key(),
        },
        SignerKind::Encryption => match receiver.announcement().as_ref() {
            Some(announcement) => {
                extra_tags.push(Tag::public_key(receiver.public_key()));
                announcement.public_key()
            }
            None => return Err(anyhow!("User has no encryption announcement")),
        },
        SignerKind::User => receiver.public_key(),
    };

    // Make sure the rumor carries an event ID before it's serialized
    let mut rumor = rumor.clone();
    rumor.ensure_id();

    let rumor_json = rumor.as_json();
    let has_encryption_announcement = receiver.announcement().is_some();

    let seal: Event = match (encryption_signer, has_encryption_announcement) {
        (Some(encryption_signer), true) => {
            let encryption_pubkey = encryption_signer.get_public_key_async().await?;
            let content = encryption_signer
                .nip44_encrypt_async(&receiver_key, &rumor_json)
                .await?;

            EventBuilder::new(Kind::Seal, content)
                .tags(vec![Tag::custom("n", [encryption_pubkey.to_hex()])])
                .custom_created_at(tweaked_timestamp())
                .finalize_async(user_signer)
                .await?
        }
        _ => {
            let content = user_signer
                .nip44_encrypt_async(&receiver_key, &rumor_json)
                .await?;

            EventBuilder::new(Kind::Seal, content)
                .custom_created_at(tweaked_timestamp())
                .finalize_async(user_signer)
                .await?
        }
    };

    let ephemeral_keys = Keys::generate();
    let content = nip44::encrypt(
        ephemeral_keys.secret_key(),
        &receiver_key,
        seal.as_json(),
        nip44::Version::default(),
    )?;
    extra_tags.push(Tag::public_key(receiver_key));

    let event = EventBuilder::new(Kind::GiftWrap, content)
        .tags(extra_tags)
        .custom_created_at(tweaked_timestamp())
        .finalize(&ephemeral_keys)?;

    // Send to the receiver's NIP-17 relays.
    client
        .send_event(&event)
        .to_nip17()
        .await
        .map_err(Into::into)
}

/// A tweaked timestamp to thwart time-analysis attacks, per NIP-59
fn tweaked_timestamp() -> Timestamp {
    const MAX_TWEAK_SECONDS: u64 = 2 * 24 * 60 * 60;

    let seconds = rand::random_range(0..=MAX_TWEAK_SECONDS);
    Timestamp::from_secs(Timestamp::now().as_secs().saturating_sub(seconds))
}
