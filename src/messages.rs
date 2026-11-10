use crate::{model::*, ui::Ui, widgets::*};
use adw::prelude::*;
use gtk::{gdk, glib};
use std::{rc::Rc, time::Duration};
impl Ui {
    pub fn render_messages(self: &Rc<Self>, bottom: bool) {
        let old_value = self.scroll.vadjustment().value();
        clear_list(&self.timeline);
        self.bubbles.borrow_mut().clear();
        let state = self.state.borrow();
        let messages = state.current().messages.clone();
        let group = state.current().group;
        let live = !state.account.is_empty();
        drop(state);
        let mut last = String::new();
        for m in messages.into_iter().filter(|m| !m.retracted) {
            let day = if m.timestamp.is_empty() {
                "Today"
            } else {
                m.timestamp.get(..10).unwrap_or("Today")
            };
            if day != last {
                let row = gtk::ListBoxRow::builder()
                    .selectable(false)
                    .activatable(false)
                    .child(&label(day, "date-pill"))
                    .build();
                self.timeline.append(&row);
                last = day.into();
            }
            let outer = bx(false, 8);
            outer.set_valign(gtk::Align::Start);
            outer.set_vexpand(false);
            outer.set_halign(if m.own {
                gtk::Align::End
            } else {
                gtk::Align::Start
            });
            if group && !m.own {
                let a = avatar(
                    &Chat {
                        name: m.author.clone(),
                        color: "purple".into(),
                        ..Default::default()
                    },
                    26,
                );
                a.set_valign(gtk::Align::End);
                outer.append(&a)
            }
            let bubble = bx(true, 0);
            bubble.set_vexpand(false);
            bubble.set_valign(gtk::Align::Start);
            bubble.add_css_class("message-bubble");
            if m.own {
                bubble.add_css_class("own")
            }
            let heading = bx(false, 6);
            heading.set_margin_bottom(6);
            let author = text(&m.author, "message-author");
            author.set_hexpand(true);
            author.set_valign(gtk::Align::Center);
            if m.pinned {
                heading.append(&gtk::Image::from_icon_name("view-pin-symbolic"))
            }
            heading.append(&author);
            let meta = bx(false, 4);
            meta.add_css_class("message-meta");
            meta.set_valign(gtk::Align::Center);
            if m.encrypted {
                let image = gtk::Image::from_icon_name("channel-secure-symbolic");
                image.set_pixel_size(11);
                meta.append(&image)
            }
            if m.edited {
                meta.append(&label("edited", ""));
            }
            meta.append(&label(&m.time, ""));
            if m.own {
                let icon = match m.delivery.as_str() {
                    "sending" => "content-loading-symbolic",
                    "failed" | "uncertain" => "dialog-warning-symbolic",
                    _ => "object-select-symbolic",
                };
                let image = gtk::Image::from_icon_name(icon);
                image.set_pixel_size(12);
                image.set_tooltip_text(Some(match m.delivery.as_str() {
                    "sent" => "Sent to stream; awaiting a delivery receipt",
                    "delivered" => "Delivered to a recipient client; not a read receipt",
                    "uncertain" => "Delivery uncertain; retry may duplicate it",
                    "failed" => "Send failed",
                    "sending" => "Sending",
                    _ => "Stored in this preview",
                }));
                meta.append(&image)
            }
            heading.append(&meta);
            bubble.append(&heading);
            if !m.reply.is_empty() {
                let reply = gtk::Button::new();
                reply.add_css_class("message-reply");
                reply.set_child(Some(&wrap(&m.reply, "")));
                let weak = Rc::downgrade(self);
                let target = m.reply_id.clone();
                let snippet = m
                    .reply
                    .split_once(" · ")
                    .map(|(_, v)| v)
                    .unwrap_or("")
                    .to_string();
                reply.connect_clicked(move |_| {
                    if let Some(ui) = weak.upgrade() {
                        let id = ui
                            .state
                            .borrow()
                            .current()
                            .messages
                            .iter()
                            .find(|m| {
                                (!target.is_empty()
                                    && (m.id == target
                                        || m.wire_id == target
                                        || m.aliases.iter().any(|a| {
                                            a == &format!("origin:{target}")
                                                || a == &format!("server:{target}")
                                        })))
                                    || (target.is_empty()
                                        && !snippet.is_empty()
                                        && m.text.starts_with(&snippet))
                            })
                            .map(|m| m.id.clone());
                        if let Some(id) = id {
                            ui.highlight(&id)
                        } else {
                            ui.notice("Original message is not loaded. Load earlier messages.")
                        }
                    }
                });
                bubble.append(&reply)
            }
            if m.kind == "photo" {
                let picture = AspectImage::new(&asset("landscape.svg"));
                picture.set_margin_bottom(9);
                picture.set_widget_name(&m.id);
                let click = gtk::GestureClick::new();
                let weak = Rc::downgrade(self);
                let source = picture.downgrade();
                click.connect_released(move |_, _, _, _| {
                    if let (Some(ui), Some(source)) = (weak.upgrade(), source.upgrade()) {
                        ui.media_viewer(Some(source.upcast()))
                    }
                });
                picture.add_controller(click);
                bubble.append(&picture)
            }
            match m.kind.as_str() {
                "poll" => {
                    bubble.append(&wrap(&m.text, "heading"));
                    bubble.append(&text("Public poll · choose one", "caption dim-label"));
                    let options = if m.options.is_empty() {
                        vec![
                            ("The café by the park".into(), 5),
                            ("Somewhere with a view".into(), 3),
                            ("Let's stay in".into(), 1),
                        ]
                    } else {
                        m.options.clone()
                    };
                    let mut first: Option<gtk::ToggleButton> = None;
                    let count = options.iter().map(|(_, n)| *n).sum::<u32>();
                    for (title, count) in options {
                        let b = gtk::ToggleButton::new();
                        b.add_css_class("poll-answer");
                        b.set_margin_top(6);
                        let answer = bx(false, 16);
                        let l = wrap(&title, "");
                        l.set_hexpand(true);
                        answer.append(&l);
                        answer.append(&label(&count.to_string(), ""));
                        b.set_child(Some(&answer));
                        if let Some(ref first) = first {
                            b.set_group(Some(first))
                        } else {
                            first = Some(b.clone())
                        }
                        let weak = Rc::downgrade(self);
                        b.connect_toggled(move |b| {
                            if b.is_active() {
                                if let Some(ui) = weak.upgrade() {
                                    ui.notice("Vote updated locally")
                                }
                            }
                        });
                        bubble.append(&b)
                    }
                    let l = text(&format!("{count} votes"), "caption dim-label");
                    l.set_margin_top(6);
                    bubble.append(&l)
                }
                "file" => {
                    let file = bx(false, 10);
                    let image = gtk::Image::from_icon_name("application-pdf-symbolic");
                    image.set_pixel_size(32);
                    file.append(&image);
                    let words = bx(true, 2);
                    words.append(&wrap(
                        if m.own { &m.text } else { "Desktop notes.pdf" },
                        "heading",
                    ));
                    words.append(&text("Local file preview", "caption dim-label"));
                    file.append(&words);
                    bubble.append(&file);
                    if !m.own {
                        bubble.append(&wrap(&m.text, ""))
                    }
                }
                "gif" => {
                    let gif = AnimatedImage::new(m.gif_index);
                    gif.set_width_request(220);
                    gif.constrain_width(220);
                    bubble.append(&gif)
                }
                "sticker" => bubble.append(&label(&m.text, "large-emoji")),
                "payment" => {
                    let card = bx(true, 5);
                    card.add_css_class("payment-card");
                    card.append(&label("MONERO REQUEST", "caption"));
                    card.append(&label(&m.text, "title-1"));
                    card.append(&label("Payment preview · no funds involved", "caption"));
                    bubble.append(&card)
                }
                "location" => {
                    let tile = bx(true, 8);
                    margins(&tile, 16);
                    let image = gtk::Image::from_icon_name("mark-location-symbolic");
                    image.set_pixel_size(40);
                    tile.append(&image);
                    tile.append(&label("Riverside Park", "heading"));
                    tile.append(&label("Location preview", "caption dim-label"));
                    bubble.append(&tile)
                }
                _ => {
                    let css = if self.big_emoji.get()
                        && m.text.chars().count() < 5
                        && m.text.chars().any(|c| c as u32 > 0x2000)
                    {
                        "large-emoji"
                    } else {
                        "message-text"
                    };
                    let l = wrap(&m.text, css);
                    l.set_selectable(true);
                    bubble.append(&l)
                }
            }
            if !m.reactions.is_empty() {
                let row = bx(false, 4);
                for reaction in &m.reactions {
                    let emoji = reaction
                        .rsplit_once(' ')
                        .map(|(e, _)| e)
                        .unwrap_or(reaction)
                        .to_string();
                    let b = gtk::ToggleButton::with_label(reaction);
                    b.set_active(m.my_reactions.contains(&emoji));
                    b.add_css_class("reaction");
                    let id = m.id.clone();
                    let weak = Rc::downgrade(self);
                    b.connect_toggled(move |_| {
                        if let Some(ui) = weak.upgrade() {
                            ui.react(&id, &emoji)
                        }
                    });
                    row.append(&b)
                }
                bubble.append(&row)
            }
            outer.append(&bubble);
            self.bubbles
                .borrow_mut()
                .insert(m.id.clone(), bubble.clone().upcast());
            let row = gtk::ListBoxRow::builder()
                .child(&outer)
                .activatable(false)
                .valign(gtk::Align::Start)
                .vexpand(false)
                .build();
            row.add_css_class("message-row");
            row.set_widget_name(&m.id);
            if self.selected.borrow().contains(&m.id) {
                row.add_css_class("selected-message")
            }
            let click = gtk::GestureClick::new();
            click.set_button(1);
            click.set_propagation_phase(gtk::PropagationPhase::Capture);
            let id = m.id.clone();
            let weak = Rc::downgrade(self);
            click.connect_pressed(move |g, _, _, _| {
                if let Some(ui) = weak.upgrade() {
                    let flags = g.current_event_state();
                    if flags
                        .intersects(gdk::ModifierType::CONTROL_MASK | gdk::ModifierType::SHIFT_MASK)
                        || !ui.selected.borrow().is_empty()
                    {
                        g.set_state(gtk::EventSequenceState::Claimed);
                        ui.select_message(&id, flags.contains(gdk::ModifierType::SHIFT_MASK));
                    }
                }
            });
            row.add_controller(click);
            let right = gtk::GestureClick::new();
            right.set_button(3);
            right.set_propagation_phase(gtk::PropagationPhase::Capture);
            let id = m.id.clone();
            let weak = Rc::downgrade(self);
            let parent = bubble.downgrade();
            right.connect_pressed(move |g, _, x, y| {
                g.set_state(gtk::EventSequenceState::Claimed);
                if let (Some(ui), Some(parent)) = (weak.upgrade(), parent.upgrade()) {
                    ui.message_menu(&parent, &id, x, y)
                }
            });
            bubble.add_controller(right);
            self.timeline.append(&row);
        }
        self.update_selection();
        let weak = Rc::downgrade(self);
        glib::idle_add_local_once(move || {
            if let Some(ui) = weak.upgrade() {
                let a = ui.scroll.vadjustment();
                a.set_value(if bottom {
                    a.upper() - a.page_size()
                } else {
                    old_value
                });
            }
        });
        if live {
            self.history.set_visible(true)
        }
    }
    pub fn highlight(self: &Rc<Self>, id: &str) {
        let weak = Rc::downgrade(self);
        let id = id.to_string();
        glib::timeout_add_local_once(Duration::from_millis(50), move || {
            if let Some(ui) = weak.upgrade() {
                if let Some(widget) = ui.bubbles.borrow().get(&id) {
                    if let Some(bounds) = widget.compute_bounds(&ui.timeline) {
                        ui.scroll
                            .vadjustment()
                            .set_value((bounds.y() as f64 - 16.).max(0.))
                    }
                    widget.add_css_class("reply-target");
                    let weak = widget.downgrade();
                    glib::timeout_add_local_once(Duration::from_millis(1200), move || {
                        if let Some(w) = weak.upgrade() {
                            w.remove_css_class("reply-target")
                        }
                    });
                }
            }
        });
    }
    pub fn select_message(self: &Rc<Self>, id: &str, extend: bool) {
        let ids: Vec<_> = self
            .state
            .borrow()
            .current()
            .messages
            .iter()
            .map(|m| m.id.clone())
            .collect();
        let mut selected = self.selected.borrow_mut();
        if extend {
            let anchor = self.anchor.borrow().clone();
            if let (Some(a), Some(b)) = (
                anchor
                    .as_ref()
                    .and_then(|a| ids.iter().position(|i| i == a)),
                ids.iter().position(|i| i == id),
            ) {
                for value in &ids[a.min(b)..=a.max(b)] {
                    selected.insert(value.clone());
                }
            }
        } else {
            if !selected.remove(id) {
                selected.insert(id.into());
            }
            self.anchor.replace(Some(id.into()));
        }
        drop(selected);
        self.update_selection();
    }
    pub fn update_selection(&self) {
        let selected = self.selected.borrow();
        self.selection.set_revealed(!selected.is_empty());
        self.selection_count
            .set_text(&format!("{} selected", selected.len()));
        let mut child = self.timeline.first_child();
        while let Some(c) = child {
            child = c.next_sibling();
            if selected.contains(c.widget_name().as_str()) {
                c.add_css_class("selected-message")
            } else {
                c.remove_css_class("selected-message")
            }
        }
    }
    pub fn clear_selection(&self) {
        self.selected.borrow_mut().clear();
        self.anchor.borrow_mut().take();
        self.update_selection();
    }
    pub fn selection_action(self: &Rc<Self>, action: &str) {
        let messages: Vec<_> = self
            .state
            .borrow()
            .current()
            .messages
            .iter()
            .filter(|m| self.selected.borrow().contains(&m.id))
            .cloned()
            .collect();
        match action {
            "select-all" => {
                self.selected.replace(
                    self.state
                        .borrow()
                        .current()
                        .messages
                        .iter()
                        .map(|m| m.id.clone())
                        .collect(),
                );
                self.update_selection()
            }
            "copy" => self.window.clipboard().set_text(
                &messages
                    .iter()
                    .map(|m| format!("{} · {}\n{}", m.author, m.time, m.text))
                    .collect::<Vec<_>>()
                    .join("\n\n"),
            ),
            "forward" => self.forward(messages),
            "delete" => self.delete_confirmation(messages.iter().map(|m| m.id.clone()).collect()),
            _ => {}
        }
    }
    pub fn delete_confirmation(self: &Rc<Self>, ids: Vec<String>) {
        let owner = self.state.borrow().account.clone();
        let chat = self.state.borrow().active.clone();
        let d=adw::AlertDialog::builder().heading(format!("Delete {} messages?",ids.len())).body("This removes the messages locally. Server history and other devices are unaffected.").build();
        d.add_responses(&[("cancel", "Cancel"), ("delete", "Delete Locally")]);
        let peer = self.state.borrow().current().jid.clone();
        if !owner.is_empty()
            && !ids.is_empty()
            && ids.iter().all(|id| {
                self.store
                    .action_target(&owner, &peer, id, "retract")
                    .is_ok()
            })
        {
            d.set_body("Delete from this device, or request removal on other clients. A retraction cannot guarantee remote deletion or undo copies already made.");
            d.add_response("retract", "Request Retraction");
            d.set_response_appearance("retract", adw::ResponseAppearance::Destructive);
        }
        d.set_response_appearance("delete", adw::ResponseAppearance::Destructive);
        let weak = Rc::downgrade(self);
        d.connect_response(None, move |_, response| {
            if matches!(response, "delete" | "retract") {
                if let Some(ui) = weak.upgrade() {
                    if ui.state.borrow().account != owner || ui.state.borrow().active != chat {
                        return;
                    }
                    if response == "retract" {
                        for id in &ids {
                            let target = ui
                                .store
                                .action_base(&owner, &peer, id)
                                .ok()
                                .and_then(|m| crate::message_actions::original_wire(&m));
                            let Some(target) = target else { break };
                            if !ui.send_action(
                                id,
                                crate::message_actions::Action::Retract {
                                    target,
                                    legacy: false,
                                },
                                "",
                            ) {
                                break;
                            }
                        }
                    } else {
                        ui.delete_messages(&ids);
                    }
                }
            }
        });
        d.present(Some(&self.window));
    }
    pub fn delete_messages(self: &Rc<Self>, ids: &[String]) {
        let mut s = self.state.borrow_mut();
        let peer = s.current().jid.clone();
        if !s.account.is_empty() {
            for id in ids {
                if self.store.delete(&s.account, &peer, id).is_err() {
                    drop(s);
                    self.notice("Local deletion failed");
                    return;
                }
            }
        }
        s.current_mut().messages.retain(|m| !ids.contains(&m.id));
        drop(s);
        self.clear_selection();
        self.persist();
        self.render_messages(false);
        self.refresh_chats(None);
    }
    pub fn react(self: &Rc<Self>, id: &str, emoji: &str) {
        if !self.state.borrow().account.is_empty() {
            let state = self.state.borrow();
            let peer = state.current().jid.clone();
            let owner = state.account.clone();
            drop(state);
            let Ok(message) = self.store.action_target(&owner, &peer, id, "react") else {
                self.notice("This message cannot be reacted to");
                return;
            };
            let Some(emoji) = crate::message_actions::canonical_emoji(emoji) else {
                return;
            };
            let Some(target) = crate::message_actions::reaction_reference(&message) else {
                return;
            };
            let mut emojis = message.my_reactions;
            if !emojis.remove(&emoji) {
                if emojis.len() >= 32 {
                    self.notice("Use at most 32 reactions");
                    return;
                }
                emojis.insert(emoji);
            }
            self.send_action(
                id,
                crate::message_actions::Action::React { target, emojis },
                "",
            );
            return;
        }
        let mut s = self.state.borrow_mut();
        if let Some(m) = s.current_mut().messages.iter_mut().find(|m| m.id == id) {
            let mine = m.my_reactions.remove(emoji);
            if !mine {
                m.my_reactions.insert(emoji.into());
            }
            let index = m
                .reactions
                .iter()
                .position(|r| r.rsplit_once(' ').is_some_and(|(e, _)| e == emoji));
            if let Some(index) = index {
                let n = m.reactions[index]
                    .rsplit_once(' ')
                    .and_then(|(_, n)| n.parse::<i32>().ok())
                    .unwrap_or(0)
                    + if mine { -1 } else { 1 };
                if n <= 0 {
                    m.reactions.remove(index);
                } else {
                    m.reactions[index] = format!("{emoji} {n}")
                }
            } else {
                m.reactions.push(format!("{emoji} 1"))
            }
        }
        drop(s);
        self.render_messages(false);
    }
    pub fn message_menu(self: &Rc<Self>, parent: &gtk::Box, id: &str, x: f64, y: f64) {
        let Some(message) = self.state.borrow().message(id).cloned() else {
            return;
        };
        let live = !self.state.borrow().account.is_empty();
        let pop = gtk::Popover::builder().has_arrow(false).build();
        pop.set_parent(parent);
        pop.set_pointing_to(Some(&gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
        pop.add_css_class("message-menu");
        let content = bx(true, 8);
        content.set_width_request(260);
        if !message.unavailable && !message.retracted {
            let reactions = bx(false, 0);
            reactions.set_homogeneous(true);
            reactions.add_css_class("message-menu-panel");
            for e in ["👍", "❤️", "😂", "😮", "😢", "🔥"] {
                let b = gtk::Button::with_label(e);
                b.add_css_class("flat");
                b.add_css_class("reaction-choice");
                let weak = Rc::downgrade(self);
                let p = pop.downgrade();
                let id = id.to_string();
                b.connect_clicked(move |_| {
                    if let Some(p) = p.upgrade() {
                        p.popdown();
                        p.unparent()
                    }
                    if let Some(ui) = weak.upgrade() {
                        ui.react(&id, e)
                    }
                });
                reactions.append(&b)
            }
            content.append(&reactions)
        }
        let items = bx(true, 2);
        items.set_halign(gtk::Align::Center);
        items.set_width_request(208);
        items.add_css_class("message-menu-panel");
        let mut choices = vec![];
        if message.own
            && !message.unavailable
            && message.kind == "text"
            && (!live
                || self
                    .store
                    .action_target(
                        &self.state.borrow().account,
                        &self.state.borrow().current().jid,
                        id,
                        "correct",
                    )
                    .is_ok())
        {
            choices.push(("document-edit-symbolic", "Edit", "edit"))
        }
        if message.own && matches!(message.delivery.as_str(), "failed" | "uncertain") {
            choices.push(("view-refresh-symbolic", "Retry", "retry"))
        }
        choices.push((
            "view-pin-symbolic",
            if message.pinned {
                "Unpin Locally"
            } else {
                "Pin Locally"
            },
            "pin",
        ));
        choices.push(("object-select-symbolic", "Select", "select"));
        if !message.own && !live {
            choices.push(("dialog-warning-symbolic", "Report", "report"))
        }
        choices.push(("info-outline-symbolic", "Message Details", "details"));
        for (icon, title, action) in choices {
            let b = gtk::Button::new();
            b.add_css_class("flat");
            let line = bx(false, 12);
            line.append(&gtk::Image::from_icon_name(icon));
            let l = text(title, "");
            l.set_hexpand(true);
            line.append(&l);
            b.set_child(Some(&line));
            self.menu_callback(&b, &pop, id, action);
            items.append(&b)
        }
        content.append(&items);
        let tools = bx(false, 0);
        tools.set_homogeneous(true);
        tools.set_halign(gtk::Align::Center);
        tools.set_width_request(208);
        tools.add_css_class("message-menu-panel");
        for (icon, title, action) in [
            ("mail-reply-sender-symbolic", "Reply", "reply"),
            ("user-trash-symbolic", "Delete Locally", "delete"),
            ("edit-copy-symbolic", "Copy", "copy"),
            ("mail-forward-symbolic", "Forward", "forward"),
        ] {
            let b = icon_button(icon, title);
            self.menu_callback(&b, &pop, id, action);
            tools.append(&b)
        }
        content.append(&tools);
        pop.set_child(Some(&content));
        pop.connect_closed(|p| {
            if p.parent().is_some() {
                p.unparent()
            }
        });
        pop.popup();
    }
    fn menu_callback(
        self: &Rc<Self>,
        button: &gtk::Button,
        pop: &gtk::Popover,
        id: &str,
        action: &'static str,
    ) {
        let weak = Rc::downgrade(self);
        let p = pop.downgrade();
        let id = id.to_string();
        let owner = self.state.borrow().account.clone();
        let chat = self.state.borrow().active.clone();
        button.connect_clicked(move |_| {
            if let Some(p) = p.upgrade() {
                p.popdown();
                if p.parent().is_some() {
                    p.unparent()
                }
            }
            if let Some(ui) = weak.upgrade() {
                if ui.state.borrow().account != owner || ui.state.borrow().active != chat {
                    return;
                }
                let Some(m) = ui.state.borrow().message(&id).cloned() else {
                    return;
                };
                match action {
                    "reply" => {
                        ui.replying.replace(Some(id.clone()));
                        ui.update_reply();
                        ui.composer.grab_focus();
                        ui.save_draft()
                    }
                    "select" => ui.select_message(&id, false),
                    "pin" => {
                        let mut s = ui.state.borrow_mut();
                        if let Some(m) = s.current_mut().messages.iter_mut().find(|m| m.id == id) {
                            m.pinned = !m.pinned
                        }
                        drop(s);
                        ui.persist();
                        ui.render_messages(false)
                    }
                    "copy" => ui.window.clipboard().set_text(&m.text),
                    "delete" => ui.delete_confirmation(vec![id.clone()]),
                    "forward" => ui.forward(vec![m]),
                    "edit" => ui.edit_message(&id),
                    "retry" => ui.retry(&id),
                    "details" => ui.message_details(&m),
                    "report" => ui.notice("Local preview — no report was sent"),
                    _ => {}
                }
            }
        });
    }
    pub fn edit_message(self: &Rc<Self>, id: &str) {
        let live = !self.state.borrow().account.is_empty();
        let owner = self.state.borrow().account.clone();
        let peer = self.state.borrow().current().jid.clone();
        let chat = self.state.borrow().active.clone();
        let generation = self.generation.get();
        if live
            && self
                .store
                .action_target(&owner, &peer, id, "correct")
                .is_err()
        {
            self.notice("This message cannot be edited");
            return;
        }
        let Some(m) = self.state.borrow().message(id).cloned() else {
            return;
        };
        let Some(bubble) = self
            .bubbles
            .borrow()
            .get(id)
            .cloned()
            .and_downcast::<gtk::Box>()
        else {
            return;
        };
        clear(&bubble);
        let editor = gtk::TextView::builder()
            .wrap_mode(gtk::WrapMode::WordChar)
            .build();
        let draft = if live {
            self.store
                .failed_edit(&owner, &peer, &m.wire_id)
                .unwrap_or_else(|| m.text.clone())
        } else {
            m.text.clone()
        };
        editor.buffer().set_text(&draft);
        let scroll = scroll(&editor);
        scroll.set_min_content_height(60);
        bubble.append(&scroll);
        let actions = bx(false, 6);
        actions.set_halign(gtk::Align::End);
        let cancel = gtk::Button::with_label("Cancel");
        self.weak_button(&cancel, |s| s.render_messages(false));
        actions.append(&cancel);
        let save = gtk::Button::with_label("Save");
        save.add_css_class("suggested-action");
        let weak = Rc::downgrade(self);
        let id = id.to_string();
        save.connect_clicked(move |_| {
            if let Some(ui) = weak.upgrade() {
                if ui.generation.get() != generation
                    || ui.state.borrow().account != owner
                    || ui.state.borrow().active != chat
                {
                    return;
                }
                let b = editor.buffer();
                let value = b.text(&b.start_iter(), &b.end_iter(), true);
                if value.trim().is_empty() {
                    return;
                }
                if live {
                    if value.len() > 64 * 1024 {
                        ui.notice("Edited message is too long");
                        return;
                    }
                    let Some(target) = crate::message_actions::original_wire(&m) else {
                        return;
                    };
                    ui.send_action(
                        &id,
                        crate::message_actions::Action::Correct { target },
                        &value,
                    );
                    return;
                }
                if let Some(m) = ui
                    .state
                    .borrow_mut()
                    .current_mut()
                    .messages
                    .iter_mut()
                    .find(|m| m.id == id)
                {
                    m.text = value.into();
                }
                ui.render_messages(false);
                ui.refresh_chats(None);
            }
        });
        actions.append(&save);
        bubble.append(&actions);
    }
}
