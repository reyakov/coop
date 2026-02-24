use chat::{ChatRegistry, InboxState};
use gpui::prelude::FluentBuilder;
use gpui::{
    div, svg, AnyElement, App, AppContext, Context, Entity, EventEmitter, FocusHandle, Focusable,
    IntoElement, ParentElement, Render, SharedString, Styled, Window,
};
use state::{NostrRegistry, RelayState};
use theme::ActiveTheme;
use ui::button::{Button, ButtonVariants};
use ui::dock_area::dock::DockPlacement;
use ui::dock_area::panel::{Panel, PanelEvent};
use ui::{h_flex, v_flex, Icon, IconName, Sizable, StyledExt};

use crate::panels::{connect, import, messaging_relays, profile, relay_list};
use crate::workspace::Workspace;

pub fn init(window: &mut Window, cx: &mut App) -> Entity<GreeterPanel> {
    cx.new(|cx| GreeterPanel::new(window, cx))
}

pub struct GreeterPanel {
    name: SharedString,
    focus_handle: FocusHandle,
}

impl GreeterPanel {
    fn new(_window: &mut Window, cx: &mut App) -> Self {
        Self {
            name: "Onboarding".into(),
            focus_handle: cx.focus_handle(),
        }
    }

    fn add_profile_panel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let nostr = NostrRegistry::global(cx);
        let signer = nostr.read(cx).signer();

        if let Some(public_key) = signer.public_key() {
            cx.spawn_in(window, async move |_this, cx| {
                cx.update(|window, cx| {
                    Workspace::add_panel(
                        profile::init(public_key, window, cx),
                        DockPlacement::Right,
                        window,
                        cx,
                    );
                })
                .ok();
            })
            .detach();
        }
    }
}

impl Panel for GreeterPanel {
    fn panel_id(&self) -> SharedString {
        self.name.clone()
    }

    fn title(&self, cx: &App) -> AnyElement {
        div()
            .child(
                svg()
                    .path("brand/coop.svg")
                    .size_4()
                    .text_color(cx.theme().text_muted),
            )
            .into_any_element()
    }
}

impl EventEmitter<PanelEvent> for GreeterPanel {}

impl Focusable for GreeterPanel {
    fn focus_handle(&self, _: &App) -> gpui::FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for GreeterPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        const TITLE: &str = "Welcome to Coop!";
        const DESCRIPTION: &str = "Chat Freely, Stay Private on Nostr.";

        let chat = ChatRegistry::global(cx);
        let nip17 = chat.read(cx).state(cx);

        let nostr = NostrRegistry::global(cx);
        let nip65 = nostr.read(cx).relay_list_state();

        let signer = nostr.read(cx).signer();
        let owned = signer.owned();

        let required_actions =
            nip65 == RelayState::NotConfigured || nip17 == InboxState::RelayNotAvailable;

