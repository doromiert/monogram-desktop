use crate::{
    encryption_policy::{Mode, Preference},
    model::*,
    protocol,
    ui::Ui,
    wallet::WalletCard,
    widgets::*,
};
use adw::prelude::*;
use gtk::{gio, glib};
use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    rc::Rc,
};
fn set_switch(row: &adw::SwitchRow, changing: &Cell<bool>, active: bool) {
    changing.set(true);
    row.set_active(active);
    changing.set(false);
}
fn grouped_fingerprint(value: &str) -> String {
    value
        .as_bytes()
        .chunks(8)
        .enumerate()
        .map(|(index, chunk)| {
            let chunk = std::str::from_utf8(chunk).unwrap_or("");
            match index {
                0 => chunk.into(),
                index if index % 4 == 0 => format!("\n{chunk}"),
                _ => format!(" {chunk}"),
            }
        })
        .collect()
}
pub struct SecurityControls {
    generation: u64,
    room: bool,
    pub(crate) sending: adw::ComboRow,
    pub(crate) pq: DeviceControls,
    pub(crate) modern: DeviceControls,
    legacy: DeviceControls,
}
pub(crate) struct DeviceControls {
    row: adw::ActionRow,
    pub(crate) enable: gtk::Button,
    pub(crate) review: gtk::Button,
    fingerprint: gtk::Label,
}
impl Ui {
    pub fn update_crypto_controls(&self) {
        let widgets = self.security_widgets.borrow();
        let Some(w) = widgets.as_ref() else { return };
        if w.generation != self.generation.get() {
            return;
        }
        for (controls, status, supported) in [
            (&w.pq, self.pq_crypto.borrow(), true),
            (&w.modern, self.crypto.borrow(), !w.room),
            (&w.legacy, self.legacy_crypto.borrow(), true),
        ] {
            controls.row.set_subtitle(&status.text);
            controls
                .enable
                .set_label(if status.enabled { "Retry" } else { "Enable" });
            controls.enable.set_visible(!status.ready);
            controls
                .enable
                .set_sensitive(self.online.get() && !status.ready);
            controls
                .review
                .set_sensitive(supported && status.ready && self.online.get());
            let fingerprint = if status.fingerprint.is_empty() {
                "No local identity yet".into()
            } else {
                format!(
                    "Device {}\n{}",
                    status.device,
                    grouped_fingerprint(&status.fingerprint)
                )
            };
            controls.fingerprint.set_text(&fingerprint);
        }
    }
    fn fingerprint_group(
        self: &Rc<Self>,
        mode: Mode,
        peer: &str,
    ) -> (adw::PreferencesGroup, DeviceControls) {
        use crate::crypto::{legacy::service::Request as Legacy, service::Request as Modern};
        let group = adw::PreferencesGroup::builder().title(mode.name()).build();
        if mode == Mode::Omemo2 {
            group.set_description(Some(
                "Incoming Classic OMEMO2 messages and fingerprints remain available independently of the sending choice",
            ));
        }
        let row = adw::ActionRow::builder()
            .title("This Device")
            .use_markup(false)
            .build();
        let enable = gtk::Button::with_label("Enable");
        enable.set_valign(gtk::Align::Center);
        row.add_suffix(&enable);
        group.add(&row);
        let identity = adw::ExpanderRow::builder()
            .title("Local Fingerprint")
            .build();
        let fingerprint = wrap("No local identity yet", "monospace");
        fingerprint.set_selectable(true);
        margins(&fingerprint, 12);
        identity.add_row(&fingerprint);
        group.add(&identity);
        let devices = adw::ActionRow::builder()
            .title("Devices and Fingerprints")
            .subtitle(peer)
            .use_markup(false)
            .build();
        let review = gtk::Button::with_label("Review");
        review.set_valign(gtk::Align::Center);
        devices.add_suffix(&review);
        group.add(&devices);
        let generation = self.generation.get();
        let weak = Rc::downgrade(self);
        enable.connect_clicked(move |button| {
            if let Some(ui) = weak
                .upgrade()
                .filter(|ui| ui.generation.get() == generation)
            {
                button.set_sensitive(false);
                if mode == Mode::Pq {
                    button.set_child(Some(&gtk::Spinner::builder().spinning(true).build()));
                    button.set_tooltip_text(Some("Preparing OMEMO2…"));
                }
                ui.command(match mode {
                    Mode::Legacy => protocol::Command::Legacy(Legacy::Enable),
                    Mode::Omemo2 => protocol::Command::Crypto(Modern::Enable),
                    Mode::Pq => {
                        protocol::Command::PqCrypto(crate::crypto::pq::service::Request::Enable)
                    }
                    Mode::Plain => return,
                });
            }
        });
        let weak = Rc::downgrade(self);
        let target = peer.to_string();
        let room = self.state.borrow().current().group;
        review.connect_clicked(move |_| {
            if let Some(ui) = weak
                .upgrade()
                .filter(|ui| ui.generation.get() == generation)
            {
                ui.review_request.replace(Some((target.clone(), mode)));
                ui.notice(&format!("Looking up {} fingerprints…", mode.name()));
                ui.command(match mode {
                    Mode::Legacy if room => protocol::Command::ReviewRoom {
                        peer: target.clone(),
                    },
                    Mode::Legacy => protocol::Command::Legacy(Legacy::Review(target.clone())),
                    Mode::Omemo2 => protocol::Command::Crypto(Modern::Review(target.clone())),
                    Mode::Pq => protocol::Command::PqCrypto(
                        crate::crypto::pq::service::Request::Review(target.clone()),
                    ),
                    Mode::Plain => return,
                });
            }
        });
        (
            group,
            DeviceControls {
                row,
                enable,
                review,
                fingerprint,
            },
        )
    }
    pub fn live_security(self: &Rc<Self>) {
        let page = adw::PreferencesPage::new();
        let room = self.state.borrow().current().group;
        let peer = self.state.borrow().current().jid.clone();
        let generation = self.generation.get();
        let choices = adw::PreferencesGroup::builder()
            .title("This Conversation")
            .build();
        let sending = adw::ComboRow::builder()
            .title("Sending Encryption")
            .subtitle("Incoming messages use the version they were sent with")
            .model(&gtk::StringList::new(&[
                "Automatic",
                "OMEMO2",
                "Legacy OMEMO",
                "Unencrypted",
            ]))
            .build();
        let preference = self.state.borrow().current().encryption_preference();
        sending.set_selected(
            Preference::OPTIONS
                .iter()
                .position(|option| *option == preference)
                .unwrap_or_default() as u32,
        );
        choices.add(&sending);
        let owner = self.state.borrow().account.clone();
        if peer != owner && !self.state.borrow().current().group {
            let contact_access = crate::contact_access::valid_peer(&owner, &peer);
            let trust = adw::SwitchRow::builder()
                .title("Blindly Trust This Contact")
                .subtitle("Approves contact devices in all OMEMO versions; OMEMO2 also includes your own devices for this conversation")
                .active(self.store.blind_trust(&owner, &peer).unwrap_or(false))
                .build();
            let weak = Rc::downgrade(self);
            let target = peer.clone();
            let setting = Rc::new(Cell::new(false));
            trust.connect_active_notify(move |row| {
                if setting.get() { return; }
                let Some(ui) = weak.upgrade().filter(|ui| ui.generation.get() == generation) else { return };
                let Ok(enabled) = ui.store.blind_trust(&owner, &target) else {
                    set_switch(row, &setting, !row.is_active());
                    ui.notice("Could not read the trust setting");
                    return;
                };
                if row.is_active() == enabled { return; }
                if enabled {
                    if !ui.set_blind_trust(&owner, &target, false) {
                        set_switch(row, &setting, true);
                    }
                    return;
                }
                set_switch(row, &setting, false);
                let confirm = adw::AlertDialog::builder()
                    .heading("Enable Blind Trust for This Conversation?")
                    .body(format!("Approve new devices and changed keys for {target} across all OMEMO versions, plus your own OMEMO2 devices used in this conversation, without fingerprint verification. Explicitly blocked devices stay blocked."))
                    .build();
                confirm.add_responses(&[("cancel", "Cancel"), ("enable", "Enable Blind Trust")]);
                confirm.set_default_response(Some("cancel"));
                confirm.set_response_appearance("enable", adw::ResponseAppearance::Destructive);
                let (owner, target, weak, row, setting) = (owner.clone(), target.clone(), weak.clone(), row.downgrade(), setting.clone());
                confirm.connect_response(Some("enable"), move |_, _| {
                    if let (Some(ui), Some(row)) = (weak.upgrade(), row.upgrade()) {
                        if ui.generation.get() == generation && ui.set_blind_trust(&owner, &target, true) {
                            set_switch(&row, &setting, true);
                        }
                    }
                });
                confirm.present(Some(&ui.window));
            });
            choices.add(&trust);
            if contact_access {
                let access = adw::ActionRow::builder()
                    .title("Control Access")
                    .subtitle("Presence subscriptions and contact-only profile data")
                    .activatable(true)
                    .build();
                access.add_prefix(&gtk::Image::from_icon_name("changes-prevent-symbolic"));
                access.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
                let weak = Rc::downgrade(self);
                let target = peer.clone();
                access.connect_activated(move |_| {
                    if let Some(ui) = weak.upgrade() {
                        ui.contact_access(&target);
                    }
                });
                choices.add(&access);
            }
        }
        page.add(&choices);
        let (pq_group, pq) = self.fingerprint_group(Mode::Pq, &peer);
        let (classic_group, modern) = self.fingerprint_group(Mode::Omemo2, &peer);
        let (legacy_group, legacy) = self.fingerprint_group(Mode::Legacy, &peer);
        page.add(&pq_group);
        page.add(&classic_group);
        page.add(&legacy_group);
        let weak = Rc::downgrade(self);
        let target = peer;
        sending.connect_selected_notify(move |row| {
            let Some(ui) = weak
                .upgrade()
                .filter(|ui| ui.generation.get() == generation)
            else {
                return;
            };
            if let Some(preference) = Preference::OPTIONS.get(row.selected() as usize) {
                ui.set_sending_preference(&target, *preference);
            }
        });
        self.security_widgets.replace(Some(SecurityControls {
            generation,
            room,
            sending,
            pq,
            modern,
            legacy,
        }));
        self.update_crypto_controls();
        let (d, _) = self.dialog("Conversation Security", &page, 560, 520);
        let weak = Rc::downgrade(self);
        d.connect_closed(move |_| {
            if let Some(ui) = weak.upgrade() {
                if generation == ui.generation.get() {
                    ui.security_widgets.borrow_mut().take();
                }
            }
        });
    }
    fn set_blind_trust(&self, owner: &str, peer: &str, enabled: bool) -> bool {
        if self.store.set_blind_trust(owner, peer, enabled).is_err() {
            self.notice("Could not save the trust setting");
            return false;
        }
        self.invalidate_sending_choice(peer);
        if enabled {
            self.command(protocol::Command::Legacy(
                crate::crypto::legacy::service::Request::RefreshTrust(peer.into()),
            ));
            self.command(protocol::Command::Crypto(
                crate::crypto::service::Request::RefreshTrust(peer.into()),
            ));
            self.command(protocol::Command::PqCrypto(
                crate::crypto::pq::service::Request::RefreshTrust(peer.into()),
            ));
        }
        true
    }
    pub fn handle_device_review(
        self: &Rc<Self>,
        peer: &str,
        mode: Mode,
        rows: Vec<crate::crypto::service::Review>,
        errors: Vec<String>,
    ) {
        if !self
            .review_request
            .borrow()
            .as_ref()
            .is_some_and(|(target, version)| target == peer && *version == mode)
        {
            return;
        }
        self.review_request.borrow_mut().take();
        use crate::crypto::{
            legacy::service::Request as Legacy, lifecycle::hex, service::Request as Modern,
        };
        use std::collections::BTreeMap;
        let page = adw::PreferencesPage::new();
        let group=adw::PreferencesGroup::builder().title("Devices and Fingerprints").description("Trust only fingerprints you have verified. Decisions apply to exactly the keys shown here; changed keys require a fresh review.").build();
        let mut keys: BTreeMap<String, BTreeMap<u32, [u8; 32]>> = BTreeMap::new();
        for row in rows {
            keys.entry(row.peer.clone())
                .or_default()
                .insert(row.device, row.key);
            let device = adw::ExpanderRow::builder()
                .title(&row.peer)
                .subtitle(format!("Device {} · {}", row.device, row.trust))
                .use_markup(false)
                .expanded(true)
                .build();
            let key = hex(&row.key);
            let label = wrap(
                &key.as_bytes()
                    .chunks(8)
                    .map(|c| std::str::from_utf8(c).unwrap())
                    .collect::<Vec<_>>()
                    .join(" "),
                "monospace",
            );
            label.set_selectable(true);
            margins(&label, 12);
            device.add_row(&label);
            group.add(&device);
        }
        if keys.is_empty() {
            group.add(&adw::ActionRow::builder().title("No Devices Found").build());
        }
        page.add(&group);
        if !errors.is_empty() {
            let group = adw::PreferencesGroup::builder()
                .title("Refresh Errors")
                .build();
            for error in errors {
                group.add(
                    &adw::ActionRow::builder()
                        .title(&error)
                        .use_markup(false)
                        .title_lines(0)
                        .build(),
                );
            }
            page.add(&group);
        }
        let (d, header) = self.dialog(&format!("{} Fingerprints", mode.name()), &page, 600, 600);
        let generation = self.generation.get();
        for (approve, title, style) in [
            (false, "Deny Shown", "destructive-action"),
            (true, "Trust Shown", "suggested-action"),
        ] {
            let button = gtk::Button::with_label(title);
            button.add_css_class(style);
            button.set_sensitive(!keys.is_empty());
            let weak = Rc::downgrade(self);
            let dialog = d.downgrade();
            let keys = keys.clone();
            let target = peer.to_string();
            button.connect_clicked(move |_| {
                if let Some(ui) = weak.upgrade() {
                    if generation == ui.generation.get() {
                        ui.command(if mode == Mode::Legacy {
                            protocol::Command::Legacy(Legacy::Decide {
                                keys: keys.clone(),
                                approve,
                            })
                        } else {
                            protocol::Command::Crypto(Modern::Decide {
                                keys: keys.clone(),
                                approve,
                            })
                        });
                        ui.invalidate_sending_choice(&target);
                        if mode == Mode::Legacy
                            && ui.state.borrow().current().group
                            && ui.state.borrow().current().jid == target
                        {
                            ui.command(protocol::Command::ReviewRoom {
                                peer: target.clone(),
                            });
                        }
                    }
                    if let Some(d) = dialog.upgrade() {
                        d.close();
                    }
                }
            });
            if approve {
                header.pack_end(&button)
            } else {
                header.pack_start(&button)
            }
        }
    }
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
                p.add_css_class("message-menu");
                p.set_parent(&parent);
                p.set_pointing_to(Some(&gtk::gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
                let content = bx(true, 2);
                content.add_css_class("message-menu-panel");
                for (title, key) in &items {
                    let b = gtk::Button::with_label(title);
                    b.add_css_class("flat");
                    b.add_css_class("context-menu-row");
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
            "folder-delete" => {
                let owner = self.state.borrow().account.clone();
                let generation = self.generation.get();
                let name = id.to_owned();
                let dialog = adw::AlertDialog::builder()
                    .heading("Delete Folder?")
                    .body(format!(
                        "Delete “{name}”? Chats in this folder will remain in Monogram."
                    ))
                    .build();
                dialog.add_responses(&[("cancel", "Cancel"), ("delete", "Delete")]);
                dialog.set_default_response(Some("cancel"));
                dialog.set_close_response("cancel");
                dialog.set_response_appearance("delete", adw::ResponseAppearance::Destructive);
                let weak = Rc::downgrade(self);
                dialog.connect_response(Some("delete"), move |_, _| {
                    let Some(ui) = weak.upgrade().filter(|ui| {
                        ui.generation.get() == generation && ui.state.borrow().account == owner
                    }) else {
                        return;
                    };
                    if !ui.delete_custom_folder(&name) {
                        ui.notice("The folder could not be deleted");
                    }
                });
                dialog.present(Some(&self.window));
            }
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
            .description("Saved sign-ins use the system keyring")
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
                    let owner = ui.state.borrow().account.clone();
                    let _ = ui.store.set_pref(&format!("auto-connect:{owner}"), "false");
                    ui.session.borrow_mut().take();
                    ui.generation.set(ui.generation.get() + 1);
                    ui.online.set(false);
                    ui.crypto.borrow_mut().ready = false;
                    ui.update_crypto_controls();
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
                ui.activate_state(state);
                ui.show_main();
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
        let group = adw::PreferencesGroup::new();
        let jid = adw::EntryRow::builder()
            .title("XMPP address")
            .text(&config.jid)
            .build();
        let password = adw::PasswordEntryRow::builder().title("Password").build();
        group.add(&jid);
        group.add(&password);
        page.add(&group);
        let advanced = adw::ExpanderRow::builder()
            .title("Advanced Connection Settings")
            .subtitle("Use DNS discovery by default")
            .expanded(!config.host.is_empty() || config.port != 5222 || config.direct_tls)
            .build();
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
        advanced.add_row(&host);
        advanced.add_row(&port);
        advanced.add_row(&direct);
        let connection = adw::PreferencesGroup::new();
        connection.add(&advanced);
        page.add(&connection);
        let error = wrap("", "error");
        let body = bx(true, 8);
        body.append(&page);
        margins(&error, 12);
        body.append(&error);
        let (d, header) = self.dialog("Sign In to XMPP", &scroll(&body), 500, 440);
        let connect = gtk::Button::with_label("Connect");
        connect.add_css_class("suggested-action");
        header.pack_end(&connect);
        d.set_default_widget(Some(&connect));
        d.set_focus(Some(&jid));
        jid.set_activates_default(true);
        password.set_activates_default(true);
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
        });
    }
    pub fn remove_account(self: &Rc<Self>, owner: &str) {
        let d=adw::AlertDialog::builder().heading("Remove Account?").body("This removes its local history, drafts and cached attachments. Encryption identity and trust are retained for reconnecting safely. Messages stored by the server are unaffected.").build();
        d.add_responses(&[("cancel", "Cancel"), ("remove", "Remove")]);
        d.set_response_appearance("remove", adw::ResponseAppearance::Destructive);
        let owner = owner.to_string();
        let weak = Rc::downgrade(self);
        d.connect_response(None, move |_, response| {
            if response != "remove" {
                return;
            }
            let owner = owner.clone();
            let weak = weak.clone();
            glib::MainContext::default().spawn_local(async move {
                let result = crate::credentials::remove(&owner).await;
                let Some(ui) = weak.upgrade() else { return };
                if let Err(error) = result {
                    ui.notice(&format!("Could not remove saved sign-in. {error}"));
                    return;
                }
                if ui.state.borrow().account == owner {
                    let demo = ui.demo.borrow().clone();
                    ui.activate_state(demo);
                }
                if ui.store.forget(&owner).is_err() {
                    ui.notice("Local account removal failed");
                    return;
                }
                let cleanup_owner = owner.clone();
                let cleanup = gtk::gio::spawn_blocking(move || {
                    (
                        crate::downloads::forget(&cleanup_owner),
                        crate::link_previews::forget(&cleanup_owner),
                    )
                });
                let weak = Rc::downgrade(&ui);
                drop(ui);
                let cleanup = cleanup.await;
                let Some(ui) = weak.upgrade() else { return };
                match cleanup {
                    Ok((attachments, previews)) => {
                        if let Err(reason) = attachments {
                            ui.notice(reason);
                        }
                        if let Err(reason) = previews {
                            ui.notice(reason);
                        }
                    }
                    Err(_) => ui.notice("Local cache removal failed"),
                }
                if !ui.smoke && ui.store.accounts().is_empty() {
                    ui.show_welcome();
                } else {
                    ui.accounts_dialog();
                }
            });
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
        let room_actions = bx(true, 6);
        body.append(&room_actions);
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
            let dialog = d.clone();
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
                            dialog.close();
                        }
                        Err(e) => ui.notice(&e),
                    }
                }
            });
        }
        {
            for (title, action) in [("New Group", "group"), ("Join a Room", "room")] {
                let b = gtk::Button::with_label(title);
                let dialog = d.clone();
                self.weak_button(&b, move |s| {
                    dialog.close();
                    if action == "group" {
                        s.new_group()
                    } else if live {
                        s.join_room_dialog()
                    } else {
                        s.preview_form("Join a Room", &["Room address", "Nickname"])
                    }
                });
                room_actions.append(&b)
            }
        }
    }
    pub fn new_group(self: &Rc<Self>) {
        if !self.state.borrow().account.is_empty() {
            self.create_group_dialog();
            return;
        }
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
        self.requests_and_invitations();
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
        if !self.state.borrow().account.is_empty() {
            self.live_security();
            return;
        }
        let page = adw::PreferencesPage::new();
        let g = adw::PreferencesGroup::builder()
            .title("Encryption")
            .description("Preview controls only. No real keys are loaded or changed.")
            .build();
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
        page.add(&g);
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
        page.add(&devices);
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
        if !owner.is_empty() {
            let read = adw::SwitchRow::builder()
                .title("Send read receipts")
                .subtitle("Tell contacts when you have actually viewed their messages")
                .active(self.read_receipts_enabled())
                .build();
            let spinner = gtk::Spinner::new();
            spinner.set_visible(false);
            spinner.set_tooltip_text(Some("Saving read receipt preference"));
            read.add_suffix(&spinner);
            let changing = Rc::new(Cell::new(false));
            let pending = Rc::new(Cell::new(false));
            let weak = Rc::downgrade(self);
            let account = owner.clone();
            read.connect_active_notify(move |row| {
                if changing.get() || pending.replace(true) {
                    return;
                }
                let Some(ui) = weak
                    .upgrade()
                    .filter(|ui| ui.state.borrow().account == account)
                else {
                    pending.set(false);
                    return;
                };
                if row.is_active() == ui.read_receipts_enabled() {
                    pending.set(false);
                    return;
                }
                row.set_sensitive(false);
                spinner.set_visible(true);
                spinner.set_spinning(true);
                let update = ui.set_read_receipts(row.is_active());
                let weak_ui = Rc::downgrade(&ui);
                let weak_row = row.downgrade();
                let weak_spinner = spinner.downgrade();
                let account = account.clone();
                let changing = changing.clone();
                let pending = pending.clone();
                glib::spawn_future_local(async move {
                    let result = update.await;
                    let ui = weak_ui
                        .upgrade()
                        .filter(|ui| ui.state.borrow().account == account);
                    if let (Some(ui), Some(row)) = (ui.as_ref(), weak_row.upgrade()) {
                        changing.set(true);
                        row.set_active(ui.read_receipts_enabled());
                        row.set_sensitive(true);
                        changing.set(false);
                    }
                    if let Some(spinner) = weak_spinner.upgrade() {
                        spinner.set_spinning(false);
                        spinner.set_visible(false);
                    }
                    pending.set(false);
                    if let (Some(ui), Err(error)) = (ui, result) {
                        ui.notice(&error);
                    }
                });
            });
            privacy.add(&read);
        }
        general.add(&privacy);
        if !owner.is_empty() {
            general.add(&self.link_preview_preferences());
        }
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
        if !owner.is_empty()
            && messages.iter().any(|m| {
                (m.kind != "text" && m.poll.is_none()) || m.unavailable || m.unsupported_attachment
            })
        {
            self.notice("Only decrypted text and polls can be forwarded here. Attachments need a fresh upload.");
            return;
        }
        let page = adw::PreferencesPage::new();
        let source_group = self.state.borrow().current().group;
        let options = adw::PreferencesGroup::new();
        let hide = adw::SwitchRow::builder()
            .title("Hide Sender")
            .subtitle("Send a copy without the original sender and date.")
            .build();
        options.add(&hide);
        page.add(&options);
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
            let hide = hide.clone();
            row.connect_activated(move |_| {
                if let Some(ui) = weak.upgrade() {
                    if ui.state.borrow().account != owner {
                        return;
                    }
                    ui.select_chat(&c.id);
                    let hide_sender = hide.is_active();
                    let messages = messages.clone();
                    let owner = owner.clone();
                    let dialog = d.downgrade();
                    ui.choose_sending(move |ui, mode| {
                        let Some(dialog) = dialog.upgrade() else {
                            return;
                        };
                        if mode == Mode::Legacy && messages.iter().any(|m| m.poll.is_some()) {
                            ui.notice("Structured encrypted polls require OMEMO 2");
                            return;
                        }
                        for m in &messages {
                            let attribution = (!hide_sender)
                                .then(|| crate::forwarding::original(m, &owner, source_group));
                            if let Some(mut poll) = m.poll.clone() {
                                poll.closed = false;
                                if !ui.send_poll_text_attribution(
                                    poll,
                                    m.poll_fallback,
                                    if m.poll_source() == crate::polls::Source::Legacy {
                                        ""
                                    } else {
                                        &m.text
                                    },
                                    attribution,
                                ) {
                                    return;
                                }
                                continue;
                            }
                            if let Some(id) =
                                ui.send_content_attribution(&m.text, &m.kind, attribution)
                            {
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
                        dialog.close();
                    });
                }
            });
            g.add(&row)
        }
    }
    pub fn forwarded_details(self: &Rc<Self>, attribution: &crate::forwarding::Attribution) {
        let page = adw::PreferencesPage::new();
        let group = adw::PreferencesGroup::builder()
            .description("Attribution provided by the person who forwarded this message.")
            .build();
        let row = adw::ActionRow::builder()
            .title("Original Sender")
            .subtitle(&attribution.name)
            .use_markup(false)
            .build();
        let copy = gtk::Button::from_icon_name("edit-copy-symbolic");
        copy.set_tooltip_text(Some("Copy Sender"));
        copy.set_valign(gtk::Align::Center);
        let name = attribution.name.clone();
        let clipboard = self.window.clipboard();
        copy.connect_clicked(move |_| clipboard.set_text(&name));
        row.add_suffix(&copy);
        group.add(&row);
        group.add(
            &adw::ActionRow::builder()
                .title("Original Date")
                .subtitle(if attribution.date == 0 {
                    "Unknown date".into()
                } else {
                    attribution.stamp()
                })
                .use_markup(false)
                .build(),
        );
        page.add(&group);
        self.dialog("Forwarded Message", &page, 480, 300);
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
                    "Not decrypted".into()
                } else if m.encrypted {
                    if self.state.borrow().account.is_empty() {
                        "Encrypted fixture".into()
                    } else if m.own
                        && matches!(m.delivery.as_str(), "sending" | "failed" | "uncertain")
                    {
                        format!("{} selected · delivery not confirmed", m.mode().name())
                    } else {
                        m.mode().name().into()
                    }
                } else {
                    "Unencrypted".into()
                },
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
        if !m.fingerprint.is_empty() {
            g.add(
                &adw::ActionRow::builder()
                    .title("Sender Fingerprint")
                    .subtitle(&m.fingerprint)
                    .use_markup(false)
                    .build(),
            );
        }
        if m.edited {
            for (index, content) in m.edit_history.iter().enumerate() {
                g.add(
                    &adw::ActionRow::builder()
                        .title(if index == 0 {
                            "Original".into()
                        } else {
                            format!("Edit {index}")
                        })
                        .subtitle(content)
                        .use_markup(false)
                        .build(),
                );
            }
        }
        page.add(&g);
        self.dialog("Message Details", &page, 520, 440);
    }
    pub fn retry(self: &Rc<Self>, id: &str) {
        if self.retry_queued_message(id) {
            return;
        }
        if self
            .state
            .borrow()
            .message(id)
            .is_some_and(|m| m.upload.is_some())
        {
            let id = id.to_string();
            self.choose_sending(move |ui, _| {
                let peer = ui.state.borrow().current().jid.clone();
                ui.start_upload(&peer, &id);
            });
            return;
        }
        let state = self.state.borrow();
        let file_mode_changed = state.message(id).is_some_and(|message| {
            message.attachment.is_some()
                && (self.sending_mode(state.current()) != Some(message.mode())
                    || self.fingerprint_for_send(message.mode()).as_deref()
                        != Some(&message.fingerprint))
        });
        drop(state);
        if file_mode_changed {
            self.notice("Encryption changed. Select the file again to send a fresh copy.");
            return;
        }
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
        fn editor(title: &str, page: &adw::PreferencesPage) -> gtk::TextView {
            let group = adw::PreferencesGroup::builder().title(title).build();
            let view = gtk::TextView::builder()
                .wrap_mode(gtk::WrapMode::WordChar)
                .top_margin(10)
                .bottom_margin(10)
                .left_margin(12)
                .right_margin(12)
                .build();
            view.update_property(&[gtk::accessible::Property::Label(title)]);
            let scroller = scroll(&view);
            scroller.set_min_content_height(60);
            scroller.set_max_content_height(120);
            group.add(&scroller);
            page.add(&group);
            view
        }
        fn add_option(fields: &Rc<RefCell<Vec<adw::EntryRow>>>, group: &adw::PreferencesGroup) {
            if fields.borrow().len() >= 32 {
                return;
            }
            let row = adw::EntryRow::builder()
                .title(format!("Option {}", fields.borrow().len() + 1))
                .build();
            let remove = icon_button("user-trash-symbolic", "Remove option");
            remove.add_css_class("flat");
            remove.set_valign(gtk::Align::Center);
            row.add_suffix(&remove);
            let weak = Rc::downgrade(fields);
            let parent = group.downgrade();
            let item = row.downgrade();
            remove.connect_clicked(move |_| {
                if let (Some(fields), Some(group), Some(row)) =
                    (weak.upgrade(), parent.upgrade(), item.upgrade())
                {
                    group.remove(&row);
                    fields.borrow_mut().retain(|r| r != &row);
                    for (i, r) in fields.borrow().iter().enumerate() {
                        r.set_title(&format!("Option {}", i + 1));
                    }
                }
            });
            group.add(&row);
            fields.borrow_mut().push(row);
        }
        let owner = self.state.borrow().account.clone();
        let chat = self.state.borrow().active.clone();
        let generation = self.generation.get();
        let page = adw::PreferencesPage::new();
        let question = editor("Question", &page);
        let description = editor("Description (optional)", &page);
        let options = adw::PreferencesGroup::builder().title("Options").build();
        let fields = Rc::new(RefCell::new(Vec::new()));
        add_option(&fields, &options);
        add_option(&fields, &options);
        page.add(&options);
        let add = gtk::Button::with_label("Add Option");
        let control = adw::PreferencesGroup::new();
        control.add(&add);
        page.add(&control);
        let rows = fields.clone();
        let group = options.downgrade();
        add.connect_clicked(move |_| {
            if let Some(group) = group.upgrade() {
                add_option(&rows, &group);
            }
        });
        let settings = adw::PreferencesGroup::builder()
            .description("Votes are visible to everyone in this conversation.")
            .build();
        let multiple = adw::SwitchRow::builder()
            .title("Multiple Selections")
            .build();
        settings.add(&multiple);
        let fallback = adw::SwitchRow::builder()
            .title("Include Readable Fallback")
            .subtitle("Allows clients without poll support to read the choices.")
            .active(true)
            .build();
        settings.add(&fallback);
        page.add(&settings);
        let (dialog, header) = self.dialog("New Poll", &page, 520, 640);
        let send = gtk::Button::with_label(if owner.is_empty() {
            "Create Preview Poll"
        } else {
            "Create Poll"
        });
        send.add_css_class("suggested-action");
        header.pack_end(&send);
        let weak = Rc::downgrade(self);
        send.connect_clicked(move |_| {
            if let Some(ui) = weak.upgrade() {
                if ui.generation.get() != generation
                    || ui.state.borrow().account != owner
                    || ui.state.borrow().active != chat
                {
                    return;
                }
                let value = |view: &gtk::TextView| {
                    let b = view.buffer();
                    b.text(&b.start_iter(), &b.end_iter(), true).to_string()
                };
                let emoji = [
                    "1️⃣", "2️⃣", "3️⃣", "4️⃣", "5️⃣", "6️⃣", "7️⃣", "8️⃣", "9️⃣", "🔟", "🔴", "🟠", "🟡",
                    "🟢", "🔵", "🟣", "🟤", "⚫", "⚪", "⭐", "🌙", "☀️", "🌈", "⚡", "🔥", "💧",
                    "🌸", "🍀", "🍎", "🍋", "🍇", "🥝",
                ];
                let poll = crate::polls::Poll {
                    question: value(&question),
                    description: value(&description),
                    options: fields
                        .borrow()
                        .iter()
                        .enumerate()
                        .map(|(i, row)| crate::polls::OptionValue {
                            emoji: emoji[i].into(),
                            label: row.text().into(),
                        })
                        .collect(),
                    multiple: multiple.is_active(),
                    closed: false,
                };
                let fallback = fallback.is_active();
                let dialog = dialog.downgrade();
                ui.choose_sending(move |ui, _| {
                    if let Some(dialog) = dialog.upgrade() {
                        if ui.send_poll(poll, fallback) {
                            dialog.close();
                        }
                    }
                });
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
        self.choose_files_in(media, None);
    }
    pub(crate) fn choose_files_in(self: &Rc<Self>, media: bool, folder: Option<&gio::File>) {
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
        chooser.set_initial_folder(folder);
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
        glib::MainContext::default().spawn_local(async move{match chooser.open_multiple_future(Some(&window)).await{Ok(files)=>{let Some(ui)=weak.upgrade()else{return};if ui.state.borrow().account!=owner||ui.state.borrow().active!=chat{ui.notice("Conversation changed. Select the files again in their destination chat.");return}ui.stage_attachment_files(files.iter::<gio::File>().flatten().collect());ui.show_chat();},Err(e)=>{if !e.matches(gtk::DialogError::Dismissed){if let Some(ui)=weak.upgrade(){ui.notice("File selection is unavailable")}}}}});
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
