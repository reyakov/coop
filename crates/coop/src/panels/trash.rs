use chat::ChatRegistry;
use gpui::{
    AnyElement, App, AppContext, ClipboardItem, Context, Entity, EventEmitter, FocusHandle,
    Focusable, InteractiveElement, IntoElement, ListAlignment, ListState, ParentElement, Render,
    SharedString, Styled, Window, div, list, px, relative,
};
use theme::ActiveTheme;
use ui::button::{Button, ButtonVariants};
use ui::dock::{Panel, PanelEvent};
use ui::scroll::Scrollbar;
use ui::{Icon, IconName, Sizable, h_flex, v_flex};

pub fn init(window: &mut Window, cx: &mut App) -> Entity<TrashPanel> {
    cx.new(|cx| TrashPanel::new(window, cx))
}

pub struct TrashPanel {
    name: SharedString,
    focus_handle: FocusHandle,

    /// List state for messages
    list_state: ListState,
}

impl TrashPanel {
    fn new(_window: &mut Window, cx: &mut App) -> Self {
        let chat = ChatRegistry::global(cx);
        let count = chat.read(cx).count_trash_messages(cx);
        let list_state = ListState::new(count, ListAlignment::Bottom, px(1024.));

        Self {
            name: "Trash".into(),
            focus_handle: cx.focus_handle(),
            list_state,
        }
    }

    fn copy(&self, ix: usize, cx: &App) {
        let chat = ChatRegistry::global(cx);
        let trashes = chat.read(cx).trashes();

        if let Some(message) = trashes.read(cx).iter().nth(ix) {
            let item = ClipboardItem::new_string(message.raw_event.to_string());
            cx.write_to_clipboard(item);
        }
    }

    fn render_list_item(
        &mut self,
        ix: usize,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let chat = ChatRegistry::global(cx);
        let trashes = chat.read(cx).trashes();

        if let Some(message) = trashes.read(cx).iter().nth(ix) {
            v_flex()
                .id(ix)
                .p_2()
                .w_full()
                .child(
                    v_flex()
                        .p_2()
                        .w_full()
                        .gap_1()
                        .rounded(cx.theme().radius_lg)
                        .bg(cx.theme().surface_background)
                        .text_sm()
                        .child(
                            div()
                                .text_color(cx.theme().text_danger)
                                .child(message.reason.clone()),
                        )
                        .child(
                            h_flex()
                                .h_10()
                                .w_full()
                                .px_2()
                                .justify_between()
                                .bg(cx.theme().elevated_surface_background)
                                .border_1()
                                .border_color(cx.theme().border)
                                .rounded(cx.theme().radius)
                                .child(
                                    div()
                                        .truncate()
                                        .text_ellipsis()
                                        .text_xs()
                                        .line_height(relative(1.))
                                        .child(message.raw_event.clone()),
                                )
                                .child(
                                    Button::new(format!("copy-{ix}"))
                                        .icon(IconName::Copy)
                                        .ghost()
                                        .small()
                                        .on_click(cx.listener(move |this, _ev, _window, cx| {
                                            this.copy(ix, cx);
                                        })),
                                ),
                        ),
                )
                .into_any_element()
        } else {
            div().id(ix).into_any_element()
        }
    }
}

impl Panel for TrashPanel {
    fn panel_id(&self) -> SharedString {
        self.name.clone()
    }

    fn title(&self, _cx: &App) -> AnyElement {
        h_flex()
            .gap_1()
            .text_sm()
            .child(Icon::new(IconName::Warning).small())
            .child("Errors")
            .into_any_element()
    }
}

impl EventEmitter<PanelEvent> for TrashPanel {}

impl Focusable for TrashPanel {
    fn focus_handle(&self, _: &App) -> gpui::FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for TrashPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex().size_full().relative().child(
            v_flex()
                .flex_1()
                .relative()
                .child(
                    list(
                        self.list_state.clone(),
                        cx.processor(move |this, ix, window, cx| {
                            this.render_list_item(ix, window, cx)
                        }),
                    )
                    .size_full(),
                )
                .child(Scrollbar::vertical(&self.list_state)),
        )
    }
}
