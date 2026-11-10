//! Native chooser/composer upload acceptance on fictional XMPP/HTTPS only.
use crate::{downloads::Status, model::*, ui::Ui, uploads::Phase};
use adw::prelude::*;
use gtk::gio;
use std::{
    cell::{Cell, RefCell},
    path::PathBuf,
    rc::Rc,
};
#[derive(Default)]
pub struct UploadSmoke {
    draft: RefCell<String>,
    stage: Cell<u8>,
    printed: Cell<u8>,
    id: RefCell<String>,
    file: RefCell<Option<crate::file_share::Attachment>>,
    received: Cell<bool>,
}
impl UploadSmoke {
    fn path() -> PathBuf {
        PathBuf::from(std::env::var_os("HOME").unwrap()).join("upload-fixture.bin")
    }
    fn plain() -> Vec<u8> {
        (0..196613).map(|n| (n % 251) as u8).collect()
    }
    pub fn start(&self, ui: &Rc<Ui>) {
        self.draft.replace(ui.composer_text());
        std::fs::write(Self::path(), Self::plain()).unwrap();
        ui.choose_files_in(
            false,
            Some(&gio::File::for_path(Self::path().parent().unwrap())),
        );
        self.stage.set(1);
    }
    pub fn incoming(&self, message: &Message, encrypted: bool) -> bool {
        if message.wire_id != *self.id.borrow() || self.id.borrow().is_empty() {
            return false;
        }
        assert_eq!(
            self.stage.get(),
            8,
            "stanza sent before retry/policy checks completed"
        );
        assert_eq!(message.text, "Uploaded native caption 😀");
        assert_eq!(message.encrypted, encrypted);
        let file = message.attachment.clone().unwrap();
        assert_eq!(file.name, "upload-fixture.bin");
        assert_eq!(file.size, Some(Self::plain().len() as u64));
        assert_eq!(file.hashes["sha-256"], openssl::sha::sha256(&Self::plain()));
        assert_eq!(file.link().unwrap().encrypted(), encrypted);
        self.file.replace(Some(file));
        self.received.set(true);
        true
    }
    pub fn tick(&self, ui: &Rc<Ui>, encrypted: bool) -> bool {
        if self.printed.get() != self.stage.get() {
            self.printed.set(self.stage.get());
            eprintln!(
                "Native upload stage {} encrypted={encrypted}",
                self.stage.get()
            );
        }
        fn chooser(w: &gtk::Widget) -> Option<gtk::FileChooserWidget> {
            if let Some(c) = w.downcast_ref::<gtk::FileChooserWidget>() {
                return Some(c.clone());
            }
            let mut child = w.first_child();
            while let Some(w) = child {
                child = w.next_sibling();
                if let Some(c) = chooser(&w) {
                    return Some(c);
                }
            }
            None
        }
        let click = |title: &str| {
            let Some(bubble) = ui.bubbles.borrow().get(&*self.id.borrow()).cloned() else {
                return false;
            };
            if let Some(b) = crate::smoke::button(&bubble, title) {
                b.emit_clicked();
                true
            } else {
                false
            }
        };
        match self.stage.get() {
            1 => {
                for w in gtk::Window::list_toplevels() {
                    if let Some(c) = chooser(&w) {
                        if c.current_folder().and_then(|f| f.path()).as_deref()
                            != Self::path().parent()
                        {
                            continue;
                        }
                        c.set_file(&gio::File::for_path(Self::path())).unwrap();
                        self.stage.set(2);
                        break;
                    }
                }
            }
            2 => {
                for w in gtk::Window::list_toplevels() {
                    if let Some(c) = chooser(&w) {
                        if c.file().and_then(|f| f.path()) != Some(Self::path()) {
                            continue;
                        }
                        let d = w.downcast_ref::<gtk::Dialog>().unwrap();
                        d.response(gtk::ResponseType::Accept);
                        self.stage.set(3);
                        break;
                    }
                }
            }
            3 if !ui.attachments.borrow().is_empty() => {
                assert_eq!(ui.attachments.borrow()[0].path(), Some(Self::path()));
                ui.composer.buffer().set_text("Uploaded native caption 😀");
                ui.send();
                assert!(ui.composer_text().is_empty());
                assert!(ui.attachments.borrow().is_empty());
                let m = ui.state.borrow().current().messages.last().unwrap().clone();
                assert!(m.upload.is_some());
                self.id.replace(m.id);
                self.stage.set(4);
            }
            4 => {
                if matches!(
                    ui.uploads.borrow().phase(&self.id.borrow()),
                    Some(Phase::Uploading { .. })
                ) {
                    assert!(!self.received.get());
                    if click("Cancel") {
                        let m = ui
                            .state
                            .borrow()
                            .message(&self.id.borrow())
                            .unwrap()
                            .clone();
                        assert_eq!(m.delivery, "failed");
                        assert!(m.upload.as_ref().unwrap().error.is_some());
                        std::fs::write(Self::path(), b"mutated after private snapshot").unwrap();
                        self.stage.set(5);
                    }
                }
            }
            5 => {
                if click("Retry") {
                    self.stage.set(6);
                }
            }
            6 => {
                if matches!(
                    ui.uploads.borrow().phase(&self.id.borrow()),
                    Some(Phase::Uploading { .. })
                ) {
                    ui.state.borrow_mut().current_mut().encryption = !encrypted;
                    ui.persist();
                    self.stage.set(7);
                }
            }
            7 => {
                let m = ui
                    .state
                    .borrow()
                    .message(&self.id.borrow())
                    .unwrap()
                    .clone();
                if m.delivery == "failed" {
                    assert!(
                        m.upload
                            .unwrap()
                            .error
                            .unwrap()
                            .contains("encryption changed")
                    );
                    assert!(!self.received.get());
                    ui.state.borrow_mut().current_mut().encryption = encrypted;
                    ui.persist();
                    if click("Retry") {
                        self.stage.set(8);
                    }
                }
            }
            8 if self.received.get() => {
                let file = self.file.borrow().clone().unwrap();
                let owner = ui.state.borrow().account.clone();
                ui.downloads.borrow_mut().start(&owner, file).unwrap();
                self.stage.set(9);
            }
            9 => {
                let file = self.file.borrow().clone().unwrap();
                let owner = ui.state.borrow().account.clone();
                match ui.downloads.borrow().status(&owner, &file) {
                    Some(Status::Ready(path)) => {
                        assert_eq!(std::fs::read(path).unwrap(), Self::plain());
                        let m = ui
                            .store
                            .message(&owner, &ui.state.borrow().current().jid, &self.id.borrow())
                            .unwrap();
                        assert!(m.upload.is_none());
                        assert_eq!(m.attachment, Some(file));
                        assert!(
                            !crate::uploads::snapshot_path(&owner, &m.id)
                                .unwrap()
                                .exists()
                        );
                        println!(
                            "Native upload: chooser, queued composer caption, private snapshot, Cancel/Retry, mode-change send fence, accepted HTTPS and persisted SFS peer reception/cache passed encrypted={encrypted}"
                        );
                        self.stage.set(10);
                        ui.composer.buffer().set_text(&self.draft.borrow());
                        ui.save_draft();
                        return true;
                    }
                    Some(Status::Failed(reason)) => panic!("uploaded cache failed: {reason}"),
                    _ => {}
                }
            }
            _ => {}
        }
        false
    }
}
