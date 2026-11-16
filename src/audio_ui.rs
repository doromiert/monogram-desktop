//! On-demand playback of authenticated audio files, including mobile voice messages.
use crate::{
    downloads::{self, Status},
    model::Message,
    ui::Ui,
    widgets::*,
};
use adw::prelude::*;
use gtk::{gio, glib};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    time::Duration,
};
impl Ui {
    pub(crate) fn audio_attachment(self: &Rc<Self>, message: &Message) -> gtk::Box {
        let file = message.attachment.as_ref().unwrap().clone();
        let owner = self.state.borrow().account.clone();
        let generation = self.generation.get();
        let auto_flags = (message.own, message.encrypted);
        let body = bx(true, 6);
        body.add_css_class("audio-attachment");
        body.set_width_request(240);
        let row = bx(false, 8);
        let action = gtk::Button::from_icon_name("media-playback-start-symbolic");
        action.set_tooltip_text(Some("Play audio"));
        action.add_css_class("circular");
        row.append(&action);
        let words = bx(true, 2);
        words.set_hexpand(true);
        let name = text(&file.name, "heading");
        name.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
        name.set_max_width_chars(28);
        name.set_xalign(0.);
        words.append(&name);
        let status = text("Audio", "caption dim-label");
        status.set_xalign(0.);
        words.append(&status);
        row.append(&words);
        let cancel = gtk::Button::from_icon_name("process-stop-symbolic");
        cancel.set_tooltip_text(Some("Cancel download"));
        cancel.add_css_class("flat");
        cancel.set_visible(false);
        row.append(&cancel);
        let save = gtk::Button::from_icon_name("document-save-symbolic");
        save.set_tooltip_text(Some("Save Audio As…"));
        save.add_css_class("flat");
        save.set_visible(false);
        row.append(&save);
        body.append(&row);
        let weak = Rc::downgrade(self);
        let account = owner.clone();
        let source = file.clone();
        let id = message.id.clone();
        save.connect_clicked(move |_| {
            if let Some(ui) = weak
                .upgrade()
                .filter(|ui| ui.state.borrow().account == account)
            {
                let state = ui.downloads.borrow().status(&account, &source);
                if let Some(Status::Ready(path)) = state {
                    ui.save_attachment(&id, &path);
                }
            }
        });
        let progress = gtk::ProgressBar::new();
        progress.set_visible(false);
        body.append(&progress);
        let controls = gtk::MediaControls::new(None::<&gtk::MediaStream>);
        controls.set_visible(false);
        body.append(&controls);
        let player: Rc<RefCell<Option<Rc<crate::video_ui::Playback>>>> =
            Rc::new(RefCell::new(None));
        let requested = Rc::new(Cell::new(false));
        let busy = Rc::new(Cell::new(false));
        let weak = Rc::downgrade(self);
        let account = owner.clone();
        let source = file.clone();
        let want = requested.clone();
        action.connect_clicked(move |_| {
            if let Some(ui) = weak.upgrade().filter(|ui| {
                ui.generation.get() == generation && ui.state.borrow().account == account
            }) {
                want.set(true);
                if !matches!(
                    ui.downloads.borrow().status(&account, &source),
                    Some(Status::Ready(_) | Status::Running { .. })
                ) {
                    if let Err(error) = ui.downloads.borrow_mut().start(&account, source.clone()) {
                        want.set(false);
                        ui.notice(error);
                    }
                }
            }
        });
        let weak = Rc::downgrade(self);
        let account = owner.clone();
        let source = file.clone();
        let want = requested.clone();
        cancel.connect_clicked(move |_| {
            want.set(false);
            if let Some(ui) = weak
                .upgrade()
                .filter(|ui| ui.state.borrow().account == account)
            {
                ui.downloads.borrow_mut().cancel(&account, &source);
            }
        });
        let current = player.clone();
        let c = controls.clone();
        let a = action.clone();
        body.connect_unmap(move |_| {
            if let Some(player) = current.borrow_mut().take() {
                player.media.pause();
            }
            c.set_media_stream(None::<&gtk::MediaStream>);
            c.set_visible(false);
            a.set_visible(true);
        });
        let weak = Rc::downgrade(self);
        let widget = body.downgrade();
        glib::timeout_add_local(Duration::from_millis(100), move || {
            let (Some(ui), Some(widget)) = (weak.upgrade(), widget.upgrade()) else {
                return glib::ControlFlow::Break;
            };
            if ui.generation.get() != generation || ui.state.borrow().account != owner {
                return glib::ControlFlow::Break;
            }
            if !widget.is_mapped() {
                return glib::ControlFlow::Continue;
            }
            let state = ui.downloads.borrow().status(&owner, &file);
            if state.is_none()
                && ui.auto_download_enabled.get()
                && !file.manual
                && ui.in_chat_viewport(&widget)
                && crate::attachment_ui::automatic_media_allowed(
                    ui.smoke,
                    auto_flags.0,
                    auto_flags.1,
                    ui.state.borrow().current(),
                )
            {
                ui.downloads.borrow_mut().start_automatic(
                    &owner,
                    file.clone(),
                    ui.auto_download_limit.get(),
                );
            }
            let running = matches!(state, Some(Status::Running { .. }));
            save.set_visible(matches!(state, Some(Status::Ready(_))));
            if player
                .borrow()
                .as_ref()
                .is_some_and(|p| p.media.error().is_some())
            {
                status.set_label("Could not decode this audio. Save it to open in another app.");
                status.set_wrap(true);
                status.set_max_width_chars(32);
            }
            cancel.set_visible(running || busy.get());
            progress.set_visible(running);
            action.set_sensitive(!running && !busy.get());
            match state {
                Some(Status::Ready(path)) if requested.replace(false) && !busy.replace(true) => {
                    status.set_label("Opening audio…");
                    let weak = Rc::downgrade(&ui);
                    let owner = owner.clone();
                    let source = file.clone();
                    let player = player.clone();
                    let status = status.clone();
                    let controls = controls.clone();
                    let action = action.clone();
                    let busy = busy.clone();
                    let widget = widget.downgrade();
                    let cancel = ui
                        .downloads
                        .borrow()
                        .token(&owner, &file)
                        .unwrap_or_else(|| {
                            std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false))
                        });
                    glib::spawn_future_local(async move {
                        let result=gio::spawn_blocking(move||{if !crate::video_ui::initialize(){return Err("Audio playback is unavailable. Restart Monogram and try again.");}downloads::verified_input(&path,&source,&cancel)}).await;
                        busy.set(false);
                        let Some(ui) = weak.upgrade().filter(|ui| {
                            ui.generation.get() == generation && ui.state.borrow().account == owner
                        }) else {
                            return;
                        };
                        if !widget.upgrade().is_some_and(|w| w.is_mapped()) {
                            return;
                        }
                        match result
                            .ok()
                            .and_then(Result::ok)
                            .and_then(crate::video_ui::Playback::new_audio)
                        {
                            Some(audio) => {
                                if let Some(previous) = ui
                                    .active_audio
                                    .borrow_mut()
                                    .take()
                                    .and_then(|p| p.upgrade())
                                {
                                    previous.media.pause();
                                }
                                ui.active_audio.replace(Some(Rc::downgrade(&audio)));
                                controls.set_media_stream(Some(&audio.media));
                                controls.set_visible(true);
                                action.set_visible(false);
                                status.set_label("Audio");
                                audio.media.play();
                                player.replace(Some(audio));
                            }
                            None => {
                                status.set_label("Could not play this audio. Download it again or save it to open in another app.");
                                status.set_wrap(true);
                                status.set_max_width_chars(32);
                            }
                        }
                    });
                }
                Some(Status::Running {
                    bytes,
                    total,
                    reconnecting,
                }) => {
                    status.set_label(&if reconnecting {
                        "Reconnecting…".into()
                    } else {
                        format!(
                            "{}{}",
                            glib::format_size(bytes),
                            total.map_or(String::new(), |n| format!(" / {}", glib::format_size(n)))
                        )
                    });
                    if let Some(total) = total.filter(|n| *n > 0) {
                        progress.set_fraction((bytes as f64 / total as f64).clamp(0., 1.));
                    } else {
                        progress.pulse();
                    }
                }
                Some(Status::Failed(error)) => {
                    requested.set(false);
                    status.set_label(error);
                    status.set_wrap(true);
                    status.set_max_width_chars(32);
                }
                Some(Status::Cancelled) => {
                    requested.set(false);
                    status.set_label("Download cancelled. Press Play to retry.");
                }
                _ => {}
            }
            glib::ControlFlow::Continue
        });
        body
    }
}
