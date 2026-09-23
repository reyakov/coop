use std::rc::Rc;

use chat::Room;
use community::Community;
use gpui::prelude::FluentBuilder;
use gpui::{
    App, ClickEvent, ElementId, Entity, ImageSource, InteractiveElement, IntoElement,
    ParentElement, RenderOnce, SharedString, StatefulInteractiveElement, Styled, Window, div, px,
};
use settings::AppSettings;
use theme::ActiveTheme;
use ui::avatar::{Avatar, PixelAvatar};
use ui::{Icon, IconName, Selectable, Sizable, StyledExt, h_flex};

use super::tab::SidebarTab;

pub enum SidebarRow {
    Section {
        label: SharedString,
        count: usize,
    },
    Room {
        room: Entity<Room>,
    },
    Community {
        community: Entity<Community>,
    },
    Action {
        label: SharedString,
        tab: SidebarTab,
    },
    Hint {
        text: SharedString,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TreeRowKind {
    Section,
    Room,
    Community,
    Action,
    Hint,
}

#[derive(IntoElement)]
pub struct TreeRow {
    id: ElementId,
    kind: TreeRowKind,
    label: SharedString,
    avatar: Option<SharedString>,
    picture: Option<ImageSource>,
    icon: Option<IconName>,
    count: Option<usize>,
    created_at: Option<SharedString>,
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
            label: label.into(),
            avatar: None,
            picture: None,
            icon: None,
            count: None,
            created_at: None,
            selected: false,
            on_click: None,
        }
    }

    /// Sets the seed for the row's generated avatar.
    pub fn avatar(mut self, seed: impl Into<SharedString>) -> Self {
        self.avatar = Some(seed.into());
        self
    }

    /// Shows `picture` instead of the generated avatar.
    pub fn picture(mut self, picture: Option<impl Into<ImageSource>>) -> Self {
        self.picture = picture.map(Into::into);
        self
    }

    /// Shows `icon` in the avatar slot when the row has no avatar or picture.
    pub fn icon(mut self, icon: IconName) -> Self {
        self.icon = Some(icon);
        self
    }

    pub fn count(mut self, count: usize) -> Self {
        self.count = Some(count);
        self
    }

    pub fn created_at(mut self, created_at: impl Into<SharedString>) -> Self {
        self.created_at = Some(created_at.into());
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

impl Selectable for TreeRow {
    fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    fn is_selected(&self) -> bool {
        self.selected
    }
}

impl RenderOnce for TreeRow {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let hide_avatar = AppSettings::get_hide_avatar(cx);

        let is_section = self.kind == TreeRowKind::Section;
        let is_room = self.kind == TreeRowKind::Room;
        let is_community = self.kind == TreeRowKind::Community;
        let is_action = self.kind == TreeRowKind::Action;
        let is_hint = self.kind == TreeRowKind::Hint;
        let is_selected = self.selected;

        let avatar = if hide_avatar {
            None
        } else {
            match (self.avatar, self.picture) {
                (None, None) => None,
                (seed, Some(picture)) => Some(
                    Avatar::from_source(picture)
                        .when_some(seed, |avatar, seed| avatar.seed(seed))
                        .xsmall()
                        .flex_shrink_0()
                        .into_any_element(),
                ),
                (Some(seed), None) => Some(
                    PixelAvatar::new(seed)
                        .xsmall()
                        .flex_shrink_0()
                        .into_any_element(),
                ),
            }
        };

        let avatar = avatar.or_else(|| {
            self.icon.map(|icon| {
                h_flex()
                    .flex_shrink_0()
                    .w(px(20.))
                    .justify_center()
                    .text_color(cx.theme().icon_muted)
                    .child(Icon::new(icon).small())
                    .into_any_element()
            })
        });

        h_flex()
            .id(self.id)
            .h_8()
            .w_full()
            .px_2()
            .gap_2()
            .rounded(cx.theme().radius)
            .when(is_section, |this| {
                this.text_xs()
                    .text_color(cx.theme().text_placeholder)
                    .font_semibold()
            })
            .when(is_room || is_community, |this| this.text_sm())
            .when(is_action, |this| {
                this.text_sm().text_color(cx.theme().text_muted)
            })
            .when(is_hint, |this| {
                this.text_xs()
                    .font_normal()
                    .text_color(cx.theme().text_placeholder)
            })
            .when_some(avatar, |this, avatar| this.child(avatar))
            .child(
                h_flex()
                    .gap_1()
                    .flex_1()
                    .child(
                        div()
                            .truncate()
                            .min_w_0()
                            .when(is_room, |this| this.font_medium())
                            .child(self.label),
                    )
                    .when(is_selected, |this| {
                        this.child(
                            Icon::new(IconName::CheckCircle)
                                .small()
                                .flex_shrink_0()
                                .text_color(cx.theme().icon_accent),
                        )
                    })
                    .when_some(self.count, |this, count| {
                        this.child(div().flex_shrink_0().font_normal().child(count.to_string()))
                    })
                    .when_some(self.created_at, |this, created_at| {
                        this.child(div().flex_1()).child(
                            div()
                                .flex_shrink_0()
                                .text_color(cx.theme().text_placeholder)
                                .text_xs()
                                .child(created_at),
                        )
                    }),
            )
            .when_some(self.on_click, |this, handler| {
                this.cursor_pointer()
                    .when(!is_section, |this| {
                        this.hover(|this| this.bg(cx.theme().ghost_element_hover))
                    })
                    .on_click(move |event, window, cx| handler(event, window, cx))
            })
    }
}
