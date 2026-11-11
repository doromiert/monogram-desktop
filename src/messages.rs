use crate::{model::*, ui::Ui, widgets::*};
use adw::prelude::*;
use gtk::{gdk, glib};
use std::{
    cell::Cell,
    collections::{HashMap, HashSet},
    rc::Rc,
    time::Duration,
};
#[derive(Default)]
pub(crate) struct RenderedRows {
    scope: (String, String),
    rows: HashMap<String, RenderedRow>,
    dates: HashMap<String, gtk::ListBoxRow>,
    big_emoji: bool,
    group: bool,
    render_request: Option<ScrollTarget>,
}
struct RenderedRow {
    message: Message,
    row: gtk::ListBoxRow,
    bubble: gtk::Box,
    status: gtk::Box,
    heading: gtk::Box,
    avatar: Option<adw::Avatar>,
}
fn same_content(a: &Message, b: &Message) -> bool {
    a.text == b.text
        && a.kind == b.kind
        && a.author == b.author
        && a.sender == b.sender
        && a.own == b.own
        && a.reply == b.reply
        && a.reply_id == b.reply_id
        && a.reply_to == b.reply_to
        && a.attachment == b.attachment
        && a.forwarded == b.forwarded
        && a.poll == b.poll
        && a.poll_tally == b.poll_tally
        && a.poll_closed_at == b.poll_closed_at
        && a.reactions == b.reactions
        && a.my_reactions == b.my_reactions
        && a.preview_spoilers == b.preview_spoilers
        && a.inline_emoji == b.inline_emoji
        && a.unavailable == b.unavailable
        && a.pinned == b.pinned
        && a.options == b.options
        && a.gif_index == b.gif_index
        && a.upload.as_ref().map(|u| (&u.uri, &u.name, &u.error))
            == b.upload.as_ref().map(|u| (&u.uri, &u.name, &u.error))
}
fn clustered(a: &Message, b: &Message, group: bool) -> bool {
    if a.own != b.own
        || group && a.sender != b.sender
        || a.unavailable != b.unavailable
        || a.forwarded != b.forwarded
        || !a.unread && b.unread
    {
        return false;
    }
    match (
        chrono::DateTime::parse_from_rfc3339(&a.timestamp),
        chrono::DateTime::parse_from_rfc3339(&b.timestamp),
    ) {
        (Ok(a), Ok(b)) => {
            let (a, b) = (
                a.with_timezone(&chrono::Local),
                b.with_timezone(&chrono::Local),
            );
            a.date_naive() == b.date_naive() && (b - a).num_seconds().abs() <= 300
        }
        _ => a.timestamp.is_empty() && b.timestamp.is_empty() && a.time == b.time,
    }
}
fn cluster_style(row: &RenderedRow, previous: bool, next: bool, group: bool) {
    for (class, enabled) in [("cluster-previous", previous), ("cluster-next", next)] {
        if enabled {
            row.bubble.add_css_class(class);
        } else {
            row.bubble.remove_css_class(class);
        }
    }
    if next {
        row.row.add_css_class("cluster-next");
    } else {
        row.row.remove_css_class("cluster-next");
    }
    row.heading
        .set_visible(group && !row.message.own && !previous || row.message.pinned);
    if let Some(avatar) = row.avatar.as_ref() {
        avatar.set_visible(!next);
    }
}
fn message_status(meta: &gtk::Box, m: &Message) {
    clear(meta);
    if m.encrypted
        && (!m.own
            || !matches!(
                m.delivery.as_str(),
                "uploading"
                    | "sending"
                    | "waiting-upload"
                    | "preparing-encryption"
                    | "queued"
                    | "failed"
                    | "uncertain"
            ))
    {
        let image = gtk::Image::from_icon_name("channel-secure-symbolic");
        image.set_pixel_size(11);
        image.set_tooltip_text(Some(m.mode().name()));
        meta.append(&image);
    }
    if m.edited {
        meta.append(&label("edited", ""));
    }
    meta.append(&label(&m.time, ""));
    if m.own {
        match m.delivery.as_str() {
            "uploading" | "sending" | "waiting-upload" | "preparing-encryption" | "queued" => {
                let spinner = gtk::Spinner::new();
                spinner.add_css_class("message-status-spinner");
                spinner.set_spinning(true);
                spinner.set_tooltip_text(Some(match m.delivery.as_str() {
                    "uploading" | "waiting-upload" => "Uploading",
                    "preparing-encryption" => "Preparing encryption",
                    "queued" => "Queued",
                    _ => "Sending",
                }));
                meta.append(&spinner);
            }
            "failed" | "uncertain" => {
                let image = gtk::Image::from_icon_name("dialog-warning-symbolic");
                image.set_pixel_size(12);
                image.set_tooltip_text(Some(if m.delivery == "failed" {
                    "Send failed"
                } else {
                    "Delivery uncertain"
                }));
                meta.append(&image);
            }
            status => {
                let read = status == "read";
                let tick = label(if read { "✓✓" } else { "✓" }, "message-status-tick");
                tick.add_css_class(if read { "read" } else { "sent" });
                tick.set_tooltip_text(Some(if read {
                    "Read"
                } else if status == "delivered" {
                    "Delivered"
                } else {
                    "Sent"
                }));
                meta.append(&tick);
            }
        }
    }
    if let Some(parent) = meta.parent() {
        if let Some(text) = parent.parent().and_downcast::<MessageText>() {
            text.refresh_shape();
        }
        parent.queue_resize();
    }
}
fn local_day(message: &Message) -> String {
    chrono::DateTime::parse_from_rfc3339(&message.timestamp)
        .map(|date| {
            date.with_timezone(&chrono::Local)
                .format("%Y-%m-%d")
                .to_string()
        })
        .unwrap_or_else(|_| "Today".into())
}
pub enum ScrollTarget {
    Bottom,
    Message(String),
    Position {
        value: f64,
        anchor: Option<(String, f64)>,
    },
}
impl Ui {
    pub(crate) fn refresh_link_preview_rows(self: &Rc<Self>, url: &str) {
        let ids: Vec<_> = self
            .state
            .borrow()
            .current()
            .messages
            .iter()
            .filter(|message| crate::link_previews::candidate(message).as_deref() == Some(url))
            .map(|message| message.id.clone())
            .collect();
        if ids.is_empty() {
            return;
        }
        for id in ids {
            self.rendered_rows.borrow_mut().rows.remove(&id);
        }
        self.render_messages(false);
    }

