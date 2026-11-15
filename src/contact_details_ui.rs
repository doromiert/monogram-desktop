//! Local contact presentation and editing; remote profiles remain independent.
use crate::{contact_details::LocalContact, ui::Ui};
use adw::prelude::*;
use gtk::{gio, glib};
use std::{
    cell::RefCell,
    collections::{HashMap, HashSet},
    fs::{self, OpenOptions},
    io::Write,
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::PathBuf,
    rc::Rc,
};
impl Ui {
    pub(crate) fn apply_local_contact_names(&self) {
        let names = self.local_contacts.borrow();
        for chat in &mut self.state.borrow_mut().chats {
            chat.local_name = names
                .get(&chat.jid)
                .map(|c| c.nickname.clone())
                .unwrap_or_default();
        }
    }
    pub(crate) fn reload_contact_details(self: &Rc<Self>) {
        let owner = self.state.borrow().account.clone();
        if owner.is_empty() {
            return;
        }
        let wanted: HashSet<String> = {
            let state = self.state.borrow();
            std::iter::once(state.current().jid.clone())
                .chain(state.chats.iter().map(|chat| chat.jid.clone()))
                .take(128)
                .collect()
        };
        let serial = self.contact_load_serial.get().wrapping_add(1);
        self.contact_load_serial.set(serial);
        let generation = self.generation.get();
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            let account = owner.clone();
            let result = ui
                .store
                .read_snapshot(move |store| {
                    let records = store.load_all_contact_overrides(&account)?;
                    let mut images = HashMap::new();
                    for (peer, record) in &records {
                        if !wanted.contains(peer) {
                            continue;
                        }
                        if let Some(path) = &record.avatar {
                            if let Ok(bytes) = fs::read(path) {
                                if let Ok(texture) = crate::avatar::Avatar::new(bytes, false)
                                    .decode(&gio::Cancellable::new())
                                {
                                    images.insert(peer.clone(), texture);
                                }
                            }
                        }
                    }
                    Ok((records, images))
                })
                .await;
            if ui.contact_load_serial.get() != serial
                || ui.generation.get() != generation
                || ui.state.borrow().account != owner
            {
                return;
            }
            if let Ok((records, images)) = result {
                ui.local_contacts.replace(records);
                ui.local_contact_avatars.replace(images);
                ui.apply_local_contact_names();
                ui.refresh_chats(None);
                if ui.pages.visible_child_name().as_deref() == Some("chat") {
                    ui.show_chat();
                }
                if ui.utility.shows_sidebar() && ui.media_mode.borrow().as_str() == "info" {
                    ui.show_info();
                }
            }
        });
    }
    pub(crate) fn edit_contact_details(self: &Rc<Self>, peer: &str) {
        let owner = self.state.borrow().account.clone();
        if owner.is_empty() {
            return;
        }
        let peer = peer.to_string();
        let generation = self.generation.get();
        let weak = Rc::downgrade(self);
        self.notice("Loading contact details…");
        glib::spawn_future_local(async move {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            let account = owner.clone();
            let address = peer.clone();
            let result = ui
                .store
                .read_snapshot(move |store| store.contact_details(&account, &address))
                .await;
            if ui.generation.get() != generation || ui.state.borrow().account != owner {
                return;
            }
            match result {
                Ok(value) => ui.local_contact_dialog(value),
                Err(_) => {
                    ui.notice("Could not load local details. Close this dialog and try again.")
                }
            }
        });
    }
    fn local_contact_dialog(self: &Rc<Self>, value: LocalContact) {
        let owner = self.state.borrow().account.clone();
        let peer = value.peer.clone();
        let generation = self.generation.get();
        let page = adw::PreferencesPage::new();
        let group = adw::PreferencesGroup::builder()
            .description("Only you and your paired devices see these details.")
            .build();
        let name = adw::EntryRow::builder()
            .title("Local nickname")
            .text(&value.nickname)
            .build();
        group.add(&name);
        let photo = adw::ActionRow::builder().title("Local picture").build();
        let preview = adw::Avatar::new(48, Some(&value.nickname), true);
        if let Some(texture) = self.local_contact_avatars.borrow().get(&peer) {
            preview.set_custom_image(Some(texture));
        }
        photo.add_prefix(&preview);
        let choose = gtk::Button::with_label("Choose…");
        choose.set_valign(gtk::Align::Center);
        photo.add_suffix(&choose);
        let clear = crate::widgets::icon_button("edit-clear-symbolic", "Remove local picture");
        clear.set_valign(gtk::Align::Center);
        photo.add_suffix(&clear);
        group.add(&photo);
        page.add(&group);
        let notes = adw::PreferencesGroup::builder().title("Notes").build();
        let text = gtk::TextView::builder()
            .wrap_mode(gtk::WrapMode::WordChar)
            .height_request(160)
            .top_margin(12)
            .bottom_margin(12)
            .left_margin(12)
            .right_margin(12)
            .build();
        text.add_css_class("card");
        text.buffer().set_text(&value.note);
        notes.add(&text);
        page.add(&notes);
        let status = gtk::Label::new(None);
        status.set_wrap(true);
        let feedback = adw::PreferencesGroup::new();
        feedback.add(&status);
        page.add(&feedback);
        let (dialog, header) = self.dialog("Local Contact Details", &page, 480, 520);
        let save = gtk::Button::with_label("Save");
        save.add_css_class("suggested-action");
        header.pack_end(&save);
        let chosen = Rc::new(RefCell::new(value.avatar));
        let temporary = Rc::new(RefCell::new(Vec::<PathBuf>::new()));
        let selection = chosen.clone();
        let avatar = preview.clone();
        clear.connect_clicked(move |_| {
            selection.borrow_mut().take();
            avatar.set_custom_image(gtk::gdk::Paintable::NONE);
        });
        let weak = Rc::downgrade(self);
        let selection = chosen.clone();
        let pending = temporary.clone();
        let feedback = status.clone();
        let avatar = preview.clone();
        let button = save.clone();
        let window = self.window.clone();
        let account = owner.clone();
        let expected = owner.clone();
        let picture_dialog = dialog.downgrade();
        let clear_picture = clear.clone();
        choose.connect_clicked(move |choose| {
            let choose = choose.clone();
            let clear_picture = clear_picture.clone();
            let picture_dialog = picture_dialog.clone();
            let expected = expected.clone();
            let weak = weak.clone();
            let selection = selection.clone();
            let pending = pending.clone();
            let feedback = feedback.clone();
            let avatar = avatar.clone();
            let button = button.clone();
            let window = window.clone();
            let account = account.clone();
            glib::spawn_future_local(async move {
                let chooser = gtk::FileDialog::builder()
                    .title("Choose Local Picture")
                    .modal(true)
                    .build();
                let filter = gtk::FileFilter::new();
                filter.add_mime_type("image/*");
                let filters = gio::ListStore::new::<gtk::FileFilter>();
                filters.append(&filter);
                chooser.set_filters(Some(&filters));
                let Ok(file) = chooser.open_future(Some(&window)).await else {
                    return;
                };
                if weak.upgrade().is_none_or(|ui| {
                    ui.generation.get() != generation || ui.state.borrow().account != expected
                }) || picture_dialog
                    .upgrade()
                    .is_none_or(|dialog| dialog.parent().is_none())
                {
                    return;
                }
                button.set_sensitive(false);
                choose.set_sensitive(false);
                clear_picture.set_sensitive(false);
                feedback.set_text("Preparing picture…");
                let result = gio::spawn_blocking(move || prepare_picture(&account, file)).await;
                button.set_sensitive(true);
                choose.set_sensitive(true);
                clear_picture.set_sensitive(true);
                let Some(ui) = weak.upgrade().filter(|ui| {
                    ui.generation.get() == generation
                        && ui.state.borrow().account == expected
                        && picture_dialog
                            .upgrade()
                            .is_some_and(|dialog| dialog.parent().is_some())
                }) else {
                    if let Ok(Ok((path, _))) = result {
                        gio::spawn_blocking(move || {
                            let _ = fs::remove_file(path);
                        });
                    }
                    return;
                };
                drop(ui);
                match result {
                    Ok(Ok((path, texture))) => {
                        pending.borrow_mut().push(path.clone());
                        selection.replace(Some(path));
                        avatar.set_custom_image(Some(&texture));
                        feedback.set_text("");
                    }
                    Ok(Err(error)) => feedback.set_text(&error),
                    Err(_) => {
                        feedback.set_text("Picture preparation stopped. Choose the image again.")
                    }
                }
            });
        });
        let cleanup = temporary.clone();
        dialog.connect_closed(move |_| {
            let paths = cleanup.take();
            gio::spawn_blocking(move || {
                for path in paths {
                    let _ = fs::remove_file(path);
                }
            });
        });
        let weak = Rc::downgrade(self);
        let closing = dialog.downgrade();
        save.connect_clicked(move|button|{
            let nickname=name.text().trim().to_string();let note=text.buffer().text(&text.buffer().start_iter(),&text.buffer().end_iter(),false).to_string();
            if nickname.chars().count()>128||nickname.encode_utf16().count()>256||nickname.chars().any(char::is_control)||note.encode_utf16().count()>8192{status.set_text("Use a nickname of at most 128 characters and notes of at most 8192 characters.");return;}
            let Some(ui)=weak.upgrade().filter(|ui|ui.generation.get()==generation&&ui.state.borrow().account==owner)else{return;};button.set_sensitive(false);status.set_text("Saving…");let account=owner.clone();let address=peer.clone();let avatar=chosen.borrow().clone();let weak=Rc::downgrade(&ui);let closing=closing.clone();let status=status.clone();let button=button.clone();
            glib::spawn_future_local(async move{let result=ui.store.write_snapshot(move|store|store.set_contact_details(&account,&address,&nickname,&note,avatar.as_deref())).await;let Some(ui)=weak.upgrade().filter(|ui|ui.generation.get()==generation)else{return;};match result{Ok(())=>{if let Some(dialog)=closing.upgrade(){dialog.close();}ui.reload_contact_details();},Err(_)=>{status.set_text("Could not save local details. Check available disk space and try again.");button.set_sensitive(true);}}});
        });
    }
    pub(crate) fn suggest_contact_picture(self: &Rc<Self>, peer: &str) {
        let owner = self.state.borrow().account.clone();
        let peer = peer.to_string();
        let generation = self.generation.get();
        let weak = Rc::downgrade(self);
        let window = self.window.clone();
        glib::spawn_future_local(async move {
            let chooser = gtk::FileDialog::builder()
                .title("Suggest Profile Picture")
                .modal(true)
                .build();
            let filter = gtk::FileFilter::new();
            filter.add_mime_type("image/*");
            let filters = gio::ListStore::new::<gtk::FileFilter>();
            filters.append(&filter);
            chooser.set_filters(Some(&filters));
            let Ok(file) = chooser.open_future(Some(&window)).await else {
                return;
            };
            let Some(ui) = weak.upgrade().filter(|ui| {
                ui.generation.get() == generation && ui.state.borrow().account == owner
            }) else {
                return;
            };
            if ui.state.borrow().current().jid != peer {
                ui.notice(
                    "The conversation changed. Open the contact and suggest the picture again.",
                );
                return;
            }
            ui.queue_local_files(vec![file], "Suggested profile photo", false, false);
        });
    }
}
fn prepare_picture(owner: &str, file: gio::File) -> Result<(PathBuf, gtk::gdk::Texture), String> {
    let stream = file
        .read(gio::Cancellable::NONE)
        .map_err(|_| "Could not open that image. Choose another file.")?;
    let mut bytes = vec![];
    loop {
        let chunk = stream
            .read_bytes(65536, gio::Cancellable::NONE)
            .map_err(|_| "Could not read that image.")?;
        if chunk.is_empty() {
            break;
        }
        bytes.extend_from_slice(&chunk);
        if bytes.len() > 32 * 1024 * 1024 {
            return Err("Choose an image smaller than 32 MiB.".into());
        }
    }
    let cancel = gio::Cancellable::new();
    let mut decoder =
        crate::image_loader::Decoder::open_bytes(&glib::Bytes::from_owned(bytes), &cancel)
            .map_err(|_| "Choose a JPEG, PNG, WebP, GIF, AVIF or HEIC image.")?;
    if !matches!(
        decoder.mime.as_str(),
        "image/jpeg"
            | "image/png"
            | "image/webp"
            | "image/gif"
            | "image/avif"
            | "image/heif"
            | "image/heic"
    ) || u64::from(decoder.width) * u64::from(decoder.height) > 40_000_000
    {
        return Err("Choose a supported image up to 40 megapixels.".into());
    }
    let texture = decoder
        .frame(256)
        .map_err(|_| "Could not decode that image. Choose another file.")?
        .texture;
    let png = texture.save_to_png_bytes();
    let root = crate::downloads::account_root(owner).map_err(str::to_string)?;
    fs::create_dir_all(&root).map_err(|_| "The image cache is unavailable.")?;
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700))
        .map_err(|_| "The image cache is unavailable.")?;
    let path = root.join(format!("contact-pending-{}.png", uuid::Uuid::new_v4()));
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)
        .map_err(|_| "Could not prepare that picture.")?;
    output
        .write_all(png.as_ref())
        .map_err(|_| "Could not prepare that picture.")?;
    Ok((path, texture))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Account, State};
    fn wait(mut ready: impl FnMut() -> bool) {
        let context = glib::MainContext::default();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(4);
        while std::time::Instant::now() < deadline {
            while context.pending() {
                context.iteration(false);
            }
            if ready() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(3));
        }
        assert!(ready());
    }
    fn find<T: IsA<gtk::Widget> + glib::object::IsClass + Clone + 'static>(
        root: &gtk::Widget,
        test: &impl Fn(&T) -> bool,
    ) -> Option<T> {
        if let Ok(widget) = root.clone().downcast::<T>() {
            if test(&widget) {
                return Some(widget);
            }
        }
        let mut child = root.first_child();
        while let Some(widget) = child {
            child = widget.next_sibling();
            if let Some(found) = find(&widget, test) {
                return Some(found);
            }
        }
        None
    }
    #[test]
    #[ignore = "requires private display and Secret Service"]
    fn local_details_edit_updates_presentation_without_overwriting_remote_profile() {
        let app = adw::Application::builder()
            .application_id("com.negzero.LocalContactTest")
            .flags(gio::ApplicationFlags::NON_UNIQUE)
            .build();
        app.register(gio::Cancellable::NONE).unwrap();
        let ui = Ui::new(&app, true);
        let owner = "owner@fixture.invalid";
        let peer = "friend@fixture.invalid";
        ui.store
            .save_account(&Account {
                jid: owner.into(),
                host: "fixture.invalid".into(),
                port: 5222,
                direct_tls: false,
            })
            .unwrap();
        let mut state = State::live(owner, vec![]);
        state.chat_for(peer).name = "Remote Profile".into();
        state.active = state.chat_for(peer).id.clone();
        ui.activate_state(state);
        ui.window.present();
        let input = std::env::temp_dir().join(format!("contact-ui-{}.png", uuid::Uuid::new_v4()));
        let bytes=base64::Engine::decode(&base64::engine::general_purpose::STANDARD,"iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+ip1sAAAAASUVORK5CYII=").unwrap();
        fs::write(&input, bytes).unwrap();
        let file = gio::File::for_path(&input);
        let (path, texture) = glib::MainContext::default()
            .block_on(gio::spawn_blocking(move || prepare_picture(owner, file)))
            .unwrap()
            .unwrap();
        assert!(texture.width() <= 256 && texture.height() <= 256);
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        ui.store
            .set_contact_details(owner, peer, "Local Name", "Private note", Some(&path))
            .unwrap();
        fs::remove_file(input).unwrap();
        fs::remove_file(path).unwrap();
        ui.reload_contact_details();
        wait(|| ui.state.borrow().current().display_name() == "Local Name");
        assert_eq!(ui.state.borrow().current().name, "Remote Profile");
        assert!(ui.local_contact_avatars.borrow().contains_key(peer));
        ui.show_info();
        assert!(
            find::<adw::ActionRow>(&ui.utility.clone().upcast(), &|row| row.title()
                == "Local Details")
            .is_some()
        );
        ui.edit_contact_details(peer);
        wait(|| ui.window.visible_dialog().is_some());
        let dialog = ui.window.visible_dialog().unwrap();
        let name = find::<adw::EntryRow>(&dialog.clone().upcast(), &|row| {
            row.title() == "Local nickname"
        })
        .unwrap();
        assert_eq!(name.text(), "Local Name");
        name.set_text("Updated nickname");
        let notes = find::<gtk::TextView>(&dialog.clone().upcast(), &|_| true).unwrap();
        notes.buffer().set_text("Updated private note");
        let save = find::<gtk::Button>(&dialog.clone().upcast(), &|b| {
            b.label().as_deref() == Some("Save")
        })
        .unwrap();
        save.emit_clicked();
        wait(|| ui.state.borrow().current().display_name() == "Updated nickname");
        assert_eq!(
            ui.store.contact_details(owner, peer).unwrap().note,
            "Updated private note"
        );
        ui.store
            .set_contact_details(owner, peer, "", "", None)
            .unwrap();
        ui.reload_contact_details();
        wait(|| ui.state.borrow().current().display_name() == "Remote Profile");
        assert!(ui.local_contact_avatars.borrow().is_empty());
        ui.window.close();
    }
}
