use crate::{ui::Ui, widgets::*};
use adw::prelude::*;
use gtk::gio;
use std::{collections::BTreeMap, rc::Rc};

/// Move the item at `index` by `delta` places, shifting the ones it passes. Returns where it ended
/// up, or `None` (changing nothing) when `index` is out of range or the move would leave the list.
pub(crate) fn shifted<T>(items: &mut Vec<T>, index: usize, delta: isize) -> Option<usize> {
    let target = index.checked_add_signed(delta)?;
    if index >= items.len() || target >= items.len() {
        return None;
    }
    let moved = items.remove(index);
    items.insert(target, moved);
    Some(target)
}

/// The first descendant of `root` (or `root` itself) whose widget name is `name`.
fn named(root: &gtk::Widget, name: &str) -> Option<gtk::Widget> {
    if root.widget_name() == name {
        return Some(root.clone());
    }
    let mut child = root.first_child();
    while let Some(current) = child {
        if let Some(found) = named(&current, name) {
            return Some(found);
        }
        child = current.next_sibling();
    }
    None
}

impl Ui {
    /// Number the cards in the order they will be sent and offer Move earlier or later only where it
    /// can do something. With a single file there is nothing to order, so the controls hide.
    pub(crate) fn refresh_attachment_order(&self) {
        let count = self.attachments.borrow().len();
        let mut child = self.chips.first_child();
        let mut index = 0;
        while let Some(card) = child {
            child = card.next_sibling();
            if let Some(position) = named(&card, "attachment-position").and_downcast::<gtk::Label>() {
                position.set_label(&(index + 1).to_string());
                position.set_visible(count > 1);
                position.update_property(&[gtk::accessible::Property::Label(&format!("Position {} of {count}", index + 1))]);
            }
            for (name, enabled) in [("attachment-earlier", index > 0), ("attachment-later", index + 1 < count)] {
                if let Some(button) = named(&card, name) {
                    button.set_visible(count > 1);
                    button.set_sensitive(enabled);
                }
            }
            index += 1;
        }
    }

