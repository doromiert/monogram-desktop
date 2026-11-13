//! Legacy OMEMO runs independently of the selected outgoing version.
use super::{Legacy, MAX_WIRE, NS, exists, probe_unavailable};
use crate::{
    crypto::{
        bundle,
        journal::{Context, Received, RoomContext, Token},
        lifecycle, payload,
        service::{Frame, Review, Status},
        *,
    },
    encryption_policy::Mode,
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
    time::Duration,
};
use tokio::sync::{mpsc as channel, oneshot};
use tokio_xmpp::{jid::Jid, minidom::Element};

const REPLY: &str = "urn:xmpp:reply:0";
const FALLBACK: &str = "urn:xmpp:fallback:0";

#[derive(Clone, Debug)]
pub struct LegacyReply {
    pub id: String,
    pub to: String,
    pub quote: String,
    pub text: String,
}
impl LegacyReply {
    fn parse(outer: &Element, body: &str, owner: &str, peer: &str, room: bool) -> Option<Self> {
        let mut replies = outer.children().filter(|child| child.is("reply", REPLY));
        let reply = replies.next()?;
        let id = reply
            .attr("id")
            .filter(|id| crate::message_actions::valid_id(id))?;
        if replies.next().is_some() || !empty(reply) {
            return None;
        }
        let to = if let Some(value) = reply.attr("to") {
            let address = value.parse::<Jid>().ok()?;
            let author = address.to_bare().to_string();
            if address.node().is_none()
                || (!room && author != owner && author != peer)
                || (room && address.resource().is_some() && author != peer)
            {
                return None;
            }
            if room { address.to_string() } else { author }
        } else {
            String::new()
        };
        let (quote, text) =
            split_fallback(outer, body).unwrap_or_else(|| (String::new(), body.into()));
        Some(Self {
            id: id.into(),
            to,
            quote,
            text,
        })
    }
}

fn empty(element: &Element) -> bool {
    element.children().next().is_none() && element.text().trim().is_empty()
}
fn reply_fallback(element: &Element) -> bool {
    element.is("fallback", FALLBACK) && element.attr("for") == Some(REPLY)
}
fn recovered_retraction(
    outer: &Element,
    stored: &Message,
    metadata: &Message,
) -> Result<Option<Message>> {
    let Some(action @ crate::message_actions::Action::Retract { .. }) =
        crate::message_actions::parse(outer).map_err(|_| Error::Invalid("legacy retraction"))?
    else {
        return Ok(None);
    };
    if stored.mode() != Mode::Legacy
        || stored.unavailable
        || stored.fingerprint.is_empty()
        || stored.own != metadata.own
        || stored.sender.split('/').next() != metadata.sender.split('/').next()
    {
        return Ok(None);
    }
    let mut message = Message {
        action: Some(action),
        id: stored.id.clone(),
        wire_id: stored.wire_id.clone(),
        sender: stored.sender.clone(),
        own: stored.own,
        timestamp: stored.timestamp.clone(),
        delivery: stored.delivery.clone(),
        aliases: stored.aliases.clone(),
        ..Default::default()
    };
    message.aliases.extend(metadata.aliases.clone());
    message.set_encryption(Mode::Legacy, stored.fingerprint.clone());
    Ok(Some(message))
}
fn preview_spoilers(
    outer: &Element,
    raw: &str,
    visible: &str,
    owner: &str,
    peer: &str,
    room: bool,
) -> Option<crate::spoilers::Context> {
    let mut content = Element::builder("message", payload::CLIENT)
        .append(
            Element::builder("body", payload::CLIENT)
                .append(raw)
                .build(),
        )
        .build();
    if let Some(reply) = LegacyReply::parse(outer, raw, owner, peer, room) {
        content.append_child(outer.get_child("reply", REPLY)?.clone());
        if !reply.quote.is_empty() {
            content.append_child(
                outer
                    .children()
                    .find(|child| reply_fallback(child))?
                    .clone(),
            );
        }
    }
    crate::spoilers::append_legacy_hints(outer, &mut content);
    crate::spoilers::normalized(&content, visible)
}
fn outgoing_content(peer: &str, message: &Message, quote: &str) -> Result<Element> {
    if message.mode() != Mode::Legacy {
        return Err(Error::Invalid("wrong outgoing Legacy OMEMO mode"));
    }
    if message.upload.is_some()
        || message.poll.is_some()
        || message
            .action
            .as_ref()
            .is_some_and(|action| !matches!(action, crate::message_actions::Action::Retract { .. }))
        || message.action.is_some() && message.attachment.is_some()
    {
        return Err(Error::Unavailable(
            "This message cannot be sent with Legacy OMEMO",
        ));
    }
    if let Some(file) = &message.attachment {
        if !file.valid_outgoing(true) {
            return Err(Error::Invalid(
                "Legacy OMEMO attachment must be an encrypted file link",
            ));
        }
        // XEP-0454 mobile receivers require URI-only content; captions are separate messages.
        let mut content = message.clone();
        content.text = file.wire_url.clone();
        content.attachment = None;
        content.forwarded = None;
        Ok(protocol::outgoing(peer, &content, quote))
    } else {
        Ok(protocol::outgoing(peer, message, quote))
    }
}
fn decorate_outgoing(xml: &mut Element, clear: &Element, message: &Message) {
    if let Some(action @ crate::message_actions::Action::Retract { .. }) = &message.action {
        crate::message_actions::decorate(xml, action);
    }
    for child in clear
        .children()
        .filter(|child| child.is("reply", REPLY) || reply_fallback(child))
    {
        xml.append_child(child.clone());
    }
    if message.attachment.as_ref().is_some_and(|file| file.sticker) {
        xml.append_child(Element::builder("sticker", "urn:xmpp:stickers:0").build());
    }
    for child in clear.children().filter(|c|c.is("spoiler",crate::formatting::SPOILER) || c.is("markup",crate::formatting::NS) || c.is("content",crate::formatting::CONTENT)){xml.append_child(child.clone());}
    crate::read_markers::decorate(xml, message);
}
fn split_fallback(outer: &Element, body: &str) -> Option<(String, String)> {
    // Legacy OMEMO authenticates the body, not the fallback range. Only move the
    // bounded, leading quote emitted by both clients into the reply preview.
    let mut fallbacks = outer.children().filter(|child| reply_fallback(child));
    let fallback = fallbacks.next()?;
    let mut children = fallback.children();
    let range = children.next()?;
    if fallbacks.next().is_some()
        || children.next().is_some()
        || !range.is("body", FALLBACK)
        || !empty(range)
        || range
            .attr_ns("http://www.w3.org/XML/1998/namespace", "lang")
            .is_some()
        || range.attr("start") != Some("0")
    {
        return None;
    }
    let end = range.attr("end")?.parse::<usize>().ok()?;
    if !(1..=2048).contains(&end) {
        return None;
    }
    let (prefix, text) = body.split_at(body.char_indices().nth(end)?.0);
    let quote = prefix
        .strip_suffix('\n')?
        .split('\n')
        .map(|line| line.strip_prefix("> "))
        .collect::<Option<Vec<_>>>()?
        .join("\n");
    (!quote.is_empty() && !text.is_empty()).then(|| (quote, text.into()))
}

