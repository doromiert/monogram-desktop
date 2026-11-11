use crate::{downloads::Status, model::Message, ui::Ui, widgets::*};
use adw::prelude::*;
use gtk::{gio, glib};
use std::{
    cell::Cell,
    rc::Rc,
    time::{Duration, Instant},
};
impl Ui {
    pub fn attachment_content(self: &Rc<Self>, message: &Message) -> gtk::Box {
        let file = message.attachment.as_ref().unwrap().clone();
        if file.media_candidate() {
            return self.media_attachment(message);
        }
        let owner = self.state.borrow().account.clone();
        let id = message.id.clone();
        let media = file.media_candidate();
        let content = bx(true, 8);
        let controls = bx(true, 8);
        controls.add_css_class("attachment-controls");
        if file.video_candidate() {
            self.attach_video(message, &content, &controls);
        } else {
            self.attach_image(message, &content, &controls);
        }
        content.append(&controls);
        let line = bx(false, 10);
        let icon = if media {
            "image-x-generic-symbolic"
        } else {
            "document-save-symbolic"
        };
        let image = gtk::Image::from_icon_name(icon);
        image.set_pixel_size(28);
        line.append(&image);
        let words = bx(true, 2);
        words.append(&wrap(
            if media {
                file.image_label()
            } else {
                &file.name
            },
            "heading",
        ));
        let detail = match (media, file.size) {
            (true, Some(size)) => glib::format_size(size).to_string(),
            (true, None) => String::new(),
            (false, Some(size)) => format!("{} · {}", glib::format_size(size), file.mime),
            (false, None) => file.mime.clone(),
        };
        let info = wrap(&detail, "caption dim-label");
        info.set_visible(!detail.is_empty());
        words.append(&info);
        line.append(&words);
        let action = gtk::Button::with_label("Download");
        action.set_valign(gtk::Align::Center);
        line.append(&action);
        controls.append(&line);
        let progress = gtk::ProgressBar::new();
        progress.set_visible(false);
        controls.append(&progress);
        let weak = Rc::downgrade(self);
        let source = file.clone();
        let account = owner.clone();
        action.connect_clicked(move |_| {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            if ui.state.borrow().account != account {
                return;
            }
            match ui.downloads.borrow().status(&account, &source) {
                Some(Status::Running { .. }) => {}
                Some(Status::Ready(path)) if path.is_file() => {
                    if !media {
                        ui.save_attachment(&id, &path);
                    }
                    return;
                }
                _ => {}
            }
            let running = matches!(
                ui.downloads.borrow().status(&account, &source),
                Some(Status::Running { .. })
            );
            if running {
                ui.downloads.borrow_mut().cancel(&account, &source);
            } else {
                let result = ui.downloads.borrow_mut().start(&account, source.clone());
                if let Err(reason) = result {
                    ui.notice(reason);
                }
            }
        });
        let weak = Rc::downgrade(self);
        let a = action.downgrade();
        let p = progress.downgrade();
        let l = info.downgrade();
        let visible = controls.downgrade();
        let allow_auto = !self.smoke
            && (message.own || message.encrypted || self.state.borrow().current().contact);
        glib::timeout_add_local(Duration::from_millis(150), move || {
            let (Some(ui), Some(action), Some(progress), Some(info)) =
                (weak.upgrade(), a.upgrade(), p.upgrade(), l.upgrade())
            else {
                return glib::ControlFlow::Break;
            };
            if ui.state.borrow().account != owner {
                return glib::ControlFlow::Break;
            }
            let Some(controls) = visible.upgrade().filter(|controls| controls.is_visible()) else {
                return glib::ControlFlow::Break;
            };
            let status = ui.downloads.borrow().status(&owner, &file);
            if status.is_none()
                && allow_auto
                && media
                && !file.manual
                && ui.in_chat_viewport(&controls)
            {
                ui.downloads
                    .borrow_mut()
                    .start_automatic(&owner, file.clone());
            }
            action.set_visible(!media || !matches!(status, Some(Status::Ready(_))));
            progress.set_visible(matches!(status, Some(Status::Running { .. })));
            let (title, subtitle) = match status {
                Some(Status::Running { bytes, total }) => {
                    if let Some(total) = total.filter(|n| *n > 0) {
                        progress.set_fraction((bytes as f64 / total as f64).clamp(0., 1.));
                    } else {
                        progress.pulse();
                    }
                    (
                        "Cancel",
                        match total {
                            Some(total) => format!(
                                "{} of {}",
                                glib::format_size(bytes),
                                glib::format_size(total)
                            ),
                            None => format!("{} downloaded", glib::format_size(bytes)),
                        },
                    )
                }
                Some(Status::Ready(_)) if media => ("", "Preparing preview…".into()),
                Some(Status::Ready(_)) => ("Save As…", "Downloaded".into()),
                Some(Status::Deferred { reason, total }) => (
                    "Download",
                    total.map_or_else(
                        || reason.into(),
                        |size| format!("{} · {reason}", glib::format_size(size)),
                    ),
                ),
                Some(Status::Failed(reason)) => ("Retry", reason.into()),
                Some(Status::Cancelled) => ("Retry", "Download cancelled".into()),
                None => ("Download", detail.clone()),
            };
            action.set_label(title);
            info.set_text(&subtitle);
            info.set_visible(!subtitle.is_empty());
            glib::ControlFlow::Continue
        });
        content
    }
    fn media_attachment(self: &Rc<Self>, message: &Message) -> gtk::Box {
        let file = message.attachment.as_ref().unwrap().clone();
        let owner = self.state.borrow().account.clone();
        let peer = self.state.borrow().current().jid.clone();
        let id = message.id.clone();
        let width = if file.sticker { 168 } else { 320 };
        let placeholder = AspectImage::thumbnail(width, file.sticker);
        let dimensions = self
            .store
            .media_dimensions(&owner, &peer, &message.id, &file);
        placeholder.set_ratio(
            dimensions
                .map_or(4. / 3., |(w, h)| f64::from(w) / f64::from(h))
                .clamp(0.3, 4.),
        );
        let overlay = gtk::Overlay::builder()
            .child(&placeholder)
            .halign(gtk::Align::Start)
            .build();
        overlay.add_css_class("media-download");
        if file.sticker {
            overlay.add_css_class("sticker-download");
        }
        let actual = bx(true, 0);
        overlay.add_overlay(&actual);
        let controls = bx(true, 4);
        controls.set_halign(gtk::Align::Center);
        controls.set_valign(gtk::Align::Center);
        controls.add_css_class("media-download-controls");
        let button = gtk::Button::from_icon_name("process-stop-symbolic");
        button.add_css_class("circular");
        button.add_css_class("media-download-action");
        button.set_tooltip_text(Some("Cancel download"));
        let disk = gtk::DrawingArea::builder()
            .content_width(58)
            .content_height(58)
            .build();
        let fraction = Rc::new(Cell::new(None::<f64>));
        let displayed_fraction = Rc::new(Cell::<f64>::new(0.));
        let draw_fraction = fraction.clone();
        let draw_displayed_fraction = displayed_fraction.clone();
        let started = Instant::now();
        disk.set_draw_func(move |_, ctx, width, height| {
            let x = f64::from(width) / 2.;
            let y = f64::from(height) / 2.;
            let radius = x.min(y) - 3.;
            ctx.set_line_width(3.);
            ctx.set_source_rgba(1., 1., 1., 0.22);
            ctx.arc(x, y, radius, 0., std::f64::consts::TAU);
            let _ = ctx.stroke();
            let start = -std::f64::consts::FRAC_PI_2;
            let (angle, amount) = draw_fraction.get().map_or_else(
                || (start + started.elapsed().as_secs_f64() * 3., 0.23),
                |_| (start, draw_displayed_fraction.get().clamp(0., 1.)),
            );
            ctx.set_source_rgba(1., 1., 1., 0.95);
            ctx.set_line_cap(gtk::cairo::LineCap::Round);
            ctx.arc(x, y, radius, angle, angle + amount * std::f64::consts::TAU);
            let _ = ctx.stroke();
        });
        let action = gtk::Overlay::builder().child(&disk).build();
        button.set_halign(gtk::Align::Center);
        button.set_valign(gtk::Align::Center);
        action.add_overlay(&button);
        controls.append(&action);
        let status = wrap("", "caption");
        status.set_visible(false);
        status.set_max_width_chars(26);
        status.set_justify(gtk::Justification::Center);
        controls.append(&status);
        overlay.add_overlay(&controls);
        if file.video_candidate() {
            self.attach_video(message, &actual, &controls);
        } else {
            self.attach_image(message, &actual, &controls);
        }
        // A RAM-cached preview may already have completed before its overlay was parented.
        fn painted(widget: &gtk::Widget) -> Option<gtk::gdk::Paintable> {
            if let Some(image) = widget.downcast_ref::<AspectImage>()
                && let Some(paintable) = image.paintable()
            {
                return Some(paintable);
            }
            let mut child = widget.first_child();
            while let Some(widget) = child {
                child = widget.next_sibling();
                if let Some(image) = painted(&widget) {
                    return Some(image);
                }
            }
            None
        }
        if !controls.is_visible()
            && let Some(image) = painted(actual.upcast_ref())
        {
            placeholder.set_ratio(image.intrinsic_aspect_ratio());
        }
        let allow_auto = !self.smoke
            && !file.manual
            && (message.own || message.encrypted || self.state.borrow().current().contact);
        let cancelled = Rc::new(Cell::new(false));
        let cancelled_action = cancelled.clone();
        let weak = Rc::downgrade(self);
        let account = owner.clone();
        let source = file.clone();
        let error_panel = controls.downgrade();
        let action_peer = peer.clone();
        let action_id = id.clone();
        button.connect_clicked(move |_| {
            let Some(ui) = weak
                .upgrade()
                .filter(|ui| ui.attachment_is_current(&account, &action_peer, &action_id, &source))
            else {
                return;
            };
            let preview_error = error_panel.upgrade().is_some_and(|controls| {
                let failed = controls.has_css_class("media-preview-error");
                controls.remove_css_class("media-preview-error");
                failed
            });
            if preview_error {
                cancelled_action.set(false);
                ui.images.borrow_mut().invalidate(&source);
                ui.downloads.borrow_mut().invalidate(&account, &source);
                if let Err(reason) = ui.downloads.borrow_mut().start(&account, source.clone()) {
                    ui.notice(reason);
                }
                return;
            }
            let state = ui.downloads.borrow().status(&account, &source);
            if matches!(state, Some(Status::Running { .. } | Status::Ready(_)))
                || state.is_none() && allow_auto && !cancelled_action.get()
            {
                cancelled_action.set(true);
                ui.downloads.borrow_mut().cancel(&account, &source);
                ui.images.borrow_mut().invalidate(&source);
            } else {
                cancelled_action.set(false);
                if let Err(reason) = ui.downloads.borrow_mut().start(&account, source.clone()) {
                    ui.notice(reason);
                }
            }
        });

        let weak = Rc::downgrade(self);
        let panel = controls.downgrade();
        let action = button.downgrade();
        let ring = disk.downgrade();
        let feedback = status.downgrade();
        let animating = Rc::new(Cell::new(false));
        let tick_running = Rc::new(Cell::new(false));
        let last_frame = Rc::new(Cell::new(None::<i64>));
        glib::timeout_add_local(Duration::from_millis(80), move || {
            let (Some(ui), Some(controls), Some(button), Some(ring), Some(label)) = (
                weak.upgrade(),
                panel.upgrade(),
                action.upgrade(),
                ring.upgrade(),
                feedback.upgrade(),
            ) else {
                animating.set(false);
                return glib::ControlFlow::Break;
            };
            if !ui.attachment_is_current(&owner, &peer, &id, &file) {
                animating.set(false);
                return glib::ControlFlow::Break;
            }
            // Video preparation/playback may fail after a preview has been revealed.
            if !controls.is_visible() || !ui.in_chat_viewport(&controls) {
                animating.set(false);
                return glib::ControlFlow::Continue;
            }
            let mut state = if cancelled.get() {
                Some(Status::Cancelled)
            } else {
                ui.downloads.borrow().status(&owner, &file)
            };
            if state.is_none() && allow_auto {
                ui.downloads
                    .borrow_mut()
                    .start_automatic(&owner, file.clone());
                state = ui.downloads.borrow().status(&owner, &file);
            }
            let preview_error = controls.has_css_class("media-preview-error");
            let active = !preview_error
                && (matches!(
                    state.as_ref(),
                    Some(Status::Running { .. } | Status::Ready(_))
                ) || state.is_none() && allow_auto);
            let (icon, title, reason, progress) = if preview_error {
                (
                    "view-refresh-symbolic",
                    "Retry preparation",
                    Some("Media preview unavailable"),
                    Some(0.),
                )
            } else {
                match state {
                    Some(Status::Running { bytes, total }) => (
                        "process-stop-symbolic",
                        "Cancel download",
                        None,
                        total
                            .filter(|total| *total > 0)
                            .map(|total| bytes as f64 / total as f64),
                    ),
                    Some(Status::Ready(_)) => {
                        ("process-stop-symbolic", "Cancel preparation", None, None)
                    }
                    Some(Status::Failed(reason) | Status::Deferred { reason, .. }) => (
                        "view-refresh-symbolic",
                        "Retry download",
                        Some(reason),
                        Some(0.),
                    ),
                    Some(Status::Cancelled) => {
                        ("folder-download-symbolic", "Download media", None, Some(0.))
                    }
                    None if allow_auto => {
                        ("process-stop-symbolic", "Preparing download", None, None)
                    }
                    None => ("folder-download-symbolic", "Download media", None, Some(0.)),
                }
            };
            button.set_icon_name(icon);
            button.set_tooltip_text(Some(title));
            let previous = fraction.replace(progress);
            if progress.is_some() && previous.is_none() {
                displayed_fraction.set(progress.unwrap_or_default());
            }
            let was_animating = animating.replace(active);
            if !active {
                displayed_fraction.set(progress.unwrap_or_default());
                if was_animating || previous != progress {
                    ring.queue_draw();
                }
            } else if ring.is_mapped() && !tick_running.replace(true) {
                let fraction = fraction.clone();
                let displayed_fraction = displayed_fraction.clone();
                let animating = animating.clone();
                let tick_running = tick_running.clone();
                let last_frame = last_frame.clone();
                let panel = controls.downgrade();
                ring.add_tick_callback(move |ring, clock| {
                    let Some(controls) = panel.upgrade() else {
                        tick_running.set(false);
                        return glib::ControlFlow::Break;
                    };
                    if !animating.get() || !controls.is_visible() || !ring.is_mapped() {
                        last_frame.set(None);
                        tick_running.set(false);
                        return glib::ControlFlow::Break;
                    }
                    let now = clock.frame_time();
                    let previous = last_frame.replace(Some(now)).unwrap_or(now);
                    let elapsed = ((now - previous).max(0) as f64 / 1_000_000.).min(0.1);
                    if let Some(target) = fraction.get() {
                        let current = displayed_fraction.get();
                        let blend = 1. - (-12. * elapsed).exp();
                        displayed_fraction.set(current + (target - current) * blend);
                    }
                    ring.queue_draw();
                    glib::ControlFlow::Continue
                });
            }
            label.set_visible(reason.is_some());
            if let Some(reason) = reason {
                label.set_text(reason);
            }
            glib::ControlFlow::Continue
        });
        let content = bx(true, 0);
        content.append(&overlay);
        content
    }
    pub(crate) fn save_attachment(self: &Rc<Self>, id: &str, path: &std::path::Path) {
        let owner = self.state.borrow().account.clone();
        let peer = self.state.borrow().current().jid.clone();
        let Some(file) = self
            .store
            .message(&owner, &peer, id)
            .and_then(|m| m.attachment)
        else {
            return;
        };
        let chooser = gtk::FileDialog::builder()
            .title("Save Attachment")
            .initial_name(&file.name)
            .modal(true)
            .build();
        let owner = self.state.borrow().account.clone();
        let id = id.to_string();
        let Some(cancel) = self.downloads.borrow().token(&owner, &file) else {
            return;
        };
        let key = file.key();
        let generation = self.generation.get();
        let path = path.to_path_buf();
        let weak = Rc::downgrade(self);
        let window = self.window.clone();
        glib::MainContext::default().spawn_local(async move {
            let source = path.clone();
            let expected = file.clone();
            let valid = gio::spawn_blocking(move || {
                crate::downloads::verified_input(&source, &expected, &cancel)
            })
            .await;
            let Some(ui) = weak.upgrade().filter(|ui| {
                ui.state.borrow().account == owner && ui.generation.get() == generation
            }) else {
                return;
            };
            let input = match valid {
                Ok(Ok(input)) => input,
                _ => {
                    ui.downloads.borrow_mut().invalidate(&owner, &file);
                    ui.images.borrow_mut().invalidate(&file);
                    ui.notice("Cached attachment failed verification. Download it again.");
                    return;
                }
            };
            drop(ui);
            let destination = match chooser.save_future(Some(&window)).await {
                Ok(file) => file,
                Err(error) => {
                    if !error.matches(gtk::DialogError::Dismissed) {
                        if let Some(ui) = weak.upgrade() {
                            ui.notice("File selection unavailable");
                        }
                    }
                    return;
                }
            };
            let Some(ui) = weak.upgrade() else {
                return;
            };
            if ui.state.borrow().account != owner
                || ui.generation.get() != generation
                || ui.state.borrow().current().jid != peer
                || ui
                    .store
                    .message(&owner, &peer, &id)
                    .filter(|m| !m.retracted)
                    .and_then(|m| m.attachment)
                    .is_none_or(|f| f.key() != key)
            {
                return;
            }
            let _hold = ui.window.application().map(|app| app.hold());
            let weak = Rc::downgrade(&ui);
            drop(ui);
            let source = crate::image_loader::input_stream(input);
            let result = match destination
                .replace_future(
                    None,
                    false,
                    gio::FileCreateFlags::PRIVATE | gio::FileCreateFlags::REPLACE_DESTINATION,
                    glib::Priority::DEFAULT,
                )
                .await
            {
                Ok(output) => output
                    .splice_future(
                        &source,
                        gio::OutputStreamSpliceFlags::CLOSE_SOURCE
                            | gio::OutputStreamSpliceFlags::CLOSE_TARGET,
                        glib::Priority::DEFAULT,
                    )
                    .await
                    .map(|_| ()),
                Err(error) => Err(error),
            };
            if let Some(ui) = weak.upgrade().filter(|ui| {
                ui.state.borrow().account == owner && ui.generation.get() == generation
            }) {
                ui.notice(if result.is_ok() {
                    "Attachment saved"
                } else {
                    "Attachment could not be saved"
                });
            }
        });
    }
}
