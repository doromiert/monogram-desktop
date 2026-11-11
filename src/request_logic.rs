//! Durable incoming-request operations. Presentation lives in invitation_ui.
use crate::{
    credentials,
    protocol::Command,
    rooms::{Password, invitations::Incoming},
    storage::{ContactRequest, PendingRequests, RoomInvitation},
    ui::Ui,
};
use gtk::glib;
use std::{
    collections::{HashMap, HashSet},
    rc::Rc,
    time::{Duration, Instant},
};

type SecretKey = (String, String);
type RecoveryKey = (String, String, String);
struct SecretWrite {
    id: String,
    password: Password,
}
#[derive(Default)]
pub struct SecretJobs {
    pending: HashMap<SecretKey, SecretWrite>,
    running: HashMap<SecretKey, String>,
    recovering: HashSet<RecoveryKey>,
    recovery_after: HashMap<RecoveryKey, Instant>,
    cleanup_running: HashSet<String>,
    cleanup_after: HashMap<String, Instant>,
    cleanup_failure: HashMap<String, (u8, String)>,
}
impl SecretJobs {
    fn writing(&self, owner: &str, room: &str, id: &str) -> bool {
        let key = (owner.into(), room.into());
        self.running.get(&key).is_some_and(|running| running == id)
            || self
                .pending
                .get(&key)
                .is_some_and(|pending| pending.id == id)
    }
    fn active(&self, owner: &str, room: &str, id: &str) -> bool {
        self.writing(owner, room, id)
            || self
                .recovering
                .contains(&(owner.into(), room.into(), id.into()))
    }
}

const CHANGED: &str = "This request changed. Close it and review the current request.";
const DATABASE: &str = "Pending requests could not be saved. Please try again.";

