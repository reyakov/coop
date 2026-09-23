use std::rc::Rc;

use gpui::prelude::FluentBuilder;
use gpui::{App, InteractiveElement, IntoElement, ParentElement, RenderOnce, Styled, Window, div};
use theme::ActiveTheme;
use ui::button::{Button, ButtonVariants};
use ui::{IconName, Selectable, h_flex};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SidebarTab {
    Recents,
    Chats,
    Communities,
}

impl SidebarTab {
    pub const ALL: [SidebarTab; 3] = [Self::Recents, Self::Chats, Self::Communities];

    pub fn label(self) -> &'static str {
        match self {
            Self::Recents => "Recents",
            Self::Chats => "Chats",
            Self::Communities => "Communities",
        }
    }

    pub fn icon(self) -> IconName {
        match self {
            Self::Recents => IconName::History,
            Self::Chats => IconName::Message,
            Self::Communities => IconName::Group,
        }
    }

    pub fn list_id(self) -> &'static str {
        match self {
            Self::Recents => "sidebar-recents",
            Self::Chats => "sidebar-chats",
            Self::Communities => "sidebar-communities",
        }
    }

    pub fn index(self) -> usize {
        match self {
            Self::Recents => 0,
            Self::Chats => 1,
            Self::Communities => 2,
        }
    }

    pub fn chat(self) -> bool {
        matches!(self, Self::Chats)
    }

    pub fn community(self) -> bool {
        matches!(self, Self::Communities)
    }
}

#[derive(IntoElement)]
#[allow(clippy::type_complexity)]
pub struct TabBar {
    active: SidebarTab,
    on_select: Option<Rc<dyn Fn(SidebarTab, &mut Window, &mut App)>>,
}

impl TabBar {
    pub fn new(active: SidebarTab) -> Self {
        Self {
            active,
            on_select: None,
        }
    }

    pub fn on_select(
        mut self,
        handler: impl Fn(SidebarTab, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_select = Some(Rc::new(handler));
        self
    }
}

impl RenderOnce for TabBar {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let Self { active, on_select } = self;

        div()
            .id("sidebar-tabs")
            .absolute()
            .bottom_3()
            .left_0()
            .w_full()
            .px_4()
            .child(
                h_flex()
                    .w_full()
                    .p_1()
                    .gap_1()
                    .rounded_full()
                    .bg(cx.theme().background)
                    .when(cx.theme().shadow, |this| this.shadow_md())
                    .children(SidebarTab::ALL.into_iter().map(|tab| {
                        let on_select = on_select.clone();

                        Button::new(format!("tab-{}", tab.list_id()))
                            .icon(tab.icon())
                            .ghost()
                            .flex_1()
                            .rounded()
                            .selected(tab == active)
                            .tooltip(tab.label())
                            .on_click(move |_event, window, cx| {
                                if let Some(on_select) = on_select.as_ref() {
                                    on_select(tab, window, cx);
                                }
                            })
                    })),
            )
    }
}
