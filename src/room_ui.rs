//! Native room workflows, durable local choices and private bookmark synchronization.
use crate::{
    protocol::Command,
    rooms::{Bookmark, BookmarkInfo},
    ui::Ui,
    widgets::*,
};
use adw::prelude::*;
use gtk::glib;
use std::{
    cell::Cell,
    rc::Rc,
    time::{Duration, Instant},
};

fn nickname(owner: &str) -> &str {
    owner.split('@').next().unwrap_or("Monogram")
}
fn busy(button: &gtk::Button) {
    button.set_sensitive(false);
    button.set_child(Some(&gtk::Spinner::builder().spinning(true).build()));
}

impl Ui {
    pub fn sync_room_bookmarks(self: &Rc<Self>) {
        if !self.online.get() {
            return;
        }
        let generation = self.generation.get();
        let owner = self.state.borrow().account.clone();
        if self
            .room_syncing
            .borrow()
            .as_ref()
            .is_some_and(|(g, _, _)| *g == generation)
        {
            return;
        }
        let Ok(choices) = self.store.dirty_room_choices(&owner) else {
            return;
        };
        let Some(choice) = choices.into_iter().find(|choice| {
            self.room_sync_retry
                .borrow()
                .get(&choice.info.jid)
                .is_none_or(|(when, _)| *when <= Instant::now())
        }) else {
            return;
        };
        let Some(iq) = self
            .session
            .borrow()
            .as_ref()
            .map(|session| crate::iq::Client::new(session.tx.clone()))
        else {
            return;
        };
        let peer = choice.info.jid.clone();
        self.room_syncing
            .replace(Some((generation, peer.clone(), choice.revision)));
        let weak = Rc::downgrade(self);
        glib::MainContext::default().spawn_local(async move {
            let result = match Bookmark::new(choice.info, None) {
                Ok(entry) => crate::rooms::save_bookmark(&iq, &owner, &entry).await,
                Err(error) => Err(error),
            };
            let Some(ui) = weak.upgrade().filter(|ui| {
                ui.generation.get() == generation && ui.state.borrow().account == owner
            }) else {
                return;
            };
            ui.room_syncing.borrow_mut().take();
            let error = result.as_ref().err().map(ToString::to_string);
            let old_error = ui.store.room_choice_error(&owner, &peer);
            let current = match ui
                .store
                .acknowledge_room_choice(&owner, &peer, choice.revision, error.as_deref())
            {
                Ok(current) => current,
                Err(_) => {
                    let previous = ui.room_sync_retry.borrow_mut().insert(
                        peer.clone(),
                        (Instant::now() + Duration::from_secs(30), 0),
                    );
                    if previous.is_none() {
                        ui.notice("Room settings synchronization could not be recorded locally. Retrying…");
                    }
                    return;
                }
            };
            if !current {
                ui.room_sync_retry.borrow_mut().remove(&peer);
            } else if let Err(error) = result {
                let mut retries = ui.room_sync_retry.borrow_mut();
                let attempt = retries
                    .get(&peer)
                    .map(|(_, attempt)| attempt.saturating_add(1))
                    .unwrap_or(0)
                    .min(6);
                let delay = if error == crate::rooms::Error::BookmarkPrivacyUnavailable {
                    86400
                } else {
                    (5u64 << attempt).min(300)
                };
                retries.insert(
                    peer.clone(),
                    (Instant::now() + Duration::from_secs(delay), attempt),
                );
                drop(retries);
                if old_error.as_deref() != Some(error.to_string().as_str()) {
                    ui.notice(&error.to_string());
                }
            } else {
                ui.room_sync_retry.borrow_mut().remove(&peer);
                ui.command(Command::LoadRoomBookmarks);
            }
            if let Ok(choices) = ui.store.room_choices(&owner) {
                ui.command(Command::UpdateRoomChoices(choices));
            }
        });
    }
    pub(crate) fn remember_room(self: &Rc<Self>, info: &BookmarkInfo) -> bool {
        let owner = self.state.borrow().account.clone();
        if self.store.save_room_choice(&owner, info).is_err() {
            self.notice(&format!(
                "Room {} is connected, but its local settings could not be saved",
                info.jid
            ));
            return false;
        }
        let mut state = self.state.borrow_mut();
        let room = state.chat_for(&info.jid);
        room.group = true;
        room.contact = false;
        room.room_autojoin = Some(info.autojoin);
        room.room_nick = info.nick.clone();
        room.name = if info.name.is_empty() {
            info.jid.clone()
        } else {
            info.name.clone()
        };
        drop(state);
        if !self.persist() {
            return false;
        }
        if let Ok(choices) = self.store.room_choices(&owner) {
            self.command(Command::UpdateRoomChoices(choices));
        }
        self.room_sync_retry.borrow_mut().remove(&info.jid);
        self.sync_room_bookmarks();
        true
    }
    pub(crate) fn open_room(self: &Rc<Self>, info: &BookmarkInfo) {
        let id = self.state.borrow_mut().chat_for(&info.jid).id.clone();
        self.select_chat(&id);
    }
    pub fn create_group_dialog(self: &Rc<Self>) {
        if !self.online.get() {
            self.notice("Connect before creating a group");
            return;
        }
        let owner = self.state.borrow().account.clone();
        let generation = self.generation.get();
        let page = adw::PreferencesPage::new();
        let details = adw::PreferencesGroup::builder().title("Group").build();
        let name = adw::EntryRow::builder().title("Group name").build();
        let server = adw::EntryRow::builder().title("Group server").build();
        let nick = adw::EntryRow::builder()
            .title("Your nickname")
            .text(nickname(&owner))
            .build();
        for row in [&name, &server, &nick] {
            details.add(row);
        }
        page.add(&details);
        let people = adw::PreferencesGroup::builder()
            .title("Choose people")
            .build();
        let selected: Vec<_> = self
            .state
            .borrow()
            .chats
            .iter()
            .filter(|chat| {
                !chat.group
                    && chat.jid != owner
                    && crate::protocol::bare(&chat.jid).ok().as_deref() == Some(chat.jid.as_str())
            })
            .map(|chat| {
                let check = gtk::CheckButton::new();
                let row = adw::ActionRow::builder()
                    .title(&chat.name)
                    .subtitle(&chat.jid)
                    .use_markup(false)
                    .activatable_widget(&check)
                    .build();
                row.add_prefix(&self.chat_avatar(chat, 32));
                row.add_suffix(&check);
                people.add(&row);
                (chat.jid.clone(), check)
            })
            .collect();
        page.add(&people);
        let status = wrap("Finding group services…", "dim-label");
        details.add(&status);
        let (dialog, header) = self.dialog("New Group", &page, 500, 600);
        let create = gtk::Button::with_label("Create");
        create.add_css_class("suggested-action");
        header.pack_end(&create);
        let (reply, receive) = tokio::sync::oneshot::channel();
        self.command(Command::RoomServices { reply });
        let target = server.downgrade();
        let feedback = status.downgrade();
        glib::MainContext::default().spawn_local(async move {
            if let (Some(server),Some(status))=(target.upgrade(),feedback.upgrade()) {
                match receive.await {
                    Ok(Ok(services)) if !services.is_empty()=>{ if server.text().trim().is_empty(){server.set_text(&services[0]);} status.set_text("Private groups disclose member identities so encrypted messages can reach every device."); },
                    _=>status.set_text("Enter a group server, or join an existing room by address."),
                }
            }
        });
        let finished = Rc::new(Cell::new(false));
        let running = Rc::new(std::cell::RefCell::new(None::<glib::JoinHandle<()>>));
        let (done, task) = (finished.clone(), running.clone());
        dialog.connect_closed(move |_| {
            if !done.get() {
                if let Some(task) = task.borrow_mut().take() {
                    task.abort();
                }
            }
        });
        let weak = Rc::downgrade(self);
        create.connect_clicked(move |button| {
            let Some(ui) = weak.upgrade().filter(|ui| {
                ui.generation.get() == generation && ui.state.borrow().account == owner
            }) else {
                return;
            };
            let title = name.text().trim().to_string();
            let service = server.text().trim().to_string();
            let nickname = nick.text().trim().to_string();
            if title.is_empty() || service.is_empty() || nickname.is_empty() {
                status.set_text("Enter a name, group server and nickname.");
                return;
            }
            let members: Vec<_> = selected
                .iter()
                .filter(|(_, check)| check.is_active())
                .map(|(jid, _)| jid.clone())
                .collect();
            let (reply, receive) = tokio::sync::oneshot::channel();
            if !ui.command(Command::CreateRoom {
                service,
                title,
                nick: nickname,
                reply,
            }) {
                status.set_text("Reconnect before creating a group.");
                return;
            }
            busy(button);
            status.set_text("Creating private group…");
            let weak = Rc::downgrade(&ui);
            let dialog = dialog.downgrade();
            let button = button.downgrade();
            let status = status.downgrade();
            let owner = owner.clone();
            let finished = finished.clone();
            let task = glib::MainContext::default().spawn_local(async move {
                let result = receive.await;
                let Some(ui) = weak.upgrade().filter(|ui| {
                    ui.generation.get() == generation && ui.state.borrow().account == owner
                }) else {
                    return;
                };
                match result {
                    Ok(Ok(info)) => {
                        if !ui.remember_room(&info) {
                            if let Some(status) = status.upgrade() {
                                status.set_text(&format!(
                                    "Room {} was created; its local settings could not be saved.",
                                    info.jid
                                ));
                            }
                            return;
                        }
                        finished.set(true);
                        if let Some(dialog) = dialog.upgrade() {
                            dialog.close();
                        }
                        ui.open_room(&info);
                        ui.invite_room_people(&info.jid, members);
                    }
                    result => {
                        if let Some(status) = status.upgrade() {
                            status.set_text(&match result {
                                Ok(Err(error)) => error.to_string(),
                                _ => "Room creation was interrupted".into(),
                            });
                        }
                        if let Some(button) = button.upgrade() {
                            button.set_label("Create");
                            button.set_sensitive(true);
                        }
                    }
                }
            });
            running.replace(Some(task));
        });
    }
    fn invite_room_people(self: &Rc<Self>, peer: &str, members: Vec<String>) {
        if members.is_empty() {
            return;
        }
        let (reply, receive) = tokio::sync::oneshot::channel();
        if !self.command(Command::InviteRoom {
            peer: peer.into(),
            members,
            reply,
        }) {
            self.notice("Group created; invitations could not start");
            return;
        }
        let weak = Rc::downgrade(self);
        let generation = self.generation.get();
        glib::MainContext::default().spawn_local(async move {
            let result = receive.await;
            if let Some(ui) = weak
                .upgrade()
                .filter(|ui| ui.generation.get() == generation)
            {
                let message = match result {
                    Ok(Ok(failed)) if failed.is_empty() => return,
                    Ok(Ok(failed)) => format!("Could not invite: {}", failed.join(", ")),
                    Ok(Err(error)) => error.to_string(),
                    Err(_) => "Invitations were interrupted".into(),
                };
                let alert = adw::AlertDialog::builder()
                    .heading("Group Created; Invitations Incomplete")
                    .body(&message)
                    .build();
                alert.add_response("close", "Close");
                alert.present(Some(&ui.window));
            }
        });
    }
    pub fn join_room_dialog(self: &Rc<Self>) {
        if !self.online.get() {
            self.notice("Connect before joining a room");
            return;
        }
        let owner = self.state.borrow().account.clone();
        let generation = self.generation.get();
        let page = adw::PreferencesPage::new();
        let fields = adw::PreferencesGroup::new();
        let address = adw::EntryRow::builder().title("Room address").build();
        let nick = adw::EntryRow::builder()
            .title("Your nickname")
            .text(nickname(&owner))
            .build();
        let password = adw::PasswordEntryRow::builder().title("Password").build();
        let saved = adw::SwitchRow::builder()
            .title("Use saved password when blank")
            .active(true)
            .build();
        fields.add(&address);
        fields.add(&nick);
        fields.add(&password);
        fields.add(&saved);
        let status = wrap("", "dim-label");
        fields.add(&status);
        page.add(&fields);
        let (dialog, header) = self.dialog("Join a Room", &page, 500, 420);
        let join = gtk::Button::with_label("Join");
        join.add_css_class("suggested-action");
        header.pack_end(&join);
        let finished = Rc::new(Cell::new(false));
        let running = Rc::new(std::cell::RefCell::new(None::<glib::JoinHandle<()>>));
        let (done, task) = (finished.clone(), running.clone());
        dialog.connect_closed(move |_| {
            if !done.get() {
                if let Some(task) = task.borrow_mut().take() {
                    task.abort();
                }
            }
        });
        let weak = Rc::downgrade(self);
        join.connect_clicked(move |button| {
            let Some(ui) = weak.upgrade().filter(|ui| {
                ui.generation.get() == generation && ui.state.borrow().account == owner
            }) else {
                return;
            };
            let Ok(peer) = crate::protocol::bare(address.text().trim()) else {
                status.set_text("Enter a room address such as room@group-server.");
                return;
            };
            let nickname = nick.text().trim().to_string();
            if nickname.is_empty() {
                status.set_text("Enter your nickname.");
                return;
            }
            let password = if password.text().is_empty() && saved.is_active() {
                None
            } else {
                Some(crate::rooms::Password(zeroize::Zeroizing::new(
                    password.text().to_string(),
                )))
            };
            let (reply, receive) = tokio::sync::oneshot::channel();
            if !ui.command(Command::JoinExistingRoom {
                peer,
                nick: nickname,
                password,
                reply,
            }) {
                status.set_text("Reconnect before joining.");
                return;
            }
            busy(button);
            status.set_text("Checking the room and joining…");
            let weak = Rc::downgrade(&ui);
            let dialog = dialog.downgrade();
            let button = button.downgrade();
            let status = status.downgrade();
            let owner = owner.clone();
            let finished = finished.clone();
            let task = glib::MainContext::default().spawn_local(async move {
                let result = receive.await;
                let Some(ui) = weak.upgrade().filter(|ui| {
                    ui.generation.get() == generation && ui.state.borrow().account == owner
                }) else {
                    return;
                };
                match result {
                    Ok(Ok(info)) => {
                        if ui.remember_room(&info) {
                            finished.set(true);
                            if let Some(dialog) = dialog.upgrade() {
                                dialog.close();
                            }
                            ui.open_room(&info);
                        }
                    }
                    result => {
                        if let Some(status) = status.upgrade() {
                            status.set_text(&match result {
                                Ok(Err(error)) => error.to_string(),
                                _ => "Joining was interrupted".into(),
                            });
                        }
                        if let Some(button) = button.upgrade() {
                            button.set_label("Join");
                            button.set_sensitive(true);
                        }
                    }
                }
            });
            running.replace(Some(task));
        });
    }
    pub fn room_preferences(self: &Rc<Self>) {
        let state = self.state.borrow();
        let owner = state.account.clone();
        let chat = state.current().clone();
        drop(state);
        let initial = self
            .store
            .room_choices(&owner)
            .unwrap_or_default()
            .into_iter()
            .find(|entry| entry.info.jid == chat.jid)
            .map(|entry| entry.info)
            .unwrap_or(BookmarkInfo {
                jid: chat.jid.clone(),
                name: chat.name.clone(),
                nick: chat
                    .room_nick
                    .clone()
                    .or_else(|| Some(nickname(&owner).into())),
                autojoin: chat.room_autojoin.unwrap_or(false),
            });
        let page = adw::PreferencesPage::new();
        let group = adw::PreferencesGroup::new();
        let nick = adw::EntryRow::builder()
            .title("Nickname on next join")
            .text(initial.nick.as_deref().unwrap_or(nickname(&owner)))
            .build();
        let autojoin = adw::SwitchRow::builder()
            .title("Rejoin automatically")
            .active(initial.autojoin)
            .build();
        group.add(&nick);
        group.add(&autojoin);
        let status = wrap(
            &self
                .store
                .room_choice_error(&owner, &chat.jid)
                .unwrap_or_default(),
            "dim-label",
        );
        group.add(&status);
        page.add(&group);
        let (dialog, header) = self.dialog("Saved Room", &page, 480, 320);
        let save = gtk::Button::with_label("Save");
        save.add_css_class("suggested-action");
        header.pack_end(&save);
        let weak = Rc::downgrade(self);
        let generation = self.generation.get();
        save.connect_clicked(move |_| {
            if let Some(ui) = weak.upgrade().filter(|ui| {
                ui.generation.get() == generation && ui.state.borrow().account == owner
            }) {
                let mut info = initial.clone();
                info.nick = Some(nick.text().trim().into());
                info.autojoin = autojoin.is_active();
                if ui.remember_room(&info) {
                    dialog.close();
                } else {
                    status.set_text("Saved room settings could not be updated.");
                }
            }
        });
    }
    pub fn leave_current_room(self: &Rc<Self>) {
        let state = self.state.borrow();
        let owner = state.account.clone();
        let chat = state.current().clone();
        drop(state);
        let mut info = self
            .store
            .room_choices(&owner)
            .unwrap_or_default()
            .into_iter()
            .find(|choice| choice.info.jid == chat.jid)
            .map(|choice| choice.info)
            .unwrap_or(BookmarkInfo {
                jid: chat.jid.clone(),
                name: chat.name,
                nick: Some(nickname(&owner).into()),
                autojoin: false,
            });
        info.autojoin = false;
        if !self.remember_room(&info) {
            return;
        }
        let (reply, receive) = tokio::sync::oneshot::channel();
        if !self.command(Command::LeaveRoom {
            peer: info.jid,
            reply,
        }) {
            self.notice("Automatic rejoin is disabled. Reconnect to leave the room.");
            return;
        }
        let weak = Rc::downgrade(self);
        let generation = self.generation.get();
        glib::MainContext::default().spawn_local(async move {
            if let Ok(Err(error)) = receive.await {
                if let Some(ui) = weak
                    .upgrade()
                    .filter(|ui| ui.generation.get() == generation)
                {
                    ui.notice(&error.to_string());
                }
            }
        });
    }
}
