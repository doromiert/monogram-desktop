use crate::{
    encryption_policy::{Mode, Preference, Selection},
    model::Chat,
    protocol::Command,
    ui::Ui,
};
use adw::prelude::*;
use gtk::glib;
use std::rc::Rc;

/// Shown instead of sending when a room that used encryption stops offering it.
pub const DOWNGRADE_NOTICE: &str = "This room no longer offers end-to-end encryption, so nothing was sent. To send unencrypted anyway, choose Unencrypted in Conversation Security.";

impl Ui {
    pub(crate) fn update_send_button(&self) {
        self.send_button.set_sensitive(
            !self.composer_text().trim().is_empty() || !self.attachments.borrow().is_empty(),
        );
    }

    pub fn sending_mode(&self, chat: &Chat) -> Option<Mode> {
        if self.smoke && chat.sending_preference.is_none() {
            return Some(if chat.encryption {
                Mode::Pq
            } else {
                Mode::Plain
            });
        }
        chat.selected_encryption()
    }

    pub fn fingerprint_for_send(&self, mode: Mode) -> Option<String> {
        match mode {
            Mode::Plain => Some(String::new()),
            Mode::Legacy => self
                .legacy_crypto
                .borrow()
                .ready
                .then(|| self.legacy_crypto.borrow().fingerprint.clone()),
            Mode::Omemo2 => self
                .crypto
                .borrow()
                .ready
                .then(|| self.crypto.borrow().fingerprint.clone()),
            Mode::Pq => self
                .pq_crypto
                .borrow()
                .ready
                .then(|| self.pq_crypto.borrow().fingerprint.clone()),
        }
    }

    /// The encryption check failed. A Legacy OMEMO contact-authorization denial offers Resolve.
    pub(crate) fn report_encryption_check_failure(
        self: &Rc<Self>,
        peer: &str,
        error: &crate::crypto::Error,
    ) {
        let reason = error.to_string();
        let title = format!("Could not check conversation encryption. {reason}");
        self.notice_resolving_authorization(peer, &reason, title);
    }

    pub fn choose_sending(self: &Rc<Self>, then: impl FnOnce(&Rc<Self>, Mode) + 'static) {
        let state = self.state.borrow();
        if state.account.is_empty() {
            drop(state);
            then(self, Mode::Plain);
            return;
        }
        if !self.online.get() {
            drop(state);
            self.notice("Connect before sending. Your draft is kept.");
            return;
        }
        let chat = state.current();
        if chat.group {
            if !chat.room_can_send {
                self.notice("Join the room before sending");
                return;
            }
        }
        if let Some(mode) = self.sending_mode(chat) {
            drop(state);
            then(self, mode);
            return;
        }
        let owner = state.account.clone();
        let peer = chat.jid.clone();
        drop(state);
        if self.encryption_checks.borrow().contains_key(&peer) {
            return;
        }
        let (reply, receive) = tokio::sync::oneshot::channel();
        if !self.command(Command::ChooseEncryption {
            peer: peer.clone(),
            reply,
        }) {
            self.notice("Encryption check could not start. Your draft is kept.");
            return;
        }
        let attempt = crate::model::new_id();
        self.encryption_checks
            .borrow_mut()
            .insert(peer.clone(), attempt.clone());
        self.update_encryption_check();
        let generation = self.generation.get();
        let revision = self.encryption_revision.get();
        let weak = Rc::downgrade(self);
        glib::MainContext::default().spawn_local(async move {
            let result = receive.await;
            let Some(ui) = weak.upgrade().filter(|ui| {
                ui.generation.get() == generation && ui.state.borrow().account == owner
            }) else {
                return;
            };
            if ui.encryption_checks.borrow().get(&peer) != Some(&attempt) {
                return;
            }
            ui.encryption_checks.borrow_mut().remove(&peer);
            ui.update_encryption_check();
            if ui.encryption_revision.get() != revision
                || !ui.online.get()
                || ui.state.borrow().current().jid != peer
            {
                return;
            }
            let mode = match result {
                Ok(Ok(mode)) => mode,
                Ok(Err(error)) => {
                    ui.report_encryption_check_failure(&peer, &error);
                    return;
                }
                Err(_) => {
                    ui.notice("Encryption check was interrupted. Your draft is kept.");
                    return;
                }
            };
            if ui.encryption_downgrade_blocked(&peer, mode) {
                ui.notice(DOWNGRADE_NOTICE);
                return;
            }
            ui.state.borrow_mut().current_mut().remember_selection(mode);
            if !ui.persist() {
                ui.state.borrow_mut().current_mut().automatic_encryption = None;
                return;
            }
            if mode == Mode::Plain {
                ui.notice("Sending without end-to-end encryption in this conversation");
            }
            then(&ui, mode);
        });
    }

    /// Whether `mode` would silently downgrade a room that has used encryption (see
    /// `Chat::downgrade_blocked`). The database is checked too, for rooms whose history predates
    /// the flag and is no longer loaded.
    pub fn encryption_downgrade_blocked(&self, peer: &str, mode: Mode) -> bool {
        let state = self.state.borrow();
        let Some(chat) = state.chats.iter().find(|chat| chat.jid == peer) else {
            return false;
        };
        chat.downgrade_blocked(mode)
            || (chat.group
                && mode == Mode::Plain
                && self.store.sent_encrypted(&state.account, peer).unwrap_or(true))
    }

    pub fn update_encryption_check(&self) {
        self.update_send_button();
        let busy = self
            .encryption_checks
            .borrow()
            .contains_key(&self.state.borrow().current().jid);
        if busy {
            let spinner = gtk::Spinner::builder().spinning(true).build();
            self.send_button.set_child(Some(&spinner));
            self.send_button
                .set_tooltip_text(Some("Checking encryption…"));
        } else {
            if !self
                .send_button
                .child()
                .is_some_and(|child| child.is::<gtk::Image>())
            {
                let plane =
                    gtk::Image::from_file(crate::widgets::asset("paper-plane-symbolic.svg"));
                plane.set_pixel_size(20);
                self.send_button.set_child(Some(&plane));
            }
            self.send_button.set_tooltip_text(Some("Send message"));
        }
    }

    pub fn set_sending_preference(&self, peer: &str, preference: Preference) {
        self.encryption_revision
            .set(self.encryption_revision.get().wrapping_add(1));
        let mut state = self.state.borrow_mut();
        if let Some(chat) = state.chats.iter_mut().find(|chat| chat.jid == peer) {
            chat.sending_preference = Some(preference);
            chat.automatic_encryption = None;
            chat.encryption = preference.forced().is_some_and(Mode::encrypted);
        }
        drop(state);
        self.persist();
        self.update_mode();
    }

    pub fn invalidate_sending_choice(&self, peer: &str) {
        self.encryption_revision
            .set(self.encryption_revision.get().wrapping_add(1));
        let mut state = self.state.borrow_mut();
        if let Some(chat) = state
            .chats
            .iter_mut()
            .find(|chat| chat.jid == peer && chat.encryption_preference() == Preference::Automatic)
        {
            chat.automatic_encryption = None;
            chat.encryption = false;
        }
        drop(state);
        self.persist();
    }
}
