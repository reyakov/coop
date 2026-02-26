use anyhow::Error;
use device::DeviceRegistry;
use gpui::prelude::FluentBuilder;
use gpui::{
    div, px, AnyElement, App, AppContext, Context, Entity, EventEmitter, FocusHandle, Focusable,
    IntoElement, ParentElement, Render, SharedString, Styled, Task, Window,
};
use nostr_sdk::prelude::*;
use person::{shorten_pubkey, PersonRegistry};
use state::Announcement;
use theme::ActiveTheme;
use ui::button::{Button, ButtonVariants};
use ui::dock_area::panel::{Panel, PanelEvent};
use ui::notification::Notification;
use ui::{divider, h_flex, v_flex, Disableable, IconName, Sizable, StyledExt, WindowExtension};

const MSG: &str =
    "Encryption Key is a special key that used to encrypt and decrypt your messages. \
     Your identity is completely decoupled from all encryption processes to protect your privacy.";

const NOTICE: &str = "By resetting your encryption key, you will lose access to \
                      all your encrypted messages before. This action cannot be undone.";

pub fn init(public_key: PublicKey, window: &mut Window, cx: &mut App) -> Entity<EncryptionPanel> {
    cx.new(|cx| EncryptionPanel::new(public_key, window, cx))
}

#[derive(Debug)]
pub struct EncryptionPanel {
    name: SharedString,
    focus_handle: FocusHandle,

    /// User's public key
    public_key: PublicKey,

    /// Whether the panel is loading
    loading: bool,

    /// Whether the encryption is resetting
    resetting: bool,

    /// Tasks
    tasks: Vec<Task<Result<(), Error>>>,
}

impl EncryptionPanel {
    fn new(public_key: PublicKey, _window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self {
            name: "Encryption".into(),
            focus_handle: cx.focus_handle(),
            public_key,
            loading: false,
            resetting: false,
            tasks: vec![],
        }
    }

    fn set_loading(&mut self, status: bool, cx: &mut Context<Self>) {
        self.loading = status;
        cx.notify();
    }

    fn approve(&mut self, event: &Event, window: &mut Window, cx: &mut Context<Self>) {
        let device = DeviceRegistry::global(cx);
        let task = device.read(cx).approve(event, cx);
        let id = event.id;

        // Update loading status
        self.set_loading(true, cx);

        self.tasks.push(cx.spawn_in(window, async move |this, cx| {
            match task.await {
                Ok(_) => {
                    this.update_in(cx, |this, window, cx| {
                        // Reset loading status
                        this.set_loading(false, cx);

                        // Remove request
                        device.update(cx, |this, cx| {
                            this.remove_request(&id, cx);
                        });

                        window.push_notification("Approved", cx);
                    })?;
                }
                Err(e) => {
                    this.update_in(cx, |this, window, cx| {
                        this.set_loading(false, cx);
                        window.push_notification(Notification::error(e.to_string()), cx);
                    })?;
                }
            }

            Ok(())
        }));
    }

    fn set_resetting(&mut self, status: bool, cx: &mut Context<Self>) {
        self.resetting = status;
        cx.notify();
    }

    fn reset(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let device = DeviceRegistry::global(cx);
        let task = device.read(cx).create_encryption(cx);

        // Update the reset status
        self.set_resetting(true, cx);

        self.tasks.push(cx.spawn_in(window, async move |this, cx| {
            match task.await {
                Ok(keys) => {
                    this.update_in(cx, |this, _window, cx| {
                        this.set_resetting(false, cx);

                        device.update(cx, |this, cx| {
                            this.set_signer(keys, cx);
                            this.listen_request(cx);
                        });
                    })?;
                }
                Err(e) => {
                    this.update_in(cx, |this, window, cx| {
                        this.set_resetting(false, cx);
                        window.push_notification(Notification::error(e.to_string()), cx);
                    })?;
                }
            }

            Ok(())
        }));
    }

