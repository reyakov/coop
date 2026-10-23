use gpui::SharedString;
use gpui_kit::assets::IconNamed;

/// Coop icons kept from before the gpui-kit migration; absent from the kit bundle.
#[derive(Clone, Copy)]
pub enum IconName {
    ArrowDownCircle,
    ArrowLeft,
    Book,
    CaretDown,
    CaretRight,
    CheckCircle,
    Close,
    Compass,
    Door,
    Emoji,
    Group,
    Input,
    Invite,
    Link,
    Lock,
    Message,
    PaperPlaneFill,
    Plus,
    PlusChat,
    Profile,
    Relay,
    Search,
    Settings,
    Settings2,
    Upload,
    Warning,
}

impl IconNamed for IconName {
    fn path(self) -> SharedString {
        match self {
            Self::ArrowDownCircle => "icons/arrow-down-circle.svg",
            Self::ArrowLeft => "icons/arrow-left.svg",
            Self::Book => "icons/book.svg",
            Self::CaretDown => "icons/caret-down.svg",
            Self::CaretRight => "icons/caret-right.svg",
            Self::CheckCircle => "icons/check-circle.svg",
            Self::Close => "icons/close.svg",
            Self::Compass => "icons/compass.svg",
            Self::Door => "icons/door.svg",
            Self::Emoji => "icons/emoji.svg",
            Self::Group => "icons/group.svg",
            Self::Input => "icons/input.svg",
            Self::Invite => "icons/invite.svg",
            Self::Link => "icons/link.svg",
            Self::Lock => "icons/lock.svg",
            Self::Message => "icons/message.svg",
            Self::PaperPlaneFill => "icons/paper-plane-fill.svg",
            Self::Plus => "icons/plus.svg",
            Self::PlusChat => "icons/plus-chat.svg",
            Self::Profile => "icons/profile.svg",
            Self::Relay => "icons/relay.svg",
            Self::Search => "icons/search.svg",
            Self::Settings => "icons/settings.svg",
            Self::Settings2 => "icons/settings2.svg",
            Self::Upload => "icons/upload.svg",
            Self::Warning => "icons/warning.svg",
        }
        .into()
    }
}
