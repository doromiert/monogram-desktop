use crate::{model::*, protocol, ui::Ui, wallet::WalletCard, widgets::*};
use adw::prelude::*;
use gtk::{gio, glib};
use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    rc::Rc,
};
impl Ui {
    pub fn dialog(
        self: &Rc<Self>,
        title: &str,
        child: &impl IsA<gtk::Widget>,
        width: i32,
        height: i32,
    ) -> (adw::Dialog, adw::HeaderBar) {
        let d = adw::Dialog::builder()
            .title(title)
            .content_width(width)
            .content_height(height)
            .build();
        let view = adw::ToolbarView::new();
        let header = adw::HeaderBar::builder()
            .title_widget(&adw::WindowTitle::new(title, ""))
            .build();
        view.add_top_bar(&header);
        view.set_content(Some(child));
        d.set_child(Some(&view));
        self.dialogs.borrow_mut().push(d.clone());
        let weak = Rc::downgrade(self);
        d.connect_closed(move |d| {
            if let Some(ui) = weak.upgrade() {
                if let Ok(mut dialogs) = ui.dialogs.try_borrow_mut() {
                    dialogs.retain(|old| old != d);
                }
            }
        });
        d.present(Some(&self.window));
        (d, header)
    }
    pub fn context_actions(
        self: &Rc<Self>,
        widget: &impl IsA<gtk::Widget>,
        items: Vec<(String, String)>,
    ) {
        let right = gtk::GestureClick::new();
        right.set_button(3);
        let parent = widget.as_ref().downgrade();
        let weak = Rc::downgrade(self);
        right.connect_pressed(move |_, _, x, y| {
            if let (Some(ui), Some(parent)) = (weak.upgrade(), parent.upgrade()) {
                let p = gtk::Popover::builder().has_arrow(false).build();
                p.set_parent(&parent);
                p.set_pointing_to(Some(&gtk::gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
                let content = bx(true, 2);
                for (title, key) in &items {
                    let b = gtk::Button::with_label(title);
                    b.add_css_class("flat");
                    let weak = Rc::downgrade(&ui);
                    let pop = p.downgrade();
                    let key = key.clone();
                    b.connect_clicked(move |_| {
                        if let Some(p) = pop.upgrade() {
                            p.popdown();
                            if p.parent().is_some() {
                                p.unparent()
                            }
                        }
                        if let Some(ui) = weak.upgrade() {
                            ui.context_action(&key)
                        }
                    });
                    content.append(&b)
                }
                p.set_child(Some(&content));
                p.connect_closed(|p| {
                    if p.parent().is_some() {
                        p.unparent()
                    }
                });
                p.popup();
            }
        });
        widget.add_controller(right);
    }
    fn context_action(self: &Rc<Self>, key: &str) {
        let (action, id) = key.split_once(':').unwrap_or((key, ""));
        match action {
            "story-new" => self.new_story(),
            "folder-edit" => self.edit_folder(id),
            "folder-new" => self.edit_folder(""),
            "folder-left" | "folder-right" => {
                let index = self
                    .state
                    .borrow()
                    .folder_order
                    .iter()
                    .position(|n| n == id);
                if let Some(i) = index {
                    let target = if action == "folder-left" {
                        i.saturating_sub(1)
                    } else {
                        (i + 1).min(self.state.borrow().folder_order.len() - 1)
                    };
                    self.state.borrow_mut().reorder(id, target);
                    self.rebuild_folders();
                    self.persist();
                }
            }
            "chat-open" => self.select_chat(id),
            "chat-pin" | "chat-unread" | "chat-archive" | "chat-mute" => {
                let mut s = self.state.borrow_mut();
                if let Some(c) = s.chats.iter_mut().find(|c| c.id == id) {
                    match action {
                        "chat-pin" => c.pinned = !c.pinned,
                        "chat-unread" => {
                            let unread = c.unread == 0 && !c.unread_mark;
                            c.unread = 0;
                            c.unread_mark = unread
                        }
                        "chat-archive" => c.archived = !c.archived,
                        _ => {
                            c.notifications = if c.notifications == "all" {
                                "forever"
                            } else {
                                "all"
                            }
                            .into()
                        }
                    }
                }
                drop(s);
                self.persist();
                self.refresh_chats(None)
            }
            _ => {}
        }
    }
    pub fn organize(self: &Rc<Self>, action: &str) {
        let mut s = self.state.borrow_mut();
        let c = s.current_mut();
        if action == "archive" {
            c.archived = !c.archived;
        }
        drop(s);
        self.persist();
        self.refresh_chats(None);
    }
    pub fn edit_folder(self: &Rc<Self>, name: &str) {
        let owner = self.state.borrow().account.clone();
        let rule = self
            .state
            .borrow()
            .folders
            .get(name)
            .cloned()
            .unwrap_or_default();
        let page = adw::PreferencesPage::new();
        let group = adw::PreferencesGroup::new();
        let entry = adw::EntryRow::builder()
            .title("Folder name")
            .text(name)
            .build();
        group.add(&entry);
        page.add(&group);
        let include = adw::PreferencesGroup::builder()
            .title("Included Chats")
            .description("Matching chats are added automatically")
            .build();
        page.add(&include);
        let mut types = HashMap::new();
        for (key, title) in [
            ("contacts", "Contacts"),
            ("non_contacts", "Non-Contacts"),
            ("groups", "Groups"),
            ("channels", "Channels"),
            ("bots", "Bots"),
        ] {
            let row = adw::SwitchRow::builder()
                .title(title)
                .active(rule.types.contains(key))
                .build();
            include.add(&row);
            types.insert(key.to_string(), row);
        }
        let exclude = adw::PreferencesGroup::builder()
            .title("Excluded Chats")
            .build();
        page.add(&exclude);
        let mut flags = HashMap::new();
        for (key, title, value) in [
            ("exclude_muted", "Muted Chats", rule.exclude_muted),
            ("exclude_read", "Read Chats", rule.exclude_read),
            ("exclude_archived", "Archived Chats", rule.exclude_archived),
        ] {
            let row = adw::SwitchRow::builder().title(title).active(value).build();
            exclude.add(&row);
            flags.insert(key, row);
        }
        let exceptions = adw::PreferencesGroup::builder()
            .title("Exceptions")
            .description("Chosen chats override the automatic rules")
            .build();
        page.add(&exceptions);
        let mut choices: HashMap<String, HashMap<String, gtk::CheckButton>> = HashMap::new();
        for (key, title, selected) in [
            ("include", "Always Include", &rule.include),
            ("exclude", "Never Include", &rule.exclude),
        ] {
            let e = adw::ExpanderRow::builder().title(title).build();
            for chat in &self.state.borrow().chats {
                let check = gtk::CheckButton::builder()
                    .active(selected.contains(&chat.id))
                    .valign(gtk::Align::Center)
                    .build();
                let row = adw::ActionRow::builder()
                    .title(&chat.name)
                    .use_markup(false)
                    .activatable_widget(&check)
                    .build();
                row.add_prefix(&avatar(chat, 32));
                row.add_suffix(&check);
                e.add_row(&row);
                choices
                    .entry(key.into())
                    .or_default()
                    .insert(chat.id.clone(), check);
            }
            exceptions.add(&e)
        }
        for (key, other) in [("include", "exclude"), ("exclude", "include")] {
            for (id, check) in &choices[key] {
                let opposite = choices[other][id].clone();
                check.connect_toggled(move |b| {
                    if b.is_active() {
                        opposite.set_active(false)
                    }
                });
            }
        }
        let (d, header) = self.dialog(
            if name.is_empty() {
                "New Folder"
            } else {
                "Edit Folder"
            },
            &page,
            520,
            560,
        );
        let save = gtk::Button::with_label("Save");
        save.add_css_class("suggested-action");
        header.pack_end(&save);
        let names = self
            .state
            .borrow()
            .folders
            .keys()
            .cloned()
            .collect::<HashSet<_>>();
        let old = name.to_string();
        let e = entry.clone();
        let b = save.clone();
        let n = old.clone();
        entry.connect_changed(move |_| {
            let v = e.text();
            b.set_sensitive(!v.trim().is_empty() && (v.trim() == n || !names.contains(v.trim())))
        });
        save.set_sensitive(!name.is_empty());
        let weak = Rc::downgrade(self);
        save.connect_clicked(move |_| {
            if let Some(ui) = weak.upgrade() {
                if ui.state.borrow().account != owner {
                    return;
                }
                let new = entry.text().trim().to_string();
                if new.is_empty() {
                    return;
                }
                let rule = Folder {
                    types: types
                        .iter()
                        .filter(|(_, r)| r.is_active())
                        .map(|(k, _)| k.clone())
                        .collect(),
                    exclude_muted: flags["exclude_muted"].is_active(),
                    exclude_read: flags["exclude_read"].is_active(),
                    exclude_archived: flags["exclude_archived"].is_active(),
                    include: choices["include"]
                        .iter()
                        .filter(|(_, c)| c.is_active())
                        .map(|(id, _)| id.clone())
                        .collect(),
                    exclude: choices["exclude"]
                        .iter()
                        .filter(|(_, c)| c.is_active())
                        .map(|(id, _)| id.clone())
                        .collect(),
                };
                let mut s = ui.state.borrow_mut();
                if old != new {
                    if let Some(i) = s.folder_order.iter().position(|n| n == &old) {
                        s.folder_order[i] = new.clone();
                        s.folders.remove(&old);
                    } else {
                        s.folder_order.push(new.clone());
                    }
                }
                s.folders.insert(new.clone(), rule);
                s.folder = new;
                drop(s);
                ui.rebuild_folders();
                ui.refresh_chats(Some(true));
                ui.persist();
                d.close();
            }
        });
    }
    pub fn accounts_dialog(self: &Rc<Self>) {
        let page = adw::PreferencesPage::new();
        let g = adw::PreferencesGroup::builder()
            .title("XMPP Accounts")
            .description("Passwords are kept only in memory for the current connection")
            .build();
        page.add(&g);
        for account in self.store.accounts() {
            let status = if self.state.borrow().account == account.jid {
                self.status.borrow().clone()
            } else {
                "Saved account".into()
            };
            let row = adw::ActionRow::builder()
                .title(&account.jid)
                .use_markup(false)
                .subtitle(&status)
                .build();
            let controls = bx(false, 6);
            controls.set_valign(gtk::Align::Center);
            let connect = gtk::Button::with_label("Connect");
            let config = account.clone();
            self.weak_button(&connect, move |s| s.login(Some(config.clone())));
            controls.append(&connect);
            let remove = icon_button("user-trash-symbolic", "Remove account");
            let owner = account.jid.clone();
            self.weak_button(&remove, move |s| s.remove_account(&owner));
            controls.append(&remove);
            row.add_suffix(&controls);
            g.add(&row)
        }
        let options = adw::PreferencesGroup::builder().title("Connection").build();
        page.add(&options);
        let add = adw::ActionRow::builder()
            .title("Add Account…")
            .activatable(true)
            .build();
        let weak = Rc::downgrade(self);
        add.connect_activated(move |_| {
            if let Some(ui) = weak.upgrade() {
                ui.login(None)
            }
        });
        options.add(&add);
        if !self.state.borrow().account.is_empty() {
            let row = adw::ActionRow::builder()
                .title("Disconnect")
                .subtitle("Keep local history and drafts")
                .activatable(true)
                .build();
            let weak = Rc::downgrade(self);
            row.connect_activated(move |_| {
                if let Some(ui) = weak.upgrade() {
                    ui.save_draft();
                    ui.session.borrow_mut().take();
                    ui.generation.set(ui.generation.get() + 1);
                    ui.online.set(false);
                    ui.status.replace("Disconnected".into());
                    ui.update_mode();
                }
            });
            options.add(&row)
        }
        let demo = adw::ActionRow::builder()
            .title("Open Offline Demo")
            .subtitle("Fictional data, separate from live accounts")
            .activatable(true)
            .build();
        let weak = Rc::downgrade(self);
        demo.connect_activated(move |_| {
            if let Some(ui) = weak.upgrade() {
                let state = ui.demo.borrow().clone();
                ui.activate_state(state)
            }
        });
        options.add(&demo);
        self.dialog("Accounts", &page, 560, 480);
    }
    pub fn login(self: &Rc<Self>, config: Option<Account>) {
        let config = config.unwrap_or(Account {
            jid: String::new(),
            host: String::new(),
            port: 5222,
            direct_tls: false,
        });
        let page = adw::PreferencesPage::new();
        let group=adw::PreferencesGroup::builder().title("Sign In").description("Direct text messaging over verified TLS. End-to-end encryption is not available in this desktop build yet.").build();
        let jid = adw::EntryRow::builder()
            .title("XMPP address")
            .text(&config.jid)
            .build();
        let password = adw::PasswordEntryRow::builder().title("Password").build();
        group.add(&jid);
        group.add(&password);
        page.add(&group);
        let advanced=adw::PreferencesGroup::builder().title("Connection").description("Leave server override empty for DNS discovery. Certificates are verified for the XMPP domain.").build();
        let host = adw::EntryRow::builder()
            .title("Server override")
            .text(&config.host)
            .build();
        let port = adw::EntryRow::builder()
            .title("Port")
            .text(config.port.to_string())
            .input_purpose(gtk::InputPurpose::Digits)
            .build();
        let direct = adw::SwitchRow::builder()
            .title("Direct TLS")
            .subtitle("Leave off for STARTTLS")
            .active(config.direct_tls)
            .build();
        advanced.add(&host);
        advanced.add(&port);
        advanced.add(&direct);
        page.add(&advanced);
        let error = wrap("", "error");
        let body = bx(true, 8);
        body.append(&page);
        margins(&error, 12);
        body.append(&error);
        let (d, header) = self.dialog("Connect Account", &scroll(&body), 500, 600);
        let connect = gtk::Button::with_label("Connect");
        connect.add_css_class("suggested-action");
        header.pack_end(&connect);
        let p = password.clone();
        d.connect_closed(move |_| p.set_text(""));
        let p = port.clone();
        direct.connect_active_notify(move |row| {
            p.set_text(if row.is_active() { "5223" } else { "5222" })
        });
        let j = jid.clone();
        let p = password.clone();
        let b = connect.clone();
        jid.connect_changed(move |_| {
            b.set_sensitive(!j.text().trim().is_empty() && !p.text().is_empty())
        });
        let j = jid.clone();
        let p = password.clone();
        let b = connect.clone();
        password.connect_changed(move |_| {
            b.set_sensitive(!j.text().trim().is_empty() && !p.text().is_empty())
        });
        connect.set_sensitive(false);
        let weak = Rc::downgrade(self);
        connect.connect_clicked(move |_| {
            let Some(ui) = weak.upgrade() else { return };
            let address = match protocol::bare(jid.text().as_str()) {
                Ok(j) => j,
                Err(e) => {
                    error.set_text(&e);
                    return;
                }
            };
            let server = host.text().trim().to_string();
            if server.chars().any(char::is_whitespace) || server.contains('/') {
                error.set_text("Enter only a server hostname or IP address");
                return;
            }
            let Ok(port) = port.text().parse::<u16>() else {
                error.set_text("Port must be between 1 and 65535");
                return;
            };
            if port == 0 {
                error.set_text("Port must be between 1 and 65535");
                return;
            }
            let secret = password.text().to_string();
            password.set_text("");
            ui.connect_account(
                Account {
                    jid: address,
                    host: server,
                    port,
                    direct_tls: direct.is_active(),
                },
                secret,
            );
            d.close();
        });
    }
    pub fn remove_account(self: &Rc<Self>, owner: &str) {
        let d=adw::AlertDialog::builder().heading("Remove Account?").body("This removes its local history and drafts. Messages stored by the server are unaffected.").build();
        d.add_responses(&[("cancel", "Cancel"), ("remove", "Remove")]);
        d.set_response_appearance("remove", adw::ResponseAppearance::Destructive);
        let owner = owner.to_string();
        let weak = Rc::downgrade(self);
        d.connect_response(None, move |_, r| {
            if r == "remove" {
                if let Some(ui) = weak.upgrade() {
                    if ui.state.borrow().account == owner {
                        let demo = ui.demo.borrow().clone();
                        ui.activate_state(demo)
                    }
                    if ui.store.forget(&owner).is_err() {
                        ui.notice("Local account removal failed");
                        return;
                    }
                    ui.accounts_dialog();
                }
            }
        });
        d.present(Some(&self.window));
    }
    pub fn new_chat(self: &Rc<Self>) {
        let live = !self.state.borrow().account.is_empty();
        let body = bx(true, 18);
        margins(&body, 24);
        let entry = gtk::SearchEntry::builder()
            .placeholder_text("Name or XMPP address")
            .build();
        body.append(&entry);
        let list = gtk::ListBox::new();
        list.add_css_class("boxed-list");
        list.set_selection_mode(gtk::SelectionMode::None);
        let (d, header) = self.dialog("New Conversation", &scroll(&body), 480, 560);
        for c in self.state.borrow().chats.clone() {
            if c.group || c.id == "saved" {
                continue;
            }
            let row = adw::ActionRow::builder()
                .title(&c.name)
                .subtitle(if live {
                    c.jid.as_str()
                } else {
                    c.subtitle.as_str()
                })
                .use_markup(false)
                .activatable(true)
                .build();
            row.add_prefix(&avatar(&c, 36));
            let weak = Rc::downgrade(self);
            let d = d.clone();
            row.connect_activated(move |_| {
                if let Some(ui) = weak.upgrade() {
                    ui.select_chat(&c.id);
                    d.close();
                }
            });
            list.append(&row)
        }
        body.append(&list);
        let list = list.clone();
        entry.connect_search_changed(move |e| {
            let query = e.text().to_lowercase();
            let mut row = list.first_child();
            while let Some(c) = row {
                row = c.next_sibling();
                let text = c
                    .clone()
                    .downcast::<adw::ActionRow>()
                    .map(|r| r.title().to_lowercase())
                    .unwrap_or_default();
                c.set_visible(query.is_empty() || text.contains(&query));
            }
        });
        if live {
            let open = gtk::Button::with_label("Open");
            open.add_css_class("suggested-action");
            header.pack_end(&open);
            let weak = Rc::downgrade(self);
            let owner = self.state.borrow().account.clone();
            open.connect_clicked(move |_| {
                if let Some(ui) = weak.upgrade() {
                    if ui.state.borrow().account != owner {
                        return;
                    }
                    match protocol::bare(entry.text().as_str()) {
                        Ok(peer) => {
                            let id = ui.state.borrow_mut().chat_for(&peer).id.clone();
                            ui.select_chat(&id);
                            ui.persist();
                            d.close();
                        }
                        Err(e) => ui.notice(&e),
                    }
                }
            });
        } else {
            body.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
            for (title, action) in [
                ("New Group", "group"),
                ("Join a Room or MIX Channel", "room"),
            ] {
                let b = gtk::Button::with_label(title);
                self.weak_button(&b, move |s| {
                    if action == "group" {
                        s.new_group()
                    } else {
                        s.preview_form("Join a Room", &["Room address", "Nickname"])
                    }
                });
                body.append(&b)
            }
        }
    }
    pub fn new_group(self: &Rc<Self>) {
        let page = adw::PreferencesPage::new();
        let g = adw::PreferencesGroup::builder()
            .title("Group Details")
            .build();
        let name = adw::EntryRow::builder().title("Group name").build();
        g.add(&name);
        page.add(&g);
        let people = adw::PreferencesGroup::builder()
            .title("Choose People")
            .build();
        for c in self
            .state
            .borrow()
            .chats
            .iter()
            .filter(|c| !c.group && c.id != "saved")
        {
            let check = gtk::CheckButton::new();
            let row = adw::ActionRow::builder()
                .title(&c.name)
                .activatable_widget(&check)
                .build();
            row.add_prefix(&avatar(c, 32));
            row.add_suffix(&check);
            people.add(&row)
        }
        page.add(&people);
        let (d, h) = self.dialog("New Group", &page, 480, 560);
        let create = gtk::Button::with_label("Create Preview Group");
        create.add_css_class("suggested-action");
        h.pack_end(&create);
        let weak = Rc::downgrade(self);
        create.connect_clicked(move |_| {
            if let Some(ui) = weak.upgrade() {
                let title = name.text().trim().to_string();
                if title.is_empty() {
                    return;
                }
                let id = new_id();
                ui.state.borrow_mut().chats.push(Chat {
                    id: id.clone(),
                    name: title,
                    subtitle: "3 members".into(),
                    group: true,
                    color: "purple".into(),
                    notifications: "all".into(),
                    ..Default::default()
                });
                ui.select_chat(&id);
                d.close();
            }
        });
    }
    pub fn requests(self: &Rc<Self>) {
        let owner = self.state.borrow().account.clone();
        let body = bx(true, 18);
        margins(&body, 24);
        body.append(&wrap(
            "Choose who may contact you and see your presence.",
            "",
        ));
        for (name, description) in self.state.borrow().requests.clone() {
            let g = adw::PreferencesGroup::new();
            let row = adw::ActionRow::builder()
                .title(&name)
                .subtitle(&description)
                .use_markup(false)
                .build();
            let actions = bx(false, 6);
            actions.set_valign(gtk::Align::Center);
            for (title, allow) in [("Decline", false), ("Accept", true)] {
                let b = gtk::Button::with_label(title);
                if allow {
                    b.add_css_class("suggested-action")
                }
                let weak = Rc::downgrade(self);
                let name = name.clone();
                let description = description.clone();
                let owner = owner.clone();
                let group = g.clone();
                b.connect_clicked(move |_| {
                    if let Some(ui) = weak.upgrade() {
                        if ui.state.borrow().account != owner {
                            return;
                        }
                        if !owner.is_empty() {
                            if !ui.online.get() {
                                ui.notice("Reconnect to respond to this request");
                                return;
                            }
                            ui.command(protocol::Command::Authorize {
                                peer: name.clone(),
                                allow,
                            });
                        }
                        ui.state
                            .borrow_mut()
                            .requests
                            .retain(|item| item != &(name.clone(), description.clone()));
                        group.set_visible(false);
                        ui.request_badge
                            .set_text(&ui.state.borrow().requests.len().to_string());
                        ui.request_badge
                            .set_visible(!ui.state.borrow().requests.is_empty());
                    }
                });
                actions.append(&b)
            }
            row.add_suffix(&actions);
            g.add(&row);
            body.append(&g)
        }
        self.dialog("Requests and Invitations", &scroll(&body), 520, 480);
    }
    pub fn contacts(self: &Rc<Self>) {
        let page = adw::PreferencesPage::new();
        let g = adw::PreferencesGroup::builder().title("Contacts").build();
        for c in self.state.borrow().chats.clone() {
            if c.group || c.id == "saved" {
                continue;
            }
            let row = adw::ActionRow::builder()
                .title(&c.name)
                .subtitle(if c.jid.is_empty() {
                    &c.subtitle
                } else {
                    &c.jid
                })
                .use_markup(false)
                .activatable(true)
                .build();
            row.add_prefix(&avatar(&c, 36));
            let weak = Rc::downgrade(self);
            row.connect_activated(move |_| {
                if let Some(ui) = weak.upgrade() {
                    ui.select_chat(&c.id)
                }
            });
            g.add(&row)
        }
        page.add(&g);
        self.show_page("Contacts", &page);
    }
    pub fn archives(self: &Rc<Self>) {
        let page = adw::PreferencesPage::new();
        let g = adw::PreferencesGroup::builder()
            .title("Archived Chats")
            .build();
        let mut count = 0;
        for c in self.state.borrow().chats.clone() {
            if !c.archived {
                continue;
            }
            count += 1;
            let row = adw::ActionRow::builder()
                .title(&c.name)
                .use_markup(false)
                .activatable(true)
                .build();
            row.add_prefix(&avatar(&c, 36));
            let id = c.id.clone();
            let weak = Rc::downgrade(self);
            row.connect_activated(move |_| {
                if let Some(ui) = weak.upgrade() {
                    ui.select_chat(&id)
                }
            });
            let unarchive = icon_button("folder-open-symbolic", "Unarchive");
            let id = c.id;
            self.weak_button(&unarchive, move |s| {
                if let Some(c) = s.state.borrow_mut().chats.iter_mut().find(|c| c.id == id) {
                    c.archived = false;
                }
                s.persist();
                s.refresh_chats(None);
                s.archives();
            });
            row.add_suffix(&unarchive);
            g.add(&row)
        }
        page.add(&g);
        if count == 0 {
            self.show_page(
                "Archived Chats",
                &adw::StatusPage::builder()
                    .title("No Archived Chats")
                    .icon_name("folder-symbolic")
                    .build(),
            )
        } else {
            self.show_page("Archived Chats", &page)
        }
    }
    pub fn security(self: &Rc<Self>) {
        let page = adw::PreferencesPage::new();
        let live = !self.state.borrow().account.is_empty();
        let g=adw::PreferencesGroup::builder().title("Encryption").description(if live{"OMEMO is not implemented in this desktop build. Live messages use verified TLS transport and are not end-to-end encrypted."}else{"Preview controls only. No real keys are loaded or changed."}).build();
        if live {
            g.add(
                &adw::ActionRow::builder()
                    .title("Connection")
                    .subtitle(&self.status.borrow().clone())
                    .build(),
            )
        } else {
            let mode = adw::ComboRow::builder()
                .title("Encryption mode")
                .model(&gtk::StringList::new(&[
                    "PQ OMEMO2",
                    "Legacy OMEMO",
                    "Unencrypted",
                ]))
                .build();
            let selected = ["PQ OMEMO2", "Legacy OMEMO", "Unencrypted"]
                .iter()
                .position(|m| m == &self.state.borrow().mode)
                .unwrap_or(0);
            mode.set_selected(selected as u32);
            let weak = Rc::downgrade(self);
            mode.connect_selected_notify(move |r| {
                if let Some(ui) = weak.upgrade() {
                    ui.state.borrow_mut().mode =
                        ["PQ OMEMO2", "Legacy OMEMO", "Unencrypted"][r.selected() as usize].into();
                }
            });
            g.add(&mode);
            g.add(
                &adw::SwitchRow::builder()
                    .title("Trust new fingerprints automatically")
                    .subtitle("Fictional devices only")
                    .build(),
            );
        }
        page.add(&g);
        if !live {
            let devices = adw::PreferencesGroup::builder()
                .title("Devices and Fingerprints")
                .build();
            for (name, status) in [
                ("Mira · Laptop", "Verified"),
                ("Alex · Phone", "Needs review"),
                ("Your account · Other device", "Needs review"),
            ] {
                let row = adw::ActionRow::builder()
                    .title(name)
                    .subtitle(status)
                    .activatable(true)
                    .build();
                let weak = Rc::downgrade(self);
                row.connect_activated(move |_| {
                    if let Some(ui) = weak.upgrade() {
                        let d = adw::AlertDialog::builder()
                            .heading(name)
                            .body("Example fingerprint\nA12B C34D E56F 7890\n1234 5678 90AB CDEF")
                            .build();
                        d.add_responses(&[
                            ("cancel", "Cancel"),
                            ("block", "Block"),
                            ("trust", "Trust"),
                        ]);
                        d.present(Some(&ui.window));
                    }
                });
                devices.add(&row)
            }
            page.add(&devices)
        }
        self.dialog("Conversation Security", &page, 560, 480);
    }
    pub fn preferences(self: &Rc<Self>) {
        let dialog = adw::PreferencesDialog::builder()
            .title("Preferences")
            .search_enabled(true)
            .build();
        let general = adw::PreferencesPage::builder()
            .title("General")
            .icon_name("preferences-system-symbolic")
            .build();
        let appearance = adw::PreferencesGroup::builder().title("Appearance").build();
        let theme = adw::ComboRow::builder()
            .title("Style")
            .model(&gtk::StringList::new(&["System", "Light", "Dark"]))
            .build();
        theme.set_selected(match self.store.pref("style", "system").as_str() {
            "light" => 1,
            "dark" => 2,
            _ => 0,
        });
        let weak = Rc::downgrade(self);
        theme.connect_selected_notify(move |r| {
            if let Some(ui) = weak.upgrade() {
                let index = r.selected() as usize;
                adw::StyleManager::default().set_color_scheme(
                    [
                        adw::ColorScheme::Default,
                        adw::ColorScheme::ForceLight,
                        adw::ColorScheme::ForceDark,
                    ][index],
                );
                let _ = ui
                    .store
                    .set_pref("style", ["system", "light", "dark"][index]);
            }
        });
        appearance.add(&theme);
        let emoji = adw::SwitchRow::builder()
            .title("Large emoji")
            .subtitle("Enlarge messages containing only emoji")
            .active(self.big_emoji.get())
            .build();
        let weak = Rc::downgrade(self);
        emoji.connect_active_notify(move |r| {
            if let Some(ui) = weak.upgrade() {
                ui.big_emoji.set(r.is_active());
                let _ = ui
                    .store
                    .set_pref("big_emoji", if r.is_active() { "true" } else { "false" });
                ui.render_messages(false);
            }
        });
        appearance.add(&emoji);
        general.add(&appearance);
        let owner = self.state.borrow().account.clone();
        let privacy = adw::PreferencesGroup::builder()
            .title("Privacy and Notifications")
            .build();
        for (key, title, description, default) in [
            (
                "typing",
                "Share typing status",
                "Let the other person know when composing",
                true,
            ),
            (
                "delivery_receipts",
                "Send delivery receipts",
                "Receipt by this client; never a read receipt",
                false,
            ),
            (
                "notifications",
                "Desktop notifications",
                "Notify about incoming messages",
                true,
            ),
            (
                "message_previews",
                "Message previews",
                "Show text in desktop notifications",
                false,
            ),
        ] {
            let row = adw::SwitchRow::builder()
                .title(title)
                .subtitle(description)
                .active(self.preference(key, default))
                .build();
            let weak = Rc::downgrade(self);
            let owner = owner.clone();
            row.connect_active_notify(move |r| {
                if let Some(ui) = weak.upgrade() {
                    let _ = ui.store.set_pref(
                        &format!("{key}:{owner}"),
                        if r.is_active() { "true" } else { "false" },
                    );
                }
            });
            privacy.add(&row)
        }
        general.add(&privacy);
        dialog.add(&general);
        let account = adw::PreferencesPage::builder()
            .title("Account")
            .icon_name("avatar-default-symbolic")
            .build();
        let g = adw::PreferencesGroup::new();
        for (title, action) in [
            ("Accounts", "accounts"),
            ("Encryption and Devices", "security"),
        ] {
            let row = adw::ActionRow::builder()
                .title(title)
                .activatable(true)
                .build();
            let weak = Rc::downgrade(self);
            row.connect_activated(move |_| {
                if let Some(ui) = weak.upgrade() {
                    if action == "accounts" {
                        ui.accounts_dialog()
                    } else {
                        ui.security()
                    }
                }
            });
            g.add(&row)
        }
        account.add(&g);
        dialog.add(&account);
        if owner.is_empty() {
            for (title, icon, groups) in [
                (
                    "Privacy",
                    "channel-secure-symbolic",
                    vec![
                        (
                            "Privacy and Security",
                            vec![
                                "Send read receipts",
                                "Sync read state between devices",
                                "Show presence to contacts",
                            ],
                        ),
                        (
                            "Media",
                            vec![
                                "Load link previews",
                                "Load location previews",
                                "Download photos automatically",
                            ],
                        ),
                        (
                            "Storage",
                            vec![
                                "Keep local conversation history",
                                "Reduce background activity",
                            ],
                        ),
                    ],
                ),
                (
                    "Chat",
                    "mail-message-new-symbolic",
                    vec![
                        (
                            "Chat behavior",
                            vec![
                                "Reply on double click",
                                "Show message details",
                                "Pause media when minimized",
                                "Remember drafts",
                            ],
                        ),
                        (
                            "Messages",
                            vec![
                                "Render Markdown articles",
                                "Show partial spoilers",
                                "Translate selected text",
                            ],
                        ),
                        (
                            "Animation",
                            vec![
                                "Animate stickers",
                                "Animate emoji",
                                "Play GIFs automatically",
                            ],
                        ),
                    ],
                ),
            ] {
                let p = adw::PreferencesPage::builder()
                    .title(title)
                    .icon_name(icon)
                    .build();
                for (name, rows) in groups {
                    let g = adw::PreferencesGroup::builder()
                        .title(name)
                        .description("Offline preview settings")
                        .build();
                    for title in rows {
                        g.add(&adw::SwitchRow::builder().title(title).active(true).build())
                    }
                    p.add(&g)
                }
                dialog.add(&p)
            }
        }
        self.dialogs.borrow_mut().push(dialog.clone().upcast());
        dialog.present(Some(&self.window));
    }
    pub fn shortcuts(self: &Rc<Self>) {
        let d = adw::ShortcutsDialog::new();
        for (title, items) in [
            (
                "Conversations",
                vec![
                    ("New conversation", "<Control>n"),
                    ("Search", "<Control>f"),
                    ("Attach files", "<Control>o"),
                ],
            ),
            (
                "View",
                vec![("Media sidebar", "F9"), ("Preferences", "<Control>comma")],
            ),
            (
                "Messages",
                vec![("Send", "Return"), ("New line", "<Shift>Return")],
            ),
        ] {
            let section = adw::ShortcutsSection::new(Some(title));
            for (name, key) in items {
                section.add(adw::ShortcutsItem::new(name, key))
            }
            d.add(section)
        }
        self.dialogs.borrow_mut().push(d.clone().upcast());
        d.present(Some(&self.window));
    }
    pub fn preview_form(self: &Rc<Self>, title: &str, fields: &[&str]) {
        let page = adw::PreferencesPage::new();
        let g = adw::PreferencesGroup::builder()
            .title(title)
            .description("Local form preview. Nothing is published or connected.")
            .build();
        for title in fields {
            g.add(&adw::EntryRow::builder().title(*title).build())
        }
        page.add(&g);
        let (d, h) = self.dialog(title, &page, 520, 440);
        let save = gtk::Button::with_label("Save Preview");
        save.add_css_class("suggested-action");
        h.pack_end(&save);
        save.connect_clicked(move |_| {
            d.close();
        });
    }
    pub fn forward(self: &Rc<Self>, messages: Vec<Message>) {
        if messages.is_empty() {
            return;
        }
        let owner = self.state.borrow().account.clone();
        if !owner.is_empty() && messages.iter().any(|m| m.kind != "text" || m.unavailable) {
            self.notice("Only decrypted text can be forwarded in this build");
            return;
        }
        let page = adw::PreferencesPage::new();
        let g = adw::PreferencesGroup::builder()
            .title("Choose a Conversation")
            .build();
        page.add(&g);
        let (d, _) = self.dialog("Forward Messages", &page, 520, 480);
        for c in self.state.borrow().chats.clone() {
            let row = adw::ActionRow::builder()
                .title(&c.name)
                .use_markup(false)
                .activatable(true)
                .build();
            row.add_prefix(&avatar(&c, 32));
            let messages = messages.clone();
            let owner = owner.clone();
            let weak = Rc::downgrade(self);
            let d = d.clone();
            row.connect_activated(move |_| {
                if let Some(ui) = weak.upgrade() {
                    if ui.state.borrow().account != owner {
                        return;
                    }
                    ui.select_chat(&c.id);
                    for m in &messages {
                        if let Some(id) = ui.send_content(
                            &format!("↪ Forwarded from {}\n\n{}", m.author, m.text),
                            &m.kind,
                        ) {
                            if let Some(sent) = ui
                                .state
                                .borrow_mut()
                                .current_mut()
                                .messages
                                .iter_mut()
                                .find(|m| m.id == id)
                            {
                                sent.options = m.options.clone();
                                sent.gif_index = m.gif_index
                            }
                        } else {
                            return;
                        }
                    }
                    ui.render_messages(true);
                    d.close();
                }
            });
            g.add(&row)
        }
    }
    pub fn message_details(self: &Rc<Self>, m: &Message) {
        let page = adw::PreferencesPage::new();
        let g = adw::PreferencesGroup::builder().title("Message").build();
        for (title, value) in [
            ("Sender", m.author.clone()),
            (
                "Time",
                if m.timestamp.is_empty() {
                    m.time.clone()
                } else {
                    m.timestamp.clone()
                },
            ),
            (
                "Encryption",
                if m.unavailable {
                    "Decryption is not available"
                } else if m.encrypted {
                    "Encrypted fixture"
                } else {
                    "Not end-to-end encrypted"
                }
                .into(),
            ),
            ("Delivery", m.delivery.clone()),
            ("Message ID", m.wire_id.clone()),
        ] {
            g.add(
                &adw::ActionRow::builder()
                    .title(title)
                    .subtitle(value)
                    .use_markup(false)
                    .build(),
            )
        }
        page.add(&g);
        self.dialog("Message Details", &page, 520, 440);
    }
    pub fn retry(self: &Rc<Self>, id: &str) {
        if !self.online.get() {
            self.notice("Reconnect before retrying");
            return;
        }
        let owner = self.state.borrow().account.clone();
        let chat = self.state.borrow().active.clone();
        let id = id.to_string();
        let d=adw::AlertDialog::builder().heading("Retry Message?").body("It may already have been received. Retrying can create a duplicate on clients that do not deduplicate IDs.").build();
        d.add_responses(&[("cancel", "Cancel"), ("retry", "Retry")]);
        let weak = Rc::downgrade(self);
        d.connect_response(None, move |_, r| {
            if r == "retry" {
                if let Some(ui) = weak.upgrade() {
                    if ui.state.borrow().account != owner || ui.state.borrow().active != chat {
                        return;
                    }
                    let mut s = ui.state.borrow_mut();
                    let peer = s.current().jid.clone();
                    if let Some(m) = s.current_mut().messages.iter_mut().find(|m| m.id == id) {
                        m.delivery = "sending".into();
                        let message = m.clone();
                        drop(s);
                        if ui.persist() {
                            ui.command(protocol::Command::Send {
                                peer,
                                message,
                                quote: String::new(),
                            });
                        }
                    }
                }
            }
        });
        d.present(Some(&self.window));
    }
    pub fn new_poll(self: &Rc<Self>) {
        let page = adw::PreferencesPage::new();
        let question = adw::EntryRow::builder().title("Question").build();
        let g = adw::PreferencesGroup::new();
        g.add(&question);
        page.add(&g);
        let answers = adw::PreferencesGroup::builder().title("Options").build();
        let fields: Vec<_> = ["First option", "Second option", "Third option"]
            .into_iter()
            .map(|t| {
                let r = adw::EntryRow::builder().title(t).build();
                answers.add(&r);
                r
            })
            .collect();
        page.add(&answers);
        let (d, h) = self.dialog("New Poll", &page, 520, 440);
        let send = gtk::Button::with_label("Create Preview Poll");
        send.add_css_class("suggested-action");
        h.pack_end(&send);
        let weak = Rc::downgrade(self);
        send.connect_clicked(move |_| {
            if let Some(ui) = weak.upgrade() {
                let q = question.text();
                let options: Vec<_> = fields
                    .iter()
                    .map(|r| r.text().trim().to_string())
                    .filter(|s| !s.is_empty())
                    .map(|s| (s, 0))
                    .collect();
                if q.trim().is_empty() || options.len() < 2 {
                    ui.notice("Enter a question and at least two options");
                    return;
                }
                if let Some(id) = ui.send_content(q.as_str(), "poll") {
                    if let Some(m) = ui
                        .state
                        .borrow_mut()
                        .current_mut()
                        .messages
                        .iter_mut()
                        .find(|m| m.id == id)
                    {
                        m.options = options
                    }
                    ui.render_messages(true);
                    d.close();
                }
            }
        });
    }
    pub fn article(self: &Rc<Self>) {
        let body = bx(true, 18);
        margins(&body, 24);
        let editor = gtk::TextView::builder()
            .wrap_mode(gtk::WrapMode::WordChar)
            .vexpand(true)
            .build();
        editor.buffer().set_text("# A little room to write\n\nUse Markdown for headings, lists, tables and quotes.\n\n| Item | Count |\n| --- | --- |\n| Ideas | 3 |");
        let scroll = scroll(&editor);
        scroll.set_min_content_height(280);
        body.append(&scroll);
        let (d, h) = self.dialog("Write an Article", &body, 640, 500);
        let b = gtk::Button::with_label("Add Preview Article");
        b.add_css_class("suggested-action");
        h.pack_end(&b);
        let weak = Rc::downgrade(self);
        b.connect_clicked(move |_| {
            if let Some(ui) = weak.upgrade() {
                let buffer = editor.buffer();
                let value = buffer.text(&buffer.start_iter(), &buffer.end_iter(), true);
                if ui.send_content(value.as_str(), "article").is_some() {
                    d.close();
                }
            }
        });
    }
    pub fn choose_files(self: &Rc<Self>, media: bool) {
        if !self.state.borrow().account.is_empty() {
            self.notice("Live file transfers are not available yet");
            return;
        }
        let owner = self.state.borrow().account.clone();
        let chat = self.state.borrow().active.clone();
        let chooser = gtk::FileDialog::builder()
            .title(if media {
                "Attach Photos or Videos"
            } else {
                "Attach Files"
            })
            .modal(true)
            .build();
        if media {
            let filter = gtk::FileFilter::new();
            filter.set_name(Some("Photos and videos"));
            filter.add_mime_type("image/*");
            filter.add_mime_type("video/*");
            let filters = gio::ListStore::new::<gtk::FileFilter>();
            filters.append(&filter);
            chooser.set_filters(Some(&filters))
        }
        let weak = Rc::downgrade(self);
        let window = self.window.clone();
        glib::MainContext::default().spawn_local(async move{match chooser.open_multiple_future(Some(&window)).await{Ok(files)=>{let Some(ui)=weak.upgrade()else{return};if ui.state.borrow().account!=owner||ui.state.borrow().active!=chat{ui.notice("Conversation changed. Select the files again in their destination chat.");return}for file in files.iter::<gio::File>().flatten(){if let Some(path)=file.path(){let chip=bx(false,4);chip.add_css_class("file-chip");chip.append(&label(path.file_name().unwrap_or_default().to_string_lossy().as_ref(),""));let remove=icon_button("window-close-symbolic","Remove attachment");let weak=Rc::downgrade(&ui);let c=chip.downgrade();let p=path.clone();remove.connect_clicked(move|_|{if let Some(ui)=weak.upgrade(){ui.attachments.borrow_mut().retain(|v|v!=&p);if let Some(c)=c.upgrade(){ui.chips.remove(&c)}ui.compose_changed();}});chip.append(&remove);ui.chips.append(&chip);ui.attachments.borrow_mut().push(path)}}ui.chips.set_visible(true);ui.compose_changed();ui.show_chat();},Err(e)=>{if !e.matches(gtk::DialogError::Dismissed){if let Some(ui)=weak.upgrade(){ui.notice("File selection is unavailable")}}}}});
    }
    pub fn payment(self: &Rc<Self>, request: bool) {
        let page = adw::PreferencesPage::new();
        let g = adw::PreferencesGroup::builder()
            .title(if request {
                "Request Monero"
            } else {
                "Send Monero"
            })
            .description("Preview only · no real wallet or funds")
            .build();
        let amount = adw::EntryRow::builder()
            .title("Amount")
            .input_purpose(gtk::InputPurpose::Number)
            .build();
        g.add(&amount);
        let currency = adw::ComboRow::builder()
            .title("Currency")
            .model(&gtk::StringList::new(&["XMR", "USD", "EUR", "PLN"]))
            .build();
        g.add(&currency);
        g.add(&adw::EntryRow::builder().title("Note").build());
        if !request {
            g.add(&adw::EntryRow::builder().title("Receiving address").build())
        }
        page.add(&g);
        let (d, h) = self.dialog(
            if request {
                "Request Payment"
            } else {
                "Send Payment"
            },
            &page,
            520,
            440,
        );
        let review = gtk::Button::with_label(if request {
            "Create Preview Request"
        } else {
            "Review Preview Payment"
        });
        review.add_css_class("suggested-action");
        h.pack_end(&review);
        bind_amount(&amount, &review);
        let weak = Rc::downgrade(self);
        review.connect_clicked(move |_| {
            if let Some(ui) = weak.upgrade() {
                if request {
                    ui.send_content(
                        &format!(
                            "{} {}",
                            amount.text(),
                            ["XMR", "USD", "EUR", "PLN"][currency.selected() as usize]
                        ),
                        "payment",
                    );
                } else {
                    ui.notice("Offline payment review — no funds sent")
                }
                d.close();
            }
        });
    }
    pub fn wallet(self: &Rc<Self>) {
        if !self.state.borrow().account.is_empty() {
            self.notice("Live wallet integration is not available yet");
            return;
        }
        let body = bx(true, 18);
        margins(&body, 24);
        let top = bx(false, 8);
        let title = text("Your wallets", "title-2");
        title.set_hexpand(true);
        top.append(&title);
        let add = icon_button("list-add-symbolic", "Add another wallet");
        self.weak_button(&add, |s| s.add_wallet());
        top.append(&add);
        body.append(&top);
        let line = bx(false, 4);
        let previous = icon_button("go-previous-symbolic", "Previous wallet");
        line.append(&previous);
        let stack = gtk::Stack::builder()
            .transition_duration(320)
            .hexpand(true)
            .build();
        let names = self.state.borrow().wallets.clone();
        self.cards.borrow_mut().clear();
        for (i, name) in names.iter().enumerate() {
            let weak = Rc::downgrade(self);
            let n = name.clone();
            let card = WalletCard::new(
                name,
                if i == 0 { "1.2048" } else { "0.4659" },
                false,
                move || {
                    if let Some(ui) = weak.upgrade() {
                        ui.wallet_qr(&n)
                    }
                },
            );
            stack.add_named(&card.root, Some(&i.to_string()));
            self.cards.borrow_mut().push(card);
        }
        stack.set_visible_child_name(&self.state.borrow().wallet_index.to_string());
        line.append(&stack);
        let next = icon_button("go-next-symbolic", "Next wallet");
        line.append(&next);
        for (button, dir) in [(&previous, -1i32), (&next, 1)] {
            let weak = Rc::downgrade(self);
            let stack = stack.clone();
            button.connect_clicked(move |_| {
                if let Some(ui) = weak.upgrade() {
                    let mut s = ui.state.borrow_mut();
                    let n = s.wallets.len() as i32;
                    s.wallet_index = (s.wallet_index as i32 + dir).rem_euclid(n) as usize;
                    stack.set_transition_type(if dir > 0 {
                        gtk::StackTransitionType::SlideLeft
                    } else {
                        gtk::StackTransitionType::SlideRight
                    });
                    stack.set_visible_child_name(&s.wallet_index.to_string());
                }
            });
        }
        body.append(&line);
        let actions = bx(false, 12);
        actions.set_homogeneous(true);
        for (title, action) in [
            ("Send", "send"),
            ("Request", "request"),
            ("Wallets", "wallets"),
        ] {
            let b = gtk::Button::with_label(title);
            self.weak_button(&b, move |s| {
                if action == "wallets" {
                    s.manage_wallets()
                } else {
                    s.payment(action == "request")
                }
            });
            actions.append(&b)
        }
        body.append(&actions);
        let g = adw::PreferencesGroup::builder().title("Activity").build();
        for (t, sub, value) in [
            ("Received", "Mira · Today, 09:42", "+0.0248 XMR"),
            ("Sent", "Everyday → Savings · Yesterday", "−0.1000 XMR"),
        ] {
            let row = adw::ActionRow::builder().title(t).subtitle(sub).build();
            row.add_suffix(&label(value, "heading"));
            g.add(&row)
        }
        body.append(&g);
        body.append(&wrap(
            "Offline wallet preview. No keys, scanning or transactions.",
            "caption dim-label",
        ));
        self.utility.set_show_sidebar(false);
        self.show_page(
            "Wallet",
            &scroll(
                &adw::Clamp::builder()
                    .child(&body)
                    .maximum_size(640)
                    .tightening_threshold(540)
                    .build(),
            ),
        );
    }
    pub fn wallet_qr(self: &Rc<Self>, name: &str) {
        let data = format!("Monogram Desktop wallet preview: {name} (no receiving address)");
        let qr = qrcode::QrCode::new(data.as_bytes()).unwrap();
        let n = qr.width();
        let codes = qr.to_colors();
        let area = gtk::DrawingArea::builder()
            .content_width(280)
            .content_height(280)
            .halign(gtk::Align::Center)
            .build();
        area.set_draw_func(move |_, c, w, h| {
            let cell = (w.min(h) as f64 / (n + 8) as f64).floor();
            let side = cell * (n + 8) as f64;
            c.set_source_rgb(1., 1., 1.);
            c.rectangle((w as f64 - side) / 2., (h as f64 - side) / 2., side, side);
            let _ = c.fill();
            c.set_source_rgb(0., 0., 0.);
            c.set_antialias(gtk::cairo::Antialias::None);
            for y in 0..n {
                for x in 0..n {
                    if codes[y * n + x] == qrcode::Color::Dark {
                        c.rectangle(
                            (w as f64 - side) / 2. + (x + 4) as f64 * cell,
                            (h as f64 - side) / 2. + (y + 4) as f64 * cell,
                            cell,
                            cell,
                        )
                    }
                }
            }
            let _ = c.fill();
        });
        let body = bx(true, 18);
        margins(&body, 24);
        body.append(&area);
        body.append(&label(name, "title-2"));
        body.append(&wrap("Offline preview · no receiving address", "dim-label"));
        self.dialog("Receive Monero", &body, 360, 420);
    }
    pub fn add_wallet(self: &Rc<Self>) {
        let page = adw::PreferencesPage::new();
        let g = adw::PreferencesGroup::builder()
            .title("Another Wallet")
            .build();
        let name = adw::EntryRow::builder().title("Wallet name").build();
        g.add(&name);
        page.add(&g);
        let (d, h) = self.dialog("Add or Create Another Wallet", &page, 520, 320);
        let add = gtk::Button::with_label("Create Preview Wallet");
        add.add_css_class("suggested-action");
        h.pack_end(&add);
        let weak = Rc::downgrade(self);
        add.connect_clicked(move |_| {
            if let Some(ui) = weak.upgrade() {
                let mut s = ui.state.borrow_mut();
                let title = name.text().trim().to_string();
                if title.is_empty() {
                    return;
                }
                s.wallets.push(title);
                s.wallet_index = s.wallets.len() - 1;
                drop(s);
                d.close();
                ui.wallet();
            }
        });
    }
    pub fn manage_wallets(self: &Rc<Self>) {
        let page = adw::PreferencesPage::new();
        let g = adw::PreferencesGroup::builder()
            .title("Wallets")
            .description("Fictional wallets — no real keys or funds")
            .build();
        for name in self.state.borrow().wallets.clone() {
            let row = adw::ActionRow::builder()
                .title(&name)
                .use_markup(false)
                .build();
            let remove = icon_button("user-trash-symbolic", "Remove wallet");
            let row_weak = row.downgrade();
            self.weak_button(&remove, move |s| {
                let mut state = s.state.borrow_mut();
                if state.wallets.len() == 1 {
                    drop(state);
                    s.notice("Keep one wallet in this preview");
                    return;
                }
                state.wallets.retain(|n| n != &name);
                state.wallet_index = 0;
                drop(state);
                if let Some(row) = row_weak.upgrade() {
                    row.set_visible(false)
                }
                s.wallet();
            });
            row.add_suffix(&remove);
            g.add(&row)
        }
        page.add(&g);
        self.dialog("Manage Wallets", &page, 520, 440);
    }
    pub fn call_preview(self: &Rc<Self>) {
        let body = bx(true, 18);
        margins(&body, 24);
        let chat = self.state.borrow().current().clone();
        body.append(&avatar(&chat, 88));
        body.append(&label(&chat.name, "title-1"));
        body.append(&wrap("Call preview · no call will be placed", "dim-label"));
        let controls = bx(false, 16);
        controls.set_halign(gtk::Align::Center);
        for (icon, title) in [
            ("microphone-sensitivity-high-symbolic", "Microphone"),
            ("camera-web-symbolic", "Camera"),
            ("audio-volume-high-symbolic", "Speaker"),
        ] {
            let b = gtk::ToggleButton::builder()
                .icon_name(icon)
                .tooltip_text(title)
                .active(true)
                .build();
            b.add_css_class("circular");
            controls.append(&b)
        }
        body.append(&controls);
        self.dialog(
            if chat.group { "Group Call" } else { "Call" },
            &body,
            520,
            420,
        );
    }
}
use std::collections::HashSet;
pub fn valid_amount(value: &str) -> bool {
    if !value.is_ascii() || value.len() > 128 {
        return false;
    }
    let mut point = false;
    let mut digits = 0;
    for c in value.chars() {
        if c == '.' || c == ',' {
            if point {
                return false;
            }
            point = true;
        } else if c.is_ascii_digit() {
            if point {
                digits += 1;
                if digits > 12 {
                    return false;
                }
            }
        } else {
            return false;
        }
    }
    true
}
pub fn positive_amount(value: &str) -> bool {
    valid_amount(value) && value.chars().any(|c| matches!(c, '1'..='9'))
}
pub fn bind_amount(entry: &impl IsA<gtk::Editable>, review: &gtk::Button) {
    let previous = Rc::new(RefCell::new(String::new()));
    let changing = Rc::new(Cell::new(false));
    let review = review.clone();
    review.set_sensitive(false);
    entry.connect_changed(move |e| {
        if changing.get() {
            return;
        }
        let value = e.text();
        if !valid_amount(value.as_str()) {
            changing.set(true);
            e.set_text(&previous.borrow());
            changing.set(false);
        } else {
            previous.replace(value.to_string());
            review.set_sensitive(positive_amount(value.as_str()));
        }
    });
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exact_amount_edits() {
        for v in ["", ".", "0,", "1", "0.000000000001"] {
            assert!(valid_amount(v))
        }
        for v in ["hello", "1e3", "-1", "1.2.3", "١٢", "0.0000000000001"] {
            assert!(!valid_amount(v))
        }
        assert!(!positive_amount("0.0"));
        assert!(positive_amount("0.1"));
    }
}
