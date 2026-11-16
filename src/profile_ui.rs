//! A compact native profile card shared by chat avatars and participant lists.
use crate::{model::Chat, protocol::Command, ui::Ui, widgets::*};
use adw::prelude::*;
use gtk::glib;
use std::{
    rc::Rc,
    time::{Duration, Instant},
};
impl Ui {
    pub(crate) fn profile_button(
        self: &Rc<Self>,
        child: &impl IsA<gtk::Widget>,
        peer: &str,
        name: &str,
    ) -> gtk::Button {
        let button = gtk::Button::builder()
            .child(child)
            .tooltip_text(format!("View {name}'s profile"))
            .css_classes(["flat", "profile-target"])
            .build();
        button.update_property(&[gtk::accessible::Property::Label(&format!(
            "View {name}'s profile"
        ))]);
        let ui = Rc::downgrade(self);
        let peer = peer.to_string();
        let name = name.to_string();
        let owner = Rc::downgrade(self);
        button.connect_unmap(move |button| {
            if let Some(ui) = owner.upgrade() {
                if let Some(popup) = ui
                    .active_profile
                    .borrow()
                    .as_ref()
                    .filter(|p| p.parent().as_ref() == Some(button.upcast_ref()))
                {
                    popup.popdown();
                    if popup.parent().is_some() {
                        popup.unparent();
                    }
                }
            }
        });
        button.connect_clicked(move |button| {
            if let Some(ui) = ui.upgrade() {
                ui.profile_card(button, &peer, &name);
            }
        });
        button
    }
    pub(crate) fn profile_card(
        self: &Rc<Self>,
        anchor: &impl IsA<gtk::Widget>,
        peer: &str,
        name: &str,
    ) {
        if peer.is_empty() {
            return;
        }
        if let Some(previous) = self.active_profile.borrow_mut().take() {
            previous.popdown();
            if previous.parent().is_some() {
                previous.unparent();
            }
        }
        let chat = self
            .state
            .borrow()
            .chats
            .iter()
            .find(|c| c.jid == peer)
            .cloned()
            .unwrap_or_else(|| Chat {
                name: name.into(),
                jid: peer.into(),
                group: peer.contains('/'),
                color: "purple".into(),
                ..Default::default()
            });
        let popup = gtk::Popover::builder()
            .has_arrow(false)
            .autohide(true)
            .build();
        popup.add_css_class("profile-card");
        popup.set_parent(anchor);
        let body = bx(true, 12);
        body.add_css_class("profile-card-panel");
        body.set_width_request(300);
        let banner = gtk::Overlay::new();
        banner.set_height_request(108);
        let color = bx(false, 0);
        color.add_css_class("profile-card-banner");
        color.set_margin_bottom(32);
        banner.set_child(Some(&color));
        let avatar = self.chat_avatar(&chat, 72);
        avatar.set_halign(gtk::Align::Start);
        avatar.set_valign(gtk::Align::End);
        avatar.set_margin_start(12);
        banner.add_overlay(&avatar);
        body.append(&banner);
        let content = bx(true, 10);
        content.set_margin_start(12);
        content.set_margin_end(12);
        content.set_margin_bottom(12);
        let title = wrap(
            &crate::text_safety::visible(chat.display_name(), 128),
            "title-2",
        );
        title.set_xalign(0.);
        title.set_max_width_chars(30);
        content.append(&title);
        let address = wrap(peer, "caption dim-label");
        address.set_selectable(true);
        address.set_xalign(0.);
        address.set_max_width_chars(35);
        content.append(&address);
        let presence = if chat.group {
            "Room participant"
        } else if !chat.subtitle.is_empty() {
            &chat.subtitle
        } else {
            "Presence unavailable"
        };
        let status = wrap(presence, "caption");
        status.set_xalign(0.);
        content.append(&status);
        let fields = bx(true, 10);
        content.append(&fields);
        let feedback = wrap(
            if self.online.get() {
                "Loading profile…"
            } else {
                "Connect to load profile details."
            },
            "caption dim-label",
        );
        feedback.set_xalign(0.);
        content.append(&feedback);
        let mut preferred_focus = None;
        if !peer.contains('/') && peer != self.state.borrow().account {
            let message = gtk::Button::with_label("Message");
            message.add_css_class("suggested-action");
            preferred_focus = Some(message.clone());
            let ui = Rc::downgrade(self);
            let peer = peer.to_string();
            let pop = popup.downgrade();
            message.connect_clicked(move |_| {
                if let Some(ui) = ui.upgrade() {
                    let id = ui.state.borrow_mut().chat_for(&peer).id.clone();
                    if let Some(pop) = pop.upgrade() {
                        pop.popdown();
                    }
                    ui.select_chat(&id);
                }
            });
            content.append(&message);
            let edit = gtk::Button::with_label("Edit Local Details");
            let ui = Rc::downgrade(self);
            let peer = chat.jid.clone();
            let pop = popup.downgrade();
            edit.connect_clicked(move |_| {
                if let Some(ui) = ui.upgrade() {
                    if let Some(pop) = pop.upgrade() {
                        pop.popdown();
                    }
                    ui.edit_contact_details(&peer);
                }
            });
            content.append(&edit);
        }
        let scroll = gtk::ScrolledWindow::builder()
            .child(&content)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .propagate_natural_height(true)
            .max_content_height(420)
            .build();
        body.append(&scroll);
        popup.set_child(Some(&body));
        let weak = popup.downgrade();
        popup.connect_closed(move |_| {
            if let Some(p) = weak.upgrade() {
                if p.parent().is_some() {
                    p.unparent();
                }
            }
        });
        self.active_profile.replace(Some(popup.clone()));
        popup.popup();
        if let Some(button) = preferred_focus {
            button.grab_focus();
        }
        if let Some((_, card)) = self.profile_cache.borrow().get(peer) {
            render(&fields, card);
            feedback.set_label("");
            feedback.set_visible(false);
        }
        if !self.online.get()
            || self
                .profile_cache
                .borrow()
                .get(peer)
                .is_some_and(|(when, _)| when.elapsed() < Duration::from_secs(300))
        {
            return;
        }
        let (reply, receive) = tokio::sync::oneshot::channel();
        if !self.command(Command::Profile {
            peer: peer.into(),
            reply,
        }) {
            feedback.set_label("Connect to load profile details.");
            return;
        }
        let ui = Rc::downgrade(self);
        let peer = peer.to_string();
        let generation = self.generation.get();
        let owner = self.state.borrow().account.clone();
        let pop = popup.downgrade();
        glib::spawn_future_local(async move {
            let result = receive.await;
            let Some(ui) = ui.upgrade().filter(|ui| {
                ui.generation.get() == generation && ui.state.borrow().account == owner
            }) else {
                return;
            };
            if pop.upgrade().is_none_or(|p| !p.is_visible()) {
                return;
            }
            match result {
                Ok(Ok(card)) => {
                    render(&fields, &card);
                    feedback.set_visible(card.fields.is_empty());
                    feedback.set_label(if card.fields.is_empty() {
                        "No public profile details."
                    } else {
                        ""
                    });
                    if ui.profile_cache.borrow().len() >= 128 {
                        ui.profile_cache.borrow_mut().clear();
                    }
                    ui.profile_cache
                        .borrow_mut()
                        .insert(peer, (Instant::now(), card));
                }
                Ok(Err(error)) => feedback.set_label(error),
                Err(_) => feedback.set_label(
                    "Profile loading was interrupted. Close and reopen the card to retry.",
                ),
            }
        });
    }
}
fn render(body: &gtk::Box, card: &crate::profiles::Profile) {
    while let Some(child) = body.first_child() {
        body.remove(&child);
    }
    for (name, value) in &card.fields {
        let group = bx(true, 3);
        let caption = text(name, "caption dim-label");
        caption.set_xalign(0.);
        group.append(&caption);
        let value = wrap(value, "");
        value.set_xalign(0.);
        value.set_max_width_chars(32);
        value.set_selectable(true);
        group.append(&value);
        body.append(&group);
    }
}
