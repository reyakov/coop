use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::time::Duration;

use anyhow::{anyhow, Context as AnyhowContext, Error};
use gpui::{
    div, App, AppContext, Context, Entity, Global, IntoElement, ParentElement, SharedString,
    Styled, Subscription, Task, Window,
};
use nostr_sdk::prelude::*;
use person::PersonRegistry;
use smallvec::{smallvec, SmallVec};
use state::{
    app_name, Announcement, DeviceState, NostrRegistry, RelayState, DEVICE_GIFTWRAP, TIMEOUT,
};
use theme::ActiveTheme;
use ui::avatar::Avatar;
use ui::button::{Button, ButtonVariants};
use ui::notification::Notification;
use ui::{h_flex, v_flex, Disableable, IconName, Sizable, WindowExtension};

const IDENTIFIER: &str = "coop:device";
const MSG: &str = "You've requested an encryption key from another device. \
                   Approve to allow Coop to share with it.";

pub fn init(window: &mut Window, cx: &mut App) {
    DeviceRegistry::set_global(cx.new(|cx| DeviceRegistry::new(window, cx)), cx);
}

struct GlobalDeviceRegistry(Entity<DeviceRegistry>);

impl Global for GlobalDeviceRegistry {}

/// Device Registry
///
/// NIP-4e: https://github.com/nostr-protocol/nips/blob/per-device-keys/4e.md
#[derive(Debug)]
pub struct DeviceRegistry {
    /// Device state
    state: DeviceState,

    /// Async tasks
    tasks: Vec<Task<Result<(), Error>>>,

