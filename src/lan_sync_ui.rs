use crate::{lan_sync, ui::Ui};
use adw::prelude::*;
use gtk::{gio, glib};
use std::{cell::Cell, rc::Rc, sync::Arc, time::Duration};

pub fn install(ui: &Rc<Ui>) {
    let weak = Rc::downgrade(ui);
    glib::timeout_add_local_once(Duration::from_millis(100), move || {
        let Some(ui) = weak.upgrade() else {
            return;
        };
        if ui.smoke || ui.store.pref("lan-sync-enabled", "false") != "true" {
            return;
        }
        let weak = Rc::downgrade(&ui);
        glib::MainContext::default().spawn_local(async move {
            if let Err(error) = lan_sync::start().await {
                if let Some(ui) = weak.upgrade() {
                    ui.notice(&error);
                }
            }
        });
    });
    let weak = Rc::downgrade(ui);
    let seen = Cell::new(lan_sync::REVISION.load(std::sync::atomic::Ordering::Relaxed));
    glib::timeout_add_local(Duration::from_secs(1), move || {
        let Some(ui) = weak.upgrade() else {
            return glib::ControlFlow::Break;
        };
        let revision = lan_sync::REVISION.load(std::sync::atomic::Ordering::Relaxed);
        if seen.replace(revision) != revision {
            ui.rebuild_pack_library();
            ui.rebuild_gif_library();
            refresh_folders(&ui);
        }
        glib::ControlFlow::Continue
    });
}
pub fn add_preferences(ui: &Rc<Ui>, dialog: &adw::PreferencesDialog) {
    build_page(ui, dialog);
}
pub fn show(ui: &Rc<Ui>) {
    let dialog = adw::PreferencesDialog::builder().title("Device Sync").search_enabled(false).build();
    build_page(ui, &dialog);
    ui.dialogs.borrow_mut().push(dialog.clone().upcast());
    dialog.present(Some(&ui.window));
}
fn pairing_row(title: &str, icon: &str) -> adw::ActionRow {
    let row = adw::ActionRow::builder().title(title).activatable(true).build();
    row.add_prefix(&gtk::Image::from_icon_name(icon));
    row.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
    row
}
fn build_page(ui: &Rc<Ui>, dialog: &adw::PreferencesDialog) {
    let page = adw::PreferencesPage::builder().title("Devices").icon_name("network-workgroup-symbolic").build();
    let group=adw::PreferencesGroup::builder().title("Device Sync").description("Keep your accounts, packs, folders and wallets in sync. Connect your devices to the same local network. Only pair devices you own.").build();
    let enabled = adw::SwitchRow::builder()
        .title("Enable device sync")
        .active(
            lan_sync::current().is_some() || ui.store.pref("lan-sync-enabled", "false") == "true",
        )
        .build();
    group.add(&enabled);
    let status = adw::ActionRow::builder()
        .title(if enabled.is_active(){"Starting…"}else{"Device sync is off"})
        .title_lines(0).use_markup(false).build();
    group.add(&status);
    let spinner = gtk::Spinner::new();
    status.add_prefix(&spinner);
    spinner.set_valign(gtk::Align::Center);
    spinner.bind_property("spinning", &spinner, "visible").sync_create().build();
    page.add(&group);
    let actions = adw::PreferencesGroup::builder()
        .title("Pair Another Device")
        .build();
    let display = pairing_row("Show Pairing QR Code", "view-grid-symbolic");
    let scan = pairing_row("Scan QR from Image…", "camera-photo-symbolic");
    let code = adw::EntryRow::builder()
        .title("Or paste a pairing code")
        .show_apply_button(true)
        .build();
    let typed = pairing_row("Show Rotating Pairing Code", "dialog-password-symbolic");
    let enter = pairing_row("Enter an Eight-Digit Code…", "input-keyboard-symbolic");
    actions.add(&display);
    actions.add(&scan);
    actions.add(&typed);
    actions.add(&enter);
    actions.add(&code);
    let weak = Rc::downgrade(ui);
    let active = enabled.clone();
    let s = status.clone();
    typed.connect_activated(move |_| {
        if let Some(ui) = weak.upgrade() {
            active.set_active(true);
            numeric_show(&ui, &s);
        }
    });
    let weak = Rc::downgrade(ui);
    let active = enabled.clone();
    let s = status.clone();
    enter.connect_activated(move |_| {
        if let Some(ui) = weak.upgrade() {
            active.set_active(true);
            numeric_join(&ui, &s);
        }
    });
    page.add(&actions);
    let peers = adw::PreferencesGroup::builder()
        .title("Paired Devices")
        .description(
            "Pairings are direct links. Devices can also sync through your other paired devices.",
        )
        .build();
    peers.set_visible(false);
    page.add(&peers);
    let fingerprints=adw::PreferencesGroup::builder().title("Synced Fingerprints").description("Compare these with your other devices to verify their encryption identities.").build();
    let owner = ui.state.borrow().account.clone();
    let records=lan_sync::public_records(&ui.store, &format!("synced-fingerprint:{owner}:"));
    fingerprints.set_visible(!records.is_empty());
    for value in records {
        let row = adw::ActionRow::builder()
            .title(format!(
                "{} · device {}",
                value["mode"].as_str().unwrap_or("OMEMO"),
                value["device"]
            ))
            .subtitle(value["fingerprint"].as_str().unwrap_or(""))
            .use_markup(false)
            .subtitle_selectable(true)
            .build();
        fingerprints.add(&row);
    }
    page.add(&fingerprints);
    dialog.add(&page);
    let weak = Rc::downgrade(ui);
    let s = status.clone();
    let progress = spinner.clone();
    enabled.connect_active_notify(move |row| {
        let Some(ui) = weak.upgrade() else {
            return;
        };
        let _ = ui.store.set_pref(
            "lan-sync-enabled",
            if row.is_active() { "true" } else { "false" },
        );
        if !row.is_active() {
            lan_sync::stop();
            s.set_title("Device sync is off.");
            return;
        }
        s.set_title("Starting encrypted sync…");
        progress.start();
        let s = s.clone();
        let progress = progress.clone();
        glib::MainContext::default().spawn_local(async move {
            let result = lan_sync::start().await;
            progress.stop();
            s.set_title(result.err().as_deref().unwrap_or("Ready to pair."));
        });
    });
    let weak = Rc::downgrade(ui);
    let status_copy = status.clone();
    let enabled_copy = enabled.clone();
    let progress = spinner.clone();
    display.connect_activated(move |_| {
        let Some(ui) = weak.upgrade() else {
            return;
        };
        enabled_copy.set_active(true);
        status_copy.set_title("Preparing a one-time pairing code…");
        progress.start();
        let s = status_copy.clone();
        let p = progress.clone();
        let weak = Rc::downgrade(&ui);
        glib::MainContext::default().spawn_local(async move {
            let result = lan_sync::start().await;
            let Some(ui) = weak.upgrade() else {
                return;
            };
            match result {
                Ok(mesh) => {
                    let m = mesh.clone();
                    match gio::spawn_blocking(move || m.invite()).await {
                        Ok(Ok(code)) => pair_qr(&ui, mesh, &code),
                        _ => s.set_title("Connect both devices to the same local network."),
                    }
                }
                Err(error) => s.set_title(&error),
            }
            p.stop();
        });
    });
    let weak = Rc::downgrade(ui);
    let s = status.clone();
    let e = enabled.clone();
    let p = spinner.clone();
    code.connect_apply(move |entry| {
        let Some(ui) = weak.upgrade() else {
            return;
        };
        let value = entry.text().trim().to_string();
        entry.set_text("");
        e.set_active(true);
        join(&ui, value, &s, &p);
    });
    let weak = Rc::downgrade(ui);
    let s = status.clone();
    let e = enabled.clone();
    let p = spinner.clone();
    scan.connect_activated(move |_| {
        let Some(ui) = weak.upgrade() else {
            return;
        };
        let weak = Rc::downgrade(&ui);
        let s = s.clone();
        let e = e.clone();
        let p = p.clone();
        let parent = ui.window.clone();
        glib::MainContext::default().spawn_local(async move {
            let chooser = gtk::FileDialog::builder()
                .title("Scan Monogram Pairing QR")
                .build();
            let filter = gtk::FileFilter::new();
            filter.add_pixbuf_formats();
            let filters = gio::ListStore::new::<gtk::FileFilter>();
            filters.append(&filter);
            chooser.set_filters(Some(&filters));
            let Ok(file) = chooser.open_future(Some(&parent)).await else {
                return;
            };
            let Some(path) = file.path() else {
                s.set_title("Choose a local image containing the pairing QR.");
                return;
            };
            s.set_title("Scanning pairing QR…");
            p.start();
            let decoded = gio::spawn_blocking(move || decode_qr(path)).await;
            let Some(ui) = weak.upgrade() else {
                return;
            };
            match decoded {
                Ok(Ok(value)) => {
                    e.set_active(true);
                    join(&ui, value, &s, &p);
                }
                _ => {
                    p.stop();
                    s.set_title("No Monogram pairing QR was found in the image.");
                }
            }
        });
    });
    let parent = dialog.downgrade();
    let s = status.downgrade();
    let active = enabled.downgrade();
    let polling = Rc::new(Cell::new(false));
    glib::timeout_add_local(Duration::from_secs(1), move || {
        let (Some(dialog), Some(status), Some(enabled)) =
            (parent.upgrade(), s.upgrade(), active.upgrade())
        else {
            return glib::ControlFlow::Break;
        };
        if !dialog.is_visible() {
            return glib::ControlFlow::Break;
        }
        if polling.replace(true) {
            return glib::ControlFlow::Continue;
        }
        let polling = polling.clone();
        glib::MainContext::default().spawn_local(async move {
            if let Some(mesh) = lan_sync::current() {
                let summary = lan_sync::summary(mesh).await;
                status.set_title(summary["status"].as_str().unwrap_or("Syncing…"));
            } else if !enabled.is_active() {
                status.set_title("Device sync is off.");
            }
            polling.set(false);
        });
        glib::ControlFlow::Continue
    });
    // Build peer rows without exposing bearer credentials.
    let parent = dialog.downgrade();
    let group = peers.downgrade();
    let rows = Rc::new(std::cell::RefCell::new(Vec::<adw::ActionRow>::new()));
    let fingerprint = Rc::new(std::cell::RefCell::new(String::new()));
    let polling = Rc::new(Cell::new(false));
    glib::timeout_add_local(Duration::from_secs(2), move || {
        let (Some(dialog), Some(group)) = (parent.upgrade(), group.upgrade()) else {
            return glib::ControlFlow::Break;
        };
        if !dialog.is_visible() {
            return glib::ControlFlow::Break;
        }
        if polling.replace(true) {
            return glib::ControlFlow::Continue;
        }
        let rows = rows.clone();
        let fingerprint = fingerprint.clone();
        let polling = polling.clone();
        glib::MainContext::default().spawn_local(async move {
            let summary = if let Some(mesh) = lan_sync::current() {
                lan_sync::summary(mesh).await
            } else {
                serde_json::json!({"peers":[]})
            };
            group.set_visible(summary["peers"].as_array().is_some_and(|p|!p.is_empty()));
            let peers = summary["peers"].to_string();
            if *fingerprint.borrow() != peers {
                fingerprint.replace(peers);
                for row in rows.borrow_mut().drain(..) {
                    group.remove(&row);
                }
                for peer in summary["peers"].as_array().into_iter().flatten() {
                    let row = adw::ActionRow::builder()
                        .title(peer["name"].as_str().unwrap_or("Device"))
                        .use_markup(false)
                        .build();
                    let remove = gtk::Button::from_icon_name("edit-delete-symbolic");
                    remove.set_tooltip_text(Some("Unpair device"));
                    let id = peer["id"].as_str().unwrap_or("").to_string();
                    remove.connect_clicked(move |button| {
                        button.set_sensitive(false);
                        let id = id.clone();
                        glib::MainContext::default().spawn_local(async move {
                            if let Some(mesh) = lan_sync::current() {
                                let _ = gio::spawn_blocking(move || mesh.forget(&id)).await;
                            }
                        });
                    });
                    row.add_suffix(&remove);
                    group.add(&row);
                    rows.borrow_mut().push(row);
                }
            }
            polling.set(false);
        });
        glib::ControlFlow::Continue
    });
}
fn join(ui: &Rc<Ui>, code: String, status: &adw::ActionRow, spinner: &gtk::Spinner) {
    status.set_title("Connecting to the paired device…");
    spinner.start();
    let s = status.clone();
    let p = spinner.clone();
    let weak = Rc::downgrade(ui);
    glib::MainContext::default().spawn_local(async move {
        let result = match lan_sync::start().await {
            Ok(mesh) => gio::spawn_blocking(move || mesh.join(&code))
                .await
                .map_err(|_| "Pairing failed.".to_string())
                .and_then(|v| v),
            Err(error) => Err(error),
        };
        if weak.upgrade().is_none() {
            return;
        }
        p.stop();
        s.set_title(match &result {
            Ok(_) => "Paired. Your settings are syncing.",
            Err(error) => error,
        });
    });
}
fn pair_qr(ui: &Rc<Ui>, mesh: Arc<monogram_sync::Mesh>, value: &str) {
    let Ok(qr) = qrcode::QrCode::new(value.as_bytes()) else {
        ui.notice("Could not create pairing QR.");
        return;
    };
    let width = qr.width();
    let pixels = qr.to_colors();
    let drawing = gtk::DrawingArea::builder()
        .content_width(360)
        .content_height(360)
        .build();
    drawing.set_draw_func(move |_, cr, w, h| {
        cr.set_source_rgb(1., 1., 1.);
        let _ = cr.paint();
        let scale = f64::from(w.min(h)) / (width + 8) as f64;
        cr.set_source_rgb(0., 0., 0.);
        for y in 0..width {
            for x in 0..width {
                if pixels[y * width + x] == qrcode::Color::Dark {
                    cr.rectangle((x + 4) as f64 * scale, (y + 4) as f64 * scale, scale, scale);
                }
            }
        }
        let _ = cr.fill();
    });
    let body = crate::widgets::bx(true, 12);
    crate::widgets::margins(&body, 18);
    body.append(&drawing);
    body.append(&crate::widgets::wrap(
        "Scan from Device Sync on your other device. This one-time code expires in two minutes.",
        "",
    ));
    let copy = gtk::Button::with_label("Copy Pairing Code");
    let value = value.to_string();
    copy.connect_clicked(move |b| b.clipboard().set_text(&value));
    body.append(&copy);
    let (dialog, _) = ui.dialog("Pair a Device", &body, 400, 500);
    let weak = Arc::downgrade(&mesh);
    dialog.connect_closed(move |_| {
        if let Some(mesh) = weak.upgrade() {
            mesh.cancel_invite();
        }
    });
}
fn decode_qr(path: std::path::PathBuf) -> Result<String, String> {
    let file = std::fs::File::open(path).map_err(|_| "Could not read image.")?;
    let cancel = gio::Cancellable::new();
    let mut decoder =
        crate::image_loader::Decoder::open(crate::downloads::VerifiedFile(file), &cancel)
            .map_err(str::to_owned)?;
    let frame = decoder.frame(2048).map_err(str::to_owned)?;
    let texture = frame.texture;
    let (w, h) = (texture.width() as usize, texture.height() as usize);
    if w * h > 4_194_304 {
        return Err("QR image is too large.".into());
    }
    let mut rgba = vec![0; w * h * 4];
    texture.download(&mut rgba, w * 4);
    let gray: Vec<_> = rgba
        .chunks_exact(4)
        .map(|p| ((u16::from(p[0]) + u16::from(p[1]) + u16::from(p[2])) / 3) as u8)
        .collect();
    let mut decoder = quircs::Quirc::default();
    for code in decoder.identify(w, h, &gray).flatten() {
        if let Ok(data) = code.decode() {
            if let Ok(value) = String::from_utf8(data.payload) {
                if value.starts_with("monogram-sync:") {
                    return Ok(value);
                }
            }
        }
    }
    Err("No pairing QR found.".into())
}

