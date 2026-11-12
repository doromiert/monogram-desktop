use crate::{ui::Ui, widgets::*};
use adw::prelude::*;
use gtk::gio;
use std::{collections::BTreeMap, rc::Rc};

impl Ui {
    pub fn stage_attachment_files(self: &Rc<Self>, files: Vec<gio::File>) -> bool {
        let available = 64usize.saturating_sub(self.attachments.borrow().len());
        let overflow = files.len() > available;
        let mut added = false;
        for file in files.into_iter().take(available) {
            if self.attachments.borrow().contains(&file) {
                continue;
            }
            let name = file
                .basename()
                .unwrap_or_else(|| "Attachment".into())
                .to_string_lossy()
                .into_owned();
            let card = gtk::Overlay::new();
            card.add_css_class("attachment-preview-card");
            card.set_size_request(180, -1);
            card.update_property(&[gtk::accessible::Property::Label(&name)]);
            let is_image = name.rsplit_once('.').is_some_and(|(_, extension)| {
                matches!(
                    extension.to_ascii_lowercase().as_str(),
                    "jpg" | "jpeg" | "png" | "gif" | "webp" | "avif" | "heic"
                )
            });
            let is_video = name.rsplit_once('.').is_some_and(|(_, extension)| {
                matches!(
                    extension.to_ascii_lowercase().as_str(),
                    "mp4" | "mkv" | "webm" | "mov" | "avi" | "m4v"
                )
            });
            let icon = gtk::Image::from_icon_name(match (is_image, is_video) {
                (true, _) => "image-x-generic-symbolic",
                (_, true) => "video-x-generic-symbolic",
                _ => "text-x-generic-symbolic",
            });
            icon.set_pixel_size(28);
            let preview = gtk::Overlay::new();
            preview.add_css_class("attachment-preview-art");
            preview.set_height_request(112);
            preview.set_child(Some(&icon));
            if is_image {
                if let Some(path) = file.path() {
                    let picture = gtk::Picture::new();
                    picture.set_content_fit(gtk::ContentFit::Cover);
                    preview.add_overlay(&picture);
                    crate::image_loader::play(
                        &picture,
                        path,
                        crate::file_share::Attachment {
                            wire_url: String::new(),
                            name: name.clone(),
                            mime: String::new(),
                            size: None,
                            width: None,
                            height: None,
                            hashes: BTreeMap::new(),
                            manual: false,
                            sticker: false,
                        },
                        180,
                        |picture| picture.is_mapped(),
                        |picture, texture| picture.set_paintable(Some(&texture)),
                    );
                }
            }
            card.set_child(Some(&preview));
            let name_label = label(&name, "attachment-preview-name");
            name_label.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
            name_label.set_max_width_chars(20);
            name_label.set_single_line_mode(true);
            name_label.set_halign(gtk::Align::Start);
            name_label.set_valign(gtk::Align::End);
            card.add_overlay(&name_label);
            let remove = icon_button("window-close-symbolic", "Remove attachment");
            remove.add_css_class("attachment-preview-remove");
            remove.set_halign(gtk::Align::End);
            remove.set_valign(gtk::Align::Start);
            let weak = Rc::downgrade(self);
            let card_weak = card.downgrade();
            let staged = file.clone();
            remove.connect_clicked(move |_| {
                let Some(ui) = weak.upgrade() else { return };
                ui.attachments.borrow_mut().retain(|file| file != &staged);
                if let Some(card) = card_weak.upgrade() {
                    ui.chips.remove(&card);
                }
                ui.chips.set_visible(!ui.attachments.borrow().is_empty());
                ui.compose_changed();
            });
            card.add_overlay(&remove);
            self.chips.append(&card);
            self.attachments.borrow_mut().push(file);
            added = true;
        }
        self.chips
            .set_visible(!self.attachments.borrow().is_empty());
        self.compose_changed();
        if overflow {
            self.notice("A message can contain up to 64 attachments");
        }
        added
    }
}