    pub fn render_messages(self: &Rc<Self>, bottom: bool) {
        if bottom && !self.history_window.newest() && !self.state.borrow().account.is_empty() {
            self.load_timeline(None);
            return;
        }
        let position = if bottom {
            ScrollTarget::Bottom
        } else {
            self.capture_scroll_position()
        };
        let mut rows = self.rendered_rows.borrow_mut();
        let scheduled = rows.render_request.is_some();
        if !scheduled || bottom {
            rows.render_request = Some(position);
        }
        drop(rows);
        if !scheduled {
            let weak = Rc::downgrade(self);
            self.scroll.add_tick_callback(move |_, _| {
                if let Some(ui) = weak.upgrade() {
                    let target = ui.rendered_rows.borrow_mut().render_request.take();
                    if let Some(target) = target {
                        ui.render_messages_at(target);
                    }
                }
                glib::ControlFlow::Break
            });
        }
    }
    pub fn render_messages_at(self: &Rc<Self>, position: ScrollTarget) {
        self.rendered_rows.borrow_mut().render_request.take();
        self.bubbles.borrow_mut().clear();
        let state = self.state.borrow();
        let messages = crate::media_caption::display_messages(&state.current().messages);
        let group = state.current().group;
        let scope = (state.account.clone(), state.current().jid.clone());
        let messages: Vec<_> = messages
            .into_iter()
            .filter(|message| !message.retracted)
            .collect();
        drop(state);
        let mut rendered = self.rendered_rows.borrow_mut();
        let big_emoji = self.big_emoji.get();
        if rendered.scope != scope || rendered.big_emoji != big_emoji || rendered.group != group {
            *rendered = RenderedRows {
                scope,
                big_emoji,
                group,
                ..Default::default()
            };
        }
        let ids: HashSet<_> = messages.iter().map(|message| message.id.as_str()).collect();
        rendered.rows.retain(|id, _| ids.contains(id.as_str()));
        let mut order = Vec::new();
        let mut days = HashSet::new();
        let mut last = String::new();
        for (index, m) in messages.iter().enumerate() {
            let day = local_day(m);
            if day != last {
                let row = rendered.dates.entry(day.clone()).or_insert_with(|| {
                    gtk::ListBoxRow::builder()
                        .selectable(false)
                        .activatable(false)
                        .child(&label(&day, "date-pill"))
                        .build()
                });
                order.push(row.clone());
                days.insert(day.clone());
                last = day;
            }
            let previous = index > 0 && clustered(&messages[index - 1], m, group);
            let next = messages
                .get(index + 1)
                .is_some_and(|next| clustered(m, next, group));
            if let Some(row) = rendered
                .rows
                .get_mut(&m.id)
                .filter(|row| same_content(&row.message, m))
            {
                if row.message.delivery != m.delivery
                    || row.message.time != m.time
                    || row.message.edited != m.edited
                    || row.message.encrypted != m.encrypted
                    || row.message.encryption_mode != m.encryption_mode
                {
                    message_status(&row.status, m);
                }
                row.message = m.clone();
                cluster_style(row, previous, next, group);
                if self
                    .history_window
                    .highlighted
                    .borrow()
                    .as_ref()
                    .is_some_and(|(id, _)| id == &m.id)
                {
                    row.bubble.add_css_class("reply-target");
                } else {
                    row.bubble.remove_css_class("reply-target");
                }
                self.bubbles
                    .borrow_mut()
                    .insert(m.id.clone(), row.bubble.clone().upcast());
                order.push(row.row.clone());
                continue;
            }
            if let Some(displaced) = rendered.rows.remove(&m.id)
                && displaced.row.parent().as_ref() == Some(self.timeline.upcast_ref())
            {
                self.timeline.remove(&displaced.row);
            }
            let outer = bx(false, 8);
            outer.set_valign(gtk::Align::Start);
            outer.set_vexpand(false);
            outer.set_halign(gtk::Align::Fill);
            outer.set_hexpand(true);
            let mut avatar = None;
            if group && !m.own {
                let a = self.chat_avatar(
                    &Chat {
                        name: m.author.clone(),
                        jid: m.sender.clone(),
                        group: true,
                        color: "purple".into(),
                        ..Default::default()
                    },
                    26,
                );
                a.set_valign(gtk::Align::End);
                let slot = bx(false, 0);
                slot.set_width_request(26);
                slot.set_valign(gtk::Align::End);
                slot.append(&a);
                outer.append(&slot);
                avatar = Some(a);
            }
            let sticker = m.attachment.as_ref().is_some_and(|file| file.sticker);
            let media = m
                .attachment
                .as_ref()
                .is_some_and(|file| file.media_candidate());
            let bubble = bx(true, 0);
            bubble.set_vexpand(false);
            bubble.set_valign(gtk::Align::Start);
            bubble.add_css_class("message-bubble");
            if self
                .history_window
                .highlighted
                .borrow()
                .as_ref()
                .is_some_and(|(id, _)| id == &m.id)
            {
                bubble.add_css_class("reply-target");
            }
            if m.own {
                bubble.add_css_class("own")
            }
            if sticker {
                bubble.add_css_class("sticker-bubble");
            } else if media {
                bubble.add_css_class("media-bubble");
            }
            let heading = bx(false, 6);
            heading.set_margin_bottom(6);
            let author = text(&m.author, "message-author");
            author.set_hexpand(true);
            author.set_valign(gtk::Align::Center);
            author.set_ellipsize(gtk::pango::EllipsizeMode::End);
            if m.pinned {
                heading.append(&gtk::Image::from_icon_name("view-pin-symbolic"))
            }
            heading.append(&author);
            let meta = bx(false, 4);
            meta.add_css_class("message-meta");
            meta.set_valign(gtk::Align::Center);
            message_status(&meta, m);
            bubble.append(&heading);
            if let Some(attribution) = m.forwarded.clone() {
                let header = gtk::Button::new();
                header.add_css_class("flat");
                header.set_halign(gtk::Align::Start);
                header.set_margin_bottom(4);
                let title = text(&format!("Forwarded from {}", attribution.name), "caption");
                title.set_ellipsize(gtk::pango::EllipsizeMode::End);
                title.set_max_width_chars(36);
                title.set_single_line_mode(true);
                header.set_child(Some(&title));
                header.set_tooltip_text(Some(&attribution.name));
                let weak = Rc::downgrade(self);
                header.connect_clicked(move |_| {
                    if let Some(ui) = weak.upgrade() {
                        ui.forwarded_details(&attribution);
                    }
                });
                bubble.append(&header);
            }
            if !m.reply.is_empty() || !m.reply_id.is_empty() {
                let reply = gtk::Button::new();
                reply.add_css_class("message-reply");
                let contents = bx(true, 2);
                let (reply_author, quote) = m.reply.split_once(" · ").unwrap_or((
                    if m.reply_to == self.state.borrow().account {
                        "You"
                    } else {
                        "Reply"
                    },
                    &m.reply,
                ));
                let title = text(reply_author, "reply-author");
                title.set_ellipsize(gtk::pango::EllipsizeMode::End);
                let preview = quote
                    .lines()
                    .map(|line| line.strip_prefix("> ").unwrap_or(line))
                    .collect::<Vec<_>>()
                    .join("\n");
                let snippet = wrap(
                    if preview.is_empty() {
                        "Original message"
                    } else {
                        &preview
                    },
                    "reply-preview",
                );
                snippet.set_lines(2);
                snippet.set_ellipsize(gtk::pango::EllipsizeMode::End);
                contents.append(&title);
                contents.append(&snippet);
                reply.set_child(Some(&contents));
                let weak = Rc::downgrade(self);
                let target = m.reply_id.clone();
                let author = m.reply_to.clone();
                let snippet = m
                    .reply
                    .split_once(" · ")
                    .map(|(_, v)| v)
                    .unwrap_or("")
                    .to_string();
                reply.connect_clicked(move |_| {
                    if let Some(ui) = weak.upgrade() {
                        let state = ui.state.borrow();
                        let chat = state.current();
                        let id = if !state.account.is_empty() {
                            ui.store
                                .resolve_reply(&state.account, &chat.jid, &author, &target)
                                .filter(|m| !m.retracted)
                                .map(|m| m.id)
                        } else {
                            chat.messages
                                .iter()
                                .find(|m| {
                                    !target.is_empty() && m.id == target
                                        || target.is_empty()
                                            && !snippet.is_empty()
                                            && m.text.starts_with(&snippet)
                                })
                                .map(|m| m.id.clone())
                        };
                        drop(state);
                        if let Some(id) = id {
                            ui.highlight(&id)
                        } else {
                            ui.notice("Original message is unavailable")
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
                "poll" if m.poll.is_some() => {
                    bubble.append(&self.poll_content(&m));
                }
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
                "file" if m.upload.is_some() => {
                    bubble.append(&self.upload_content(&m));
                    if !m.text.is_empty() {
                        bubble.append(&MessageText::new(&wrap(&m.text, "message-text"), &meta));
                    }
                }
                "file" if m.attachment.is_some() => {
                    let attachment = self.attachment_content(&m);
                    if sticker || media && m.text.is_empty() {
                        let overlay = gtk::Overlay::new();
                        overlay.set_child(Some(&attachment));
                        meta.add_css_class("sticker-meta");
                        meta.set_halign(gtk::Align::End);
                        meta.set_valign(gtk::Align::End);
                        meta.set_can_target(false);
                        overlay.add_overlay(&meta);
                        bubble.append(&overlay);
                    } else {
                        bubble.append(&attachment);
                    }
                    if !m.text.is_empty() {
                        let caption = MessageText::new(&wrap(&m.text, "message-text"), &meta);
                        caption.set_margin_start(9);
                        caption.set_margin_end(9);
                        caption.set_margin_top(5);
                        caption.set_margin_bottom(4);
                        bubble.append(&caption);
                    }
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
                _ if !m.text.is_empty() => {
                    let css = if self.big_emoji.get()
                        && m.text.chars().count() < 5
                        && m.text.chars().any(|c| c as u32 > 0x2000)
                    {
                        "large-emoji"
                    } else {
                        "message-text"
                    };
                    let preview = self.link_preview_for_message(&m);
                    let inline = m.inline_emoji.as_ref().map(|emoji| {
                        self.inline_message_text(
                            &m.text,
                            emoji,
                            preview.is_none().then_some(&meta),
                        )
                    });
                    let l = wrap(&m.text, css);
                    l.set_selectable(true);
                    if let Some(preview) = preview {
                        // Keep the status row after the native card so time,
                        // encryption and delivery state follow the full bubble.
                        if let Some(inline) = inline.as_ref() {
                            bubble.append(inline);
                        } else {
                            bubble.append(&l);
                        }
                        bubble.append(&crate::link_preview_ui::link_preview_card(&preview));
                    } else if let Some(inline) = inline {
                        bubble.append(&inline);
                    } else {
                        bubble.append(&MessageText::new(&l, &meta));
                    }
                }
                _ => {}
            }
            if meta.parent().is_none() {
                meta.set_halign(gtk::Align::End);
                meta.set_margin_top(3);
                bubble.append(&meta);
            }
            if !m.reactions.is_empty() {
                let row = bx(false, 4);
                for reaction in &m.reactions {
                    let emoji = reaction
                        .rsplit_once(' ')
                        .map(|(e, _)| e)
                        .unwrap_or(reaction)
                        .to_string();
                    if m.poll.as_ref().is_some_and(|p| p.is_option(&emoji)) {
                        continue;
                    }
                    let b = gtk::ToggleButton::new();
                    let chip = bx(false, 3);
                    chip.append(&label(&emoji, "reaction-emoji"));
                    if let Some((_, count)) = reaction.rsplit_once(' ') {
                        chip.append(&label(count, "reaction-count"));
                    }
                    b.set_child(Some(&chip));
                    b.update_property(&[gtk::accessible::Property::Label(reaction)]);
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
            outer.append(&MessageBubbleLimit::new(&bubble, m.own));
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
            let entry = RenderedRow {
                message: m.clone(),
                row: row.clone(),
                bubble,
                status: meta,
                heading,
                avatar,
            };
            cluster_style(&entry, previous, next, group);
            rendered.rows.insert(m.id.clone(), entry);
            order.push(row);
        }
        rendered.dates.retain(|day, _| days.contains(day));
        let retained: HashSet<gtk::Widget> = order.iter().map(|row| row.clone().upcast()).collect();
        let mut child = self.timeline.first_child();
        while let Some(widget) = child {
            child = widget.next_sibling();
            if !retained.contains(&widget) {
                self.timeline.remove(&widget);
            }
        }
        for (index, row) in order.iter().enumerate() {
            if row.parent().is_none() {
                self.timeline.insert(row, index as i32);
            } else if row.index() != index as i32 {
                self.timeline.remove(row);
                self.timeline.insert(row, index as i32);
            }
        }
        drop(rendered);
        self.update_selection();
        self.queue_message_scroll(position);
    }
    fn capture_scroll_position(&self) -> ScrollTarget {
        let value = self.scroll.vadjustment().value();
        let mut row = self
            .timeline
            .row_at_y(value as i32)
            .map(|row| row.upcast::<gtk::Widget>());
        let mut anchor = None;
        while let Some(widget) = row {
            let id = widget.widget_name();
            if let Some(bubble) = self.bubbles.borrow().get(id.as_str()) {
                if let Some(bounds) = bubble.compute_bounds(&self.timeline) {
                    anchor = Some((id.to_string(), f64::from(bounds.y()) - value));
                    break;
                }
            }
            row = widget.next_sibling();
        }
        ScrollTarget::Position { value, anchor }
    }
    pub fn at_message_end(&self) -> bool {
        let adjustment = self.scroll.vadjustment();
        (self.state.borrow().account.is_empty() || self.history_window.newest())
            && adjustment.upper() - adjustment.page_size() - adjustment.value() < 24.
    }
    pub fn queue_message_scroll(self: &Rc<Self>, target: ScrollTarget) {
        let pending = self.message_scroll.borrow().is_some();
        // A receipt or an archive response may rebuild the rows in the same frame.
        if !pending || matches!(target, ScrollTarget::Bottom | ScrollTarget::Message(_)) {
            self.message_scroll.replace(Some(target));
        }
        if pending {
            return;
        }
        let weak = Rc::downgrade(self);
        let install = move |clock: &gdk::FrameClock| {
            let weak = weak.clone();
            let handler = Rc::new(Cell::new(None));
            let signal = handler.clone();
            handler.set(Some(clock.connect_local("layout", true, move |values| {
                let clock = values[0].get::<gdk::FrameClock>().unwrap();
                clock.disconnect(signal.take().unwrap());
                if let Some(ui) = weak.upgrade() {
                    if let Some(target) = ui.message_scroll.take() {
                        let adjustment = ui.scroll.vadjustment();
                        ui.history_window.moving.set(true);
                        adjustment.set_value(match target {
                            ScrollTarget::Bottom => adjustment.upper() - adjustment.page_size(),
                            ScrollTarget::Message(id) => ui
                                .highlight_position(id)
                                .unwrap_or_else(|| adjustment.value()),
                            ScrollTarget::Position { value, anchor } => {
                                let maximum = (adjustment.upper() - adjustment.page_size()).max(0.);
                                let continued_scroll = adjustment.value() - value.min(maximum);
                                anchor
                                    .and_then(|(id, delta)| {
                                        let bubbles = ui.bubbles.borrow();
                                        let bounds =
                                            bubbles.get(&id)?.compute_bounds(&ui.timeline)?;
                                        Some(f64::from(bounds.y()) - delta + continued_scroll)
                                    })
                                    .unwrap_or(adjustment.value())
                            }
                        });
                        ui.history_window.moving.set(false);
                    }
                    ui.schedule_history_maintenance();
                }
                None
            })));
            clock.request_phase(gdk::FrameClockPhase::LAYOUT);
        };
        if let Some(clock) = self.scroll.frame_clock() {
            install(&clock);
        } else {
            self.scroll.add_tick_callback(move |_, clock| {
                install(clock);
                glib::ControlFlow::Break
            });
        }
    }
    fn highlight_position(self: &Rc<Self>, id: String) -> Option<f64> {
        let bubble = self.bubbles.borrow().get(&id)?.clone();
        let bounds = bubble.compute_bounds(&self.timeline)?;
        let marker = (id, std::time::Instant::now());
        if let Some((previous, _)) = self
            .history_window
            .highlighted
            .replace(Some(marker.clone()))
        {
            if let Some(previous) = self.bubbles.borrow().get(&previous) {
                previous.remove_css_class("reply-target");
            }
        }
        bubble.add_css_class("reply-target");
        let weak = Rc::downgrade(self);
        glib::timeout_add_local_once(Duration::from_millis(1200), move || {
            let Some(ui) = weak.upgrade() else { return };
            if ui.history_window.highlighted.borrow().as_ref() == Some(&marker) {
                ui.history_window.highlighted.borrow_mut().take();
                if let Some(bubble) = ui.bubbles.borrow().get(&marker.0) {
                    bubble.remove_css_class("reply-target");
                }
            }
        });
        Some((f64::from(bounds.y()) - 16.0).max(0.0))
    }
    pub fn highlight(self: &Rc<Self>, id: &str) {
        if self.bubbles.borrow().contains_key(id) && !self.history_window.busy() {
            self.queue_message_scroll(ScrollTarget::Message(id.into()));
        } else if !self.state.borrow().account.is_empty() {
            self.load_timeline(Some(id.into()));
        }
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
            } else if let Some(anchor) = anchor {
                drop(selected);
                self.select_history_span(anchor, id.into());
                return;
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
        if action == "select-all" {
            self.selected.replace(
                self.state
                    .borrow()
                    .current()
                    .messages
                    .iter()
                    .map(|m| m.id.clone())
                    .collect(),
            );
            self.update_selection();
            return;
        }
        if action == "delete" {
            self.delete_confirmation(self.selected.borrow().iter().cloned().collect());
            return;
        }
        if matches!(action, "copy" | "forward") {
            self.resolve_selected_messages(action == "forward");
        }
    }
    pub(crate) fn apply_selection(self: &Rc<Self>, messages: Vec<Message>, forward: bool) {
        if forward {
            self.forward(messages);
        } else {
            self.window.clipboard().set_text(
                &messages
                    .iter()
                    .map(|m| format!("{} · {}\n{}", m.author, m.time, m.content_text()))
                    .collect::<Vec<_>>()
                    .join("\n\n"),
            );
        }
    }
    pub fn delete_confirmation(self: &Rc<Self>, ids: Vec<String>) {
        let owner = self.state.borrow().account.clone();
        let chat = self.state.borrow().active.clone();
        let peer = self.state.borrow().current().jid.clone();
        let Ok(ids) = self.store.deletion_targets(&owner, &peer, &ids) else {
            self.notice("Message deletion could not be prepared");
            return;
        };
        let d=adw::AlertDialog::builder().heading(format!("Delete {} messages?",ids.len())).body("This removes the messages locally. Server history and other devices are unaffected.").build();
        d.add_responses(&[("cancel", "Cancel"), ("delete", "Delete Locally")]);
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
        let Ok(ids) = self.store.deletion_targets(&s.account, &peer, ids) else {
            drop(s);
            self.notice("Message deletion could not be prepared");
            return;
        };
        if !s.account.is_empty() {
            for id in &ids {
                let message = self.store.message(&s.account, &peer, id);
                if self.store.delete(&s.account, &peer, id).is_err() {
                    drop(s);
                    self.notice("Local deletion failed");
                    return;
                }
                self.uploads.borrow_mut().cancel(id);
                if message.as_ref().is_some_and(|m| m.upload.is_some()) {
                    let (account, id) = (s.account.clone(), id.clone());
                    gtk::gio::spawn_blocking(move || {
                        let _guard = crate::downloads::CACHE_IO.lock().unwrap();
                        if let Ok(path) = crate::uploads::snapshot_path(&account, &id) {
                            let _ = std::fs::remove_file(path);
                        }
                    });
                }
                if let Some(file) = message.as_ref().and_then(|m| m.attachment.as_ref()) {
                    self.downloads.borrow_mut().cancel(&s.account, file);
                    self.images.borrow_mut().invalidate(file);
                }
            }
        }
        s.current_mut().messages.retain(|m| {
            !ids.contains(&m.id)
                && !(m.own
                    && m.mode() == crate::encryption_policy::Mode::Legacy
                    && m.delivery == "waiting-upload"
                    && ids.contains(&m.upload_caption_for))
        });
        let owner = s.account.clone();
        drop(s);
        if !owner.is_empty() {
            self.history_window.content_changed();
            self.sync_latest_preview(&owner, &peer);
        }
        self.clear_selection();
        self.persist();
        self.render_messages(false);
        self.refresh_chats(None);
    }
    pub fn react(self: &Rc<Self>, id: &str, emoji: &str) {
        if self.public_reaction_consent_required(id) {
            self.confirm_public_reaction(id, emoji);
            return;
        }
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
        let reactions = (!message.unavailable && !message.retracted)
            .then(|| self.message_reaction_picker(&pop, &message));
        if let Some(reactions) = &reactions {
            content.append(&reactions.root)
        }
        let action_menu = bx(true, 8);
        if let Some(reactions) = &reactions {
            reactions.attach_actions(&action_menu);
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
        if message.own && message.poll.as_ref().is_some_and(|p| !p.closed) {
            choices.push(("process-stop-symbolic", "Close Poll", "close-poll"));
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
        action_menu.append(&items);
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
        action_menu.append(&tools);
        content.append(&action_menu);
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
                    "copy" => ui.window.clipboard().set_text(&m.content_text()),
                    "delete" => ui.delete_confirmation(vec![id.clone()]),
                    "forward" => ui.forward(vec![m]),
                    "edit" => ui.edit_message(&id),
                    "close-poll" => ui.close_poll(&id),
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
