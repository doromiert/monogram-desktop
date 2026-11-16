use crate::{ui::Ui, widgets::*};
use adw::prelude::*;
use gtk::gio;
use std::{collections::BTreeMap, rc::Rc};

impl Ui {
    /// Swap `old` for `new` (an edited copy) keeping its position among the attachments.
    pub fn replace_attachment(self: &Rc<Self>, old: &gio::File, old_card: Option<gtk::Overlay>, new: gio::File) {
        let Some(index) = self.attachments.borrow().iter().position(|file| file == old) else {
            // The attachment was removed while the editor was open.
            crate::picture_editor_ui::discard_edited(new.path());
            return;
        };
        let before = old_card.as_ref().and_then(|card| card.prev_sibling());
        if !self.stage_attachment_files(vec![new.clone()]) {
            crate::picture_editor_ui::discard_edited(new.path());
            return;
        }
        // The new attachment and its card were appended; move both into the old one's place.
        let mut attachments = self.attachments.borrow_mut();
        if let Some(position) = attachments.iter().rposition(|file| file == &new) {
            let moved = attachments.remove(position);
            let at = index.min(attachments.len());
            attachments.insert(at, moved);
        }
        attachments.retain(|file| file != old);
        drop(attachments);
        if let (Some(new_card), Some(old_card)) = (self.chips.last_child(), old_card) {
            self.chips.reorder_child_after(&new_card, before.as_ref());
            self.chips.remove(&old_card);
        }
        // An edited copy that was edited again no longer needs its predecessor on disk.
        crate::picture_editor_ui::discard_edited(old.path());
        self.chips.set_visible(!self.attachments.borrow().is_empty());
        self.compose_changed();
    }

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
            if crate::picture_editor_ui::editable(&name) {
                if let Some(path) = file.path() {
                    let edit = icon_button("document-edit-symbolic", "Edit picture");
                    edit.add_css_class("attachment-preview-remove");
                    edit.set_halign(gtk::Align::Start);
                    edit.set_valign(gtk::Align::Start);
                    let (weak, card_weak, original) = (Rc::downgrade(self), card.downgrade(), file.clone());
                    edit.connect_clicked(move |_| {
                        let Some(ui) = weak.upgrade() else { return };
                        let (weak, card_weak, original) = (Rc::downgrade(&ui), card_weak.clone(), original.clone());
                        ui.open_picture_editor(path.clone(), move |edited| {
                            if let Some(ui) = weak.upgrade() {
                                ui.replace_attachment(&original, card_weak.upgrade(), gio::File::for_path(edited));
                            }
                        });
                    });
                    card.add_overlay(&edit);
                }
            }
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
                // An edited copy exists only for this attachment; the user's original never is.
                crate::picture_editor_ui::discard_edited(staged.path());
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
