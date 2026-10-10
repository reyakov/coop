use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, Context, Hsla, IntoElement, Render, RenderOnce, SharedString, StyleRefinement,
    Styled, Svg, Window, svg,
};
use theme::ActiveTheme;

use crate::{Sizable, Size};

impl From<IconName> for Icon {
    fn from(value: IconName) -> Self {
        Icon::build(value)
    }
}

#[derive(IntoElement, Clone)]
pub enum IconName {
    ArrowLeft,
    ArrowRight,
    ArrowDownCircle,
    Book,
    Ban,
    ChevronDown,
    CaretDown,
    CaretRight,
    Check,
    CheckCircle,
    Close,
    CloseCircle,
    CloseCircleFill,
    Compass,
    Copy,
    Door,
    Ellipsis,
    Emoji,
    Eye,
    Input,
    Info,
    Invite,
    Inbox,
    Link,
    Loader,
    Lock,
    Login,
    Logout,
    Message,
    Moon,
    Plus,
    PlusChat,
    PlusCircle,
    Profile,
    Relay,
    Reply,
    Search,
    Settings,
    Settings2,
    Sun,
    Group,
    Upload,
    Edit,
    PanelLeft,
    PanelLeftOpen,
    PanelRight,
    PanelRightOpen,
    PanelBottom,
    PanelBottomOpen,
    PaperPlaneFill,
    Warning,
    WindowClose,
    WindowMaximize,
    WindowMinimize,
    WindowRestore,
}

impl IconName {
    fn path(self) -> SharedString {
        match self {
            Self::ArrowLeft => "icons/arrow-left.svg",
            Self::ArrowRight => "icons/arrow-right.svg",
            Self::ArrowDownCircle => "icons/arrow-down-circle.svg",
            Self::Book => "icons/book.svg",
            Self::Ban => "icons/ban.svg",
            Self::ChevronDown => "icons/chevron-down.svg",
            Self::CaretDown => "icons/caret-down.svg",
            Self::CaretRight => "icons/caret-right.svg",
            Self::Check => "icons/check.svg",
            Self::CheckCircle => "icons/check-circle.svg",
            Self::Close => "icons/close.svg",
            Self::CloseCircle => "icons/close-circle.svg",
            Self::CloseCircleFill => "icons/close-circle-fill.svg",
            Self::Compass => "icons/compass.svg",
            Self::Copy => "icons/copy.svg",
            Self::Door => "icons/door.svg",
            Self::Ellipsis => "icons/ellipsis.svg",
            Self::Emoji => "icons/emoji.svg",
            Self::Eye => "icons/eye.svg",
            Self::Input => "icons/input.svg",
            Self::Info => "icons/info.svg",
            Self::Invite => "icons/invite.svg",
            Self::Inbox => "icons/inbox.svg",
            Self::Link => "icons/link.svg",
            Self::Loader => "icons/loader.svg",
            Self::Lock => "icons/lock.svg",
            Self::Login => "icons/login.svg",
            Self::Logout => "icons/logout.svg",
            Self::Message => "icons/message.svg",
            Self::Moon => "icons/moon.svg",
            Self::Plus => "icons/plus.svg",
            Self::PlusChat => "icons/plus-chat.svg",
            Self::PlusCircle => "icons/plus-circle.svg",
            Self::Profile => "icons/profile.svg",
            Self::Relay => "icons/relay.svg",
            Self::Reply => "icons/reply.svg",
            Self::Search => "icons/search.svg",
            Self::Settings => "icons/settings.svg",
            Self::Settings2 => "icons/settings2.svg",
            Self::Sun => "icons/sun.svg",
            Self::Upload => "icons/upload.svg",
            Self::Group => "icons/group.svg",
            Self::PanelLeft => "icons/panel-left.svg",
            Self::PanelLeftOpen => "icons/panel-left-open.svg",
            Self::PanelRight => "icons/panel-right.svg",
            Self::PanelRightOpen => "icons/panel-right-open.svg",
            Self::PanelBottom => "icons/panel-bottom.svg",
            Self::PanelBottomOpen => "icons/panel-bottom-open.svg",
            Self::PaperPlaneFill => "icons/paper-plane-fill.svg",
            Self::Edit => "icons/edit.svg",
            Self::Warning => "icons/warning.svg",
            Self::WindowClose => "icons/window-close.svg",
            Self::WindowMaximize => "icons/window-maximize.svg",
            Self::WindowMinimize => "icons/window-minimize.svg",
            Self::WindowRestore => "icons/window-restore.svg",
        }
        .into()
    }
}