    /// Subscriptions
    _subscriptions: SmallVec<[Subscription; 1]>,
}

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
        let mut subscriptions = smallvec![];

        subscriptions.push(
            // Observe the NIP-65 state
            cx.observe(&nostr, |this, state, cx| {
                if state.read(cx).relay_list_state() == RelayState::Configured {
                    this.get_announcement(cx);
                };
            }),
        );

        // Run at the end of current cycle
        cx.defer_in(window, |this, window, cx| {
            this.handle_notifications(window, cx);
        });

        Self {
            state: DeviceState::default(),
            tasks: vec![],
            _subscriptions: subscriptions,
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

        self.tasks.push(
            // Update GPUI states
            cx.spawn_in(window, async move |this, cx| {
                while let Ok(event) = rx.recv_async().await {
                    match event.kind {
                        // New request event
                        Kind::Custom(4454) => {
                            this.update_in(cx, |this, window, cx| {
                                this.ask_for_approval(event, window, cx);
                            })?;
                        }
                        // New response event
                        Kind::Custom(4455) => {
                            this.update(cx, |this, cx| {
                                this.extract_encryption(event, cx);
                            })?;
                        }
                        _ => {}
                    }
                }

                Ok(())
            }),
        );
    }

    /// Get the device state
    pub fn state(&self) -> DeviceState {
        self.state.clone()
    }

    /// Set the device state
    fn set_state(&mut self, state: DeviceState, cx: &mut Context<Self>) {
        self.state = state;
        cx.notify();
    }

    /// Set the decoupled encryption key for the current user
    pub fn set_signer<S>(&mut self, new: S, cx: &mut Context<Self>)
    where
        S: NostrSigner + 'static,
    {
        let nostr = NostrRegistry::global(cx);
        let signer = nostr.read(cx).signer();

        self.tasks.push(cx.spawn(async move |this, cx| {
            signer.set_encryption_signer(new).await;

            // Update state
            this.update(cx, |this, cx| {
                this.set_state(DeviceState::Set, cx);
                this.get_messages(cx);
            })?;

            Ok(())
        }));
    }

    /// Reset the device state
    fn reset(&mut self, cx: &mut Context<Self>) {
        self.state = DeviceState::Idle;
        cx.notify();
    }

    /// Get all messages for encryption keys
    fn get_messages(&mut self, cx: &mut Context<Self>) {
        let task = self.subscribe_to_giftwrap_events(cx);

        self.tasks.push(cx.spawn(async move |_this, _cx| {
            task.await?;

            // Update state

            Ok(())
        }));
    }

    /// Continuously get gift wrap events for the current user in their messaging relays
    fn subscribe_to_giftwrap_events(&mut self, cx: &mut Context<Self>) -> Task<Result<(), Error>> {
        let nostr = NostrRegistry::global(cx);
        let client = nostr.read(cx).client();

        let signer = nostr.read(cx).signer();
        let public_key = signer.public_key().unwrap();

        let persons = PersonRegistry::global(cx);
        let profile = persons.read(cx).get(&public_key, cx);
        let relay_urls = profile.messaging_relays().clone();

        cx.background_spawn(async move {
            let filter = Filter::new().kind(Kind::GiftWrap).pubkey(public_key);
            let id = SubscriptionId::new(DEVICE_GIFTWRAP);

            // Construct target for subscription
            let target: HashMap<RelayUrl, Filter> = relay_urls
                .into_iter()
                .map(|relay| (relay, filter.clone()))
                .collect();

            let output = client.subscribe(target).with_id(id).await?;

            log::info!(
                "Successfully subscribed to encryption gift-wrap messages on: {:?}",
                output.success
            );

            Ok(())
        })
    }

    /// Get device announcement for current user
    pub fn get_announcement(&mut self, cx: &mut Context<Self>) {
        let nostr = NostrRegistry::global(cx);
        let client = nostr.read(cx).client();

        let signer = nostr.read(cx).signer();
        let public_key = signer.public_key().unwrap();

        // Reset state before fetching announcement
        self.reset(cx);

        // Get user's write relays
        let write_relays = nostr.read(cx).write_relays(&public_key, cx);

        let task: Task<Result<Event, Error>> = cx.background_spawn(async move {
            let urls = write_relays.await;

            // Construct the filter for the device announcement event
            let filter = Filter::new()
                .kind(Kind::Custom(10044))
                .author(public_key)
                .limit(1);

            // Construct target for subscription
            let target: HashMap<&RelayUrl, Filter> =
                urls.iter().map(|relay| (relay, filter.clone())).collect();

            // Stream events from user's write relays
            let mut stream = client
                .stream_events(target)
                .timeout(Duration::from_secs(TIMEOUT))
                .await?;

            while let Some((_url, res)) = stream.next().await {
                match res {
                    Ok(event) => {
                        log::info!("Received device announcement event: {event:?}");
                        return Ok(event);
                    }
                    Err(e) => {
                        log::error!("Failed to receive device announcement event: {e}");
                    }
                }
            }

            Err(anyhow!("Device announcement not found"))
        });

        self.tasks.push(cx.spawn(async move |this, cx| {
            match task.await {
                Ok(event) => {
                    this.update(cx, |this, cx| {
                        this.new_signer(&event, cx);
                    })?;
                }
                Err(_) => {
                    this.update(cx, |this, cx| {
                        this.announce(cx);
                    })?;
                }
            }

            Ok(())
        }));
    }

    /// Create new encryption keys
    pub fn create_encryption(&self, cx: &App) -> Task<Result<Keys, Error>> {
        let nostr = NostrRegistry::global(cx);
        let client = nostr.read(cx).client();

        // Get current user
        let signer = nostr.read(cx).signer();
        let public_key = signer.public_key().unwrap();

        // Get user's write relays
        let write_relays = nostr.read(cx).write_relays(&public_key, cx);

        let keys = Keys::generate();
        let secret = keys.secret_key().to_secret_hex();
        let n = keys.public_key();

        cx.background_spawn(async move {
            let urls = write_relays.await;

            // Construct an announcement event
            let event = client
                .sign_event_builder(EventBuilder::new(Kind::Custom(10044), "").tags(vec![
                    Tag::custom(TagKind::custom("n"), vec![n]),
                    Tag::client(app_name()),
                ]))
                .await?;

            // Publish announcement
            client.send_event(&event).to(urls).await?;

            // Save device keys to the database
            set_keys(&client, &secret).await?;

            Ok(keys)
        })
    }

    /// Create a new device signer and announce it
    fn announce(&mut self, cx: &mut Context<Self>) {
        let task = self.create_encryption(cx);

        self.tasks.push(cx.spawn(async move |this, cx| {
            let keys = task.await?;

            // Update signer
            this.update(cx, |this, cx| {
                this.set_signer(keys, cx);
                this.listen_request(cx);
            })?;

            Ok(())
        }));
    }

    /// Initialize device signer (decoupled encryption key) for the current user
    pub fn new_signer(&mut self, event: &Event, cx: &mut Context<Self>) {
        let nostr = NostrRegistry::global(cx);
        let client = nostr.read(cx).client();

        let announcement = Announcement::from(event);
        let device_pubkey = announcement.public_key();

        let task: Task<Result<Keys, Error>> = cx.background_spawn(async move {
            if let Ok(keys) = get_keys(&client).await {
                if keys.public_key() != device_pubkey {
                    return Err(anyhow!("Key mismatch"));
                };

                Ok(keys)
            } else {
                Err(anyhow!("Key not found"))
            }
        });

        self.tasks.push(cx.spawn(async move |this, cx| {
            match task.await {
                Ok(keys) => {
                    this.update(cx, |this, cx| {
                        this.set_signer(keys, cx);
                        this.listen_request(cx);
                    })?;
                }
                Err(e) => {
                    log::warn!("Failed to initialize device signer: {e}");
                    this.update(cx, |this, cx| {
                        this.request(cx);
                        this.listen_approval(cx);
                    })?;
                }
            };

            Ok(())
        }));
    }

    /// Listen for device key requests on user's write relays
    pub fn listen_request(&mut self, cx: &mut Context<Self>) {
        let nostr = NostrRegistry::global(cx);
        let client = nostr.read(cx).client();

        let signer = nostr.read(cx).signer();
        let public_key = signer.public_key().unwrap();

        let write_relays = nostr.read(cx).write_relays(&public_key, cx);

        let task: Task<Result<(), Error>> = cx.background_spawn(async move {
            let urls = write_relays.await;

            // Construct a filter for device key requests
            let filter = Filter::new()
                .kind(Kind::Custom(4454))
                .author(public_key)
                .since(Timestamp::now());

            // Construct target for subscription
            let target: HashMap<&RelayUrl, Filter> =
                urls.iter().map(|relay| (relay, filter.clone())).collect();

            // Subscribe to the device key requests on user's write relays
            client.subscribe(target).await?;

            Ok(())
        });

        task.detach();
    }

    /// Listen for device key approvals on user's write relays
    fn listen_approval(&mut self, cx: &mut Context<Self>) {
        let nostr = NostrRegistry::global(cx);
        let client = nostr.read(cx).client();

        let signer = nostr.read(cx).signer();
        let public_key = signer.public_key().unwrap();

        let write_relays = nostr.read(cx).write_relays(&public_key, cx);

        self.tasks.push(cx.background_spawn(async move {
            let urls = write_relays.await;

            // Construct a filter for device key requests
            let filter = Filter::new()
                .kind(Kind::Custom(4455))
                .author(public_key)
                .since(Timestamp::now());

            // Construct target for subscription
            let target: HashMap<&RelayUrl, Filter> =
                urls.iter().map(|relay| (relay, filter.clone())).collect();

            // Subscribe to the device key requests on user's write relays
            client.subscribe(target).await?;

            Ok(())
        }));
    }

    /// Request encryption keys from other device
    fn request(&mut self, cx: &mut Context<Self>) {
        let nostr = NostrRegistry::global(cx);
        let client = nostr.read(cx).client();

        let signer = nostr.read(cx).signer();
        let public_key = signer.public_key().unwrap();

        let write_relays = nostr.read(cx).write_relays(&public_key, cx);

        let app_keys = nostr.read(cx).app_keys().clone();
        let app_pubkey = app_keys.public_key();

        let task: Task<Result<Option<Keys>, Error>> = cx.background_spawn(async move {
            let public_key = signer.get_public_key().await?;

            let filter = Filter::new()
                .kind(Kind::Custom(4455))
                .author(public_key)
                .pubkey(app_pubkey)
                .limit(1);

            match client.database().query(filter).await?.first_owned() {
                Some(event) => {
                    let root_device = event
                        .tags
                        .find(TagKind::custom("P"))
                        .and_then(|tag| tag.content())
                        .and_then(|content| PublicKey::parse(content).ok())
                        .context("Invalid event's tags")?;

                    let payload = event.content.as_str();
                    let decrypted = app_keys.nip44_decrypt(&root_device, payload).await?;

                    let secret = SecretKey::from_hex(&decrypted)?;
                    let keys = Keys::new(secret);

                    Ok(Some(keys))
                }
                None => {
                    let urls = write_relays.await;

                    // Construct an event for device key request
                    let event = client
                        .sign_event_builder(EventBuilder::new(Kind::Custom(4454), "").tags(vec![
                            Tag::client(app_name()),
                            Tag::custom(TagKind::custom("P"), vec![app_pubkey]),
                        ]))
                        .await?;

                    // Send the event to write relays
                    client.send_event(&event).to(urls).await?;

                    Ok(None)
                }
            }
        });

        self.tasks.push(cx.spawn(async move |this, cx| {
            match task.await {
                Ok(Some(keys)) => {
                    this.update(cx, |this, cx| {
                        this.set_signer(keys, cx);
                    })?;
                }
                Ok(None) => {
                    this.update(cx, |this, cx| {
                        this.set_state(DeviceState::Requesting, cx);
                    })?;
                }
                Err(e) => {
                    log::error!("Failed to request the encryption key: {e}");
                }
            };

            Ok(())
        }));
    }

    /// Parse the response event for device keys from other devices
    fn extract_encryption(&mut self, event: Event, cx: &mut Context<Self>) {
        let nostr = NostrRegistry::global(cx);
        let app_keys = nostr.read(cx).app_keys().clone();

        let task: Task<Result<Keys, Error>> = cx.background_spawn(async move {
            let root_device = event
                .tags
                .find(TagKind::custom("P"))
                .and_then(|tag| tag.content())
                .and_then(|content| PublicKey::parse(content).ok())
                .context("Invalid event's tags")?;

            let payload = event.content.as_str();
            let decrypted = app_keys.nip44_decrypt(&root_device, payload).await?;

            let secret = SecretKey::from_hex(&decrypted)?;
            let keys = Keys::new(secret);

            Ok(keys)
        });

        self.tasks.push(cx.spawn(async move |this, cx| {
            let keys = task.await?;

            // Update signer
            this.update(cx, |this, cx| {
                this.set_signer(keys, cx);
            })?;

            Ok(())
        }));
    }

    /// Approve requests for device keys from other devices
    fn approve(&mut self, event: &Event, window: &mut Window, cx: &mut Context<Self>) {
        let nostr = NostrRegistry::global(cx);
        let client = nostr.read(cx).client();

        // Get current user
        let signer = nostr.read(cx).signer();
        let public_key = signer.public_key().unwrap();

        // Get user's write relays
        let write_relays = nostr.read(cx).write_relays(&public_key, cx);
        let event = event.clone();
        let id: SharedString = event.id.to_hex().into();

        let task: Task<Result<(), Error>> = cx.background_spawn(async move {
            let urls = write_relays.await;

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
            client.send_event(&event).to(urls).await?;

            Ok(())
        });

        cx.spawn_in(window, async move |_this, cx| {
            match task.await {
                Ok(_) => {
                    cx.update(|window, cx| {
                        window.clear_notification(id, cx);
                    })
                    .ok();
                }
                Err(e) => {
                    cx.update(|window, cx| {
                        window.push_notification(Notification::error(e.to_string()), cx);
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

        Notification::new()
            .custom_id(SharedString::from(event.id.to_hex()))
            .autohide(false)
            .icon(IconName::UserKey)
            .title(SharedString::from("New request"))
            .content(move |_window, cx| {
                v_flex()
                    .gap_2()
                    .text_sm()
                    .child(SharedString::from(MSG))
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
            .action(move |_window, _cx| {
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

/// Verify the author of an event
async fn verify_author(client: &Client, event: &Event) -> bool {
    if let Some(signer) = client.signer() {
        if let Ok(public_key) = signer.get_public_key().await {
            return public_key == event.pubkey;
        }
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
