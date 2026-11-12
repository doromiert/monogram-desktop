//! XEP-0384 v0.3 over libomemo-c's Signal v3 mode. Separate profile, ratchets and trust.
pub mod service;
#[cfg(test)]
mod tests;
mod wire;

use crate::{
    crypto::{
        bundle::{self, Bundle},
        device::{self, Trust},
        journal::{Context, Journal, Received, Token},
        lifecycle, native, payload,
        pep::{self, Pep},
        records::*,
        *,
    },
    iq,
};
use openssl::symm::{Cipher, decrypt_aead, encrypt_aead};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::File,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
use tokio_xmpp::minidom::Element;
use wire::{Envelope, attr, element};

pub const NS: &str = "eu.siacs.conversations.axolotl";
const DEVICES: &str = "eu.siacs.conversations.axolotl.devicelist";
const MAX_WIRE: usize = 1024 * 1024;
const NO_DEVICES: &str = "No eligible Legacy OMEMO devices";
const DENIED: &str = "Legacy OMEMO keys require contact authorization";
const UNSUPPORTED: &str = "Legacy OMEMO key publication is unsupported";

pub struct Device {
    pub peer: String,
    pub id: u32,
    pub key: [u8; 32],
    pub trust: Trust,
}
pub struct Legacy {
    store: Store,
    journal: Journal,
    pep: Pep,
    history_path: PathBuf,
    _lock: File,
    repairs: BTreeMap<(String, u32), Instant>,
    pending_repairs: BTreeMap<(String, u32), Instant>,
}
#[derive(Debug)]
pub struct Repair {
    peer: String,
    device: u32,
    identity: [u8; 32],
    started: Instant,
}
pub fn exists(root: &Path, owner: &str) -> bool {
    lifecycle::exists_for(root, owner, true)
}
fn bundle_node(id: u32) -> String {
    format!("{NS}.bundles:{id}")
}
fn lookup(error: pep::Error, local: bool) -> Error {
    if error.unsupported() {
        return Error::Unavailable(UNSUPPORTED);
    }
    if error.access_denied() && !local {
        return Error::Unavailable(DENIED);
    }
    match error {
        pep::Error::Crypto(error) => error,
        _ => Error::Unavailable("Could not read or publish Legacy OMEMO keys"),
    }
}
fn probe_unavailable(error: &Error) -> bool {
    matches!(
        error,
        Error::TrustRequired
            | Error::InvalidBundle
            | Error::Unavailable(NO_DEVICES | DENIED | UNSUPPORTED)
    )
}
fn history(path: &Path) -> Result<Option<crate::storage::Store>> {
    if !path.exists() {
        return Ok(None);
    }
    let db =
        rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    Ok(Some(crate::storage::Store { db }))
}
fn now() -> u64 {
    chrono::Utc::now().timestamp_millis().max(0) as u64
}

