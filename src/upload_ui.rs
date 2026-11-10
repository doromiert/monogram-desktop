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
    collections::VecDeque,
    rc::Rc,
    sync::{Arc, mpsc},
    time::Duration,
};
#[derive(Clone)]
struct Work {
    owner: String,
    peer: String,
    id: String,
    generation: u64,
    encrypted: bool,
    fingerprint: String,
    draft: Draft,
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
                    Ok(rt)=>rt.block_on(async{tokio::select!{biased; _=stopped=>Err("Upload cancelled"), result=uploads::run(&iq,&work.owner,&work.id,&work.draft,work.encrypted,job)=>result}}),
                    Err(_)=>Err("Upload runtime unavailable"),
                };
                let _ = tx.send(result);
            });
        }
        complete
    }
}
impl Ui {
    fn upload_row(&self, owner: &str, peer: &str, message: Message) -> bool {
        match self.store.update_upload(owner, peer, &message) {
            Ok(true) => {
                if self.state.borrow().account == owner {
                    let mut s = self.state.borrow_mut();
                    if let Some(chat) = s.chats.iter_mut().find(|c| c.jid == peer) {
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
    pub fn start_upload(self: &Rc<Self>, peer: &str, id: &str) {
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
        let mode = self
            .state
            .borrow()
            .chats
            .iter()
            .find(|c| c.jid == peer)
            .and_then(|chat| self.sending_mode(chat));
        let Some(mode) = mode else {
            self.notice("Check conversation encryption before uploading");
            return;
        };
        if mode == Mode::Legacy {
            self.notice("Legacy OMEMO file sending is unavailable");
            return;
        }
        let Some(fingerprint) = self.fingerprint_for_send(mode) else {
            self.notice("The selected OMEMO version is not ready");
            return;
        };
        let encrypted = mode.encrypted();
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
                encrypted,
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
            let selected = self
                .state
                .borrow()
                .chats
                .iter()
                .find(|c| c.jid == work.peer)
                .and_then(|chat| self.sending_mode(chat));
            let mode = if work.encrypted {
                Mode::Omemo2
            } else {
                Mode::Plain
            };
            let unchanged = selected == Some(mode)
                && message.mode() == mode
                && message.fingerprint == work.fingerprint
                && (!work.encrypted
                    || (self.crypto.borrow().ready
                        && self.crypto.borrow().fingerprint == work.fingerprint));
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
                    let cache = crate::downloads::account_root(&work.owner)
                        .map(|p| p.join(uploaded.attachment.key()));
                    if let Ok(cache) = cache {
                        let _guard = crate::downloads::CACHE_IO.lock().unwrap();
                        let _ = std::fs::rename(uploaded.snapshot, cache);
                    }
                    if !self.command(Command::Send {
                        peer: work.peer.clone(),
                        quote: message.reply.clone(),
                        message: message.clone(),
                    }) {
                        self.delivery(&work.peer, &message.wire_id, "failed");
                    }
                }
            }
        }
        self.render_messages(false);
        self.refresh_chats(None);
    }
    pub fn send_files(self: &Rc<Self>, caption: &str) -> bool {
        if !self.online.get() {
            self.notice("Connect before uploading. Your draft is kept.");
            return false;
        }
        let Some(mode) = self.sending_mode(self.state.borrow().current()) else {
            return false;
        };
        if mode == Mode::Legacy {
            self.notice("Legacy OMEMO file sending is unavailable");
            return false;
        }
        let encrypted = mode.encrypted();
        if encrypted && (!self.crypto.borrow().ready || self.crypto.borrow().fingerprint.is_empty())
        {
            self.notice("Enable OMEMO 2 and review devices before uploading");
            return false;
        }
        let files = self.attachments.borrow().clone();
        if files.len() + self.uploads.borrow().tasks.len() > 64 {
            self.notice("Send at most 64 queued attachments at a time");
            return false;
        }
        let reply = self
            .replying
            .borrow()
            .as_ref()
            .and_then(|id| self.state.borrow().message(id).cloned());
        let (owner, peer) = {
            let s = self.state.borrow();
            (s.account.clone(), s.current().jid.clone())
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
                text: if index == 0 {
                    caption.trim().into()
                } else {
                    String::new()
                },
                encrypted,
                fingerprint: if encrypted {
                    self.crypto.borrow().fingerprint.clone()
                } else {
                    String::new()
                },
                unsupported_attachment: true,
                delivery: "sending".into(),
                upload: Some(Draft {
                    uri: file.uri().into(),
                    name: filename,
                    error: None,
                }),
                ..Default::default()
            };
            if let Some(r) = &reply {
                message.reply = format!(
                    "{} · {}",
                    r.author,
                    r.preview().chars().take(80).collect::<String>()
                );
                message.reply_id = r.wire_id.clone();
                message.reply_to = if r.own { owner.clone() } else { peer.clone() };
            }
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
            self.start_upload(&peer, &id);
        }
        true
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
