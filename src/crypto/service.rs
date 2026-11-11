//! Crypto has its own cancellable worker so IQ waits never block the transport or GTK.
use super::{
    bundle::{self, Bundle, NS},
    device::{self, Trust},
    journal::Token,
    lifecycle::{self, Local},
    pep::{self, Pep},
    *,
};
use crate::{
    model::Message,
    protocol::{self, BackendEvent, Command, Envelope},
};
use std::{collections::BTreeMap, path::PathBuf, sync::mpsc, thread};
use tokio::sync::{mpsc as channel, oneshot};
use tokio_xmpp::minidom::Element;
#[derive(Clone, Debug)]
pub struct Status {
    pub enabled: bool,
    pub ready: bool,
    pub device: u32,
    pub fingerprint: String,
    pub text: String,
}
impl Default for Status {
    fn default() -> Self {
        Self {
            enabled: false,
            ready: false,
            device: 0,
            fingerprint: String::new(),
            text: "OMEMO 2 is not enabled for this account".into(),
        }
    }
}
#[derive(Clone, Debug)]
pub struct Review {
    pub peer: String,
    pub device: u32,
    pub key: [u8; 32],
    pub trust: String,
}
#[derive(Clone, Debug)]
pub struct Frame {
    pub xml: Element,
    pub archive: Option<(String, String)>,
    pub historical: bool,
    pub receipt_to: Option<String>,
}
#[derive(Debug)]
pub enum Request {
    Online(bool),
    Enable,
    Probe {
        peer: String,
        reply: oneshot::Sender<Result<bool>>,
    },
    Review(String),
    RefreshTrust(String),
    Decide {
        keys: BTreeMap<String, BTreeMap<u32, [u8; 32]>>,
        approve: bool,
    },
    Send {
        peer: String,
        message: Message,
        quote: String,
    },
    Incoming(Frame),
    Accepted(Token),
}
pub struct Handle {
    tx: channel::UnboundedSender<Request>,
    frames: channel::Sender<Frame>,
    owner: String,
    generation: u64,
    events: mpsc::Sender<Envelope>,
    cancel: Option<oneshot::Sender<()>>,
}
impl Handle {
    pub fn start(
        owner: String,
        generation: u64,
        network: channel::UnboundedSender<Command>,
        events: mpsc::Sender<Envelope>,
    ) -> Self {
        let (tx, mut rx) = channel::unbounded_channel();
        let (frames, mut incoming) = channel::channel(32);
        let (cancel, stopped) = oneshot::channel();
        let origin = owner.clone();
        let notifications = events.clone();
        thread::Builder::new().name("monogram-crypto".into()).spawn(move||{
            let runtime=tokio::runtime::Builder::new_current_thread().enable_all().build().expect("crypto runtime");
            runtime.block_on(async move{tokio::select!{_=stopped=>{},_=async move {
                let mut worker=Worker{root:lifecycle::root(),owner,generation,network,events,local:None,online:false,ready:false,reviews:BTreeMap::new()};
                loop {tokio::select! {biased;request=rx.recv()=>{let Some(request)=request else{break};worker.process(request).await;},frame=incoming.recv()=>{let Some(frame)=frame else{break};worker.process(Request::Incoming(frame)).await;}}}
            }=>{}}});
        }).expect("crypto thread");
        Self {
            tx,
            frames,
            owner: origin,
            generation,
            events: notifications,
            cancel: Some(cancel),
        }
    }
    pub fn send(&self, request: Request) {
        if let Request::Incoming(frame) = request {
            if bundle::xml(&frame.xml, 1024 * 1024).is_err() {
                return;
            }
            if let Err(error) = self.frames.try_send(frame) {
                if let Some(event) = unavailable(
                    &self.owner,
                    &error.into_inner(),
                    "Encryption queue is busy. The ciphertext is saved; review devices or reconnect to retry.",
                ) {
                    let _ = self
                        .events
                        .send((self.generation, self.owner.clone(), event));
                }
            }
        } else {
            let _ = self.tx.send(request);
        }
    }
}
fn unavailable(owner: &str, frame: &Frame, text: &str) -> Option<BackendEvent> {
    if frame
        .xml
        .get_child("encrypted", NS)
        .is_none_or(|e| e.get_child("payload", NS).is_none())
    {
        return None;
    }
    let (peer, mut message, historical) = protocol::normalize(
        &frame.xml,
        owner,
        frame
            .archive
            .as_ref()
            .map(|(id, stamp)| (id.as_str(), stamp.as_str())),
    )?;
    message.text = text.into();
    if let Some(header) = frame
        .xml
        .get_child("encrypted", NS)
        .and_then(|e| e.get_child("header", NS))
    {
        if let Some(id) = header
            .children()
            .filter(|g| g.is("keys", NS) && g.attr("jid") == Some(owner))
            .flat_map(|g| g.children())
            .find_map(|key| bundle::positive(key.attr("rid")).ok())
        {
            if let Ok(digest) = super::engine::Engine::digest(&frame.xml, owner, id) {
                message
                    .aliases
                    .push(format!("omemo2:{}", lifecycle::hex(&digest)));
            }
        }
    }
    message.ciphertext = String::from_utf8(bundle::xml(&frame.xml, 1024 * 1024).ok()?).ok()?;
    Some(BackendEvent::Message {
        peer,
        message,
        historical: frame.historical || historical,
        receipt_to: None,
    })
}
impl Drop for Handle {
    fn drop(&mut self) {
        if let Some(cancel) = self.cancel.take() {
            let _ = cancel.send(());
        }
    }
}
struct Worker {
    root: PathBuf,
    owner: String,
    generation: u64,
    network: channel::UnboundedSender<Command>,
    events: mpsc::Sender<Envelope>,
    local: Option<Local>,
    online: bool,
    ready: bool,
    reviews: BTreeMap<(String, u32), Review>,
}
enum PreparationError {
    Crypto(Error),
    Pep(pep::Error),
    NoDevices,
}
impl From<Error> for PreparationError {
    fn from(error: Error) -> Self {
        Self::Crypto(error)
    }
}
impl From<pep::Error> for PreparationError {
    fn from(error: pep::Error) -> Self {
        Self::Pep(error)
    }
}
impl PreparationError {
    fn fallback(&self) -> bool {
        match self {
            Self::NoDevices | Self::Crypto(Error::TrustRequired | Error::InvalidBundle) => true,
            Self::Pep(error) => error.access_denied() || error.unsupported(),
            Self::Crypto(_) => false,
        }
    }
    fn into_error(self) -> Error {
        match self {
            Self::Crypto(error) | Self::Pep(pep::Error::Crypto(error)) => error,
            Self::NoDevices => Error::Unavailable(
                "No eligible OMEMO 2 devices. Review fingerprints and contact authorization.",
            ),
            Self::Pep(error) if error.access_denied() => {
                Error::Unavailable("Access to OMEMO 2 keys was denied")
            }
            Self::Pep(error) if error.unsupported() => {
                Error::Unavailable("The server does not support OMEMO 2 public keys")
            }
            Self::Pep(pep::Error::PublicAccess) => {
                Error::Unavailable("Could not verify public access to your OMEMO 2 keys")
            }
            Self::Pep(_) => Error::Unavailable(
                "Could not look up or publish OMEMO 2 keys. Check the connection.",
            ),
        }
    }
}
type Prepared<T> = std::result::Result<T, PreparationError>;
impl Worker {
    fn emit(&self, event: BackendEvent) {
        let _ = self
            .events
            .send((self.generation, self.owner.clone(), event));
    }
    fn status(&mut self, text: Option<String>) {
        let mut status = Status {
            enabled: self.local.is_some() || lifecycle::exists(&self.root, &self.owner),
            ready: self.ready,
            ..Default::default()
        };
        if let Some(local) = self.local.as_mut() {
            status.device = local.engine.store.device;
            status.fingerprint = local
                .fingerprint()
                .map(|key| lifecycle::hex(&key))
                .unwrap_or_default();
        }
        status.text = text.unwrap_or_else(|| {
            if self.ready {
                "OMEMO 2 ready"
            } else if status.enabled {
                "OMEMO 2 unavailable while disconnected"
            } else {
                "Enable OMEMO 2 to create this device's identity"
            }
            .into()
        });
        let _ = self.network.send(Command::CryptoReady(status.ready));
        self.emit(BackendEvent::CryptoStatus(status));
    }
    fn pep(&self) -> Result<Pep> {
        let local = self.local.as_ref().ok_or(Error::Incomplete)?;
        Pep::new(
            crate::iq::Client::new(self.network.clone()),
            &self.owner,
            local.engine.store.device,
        )
    }
    async fn initialize(&mut self) -> Prepared<()> {
        if !self.online {
            return Err(Error::Unavailable("Connect before enabling OMEMO 2").into());
        }
        if self.ready {
            self.status(None);
            return Ok(());
        }
        self.ready = false;
        self.status(Some("Preparing OMEMO 2…".into()));
        if self.local.is_none() {
            self.local = Some(Local::open(&self.root, &self.owner, true).await?);
        }
        let local = self.local.as_mut().unwrap();
        let bundle = device::prepare(
            &mut local.engine.store,
            chrono::Utc::now().timestamp_millis() as u64,
        )?;
        self.pep()?.initialize(&bundle).await?;
        self.recover().await?;
        self.ready = true;
        self.status(None);
        self.retry().await;
        Ok(())
    }
    async fn wire(&self, xml: Element) -> bool {
        let (reply, received) = oneshot::channel();
        if self.network.send(Command::Wire { xml, reply }).is_err() {
            return false;
        }
        tokio::time::timeout(std::time::Duration::from_secs(20), received)
            .await
            .is_ok_and(|v| v.unwrap_or(false))
    }
    async fn observe(&mut self, peer: &str, id: u32) -> Prepared<Bundle> {
        let bundle = self
            .pep()?
            .bundle(peer, id)
            .await
            .map_err(|error| match error {
                pep::Error::Crypto(Error::Invalid(_) | Error::Crypto(_)) => {
                    PreparationError::Crypto(Error::InvalidBundle)
                }
                _ => error.into(),
            })?;
        let blind_trust = if let Some(history) = self.history()? {
            history
                .blind_trust(&self.owner, peer)
                .map_err(Error::from)?
        } else {
            false
        };
        device::observe(
            &mut self.local.as_mut().ok_or(Error::Incomplete)?.engine.store,
            peer,
            id,
            &bundle,
            blind_trust,
        )?;
        Ok(bundle)
    }
    async fn targets(&mut self, peer: &str, optional: bool) -> Prepared<BTreeMap<u32, Bundle>> {
        let root = self.pep()?.devices(peer).await?;
        let ids = super::pep::ids(root.as_ref())?;
        let local = self
            .local
            .as_ref()
            .ok_or(Error::Incomplete)?
            .engine
            .store
            .device;
        let mut targets = BTreeMap::new();
        for id in ids {
            if peer == self.owner && id == local {
                continue;
            }
            match self.observe(peer, id).await {
                Ok(bundle) => {
                    let store = &mut self.local.as_mut().unwrap().engine.store;
                    match store.transaction(|tx| device::trust(tx, peer, id, &bundle.identity))? {
                        Trust::Trusted => {
                            targets.insert(id, bundle);
                        }
                        Trust::Rejected => {}
                        Trust::Undecided => {
                            return Err(if peer == self.owner {
                                Error::OwnDeviceTrustRequired
                            } else {
                                Error::TrustRequired
                            }
                            .into());
                        }
                    }
                }
                Err(PreparationError::Crypto(Error::InvalidBundle))
                    if optional && peer == self.owner => {}
                Err(error) => return Err(error),
            }
        }
        Ok(targets)
    }
    async fn review(&mut self, peer: &str) {
        let mut errors = vec![];
        let mut peers = vec![peer.to_string()];
        if peer != self.owner {
            peers.push(self.owner.clone());
        }
        for peer in peers {
            let root = match self.pep() {
                Ok(pep) => pep.devices(&peer).await,
                Err(error) => {
                    errors.push(error.to_string());
                    continue;
                }
            };
            let ids = match root.and_then(|r| super::pep::ids(r.as_ref()).map_err(Into::into)) {
                Ok(ids) => ids,
                Err(_) => {
                    errors.push(format!(
                        "Could not refresh {peer}; previous fingerprints retained"
                    ));
                    continue;
                }
            };
            self.reviews
                .retain(|(owner, id), _| owner != &peer || ids.contains(id));
            for id in ids {
                if peer == self.owner
                    && self
                        .local
                        .as_ref()
                        .is_some_and(|l| l.engine.store.device == id)
                {
                    continue;
                }
                match self.observe(&peer, id).await {
                    Ok(bundle) => {
                        let trust = self
                            .local
                            .as_mut()
                            .unwrap()
                            .engine
                            .store
                            .transaction(|tx| device::trust(tx, &peer, id, &bundle.identity));
                        match trust {
                            Ok(trust) => {
                                self.reviews.insert(
                                    (peer.clone(), id),
                                    Review {
                                        peer: peer.clone(),
                                        device: id,
                                        key: bundle.identity,
                                        trust: format!("{trust:?}"),
                                    },
                                );
                            }
                            Err(error) => errors.push(error.to_string()),
                        }
                    }
                    Err(_) => errors.push(format!("Device {id} on {peer} could not be refreshed")),
                }
            }
        }
        let rows = self
            .reviews
            .values()
            .filter(|row| row.peer == peer || row.peer == self.owner)
            .cloned()
            .collect();
        self.emit(BackendEvent::CryptoReview {
            peer: peer.into(),
            rows,
            errors,
        });
    }
    async fn prepare_message(
        &mut self,
        peer: &str,
        message: &Message,
        quote: &str,
    ) -> Prepared<Element> {
        if message.upload.is_some()
            || message.attachment.as_ref().is_some_and(|file| {
                !file.valid_outgoing(true) || message.poll.is_some() || message.action.is_some()
            })
        {
            return Err(Error::Invalid("attachment encryption").into());
        }
        if !self.ready || !self.online {
            return Err(Error::Unavailable(
                "OMEMO 2 is not ready; reconnect or retry setup in Conversation Security",
            )
            .into());
        }
        // UI state can lag an identity replacement queued ahead of this send.
        if !message.fingerprint.is_empty()
            && lifecycle::hex(
                &self
                    .local
                    .as_mut()
                    .ok_or(Error::Incomplete)?
                    .fingerprint()?,
            ) != message.fingerprint
        {
            return Err(Error::Unavailable(
                "Encryption identity changed. Select the file again or retry the message.",
            )
            .into());
        }
        address(peer, 1)?;
        let targets = self.targets(peer, false).await?;
        if targets.is_empty() {
            return Err(PreparationError::NoDevices);
        }
        let mut recipients = BTreeMap::from([(peer.to_string(), targets)]);
        if peer != self.owner {
            let owner = self.owner.clone();
            recipients.insert(owner.clone(), self.targets(&owner, true).await?);
        }
        let root = protocol::outgoing(peer, message, quote);
        let mut plain = Element::builder("message", payload::CLIENT)
            .attr("from".try_into().unwrap(), &self.owner)
            .attr("to".try_into().unwrap(), peer)
            .attr("type".try_into().unwrap(), "chat")
            .attr("id".try_into().unwrap(), &message.wire_id)
            .build();
        for child in root.children().filter(|c| payload::allowed(c)) {
            plain.append_child(child.clone());
        }
        let mut encrypted = self
            .local
            .as_mut()
            .unwrap()
            .engine
            .encrypt(&plain, &recipients)?;
        for child in root.children().filter(|c| !payload::allowed(c)) {
            if !child.is("store", "urn:xmpp:hints") {
                encrypted.append_child(child.clone());
            }
        }
        Ok(encrypted)
    }
    async fn send_message(&mut self, peer: &str, message: &Message, quote: &str) -> Result<()> {
        let encrypted = self
            .prepare_message(peer, message, quote)
            .await
            .map_err(PreparationError::into_error)?;
        if !self.wire(encrypted).await {
            return Err(Error::Uncertain);
        }
        Ok(())
    }
    async fn probe(&mut self, peer: &str) -> Result<bool> {
        async fn prepare(worker: &mut Worker, peer: &str) -> Prepared<()> {
            if !worker.online {
                return Err(Error::Unavailable("Connect before checking encryption").into());
            }
            if !worker.ready {
                let requests = crate::iq::Client::new(worker.network.clone());
                let pep = Pep::new(requests, &worker.owner, 1)?;
                if pep::ids(pep.devices(peer).await?.as_ref())?.is_empty() {
                    return Err(PreparationError::NoDevices);
                }
                worker.initialize().await?;
            }
            worker
                .prepare_message(
                    peer,
                    &Message {
                        text: "Monogram encryption check".into(),
                        wire_id: crate::model::new_id(),
                        encrypted: true,
                        ..Default::default()
                    },
                    "",
                )
                .await?;
            Ok(())
        }
        match prepare(self, peer).await {
            Ok(()) => Ok(true),
            Err(error) if error.fallback() => Ok(false),
            Err(error) => Err(error.into_error()),
        }
    }
    fn history(&self) -> Result<Option<crate::storage::Store>> {
        let path = self.root.join("history.sqlite3");
        if !path.exists() {
            return Ok(None);
        }
        Ok(Some(crate::storage::Store {
            db: rusqlite::Connection::open_with_flags(
                path,
                rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
            )?,
        }))
    }
    fn placeholder(&self, frame: &Frame, text: &str) {
        if let Some(event) = unavailable(&self.owner, frame, text) {
            self.emit(event);
        }
    }
    async fn incoming(&mut self, frame: Frame) -> Result<()> {
        if !self.ready {
            self.placeholder(
                &frame,
                "Encrypted message — enable OMEMO 2 and review the sender's devices",
            );
            return Ok(());
        }
        let local = self.local.as_ref().ok_or(Error::Incomplete)?;
        let id = local.engine.store.device;
        let digest =
            super::engine::Engine::digest(&frame.xml, &self.owner, id).map_err(|error| {
                if matches!(error, Error::Invalid("not addressed to this device")) {
                    Error::Unavailable("This OMEMO 2 message has no key for this device")
                } else {
                    error
                }
            })?;
        let alias = format!("omemo2:{}", lifecycle::hex(&digest));
        let outer = protocol::normalize(
            &frame.xml,
            &self.owner,
            frame
                .archive
                .as_ref()
                .map(|(id, stamp)| (id.as_str(), stamp.as_str())),
        );
        if let Some((peer, message, _)) = &outer {
            if let Some(history) = self.history()? {
                if let Some(id) = history.known(&self.owner, peer, message.own, &[alias.clone()]) {
                    if history
                        .message(&self.owner, peer, &id)
                        .is_none_or(|m| !m.unavailable)
                    {
                        self.emit(BackendEvent::CryptoAliases {
                            legacy_sticker: false,
                            legacy_reply: None,
                            peer: peer.clone(),
                            own: message.own,
                            id,
                            aliases: message.aliases.clone(),
                        });
                        let local = self.local.as_mut().unwrap();
                        if let Some(pending) = local
                            .engine
                            .store
                            .transaction(|tx| local.engine.journal.find(tx, &digest))?
                        {
                            if let Some(token) = pending.token {
                                self.accept(token).await?;
                            }
                        }
                        return Ok(());
                    }
                }
            }
        }
        let header = bundle::single(bundle::single(&frame.xml, "encrypted", NS)?, "header", NS)?;
        let sender = bundle::positive(header.attr("sid"))?;
        let peer = payload::routing(&frame.xml)?.0;
        let prekey = header
            .children()
            .filter(|g| g.attr("jid") == Some(&self.owner))
            .flat_map(|g| g.children())
            .any(|k| {
                k.attr("rid").and_then(|v| v.parse::<u32>().ok()) == Some(id)
                    && matches!(k.attr("kex"), Some("true" | "1"))
            });
        if prekey {
            let bundle = self
                .observe(&peer, sender)
                .await
                .map_err(PreparationError::into_error)?;
            if self
                .local
                .as_mut()
                .unwrap()
                .engine
                .store
                .transaction(|tx| device::trust(tx, &peer, sender, &bundle.identity))?
                != Trust::Trusted
            {
                return Err(Error::TrustRequired);
            }
        }
        let stamp = outer
            .as_ref()
            .and_then(|(_, m, _)| chrono::DateTime::parse_from_rfc3339(&m.timestamp).ok())
            .map(|t| t.timestamp_millis())
            .unwrap_or_else(|| chrono::Utc::now().timestamp_millis());
        let received = self.local.as_mut().unwrap().engine.receive_with_context(
            &frame.xml,
            stamp,
            Some(super::journal::Context {
                room: None,
                archive: frame.archive.clone(),
                historical: frame.historical,
                receipt_to: frame.receipt_to.clone(),
            }),
        )?;
        self.deliver(received, Some(frame), alias).await
    }
    async fn deliver(
        &mut self,
        received: super::journal::Received,
        frame: Option<Frame>,
        alias: String,
    ) -> Result<()> {
        let frame = frame.or_else(|| {
            received.outer.as_ref().map(|xml| Frame {
                xml: xml.clone(),
                archive: received.context.as_ref().and_then(|v| v.archive.clone()),
                historical: received.context.as_ref().is_none_or(|v| v.historical),
                receipt_to: received.context.as_ref().and_then(|v| v.receipt_to.clone()),
            })
        });
        if let Some(content) = received.content {
            let outer = frame
                .as_ref()
                .map(|f| &f.xml)
                .or(received.outer.as_ref())
                .ok_or(Error::Incomplete)?;
            let archive = frame
                .as_ref()
                .and_then(|f| f.archive.as_ref())
                .map(|(id, stamp)| (id.as_str(), stamp.as_str()));
            let (peer, mut metadata, historical) = protocol::normalize(outer, &self.owner, archive)
                .ok_or(Error::Invalid("encrypted metadata"))?;
            let (_, mut message, _) = protocol::normalize_protected(&content, &self.owner, None)
                .ok_or(Error::Invalid("unsupported encrypted message"))?;
            message.encrypted = true;
            message.unavailable = false;
            message.fingerprint = lifecycle::hex(&received.fingerprint);
            message.aliases.append(&mut metadata.aliases);
            message.aliases.push(alias);
            let stamp = chrono::DateTime::from_timestamp_millis(received.sent_time)
                .ok_or(Error::Incomplete)?;
            message.timestamp = stamp.to_rfc3339_opts(chrono::SecondsFormat::Micros, false);
            message.time = stamp
                .with_timezone(&chrono::Local)
                .format("%H:%M")
                .to_string();
            self.emit(BackendEvent::EncryptedMessage {
                peer,
                message,
                historical: frame.as_ref().is_none_or(|f| f.historical) || historical,
                receipt_to: frame.and_then(|f| f.receipt_to),
                token: received.token.ok_or(Error::Incomplete)?,
            });
        } else if let Some(token) = received.token {
            self.accept(token).await?;
        }
        Ok(())
    }
    async fn accept(&mut self, token: Token) -> Result<()> {
        let local = self.local.as_mut().ok_or(Error::Incomplete)?;
        let pending = local
            .engine
            .journal
            .pending(&mut local.engine.store)?
            .into_iter()
            .find(|r| r.token.as_ref() == Some(&token));
        if !local
            .engine
            .journal
            .accept(&mut local.engine.store, &token)?
        {
            return Ok(());
        }
        if let Some(received) = pending {
            if received.replenish {
                let bundle = device::prepare(
                    &mut local.engine.store,
                    chrono::Utc::now().timestamp_millis() as u64,
                )?;
                self.pep()?.replenish(&bundle).await.map_err(|_| {
                    Error::Unavailable("Could not refresh public prekeys; retry OMEMO 2 setup")
                })?;
            }
            if let Some(ack) = received.acknowledgement {
                if self.wire(ack).await {
                    let local = self.local.as_mut().unwrap();
                    local
                        .engine
                        .journal
                        .acknowledge(&mut local.engine.store, &token)?;
                }
            }
        }
        Ok(())
    }
    async fn recover(&mut self) -> Result<()> {
        let local = self.local.as_mut().unwrap();
        let pending = local.engine.journal.pending(&mut local.engine.store)?;
        for received in pending {
            let alias = received
                .outer
                .as_ref()
                .and_then(|outer| {
                    super::engine::Engine::digest(
                        outer,
                        &self.owner,
                        self.local.as_ref().unwrap().engine.store.device,
                    )
                    .ok()
                })
                .map(|v| format!("omemo2:{}", lifecycle::hex(&v)))
                .unwrap_or_default();
            self.deliver(received, None, alias).await?;
        }
        Ok(())
    }
    async fn retry(&mut self) {
        let rows = (|| -> Result<Vec<Message>> {
            let Some(history) = self.history()? else {
                return Ok(vec![]);
            };
            let mut query=history.db.prepare("SELECT body FROM messages WHERE account=? AND json_extract(body,'$.unavailable')=1 AND length(json_extract(body,'$.ciphertext'))>0 LIMIT 128")?;
            let rows = query
                .query_map([&self.owner], |r| r.get::<_, String>(0))?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            Ok(rows
                .into_iter()
                .filter_map(|b| serde_json::from_str(&b).ok())
                .collect())
        })();
        if let Ok(rows) = rows {
            for message in rows {
                if let Ok(mut xml) = bundle::document(&message.ciphertext, 1024 * 1024) {
                    if xml.get_child("delay", "urn:xmpp:delay").is_none() {
                        xml.append_child(
                            Element::builder("delay", "urn:xmpp:delay")
                                .attr("stamp".try_into().unwrap(), &message.timestamp)
                                .build(),
                        );
                    }
                    let frame = Frame {
                        xml,
                        archive: None,
                        historical: true,
                        receipt_to: None,
                    };
                    if let Err(error) = Box::pin(self.incoming(frame.clone())).await {
                        self.placeholder(&frame, &error.to_string());
                    }
                }
            }
        }
    }
    async fn process(&mut self, request: Request) {
        match request {
            Request::Online(online) => {
                self.online = online;
                if !online {
                    self.ready = false;
                    self.status(None)
                } else if let Err(error) = self.initialize().await {
                    self.status(Some(error.into_error().to_string()));
                }
            }
            Request::Enable => {
                if let Err(error) = self.initialize().await {
                    self.status(Some(error.into_error().to_string()));
                }
            }
            Request::Probe { peer, reply } => {
                if !reply.is_closed() {
                    let _ = reply.send(self.probe(&peer).await);
                }
            }
            Request::Review(peer) => self.review(&peer).await,
            Request::RefreshTrust(peer) => {
                if self.ready {
                    self.review(&peer).await;
                    self.retry().await;
                }
            }
            Request::Decide { keys, approve } => {
                let result = self
                    .local
                    .as_mut()
                    .ok_or(Error::Incomplete)
                    .and_then(|l| device::decide(&mut l.engine.store, &keys, approve));
                if let Err(error) = result {
                    self.status(Some(error.to_string()));
                } else {
                    for peer in keys.keys() {
                        self.review(peer).await;
                    }
                    self.retry().await;
                }
            }
            Request::Send {
                peer,
                message,
                quote,
            } => {
                let id = message.wire_id.clone();
                match self.send_message(&peer, &message, &quote).await {
                    Ok(()) => self.emit(BackendEvent::Sent { peer, id }),
                    Err(error) => {
                        self.emit(if matches!(error, Error::Uncertain) {
                            BackendEvent::Uncertain {
                                peer: peer.clone(),
                                id,
                            }
                        } else {
                            BackendEvent::Failed {
                                peer: peer.clone(),
                                id,
                            }
                        });
                        self.emit(BackendEvent::CryptoError {
                            peer,
                            text: error.to_string(),
                        });
                    }
                }
            }
            Request::Incoming(frame) => {
                if let Err(error) = self.incoming(frame.clone()).await {
                    self.placeholder(&frame, &error.to_string());
                }
            }
            Request::Accepted(token) => {
                if let Err(error) = self.accept(token).await {
                    self.ready = false;
                    self.status(Some(error.to_string()));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn probing_cannot_downgrade_after_an_ambiguous_iq_failure() {
        use crate::iq;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let rejected = |condition: &str| iq::Error::Rejected {
                condition: condition.into(),
                pubsub: None,
            };
            for (failure, may_fallback) in [
                (rejected("item-not-found"), true),
                (rejected("feature-not-implemented"), true),
                (rejected("forbidden"), true),
                (rejected("service-unavailable"), false),
                (iq::Error::Timeout, false),
                (iq::Error::Closed, false),
            ] {
                let (network, mut requests) = channel::unbounded_channel();
                let (events, _) = mpsc::channel();
                let mut worker = Worker {
                    root: PathBuf::new(),
                    owner: "a@example.invalid".into(),
                    generation: 1,
                    network,
                    events,
                    local: None,
                    online: true,
                    ready: false,
                    reviews: BTreeMap::new(),
                };
                let answer = async {
                    let Some(Command::Iq {
                        kind: iq::Kind::Get,
                        reply,
                        ..
                    }) = requests.recv().await
                    else {
                        panic!("The probe should only request peer key metadata");
                    };
                    reply.send(Err(failure)).unwrap();
                };
                let (result, ()) = tokio::join!(worker.probe("b@example.invalid"), answer);
                if may_fallback {
                    assert!(!result.unwrap());
                } else {
                    assert!(result.is_err());
                }
                assert!(worker.local.is_none());
                assert!(requests.try_recv().is_err());
            }
        });
    }
    #[test]
    fn idless_ciphertext_retains_an_alias_before_device_approval() {
        let (mut a, mut b) = (
            records::Store::open(None, "a@example.invalid", 101, MasterKey::new([1; 32])).unwrap(),
            records::Store::open(None, "b@example.invalid", 202, MasterKey::new([2; 32])).unwrap(),
        );
        device::prepare(&mut a, 1).unwrap();
        let bundle = device::prepare(&mut b, 1).unwrap();
        device::observe(&mut a, "b@example.invalid", 202, &bundle, false).unwrap();
        device::decide(
            &mut a,
            &BTreeMap::from([(
                "b@example.invalid".into(),
                BTreeMap::from([(202, bundle.identity)]),
            )]),
            true,
        )
        .unwrap();
        let mut engine = super::super::engine::Engine::new(a, 1).unwrap();
        let plain:Element="<message xmlns='jabber:client' from='a@example.invalid' to='b@example.invalid' type='chat'><body>Private content without an ID</body></message>".parse().unwrap();
        let xml = engine
            .encrypt(
                &plain,
                &BTreeMap::from([("b@example.invalid".into(), BTreeMap::from([(202, bundle)]))]),
            )
            .unwrap();
        let digest = super::super::engine::Engine::digest(&xml, "b@example.invalid", 202).unwrap();
        let frame = Frame {
            xml,
            archive: None,
            historical: false,
            receipt_to: None,
        };
        let Some(BackendEvent::Message { message, .. }) =
            unavailable("b@example.invalid", &frame, "Review the device")
        else {
            panic!("Missing placeholder")
        };
        assert!(message.wire_id.is_empty());
        assert_eq!(
            message.aliases,
            vec![format!("omemo2:{}", lifecycle::hex(&digest))]
        );
        assert!(!message.text.contains("Private content"));
        assert!(!message.ciphertext.is_empty());
    }
    use crate::{
        model::{Account, State, new_id},
        protocol::Session,
        transport::{Client, Event},
    };
    use std::{
        sync::mpsc::{Receiver, RecvTimeoutError},
        time::{Duration, Instant},
    };
    use tokio_xmpp::parsers::stanza::Stanza;
    #[track_caller]
    fn wait(rx: &Receiver<Envelope>, predicate: impl Fn(&BackendEvent) -> bool) -> BackendEvent {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let (_, _, event) = rx
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .expect("crypto event deadline");
            if let BackendEvent::CryptoStatus(status) = &event {
                eprintln!("crypto: {} (ready={})", status.text, status.ready);
            }
            if predicate(&event) {
                return event;
            }
        }
    }
    fn config(user: &str) -> Account {
        Account {
            jid: format!("{user}@localhost"),
            host: "127.0.0.1".into(),
            port: std::env::var("MONOGRAM_IT_PORT").unwrap().parse().unwrap(),
            direct_tls: false,
        }
    }
    fn start(user: &str) -> (Session, Receiver<Envelope>) {
        let (tx, rx) = mpsc::channel();
        let session = Session::start(config(user), "fixture-password".into(), 17, tx);
        wait(&rx, |e| {
            matches!(e, BackendEvent::Status { online: true, .. })
        });
        (session, rx)
    }
    async fn probe(session: &Session, peer: &str) -> Result<bool> {
        let (reply, result) = oneshot::channel();
        assert!(session.send(Command::Crypto(Request::Probe {
            peer: peer.into(),
            reply
        })));
        tokio::time::timeout(Duration::from_secs(30), result)
            .await
            .unwrap()
            .unwrap()
    }
    async fn next_xml(client: &mut Client, predicate: impl Fn(&Element) -> bool) -> Element {
        tokio::time::timeout(Duration::from_secs(20), async {
            loop {
                if let Some(Event::Stanza(stanza)) = client.next().await {
                    let xml: Element = stanza.into();
                    if predicate(&xml) {
                        return xml;
                    }
                }
            }
        })
        .await
        .expect("fixture stanza deadline")
    }
    fn keys(rows: Vec<Review>) -> BTreeMap<String, BTreeMap<u32, [u8; 32]>> {
        let mut keys: BTreeMap<_, BTreeMap<_, _>> = BTreeMap::new();
        for row in rows {
            keys.entry(row.peer)
                .or_default()
                .insert(row.device, row.key);
        }
        keys
    }
    fn review(session: &Session, rx: &Receiver<Envelope>, peer: &str) -> Vec<Review> {
        session.send(Command::Crypto(Request::Review(peer.into())));
        match wait(
            rx,
            |e| matches!(e,BackendEvent::CryptoReview{peer:p,..} if p==peer),
        ) {
            BackendEvent::CryptoReview { rows, errors, .. } => {
                assert!(errors.is_empty(), "{errors:?}");
                rows
            }
            _ => unreachable!(),
        }
    }
    fn decide(
        session: &Session,
        rx: &Receiver<Envelope>,
        peer: &str,
        rows: Vec<Review>,
        approve: bool,
    ) {
        session.send(Command::Crypto(Request::Decide {
            keys: keys(rows),
            approve,
        }));
        wait(
            rx,
            |e| matches!(e,BackendEvent::CryptoReview{peer:p,..} if p==peer),
        );
    }
    #[test]
    #[ignore = "requires disposable Prosody, private D-Bus and keyring"]
    fn live_service_roundtrip() {
        assert_eq!(
            std::env::var("MONOGRAM_KEYRING_FIXTURE").as_deref(),
            Ok("1")
        );
        let _ = tokio_xmpp::rustls::crypto::aws_lc_rs::default_provider().install_default();
        let history = crate::storage::Store::open(false).unwrap();
        for owner in ["carol@localhost", "dave@localhost"] {
            history
                .save_account(&Account {
                    jid: owner.into(),
                    host: String::new(),
                    port: 5222,
                    direct_tls: false,
                })
                .unwrap();
        }
        let (alice, a) = start("carol");
        let (bob, b) = start("dave");
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        for rx in [&a, &b] {
            wait(
                rx,
                |e| matches!(e, BackendEvent::CryptoStatus(s) if s.ready),
            );
        }
        assert!(!runtime.block_on(probe(&alice, "frank@localhost")).unwrap());
        assert!(!runtime.block_on(probe(&alice, "dave@localhost")).unwrap());
        let arows = review(&alice, &a, "dave@localhost");
        let bob_fingerprint = lifecycle::hex(&arows[0].key);
        let brows = review(&bob, &b, "carol@localhost");
        assert_eq!(arows.len(), 1);
        assert_eq!(brows.len(), 1);
        assert_eq!(arows[0].trust, "Undecided");
        let blocked_id = new_id();
        alice.send(Command::Send {
            peer: "dave@localhost".into(),
            message: Message {
                text: "Must not leave without trust".into(),
                wire_id: blocked_id.clone(),
                encrypted: true,
                ..Default::default()
            },
            quote: String::new(),
        });
        wait(
            &a,
            |e| matches!(e,BackendEvent::Failed{id,..} if id==&blocked_id),
        );
        history
            .set_blind_trust("carol@localhost", "dave@localhost", true)
            .unwrap();
        assert!(runtime.block_on(probe(&alice, "dave@localhost")).unwrap());
        let stale_id = new_id();
        alice.send(Command::Send {
            peer: "dave@localhost".into(),
            message: Message {
                text: "Must not use a replacement local identity".into(),
                wire_id: stale_id.clone(),
                encrypted: true,
                fingerprint: "stale selected fingerprint".into(),
                ..Default::default()
            },
            quote: String::new(),
        });
        wait(
            &a,
            |e| matches!(e,BackendEvent::Failed{id,..} if id==&stale_id),
        );
        let id = new_id();
        alice.send(Command::Send {
            peer: "dave@localhost".into(),
            message: Message {
                text: "Live encrypted 🌿".into(),
                wire_id: id.clone(),
                encrypted: true,
                ..Default::default()
            },
            quote: String::new(),
        });
        wait(
            &a,
            |e| matches!(e,BackendEvent::Sent{id:sent,..} if sent==&id),
        );
        let unavailable = match wait(
            &b,
            |e| matches!(e,BackendEvent::Message{message,..} if message.wire_id==id),
        ) {
            BackendEvent::Message { message, .. } => message,
            _ => unreachable!(),
        };
        assert!(unavailable.unavailable && !unavailable.encrypted);
        assert!(!unavailable.ciphertext.is_empty());
        assert!(!unavailable.text.contains("Live encrypted"));
        let mut state = State::live("dave@localhost", vec![]);
        state.chat_for("carol@localhost").messages.push(unavailable);
        history.save(&state).unwrap();
        decide(&bob, &b, "carol@localhost", brows, true);
        let (message, token) = match wait(
            &b,
            |e| matches!(e,BackendEvent::EncryptedMessage{message,..} if message.wire_id==id),
        ) {
            BackendEvent::EncryptedMessage { message, token, .. } => (message, token),
            _ => unreachable!(),
        };
        assert_eq!(message.text, "Live encrypted 🌿");
        assert!(message.encrypted && !message.unavailable);
        assert_eq!(message.fingerprint.len(), 64);
        let old = state.chat_for("carol@localhost").messages.pop().unwrap();
        let mut stored = message.clone();
        stored.id = old.id;
        stored.aliases.extend(old.aliases);
        state.chat_for("carol@localhost").messages.push(stored);
        history.save(&state).unwrap();
        // Crash after durable history, before journal acceptance. The same plaintext must recover.
        drop(bob);
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            match b.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
                Err(RecvTimeoutError::Disconnected) => break,
                Err(error) => panic!("crypto worker did not stop: {error}"),
                Ok(_) => {}
            }
        }
        let (bob, b) = start("dave");
        let recovered = wait(
            &b,
            |e| matches!(e,BackendEvent::EncryptedMessage{message,..} if message.wire_id==id),
        );
        if let BackendEvent::EncryptedMessage {
            message: recovered,
            token: again,
            ..
        } = recovered
        {
            assert_eq!(again, token);
            assert_eq!(recovered.text, message.text);
            assert_eq!(recovered.timestamp, message.timestamp);
            assert!(
                history
                    .known(
                        "dave@localhost",
                        "carol@localhost",
                        false,
                        &recovered.aliases
                    )
                    .is_some()
            );
        }
        let status = wait(&b, |e| matches!(e,BackendEvent::CryptoStatus(s) if s.ready));
        if let BackendEvent::CryptoStatus(s) = status {
            assert_eq!(s.fingerprint, bob_fingerprint);
        }
        bob.send(Command::Crypto(Request::Accepted(token)));
        bob.send(Command::History {
            peer: "carol@localhost".into(),
            before: String::new(),
        });
        wait(
            &b,
            |e| matches!(e,BackendEvent::CryptoAliases{aliases,..} if aliases.iter().any(|v|v.starts_with("archive:"))),
        );
        // An invalid auxiliary bundle is optional; an own-device storage failure is not.
        runtime.block_on(async {
            use crate::iq::{Client as Iq, Kind};
            let owner = "carol@localhost";
            let peer = "dave@localhost";
            let requests = Iq::new(alice.tx.clone());
            let own_pep = Pep::new(requests.clone(), owner, 999).unwrap();
            let original = own_pep.devices(owner).await.unwrap().unwrap();
            let sender = *pep::ids(Some(&original)).unwrap().first().unwrap();
            let publish_list = |list| {
                const PUBSUB: &str = "http://jabber.org/protocol/pubsub";
                Element::builder("pubsub", PUBSUB)
                    .append(Element::builder("publish", PUBSUB)
                        .attr("node".try_into().unwrap(), pep::DEVICES)
                        .append(Element::builder("item", PUBSUB)
                            .attr("id".try_into().unwrap(), "current").append(list).build()).build()).build()
            };
            let merged = pep::add_own(Some(&original), 999).unwrap();
            requests.request(owner, Kind::Set, publish_list(merged)).await.unwrap();
            let send = |body: &str| {
                let wire_id = new_id();
                assert!(alice.send(Command::Send {
                    peer: peer.into(),
                    message: Message { text: body.into(), wire_id: wire_id.clone(), encrypted: true, ..Default::default() },
                    quote: String::new(),
                }));
                wire_id
            };
            let id = send("Optional invalid auxiliary copy");
            wait(&a, |e| matches!(e, BackendEvent::Sent { id: sent, .. } if sent == &id));

            let mut aux = records::Store::open(None, owner, 999, MasterKey::new([8; 32])).unwrap();
            let aux_bundle = device::prepare(&mut aux, 1).unwrap();
            own_pep.replenish(&aux_bundle).await.unwrap();
            let path = lifecycle::root().join("crypto").join(lifecycle::hex(&openssl::sha::sha256(owner.as_bytes()))).join("omemo2.sqlite3");
            let database = rusqlite::Connection::open(path).unwrap();
            database.execute_batch("CREATE TRIGGER fixture_own_seen BEFORE INSERT ON omemo2_records WHEN NEW.peer='carol@localhost' AND NEW.kind=9 BEGIN SELECT RAISE(ABORT,'fixture own-device persistence');END;").unwrap();
            assert!(matches!(probe(&alice, peer).await, Err(Error::Storage(_))));
            let id = send("Own storage failure must block");
            wait(&a, |e| matches!(e, BackendEvent::Failed { id: failed, .. } if failed == &id));
            database.execute_batch("DROP TRIGGER fixture_own_seen;").unwrap();

            let aux_rows = review(&alice, &a, owner);
            assert_eq!(aux_rows.len(), 1);
            assert_eq!(aux_rows[0].key, aux_bundle.identity);
            assert_eq!(aux_rows[0].trust, "Undecided");
            assert!(matches!(probe(&alice, peer).await, Err(Error::OwnDeviceTrustRequired)));
            let (reply, selected) = oneshot::channel();
            assert!(alice.send(Command::ChooseEncryption { peer: peer.into(), reply }));
            assert!(matches!(tokio::time::timeout(Duration::from_secs(15), selected).await.unwrap().unwrap(), Err(Error::OwnDeviceTrustRequired)));
            decide(&alice, &a, owner, aux_rows, true);

            let mut other_resource = Client::new(config("carol"), "fixture-password".into());
            assert!(matches!(tokio::time::timeout(Duration::from_secs(20), other_resource.next()).await.unwrap(), Some(Event::Online)));
            let enable: Element = "<iq xmlns='jabber:client' type='set' id='own-carbon'><enable xmlns='urn:xmpp:carbons:2'/></iq>".parse().unwrap();
            other_resource.send_stanza(Stanza::try_from(enable).unwrap()).await.unwrap();
            let response = next_xml(&mut other_resource, |xml| xml.is("iq", payload::CLIENT) && xml.attr("id") == Some("own-carbon")).await;
            assert_eq!(response.attr("type"), Some("result"));
            assert!(probe(&alice, peer).await.unwrap());
            let id = send("Private outgoing copy to my second device");
            wait(&a, |e| matches!(e, BackendEvent::Sent { id: sent, .. } if sent == &id));
            let carbon = next_xml(&mut other_resource, |xml| xml.get_child("sent", "urn:xmpp:carbons:2").is_some()).await;
            assert_eq!(carbon.attr("from"), Some(owner));
            let wire = carbon.get_child("sent", "urn:xmpp:carbons:2").unwrap()
                .get_child("forwarded", "urn:xmpp:forward:0").unwrap()
                .get_child("message", payload::CLIENT).unwrap();
            // The first carbon is the real send. The preceding successful probe emitted no chat stanza.
            assert_eq!(wire.attr("id"), Some(id.as_str()));
            assert!(wire.get_child("body", payload::CLIENT).is_none());
            let original_sender = own_pep.bundle(owner, sender).await.unwrap();
            let mut aux = super::super::engine::Engine::new(aux, 2).unwrap();
            assert!(aux.receive(wire, 2).is_err(), "Own devices still require exact sender trust");
            device::observe(&mut aux.store, owner, sender, &original_sender, false).unwrap();
            device::decide(&mut aux.store, &BTreeMap::from([(owner.into(), BTreeMap::from([(sender, original_sender.identity)]))]), true).unwrap();
            let decrypted = aux.receive(wire, 2).unwrap().content.unwrap();
            let (received_peer, decoded, _) = protocol::normalize_protected(&decrypted, owner, None).unwrap();
            assert_eq!(received_peer, peer);
            assert!(decoded.own);
            assert_eq!(decoded.text, "Private outgoing copy to my second device");
            other_resource.send_end().await.unwrap();
            requests.request(owner, Kind::Set, publish_list(original)).await.unwrap();
            eprintln!("OMEMO 2 direct send and sent carbon decrypt on separate resource with independent trusted identity");
        });
        let rows = review(&alice, &a, "dave@localhost");
        decide(&alice, &a, "dave@localhost", rows, false);
        let failed = new_id();
        alice.send(Command::Send {
            peer: "dave@localhost".into(),
            message: Message {
                text: "Blocked after denial".into(),
                wire_id: failed.clone(),
                encrypted: true,
                ..Default::default()
            },
            quote: String::new(),
        });
        wait(
            &a,
            |e| matches!(e,BackendEvent::Failed{id,..} if id==&failed),
        );
        drop(alice);
        drop(bob);
    }
}
