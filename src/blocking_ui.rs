// SPDX-License-Identifier: GPL-3.0-or-later
use crate::{
    blocking::Snapshot,
    model::Chat,
    protocol::Command,
    ui::Ui,
    widgets::{bx, clear, clear_list, icon_button, label},
};
use adw::prelude::*;
use gtk::{glib, prelude::*};
use std::{cell::Cell, rc::Rc};

pub struct ListControls {
    owner: String,
    generation: u64,
    body: glib::WeakRef<gtk::Box>,
    search: glib::WeakRef<gtk::SearchEntry>,
    block: glib::WeakRef<adw::ActionRow>,
    dialog: glib::WeakRef<adw::Dialog>,
}

impl Ui {
    pub fn load_blocking_cache(self: &Rc<Self>) {
        self.blocking_snapshot.replace(Snapshot::default());
        self.blocking_loading.set(false);
        self.blocking_error.borrow_mut().take();
        if let Some(dialog) = self
            .blocking_list
            .borrow_mut()
            .take()
            .and_then(|controls| controls.dialog.upgrade())
        {
            dialog.close();
        }
        let owner = self.state.borrow().account.clone();
        if owner.is_empty() {
            return;
        }
        let generation = self.generation.get();
        let account = owner.clone();
        let weak = Rc::downgrade(self);
        glib::MainContext::default().spawn_local(async move {
            let cached = self::Ui::blocking_cache_read(&weak, account).await;
            let Some(ui) = weak.upgrade().filter(|ui| {
                ui.generation.get() == generation && ui.state.borrow().account == owner
            }) else {
                return;
            };
            if ui.blocking_snapshot.borrow().jids.is_none() {
                if let Ok(Some(jids)) = cached {
                    ui.blocking_snapshot.replace(Snapshot {
                        jids: Some(jids),
                        fresh: false,
                        supported: None,
                    });
                    ui.refresh_blocking_ui();
                }
            }
        });
    }

    async fn blocking_cache_read(
        weak: &std::rc::Weak<Self>,
        owner: String,
    ) -> rusqlite::Result<Option<Vec<String>>> {
        let Some(ui) = weak.upgrade() else {
            return Ok(None);
        };
        ui.store
            .read_snapshot(move |store| store.cached_blocked(&owner))
            .await
    }

    pub fn blocked_contacts(self: &Rc<Self>) {
        let owner = self.state.borrow().account.clone();
        if owner.is_empty() {
            return;
        }
        let root = bx(true, 0);
        let actions = adw::PreferencesGroup::new();
        actions.set_margin_top(12);
        actions.set_margin_start(12);
        actions.set_margin_end(12);
        let block = adw::ActionRow::builder()
            .title("Block Contact")
            .subtitle("Choose a contact or enter an XMPP address")
            .activatable(true)
            .build();
        block.add_prefix(&gtk::Image::from_icon_name("contact-new-symbolic"));
        block.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
        let weak = Rc::downgrade(self);
        block.connect_activated(move |_| {
            let Some(ui) = weak.upgrade() else { return };
            let parent = ui
                .blocking_list
                .borrow()
                .as_ref()
                .and_then(|controls| controls.dialog.upgrade());
            if let Some(parent) = parent {
                let weak = Rc::downgrade(&ui);
                parent.connect_closed(move |_| {
                    if let Some(ui) = weak.upgrade() {
                        ui.choose_block_contact();
                    }
                });
                parent.close();
            } else {
                ui.choose_block_contact();
            }
        });
        actions.add(&block);
        root.append(&actions);
        let search = gtk::SearchEntry::builder()
            .placeholder_text("Search blocked addresses")
            .margin_top(12)
            .margin_start(12)
            .margin_end(12)
            .margin_bottom(8)
            .build();
        root.append(&search);
        let body = bx(true, 10);
        body.set_margin_start(12);
        body.set_margin_end(12);
        body.set_margin_bottom(12);
        body.set_vexpand(true);
        root.append(&body);
        let (dialog, header) = self.dialog("Blocked Contacts", &root, 520, 620);
        let refresh = icon_button("view-refresh-symbolic", "Refresh blocked contacts");
        header.pack_end(&refresh);
        self.blocking_list.replace(Some(ListControls {
            owner: owner.clone(),
            generation: self.generation.get(),
            body: body.downgrade(),
            search: search.downgrade(),
            block: block.downgrade(),
            dialog: dialog.downgrade(),
        }));
        let weak = Rc::downgrade(self);
        search.connect_search_changed(move |_| {
            if let Some(ui) = weak.upgrade() {
                ui.render_blocked_contacts();
            }
        });
        let weak = Rc::downgrade(self);
        refresh.connect_clicked(move |_| {
            if let Some(ui) = weak.upgrade() {
                ui.request_blocking_list();
            }
        });
        let weak = Rc::downgrade(self);
        dialog.connect_closed(move |closed| {
            let Some(ui) = weak.upgrade() else { return };
            let active = ui
                .blocking_list
                .borrow()
                .as_ref()
                .and_then(|controls| controls.dialog.upgrade())
                .is_some_and(|dialog| dialog == *closed);
            if active {
                ui.blocking_list.borrow_mut().take();
            }
        });
        self.render_blocked_contacts();
        if !self.blocking_snapshot.borrow().fresh {
            self.request_blocking_list();
        }
    }

