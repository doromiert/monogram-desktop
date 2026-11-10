//! Native chooser/composer upload acceptance on fictional XMPP/HTTPS only.
use crate::{downloads::Status, model::*, ui::Ui, uploads::Phase};
use adw::prelude::*;
use gtk::{gdk, gio};
use std::{
    cell::{Cell, RefCell},
    path::PathBuf,
    rc::Rc,
};
#[derive(Default)]
pub struct UploadSmoke {
    draft: RefCell<String>,
    stage_started: Cell<Option<std::time::Instant>>,
    stage: Cell<u8>,
    printed: Cell<u8>,
    id: RefCell<String>,
    file: RefCell<Option<crate::file_share::Attachment>>,
    received: Cell<bool>,
}
impl UploadSmoke {
    fn capture(ui: &Ui, tag: &str, encrypted: bool) {
        let Some(dir) = std::env::var_os("MONOGRAM_IMAGE_SCREENSHOTS") else {
            return;
        };
        let w = ui.window.width() as f32;
        let h = ui.window.height() as f32;
        let snapshot = gtk::Snapshot::new();
        gtk::WidgetPaintable::new(Some(&ui.window)).snapshot(&snapshot, w as f64, h as f64);
        let Some(node) = snapshot.to_node() else {
            return;
        };
        ui.window
            .renderer()
            .unwrap()
            .render_texture(&node, Some(&gtk::graphene::Rect::new(0., 0., w, h)))
            .save_to_png(PathBuf::from(dir).join(format!(
                "{tag}-{}.png",
                if encrypted { "encrypted" } else { "plain" }
            )))
            .unwrap();
    }
    fn photo() -> bool {
        std::env::var("MONOGRAM_IMAGE_FIXTURE").as_deref() == Ok("1")
    }
    fn showing() -> bool {
        std::env::var_os("MONOGRAM_IMAGE_SCREENSHOTS").is_some()
    }
    fn caption() -> &'static str {
        if Self::photo() {
            "Found a lovely spot for Saturday!"
        } else {
            "Uploaded native caption 😀"
        }
    }
    fn name() -> &'static str {
        if Self::photo() {
            "upload-fixture.png"
        } else {
            "upload-fixture.bin"
        }
    }
    fn path() -> PathBuf {
        PathBuf::from(std::env::var_os("HOME").unwrap()).join(Self::name())
    }
    fn plain() -> Vec<u8> {
        if Self::photo() {
            let photo = gtk::gdk_pixbuf::Pixbuf::from_file_at_scale(
                crate::widgets::asset("landscape.svg"),
                1920,
                960,
                true,
            )
            .unwrap();
            gdk::Texture::for_pixbuf(&photo)
                .save_to_png_bytes()
                .to_vec()
        } else {
            (0..196613).map(|n| (n % 251) as u8).collect()
        }
    }
    pub fn start(&self, ui: &Rc<Ui>) {
        self.draft.replace(ui.composer_text());
        if Self::showing() {
            adw::StyleManager::default().set_color_scheme(adw::ColorScheme::ForceDark);
            ui.toast.dismiss_all();
        }
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
        assert_eq!(message.text, Self::caption());
        assert_eq!(message.encrypted, encrypted);
        let file = message.attachment.clone().unwrap();
        assert_eq!(file.name, Self::name());
        if Self::photo() {
            assert_eq!(file.mime, "image/png");
        }
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
                ui.composer.buffer().set_text(Self::caption());
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
                        if Self::photo() {
                            if Self::showing() {
                                let mut state = ui.state.borrow_mut();
                                let chat = state.current_mut();
                                chat.name = "Mira".into();
                                chat.messages.retain(|m| m.id == *self.id.borrow());
                                for (index, text) in [
                                    "Are we still on for Saturday?",
                                    "Yes! I took a photo of the trail.",
                                    "It looks like the weather will hold.",
                                ]
                                .into_iter()
                                .enumerate()
                                {
                                    chat.messages.insert(
                                        index,
                                        Message {
                                            author: if index == 1 {
                                                "You".into()
                                            } else {
                                                "Mira".into()
                                            },
                                            text: text.into(),
                                            own: index == 1,
                                            encrypted,
                                            ..Default::default()
                                        },
                                    );
                                }
                                drop(state);
                                ui.toast.dismiss_all();
                                ui.show_chat();
                                ui.refresh_chats(None);
                                ui.render_messages(true);
                            }
                            self.stage_started.set(Some(std::time::Instant::now()));
                            self.stage.set(20);
                        } else {
                            self.stage.set(10);
                            ui.composer.buffer().set_text(&self.draft.borrow());
                            ui.save_draft();
                            return true;
                        }
                    }
                    Some(Status::Failed(reason)) => panic!("uploaded cache failed: {reason}"),
                    _ => {}
                }
            }
            20 => {
                if Self::showing()
                    && self
                        .stage_started
                        .get()
                        .is_some_and(|t| t.elapsed() < std::time::Duration::from_millis(450))
                {
                    return false;
                }
                let bubble = ui
                    .bubbles
                    .borrow()
                    .get(&*self.id.borrow())
                    .cloned()
                    .unwrap();
                if let Some(button) =
                    crate::smoke::button(&bubble, "View image").filter(|b| b.is_visible())
                {
                    let original = ui
                        .state
                        .borrow()
                        .message(&self.id.borrow())
                        .unwrap()
                        .clone();
                    let mut state = ui.state.borrow().clone();
                    let time = chrono::DateTime::parse_from_rfc3339(&original.timestamp).unwrap();
                    for n in -12..=12 {
                        if n == 0 {
                            continue;
                        }
                        let mut m = original.clone();
                        m.id = new_id();
                        m.wire_id = m.id.clone();
                        m.aliases = vec![format!("wire:{}", m.id), format!("origin:{}", m.id)];
                        m.timestamp = (time + chrono::Duration::seconds(n))
                            .to_rfc3339_opts(chrono::SecondsFormat::Micros, false);
                        state.current_mut().messages.push(m);
                    }
                    ui.store.save(&state).unwrap();
                    assert_eq!(
                        ui.store
                            .media_window(&state.account, &state.current().jid, &original.id)
                            .unwrap()
                            .messages
                            .len(),
                        21
                    );
                    Self::capture(ui, "chat", encrypted);
                    button.emit_clicked();
                    self.stage.set(21);
                }
            }
            21 => {
                let viewer = ui.active_media.borrow().clone().unwrap();
                if viewer.root.opacity() > 0.99 {
                    Self::capture(ui, "viewer", encrypted);
                    assert!(
                        crate::smoke::button(&viewer.root.clone().upcast(), "Next media")
                            .unwrap()
                            .is_sensitive()
                    );
                    crate::smoke::button(&viewer.root.clone().upcast(), "Next media")
                        .unwrap()
                        .emit_clicked();
                    let id = viewer
                        .root
                        .child()
                        .and_downcast::<gtk::Stack>()
                        .unwrap()
                        .visible_child_name()
                        .unwrap();
                    assert_ne!(id.as_str(), self.id.borrow().as_str());
                    assert!(
                        ui.state.borrow().message(&id).is_none(),
                        "viewer must reach unloaded history"
                    );
                    self.stage.set(22);
                }
            }
            22 => {
                let viewer = ui.active_media.borrow().clone().unwrap();
                let next =
                    crate::smoke::button(&viewer.root.clone().upcast(), "Next media").unwrap();
                for _ in 0..11 {
                    next.emit_clicked();
                }
                assert!(!next.is_sensitive());
                let previous =
                    crate::smoke::button(&viewer.root.clone().upcast(), "Previous media").unwrap();
                for _ in 0..12 {
                    previous.emit_clicked();
                }
                assert_eq!(
                    viewer
                        .root
                        .child()
                        .and_downcast::<gtk::Stack>()
                        .unwrap()
                        .visible_child_name()
                        .as_deref(),
                    Some(self.id.borrow().as_str())
                );
                crate::smoke::button(&viewer.root.clone().upcast(), "Back")
                    .unwrap()
                    .emit_clicked();
                self.stage.set(23);
            }
            23 => {
                if let Some(viewer) = ui.active_media.borrow().clone() {
                    assert!(viewer.root.opacity() < 1.);
                } else {
                    ui.shared_media();
                    assert!(ui.active_media.borrow().is_none());
                    self.stage_started.set(Some(std::time::Instant::now()));
                    self.stage.set(24);
                }
            }
            24 => {
                if self
                    .stage_started
                    .get()
                    .is_some_and(|t| t.elapsed() < std::time::Duration::from_millis(450))
                {
                    return false;
                }
                let pane = ui.utility.sidebar().unwrap();
                fn grid(w: &gtk::Widget) -> Option<gtk::GridView> {
                    if let Some(g) = w.downcast_ref::<gtk::GridView>() {
                        return Some(g.clone());
                    }
                    let mut c = w.first_child();
                    while let Some(w) = c {
                        c = w.next_sibling();
                        if let Some(g) = grid(&w) {
                            return Some(g);
                        }
                    }
                    None
                }
                let Some(grid) = grid(&pane) else {
                    return false;
                };
                assert_eq!((grid.min_columns(), grid.max_columns()), (3, 3));
                if grid.model().unwrap().n_items() == 0 {
                    return false;
                }
                assert_eq!(grid.model().unwrap().n_items(), 9);
                let photo = crate::smoke::button(&pane, Self::name()).unwrap();
                assert!(photo.width() >= 70 && (photo.width() - photo.height()).abs() <= 4);
                Self::capture(ui, "shared-media", encrypted);
                println!(
                    "Native photo: sandboxed inline preview, protected PNG upload, bounded database viewer, unloaded-history navigation, rebase/endpoints, animated close and three-column shared-media grid passed encrypted={encrypted}"
                );
                ui.utility.set_show_sidebar(false);
                ui.composer.buffer().set_text(&self.draft.borrow());
                ui.save_draft();
                self.stage.set(10);
                return true;
            }
            _ => {}
        }
        false
    }
}
