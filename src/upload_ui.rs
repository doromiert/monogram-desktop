//! Durable pending attachments, bounded upload workers, and native progress controls.
use crate::{
    encryption_policy::Mode,
    model::*,
    protocol::Command,
    ui::Ui,
    uploads::{self, Draft, Job, Phase, Uploaded},
    widgets::*,
};
use adw::prelude::*;
use gtk::glib;
use std::{
    cell::Cell,
    collections::VecDeque,
    rc::Rc,
    sync::{Arc, mpsc},
    time::Duration,
};
pub(crate) fn pending_media(message: &Message) -> Option<crate::file_share::Attachment> {
    let draft = message.upload.as_ref()?;
    let file = crate::file_share::Attachment {
        wire_url: String::new(),
        name: draft.name.clone(),
        mime: String::new(),
        size: None,
        width: None,
        height: None,
        hashes: Default::default(),
        manual: true,
        sticker: draft.sticker,
    };
    file.media_candidate().then_some(file)
}
#[derive(Clone)]
struct Work {
    owner: String,
    peer: String,
    id: String,
    generation: u64,
    encryption_revision: u64,
    mode: Mode,
    fingerprint: String,
    draft: Draft,
}
impl Work {
    fn policy_unchanged(&self, chat: &Chat, revision: u64) -> bool {
        // PQ preflight approvals are not reusable cache entries. A cache miss does
        // not revoke this upload's approval; settings and identity changes still do.
        self.encryption_revision == revision
            && chat.encryption_preference().forced().is_none_or(|mode| mode == self.mode)
            && !chat.downgrade_blocked(self.mode)
    }
}

