// SPDX-License-Identifier: AGPL-3.0-only
//! Independent cancellable PQ actor; transport, GTK and classic ratchets never block on it.
use super::{
    bundle::{self, Bundle, NS},
    device::{Device, Trust},
    native, payload,
    pep::{self, Pep},
    wire::{self, Target, Token},
};
use crate::{
    crypto::{
        Error, Result,
        journal::{Context, RoomContext},
        lifecycle,
        service::{Frame, Status},
    },
    iq,
    model::Message,
    protocol::{self, BackendEvent, Command, Envelope},
    rooms::members::{RoomRecipients, RoomSender},
};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};
use tokio::sync::{mpsc as channel, oneshot};
use tokio_xmpp::{jid::Jid, minidom::Element};

#[derive(Clone, Debug)]
pub struct Review {
    pub peer: String,
    pub device: u32,
    pub fingerprint: String,
    pub trust: String,
}
#[derive(Debug)]
pub enum Request {
    Online(bool),
    Enable,
    Review(String),
    RefreshTrust(String),
    ReviewRoom {
        peer: String,
        members: BTreeSet<String>,
    },
    Decide {
        keys: BTreeMap<String, BTreeMap<u32, String>>,
        approve: bool,
    },
    Probe {
        peer: String,
        reply: oneshot::Sender<Result<bool>>,
    },
    ProbeRoom {
        recipients: RoomRecipients,
        reply: oneshot::Sender<Result<bool>>,
    },
    Send {
        peer: String,
        message: Message,
        quote: String,
    },
    SendRoom {
        recipients: RoomRecipients,
        message: Message,
        quote: String,
        epoch: u64,
    },
    Incoming(Frame),
    IncomingRoom {
        frame: Frame,
        sender: RoomSender,
        metadata: Message,
    },
    Preview {
        frame: Frame,
        room: Option<RoomContext>,
        reply: oneshot::Sender<bool>,
    },
    Accepted(Token),
}
pub struct Handle {
    tx: channel::UnboundedSender<Request>,
    incoming: channel::Sender<Request>,
    previews: channel::Sender<Request>,
    stop: Option<oneshot::Sender<()>>,
    owner: String,
    generation: u64,
    events: mpsc::Sender<Envelope>,
}
impl Handle {
    /// Control requests only; incoming ciphertext must use the bounded `send` path.
    pub fn sender(&self) -> channel::UnboundedSender<Request> {
        self.tx.clone()
    }
    pub fn start(
        owner: String,
        generation: u64,
        network: channel::UnboundedSender<Command>,
        events: mpsc::Sender<Envelope>,
    ) -> Self {
        let (tx, mut requests) = channel::unbounded_channel();
        let (incoming, mut frames) = channel::channel(32);
        let (previews, mut preview) = channel::channel(150);
        let (stop, stopped) = oneshot::channel();
        let (account, notifications) = (owner.clone(), events.clone());
        thread::Builder::new().name("monogram-pq-omemo".into()).spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().expect("PQ crypto runtime");
            runtime.block_on(async move {
                let mut worker = Worker { owner, generation, network, events, root: lifecycle::root(), local: None,
                    online: false, ready: false, publication_pending: false, bundles: BTreeMap::new(), lists: BTreeMap::new() };
                tokio::select! { _ = stopped => {}, _ = async move {
                    loop { tokio::select! { biased;
                        request = requests.recv() => { let Some(request) = request else { break }; worker.process(request).await; },
                        frame = frames.recv() => { let Some(frame) = frame else { break }; worker.process(frame).await; },
                        request = preview.recv() => { let Some(request) = request else { break }; worker.process(request).await; },
                    }}
                } => {} }
            });
        }).expect("PQ crypto worker");
        Self {
            tx,
            incoming,
            previews,
            stop: Some(stop),
            owner: account,
            generation,
            events: notifications,
        }
    }
    pub fn preview(&self, frame: Frame) -> Option<oneshot::Receiver<bool>> {
        self.preview_with_room(frame, None)
    }
    pub fn preview_with_room(
        &self,
        frame: Frame,
        room: Option<RoomContext>,
    ) -> Option<oneshot::Receiver<bool>> {
        bundle::xml(&frame.xml, wire::MAX_WIRE).ok()?;
        let (reply, response) = oneshot::channel();
        self.previews
            .try_send(Request::Preview { frame, room, reply })
            .ok()?;
        Some(response)
    }
    pub fn send(&self, request: Request) {
        let request = match request {
            Request::Preview { .. } => {
                let _ = self.previews.try_send(request);
                return;
            }
            request @ (Request::Incoming(_) | Request::IncomingRoom { .. }) => request,
            request => {
                let _ = self.tx.send(request);
                return;
            }
        };
        let (frame, room) = match &request {
            Request::Incoming(frame) => (frame, None),
            Request::IncomingRoom {
                frame,
                sender,
                metadata,
            } => (
                frame,
                Some(RoomContext {
                    sender: sender.clone(),
                    metadata: metadata.clone(),
                }),
            ),
            _ => unreachable!(),
        };
        if bundle::xml(&frame.xml, wire::MAX_WIRE).is_err() {
            return;
        }
        if let Err(failed) = self.incoming.try_send(request) {
            let request = failed.into_inner();
            let frame = match &request {
                Request::Incoming(frame) | Request::IncomingRoom { frame, .. } => frame,
                _ => unreachable!(),
            };
            if let Some(event) = unavailable(
                &self.owner,
                frame,
                room.as_ref(),
                "Encryption queue is busy; the ciphertext was saved for retry",
            ) {
                let _ = self
                    .events
                    .send((self.generation, self.owner.clone(), event));
            }
        }
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
    local: Option<Device>,
    online: bool,
    ready: bool,
    publication_pending: bool,
    bundles: BTreeMap<(String, u32), (Instant, Bundle)>,
    lists: BTreeMap<String, (Instant, BTreeSet<u32>)>,
}
fn archive(frame: &Frame) -> Option<(&str, &str)> {
    frame
        .archive
        .as_ref()
        .map(|(id, stamp)| (id.as_str(), stamp.as_str()))
}
fn metadata(
    owner: &str,
    frame: &Frame,
    room: Option<&RoomContext>,
) -> Option<(String, Message, bool)> {
    let Some(room) = room else {
        return protocol::normalize(&frame.xml, owner, archive(frame));
    };
    let address = room.sender.occupant.parse::<Jid>().ok()?;
    if !frame.xml.is("message", payload::CLIENT)
        || frame.xml.attr("type") != Some("groupchat")
        || frame.xml.attr("from") != Some(room.sender.occupant.as_str())
        || address.resource().is_none()
        || address.to_bare().as_str() != room.sender.room
        || protocol::bare(&room.sender.author).ok().as_deref() != Some(room.sender.author.as_str())
    {
        return None;
    }
    let mut message = room.metadata.clone();
    message.sender = room.sender.occupant.clone();
    message.own = room.sender.author == owner;
    message.author = if message.own {
        "You".into()
    } else {
        address.resource()?.as_str().into()
    };
    Some((
        room.sender.room.clone(),
        message,
        frame.historical || frame.archive.is_some(),
    ))
}
fn unavailable(
    owner: &str,
    frame: &Frame,
    room: Option<&RoomContext>,
    reason: &str,
) -> Option<BackendEvent> {
    frame
        .xml
        .get_child("encrypted", NS)?
        .get_child("payload", NS)?;
    let (peer, mut message, historical) = metadata(owner, frame, room)?;
    message.text = reason.into();
    message.unavailable = true;
    message.ciphertext = String::from_utf8(bundle::xml(&frame.xml, wire::MAX_WIRE).ok()?).ok()?;
    message.set_encryption(crate::encryption_policy::Mode::Pq, String::new());
    Some(BackendEvent::Message {
        peer,
        message,
        historical: frame.historical || historical,
        receipt_to: None,
    })
}
fn describe(error: &Error) -> String {
    match error {
        Error::Invalid(_) | Error::Crypto(_) => {
            "The OMEMO 2 message could not be authenticated".into()
        }
        Error::Storage(_) | Error::Incomplete => {
            "OMEMO 2 state could not be read or saved; the existing identity was preserved".into()
        }
        _ => error.to_string(),
    }
}
fn pep_error(error: pep::Error) -> Error {
    match error {
        pep::Error::Crypto(error) => error,
        pep::Error::PublicAccess => Error::Unavailable("OMEMO 2 public keys are not readable"),
        pep::Error::Request(_) => {
            Error::Unavailable("OMEMO 2 public keys could not be fetched or published")
        }
    }
}
impl Worker {
    fn emit(&self, event: BackendEvent) {
        let _ = self
            .events
            .send((self.generation, self.owner.clone(), event));
    }
    fn status(&mut self, error: Option<String>) {
        let fingerprint = self
            .local
            .as_mut()
            .and_then(|local| local.fingerprint().ok())
            .unwrap_or_default();
        let status = Status {
            enabled: self.local.is_some(),
            ready: self.ready && self.online,
            device: self.local.as_ref().map_or(0, |local| local.id),
            fingerprint,
            text: error.unwrap_or_else(|| {
                if self.ready {
                    "OMEMO 2 ready"
                } else {
                    "OMEMO 2 is preparing or disconnected"
                }
                .into()
            }),
        };
        let _ = self.network.send(Command::PqReady(status.ready));
        self.emit(BackendEvent::PqCrypto(status));
    }
    fn pep(&self) -> Result<Pep> {
        Pep::new(
            iq::Client::new(self.network.clone()),
            &self.owner,
            self.local.as_ref().ok_or(Error::Incomplete)?.id,
        )
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
    fn blind(&self, peer: &str) -> Result<bool> {
        if peer == self.owner {
            return Ok(false);
        }
        Ok(self
            .history()?
            .map(|history| history.blind_trust(&self.owner, peer))
            .transpose()?
            .unwrap_or(false))
    }
    async fn local(&mut self, create: bool) -> Result<()> {
        if self.local.is_none() {
            self.local = Some(Device::open(&self.root, &self.owner, create).await?);
        }
        Ok(())
    }
    async fn initialize(&mut self) -> Result<()> {
        if !self.online {
            return Err(Error::Unavailable("Connect before setting up OMEMO 2"));
        }
        if self.ready && !self.publication_pending {
            return Ok(());
        }
        self.status(Some("Preparing OMEMO 2…".into()));
        self.local(true).await?;
        let bundle = Bundle::from_parts(self.local.as_mut().unwrap().prepare()?)?;
        self.pep()?.initialize(&bundle).await.map_err(pep_error)?;
        self.publication_pending = false;
        self.ready = true;
        self.status(None);
        self.recover().await?;
        Ok(())
    }
    async fn devices(&mut self, peer: &str, force: bool) -> Result<BTreeSet<u32>> {
        crate::crypto::address(peer, 1)?;
        if !force {
            if let Some((time, ids)) = self
                .lists
                .get(peer)
                .filter(|(time, _)| time.elapsed() < Duration::from_secs(60))
            {
                let _ = time;
                return Ok(ids.clone());
            }
        }
        let ids = pep::ids(self.pep()?.devices(peer).await.map_err(pep_error)?.as_ref())?;
        if self.lists.len() >= 256 && !self.lists.contains_key(peer) {
            self.lists.clear();
        }
        self.lists
            .insert(peer.into(), (Instant::now(), ids.clone()));
        Ok(ids)
    }
    async fn observe(&mut self, peer: &str, id: u32, force: bool) -> Result<Option<Bundle>> {
        if !force {
            if let Some((_, bundle)) = self
                .bundles
                .get(&(peer.into(), id))
                .filter(|(time, _)| time.elapsed() < Duration::from_secs(60))
            {
                return Ok(Some(bundle.clone()));
            }
        }
        self.bundles.remove(&(peer.into(), id));
        let xml = self
            .pep()?
            .item(peer, pep::BUNDLES, Some(&id.to_string()))
            .await
            .map_err(pep_error)?;
        let Some(xml) = xml else {
            return Ok(None);
        };
        let bundle = Bundle::parse(&xml)?;
        let blind = self.blind(peer)?;
        self.local
            .as_mut()
            .ok_or(Error::Incomplete)?
            .observe(peer, id, &bundle, blind)?;
        if self.bundles.len() >= 1024 {
            self.bundles.clear();
        }
        self.bundles
            .insert((peer.into(), id), (Instant::now(), bundle.clone()));
        Ok(Some(bundle))
    }
    async fn targets(
        &mut self,
        peer: &str,
        optional: bool,
        force: bool,
    ) -> Result<Option<Vec<Target>>> {
        let ids = self.devices(peer, force).await?;
        if ids.is_empty() {
            return if optional { Ok(Some(vec![])) } else { Ok(None) };
        }
        let local_id = self.local.as_ref().ok_or(Error::Incomplete)?.id;
        let mut result = Vec::new();
        for id in ids {
            if peer == self.owner && id == local_id {
                continue;
            }
            let bundle = self.observe(peer, id, force).await?;
            let local = self.local.as_mut().ok_or(Error::Incomplete)?;
            let tx = local.begin()?;
            let blocked = tx
                .get(&format!("blocked/{}", native::peer_key(peer, id)?))?
                .is_some();
            if blocked {
                continue;
            }
            let trusted = native::trust_record(&tx, peer, id)?;
            let Some(trusted) = trusted else {
                return Err(if peer == self.owner {
                    Error::OwnDeviceTrustRequired
                } else {
                    Error::TrustRequired
                });
            };
            if trusted.len() != 2625 {
                return Err(Error::Incomplete);
            }
            native::authorize(
                &tx,
                peer,
                id,
                trusted[..33].try_into().map_err(|_| Error::Incomplete)?,
                &trusted[33..],
            )?;
            if bundle.is_none() && !native::has_session(&tx, peer, id)? {
                return Err(Error::InvalidBundle);
            }
            result.push(Target {
                peer: peer.into(),
                device: id,
                bundle,
            });
        }
        if result.is_empty() && !optional {
            return Err(Error::TrustRequired);
        }
        Ok(Some(result))
    }
    async fn discover(&mut self, peer: &str) -> Result<Vec<Review>> {
        self.initialize().await?;
        let ids = self.devices(peer, true).await?;
        for id in ids {
            if peer != self.owner || id != self.local.as_ref().unwrap().id {
                self.observe(peer, id, true).await?;
            }
        }
        Ok(self
            .local
            .as_mut()
            .unwrap()
            .review(peer)?
            .into_iter()
            .map(|review| Review {
                peer: review.peer,
                device: review.device,
                fingerprint: review.fingerprint,
                trust: match review.trust {
                    Trust::Trusted => "trusted",
                    Trust::Rejected => "rejected",
                    Trust::Undecided => "undecided",
                }
                .into(),
            })
            .collect())
    }
    async fn recipients(&mut self, peer: &str) -> Result<Option<Vec<Target>>> {
        self.initialize().await?;
        let Some(mut targets) = self.targets(peer, false, false).await? else {
            return Ok(None);
        };
        if peer != self.owner {
            let owner = self.owner.clone();
            targets.extend(self.targets(&owner, true, false).await?.unwrap_or_default());
        }
        Ok(Some(targets))
    }
    async fn room_targets(&mut self, recipients: &RoomRecipients) -> Result<Option<Vec<Target>>> {
        self.initialize().await?;
        if recipients.members.is_empty() || !recipients.members.contains_key(&self.owner) {
            return Err(Error::Unavailable(
                "A complete room member list is required for encryption",
            ));
        }
        let mut targets = Vec::new();
        for peer in recipients.members.keys() {
            let Some(found) = self.targets(peer, peer == &self.owner, false).await? else {
                return Ok(None);
            };
            targets.extend(found);
            if targets.len() > 500 {
                return Err(Error::Invalid("PQ recipient limit"));
            }
        }
        Ok(Some(targets))
    }
    fn clear_message(
        &self,
        peer: &str,
        message: &Message,
        quote: &str,
        room: bool,
    ) -> Result<(Element, Element)> {
        if message.upload.is_some()
            || message.attachment.as_ref().is_some_and(|file| {
                !file.valid_outgoing(true) || message.poll.is_some() || message.action.is_some()
            })
        {
            return Err(Error::Invalid("PQ attachment"));
        }
        let root = protocol::outgoing(peer, message, quote);
        let mut plain = Element::builder("message", payload::CLIENT)
            .attr("from".try_into().unwrap(), &self.owner)
            .attr("to".try_into().unwrap(), peer)
            .attr(
                "type".try_into().unwrap(),
                if room { "groupchat" } else { "chat" },
            )
            .attr("id".try_into().unwrap(), &message.wire_id)
            .build();
        for child in root.children().filter(|child| payload::allowed(child)) {
            plain.append_child(child.clone());
        }
        Ok((root, plain))
    }
    async fn encrypted(
        &mut self,
        peer: &str,
        message: &Message,
        quote: &str,
        room: Option<&RoomRecipients>,
    ) -> Result<Element> {
        if !self.online {
            return Err(Error::Unavailable("OMEMO 2 is disconnected"));
        }
        let targets = if let Some(room) = room {
            self.room_targets(room).await?
        } else {
            self.recipients(peer).await?
        }
        .ok_or(Error::Unavailable("This contact has no OMEMO 2 devices"))?;
        if !message.fingerprint.is_empty()
            && self.local.as_mut().unwrap().fingerprint()? != message.fingerprint
        {
            return Err(Error::Changed);
        }
        let (root, plain) = self.clear_message(peer, message, quote, room.is_some())?;
        let encrypted =
            wire::encrypt(self.local.as_mut().unwrap(), &plain, &targets, false).await?;
        let mut outer = Element::builder("message", payload::CLIENT)
            .attr("from".try_into().unwrap(), &self.owner)
            .attr("to".try_into().unwrap(), peer)
            .attr(
                "type".try_into().unwrap(),
                if room.is_some() { "groupchat" } else { "chat" },
            )
            .attr("id".try_into().unwrap(), &message.wire_id)
            .append(encrypted)
            .build();
        for child in root.children().filter(|child| !payload::allowed(child)) {
            outer.append_child(child.clone());
        }
        if !outer
            .children()
            .any(|child| child.is("store", "urn:xmpp:hints"))
        {
            outer.append_child(Element::builder("store", "urn:xmpp:hints").build());
        }
        Ok(outer)
    }
    async fn send_wire(&self, xml: Element, room: Option<(&RoomRecipients, u64)>) -> Result<()> {
        let (reply, result) = oneshot::channel();
        let command = if let Some((recipients, epoch)) = room {
            let current = crate::rooms::members::fetch(
                &iq::Client::new(self.network.clone()),
                &recipients.room,
            )
            .await
            .map_err(|_| Error::Unavailable("Could not verify final room membership"))?;
            if current.affiliations != recipients.members {
                return Err(Error::Unavailable("Room membership changed before sending"));
            }
            Command::RoomWire {
                xml,
                peer: recipients.room.clone(),
                epoch,
                revision: recipients.revision,
                reply,
            }
        } else {
            Command::Wire { xml, reply }
        };
        self.network.send(command).map_err(|_| Error::Uncertain)?;
        if tokio::time::timeout(Duration::from_secs(20), result)
            .await
            .is_ok_and(|reply| reply.unwrap_or(false))
        {
            Ok(())
        } else {
            Err(Error::Uncertain)
        }
    }
    async fn incoming(&mut self, frame: Frame, room: Option<RoomContext>) -> Result<()> {
        self.local(false).await?;
        if room
            .as_ref()
            .is_some_and(|room| room.sender.author == self.owner)
            && frame
                .xml
                .get_child("encrypted", NS)
                .and_then(|encrypted| encrypted.get_child("header", NS))
                .and_then(|header| bundle::positive(header.attr("sid")).ok())
                == self.local.as_ref().map(|local| local.id)
        {
            return Ok(());
        }
        let (_, outer, _) = metadata(&self.owner, &frame, room.as_ref())
            .ok_or(Error::Invalid("PQ message route"))?;
        let (sender, destination) = if let Some(room) = &room {
            (room.sender.author.clone(), room.sender.room.clone())
        } else {
            payload::routing(&frame.xml)?
        };
        let stamp = chrono::DateTime::parse_from_rfc3339(&outer.timestamp)
            .map(|time| time.timestamp_millis())
            .unwrap_or_else(|_| chrono::Utc::now().timestamp_millis());
        let blind = self.blind(&sender)?;
        let received = wire::receive(
            self.local.as_mut().unwrap(),
            &frame.xml,
            &sender,
            &destination,
            stamp,
            Context {
                archive: frame.archive,
                historical: frame.historical,
                receipt_to: frame.receipt_to,
                room,
            },
            blind,
        )
        .await?;
        self.deliver(received).await
    }
    async fn deliver(&mut self, received: wire::Received) -> Result<()> {
        let frame = Frame {
            xml: received.outer.clone(),
            archive: received.context.archive.clone(),
            historical: received.context.historical,
            receipt_to: received.context.receipt_to.clone(),
        };
        if let Some(mut content) = received.content {
            let (peer, mut outer, historical) =
                metadata(&self.owner, &frame, received.context.room.as_ref())
                    .ok_or(Error::Invalid("PQ metadata"))?;
            let mut message = if let Some(room) = &received.context.room {
                content.set_attr("".into(), "type".try_into().unwrap(), "chat");
                content.set_attr(
                    "".into(),
                    "from".try_into().unwrap(),
                    room.sender.author.as_str(),
                );
                content.set_attr("".into(), "to".try_into().unwrap(), self.owner.as_str());
                let mut projected = protocol::normalize_protected(&content, &self.owner, None)
                    .ok_or(Error::Invalid("PQ room content"))?
                    .1;
                projected.id = outer.id.clone();
                projected.sender = outer.sender.clone();
                projected.author = outer.author.clone();
                projected.own = outer.own;
                projected.wire_id = outer.wire_id.clone();
                projected.aliases.clear();
                projected
            } else {
                protocol::normalize_protected(&content, &self.owner, None)
                    .ok_or(Error::Invalid("PQ content"))?
                    .1
            };
            message.set_encryption(crate::encryption_policy::Mode::Pq, received.fingerprint);
            message.markable_id = outer.markable_id.clone();
            message.unavailable = false;
            message.aliases.append(&mut outer.aliases);
            message
                .aliases
                .push(format!("pq:{}", received.token.digest()));
            message.timestamp = outer.timestamp;
            message.time = outer.time;
            self.emit(BackendEvent::PqEncrypted {
                peer,
                message,
                historical: received.context.historical || historical,
                receipt_to: received.context.receipt_to,
                token: received.token,
            });
        } else {
            self.accept(received.token).await?;
        }
        Ok(())
    }
    async fn accept(&mut self, token: Token) -> Result<()> {
        wire::accepted(self.local.as_mut().ok_or(Error::Incomplete)?, &token)?;
        if self.online
            && self.ready
            && (self.publication_pending || self.local.as_mut().unwrap().needs_replenish()?)
        {
            self.publication_pending = true;
            let bundle = Bundle::from_parts(self.local.as_mut().unwrap().prepare()?)?;
            self.pep()?.replenish(&bundle).await.map_err(pep_error)?;
            self.publication_pending = false;
        }
        Ok(())
    }
    async fn recover(&mut self) -> Result<()> {
        for received in wire::pending(self.local.as_mut().ok_or(Error::Incomplete)?)? {
            self.deliver(received).await?;
        }
        Ok(())
    }
    async fn retry(&mut self, peer: &str) {
        let Ok(Some(history)) = self.history() else {
            return;
        };
        let messages = history.history(&self.owner, peer, None, 150);
        for message in messages.into_iter().filter(|message| {
            message.unavailable
                && message.mode() == crate::encryption_policy::Mode::Pq
                && !message.ciphertext.is_empty()
        }) {
            let Ok(mut xml) = bundle::document(&message.ciphertext, wire::MAX_WIRE) else {
                continue;
            };
            if xml.get_child("delay", "urn:xmpp:delay").is_none() {
                xml.append_child(
                    Element::builder("delay", "urn:xmpp:delay")
                        .attr("stamp".try_into().unwrap(), message.timestamp)
                        .build(),
                );
            }
            let frame = Frame {
                xml,
                archive: None,
                historical: true,
                receipt_to: None,
            };
            let _ = self.incoming(frame, None).await;
        }
    }
    async fn process(&mut self, request: Request) {
        match request {
            Request::Online(online) => {
                self.online = online;
                self.bundles.clear();
                self.lists.clear();
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
                let result = self
                    .recipients(&peer)
                    .await
                    .map(|targets| targets.is_some());
                let _ = reply.send(result);
            }
            Request::ProbeRoom { recipients, reply } => {
                let result = self
                    .room_targets(&recipients)
                    .await
                    .map(|targets| targets.is_some());
                let _ = reply.send(result);
            }
            Request::Review(peer) | Request::RefreshTrust(peer) => {
                if peer == self.owner && self.ready {
                    match self.pep() {
                        Ok(pep) => {
                            if let Err(error) = pep.announce().await {
                                self.status(Some(describe(&pep_error(error))));
                            }
                        }
                        Err(error) => self.status(Some(describe(&error))),
                    }
                }
                self.bundles.retain(|(address, _), _| address != &peer);
                self.lists.remove(&peer);
                match self.discover(&peer).await {
                    Ok(devices) => {
                        self.emit(BackendEvent::PqDevices {
                            peer: peer.clone(),
                            devices,
                        });
                        self.retry(&peer).await;
                    }
                    Err(error) => self.emit(BackendEvent::CryptoError {
                        peer,
                        text: describe(&error),
                    }),
                }
            }
            Request::ReviewRoom { peer, members } => {
                let mut devices = Vec::new();
                let mut failed = None;
                for member in members {
                    match self.discover(&member).await {
                        Ok(found) => devices.extend(found),
                        Err(error) => {
                            failed = Some(error);
                            break;
                        }
                    }
                }
                if let Some(error) = failed {
                    self.emit(BackendEvent::CryptoError {
                        peer,
                        text: describe(&error),
                    });
                } else {
                    self.emit(BackendEvent::PqDevices { peer, devices });
                }
            }
            Request::Decide { keys, approve } => {
                for (peer, devices) in keys {
                    for (id, fingerprint) in devices {
                        let result = self
                            .local
                            .as_mut()
                            .ok_or(Error::Incomplete)
                            .and_then(|local| local.decide(&peer, id, &fingerprint, approve));
                        if let Err(error) = result {
                            self.emit(BackendEvent::CryptoError {
                                peer: peer.clone(),
                                text: describe(&error),
                            });
                        }
                    }
                    self.process_review(&peer).await;
                }
            }
            Request::Send {
                peer,
                message,
                quote,
            } => {
                let result = match self.encrypted(&peer, &message, &quote, None).await {
                    Ok(xml) => self.send_wire(xml, None).await,
                    Err(error) => Err(error),
                };
                self.sent(peer, message.wire_id, result);
            }
            Request::SendRoom {
                recipients,
                message,
                quote,
                epoch,
            } => {
                let peer = recipients.room.clone();
                let result = match self
                    .encrypted(&peer, &message, &quote, Some(&recipients))
                    .await
                {
                    Ok(xml) => self.send_wire(xml, Some((&recipients, epoch))).await,
                    Err(error) => Err(error),
                };
                self.sent(peer, message.wire_id, result);
            }
            Request::Incoming(frame) => {
                self.receive(frame, None).await;
            }
            Request::IncomingRoom {
                frame,
                sender,
                metadata,
            } => {
                self.receive(frame, Some(RoomContext { sender, metadata }))
                    .await;
            }
            Request::Preview { frame, room, reply } => {
                let result = self.incoming(frame, room).await;
                let _ = reply.send(
                    result.is_ok() || matches!(result, Err(Error::Invalid("PQ duplicate message"))),
                );
            }
            Request::Accepted(token) => {
                if let Err(error) = self.accept(token).await {
                    self.status(Some(describe(&error)));
                }
            }
        }
    }
    async fn process_review(&mut self, peer: &str) {
        match self.discover(peer).await {
            Ok(devices) => {
                self.emit(BackendEvent::PqDevices {
                    peer: peer.into(),
                    devices,
                });
                self.retry(peer).await;
            }
            Err(error) => self.emit(BackendEvent::CryptoError {
                peer: peer.into(),
                text: describe(&error),
            }),
        }
    }
    fn sent(&self, peer: String, id: String, result: Result<()>) {
        match result {
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
    async fn receive(&mut self, frame: Frame, room: Option<RoomContext>) {
        if let Err(error) = self.incoming(frame.clone(), room.clone()).await {
            if matches!(error, Error::Invalid("PQ duplicate message")) {
                return;
            }
            if let Some(event) = unavailable(&self.owner, &frame, room.as_ref(), &describe(&error))
            {
                self.emit(event);
            }
        }
    }
}

#[cfg(test)]
mod actor_tests {
    use super::*;
    use crate::{encryption_policy::Mode, model::Account, protocol::Session};
    use std::sync::mpsc::Receiver;
    fn wait(events: &Receiver<Envelope>, wanted: impl Fn(&BackendEvent) -> bool) -> BackendEvent {
        let deadline = Instant::now() + Duration::from_secs(35);
        loop {
            let (_, _, event) = events
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .expect("PQ actor event deadline");
            if let BackendEvent::Status {
                error: true, text, ..
            } = &event
            {
                panic!("Fixture connection failed: {text}");
            }
            if let BackendEvent::PqCrypto(status) = &event {
                eprintln!("PQ fixture actor: {} ready={}", status.text, status.ready);
            }
            if let BackendEvent::CryptoError { text, .. } = &event {
                eprintln!("PQ fixture diagnostic: {text}");
            }
            if wanted(&event) {
                return event;
            }
        }
    }
    fn start(name: &str) -> (Session, Receiver<Envelope>) {
        let (events, receive) = mpsc::channel();
        (
            Session::start(
                Account {
                    jid: format!("{name}@localhost"),
                    host: "localhost".into(),
                    port: std::env::var("MONOGRAM_IT_PORT").unwrap().parse().unwrap(),
                    direct_tls: false,
                },
                "fixture-password".into(),
                1,
                events,
            ),
            receive,
        )
    }
    fn ready(events: &Receiver<Envelope>) -> Status {
        match wait(
            events,
            |event| matches!(event, BackendEvent::PqCrypto(status) if status.ready),
        ) {
            BackendEvent::PqCrypto(status) => status,
            _ => unreachable!(),
        }
    }
    fn review(session: &Session, events: &Receiver<Envelope>, peer: &str) -> Vec<Review> {
        session.send(Command::PqCrypto(Request::Review(peer.into())));
        match wait(
            events,
            |event| matches!(event, BackendEvent::PqDevices { peer: who, .. } if who == peer),
        ) {
            BackendEvent::PqDevices { devices, .. } => devices,
            _ => unreachable!(),
        }
    }
    fn approve(session: &Session, rows: &[Review]) {
        let mut keys: BTreeMap<String, BTreeMap<u32, String>> = BTreeMap::new();
        for row in rows {
            keys.entry(row.peer.clone())
                .or_default()
                .insert(row.device, row.fingerprint.clone());
        }
        session.send(Command::PqCrypto(Request::Decide {
            keys,
            approve: true,
        }));
    }
    fn send(session: &Session, status: &Status, peer: &str, body: &str) -> String {
        let id = uuid::Uuid::new_v4().to_string();
        let mut message = Message {
            id: id.clone(),
            wire_id: id.clone(),
            sender: "alice@localhost".into(),
            author: "You".into(),
            own: true,
            text: body.into(),
            timestamp: chrono::Utc::now().to_rfc3339(),
            delivery: "sending".into(),
            ..Default::default()
        };
        message.set_encryption(Mode::Pq, status.fingerprint.clone());
        assert!(session.send(Command::Send {
            peer: peer.into(),
            message,
            quote: String::new()
        }));
        id
    }
    fn persist(
        session: &Session,
        events: &Receiver<Envelope>,
        owner: &str,
        peer: &str,
        body: &str,
    ) {
        let event = wait(
            events,
            |event| matches!(event, BackendEvent::PqEncrypted { message, .. } if message.text == body),
        );
        let BackendEvent::PqEncrypted {
            peer: address,
            message,
            token,
            ..
        } = event
        else {
            unreachable!()
        };
        assert_eq!(address, peer);
        assert_eq!(message.mode(), Mode::Pq);
        assert!(!message.unavailable);
        assert_eq!(message.fingerprint.len(), 128);
        let store = crate::storage::Store::open(false).unwrap();
        store
            .save_account(&Account {
                jid: owner.into(),
                host: "localhost".into(),
                port: std::env::var("MONOGRAM_IT_PORT").unwrap().parse().unwrap(),
                direct_tls: false,
            })
            .unwrap();
        let mut state = store.load_windowed(owner);
        store
            .save_chat_message(owner, state.chat_for(peer), Some(&message))
            .unwrap();
        assert!(store.message(owner, peer, &message.id).is_some());
        session.send(Command::PqCrypto(Request::Accepted(token)));
    }
    #[test]
    #[ignore = "Requires the private TLS Prosody and isolated keyring fixture"]
    fn real_tls_actor_pep_review_send_and_durable_acceptance() {
        assert_eq!(
            std::env::var("MONOGRAM_KEYRING_FIXTURE").as_deref(),
            Ok("1")
        );
        let _ = tokio_xmpp::rustls::crypto::aws_lc_rs::default_provider().install_default();
        let (alice, a) = start("alice");
        let (bob, b) = start("bob");
        let alice_status = ready(&a);
        let bob_status = ready(&b);
        let alice_rows = review(&alice, &a, "bob@localhost");
        let bob_rows = review(&bob, &b, "alice@localhost");
        assert_eq!(alice_rows.len(), 1);
        assert_eq!(bob_rows.len(), 1);
        assert_eq!(alice_rows[0].fingerprint, bob_status.fingerprint);
        assert_eq!(bob_rows[0].fingerprint, alice_status.fingerprint);
        approve(&alice, &alice_rows);
        approve(&bob, &bob_rows);
        let trusted_a = review(&alice, &a, "bob@localhost");
        let trusted_b = review(&bob, &b, "alice@localhost");
        assert!(trusted_a.iter().all(|row| row.trust == "trusted"));
        assert!(trusted_b.iter().all(|row| row.trust == "trusted"));
        let (reply, probe) = oneshot::channel();
        alice.send(Command::PqCrypto(Request::Probe {
            peer: "bob@localhost".into(),
            reply,
        }));
        assert!(probe.blocking_recv().unwrap().unwrap());
        for owner in ["alice@localhost", "bob@localhost"] {
            let path = lifecycle::root()
                .join("crypto")
                .join(lifecycle::hex(&openssl::sha::sha256(owner.as_bytes())))
                .join("pq/pq.sqlite3");
            let db = rusqlite::Connection::open_with_flags(
                path,
                rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
            )
            .unwrap();
            let count: u32 = db
                .query_row(
                    "SELECT count(*) FROM pq_records WHERE key LIKE 'session/%'",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(
                count, 0,
                "capability probe must not create a discarded session"
            );
        }
        if let Ok(path) = std::env::var("MONOGRAM_PQ_ACTOR_ARTIFACT") {
            let rows: Vec<_> = trusted_a.iter().map(|row| serde_json::json!({"peer":row.peer,"device":row.device,"fingerprint":row.fingerprint,"trust":row.trust})).collect();
            let public = serde_json::json!({"owner":"alice@localhost","peer":"bob@localhost","status":{"enabled":alice_status.enabled,"ready":alice_status.ready,"device":alice_status.device,"fingerprint":alice_status.fingerprint,"text":alice_status.text},"rows":rows});
            std::fs::write(path, serde_json::to_vec_pretty(&public).unwrap()).unwrap();
        }
        send(
            &alice,
            &alice_status,
            "bob@localhost",
            "Real PQ actor over TLS",
        );
        persist(
            &bob,
            &b,
            "bob@localhost",
            "alice@localhost",
            "Real PQ actor over TLS",
        );
        send(&bob, &bob_status, "alice@localhost", "PQ actor reply");
        persist(
            &alice,
            &a,
            "alice@localhost",
            "bob@localhost",
            "PQ actor reply",
        );
        drop(alice);
        drop(bob);
    }
}
