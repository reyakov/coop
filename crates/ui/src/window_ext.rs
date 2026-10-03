use std::rc::Rc;

use gpui::{App, ElementId, Entity, Window};

use crate::Root;
use crate::dialog::Dialog;
use crate::notification::Notification;

/// Extension trait for [`Window`] to add dialog, notification .. functionality.
pub trait WindowExtension: Sized {
    /// Opens a Dialog.
    fn open_dialog<F>(&mut self, cx: &mut App, builder: F)
    where
        F: Fn(Dialog, &mut Window, &mut App) -> Dialog + 'static;

    /// Closes the last active Dialog.
    fn close_dialog(&mut self, cx: &mut App);

    /// Closes all active Dialogs.
    fn close_all_dialogs(&mut self, cx: &mut App);

    /// Returns number of notifications.
    fn notifications(&mut self, cx: &mut App) -> Rc<Vec<Entity<Notification>>>;

    /// Pushes a notification to the notification list.
    fn push_notification<T>(&mut self, note: T, cx: &mut App)
    where
        T: Into<Notification>;

    /// Clear the unique notification.
    fn clear_notification<T: Sized + 'static>(&mut self, cx: &mut App);

    /// Clear the unique notification with the given id.
    fn clear_notification_by_id<T: Sized + 'static>(
        &mut self,
        key: impl Into<ElementId>,
        cx: &mut App,
    );

    /// Clear all notifications
    fn clear_notifications(&mut self, cx: &mut App);
}

impl WindowExtension for Window {
    #[inline]
    fn open_dialog<F>(&mut self, cx: &mut App, builder: F)
    where
        F: Fn(Dialog, &mut Window, &mut App) -> Dialog + 'static,
    {
        Root::update(self, cx, move |root, window, cx| {
            root.open_dialog(builder, window, cx);
        })
    }

    #[inline]
    fn close_dialog(&mut self, cx: &mut App) {
        Root::update(self, cx, move |root, window, cx| {
            root.close_dialog(window, cx);
        })
    }

    #[inline]
    fn close_all_dialogs(&mut self, cx: &mut App) {
        Root::update(self, cx, |root, window, cx| {
            root.close_all_dialogs(window, cx);
        })
    }

    #[inline]
    fn push_notification<T>(&mut self, note: T, cx: &mut App)
    where
        T: Into<Notification>,
    {
        let note = note.into();
        Root::update(self, cx, move |root, window, cx| {
            root.push_notification(note, window, cx);
        })
    }

    #[inline]
    fn clear_notification<T: Sized + 'static>(&mut self, cx: &mut App) {
        Root::update(self, cx, |root, window, cx| {
            root.clear_notification::<T>(window, cx);
        })
    }

    #[inline]
    fn clear_notification_by_id<T: Sized + 'static>(
        &mut self,
        key: impl Into<ElementId>,
        cx: &mut App,
    ) {
        let key: ElementId = key.into();
        Root::update(self, cx, |root, window, cx| {
            root.clear_notification_by_id::<T>(key, window, cx);
        })
    }

    #[inline]
    fn clear_notifications(&mut self, cx: &mut App) {
        Root::update(self, cx, move |root, window, cx| {
            root.clear_notifications(window, cx);
        })
    }

    fn notifications(&mut self, cx: &mut App) -> Rc<Vec<Entity<Notification>>> {
        let entity = Root::read(self, cx).notification.clone();
        Rc::new(entity.read(cx).notifications())
    }
}
