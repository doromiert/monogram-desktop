use crate::{model::*, ui::Ui, widgets::*};
#[path = "reactions_ui.rs"]
pub(crate) mod reactions_ui;
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
    serial: u64,
    rendering: bool,
}
struct PreparedMessages {
    messages: Vec<Message>,
    days: Vec<String>,
    links: Vec<Vec<crate::link_previews::ClickableUrl>>,
    emoji_only: Vec<bool>,
    formatting:Vec<(crate::formatting::Display,crate::markdown::RichText,Option<crate::markdown::Document>)>,
    scope: (String, String),
    group: bool,
    serial: u64,
    generation: u64,
}
struct RenderedRow {
    message: Message,
    row: gtk::ListBoxRow,
    bubble: gtk::Box,
    status: gtk::Box,
    heading: gtk::Box,
    avatar: Option<adw::Avatar>,
    reactions: reactions_ui::MessageReactions,
}
fn message_focus(root: &gtk::Widget, emoji: Option<&str>) -> Option<gtk::Widget> {
    if let Some(emoji) = emoji {
        if root.has_css_class("reaction")
            && root
                .first_child()
                .and_then(|child| child.first_child())
                .and_downcast::<gtk::Label>()
                .is_some_and(|label| label.text() == emoji)
        {
            return Some(root.clone());
        }
    } else if root.has_css_class("message-text") && root.is_focusable() {
        return Some(root.clone());
    }
    let mut child = root.first_child();
    while let Some(widget) = child {
        child = widget.next_sibling();
        if let Some(target) = message_focus(&widget, emoji) {
            return Some(target);
        }
    }
    None
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
        && a.formatting == b.formatting
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
    if action_body(a).is_some()
        || action_body(b).is_some()
        || a.own != b.own
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
    if row.bubble.has_css_class("action-pill") {
        row.heading.set_visible(false);
        return;
    }
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
// Action commands change presentation only; original bodies remain copyable and encrypted as sent.
fn action_body(message: &Message) -> Option<&str> {
    if message.kind != "text"
        || message.unavailable
        || message.attachment.is_some()
        || message.upload.is_some()
        || message.forwarded.is_some()
    {
        return None;
    }
    ["/me ", "/you ", "/nar "]
        .iter()
        .find_map(|prefix| message.text.strip_prefix(prefix))
}

fn action_presentation(message: &Message, chat: &Chat, owner: &str) -> Option<String> {
    let body = action_body(message)?;
    Some(if message.text.starts_with("/nar ") {
        body.to_owned()
    } else if message.text.starts_with("/you ") {
        format!("* you {body}")
    } else {
        format!("* {} {body}", action_actor(message, chat, owner))
    })
}

fn action_actor(message: &Message, chat: &Chat, owner: &str) -> String {
    let actor = if message.own && chat.group {
        chat.room_nick.as_deref().unwrap_or(&message.author)
    } else if message.own {
        owner
            .split_once('@')
            .map(|(name, _)| name)
            .unwrap_or(&message.author)
    } else if chat.group {
        &message.author
    } else {
        &chat.name
    };
    actor.replace(['\n', '\r'], " ")
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
                let title = if read {
                    "Read"
                } else if status == "delivered" {
                    "Delivered"
                } else {
                    "Sent"
                };
                if read {
                    let checks = gtk::Overlay::new();
                    checks.add_css_class("message-status-check");
                    checks.add_css_class("read");
                    let first = gtk::Image::from_icon_name("object-select-symbolic");
                    first.set_pixel_size(12);
                    first.set_halign(gtk::Align::Start);
                    checks.set_child(Some(&first));
                    let second = gtk::Image::from_icon_name("object-select-symbolic");
                    second.set_pixel_size(12);
                    second.set_halign(gtk::Align::Start);
                    second.set_margin_start(8);
                    checks.add_overlay(&second);
                    checks.set_tooltip_text(Some(title));
                    checks.update_property(&[gtk::accessible::Property::Label(title)]);
                    meta.append(&checks);
                } else {
                    let check = gtk::Image::from_icon_name("object-select-symbolic");
                    check.set_pixel_size(12);
                    check.add_css_class("message-status-check");
                    check.add_css_class("sent");
                    check.set_tooltip_text(Some(title));
                    check.update_property(&[gtk::accessible::Property::Label(title)]);
                    meta.append(&check);
                }
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
#[derive(Clone)]
pub enum ScrollTarget {
    Bottom,
    Message(String),
    Position {
        value: f64,
        anchor: Option<(String, f64)>,
    },
}
impl Ui {
    pub(crate) fn rendering_messages(&self) -> bool {
        self.rendered_rows.borrow().rendering
    }
    fn update_cached_row(
        self: &Rc<Self>,
        row: &mut RenderedRow,
        message: &Message,
        previous: bool,
        next: bool,
        group: bool,
    ) {
        if row.message.delivery != message.delivery
            || row.message.time != message.time
            || row.message.edited != message.edited
            || row.message.encrypted != message.encrypted
            || row.message.encryption_mode != message.encryption_mode
        {
            message_status(&row.status, message);
        }
        if row.message.reactions != message.reactions
            || row.message.my_reactions != message.my_reactions
        {
            row.reactions.update(self, message);
        }
        row.message = message.clone();
        cluster_style(row, previous, next, group);
        if self
            .history_window
            .highlighted
            .borrow()
            .as_ref()
            .is_some_and(|(id, _)| id == &message.id)
        {
            row.bubble.add_css_class("reply-target");
        } else {
            row.bubble.remove_css_class("reply-target");
        }
    }
    // Reaction/count/status changes reuse the allocated body and never start a history render.
    fn update_cached_rows(self: &Rc<Self>) -> bool {
        let (messages, group) = {
            let state = self.state.borrow();
            let chat = state.current();
            let rows = self.rendered_rows.borrow();
            if rows.rendering
                || rows.render_request.is_some()
                || rows.rows.is_empty()
                || rows.scope != (state.account.clone(), chat.jid.clone())
                || rows.group != chat.group
                || rows.big_emoji != self.big_emoji.get()
            {
                return false;
            }
            (
                crate::media_caption::display_messages(&chat.messages)
                    .into_iter()
                    .filter(|message| !message.retracted)
                    .collect::<Vec<_>>(),
                chat.group,
            )
        };
        let mut rendered = self.rendered_rows.borrow_mut();
        let mut previous_index = -1;
        if messages.len() != rendered.rows.len()
            || messages.iter().any(|message| {
                let Some(row) = rendered.rows.get(&message.id) else {
                    return true;
                };
                let index = row.row.index();
                let changed = index <= previous_index
                    || !same_content(&row.message, message)
                    || row.message.timestamp != message.timestamp
                    || row.message.action != message.action
                    || row.message.poll_source != message.poll_source
                    || row.message.poll_fallback != message.poll_fallback;
                previous_index = index;
                changed
            })
        {
            return false;
        }
        let position = self.capture_scroll_position();
        for (index, message) in messages.iter().enumerate() {
            let previous = index > 0 && clustered(&messages[index - 1], message, group);
            let next = messages
                .get(index + 1)
                .is_some_and(|next| clustered(message, next, group));
            self.update_cached_row(
                rendered.rows.get_mut(&message.id).unwrap(),
                message,
                previous,
                next,
                group,
            );
        }
        drop(rendered);
        self.update_selection();
        self.queue_message_scroll(position);
        true
    }
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
        if !bottom && self.update_cached_rows() {
            return;
        }
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
        rows.serial = rows.serial.wrapping_add(1);
        rows.rendering = true;
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
        let serial = {
            let mut rendered = self.rendered_rows.borrow_mut();
            rendered.render_request.take();
            rendered.serial = rendered.serial.wrapping_add(1);
            rendered.rendering = true;
            rendered.serial
        };
        self.update_history_spinner();
        let state = self.state.borrow();
        let messages = state.current().messages.clone();
        let group = state.current().group;
        let scope = (state.account.clone(), state.current().jid.clone());
        drop(state);
        let generation = self.generation.get();
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let planned = gtk::gio::spawn_blocking(move || {
                let messages: Vec<_> = crate::media_caption::display_messages(&messages)
                    .into_iter()
                    .filter(|message| !message.retracted)
                    .collect();
                let days = messages.iter().map(local_day).collect();
                let emoji_only = messages
                    .iter()
                    .map(|message| {
                        message.kind == "text"
                            && !message.unavailable
                            && message.attachment.is_none()
                            && crate::emoji_completion::only_emojis(&message.text)
                    })
                    .collect();
                let formatting:Vec<_>=messages.iter().map(|m|{let(d,r)=crate::formatting_ui::display(m);(d,r,crate::formatting_ui::article(m))}).collect();
                let links = messages
                    .iter()
                    .map(|message| {
                        crate::link_previews::clickable_urls(
                            &message.text,
                            message.preview_spoilers.as_ref(),
                        )
                    })
                    .collect();
                PreparedMessages {
                    messages,
                    days,
                    links,
                    emoji_only,
                    formatting,
                    scope,
                    group,
                    serial,
                    generation,
                }
            })
            .await;
            if let Some(ui) = weak.upgrade() {
                if let Ok(planned) = planned {
                    ui.apply_message_batch(Rc::new(planned), position);
                } else if ui.rendered_rows.borrow().serial == serial {
                    ui.rendered_rows.borrow_mut().rendering = false;
                    ui.update_history_spinner();
                    ui.notice("Messages could not be prepared");
                }
            }
        });
    }
    fn apply_message_batch(self: &Rc<Self>, planned: Rc<PreparedMessages>, position: ScrollTarget) {
        if self.generation.get() != planned.generation
            || self.rendered_rows.borrow().serial != planned.serial
        {
            return;
        }
        let state = self.state.borrow();
        if state.account != planned.scope.0 || state.current().jid != planned.scope.1 {
            return;
        }
        drop(state);
        let started = std::time::Instant::now();
        let messages = &planned.messages;
        let group = planned.group;
        let scope = planned.scope.clone();
        self.bubbles.borrow_mut().clear();
        let mut rendered = self.rendered_rows.borrow_mut();
        let big_emoji = self.big_emoji.get();
        if rendered.scope != scope || rendered.big_emoji != big_emoji || rendered.group != group {
            *rendered = RenderedRows {
                scope,
                big_emoji,
                group,
                serial: planned.serial,
                rendering: true,
                ..Default::default()
            };
        }
        let ids: HashSet<_> = messages.iter().map(|message| message.id.as_str()).collect();
        rendered.rows.retain(|id, _| ids.contains(id.as_str()));
        let mut order = Vec::new();
        let mut days = HashSet::new();
        let mut last = String::new();
        let mut created = 0;
        let mut pending = false;
        for (index, m) in messages.iter().enumerate() {
            let day = planned.days[index].clone();
            if day != last {
                let row = rendered.dates.entry(day.clone()).or_insert_with(|| {
                    let pill = label(&day, "date-pill");
                    pill.set_halign(gtk::Align::Center);
                    pill.set_hexpand(false);
                    gtk::ListBoxRow::builder()
                        .selectable(false)
                        .activatable(false)
                        .child(&pill)
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
                self.update_cached_row(row, m, previous, next, group);
                self.bubbles
                    .borrow_mut()
                    .insert(m.id.clone(), row.bubble.clone().upcast());
                order.push(row.row.clone());
                continue;
            }
            if created >= 2 || created > 0 && started.elapsed() >= Duration::from_millis(4) {
                pending = true;
                if let Some(row) = rendered.rows.get(&m.id) {
                    order.push(row.row.clone());
                    self.bubbles
                        .borrow_mut()
                        .insert(m.id.clone(), row.bubble.clone().upcast());
                }
                continue;
            }
            created += 1;
            let row_started = std::time::Instant::now();
            let displaced = rendered.rows.remove(&m.id);
            let focused = gtk::prelude::GtkWindowExt::focus(&self.window).filter(|focus| {
                displaced
                    .as_ref()
                    .is_some_and(|old| focus == &old.row || focus.is_ancestor(&old.row))
            });
            let focused_emoji = focused
                .as_ref()
                .filter(|focus| focus.has_css_class("reaction"))
                .and_then(|focus| focus.first_child())
                .and_then(|child| child.first_child())
                .and_downcast::<gtk::Label>()
                .map(|label| label.text().to_string());
            if focused.is_some() {
                // Detaching a focused row otherwise focuses the date separator and scrolls to it.
                gtk::prelude::GtkWindowExt::set_focus(&self.window, None::<&gtk::Widget>);
            }
            let action = {
                let state = self.state.borrow();
                action_presentation(m, state.current(), &state.account)
            };
            let outer = bx(false, 8);
            outer.set_valign(gtk::Align::Start);
            outer.set_vexpand(false);
            outer.set_halign(gtk::Align::Fill);
            outer.set_hexpand(true);
            let mut avatar = None;
            if group && !m.own && action.is_none() {
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
            if action.is_some() {
                bubble.add_css_class("date-pill");
                bubble.add_css_class("action-pill");
                bubble.set_halign(gtk::Align::Center);
            } else {
                bubble.add_css_class("message-bubble");
            }
            if self
                .history_window
                .highlighted
                .borrow()
                .as_ref()
                .is_some_and(|(id, _)| id == &m.id)
            {
                bubble.add_css_class("reply-target");
            }
            if planned.emoji_only[index] {
                bubble.add_css_class("emoji-only");
            }
            if m.own && action.is_none() {
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
                let preview_row = bx(false, 7);
                crate::reply_preview_ui::attach(self, m, &preview_row);
                preview_row.append(&contents);
                reply.set_child(Some(&preview_row));
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
                _ if action.is_some() => {
                    let presented=action.as_ref().unwrap();
                    let body=action_body(m).unwrap();
                    let original=crate::formatting_ui::model(m);
                    let rich=original.slice(m.text.chars().count()-body.chars().count(),m.text.chars().count()).and_then(|content|content.in_body(presented,presented.chars().count()-body.chars().count())).map(|formatting|crate::formatting_ui::display(&Message{text:presented.clone(),formatting:Some(formatting),..Default::default()}).1).unwrap_or_else(||crate::markdown::RichText::plain(presented));
                    let label = wrap(&rich.text, "message-text");
                    crate::formatting_ui::FormattedLabel::new(&label,rich);
                    label.set_justify(gtk::Justification::Center);
                    label.set_selectable(true);
                    bubble.append(&label);
                    bubble.set_tooltip_text(Some(&format!("{} · {}", m.time, m.delivery)));
                }
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
                    let (attachment,spoiler_button) = crate::formatting_ui::hidden_media(&self.attachment_content(&m),crate::formatting_ui::model(&m).spoiler);
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
                        let body = wrap(&m.text, "message-text");
                        if media {
                            // Let the media set the bubble width; wrap to its allocation.
                            body.set_max_width_chars(1);
                        }
                        let caption = MessageText::with_rich(&body, &meta,planned.formatting[index].1.clone());
                        if let (Some(button),Some(state))=(spoiler_button.as_ref(),caption.formatted_state()){
                            let weak=Rc::downgrade(&state);button.connect_clicked(move|_|{if let Some(state)=weak.upgrade(){state.reveal_all();}});
                            let button=button.downgrade();state.on_reveal(move||{if let Some(button)=button.upgrade(){if button.is_visible(){button.emit_clicked();}}});
                        }
                        caption.add_css_class("media-caption");
                        if media {
                            caption.set_orientation(gtk::Orientation::Vertical);
                        }
                        caption.set_margin_start(if media { 11 } else { 9 });
                        caption.set_margin_end(if media { 11 } else { 9 });
                        caption.set_margin_top(if media { 8 } else { 5 });
                        caption.set_margin_bottom(if media { 5 } else { 4 });
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
                        && planned.emoji_only[index]
                        && m.text.chars().count() < 5
                    {
                        "large-emoji"
                    } else {
                        "message-text"
                    };
                    let (display,rich,article)=&planned.formatting[index];
                    if let Some(document)=article {
                        bubble.append(&crate::formatting_ui::article_widget(&document,&meta));
                    } else {
                        let preview=self.link_preview_for_message(&m);
                        let links=crate::link_previews::clickable_urls(&display.body,None);
                        let inline=m.inline_emoji.as_ref().and_then(|emoji|emoji.without(&display.removed).ok()).filter(|emoji|emoji.body==display.body).map(|emoji|self.inline_message_text_formatted(&display.body,&emoji,preview.is_none().then_some(&meta),&links,Some(&rich)));
                        let l=wrap(&display.body,css);
                        if let Some(preview)=preview {
                            if let Some(inline)=inline.as_ref(){bubble.append(inline);}else{crate::formatting_ui::FormattedLabel::new(&l,rich.clone());bubble.append(&l);}
                            bubble.append(&crate::link_preview_ui::link_preview_card(&preview));
                        }else if let Some(inline)=inline{bubble.append(&inline);}else{bubble.append(&MessageText::with_rich(&l,&meta,rich.clone()));}
                    }
                }
                _ => {}
            }
            if meta.parent().is_none() && action.is_none() {
                meta.set_halign(gtk::Align::End);
                meta.set_margin_top(3);
                bubble.append(&meta);
            }
            let reactions = reactions_ui::MessageReactions::new(self, &m, &bubble, media, &meta);
            let presentation=self.send_animation.wrap(&m.id,&bubble);
            if action.is_some() {
                outer.set_halign(gtk::Align::Center);
                outer.set_hexpand(false);
                outer.append(&presentation);
            } else {
                outer.append(&MessageBubbleLimit::new(&presentation, m.own));
            }
            self.bubbles
                .borrow_mut()
                .insert(m.id.clone(), bubble.clone().upcast());
            let reused = displaced.is_some();
            let row = displaced.map(|old| old.row).unwrap_or_else(|| {
                gtk::ListBoxRow::builder()
                    .activatable(false)
                    .valign(gtk::Align::Start)
                    .vexpand(false)
                    .build()
            });
            row.set_child(Some(&outer));
            row.add_css_class("message-row");
            row.set_widget_name(&m.id);
            if self.selected.borrow().contains(&m.id) {
                row.add_css_class("selected-message")
            }
            if !reused {
                let click = gtk::GestureClick::new();
                click.set_button(1);
                click.set_propagation_phase(gtk::PropagationPhase::Capture);
                let id = m.id.clone();
                let weak = Rc::downgrade(self);
                click.connect_pressed(move |g, _, _, _| {
                    if let Some(ui) = weak.upgrade() {
                        let flags = g.current_event_state();
                        if flags.intersects(
                            gdk::ModifierType::CONTROL_MASK | gdk::ModifierType::SHIFT_MASK,
                        ) || !ui.selected.borrow().is_empty()
                        {
                            g.set_state(gtk::EventSequenceState::Claimed);
                            ui.select_message(&id, flags.contains(gdk::ModifierType::SHIFT_MASK));
                        }
                    }
                });
                row.add_controller(click);
            }
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
                reactions,
            };
            cluster_style(&entry, previous, next, group);
            if focused.is_some() {
                message_focus(entry.bubble.upcast_ref(), focused_emoji.as_deref())
                    .or_else(|| message_focus(entry.bubble.upcast_ref(), None))
                    .unwrap_or_else(|| row.clone().upcast())
                    .grab_focus();
            }
            rendered.rows.insert(m.id.clone(), entry);
            order.push(row);
            if row_started.elapsed() >= Duration::from_millis(50) {
                eprintln!(
                    "Monogram slow message widget: {} ms, kind={}, text_bytes={}",
                    row_started.elapsed().as_millis(),
                    m.kind,
                    m.text.len()
                );
            }
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
        rendered.rendering = pending;
        drop(rendered);
        self.update_history_spinner();
        self.update_selection();
        self.queue_message_scroll(position.clone());
        if started.elapsed() >= Duration::from_millis(50) {
            eprintln!(
                "Monogram slow message batch: {} ms, created={created}",
                started.elapsed().as_millis()
            );
        }
        if pending {
            let weak = Rc::downgrade(self);
            self.scroll.add_tick_callback(move |_, _| {
                if let Some(ui) = weak.upgrade() {
                    let position = match &position {
                        ScrollTarget::Position { .. } => ui.capture_scroll_position(),
                        _ => position.clone(),
                    };
                    ui.apply_message_batch(planned.clone(), position);
                }
                glib::ControlFlow::Break
            });
        } else {
            self.schedule_history_maintenance();
        }
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
                        ui.cancel_reply_scroll();
                        ui.history_window.moving.set(true);
                        if let ScrollTarget::Message(id)=target {
                            if let Some(value)=ui.highlight_position(id){ui.animate_reply_scroll(value);}
                            return None;
                        }
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
        let adjustment=self.scroll.vadjustment();
        Some(crate::navigation_ui::destination(f64::from(bounds.y()),f64::from(bounds.height()),adjustment.value(),adjustment.page_size()))
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
        if matches!(action, "select-day" | "select-all") {
            self.select_message_day();
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
        self.notice("Preparing message deletion…");
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            let prepared = match ui.prepare_deletion(ids).await {
                Ok(prepared) => prepared,
                Err(message) => {
                    ui.notice(message);
                    return;
                }
            };
            if ui.state.borrow().account != owner || ui.state.borrow().active != chat {
                return;
            }
            if prepared.local_count == 0 {
                ui.notice("No messages are available to delete");
                return;
            }

            let request = gtk::CheckButton::builder()
                .active(false)
                .valign(gtk::Align::Center)
                .build();
            let d = adw::AlertDialog::builder()
                .heading(format!(
                    "Delete {} {}?",
                    prepared.local_count,
                    if prepared.local_count == 1 {
                        "message"
                    } else {
                        "messages"
                    }
                ))
                .body("Delete the selected messages from this device?")
                .build();
            if prepared.own_count > 0 {
                let subtitle = match (prepared.eligible_own_count, prepared.ineligible_own_count) {
                    (0, _) => "These messages have not been sent or do not support deletion requests.",
                    (_, 0) => "",
                    _ => "Unsent or unsupported messages will only be deleted from this device.",
                };
                let mut row = adw::ActionRow::builder()
                    .title("Also request deletion of my messages")
                    .activatable_widget(&request);
                if !subtitle.is_empty() {
                    row = row.subtitle(subtitle);
                }
                let row = row.build();
                row.add_suffix(&request);
                row.set_sensitive(prepared.eligible_own_count > 0);
                let group = adw::PreferencesGroup::new();
                group.add(&row);
                d.set_extra_child(Some(&group));
            }
            d.add_responses(&[("cancel", "Cancel"), ("delete", "Delete")]);
            d.set_default_response(Some("cancel"));
            d.set_close_response("cancel");
            d.set_response_appearance("delete", adw::ResponseAppearance::Destructive);
            let prepared = Rc::new(std::cell::RefCell::new(Some(prepared)));
            let weak = Rc::downgrade(&ui);
            d.connect_response(None, move |_, response| {
                if response != "delete" {
                    return;
                }
                if let Some(ui) = weak.upgrade() {
                    if ui.state.borrow().account != owner || ui.state.borrow().active != chat {
                        return;
                    }
                    if let Some(prepared) = prepared.borrow_mut().take() {
                        ui.apply_deletion(prepared, request.is_active());
                    }
                }
            });
            d.present(Some(&ui.window));
        });
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
        // Restoring focus from the search field must not select the message.
        let mut selections = Vec::new();
        let mut pending = vec![parent.clone().upcast::<gtk::Widget>()];
        while let Some(widget) = pending.pop() {
            if let Some(label) = widget
                .downcast_ref::<gtk::Label>()
                .filter(|label| label.is_selectable())
            {
                selections.push((label.downgrade(), label.selection_bounds().unwrap_or((0, 0))));
            }
            let mut child = widget.first_child();
            while let Some(widget) = child {
                child = widget.next_sibling();
                pending.push(widget);
            }
        }
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
        if reactions.is_none() {
            content.append(&action_menu);
        }
        pop.set_child(Some(&content));
        pop.connect_closed(move |p| {
            if p.parent().is_some() {
                p.unparent()
            }
            let selections = selections.clone();
            // GTK restores focus after closed; restore the range before painting.
            glib::idle_add_local_full(glib::Priority::HIGH_IDLE, move || {
                for (label, (start, end)) in &selections {
                    if let Some(label) = label.upgrade() {
                        label.select_region(*start, *end);
                    }
                }
                glib::ControlFlow::Break
            });
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
                    "copy" if m.attachment.is_some() => ui.copy_attachment(&id),
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
        let edit_spoiler=gtk::gio::SimpleAction::new_stateful("spoiler-all",None,&false.to_variant());
        crate::formatting_ui::install_editor(self,&editor,&edit_spoiler);
        let original=crate::formatting_ui::model(&m);
        let projected=original.display();
        let initial=if draft==m.text && !original.markdown {crate::formatting::Markup{body:projected.body,ranges:projected.ranges,unstyled:true,spoiler:original.spoiler,..Default::default()}}else{crate::formatting::Markup{body:draft.clone(),markdown:original.markdown,spoiler:original.spoiler,..Default::default()}};
        editor.buffer().set_text(&initial.body);
        crate::formatting_ui::restore_editor_formatting(&editor,&edit_spoiler,Some(&initial));
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
                    let mut formatting=crate::formatting_ui::editor_formatting(&editor,&edit_spoiler);
                    formatting.markdown=original.markdown;
                    if formatting.markdown{formatting.unstyled=true;}
                    ui.send_formatted_action(&id,crate::message_actions::Action::Correct { target },formatting.styled());
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
                    let formatting=crate::formatting_ui::editor_formatting(&editor,&edit_spoiler).styled();
                    m.text = formatting.body.clone();m.preview_spoilers=Some(formatting.spoiler_context());m.formatting=Some(formatting);
                }
                ui.render_messages(false);
                ui.refresh_chats(None);
            }
        });
        actions.append(&save);
        bubble.append(&actions);
    }
}

