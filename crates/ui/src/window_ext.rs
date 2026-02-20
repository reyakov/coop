use std::rc::Rc;

use gpui::{App, Entity, SharedString, Window};

use crate::input::InputState;
use crate::modal::Modal;
use crate::notification::Notification;
use crate::Root;

/// Extension trait for [`Window`] to add modal, notification .. functionality.
pub trait WindowExtension: Sized {
    /// Opens a Modal.
    fn open_modal<F>(&mut self, cx: &mut App, builder: F)
    where
        F: Fn(Modal, &mut Window, &mut App) -> Modal + 'static;

    /// Return true, if there is an active Modal.
    fn has_active_modal(&mut self, cx: &mut App) -> bool;

    /// Closes the last active Modal.
    fn close_modal(&mut self, cx: &mut App);

    /// Closes all active Modals.
    fn close_all_modals(&mut self, cx: &mut App);

    /// Returns number of notifications.
    fn notifications(&mut self, cx: &mut App) -> Rc<Vec<Entity<Notification>>>;

    /// Pushes a notification to the notification list.
    fn push_notification<T>(&mut self, note: T, cx: &mut App)
    where
        T: Into<Notification>;

    /// Clears a notification by its ID.
    fn clear_notification<T>(&mut self, id: T, cx: &mut App)
    where
        T: Into<SharedString>;

    /// Clear all notifications
    fn clear_notifications(&mut self, cx: &mut App);

    /// Return current focused Input entity.
    fn focused_input(&mut self, cx: &mut App) -> Option<Entity<InputState>>;

    /// Returns true if there is a focused Input entity.
    fn has_focused_input(&mut self, cx: &mut App) -> bool;
}

impl WindowExtension for Window {
    #[inline]
    fn open_modal<F>(&mut self, cx: &mut App, builder: F)
    where
        F: Fn(Modal, &mut Window, &mut App) -> Modal + 'static,
    {
        Root::update(self, cx, move |root, window, cx| {
            root.open_modal(builder, window, cx);
        })
    }

    #[inline]
    fn has_active_modal(&mut self, cx: &mut App) -> bool {
        Root::read(self, cx).has_active_modals()
    }

    #[inline]
    fn close_modal(&mut self, cx: &mut App) {
        Root::update(self, cx, move |root, window, cx| {
            root.close_modal(window, cx);
        })
    }

    #[inline]
    fn close_all_modals(&mut self, cx: &mut App) {
        Root::update(self, cx, |root, window, cx| {
            root.close_all_modals(window, cx);
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
    fn clear_notification<T>(&mut self, id: T, cx: &mut App)
    where
        T: Into<SharedString>,
    {
        let id = id.into();
        Root::update(self, cx, move |root, window, cx| {
            root.clear_notification(id, window, cx);
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

    fn has_focused_input(&mut self, cx: &mut App) -> bool {
        Root::read(self, cx).focused_input.is_some()
    }

    fn focused_input(&mut self, cx: &mut App) -> Option<Entity<InputState>> {
        Root::read(self, cx).focused_input.clone()
    }
}
