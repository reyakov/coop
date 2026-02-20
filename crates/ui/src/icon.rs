use gpui::prelude::FluentBuilder as _;
use gpui::{
    svg, AnyElement, App, AppContext, Entity, Hsla, IntoElement, Radians, Render, RenderOnce,
    SharedString, StyleRefinement, Styled, Svg, Transformation, Window,
};
use theme::ActiveTheme;

use crate::{Sizable, Size};

#[derive(IntoElement, Clone)]
pub enum IconName {
    ArrowLeft,
    ArrowRight,
    Boom,
    ChevronDown,
    CaretDown,
    CaretRight,
    CaretUp,
    Check,
    CheckCircle,
    Close,
    CloseCircle,
    CloseCircleFill,
    Copy,
    Door,
    Ellipsis,
    Emoji,
    Eye,
    Info,
    Invite,
    Inbox,
    InboxFill,
    Link,
    Loader,
    Moon,
    Plus,
    PlusCircle,
    Profile,
    Relay,
    Reply,
    Search,
    Settings,
    Sun,
    Ship,
    Shield,
    Upload,
    Usb,
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
    Fistbump,
    FistbumpFill,
    Zoom,
}

impl IconName {
    pub fn path(self) -> SharedString {
        match self {
            Self::ArrowLeft => "icons/arrow-left.svg",
            Self::ArrowRight => "icons/arrow-right.svg",
            Self::Boom => "icons/boom.svg",
            Self::ChevronDown => "icons/chevron-down.svg",
            Self::CaretDown => "icons/caret-down.svg",
            Self::CaretRight => "icons/caret-right.svg",
            Self::CaretUp => "icons/caret-up.svg",
            Self::Check => "icons/check.svg",
            Self::CheckCircle => "icons/check-circle.svg",
            Self::Close => "icons/close.svg",
            Self::CloseCircle => "icons/close-circle.svg",
            Self::CloseCircleFill => "icons/close-circle-fill.svg",
            Self::Copy => "icons/copy.svg",
            Self::Door => "icons/door.svg",
            Self::Ellipsis => "icons/ellipsis.svg",
            Self::Emoji => "icons/emoji.svg",
            Self::Eye => "icons/eye.svg",
            Self::Info => "icons/info.svg",
            Self::Invite => "icons/invite.svg",
            Self::Inbox => "icons/inbox.svg",
            Self::InboxFill => "icons/inbox-fill.svg",
            Self::Link => "icons/link.svg",
            Self::Loader => "icons/loader.svg",
            Self::Moon => "icons/moon.svg",
            Self::Plus => "icons/plus.svg",
            Self::PlusCircle => "icons/plus-circle.svg",
            Self::Profile => "icons/profile.svg",
            Self::Relay => "icons/relay.svg",
            Self::Reply => "icons/reply.svg",
            Self::Search => "icons/search.svg",
            Self::Settings => "icons/settings.svg",
            Self::Sun => "icons/sun.svg",
            Self::Ship => "icons/ship.svg",
            Self::Shield => "icons/shield.svg",
            Self::Upload => "icons/upload.svg",
            Self::Usb => "icons/usb.svg",
            Self::PanelLeft => "icons/panel-left.svg",
            Self::PanelLeftOpen => "icons/panel-left-open.svg",
            Self::PanelRight => "icons/panel-right.svg",
            Self::PanelRightOpen => "icons/panel-right-open.svg",
            Self::PanelBottom => "icons/panel-bottom.svg",
            Self::PanelBottomOpen => "icons/panel-bottom-open.svg",
            Self::PaperPlaneFill => "icons/paper-plane-fill.svg",
            Self::Warning => "icons/warning.svg",
            Self::WindowClose => "icons/window-close.svg",
            Self::WindowMaximize => "icons/window-maximize.svg",
            Self::WindowMinimize => "icons/window-minimize.svg",
            Self::WindowRestore => "icons/window-restore.svg",
            Self::Fistbump => "icons/fistbump.svg",
            Self::FistbumpFill => "icons/fistbump-fill.svg",
            Self::Zoom => "icons/zoom.svg",
        }
        .into()
    }

    /// Return the icon as a Entity<Icon>
    pub fn view(self, window: &mut Window, cx: &mut App) -> Entity<Icon> {
        Icon::build(self).view(window, cx)
    }
}

impl From<IconName> for Icon {
    fn from(val: IconName) -> Self {
        Icon::build(val)
    }
}

impl From<IconName> for AnyElement {
    fn from(val: IconName) -> Self {
        Icon::build(val).into_any_element()
    }
}

impl RenderOnce for IconName {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        Icon::build(self)
    }
}

#[derive(IntoElement)]
pub struct Icon {
    base: Svg,
    path: SharedString,
    text_color: Option<Hsla>,
    size: Option<Size>,
    rotation: Option<Radians>,
}

impl Default for Icon {
    fn default() -> Self {
        Self {
            base: svg().flex_none().size_4(),
            path: "".into(),
            text_color: None,
            size: None,
            rotation: None,
        }
    }
}

impl Clone for Icon {
    fn clone(&self) -> Self {
        let mut this = Self::default().path(self.path.clone());
        if let Some(size) = self.size {
            this = this.with_size(size);
        }
        this
    }
}

pub trait IconNamed {
    fn path(&self) -> SharedString;
}

impl Icon {
    pub fn new(icon: impl Into<Icon>) -> Self {
        icon.into()
    }

    fn build(name: IconName) -> Self {
        Self::default().path(name.path())
    }

    /// Set the icon path of the Assets bundle
    ///
    /// For example: `icons/foo.svg`
    pub fn path(mut self, path: impl Into<SharedString>) -> Self {
        self.path = path.into();
        self
    }

    /// Create a new view for the icon
    pub fn view(self, _window: &mut Window, cx: &mut App) -> Entity<Icon> {
        cx.new(|_| self)
    }

    pub fn transform(mut self, transformation: gpui::Transformation) -> Self {
        self.base = self.base.with_transformation(transformation);
        self
    }

    pub fn empty() -> Self {
        Self::default()
    }

    /// Rotate the icon by the given angle
    pub fn rotate(mut self, radians: impl Into<Radians>) -> Self {
        self.base = self
            .base
            .with_transformation(Transformation::rotate(radians));
        self
    }
}

impl Styled for Icon {
    fn style(&mut self) -> &mut StyleRefinement {
        self.base.style()
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

        self.base
            .text_color(text_color)
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
    fn render(
        &mut self,
        _window: &mut gpui::Window,
        cx: &mut gpui::Context<Self>,
    ) -> impl IntoElement {
        let text_color = self.text_color.unwrap_or_else(|| cx.theme().icon);

        svg()
            .flex_none()
            .text_color(text_color)
            .when_some(self.size, |this, size| match size {
                Size::Size(px) => this.size(px),
                Size::XSmall => this.size_3(),
                Size::Small => this.size_4(),
                Size::Medium => this.size_5(),
                Size::Large => this.size_6(),
            })
            .when(!self.path.is_empty(), |this| this.path(self.path.clone()))
            .when_some(self.rotation, |this, rotation| {
                this.with_transformation(Transformation::rotate(rotation))
            })
    }
}
