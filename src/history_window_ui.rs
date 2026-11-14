use crate::{
    history_window::{PAGE, SCREENS, Side, retained},
    messages::ScrollTarget,
    model::{Message, MessagePreview},
    ui::Ui,
};
use adw::prelude::*;
use gtk::glib;
use std::{collections::HashSet, rc::Rc};

impl Ui {
    pub fn timeline_scope_changed(&self) {
        self.cancel_reply_scroll();
        self.send_animation.cancel();
        self.set_composer_send_hint(false);
        self.history_window.reset(true);
        self.history_window.highlighted.borrow_mut().take();
        self.history_latest.set_visible(false);
    }

    pub fn history_bounds(&self) -> (f64, f64, f64) {
        let adjustment = self.scroll.vadjustment();
        (
            adjustment.value(),
            adjustment.page_size(),
            adjustment.upper(),
        )
    }

    pub fn history_edge(self: &Rc<Self>, side: Side) {
        if self.state.borrow().account.is_empty() || self.history_window.moving.get() {
            return;
        }
        let (top, height, end) = self.history_bounds();
        if height <= 0.0 {
            return;
        }
        let distance = match side {
            Side::Older => top,
            Side::Newer => end - top - height,
        };
        if distance < height * 0.5 {
            if side == Side::Older && self.history_window.at_end(side) {
                let peer = self.state.borrow().current().jid.clone();
                self.request_server_history(&peer);
            } else {
                self.page_timeline(side);
            }
        }
    }

    pub fn schedule_history_maintenance(self: &Rc<Self>) {
        if self.history_window.scheduled.replace(true) {
            return;
        }
        let weak = Rc::downgrade(self);
        self.scroll.add_tick_callback(move |_, _| {
            if let Some(ui) = weak.upgrade() {
                ui.history_window.scheduled.set(false);
                ui.maintain_history_window();
            }
            glib::ControlFlow::Break
        });
    }

    fn maintain_history_window(self: &Rc<Self>) {
        if self.rendering_messages() || self.history_window.moving.get() {
            return;
        }
        self.mark_visible_chat_read();
        if self.state.borrow().account.is_empty()
            || !self.timeline.is_mapped()
            || self.message_scroll.borrow().is_some()
        {
            return;
        }
        let (top, height, end) = self.history_bounds();
        if height <= 0.0 {
            return;
        }
        self.history_latest.set_visible(!self.at_message_end());
        if self.history_window.busy() {
            return;
        }
        let mut rows = Vec::new();
        let mut child = self.timeline.first_child();
        while let Some(widget) = child {
            child = widget.next_sibling();
            if !widget.has_css_class("message-row") {
                continue;
            }
            let Some(bounds) = widget.compute_bounds(&self.timeline) else {
                return;
            };
            let start = f64::from(bounds.y());
            rows.push((
                widget.widget_name().to_string(),
                start..start + f64::from(bounds.height()),
            ));
        }
        let bounds: Vec<_> = rows.iter().map(|(_, bounds)| bounds.clone()).collect();
        let retained = retained(&bounds, top..top + height);
        let prune_older = retained.start != 0;
        let prune_newer = retained.end < rows.len();
        if !retained.is_empty() && (prune_older || prune_newer) {
            let start = if prune_older { retained.start } else { 0 };
            let end = if prune_newer {
                retained.end
            } else {
                rows.len()
            };
            let discard: HashSet<_> = rows[..start]
                .iter()
                .chain(&rows[end..])
                .map(|(id, _)| id.as_str())
                .collect();
            let mut state = self.state.borrow_mut();
            let chat = state.current_mut();
            chat.remember_latest();
            chat.messages.retain(|m| !discard.contains(m.id.as_str()));
            drop(state);
            if prune_older {
                self.history_window.end(Side::Older, false);
            }
            if prune_newer {
                self.history_window.end(Side::Newer, false);
            }
            self.render_messages(false);
            return;
        }
        // Prefetch inside the retained buffer, so trimming cannot immediately fetch it again.
        let prefetch = height * SCREENS / 2.;
        if top < prefetch && !self.history_window.at_end(Side::Older) {
            self.page_timeline(Side::Older);
        } else if end - top - height < prefetch && !self.history_window.at_end(Side::Newer) {
            self.page_timeline(Side::Newer);
        }
    }

