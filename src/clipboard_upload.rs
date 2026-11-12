//! Image clipboard input shares the durable media outbox and leaves the text draft alone.
use crate::ui::Ui;
use adw::prelude::*;
use gtk::{gdk, gio, glib};
use std::{cell::Cell, rc::Rc};

fn has_media(clipboard: &gdk::Clipboard) -> bool {
    clipboard
        .formats()
        .contains_type(gdk::FileList::static_type())
        || clipboard
            .formats()
            .contains_type(gdk::Texture::static_type())
        || clipboard
            .formats()
            .mime_types()
            .iter()
            .any(|mime| mime.starts_with("image/"))
}

fn enable_media_paste(view: &gtk::TextView) {
    if view.is_editable() && has_media(&view.clipboard()) {
        view.action_set_enabled("clipboard.paste", true);
    }
}

fn after_popup(view: &gtk::TextView) {
    // GTK enables its native Paste menu item for strings only while constructing the popup.
    let view = view.downgrade();
    glib::idle_add_local_once(move || {
        if let Some(view) = view.upgrade() {
            enable_media_paste(&view);
        }
    });
}

pub fn attach(ui: &Rc<Ui>) {
    let busy = Rc::new(Cell::new(false));
    let weak = Rc::downgrade(ui);
    ui.composer.connect_paste_clipboard(move |view| {
        let clipboard = view.clipboard();
        if !has_media(&clipboard) {
            return;
        }
        view.stop_signal_emission_by_name("paste-clipboard");
        let Some(ui) = weak.upgrade() else { return };
        if clipboard
            .formats()
            .contains_type(gdk::FileList::static_type())
        {
            if busy.replace(true) {
                ui.notice("Preparing pasted files…");
                return;
            }
            let owner = ui.state.borrow().account.clone();
            let peer = ui.state.borrow().current().jid.clone();
            let generation = ui.generation.get();
            let weak = Rc::downgrade(&ui);
            let busy = busy.clone();
            ui.notice("Preparing pasted files…");
            glib::spawn_future_local(async move {
                let result = glib::future_with_timeout(
                    std::time::Duration::from_secs(15),
                    clipboard
                        .read_value_future(gdk::FileList::static_type(), glib::Priority::DEFAULT),
                )
                .await;
                busy.set(false);
                let Some(ui) = weak.upgrade().filter(|ui| {
                    ui.generation.get() == generation
                        && ui.state.borrow().account == owner
                        && ui.state.borrow().current().jid == peer
                }) else {
                    return;
                };
                match result {
                    Ok(Ok(value)) => match value.get::<gdk::FileList>() {
                        Ok(files) => {
                            let files: Vec<_> = files
                                .files()
                                .into_iter()
                                .filter(|file| file.path().is_some())
                                .take(64)
                                .collect();
                            if files.is_empty() || !ui.stage_attachment_files(files) {
                                ui.notice("The clipboard has no available local files");
                            }
                        }
                        Err(_) => ui.notice("The clipboard files could not be read"),
                    },
                    _ => ui.notice("The clipboard files could not be read"),
                }
            });
            return;
        }
        if busy.replace(true) {
            ui.notice("Preparing pasted image…");
            return;
        }
        let owner = ui.state.borrow().account.clone();
        let peer = ui.state.borrow().current().jid.clone();
        let generation = ui.generation.get();
        if owner.is_empty() || peer.is_empty() {
            busy.set(false);
            return;
        }
        let progress = adw::Toast::new("Preparing pasted image…");
        progress.set_timeout(0);
        ui.toast.add_toast(progress.clone());
        let weak = Rc::downgrade(&ui);
        let busy = busy.clone();
        glib::MainContext::default().spawn_local(async move {
            let result = async {
                let texture = glib::future_with_timeout(
                    std::time::Duration::from_secs(15),
                    clipboard.read_texture_future(),
                )
                .await
                .map_err(|_| "Reading the clipboard image timed out")?
                .map_err(|_| "Cannot read the image from the clipboard")?
                .ok_or("The clipboard no longer contains an image")?;
                let width = texture.width() as i64;
                let height = texture.height() as i64;
                if width <= 0 || height <= 0 || width * height > 64 * 1024 * 1024 {
                    return Err("The pasted image is too large");
                }
                let owner = owner.clone();
                gio::spawn_blocking(move || snapshot(&owner, &texture))
                    .await
                    .map_err(|_| "The pasted image could not be prepared")?
            }
            .await;
            progress.dismiss();
            busy.set(false);
            let current = weak.upgrade().filter(|ui| {
                ui.generation.get() == generation
                    && ui.state.borrow().account == owner
                    && ui.state.borrow().current().jid == peer
                    && ui.composer.is_mapped()
            });
            match (current, result) {
                (Some(ui), Ok(path)) => {
                    if !ui.stage_attachment_files(vec![gio::File::for_path(&path)]) {
                        let _ = gio::spawn_blocking(move || std::fs::remove_file(path)).await;
                        ui.notice("The pasted image could not be attached");
                    }
                }
                (None, Ok(path)) => {
                    let _ = gio::spawn_blocking(move || std::fs::remove_file(path)).await;
                }
                (Some(ui), Err(reason)) => ui.notice(reason),
                (None, Err(_)) => {}
            }
        });
    });
    let view = ui.composer.downgrade();
    ui.composer.clipboard().connect_changed(move |_| {
        if let Some(view) = view.upgrade() {
            enable_media_paste(&view);
        }
    });
    enable_media_paste(&ui.composer);
    let pointer = gtk::GestureClick::new();
    pointer.set_button(gdk::BUTTON_SECONDARY);
    pointer.set_propagation_phase(gtk::PropagationPhase::Capture);
    let view = ui.composer.downgrade();
    pointer.connect_pressed(move |_, _, _, _| {
        if let Some(view) = view.upgrade() {
            after_popup(&view);
        }
    });
    ui.composer.add_controller(pointer);
    let keys = gtk::EventControllerKey::new();
    keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    let view = ui.composer.downgrade();
    keys.connect_key_pressed(move |_, key, _, state| {
        if key == gdk::Key::Menu
            || key == gdk::Key::F10 && state.contains(gdk::ModifierType::SHIFT_MASK)
        {
            if let Some(view) = view.upgrade() {
                after_popup(&view);
            }
        }
        glib::Propagation::Proceed
    });
    ui.composer.add_controller(keys);
}

