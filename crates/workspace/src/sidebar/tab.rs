#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SidebarTab {
    Inbox,
    Groups,
}

impl SidebarTab {
    pub const ALL: [SidebarTab; 2] = [Self::Inbox, Self::Groups];

    pub fn label(self) -> &'static str {
        match self {
            Self::Inbox => "Inbox",
            Self::Groups => "Groups",
        }
    }

    /// Heading for the tab's list of items.
    pub fn list_title(self) -> &'static str {
        match self {
            Self::Inbox => "Direct Messages",
            Self::Groups => "Groups",
        }
    }

    pub fn list_id(self) -> &'static str {
        match self {
            Self::Inbox => "sidebar-inbox",
            Self::Groups => "sidebar-groups",
        }
    }

    pub fn index(self) -> usize {
        match self {
            Self::Inbox => 0,
            Self::Groups => 1,
        }
    }
}
