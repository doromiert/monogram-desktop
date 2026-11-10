//! Real file-message controls on the private XMPP/HTTPS fixture, never host accounts.
use crate::{
    downloads::Status,
    file_share::Attachment,
    model::*,
    protocol::{Command, Session},
    ui::Ui,
};
use adw::prelude::*;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use std::{
    cell::{Cell, RefCell},
    path::PathBuf,
    rc::Rc,
};
#[derive(Default)]
pub struct DownloadSmoke {
    discovery: RefCell<Option<tokio::sync::oneshot::Receiver<crate::iq::Reply>>>,
    stage: Cell<u8>,
    printed: Cell<u8>,
    id: RefCell<String>,
    chat: RefCell<String>,
    file: RefCell<Option<Attachment>>,
}
impl DownloadSmoke {
    fn vector() -> serde_json::Value {
        serde_json::from_str::<Vec<serde_json::Value>>(include_str!(
            "../tests/mobile-reference/file-cipher-vectors.json"
        ))
        .unwrap()
        .remove(1)
    }
    fn plain() -> Vec<u8> {
        STANDARD
            .decode(Self::vector()["plain"].as_str().unwrap())
            .unwrap()
    }
    fn export() -> PathBuf {
        PathBuf::from(std::env::var_os("HOME").unwrap()).join("fixture-export.bin")
    }
    pub fn start(&self, ui: &Rc<Ui>, peer: &Session, encrypted: bool, resource: &str) {
        let (reply, received) = tokio::sync::oneshot::channel();
        assert!(peer.send(Command::Iq {
            to: resource.into(),
            kind: crate::iq::Kind::Get,
            payload: crate::protocol::el("query", "http://jabber.org/protocol/disco#info"),
            reply
        }));
        self.discovery.replace(Some(received));
        let value = Self::vector();
        let mut key = STANDARD.decode(value["iv"].as_str().unwrap()).unwrap();
        key.extend(STANDARD.decode(value["key"].as_str().unwrap()).unwrap());
        let hex: String = key.iter().map(|b| format!("{b:02x}")).collect();
        let base = std::env::var("MONOGRAM_FILE_URL").unwrap();
        let url = if encrypted {
            format!("aesgcm{}/encrypted-slow#{hex}", &base[5..])
        } else {
            format!("{base}/plain-slow")
        };
        let mut file = Attachment::bare(&url).unwrap();
        file.name = "Received file.bin".into();
        file.size = Some(Self::plain().len() as u64);
        file.hashes = value["hashes"]
            .as_object()
            .unwrap()
            .iter()
            .map(|(key, v)| (key.clone(), STANDARD.decode(v.as_str().unwrap()).unwrap()))
            .collect();
        peer.send(Command::Send {
            peer: ui.state.borrow().account.clone(),
            message: Message {
                text: "File caption 😀".into(),
                wire_id: new_id(),
                attachment: Some(file.clone()),
                encrypted,
                ..Default::default()
            },
            quote: String::new(),
        });
        self.file.replace(Some(file));
        self.chat.replace(ui.state.borrow().active.clone());
        self.stage.set(1);
    }
    pub fn tick(&self, ui: &Rc<Ui>, encrypted: bool) -> bool {
        let Some(file) = self.file.borrow().clone() else {
            return false;
        };
        let owner = ui.state.borrow().account.clone();
        if self.printed.get() != self.stage.get() {
            self.printed.set(self.stage.get());
            eprintln!(
                "Native attachment stage {} encrypted={encrypted}",
                self.stage.get()
            );
        }
        let click = |title: &str| {
            let Some(bubble) = ui.bubbles.borrow().get(&*self.id.borrow()).cloned() else {
                return false;
            };
            if let Some(button) = crate::smoke::button(&bubble, title) {
                button.emit_clicked();
                true
            } else {
                false
            }
        };
        match self.stage.get() {
            1 => {
                if self.discovery.borrow().is_some() {
                    let result = self.discovery.borrow_mut().as_mut().unwrap().try_recv();
                    match result {
                        Ok(result) => {
                            let query = result.unwrap().unwrap();
                            assert!(query.children().any(|feature| {
                                feature.is("feature", "http://jabber.org/protocol/disco#info")
                                    && feature.attr("var") == Some(crate::file_share::NS)
                            }));
                            self.discovery.borrow_mut().take();
                        }
                        Err(tokio::sync::oneshot::error::TryRecvError::Empty) => return false,
                        Err(error) => panic!("File discovery failed: {error}"),
                    }
                }
                let source = ui
                    .state
                    .borrow()
                    .current()
                    .messages
                    .iter()
                    .find(|m| !m.own && m.attachment.as_ref() == Some(&file))
                    .cloned();
                let Some(message) = source else {
                    return false;
                };
                assert_eq!(message.text, "File caption 😀");
                assert_eq!(message.encrypted, encrypted);
                assert!(message.unsupported_attachment);
                self.id.replace(message.id);
                assert!(click("Download"));
                self.stage.set(2);
            }
            2 => {
                match ui.downloads.borrow().status(&owner, &file) {
                    Some(Status::Failed(reason)) => panic!("native download failed: {reason}"),
                    Some(Status::Running { .. }) => {}
                    _ => return false,
                }
                if click("Cancel") {
                    assert_eq!(
                        ui.downloads.borrow().status(&owner, &file),
                        Some(Status::Cancelled)
                    );
                    self.stage.set(3);
                }
            }
            3 => {
                if click("Retry") {
                    let own = ui.state.borrow_mut().chat_for(&owner).id.clone();
                    ui.select_chat(&own);
                    self.stage.set(4);
                }
            }
            4 => match ui.downloads.borrow().status(&owner, &file) {
                Some(Status::Ready(path)) => {
                    assert_eq!(std::fs::read(&path).unwrap(), Self::plain());
                    ui.select_chat(&self.chat.borrow());
                    self.stage.set(5);
                }
                Some(Status::Failed(reason)) => panic!("native retry failed: {reason}"),
                _ => {}
            },
            5 => {
                if click("Save As…") {
                    self.stage.set(6);
                }
            }
            6 => {
                fn chooser(widget: &gtk::Widget) -> Option<gtk::FileChooserWidget> {
                    if let Some(c) = widget.downcast_ref::<gtk::FileChooserWidget>() {
                        return Some(c.clone());
                    }
                    let mut child = widget.first_child();
                    while let Some(w) = child {
                        child = w.next_sibling();
                        if let Some(c) = chooser(&w) {
                            return Some(c);
                        }
                    }
                    None
                }
                for window in gtk::Window::list_toplevels() {
                    if let Some(chooser) = chooser(&window) {
                        if chooser.current_folder().and_then(|f| f.path()).as_deref()
                            != Self::export().parent()
                        {
                            continue;
                        }
                        chooser.set_current_name("fixture-export.bin");
                        self.stage.set(7);
                        break;
                    }
                }
            }
            7 => {
                for window in gtk::Window::list_toplevels() {
                    if let Some(dialog) = window.downcast_ref::<gtk::Dialog>()
                        && dialog.title().as_deref() == Some("Save Attachment")
                    {
                        dialog.response(gtk::ResponseType::Accept);
                        self.stage.set(8);
                        break;
                    }
                }
            }
            8 if Self::export().exists() => {
                assert_eq!(std::fs::read(Self::export()).unwrap(), Self::plain());
                let state = ui.store.load(&owner);
                let saved = state
                    .chats
                    .iter()
                    .flat_map(|c| &c.messages)
                    .find(|m| m.id == *self.id.borrow())
                    .unwrap();
                assert_eq!(saved.attachment.as_ref(), Some(&file));
                assert_eq!(saved.text, "File caption 😀");
                assert_eq!(ui.composer_text(), "Keep the ordinary poll draft");
                let mut next = file.clone();
                next.wire_url = next.wire_url.replace("-slow", "-slow?next=1");
                ui.downloads
                    .borrow_mut()
                    .start(&owner, next.clone())
                    .unwrap();
                let root = crate::downloads::account_root(&owner).unwrap();
                let demo = ui.demo.borrow().clone();
                ui.activate_state(demo);
                assert!(ui.downloads.borrow().status(&owner, &next).is_none());
                crate::downloads::forget(&owner).unwrap();
                assert!(!root.exists());
                println!(
                    "Native attachment: live SFS/protected metadata, Download/Cancel/Retry, leave-chat transfer, verified Save As export, draft/reload and account cache removal passed (encrypted={encrypted})"
                );
                self.stage.set(9);
                return true;
            }
            _ => {}
        }
        false
    }
}