impl Legacy {
    pub async fn open(root: &Path, owner: &str, iq: iq::Client, create: bool) -> Result<Self> {
        let published = if create && !exists(root, owner) {
            let discovery = Pep::new(iq.clone(), owner, 1)?;
            let list = discovery
                .item(owner, DEVICES, None)
                .await
                .map_err(|e| lookup(e, true))?;
            wire::device_ids(list.as_ref())?
        } else {
            BTreeSet::new()
        };
        let (store, journal, lock) =
            lifecycle::open_profile(root, owner, create, true, &published).await?;
        let pep = Pep::new(iq, owner, store.device)?;
        Ok(Self {
            store,
            journal,
            pep,
            history_path: root.join("history.sqlite3"),
            _lock: lock,
            repairs: BTreeMap::new(),
            pending_repairs: BTreeMap::new(),
        })
    }
    pub fn device(&self) -> u32 {
        self.store.device
    }
    pub fn fingerprint(&mut self) -> Result<[u8; 32]> {
        self.store.transaction(|tx| {
            tx.load(IDENTITY, "", 1)?
                .ok_or(Error::Incomplete)?
                .as_slice()
                .try_into()
                .map_err(|_| Error::Incomplete)
        })
    }
    pub async fn initialize(&mut self) -> Result<()> {
        let owner = self.store.owner.clone();
        let mut latest = self
            .pep
            .item(&owner, DEVICES, None)
            .await
            .map_err(|e| lookup(e, true))?;
        let mut seen = wire::device_ids(latest.as_ref())?;
        self.replenish().await?;
        for _ in 0..3 {
            let mut current = wire::device_ids(latest.as_ref())?;
            seen.insert(self.device());
            if seen.is_subset(&current) {
                break;
            }
            let mut list = latest.unwrap_or_else(|| element("list", NS));
            for id in seen.difference(&current) {
                list.append_child(attr(element("device", NS), "id", id.to_string()));
            }
            wire::device_ids(Some(&list))?;
            self.pep
                .publish_item(DEVICES, "current", list, "1")
                .await
                .map_err(|e| lookup(e, true))?;
            latest = self
                .pep
                .item(&owner, DEVICES, None)
                .await
                .map_err(|e| lookup(e, true))?;
            current = wire::device_ids(latest.as_ref())?;
            seen.extend(current);
        }
        if !seen.is_subset(&wire::device_ids(latest.as_ref())?) {
            return Err(Error::Changed);
        }
        self.pep
            .ensure_open_access(DEVICES)
            .await
            .map_err(|e| lookup(e, true))?;
        self.pep
            .ensure_open_access(&bundle_node(self.device()))
            .await
            .map_err(|e| lookup(e, true))?;
        Ok(())
    }
    pub async fn replenish(&mut self) -> Result<()> {
        let bundle = device::prepare_for(&mut self.store, now(), true)?;
        self.pep
            .publish_item(
                &bundle_node(self.device()),
                "current",
                wire::bundle_xml(&bundle),
                "1",
            )
            .await
            .map_err(|e| lookup(e, true))
    }
    async fn bundle(&self, peer: &str, id: u32) -> Result<Bundle> {
        address(peer, id)?;
        let data = self
            .pep
            .item(peer, &bundle_node(id), None)
            .await
            .map_err(|e| lookup(e, peer == self.store.owner))?
            .ok_or(Error::InvalidBundle)?;
        wire::parse_bundle(&data).map_err(|error| match error {
            Error::Invalid(_) | Error::Native(_) => Error::InvalidBundle,
            other => other,
        })
    }
    fn blind_trust(&self, peer: &str, conversation: Option<&str>) -> Result<bool> {
        if let Some(history) = history(&self.history_path)? {
            return Ok(history.blind_trust_for(&self.store.owner, peer, conversation)?);
        }
        Ok(false)
    }
    fn observe(
        &mut self,
        peer: &str,
        id: u32,
        key: &[u8; 32],
        conversation: Option<&str>,
    ) -> Result<Trust> {
        // Read after any PEP request so disabling this preference during a lookup takes effect.
        let accept = self.blind_trust(peer, conversation)?;
        self.store.transaction(|tx| {
            tx.save(SEEN, peer, id, Some(key))?;
            let trust = device::trust(tx, peer, id, key)?;
            if accept && trust == Trust::Undecided && tx.load(REJECTED, peer, id)?.is_none() {
                tx.save(TRUSTED, peer, id, Some(key))?;
                return Ok(Trust::Trusted);
            }
            Ok(trust)
        })
    }
    async fn published_ids(&self, peer: &str) -> Result<BTreeSet<u32>> {
        let own = peer == self.store.owner;
        let list = self
            .pep
            .item(peer, DEVICES, None)
            .await
            .map_err(|e| lookup(e, own))?;
        let mut ids = wire::device_ids(list.as_ref())?;
        if own && !ids.remove(&self.device()) {
            return Err(Error::Unavailable(
                "Refresh Legacy OMEMO setup to recover your device list",
            ));
        }
        Ok(ids)
    }
    async fn observe_bundle(
        &mut self,
        peer: &str,
        id: u32,
        conversation: Option<&str>,
    ) -> Result<(Device, Bundle)> {
        let bundle = self.bundle(peer, id).await?;
        let trust = self.observe(peer, id, &bundle.identity, conversation)?;
        Ok((
            Device {
                peer: peer.into(),
                id,
                key: bundle.identity,
                trust,
            },
            bundle,
        ))
    }
    async fn devices(
        &mut self,
        peer: &str,
        conversation: Option<&str>,
    ) -> Result<Vec<(Device, Bundle)>> {
        let mut entries = vec![];
        for id in self.published_ids(peer).await? {
            entries.push(self.observe_bundle(peer, id, conversation).await?);
        }
        Ok(entries)
    }
    pub async fn discover(&mut self, peer: &str) -> Result<(Vec<Device>, Vec<(u32, Error)>)> {
        self.discover_for(peer, None).await
    }
    pub(crate) async fn discover_for(
        &mut self,
        peer: &str,
        conversation: Option<&str>,
    ) -> Result<(Vec<Device>, Vec<(u32, Error)>)> {
        let (mut entries, mut errors) = (vec![], vec![]);
        for id in self.published_ids(peer).await? {
            match self.observe_bundle(peer, id, conversation).await {
                Ok((device, _)) => entries.push(device),
                Err(error) => errors.push((id, error)),
            }
        }
        Ok((entries, errors))
    }
    pub fn decide(
        &mut self,
        keys: &BTreeMap<String, BTreeMap<u32, [u8; 32]>>,
        approve: bool,
    ) -> Result<()> {
        device::decide(&mut self.store, keys, approve)
    }
    pub async fn probe(&mut self, peer: &str) -> Result<bool> {
        match self
            .encrypt(peer, "Monogram encryption check", &crate::model::new_id())
            .await
        {
            Ok(_) => Ok(true),
            Err(error) if probe_unavailable(&error) => Ok(false),
            Err(error) => Err(error),
        }
    }
    pub async fn encrypt(&mut self, peer: &str, body: &str, wire_id: &str) -> Result<Element> {
        address(peer, 1)?;
        let owner = self.store.owner.clone();
        let mut targets = self.devices(peer, Some(peer)).await?;
        if !targets
            .iter()
            .any(|(device, _)| device.trust != Trust::Rejected)
        {
            return Err(Error::Unavailable(NO_DEVICES));
        }
        if targets
            .iter()
            .any(|(device, _)| device.trust == Trust::Undecided)
        {
            return Err(if peer == owner {
                Error::OwnDeviceTrustRequired
            } else {
                Error::TrustRequired
            });
        }
        if owner != peer {
            // Failure to read our own devices is not evidence that they don't exist.
            let copies = self.devices(&owner, Some(peer)).await.map_err(|error| {
                if probe_unavailable(&error) {
                    Error::Unavailable("Could not verify your other Legacy OMEMO devices")
                } else {
                    error
                }
            })?;
            if copies
                .iter()
                .any(|(device, _)| device.trust == Trust::Undecided)
            {
                return Err(Error::OwnDeviceTrustRequired);
            }
            targets.extend(copies);
        }
        targets.retain(|(device, _)| device.trust == Trust::Trusted);
        encrypt_for(&mut self.store, peer, body, wire_id, &targets)
    }
    pub async fn encrypt_room(
        &mut self,
        room: &str,
        members: &BTreeSet<String>,
        body: &str,
        wire_id: &str,
    ) -> Result<Element> {
        address(room, 1)?;
        if members.is_empty() || members.len() > 4096 || !members.contains(&self.store.owner) {
            return Err(Error::Invalid("legacy complete room recipients"));
        }
        let owner = self.store.owner.clone();
        let mut targets = vec![];
        let mut seen = BTreeSet::new();
        for peer in members {
            address(peer, 1)?;
            let devices = self.devices(peer, Some(room)).await?;
            if devices
                .iter()
                .any(|(device, _)| device.trust == Trust::Undecided)
            {
                return Err(if peer == &owner {
                    Error::OwnDeviceTrustRequired
                } else {
                    Error::TrustRequired
                });
            }
            if peer != &owner
                && !devices
                    .iter()
                    .any(|(device, _)| device.trust == Trust::Trusted)
            {
                return Err(Error::Unavailable(NO_DEVICES));
            }
            for (device, bundle) in devices {
                if device.trust == Trust::Trusted {
                    if targets.len() >= 1000 || !seen.insert((device.peer.clone(), device.id)) {
                        return Err(Error::Invalid("legacy room recipient device limit"));
                    }
                    targets.push((device, bundle));
                }
            }
        }
        encrypt_for_kind(&mut self.store, room, body, wire_id, &targets, true)
    }
    pub fn digest(&self, outer: &Element) -> Result<[u8; 32]> {
        Ok(Envelope::parse(outer, &self.store.owner, self.device())?.digest)
    }
    pub fn digest_room(
        &self,
        outer: &Element,
        sender: &crate::rooms::members::RoomSender,
    ) -> Result<[u8; 32]> {
        Ok(Envelope::parse_room(outer, &self.store.owner, self.device(), sender)?.digest)
    }
    pub async fn receive(
        &mut self,
        outer: &Element,
        sent_time: i64,
        context: Option<Context>,
    ) -> Result<Received> {
        let envelope = Envelope::parse(outer, &self.store.owner, self.device())?;
        let (sender, destination) = payload::routing(outer)?;
        let conversation = if sender == self.store.owner {
            destination
        } else {
            sender
        };
        self.receive_envelope(outer, envelope, sent_time, context, Some(&conversation))
            .await
    }
    pub async fn receive_room(
        &mut self,
        outer: &Element,
        sender: &crate::rooms::members::RoomSender,
        sent_time: i64,
        context: Option<Context>,
    ) -> Result<Received> {
        let envelope = Envelope::parse_room(outer, &self.store.owner, self.device(), sender)?;
        self.receive_envelope(outer, envelope, sent_time, context, Some(&sender.room))
            .await
    }
    async fn receive_envelope(
        &mut self,
        outer: &Element,
        envelope: Envelope,
        sent_time: i64,
        context: Option<Context>,
        conversation: Option<&str>,
    ) -> Result<Received> {
        if let Some(cached) = self
            .store
            .transaction(|tx| self.journal.find(tx, &envelope.digest))?
        {
            return Ok(cached);
        }
        let first = self.receive_for(outer, &envelope, sent_time, context.clone());
        if !envelope.keys.iter().any(|k| k.prekey)
            || !matches!(first, Err(Error::Native(-1010)) | Err(Error::TrustRequired))
        {
            return first;
        }
        if !self.blind_trust(&envelope.peer, conversation)? {
            return Err(Error::TrustRequired);
        }
        let bundle = self.bundle(&envelope.peer, envelope.sender).await?;
        if self.observe(
            &envelope.peer,
            envelope.sender,
            &bundle.identity,
            conversation,
        )? != Trust::Trusted
        {
            return Err(Error::TrustRequired);
        }
        self.receive_for(outer, &envelope, sent_time, context)
    }
    fn receive_for(
        &mut self,
        outer: &Element,
        envelope: &Envelope,
        sent_time: i64,
        context: Option<Context>,
    ) -> Result<Received> {
        receive_for(
            &mut self.store,
            &self.journal,
            outer,
            envelope,
            sent_time,
            context,
        )
    }
    pub fn pending(&mut self) -> Result<Vec<Received>> {
        self.journal.pending(&mut self.store)
    }
    fn pending_token(&mut self, digest: &[u8; 32]) -> Result<Option<Token>> {
        self.store
            .transaction(|tx| Ok(self.journal.find(tx, digest)?.and_then(|saved| saved.token)))
    }
    pub fn accept(&mut self, token: &Token) -> Result<bool> {
        self.journal.accept(&mut self.store, token)
    }
    pub fn acknowledge(&mut self, token: &Token) -> Result<()> {
        self.journal.acknowledge(&mut self.store, token)
    }