impl From<IconName> for AnyElement {
    fn from(val: IconName) -> Self {
        Icon::build(val).into_any_element()
    }
}

impl RenderOnce for IconName {
    fn render(self, _: &mut Window, _cx: &mut App) -> impl IntoElement {
        Icon::build(self)
    }
}

#[derive(IntoElement)]
pub struct Icon {
    base: Svg,
    style: StyleRefinement,
    path: SharedString,
    text_color: Option<Hsla>,
    size: Option<Size>,
}

impl Default for Icon {
    fn default() -> Self {
        Self {
            base: svg().flex_none().size_4(),
            style: StyleRefinement::default(),
            path: "".into(),
            text_color: None,
            size: None,
        }
    }
}

impl Clone for Icon {
    fn clone(&self) -> Self {
        let mut this = Self::default().path(self.path.clone());
        this.style = self.style.clone();
        this.size = self.size;
        this.text_color = self.text_color;
        this
    }
}

impl Icon {
    pub fn new(icon: impl Into<Icon>) -> Self {
        icon.into()
    }

    fn build(name: IconName) -> Self {
        Self::default().path(name.path())
    }

    /// Sets the icon path, relative to the assets bundle (for example `icons/foo.svg`).
    pub fn path(mut self, path: impl Into<SharedString>) -> Self {
        self.path = path.into();
        self
    }

    pub fn transform(mut self, transformation: gpui::Transformation) -> Self {
        self.base = self.base.with_transformation(transformation);
        self
    }

    pub fn empty() -> Self {
        Self::default()
    }
}

impl Styled for Icon {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }

    fn text_color(mut self, color: impl Into<Hsla>) -> Self {
        self.text_color = Some(color.into());
        self
    }
}

impl Sizable for Icon {
    fn with_size(mut self, size: impl Into<Size>) -> Self {
        self.size = Some(size.into());
        self
    }
}

impl RenderOnce for Icon {
    fn render(self, window: &mut Window, _cx: &mut App) -> impl IntoElement {
        let text_color = self.text_color.unwrap_or_else(|| window.text_style().color);
        let text_size = window.text_style().font_size.to_pixels(window.rem_size());
        let has_base_size = self.style.size.width.is_some() || self.style.size.height.is_some();

        let mut base = self.base;
        *base.style() = self.style;

        base.flex_shrink_0()
            .text_color(text_color)
            .when(!has_base_size, |this| this.size(text_size))
            .when_some(self.size, |this, size| match size {
                Size::Size(px) => this.size(px),
                Size::XSmall => this.size_3(),
                Size::Small => this.size_4(),
                Size::Medium => this.size_5(),
                Size::Large => this.size_6(),
            })
            .path(self.path)
    }
}

impl From<Icon> for AnyElement {
    fn from(val: Icon) -> Self {
        val.into_any_element()
    }
}

impl Render for Icon {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let text_color = self.text_color.unwrap_or_else(|| cx.theme().text_muted);
        let text_size = window.text_style().font_size.to_pixels(window.rem_size());
        let has_base_size = self.style.size.width.is_some() || self.style.size.height.is_some();

        let mut base = svg().flex_none();
        *base.style() = self.style.clone();

        base.flex_shrink_0()
            .text_color(text_color)
            .when(!has_base_size, |this| this.size(text_size))
            .when_some(self.size, |this, size| match size {
                Size::Size(px) => this.size(px),
                Size::XSmall => this.size_3(),
                Size::Small => this.size_4(),
                Size::Medium => this.size_5(),
                Size::Large => this.size_6(),
            })
            .path(self.path.clone())
    }
}
