#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SidebarTab {
    Inbox,
    Communities,
}

impl SidebarTab {
    pub const ALL: [SidebarTab; 2] = [Self::Inbox, Self::Communities];

    pub fn label(self) -> &'static str {
        match self {
            Self::Inbox => "Inbox",
            Self::Communities => "Communities",
        }
    }

    /// Heading for the tab's list of items.
    pub fn list_title(self) -> &'static str {
        match self {
            Self::Inbox => "Direct Messages",
            Self::Communities => "Communities",
        }
    }

    pub fn list_id(self) -> &'static str {
        match self {
            Self::Inbox => "sidebar-inbox",
            Self::Communities => "sidebar-communities",
        }
    }

    pub fn index(self) -> usize {
        match self {
            Self::Inbox => 0,
            Self::Communities => 1,
        }
    }
}
