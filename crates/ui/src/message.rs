use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, ClickEvent, ElementId, InteractiveElement as _, IntoElement, MouseButton,
    MouseDownEvent, ParentElement as _, RenderOnce, SharedString, StyleRefinement, Styled, Window,
    div, px,
};
use smallvec::SmallVec;
use theme::ActiveTheme;

use crate::{InteractiveElementExt as _, StyledExt as _, h_flex, v_flex};

type MouseDownListener = Box<dyn Fn(&MouseDownEvent, &mut Window, &mut App) + 'static>;
type DoubleClickListener = Box<dyn Fn(&ClickEvent, &mut Window, &mut App) + 'static>;

/// A single message row in a chat-like list.
#[derive(IntoElement)]
pub struct MessageRow {
    id: ElementId,
    style: StyleRefinement,
    show_author: bool,
    hide_avatar: bool,
    avatar: Option<AnyElement>,
    author: Option<SharedString>,
    timestamp: Option<SharedString>,
    header_extras: SmallVec<[AnyElement; 1]>,
    body: SmallVec<[AnyElement; 4]>,
    overlays: SmallVec<[AnyElement; 2]>,
    on_mouse_down: Option<(MouseButton, MouseDownListener)>,
    on_double_click: Option<DoubleClickListener>,
}

impl MessageRow {
    /// Create a message row with the given element id.
    pub fn new(id: impl Into<ElementId>) -> Self {
        Self {
            id: id.into(),
            style: StyleRefinement::default(),
            show_author: false,
            hide_avatar: false,
            avatar: None,
            author: None,
            timestamp: None,
            header_extras: SmallVec::new(),
            body: SmallVec::new(),
            overlays: SmallVec::new(),
            on_mouse_down: None,
            on_double_click: None,
        }
    }

    /// Whether this row opens a run of messages from one author.
    #[must_use]
    pub fn show_author(mut self, show_author: bool) -> Self {
        self.show_author = show_author;
        self
    }

    /// Hide the avatar column entirely.
    #[must_use]
    pub fn hide_avatar(mut self, hide_avatar: bool) -> Self {
        self.hide_avatar = hide_avatar;
        self
    }

    /// The avatar shown when this row opens a run of messages.
    #[must_use]
    pub fn avatar(mut self, avatar: impl IntoElement) -> Self {
        self.avatar = Some(avatar.into_any_element());
        self
    }

    /// The author's display name, shown next to the timestamp.
    #[must_use]
    pub fn author(mut self, author: impl Into<SharedString>) -> Self {
        self.author = Some(author.into());
        self
    }

    /// The time the message was sent.
    #[must_use]
    pub fn timestamp(mut self, timestamp: impl Into<SharedString>) -> Self {
        self.timestamp = Some(timestamp.into());
        self
    }

    /// Append an element to the header row.
    #[must_use]
    pub fn header_extra(mut self, extra: impl IntoElement) -> Self {
        self.header_extras.push(extra.into_any_element());
        self
    }

    /// Append an element to the message body, in order.
    #[must_use]
    pub fn child(mut self, child: impl IntoElement) -> Self {
        self.body.push(child.into_any_element());
        self
    }

    /// Append several elements to the message body, in order.
    #[must_use]
    pub fn children(mut self, children: impl IntoIterator<Item = impl IntoElement>) -> Self {
        self.body
            .extend(children.into_iter().map(|child| child.into_any_element()));
        self
    }

    /// Append an element positioned over the row.
    #[must_use]
    pub fn overlay(mut self, overlay: impl IntoElement) -> Self {
        self.overlays.push(overlay.into_any_element());
        self
    }

    /// Handle a mouse button press anywhere on the row.
    #[must_use]
    pub fn on_mouse_down(
        mut self,
        button: MouseButton,
        listener: impl Fn(&MouseDownEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_mouse_down = Some((button, Box::new(listener)));
        self
    }

    /// Handle a double click anywhere on the row.
    #[must_use]
    pub fn on_double_click(
        mut self,
        listener: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_double_click = Some(Box::new(listener));
        self
    }
}

impl Styled for MessageRow {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

impl RenderOnce for MessageRow {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let Self {
            id,
            style,
            show_author,
            hide_avatar,
            avatar,
            author,
            timestamp,
            header_extras,
            body,
            overlays,
            on_mouse_down,
            on_double_click,
        } = self;

