use std::sync::Arc;

use common::TextUtils;
use gpui::prelude::FluentBuilder;
use gpui::{
    div, img, px, relative, AnyElement, App, AppContext, Context, Entity, EventEmitter,
    FocusHandle, Focusable, Image, IntoElement, ParentElement, Render, SharedString, Styled, Task,
    Window,
};
use smallvec::{smallvec, SmallVec};
use state::NostrRegistry;
use theme::ActiveTheme;
use ui::dock_area::panel::{Panel, PanelEvent};
use ui::dock_area::ClosePanel;
use ui::notification::Notification;
use ui::{v_flex, StyledExt, WindowExtension};

pub fn init(window: &mut Window, cx: &mut App) -> Entity<ConnectPanel> {
    cx.new(|cx| ConnectPanel::new(window, cx))
}

pub struct ConnectPanel {
    name: SharedString,
    focus_handle: FocusHandle,

    /// QR Code
    qr_code: Option<Arc<Image>>,

    /// Background tasks
    _tasks: SmallVec<[Task<()>; 1]>,
}

impl ConnectPanel {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let nostr = NostrRegistry::global(cx);
        let weak_state = nostr.downgrade();
        let (signer, uri) = nostr.read(cx).client_connect(None);

        // Generate a QR code for quick connection
        let qr_code = uri.to_string().to_qr();

        let mut tasks = smallvec![];

        tasks.push(
            // Wait for nostr connect
            cx.spawn_in(window, async move |_this, cx| {
                let result = signer.bunker_uri().await;

                weak_state
                    .update_in(cx, |this, window, cx| {
                        match result {
                            Ok(uri) => {
                                this.persist_bunker(uri, cx);
                                this.set_signer(signer, true, cx);
                                // Close the current panel after setting the signer
                                window.dispatch_action(Box::new(ClosePanel), cx);
                            }
                            Err(e) => {
                                window.push_notification(Notification::error(e.to_string()), cx);
                            }
                        };
                    })
                    .ok();
            }),
        );

        Self {
            name: "Nostr Connect".into(),
            focus_handle: cx.focus_handle(),
            qr_code,
            _tasks: tasks,
        }
    }
}

impl Panel for ConnectPanel {
    fn panel_id(&self) -> SharedString {
        self.name.clone()
    }

    fn title(&self, _cx: &App) -> AnyElement {
        self.name.clone().into_any_element()
    }
}

impl EventEmitter<PanelEvent> for ConnectPanel {}

impl Focusable for ConnectPanel {
    fn focus_handle(&self, _: &App) -> gpui::FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for ConnectPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .p_2()
            .gap_10()
            .child(
                v_flex()
                    .justify_center()
                    .items_center()
                    .text_center()
                    .child(
                        div()
                            .font_semibold()
                            .line_height(relative(1.25))
                            .child(SharedString::from("Continue with Nostr Connect")),
                    )
                    .child(div().text_sm().text_color(cx.theme().text_muted).child(
                        SharedString::from("Use Nostr Connect apps to scan the code"),
                    )),
            )
            .when_some(self.qr_code.as_ref(), |this, qr| {
                this.child(
                    img(qr.clone())
                        .size(px(256.))
                        .rounded(cx.theme().radius_lg)
                        .border_1()
                        .border_color(cx.theme().border),
                )
            })
    }
}
