use crate::ui::Ui;
use adw::prelude::*;
use std::rc::Rc;

#[derive(Default)]
pub struct ReadState {
    permission: Option<bool>,
    last: Option<(String, String, String, u64)>,
}

impl Ui {
    pub fn read_receipts_enabled(&self) -> bool {
        self.read_ui.borrow().permission.unwrap_or_else(|| {
            self.store
                .read_receipts(&self.state.borrow().account)
                .enabled
        })
    }

    pub fn set_read_receipts(
        self: &Rc<Self>,
        enabled: bool,
    ) -> impl std::future::Future<Output = Result<(), String>> + 'static {
        let owner = self.state.borrow().account.clone();
        let weak = Rc::downgrade(self);
        if !enabled {
            self.read_ui.borrow_mut().permission = Some(false);
            let policy = self.store.read_receipts(&owner);
            self.command(crate::protocol::Command::DisplayedPolicy {
                enabled: false,
                revision: policy.revision,
            });
        }
        async move {
            let Some(ui) = weak.upgrade() else {
                return Err("The account is no longer open".into());
            };
            let account = owner.clone();
            let policy = ui
                .store
                .write_snapshot(move |store| store.set_read_receipts(&account, enabled))
                .await
                .map_err(|_| "Read receipt settings could not be saved".to_owned())?;
            if ui.state.borrow().account != owner {
                return Err("The active account changed".into());
            }
            ui.read_ui.replace(ReadState {
                permission: Some(policy.enabled),
                last: None,
            });
            ui.command(crate::protocol::Command::DisplayedPolicy {
                enabled: policy.enabled,
                revision: policy.revision,
            });
            ui.queue_displayed_marker();
            Ok(())
        }
    }

    fn queue_displayed_marker(self: &Rc<Self>) {
        if !self.window.is_active() || !self.read_receipts_enabled() || !self.reading_current_chat()
        {
            return;
        }
        let state = self.state.borrow();
        let chat = state.current();
        if chat.group || state.account.is_empty() {
            return;
        }
        let bubbles = self.bubbles.borrow();
        let visible = chat.messages.iter().rev().find(|message| {
            !message.own
                && !message.unavailable
                && !message.retracted
                && !message.markable_id.is_empty()
                && bubbles
                    .get(&message.id)
                    .and_then(|bubble| bubble.compute_bounds(&self.scroll))
                    .is_some_and(|bounds| {
                        bounds.y() < self.scroll.height() as f32
                            && bounds.y() + bounds.height() > 0.
                    })
        });
        let Some(message) = visible else { return };
        let key = (
            state.account.clone(),
            chat.jid.clone(),
            message.id.clone(),
            self.store.read_receipts(&state.account).revision,
        );
        drop(bubbles);
        drop(state);
        if self.read_ui.borrow().last.as_ref() == Some(&key) {
            return;
        }
        self.read_ui.borrow_mut().last = Some(key.clone());
        let weak = Rc::downgrade(self);
        gtk::glib::MainContext::default().spawn_local(async move {
            let Some(ui) = weak.upgrade() else { return };
            let (owner, peer, id, _) = key.clone();
            let candidate = ui
                .store
                .write_snapshot(move |store| store.mark_displayed(&owner, &peer, &[id]))
                .await;
            if ui.state.borrow().account != key.0 || !ui.read_receipts_enabled() {
                return;
            }
            if let Ok(Some(candidate)) = candidate {
                ui.command(crate::protocol::Command::Displayed {
                    peer: candidate.peer,
                    id: candidate.id,
                });
            }
        });
    }
    pub fn receive_read_marker(self: &Rc<Self>, owner: &str, peer: &str, wire: &str, own: bool) {
        let (owner, peer, wire) = (owner.to_owned(), peer.to_owned(), wire.to_owned());
        let generation = self.generation.get();
        let weak = Rc::downgrade(self);
        gtk::glib::MainContext::default().spawn_local(async move {
            let Some(ui) = weak.upgrade() else { return };
            let (account, address) = (owner.clone(), peer.clone());
            let changed = ui
                .store
                .write_snapshot(move |store| {
                    if own {
                        store.received_own_displayed(&account, &address, &wire)
                    } else {
                        store.received_displayed(&account, &address, &wire)
                    }
                })
                .await;
            if ui.generation.get() != generation || ui.state.borrow().account != owner {
                return;
            }
            let Ok(changed) = changed else { return };
            if changed.is_empty() {
                return;
            }
            let mut state = ui.state.borrow_mut();
            let active = state.current().jid == peer;
            if let Some(chat) = state.chats.iter_mut().find(|chat| chat.jid == peer) {
                for message in changed {
                    if let Some(row) = chat.messages.iter_mut().find(|row| row.id == message.id) {
                        *row = message;
                    }
                }
                chat.unread = ui.store.unread_count(&owner, &peer);
                chat.remember_latest();
            }
            drop(state);
            if active {
                ui.render_messages(false);
            }
            ui.refresh_chats(None);
        });
    }
    pub fn reading_current_chat(&self) -> bool {
        // A chat visible on another monitor is still being viewed even when
        // keyboard focus is on a different application.
        self.window.is_mapped()
            && self.timeline.is_mapped()
            && self
                .window
                .surface()
                .and_then(|surface| surface.dynamic_cast::<gtk::gdk::Toplevel>().ok())
                .is_none_or(|toplevel| {
                    !toplevel.state().intersects(
                        gtk::gdk::ToplevelState::MINIMIZED | gtk::gdk::ToplevelState::SUSPENDED,
                    )
                })
            && self.pages.visible_child_name().as_deref() == Some("chat")
            && !self
                .dialogs
                .borrow()
                .iter()
                .any(|dialog| dialog.is_visible())
            && self.active_media.borrow().is_none()
            && self.active_story.borrow().is_none()
            && self.history_window.newest()
            && self.at_message_end()
    }

    pub fn mark_visible_chat_read(self: &Rc<Self>) {
        if !self.reading_current_chat() {
            return;
        }
        self.queue_displayed_marker();
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
        drop(state);
        self.refresh_chats(None);
    }
}
