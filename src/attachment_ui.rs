use crate::{downloads::Status, model::Message, ui::Ui, widgets::*};
use adw::prelude::*;
use gtk::{gio, glib};
use std::{rc::Rc, time::Duration};
impl Ui {
    pub fn attachment_content(self: &Rc<Self>, message: &Message) -> gtk::Box {
        let file = message.attachment.as_ref().unwrap().clone();
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