impl Ui {
    pub fn pending_requests(&self) -> Result<PendingRequests, String> {
        self.pending_requests_before(None)
    }
    pub fn pending_requests_before(&self, before: Option<u64>) -> Result<PendingRequests, String> {
        let state = self.state.borrow();
        if state.account.is_empty() {
            return Ok(PendingRequests {
                contacts: state
                    .requests
                    .iter()
                    .map(|(peer, description)| ContactRequest {
                        owner: String::new(),
                        peer: peer.clone(),
                        description: description.clone(),
                        revision: 0,
                    })
                    .collect(),
                rooms: Vec::new(),
                contact_cursor: None,
                contact_total: state.requests.len(),
            });
        }
        self.store
            .pending_requests_before(&state.account, before)
            .map_err(|_| DATABASE.into())
    }
    pub fn request_count(&self) -> usize {
        let state = self.state.borrow();
        if state.account.is_empty() {
            return state.requests.len();
        }
        self.store
            .pending_request_count(&state.account)
            .unwrap_or(state.requests.len())
    }
    fn refresh_pending_requests(self: &Rc<Self>) {
        let owner = self.state.borrow().account.clone();
        if !owner.is_empty() {
            if let Ok(requests) = self.store.pending_requests(&owner) {
                self.state.borrow_mut().requests = requests
                    .contacts
                    .into_iter()
                    .map(|request| (request.peer, request.description))
                    .collect();
            }
        }
        self.refresh_chats(None);
    }
    pub fn receive_contact_request(self: &Rc<Self>, peer: String) {
        let owner = self.state.borrow().account.clone();
        if owner.is_empty() {
            return;
        }
        match self.store.upsert_contact_request(&owner, &peer) {
            Ok(Some(_)) => self.refresh_pending_requests(),
            Ok(None) => self.notice("Review pending contact requests before receiving more."),
            Err(_) => self.notice(DATABASE),
        }
    }
    pub fn clear_contact_request(self: &Rc<Self>, peer: &str) {
        self.clear_contact_requests(&[peer.to_owned()]);
    }
    pub fn clear_contact_requests(self: &Rc<Self>, peers: &[String]) {
        let owner = self.state.borrow().account.clone();
        if owner.is_empty() || peers.is_empty() {
            return;
        }
        match self.store.remove_contact_requests(&owner, peers) {
            Ok(true) => self.refresh_pending_requests(),
            Err(_) => self.notice(DATABASE),
            _ => {}
        }
    }
    pub fn receive_room_invitation(self: &Rc<Self>, mut incoming: Incoming) {
        let owner = self.state.borrow().account.clone();
        if owner.is_empty() {
            return;
        }
        let update = match self.store.upsert_room_invitation(&owner, &incoming) {
            Ok(Some(update)) => update,
            Ok(None) => {
                self.notice("Review pending group invitations before receiving more.");
                return;
            }
            Err(_) => {
                self.notice(DATABASE);
                return;
            }
        };
        if let Some(password) = incoming
            .password
            .take()
            .filter(|password| !password.0.is_empty())
        {
            if let Some(id) = update.record.secret_id {
                let key = (owner, update.record.room);
                let start = {
                    let mut jobs = self.request_secrets.borrow_mut();
                    // Only the newest queued password is retained for each account/room.
                    if let Some(queued) = jobs
                        .pending
                        .insert(key.clone(), SecretWrite { id, password })
                    {
                        // A coalesced password never reached Secret Service.
                        let _ = self
                            .store
                            .finish_invitation_secret_cleanup(&key.0, &key.1, &queued.id);
                    }
                    if jobs.running.contains_key(&key) {
                        false
                    } else {
                        jobs.running.insert(key.clone(), String::new());
                        true
                    }
                };
                if start {
                    self.start_invitation_writer(key);
                }
            }
        }
        self.refresh_pending_requests();
        self.retry_request_cleanup();
    }
    fn start_invitation_writer(self: &Rc<Self>, key: SecretKey) {
        let weak = Rc::downgrade(self);
        glib::MainContext::default().spawn_local(async move {
            loop {
                let Some(ui) = weak.upgrade() else { return };
                let work = {
                    let mut jobs = ui.request_secrets.borrow_mut();
                    match jobs.pending.remove(&key) {
                        Some(work) => {
                            jobs.running.insert(key.clone(), work.id.clone());
                            Some(work)
                        }
                        None => {
                            jobs.running.remove(&key);
                            None
                        }
                    }
                };
                let Some(work) = work else {
                    ui.retry_request_cleanup();
                    return;
                };
                let obsolete = match ui.store.room_invitation(&key.0, &key.1) {
                    Ok(Some(record)) => record.secret_id.as_deref() != Some(work.id.as_str()),
                    Ok(None) => true,
                    Err(_) => false,
                };
                if obsolete { continue }
                drop(ui);
                let result = credentials::store_invitation(&key.0, &key.1, &work.id, work.password.0).await;
                let Some(ui) = weak.upgrade() else { return };
                let error = result.as_ref().err().map(ToString::to_string);
                match ui.store.finish_invitation_secret(&key.0, &key.1, &work.id, error.as_deref()) {
                    Ok(false) => {
                        if ui.store.queue_invitation_secret_cleanup(&key.0, &key.1, &work.id).is_err() {
                            // Account deletion also purges its invitation keyring scope.
                            drop(ui);
                            let _ = credentials::remove_invitation(&key.0, &key.1, &work.id).await;
                            continue;
                        }
                    }
                    Ok(true) if error.is_some() && ui.state.borrow().account == key.0 => {
                        ui.notice("The invitation was saved, but its password could not be saved in the keyring. You can enter it when joining.");
                    }
                    Err(_) => {
                        if ui.state.borrow().account == key.0 { ui.notice(DATABASE); }
                    }
                    _ => {}
                }
                if ui.state.borrow().account == key.0 { ui.refresh_pending_requests(); }
            }
        });
    }
    /// Called by the ordinary UI timer; I/O is asynchronous and failures back off.
    pub fn retry_request_cleanup(self: &Rc<Self>) {
        let owner = self.state.borrow().account.clone();
        if owner.is_empty() {
            return;
        }
        if let Ok(requests) = self.store.pending_requests(&owner) {
            for record in requests
                .rooms
                .into_iter()
                .filter(|record| !record.password_ready)
            {
                let Some(id) = record.secret_id else {
                    continue;
                };
                let key = (owner.clone(), record.room.clone(), id.clone());
                let start = {
                    let mut jobs = self.request_secrets.borrow_mut();
                    if jobs.active(&owner, &record.room, &id)
                        || jobs
                            .recovery_after
                            .get(&key)
                            .is_some_and(|when| *when > Instant::now())
                    {
                        false
                    } else {
                        jobs.recovering.insert(key.clone());
                        jobs.recovery_after
                            .insert(key.clone(), Instant::now() + Duration::from_secs(30));
                        true
                    }
                };
                if !start {
                    continue;
                }
                let weak = Rc::downgrade(self);
                glib::MainContext::default().spawn_local(async move {
                    let result = credentials::load_invitation(&key.0, &key.1, &key.2).await;
                    let Some(ui) = weak.upgrade() else {
                        return;
                    };
                    ui.request_secrets.borrow_mut().recovering.remove(&key);
                    let error = result.err().map(|error| {
                        format!("The invitation password could not be recovered. {error}")
                    });
                    if ui
                        .store
                        .finish_invitation_secret(&key.0, &key.1, &key.2, error.as_deref())
                        .is_ok()
                        && error.is_none()
                    {
                        ui.request_secrets.borrow_mut().recovery_after.remove(&key);
                    }
                });
            }
        }
        {
            let jobs = self.request_secrets.borrow();
            if jobs.cleanup_running.contains(&owner)
                || jobs
                    .cleanup_after
                    .get(&owner)
                    .is_some_and(|when| *when > Instant::now())
            {
                return;
            }
        }
        let garbage = match self.store.invitation_secret_cleanup(&owner) {
            Ok(garbage) => garbage,
            Err(_) => return,
        };
        if garbage.is_empty() {
            return;
        }
        self.request_secrets
            .borrow_mut()
            .cleanup_running
            .insert(owner.clone());
        let weak = Rc::downgrade(self);
        glib::MainContext::default().spawn_local(async move {
            let mut failure = None;
            for (room, id) in garbage {
                let Some(ui) = weak.upgrade() else { return };
                if ui.request_secrets.borrow().writing(&owner, &room, &id) { continue }
                drop(ui);
                let result = credentials::remove_invitation(&owner, &room, &id).await;
                let Some(ui) = weak.upgrade() else { return };
                if let Err(error) = result {
                    failure = Some(error.to_string());
                    break;
                }
                if ui.store.finish_invitation_secret_cleanup(&owner, &room, &id).is_err() {
                    failure = Some(DATABASE.into());
                    break;
                }
            }
            let Some(ui) = weak.upgrade() else { return };
            let mut jobs = ui.request_secrets.borrow_mut();
            jobs.cleanup_running.remove(&owner);
            let notice = if let Some(error) = failure {
                let previous = jobs.cleanup_failure.get(&owner).cloned();
                let attempt = previous.as_ref().map(|(attempt, _)| attempt.saturating_add(1)).unwrap_or(0).min(6);
                jobs.cleanup_after.insert(owner.clone(), Instant::now() + Duration::from_secs((5u64 << attempt).min(300)));
                let changed = previous.as_ref().is_none_or(|(_, old)| old != &error);
                jobs.cleanup_failure.insert(owner.clone(), (attempt, error));
                changed
            } else {
                jobs.cleanup_failure.remove(&owner);
                jobs.cleanup_after.insert(owner.clone(), Instant::now() + Duration::from_secs(1));
                false
            };
            drop(jobs);
            if notice && ui.state.borrow().account == owner {
                ui.notice("Unlock the keyring to finish removing saved invitation passwords. Cleanup will retry.");
            }
        });
    }
    fn current_invitation(&self, record: &RoomInvitation) -> Result<RoomInvitation, String> {
        if self.state.borrow().account != record.owner {
            return Err(CHANGED.into());
        }
        self.store
            .room_invitation(&record.owner, &record.room)
            .map_err(|_| DATABASE.to_string())?
            .filter(|current| {
                current.revision == record.revision && current.secret_id == record.secret_id
            })
            .ok_or_else(|| CHANGED.into())
    }
    pub fn invitation_password(
        self: &Rc<Self>,
        record: &RoomInvitation,
    ) -> impl std::future::Future<Output = Result<Option<Password>, String>> + 'static + use<> {
        let weak = Rc::downgrade(self);
        let record = record.clone();
        async move {
            {
                let ui = weak.upgrade().ok_or_else(|| CHANGED.to_string())?;
                if let Some(id) = record.secret_id.as_ref() {
                    ui.request_secrets.borrow_mut().recovery_after.remove(&(
                        record.owner.clone(),
                        record.room.clone(),
                        id.clone(),
                    ));
                }
                ui.retry_request_cleanup();
            }
            for _ in 0..=70 {
                let ui = weak.upgrade().ok_or_else(|| CHANGED.to_string())?;
                let current = ui.current_invitation(&record)?;
                let Some(secret) = current.secret_id else {
                    return Ok(None);
                };
                if let Some(error) = current.password_error.filter(|_| {
                    !ui.request_secrets
                        .borrow()
                        .active(&current.owner, &current.room, &secret)
                }) {
                    return Err(format!(
                        "The saved invitation password is unavailable. {error} Enter it manually or retry."
                    ));
                }
                if current.password_ready {
                    drop(ui);
                    let password =
                        credentials::load_invitation(&current.owner, &current.room, &secret)
                            .await
                            .map_err(|error| {
                                format!("The invitation password could not be read. {error}")
                            })?;
                    let ui = weak.upgrade().ok_or_else(|| CHANGED.to_string())?;
                    ui.current_invitation(&record)?;
                    return Ok(Some(Password(password)));
                }
                drop(ui);
                glib::timeout_future(Duration::from_millis(150)).await;
            }
            Err("The invitation password is still being saved. Enter it manually or try again shortly.".into())
        }
    }
    pub fn accept_room_invitation(
        self: &Rc<Self>,
        record: &RoomInvitation,
        nick: String,
        password: Option<Password>,
    ) -> impl std::future::Future<Output = Result<(), String>> + 'static + use<> {
        let weak = Rc::downgrade(self);
        let record = record.clone();
        let generation = self.generation.get();
        async move {
            let password_load = {
                let ui = weak.upgrade().ok_or_else(|| CHANGED.to_string())?;
                if ui.generation.get() != generation {
                    return Err(CHANGED.into());
                }
                ui.current_invitation(&record)?;
                if !ui.online.get() {
                    return Err("Reconnect before joining this group.".into());
                }
                if nick.trim().is_empty() {
                    return Err("Enter your nickname.".into());
                }
                ui.invitation_password(&record)
            };
            let password = match password {
                Some(password) => Some(password),
                None => password_load.await?,
            };
            let receive = {
                let ui = weak.upgrade().ok_or_else(|| CHANGED.to_string())?;
                if ui.generation.get() != generation {
                    return Err(CHANGED.into());
                }
                ui.current_invitation(&record)?;
                let (reply, receive) = tokio::sync::oneshot::channel();
                if !ui.command(Command::JoinExistingRoom {
                    peer: record.room.clone(),
                    nick: nick.trim().into(),
                    password,
                    reply,
                }) {
                    return Err("Reconnect before joining this group.".into());
                }
                receive
            };
            let result = receive
                .await
                .map_err(|_| "Joining this group was interrupted. Please try again.".to_string())?;
            let ui = weak.upgrade().ok_or_else(|| CHANGED.to_string())?;
            if ui.generation.get() != generation {
                return Err(CHANGED.into());
            }
            ui.current_invitation(&record)?;
            let room = result.map_err(|error| error.to_string())?;
            if !ui.remember_room(&room) {
                return Err("The group is joined, but its local settings could not be saved. The invitation has been kept.".into());
            }
            let removed = ui.dismiss_room_invitation(&record)?;
            ui.open_room(&room);
            if !removed {
                return Err(CHANGED.into());
            }
            Ok(())
        }
    }
    pub fn dismiss_room_invitation(
        self: &Rc<Self>,
        record: &RoomInvitation,
    ) -> Result<bool, String> {
        let current = self.current_invitation(record)?;
        let removed = self
            .store
            .remove_room_invitation(&current)
            .map_err(|_| DATABASE.to_string())?;
        if removed {
            self.refresh_pending_requests();
            self.retry_request_cleanup();
        }
        Ok(removed)
    }
    pub fn respond_contact_request(
        self: &Rc<Self>,
        record: &ContactRequest,
        allow: bool,
    ) -> impl std::future::Future<Output = Result<(), String>> + 'static + use<> {
        let weak = Rc::downgrade(self);
        let record = record.clone();
        let generation = self.generation.get();
        async move {
            let (current, receive) = {
                let ui = weak.upgrade().ok_or_else(|| CHANGED.to_string())?;
                if ui.generation.get() != generation || ui.state.borrow().account != record.owner {
                    return Err(CHANGED.into());
                }
                if record.owner.is_empty() {
                    ui.state
                        .borrow_mut()
                        .requests
                        .retain(|(peer, description)| {
                            peer != &record.peer || description != &record.description
                        });
                    ui.refresh_pending_requests();
                    return Ok(());
                }
                let current = ui
                    .store
                    .contact_request(&record.owner, &record.peer)
                    .map_err(|_| DATABASE.to_string())?
                    .filter(|current| current.revision == record.revision)
                    .ok_or_else(|| CHANGED.to_string())?;
                if !ui.online.get() {
                    return Err("Reconnect to respond to this request.".into());
                }
                let (reply, receive) = tokio::sync::oneshot::channel();
                if !ui.command(Command::Authorize {
                    peer: record.peer.clone(),
                    allow,
                    reply,
                }) {
                    return Err("Reconnect to respond to this request.".into());
                }
                (current, receive)
            };
            receive
                .await
                .map_err(|_| "The response was interrupted. Please try again.".to_string())??;
            let ui = weak.upgrade().ok_or_else(|| CHANGED.to_string())?;
            if ui.generation.get() != generation || ui.state.borrow().account != record.owner {
                return Err(CHANGED.into());
            }
            if !ui
                .store
                .remove_contact_request(&current)
                .map_err(|_| DATABASE.to_string())?
                && ui
                    .store
                    .contact_request(&current.owner, &current.peer)
                    .map_err(|_| DATABASE.to_string())?
                    .is_some()
            {
                return Err(CHANGED.into());
            }
            ui.refresh_pending_requests();
            Ok(())
        }
    }
}