fn snapshot(owner: &str, texture: &gdk::Texture) -> Result<std::path::PathBuf, &'static str> {
    use std::{
        fs,
        io::Write,
        os::unix::fs::{OpenOptionsExt, PermissionsExt},
    };
    let bytes = texture.save_to_png_bytes();
    if bytes.is_empty() || bytes.len() > 64 * 1024 * 1024 {
        return Err("The pasted image is too large");
    }
    let _guard = crate::downloads::CACHE_IO.lock().unwrap();
    let root = crate::downloads::account_root(owner)?;
    let directory = root
        .join("clipboard")
        .join(uuid::Uuid::new_v4().to_string());
    fs::create_dir_all(&directory).map_err(|_| "Image storage is unavailable")?;
    for path in [&root, &directory] {
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))
            .map_err(|_| "Image storage is unavailable")?;
    }
    let path = directory.join("image.png");
    let partial = path.with_extension("part");
    let result = (|| {
        let mut output = fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(&partial)
            .map_err(|_| "Image storage is unavailable")?;
        output
            .write_all(&bytes)
            .and_then(|_| output.sync_all())
            .map_err(|_| "The pasted image could not be saved")?;
        fs::rename(&partial, &path).map_err(|_| "The pasted image could not be saved")?;
        Ok(path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(partial);
    }
    result
}

impl Ui {
    pub(crate) fn copy_attachment(self: &Rc<Self>, id: &str) {
        let owner = self.state.borrow().account.clone();
        let peer = self.state.borrow().current().jid.clone();
        let Some(file) = self
            .store
            .message(&owner, &peer, id)
            .filter(|message| !message.retracted)
            .and_then(|message| message.attachment)
        else {
            return;
        };
        let Some(crate::downloads::Status::Ready(path)) =
            self.downloads.borrow().status(&owner, &file)
        else {
            self.notice("Download this media before copying it");
            return;
        };
        let Some(cancel) = self.downloads.borrow().token(&owner, &file) else {
            return;
        };
        self.notice("Copying media…");
        let generation = self.generation.get();
        let weak = Rc::downgrade(self);
        let id = id.to_owned();
        let expected = file.clone();
        let account = owner.clone();
        glib::spawn_future_local(async move {
            let copied = gio::spawn_blocking(move || -> Result<_, &'static str> {
                use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
                let input = crate::downloads::verified_input(&path, &expected, &cancel)?;
                let root = crate::downloads::account_root(&account)?.join("clipboard-media");
                std::fs::create_dir_all(&root).map_err(|_| "Clipboard storage is unavailable")?;
                std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700))
                    .map_err(|_| "Clipboard storage is unavailable")?;
                let directory = root.join(uuid::Uuid::new_v4().to_string());
                std::fs::create_dir(&directory).map_err(|_| "Clipboard storage is unavailable")?;
                std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700))
                    .map_err(|_| "Clipboard storage is unavailable")?;
                let name = std::path::Path::new(&expected.name)
                    .file_name()
                    .filter(|name| !name.is_empty())
                    .unwrap_or_else(|| std::ffi::OsStr::new("media"));
                let destination = directory.join(name);
                let mut output = std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(0o600)
                    .open(&destination)
                    .map_err(|_| "Clipboard media could not be saved")?;
                let mut source = input.reopen()?.0;
                std::io::copy(&mut source, &mut output)
                    .map_err(|_| "Clipboard media could not be saved")?;
                let texture = if expected.mime.starts_with("image/") {
                    let cancel = gio::Cancellable::new();
                    let decoder = if expected.sticker {
                        crate::image_loader::Decoder::open_sticker(input, &cancel)
                    } else {
                        crate::image_loader::Decoder::open(input, &cancel)
                    };
                    decoder
                        .and_then(|mut decoder| decoder.frame(4096))
                        .ok()
                        .map(|frame| frame.texture)
                } else {
                    None
                };
                Ok((destination, texture))
            })
            .await;
            let Some(ui) = weak.upgrade().filter(|ui| {
                ui.generation.get() == generation && ui.state.borrow().account == owner
            }) else {
                return;
            };
            let Ok(Ok((path, texture))) = copied else {
                ui.notice("Could not copy this media. Check its download and free space.");
                return;
            };
            if ui
                .store
                .message(&owner, &peer, &id)
                .filter(|message| !message.retracted)
                .and_then(|message| message.attachment)
                .is_none_or(|current| current.key() != file.key())
            {
                return;
            }
            let files = gdk::FileList::from_array(&[gio::File::for_path(path)]);
            let mut providers = vec![gdk::ContentProvider::for_value(&files.to_value())];
            if let Some(texture) = texture {
                providers.push(gdk::ContentProvider::for_value(&texture.to_value()));
            }
            if ui
                .window
                .clipboard()
                .set_content(Some(&gdk::ContentProvider::new_union(&providers)))
                .is_err()
            {
                ui.notice("The clipboard is unavailable");
            } else {
                ui.notice("Media copied");
            }
        });
    }
}