    fn begin_repair(
        &mut self,
        outer: &Element,
        room: Option<&crate::rooms::members::RoomSender>,
    ) -> Result<Option<Repair>> {
        let envelope = if let Some(room) = room {
            Envelope::parse_room(outer, &self.store.owner, self.device(), room)?
        } else {
            Envelope::parse(outer, &self.store.owner, self.device())?
        };
        if !envelope.keys.iter().any(|key| key.prekey) || self.pending_repairs.len() >= 4 {
            return Ok(None);
        }
        let identity = self.store.transaction(|tx| {
            let Some(key) = tx.load(TRUSTED, &envelope.peer, envelope.sender)? else {
                return Ok(None);
            };
            let key: [u8; 32] = key.as_slice().try_into().map_err(|_| Error::Incomplete)?;
            if device::trust(tx, &envelope.peer, envelope.sender, &key)? != Trust::Trusted
                || tx.load(SESSION, &envelope.peer, envelope.sender)?.is_some()
            {
                return Ok(None);
            }
            Ok(Some(key))
        })?;
        let Some(identity) = identity else {
            return Ok(None);
        };
        self.repairs
            .retain(|_, last| last.elapsed() < Duration::from_secs(600));
        let key = (envelope.peer.clone(), envelope.sender);
        if self.repairs.len() >= 128 && !self.repairs.contains_key(&key)
            || self
                .repairs
                .get(&key)
                .is_some_and(|last| last.elapsed() < Duration::from_secs(60))
        {
            return Ok(None);
        }
        let started = Instant::now();
        self.repairs.insert(key.clone(), started);
        self.pending_repairs.insert(key, started);
        Ok(Some(Repair {
            peer: envelope.peer,
            device: envelope.sender,
            identity,
            started,
        }))
    }

