use std::borrow::Cow;
use std::cell::Cell;
use std::collections::HashSet;
use std::hash::Hash;
use std::rc::Rc;
use std::sync::Arc;

use anyhow::{anyhow, Context as AnyhowContext, Error};
use gpui::{
    App, AppContext, Context, Entity, Global, IntoElement, ParentElement, SharedString, Styled,
    Task, Window,
};
use nostr_sdk::prelude::*;
use settings::{AppSettings, AuthMode};
use smallvec::{smallvec, SmallVec};
use state::NostrRegistry;
use theme::ActiveTheme;
use ui::button::{Button, ButtonVariants};
use ui::notification::Notification;
use ui::{v_flex, Disableable, IconName, Sizable, WindowExtension};

const AUTH_MESSAGE: &str =
    "Approve the authentication request to allow Coop to continue sending or receiving events.";

pub fn init(window: &mut Window, cx: &mut App) {
    RelayAuth::set_global(cx.new(|cx| RelayAuth::new(window, cx)), cx);
}

/// Authentication request
#[derive(Debug, Clone, Hash, PartialEq, Eq, PartialOrd, Ord)]
struct AuthRequest {
    url: RelayUrl,
    challenge: String,
}

impl AuthRequest {
    pub fn new(challenge: impl Into<String>, url: RelayUrl) -> Self {
        Self {
            challenge: challenge.into(),
            url,
        }
    }

    pub fn url(&self) -> &RelayUrl {
        &self.url
    }