    fn render_requests(&mut self, cx: &mut Context<Self>) -> Vec<impl IntoElement> {
        const TITLE: &str = "You've requested for the Encryption Key from:";

        let device = DeviceRegistry::global(cx);
        let requests = device.read(cx).requests.clone();
        let mut items = Vec::new();

        for event in requests.into_iter() {
            let request = Announcement::from(&event);
            let client_name = request.client_name();
            let target = request.public_key();

            items.push(
                v_flex()
                    .gap_2()
                    .text_sm()
                    .child(SharedString::from(TITLE))
                    .child(
                        v_flex()
                            .h_12()
                            .items_center()
                            .justify_center()
                            .px_2()
                            .rounded(cx.theme().radius)
                            .bg(cx.theme().warning_background)
                            .text_color(cx.theme().warning_foreground)
                            .child(client_name.clone()),
                    )
                    .child(
                        h_flex()
                            .h_7()
                            .w_full()
                            .px_2()
                            .rounded(cx.theme().radius)
                            .bg(cx.theme().elevated_surface_background)
                            .child(SharedString::from(target.to_hex())),
                    )
                    .child(
                        h_flex().justify_end().gap_2().child(
                            Button::new("approve")
                                .label("Approve")
                                .ghost()
                                .small()
                                .disabled(self.loading)
                                .loading(self.loading)
                                .on_click(cx.listener(move |this, _ev, window, cx| {
                                    this.approve(&event, window, cx);
                                })),
                        ),
                    ),
            )
        }

        items
    }
}

impl Panel for EncryptionPanel {
    fn panel_id(&self) -> SharedString {
        self.name.clone()
    }

    fn title(&self, _cx: &App) -> AnyElement {
        self.name.clone().into_any_element()
    }
}

impl EventEmitter<PanelEvent> for EncryptionPanel {}

impl Focusable for EncryptionPanel {
    fn focus_handle(&self, _: &App) -> gpui::FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for EncryptionPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let device = DeviceRegistry::global(cx);
        let state = device.read(cx).state();
        let has_requests = device.read(cx).has_requests();

        let persons = PersonRegistry::global(cx);
        let profile = persons.read(cx).get(&self.public_key, cx);

        let Some(announcement) = profile.announcement() else {
            return div();
        };

        let pubkey = SharedString::from(shorten_pubkey(announcement.public_key(), 16));
        let client_name = announcement.client_name();

        v_flex()
            .p_3()
            .gap_3()
            .w_full()
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().text_muted)
                    .child(SharedString::from(MSG)),
            )
            .child(divider(cx))
            .child(
                v_flex()
                    .gap_3()
                    .text_sm()
                    .child(
                        v_flex()
                            .gap_1p5()
                            .child(
                                div()
                                    .text_color(cx.theme().text_muted)
                                    .child(SharedString::from("Device Name:")),
                            )
                            .child(
                                h_flex()
                                    .h_12()
                                    .items_center()
                                    .justify_center()
                                    .rounded(cx.theme().radius)
                                    .bg(cx.theme().elevated_surface_background)
                                    .child(client_name.clone()),
                            ),
                    )
                    .child(
                        v_flex()
                            .gap_1p5()
                            .child(
                                div()
                                    .text_color(cx.theme().text_muted)
                                    .child(SharedString::from("Encryption Public Key:")),
                            )
                            .child(
                                h_flex()
                                    .h_7()
                                    .w_full()
                                    .px_2()
                                    .rounded(cx.theme().radius)
                                    .bg(cx.theme().elevated_surface_background)
                                    .child(pubkey),
                            ),
                    ),
            )
            .when(has_requests, |this| {
                this.child(divider(cx)).child(
                    v_flex()
                        .gap_1p5()
                        .w_full()
                        .child(
                            div()
                                .text_color(cx.theme().text_muted)
                                .child(SharedString::from("Requests:")),
                        )
                        .child(
                            v_flex()
                                .gap_2()
                                .flex_1()
                                .w_full()
                                .children(self.render_requests(cx)),
                        ),
                )
            })
            .child(divider(cx))
            .when(state.requesting(), |this| {
                this.child(
                    h_flex()
                        .h_8()
                        .justify_center()
                        .text_xs()
                        .text_center()
                        .text_color(cx.theme().text_accent)
                        .bg(cx.theme().elevated_surface_background)
                        .rounded(cx.theme().radius)
                        .child(SharedString::from(
                            "Please open other device and approve the request",
                        )),
                )
            })
            .child(
                v_flex()
                    .gap_1()
                    .child(
                        Button::new("reset")
                            .icon(IconName::Reset)
                            .label("Reset")
                            .warning()
                            .small()
                            .font_semibold()
                            .on_click(
                                cx.listener(move |this, _ev, window, cx| this.reset(window, cx)),
                            ),
                    )
                    .child(
                        div()
                            .italic()
                            .text_size(px(10.))
                            .text_color(cx.theme().text_muted)
                            .child(SharedString::from(NOTICE)),
                    ),
            )
    }
}