    pub fn request_blocking_list(self: &Rc<Self>) {
        let owner = self.state.borrow().account.clone();
        let generation = self.generation.get();
        if owner.is_empty() || self.blocking_loading.replace(true) {
            return;
        }
        self.blocking_error.borrow_mut().take();
        self.refresh_blocking_ui();
        let (reply, receive) = tokio::sync::oneshot::channel();
        if !self.command(Command::BlockingList { reply }) {
            self.blocking_loading.set(false);
            self.blocking_error
                .replace(Some("Reconnect to refresh blocked contacts".into()));
            self.refresh_blocking_ui();
            return;
        }
        let weak = Rc::downgrade(self);
        glib::MainContext::default().spawn_local(async move {
            let result = receive.await;
            let Some(ui) = weak.upgrade().filter(|ui| {
                ui.generation.get() == generation && ui.state.borrow().account == owner
            }) else {
                return;
            };
            ui.blocking_loading.set(false);
            match result {
                Ok(Ok(snapshot)) => {
                    ui.blocking_snapshot.replace(snapshot);
                    ui.blocking_error.borrow_mut().take();
                }
                Ok(Err(error)) => {
                    ui.blocking_error.replace(Some(error));
                }
                Err(_) => {
                    ui.blocking_error
                        .replace(Some("Blocking request was interrupted".into()));
                }
            };
            ui.refresh_blocking_ui();
        });
    }

    pub fn refresh_blocking_ui(self: &Rc<Self>) {
        self.render_blocked_contacts();
        if self.utility.shows_sidebar() && self.media_mode.borrow().as_str() == "info" {
            self.show_info();
        }
    }

