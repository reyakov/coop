use gpui::{Anchor, Pixels, px};

use crate::{Edges, TITLEBAR_HEIGHT};

/// The settings for notifications.
#[derive(Debug, Clone)]
pub struct NotificationSettings {
    /// The placement of the notification, default: [`Anchor::TopRight`]
    pub placement: Anchor,
    /// The margins of the notification with respect to the window edges.
    pub margins: Edges<Pixels>,
    /// The maximum number of notifications to show at once, default: 10
    pub max_items: usize,
}

impl Default for NotificationSettings {
    fn default() -> Self {
        let offset = px(16.);
        Self {
            placement: Anchor::TopRight,
            margins: Edges {
                top: TITLEBAR_HEIGHT + offset, // avoid overlap with title bar
                right: offset,
                bottom: offset,
                left: offset,
            },
            max_items: 10,
        }
    }
}
