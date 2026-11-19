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
/// The refusal reads `<DENIED_PREFIX> <contact's bare address>`.
pub(crate) const DENIED_PREFIX: &str = "Legacy OMEMO keys require contact authorization from";
/// The contact a [`Error::ContactAuthorization`] message names, or `None` for any other text, so
/// words that merely resemble it (a server's, for instance) never match.
pub(crate) fn denied_contact(reason: &str) -> Option<&str> {
    reason
        .strip_prefix(DENIED_PREFIX)?
        .strip_prefix(' ')
        .filter(|contact| !contact.is_empty() && !contact.contains(char::is_whitespace))
}
const UNSUPPORTED: &str = "Legacy OMEMO key publication is unsupported";
/// How many room members' keys are fetched at once. The IQ layer allows 256 requests in flight and
/// the rest of the app shares it.
const KEY_FETCH_CONCURRENCY: usize = 16;
/// How long a room member whose keys could not be fetched is remembered as unreachable, so that
/// while the sender has chosen to leave such members out, each message does not wait out another
/// request timeout for them. They are reported as left out every time, with the same reason.
const UNREACHABLE_MEMORY: Duration = Duration::from_secs(5 * 60);
const NOBODY_REACHABLE: &str = "No other room member's keys could be used";
const FETCH_FAILED: &str = "Could not read or publish Legacy OMEMO keys";
const KEY_SERVER_TIMEOUT: &str = "The encryption key server did not respond. Your message is kept; retry when it is reachable.";
const MISSING_KEYS: &str = "A participant has no usable encryption keys yet. They need to reconnect their client before you can send encrypted messages.";
#[derive(serde::Serialize, serde::Deserialize)]
struct CachedDevices {
    time: u64,
    ids: BTreeSet<u32>,
}

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
    /// Room members whose keys could not be fetched, when, and why. Only filled and read while the
    /// sender has chosen to leave unreachable members out.
    unreachable: BTreeMap<String, (tokio::time::Instant, String)>,
    discovery_backoff: BTreeMap<String, Instant>,
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
/// `lookup` for fetching `peer`'s keys: a refusal names the contact so it can be resolved.
fn lookup_peer(error: pep::Error, peer: &str, local: bool) -> Error {
    if !local && !error.unsupported() && error.access_denied() {
        return Error::ContactAuthorization(peer.into());
    }
    lookup(error, local)
}
fn lookup(error: pep::Error, local: bool) -> Error {
    if let pep::Error::Request(request) = &error {
        eprintln!("Monogram Legacy key request (own={local}): {request:?}");
    }
    if error.unsupported() {
        return Error::Unavailable(UNSUPPORTED);
    }
    match error {
        pep::Error::Crypto(error) => error,
        pep::Error::Request(iq::Error::Timeout) => Error::Unavailable(KEY_SERVER_TIMEOUT),
        pep::Error::Request(iq::Error::Rejected { condition, .. })
            if condition == "remote-server-timeout" => Error::Unavailable(KEY_SERVER_TIMEOUT),
        _ => Error::Unavailable(FETCH_FAILED),
    }
}
/// Whether a failure to get one room member's keys is a reason the sender may choose to leave that
/// member out: their keys are refused, missing, invalid or unreachable. Unreviewed keys
/// ([`Error::TrustRequired`]) are never skippable, and neither is anything about our own devices.
pub(crate) fn skippable(error: &Error) -> bool {
    matches!(
        error,
        Error::ContactAuthorization(_)
            | Error::InvalidBundle(_)
            | Error::Unavailable(
                NO_DEVICES | UNSUPPORTED | FETCH_FAILED | KEY_SERVER_TIMEOUT | MISSING_KEYS,
            )
    )
}
/// [`skippable`] for a failure that is only available as the text shown to the user.
pub(crate) fn skippable_reason(reason: &str) -> bool {
    denied_contact(reason).is_some()
        || matches!(
            reason,
            NO_DEVICES | UNSUPPORTED | FETCH_FAILED | KEY_SERVER_TIMEOUT | MISSING_KEYS
        )
        || (reason.starts_with("No usable ") && reason.contains(" keys from "))
}
fn probe_unavailable(error: &Error) -> bool {
    matches!(
        error,
        Error::TrustRequired
            | Error::InvalidBundle(_)
            | Error::Unavailable(NO_DEVICES | UNSUPPORTED)
            | Error::ContactAuthorization(_)
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
            unreachable: BTreeMap::new(),
            discovery_backoff: BTreeMap::new(),
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
            .map_err(|e| lookup_peer(e, peer, peer == self.store.owner))?
            .ok_or_else(|| Error::invalid_bundle(crate::encryption_policy::Mode::Legacy.name(), peer, Some(id)))?;
        wire::parse_bundle(&data).map_err(|error| match error {
            Error::Invalid(_) | Error::Native(_) => {
                Error::invalid_bundle(crate::encryption_policy::Mode::Legacy.name(), peer, Some(id))
            }
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
            .map_err(|e| lookup_peer(e, peer, own))?;
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
    /// The network half of [`Self::devices`]: a member's published device ids and their bundles. It
    /// needs only `&self`, so many members can be fetched at once.
    async fn fetch_member(&self, peer: &str) -> Result<Vec<(u32, Bundle)>> {
        let mut bundles = vec![];
        for id in self.published_ids(peer).await? {
            bundles.push((id, self.bundle(peer, id).await?));
        }
        Ok(bundles)
    }
    /// The store half: record what was seen and decide trust for fetched bundles.
    fn observe_fetched(
        &mut self,
        peer: &str,
        fetched: Vec<(u32, Bundle)>,
        conversation: Option<&str>,
    ) -> Result<Vec<(Device, Bundle)>> {
        let mut entries = vec![];
        for (id, bundle) in fetched {
            let trust = self.observe(peer, id, &bundle.identity, conversation)?;
            entries.push((
                Device {
                    peer: peer.into(),
                    id,
                    key: bundle.identity,
                    trust,
                },
                bundle,
            ));
        }
        Ok(entries)
    }
    fn cached_devices(&mut self, peer: &str) -> Result<Option<CachedDevices>> {
        self.store.transaction(|tx| {
            let Some(bytes) = tx.load(crate::crypto::records::DEVICES, peer, 0)? else {
                return Ok(None);
            };
            let cached: CachedDevices = serde_json::from_slice(&bytes).map_err(|_| Error::Incomplete)?;
            if cached.ids.len() > 500 || cached.ids.iter().any(|id| address(peer, *id).is_err()) {
                return Err(Error::Incomplete);
            }
            Ok(Some(cached))
        })
    }
    fn save_devices(&mut self, peer: &str, ids: &BTreeSet<u32>) -> Result<()> {
        let bytes = serde_json::to_vec(&CachedDevices { time: now(), ids: ids.clone() })
            .map_err(|_| Error::Incomplete)?;
        self.store.transaction(|tx| tx.save(crate::crypto::records::DEVICES, peer, 0, Some(&bytes)))
    }
    async fn sending_ids(&mut self, peer: &str, conversation: Option<&str>) -> Result<BTreeSet<u32>> {
        let cached = self.cached_devices(peer)?;
        let blind = self.blind_trust(peer, conversation)?;
        if blind && cached.as_ref().is_some_and(|cache| now().checked_sub(cache.time).is_some_and(|age| age < 60_000)) {
            return Ok(cached.unwrap().ids);
        }
        let result = if blind && self.discovery_backoff.get(peer).is_some_and(|at| at.elapsed() < Duration::from_secs(60)) {
            Err(Error::Unavailable(KEY_SERVER_TIMEOUT))
        } else { self.published_ids(peer).await };
        match result {
            Ok(ids) => {
                self.discovery_backoff.remove(peer);
                self.save_devices(peer, &ids)?;
                Ok(ids)
            }
            Err(error @ Error::Unavailable(KEY_SERVER_TIMEOUT))
                if self.blind_trust(peer, conversation)? => {
                if self.discovery_backoff.len() >= 256 { self.discovery_backoff.clear(); }
                let at = self.discovery_backoff.entry(peer.into()).or_insert_with(Instant::now);
                if at.elapsed() >= Duration::from_secs(60) { *at = Instant::now(); }
                if let Some(cache) = cached {
                    return if now().checked_sub(cache.time).is_some_and(|age| age < 86_400_000) {
                        eprintln!("Monogram Legacy discovery: using cached device list during remote timeout");
                        Ok(cache.ids)
                    } else { Err(error) };
                }
                // Upgrade profiles have trusted ratchets but no persisted discovery cache yet.
                let known = self.store.transaction(|tx| tx.peer_ids(SESSION, peer))?;
                let mut ids = BTreeSet::new();
                for id in known {
                    if self.session_device(peer, id)?.is_some() {
                        ids.insert(id);
                    }
                }
                if ids.is_empty() { return Err(error); }
                self.save_devices(peer, &ids)?;
                eprintln!("Monogram Legacy discovery: recovered device list from established sessions");
                Ok(ids)
            }
            Err(error) => Err(error),
        }
    }
    async fn devices(
        &mut self,
        peer: &str,
        conversation: Option<&str>,
    ) -> Result<Vec<(Device, Option<Bundle>)>> {
        let mut entries = vec![];
        let mut missing = false;
        for id in self.sending_ids(peer, conversation).await? {
            if self.blind_trust(peer, conversation)? {
                if let Some(device) = self.session_device(peer, id)? {
                    entries.push((device, None));
                    continue;
                }
            }
            match self.observe_bundle(peer, id, conversation).await {
                Ok((device, bundle)) => entries.push((device, Some(bundle))),
                Err(Error::InvalidBundle(_) | Error::Unavailable(KEY_SERVER_TIMEOUT)) if self.blind_trust(peer, conversation)? => {
                    // Stale PEP entries must not block other working devices or existing ratchets.
                    if let Some(device) = self.session_device(peer, id)? {
                        entries.push((device, None));
                    } else {
                        missing = true;
                    }
                }
                Err(error) => return Err(error),
            }
        }
        if entries.is_empty() && missing && peer != self.store.owner {
            return Err(Error::Unavailable(MISSING_KEYS));
        }
        Ok(entries)
    }
    fn session_device(&mut self, peer: &str, id: u32) -> Result<Option<Device>> {
        self.store.transaction(|tx| {
            if tx.load(SESSION, peer, id)?.is_none() {
                return Ok(None);
            }
            let Some(identity) = tx.load(SESSION_IDENTITY, peer, id)? else {
                return Err(Error::Incomplete);
            };
            let key = identity.as_slice().try_into().map_err(|_| Error::Incomplete)?;
            if device::trust(tx, peer, id, &key)? != Trust::Trusted {
                return Ok(None);
            }
            Ok(Some(Device { peer: peer.into(), id, key, trust: Trust::Trusted }))
        })
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
        skip_unreachable: bool,
    ) -> Result<(Element, Vec<(String, String)>)> {
        address(room, 1)?;
        if members.is_empty() || members.len() > 4096 || !members.contains(&self.store.owner) {
            return Err(Error::Invalid("legacy complete room recipients"));
        }
        let owner = self.store.owner.clone();
        let mut targets = vec![];
        let mut seen = BTreeSet::new();
        let automatic = self.blind_trust(room, Some(room))?;
        // Members left out because their keys could not be fetched or used. In an automatic room that
        // happens on its own and is only logged; otherwise only when the sender asked for it, never
        // for our own account, and never for unreviewed keys.
        let mut skipped = vec![];
        let mut automatic_skipped = 0;
        // Fetch every member's keys at once: a member that never answers costs one timeout for the
        // whole room instead of one each. Trust and errors are then handled in member order.
        // Members found unreachable a moment ago are not waited for again while the sender has chosen
        // to leave unreachable members out; the choice, not a guess, is what allows skipping them.
        let remembered: BTreeMap<String, String> = if skip_unreachable && !automatic {
            self.unreachable
                .retain(|_, (when, _)| when.elapsed() < UNREACHABLE_MEMORY);
            self.unreachable
                .iter()
                .filter(|(peer, _)| *peer != &owner && members.contains(*peer))
                .map(|(peer, (_, why))| (peer.clone(), why.clone()))
                .collect()
        } else {
            BTreeMap::new()
        };
        let mut fetched: BTreeMap<String, Result<Vec<(u32, Bundle)>>> = if automatic {
            BTreeMap::new()
        } else {
            use futures_util::stream::{self, StreamExt};
            let this = &*self;
            stream::iter(members.iter().filter(|peer| !remembered.contains_key(*peer)).cloned())
                .map(|peer| async move {
                    let result = this.fetch_member(&peer).await;
                    (peer, result)
                })
                .buffer_unordered(KEY_FETCH_CONCURRENCY)
                .collect()
                .await
        };
        // Whatever the sender chose this time, remember who could not be reached. A retry that does
        // allow leaving them out (Send Anyway) then does not wait for them a second time.
        for (peer, result) in &fetched {
            if let Err(error) = result {
                if peer != &owner && skippable(error) {
                    self.unreachable.insert(peer.clone(), (tokio::time::Instant::now(), error.to_string()));
                }
            }
        }
        for peer in members {
            address(peer, 1)?;
            if let Some(why) = remembered.get(peer) {
                skipped.push((peer.clone(), why.clone()));
                continue;
            }
            let result = if automatic {
                self.devices(peer, Some(room)).await
            } else {
                let found = fetched.remove(peer).expect("every member was fetched");
                found
                    .and_then(|found| self.observe_fetched(peer, found, Some(room)))
                    .map(|devices| devices.into_iter().map(|(device, bundle)| (device, Some(bundle))).collect())
            };
            let mut devices = match result {
                Ok(devices) => {
                    self.unreachable.remove(peer);
                    devices
                }
                Err(Error::InvalidBundle(_) | Error::ContactAuthorization(_)
                    | Error::Unavailable(KEY_SERVER_TIMEOUT | MISSING_KEYS | NO_DEVICES | UNSUPPORTED)) if automatic => {
                    automatic_skipped += 1;
                    continue;
                }
                Err(error) if skip_unreachable && peer != &owner && skippable(&error) => {
                    let why = error.to_string();
                    self.unreachable.insert(peer.clone(), (tokio::time::Instant::now(), why.clone()));
                    skipped.push((peer.clone(), why));
                    continue;
                }
                Err(error) => return Err(error),
            };
            if !automatic && devices
                .iter()
                .any(|(device, _)| device.trust == Trust::Undecided)
            {
                return Err(if peer == &owner {
                    Error::OwnDeviceTrustRequired
                } else {
                    Error::TrustRequired
                });
            }
            if automatic { devices.retain(|(device, _)| device.trust == Trust::Trusted); }
            if peer != &owner
                && !devices
                    .iter()
                    .any(|(device, _)| device.trust == Trust::Trusted)
            {
                if automatic {
                    automatic_skipped += 1;
                    continue;
                }
                if skip_unreachable {
                    skipped.push((peer.clone(), NO_DEVICES.to_owned()));
                    continue;
                }
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
        if !skipped.is_empty() && skipped.len() + 1 >= members.len() {
            return Err(Error::Unavailable(NOBODY_REACHABLE));
        }
        if members.iter().any(|peer| peer != &owner)
            && !targets.iter().any(|(device, _)| device.peer != owner) {
            return Err(Error::Unavailable(NO_DEVICES));
        }
        if automatic_skipped > 0 {
            eprintln!("Monogram Legacy automatic room encryption: skipped {automatic_skipped} unavailable participants; retained {} encrypted device targets", targets.len());
        }
        let xml = encrypt_for_kind(&mut self.store, room, body, wire_id, &targets, true)?;
        Ok((xml, skipped))
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
                return Err(Error::invalid_bundle(
                    crate::encryption_policy::Mode::Legacy.name(),
                    &repair.peer,
                    Some(repair.device),
                ));
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
                .ok_or_else(|| {
                    Error::invalid_bundle(crate::encryption_policy::Mode::Legacy.name(), &repair.peer, Some(repair.device))
                })?;
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
    recipients: &[(Device, Option<Bundle>)],
) -> Result<Element> {
    encrypt_for_kind(store, peer, body, id, recipients, false)
}

fn encrypt_for_kind(
    store: &mut Store,
    peer: &str,
    body: &str,
    id: &str,
    recipients: &[(Device, Option<Bundle>)],
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
            if device::trust(tx, &device.peer, device.id, &device.key)? != Trust::Trusted {
                return Err(Error::TrustRequired);
            }
            if tx.load(SESSION, &device.peer, device.id)?.is_none() {
                let bundle = bundle.as_ref().ok_or_else(|| Error::invalid_bundle(crate::encryption_policy::Mode::Legacy.name(), &device.peer, Some(device.id)))?;
                if bundle.identity != device.key || bundle.prekeys.is_empty() {
                    return Err(Error::invalid_bundle(crate::encryption_policy::Mode::Legacy.name(), &device.peer, Some(device.id)));
                }
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
                    .ok_or_else(|| {
                        Error::invalid_bundle(crate::encryption_policy::Mode::Legacy.name(), &device.peer, Some(device.id))
                    })?;
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
                    if let Some(content)=crate::legacy_inline::decode(body).map_err(Error::Invalid)? {
                        for child in content.children(){message.append_child(child.clone());}
                    } else {
                    message.append_child(
                        Element::builder("body", payload::CLIENT)
                            .append(body)
                            .build(),
                    );
                    }
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
                &[(device(B, 202, &bob_bundle), Some(bob_bundle.clone()))],
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
                &[(device(B, 202, &bob_bundle), Some(bob_bundle.clone()))],
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
            unreachable: BTreeMap::new(),
            discovery_backoff: BTreeMap::new(),
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
                &[(device(B, 202, &fresh_bundle), Some(fresh_bundle))],
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

#[cfg(test)]
mod denial_tests {
    use super::*;
    use crate::iq;

    fn rejected(condition: &str, pubsub: Option<&str>) -> pep::Error {
        pep::Error::Request(iq::Error::Rejected {
            condition: condition.into(),
            pubsub: pubsub.map(str::to_owned),
        })
    }

    #[test]
    fn a_refused_key_fetch_names_the_contact_who_refused() {
        for error in [
            rejected("forbidden", None),
            rejected("not-authorized", None),
            rejected("subscription-required", None),
            rejected("auth", Some("presence-subscription-required")),
        ] {
            let Error::ContactAuthorization(contact) = lookup_peer(error, "carol@example.org", false) else {
                panic!("a refusal must be a ContactAuthorization");
            };
            assert_eq!(contact, "carol@example.org");
        }
    }

    #[test]
    fn the_message_names_the_contact_and_only_that_text_parses_back() {
        let text = Error::ContactAuthorization("carol@example.org".into()).to_string();
        assert_eq!(text, "Legacy OMEMO keys require contact authorization from carol@example.org");
        assert_eq!(denied_contact(&text), Some("carol@example.org"));
        for other in [
            "Legacy OMEMO keys require contact authorization",
            "Legacy OMEMO keys require contact authorization from ",
            "Legacy OMEMO keys require contact authorization from a b",
            "the server said: Legacy OMEMO keys require contact authorization from carol@example.org",
            "No eligible Legacy OMEMO devices",
        ] {
            assert_eq!(denied_contact(other), None, "{other:?}");
        }
    }

    #[test]
    fn our_own_devices_and_other_failures_are_not_a_contact_refusal() {
        assert!(!matches!(lookup_peer(rejected("forbidden", None), "me@example.org", true), Error::ContactAuthorization(_)));
        assert!(matches!(lookup_peer(rejected("feature-not-implemented", None), "carol@example.org", false), Error::Unavailable(UNSUPPORTED)));
        assert!(matches!(lookup_peer(rejected("item-not-found", None), "carol@example.org", false), Error::Unavailable(_)));
    }

    #[test]
    fn a_refusal_is_a_definitive_probe_answer_not_a_failure() {
        assert!(probe_unavailable(&Error::ContactAuthorization("carol@example.org".into())));
    }

    /// The real send path: `probe` fetches the contact's device list, the server refuses it, and
    /// the error that comes back names that contact (here a room member, not the chat's address).
    #[test]
    fn a_probe_whose_key_fetch_is_refused_names_the_refusing_contact() {
        use crate::protocol::Command;
        use super::tests::fixture;
        let (store, journal, _) = fixture("alice@example.invalid", 101);
        let (network, mut requests) = tokio::sync::mpsc::unbounded_channel();
        let mut legacy = Legacy {
            store,
            journal,
            pep: Pep::new(iq::Client::new(network), "alice@example.invalid", 101).unwrap(),
            history_path: PathBuf::new(),
            _lock: File::open("/dev/null").unwrap(),
            repairs: BTreeMap::new(),
            pending_repairs: BTreeMap::new(),
            unreachable: BTreeMap::new(),
            discovery_backoff: BTreeMap::new(),
        };
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let outcome = runtime.block_on(async {
            let server = async {
                while let Some(command) = requests.recv().await {
                    if let Command::Iq { reply, .. } = command {
                        let _ = reply.send(Err(iq::Error::Rejected {
                            condition: "forbidden".into(),
                            pubsub: None,
                        }));
                    }
                }
            };
            tokio::select! {
                result = legacy.probe("carol@example.invalid") => result,
                _ = server => unreachable!("the fake server only ends when the client does"),
            }
        });
        // A refusal is a definitive answer for selection, but the typed error is still what `encrypt` reports.
        assert!(matches!(outcome, Ok(false)), "a refusal makes the probe say \"not available\": {outcome:?}");
        let error = runtime.block_on(async {
            let server = async {
                while let Some(command) = requests.recv().await {
                    if let Command::Iq { reply, .. } = command {
                        let _ = reply.send(Err(iq::Error::Rejected {
                            condition: "forbidden".into(),
                            pubsub: None,
                        }));
                    }
                }
            };
            tokio::select! {
                result = legacy.encrypt("carol@example.invalid", "hi", "wire") => result.unwrap_err(),
                _ = server => unreachable!(),
            }
        });
        match error {
            Error::ContactAuthorization(contact) => assert_eq!(contact, "carol@example.invalid"),
            other => panic!("expected the refusing contact, got {other:?}"),
        }
    }
}

#[cfg(test)]
mod bundle_tests {
    use super::*;
    use crate::{iq, protocol::Command};

    const PUBSUB: &str = "http://jabber.org/protocol/pubsub";
    const CAROL: &str = "carol@example.invalid";

    fn pubsub(node: &str, item: Element) -> Element {
        Element::builder("pubsub", PUBSUB)
            .append(
                Element::builder("items", PUBSUB)
                    .attr("node".try_into().unwrap(), node)
                    .append(Element::builder("item", PUBSUB).append(item).build())
                    .build(),
            )
            .build()
    }
    fn device_list(id: u32) -> Element {
        pubsub(
            DEVICES,
            Element::builder("list", NS)
                .append(
                    Element::builder("device", NS)
                        .attr("id".try_into().unwrap(), id.to_string())
                        .build(),
                )
                .build(),
        )
    }

    /// `encrypt` to Carol against a server that lists device 7 and answers the bundle request with
    /// `bundle`: `None` is item-not-found, `Some` is the item's content.
    fn encrypt_with_bundle(bundle: Option<Element>) -> Error {
        use super::tests::fixture;
        let (store, journal, _) = fixture("alice@example.invalid", 101);
        let (network, mut requests) = tokio::sync::mpsc::unbounded_channel();
        let mut legacy = Legacy {
            store,
            journal,
            pep: Pep::new(iq::Client::new(network), "alice@example.invalid", 101).unwrap(),
            history_path: PathBuf::new(),
            _lock: File::open("/dev/null").unwrap(),
            repairs: BTreeMap::new(),
            pending_repairs: BTreeMap::new(),
            unreachable: BTreeMap::new(),
            discovery_backoff: BTreeMap::new(),
        };
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        runtime.block_on(async {
            let server = async {
                while let Some(command) = requests.recv().await {
                    let Command::Iq { payload, reply, .. } = command else { continue };
                    let node = payload.children().next().and_then(|items| items.attr("node")).unwrap_or("").to_owned();
                    let _ = reply.send(if node == DEVICES {
                        Ok(Some(device_list(7)))
                    } else {
                        match &bundle {
                            None => Err(iq::Error::Rejected { condition: "item-not-found".into(), pubsub: None }),
                            Some(content) => Ok(Some(pubsub(&node, content.clone()))),
                        }
                    });
                }
            };
            tokio::select! {
                result = legacy.encrypt(CAROL, "hi", "wire") => result.unwrap_err(),
                _ = server => unreachable!("the fake server only ends when the client does"),
            }
        })
    }
    fn named(error: Error) -> (String, String, Option<u32>) {
        match error {
            Error::InvalidBundle(Some(who)) => (who.protocol.into(), who.peer, who.device),
            other => panic!("expected a named InvalidBundle, got {other:?}"),
        }
    }

    #[test]
    fn a_missing_bundle_names_the_protocol_contact_and_device() {
        let (protocol, peer, device) = named(encrypt_with_bundle(None));
        assert_eq!((protocol.as_str(), peer.as_str(), device), ("Legacy OMEMO", CAROL, Some(7)));
    }

    #[test]
    fn a_malformed_bundle_names_the_protocol_contact_and_device() {
        let junk = Element::builder("bundle", NS).build();
        let (protocol, peer, device) = named(encrypt_with_bundle(Some(junk)));
        assert_eq!((protocol.as_str(), peer.as_str(), device), ("Legacy OMEMO", CAROL, Some(7)));
    }

    #[test]
    fn the_message_reads_clearly_and_is_kept_whole_for_the_message_details() {
        let error = Error::invalid_bundle("Legacy OMEMO", CAROL, Some(1_234_567_890));
        let text = error.to_string();
        assert_eq!(text, "No usable Legacy OMEMO keys from carol@example.invalid (device 1234567890); refresh device review");
        // The toast is short; the whole message, advice included, is what Message Details shows.
        assert_eq!(crate::text_safety::failed_send_toast(&text), "Message not sent. See Message Details.");
        assert_eq!(crate::text_safety::failure_reason(&text), text, "stored and shown unchanged");
        assert_eq!(
            Error::invalid_bundle("OMEMO2", CAROL, None).to_string(),
            "No usable OMEMO2 keys from carol@example.invalid; refresh device review"
        );
        assert_eq!(
            Error::InvalidBundle(None).to_string(),
            "A device's encryption keys are unavailable. Reconnect and retry.",
            "call sites that do not know the contact keep the old wording"
        );
    }
}

#[cfg(test)]
mod room_skip_tests {
    use super::tests::{approve, fixture};
    use super::*;
    use crate::{iq, protocol::Command};

    const PUBSUB: &str = "http://jabber.org/protocol/pubsub";
    const ALICE: &str = "alice@example.invalid";
    const BOB: &str = "bob@example.invalid";
    const CAROL: &str = "carol@example.invalid";
    const DAVE: &str = "dave@example.invalid";
    const ROOM: &str = "room@conference.example.invalid";

    fn items(node: &str, content: Element) -> Element {
        Element::builder("pubsub", PUBSUB)
            .append(
                Element::builder("items", PUBSUB)
                    .attr("node".try_into().unwrap(), node)
                    .append(Element::builder("item", PUBSUB).append(content).build())
                    .build(),
            )
            .build()
    }
    fn list(ids: &[u32]) -> Element {
        let mut list = Element::builder("list", NS).build();
        for id in ids {
            list.append_child(
                Element::builder("device", NS).attr("id".try_into().unwrap(), id.to_string()).build(),
            );
        }
        list
    }
    fn forbidden() -> std::result::Result<Option<Element>, iq::Error> {
        Err(iq::Error::Rejected { condition: "forbidden".into(), pubsub: None })
    }

    /// Alice sends to {Alice, Bob, Carol}. Bob is reachable and his key is `bob_trusted`; Carol's
    /// server refuses every request; `alice_ok` is whether Alice's own list can be read.
    fn send(skip: bool, members: &[&str], bob_trusted: bool, alice_ok: bool) -> Result<(Element, Vec<(String, String)>)> {
        let (mut store, journal, _) = fixture(ALICE, 101);
        let (_, _, bob_bundle) = fixture(BOB, 202);
        if bob_trusted {
            approve(&mut store, BOB, 202, &bob_bundle);
        }
        let (network, mut requests) = tokio::sync::mpsc::unbounded_channel();
        let mut legacy = Legacy {
            store,
            journal,
            pep: Pep::new(iq::Client::new(network), ALICE, 101).unwrap(),
            history_path: PathBuf::new(),
            _lock: File::open("/dev/null").unwrap(),
            repairs: BTreeMap::new(),
            pending_repairs: BTreeMap::new(),
            unreachable: BTreeMap::new(),
            discovery_backoff: BTreeMap::new(),
        };
        let members: BTreeSet<String> = members.iter().map(|m| m.to_string()).collect();
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        runtime.block_on(async {
            let server = async {
                while let Some(command) = requests.recv().await {
                    let Command::Iq { to, payload, reply, .. } = command else { continue };
                    let node = payload.children().next().and_then(|i| i.attr("node")).unwrap_or("").to_owned();
                    let answer = match (to.as_str(), node.as_str()) {
                        (ALICE, DEVICES) if alice_ok => Ok(Some(items(DEVICES, list(&[101])))),
                        (ALICE, _) => Err(iq::Error::Closed),
                        (BOB, DEVICES) => Ok(Some(items(DEVICES, list(&[202])))),
                        // Dave has no Legacy OMEMO devices at all.
                        (DAVE, DEVICES) => Ok(Some(items(DEVICES, list(&[])))),
                        (BOB, bundles) => Ok(Some(items(bundles, wire::bundle_xml(&bob_bundle)))),
                        _ => forbidden(),
                    };
                    let _ = reply.send(answer);
                }
            };
            tokio::select! {
                result = legacy.encrypt_room(ROOM, &members, "hi", "wire", skip) => result,
                _ = server => unreachable!("the fake server only ends when the client does"),
            }
        })
    }

    #[test]
    fn without_the_choice_one_refusing_member_blocks_the_whole_room_message() {
        let error = send(false, &[ALICE, BOB, CAROL], true, true).unwrap_err();
        assert!(matches!(error, Error::ContactAuthorization(ref who) if who == CAROL), "{error:?}");
    }

    #[test]
    fn with_the_choice_the_message_is_still_encrypted_for_everyone_reachable() {
        let (xml, skipped) = send(true, &[ALICE, BOB, CAROL], true, true).unwrap();
        assert_eq!(
            skipped,
            vec![(CAROL.to_owned(), "Legacy OMEMO keys require contact authorization from carol@example.invalid".to_owned())],
            "who was left out and why, as the message details will show it"
        );
        let text = String::from(&xml);
        assert!(text.contains("rid='202'"), "Bob's device is a recipient: {text}");
        assert!(text.contains("<encrypted") && text.contains("<payload>"), "an encrypted payload: {text}");
        assert!(!text.contains("<body"), "no plaintext body: {text}");
        assert!(!text.contains("rid='303'"), "no key for the skipped member");
    }

    /// A member with no devices at all (the usual "dead participant") makes automatic selection
    /// treat the room as unencryptable. Sending anyway encrypts for the members who do have keys.
    #[test]
    fn a_member_with_no_devices_blocks_the_room_until_the_choice_is_made() {
        let error = send(false, &[ALICE, BOB, DAVE], true, true).unwrap_err();
        assert!(matches!(error, Error::Unavailable(NO_DEVICES)), "{error:?}");
        let (xml, skipped) = send(true, &[ALICE, BOB, DAVE], true, true).unwrap();
        assert_eq!(skipped, vec![(DAVE.to_owned(), NO_DEVICES.to_owned())]);
        let text = String::from(&xml);
        assert!(text.contains("rid='202'") && text.contains("<encrypted") && !text.contains("<body"), "{text}");
    }

    /// The encryption check automatic selection runs. A member with no devices used to make it answer
    /// "not available", which for a room that had used encryption ended as "no longer offers
    /// encryption". With the choice it answers "can be encrypted".
    #[test]
    fn the_encryption_check_honours_the_choice() {
        use super::service::probe_outcome;
        let blocked = probe_outcome(send(false, &[ALICE, BOB, DAVE], true, true));
        assert!(matches!(blocked, Ok(false)), "a member with no devices: not available: {blocked:?}");
        let chosen = probe_outcome(send(true, &[ALICE, BOB, DAVE], true, true));
        assert!(matches!(chosen, Ok(true)), "with the choice the room can be encrypted: {chosen:?}");
        // Anything that should stop the send still does.
        let trust = probe_outcome(send(true, &[ALICE, BOB, CAROL], false, true));
        assert!(matches!(trust, Err(Error::TrustRequired)), "{trust:?}");
        let nobody = probe_outcome(send(true, &[ALICE, DAVE], true, true));
        assert!(matches!(nobody, Err(Error::Unavailable(NOBODY_REACHABLE))), "{nobody:?}");
        let refused = probe_outcome(send(false, &[ALICE, BOB, CAROL], true, true));
        assert!(matches!(refused, Err(Error::ContactAuthorization(_))), "{refused:?}");
    }

    #[test]
    fn nothing_is_sent_when_nobody_but_the_sender_could_be_reached() {
        let error = send(true, &[ALICE, CAROL], true, true).unwrap_err();
        assert!(matches!(error, Error::Unavailable(NOBODY_REACHABLE)), "{error:?}");
    }

    #[test]
    fn unreviewed_keys_are_never_skipped() {
        let error = send(true, &[ALICE, BOB, CAROL], false, true).unwrap_err();
        assert!(matches!(error, Error::TrustRequired), "{error:?}");
    }

    #[test]
    fn our_own_devices_are_never_skipped() {
        let error = send(true, &[ALICE, BOB, CAROL], true, false).unwrap_err();
        assert!(!matches!(error, Error::ContactAuthorization(_)));
        assert!(send(true, &[ALICE, BOB], true, false).is_err(), "an unreadable own list still blocks");
    }

    #[test]
    fn the_text_the_user_sees_is_recognised_as_skippable_only_for_skippable_errors() {
        for error in [
            Error::ContactAuthorization(CAROL.into()),
            Error::invalid_bundle("Legacy OMEMO", CAROL, Some(7)),
            Error::InvalidBundle(None),
            Error::Unavailable(NO_DEVICES),
            Error::Unavailable(UNSUPPORTED),
            Error::Unavailable(FETCH_FAILED),
        ] {
            assert!(skippable(&error), "{error:?}");
            if !matches!(error, Error::InvalidBundle(None)) {
                assert!(skippable_reason(&error.to_string()), "{error}");
            }
        }
        for error in [
            Error::TrustRequired,
            Error::OwnDeviceTrustRequired,
            Error::Changed,
            Error::Vault,
            Error::Unavailable("Legacy OMEMO is not ready"),
            Error::Unavailable(NOBODY_REACHABLE),
        ] {
            assert!(!skippable(&error), "{error:?}");
            assert!(!skippable_reason(&error.to_string()), "{error}");
        }
    }
}

/// Cost and behaviour of a room send, on a virtual clock so network latency and timeouts cost
/// nothing to run. `cargo test -- --ignored --nocapture room_send_cost` prints the numbers; the other
/// tests hold the budgets and the rules the speed-ups must keep.
#[cfg(test)]
mod room_perf {
    use super::tests::{approve, fixture};
    use super::*;
    use crate::{iq, protocol::Command};
    use std::sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    };

    const PUBSUB: &str = "http://jabber.org/protocol/pubsub";
    const ALICE: &str = "alice@example.invalid";
    const ROOM: &str = "room@conference.example.invalid";
    /// What one request to a healthy member costs: a network round trip.
    const RTT: Duration = Duration::from_millis(40);
    /// The request layer's own limit for a request that is never answered.
    const TIMEOUT: Duration = Duration::from_secs(25);

    fn items(node: &str, content: Element) -> Element {
        Element::builder("pubsub", PUBSUB)
            .append(
                Element::builder("items", PUBSUB)
                    .attr("node".try_into().unwrap(), node)
                    .append(Element::builder("item", PUBSUB).append(content).build())
                    .build(),
            )
            .build()
    }
    fn list(ids: &[u32]) -> Element {
        let mut list = Element::builder("list", NS).build();
        for id in ids {
            list.append_child(Element::builder("device", NS).attr("id".try_into().unwrap(), id.to_string()).build());
        }
        list
    }
    fn member(n: usize) -> String {
        format!("member{n:02}@example.invalid")
    }

    #[derive(Clone, Copy)]
    enum Step {
        /// One `encrypt_room` call; the flag is whether it may leave unreachable members out.
        Send(bool),
        /// Time passes with nothing happening.
        Wait(Duration),
        /// The unreachable members come back online.
        Revive,
    }
    use Step::*;

    struct Sent {
        /// Virtual time this call took.
        took: Duration,
        /// `None` when the call failed, else who was left out.
        skipped: Option<Vec<(String, String)>>,
    }
    struct Report {
        elapsed: Duration,
        requests: usize,
        sent: Vec<Sent>,
    }

    /// Run `steps` against a room of `total` members, the last `dead` of whom never answer until
    /// revived (a request to them waits out [`TIMEOUT`]).
    fn run(total: usize, dead: usize, steps: &[Step]) -> Report {
        let (mut store, journal, _) = fixture(ALICE, 101);
        let (_, _, shared) = fixture("shared@example.invalid", 202);
        let members: Vec<String> = (0..total).map(member).collect();
        for peer in &members {
            approve(&mut store, peer, 202, &shared);
        }
        let dead_set: Arc<Mutex<BTreeSet<String>>> = Arc::new(Mutex::new(members.iter().rev().take(dead).cloned().collect()));
        let mut room: BTreeSet<String> = members.iter().cloned().collect();
        room.insert(ALICE.to_owned());

        let (network, mut requests) = tokio::sync::mpsc::unbounded_channel();
        let mut legacy = Legacy {
            store,
            journal,
            pep: Pep::new(iq::Client::new(network), ALICE, 101).unwrap(),
            history_path: PathBuf::new(),
            _lock: File::open("/dev/null").unwrap(),
            repairs: BTreeMap::new(),
            pending_repairs: BTreeMap::new(),
            unreachable: BTreeMap::new(),
            discovery_backoff: BTreeMap::new(),
        };
        let count = Arc::new(AtomicUsize::new(0));
        let held = Arc::new(Mutex::new(Vec::new()));
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().start_paused(true).build().unwrap();
        runtime.block_on(async {
            let server = {
                let (count, held, dead_set) = (count.clone(), held.clone(), dead_set.clone());
                async move {
                    while let Some(command) = requests.recv().await {
                        let Command::Iq { to, payload, reply, .. } = command else { continue };
                        count.fetch_add(1, Ordering::SeqCst);
                        if dead_set.lock().unwrap().contains(&to) {
                            // A dead server never answers; the sender's own timeout ends the wait.
                            held.lock().unwrap().push(reply);
                            continue;
                        }
                        let node = payload.children().next().and_then(|i| i.attr("node")).unwrap_or("").to_owned();
                        let answer = if to == ALICE {
                            items(DEVICES, list(&[101]))
                        } else if node == DEVICES {
                            items(DEVICES, list(&[202]))
                        } else {
                            items(&node, wire::bundle_xml(&shared))
                        };
                        // Replies are concurrent, as on a real server.
                        tokio::spawn(async move {
                            tokio::time::sleep(RTT).await;
                            let _ = reply.send(Ok(Some(answer)));
                        });
                    }
                }
            };
            let work = async {
                let start = tokio::time::Instant::now();
                let mut sent = vec![];
                for step in steps {
                    match *step {
                        Send(skip) => {
                            let began = tokio::time::Instant::now();
                            let skipped = legacy.encrypt_room(ROOM, &room, "hi", "wire", skip).await.ok().map(|(_, left_out)| left_out);
                            sent.push(Sent { took: began.elapsed(), skipped });
                        }
                        Wait(time) => tokio::time::sleep(time).await,
                        Revive => dead_set.lock().unwrap().clear(),
                    }
                }
                Report { elapsed: start.elapsed(), requests: count.load(Ordering::SeqCst), sent }
            };
            tokio::select! {
                report = work => report,
                _ = server => unreachable!("the fake server only ends when the client does"),
            }
        })
    }

    /// Prints what a room send costs. Not an assertion: the numbers the speed-ups are judged by.
    #[test]
    #[ignore = "a measurement, run with --nocapture"]
    fn room_send_cost() {
        for (label, dead, steps) in [
            ("20 members, all reachable, one pass", 0, &[Send(true)][..]),
            ("20 members, 3 unreachable, one pass", 3, &[Send(true)][..]),
            ("20 members, 3 unreachable, probe + send (2 passes)", 3, &[Send(true), Send(true)][..]),
            ("20 members, 3 unreachable, 5 messages in a row", 3, &[Send(true); 5][..]),
            ("20 members, 3 unreachable, fail, then Send Anyway", 3, &[Send(false), Send(true)][..]),
        ] {
            let report = run(20, dead, steps);
            eprintln!(
                "ROOMPERF {label}: {:>8.1} s virtual, {:>4} requests",
                report.elapsed.as_secs_f64(),
                report.requests,
            );
        }
    }

    // ---- budgets: what a user waits, with margin over one timeout ----

    #[test]
    fn a_room_waits_for_its_slowest_member_not_for_the_sum_of_them() {
        // Three members who never answer cost one timeout together, not three.
        let report = run(10, 3, &[Send(true)]);
        assert!(report.elapsed < TIMEOUT + Duration::from_secs(2), "{:?}", report.elapsed);
        assert_eq!(report.sent[0].skipped.as_ref().map(Vec::len), Some(3));
        // Everyone reachable: a few round trips, not one per member.
        let report = run(10, 0, &[Send(true)]);
        assert!(report.elapsed < Duration::from_secs(1), "{:?}", report.elapsed);
    }

    #[test]
    fn a_dead_member_is_not_waited_for_again_while_skipping_is_chosen() {
        let report = run(10, 3, &[Send(true), Send(true), Send(true), Send(true)]);
        let (first, later) = report.sent.split_first().unwrap();
        assert!(first.took >= TIMEOUT, "the first call finds out: {:?}", first.took);
        for call in later {
            assert!(call.took < Duration::from_secs(1), "later calls do not wait: {:?}", call.took);
            assert_eq!(call.skipped.as_ref().map(Vec::len), Some(3), "and still report who was left out");
            assert_eq!(call.skipped, first.skipped, "with the same reasons");
        }
    }

    #[test]
    fn send_anyway_after_a_failure_does_not_wait_a_second_time() {
        let report = run(10, 2, &[Send(false), Send(true)]);
        assert!(report.sent[0].skipped.is_none(), "without the choice the send fails");
        assert!(report.sent[0].took >= TIMEOUT);
        assert!(report.sent[1].took < Duration::from_secs(1), "{:?}", report.sent[1].took);
        assert_eq!(report.sent[1].skipped.as_ref().map(Vec::len), Some(2));
    }

    // ---- rules the speed-ups must keep ----

    #[test]
    fn remembering_a_dead_member_never_lets_a_send_skip_them_without_the_choice() {
        // The memory exists, but a call that did not choose to skip must still try, wait and fail.
        let report = run(10, 2, &[Send(false), Send(false)]);
        for call in &report.sent {
            assert!(call.skipped.is_none(), "still an error, never a silent skip");
            assert!(call.took >= TIMEOUT, "and it really asked again: {:?}", call.took);
        }
    }

    #[test]
    fn the_memory_expires_so_a_recovered_member_gets_messages_again() {
        let report = run(
            10,
            2,
            &[Send(true), Revive, Send(true), Wait(UNREACHABLE_MEMORY + Duration::from_secs(1)), Send(true)],
        );
        assert_eq!(report.sent[0].skipped.as_ref().map(Vec::len), Some(2));
        assert_eq!(
            report.sent[1].skipped.as_ref().map(Vec::len),
            Some(2),
            "still remembered a moment later, so still left out (and reported)"
        );
        assert_eq!(report.sent[2].skipped.as_ref().map(Vec::len), Some(0), "after the memory ends they are asked again and included");
    }

    #[test]
    fn our_own_account_is_never_remembered_as_skippable() {
        // Nobody else is reachable and our own devices are fine: the send is refused, not sent to
        // ourselves alone, with or without a remembered member.
        let report = run(3, 3, &[Send(true), Send(true)]);
        for call in &report.sent {
            assert!(call.skipped.is_none(), "refused when nobody else could be reached");
        }
    }
}
