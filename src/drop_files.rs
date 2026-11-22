//! Dropping files from a file manager onto the conversation attaches them to the message being
//! written, exactly as the attach button does. Nothing is sent by the drop itself.
use crate::ui::Ui;
use gtk::{gdk, gio, glib, prelude::*};
use adw::prelude::*;
use std::rc::Rc;

/// More files than a message can hold are not looked at.
const MAX_FILES: usize = 64;

/// What of a drop can be attached: files on this computer, each once. Folders and anything
/// without a local path (a remote share that is not mounted, a link) are counted as skipped.
pub fn attachable(files: &[gio::File]) -> (Vec<gio::File>, usize) {
    let mut kept: Vec<gio::File> = Vec::new();
    let mut skipped = 0;
    for file in files.iter().take(MAX_FILES * 4) {
        let usable = file
            .path()
            .and_then(|path| std::fs::metadata(path).ok())
            .is_some_and(|meta| meta.is_file());
        if !usable || kept.iter().any(|seen| seen.equal(file)) {
            skipped += usize::from(!usable);
            continue;
        }
        if kept.len() < MAX_FILES {
            kept.push(file.clone());
        }
    }
    (kept, skipped)
}

pub fn install(ui: &Rc<Ui>, area: &impl IsA<gtk::Widget>) {
    let target = gtk::DropTarget::new(gdk::FileList::static_type(), gdk::DragAction::COPY);
    let area_weak = area.upcast_ref::<gtk::Widget>().downgrade();
    let highlight = |weak: &glib::WeakRef<gtk::Widget>, on: bool| {
        if let Some(widget) = weak.upgrade() {
            if on {
                widget.add_css_class("drop-target");
            } else {
                widget.remove_css_class("drop-target");
            }
        }
    };
    let entered = area_weak.clone();
    target.connect_enter(move |_, _, _| {
        highlight(&entered, true);
        gdk::DragAction::COPY
    });
    let left = area_weak.clone();
    target.connect_leave(move |_| highlight(&left, false));
    let weak = Rc::downgrade(ui);
    let dropped = area_weak;
    target.connect_drop(move |_, value, _, _| {
        highlight(&dropped, false);
        let (Some(ui), Ok(list)) = (weak.upgrade(), value.get::<gdk::FileList>()) else {
            return false;
        };
        if ui.window.visible_dialog().is_some() || ui.state.borrow().account.is_empty() {
            return false;
        }
        let (files, skipped) = attachable(&list.files());
        if files.is_empty() {
            ui.notice("Only files on this computer can be attached. Folders are not supported");
            return false;
        }
        let staged = ui.stage_attachment_files(files);
        if staged && skipped > 0 {
            ui.notice(&format!("{skipped} dropped item(s) were skipped: only files can be attached"));
        }
        staged
    });
    area.add_controller(target);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_new_local_regular_files_are_attachable() {
        let dir = std::env::temp_dir().join(format!("drop-files-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("folder")).unwrap();
        let (a, b) = (dir.join("a.txt"), dir.join("b.png"));
        std::fs::write(&a, "a").unwrap();
        std::fs::write(&b, "b").unwrap();
        fn f(p: &std::path::Path) -> gio::File {
            gio::File::for_path(p)
        }
        let drop = [f(&a), f(dir.join("folder").as_path()), f(&b), f(&a), f(dir.join("missing").as_path()), gio::File::for_uri("https://example.com/x.png")];
        let (kept, skipped) = attachable(&drop);
        assert_eq!(kept.iter().filter_map(|k| k.basename()).map(|n| n.to_string_lossy().into_owned()).collect::<Vec<_>>(), ["a.txt", "b.png"]);
        assert_eq!(skipped, 3, "folder, missing and remote; the repeat is not a skip");
        assert_eq!(attachable(&[]), (vec![], 0));
        let many: Vec<_> = (0..200)
            .map(|i| {
                let p = dir.join(format!("n{}.txt", i % 70));
                std::fs::write(&p, "x").unwrap();
                f(&p)
            })
            .collect();
        assert_eq!(attachable(&many).0.len(), 64);
        let _ = std::fs::remove_dir_all(dir);
        let _ = glib::MainContext::default();
    }
}
