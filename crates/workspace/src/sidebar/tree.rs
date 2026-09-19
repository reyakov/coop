use std::path::PathBuf;
use std::rc::Rc;

use chat::Room;
use community::Community;
use gpui::prelude::FluentBuilder;
use gpui::{
    App, ClickEvent, ElementId, Entity, InteractiveElement, IntoElement, ParentElement, RenderOnce,
    SharedString, StatefulInteractiveElement, Styled, Window, div, px,
};
use theme::ActiveTheme;
use ui::avatar::{Avatar, PixelAvatar};
use ui::{Icon, IconName, Sizable, StyledExt, h_flex};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum TreeSection {
    Pins,
    Requests,
    Community,
    Messages,
}

impl TreeSection {
    pub fn label(self) -> &'static str {
        match self {
            Self::Pins => "Pinned",
            Self::Requests => "Requests",
            Self::Community => "Community",
            Self::Messages => "Messages",
        }
    }

    pub fn icon(self) -> IconName {
        match self {
            Self::Pins | Self::Requests | Self::Community => IconName::Folder,
            Self::Messages => IconName::Message,
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            Self::Pins => "pins",
            Self::Requests => "requests",
            Self::Community => "community",
            Self::Messages => "messages",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        match key {
            "pins" => Some(Self::Pins),
            "requests" => Some(Self::Requests),
            "community" => Some(Self::Community),
            "messages" => Some(Self::Messages),
            _ => None,
        }
    }
}

pub enum SidebarRow {
    Section {
        section: TreeSection,
        count: usize,
    },
    Room {
        room: Entity<Room>,
        depth: u8,
        pinned: bool,
    },
    Community {
        community: Entity<Community>,
        depth: u8,
    },
    NewCommunity {
        depth: u8,
    },
    Hint {
        text: SharedString,
        depth: u8,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TreeRowKind {
    Section,
    Community,
    Hint,
}

#[derive(IntoElement)]
pub struct TreeRow {
    id: ElementId,
    kind: TreeRowKind,
    depth: u8,
    caret: Option<IconName>,
    icon: Option<IconName>,
    avatar: Option<SharedString>,
    picture: Option<PathBuf>,
    label: SharedString,
    count: Option<usize>,
    dot: bool,
    #[allow(clippy::type_complexity)]
    on_click: Option<Rc<dyn Fn(&ClickEvent, &mut Window, &mut App)>>,
}

impl TreeRow {
    pub fn new(
        id: impl Into<ElementId>,
        kind: TreeRowKind,
        label: impl Into<SharedString>,
    ) -> Self {
        Self {
            id: id.into(),
            kind,
            depth: 0,
            caret: None,
            icon: None,
            avatar: None,
            picture: None,
            label: label.into(),
            count: None,
            dot: false,
            on_click: None,
        }
    }

    pub fn depth(mut self, depth: u8) -> Self {
        self.depth = depth;
        self
    }

    pub fn caret(mut self, caret: IconName) -> Self {
        self.caret = Some(caret);
        self
    }

    pub fn icon(mut self, icon: IconName) -> Self {
        self.icon = Some(icon);
        self
    }

    /// Sets the seed for the row's generated avatar.
    pub fn avatar(mut self, seed: impl Into<SharedString>) -> Self {
        self.avatar = Some(seed.into());
        self
    }

    /// Shows `picture` instead of the generated avatar.
    pub fn picture(mut self, picture: Option<PathBuf>) -> Self {
        self.picture = picture;
        self
    }

    pub fn count(mut self, count: usize) -> Self {
        self.count = Some(count);
        self
    }

    pub fn dot(mut self) -> Self {
        self.dot = true;
        self
    }

    pub fn on_click(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_click = Some(Rc::new(handler));
        self
    }
}

impl RenderOnce for TreeRow {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let indent = px(6. + self.depth as f32 * 14.);
        let is_section = self.kind == TreeRowKind::Section;
        let is_community = self.kind == TreeRowKind::Community;
        let is_hint = self.kind == TreeRowKind::Hint;

        let avatar = match (self.avatar, self.picture) {
            (seed, Some(picture)) => Some(
                Avatar::from_source(picture)
                    .when_some(seed, |avatar, seed| avatar.seed(seed))
                    .xsmall()
                    .into_any_element(),
            ),
            (Some(seed), None) => Some(PixelAvatar::new(seed).xsmall().into_any_element()),
            (None, None) => None,
        };

        h_flex()
            .id(self.id)
            .h_8()
            .w_full()
            .pl(indent)
            .pr_1p5()
            .gap_2()
            .rounded(cx.theme().radius)
            .when(is_section, |this| {
                this.text_xs().text_color(cx.theme().text_muted)
            })
            .when(is_community, |this| this.text_sm())
            .when(is_hint, |this| {
                this.text_xs()
                    .font_normal()
                    .text_color(cx.theme().text_placeholder)
            })
            .when_some(self.icon, |this, icon| {
                this.child(Icon::new(icon).small().text_color(cx.theme().icon_muted))
            })
            .when_some(avatar, |this, avatar| this.child(avatar))
            .child(
                h_flex()
                    .gap_1()
                    .flex_1()
                    .child(div().truncate().min_w_0().child(self.label))
                    .when_some(self.count, |this, count| {
                        this.child(
                            div()
                                .flex_shrink_0()
                                .text_xs()
                                .text_color(cx.theme().text_placeholder)
                                .font_semibold()
                                .child(count.to_string()),
                        )
                    }),
            )
            .when_some(self.caret, |this, caret| {
                this.child(Icon::new(caret).xsmall().text_color(cx.theme().icon_muted))
            })
            .when(self.dot, |this| {
                this.child(
                    div()
                        .flex_shrink_0()
                        .size_1()
                        .rounded_full()
                        .bg(cx.theme().cursor),
                )
            })
            .when_some(self.on_click, |this, handler| {
                this.cursor_pointer()
                    .hover(|this| this.bg(cx.theme().ghost_element_hover))
                    .on_click(move |event, window, cx| handler(event, window, cx))
            })
    }
}