#[derive(Debug)]
pub enum Request {
    Online(bool),
    Enable,
    Review(String),
    ReviewRoom {
        peer: String,
        members: BTreeSet<String>,
    },
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
    ProbeRoom {
        recipients: RoomRecipients,
        reply: oneshot::Sender<Result<bool>>,
    },
    SendRoom {
        recipients: RoomRecipients,
        message: Message,
        quote: String,
        epoch: u64,
    },
    Incoming(Frame),
    Preview {
        frame: Frame,
        room: Option<RoomContext>,
        reply: oneshot::Sender<bool>,
    },
    IncomingRoom {
        frame: Frame,
        sender: RoomSender,
        metadata: Message,
    },
    Accepted(Token),
    RepairComplete {
        repair: super::Repair,
        bundle: Result<bundle::Bundle>,
    },
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
    pub fn start(
        owner: String,
        generation: u64,
        network: channel::UnboundedSender<Command>,
        events: mpsc::Sender<Envelope>,
    ) -> Self {
        let (tx, mut requests) = channel::unbounded_channel();
        let (incoming, mut frames) = channel::channel(32);
        let (previews, mut preview_frames) = channel::channel(150);
        let (stop, stopped) = oneshot::channel();
        let handle = Self {
            tx,
            incoming,
            previews,
            stop: Some(stop),
            owner: owner.clone(),
            generation,
            events: events.clone(),
        };
        let repair_tx = handle.tx.downgrade();
        thread::Builder::new().name("monogram-legacy-omemo".into()).spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().expect("Legacy OMEMO runtime");
            let mut worker = Worker { owner, generation, network, events, root: lifecycle::root(), local: None, online: false, ready: false, repair_tx: Some(repair_tx) };
            runtime.block_on(async move {
                tokio::select! {
                    _ = stopped => {},
                    _ = async {
                        loop {
                            tokio::select! {
                                biased;
                                next = requests.recv() => match next { Some(request) => worker.process(request).await, None => break },
                                next = frames.recv() => match next { Some(request) => worker.process(request).await, None => break },
                                next = preview_frames.recv() => match next { Some(request) => worker.process(request).await, None => break },
                            }
                        }
                    } => {},
                }
            });
        }).expect("Legacy OMEMO worker");
        handle
    }
    pub fn preview(
        &self,
        frame: Frame,
        room: Option<RoomContext>,
    ) -> Option<oneshot::Receiver<bool>> {
        if bundle::xml(&frame.xml, MAX_WIRE).is_err() {
            return None;
        }
        let (reply, received) = oneshot::channel();
        self.previews
            .try_send(Request::Preview { frame, room, reply })
            .ok()?;
        Some(received)
    }
    pub fn send(&self, request: Request) {
        if matches!(request, Request::Preview { .. }) {
            let _ = self.previews.try_send(request);
            return;
        }
        let frame = match &request {
            Request::Incoming(frame) | Request::IncomingRoom { frame, .. } => Some(frame),
            _ => None,
        };
        if let Some(frame) = frame {
            if bundle::xml(&frame.xml, MAX_WIRE).is_err() {
                return;
            }
            if let Err(error) = self.incoming.try_send(request) {
                let request = error.into_inner();
                let (frame, room) = match request {
                    Request::Incoming(frame) => (frame, None),
                    Request::IncomingRoom {
                        frame,
                        sender,
                        metadata,
                    } => (frame, Some(RoomContext { sender, metadata })),
                    _ => unreachable!(),
                };
                if let Some(event) = unavailable(
                    &self.owner,
                    &frame,
                    "Legacy OMEMO is busy. Reconnect or refresh fingerprints to retry.",
                    None,
                    room.as_ref(),
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
    repair_tx: Option<channel::WeakUnboundedSender<Request>>,
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
    room: Option<&RoomContext>,
) -> Option<BackendEvent> {
    frame
        .xml
        .get_child("encrypted", NS)?
        .get_child("payload", NS)?;
    let (peer, mut message, historical) = outer_metadata(owner, frame, room)?;
    message.text = text.into();
    message.ciphertext = String::from_utf8(bundle::xml(&frame.xml, MAX_WIRE).ok()?).ok()?;
    message.set_encryption(Mode::Legacy, String::new());
    message.unavailable = true;
    message.legacy_room_sender = room.map(|room| room.sender.clone());
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
fn outer_metadata(
    owner: &str,
    frame: &Frame,
    room: Option<&RoomContext>,
) -> Option<(String, Message, bool)> {
    let Some(room) = room else {
        return protocol::normalize(&frame.xml, owner, archive(frame));
    };
    let sender = &room.sender;
    let address = sender.occupant.parse::<Jid>().ok()?;
    if !frame.xml.is("message", payload::CLIENT)
        || frame.xml.attr("type") != Some("groupchat")
        || frame.xml.attr("from") != Some(sender.occupant.as_str())
        || address.resource().is_none()
        || address.to_bare().as_str() != sender.room
        || protocol::bare(&sender.author).ok().as_deref() != Some(sender.author.as_str())
        || frame.xml.attr("to").is_some_and(|to| {
            to.parse::<Jid>()
                .ok()
                .is_none_or(|jid| jid.to_bare().as_str() != owner)
        })
    {
        return None;
    }
    let mut metadata = room.metadata.clone();
    metadata.sender = sender.occupant.clone();
    metadata.own = sender.author == owner;
    metadata.author = if metadata.own {
        "You".into()
    } else {
        address.resource()?.as_str().into()
    };
    metadata.legacy_room_sender = Some(sender.clone());
    Some((
        sender.room.clone(),
        metadata,
        frame.historical || frame.archive.is_some(),
    ))
}
fn describe(error: &Error) -> String {
    match error {
        Error::Invalid(_) => "Invalid Legacy OMEMO message or key data".into(),
        Error::Native(-1003) => {
            "This Legacy OMEMO message refers to a key that is no longer available".into()
        }
        Error::Native(_) => "Could not open or encrypt the Legacy OMEMO message".into(),
        Error::Storage(_) => "Could not save Legacy OMEMO state".into(),
        Error::Crypto(_) => "Legacy OMEMO message authentication failed".into(),
        Error::Incomplete => {
            "Legacy OMEMO state is incomplete. The existing identity was preserved.".into()
        }
        _ => error.to_string(),
    }
}
fn repair_candidate(frame: &Frame, error: &Error) -> bool {
    matches!(error, Error::Native(-1003))
        && !frame.historical
        && frame.archive.is_none()
        && frame.xml.get_child("delay", "urn:xmpp:delay").is_none()
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
        let peers = BTreeSet::from([peer.to_string(), self.owner.clone()]);
        self.review_members(peer, peers).await;
    }
    async fn review_members(&mut self, peer: &str, mut peers: BTreeSet<String>) {
        peers.insert(self.owner.clone());
        let mut rows = vec![];
        let mut errors = vec![];
        for address in peers {
            let result = match self.local.as_mut().filter(|_| self.ready) {
                Some(local) => local.discover_for(&address, Some(peer)).await,
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
        let clear = outgoing_content(peer, message, quote)?;
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
        let body = clear
            .get_child("body", payload::CLIENT)
            .ok_or(Error::Invalid("legacy body"))?
            .text();
        let mut xml = local.encrypt(peer, &body, &message.wire_id).await?;
        decorate_outgoing(&mut xml, &clear, message);
        xml.append_child(protocol::el("request", "urn:xmpp:receipts"));
        if self.wire(xml).await {
            Ok(())
        } else {
            Err(Error::Uncertain)
        }
    }
    async fn probe_room(&mut self, recipients: &RoomRecipients) -> Result<bool> {
        if !self.ready {
            self.initialize().await?;
        }
        let members = recipients.members.keys().cloned().collect();
        match self
            .local
            .as_mut()
            .ok_or(Error::Incomplete)?
            .encrypt_room(
                &recipients.room,
                &members,
                "Monogram encryption check",
                &crate::model::new_id(),
            )
            .await
        {
            Ok(_) => Ok(true),
            Err(Error::Unavailable(super::NO_DEVICES | super::UNSUPPORTED)) => Ok(false),
            Err(error) => Err(error),
        }
    }
    async fn send_room(
        &mut self,
        recipients: &RoomRecipients,
        message: &Message,
        quote: &str,
        epoch: u64,
    ) -> Result<()> {
        let clear = outgoing_content(&recipients.room, message, quote)?;
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
        let body = clear
            .get_child("body", payload::CLIENT)
            .ok_or(Error::Invalid("legacy room body"))?
            .text();
        let members = recipients.members.keys().cloned().collect();
        let mut xml = local
            .encrypt_room(&recipients.room, &members, &body, &message.wire_id)
            .await?;
        decorate_outgoing(&mut xml, &clear, message);
        let current =
            crate::rooms::members::fetch(&iq::Client::new(self.network.clone()), &recipients.room)
                .await
                .map_err(|_| {
                    Error::Unavailable(
                        "Could not verify final room membership; the message was not sent",
                    )
                })?;
        if current.affiliations != recipients.members {
            return Err(Error::Unavailable(
                "Room membership changed while encrypting; retry after reviewing its members",
            ));
        }
        let (reply, received) = oneshot::channel();
        self.network
            .send(Command::RoomWire {
                xml,
                peer: recipients.room.clone(),
                epoch,
                revision: recipients.revision,
                reply,
            })
            .map_err(|_| {
                Error::Unavailable(
                    "The connection closed before sending the encrypted room message",
                )
            })?;
        if tokio::time::timeout(Duration::from_secs(20), received)
            .await
            .is_ok_and(|value| value.unwrap_or(false))
        {
            Ok(())
        } else {
            Err(Error::Uncertain)
        }
    }
    fn digest(&self, frame: &Frame, room: Option<&RoomContext>) -> Result<[u8; 32]> {
        let local = self.local.as_ref().ok_or(Error::Incomplete)?;
        if let Some(room) = room {
            local.digest_room(&frame.xml, &room.sender)
        } else {
            local.digest(&frame.xml)
        }
    }
    async fn incoming(&mut self, frame: &Frame, room: Option<&RoomContext>) -> Result<()> {
        self.incoming_mode(frame, room, false).await
    }
    async fn incoming_mode(
        &mut self,
        frame: &Frame,
        room: Option<&RoomContext>,
        preview: bool,
    ) -> Result<()> {
        if !self.ready && !preview {
            return Err(Error::Unavailable("Legacy OMEMO is not ready"));
        }
        let outer = outer_metadata(&self.owner, frame, room);
        if preview
            && let Some((peer, metadata, _)) = &outer
            && metadata.own
            && let Some(local) = self.local.as_mut()
            && frame
                .xml
                .get_child("encrypted", NS)
                .and_then(|encrypted| encrypted.get_child("header", NS))
                .and_then(|header| header.attr("sid"))
                .and_then(|sid| sid.parse::<u32>().ok())
                == Some(local.device())
        {
            let fingerprint = lifecycle::hex(&local.fingerprint()?);
            if let Some(history) = self.history()?
                && let Some(id) = history.known(&self.owner, peer, true, &metadata.aliases)
                && let Some(stored) = history.message(&self.owner, peer, &id).filter(|stored| {
                    stored.own
                        && !stored.unavailable
                        && !stored.retracted
                        && stored.mode() == Mode::Legacy
                        && (stored.sender.split('/').next() == Some(self.owner.as_str())
                            || stored.legacy_room_sender.as_ref().is_some_and(|sender| {
                                sender.author == self.owner && sender.room == *peer
                            }))
                        && stored.fingerprint == fingerprint
                        && stored.wire_id == metadata.wire_id
                        && matches!(stored.delivery.as_str(), "sent" | "delivered" | "read")
                })
                && stored.preview_spoilers.is_none()
                && let Some(context) = crate::spoilers::cached_legacy(&frame.xml, &stored)
            {
                self.emit(BackendEvent::PreviewSpoilers {
                    peer: peer.clone(),
                    id,
                    context,
                });
            }
            return Ok(());
        }
        let digest = self.digest(frame, room)?;
        let alias = alias(digest);
        if let Some((peer, message, _)) = &outer {
            if let Some(history) = self.history()? {
                if let Some(id) = history.known(&self.owner, peer, message.own, &[alias.clone()]) {
                    if let Some(stored) = history
                        .message(&self.owner, peer, &id)
                        .filter(|m| !m.unavailable)
                    {
                        if room.is_none()
                            && let Some(retraction) =
                                recovered_retraction(&frame.xml, &stored, message)?
                        {
                            self.emit(BackendEvent::Message {
                                peer: peer.clone(),
                                message: retraction,
                                historical: true,
                                receipt_to: None,
                            });
                        }
                        let legacy_reply = if stored.mode() == Mode::Legacy
                            && stored.reply_id.is_empty()
                            && stored.kind == "text"
                            && !stored.edited
                            && !stored.retracted
                        {
                            LegacyReply::parse(
                                &frame.xml,
                                &stored.text,
                                &self.owner,
                                peer,
                                room.is_some(),
                            )
                        } else {
                            None
                        };
                        let local = self.local.as_mut().unwrap();
                        let saved = local
                            .store
                            .transaction(|tx| local.journal.find(tx, &digest))?;
                        let context = if stored.preview_spoilers.is_none()
                            && stored.mode() == Mode::Legacy
                            && !stored.fingerprint.is_empty()
                        {
                            let mut visible = stored.clone();
                            if let Some(reply) = &legacy_reply {
                                visible.text = reply.text.clone();
                                visible.reply_id = reply.id.clone();
                                visible.hydrate_compatibility();
                            }
                            saved
                                .as_ref()
                                .and_then(|saved| {
                                    saved
                                        .content
                                        .as_ref()
                                        .and_then(|content| {
                                            content.get_child("body", payload::CLIENT)
                                        })
                                        .map(Element::text)
                                        .and_then(|raw| {
                                            preview_spoilers(
                                                saved.outer.as_ref().unwrap_or(&frame.xml),
                                                &raw,
                                                &visible.text,
                                                &self.owner,
                                                peer,
                                                room.is_some(),
                                            )
                                        })
                                })
                                .or_else(|| {
                                    if legacy_reply.is_some() {
                                        preview_spoilers(
                                            &frame.xml,
                                            &stored.text,
                                            &visible.text,
                                            &self.owner,
                                            peer,
                                            room.is_some(),
                                        )
                                    } else {
                                        crate::spoilers::cached_legacy(&frame.xml, &stored)
                                    }
                                })
                        } else {
                            None
                        };
                        self.emit(BackendEvent::CryptoAliases {
                            markable_id: message.markable_id.clone(),
                            legacy_reply,
                            legacy_sticker: crate::file_share::Attachment::sticker_marker(
                                &frame.xml,
                            ),
                            peer: peer.clone(),
                            own: message.own,
                            id: id.clone(),
                            aliases: message.aliases.clone(),
                        });
                        if let Some(context) = context {
                            self.emit(BackendEvent::PreviewSpoilers {
                                peer: peer.clone(),
                                id,
                                context,
                            });
                        }
                        if let Some(token) = saved.and_then(|saved| saved.token) {
                            self.accept(token).await?;
                        }
                        return Ok(());
                    }
                }
            }
        }
        if preview {
            return Ok(());
        }
        let stamp = outer
            .as_ref()
            .and_then(|(_, message, _)| {
                chrono::DateTime::parse_from_rfc3339(&message.timestamp).ok()
            })
            .map(|stamp| stamp.timestamp_millis())
            .unwrap_or_else(|| chrono::Utc::now().timestamp_millis());
        let context = Some(Context {
            archive: frame.archive.clone(),
            historical: frame.historical,
            receipt_to: if room.is_some() {
                None
            } else {
                frame.receipt_to.clone()
            },
            room: room.cloned(),
        });
        let received = if let Some(room) = room {
            self.local
                .as_mut()
                .unwrap()
                .receive_room(&frame.xml, &room.sender, stamp, context)
                .await?
        } else {
            self.local
                .as_mut()
                .unwrap()
                .receive(&frame.xml, stamp, context)
                .await?
        };
        self.deliver(received, Some(frame), alias).await
    }
    async fn deliver(
        &mut self,
        received: Received,
        frame: Option<&Frame>,
        alias: String,
    ) -> Result<()> {
        if let Some(mut content) = received.content {
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
            let room = received
                .context
                .as_ref()
                .and_then(|context| context.room.as_ref());
            let metadata_frame = Frame {
                xml: outer.clone(),
                archive: info.cloned(),
                historical: frame.map(|frame| frame.historical).unwrap_or_else(|| {
                    received
                        .context
                        .as_ref()
                        .is_none_or(|context| context.historical)
                }),
                receipt_to: None,
            };
            let (peer, metadata, historical) = outer_metadata(&self.owner, &metadata_frame, room)
                .ok_or(Error::Invalid("legacy envelope metadata"))?;
            // Mobile Legacy OMEMO encrypts the body and keeps XEP-0424 outside it.
            // Projection still requires the original author's exact protocol and fingerprint.
            if room.is_none()
                && let Some(action @ crate::message_actions::Action::Retract { .. }) =
                    crate::message_actions::parse(outer)
                        .map_err(|_| Error::Invalid("legacy retraction"))?
            {
                crate::message_actions::decorate(&mut content, &action);
            }
            crate::spoilers::append_legacy_hints(outer, &mut content);
            let reply = content.get_child("body", payload::CLIENT).and_then(|body| {
                LegacyReply::parse(outer, &body.text(), &self.owner, &peer, room.is_some())
            });
            if let Some(reply) = &reply {
                content.append_child(outer.get_child("reply", REPLY).unwrap().clone());
                if !reply.quote.is_empty() {
                    content.append_child(
                        outer
                            .children()
                            .find(|child| reply_fallback(child))
                            .unwrap()
                            .clone(),
                    );
                }
            }
            if let Some(room) = room {
                content.set_attr("".into(), "type".try_into().unwrap(), "chat");
                content.set_attr(
                    "".into(),
                    "from".try_into().unwrap(),
                    room.sender.author.as_str(),
                );
                content.set_attr("".into(), "to".try_into().unwrap(), self.owner.as_str());
            }
            let (_, mut message, _) = protocol::normalize_protected(&content, &self.owner, None)
                .ok_or(Error::Invalid("legacy body"))?;
            if room.is_none() {
                message.markable_id = metadata.markable_id.clone();
            }
            if room.is_some() {
                message.id = metadata.id.clone();
                message.sender = metadata.sender.clone();
                message.author = metadata.author.clone();
                message.own = metadata.own;
                message.wire_id = metadata.wire_id.clone();
                message.legacy_room_sender = metadata.legacy_room_sender.clone();
                message.aliases.clear();
            }
            if let Some(reply) = reply {
                message.reply_id = reply.id;
                message.reply_to = reply.to;
                message.reply = reply.quote;
            }
            if let Some(file) = message.attachment.as_mut()
                && file.link().is_some_and(|link| link.encrypted())
                && crate::file_share::Attachment::sticker_marker(outer)
            {
                file.sticker = true;
            }
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
            let receipt_to = if room.is_some() {
                None
            } else {
                frame.and_then(|f| f.receipt_to.clone()).or_else(|| {
                    received
                        .context
                        .as_ref()
                        .and_then(|ctx| ctx.receipt_to.clone())
                })
            };
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
                .and_then(|outer| {
                    let local = self.local.as_ref().unwrap();
                    if let Some(room) = pending
                        .context
                        .as_ref()
                        .and_then(|context| context.room.as_ref())
                    {
                        local.digest_room(outer, &room.sender).ok()
                    } else {
                        local.digest(outer).ok()
                    }
                })
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
                        message.timestamp.clone(),
                    ));
                }
                let frame = Frame {
                    xml,
                    archive: None,
                    historical: true,
                    receipt_to: None,
                };
                let room = message.legacy_room_sender.as_ref().map(|sender| {
                    let mut metadata = message.clone();
                    metadata.ciphertext.clear();
                    RoomContext {
                        sender: sender.clone(),
                        metadata,
                    }
                });
                let _ =
                    tokio::time::timeout_at(deadline, self.incoming(&frame, room.as_ref())).await;
            }
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
    async fn receive_request(&mut self, frame: Frame, room: Option<RoomContext>) {
        if let Err(error) = self.incoming(&frame, room.as_ref()).await {
            let digest = self.digest(&frame, room.as_ref()).ok();
            if let Some(event) = unavailable(
                &self.owner,
                &frame,
                &describe(&error),
                digest,
                room.as_ref(),
            ) {
                self.emit(event);
            }
            if self.online && self.ready && repair_candidate(&frame, &error) {
                self.repair(&frame.xml, room.as_ref());
            }
        }
    }

    fn repair(&mut self, outer: &Element, room: Option<&RoomContext>) {
        let Some(reply) = self.repair_tx.as_ref().and_then(|tx| tx.upgrade()) else {
            return;
        };
        let Some(local) = self.local.as_mut() else {
            return;
        };
        let Ok(Some(repair)) = local.begin_repair(outer, room.map(|context| &context.sender))
        else {
            return;
        };
        let Ok(discovery) = crate::crypto::pep::Pep::new(
            iq::Client::new(self.network.clone()),
            &self.owner,
            local.device(),
        ) else {
            return;
        };
        let peer = repair.peer.clone();
        let own = peer == self.owner;
        tokio::spawn(async move {
            let bundle = async {
                let data = discovery
                    .item(&peer, &super::bundle_node(repair.device), None)
                    .await
                    .map_err(|error| super::lookup(error, own))?
                    .ok_or(Error::InvalidBundle)?;
                super::wire::parse_bundle(&data)
            }
            .await;
            let _ = reply.send(Request::RepairComplete { repair, bundle });
        });
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
                    if let Some(local) = self.local.as_mut() {
                        local.cancel_repairs();
                    }
                    self.ready = false;
                    self.status(None);
                }
            }
            Request::RepairComplete { repair, bundle } => {
                let Some(local) = self.local.as_mut() else {
                    return;
                };
                if !self.online || !self.ready {
                    local.cancel_repairs();
                    return;
                }
                if let Ok(Some(token)) = local.complete_repair(repair, bundle) {
                    let _ = self.accept(token).await;
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
                let result = self.send_message(&peer, &message, &quote).await;
                self.sent(peer, id, result);
            }
            Request::SendRoom {
                recipients,
                message,
                quote,
                epoch,
            } => {
                let id = message.wire_id.clone();
                let result = self.send_room(&recipients, &message, &quote, epoch).await;
                self.sent(recipients.room, id, result);
            }
            Request::ProbeRoom { recipients, reply } => {
                if !reply.is_closed() {
                    let _ = reply.send(self.probe_room(&recipients).await);
                }
            }
            Request::ReviewRoom { peer, members } => {
                self.review_members(&peer, members).await;
                self.retry().await;
            }
            Request::Incoming(frame) => self.receive_request(frame, None).await,
            Request::IncomingRoom {
                frame,
                sender,
                mut metadata,
            } => {
                metadata.ciphertext.clear();
                metadata.legacy_room_sender = Some(sender.clone());
                self.receive_request(frame, Some(RoomContext { sender, metadata }))
                    .await;
            }
            Request::Preview { frame, room, reply } => {
                let result = self.incoming_mode(&frame, room.as_ref(), true).await;
                let ok = matches!(
                    result,
                    Ok(())
                        | Err(Error::Unavailable(
                            "Message was encrypted by this device"
                                | "Encrypted message does not include this device"
                        ))
                );
                let _ = reply.send(ok);
            }
            Request::Accepted(token) => {
                if let Err(error) = self.accept(token).await {
                    self.status(Some(describe(&error)));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mobile_legacy_retraction_send_receive_and_cached_mam_repair() {
        use super::super::{
            encrypt_for, receive_for,
            tests::{approve, device, fixture},
            wire::Envelope,
        };
        use crate::message_actions::{self as actions, Action};
        let owner = "owner@example.org";
        let peer = "peer@example.org";
        let (mut sender, _sender_journal, sender_bundle) = fixture(peer, 101);
        let (mut receiver, receiver_journal, receiver_bundle) = fixture(owner, 202);
        approve(&mut sender, owner, 202, &receiver_bundle);
        approve(&mut receiver, peer, 101, &sender_bundle);
        let fingerprint = lifecycle::hex(&sender_bundle.identity);
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        for (metadata, legacy) in [
            (
                "<retract xmlns='urn:xmpp:message-retract:1' id='original'/><fallback xmlns='urn:xmpp:fallback:0' for='urn:xmpp:message-retract:1'/>",
                false,
            ),
            (
                "<apply-to xmlns='urn:xmpp:fasten:0' id='original'><retract xmlns='urn:xmpp:message-retract:0'/></apply-to><fallback xmlns='urn:xmpp:fallback:0' for='urn:xmpp:message-retract:0'/>",
                true,
            ),
        ] {
            // Literal metadata matches mobile MessageRetraction, independently of our serializer.
            let mut outer = encrypt_for(
                &mut sender,
                owner,
                "A previous message was retracted.",
                "control",
                &[(
                    device(owner, 202, &receiver_bundle),
                    receiver_bundle.clone(),
                )],
            )
            .unwrap();
            let literal: Element = format!("<message xmlns='jabber:client'>{metadata}</message>")
                .parse()
                .unwrap();
            for child in literal.children() {
                outer.append_child(child.clone());
            }
            let envelope = Envelope::parse(&outer, owner, 202).unwrap();
            let authenticated_alias = alias(envelope.digest);
            let received = receive_for(
                &mut receiver,
                &receiver_journal,
                &outer,
                &envelope,
                1794355200000,
                None,
            )
            .unwrap();
            let (events, result) = mpsc::channel();
            let (network, _requests) = channel::unbounded_channel();
            let mut worker = Worker {
                owner: owner.into(),
                generation: 1,
                network,
                events,
                root: PathBuf::new(),
                local: None,
                online: false,
                ready: false,
                repair_tx: None,
            };
            runtime
                .block_on(worker.deliver(received, None, authenticated_alias))
                .unwrap();
            let (
                _,
                _,
                BackendEvent::LegacyEncryptedMessage {
                    message: control, ..
                },
            ) = result.recv().unwrap()
            else {
                panic!("missing decrypted control")
            };
            assert!(control.text.is_empty());
            assert_eq!(
                control.action,
                Some(Action::Retract {
                    target: "original".into(),
                    legacy
                })
            );
            let clear = outgoing_content(owner, &control, "").unwrap();
            assert_eq!(
                clear.get_child("body", payload::CLIENT).unwrap().text(),
                "A previous message was retracted."
            );
            let mut outgoing = Element::builder("message", payload::CLIENT)
                .append(Element::builder("encrypted", NS).build())
                .build();
            decorate_outgoing(&mut outgoing, &clear, &control);
            assert_eq!(actions::parse(&outgoing).unwrap(), control.action);
            assert!(outgoing.get_child("body", payload::CLIENT).is_none());
            let db = crate::storage::Store::open(true).unwrap();
            db.save_account(&crate::model::Account {
                jid: owner.into(),
                host: String::new(),
                port: 5222,
                direct_tls: false,
            })
            .unwrap();
            let mut original = Message {
                id: "base".into(),
                wire_id: "original".into(),
                aliases: vec!["wire:original".into(), "origin:original".into()],
                sender: format!("{peer}/mobile"),
                text: "private original".into(),
                ..Default::default()
            };
            original.set_encryption(Mode::Legacy, "different-original-device".into());
            let chat = crate::model::Chat {
                jid: peer.into(),
                ..Default::default()
            };
            db.save_chat_message(owner, &chat, Some(&original)).unwrap();
            let mut fallback = control.clone();
            fallback.action = None;
            fallback.text = "A previous message was retracted.".into();
            db.save_chat_message(owner, &chat, Some(&fallback)).unwrap();
            let recovered = recovered_retraction(&outer, &fallback, &control)
                .unwrap()
                .unwrap();
            for (mode, key, own) in [
                (Mode::Plain, "", false),
                (Mode::Omemo2, fingerprint.as_str(), false),
                (Mode::Legacy, "", false),
                (Mode::Legacy, fingerprint.as_str(), true),
            ] {
                let mut invalid = recovered.clone();
                invalid.set_encryption(mode, key.into());
                invalid.own = own;
                assert!(
                    !actions::project(
                        vec![original.clone()],
                        vec![actions::Event {
                            row: 1,
                            message: invalid,
                            failed: false,
                            resolved: None
                        }],
                        owner,
                        peer
                    )
                    .messages[0]
                        .retracted
                );
            }
            for _ in 0..2 {
                db.record_action(owner, peer, &recovered, "received")
                    .unwrap();
                db.reconcile_actions(owner, peer).unwrap();
            }
            assert!(db.message(owner, peer, "base").unwrap().retracted);
            assert!(db.message(owner, peer, &fallback.id).is_none());
            assert!(db.history(owner, peer, None, 20).is_empty());
            let mut media = original.clone();
            media.id = "own-media".into();
            media.wire_id = "media-wire".into();
            media.aliases = vec!["wire:media-wire".into()];
            media.own = true;
            media.sender = owner.into();
            media.fingerprint = fingerprint.clone();
            media.attachment =
                crate::file_share::Attachment::bare("https://files.example.org/file");
            let mut caption = fallback.clone();
            caption.id = "own-caption".into();
            caption.wire_id = "caption-wire".into();
            caption.aliases = vec!["wire:caption-wire".into()];
            caption.own = true;
            caption.sender = owner.into();
            caption.upload_caption_for = media.wire_id.clone();
            db.save_chat_message(owner, &chat, Some(&media)).unwrap();
            db.save_chat_message(owner, &chat, Some(&caption)).unwrap();
            let mut unrelated = caption.clone();
            unrelated.id = "unrelated".into();
            unrelated.wire_id = "unrelated-wire".into();
            unrelated.aliases = vec!["wire:unrelated-wire".into()];
            unrelated.fingerprint = "other-device".into();
            db.save_chat_message(owner, &chat, Some(&unrelated))
                .unwrap();
            let targets = db
                .deletion_targets(owner, peer, &[media.id.clone()])
                .unwrap();
            assert_eq!(targets, vec![media.id.clone(), caption.id.clone()]);
            for target in targets {
                assert!(db.action_target(owner, peer, &target, "retract").is_ok());
            }
            let plain_text: Element = format!("<message xmlns='jabber:client' from='{peer}' to='{owner}' type='chat'><body>A previous message was retracted.</body></message>").parse().unwrap();
            assert!(
                recovered_retraction(&plain_text, &fallback, &control)
                    .unwrap()
                    .is_none()
            );
        }
    }

    #[test]
    fn legacy_attachment_authenticates_only_uri_and_reply_without_exposing_metadata() {
        let uri = format!(
            "aesgcm://files.example.org/opaque-object#{}",
            "01".repeat(44)
        );
        let mut file = crate::file_share::Attachment::bare(&uri).unwrap();
        file.name = "secret-display-name.png".into();
        file.mime = "image/png".into();
        file.size = Some(128);
        file.hashes.insert("sha-256".into(), vec![2; 32]);
        file.sticker = true;
        assert!(file.valid_outgoing(true));
        let mut message = Message {
            text: "caption stays separate".into(),
            attachment: Some(file),
            wire_id: "file-message".into(),
            reply_id: "original-message".into(),
            reply_to: "peer@example.org".into(),
            ..Default::default()
        };
        message.set_encryption(Mode::Legacy, String::new());
        for peer in ["peer@example.org", "room@conference.example.org"] {
            let clear = outgoing_content(peer, &message, "quoted").unwrap();
            let body = clear.get_child("body", payload::CLIENT).unwrap().text();
            assert_eq!(body, format!("> quoted\n{uri}"));
            let parsed = crate::file_share::Attachment::incoming(&clear, &body, true).unwrap();
            assert_eq!(parsed.wire_url, uri);
            assert_eq!(parsed.mime, "application/octet-stream");
            assert!(parsed.hashes.is_empty());
            let mut wire = Element::builder("message", payload::CLIENT)
                .append(Element::builder("encrypted", NS).build())
                .build();
            decorate_outgoing(&mut wire, &clear, &message);
            let bytes = bundle::xml(&wire, MAX_WIRE).unwrap();
            let wire_text = std::str::from_utf8(&bytes).unwrap();
            for private in [
                &uri,
                "secret-display-name.png",
                "image/png",
                "caption stays separate",
                crate::file_share::NS,
                crate::file_share::OOB,
                crate::file_share::META,
                crate::file_share::HASH,
            ] {
                assert!(
                    !wire_text.contains(private),
                    "Legacy outer stanza leaked {private}"
                );
            }
            assert!(crate::file_share::Attachment::sticker_marker(&wire));
            assert_eq!(wire.children().count(), 4);
        }
        assert_eq!(message.text, "caption stays separate");
        message.attachment = crate::file_share::Attachment::bare("https://files.example.org/plain");
        assert!(outgoing_content("peer@example.org", &message, "").is_err());
    }

    #[test]
    fn recovered_room_content_keeps_captured_author_and_never_sends_room_receipts() {
        let owner = "owner@example.org";
        let room = "room@conference.example.org";
        let occupant = format!("{room}/Old nickname");
        let sender = RoomSender {
            room: room.into(),
            occupant: occupant.clone(),
            author: "author@example.org".into(),
        };
        let metadata = Message {
            sender: occupant.clone(),
            author: "Old nickname".into(),
            wire_id: "wire-id".into(),
            aliases: vec!["room-archive:archive-id".into()],
            own: true,
            ..Default::default()
        };
        let outer: Element = format!("<message xmlns='jabber:client' from='{occupant}' type='groupchat' id='wire-id'><reply xmlns='{REPLY}' id='target-id' to='{room}/Other nickname'/><fallback xmlns='{FALLBACK}' for='{REPLY}'><body start='0' end='9'/></fallback></message>").parse().unwrap();
        let content: Element = format!("<message xmlns='jabber:client' from='{occupant}' type='groupchat' id='wire-id'><body>&gt; quoted\nreply</body></message>").parse().unwrap();
        let context = Context {
            archive: Some(("archive-id".into(), "2026-11-11T00:00:00Z".into())),
            historical: true,
            receipt_to: Some(occupant.clone()),
            room: Some(RoomContext {
                sender: sender.clone(),
                metadata,
            }),
        };
        let context: Context =
            serde_json::from_slice(&serde_json::to_vec(&context).unwrap()).unwrap();
        let token = serde_json::from_value(
            serde_json::json!({"epoch":vec![0u8;16],"slot":1,"digest":vec![0u8;32]}),
        )
        .unwrap();
        let received = Received {
            content: Some(content),
            outer: Some(outer),
            acknowledgement: None,
            fingerprint: [1; 32],
            replenish: false,
            token: Some(token),
            sent_time: 1794355200000,
            context: Some(context),
        };
        let (events, result) = mpsc::channel();
        let (network, _requests) = channel::unbounded_channel();
        let mut worker = Worker {
            owner: owner.into(),
            generation: 1,
            network,
            events,
            root: PathBuf::new(),
            local: None,
            online: false,
            ready: false,
            repair_tx: None,
        };
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(worker.deliver(received, None, "legacy-omemo:digest".into()))
            .unwrap();
        let (
            _,
            _,
            BackendEvent::LegacyEncryptedMessage {
                peer,
                message,
                historical,
                receipt_to,
                ..
            },
        ) = result.recv().unwrap()
        else {
            panic!("room content was not projected")
        };
        assert_eq!(peer, room);
        assert_eq!(message.sender, occupant);
        assert_eq!(message.author, "Old nickname");
        assert!(
            !message.own,
            "snapshot ownership must follow captured real author"
        );
        assert_eq!(message.text, "reply");
        assert_eq!(message.reply, "quoted");
        assert_eq!(message.reply_to, format!("{room}/Other nickname"));
        assert!(message.aliases.contains(&"room-archive:archive-id".into()));
        assert!(!message.aliases.contains(&"wire:wire-id".into()));
        assert_eq!(message.legacy_room_sender.unwrap(), sender);
        assert!(historical);
        assert!(receipt_to.is_none());
        let old: Context =
            serde_json::from_str(r#"{"archive":null,"historical":true,"receipt_to":null}"#)
                .unwrap();
        assert!(
            old.room.is_none(),
            "old direct journal contexts remain readable"
        );
    }
}

#[cfg(test)]
mod repair_gate_tests {
    use super::*;
    #[test]
    fn invalid_prekeys_only_repair_live_non_delayed_frames() {
        let mut frame = Frame {
            xml: Element::builder("message", payload::CLIENT).build(),
            archive: None,
            historical: false,
            receipt_to: None,
        };
        assert!(repair_candidate(&frame, &Error::Native(-1003)));
        assert!(!repair_candidate(&frame, &Error::TrustRequired));
        frame.historical = true;
        assert!(!repair_candidate(&frame, &Error::Native(-1003)));
        frame.historical = false;
        frame.archive = Some(("archive".into(), "id".into()));
        assert!(!repair_candidate(&frame, &Error::Native(-1003)));
        frame.archive = None;
        frame
            .xml
            .append_child(Element::builder("delay", "urn:xmpp:delay").build());
        assert!(!repair_candidate(&frame, &Error::Native(-1003)));
    }
}