    fn cancel_repairs(&mut self) {
        self.pending_repairs.clear();
    }

    fn complete_repair(&mut self, repair: Repair, bundle: Result<Bundle>) -> Result<Option<Token>> {
        if self
            .pending_repairs
            .remove(&(repair.peer.clone(), repair.device))
            != Some(repair.started)
        {
            return Ok(None);
        }
        let bundle = bundle?;
        if bundle.identity != repair.identity {
            return Err(Error::TrustRequired);
        }
        let owner = self.store.owner.clone();
        let local = self.device();
        self.store.transaction(|tx| {
            if device::trust(tx, &repair.peer, repair.device, &repair.identity)? != Trust::Trusted
                || tx.load(SESSION, &repair.peer, repair.device)?.is_some()
            {
                return Ok(None);
            }
            // XEP-0384 v0.3 §5 repairs a consumed-prekey collision with a new key transport.
            // A session established while discovery was pending always takes precedence.
            if bundle.prekeys.is_empty() || bundle.prekeys.len() > 1000 {
                return Err(Error::InvalidBundle);
            }
            native::verify_legacy(&bundle)?;
            let bound = (u64::from(u32::MAX) + 1) / bundle.prekeys.len() as u64
                * bundle.prekeys.len() as u64;
            let index = loop {
                let value = u64::from(u32::from_ne_bytes(random::<4>()?));
                if value < bound {
                    break (value % bundle.prekeys.len() as u64) as usize;
                }
            };
            let (id, prekey) = bundle
                .prekeys
                .iter()
                .nth(index)
                .ok_or(Error::InvalidBundle)?;
            native::build_for(
                true,
                tx,
                local,
                &repair.peer,
                repair.device,
                *id,
                prekey,
                &bundle,
            )?;
            let material = Secret::new(random::<16>()?.to_vec());
            let (prekey, bytes) =
                native::encrypt_for(true, tx, local, &repair.peer, repair.device, &material)?;
            if !prekey {
                return Err(Error::Invalid("legacy repair prekey transport"));
            }
            let ack = wire::stanza(
                &owner,
                &repair.peer,
                &crate::model::new_id(),
                local,
                vec![wire::key_xml(repair.device, prekey, &bytes)],
                &random::<12>()?,
                None,
            );
            let mut digest = b"legacy-session-repair\0".to_vec();
            digest.extend(bundle::xml(&ack, 65536)?);
            let received = self.journal.append(
                tx,
                openssl::sha::sha256(&digest),
                &ack,
                None,
                Some(&ack),
                repair.identity,
                false,
                chrono::Utc::now().timestamp_millis(),
                None,
            )?;
            Ok(Some(received.token.ok_or(Error::Incomplete)?))
        })
    }
}

