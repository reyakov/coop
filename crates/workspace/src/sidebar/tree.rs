use std::rc::Rc;

use chat::Room;
use gpui::prelude::FluentBuilder;
use gpui::{
    App, ClickEvent, ElementId, Entity, InteractiveElement, IntoElement, ParentElement, RenderOnce,
    SharedString, StatefulInteractiveElement, Styled, Window, div, px,
};
use theme::ActiveTheme;
use ui::{Icon, IconName, Sizable, StyledExt, h_flex};

/// Collapsible tree sections; declaration order is render order.
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
}

/// One rendered tree row, in flattened order.
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
        entry: &'static CommunityEntry,
        depth: u8,
    },
    Hint {
        text: SharedString,
        depth: u8,
    },
}

/// A community shown under the Community section.
pub struct CommunityEntry {
    pub name: &'static str,
}

/// Communities to show until the Concord backend is wired up.
pub fn dummy_communities() -> &'static [CommunityEntry] {
    // TODO(concord): replace with ConcordRegistry communities, see docs/concord-usage.md.
    &[
        CommunityEntry {
            name: "Coop Contributors",
        },
        CommunityEntry {
            name: "Nostr Design",
        },
    ]
}

/// Presentation differences between the rows [`TreeRow`] draws.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TreeRowKind {
    Section,
    Community,
    Hint,
}

/// Folder/file row. One element for section headers, community rows and hints.
#[derive(IntoElement)]
pub struct TreeRow {
    id: ElementId,
    kind: TreeRowKind,
    depth: u8,
    caret: Option<IconName>,
    icon: Option<IconName>,
    avatar: Option<SharedString>,
    label: SharedString,
    count: Option<usize>,
    dot: bool,
    selected: bool,
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
            label: label.into(),
            count: None,
            dot: false,
            selected: false,
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

    pub fn avatar(mut self, name: impl Into<SharedString>) -> Self {
        self.avatar = Some(name.into());
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

    pub fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
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
        let avatar_initial = self
            .avatar
            .as_ref()
            .and_then(|name| name.chars().next())
            .map(|letter| SharedString::from(letter.to_uppercase().to_string()));
        let is_section = self.kind == TreeRowKind::Section;
        let is_community = self.kind == TreeRowKind::Community;
        let is_hint = self.kind == TreeRowKind::Hint;

        h_flex()
            .id(self.id)
            .h_8()
            .w_full()
            .pl(indent)
            .pr_1p5()
            .gap_2()
            .rounded(cx.theme().radius)
            .when(is_section, |this| {
                this.text_xs()
                    .font_semibold()
                    .text_color(cx.theme().text_muted)
            })
            .when(is_community, |this| this.text_sm())
            .when(is_hint, |this| {
                this.text_xs()
                    .font_normal()
                    .text_color(cx.theme().text_placeholder)
            })
            .when(self.selected, |this| {
                this.bg(cx.theme().ghost_element_selected)
            })
            .when_some(self.caret, |this, caret| {
                this.child(Icon::new(caret).xsmall().text_color(cx.theme().icon_muted))
            })
            .when_some(self.icon, |this, icon| {
                this.child(Icon::new(icon).small().text_color(cx.theme().icon_muted))
            })
            .when_some(avatar_initial, |this, initial| {
                this.child(
                    div()
                        .flex_shrink_0()
                        .size_5()
                        .rounded_full()
                        .bg(cx.theme().element_background)
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_xs()
                        .text_color(cx.theme().text)
                        .child(initial),
                )
            })
            .child(div().flex_1().truncate().child(self.label))
            .when_some(self.count, |this, count| {
                this.child(
                    div()
                        .flex_shrink_0()
                        .text_xs()
                        .text_color(cx.theme().text_placeholder)
                        .child(count.to_string()),
                )
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
