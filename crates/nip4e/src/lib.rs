use std::cell::Cell;
use std::collections::{HashSet, VecDeque};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{Context as AnyhowContext, Error, anyhow};
use gpui::{
    App, AppContext, Context, Entity, EventEmitter, Global, IntoElement, ParentElement,
    SharedString, Styled, Subscription, Task, Window, div, relative,
};
use gpui_kit::base::StyledExt as _;
use gpui_kit::component::button::Button;
use gpui_kit::component::notification::{Notification, NotificationType};
use gpui_kit::component::{ActiveTheme, Disableable, Sizable, WindowExt, h_flex, v_flex};
use nostr_sdk::prelude::*;
use person::Directory;
use settings::{AppSettings, NIP4E_ROTATION_MAX_DAYS, NIP4E_ROTATION_MIN_DAYS};
use smallvec::{SmallVec, smallvec};
use state::{Announcement, NostrRegistry, UniversalSigner, credentials};
use storage::{SECONDS_PER_DAY, archive_key, get_keys, load_device_keys, set_keys};
use ui::Avatar;

mod storage;

/// Keep the durable processed-request log bounded
const PROCESSED_REQUESTS_LIMIT: usize = 256;
/// Ignore key requests older than a day so stale events on relays cannot resurface
const REQUEST_MAX_AGE: u64 = SECONDS_PER_DAY;

pub fn init(cx: &mut App) {
    DeviceRegistry::set_global(cx.new(DeviceRegistry::new), cx);
}

struct DeviceNotification;

/// A key request awaiting user approval on this device
#[derive(Debug)]
struct PendingRequest {
    event: Event,
    client_key: PublicKey,
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
    pub(crate) fn error<T>(error: T) -> Self
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
    /// Ephemeral client key for the in-flight key request; never persisted
    app_keys: Option<Keys>,
    /// The key request currently awaiting user approval on this device
    pending_request: Option<PendingRequest>,
    /// Handled key-sync request ids, bounded and persisted
    processed_requests: VecDeque<EventId>,
    /// Whether an announcement has been made for this device
    pub(crate) announcement_existed: Arc<AtomicBool>,
    /// Whether the local key is stale against the current announcement.
    resync: Arc<AtomicBool>,
    /// Whether a key rotation is in flight, to coalesce overlapping requests
    rotating: bool,
    /// Person directory for requesters shown in the approval dialog
    directory: Entity<Directory>,
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
                    this.resync.store(false, Ordering::Relaxed);
                    // The in-flight request and client key belong to the old account
                    this.app_keys = None;

                    if this.pending_request.take().is_some() {
                        // The old approval must not target the new account's signer
                        if let Some(window) = cx.windows().first().copied() {
                            window
                                .update(cx, |_view, window, cx| {
                                    window.remove_notification::<DeviceNotification>(cx);
                                })
                                .ok();
                        }
                    }