        div()
            .id(id)
            .group("")
            .relative()
            .w_full()
            .py_1()
            .when(show_author, |this| this.pt_2())
            .px_3()
            .refine_style(&style)
            .child(
                h_flex()
                    .items_start()
                    .gap_3()
                    .when(!hide_avatar, |this| {
                        if show_author {
                            match avatar {
                                Some(avatar) => this.child(avatar),
                                None => this.child(div().flex_shrink_0().w(px(32.))),
                            }
                        } else {
                            this.child(div().flex_shrink_0().w(px(32.)))
                        }
                    })
                    .child(
                        v_flex()
                            .flex_1()
                            .w_full()
                            .min_w_0()
                            .flex_initial()
                            .overflow_hidden()
                            .when(show_author, |this| {
                                this.child(
                                    h_flex()
                                        .gap_2()
                                        .text_sm()
                                        .text_color(cx.theme().text_placeholder)
                                        .when_some(author, |this, author| {
                                            this.child(div().font_semibold().child(author))
                                        })
                                        .when_some(timestamp, |this, timestamp| {
                                            this.child(timestamp)
                                        })
                                        .children(header_extras),
                                )
                            })
                            .children(body),
                    ),
            )
            .children(overlays)
            .when_some(on_mouse_down, |this, (button, listener)| {
                this.on_mouse_down(button, listener)
            })
            .when_some(on_double_click, |this, listener| {
                this.on_double_click(listener)
            })
            .hover(|this| this.bg(cx.theme().surface_background))
            .into_any_element()
    }
}

/// A welcome message shown at the top of a message list.
#[derive(IntoElement)]
pub struct WelcomeMessage {
    id: ElementId,
    style: StyleRefinement,
    icon: Option<AnyElement>,
    title: Option<SharedString>,
    message: Option<SharedString>,
    children: SmallVec<[AnyElement; 2]>,
}

impl WelcomeMessage {
    /// Create a welcome message with the given element id.
    pub fn new(id: impl Into<ElementId>) -> Self {
        Self {
            id: id.into(),
            style: StyleRefinement::default(),
            icon: None,
            title: None,
            message: None,
            children: SmallVec::new(),
        }
    }

    /// The icon shown above the title.
    #[must_use]
    pub fn icon(mut self, icon: impl IntoElement) -> Self {
        self.icon = Some(icon.into_any_element());
        self
    }

    /// The welcome title.
    #[must_use]
    pub fn title(mut self, title: impl Into<SharedString>) -> Self {
        self.title = Some(title.into());
        self
    }

    /// The welcome body text.
    #[must_use]
    pub fn message(mut self, message: impl Into<SharedString>) -> Self {
        self.message = Some(message.into());
        self
    }

    /// Append an element below the body text.
    #[must_use]
    pub fn child(mut self, child: impl IntoElement) -> Self {
        self.children.push(child.into_any_element());
        self
    }
}

impl Styled for WelcomeMessage {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

impl RenderOnce for WelcomeMessage {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let Self {
            id,
            style,
            icon,
            title,
            message,
            children,
        } = self;

        v_flex()
            .id(id)
            .w_full()
            .gap_2()
            .p_3()
            .items_center()
            .justify_center()
            .text_center()
            .refine_style(&style)
            .when_some(icon, |this, icon| this.child(icon))
            .child(
                v_flex()
                    .items_center()
                    .justify_center()
                    .text_center()
                    .when_some(title, |this, title| {
                        this.child(
                            div()
                                .text_sm()
                                .font_semibold()
                                .text_color(cx.theme().text)
                                .child(title),
                        )
                    })
                    .when_some(message, |this, message| {
                        this.child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().text_placeholder)
                                .child(message),
                        )
                    }),
            )
            .children(children)
    }
}
