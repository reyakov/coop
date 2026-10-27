use anyhow::Error;
use gpui::{
    App, AppContext, Context, Entity, EventEmitter, FocusHandle, Focusable, IntoElement,
    ParentElement, Render, SharedString, Styled, Task, Window, div,
};
use gpui_kit::base::StyledExt as _;
use gpui_kit::component::clipboard::Clipboard;
use gpui_kit::component::dock::{Panel, PanelEvent};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::separator::Separator;
use gpui_kit::component::{ActiveTheme, v_flex};
use nostr_sdk::prelude::*;
use state::USER_KEYRING;

const MSG: &str = "Store your account keys in a safe location. \
                   You can restore your account or move to another client anytime you want.";

pub fn init(window: &mut Window, cx: &mut App) -> Entity<BackupPanel> {
    cx.new(|cx| BackupPanel::new(window, cx))
}

#[derive(Debug)]
pub struct BackupPanel {
    name: SharedString,
    focus_handle: FocusHandle,

    /// Public key input
    npub_input: Entity<InputState>,

    /// Secret key input
    nsec_input: Entity<InputState>,

    /// Background tasks
    tasks: Vec<Task<Result<(), Error>>>,
}

impl BackupPanel {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let npub_input = cx.new(|cx| InputState::new(window, cx));
        let nsec_input = cx.new(|cx| InputState::new(window, cx).masked(true));

        // Run at the end of current cycle
        cx.defer_in(window, |this, window, cx| {
            this.load(window, cx);
        });

        Self {
            name: "Backup".into(),
            focus_handle: cx.focus_handle(),
            npub_input,
            nsec_input,
            tasks: vec![],
        }
    }

    fn load(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let keyring = cx.read_credentials(USER_KEYRING);

        self.tasks.push(cx.spawn_in(window, async move |this, cx| {
            if let Some((_, secret)) = keyring.await? {
                let secret = SecretKey::from_slice(&secret)?;
                let keys = Keys::new(secret);

                this.update_in(cx, |this, window, cx| {
                    this.npub_input.update(cx, |this, cx| {
                        this.set_value(keys.public_key().to_bech32().unwrap(), window, cx);
                    });

                    this.nsec_input.update(cx, |this, cx| {
                        this.set_value(keys.secret_key().to_bech32().unwrap(), window, cx);
                    });
                })?;
            }

            Ok(())
        }));
    }
}

impl gpui_kit::component::dock::BasePanel for BackupPanel {
    fn panel_name(&self) -> &'static str {
        "Backup"
    }
}

impl Panel for BackupPanel {
    fn title(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        self.name.clone()
    }
}

impl EventEmitter<PanelEvent> for BackupPanel {}

impl Focusable for BackupPanel {
    fn focus_handle(&self, _: &App) -> gpui::FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for BackupPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .p_3()
            .gap_3()
            .w_full()
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(SharedString::from(MSG)),
            )
            .child(Separator::horizontal())
            .child(
                v_flex()
                    .gap_2()
                    .flex_1()
                    .w_full()
                    .text_xs()
                    .child(
                        v_flex()
                            .gap_1p5()
                            .w_full()
                            .child(
                                div()
                                    .text_xs()
                                    .font_semibold()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(SharedString::from("Public Key:")),
                            )
                            .child(Input::new(&self.npub_input).disabled(true)),
                    )
                    .child(
                        v_flex()
                            .gap_1p5()
                            .w_full()
                            .child(
                                div()
                                    .text_xs()
                                    .font_semibold()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(SharedString::from("Secret Key:")),
                            )
                            .child(Input::new(&self.nsec_input).disabled(true)),
                    )
                    .child(
                        Clipboard::new("copy-secret")
                            .accessibility_label("Copy secret key")
                            .value_fn({
                                let nsec_input = self.nsec_input.clone();
                                move |_, cx| nsec_input.read(cx).value()
                            }),
                    ),
            )
    }
}