fn refresh_folders(ui: &Rc<Ui>) {
    let owner = ui.state.borrow().account.clone();
    let data = ui.store.pref(&format!("folders:{owner}"), "");
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&data) else {
        return;
    };
    let (Ok(rules), Ok(order)) = (
        serde_json::from_value(value["rules"].clone()),
        serde_json::from_value(value["order"].clone()),
    ) else {
        return;
    };
    let mut state = ui.state.borrow_mut();
    state.folders = rules;
    state.folder_order = order;
    state.all_folder_hidden = value["all_hidden"].as_bool().unwrap_or(false);
    if state.visible_folder_names().is_empty() {
        state.all_folder_hidden = false;
    }
    if !state.folder_visible(&state.folder) {
        state.folder = state.visible_folder_names()[0].clone();
    }
    ui.folder_snapshot.replace(folder_signature(&state));
    drop(state);
    ui.rebuild_folders();
    ui.refresh_chats(None);
}
fn numeric_show(ui: &Rc<Ui>, status: &adw::ActionRow) {
    numeric_dialog(ui, status, true);
}
fn numeric_join(ui: &Rc<Ui>, status: &adw::ActionRow) {
    numeric_dialog(ui, status, false);
}
fn numeric_dialog(ui: &Rc<Ui>, status: &adw::ActionRow, display: bool) {
    status.set_title("Starting code pairing…");
    let weak = Rc::downgrade(ui);
    let status = status.clone();
    glib::MainContext::default().spawn_local(async move {
        let mesh = match lan_sync::start().await {
            Ok(m) => m,
            Err(e) => {
                status.set_title(&e);
                return;
            }
        };
        let Some(ui) = weak.upgrade() else {
            return;
        };
        let body = crate::widgets::bx(true, 12);
        crate::widgets::margins(&body, 18);
        let code = gtk::Label::new(Some(if display {
            "Preparing code…"
        } else {
            "Choose the device showing a code"
        }));
        code.add_css_class("title-1");
        code.set_selectable(true);
        body.append(&code);
        let remaining = crate::widgets::wrap(
            "Both devices must be on the same local network. Keep this screen open while pairing.",
            "",
        );
        body.append(&remaining);
        let entry = gtk::Entry::builder()
            .placeholder_text("Eight-digit code")
            .max_length(8)
            .input_purpose(gtk::InputPurpose::Digits)
            .build();
        entry.set_visible(!display);
        body.append(&entry);
        let devices = crate::widgets::bx(true, 6);
        devices.set_visible(!display);
        body.append(&devices);
        let feedback = crate::widgets::wrap(
            if display {
                "Type this rotating code on your other device."
            } else {
                "Looking for nearby devices…"
            },
            "",
        );
        body.append(&feedback);
        let (dialog, _) = ui.dialog(
            if display {
                "Pair with a Rotating Code"
            } else {
                "Enter a Pairing Code"
            },
            &body,
            440,
            400,
        );
        let stop = Arc::downgrade(&mesh);
        dialog.connect_closed(move |_| {
            if let Some(m) = stop.upgrade() {
                m.cancel_invite();
            }
        });
        if display {
            let m = mesh.clone();
            if let Err(e) = gio::spawn_blocking(move || m.numeric_invite())
                .await
                .unwrap_or_else(|_| Err("Could not prepare pairing code.".into()))
            {
                feedback.set_text(&e);
                return;
            }
        }
        let parent = dialog.downgrade();
        let busy = Rc::new(Cell::new(false));
        let previous = Rc::new(std::cell::RefCell::new(String::new()));
        glib::timeout_add_local(Duration::from_secs(1), move || {
            let Some(dialog) = parent.upgrade().filter(|d| d.is_visible()) else {
                return glib::ControlFlow::Break;
            };
            if mesh.is_stopped() {
                return glib::ControlFlow::Break;
            }
            if busy.replace(true) {
                return glib::ControlFlow::Continue;
            }
            let m = mesh.clone();
            let busy = busy.clone();
            let previous = previous.clone();
            let devices = devices.clone();
            let code = code.clone();
            let remaining = remaining.clone();
            let entry = entry.clone();
            let feedback = feedback.clone();
            let parent = dialog.downgrade();
            glib::MainContext::default().spawn_local(async move {
                let inspect = m.clone();
                let result =
                    gio::spawn_blocking(move || (inspect.numeric_status(), inspect.nearby())).await;
                if parent.upgrade().is_none_or(|d| !d.is_visible()) {
                    busy.set(false);
                    return;
                }
                if let Ok((numeric, nearby)) = result {
                    if display {
                        match numeric {
                            Ok(value) => {
                                code.set_text(value["code"].as_str().unwrap_or(""));
                                remaining.set_text(&format!(
                                    "New code in {} seconds · expires after two minutes",
                                    value["remaining"]
                                ));
                            }
                            Err(e) => {
                                code.set_text("Pairing ended");
                                remaining.set_text(&e);
                            }
                        }
                    } else if *previous.borrow() != nearby.to_string() {
                        previous.replace(nearby.to_string());
                        while let Some(child) = devices.first_child() {
                            devices.remove(&child);
                        }
                        let list = nearby.as_array().cloned().unwrap_or_default();
                        feedback.set_text(if list.is_empty() {
                            "Open Show Rotating Pairing Code on your other device."
                        } else {
                            "Enter its code, then choose that device below."
                        });
                        for peer in list {
                            let button =
                                gtk::Button::with_label(peer["name"].as_str().unwrap_or("Device"));
                            let id = peer["id"].as_str().unwrap_or("").to_string();
                            let m = m.clone();
                            let entry = entry.clone();
                            let output = feedback.clone();
                            button.connect_clicked(move |button| {
                                let value = entry.text().to_string();
                                if value.len() != 8 || !value.bytes().all(|b| b.is_ascii_digit()) {
                                    output.set_text("Enter the eight-digit code first.");
                                    return;
                                }
                                button.set_sensitive(false);
                                output.set_text("Verifying pairing code…");
                                let m = m.clone();
                                let id = id.clone();
                                let output = output.clone();
                                let button = button.clone();
                                glib::MainContext::default().spawn_local(async move {
                                    let result =
                                        gio::spawn_blocking(move || m.join_numeric(&id, &value))
                                            .await;
                                    output.set_text(match &result {
                                        Ok(Ok(_)) => "Paired. Your settings are syncing.",
                                        Ok(Err(e)) => e,
                                        _ => "Could not pair. Try again.",
                                    });
                                    button.set_sensitive(true);
                                });
                            });
                            devices.append(&button);
                        }
                    }
                }
                busy.set(false);
            });
            glib::ControlFlow::Continue
        });
    });
}

