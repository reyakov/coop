use std::cell::Cell;
use std::collections::HashSet;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Context as AnyhowContext, Error, anyhow};
use gpui::{
    App, AppContext, Context, Entity, EventEmitter, Global, IntoElement, ParentElement,
    SharedString, Styled, Subscription, Task, Window, div, relative,
};
use nostr_sdk::prelude::*;
use person::PersonRegistry;
use settings::AppSettings;
use smallvec::{SmallVec, smallvec};
use state::{Announcement, CLIENT_NAME, NostrRegistry, UniversalSigner};
use theme::ActiveTheme;
use ui::avatar::Avatar;
use ui::button::Button;
use ui::notification::{Notification, NotificationKind};
use ui::{Disableable, Sizable, StyledExt, WindowExtension, h_flex, v_flex};

const IDENTIFIER: &str = "coop:device";

pub fn init(cx: &mut App) {
    DeviceRegistry::set_global(cx.new(DeviceRegistry::new), cx);
}

struct GlobalDeviceRegistry(Entity<DeviceRegistry>);

impl Global for GlobalDeviceRegistry {}

/// Device event.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum DeviceEvent {
    /// A new encryption signer has been set
    Set,
    /// User have not setup encryption key
    NotSet,
    /// The device is requesting an encryption key
    Requesting(SharedString),
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
}

/// Device Registry
///
/// NIP-4e: https://github.com/nostr-protocol/nips/blob/per-device-keys/4e.md
#[derive(Debug)]
pub struct DeviceRegistry {
    signer: Entity<Option<UniversalSigner>>,
    /// Whether there is a pending request for encryption key approval
    pub pending_request: bool,
    /// Whether an announcement has been made for this device
    pub announcement_existed: Arc<AtomicBool>,
    /// Relay notification listener
    notification_listener: Option<Task<Result<(), Error>>>,
    tasks: Vec<Task<Result<(), Error>>>,
    _subscriptions: SmallVec<[Subscription; 2]>,
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
    fn new(cx: &mut Context<Self>) -> Self {
        let entity = cx.entity().downgrade();
        let nostr = NostrRegistry::global(cx);
        let settings = AppSettings::global(cx);

        let signer = cx.new(|_| None);
        let mut subscriptions = smallvec![];

        subscriptions.push(
            // Subscribe to nostr state events
            cx.observe(&settings, move |this, settings, cx| {
                if settings.read(cx).is_nip4e_enabled(cx) {
                    this.get_announcement(cx);
                };
            }),
        );

        subscriptions.push(
            // Observe the user signer
            cx.subscribe(&nostr, move |this, _nostr, event, cx| {
                if event.signer_changed() && settings.read(cx).is_nip4e_enabled(cx) {
                    // The old account's state must not leak into the new one
                    this.announcement_existed.store(false, Ordering::Relaxed);
                    this.handle_notifications(cx);
                    this.get_announcement(cx);
                }
            }),
        );

        cx.defer(move |cx| {
            entity
                .update(cx, |this, cx| this.handle_notifications(cx))
                .ok();
        });

        Self {
            signer,
            pending_request: false,
            announcement_existed: Arc::new(AtomicBool::new(false)),
            notification_listener: None,
            tasks: vec![],
            _subscriptions: subscriptions,
        }
    }