    /// Send `file` earlier (`delta` -1) or later (+1) than its neighbour. The cards follow, and the
    /// button that was used keeps the focus so the order can be changed from the keyboard.
    pub fn move_attachment(self: &Rc<Self>, file: &gio::File, delta: isize) {
        let moved = {
            let mut attachments = self.attachments.borrow_mut();
            let Some(index) = attachments.iter().position(|candidate| candidate == file) else {
                return;
            };
            shifted(&mut attachments, index, delta).map(|target| (index, target))
        };
        let Some((from, _)) = moved else { return };
        let mut card = self.chips.first_child();
        for _ in 0..from {
            card = card.and_then(|card| card.next_sibling());
        }
        let Some(card) = card else { return };
        if delta < 0 {
            let before = card.prev_sibling().and_then(|previous| previous.prev_sibling());
            self.chips.reorder_child_after(&card, before.as_ref());
        } else {
            let after = card.next_sibling();
            self.chips.reorder_child_after(&card, after.as_ref());
        }
        self.refresh_attachment_order();
        let (used, other) = if delta < 0 { ("attachment-earlier", "attachment-later") } else { ("attachment-later", "attachment-earlier") };
        for name in [used, other] {
            if let Some(button) = named(&card, name).filter(|button| button.is_sensitive()) {
                button.grab_focus();
                break;
            }
        }
        self.compose_changed();
    }

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
        self.refresh_attachment_order();
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
                ui.refresh_attachment_order();
                ui.compose_changed();
            });
            card.add_overlay(&remove);
            // Where this file goes in the send order, and the buttons to change it.
            let position = label("", "attachment-preview-position");
            position.set_widget_name("attachment-position");
            // Top centre: the corners already hold the edit and remove buttons.
            position.set_halign(gtk::Align::Center);
            position.set_valign(gtk::Align::Start);
            position.set_visible(false);
            card.add_overlay(&position);
            let order = bx(false, 2);
            order.set_halign(gtk::Align::End);
            order.set_valign(gtk::Align::End);
            for (icon, tip, name, delta) in [
                ("go-previous-symbolic", "Send earlier", "attachment-earlier", -1isize),
                ("go-next-symbolic", "Send later", "attachment-later", 1),
            ] {
                let button = icon_button(icon, tip);
                button.add_css_class("attachment-preview-remove");
                button.set_widget_name(name);
                button.set_visible(false);
                let (weak, staged) = (Rc::downgrade(self), file.clone());
                button.connect_clicked(move |_| {
                    if let Some(ui) = weak.upgrade() {
                        ui.move_attachment(&staged, delta);
                    }
                });
                order.append(&button);
            }
            card.add_overlay(&order);
            self.chips.append(&card);
            self.attachments.borrow_mut().push(file);
            added = true;
        }
        self.chips
            .set_visible(!self.attachments.borrow().is_empty());
        self.refresh_attachment_order();
        self.compose_changed();
        if overflow {
            self.notice("A message can contain up to 64 attachments");
        }
        added
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gtk::glib;
    use std::path::PathBuf;

    #[test]
    fn a_move_shifts_the_items_it_passes_and_reports_where_it_landed() {
        let mut items = vec!['a', 'b', 'c', 'd'];
        assert_eq!(shifted(&mut items, 2, -1), Some(1));
        assert_eq!(items, ['a', 'c', 'b', 'd']);
        assert_eq!(shifted(&mut items, 1, 1), Some(2));
        assert_eq!(items, ['a', 'b', 'c', 'd']);
        assert_eq!(shifted(&mut items, 0, 3), Some(3));
        assert_eq!(items, ['b', 'c', 'd', 'a']);
        assert_eq!(shifted(&mut items, 3, -3), Some(0));
        assert_eq!(items, ['a', 'b', 'c', 'd']);
    }

    #[test]
    fn a_move_off_either_end_or_from_nowhere_changes_nothing() {
        let mut items = vec![1, 2, 3];
        assert_eq!(shifted(&mut items, 0, -1), None, "the first cannot go earlier");
        assert_eq!(shifted(&mut items, 2, 1), None, "the last cannot go later");
        assert_eq!(shifted(&mut items, 3, -1), None, "no such item");
        assert_eq!(shifted(&mut items, 1, 5), None);
        assert_eq!(shifted(&mut items, 1, isize::MIN), None);
        assert_eq!(shifted(&mut items, usize::MAX, 1), None);
        assert_eq!(shifted(&mut items, 1, 0), Some(1), "staying put is not an error");
        assert_eq!(items, [1, 2, 3]);
        let mut none: Vec<u8> = vec![];
        assert_eq!(shifted(&mut none, 0, 1), None);
    }

    #[test]
    fn a_move_keeps_every_item_exactly_once() {
        for start in 0..6 {
            for delta in -6isize..=6 {
                let mut items: Vec<usize> = (0..6).collect();
                shifted(&mut items, start, delta);
                let mut sorted = items.clone();
                sorted.sort();
                assert_eq!(sorted, (0..6).collect::<Vec<_>>(), "start {start}, delta {delta}");
            }
        }
    }

    fn open_ui(id: &str) -> Rc<Ui> {
        adw::init().unwrap();
        let app = adw::Application::builder().application_id(id).flags(gio::ApplicationFlags::NON_UNIQUE).build();
        app.register(None::<&gio::Cancellable>).unwrap();
        let ui = Ui::new(&app, true);
        ui.window.set_default_size(1200, 900);
        ui.window.present();
        settle();
        ui
    }
    fn settle() {
        let context = glib::MainContext::default();
        for _ in 0..30 {
            while context.iteration(false) {}
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }
    fn files(names: &[&str]) -> Vec<gio::File> {
        let dir = std::env::temp_dir().join(format!("album-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        names
            .iter()
            .map(|name| {
                let path: PathBuf = dir.join(name);
                std::fs::write(&path, b"not really a picture").unwrap();
                gio::File::for_path(path)
            })
            .collect()
    }
    fn cards(ui: &Ui) -> Vec<gtk::Widget> {
        let mut found = Vec::new();
        let mut child = ui.chips.first_child();
        while let Some(card) = child {
            child = card.next_sibling();
            found.push(card);
        }
        found
    }
    fn card_name(card: &gtk::Widget) -> String {
        fn find(widget: &gtk::Widget) -> Option<String> {
            if let Some(label) = widget.downcast_ref::<gtk::Label>().filter(|l| l.has_css_class("attachment-preview-name")) {
                return Some(label.label().to_string());
            }
            let mut child = widget.first_child();
            while let Some(current) = child {
                if let Some(found) = find(&current) {
                    return Some(found);
                }
                child = current.next_sibling();
            }
            None
        }
        find(card).unwrap_or_default()
    }
    fn badge(card: &gtk::Widget) -> (String, bool) {
        let label = named(card, "attachment-position").and_downcast::<gtk::Label>().expect("a position badge");
        (label.label().to_string(), label.is_visible())
    }
    fn names(ui: &Ui) -> Vec<String> {
        cards(ui).iter().map(card_name).collect()
    }
    fn staged(ui: &Ui) -> Vec<String> {
        ui.attachments.borrow().iter().map(|f| f.basename().unwrap().to_string_lossy().into_owned()).collect()
    }
    fn press(card: &gtk::Widget, name: &str) {
        named(card, name).and_downcast::<gtk::Button>().expect("a button").emit_clicked();
    }

    #[gtk::test]
    #[ignore = "requires a native display"]
    fn a_single_file_has_nothing_to_order() {
        let ui = open_ui("com.negzero.AlbumOrderOne");
        assert!(ui.stage_attachment_files(files(&["only.bin"])));
        let card = cards(&ui).remove(0);
        assert!(!badge(&card).1, "no number on a lone file");
        assert!(!named(&card, "attachment-earlier").unwrap().is_visible());
        assert!(!named(&card, "attachment-later").unwrap().is_visible());
        ui.window.close();
    }

    #[gtk::test]
    #[ignore = "requires a native display"]
    fn files_are_numbered_in_the_order_they_will_be_sent() {
        let ui = open_ui("com.negzero.AlbumOrderNumbers");
        ui.stage_attachment_files(files(&["a.bin", "b.bin", "c.bin"]));
        assert_eq!(staged(&ui), ["a.bin", "b.bin", "c.bin"]);
        let numbers: Vec<_> = cards(&ui).iter().map(badge).collect();
        assert_eq!(numbers, [("1".into(), true), ("2".into(), true), ("3".into(), true)]);
        let cards = cards(&ui);
        assert!(!named(&cards[0], "attachment-earlier").unwrap().is_sensitive(), "the first cannot go earlier");
        assert!(named(&cards[0], "attachment-later").unwrap().is_sensitive());
        assert!(named(&cards[1], "attachment-earlier").unwrap().is_sensitive());
        assert!(named(&cards[1], "attachment-later").unwrap().is_sensitive());
        assert!(named(&cards[2], "attachment-earlier").unwrap().is_sensitive());
        assert!(!named(&cards[2], "attachment-later").unwrap().is_sensitive(), "the last cannot go later");
        ui.window.close();
    }

    #[gtk::test]
    #[ignore = "requires a native display"]
    fn the_buttons_reorder_both_the_cards_and_what_will_be_sent() {
        let ui = open_ui("com.negzero.AlbumOrderMove");
        ui.stage_attachment_files(files(&["a.bin", "b.bin", "c.bin"]));
        press(&cards(&ui)[2], "attachment-earlier");
        assert_eq!(staged(&ui), ["a.bin", "c.bin", "b.bin"]);
        assert_eq!(names(&ui), ["a.bin", "c.bin", "b.bin"], "the cards follow the files");
        press(&cards(&ui)[0], "attachment-later");
        assert_eq!(staged(&ui), ["c.bin", "a.bin", "b.bin"]);
        assert_eq!(names(&ui), ["c.bin", "a.bin", "b.bin"]);
        let numbers: Vec<_> = cards(&ui).iter().map(|c| badge(c).0).collect();
        assert_eq!(numbers, ["1", "2", "3"], "numbers always count from the front");
        ui.window.close();
    }

    #[gtk::test]
    #[ignore = "requires a native display"]
    fn moving_past_either_end_does_nothing() {
        let ui = open_ui("com.negzero.AlbumOrderEnds");
        let staged_files = files(&["a.bin", "b.bin"]);
        ui.stage_attachment_files(staged_files.clone());
        ui.move_attachment(&staged_files[0], -1);
        ui.move_attachment(&staged_files[1], 1);
        assert_eq!(staged(&ui), ["a.bin", "b.bin"]);
        assert_eq!(names(&ui), ["a.bin", "b.bin"]);
        // A file that is no longer attached is ignored.
        ui.move_attachment(&files(&["gone.bin"])[0], 1);
        assert_eq!(staged(&ui), ["a.bin", "b.bin"]);
        ui.window.close();
    }

    #[gtk::test]
    #[ignore = "requires a native display"]
    fn removing_a_file_renumbers_the_rest_and_the_last_one_loses_its_controls() {
        let ui = open_ui("com.negzero.AlbumOrderRemove");
        ui.stage_attachment_files(files(&["a.bin", "b.bin", "c.bin"]));
        let second = cards(&ui).remove(1);
        let remove = fn_remove(&second);
        remove.emit_clicked();
        assert_eq!(staged(&ui), ["a.bin", "c.bin"]);
        let numbers: Vec<_> = cards(&ui).iter().map(|c| badge(c).0).collect();
        assert_eq!(numbers, ["1", "2"]);
        let last = cards(&ui).remove(1);
        fn_remove(&last).emit_clicked();
        let only = cards(&ui).remove(0);
        assert!(!badge(&only).1 && !named(&only, "attachment-later").unwrap().is_visible(), "one left: nothing to order");
        ui.window.close();
    }
    fn fn_remove(card: &gtk::Widget) -> gtk::Button {
        fn find(widget: &gtk::Widget) -> Option<gtk::Button> {
            if let Some(button) = widget.downcast_ref::<gtk::Button>().filter(|b| b.tooltip_text().as_deref() == Some("Remove attachment")) {
                return Some(button.clone());
            }
            let mut child = widget.first_child();
            while let Some(current) = child {
                if let Some(found) = find(&current) {
                    return Some(found);
                }
                child = current.next_sibling();
            }
            None
        }
        find(card).expect("a remove button")
    }

    #[gtk::test]
    #[ignore = "requires a native display"]
    fn an_edited_copy_keeps_the_place_and_number_of_the_picture_it_replaces() {
        let ui = open_ui("com.negzero.AlbumOrderReplace");
        let staged_files = files(&["a.bin", "b.bin", "c.bin"]);
        ui.stage_attachment_files(staged_files.clone());
        let card = cards(&ui).remove(1);
        let edited = files(&["b-edited.bin"]).remove(0);
        ui.replace_attachment(&staged_files[1], card.downcast::<gtk::Overlay>().ok(), edited);
        assert_eq!(staged(&ui), ["a.bin", "b-edited.bin", "c.bin"]);
        assert_eq!(names(&ui), ["a.bin", "b-edited.bin", "c.bin"]);
        let numbers: Vec<_> = cards(&ui).iter().map(|c| badge(c).0).collect();
        assert_eq!(numbers, ["1", "2", "3"]);
        ui.window.close();
    }

    #[gtk::test]
    #[ignore = "requires a native display"]
    fn a_moved_card_keeps_working_controls_so_it_can_be_moved_again() {
        let ui = open_ui("com.negzero.AlbumOrderFocus");
        ui.stage_attachment_files(files(&["a.bin", "b.bin", "c.bin"]));
        let first = cards(&ui).remove(0);
        press(&first, "attachment-later");
        // The moved card is now second, and its buttons still work.
        let moved = cards(&ui).remove(1);
        assert_eq!(card_name(&moved), "a.bin");
        let later = named(&moved, "attachment-later").unwrap();
        assert!(later.is_sensitive());
        ui.window.close();
    }
}