    fn page_timeline(self: &Rc<Self>, side: Side) {
        if self.history_window.at_end(side) {
            return;
        }
        let (owner, peer, boundary, limit) = {
            let state = self.state.borrow();
            let chat = state.current();
            let edge = match side {
                Side::Older => chat.messages.first(),
                Side::Newer => chat.messages.last(),
            };
            let Some(edge) = edge else { return };
            let average_height =
                self.timeline.height().max(1) as f64 / chat.messages.len().max(1) as f64;
            let limit = (self.scroll.height().max(1) as f64 * 1.5 / average_height)
                .ceil()
                .clamp(4., PAGE as f64) as u32;
            (
                state.account.clone(),
                chat.jid.clone(),
                (edge.timestamp.clone(), edge.id.clone()),
                limit,
            )
        };
        let Some(revision) = self.history_window.begin() else {
            return;
        };
        self.update_history_spinner();
        let generation = self.generation.get();
        let content = self.history_window.content_revision();
        let weak = Rc::downgrade(self);
        glib::MainContext::default().spawn_local(async move {
            let Some(ui) = weak.upgrade() else { return };
            let (account, chat) = (owner.clone(), peer.clone());
            let result = ui
                .store
                .read_snapshot(move |store| {
                    store.history_side(&account, &chat, (&boundary.0, &boundary.1), side, limit)
                })
                .await;
            if !ui.finish_history_query(&owner, &peer, generation, revision) {
                return;
            }
            if ui.history_window.content_revision() != content {
                ui.schedule_history_maintenance();
                return;
            }
            match result {
                Ok(mut page) => {
                    ui.history_window.end(side, page.len() < limit as usize);
                    let mut state = ui.state.borrow_mut();
                    let chat = state.current_mut();
                    let present: HashSet<_> = chat.messages.iter().map(|m| m.id.as_str()).collect();
                    page.retain(|message| !present.contains(message.id.as_str()));
                    if page.is_empty() {
                        drop(state);
                        ui.schedule_history_maintenance();
                        return;
                    }
                    match side {
                        Side::Older => {
                            chat.messages.splice(0..0, page);
                        }
                        Side::Newer => chat.messages.extend(page),
                    }
                    chat.remember_latest();
                    drop(state);
                    ui.render_messages(false);
                }
                Err(_) => ui.notice("Local history could not be loaded"),
            }
        });
    }

    fn finish_history_query(
        &self,
        owner: &str,
        peer: &str,
        generation: u64,
        revision: u64,
    ) -> bool {
        let current = {
            let state = self.state.borrow();
            state.account == owner && state.current().jid == peer
        };
        if !current || self.generation.get() != generation || !self.history_window.finish(revision)
        {
            return false;
        }
        self.update_history_spinner();
        true
    }

    pub fn load_timeline(self: &Rc<Self>, target: Option<String>) {
        let (owner, peer) = {
            let state = self.state.borrow();
            (state.account.clone(), state.current().jid.clone())
        };
        if owner.is_empty() {
            return;
        }
        let was_newest = self.history_window.newest();
        self.history_window.reset(target.is_none());
        let revision = self.history_window.begin().unwrap();
        self.update_history_spinner();
        let generation = self.generation.get();
        let content = self.history_window.content_revision();
        let weak = Rc::downgrade(self);
        glib::MainContext::default().spawn_local(async move {
            let Some(ui) = weak.upgrade() else { return };
            let (account, chat, id) = (owner.clone(), peer.clone(), target.clone());
            let result = ui
                .store
                .read_snapshot(move |store| {
                    if let Some(id) = id {
                        store.history_around(&account, &chat, &id)
                    } else {
                        store.try_history(&account, &chat, None, PAGE)
                    }
                })
                .await;
            if !ui.finish_history_query(&owner, &peer, generation, revision) {
                return;
            }
            if ui.history_window.content_revision() != content {
                ui.load_timeline(target);
                return;
            }
            match result {
                Ok(page) if page.is_empty() && target.is_some() => {
                    ui.history_window.end(Side::Newer, was_newest);
                    ui.notice("Original message is unavailable");
                }
                Ok(page) => {
                    let mut state = ui.state.borrow_mut();
                    let chat = state.current_mut();
                    let newest = target.is_none()
                        || page
                            .last()
                            .is_some_and(|m| Some(m.order_key()) == chat.latest_key());
                    ui.history_window.end(Side::Newer, newest);
                    chat.remember_latest();
                    chat.messages = page;
                    chat.remember_latest();
                    drop(state);
                    let scroll = target.map_or(ScrollTarget::Bottom, ScrollTarget::Message);
                    ui.render_messages_at(scroll);
                }
                Err(_) => {
                    ui.history_window.end(Side::Newer, was_newest);
                    ui.notice("Local history could not be loaded");
                }
            }
        });
    }