    pub fn challenge(&self) -> &str {
        &self.challenge
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum Signal {
    Auth(Arc<AuthRequest>),
    Pending((EventId, RelayUrl)),
}

struct GlobalRelayAuth(Entity<RelayAuth>);

impl Global for GlobalRelayAuth {}

// Relay authentication
#[derive(Debug)]
pub struct RelayAuth {
    /// Pending events waiting for resend after authentication
    pending_events: HashSet<(EventId, RelayUrl)>,

    /// Tasks for asynchronous operations
    tasks: SmallVec<[Task<()>; 2]>,
}

impl RelayAuth {
    /// Retrieve the global relay auth state
    pub fn global(cx: &App) -> Entity<Self> {
        cx.global::<GlobalRelayAuth>().0.clone()
    }

    /// Set the global relay auth instance
    fn set_global(state: Entity<Self>, cx: &mut App) {
        cx.set_global(GlobalRelayAuth(state));
    }

    /// Create a new relay auth instance
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        cx.defer_in(window, |this, window, cx| {
            this.handle_notifications(window, cx);
        });

        Self {
            pending_events: HashSet::default(),
            tasks: smallvec![],
        }
    }

    /// Handle nostr notifications
    fn handle_notifications(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let nostr = NostrRegistry::global(cx);
        let client = nostr.read(cx).client();

        // Channel for communication between nostr and gpui
        let (tx, rx) = flume::bounded::<Signal>(256);

        self.tasks.push(cx.background_spawn(async move {
            log::info!("Started handling nostr notifications");
            let mut notifications = client.notifications();
            let mut challenges: HashSet<Cow<'_, str>> = HashSet::default();

            while let Some(notification) = notifications.next().await {
                if let ClientNotification::Message { relay_url, message } = notification {
                    match message {
                        RelayMessage::Auth { challenge } => {
                            if challenges.insert(challenge.clone()) {
                                let request = Arc::new(AuthRequest::new(challenge, relay_url));
                                let signal = Signal::Auth(request);

                                tx.send_async(signal).await.ok();
                            }
                        }
                        RelayMessage::Ok {
                            event_id, message, ..
                        } => {
                            let msg = MachineReadablePrefix::parse(&message);

                            // Handle authentication messages
                            if let Some(MachineReadablePrefix::AuthRequired) = msg {
                                let signal = Signal::Pending((event_id, relay_url));
                                tx.send_async(signal).await.ok();
                            }
                        }
                        _ => {}
                    }
                }
            }
        }));

        self.tasks.push(cx.spawn_in(window, async move |this, cx| {
            while let Ok(signal) = rx.recv_async().await {
                match signal {
                    Signal::Auth(req) => {
                        this.update_in(cx, |this, window, cx| {
                            this.handle_auth(&req, window, cx);
                        })
                        .ok();
                    }
                    Signal::Pending((event_id, relay_url)) => {
                        this.update_in(cx, |this, _window, cx| {
                            this.insert_pending_event(event_id, relay_url, cx);
                        })
                        .ok();
                    }
                }
            }
        }));
    }

    /// Insert a pending event waiting for resend after authentication
    fn insert_pending_event(&mut self, id: EventId, relay: RelayUrl, cx: &mut Context<Self>) {
        self.pending_events.insert((id, relay));
        cx.notify();
    }

    /// Get all pending events for a specific relay,
    fn get_pending_events(&self, relay: &RelayUrl, _cx: &App) -> Vec<EventId> {
        let pending_events: Vec<EventId> = self
            .pending_events
            .iter()
            .filter(|(_, pending_relay)| pending_relay == relay)
            .map(|(id, _relay)| id)
            .cloned()
            .collect();

        pending_events
    }

    /// Clear all pending events for a specific relay,
    fn clear_pending_events(&mut self, relay: &RelayUrl, cx: &mut Context<Self>) {
        self.pending_events
            .retain(|(_, pending_relay)| pending_relay != relay);
        cx.notify();
    }

    /// Handle authentication request
    fn handle_auth(&mut self, req: &Arc<AuthRequest>, window: &mut Window, cx: &mut Context<Self>) {
        let settings = AppSettings::global(cx);
        let trusted_relay = settings.read(cx).trusted_relay(req.url(), cx);
        let mode = AppSettings::get_auth_mode(cx);

        if trusted_relay && mode == AuthMode::Auto {
            // Automatically authenticate if the relay is authenticated before
            self.response(req, window, cx);
        } else {
            // Otherwise open the auth request popup
            self.ask_for_approval(req, window, cx);
        }
    }

    /// Send auth response and wait for confirmation
    fn auth(&self, req: &Arc<AuthRequest>, cx: &App) -> Task<Result<(), Error>> {
        let nostr = NostrRegistry::global(cx);
        let client = nostr.read(cx).client();
        let req = req.clone();

        // Get all pending events for the relay
        let pending_events = self.get_pending_events(req.url(), cx);

        cx.background_spawn(async move {
            // Construct event
            let builder = EventBuilder::auth(req.challenge(), req.url().clone());
            let event = client.sign_event_builder(builder).await?;

            // Get the event ID
            let id = event.id;

            // Get the relay
            let relay = client.relay(req.url()).await?.context("Relay not found")?;

            // Subscribe to notifications
            let mut notifications = relay.notifications();

            // Send the AUTH message
            relay
                .send_msg(ClientMessage::Auth(Cow::Borrowed(&event)))
                .await?;

            log::info!("Sending AUTH event");

            while let Some(notification) = notifications.next().await {
                match notification {
                    RelayNotification::Message {
                        message: RelayMessage::Ok { event_id, .. },
                    } => {
                        if id != event_id {
                            continue;
                        }

                        // Get all subscriptions
                        let subscriptions = relay.subscriptions().await;

                        // Re-subscribe to previous subscriptions
                        for (id, filters) in subscriptions.into_iter() {
                            if !filters.is_empty() {
                                relay.send_msg(ClientMessage::req(id, filters)).await?;
                            }
                        }

                        // Re-send pending events
                        for id in pending_events {
                            if let Some(event) = client.database().event_by_id(&id).await? {
                                relay.send_event(&event).await?;
                            }
                        }

                        return Ok(());
                    }
                    RelayNotification::AuthenticationFailed => break,
                    _ => {}
                }
            }

            Err(anyhow!("Authentication failed"))
        })
    }

    /// Respond to an authentication request.
    fn response(&self, req: &Arc<AuthRequest>, window: &Window, cx: &Context<Self>) {
        let settings = AppSettings::global(cx);
        let req = req.clone();
        let challenge = req.challenge().to_string();

        // Create a task for authentication
        let task = self.auth(&req, cx);

        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            let url = req.url();

            this.update_in(cx, |this, window, cx| {
                window.clear_notification(challenge, cx);

                match result {
                    Ok(_) => {
                        // Clear pending events for the authenticated relay
                        this.clear_pending_events(url, cx);
                        // Save the authenticated relay to automatically authenticate future requests
                        settings.update(cx, |this, cx| {
                            this.add_trusted_relay(url, cx);
                        });
                        window.push_notification(format!("{} has been authenticated", url), cx);
                    }
                    Err(e) => {
                        window.push_notification(Notification::error(e.to_string()), cx);
                    }
                }
            })
            .ok();
        })
        .detach();
    }

    /// Push a popup to approve the authentication request.
    fn ask_for_approval(&self, req: &Arc<AuthRequest>, window: &Window, cx: &Context<Self>) {
        let notification = self.notification(req, cx);

        cx.spawn_in(window, async move |_this, cx| {
            cx.update(|window, cx| {
                window.push_notification(notification, cx);
            })
            .ok();
        })
        .detach();
    }

    /// Build a notification for the authentication request.
    fn notification(&self, req: &Arc<AuthRequest>, cx: &Context<Self>) -> Notification {
        let req = req.clone();
        let url = SharedString::from(req.url().to_string());
        let entity = cx.entity().downgrade();
        let loading = Rc::new(Cell::new(false));

        Notification::new()
            .custom_id(SharedString::from(&req.challenge))
            .autohide(false)
            .icon(IconName::Info)
            .title(SharedString::from("Authentication Required"))
            .content(move |_window, cx| {
                v_flex()
                    .gap_2()
                    .text_sm()
                    .child(SharedString::from(AUTH_MESSAGE))
                    .child(
                        v_flex()
                            .py_1()
                            .px_1p5()
                            .rounded_sm()
                            .text_xs()
                            .bg(cx.theme().warning_background)
                            .text_color(cx.theme().warning_foreground)
                            .child(url.clone()),
                    )
                    .into_any_element()
            })
            .action(move |_window, _cx| {
                let view = entity.clone();
                let req = req.clone();

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
                                this.response(&req, window, cx);
                            })
                            .ok();
                        }
                    })
            })
    }
}