#[cfg(test)]
mod action_command_tests {
    use super::*;

    #[test]
    fn action_commands_preserve_body_and_match_exact_prefixes() {
        let chat = Chat {
            name: "Alex".into(),
            ..Default::default()
        };
        for own in [false, true] {
            for (body, expected) in [
                (
                    "/me waves",
                    if own { "* Sam waves" } else { "* Alex waves" },
                ),
                ("/you waves 👋", "* you waves 👋"),
                (
                    "/nar The room falls silent.\nThen a bell rings.",
                    "The room falls silent.\nThen a bell rings.",
                ),
            ] {
                let mut message = Message {
                    text: body.into(),
                    own,
                    ..Default::default()
                };
                assert_eq!(
                    action_presentation(&message, &chat, "Sam@example.invalid").as_deref(),
                    Some(expected)
                );
                assert_eq!(message.text, body);
                message.unavailable = true;
                assert!(action_presentation(&message, &chat, "Sam").is_none());
            }
        }
        for body in [
            "/you",
            "/nar",
            "/YOU waves",
            " /nar text",
            "/narrator text",
            "/you\twaves",
            "hello /nar text",
        ] {
            assert!(
                action_body(&Message {
                    text: body.into(),
                    ..Default::default()
                })
                .is_none()
            );
        }
    }
}

#[cfg(test)]
#[path = "../tests/reaction_scroll_native.rs"]
mod reaction_scroll_native;

#[cfg(test)]
#[path = "../tests/reaction_media_native.rs"]
mod reaction_media_native;