    pub fn select_history_span(self: &Rc<Self>, anchor: String, id: String) {
        let (owner, peer) = {
            let state = self.state.borrow();
            (state.account.clone(), state.current().jid.clone())
        };
        if owner.is_empty() {
            return;
        }
        let generation = self.generation.get();
        let weak = Rc::downgrade(self);
        glib::MainContext::default().spawn_local(async move {
            let Some(ui) = weak.upgrade() else { return };
            let (account, chat, start) = (owner.clone(), peer.clone(), anchor.clone());
            let selection = ui
                .store
                .read_snapshot(move |store| store.history_ids_between(&account, &chat, &start, &id))
                .await;
            let state = ui.state.borrow();
            if ui.generation.get() != generation
                || state.account != owner
                || state.current().jid != peer
                || ui.anchor.borrow().as_ref() != Some(&anchor)
            {
                return;
            }
            drop(state);
            match selection {
                Ok(ids) => {
                    ui.selected.borrow_mut().extend(ids);
                    ui.update_selection();
                }
                Err(_) => ui.notice("Selected history could not be loaded"),
            }
        });
    }

    pub fn resolve_selected_messages(self: &Rc<Self>, forward: bool) {
        let selected = self.selected.borrow().clone();
        let state = self.state.borrow();
        if state.account.is_empty() {
            let messages = state
                .current()
                .messages
                .iter()
                .filter(|m| selected.contains(&m.id))
                .cloned()
                .collect();
            drop(state);
            self.apply_selection(messages, forward);
            return;
        }
        let (owner, peer) = (state.account.clone(), state.current().jid.clone());
        drop(state);
        let generation = self.generation.get();
        let weak = Rc::downgrade(self);
        glib::MainContext::default().spawn_local(async move {
            let Some(ui) = weak.upgrade() else { return };
            let (account, chat, ids) = (owner.clone(), peer.clone(), selected.clone());
            let messages = ui
                .store
                .read_snapshot(move |store| {
                    let mut rows: Vec<_> = ids
                        .iter()
                        .filter_map(|id| store.message(&account, &chat, id))
                        .filter(|m| !m.retracted)
                        .collect();
                    rows.sort_by(|a, b| a.order_key().cmp(&b.order_key()));
                    Ok(rows)
                })
                .await;
            let state = ui.state.borrow();
            if ui.generation.get() != generation
                || state.account != owner
                || state.current().jid != peer
                || *ui.selected.borrow() != selected
            {
                return;
            }
            drop(state);
            match messages {
                Ok(messages) => ui.apply_selection(messages, forward),
                Err(_) => ui.notice("Selected messages could not be loaded"),
            }
        });
    }

    pub fn active_message(&self, id: &str) -> Option<Message> {
        let state = self.state.borrow();
        state.message(id).cloned().or_else(|| {
            self.store
                .message(&state.account, &state.current().jid, id)
                .filter(|m| !m.retracted)
        })
    }

