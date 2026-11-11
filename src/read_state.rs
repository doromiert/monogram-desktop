use crate::ui::Ui;
use adw::prelude::*;
use std::rc::Rc;

impl Ui {
    pub fn reading_current_chat(&self) -> bool {
        // A chat visible on another monitor is still being viewed even when
        // keyboard focus is on a different application.
        self.window.is_mapped()
            && self.timeline.is_mapped()
            && self.window.surface().and_then(|surface| surface.dynamic_cast::<gtk::gdk::Toplevel>().ok())
                .is_none_or(|toplevel| !toplevel.state().intersects(gtk::gdk::ToplevelState::MINIMIZED | gtk::gdk::ToplevelState::SUSPENDED))
            && self.pages.visible_child_name().as_deref() == Some("chat")
            && !self.dialogs.borrow().iter().any(|dialog| dialog.is_visible())
            && self.active_media.borrow().is_none()
            && self.active_story.borrow().is_none()
            && self.history_window.newest()
            && self.at_message_end()
    }

    pub fn mark_visible_chat_read(self: &Rc<Self>) {
        if !self.reading_current_chat() {
            return;
        }
        let mut state = self.state.borrow_mut();
        if state.account.is_empty()
            || (state.current().unread == 0
                && !state
                    .current()
                    .messages
                    .iter()
                    .any(|message| message.unread))
        {
            return;
        }
        let owner = state.account.clone();
        let peer = state.current().jid.clone();
        if self.store.mark_read(&owner, &peer).is_err() {
            return;
        }
        let chat = state.current_mut();
        chat.unread = 0;
        for message in &mut chat.messages {
            message.unread = false;
        }
        let _ = self.store.save_chat_message(&owner, chat, None);
        drop(state);
        self.refresh_chats(None);
    }
}