fn encrypt_for(
    store: &mut Store,
    peer: &str,
    body: &str,
    id: &str,
    recipients: &[(Device, Bundle)],
) -> Result<Element> {
    encrypt_for_kind(store, peer, body, id, recipients, false)
}

fn encrypt_for_kind(
    store: &mut Store,
    peer: &str,
    body: &str,
    id: &str,
    recipients: &[(Device, Bundle)],
    room: bool,
) -> Result<Element> {
    if body.is_empty()
        || body.len() > payload::MAX
        || id.is_empty()
        || id.len() > 4096
        || recipients.is_empty()
        || recipients.len() > 1000
    {
        return Err(Error::Invalid("Legacy OMEMO outgoing message shape"));
    }
    let owner = store.owner.clone();
    let local = store.device;
    let mut material = zeroize::Zeroizing::new([0u8; 32]);
    openssl::rand::rand_bytes(&mut material[..16])?;
    let iv = random::<12>()?;
    let mut tag = [0; 16];
    let ciphertext = encrypt_aead(
        Cipher::aes_128_gcm(),
        &material[..16],
        Some(&iv),
        &[],
        body.as_bytes(),
        &mut tag,
    )?;
    material[16..].copy_from_slice(&tag);
    store.transaction(|tx| {
        let mut keys = Vec::with_capacity(recipients.len());
        for (device, bundle) in recipients {
            if device.peer == owner && device.id == local {
                return Err(Error::Invalid("legacy local device target"));
            }
            if device::trust(tx, &device.peer, device.id, &bundle.identity)? != Trust::Trusted {
                return Err(Error::TrustRequired);
            }
            if tx.load(SESSION, &device.peer, device.id)?.is_none() {
                // Sample uniformly from the published prekeys without bias.
                let bound = (u64::from(u32::MAX) + 1) / bundle.prekeys.len() as u64
                    * bundle.prekeys.len() as u64;
                let index = loop {
                    let value = u64::from(u32::from_ne_bytes(random::<4>()?));
                    if value < bound {
                        break (value % bundle.prekeys.len() as u64) as usize;
                    }
                };
                let (pre_id, pre) = bundle
                    .prekeys
                    .iter()
                    .nth(index)
                    .ok_or(Error::InvalidBundle)?;
                native::build_for(
                    true,
                    tx,
                    local,
                    &device.peer,
                    device.id,
                    *pre_id,
                    pre,
                    bundle,
                )?;
            }
            let (prekey, bytes) = native::encrypt_for(
                true,
                tx,
                local,
                &device.peer,
                device.id,
                material.as_slice(),
            )?;
            keys.push(wire::key_xml(device.id, prekey, &bytes));
        }
        let mut message = wire::stanza(&owner, peer, id, local, keys, &iv, Some(&ciphertext));
        if room {
            message = attr(message, "type", "groupchat");
        }
        bundle::xml(&message, MAX_WIRE)?;
        Ok(message)
    })
}