    fn handle_notifications(&mut self, cx: &mut Context<Self>) {
        self.notification_listener = None;

        let nostr = NostrRegistry::global(cx);
        let client = nostr.read(cx).client();
        let signer = nostr.read(cx).signer();

        let announcement_existed = self.announcement_existed.clone();
        let (tx, rx) = flume::bounded::<Event>(100);

        self.notification_listener = Some(cx.background_spawn(async move {
            let mut notifications = client.notifications();
            let mut processed_events = HashSet::new();
            let current_user = signer.get_public_key_async().await.ok();

            while let Some(notification) = notifications.next().await {
                if let ClientNotification::Message { message, .. } = notification
                    && let RelayMessage::Event { event, .. } = *message
                {
                    if !processed_events.insert(event.id) {
                        // Skip if the event has already been processed
                        continue;
                    }

                    match event.kind {
                        Kind::Custom(10044) if current_user == Some(event.pubkey) => {
                            announcement_existed.store(true, Ordering::Relaxed);
                            tx.send_async(event.into_owned()).await?;
                        }
                        Kind::Custom(4454) if current_user == Some(event.pubkey) => {
                            tx.send_async(event.into_owned()).await?;
                        }
                        Kind::Custom(4455) if current_user == Some(event.pubkey) => {
                            tx.send_async(event.into_owned()).await?;
                        }
                        _ => {}
                    }
                }
            }

            Ok(())
        }));

        self.tasks.push(cx.spawn(async move |this, cx| {
            while let Ok(event) = rx.recv_async().await {
                match event.kind {
                    Kind::Custom(10044) => {
                        this.update(cx, |this, cx| {
                            this.set_encryption(&event, cx);
                        })?;
                    }
                    // New request event from other device
                    Kind::Custom(4454) => {
                        this.update(cx, |this, cx| {
                            this.ask_for_approval(event, cx);
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

    /// Set whether there is a pending request for encryption key approval
    fn set_pending_request(&mut self, pending: bool, cx: &mut Context<Self>) {
        self.pending_request = pending;
        cx.notify();
    }

    /// Get the signer
    pub fn signer(&self, cx: &App) -> Option<UniversalSigner> {
        self.signer.read(cx).clone()
    }

    /// Set the decoupled encryption key for the current user
    fn set_signer(&mut self, new_signer: Keys, cx: &mut Context<Self>) {
        self.signer.update(cx, |this, cx| {
            *this = Some(UniversalSigner::new(new_signer));
            cx.notify();
        });
        cx.emit(DeviceEvent::Set);
    }

    /// Backup the encryption's secret key to a file
    pub fn backup(&self, path: PathBuf, cx: &App) -> Task<Result<(), Error>> {
        let nostr = NostrRegistry::global(cx);
        let client = nostr.read(cx).client();
        let signer = nostr.read(cx).signer();

        cx.background_spawn(async move {
            let keys = get_keys(&client, &signer).await?;
            let content = keys.secret_key().to_bech32()?;

            if cfg!(target_arch = "wasm32") {
                return Err(anyhow!("Not supported"));
            }

            #[cfg(not(target_arch = "wasm32"))]
            smol::fs::write(path, &content).await?;

            Ok(())
        })
    }

    /// Get device announcement for current user
    pub fn get_announcement(&mut self, cx: &mut Context<Self>) {
        let nostr = NostrRegistry::global(cx);
        let client = nostr.read(cx).client();
        let signer = nostr.read(cx).signer();

        let announcement_existed = self.announcement_existed.clone();

        self.tasks.push(cx.spawn(async move |this, cx| {
            let task: Task<Result<(), Error>> = cx.background_spawn(async move {
                let opts =
                    SubscribeAutoCloseOptions::default().exit_policy(ReqExitPolicy::ExitOnEOSE);
                let current_user = signer.get_public_key_async().await?;

                // Construct the filter for the device announcement event
                let filter = Filter::new()
                    .kind(Kind::Custom(10044))
                    .author(current_user)
                    .limit(1);

                client
                    .subscribe(filter)
                    .close_on(opts)
                    .with_id(SubscriptionId::new("nip4e"))
                    .await?;

                Ok(())
            });

            if let Err(e) = task.await {
                this.update(cx, |_this, cx| {
                    cx.emit(DeviceEvent::error(e.to_string()));
                })?;
                return Ok(());
            }

            if !announcement_existed.load(Ordering::Acquire) {
                this.update(cx, |_this, cx| {
                    cx.emit(DeviceEvent::NotSet);
                })?;
            }

            Ok(())
        }));
    }

    /// Create a new device signer and announce it to user's relay list
    pub fn set_announcement(&mut self, keys: Keys, cx: &mut Context<Self>) {
        let task = self.create_encryption(keys, cx);

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
        let signer = nostr.read(cx).signer();

        let secret = keys.secret_key().to_secret_hex();
        let n = keys.public_key();

        cx.background_spawn(async move {
            // Construct an announcement event
            let event = EventBuilder::new(Kind::Custom(10044), "")
                .tag(Tag::custom("n", vec![n]))
                .finalize_async(&signer)
                .await?;

            // Persist the key locally before announcing it
            set_keys(&client, &signer, &secret).await?;

            // Publish announcement
            client
                .send_event(&event)
                .to_nip65()
                .ack_policy(AckPolicy::none())
                .await?;

            Ok(keys)
        })
    }

    /// Set encryption key from the announcement event
    fn set_encryption(&mut self, event: &Event, cx: &mut Context<Self>) {
        let Some(announcement) = Announcement::from_event(event) else {
            log::warn!("Ignoring malformed encryption announcement");
            return;
        };

        let nostr = NostrRegistry::global(cx);
        let client = nostr.read(cx).client();
        let signer = nostr.read(cx).signer();

        let device_pubkey = announcement.public_key();

        // Get encryption key from the database and compare with the announcement
        let task: Task<Result<Keys, Error>> = cx.background_spawn(async move {
            let keys = get_keys(&client, &signer).await?;

            // Compare the public key from the announcement with the one from the database
            if keys.public_key() != device_pubkey {
                return Err(anyhow!("Encryption Key doesn't match the announcement"));
            };

            Ok(keys)
        });

        self.tasks.push(cx.spawn(async move |this, cx| {
            if let Ok(keys) = task.await {
                this.update(cx, |this, cx| {
                    this.set_signer(keys, cx);
                    this.wait_for_request(cx);
                })?;
            } else {
                this.update(cx, |this, cx| {
                    this.request(cx);
                })?;
            }
            Ok(())
        }));
    }

    /// Wait for encryption key requests from now on
    fn wait_for_request(&mut self, cx: &mut Context<Self>) {
        let nostr = NostrRegistry::global(cx);
        let client = nostr.read(cx).client();
        let signer = nostr.read(cx).signer();

        self.tasks.push(cx.background_spawn(async move {
            let public_key = signer.get_public_key_async().await?;
            let id = SubscriptionId::new("dekey-requests");

            // Construct a filter for encryption key requests.
            let filter = Filter::new().kind(Kind::Custom(4454)).author(public_key);

            // Subscribe to the device key requests on user's write relays
            client.subscribe(vec![filter]).with_id(id).await?;

            Ok(())
        }));
    }

    /// Request encryption keys from other device
    pub fn request(&mut self, cx: &mut Context<Self>) {
        let nostr = NostrRegistry::global(cx);
        let client = nostr.read(cx).client();
        let signer = nostr.read(cx).signer();

        let app_keys_task = get_or_init_app_keys(cx);

        let task: Task<Result<Option<Event>, Error>> = cx.background_spawn(async move {
            let app_keys = app_keys_task.await?;
            let app_pubkey = app_keys.public_key();
            let public_key = signer.get_public_key_async().await?;

            // Construct a filter to get the latest approval event
            let filter = Filter::new()
                .kind(Kind::Custom(4455))
                .author(public_key)
                .pubkey(app_pubkey)
                .limit(1);

            if let Some(event) = client.database().query(filter).await?.into_iter().next() {
                // Only reuse the stored approval if its key is still valid,
                // otherwise it's stale and a fresh request is needed
                if decrypt_approval(&client, &app_keys, public_key, &event)
                    .await
                    .is_ok()
                {
                    return Ok(Some(event));
                }
            }

            // No usable approval event found, construct a request event
            let event = EventBuilder::new(Kind::Custom(4454), "")
                .tags(vec![Tag::custom("P", vec![app_pubkey])])
                .finalize_async(&signer)
                .await?;

            // Send the event to write relays
            client.send_event(&event).to_nip65().await?;

            Ok(None)
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
                        this.wait_for_approval(cx);
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
        let app_keys_task = get_or_init_app_keys(cx);

        self.tasks.push(cx.spawn(async move |this, cx| {
            let app_keys = match app_keys_task.await {
                Ok(app_keys) => app_keys,
                Err(e) => {
                    this.update(cx, |_this, cx| {
                        cx.emit(DeviceEvent::error(e.to_string()));
                    })?;
                    return Ok(());
                }
            };

            this.update(cx, |_this, cx| {
                cx.emit(DeviceEvent::Requesting(authorization_code(
                    &app_keys.public_key(),
                )));
            })?;

            let task: Task<Result<(), Error>> = cx.background_spawn(async move {
                let public_key = signer.get_public_key_async().await?;

                // Only approvals addressed to this device's app key matter
                let filter = Filter::new()
                    .kind(Kind::Custom(4455))
                    .author(public_key)
                    .pubkey(app_keys.public_key());

                // Subscribe to the device key approvals on user's write relays
                client.subscribe(filter).await?;

                Ok(())
            });

            if let Err(e) = task.await {
                this.update(cx, |_this, cx| {
                    cx.emit(DeviceEvent::error(e.to_string()));
                })?;
            }

            Ok(())
        }));
    }

    /// Parse the approval event to get encryption key then set it
    fn extract_encryption(&mut self, event: Event, cx: &mut Context<Self>) {
        let app_keys_task = get_or_init_app_keys(cx);
        let nostr = NostrRegistry::global(cx);
        let client = nostr.read(cx).client();
        let signer = nostr.read(cx).signer();

        let approval_id = event.id;

        let task: Task<Result<Keys, Error>> = cx.background_spawn(async move {
            let app_keys = app_keys_task.await?;
            let identity = signer.get_public_key_async().await?;
            let keys = decrypt_approval(&client, &app_keys, identity, &event).await?;

            Ok(keys)
        });

        self.tasks.push(cx.spawn(async move |this, cx| {
            match task.await {
                Ok(keys) => {
                    this.update(cx, |this, cx| {
                        this.set_signer(keys, cx);
                        // Clean up our request and the approval event from relays
                        this.cleanup_sync_events(approval_id, cx);
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

    /// Delete our key request and the consumed approval event from relays
    fn cleanup_sync_events(&mut self, approval_id: EventId, cx: &mut Context<Self>) {
        let nostr = NostrRegistry::global(cx);
        let client = nostr.read(cx).client();
        let signer = nostr.read(cx).signer();

        self.tasks.push(cx.background_spawn(async move {
            let result: Result<(), Error> = async {
                let public_key = signer.get_public_key_async().await?;

                // Collect our key request events
                let filter = Filter::new().kind(Kind::Custom(4454)).author(public_key);

                let mut ids: Vec<EventId> = client
                    .database()
                    .query(filter)
                    .await?
                    .into_iter()
                    .map(|event| event.id)
                    .collect();
                ids.push(approval_id);

                let deletion = EventBuilder::new(Kind::EventDeletion, "")
                    .tags(Tags::from_list(ids.into_iter().map(Tag::event).collect()))
                    .finalize_async(&signer)
                    .await?;

                client.send_event(&deletion).to_nip65().await?;

                Ok(())
            }
            .await;

            if let Err(e) = result {
                log::error!("Failed to delete sync events: {e}");
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
            let keys = get_keys(&client, &signer).await?;
            let secret = keys.secret_key().to_secret_hex();

            // Extract the target public key from the event tags
            let target = event
                .tags
                .iter()
                .find(|tag| tag.kind() == "P")
                .and_then(|tag| tag.content())
                .and_then(|content| PublicKey::parse(content).ok())
                .context("Target is not a valid public key")?;

            // Encrypt the device keys with the user's signer
            let payload = keys.nip44_encrypt_async(&target, &secret).await?;

            // Construct the response event
            //
            // P tag: the current device's public key
            // p tag: the requester's public key
            let event = EventBuilder::new(Kind::Custom(4455), payload)
                .tags(vec![
                    Tag::custom("P", vec![keys.public_key().to_hex()]),
                    Tag::public_key(target),
                ])
                .finalize_async(&signer)
                .await?;

            // Send the response event to the user's relay list
            client.send_event(&event).to_nip65().await?;

            Ok(())
        });

        self.tasks.push(cx.spawn_in(window, async move |this, cx| {
            match task.await {
                Ok(_) => {
                    this.update(cx, |this, cx| {
                        // Allow future requests to surface again
                        this.set_pending_request(false, cx);
                    })?;

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

            Ok(())
        }));
    }

    /// Handle encryption request
    fn ask_for_approval(&mut self, event: Event, cx: &mut Context<Self>) {
        // Ignore if there is already a pending request
        if self.pending_request {
            return;
        }
        self.set_pending_request(true, cx);

        let notification = match self.notification(event, cx) {
            Ok(notification) => notification,
            Err(e) => {
                log::warn!("Ignoring invalid encryption key request: {e}");
                self.set_pending_request(false, cx);
                return;
            }
        };

        if let Some(window) = cx.windows().first().copied() {
            if let Err(error) = window.update(cx, |_view, window, cx| {
                window.push_notification(notification, cx);
            }) {
                log::warn!("Failed to show encryption key request: {error}");
            }
        } else {
            log::warn!("Failed to show encryption key request: no open window");
        }
    }

    /// Build a notification for the encryption request.
    fn notification(&self, event: Event, cx: &Context<Self>) -> Result<Notification, Error> {
        const MSG: &str = "You've requested an encryption key from another device. \
                           Approve to allow Coop to share with it. Before approving, \
                           compare the code with the one shown on the requesting \
                           device.";

        let requester = event
            .tags
            .iter()
            .find(|tag| tag.kind() == "P")
            .and_then(|tag| tag.content())
            .and_then(|content| PublicKey::parse(content).ok())
            .context("Request has no valid requester key")?;

        let code = authorization_code(&requester);
        let persons = PersonRegistry::global(cx);
        let profile = persons.read(cx).get(&requester, cx);
        let code_label = code.clone();

        let entity = cx.entity().downgrade();
        let loading = Rc::new(Cell::new(false));
        let key = SharedString::from(event.id.to_hex());

        Ok(Notification::new()
            .type_id::<DeviceNotification>(key)
            .autohide(false)
            .with_kind(NotificationKind::Info)
            .title("Encryption Key Request")
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
                                            .font_semibold()
                                            .text_xs()
                                            .text_color(cx.theme().text_muted)
                                            .child(SharedString::from("From:")),
                                    )
                                    .child(
                                        div()
                                            .h_8()
                                            .w_full()
                                            .px_2()
                                            .rounded(cx.theme().radius)
                                            .bg(cx.theme().elevated_surface_background)
                                            .child(
                                                h_flex()
                                                    .gap_2()
                                                    .child(
                                                        Avatar::new(profile.avatar())
                                                            .seed(profile.avatar_seed())
                                                            .xsmall(),
                                                    )
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
                                            .font_semibold()
                                            .text_xs()
                                            .text_color(cx.theme().text_muted)
                                            .child(SharedString::from("Code:")),
                                    )
                                    .child(
                                        div()
                                            .h_8()
                                            .w_full()
                                            .px_2()
                                            .rounded(cx.theme().radius)
                                            .bg(cx.theme().elevated_surface_background)
                                            .child(code_label.clone()),
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
            }))
    }
}

struct DeviceNotification;

/// The first eight hexadecimal characters of the client public key.
fn authorization_code(public_key: &PublicKey) -> SharedString {
    let hex = public_key.to_hex();
    let code = hex[..8].to_uppercase();
    SharedString::from(format!("{} {}", &code[..4], &code[4..]))
}

/// Get or create new app keys (async, returns a task)
fn get_or_init_app_keys(cx: &App) -> Task<Result<Keys, Error>> {
    let read = cx.read_credentials(CLIENT_NAME);

    cx.spawn(async move |cx| {
        if let Ok(Some((_, secret))) = read.await
            && let Ok(keys) = SecretKey::from_slice(&secret).map(Keys::new)
        {
            return Ok(keys);
        }

        // No stored keys found or invalid — generate new ones
        let keys = Keys::generate();
        let user = keys.public_key().to_hex();
        let secret = keys.secret_key().to_secret_bytes();

        cx.update(|cx| {
            let write = cx.write_credentials(CLIENT_NAME, &user, &secret);
            cx.background_spawn(async move {
                if let Err(e) = write.await {
                    log::error!("Keyring not available or panic: {e}")
                }
            })
            .detach();
        });

        Ok(keys)
    })
}

/// Decrypt and validate an approval event against the current announcement.
async fn decrypt_approval(
    client: &Client,
    app_keys: &Keys,
    identity: PublicKey,
    event: &Event,
) -> Result<Keys, Error> {
    let master = event
        .tags
        .iter()
        .find(|tag| tag.kind() == "P")
        .and_then(|tag| tag.content())
        .and_then(|content| PublicKey::parse(content).ok())
        .context("Invalid event's tags")?;

    let decrypted = app_keys
        .nip44_decrypt_async(&master, event.content.as_str())
        .await?;

    let secret = SecretKey::from_hex(&decrypted)?;
    let keys = Keys::new(secret);

    // Reject approvals carrying a key that no longer matches the published announcement.
    let filter = Filter::new()
        .kind(Kind::Custom(10044))
        .author(identity)
        .limit(1);

    if let Some(announcement) = client.database().query(filter).await?.into_iter().next() {
        let announced = announcement
            .tags
            .iter()
            .find(|tag| tag.kind() == "n")
            .and_then(|tag| tag.content())
            .and_then(|content| PublicKey::parse(content).ok())
            .context("Announcement has no valid encryption key")?;

        if keys.public_key() != announced {
            return Err(anyhow!("Approval key doesn't match the announcement"));
        }
    }

    Ok(keys)
}

/// Encrypt and store device keys in the local database.
async fn set_keys<S>(client: &Client, signer: &S, secret: &str) -> Result<(), Error>
where
    S: AsyncGetPublicKey + AsyncSignEvent + AsyncNip44 + ?Sized,
{
    let public_key = signer.get_public_key_async().await?;
    let content = signer.nip44_encrypt_async(&public_key, secret).await?;

    // Construct the application data event
    let event = EventBuilder::new(Kind::ApplicationSpecificData, content)
        .tag(Tag::identifier(IDENTIFIER))
        .finalize_async(signer)
        .await?;

    // Save the event to the database
    client.database().save_event(&event).await?;

    Ok(())
}

/// Get device keys from the local database.
async fn get_keys<S>(client: &Client, signer: &S) -> Result<Keys, Error>
where
    S: AsyncGetPublicKey + AsyncNip44 + ?Sized,
{
    let public_key = signer.get_public_key_async().await?;

    let filter = Filter::new()
        .kind(Kind::ApplicationSpecificData)
        .identifier(IDENTIFIER)
        .author(public_key);

    if let Some(event) = client.database().query(filter).await?.first() {
        let content = signer
            .nip44_decrypt_async(&public_key, &event.content)
            .await?;

        let secret = SecretKey::parse(&content)?;
        let keys = Keys::new(secret);

        Ok(keys)
    } else {
        Err(anyhow!("Key not found"))
    }
}