    fn render_blocked_contacts(self: &Rc<Self>) {
        let Some((body, search, block)) = self.blocking_list.borrow().as_ref().and_then(|controls| {
            (controls.owner == self.state.borrow().account
                && controls.generation == self.generation.get())
            .then(|| {
                Some((
                    controls.body.upgrade()?,
                    controls.search.upgrade()?,
                    controls.block.upgrade()?,
                ))
            })?
        }) else {
            return;
        };
        clear(&body);
        let snapshot = self.blocking_snapshot.borrow().clone();
        let error = self.blocking_error.borrow().clone();
        let loading = self.blocking_loading.get();
        block.set_sensitive(snapshot.supported != Some(false) && !loading);
        if snapshot.supported == Some(false) {
            body.append(
                &adw::StatusPage::builder()
                    .icon_name("changes-prevent-symbolic")
                    .title("Blocking Unavailable")
                    .description("This server does not support contact blocking.")
                    .build(),
            );
            return;
        }
        let Some(jids) = snapshot.jids else {
            let status = adw::StatusPage::builder()
                .icon_name("changes-prevent-symbolic")
                .title(if loading {
                    "Loading Blocked Contacts"
                } else {
                    "Blocking Status Unknown"
                })
                .description(error.as_deref().unwrap_or(
                    "Connect and refresh to retrieve the server-owned block list.",
                ))
                .build();
            if loading {
                status.set_child(Some(&gtk::Spinner::builder().spinning(true).build()));
            } else {
                let retry = gtk::Button::with_label("Refresh");
                retry.add_css_class("suggested-action");
                self.weak_button(&retry, |ui| ui.request_blocking_list());
                status.set_child(Some(&retry));
            }
            body.append(&status);
            return;
        };
        if !snapshot.fresh {
            let stale = adw::Banner::builder()
                .title("Showing the saved block list. Refresh to confirm it with the server.")
                .button_label("Refresh")
                .revealed(true)
                .build();
            let weak = Rc::downgrade(self);
            stale.connect_button_clicked(move |_| {
                if let Some(ui) = weak.upgrade() {
                    ui.request_blocking_list();
                }
            });
            body.append(&stale);
        }
        let query = search.text().trim().to_lowercase();
        let matches: Vec<_> = jids
            .into_iter()
            .filter(|jid| query.is_empty() || jid.to_lowercase().contains(&query))
            .collect();
        if matches.is_empty() {
            body.append(
                &adw::StatusPage::builder()
                    .icon_name("changes-prevent-symbolic")
                    .title(if query.is_empty() {
                        "No Blocked Contacts"
                    } else {
                        "No Matching Addresses"
                    })
                    .description(if query.is_empty() {
                        "Blocked XMPP addresses will appear here."
                    } else {
                        "Try a different search."
                    })
                    .build(),
            );
        } else {
            let values: Vec<_> = matches.iter().map(String::as_str).collect();
            let model = gtk::StringList::new(&values);
            let selection = gtk::NoSelection::new(Some(model));
            let factory = gtk::SignalListItemFactory::new();
            let weak = Rc::downgrade(self);
            factory.connect_setup(move |_, item| {
                let row = adw::ActionRow::builder().use_markup(false).build();
                let remove = gtk::Button::builder()
                    .label("Unblock")
                    .valign(gtk::Align::Center)
                    .build();
                remove.add_css_class("flat");
                let row_ref = row.downgrade();
                let weak = weak.clone();
                remove.connect_clicked(move |_| {
                    let Some((ui, row)) = weak.upgrade().zip(row_ref.upgrade()) else {
                        return;
                    };
                    let target = row.title().to_string();
                    ui.confirm_blocking_change(&target, &target, false);
                });
                row.add_suffix(&remove);
                item.downcast_ref::<gtk::ListItem>()
                    .expect("factory item")
                    .set_child(Some(&row));
            });
            factory.connect_bind(|_, item| {
                let item = item.downcast_ref::<gtk::ListItem>().expect("factory item");
                let rule = item
                    .item()
                    .and_downcast::<gtk::StringObject>()
                    .expect("string item")
                    .string();
                let row = item
                    .child()
                    .and_downcast::<adw::ActionRow>()
                    .expect("action row");
                row.set_title(&rule);
                row.set_subtitle(&blocking_scope(&rule));
                if let Some(button) = row.last_child().and_downcast::<gtk::Button>() {
                    button.set_tooltip_text(Some(&format!("Unblock {rule}")));
                }
            });
            let list = gtk::ListView::new(Some(selection), Some(factory));
            list.add_css_class("boxed-list");
            let scrolling = gtk::ScrolledWindow::builder()
                .hscrollbar_policy(gtk::PolicyType::Never)
                .vexpand(true)
                .child(&list)
                .build();
            body.append(&scrolling);
        }
        if loading {
            let feedback = bx(false, 8);
            feedback.set_halign(gtk::Align::Center);
            feedback.append(&gtk::Spinner::builder().spinning(true).build());
            feedback.append(&label("Refreshing blocked contacts…", "dim-label"));
            body.append(&feedback);
        } else if let Some(error) = error {
            let feedback = bx(false, 8);
            feedback.set_halign(gtk::Align::Center);
            feedback.append(&label(&error, "error"));
            let retry = gtk::Button::with_label("Retry");
            self.weak_button(&retry, |ui| ui.request_blocking_list());
            feedback.append(&retry);
            body.append(&feedback);
        }
    }

    fn choose_block_contact(self: &Rc<Self>) {
        let root = bx(true, 0);
        let search = gtk::SearchEntry::builder()
            .placeholder_text("Search contacts or enter an XMPP address")
            .margin_top(12)
            .margin_start(12)
            .margin_end(12)
            .margin_bottom(8)
            .build();
        root.append(&search);
        let list = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .css_classes(["boxed-list"])
            .margin_start(12)
            .margin_end(12)
            .margin_bottom(12)
            .build();
        let scrolling = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .child(&list)
            .build();
        root.append(&scrolling);
        let (dialog, _) = self.dialog("Block Contact", &root, 480, 560);
        let refresh = {
            let weak = Rc::downgrade(self);
            let list = list.downgrade();
            let search = search.downgrade();
            let dialog = dialog.downgrade();
            move || {
                let Some((ui, list, search, dialog)) = weak
                    .upgrade()
                    .zip(list.upgrade())
                    .zip(search.upgrade())
                    .zip(dialog.upgrade())
                    .map(|(((ui, list), search), dialog)| (ui, list, search, dialog))
                else {
                    return;
                };
                ui.render_block_contact_choices(&list, &search, &dialog);
            }
        };
        refresh();
        search.connect_search_changed(move |_| refresh());
        search.grab_focus();
    }