fn receive_for(
    store: &mut Store,
    journal: &Journal,
    outer: &Element,
    envelope: &Envelope,
    sent_time: i64,
    context: Option<Context>,
) -> Result<Received> {
    let owner = store.owner.clone();
    let local = store.device;
    let mut failure = Error::TrustRequired;
    for key in &envelope.keys {
        let result = store.transaction(|tx| {
            let material = native::decrypt_for(
                true,
                tx,
                local,
                &envelope.peer,
                envelope.sender,
                &key.bytes,
                key.prekey,
            )?;
            let content = match &envelope.ciphertext {
                Some(ciphertext) if material.len() == 32 => {
                    let body = Secret::new(decrypt_aead(
                        Cipher::aes_128_gcm(),
                        &material[..16],
                        Some(&envelope.iv),
                        &[],
                        ciphertext,
                        &material[16..],
                    )?);
                    let body = std::str::from_utf8(&body)
                        .map_err(|_| Error::Invalid("legacy message UTF-8"))?;
                    let mut message = element("message", payload::CLIENT);
                    for name in ["from", "to", "type", "id"] {
                        if let Some(value) = outer.attr(name) {
                            message = attr(message, name, value);
                        }
                    }
                    message.append_child(
                        Element::builder("body", payload::CLIENT)
                            .append(body)
                            .build(),
                    );
                    // Legacy authenticates only the body. Ignore every outside extension.
                    Some(message)
                }
                None if matches!(material.len(), 16 | 32) => None,
                _ => return Err(Error::Invalid("legacy payload material")),
            };
            let acknowledgement = if key.prekey {
                let random_key = zeroize::Zeroizing::new(random::<16>()?);
                let (first, bytes) = native::encrypt_for(
                    true,
                    tx,
                    local,
                    &envelope.peer,
                    envelope.sender,
                    random_key.as_slice(),
                )?;
                Some(wire::stanza(
                    &owner,
                    &envelope.ack_to,
                    &crate::model::new_id(),
                    local,
                    vec![wire::key_xml(envelope.sender, first, &bytes)],
                    &random::<12>()?,
                    None,
                ))
            } else {
                None
            };
            let fingerprint = tx
                .load(SESSION_IDENTITY, &envelope.peer, envelope.sender)?
                .ok_or(Error::Incomplete)?
                .as_slice()
                .try_into()
                .map_err(|_| Error::Incomplete)?;
            if content.is_some() || acknowledgement.is_some() {
                journal.append(
                    tx,
                    envelope.digest,
                    outer,
                    content.as_ref(),
                    acknowledgement.as_ref(),
                    fingerprint,
                    key.prekey,
                    sent_time,
                    context.clone(),
                )
            } else {
                Ok(Received {
                    content: None,
                    outer: None,
                    acknowledgement: None,
                    fingerprint,
                    replenish: false,
                    token: None,
                    sent_time,
                    context: context.clone(),
                })
            }
        });
        match result {
            Ok(received) => return Ok(received),
            Err(Error::Native(-1010)) => failure = Error::TrustRequired, // libomemo-c: SG_ERR_UNTRUSTED_IDENTITY
            Err(error @ (Error::Native(_) | Error::Crypto(_))) => failure = error,
            Err(error) => return Err(error),
        }
    }
    Err(failure)
}

