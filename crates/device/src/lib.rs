use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

use anyhow::{Context as AnyhowContext, Error, anyhow};
use gpui::{
    App, AppContext, Context, Entity, EventEmitter, Global, IntoElement, ParentElement,
    SharedString, Styled, Subscription, Task, Window, div, relative,
};
use nostr_sdk::prelude::*;
use person::PersonRegistry;
use state::{Announcement, DEVICE_GIFTWRAP, NostrRegistry, StateEvent, TIMEOUT, app_name};
use theme::ActiveTheme;
use ui::avatar::Avatar;
use ui::button::{Button, ButtonVariants};
use ui::notification::Notification;
use ui::{Disableable, IconName, Sizable, WindowExtension, h_flex, v_flex};

const IDENTIFIER: &str = "coop:device";
const MSG: &str = "You've requested an encryption key from another device. \
                   Approve to allow Coop to share with it.";

pub fn init(window: &mut Window, cx: &mut App) {
    DeviceRegistry::set_global(cx.new(|cx| DeviceRegistry::new(window, cx)), cx);
}

struct GlobalDeviceRegistry(Entity<DeviceRegistry>);

impl Global for GlobalDeviceRegistry {}

/// Device event.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum DeviceEvent {
    /// A new encryption signer has been set
    Set,
    /// The device is requesting an encryption key
    Requesting,
    /// The device is creating a new encryption key
    Creating,
    /// Encryption key is not set
    NotSet { reason: SharedString },
    /// An event to notify that Coop isn't subscribed to gift wrap events
    NotSubscribe { reason: SharedString },
    /// An error occurred
    Error(SharedString),
}

impl DeviceEvent {
    pub fn error<T>(error: T) -> Self
    where
        T: Into<SharedString>,
    {
        Self::Error(error.into())
    }

    pub fn not_subscribe<T>(reason: T) -> Self
    where
        T: Into<SharedString>,
    {
        Self::NotSubscribe {
            reason: reason.into(),
        }
    }

    pub fn not_set<T>(reason: T) -> Self
    where
        T: Into<SharedString>,
    {
        Self::NotSet {
            reason: reason.into(),
        }
    }
}

/// Device Registry
///
/// NIP-4e: https://github.com/nostr-protocol/nips/blob/per-device-keys/4e.md
#[derive(Debug)]
pub struct DeviceRegistry {
    /// Whether the registry is currently subscribing to gift wrap events
    pub subscribing: bool,

    /// Whether the registry is waiting for encryption key approval from other devices
    pub requesting: bool,

    /// Async tasks
    tasks: Vec<Task<Result<(), Error>>>,

    /// Event subscription
    _subscription: Option<Subscription>,
}

impl EventEmitter<DeviceEvent> for DeviceRegistry {}

impl DeviceRegistry {
    /// Retrieve the global device registry state
    pub fn global(cx: &App) -> Entity<Self> {
        cx.global::<GlobalDeviceRegistry>().0.clone()
    }

    /// Set the global device registry instance
    fn set_global(state: Entity<Self>, cx: &mut App) {
        cx.set_global(GlobalDeviceRegistry(state));
    }

    /// Create a new device registry instance
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let nostr = NostrRegistry::global(cx);

        // Get announcement when signer is set
        let subscription = cx.subscribe_in(&nostr, window, |this, _e, event, _window, cx| {
            match event {
                StateEvent::SignerSet => {
                    this.set_subscribing(false, cx);
                    this.set_requesting(false, cx);
                }
                StateEvent::RelayConnected => {
                    this.get_announcement(cx);
                }
                _ => {}
            };
        });

        cx.defer_in(window, |this, window, cx| {
            this.handle_notifications(window, cx);
        });

