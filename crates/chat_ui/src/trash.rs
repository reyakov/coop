use std::rc::Rc;

use chat::{ChatRegistry, TrashedMessage};
use common::TimestampExt;
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, AppContext, Context, EventEmitter, FocusHandle, Focusable, IntoElement,
    ParentElement, Render, Styled, Window, div, uniform_list,
};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::dock::{Panel, PanelEvent};
use gpui_kit::component::{ActiveTheme, Disableable, Icon, Sizable, h_flex, v_flex};
use ui::IconName;

/// Initialize the trash panel.
pub fn init(window: &mut Window, cx: &mut App) -> gpui::Entity<TrashPanel> {
    cx.new(|cx| TrashPanel::new(window, cx))
}

/// The messages that could not be decrypted, shown like a chat room.
pub struct TrashPanel {
    focus_handle: FocusHandle,
}

impl TrashPanel {
    fn new(_window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self {
            focus_handle: cx.focus_handle(),
        }
    }

    fn render_entry(&self, message: &TrashedMessage, cx: &Context<Self>) -> AnyElement {
        h_flex()
            .w_full()
            .gap_2()
            .items_center()
            .px_2()
            .py_1p5()
            .rounded(cx.theme().radius)
            .bg(cx.theme().secondary.opacity(0.5))
            .child(
                Icon::new(IconName::Warning)
                    .small()
                    .text_color(cx.theme().warning),
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .child(div().text_sm().child("Could not be decrypted"))
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .truncate()
                            .child(message.reason.clone()),
                    ),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(message.created_at.to_ago()),
            )
            .into_any_element()
    }

    fn render_empty(&self, cx: &Context<Self>) -> AnyElement {
        div()
            .text_color(cx.theme().muted_foreground)
            .child("Trash is empty")
            .into_any_element()
    }
}

impl gpui_kit::component::dock::BasePanel for TrashPanel {
    fn panel_name(&self) -> &'static str {
        "TrashPanel"
    }
}

impl Panel for TrashPanel {
    fn title(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        h_flex()
            .gap_1p5()
            .child(
                Icon::new(IconName::Warning)
                    .small()
                    .text_color(cx.theme().muted_foreground),
            )
            .child("Trash")
            .into_any_element()
    }
}

impl EventEmitter<PanelEvent> for TrashPanel {}

impl Focusable for TrashPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for TrashPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let messages = Rc::new(ChatRegistry::global(cx).read(cx).trashed(cx));
        let count = messages.len();

        v_flex()
            .size_full()
            .p_3()
            .gap_3()
            .text_xs()
            .child(
                h_flex()
                    .w_full()
                    .gap_2()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .flex_1()
                            .text_color(cx.theme().muted_foreground)
                            .child("Messages that could not be decrypted. New encryption keys may make them readable."),
                    )
                    .child(
                        Button::new("trash-retry")
                            .label("Try again")
                            .ghost()
                            .small()
                            .disabled(count == 0)
                            .on_click(|_, _window, cx| {
                                ChatRegistry::global(cx).update(cx, |registry, cx| {
                                    registry.reprocess_trash(cx);
                                });
                            }),
                    ),
            )
            .when(count == 0, |this| this.child(self.render_empty(cx)))
            .when(count > 0, |this| {
                this.child(
                    uniform_list(
                        "trash-list",
                        count,
                        cx.processor(move |this, range, _window, cx| {
                            this.render_entries(range, messages.as_slice(), cx)
                        }),
                    )
                    .min_h_0()
                    .flex_1(),
                )
            })
    }
}

impl TrashPanel {
    fn render_entries(
        &self,
        range: std::ops::Range<usize>,
        messages: &[TrashedMessage],
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        messages
            .get(range)
            .map(|slice| {
                slice
                    .iter()
                    .map(|message| {
                        // Keep a small gap between entries so they read as separate rows
                        div()
                            .pb_1p5()
                            .child(self.render_entry(message, cx))
                            .into_any_element()
                    })
                    .collect()
            })
            .unwrap_or_default()
    }
}
