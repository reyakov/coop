use std::time::Duration;

use anyhow::Error;
use gpui::{
    div, AnyElement, App, AppContext, ClipboardItem, Context, Entity, EventEmitter, FocusHandle,
    Focusable, IntoElement, ParentElement, Render, SharedString, Styled, Task, Window,
};
use nostr_sdk::prelude::*;
use state::KEYRING;
use theme::ActiveTheme;
use ui::button::{Button, ButtonVariants};
use ui::dock_area::panel::{Panel, PanelEvent};
use ui::input::{InputState, TextInput};
use ui::{divider, v_flex, IconName, Sizable, StyledExt};

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

    /// Copied status
    copied: bool,

    /// Background tasks
    tasks: Vec<Task<Result<(), Error>>>,
}

impl BackupPanel {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let npub_input = cx.new(|cx| InputState::new(window, cx).disabled(true));
        let nsec_input = cx.new(|cx| InputState::new(window, cx).disabled(true));

        // Run at the end of current cycle
        cx.defer_in(window, |this, window, cx| {
            this.load(window, cx);
        });

        Self {
            name: "Backup".into(),
            focus_handle: cx.focus_handle(),
            npub_input,
            nsec_input,
            copied: false,
            tasks: vec![],
        }
    }

    fn load(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let keyring = cx.read_credentials(KEYRING);

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

    fn copy_secret_key(&mut self, cx: &mut Context<Self>) {
        let value = self.nsec_input.read(cx).value();
        let item = ClipboardItem::new_string(value.to_string());

        // Copy to clipboard
        cx.write_to_clipboard(item);

        // Set the copied status to true
        self.set_copied(true, cx);
    }

    fn set_copied(&mut self, status: bool, cx: &mut Context<Self>) {
        self.copied = status;
        cx.notify();

        self.tasks.push(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_secs(2)).await;

            // Clear the error message after a delay
            this.update(cx, |this, cx| {
                this.set_copied(false, cx);
            })?;

            Ok(())
        }));
    }
}

impl Panel for BackupPanel {
    fn panel_id(&self) -> SharedString {
        self.name.clone()
    }

    fn title(&self, _cx: &App) -> AnyElement {
        self.name.clone().into_any_element()
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
                    .text_color(cx.theme().text_muted)
                    .child(SharedString::from(MSG)),
            )
            .child(divider(cx))
            .child(
                v_flex()
                    .gap_2()
                    .flex_1()
                    .w_full()
                    .text_sm()
                    .child(
                        v_flex()
                            .gap_1p5()
                            .w_full()
                            .child(
                                div()
                                    .text_xs()
                                    .font_semibold()
                                    .text_color(cx.theme().text_muted)
                                    .child(SharedString::from("Public Key:")),
                            )
                            .child(TextInput::new(&self.npub_input).small().bordered(false)),
                    )
                    .child(
                        v_flex()
                            .gap_1p5()
                            .w_full()
                            .child(
                                div()
                                    .text_xs()
                                    .font_semibold()
                                    .text_color(cx.theme().text_muted)
                                    .child(SharedString::from("Secret Key:")),
                            )
                            .child(TextInput::new(&self.nsec_input).small().bordered(false)),
                    )
                    .child(
                        Button::new("copy")
                            .icon(IconName::Copy)
                            .label({
                                if self.copied {
                                    "Copied"
                                } else {
                                    "Copy secret key"
                                }
                            })
                            .primary()
                            .small()
                            .font_semibold()
                            .on_click(cx.listener(move |this, _ev, _window, cx| {
                                this.copy_secret_key(cx);
                            })),
                    ),
            )
    }
}
