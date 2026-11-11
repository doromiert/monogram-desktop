//! Image clipboard input shares the durable media outbox and leaves the text draft alone.
use crate::ui::Ui;
use adw::prelude::*;
use gtk::{gdk, gio, glib};
use std::{cell::Cell, rc::Rc};

fn has_image(clipboard: &gdk::Clipboard) -> bool {
    clipboard
        .formats()
        .contains_type(gdk::Texture::static_type())
        || clipboard
            .formats()
            .mime_types()
            .iter()
            .any(|mime| mime.starts_with("image/"))
}

fn enable_image_paste(view: &gtk::TextView) {
    if view.is_editable() && has_image(&view.clipboard()) {
        view.action_set_enabled("clipboard.paste", true);
    }
}

fn after_popup(view: &gtk::TextView) {
    // GTK enables its native Paste menu item for strings only while constructing the popup.
    let view = view.downgrade();
    glib::idle_add_local_once(move || {
        if let Some(view) = view.upgrade() {
            enable_image_paste(&view);
        }
    });
}

pub fn attach(ui: &Rc<Ui>) {
    let busy = Rc::new(Cell::new(false));
    let weak = Rc::downgrade(ui);
    ui.composer.connect_paste_clipboard(move |view| {
        let clipboard = view.clipboard();
        if !has_image(&clipboard) {
            return;
        }
        view.stop_signal_emission_by_name("paste-clipboard");
        let Some(ui) = weak.upgrade() else { return };
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
        ui.notice("Preparing pasted image…");
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
            busy.set(false);
            let current = weak.upgrade().filter(|ui| {
                ui.generation.get() == generation
                    && ui.state.borrow().account == owner
                    && ui.state.borrow().current().jid == peer
                    && ui.composer.is_mapped()
            });
            match (current, result) {
                (Some(ui), Ok(path)) => {
                    if !ui.send_library_file(&path, false) {
                        let _ = gio::spawn_blocking(move || std::fs::remove_file(path)).await;
                        ui.notice("The pasted image could not be queued");
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
            enable_image_paste(&view);
        }
    });
    enable_image_paste(&ui.composer);
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
    let directory = root.join("clipboard");
    fs::create_dir_all(&directory).map_err(|_| "Image storage is unavailable")?;
    for path in [&root, &directory] {
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))
            .map_err(|_| "Image storage is unavailable")?;
    }
    let path = directory.join(format!("{}.png", uuid::Uuid::new_v4()));
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