        h_flex()
            .size_full()
            .items_center()
            .justify_center()
            .p_2()
            .child(
                v_flex()
                    .h_full()
                    .w_112()
                    .gap_6()
                    .items_center()
                    .justify_center()
                    .child(
                        h_flex()
                            .mb_4()
                            .gap_2()
                            .w_full()
                            .child(
                                svg()
                                    .path("brand/coop.svg")
                                    .size_12()
                                    .text_color(cx.theme().icon_muted),
                            )
                            .child(
                                v_flex()
                                    .child(
                                        div()
                                            .font_semibold()
                                            .text_color(cx.theme().text)
                                            .child(SharedString::from(TITLE)),
                                    )
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(cx.theme().text_muted)
                                            .child(SharedString::from(DESCRIPTION)),
                                    ),
                            ),
                    )
                    .when(required_actions, |this| {
                        this.child(
                            v_flex()
                                .gap_2()
                                .w_full()
                                .child(
                                    h_flex()
                                        .gap_2()
                                        .w_full()
                                        .text_xs()
                                        .font_semibold()
                                        .text_color(cx.theme().text_muted)
                                        .child(SharedString::from("Required Actions"))
                                        .child(div().flex_1().h_px().bg(cx.theme().border)),
                                )
                                .child(
                                    v_flex()
                                        .gap_2()
                                        .w_full()
                                        .when(nip65.not_configured(), |this| {
                                            this.child(
                                                Button::new("relaylist")
                                                    .icon(Icon::new(IconName::Relay))
                                                    .label("Set up relay list")
                                                    .ghost()
                                                    .small()
                                                    .justify_start()
                                                    .on_click(move |_ev, window, cx| {
                                                        Workspace::add_panel(
                                                            relay_list::init(window, cx),
                                                            DockPlacement::Center,
                                                            window,
                                                            cx,
                                                        );
                                                    }),
                                            )
                                        })
                                        .when(nip17.not_configured(), |this| {
                                            this.child(
                                                Button::new("import")
                                                    .icon(Icon::new(IconName::Relay))
                                                    .label("Set up messaging relays")
                                                    .ghost()
                                                    .small()
                                                    .justify_start()
                                                    .on_click(move |_ev, window, cx| {
                                                        Workspace::add_panel(
                                                            messaging_relays::init(window, cx),
                                                            DockPlacement::Center,
                                                            window,
                                                            cx,
                                                        );
                                                    }),
                                            )
                                        }),
                                ),
                        )
                    })
                    .when(!owned, |this| {
                        this.child(
                            v_flex()
                                .gap_2()
                                .w_full()
                                .child(
                                    h_flex()
                                        .gap_2()
                                        .w_full()
                                        .text_xs()
                                        .font_semibold()
                                        .text_color(cx.theme().text_muted)
                                        .child(SharedString::from("Use your own identity"))
                                        .child(div().flex_1().h_px().bg(cx.theme().border)),
                                )
                                .child(
                                    v_flex()
                                        .gap_2()
                                        .w_full()
                                        .child(
                                            Button::new("connect")
                                                .icon(Icon::new(IconName::Door))
                                                .label("Connect account via Nostr Connect")
                                                .ghost()
                                                .small()
                                                .justify_start()
                                                .on_click(move |_ev, window, cx| {
                                                    Workspace::add_panel(
                                                        connect::init(window, cx),
                                                        DockPlacement::Center,
                                                        window,
                                                        cx,
                                                    );
                                                }),
                                        )
                                        .child(
                                            Button::new("import")
                                                .icon(Icon::new(IconName::Usb))
                                                .label("Import a secret key or bunker")
                                                .ghost()
                                                .small()
                                                .justify_start()
                                                .on_click(move |_ev, window, cx| {
                                                    Workspace::add_panel(
                                                        import::init(window, cx),
                                                        DockPlacement::Center,
                                                        window,
                                                        cx,
                                                    );
                                                }),
                                        ),
                                ),
                        )
                    })
                    .child(
                        v_flex()
                            .gap_2()
                            .w_full()
                            .child(
                                h_flex()
                                    .gap_2()
                                    .w_full()
                                    .text_xs()
                                    .font_semibold()
                                    .text_color(cx.theme().text_muted)
                                    .child(SharedString::from("Get Started"))
                                    .child(div().flex_1().h_px().bg(cx.theme().border)),
                            )
                            .child(
                                v_flex()
                                    .gap_2()
                                    .w_full()
                                    .child(
                                        Button::new("profile")
                                            .icon(Icon::new(IconName::Profile))
                                            .label("Update profile")
                                            .ghost()
                                            .small()
                                            .justify_start()
                                            .on_click(cx.listener(move |this, _ev, window, cx| {
                                                this.add_profile_panel(window, cx)
                                            })),
                                    )
                                    .child(
                                        Button::new("invite")
                                            .icon(Icon::new(IconName::Invite))
                                            .label("Invite friends")
                                            .ghost()
                                            .small()
                                            .justify_start(),
                                    ),
                            ),
                    ),
            )
    }
}