pub(crate)fn folder_signature(state:&crate::model::State)->String{serde_json::to_string(&(&state.account,&state.folders,&state.folder_order,state.all_folder_hidden)).unwrap()}

#[cfg(test)]
mod review_tests {
    use super::*;
    use crate::{model::Message, widgets::*};
    fn descendants(root: &impl IsA<gtk::Widget>) -> Vec<gtk::Widget> {
        let mut all=vec![];let mut pending=vec![root.clone().upcast::<gtk::Widget>()];
        while let Some(w)=pending.pop(){let mut c=w.first_child();while let Some(v)=c{c=v.next_sibling();pending.push(v);}all.push(w);}all
    }
    fn pump() { let main=glib::MainContext::default();let until=std::time::Instant::now()+Duration::from_millis(500);while std::time::Instant::now()<until{main.iteration(false);std::thread::sleep(Duration::from_millis(1));} }
    #[test]
    #[ignore="requires private GTK display and D-Bus"]
    fn preferences_icons_links_and_documents() {
        let app=adw::Application::builder().application_id("com.negzero.MonogramReviewTest").flags(gio::ApplicationFlags::NON_UNIQUE).build();
        app.register(None::<&gio::Cancellable>).unwrap();
        install_icons();let theme=gtk::IconTheme::for_display(&gtk::gdk::Display::default().unwrap());
        assert!(theme.has_icon("com.negzero.MonogramDesktop"));assert!(theme.has_icon("monogram-monero-symbolic"));
        adw::StyleManager::default().set_color_scheme(adw::ColorScheme::ForceDark);
        let ui=Ui::new(&app,true);adw::StyleManager::default().set_color_scheme(adw::ColorScheme::ForceDark);ui.window.set_default_size(900,800);ui.window.present();
        let css=gtk::CssProvider::new();css.load_from_path(asset("style.css"));gtk::style_context_add_provider_for_display(&gtk::gdk::Display::default().unwrap(),&css,gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
        let dialog=adw::PreferencesDialog::new();add_preferences(&ui,&dialog);crate::monero_settings_ui::add(&ui,&dialog);dialog.present(Some(&ui.window));pump();
        let all=descendants(&dialog);
        let status=all.iter().filter_map(|w|w.downcast_ref::<gtk::Label>()).find(|label|label.text()=="Device sync is off").unwrap();
        assert_eq!(status.layout().line_count(),1,"Short sync status should use the row width");
        for title in ["Show Pairing QR Code","Scan QR from Image…","Show Rotating Pairing Code","Enter an Eight-Digit Code…"] {
            assert!(all.iter().filter_map(|w|w.downcast_ref::<adw::ActionRow>()).any(|row|row.title()==title && row.is_activatable()));
        }
        for title in ["Devices","Monero"] { assert_eq!(all.iter().filter_map(|w|w.downcast_ref::<adw::PreferencesPage>()).filter(|p|p.title()==title).count(),1); }
        assert!(!all.iter().filter_map(|w|w.downcast_ref::<adw::ActionRow>()).any(|r|r.title()=="Manage Paired Devices"));
        assert!(all.iter().filter_map(|w|w.downcast_ref::<adw::ComboRow>()).any(|row|row.title()=="Node"));
        if let Some(path)=std::env::var_os("MONOGRAM_REVIEW_CAPTURE") {
            fn capture(window:&impl IsA<gtk::Window>,path:std::path::PathBuf){let w=window.upcast_ref::<gtk::Window>();let shot=gtk::Snapshot::new();gtk::WidgetPaintable::new(Some(w)).snapshot(&shot,w.width() as f64,w.height() as f64);if let Some(node)=shot.to_node(){w.renderer().unwrap().render_texture(&node,None).save_to_png(path).unwrap();}}
            capture(&ui.window,std::path::Path::new(&path).with_extension("devices.png"));
            let page=all.iter().filter_map(|w|w.downcast_ref::<adw::PreferencesPage>()).find(|p|p.title()=="Monero").unwrap();dialog.set_visible_page(page);pump();
            capture(&ui.window,std::path::Path::new(&path).with_extension("monero.png"));
        }
        dialog.close();pump();
        let root=bx(true,12);
        let source="A short message with https://litterbox.catbox.moe/";
        let body=wrap(source,"message-text");let status=bx(false,0);status.append(&text("13:31","message-meta"));
        let rich=crate::formatting_ui::display(&Message{text:source.into(),..Default::default()}).1;
        let message=MessageText::with_rich(&body,&status,rich);root.append(&message);
        let file:crate::file_share::Attachment=serde_json::from_value(serde_json::json!({"wire_url":"https://example.invalid/log.zip","name":"logs (1).zip","mime":"application/zip","size":2048,"hashes":{},"manual":true,"sticker":false})).unwrap();
        let attachment=ui.attachment_content(&Message{id:"file-review".into(),kind:"file".into(),attachment:Some(file),..Default::default()});root.append(&attachment);
        let window=gtk::Window::builder().default_width(330).child(&root).build();window.present();pump();
        let layout=body.layout();let link=source.find("https://").unwrap();
        assert_eq!(layout.index_to_line_x(link as i32,false).0,layout.index_to_line_x((source.len()-1)as i32,false).0,"A fitting URL must move together to the next line");
        assert!(attachment.height()<80,"Document row grew to {}",attachment.height());
        eprintln!("Native review: icons resolve, Devices is inline, four activatable rows, mobile node chooser, intact fitting link, document height={}px",attachment.height());
        window.close();ui.window.close();
    }
}