    fn render_block_contact_choices(
        self: &Rc<Self>,
        list: &gtk::ListBox,
        search: &gtk::SearchEntry,
        dialog: &adw::Dialog,
    ) {
        clear_list(list);
        let owner = self.state.borrow().account.clone();
        let query = search.text().trim().to_lowercase();
        let chats = self.state.borrow().chats.clone();
        let mut shown = false;
        for chat in chats.into_iter().filter(|chat| {
            !chat.group
                && crate::contact_access::valid_peer(&owner, &chat.jid)
                && (query.is_empty()
                    || chat.name.to_lowercase().contains(&query)
                    || chat.jid.to_lowercase().contains(&query))
        }) {
            shown = true;
            self.append_block_contact_choice(list, dialog, &chat.jid, &chat.name);
        }
        let entered = search.text();
        let typed = entered.trim();
        if !typed.is_empty()
            && crate::contact_access::valid_peer(&owner, typed)
            && !self.state.borrow().chats.iter().any(|chat| chat.jid == typed)
        {
            shown = true;
            self.append_block_contact_choice(list, dialog, typed, typed);
        }
        if !shown {
            let status = adw::StatusPage::builder()
                .title("No Matching Contacts")
                .description("Enter a complete bare XMPP address to block it directly.")
                .icon_name("system-search-symbolic")
                .build();
            list.append(&status);
        }
    }

    fn append_block_contact_choice(
        self: &Rc<Self>,
        list: &gtk::ListBox,
        dialog: &adw::Dialog,
        peer: &str,
        name: &str,
    ) {
        let row = adw::ActionRow::builder()
            .title(name)
            .subtitle(peer)
            .use_markup(false)
            .activatable(true)
            .build();
        row.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
        let weak = Rc::downgrade(self);
        let dialog = dialog.downgrade();
        let peer = peer.to_owned();
        let name = name.to_owned();
        row.connect_activated(move |_| {
            let Some((ui, dialog)) = weak.upgrade().zip(dialog.upgrade()) else {
                return;
            };
            dialog.close();
            ui.confirm_blocking_change(&peer, &name, true);
        });
        list.append(&row);
    }

    pub fn append_blocking_profile(self: &Rc<Self>, group: &adw::PreferencesGroup, chat: &Chat) {
        let owner = self.state.borrow().account.clone();
        if owner.is_empty()
            || chat.group
            || !crate::contact_access::valid_peer(&owner, &chat.jid)
        {
            return;
        }
        let snapshot = self.blocking_snapshot.borrow().clone();
        let rules = snapshot
            .jids
            .as_deref()
            .map(|jids| crate::blocking::matching_rules(jids, &chat.jid))
            .unwrap_or_default();
        let exact = rules.iter().any(|rule| rule == &chat.jid);
        let broader = rules.iter().find(|rule| *rule != &chat.jid).cloned();
        let managed = broader.is_some();
        let row = if let Some(ref rule) = broader {
            adw::ActionRow::builder()
                .title("Managed by a Broader Block Rule")
                .subtitle(rule)
                .activatable(true)
                .build()
        } else if exact {
            adw::ActionRow::builder()
                .title("Unblock Contact")
                .subtitle("Remove this exact address from the server block list")
                .activatable(true)
                .build()
        } else if snapshot.supported == Some(false) {
            adw::ActionRow::builder()
                .title("Contact Blocking")
                .subtitle("Unavailable on this server")
                .build()
        } else if snapshot.fresh {
            adw::ActionRow::builder()
                .title("Block Contact")
                .subtitle("Keep this chat and history, but stop contact from this address")
                .activatable(true)
                .build()
        } else {
            adw::ActionRow::builder()
                .title("Manage Blocked Contacts")
                .subtitle(if snapshot.jids.is_some() {
                    "Saved status — refresh to confirm with the server"
                } else {
                    "Blocking status unknown"
                })
                .activatable(true)
                .build()
        };
        row.add_prefix(&gtk::Image::from_icon_name("changes-prevent-symbolic"));
        if row.is_activatable() {
            row.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
            let weak = Rc::downgrade(self);
            let peer = chat.jid.clone();
            let name = chat.name.clone();
            row.connect_activated(move |_| {
                let Some(ui) = weak.upgrade() else { return };
                if managed || (!exact && !snapshot.fresh) {
                    ui.blocked_contacts();
                } else {
                    ui.confirm_blocking_change(&peer, &name, !exact);
                }
            });
        }
        if self.blocking_loading.get() {
            row.add_suffix(&gtk::Spinner::builder().spinning(true).build());
        }
        group.add(&row);
    }

