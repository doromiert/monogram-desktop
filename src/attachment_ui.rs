use crate::{downloads::Status, model::Message, ui::Ui, widgets::*};
use adw::prelude::*;
use gtk::{gio, glib};
use std::{rc::Rc, time::Duration};
impl Ui {
    pub fn attachment_content(self: &Rc<Self>, message: &Message) -> gtk::Box {
        let file = message.attachment.as_ref().unwrap().clone();
        let owner = self.state.borrow().account.clone();
        let id = message.id.clone();
        let content = bx(true, 8);
        let line = bx(false, 10);
        let image = gtk::Image::from_icon_name("document-save-symbolic");
        image.set_pixel_size(28);
        line.append(&image);
        let words = bx(true, 2);
        words.set_hexpand(true);
        words.append(&wrap(&file.name, "heading"));
        let detail = match file.size {
            Some(size) => format!("{} · {}", glib::format_size(size), file.mime),
            None => file.mime.clone(),
        };
        let info = wrap(&detail, "caption dim-label");
        words.append(&info);
        line.append(&words);
        let action = gtk::Button::with_label("Download");
        action.set_valign(gtk::Align::Center);
        line.append(&action);
        content.append(&line);
        let progress = gtk::ProgressBar::new();
        progress.set_visible(false);
        content.append(&progress);
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
                    ui.save_attachment(&id, &path);
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
        glib::timeout_add_local(Duration::from_millis(150), move || {
            let (Some(ui), Some(action), Some(progress), Some(info)) =
                (weak.upgrade(), a.upgrade(), p.upgrade(), l.upgrade())
            else {
                return glib::ControlFlow::Break;
            };
            if ui.state.borrow().account != owner {
                return glib::ControlFlow::Break;
            }
            let status = ui.downloads.borrow().status(&owner, &file);
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
                Some(Status::Ready(_)) => ("Save As…", "Downloaded".into()),
                Some(Status::Failed(reason)) => ("Retry", reason.into()),
                Some(Status::Cancelled) => ("Retry", "Download cancelled".into()),
                None => ("Download", detail.clone()),
            };
            action.set_label(title);
            info.set_text(&subtitle);
            glib::ControlFlow::Continue
        });
        content
    }
    fn save_attachment(self: &Rc<Self>, id: &str, path: &std::path::Path) {
        let Some(file) = self
            .state
            .borrow()
            .message(id)
            .and_then(|m| m.attachment.clone())
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
        let path = path.to_path_buf();
        let weak = Rc::downgrade(self);
        let window = self.window.clone();
        glib::MainContext::default().spawn_local(async move {
            let source = path.clone();
            let expected = file.clone();
            let valid =
                gio::spawn_blocking(move || crate::downloads::verify(&source, &expected, &cancel))
                    .await;
            let Some(ui) = weak
                .upgrade()
                .filter(|ui| ui.state.borrow().account == owner)
            else {
                return;
            };
            if !matches!(valid, Ok(Ok(()))) {
                ui.downloads.borrow_mut().invalidate(&owner, &file);
                ui.notice("Cached attachment failed verification. Download it again.");
                return;
            }
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
                || ui
                    .state
                    .borrow()
                    .message(&id)
                    .and_then(|m| m.attachment.as_ref())
                    .is_none_or(|f| f.key() != key)
            {
                return;
            }
            let weak = Rc::downgrade(&ui);
            gio::File::for_path(path).copy_async(
                &destination,
                gio::FileCopyFlags::OVERWRITE,
                glib::Priority::DEFAULT,
                None::<&gio::Cancellable>,
                None,
                move |result| {
                    if let Some(ui) = weak.upgrade() {
                        if ui.state.borrow().account == owner {
                            ui.notice(if result.is_ok() {
                                "Attachment saved"
                            } else {
                                "Attachment could not be saved"
                            });
                        }
                    }
                },
            );
        });
    }
}