#[cfg(test)]
mod repair_tests {
    use super::tests::{approve, device, fixture};
    use super::*;
    const A: &str = "alice@example.invalid";
    const B: &str = "bob@example.invalid";
    const C: &str = "carol@example.invalid";

    #[test]
    fn consumed_prekey_repairs_only_the_approved_missing_session() {
        let (mut alice, _, alice_bundle) = fixture(A, 101);
        let (mut bob, journal, mut bob_bundle) = fixture(B, 202);
        let (mut carol, carol_journal, carol_bundle) = fixture(C, 303);
        approve(&mut alice, B, 202, &bob_bundle);
        approve(&mut carol, B, 202, &bob_bundle);
        approve(&mut bob, A, 101, &alice_bundle);
        bob_bundle.prekeys.retain(|id, _| *id == 77);
        let first = attr(
            encrypt_for(
                &mut alice,
                B,
                "first",
                "first",
                &[(device(B, 202, &bob_bundle), bob_bundle.clone())],
            )
            .unwrap(),
            "from",
            A,
        );
        let stale = attr(
            encrypt_for(
                &mut carol,
                B,
                "stale",
                "stale",
                &[(device(B, 202, &bob_bundle), bob_bundle.clone())],
            )
            .unwrap(),
            "from",
            C,
        );
        let (network, _requests) = tokio::sync::mpsc::unbounded_channel();
        let mut receiver = Legacy {
            store: bob,
            journal,
            pep: Pep::new(iq::Client::new(network), B, 202).unwrap(),
            history_path: PathBuf::new(),
            _lock: File::open("/dev/null").unwrap(),
            repairs: BTreeMap::new(),
            pending_repairs: BTreeMap::new(),
        };
        let envelope = Envelope::parse(&first, B, 202).unwrap();
        let accepted = receiver.receive_for(&first, &envelope, 0, None).unwrap();
        let token = accepted.token.unwrap();
        receiver.accept(&token).unwrap();
        receiver.acknowledge(&token).unwrap();
        assert!(
            receiver.begin_repair(&stale, None).unwrap().is_none(),
            "untrusted devices never trigger discovery"
        );
        approve(&mut receiver.store, C, 303, &carol_bundle);
        let envelope = Envelope::parse(&stale, B, 202).unwrap();
        assert!(matches!(
            receiver.receive_for(&stale, &envelope, 0, None),
            Err(Error::Native(-1003))
        ));
        let repair = receiver.begin_repair(&stale, None).unwrap().unwrap();
        for _ in 0..10 {
            assert!(receiver.begin_repair(&stale, None).unwrap().is_none());
        }
        receiver.cancel_repairs();
        assert!(
            receiver
                .complete_repair(repair, Ok(carol_bundle.clone()))
                .unwrap()
                .is_none(),
            "disconnected lookup completion is discarded"
        );
        receiver.repairs.clear();
        let repair = receiver.begin_repair(&stale, None).unwrap().unwrap();
        let mut changed = carol_bundle.clone();
        changed.identity = alice_bundle.identity;
        assert!(matches!(
            receiver.complete_repair(repair, Ok(changed)),
            Err(Error::TrustRequired)
        ));
        receiver.repairs.clear();
        let repair = receiver.begin_repair(&stale, None).unwrap().unwrap();
        let token = receiver
            .complete_repair(repair, Ok(carol_bundle))
            .unwrap()
            .unwrap();
        let pending = receiver
            .pending()
            .unwrap()
            .into_iter()
            .find(|entry| entry.token.as_ref() == Some(&token))
            .unwrap();
        assert!(
            pending.content.is_none(),
            "repair never resends old message content"
        );
        let ack = pending.acknowledgement.unwrap();
        assert!(ack.get_child("payload", NS).is_none());
        assert_eq!(receiver.fingerprint().unwrap(), bob_bundle.identity);
        let ack = attr(ack, "from", B);
        let envelope = Envelope::parse(&ack, C, 303).unwrap();
        receive_for(&mut carol, &carol_journal, &ack, &envelope, 0, None).unwrap();
        let fresh_bundle = device::prepare_for(&mut receiver.store, 2, true).unwrap();
        let next = attr(
            encrypt_for(
                &mut carol,
                B,
                "next",
                "next",
                &[(device(B, 202, &fresh_bundle), fresh_bundle)],
            )
            .unwrap(),
            "from",
            C,
        );
        let envelope = Envelope::parse(&next, B, 202).unwrap();
        assert!(
            receiver
                .receive_for(&next, &envelope, 0, None)
                .unwrap()
                .content
                .is_some()
        );
        receiver.repairs.clear();
        assert!(
            receiver.begin_repair(&stale, None).unwrap().is_none(),
            "a working session is never replaced by an old failure"
        );
    }
}