    pub fn confirm_blocking_change(self: &Rc<Self>, peer: &str, name: &str, blocked: bool) {
        let owner = self.state.borrow().account.clone();
        let generation = self.generation.get();
        let dialog = adw::AlertDialog::builder()
            .heading(if blocked {
                format!("Block {name}?")
            } else {
                format!("Unblock {name}?")
            })
            .body(if blocked {
                "They will no longer be able to contact you. This chat and its history are kept."
            } else {
                "Remove only this exact server blocking rule. Broader domain rules remain in effect."
            })
            .build();
        let action = if blocked { "block" } else { "unblock" };
        dialog.add_responses(&[("cancel", "Cancel"), (action, if blocked { "Block" } else { "Unblock" })]);
        dialog.set_default_response(Some("cancel"));
        dialog.set_close_response("cancel");
        dialog.set_response_appearance(action, adw::ResponseAppearance::Destructive);
        let weak = Rc::downgrade(self);
        let peer = peer.to_owned();
        dialog.connect_response(Some(action), move |_, _| {
            if let Some(ui) = weak.upgrade().filter(|ui| {
                ui.generation.get() == generation && ui.state.borrow().account == owner
            }) {
                ui.change_blocking(&peer, blocked);
            }
        });
        dialog.present(Some(&self.window));
    }

    fn change_blocking(self: &Rc<Self>, peer: &str, blocked: bool) {
        let owner = self.state.borrow().account.clone();
        let generation = self.generation.get();
        if owner.is_empty() || self.blocking_loading.replace(true) {
            return;
        }
        self.blocking_error.borrow_mut().take();
        self.refresh_blocking_ui();
        let (reply, receive) = tokio::sync::oneshot::channel();
        if !self.command(Command::BlockContact {
            peer: peer.to_owned(),
            blocked,
            reply,
        }) {
            self.blocking_loading.set(false);
            self.blocking_error
                .replace(Some("Reconnect before changing blocked contacts".into()));
            self.refresh_blocking_ui();
            return;
        }
        let weak = Rc::downgrade(self);
        glib::MainContext::default().spawn_local(async move {
            let result = receive.await;
            let Some(ui) = weak.upgrade().filter(|ui| {
                ui.generation.get() == generation && ui.state.borrow().account == owner
            }) else {
                return;
            };
            ui.blocking_loading.set(false);
            match result {
                Ok(Ok(snapshot)) => {
                    ui.blocking_snapshot.replace(snapshot);
                    ui.blocking_error.borrow_mut().take();
                }
                Ok(Err(error)) => {
                    ui.blocking_error.replace(Some(error.clone()));
                    ui.notice(&error);
                }
                Err(_) => {
                    let error = "Blocking change was interrupted".to_string();
                    ui.blocking_error.replace(Some(error.clone()));
                    ui.notice(&error);
                }
            }
            ui.refresh_blocking_ui();
        });
    }

    pub fn blocking_summary(&self) -> String {
        let snapshot = self.blocking_snapshot.borrow();
        if snapshot.supported == Some(false) {
            "Unavailable".into()
        } else if let Some(jids) = &snapshot.jids {
            if snapshot.fresh {
                format!("{} blocked", jids.len())
            } else {
                format!("{} saved", jids.len())
            }
        } else {
            "Status unknown".into()
        }
    }
}

fn blocking_scope(rule: &str) -> &'static str {
    if rule.contains('/') {
        "Specific device or resource"
    } else if rule.contains('@') {
        "Contact address"
    } else {
        "Entire domain"
    }
}