struct Task {
    work: Work,
    iq: crate::iq::Client,
    job: Arc<Job>,
    cancel: Option<tokio::sync::oneshot::Sender<()>>,
    result: Option<mpsc::Receiver<Result<Uploaded, &'static str>>>,
}
impl Drop for Task {
    fn drop(&mut self) {
        self.job.stop();
        if let Some(tx) = self.cancel.take() {
            let _ = tx.send(());
        }
    }
}
#[derive(Default)]
pub struct Uploads {
    tasks: VecDeque<Task>,
}
impl Uploads {
    fn push(&mut self, work: Work, iq: crate::iq::Client) -> Result<(), &'static str> {
        if self
            .tasks
            .iter()
            .any(|t| t.work.id == work.id && t.work.owner == work.owner)
        {
            return Ok(());
        }
        if self.tasks.len() >= 64 {
            return Err("Wait for queued uploads to finish");
        }
        self.tasks.push_back(Task {
            work,
            iq,
            job: Arc::new(Job::default()),
            cancel: None,
            result: None,
        });
        Ok(())
    }
    pub(crate) fn phase(&self, id: &str) -> Option<Phase> {
        self.tasks.iter().find(|t| t.work.id == id).map(|t| {
            if t.result.is_none() {
                Phase::Discovering
            } else {
                t.job.phase.lock().unwrap().clone()
            }
        })
    }
    pub fn cancel(&mut self, id: &str) {
        self.tasks.retain(|t| t.work.id != id);
    }
    fn clear(&mut self) -> Vec<Work> {
        self.tasks.drain(..).map(|t| t.work.clone()).collect()
    }
    fn poll(&mut self) -> Vec<(Work, Result<Uploaded, &'static str>)> {
        let mut complete = vec![];
        self.tasks.retain_mut(|task| {
            let Some(rx) = &task.result else {
                return true;
            };
            match rx.try_recv() {
                Ok(result) => {
                    complete.push((task.work.clone(), result));
                    false
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    complete.push((task.work.clone(), Err("Upload worker stopped")));
                    false
                }
                Err(mpsc::TryRecvError::Empty) => true,
            }
        });
        let mut active = self.tasks.iter().filter(|t| t.result.is_some()).count();
        for task in &mut self.tasks {
            if active >= 4 {
                break;
            }
            if task.result.is_some() {
                continue;
            }
            let (cancel, stopped) = tokio::sync::oneshot::channel();
            let (tx, rx) = mpsc::channel();
            task.cancel = Some(cancel);
            task.result = Some(rx);
            active += 1;
            let work = task.work.clone();
            let job = task.job.clone();
            let iq = task.iq.clone();
            std::thread::spawn(move || {
                let result=match tokio::runtime::Builder::new_current_thread().enable_all().build(){
                    Ok(rt)=>rt.block_on(async{tokio::select!{biased; _=stopped=>Err("Upload cancelled"), result=uploads::run(&iq,&work.owner,&work.id,&work.draft,work.mode,job)=>result}}),
                    Err(_)=>Err("Upload runtime unavailable"),
                };
                let _ = tx.send(result);
            });
        }
        complete
    }
}
impl Ui {
    pub fn release_upload_captions(self: &Rc<Self>) {
        if !self.online.get() || !self.legacy_crypto.borrow().ready {
            return;
        }
        let owner = self.state.borrow().account.clone();
        let Ok(ready) = self.store.ready_upload_captions(&owner) else {
            self.notice("Pending captions could not be read");
            return;
        };
        let mut identity_changed = false;
        for (peer, candidate) in ready {
            if self
                .state
                .borrow()
                .chats
                .iter()
                .any(|chat| chat.jid == peer && chat.group && !chat.room_can_send)
            {
                continue;
            }
            let Ok(Some(message)) = self
                .store
                .claim_upload_caption(&owner, &peer, &candidate.id)
            else {
                continue;
            };
            if let Some(row) = self
                .state
                .borrow_mut()
                .chat_for(&peer)
                .messages
                .iter_mut()
                .find(|row| row.id == message.id)
            {
                *row = message.clone();
            }
            if self.fingerprint_for_send(Mode::Legacy).as_deref()
                != Some(message.fingerprint.as_str())
            {
                self.delivery(&peer, &message.wire_id, "failed");
                identity_changed = true;
            } else if !self.command(Command::Send {
                peer: peer.clone(),
                quote: String::new(),
                message: message.clone(),
            }) {
                self.delivery(&peer, &message.wire_id, "failed");
            }
        }
        if identity_changed {
            self.notice("Caption kept for retry: the Legacy OMEMO identity changed");
        }
        self.render_messages(false);
    }
    fn upload_row(&self, owner: &str, peer: &str, message: Message) -> bool {
        match self.store.update_upload(owner, peer, &message) {
            Ok(true) => {
                if self.state.borrow().account == owner {
                    let mut s = self.state.borrow_mut();
                    if s.current().jid == peer {
                        self.history_window.content_changed();
                    }
                    if let Some(chat) = s.chats.iter_mut().find(|c| c.jid == peer) {
                        if chat
                            .latest
                            .as_ref()
                            .is_some_and(|latest| latest.id == message.id)
                        {
                            chat.latest = Some(MessagePreview::from(&message));
                        }
                        if let Some(row) = chat.messages.iter_mut().find(|m| m.id == message.id) {
                            *row = message;
                        }
                    }
                }
                true
            }
            Ok(false) => false,
            Err(_) => {
                self.notice("Upload status could not be saved");
                false
            }
        }
    }
    pub fn stop_uploads(&self) {
        let works = self.uploads.borrow_mut().clear();
        for work in works {
            self.fail_upload(
                &work.owner,
                &work.peer,
                &work.id,
                "Upload interrupted. Retry to send.",
            );
        }
    }
    fn fail_upload(&self, owner: &str, peer: &str, id: &str, reason: &str) {
        if let Some(mut m) = self.store.message(owner, peer, id) {
            if let Some(draft) = &mut m.upload {
                draft.error = Some(reason.into());
                m.delivery = "failed".into();
                self.upload_row(owner, peer, m);
            }
        }
    }
    pub(crate) fn enforce_blocked_uploads(self: &Rc<Self>) {
        let blocked: Vec<_> = self
            .uploads
            .borrow()
            .tasks
            .iter()
            .filter(|task| self.peer_blocked(&task.work.peer))
            .map(|task| task.work.clone())
            .collect();
        for work in blocked {
            self.uploads.borrow_mut().cancel(&work.id);
            self.fail_upload(
                &work.owner,
                &work.peer,
                &work.id,
                "Unblock this address before retrying",
            );
        }
    }
    pub fn start_upload(self: &Rc<Self>, peer: &str, id: &str) {
        if self.uploads.borrow().phase(id).is_some()
            || self.state.borrow().current().jid != peer
        {
            return;
        }
        let peer = peer.to_owned();
        let id = id.to_owned();
        self.choose_sending(move |ui, mode| ui.start_upload_with_mode(&peer, &id, mode));
    }
    pub(crate) fn start_upload_with_mode(self: &Rc<Self>, peer: &str, id: &str, mode: Mode) {
        if self.peer_blocked(peer) {
            self.notice("Unblock this address before uploading. Your file is kept.");
            return;
        }
        if self.uploads.borrow().phase(id).is_some() {
            return;
        }
        let owner = self.state.borrow().account.clone();
        if owner.is_empty() || !self.online.get() {
            self.notice("Connect before uploading. Your file is kept.");
            return;
        }
        let Some(mut message) = self.store.message(&owner, peer, id) else {
            return;
        };
        let Some(mut draft) = message.upload.clone() else {
            return;
        };
        if mode != message.mode() {
            self.notice(&format!("Restore {} to retry this upload, or cancel and attach the file again with the new mode.", message.mode().name()));
            return;
        }
        let Some(fingerprint) = self.fingerprint_for_send(mode) else {
            self.notice("The selected OMEMO version is not ready");
            return;
        };
        draft.error = None;
        message.upload = Some(draft.clone());
        message.set_encryption(mode, fingerprint.clone());
        message.delivery = "sending".into();
        if !self.upload_row(&owner, peer, message) {
            return;
        }
        let Some(iq) = self
            .session
            .borrow()
            .as_ref()
            .map(|s| crate::iq::Client::new(s.tx.clone()))
        else {
            self.fail_upload(&owner, peer, id, "Connection stopped. Retry to send.");
            return;
        };
        let result = self.uploads.borrow_mut().push(
            Work {
                owner: owner.clone(),
                peer: peer.into(),
                id: id.into(),
                generation: self.generation.get(),
                encryption_revision: self.encryption_revision.get(),
                mode,
                fingerprint,
                draft,
            },
            iq,
        );
        if let Err(reason) = result {
            self.fail_upload(&owner, peer, id, reason);
            self.notice(reason);
        }
    }
    pub fn poll_uploads(self: &Rc<Self>) {
        let completed = self.uploads.borrow_mut().poll();
        if completed.is_empty() {
            return;
        }
        for (work, result) in completed {
            if self.state.borrow().account != work.owner || self.generation.get() != work.generation
            {
                continue;
            }
            let Some(mut message) = self
                .store
                .message(&work.owner, &work.peer, &work.id)
                .filter(|m| m.upload.is_some())
            else {
                continue;
            };
            let mode = work.mode;
            let unchanged = self.state.borrow().chats.iter()
                .find(|chat| chat.jid == work.peer)
                .is_some_and(|chat| work.policy_unchanged(chat, self.encryption_revision.get()))
                && message.mode() == mode
                && message.fingerprint == work.fingerprint
                && self.fingerprint_for_send(mode).as_deref() == Some(work.fingerprint.as_str());
            if !unchanged || !self.online.get() {
                self.fail_upload(
                    &work.owner,
                    &work.peer,
                    &work.id,
                    "Connection or encryption changed. Retry to send.",
                );
                continue;
            }
            match result {
                Err(reason) => self.fail_upload(&work.owner, &work.peer, &work.id, reason),
                Ok(uploaded) => {
                    message.upload = None;
                    message.attachment = Some(uploaded.attachment.clone());
                    message.unsupported_attachment = true;
                    message.delivery = "sending".into();
                    if !self.upload_row(&work.owner, &work.peer, message.clone()) {
                        continue;
                    }
                    // Metadata is durable before moving the snapshot or submitting the stanza.
                    let weak = Rc::downgrade(self);
                    let history = self.store.db.path().map(std::path::PathBuf::from);
                    glib::MainContext::default().spawn_local(async move {
                        let account = work.owner.clone();
                        let file = uploaded.attachment.clone();
                        let _ = gtk::gio::spawn_blocking(move || {
                            let _guard = crate::downloads::CACHE_IO.lock().unwrap();
                            let retained = history
                                .as_ref()
                                .and_then(|path| {
                                    rusqlite::Connection::open_with_flags(
                                        path,
                                        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
                                    )
                                    .ok()
                                })
                                .is_some_and(|db| {
                                    db.query_row(
                                        "SELECT EXISTS(SELECT 1 FROM accounts WHERE jid=?)",
                                        [&account],
                                        |row| row.get::<_, bool>(0),
                                    )
                                    .unwrap_or(false)
                                });
                            if !retained {
                                let _ = std::fs::remove_file(&uploaded.snapshot);
                                return;
                            }
                            let moved =
                                crate::downloads::account_root(&account)
                                    .ok()
                                    .is_some_and(|root| {
                                        std::fs::rename(&uploaded.snapshot, root.join(file.key()))
                                            .is_ok()
                                    });
                            if !moved {
                                let _ = std::fs::remove_file(&uploaded.snapshot);
                            }
                        })
                        .await;
                        let Some(ui) = weak.upgrade().filter(|ui| {
                            ui.generation.get() == work.generation
                                && ui.state.borrow().account == work.owner
                        }) else {
                            return;
                        };
                        if !ui.online.get()
                            || ui.fingerprint_for_send(work.mode).as_deref()
                                != Some(work.fingerprint.as_str())
                            || ui
                                .state
                                .borrow()
                                .chats
                                .iter()
                                .find(|chat| chat.jid == work.peer)
                                .and_then(|chat| ui.sending_mode(chat))
                                != Some(work.mode)
                            || !ui
                                .store
                                .message(&work.owner, &work.peer, &message.id)
                                .is_some_and(|stored| {
                                    !stored.retracted
                                        && stored.upload.is_none()
                                        && stored.attachment == message.attachment
                                })
                        {
                            ui.delivery(&work.peer, &message.wire_id, "failed");
                            return;
                        }
                        if !ui.command(Command::Send {
                            peer: work.peer.clone(),
                            quote: message.reply.clone(),
                            message: message.clone(),
                        }) {
                            ui.delivery(&work.peer, &message.wire_id, "failed");
                        }
                    });
                }
            }
        }
        self.render_messages(false);
        self.refresh_chats(None);
    }
    pub fn send_files(self: &Rc<Self>, caption: &str, mode: Mode) -> bool {
        if self.peer_blocked(&self.state.borrow().current().jid) {
            self.notice("Unblock this address before uploading. Your files are kept.");
            return false;
        }
        if !self.online.get() {
            self.notice("Connect before uploading. Your draft is kept.");
            return false;
        }
        let encrypted = mode.encrypted();
        let Some(fingerprint) = self.fingerprint_for_send(mode) else {
            self.notice("Enable the selected OMEMO version and review devices before uploading");
            return false;
        };
        let files = self.attachments.borrow().clone();
        if files.len() + self.uploads.borrow().tasks.len() > 64 {
            self.notice("Send at most 64 queued attachments at a time");
            return false;
        }
        let reply = self
            .replying
            .borrow()
            .as_ref()
            .and_then(|id| self.active_message(id));
        let (owner, peer, group) = {
            let s = self.state.borrow();
            (
                s.account.clone(),
                s.current().jid.clone(),
                s.current().group,
            )
        };
        let mut ids = vec![];
        for (index, file) in files.into_iter().enumerate() {
            let id = new_id();
            let filename = file
                .basename()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_else(|| "attachment".into());
            let mut message = Message {
                id: id.clone(),
                wire_id: id.clone(),
                aliases: vec![format!("wire:{id}"), format!("origin:{id}")],
                author: "You".into(),
                sender: owner.clone(),
                own: true,
                kind: "file".into(),
                text: if index == 0 && mode != Mode::Legacy {
                    caption.trim().into()
                } else {
                    String::new()
                },
                encrypted,
                encryption_mode: Some(mode),
                fingerprint: fingerprint.clone(),
                unsupported_attachment: true,
                delivery: "sending".into(),
                upload: Some(Draft {
                    uri: file.uri().into(),
                    name: filename,
                    error: None,
                    sticker: false,
                    local_sticker: None,
                }),
                ..Default::default()
            };
            if let Some(r) = &reply {
                message.reply = format!("{} · {}", r.author, r.reply_preview());
                message.reply_id = crate::message_actions::stanza_reference(r).unwrap_or_default();
                message.reply_to = if group {
                    crate::message_actions::room_reply_address(r, &peer)
                } else if r.own {
                    owner.clone()
                } else {
                    peer.clone()
                };
            }
            self.state.borrow_mut().current_mut().messages.push(message);
            ids.push(id);
        }
        if mode == Mode::Legacy && !caption.trim().is_empty() && !ids.is_empty() {
            let id = new_id();
            let message = Message {
                id: id.clone(),
                wire_id: id.clone(),
                aliases: vec![format!("wire:{id}"), format!("origin:{id}")],
                text: caption.trim().into(),
                author: "You".into(),
                sender: owner.clone(),
                own: true,
                encrypted: true,
                encryption_mode: Some(mode),
                fingerprint,
                upload_caption_for: ids[0].clone(),
                delivery: "waiting-upload".into(),
                ..Default::default()
            };
            self.state.borrow_mut().current_mut().messages.push(message);
            ids.push(id);
        }
        if !self.persist() {
            self.state
                .borrow_mut()
                .current_mut()
                .messages
                .retain(|m| !ids.contains(&m.id));
            return false;
        }
        for id in ids {
            self.start_upload_with_mode(&peer, &id, mode);
        }
        true
    }
    pub(crate) fn pending_sticker_content(self:&Rc<Self>,message:&Message)->gtk::Box {
        if let Some(item)=message.upload.as_ref().and_then(|draft|draft.local_sticker){self.pack_item_preview(item,168,false)}else{bx(true,0)}
    }
    pub(crate) fn pack_item_preview(self:&Rc<Self>,item:i64,edge:i32,loading:bool)->gtk::Box {
        let content=bx(true,0);let picture=AspectImage::thumbnail(edge,true);let overlay=gtk::Overlay::new();overlay.set_child(Some(&picture));let spinner=gtk::Spinner::new();spinner.set_spinning(loading);spinner.set_visible(loading);spinner.set_halign(gtk::Align::Center);spinner.set_valign(gtk::Align::Center);overlay.add_overlay(&spinner);content.append(&overlay);let indicator=spinner.downgrade();
        let owner=self.state.borrow().account.clone();let generation=self.generation.get();let weak=Rc::downgrade(self);let image=picture.downgrade();
        glib::spawn_future_local(async move {
            let result=async {
                let library=crate::packs::Library::open(&owner).await.map_err(|e|e.to_string())?;
                loop {
                    let Some(picture)=image.upgrade()else{return Err("Preview closed".into());};
                    let visible=picture.is_mapped()&&picture.ancestor(gtk::ScrolledWindow::static_type()).and_then(|w|w.downcast::<gtk::ScrolledWindow>().ok()).is_some_and(|scroll|picture.compute_bounds(&scroll).is_some_and(|bounds|bounds.y()+bounds.height()>0.&&bounds.y()<scroll.height()as f32));
                    if visible{break;}glib::timeout_future(Duration::from_millis(50)).await;
                }
                let _permit=crate::media_library_ui::lottie_permit().await;
                if image.upgrade().is_none(){return Err("Preview closed".into());}
                let (asset,input)=library.verified_asset(item,crate::packs::Job::new()).await.map_err(|e|e.to_string())?;
                gtk::gio::spawn_blocking(move||{
                    let cancel=gtk::gio::Cancellable::new();
                    if asset.item.mime=="video/webm" {
                        use std::os::fd::AsRawFd;
                        let path=std::path::PathBuf::from(format!("/proc/self/fd/{}",input.0.as_raw_fd()));
                        let preview=crate::image_loader::encode_video_preview(&path,&cancel,1500)?;
                        gtk::gdk::Texture::from_bytes(&glib::Bytes::from_owned(preview.bytes)).map_err(|_|"Sticker preview unavailable")
                    } else {crate::image_loader::Decoder::open_sticker(input,&cancel)?.frame(336).map(|frame|frame.texture)}
                }).await.map_err(|_|"Preview worker stopped".to_string())?.map_err(str::to_string)
            }.await;
            let(Some(ui),Some(picture))=(weak.upgrade(),image.upgrade())else{return;};
            if ui.generation.get()!=generation||ui.state.borrow().account!=owner{return;}
            if let Some(spinner)=indicator.upgrade(){spinner.stop();spinner.set_visible(false);}
            match result {Ok(texture)=>picture.set_paintable(Some(texture.upcast())),Err(error)=>picture.set_tooltip_text(Some(&error))}
        });content
    }
    pub(crate) fn media_upload_content(self: &Rc<Self>, message: &Message) -> gtk::Box {
        let draft = message.upload.as_ref().unwrap();
        let file = pending_media(message).unwrap();
        let owner = self.state.borrow().account.clone();
        let peer = self.state.borrow().current().jid.clone();
        let generation = self.generation.get();
        let id = message.id.clone();
        let content = bx(true, 0);
        let overlay = gtk::Overlay::new();
        overlay.add_css_class("media-download");
        overlay.add_css_class("media-upload");
        if file.sticker {
            overlay.add_css_class("sticker-download");
        }
        if draft.local_sticker.is_some() {
            overlay.set_child(Some(&self.pending_sticker_content(message)));
        } else {
            let picture =
                AspectImage::thumbnail(if file.sticker { 168 } else { 320 }, file.sticker);
            picture.set_ratio(4. / 3.);
            overlay.set_child(Some(&picture));
            if let Some(path) = gtk::gio::File::for_uri(&draft.uri).path() {
                if file.video_candidate() {
                    let weak = Rc::downgrade(self);
                    let image = picture.downgrade();
                    let account = owner.clone();
                    glib::spawn_future_local(async move {
                        let _permit = crate::media_library_ui::lottie_permit().await;
                        if image.upgrade().is_none() {
                            return;
                        }
                        let result = gtk::gio::spawn_blocking(move || {
                            crate::image_loader::encode_video_preview(
                                &path,
                                &gtk::gio::Cancellable::new(),
                                1500,
                            )
                        })
                        .await;
                        let (Some(ui), Some(picture)) = (weak.upgrade(), image.upgrade()) else {
                            return;
                        };
                        if ui.generation.get() != generation || ui.state.borrow().account != account
                        {
                            return;
                        }
                        if let Ok(Ok(preview)) = result {
                            if let Ok(texture) = gtk::gdk::Texture::from_bytes(
                                &glib::Bytes::from_owned(preview.bytes),
                            ) {
                                picture.set_paintable(Some(texture.upcast()));
                            }
                        }
                    });
                } else {
                    let weak = Rc::downgrade(self);
                    let account = owner.clone();
                    let visible_ui = Rc::downgrade(self);
                    crate::image_loader::play_input(
                        &picture,
                        640,
                        move |picture| visible_ui.upgrade().is_some_and(|ui| ui.in_chat_viewport(picture)),
                        move |picture, texture| {
                            if weak.upgrade().is_some_and(|ui| {
                                ui.generation.get() == generation
                                    && ui.state.borrow().account == account
                            }) {
                                picture.set_paintable(Some(texture.upcast()));
                            }
                        },
                        file.sticker,
                        move || {
                            let input = std::fs::File::open(&path)
                                .map_err(|_| "Upload preview unavailable")?;
                            let metadata =
                                input.metadata().map_err(|_| "Upload preview unavailable")?;
                            if !metadata.is_file() || metadata.len() > crate::file_share::MAX {
                                return Err("Upload preview unavailable");
                            }
                            Ok(crate::downloads::VerifiedFile(input))
                        },
                    );
                }
            }
        }
        let controls = bx(true, 4);
        controls.set_halign(gtk::Align::Center);
        controls.set_valign(gtk::Align::Center);
        controls.add_css_class("media-download-controls");
        let fraction = Rc::new(Cell::new(None));
        let displayed = Rc::new(Cell::new(0.));
        let ring = transfer_progress_ring(fraction.clone(), displayed.clone());
        let action = gtk::Overlay::new();
        action.set_child(Some(&ring));
        let button = gtk::Button::from_icon_name(if draft.error.is_some() {
            "view-refresh-symbolic"
        } else {
            "process-stop-symbolic"
        });
        button.add_css_class("circular");
        button.add_css_class("media-download-action");
        button.set_halign(gtk::Align::Center);
        button.set_valign(gtk::Align::Center);
        let action_name = if draft.error.is_some() { "Retry upload" } else { "Cancel upload" };
        button.set_tooltip_text(Some(action_name));
        button.update_property(&[gtk::accessible::Property::Label(action_name)]);
        action.add_overlay(&button);
        controls.append(&action);
        let status = text(
            draft.error.as_deref().unwrap_or("Queued for upload"),
            "caption",
        );
        status.add_css_class("media-upload-status");
        status.set_max_width_chars(26);
        status.set_ellipsize(gtk::pango::EllipsizeMode::End);
        controls.append(&status);
        overlay.add_overlay(&controls);
        content.append(&overlay);
        let weak = Rc::downgrade(self);
        let account = owner.clone();
        let destination = peer.clone();
        let target = id.clone();
        button.connect_clicked(move |_| {
            let Some(ui) = weak.upgrade().filter(|ui| {
                ui.generation.get() == generation && ui.state.borrow().account == account
            }) else {
                return;
            };
            if ui.uploads.borrow().phase(&target).is_some() {
                ui.uploads.borrow_mut().cancel(&target);
                ui.fail_upload(
                    &account,
                    &destination,
                    &target,
                    "Upload cancelled. Retry to send.",
                );
                if ui.state.borrow().current().jid == destination {
                    ui.render_messages(false);
                }
            } else {
                ui.start_upload(&destination, &target);
            }
        });
        let weak = Rc::downgrade(self);
        let action = button.downgrade();
        let label = status.downgrade();
        let disk = ring.downgrade();
        let running = Rc::new(Cell::new(false));
        let animate = Rc::new(Cell::new(false));
        let fallback = draft
            .error
            .clone()
            .unwrap_or_else(|| "Upload interrupted. Retry to send.".into());
        glib::timeout_add_local(Duration::from_millis(80), move || {
            let (Some(ui), Some(button), Some(label), Some(ring)) = (
                weak.upgrade(),
                action.upgrade(),
                label.upgrade(),
                disk.upgrade(),
            ) else {
                return glib::ControlFlow::Break;
            };
            if ui.generation.get() != generation || ui.state.borrow().account != owner {
                return glib::ControlFlow::Break;
            }
            let phase = ui.uploads.borrow().phase(&id);
            let active = phase.is_some();
            let (progress, detail) = match phase {
                Some(Phase::Uploading { bytes, total }) => (
                    Some((bytes as f64 / total.max(1) as f64).clamp(0., 1.)),
                    format!(
                        "{} of {}",
                        glib::format_size(bytes),
                        glib::format_size(total)
                    ),
                ),
                Some(Phase::Waiting) => (Some(1.), "Confirming upload…".into()),
                Some(Phase::Discovering) => (None, "Finding upload service…".into()),
                Some(Phase::Preparing) => (None, "Preparing upload…".into()),
                Some(Phase::Reconnecting) => (None, "Reconnecting upload…".into()),
                None => (Some(0.), fallback.clone()),
            };
            button.set_icon_name(if active {
                "process-stop-symbolic"
            } else {
                "view-refresh-symbolic"
            });
            let action_name = if active { "Cancel upload" } else { "Retry upload" };
            button.set_tooltip_text(Some(&format!("{action_name} · {detail}")));
            button.update_property(&[gtk::accessible::Property::Label(action_name)]);
            label.set_text(&detail);
            let previous = fraction.replace(progress);
            if previous.is_none() && progress.is_some() {
                displayed.set(progress.unwrap_or_default());
            }
            let visible = active && ui.in_chat_viewport(&ring);
            animate.set(visible);
            if visible && !running.replace(true) {
                let (fraction, displayed, animate, running) = (
                    fraction.clone(),
                    displayed.clone(),
                    animate.clone(),
                    running.clone(),
                );
                let last = Cell::new(None::<i64>);
                ring.add_tick_callback(move |ring, clock| {
                    if !animate.get() || !ring.is_mapped() {
                        running.set(false);
                        return glib::ControlFlow::Break;
                    }
                    let now = clock.frame_time();
                    let dt = last.replace(Some(now)).map_or(1. / 60., |old| {
                        ((now - old) as f64 / 1_000_000.).clamp(0., 0.1)
                    });
                    if let Some(target) = fraction.get() {
                        displayed.set(
                            displayed.get() + (target - displayed.get()) * (1. - (-dt * 16.).exp()),
                        );
                    }
                    ring.queue_draw();
                    glib::ControlFlow::Continue
                });
            } else if !active {
                displayed.set(progress.unwrap_or_default());
                ring.queue_draw();
            }
            glib::ControlFlow::Continue
        });
        content
    }
    pub fn upload_content(self: &Rc<Self>, message: &Message) -> gtk::Box {
        let draft = message.upload.as_ref().unwrap();
        let id = message.id.clone();
        let owner = self.state.borrow().account.clone();
        let peer = self.state.borrow().current().jid.clone();
        let content = bx(true, 8);
        let line = bx(false, 10);
        let info = bx(true, 2);
        info.set_hexpand(true);
        info.append(&wrap(&draft.name, "heading"));
        let detail = wrap(
            draft.error.as_deref().unwrap_or("Queued for upload"),
            "caption dim-label",
        );
        info.append(&detail);
        line.append(&info);
        let action = gtk::Button::with_label("Cancel");
        action.set_valign(gtk::Align::Center);
        line.append(&action);
        content.append(&line);
        let progress = gtk::ProgressBar::new();
        content.append(&progress);
        let weak = Rc::downgrade(self);
        let target = id.clone();
        let account = owner.clone();
        let dest = peer.clone();
        action.connect_clicked(move |_| {
            let Some(ui) = weak
                .upgrade()
                .filter(|ui| ui.state.borrow().account == account)
            else {
                return;
            };
            let active = ui.uploads.borrow().phase(&target).is_some();
            if active {
                ui.uploads.borrow_mut().cancel(&target);
                ui.fail_upload(&account, &dest, &target, "Upload cancelled. Retry to send.");
            } else {
                ui.start_upload(&dest, &target);
            }
        });
        let weak = Rc::downgrade(self);
        let a = action.downgrade();
        let p = progress.downgrade();
        let d = detail.downgrade();
        glib::timeout_add_local(Duration::from_millis(150), move || {
            let (Some(ui), Some(action), Some(progress), Some(detail)) =
                (weak.upgrade(), a.upgrade(), p.upgrade(), d.upgrade())
            else {
                return glib::ControlFlow::Break;
            };
            if ui.state.borrow().account != owner {
                return glib::ControlFlow::Break;
            }
            let phase = ui.uploads.borrow().phase(&id);
            progress.set_visible(phase.is_some());
            action.set_label(if phase.is_some() { "Cancel" } else { "Retry" });
            match phase {
                Some(Phase::Discovering) => {
                    progress.pulse();
                    detail.set_text("Finding upload service…");
                }
                Some(Phase::Preparing) => {
                    progress.pulse();
                    detail.set_text("Preparing private file copy…");
                }
                Some(Phase::Waiting) => {
                    progress.set_fraction(1.);
                    detail.set_text("Confirming upload…");
                }
                Some(Phase::Reconnecting) => {
                    progress.pulse();
                    detail.set_text("Reconnecting upload…");
                }
                Some(Phase::Uploading { bytes, total }) => {
                    progress.set_fraction((bytes as f64 / total.max(1) as f64).clamp(0., 1.));
                    detail.set_text(&format!(
                        "{} of {}",
                        glib::format_size(bytes),
                        glib::format_size(total)
                    ));
                }
                None => {
                    let reason = ui
                        .store
                        .message(&owner, &peer, &id)
                        .and_then(|m| m.upload)
                        .and_then(|d| d.error)
                        .unwrap_or_else(|| "Upload interrupted. Retry to send.".into());
                    detail.set_text(&reason);
                }
            }
            glib::ControlFlow::Continue
        });
        content
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    include!("../tests/media_upload_native.rs");
    #[test]
    fn approved_pq_upload_survives_uncacheable_selection_but_not_policy_changes() {
        let mut chat = Chat::default();
        chat.remember_selection(Mode::Pq);
        assert_eq!(chat.selected_encryption(), None);
        let work = Work {
            owner: "owner@fixture.invalid".into(),
            peer: "peer@fixture.invalid".into(),
            id: "approved-upload".into(),
            generation: 1,
            encryption_revision: 3,
            mode: Mode::Pq,
            fingerprint: "original-identity".into(),
            draft: Draft {
                uri: "file:///fixture.png".into(),
                name: "fixture.png".into(),
                error: None,
                sticker: false,
                local_sticker: None,
            },
        };
        assert!(work.policy_unchanged(&chat, 3));
        assert!(!work.policy_unchanged(&chat, 4));
        chat.sending_preference = Some(crate::encryption_policy::Preference::Legacy);
        assert!(!work.policy_unchanged(&chat, 3));
        chat.sending_preference = Some(crate::encryption_policy::Preference::Omemo2);
        assert!(work.policy_unchanged(&chat, 3));
    }
    #[test]
    fn pending_uploads_are_durable_but_never_immutable_wire_payloads() {
        let store = crate::storage::Store::open(true).unwrap();
        let owner = "alice@example.org";
        let peer = "bob@example.org";
        store
            .save_account(&Account {
                jid: owner.into(),
                host: String::new(),
                port: 5222,
                direct_tls: false,
            })
            .unwrap();
        let mut state = State::live(owner, vec![]);
        let chat = state.chat_for(peer);
        let id = new_id();
        let mut m = Message {
            id: id.clone(),
            wire_id: id.clone(),
            own: true,
            delivery: "sending".into(),
            kind: "file".into(),
            upload: Some(Draft {
                uri: "file:///fictional/input".into(),
                name: "input.txt".into(),
                error: None,
                sticker: false,
                    local_sticker: None,
            }),
            unsupported_attachment: true,
            ..Default::default()
        };
        chat.messages.push(m.clone());
        store.save(&state).unwrap();
        store.init_actions().unwrap();
        let count = || {
            store
                .db
                .query_row(
                    "SELECT count(*) FROM message_bases WHERE id=?",
                    [&id],
                    |r| r.get::<_, u32>(0),
                )
                .unwrap()
        };
        assert_eq!(count(), 0);
        let loaded = store.load(owner);
        assert_eq!(
            loaded
                .chats
                .iter()
                .find(|c| c.jid == peer)
                .unwrap()
                .messages[0]
                .delivery,
            "failed"
        );
        assert!(
            loaded
                .chats
                .iter()
                .find(|c| c.jid == peer)
                .unwrap()
                .messages[0]
                .upload
                .as_ref()
                .unwrap()
                .error
                .is_some()
        );
        assert_eq!(count(), 0);
        m.upload = None;
        m.attachment =
            Some(crate::file_share::Attachment::bare("https://files.example.org/file").unwrap());
        assert!(store.update_upload(owner, peer, &m).unwrap());
        assert_eq!(count(), 1);
        assert!(
            store
                .message(owner, peer, &id)
                .unwrap()
                .attachment
                .is_some()
        );
        assert!(!store.update_upload(owner, peer, &m).unwrap());
        store.delete(owner, peer, &id).unwrap();
        assert!(!store.update_upload(owner, peer, &m).unwrap());
        assert!(store.message(owner, peer, &id).is_none());
    }
}
