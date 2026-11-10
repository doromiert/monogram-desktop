//! Legacy OMEMO runs independently of the selected outgoing version.
use super::{Legacy, MAX_WIRE, NS, exists, probe_unavailable};
use crate::{
    crypto::{
        bundle,
        journal::{Context, Received, Token},
        lifecycle, payload,
        service::{Frame, Review, Status},
        *,
    },
    encryption_policy::Mode,
    iq,
    model::Message,
    protocol::{self, BackendEvent, Command, Envelope},
};
use std::{collections::BTreeMap, path::PathBuf, sync::mpsc, thread, time::Duration};
use tokio::sync::{mpsc as channel, oneshot};
use tokio_xmpp::minidom::Element;

#[derive(Debug)]
pub enum Request {
    Online(bool),
    Enable,
    Review(String),
    RefreshTrust(String),
    Decide {
        keys: BTreeMap<String, BTreeMap<u32, [u8; 32]>>,
        approve: bool,
    },
    Probe {
        peer: String,
        reply: oneshot::Sender<Result<bool>>,
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
    incoming: channel::Sender<Frame>,
    stop: Option<oneshot::Sender<()>>,
    owner: String,
    generation: u64,
    events: mpsc::Sender<Envelope>,
}
impl Handle {
    pub fn start(
        owner: String,
        generation: u64,
        network: channel::UnboundedSender<Command>,
        events: mpsc::Sender<Envelope>,
    ) -> Self {
        let (tx, mut requests) = channel::unbounded_channel();
        let (incoming, mut frames) = channel::channel(32);
        let (stop, stopped) = oneshot::channel();
        let handle = Self {
            tx,
            incoming,
            stop: Some(stop),
            owner: owner.clone(),
            generation,
            events: events.clone(),
        };
        thread::Builder::new().name("monogram-legacy-omemo".into()).spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().expect("Legacy OMEMO runtime");
            let mut worker = Worker { owner, generation, network, events, root: lifecycle::root(), local: None, online: false, ready: false };
            runtime.block_on(async move {
                tokio::select! {
                    _ = stopped => {},
                    _ = async {
                        loop {
                            tokio::select! {
                                biased;
                                next = requests.recv() => match next { Some(request) => worker.process(request).await, None => break },
                                next = frames.recv() => match next { Some(frame) => worker.process(Request::Incoming(frame)).await, None => break },
                            }
                        }
                    } => {},
                }
            });
        }).expect("Legacy OMEMO worker");
        handle
    }
    pub fn send(&self, request: Request) {
        if let Request::Incoming(frame) = request {
            if bundle::xml(&frame.xml, MAX_WIRE).is_err() {
                return;
            }
            if let Err(error) = self.incoming.try_send(frame) {
                if let Some(event) = unavailable(
                    &self.owner,
                    &error.into_inner(),
                    "Legacy OMEMO is busy. Reconnect or refresh fingerprints to retry.",
                    None,
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
    pub fn sender(&self) -> channel::UnboundedSender<Request> {
        self.tx.clone()
    }
}
impl Drop for Handle {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
    }
}

struct Worker {
    owner: String,
    generation: u64,
    network: channel::UnboundedSender<Command>,
    events: mpsc::Sender<Envelope>,
    root: PathBuf,
    local: Option<Legacy>,
    online: bool,
    ready: bool,
}
fn archive(frame: &Frame) -> Option<(&str, &str)> {
    frame
        .archive
        .as_ref()
        .map(|(id, stamp)| (id.as_str(), stamp.as_str()))
}
fn alias(digest: [u8; 32]) -> String {
    format!("legacy-omemo:{}", lifecycle::hex(&digest))
}
fn unavailable(
    owner: &str,
    frame: &Frame,
    text: &str,
    digest: Option<[u8; 32]>,
) -> Option<BackendEvent> {
    frame
        .xml
        .get_child("encrypted", NS)?
        .get_child("payload", NS)?;
    let (peer, mut message, historical) = protocol::normalize(&frame.xml, owner, archive(frame))?;
    message.text = text.into();
    message.ciphertext = String::from_utf8(bundle::xml(&frame.xml, MAX_WIRE).ok()?).ok()?;
    if let Some(digest) = digest {
        message.aliases.push(alias(digest));
    }
    Some(BackendEvent::Message {
        peer,
        message,
        historical: frame.historical || historical,
        receipt_to: None,
    })
}
fn describe(error: &Error) -> String {
    match error {
        Error::Invalid(_) => "Invalid Legacy OMEMO message or key data".into(),
        Error::Native(_) => "Could not open or encrypt the Legacy OMEMO message".into(),
        Error::Storage(_) => "Could not save Legacy OMEMO state".into(),
        Error::Crypto(_) => "Legacy OMEMO message authentication failed".into(),
        Error::Incomplete => {
            "Legacy OMEMO state is incomplete. The existing identity was preserved.".into()
        }
        _ => error.to_string(),
    }
}
impl Worker {
    fn emit(&self, event: BackendEvent) {
        let _ = self
            .events
            .send((self.generation, self.owner.clone(), event));
    }
    fn status(&mut self, text: Option<String>) {
        let enabled = self.local.is_some() || exists(&self.root, &self.owner);
        let mut status = Status {
            enabled,
            ready: self.ready,
            device: 0,
            fingerprint: String::new(),
            text: text.unwrap_or_else(|| {
                if self.ready {
                    "Legacy OMEMO ready"
                } else if self.online {
                    "Legacy OMEMO is unavailable"
                } else {
                    "Legacy OMEMO is offline"
                }
                .into()
            }),
        };
        if let Some(local) = self.local.as_mut() {
            status.device = local.device();
            status.fingerprint = local
                .fingerprint()
                .map(|key| lifecycle::hex(&key))
                .unwrap_or_default();
        }
        let _ = self.network.send(Command::LegacyReady(status.ready));
        self.emit(BackendEvent::LegacyStatus(status));
    }
    async fn initialize(&mut self) -> Result<()> {
        if !self.online {
            return Err(Error::Unavailable("Connect before using Legacy OMEMO"));
        }
        self.ready = false;
        self.status(Some("Preparing Legacy OMEMO…".into()));
        if self.local.is_none() {
            self.local = Some(
                Legacy::open(
                    &self.root,
                    &self.owner,
                    iq::Client::new(self.network.clone()),
                    true,
                )
                .await?,
            );
        }
        self.local.as_mut().unwrap().initialize().await?;
        self.ready = true;
        self.status(None);
        self.recover().await?;
        self.retry().await;
        Ok(())
    }
    fn history(&self) -> Result<Option<crate::storage::Store>> {
        super::history(&self.root.join("history.sqlite3"))
    }
    async fn wire(&self, xml: Element) -> bool {
        let (reply, received) = oneshot::channel();
        if self.network.send(Command::Wire { xml, reply }).is_err() {
            return false;
        }
        tokio::time::timeout(Duration::from_secs(20), received)
            .await
            .is_ok_and(|v| v.unwrap_or(false))
    }
    async fn review(&mut self, peer: &str) {
        let mut rows = vec![];
        let mut errors = vec![];
        let mut peers = vec![peer.to_string()];
        if peer != self.owner {
            peers.push(self.owner.clone());
        }
        for address in peers {
            let result = match self.local.as_mut().filter(|_| self.ready) {
                Some(local) => local.discover(&address).await,
                None => Err(Error::Unavailable("Legacy OMEMO is not ready")),
            };
            match result {
                Ok((devices, failed)) => {
                    rows.extend(devices.into_iter().map(|row| Review {
                        peer: row.peer,
                        device: row.id,
                        key: row.key,
                        trust: format!("{:?}", row.trust),
                    }));
                    errors.extend(
                        failed
                            .into_iter()
                            .map(|(id, error)| format!("{address} · {id}: {}", describe(&error))),
                    );
                }
                Err(error) => errors.push(format!("{address}: {}", describe(&error))),
            }
        }
        self.emit(BackendEvent::LegacyReview {
            peer: peer.into(),
            rows,
            errors,
        });
    }
    async fn probe(&mut self, peer: &str) -> Result<bool> {
        if !self.ready {
            if let Err(error) = self.initialize().await {
                self.status(Some(describe(&error)));
                return if probe_unavailable(&error) {
                    Ok(false)
                } else {
                    Err(error)
                };
            }
        }
        self.local
            .as_mut()
            .ok_or(Error::Incomplete)?
            .probe(peer)
            .await
    }
    async fn send_message(&mut self, peer: &str, message: &Message, quote: &str) -> Result<()> {
        if message.mode() != Mode::Legacy {
            return Err(Error::Invalid("wrong outgoing encryption mode"));
        }
        if message.upload.is_some()
            || message.attachment.is_some()
            || message.poll.is_some()
            || message.action.is_some()
        {
            return Err(Error::Unavailable(
                "This message needs OMEMO 2. Legacy OMEMO protects message text only.",
            ));
        }
        if !self.ready || !self.online {
            return Err(Error::Unavailable("Legacy OMEMO is not ready"));
        }
        let local = self.local.as_mut().ok_or(Error::Incomplete)?;
        if !message.fingerprint.is_empty()
            && lifecycle::hex(&local.fingerprint()?) != message.fingerprint
        {
            return Err(Error::Unavailable(
                "The local Legacy OMEMO identity changed. Review it before retrying.",
            ));
        }
        let clear = protocol::outgoing(peer, message, quote);
        let body = clear
            .get_child("body", payload::CLIENT)
            .ok_or(Error::Invalid("legacy body"))?
            .text();
        let mut xml = local.encrypt(peer, &body, &message.wire_id).await?;
        xml.append_child(protocol::el("request", "urn:xmpp:receipts"));
        if self.wire(xml).await {
            Ok(())
        } else {
            Err(Error::Uncertain)
        }
    }
    async fn incoming(&mut self, frame: &Frame) -> Result<()> {
        if !self.ready {
            return Err(Error::Unavailable("Legacy OMEMO is not ready"));
        }
        let digest = self
            .local
            .as_ref()
            .ok_or(Error::Incomplete)?
            .digest(&frame.xml)?;
        let outer = protocol::normalize(&frame.xml, &self.owner, archive(frame));
        let alias = alias(digest);
        if let Some((peer, message, _)) = &outer {
            if let Some(history) = self.history()? {
                if let Some(id) = history.known(&self.owner, peer, message.own, &[alias.clone()]) {
                    if history
                        .message(&self.owner, peer, &id)
                        .is_some_and(|m| !m.unavailable)
                    {
                        self.emit(BackendEvent::CryptoAliases {
                            peer: peer.clone(),
                            own: message.own,
                            id,
                            aliases: message.aliases.clone(),
                        });
                        if let Some(token) = self.local.as_mut().unwrap().pending_token(&digest)? {
                            self.accept(token).await?;
                        }
                        return Ok(());
                    }
                }
            }
        }
        let stamp = outer
            .as_ref()
            .and_then(|(_, message, _)| {
                chrono::DateTime::parse_from_rfc3339(&message.timestamp).ok()
            })
            .map(|stamp| stamp.timestamp_millis())
            .unwrap_or_else(|| chrono::Utc::now().timestamp_millis());
        let received = self
            .local
            .as_mut()
            .unwrap()
            .receive(
                &frame.xml,
                stamp,
                Some(Context {
                    archive: frame.archive.clone(),
                    historical: frame.historical,
                    receipt_to: frame.receipt_to.clone(),
                }),
            )
            .await?;
        self.deliver(received, Some(frame), alias).await
    }
    async fn deliver(
        &mut self,
        received: Received,
        frame: Option<&Frame>,
        alias: String,
    ) -> Result<()> {
        if let Some(content) = received.content {
            let outer = frame
                .map(|f| &f.xml)
                .or(received.outer.as_ref())
                .ok_or(Error::Incomplete)?;
            let info = frame.and_then(|f| f.archive.as_ref()).or_else(|| {
                received
                    .context
                    .as_ref()
                    .and_then(|ctx| ctx.archive.as_ref())
            });
            let (peer, metadata, historical) = protocol::normalize(
                outer,
                &self.owner,
                info.map(|(id, stamp)| (id.as_str(), stamp.as_str())),
            )
            .ok_or(Error::Invalid("legacy envelope metadata"))?;
            let (_, mut message, _) = protocol::normalize_protected(&content, &self.owner, None)
                .ok_or(Error::Invalid("legacy body"))?;
            message.set_encryption(Mode::Legacy, lifecycle::hex(&received.fingerprint));
            message.unavailable = false;
            message.aliases.extend(metadata.aliases);
            if !alias.is_empty() {
                message.aliases.push(alias);
            }
            let stamp = chrono::DateTime::from_timestamp_millis(received.sent_time)
                .ok_or(Error::Incomplete)?;
            message.timestamp = stamp.to_rfc3339_opts(chrono::SecondsFormat::Micros, false);
            message.time = stamp
                .with_timezone(&chrono::Local)
                .format("%H:%M")
                .to_string();
            let receipt_to = frame.and_then(|f| f.receipt_to.clone()).or_else(|| {
                received
                    .context
                    .as_ref()
                    .and_then(|ctx| ctx.receipt_to.clone())
            });
            let historical = historical
                || frame
                    .map(|f| f.historical)
                    .unwrap_or_else(|| received.context.as_ref().is_none_or(|ctx| ctx.historical));
            self.emit(BackendEvent::LegacyEncryptedMessage {
                peer,
                message,
                historical,
                receipt_to,
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
            .pending()?
            .into_iter()
            .find(|r| r.token.as_ref() == Some(&token));
        if !local.accept(&token)? {
            return Ok(());
        }
        if let Some(received) = pending {
            if received.replenish {
                local.replenish().await?;
            }
            if let Some(ack) = received.acknowledgement {
                if self.wire(ack).await {
                    self.local.as_mut().unwrap().acknowledge(&token)?;
                }
            }
        }
        Ok(())
    }
    async fn recover(&mut self) -> Result<()> {
        let local = self.local.as_mut().ok_or(Error::Incomplete)?;
        for pending in local.pending()? {
            let alias = pending
                .outer
                .as_ref()
                .and_then(|outer| self.local.as_ref().unwrap().digest(outer).ok())
                .map(alias)
                .unwrap_or_default();
            self.deliver(pending, None, alias).await?;
        }
        Ok(())
    }
    async fn retry(&mut self) {
        let rows = (|| -> Result<Vec<Message>> {
            let Some(history) = self.history()? else {
                return Ok(vec![]);
            };
            let mut query = history.db.prepare("SELECT body FROM messages WHERE account=? AND json_extract(body,'$.unavailable')=1 AND instr(json_extract(body,'$.ciphertext'),?)>0 LIMIT 128")?;
            let rows = query
                .query_map([&self.owner, NS], |r| r.get::<_, String>(0))?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            Ok(rows
                .into_iter()
                .filter_map(|b| serde_json::from_str(&b).ok())
                .collect())
        })();
        if let Ok(rows) = rows {
            let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
            for message in rows {
                if tokio::time::Instant::now() >= deadline {
                    break;
                }
                let Ok(mut xml) = bundle::document(&message.ciphertext, MAX_WIRE) else {
                    continue;
                };
                if xml.get_child("delay", "urn:xmpp:delay").is_none() {
                    xml.append_child(super::wire::attr(
                        super::wire::element("delay", "urn:xmpp:delay"),
                        "stamp",
                        message.timestamp,
                    ));
                }
                let frame = Frame {
                    xml,
                    archive: None,
                    historical: true,
                    receipt_to: None,
                };
                let _ = tokio::time::timeout_at(deadline, self.incoming(&frame)).await;
            }
        }
    }
    async fn process(&mut self, request: Request) {
        match request {
            Request::Online(online) => {
                self.online = online;
                if online {
                    if let Err(error) = self.initialize().await {
                        self.status(Some(describe(&error)));
                    }
                } else {
                    self.ready = false;
                    self.status(None);
                }
            }
            Request::Enable => {
                if let Err(error) = self.initialize().await {
                    self.status(Some(describe(&error)));
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
                    .and_then(|local| local.decide(&keys, approve));
                if let Err(error) = result {
                    self.status(Some(describe(&error)));
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
                            text: describe(&error),
                        });
                    }
                }
            }
            Request::Incoming(frame) => {
                if let Err(error) = self.incoming(&frame).await {
                    let digest = self
                        .local
                        .as_ref()
                        .and_then(|local| local.digest(&frame.xml).ok());
                    if let Some(event) = unavailable(&self.owner, &frame, &describe(&error), digest)
                    {
                        self.emit(event);
                    }
                }
            }
            Request::Accepted(token) => {
                if let Err(error) = self.accept(token).await {
                    self.status(Some(describe(&error)));
                }
            }
        }
    }
}
