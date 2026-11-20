//! A compact native profile card shared by chat avatars and participant lists.
use crate::{model::Chat, protocol::Command, ui::Ui, widgets::*};
use adw::prelude::*;
use gtk::glib;
use std::{
    rc::Rc,
    time::{Duration, Instant},
};
pub(crate) struct ProfileControls {
    owner: String,
    peer: String,
    fallback: String,
    room: Option<(String,String)>,
    title: gtk::Label,
    status: gtk::Label,
    banner: gtk::Overlay,
}

impl Ui {
    fn profile_chat(&self, peer: &str, name: &str, room: Option<&(String,String)>) -> Chat {
        if let Some((room,nick)) = room {
            if let Some(person) = self.room_rosters.borrow().get(room)
                .and_then(|people| people.iter().find(|person| &person.nick == nick))
            {
                return self.participant_chat(room,person);
            }
        }
        let mut chat = self.state.borrow().chats.iter().find(|chat| chat.jid == peer).cloned()
            .unwrap_or_else(||Chat { name:name.into(),jid:peer.into(),group:peer.contains('/'),..Default::default() });
        if let Some(local) = self.local_contacts.borrow().get(peer) { chat.local_name=local.nickname.clone(); }
        if room.is_some() { chat.name=name.into();chat.subtitle="Not in this room".into(); }
        chat
    }
    pub(crate) fn participant_profile_card(self: &Rc<Self>, anchor:&impl IsA<gtk::Widget>, room:&str, person:&crate::rooms::members::Participant) {
        let peer=person.jid.clone().unwrap_or_else(||format!("{room}/{}",person.nick));
        self.profile_card_context(anchor,&peer,&person.nick,Some((room.into(),person.nick.clone())));
    }
    pub(crate) fn refresh_profile_card(&self) {
        let controls=self.profile_controls.borrow();
        let Some(card)=controls.as_ref().filter(|card|card.owner==self.state.borrow().account) else{return};
        if self.active_profile.borrow().as_ref().is_none_or(|popup|!popup.is_visible()) {return;}
        let chat=self.profile_chat(&card.peer,&card.fallback,card.room.as_ref());
        card.title.set_label(&crate::text_safety::visible(chat.display_name(),128));
        card.status.set_label(profile_status(&chat,card.room.is_some()));
        if let Some(old)=card.banner.last_child().and_downcast::<adw::Avatar>() {card.banner.remove_overlay(&old);}
        add_avatar(self,&card.banner,&chat);
    }

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
        self.profile_card_context(anchor,peer,name,None);
    }
    fn profile_card_context(self:&Rc<Self>,anchor:&impl IsA<gtk::Widget>,peer:&str,name:&str,room:Option<(String,String)>) {
        if peer.is_empty() {
            return;
        }
        if let Some(previous) = self.active_profile.borrow_mut().take() {
            previous.popdown();
            if previous.parent().is_some() {
                previous.unparent();
            }
        }
        let chat = self.profile_chat(peer,name,room.as_ref());
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
        add_avatar(self,&banner,&chat);
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
        let status = wrap(profile_status(&chat,room.is_some()), "caption");
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
        self.profile_controls.replace(Some(ProfileControls {
            owner:self.state.borrow().account.clone(),peer:peer.into(),fallback:name.into(),room,
            title:title.clone(),status:status.clone(),banner:banner.clone(),
        }));
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

fn profile_status(chat:&Chat, room:bool)->&str {
    if room && !chat.subtitle.is_empty() { &chat.subtitle }
    else if chat.group { "Room participant" }
    else if !chat.subtitle.is_empty() { &chat.subtitle }
    else { "Presence unavailable" }
}
fn add_avatar(ui:&Ui,banner:&gtk::Overlay,chat:&Chat) {
    let avatar=ui.chat_avatar(chat,72);
    avatar.set_halign(gtk::Align::Start);avatar.set_valign(gtk::Align::End);avatar.set_margin_start(12);
    banner.add_overlay(&avatar);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{model::{Account,State},rooms::members::Participant};
    use gtk::{gdk,gio};
    #[test]
    #[ignore="requires private GTK display and D-Bus"]
    fn room_profile_matches_row_name_avatar_and_live_room_presence() {
        let app=adw::Application::builder().application_id("com.negzero.ProfileConsistencyFixture").flags(gio::ApplicationFlags::NON_UNIQUE).build();app.register(None::<&gio::Cancellable>).unwrap();
        let ui=Ui::new(&app,true);let owner="owner@example.invalid";let peer="friend@example.invalid";let room="room@conference.example.invalid";
        ui.store.save_account(&Account{jid:owner.into(),host:String::new(),port:5222,direct_tls:false}).unwrap();
        let mut state=State::live(owner,vec![]);let direct=state.chat_for(peer);direct.name=peer.into();direct.subtitle="Offline".into();
        ui.activate_state(state);ui.window.set_default_size(1100,700);ui.window.present();
        let pump=||{let until=Instant::now()+Duration::from_millis(200);while Instant::now()<until{glib::MainContext::default().iteration(false);std::thread::sleep(Duration::from_millis(2));}};pump();
        let image:gdk::Texture=gdk::MemoryTexture::new(1,1,gdk::MemoryFormat::R8g8b8a8,&glib::Bytes::from_owned(vec![255,0,0,255]),4).upcast();
        ui.avatars.borrow_mut().cache.insert(peer.into(),(Instant::now()+Duration::from_secs(300),Some(image.clone())));
        let mut person=Participant{nick:"Room Nick".into(),jid:Some(peer.into()),affiliation:"member".into(),role:"participant".into(),show:String::new(),own:false};
        ui.room_roster_changed(room,vec![person.clone()]);
        let row=ui.participant_chat(room,&person);assert_eq!(row.display_name(),"Room Nick");assert_eq!(row.subtitle,"Online");
        let row_avatar=ui.chat_avatar(&row,40);assert_eq!(row_avatar.custom_image().unwrap(),image.clone().upcast::<gdk::Paintable>());
        ui.participant_profile_card(&ui.title,room,&person);pump();
        let controls=ui.profile_controls.borrow();let card=controls.as_ref().unwrap();assert_eq!(card.title.label(),"Room Nick");assert_eq!(card.status.label(),"Online");
        let avatar=card.banner.last_child().and_downcast::<adw::Avatar>().unwrap();assert_eq!(avatar.custom_image(),row_avatar.custom_image());drop(controls);
        ui.backend_event(ui.generation.get(),owner,crate::protocol::BackendEvent::Presence{peer:peer.into(),online:false,visible:true});
        assert_eq!(ui.profile_controls.borrow().as_ref().unwrap().status.label(),"Online","direct offline must not override known room presence");
        person.show="away".into();ui.room_roster_changed(room,vec![person.clone()]);assert_eq!(ui.profile_controls.borrow().as_ref().unwrap().status.label(),"Away");
        let mut local=crate::contact_details::LocalContact::default();local.peer=peer.into();local.nickname="Local Nick".into();ui.local_contacts.borrow_mut().insert(peer.into(),local);
        ui.local_contact_avatars.borrow_mut().insert(peer.into(),image.clone());ui.refresh_profile_card();
        assert_eq!(ui.participant_chat(room,&person).display_name(),"Local Nick");assert_eq!(ui.profile_controls.borrow().as_ref().unwrap().title.label(),"Local Nick");
        ui.room_roster_changed(room,vec![]);assert_eq!(ui.profile_controls.borrow().as_ref().unwrap().status.label(),"Not in this room");
        ui.active_profile.borrow().as_ref().unwrap().popdown();ui.window.close();
    }
}