        Self {
            subscribing: false,
            requesting: false,
            tasks: vec![],
            _subscription: Some(subscription),
        }
    }

    fn handle_notifications(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let nostr = NostrRegistry::global(cx);
        let client = nostr.read(cx).client();
        let (tx, rx) = flume::bounded::<Event>(100);

        self.tasks.push(cx.background_spawn(async move {
            let mut notifications = client.notifications();
            let mut processed_events = HashSet::new();

            while let Some(notification) = notifications.next().await {
                if let ClientNotification::Message {
                    message: RelayMessage::Event { event, .. },
                    ..
                } = notification
                {
                    if !processed_events.insert(event.id) {
                        // Skip if the event has already been processed
                        continue;
                    }

                    match event.kind {
                        Kind::Custom(4454) => {
                            if verify_author(&client, event.as_ref()).await {
                                tx.send_async(event.into_owned()).await?;
                            }
                        }
                        Kind::Custom(4455) => {
                            if verify_author(&client, event.as_ref()).await {
                                tx.send_async(event.into_owned()).await?;
                            }
                        }
                        _ => {}
                    }
                }
            }

            Ok(())
        }));

        self.tasks.push(cx.spawn_in(window, async move |this, cx| {
            while let Ok(event) = rx.recv_async().await {
                match event.kind {
                    // New request event from other device
                    Kind::Custom(4454) => {
                        this.update_in(cx, |this, window, cx| {
                            this.ask_for_approval(event, window, cx);
                        })?;
                    }
                    // New response event from the master device
                    Kind::Custom(4455) => {
                        this.update(cx, |this, cx| {
                            this.extract_encryption(event, cx);
                        })?;
                    }
                    _ => {}
                }
            }
            Ok(())
        }));
    }

    /// Set whether the registry is currently subscribing to gift wrap events
    fn set_subscribing(&mut self, subscribing: bool, cx: &mut Context<Self>) {
        self.subscribing = subscribing;
        cx.notify();
    }

    /// Set whether the registry is waiting for encryption key approval from other devices
    fn set_requesting(&mut self, requesting: bool, cx: &mut Context<Self>) {
        self.requesting = requesting;
        cx.notify();
    }

    /// Set the decoupled encryption key for the current user
    fn set_signer<S>(&mut self, new: S, cx: &mut Context<Self>)
    where
        S: NostrSigner + 'static,
    {
        let nostr = NostrRegistry::global(cx);
        let signer = nostr.read(cx).signer();

        self.tasks.push(cx.spawn(async move |this, cx| {
            signer.set_encryption_signer(new).await;

            // Update state
            this.update(cx, |this, cx| {
                cx.emit(DeviceEvent::Set);
                this.get_messages(cx);
            })?;

            Ok(())
        }));
    }

    /// Get all messages for encryption keys
    fn get_messages(&mut self, cx: &mut Context<Self>) {
        let task = self.subscribe_to_giftwrap_events(cx);

        self.tasks.push(cx.spawn(async move |this, cx| {
            if let Err(e) = task.await {
                this.update(cx, |_this, cx| {
                    cx.emit(DeviceEvent::not_subscribe(e.to_string()));
                })?;
            } else {
                this.update(cx, |this, cx| {
                    this.set_subscribing(true, cx);
                })?;
            }
            Ok(())
        }));
    }

    /// Continuously get gift wrap events for the current user in their messaging relays
    fn subscribe_to_giftwrap_events(&self, cx: &App) -> Task<Result<(), Error>> {
        let persons = PersonRegistry::global(cx);
        let nostr = NostrRegistry::global(cx);
        let client = nostr.read(cx).client();
        let signer = nostr.read(cx).signer();

        let Some(user) = signer.public_key() else {
            return Task::ready(Err(anyhow!("User not found")));
        };

        let profile = persons.read(cx).get(&user, cx);
        let relays = profile.messaging_relays().clone();

        cx.background_spawn(async move {
            let encryption = signer.get_encryption_signer().await.context("not found")?;
            let public_key = encryption.get_public_key().await?;

            let filter = Filter::new().kind(Kind::GiftWrap).pubkey(public_key);
            let id = SubscriptionId::new(DEVICE_GIFTWRAP);

            // Ensure user has relays configured
            if relays.is_empty() {
                return Err(anyhow!("No messaging relays found"));
            }

            // Ensure relays are connected
            for url in relays.iter() {
                client.add_relay(url).and_connect().await?;
            }

            // Construct target for subscription
            let target: HashMap<RelayUrl, Filter> = relays
                .into_iter()
                .map(|relay| (relay, filter.clone()))
                .collect();

            // Subscribe
            client.subscribe(target).with_id(id).await?;

            Ok(())
        })
    }

    /// Backup the encryption's secret key to a file
    pub fn backup(&self, path: PathBuf, cx: &App) -> Task<Result<(), Error>> {
        let nostr = NostrRegistry::global(cx);
        let client = nostr.read(cx).client();

        cx.background_spawn(async move {
            let keys = get_keys(&client).await?;
            let content = keys.secret_key().to_bech32()?;

            smol::fs::write(path, &content).await?;

            Ok(())
        })
    }

    /// Get device announcement for current user
    pub fn get_announcement(&mut self, cx: &mut Context<Self>) {
        let nostr = NostrRegistry::global(cx);
        let client = nostr.read(cx).client();

        let task: Task<Result<Event, Error>> = cx.background_spawn(async move {
            let signer = client.signer().context("Signer not found")?;
            let public_key = signer.get_public_key().await?;

            // Construct the filter for the device announcement event
            let filter = Filter::new()
                .kind(Kind::Custom(10044))
                .author(public_key)
                .limit(1);

            // Stream events from user's write relays
            let mut stream = client
                .stream_events(filter)
                .timeout(Duration::from_secs(TIMEOUT))
                .await?;

            while let Some((_url, res)) = stream.next().await {
                if let Ok(event) = res {
                    return Ok(event);
                }
            }

            Err(anyhow!("Announcement not found"))
        });

        self.tasks.push(cx.spawn(async move |this, cx| {
            match task.await {
                Ok(event) => {
                    // Set encryption key from the announcement event
                    this.update(cx, |this, cx| {
                        this.set_encryption(&event, cx);
                    })?;
                }
                Err(_) => {
                    // User has no announcement, create a new one
                    this.update(cx, |this, cx| {
                        this.set_announcement(Keys::generate(), cx);
                    })?;
                }
            }

            Ok(())
        }));
    }

    /// Create a new device signer and announce it to user's relay list
    pub fn set_announcement(&mut self, keys: Keys, cx: &mut Context<Self>) {
        let task = self.create_encryption(keys, cx);

        // Notify that we're creating a new encryption key
        cx.emit(DeviceEvent::Creating);

        self.tasks.push(cx.spawn(async move |this, cx| {
            match task.await {
                Ok(keys) => {
                    this.update(cx, |this, cx| {
                        this.set_signer(keys, cx);
                        this.wait_for_request(cx);
                    })?;
                }
                Err(e) => {
                    this.update(cx, |_this, cx| {
                        cx.emit(DeviceEvent::error(e.to_string()));
                    })?;
                }
            }
            Ok(())
        }));
    }

    /// Create new encryption key and announce it to user's relay list
    fn create_encryption(&self, keys: Keys, cx: &App) -> Task<Result<Keys, Error>> {
        let nostr = NostrRegistry::global(cx);
        let client = nostr.read(cx).client();

        let secret = keys.secret_key().to_secret_hex();
        let n = keys.public_key();

        cx.background_spawn(async move {
            // Construct an announcement event
            let builder = EventBuilder::new(Kind::Custom(10044), "").tags(vec![
                Tag::custom(TagKind::custom("n"), vec![n]),
                Tag::client(app_name()),
            ]);

            // Sign the event with user's signer
            let event = client.sign_event_builder(builder).await?;

            // Publish announcement
            client
                .send_event(&event)
                .to_nip65()
                .ack_policy(AckPolicy::none())
                .await?;

            // Save device keys to the database
            set_keys(&client, &secret).await?;

            Ok(keys)
        })
    }

    /// Set encryption key from the announcement event
    fn set_encryption(&mut self, event: &Event, cx: &mut Context<Self>) {
        let nostr = NostrRegistry::global(cx);
        let client = nostr.read(cx).client();

        let announcement = Announcement::from(event);
        let device_pubkey = announcement.public_key();

        // Get encryption key from the database and compare with the announcement
        let task: Task<Result<Keys, Error>> = cx.background_spawn(async move {
            if let Ok(keys) = get_keys(&client).await {
                if keys.public_key() != device_pubkey {
                    return Err(anyhow!("Encryption Key doesn't match the announcement"));
                };
                Ok(keys)
            } else {
                Err(anyhow!("Encryption Key not found. Please create a new key"))
            }
        });

        self.tasks.push(cx.spawn(async move |this, cx| {
            match task.await {
                Ok(keys) => {
                    this.update(cx, |this, cx| {
                        this.set_signer(keys, cx);
                        this.wait_for_request(cx);
                    })?;
                }
                Err(e) => {
                    this.update(cx, |_this, cx| {
                        cx.emit(DeviceEvent::not_set(e.to_string()));
                    })?;
                }
            };
            Ok(())
        }));
    }

    /// Wait for encryption key requests from now on
    fn wait_for_request(&mut self, cx: &mut Context<Self>) {
        let nostr = NostrRegistry::global(cx);
        let client = nostr.read(cx).client();
        let signer = nostr.read(cx).signer();

        self.tasks.push(cx.background_spawn(async move {
            let public_key = signer.get_public_key().await?;

            // Construct a filter for encryption key requests
            let now = Filter::new()
                .kind(Kind::Custom(4454))
                .author(public_key)
                .since(Timestamp::now());

            // Construct a filter for the last encryption key request
            let last = Filter::new()
                .kind(Kind::Custom(4454))
                .author(public_key)
                .limit(1);

            // Subscribe to the device key requests on user's write relays
            client.subscribe(vec![now, last]).await?;

            Ok(())
        }));
    }

    /// Request encryption keys from other device
    pub fn request(&mut self, cx: &mut Context<Self>) {
        let nostr = NostrRegistry::global(cx);
        let client = nostr.read(cx).client();
        let signer = nostr.read(cx).signer();

        let app_keys = nostr.read(cx).keys();
        let app_pubkey = app_keys.public_key();

        let task: Task<Result<Option<Event>, Error>> = cx.background_spawn(async move {
            let public_key = signer.get_public_key().await?;

            // Construct a filter to get the latest approval event
            let filter = Filter::new()
                .kind(Kind::Custom(4455))
                .author(public_key)
                .pubkey(app_pubkey)
                .limit(1);

            match client.database().query(filter).await?.first_owned() {
                // Found an approval event
                Some(event) => Ok(Some(event)),
                // No approval event found, construct a request event
                None => {
                    // Construct an event for device key request
                    let builder = EventBuilder::new(Kind::Custom(4454), "").tags(vec![
                        Tag::custom(TagKind::custom("P"), vec![app_pubkey]),
                        Tag::client(app_name()),
                    ]);

                    // Sign the event with user's signer
                    let event = client.sign_event_builder(builder).await?;

                    // Send the event to write relays
                    client.send_event(&event).to_nip65().await?;

                    Ok(None)
                }
            }
        });

        self.tasks.push(cx.spawn(async move |this, cx| {
            match task.await {
                Ok(Some(event)) => {
                    this.update(cx, |this, cx| {
                        this.extract_encryption(event, cx);
                    })?;
                }
                Ok(None) => {
                    this.update(cx, |this, cx| {
                        this.set_requesting(true, cx);
                        this.wait_for_approval(cx);

                        cx.emit(DeviceEvent::Requesting);
                    })?;
                }
                Err(e) => {
                    this.update(cx, |_this, cx| {
                        cx.emit(DeviceEvent::error(e.to_string()));
                    })?;
                }
            };
            Ok(())
        }));
    }

    /// Wait for encryption key approvals
    fn wait_for_approval(&mut self, cx: &mut Context<Self>) {
        let nostr = NostrRegistry::global(cx);
        let client = nostr.read(cx).client();
        let signer = nostr.read(cx).signer();

        self.tasks.push(cx.background_spawn(async move {
            let public_key = signer.get_public_key().await?;

            // Construct a filter for device key requests
            let filter = Filter::new()
                .kind(Kind::Custom(4455))
                .author(public_key)
                .since(Timestamp::now());

            // Subscribe to the device key requests on user's write relays
            client.subscribe(filter).await?;

            Ok(())
        }));
    }

    /// Parse the approval event to get encryption key then set it
    fn extract_encryption(&mut self, event: Event, cx: &mut Context<Self>) {
        let nostr = NostrRegistry::global(cx);
        let app_keys = nostr.read(cx).keys();

        let task: Task<Result<Keys, Error>> = cx.background_spawn(async move {
            let master = event
                .tags
                .find(TagKind::custom("P"))
                .and_then(|tag| tag.content())
                .and_then(|content| PublicKey::parse(content).ok())
                .context("Invalid event's tags")?;

            let payload = event.content.as_str();
            let decrypted = app_keys.nip44_decrypt(&master, payload).await?;

            let secret = SecretKey::from_hex(&decrypted)?;
            let keys = Keys::new(secret);

            Ok(keys)
        });

        self.tasks.push(cx.spawn(async move |this, cx| {
            match task.await {
                Ok(keys) => {
                    this.update(cx, |this, cx| {
                        this.set_signer(keys, cx);
                        this.set_requesting(false, cx);
                    })?;
                }
                Err(e) => {
                    this.update(cx, |_this, cx| {
                        cx.emit(DeviceEvent::not_set(e.to_string()));
                    })?;
                }
            }
            Ok(())
        }));
    }

    /// Approve requests for device keys from other devices
    fn approve(&mut self, event: &Event, window: &mut Window, cx: &mut Context<Self>) {
        let nostr = NostrRegistry::global(cx);
        let client = nostr.read(cx).client();
        let signer = nostr.read(cx).signer();

        // Get user's write relays
        let event = event.clone();
        let id: SharedString = event.id.to_hex().into();

        let task: Task<Result<(), Error>> = cx.background_spawn(async move {
            // Get device keys
            let keys = get_keys(&client).await?;
            let secret = keys.secret_key().to_secret_hex();

            // Extract the target public key from the event tags
            let target = event
                .tags
                .find(TagKind::custom("P"))
                .and_then(|tag| tag.content())
                .and_then(|content| PublicKey::parse(content).ok())
                .context("Target is not a valid public key")?;

            // Encrypt the device keys with the user's signer
            let payload = signer.nip44_encrypt(&target, &secret).await?;

            // Construct the response event
            //
            // P tag: the current device's public key
            // p tag: the requester's public key
            let builder = EventBuilder::new(Kind::Custom(4455), payload).tags(vec![
                Tag::custom(TagKind::custom("P"), vec![keys.public_key()]),
                Tag::public_key(target),
            ]);

            // Sign the builder
            let event = client.sign_event_builder(builder).await?;

            // Send the response event to the user's relay list
            client.send_event(&event).to_nip65().await?;

            Ok(())
        });

        cx.spawn_in(window, async move |_this, cx| {
            match task.await {
                Ok(_) => {
                    cx.update(|window, cx| {
                        window.clear_notification_by_id::<DeviceNotification>(id, cx);
                    })
                    .ok();
                }
                Err(e) => {
                    cx.update(|window, cx| {
                        window.push_notification(
                            Notification::error(e.to_string()).autohide(false),
                            cx,
                        );
                    })
                    .ok();
                }
            };
        })
        .detach();
    }

    /// Handle encryption request
    fn ask_for_approval(&mut self, event: Event, window: &mut Window, cx: &mut Context<Self>) {
        let notification = self.notification(event, cx);

        cx.spawn_in(window, async move |_this, cx| {
            cx.update(|window, cx| {
                window.push_notification(notification, cx);
            })
            .ok();
        })
        .detach();
    }

    /// Build a notification for the encryption request.
    fn notification(&self, event: Event, cx: &Context<Self>) -> Notification {
        let request = Announcement::from(&event);
        let persons = PersonRegistry::global(cx);
        let profile = persons.read(cx).get(&request.public_key(), cx);

        let entity = cx.entity().downgrade();
        let loading = Rc::new(Cell::new(false));
        let key = SharedString::from(event.id.to_hex());

        Notification::new()
            .type_id::<DeviceNotification>(key)
            .autohide(false)
            .icon(IconName::UserKey)
            .title(SharedString::from("New request"))
            .content(move |_this, _window, cx| {
                v_flex()
                    .gap_2()
                    .text_sm()
                    .child(
                        div()
                            .text_sm()
                            .line_height(relative(1.25))
                            .child(SharedString::from(MSG)),
                    )
                    .child(
                        v_flex()
                            .gap_2()
                            .child(
                                v_flex()
                                    .gap_1()
                                    .text_sm()
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(cx.theme().text_muted)
                                            .child(SharedString::from("Requester:")),
                                    )
                                    .child(
                                        div()
                                            .h_7()
                                            .w_full()
                                            .px_2()
                                            .rounded(cx.theme().radius)
                                            .bg(cx.theme().elevated_surface_background)
                                            .child(
                                                h_flex()
                                                    .gap_2()
                                                    .child(Avatar::new(profile.avatar()).xsmall())
                                                    .child(profile.name()),
                                            ),
                                    ),
                            )
                            .child(
                                v_flex()
                                    .gap_1()
                                    .text_sm()
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(cx.theme().text_muted)
                                            .child(SharedString::from("Client:")),
                                    )
                                    .child(
                                        div()
                                            .h_7()
                                            .w_full()
                                            .px_2()
                                            .rounded(cx.theme().radius)
                                            .bg(cx.theme().elevated_surface_background)
                                            .child(request.client_name()),
                                    ),
                            ),
                    )
                    .into_any_element()
            })
            .action(move |_this, _window, _cx| {
                let view = entity.clone();
                let event = event.clone();

                Button::new("approve")
                    .label("Approve")
                    .small()
                    .primary()
                    .loading(loading.get())
                    .disabled(loading.get())
                    .on_click({
                        let loading = Rc::clone(&loading);
                        move |_ev, window, cx| {
                            // Set loading state to true
                            loading.set(true);
                            // Process to approve the request
                            view.update(cx, |this, cx| {
                                this.approve(&event, window, cx);
                            })
                            .ok();
                        }
                    })
            })
    }
}