                    this.handle_notifications(cx);
                    this.get_announcement(cx);
                }
            }),
        );

        cx.defer(move |cx| {
            let _ = entity.update(cx, |this, cx| {
                this.handle_notifications(cx);
            });
        });

        Self {
            signer,
            app_keys: None,
            pending_request: None,
            processed_requests: VecDeque::new(),
            announcement_existed: Arc::new(AtomicBool::new(false)),
            resync: Arc::new(AtomicBool::new(false)),
            rotating: false,
            directory: Directory::new(cx),
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

        let read = credentials::read_processed_requests(cx);

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
            // Hydrate the processed-request log before routing any events
            match read.await {
                Ok(bytes) => {
                    let processed = bytes
                        .map(|bytes| parse_processed_requests(&bytes))
                        .unwrap_or_default();

                    this.update(cx, |this, _cx| {
                        this.processed_requests = processed;
                    })
                    .ok();
                }
                Err(e) => log::error!("Failed to read processed key-sync requests: {e}"),
            }

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
                            this.handle_transfer(event, cx);
                        })?;
                    }
                    _ => {}
                }
            }
            Ok(())
        }));
    }

    /// Record a handled key-sync request id, persisting a bounded log
    fn mark_request_processed(&mut self, event_id: EventId, cx: &mut Context<Self>) {
        if self.processed_requests.contains(&event_id) {
            return;
        }
        self.processed_requests.push_back(event_id);
        while self.processed_requests.len() > PROCESSED_REQUESTS_LIMIT {
            self.processed_requests.pop_front();
        }

        let bytes: Vec<u8> = self
            .processed_requests
            .iter()
            .flat_map(|id| id.to_hex().into_bytes())
            .collect();
        let write = credentials::write_processed_requests(cx, &bytes);
        cx.background_spawn(async move {
            if let Err(e) = write.await {
                log::error!("Failed to persist processed key-sync requests: {e}");
            }
        })
        .detach();
    }

    /// Withdraw the pending approval.
    fn resolve_pending_request(&mut self, cx: &mut Context<Self>) {
        let Some(pending) = self.pending_request.take() else {
            return;
        };
        self.mark_request_processed(pending.event.id, cx);

        if let Some(window) = cx.windows().first().copied() {
            window
                .update(cx, |_view, window, cx| {
                    window.remove_notification::<DeviceNotification>(cx);
                })
                .ok();
        }
        cx.notify();
    }

    /// Route a key transfer.
    fn handle_transfer(&mut self, event: Event, cx: &mut Context<Self>) {
        let resolved = self
            .pending_request
            .as_ref()
            .is_some_and(|pending| transfer_addresses(&event, pending.client_key));

        if resolved {
            self.resolve_pending_request(cx);
            return;
        }

        self.extract_encryption(event, cx);
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
        // The local key is now consistent with the announcement
        self.resync.store(false, Ordering::Release);
        cx.emit(DeviceEvent::Set);
    }

    /// Whether the local encryption key is stale against the current announcement.
    pub fn is_resyncing(&self) -> bool {
        self.resync.load(Ordering::Acquire)
    }

    /// Backup the encryption's secret key, returning the `nsec1...` content
    pub fn backup_bytes(&self, cx: &App) -> Task<Result<String, Error>> {
        let nostr = NostrRegistry::global(cx);
        let client = nostr.read(cx).client();
        let signer = nostr.read(cx).signer();

        cx.background_spawn(async move {
            let keys = get_keys(&client, &signer).await?;
            Ok(keys.secret_key().to_bech32()?)
        })
    }

    /// Backup the encryption's secret key to a file
    pub fn backup(&self, path: PathBuf, cx: &App) -> Task<Result<(), Error>> {
        let backup = self.backup_bytes(cx);

        #[cfg(not(target_arch = "wasm32"))]
        return cx.background_spawn(async move {
            let content = backup.await?;
            smol::fs::write(path, &content).await?;

            Ok(())
        });

        #[cfg(target_arch = "wasm32")]
        {
            let _ = path;
            cx.background_spawn(async move {
                backup.await?;
                Err(anyhow!("File backups are not supported on web; copy the key from the backup panel instead."))
            })
        }
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

    /// Replace the encryption key with a new one, archiving the old key.
    pub fn rotate(&mut self, cx: &mut Context<Self>) {
        if self.rotating {
            return;
        }
        self.rotating = true;

        let nostr = NostrRegistry::global(cx);
        let client = nostr.read(cx).client();
        let signer = nostr.read(cx).signer();

        self.tasks.push(cx.spawn(async move |this, cx| {
            let result: Result<Keys, Error> = async {
                // Archive the current key before replacing it
                if let Ok(old) = get_keys(&client, &signer).await {
                    archive_key(&client, &signer, &old.secret_key().to_secret_hex()).await?;
                }

                let created = this
                    .update(cx, |this, cx| this.create_encryption(Keys::generate(), cx))?
                    .await?;

                this.update(cx, |this, cx| {
                    this.set_signer(created.clone(), cx);
                    this.wait_for_request(cx);
                })?;

                Ok(created)
            }
            .await;

            this.update(cx, |this, cx| {
                this.rotating = false;
                if let Err(e) = result {
                    cx.emit(DeviceEvent::error(e.to_string()));
                }
            })
            .ok();

            Ok(())
        }));
    }

    /// Rotate the encryption key when the configured interval has elapsed.
    ///
    /// Runs after the announcement reconciled, outside the blocking boot path.
    pub fn rotate_if_due(&mut self, cx: &mut Context<Self>) {
        let Some(interval_days) = AppSettings::get_nip4e_rotation_days(cx) else {
            return;
        };
        let interval_days = interval_days.clamp(NIP4E_ROTATION_MIN_DAYS, NIP4E_ROTATION_MAX_DAYS);

        let nostr = NostrRegistry::global(cx);
        let client = nostr.read(cx).client();
        let signer = nostr.read(cx).signer();

        self.tasks.push(cx.spawn(async move |this, cx| {
            let Ok(keys) = load_device_keys(&client, &signer).await else {
                return Ok(());
            };

            let elapsed = Timestamp::now()
                .as_secs()
                .saturating_sub(keys.created_at.as_secs());
            if elapsed < u64::from(interval_days) * SECONDS_PER_DAY {
                return Ok(());
            }

            this.update(cx, |this, cx| this.rotate(cx)).ok();

            Ok(())
        }));
    }

    /// Load encryption keys retired by a rotation from the local database.
    pub async fn archived_keys<S>(client: &Client, signer: &S) -> Result<Vec<Keys>, Error>
    where
        S: AsyncGetPublicKey + AsyncNip44 + ?Sized,
    {
        storage::archived_keys(client, signer).await
    }

    /// Create new encryption key and announce it to user's relay list
    fn create_encryption(&self, keys: Keys, cx: &App) -> Task<Result<Keys, Error>> {
        let nostr = NostrRegistry::global(cx);
        let client = nostr.read(cx).client();
        let signer = nostr.read(cx).signer();

        let secret = keys.secret_key().to_secret_hex();
        let n = keys.public_key();
        let created_at = Timestamp::now();

        cx.background_spawn(async move {
            // Construct an announcement event
            let event = EventBuilder::new(Kind::Custom(10044), "")
                .tag(Tag::custom("n", vec![n]))
                .finalize_async(&signer)
                .await?;

            // Persist the key locally before announcing it
            set_keys(&client, &signer, &secret, created_at).await?;

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
                    this.rotate_if_due(cx);
                })?;
            } else {
                this.update(cx, |this, cx| {
                    this.resync.store(true, Ordering::Release);
                    this.request(Some(device_pubkey), cx);
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
            let since = Timestamp::now().as_secs().saturating_sub(REQUEST_MAX_AGE);
            let filter = Filter::new()
                .kind(Kind::Custom(4454))
                .author(public_key)
                .since(Timestamp::from_secs(since));

            // Subscribe to the device key requests on user's write relays
            client.subscribe(vec![filter]).with_id(id).await?;

            Ok(())
        }));
    }

    /// Request encryption keys from other device
    pub(crate) fn request(
        &mut self,
        expected_encryption: Option<PublicKey>,
        cx: &mut Context<Self>,
    ) {
        let nostr = NostrRegistry::global(cx);
        let client = nostr.read(cx).client();
        let signer = nostr.read(cx).signer();
        let app_keys = self.app_keys.get_or_insert_with(Keys::generate).clone();

        let task: Task<Result<Option<Event>, Error>> = cx.background_spawn(async move {
            let app_pubkey = app_keys.public_key();
            let public_key = signer.get_public_key_async().await?;

            // Construct a filter to get the latest approval event
            let filter = Filter::new()
                .kind(Kind::Custom(4455))
                .author(public_key)
                .pubkey(app_pubkey)
                .limit(1);

            if let Some(event) = client.database().query(filter).await?.into_iter().next() {
                // Only reuse the stored approval if its key is still valid
                if decrypt_approval(&client, &app_keys, public_key, &event)
                    .await
                    .is_ok()
                {
                    return Ok(Some(event));
                }
            }

            // No usable approval event found, construct a request event.
            //
            // The `pubkey` tag is a compatibility alias for the draft's `P`,
            // `relay` hints tell other devices where the response is accepted,
            // `n` names the exact key this device expects to be transferred
            let mut tags = vec![
                Tag::custom("P", vec![app_pubkey]),
                Tag::custom("pubkey", vec![app_pubkey]),
            ];

            for url in write_relay_hints(&client, public_key).await? {
                tags.push(Tag::custom("relay", vec![url.to_string()]));
            }

            if let Some(expected) = expected_encryption {
                tags.push(Tag::custom("n", vec![expected]));
            }

            let event = EventBuilder::new(Kind::Custom(4454), "")
                .tags(tags)
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

        let Some(app_keys) = self.app_keys.clone() else {
            log::warn!("Cannot wait for approval without a client key");
            return;
        };

        self.tasks.push(cx.spawn(async move |this, cx| {
            this.update(cx, |_this, cx| {
                cx.emit(DeviceEvent::Requesting(authorization_code(
                    &app_keys.public_key(),
                )));
            })?;

            let task: Task<Result<(), Error>> = cx.background_spawn(async move {
                let public_key = signer.get_public_key_async().await?;

                // Only approvals addressed to this device's client key matter
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
        let nostr = NostrRegistry::global(cx);
        let client = nostr.read(cx).client();
        let signer = nostr.read(cx).signer();

        let Some(app_keys) = self.app_keys.clone() else {
            log::warn!("Ignoring key transfer addressed to another device");
            return;
        };

        let approval_id = event.id;

        let task: Task<Result<Keys, Error>> = cx.background_spawn(async move {
            let identity = signer.get_public_key_async().await?;
            let keys = decrypt_approval(&client, &app_keys, identity, &event).await?;

            Ok(keys)
        });

        self.tasks.push(cx.spawn(async move |this, cx| {
            match task.await {
                Ok(keys) => {
                    this.update(cx, |this, cx| {
                        this.set_signer(keys, cx);
                        // The client key was consumed; start fresh on the next request
                        this.app_keys = None;
                        // Clean up our request and the approval event from relays
                        this.cleanup_sync_events(approval_id, cx);
                    })?;
                }
                Err(e) => {
                    log::warn!("Ignoring key transfer: {e}");
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

        let event = event.clone();
        let request_id = event.id;

        let task: Task<Result<(), Error>> = cx.background_spawn(async move {
            // Get device keys
            let keys = get_keys(&client, &signer).await?;
            let secret = keys.secret_key().to_secret_hex();
            let identity = signer.get_public_key_async().await?;

            // Extract the target public key from the event tags
            let target =
                client_pubkey_from_event(&event).context("Target is not a valid public key")?;

            // The requester may pin the exact key it expects
            if let Some(requested) = requested_encryption_key(&event)
                && requested != keys.public_key()
            {
                return Err(anyhow!(
                    "The requesting device expects an encryption key this device no longer holds"
                ));
            }

            // Encrypt to a fresh one-time keypair so the transfer event
            // can never be decrypted with the long-lived encryption key.
            let sender = Keys::generate();
            let payload = sender.nip44_encrypt_async(&target, &secret).await?;

            // Construct the response event
            //
            // P tag: the one-time sender key used for the encryption
            // p tag: the account identity, for inbox routing
            // p tag: the requester's client key
            let response = EventBuilder::new(Kind::Custom(4455), payload)
                .tags(vec![
                    Tag::custom("P", vec![sender.public_key().to_hex()]),
                    Tag::public_key(identity),
                    Tag::public_key(target),
                ])
                .finalize_async(&signer)
                .await?;

            let relays = merge_relays(
                write_relay_hints(&client, identity).await?,
                request_relay_hints(&event),
            );

            let send = client.send_event(&response);
            if relays.is_empty() {
                send.to_nip65().await?;
            } else {
                send.to(relays).await?;
            }

            Ok(())
        });

        self.tasks.push(cx.spawn_in(window, async move |this, cx| {
            match task.await {
                Ok(_) => {
                    this.update(cx, |this, cx| {
                        // Record the request as handled so it cannot resurface
                        this.mark_request_processed(request_id, cx);
                        // Allow future requests to surface again
                        this.pending_request = None;
                        cx.notify();
                    })?;

                    cx.update(|window, cx| {
                        window.remove_notification::<DeviceNotification>(cx);
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
        let expires_at = Timestamp::now()
            .as_secs()
            .saturating_sub(event.created_at.as_secs());

        if expires_at > REQUEST_MAX_AGE {
            log::warn!("Ignoring expired encryption key request");
            return;
        }

        let Some(client_key) = client_pubkey_from_event(&event) else {
            log::warn!("Ignoring invalid encryption key request: no valid requester key");
            return;
        };

        let read = credentials::read_processed_requests(cx);

        cx.spawn(async move |this, cx| {
            let processed = match read.await {
                Ok(bytes) => bytes
                    .map(|bytes| parse_processed_requests(&bytes).contains(&event.id))
                    .unwrap_or(false),
                Err(e) => {
                    log::error!("Failed to read processed key-sync requests: {e}");
                    false
                }
            };

            this.update(cx, |this, cx| {
                if processed || this.processed_requests.contains(&event.id) {
                    return;
                }

                match this.pending_request.take() {
                    // A retry signs a new request id but keeps the same client key
                    Some(mut pending) if pending.client_key == client_key => {
                        this.mark_request_processed(event.id, cx);
                        pending.event = event;
                        this.pending_request = Some(pending);
                    }

                    // A different client key is a remounted requester
                    other => {
                        if let Some(pending) = other {
                            this.mark_request_processed(pending.event.id, cx);
                            if let Some(window) = cx.windows().first().copied() {
                                window
                                    .update(cx, |_view, window, cx| {
                                        window.remove_notification::<DeviceNotification>(cx);
                                    })
                                    .ok();
                            }
                        }
                        this.show_approval(event, client_key, cx);
                    }
                }
            })
            .ok();
        })
        .detach();
    }

    /// Build and surface the approval notification for a key request
    fn show_approval(&mut self, event: Event, client_key: PublicKey, cx: &mut Context<Self>) {
        let notification = match self.notification(event.clone(), cx) {
            Ok(notification) => notification,
            Err(e) => {
                log::warn!("Ignoring invalid encryption key request: {e}");
                return;
            }
        };

        self.pending_request = Some(PendingRequest { event, client_key });
        cx.notify();

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
    fn notification(
        &mut self,
        event: Event,
        cx: &mut Context<Self>,
    ) -> Result<Notification, Error> {
        const MSG: &str = "You've requested an encryption key from another device. \
                           Approve to allow Coop to share with it. Before approving, \
                           compare the code with the one shown on the requesting \
                           device.";

        let requester =
            client_pubkey_from_event(&event).context("Request has no valid requester key")?;

        let code = authorization_code(&requester);
        let code_label = code.clone();

        let directory = self.directory.clone();
        directory.update(cx, |directory, cx| {
            directory.track([requester], cx);
        });

        let entity = cx.entity().downgrade();
        let loading = Rc::new(Cell::new(false));

        Ok(Notification::new()
            .id::<DeviceNotification>()
            .autohide(false)
            .with_type(NotificationType::Info)
            .title("Encryption Key Request")
            .content(move |_this, _window, cx| {
                let profile = directory.read(cx).person(&requester, cx);

                v_flex()
                    .gap_2()
                    .text_xs()
                    .child(
                        div()
                            .text_xs()
                            .line_height(relative(1.25))
                            .child(SharedString::from(MSG)),
                    )
                    .child(
                        v_flex()
                            .gap_2()
                            .child(
                                v_flex()
                                    .gap_1()
                                    .text_xs()
                                    .child(
                                        div()
                                            .font_semibold()
                                            .text_xs()
                                            .text_color(cx.theme().muted_foreground)
                                            .child(SharedString::from("From:")),
                                    )
                                    .child(
                                        div()
                                            .h_8()
                                            .w_full()
                                            .px_2()
                                            .rounded(cx.theme().radius)
                                            .bg(cx.theme().muted)
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
                                    .text_xs()
                                    .child(
                                        div()
                                            .font_semibold()
                                            .text_xs()
                                            .text_color(cx.theme().muted_foreground)
                                            .child(SharedString::from("Code:")),
                                    )
                                    .child(
                                        div()
                                            .h_8()
                                            .w_full()
                                            .px_2()
                                            .rounded(cx.theme().radius)
                                            .bg(cx.theme().muted)
                                            .child(code_label.clone()),
                                    ),
                            ),
                    )
                    .into_any_element()
            })
            .on_close({
                let entity = entity.clone();
                let request_id = event.id;

                move |_window, cx| {
                    // Closing without approving rejects the request, so it must not resurface
                    entity
                        .update(cx, |this, cx| {
                            this.mark_request_processed(request_id, cx);
                        })
                        .ok();
                }
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

/// Parse concatenated hex event ids
fn parse_processed_requests(bytes: &[u8]) -> VecDeque<EventId> {
    let Ok(hex) = std::str::from_utf8(bytes) else {
        return VecDeque::new();
    };

    hex.as_bytes()
        .as_chunks::<64>()
        .0
        .iter()
        .filter_map(|chunk| EventId::from_hex(std::str::from_utf8(chunk).ok()?).ok())
        .collect()
}

/// Extract the client public key from an event.
fn client_pubkey_from_event(event: &Event) -> Option<PublicKey> {
    let key = event
        .tags
        .iter()
        .find(|tag| tag.kind() == "P")
        .and_then(|tag| tag.content())
        .and_then(|content| PublicKey::parse(content).ok());

    key.or_else(|| {
        event
            .tags
            .iter()
            .find(|tag| tag.kind() == "pubkey")
            .and_then(|tag| tag.content())
            .and_then(|content| PublicKey::parse(content).ok())
    })
}

/// Whether the transfer event is p-tagged to the given client key
fn transfer_addresses(event: &Event, client_key: PublicKey) -> bool {
    event.tags.iter().any(|tag| {
        tag.kind() == "p"
            && tag
                .content()
                .and_then(|content| PublicKey::parse(content).ok())
                .is_some_and(|key| key == client_key)
    })
}

/// Parse the `relay` hints attached to a key request.
fn request_relay_hints(event: &Event) -> Vec<RelayUrl> {
    event
        .tags
        .iter()
        .filter(|tag| tag.kind() == "relay")
        .filter_map(|tag| {
            tag.content()
                .and_then(|content| RelayUrl::parse(content).ok())
        })
        .collect()
}

/// Parse the optional `n` tag of a key request.
fn requested_encryption_key(event: &Event) -> Option<PublicKey> {
    event
        .tags
        .iter()
        .find(|tag| tag.kind() == "n")
        .and_then(|tag| tag.content())
        .and_then(|content| PublicKey::parse(content).ok())
}

/// Collect the user's NIP-65 write relays.
async fn write_relay_hints(client: &Client, identity: PublicKey) -> Result<Vec<RelayUrl>, Error> {
    let filter = Filter::new()
        .kind(Kind::RelayList)
        .author(identity)
        .limit(1);

    let Some(event) = client.database().query(filter).await?.into_iter().next() else {
        return Ok(vec![]);
    };

    // Unmarked relays serve both reads and writes
    Ok(nip65::extract_relay_list(&event)
        .filter(|(_, metadata)| *metadata != Some(RelayMetadata::Read))
        .map(|(url, _)| url)
        .collect())
}

/// Merge relay lists, dropping duplicates
fn merge_relays(first: Vec<RelayUrl>, second: Vec<RelayUrl>) -> Vec<RelayUrl> {
    first
        .into_iter()
        .chain(second)
        .collect::<HashSet<_>>()
        .into_iter()
        .collect()
}

/// The first eight hexadecimal characters of the client public key.
fn authorization_code(public_key: &PublicKey) -> SharedString {
    let hex = public_key.to_hex();
    let code = hex[..8].to_uppercase();
    SharedString::from(format!("{} {}", &code[..4], &code[4..]))
}

/// Decrypt and validate an approval event against the current announcement.
async fn decrypt_approval(
    client: &Client,
    app_keys: &Keys,
    identity: PublicKey,
    event: &Event,
) -> Result<Keys, Error> {
    let master = client_pubkey_from_event(event).context("Invalid event's tags")?;

    let decrypted = app_keys
        .nip44_decrypt_async(&master, event.content.as_str())
        .await?;

    let secret = SecretKey::from_hex(&decrypted)?;
    let keys = Keys::new(secret);

    // Reject approvals carrying a key that no longer matches the published announcement
    let filter = Filter::new()
        .kind(Kind::Custom(10044))
        .author(identity)
        .limit(1);

    let announcement = client
        .database()
        .query(filter.clone())
        .await?
        .into_iter()
        .next();

    let announcement = match announcement {
        Some(announcement) => Some(announcement),
        None => {
            let mut stream = client
                .stream_events(filter)
                .timeout(Duration::from_secs(3))
                .await?;

            stream
                .next()
                .await
                .map(|(_url, result)| result)
                .transpose()?
        }
    };

    let Some(announcement) = announcement else {
        return Err(anyhow!(
            "No encryption announcement available to validate the approval"
        ));
    };

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

    Ok(keys)
}