    pub fn sync_latest_preview(&self, owner: &str, peer: &str) {
        let latest = self
            .store
            .history(owner, peer, None, 1)
            .first()
            .map(MessagePreview::from);
        let mut state = self.state.borrow_mut();
        if state.account == owner {
            state.chat_for(peer).latest = latest;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Account, State};
    use gtk::{gdk, gio};
    use std::time::{Duration, Instant};

    #[track_caller]
    fn wait(ui: &Ui, mut condition: impl FnMut() -> bool) {
        let main = glib::MainContext::default();
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline {
            while main.pending() {
                main.iteration(false);
            }
            if !ui.rendering_messages() && condition() {
                return;
            }
            std::thread::sleep(Duration::from_millis(4));
        }
        let state = ui.state.borrow();
        let chat = state.current();
        assert!(
            !ui.rendering_messages() && condition(),
            "Native history window did not settle: mapped={} bounds={:?} rows={} first={:?} last={:?} busy={} newest={}",
            ui.timeline.is_mapped(),
            ui.history_bounds(),
            chat.messages.len(),
            chat.messages.first().map(|m| &m.id),
            chat.messages.last().map(|m| &m.id),
            ui.history_window.busy(),
            ui.history_window.newest()
        );
    }

    #[test]
    #[ignore = "requires a private GTK display and D-Bus"]
    fn switching_into_140_unread_keeps_five_screen_bound_and_input_responsive() {
        let application = adw::Application::builder()
            .application_id("com.negzero.MonogramDesktop.UnreadHistoryFixture")
            .flags(gio::ApplicationFlags::NON_UNIQUE)
            .build();
        application.register(None::<&gio::Cancellable>).unwrap();
        let ui = Ui::new(&application, true);
        let owner = "alice@fixture.invalid";
        let peer = "busy@fixture.invalid";
        ui.store
            .save_account(&Account {
                jid: owner.into(),
                host: String::new(),
                port: 5222,
                direct_tls: false,
            })
            .unwrap();
        let mut fixture = State::live(owner, vec![]);
        fixture.active = fixture.chat_for("other@fixture.invalid").id.clone();
        let chat = fixture.chat_for(peer);
        chat.unread = 140;
        for n in 0..180 {
            chat.messages.push(Message {
                id: format!("busy-{n:03}"),
                wire_id: format!("busy-{n:03}"),
                sender: peer.into(),
                text: format!(
                    "Unread message {n}{}",
                    "\nContent in this message".repeat(n % 5)
                ),
                timestamp: format!("2026-11-12T{:02}:{:02}:00Z", 10 + n / 60, n % 60),
                ..Default::default()
            });
        }
        ui.store.save(&fixture).unwrap();
        ui.activate_state(ui.store.load_windowed(owner));
        ui.window.set_default_size(900, 520);
        ui.window.present();
        wait(&ui, || ui.timeline.is_mapped());
        let gap = Rc::new(std::cell::Cell::new(Duration::ZERO));
        let last = Rc::new(std::cell::Cell::new(Instant::now()));
        let measured_gap = gap.clone();
        let measured_last = last.clone();
        let timer = glib::timeout_add_local(Duration::from_millis(4), move || {
            let now = Instant::now();
            measured_gap.set(measured_gap.get().max(now - measured_last.replace(now)));
            glib::ControlFlow::Continue
        });
        let start = Instant::now();
        ui.select_chat(peer);
        assert!(start.elapsed() < Duration::from_millis(400));
        wait(&ui, || {
            ui.at_message_end()
                && !ui.history_window.busy()
                && ui.message_scroll.borrow().is_none()
                && ui.bubbles.borrow().contains_key("busy-179")
        });
        // Allow post-allocation pruning to settle before checking retained rows.
        let settle = Instant::now() + Duration::from_millis(100);
        let main = glib::MainContext::default();
        while Instant::now() < settle {
            while main.pending() {
                main.iteration(false);
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        timer.remove();
        assert!(
            gap.get() < Duration::from_millis(400),
            "GTK stalled for {:?}",
            gap.get()
        );
        let (top, height, _) = ui.history_bounds();
        let mut rows = Vec::new();
        let mut child = ui.timeline.first_child();
        while let Some(widget) = child {
            child = widget.next_sibling();
            if !widget.has_css_class("message-row") {
                continue;
            }
            let bounds = widget.compute_bounds(&ui.timeline).unwrap();
            rows.push(f64::from(bounds.y())..f64::from(bounds.y() + bounds.height()));
        }
        assert_eq!(retained(&rows, top..top + height), 0..rows.len());
        assert!(ui.state.borrow().current().messages.len() < 36);
        assert_eq!(ui.store.history(owner, peer, None, 200).len(), 180);
        eprintln!(
            "140-unread chat: settled={:?}, max GTK gap={:?}, retained={} rows across viewport+two screens each side",
            start.elapsed(),
            gap.get(),
            rows.len()
        );
        ui.window.close();
    }

    #[test]
    #[ignore = "requires a private GTK display and D-Bus"]
    fn live_viewport_keeps_archive_reply_and_incoming_updates() {
        const OWNER: &str = "alice@fixture.invalid";
        const PEER: &str = "bob@fixture.invalid";
        let application = adw::Application::builder()
            .application_id("com.negzero.MonogramDesktop.HistoryFixture")
            .flags(gio::ApplicationFlags::NON_UNIQUE)
            .build();
        application.register(None::<&gio::Cancellable>).unwrap();
        let css = gtk::CssProvider::new();
        css.load_from_path(crate::widgets::asset("style.css"));
        gtk::style_context_add_provider_for_display(
            &gdk::Display::default().unwrap(),
            &css,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
        let ui = Ui::new(&application, true);
        ui.store
            .save_account(&Account {
                jid: OWNER.into(),
                host: String::new(),
                port: 5222,
                direct_tls: false,
            })
            .unwrap();
        let mut fixture = State::live(OWNER, vec![]);
        fixture.active = fixture.chat_for(PEER).id.clone();
        for n in 0..24 {
            let id = format!("window-{n:02}");
            let time = format!("2026-02-03T13:{n:02}:00.000000+00:00");
            fixture.current_mut().messages.push(Message {
                id: id.clone(),
                author: "Bob".into(),
                text: format!(
                    "Message {n}{}",
                    "\nA line with a few words".repeat(if n % 4 == 0 { 16 } else { 9 })
                ),
                kind: if n == 3 || n == 18 { "photo" } else { "text" }.into(),
                wire_id: id.clone(),
                aliases: vec![format!("wire:{id}")],
                sender: PEER.into(),
                timestamp: time,
                ..Default::default()
            });
        }
        ui.store.save(&fixture).unwrap();
        ui.activate_state(ui.store.load_windowed(OWNER));
        ui.select_chat(PEER);
        ui.window.set_default_size(900, 520);
        ui.window.present();
        wait(&ui, || {
            ui.at_message_end()
                && ui.timeline.height() > 0
                && !ui.history_window.busy()
                && ui.message_scroll.borrow().is_none()
                && ui.state.borrow().current().messages.len() < 24
        });
        assert!(ui.bubbles.borrow().contains_key("window-23"));
        let first = ui
            .state
            .borrow()
            .current()
            .messages
            .first()
            .unwrap()
            .id
            .clone();
        assert_ne!(first, "window-00");
        assert!(ui.store.message(OWNER, PEER, "window-00").is_some());

        // Reaching the upper local edge prepends an older page without moving the visible row.
        let adjustment = ui.scroll.vadjustment();
        adjustment.set_value(0.0);
        let before = ui.bubbles.borrow()[&first]
            .compute_bounds(&ui.timeline)
            .unwrap()
            .y() as f64;
        wait(&ui, || {
            ui.state
                .borrow()
                .current()
                .messages
                .first()
                .is_some_and(|m| m.id < first)
                && !ui.history_window.busy()
                && ui.message_scroll.borrow().is_none()
        });
        let after = ui.bubbles.borrow()[&first]
            .compute_bounds(&ui.timeline)
            .unwrap()
            .y() as f64
            - adjustment.value();
        assert!(
            (before - after).abs() < 2.0,
            "Older page must preserve row position: {before} != {after}"
        );
        assert!(!ui.at_message_end());

        ui.highlight("window-01");
        wait(&ui, || {
            ui.bubbles
                .borrow()
                .get("window-01")
                .is_some_and(|row| row.has_css_class("reply-target"))
        });
        let latest = Message {
            id: "window-24".into(),
            wire_id: "window-24".into(),
            aliases: vec!["wire:window-24".into()],
            author: "Bob".into(),
            sender: PEER.into(),
            text: "Incoming while viewing older messages".into(),
            timestamp: "2026-02-03T13:24:00.000000+00:00".into(),
            ..Default::default()
        };
        assert!(ui.receive_message(OWNER, PEER.into(), latest, false, None));
        assert!(ui.store.message(OWNER, PEER, "window-24").is_some());
        assert!(!ui.bubbles.borrow().contains_key("window-24"));
        assert_eq!(
            ui.state.borrow().current().latest_preview().unwrap().id,
            "window-24"
        );
        wait(&ui, || {
            !ui.history_window.busy() && ui.message_scroll.borrow().is_none()
        });
        let last = ui
            .state
            .borrow()
            .current()
            .messages
            .last()
            .unwrap()
            .id
            .clone();
        adjustment.set_value(adjustment.upper() - adjustment.page_size());
        wait(&ui, || {
            ui.state
                .borrow()
                .current()
                .messages
                .last()
                .is_some_and(|m| m.id > last)
                && !ui.history_window.busy()
                && ui.message_scroll.borrow().is_none()
        });
        ui.history_latest.emit_clicked();
        wait(&ui, || {
            ui.at_message_end()
                && !ui.history_window.busy()
                && ui.message_scroll.borrow().is_none()
                && !ui.bubbles.borrow().contains_key("window-01")
        });
        ui.highlight("window-01");
        wait(&ui, || {
            ui.bubbles
                .borrow()
                .get("window-01")
                .is_some_and(|row| row.has_css_class("reply-target"))
        });
        ui.load_timeline(None);
        wait(&ui, || {
            ui.at_message_end() && !ui.history_window.busy() && ui.message_scroll.borrow().is_none()
        });
        ui.select_message("window-24", false);
        ui.highlight("window-01");
        wait(&ui, || {
            ui.bubbles
                .borrow()
                .get("window-01")
                .is_some_and(|row| row.has_css_class("reply-target"))
        });
        assert!(ui.selected.borrow().contains("window-24"));
        ui.select_message("window-01", true);
        wait(&ui, || ui.selected.borrow().len() == 24);
        assert_eq!(ui.store.history(OWNER, PEER, None, 100).len(), 25);
        ui.window.close();
    }
}