struct DeviceNotification;

/// Verify the author of an event
async fn verify_author(client: &Client, event: &Event) -> bool {
    if let Some(signer) = client.signer()
        && let Ok(public_key) = signer.get_public_key().await
    {
        return public_key == event.pubkey;
    }
    false
}

/// Encrypt and store device keys in the local database.
async fn set_keys(client: &Client, secret: &str) -> Result<(), Error> {
    let signer = client.signer().context("Signer not found")?;
    let public_key = signer.get_public_key().await?;

    // Encrypt the value
    let content = signer.nip44_encrypt(&public_key, secret).await?;

    // Construct the application data event
    let event = EventBuilder::new(Kind::ApplicationSpecificData, content)
        .tag(Tag::identifier(IDENTIFIER))
        .build(public_key)
        .sign(&Keys::generate())
        .await?;

    // Save the event to the database
    client.database().save_event(&event).await?;

    Ok(())
}

/// Get device keys from the local database.
async fn get_keys(client: &Client) -> Result<Keys, Error> {
    let signer = client.signer().context("Signer not found")?;
    let public_key = signer.get_public_key().await?;

    let filter = Filter::new()
        .kind(Kind::ApplicationSpecificData)
        .identifier(IDENTIFIER)
        .author(public_key);

    if let Some(event) = client.database().query(filter).await?.first() {
        let content = signer.nip44_decrypt(&public_key, &event.content).await?;
        let secret = SecretKey::parse(&content)?;
        let keys = Keys::new(secret);

        Ok(keys)
    } else {
        Err(anyhow!("Key not found"))
    }
}
