//! RFC 6121 authorization, with mobile's explicit device-local approval policy.
use crate::{
    iq,
    protocol::{BackendEvent, Command, Envelope},
    storage::Store,
};
use rusqlite::{OptionalExtension, params};
use std::{
    collections::{BTreeMap, BTreeSet, HashSet},
    sync::{Arc, Mutex, Weak, mpsc},
    time::Duration,
};
use tokio::sync::{Mutex as AsyncMutex, mpsc as channel, oneshot};
use tokio_xmpp::{jid::Jid, minidom::Element};
const CLIENT: &str = "jabber:client";
pub const ROSTER: &str = "jabber:iq:roster";
type Result<T> = std::result::Result<T, String>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Request,
    Allow,
    Accept,
    Deny,
    Cancel,
    Revoke,
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Item {
    pub jid: String,
    pub name: String,
    pub subscription: String,
    pub ask: bool,
    pub approved: bool,
    pub groups: Vec<String>,
}
impl Item {
    pub fn sees_their_presence(&self) -> bool {
        matches!(self.subscription.as_str(), "to" | "both")
    }
    pub fn shares_own_presence(&self) -> bool {
        matches!(self.subscription.as_str(), "from" | "both")
    }
    pub fn removed(&self) -> bool {
        self.subscription == "remove"
    }
    fn stanza(&self) -> Element {
        let mut item = Element::builder("item", ROSTER).attr("jid".try_into().unwrap(), &self.jid);
        if !self.name.is_empty() {
            item = item.attr("name".try_into().unwrap(), &self.name);
        }
        for group in &self.groups {
            item = item.append(
                Element::builder("group", ROSTER)
                    .append(group.as_str())
                    .build(),
            );
        }
        Element::builder("query", ROSTER)
            .append(item.build())
            .build()
    }
}
#[derive(Clone, Debug)]
pub struct RosterUpdate {
    pub items: Vec<Item>,
    pub full: bool,
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct State {
    pub listed: bool,
    pub sees_their_presence: bool,
    pub shares_own_presence: bool,
    pub outgoing_pending: bool,
    pub pre_approved: bool,
    pub incoming_pending: bool,
    pub allow_requests: bool,
    pub loaded: bool,
}
pub fn valid_peer(owner: &str, peer: &str) -> bool {
    peer != owner && crate::protocol::bare(peer).ok().as_deref() == Some(peer)
}
pub fn parse_roster(query: &Element, full: bool) -> Result<RosterUpdate> {
    struct Budget(usize);
    impl std::io::Write for Budget {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0 = self
                .0
                .checked_add(bytes.len())
                .filter(|size| *size <= 4 * 1024 * 1024)
                .ok_or_else(|| std::io::Error::other("Contact list is too large"))?;
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    if !query.is("query", ROSTER) || query.write_to(&mut Budget(0)).is_err() {
        return Err("Invalid or oversized contact list response".into());
    }
    let mut items = Vec::new();
    let mut seen = HashSet::new();
    for child in query.children().filter(|child| child.is("item", ROSTER)) {
        let address = child
            .attr("jid")
            .ok_or("A contact list item has no address")?;
        let jid = address
            .parse::<Jid>()
            .map_err(|_| "Invalid contact address")?;
        if jid.resource().is_some()
            || jid.to_string() != address
            || !seen.insert(address.to_string())
        {
            return Err("Invalid or duplicate contact list item".into());
        }
        let subscription = child.attr("subscription").unwrap_or("none");
        if !matches!(subscription, "none" | "to" | "from" | "both" | "remove")
            || full && subscription == "remove"
        {
            return Err("Invalid contact subscription".into());
        }
        let ask = match child.attr("ask") {
            None => false,
            Some("subscribe") => true,
            _ => return Err("Invalid pending subscription".into()),
        };
        let approved = match child.attr("approved") {
            None | Some("false" | "0") => false,
            Some("true" | "1") => true,
            _ => return Err("Invalid subscription approval".into()),
        };
        items.push(Item {
            jid: address.into(),
            name: child.attr("name").unwrap_or("").into(),
            subscription: subscription.into(),
            ask,
            approved,
            groups: child
                .children()
                .filter(|group| group.is("group", ROSTER))
                .map(Element::text)
                .collect(),
        });
    }
    if !full && items.len() != 1 {
        return Err("Invalid contact list update".into());
    }
    Ok(RosterUpdate { items, full })
}
impl Store {
    fn init_contact_access(&self) -> rusqlite::Result<()> {
        self.db.execute_batch("CREATE TABLE IF NOT EXISTS contact_allow(account TEXT NOT NULL REFERENCES accounts(jid) ON DELETE CASCADE,peer TEXT NOT NULL,PRIMARY KEY(account,peer));")
    }
    pub fn set_contact_allow(
        &self,
        owner: &str,
        peer: &str,
        allowed: bool,
    ) -> rusqlite::Result<()> {
        if !valid_peer(owner, peer) {
            return Err(rusqlite::Error::InvalidQuery);
        }
        self.init_contact_access()?;
        if allowed {
            self.db.execute(
                "INSERT OR IGNORE INTO contact_allow VALUES(?1,?2)",
                params![owner, peer],
            )?;
        } else {
            self.db.execute(
                "DELETE FROM contact_allow WHERE account=?1 AND peer=?2",
                params![owner, peer],
            )?;
        }
        Ok(())
    }
    pub fn contact_allowed(&self, owner: &str, peer: &str) -> rusqlite::Result<bool> {
        self.init_contact_access()?;
        self.db
            .query_row(
                "SELECT 1 FROM contact_allow WHERE account=?1 AND peer=?2",
                params![owner, peer],
                |_| Ok(true),
            )
            .optional()
            .map(|value| value.unwrap_or(false))
    }
    fn contact_snapshot(
        &self,
        owner: &str,
    ) -> rusqlite::Result<(BTreeSet<String>, BTreeSet<String>)> {
        self.init_contact_access()?;
        let mut allowed = self
            .db
            .prepare("SELECT peer FROM contact_allow WHERE account=?1")?;
        let mut pending = self
            .db
            .prepare("SELECT peer FROM pending_contacts WHERE account=?1")?;
        Ok((
            allowed
                .query_map([owner], |row| row.get(0))?
                .collect::<rusqlite::Result<_>>()?,
            pending
                .query_map([owner], |row| row.get(0))?
                .collect::<rusqlite::Result<_>>()?,
        ))
    }
}
struct FullRequest {
    serial: u64,
    pushes: BTreeMap<String, Item>,
}
#[derive(Default)]
struct Data {
    epoch: u64,
    online: bool,
    loaded: bool,
    items: BTreeMap<String, Item>,
    allowed: BTreeSet<String>,
    pending: BTreeSet<String>,
    resources: BTreeMap<String, HashSet<String>>,
    jobs: BTreeMap<String, Weak<AsyncMutex<()>>>,
    store_error: Option<String>,
    full_serial: u64,
    full_request: Option<FullRequest>,
}
#[derive(Clone)]
pub struct Manager {
    owner: String,
    generation: u64,
    events: mpsc::Sender<Envelope>,
    data: Arc<Mutex<Data>>,
    refresh_gate: Arc<AsyncMutex<()>>,
}
struct RefreshCleanup {
    manager: Manager,
    epoch: u64,
}
impl Drop for RefreshCleanup {
    fn drop(&mut self) {
        self.manager.clear_full(self.epoch, None);
    }
}
impl Manager {
    pub async fn new(owner: String, generation: u64, events: mpsc::Sender<Envelope>) -> Self {
        let account = owner.clone();
        let stored =
            tokio::task::spawn_blocking(move || Store::open(false)?.contact_snapshot(&account))
                .await;
        let mut data = Data::default();
        match stored {
            Ok(Ok((allowed, pending))) => {
                data.allowed = allowed;
                data.pending = pending;
            }
            _ => {
                data.store_error =
                    Some("Saved contact access preferences could not be loaded".into())
            }
        }
        Self {
            owner,
            generation,
            events,
            data: Arc::new(Mutex::new(data)),
            refresh_gate: Arc::new(AsyncMutex::new(())),
        }
    }
    fn emit(&self, event: BackendEvent) {
        let _ = self
            .events
            .send((self.generation, self.owner.clone(), event));
    }
    pub fn error(&self, error: String) {
        self.emit(BackendEvent::ContactAccessError(error));
    }
    pub fn online(&self) -> u64 {
        let mut data = self.data.lock().unwrap();
        data.epoch = data.epoch.wrapping_add(1);
        data.online = true;
        data.loaded = false;
        data.full_request = None;
        data.resources.clear();
        let epoch = data.epoch;
        let error = data.store_error.clone();
        drop(data);
        if let Some(error) = error {
            self.error(error);
        }
        epoch
    }
    pub fn disconnect(&self) {
        let peers = {
            let mut data = self.data.lock().unwrap();
            data.online = false;
            data.loaded = false;
            data.full_request = None;
            data.epoch = data.epoch.wrapping_add(1);
            data.resources.clear();
            data.items.keys().cloned().collect::<Vec<_>>()
        };
        for peer in peers {
            self.emit(BackendEvent::Presence {
                peer,
                online: false,
                visible: false,
            });
        }
    }
    pub fn epoch(&self) -> u64 {
        self.data.lock().unwrap().epoch
    }
    pub fn valid(&self, epoch: u64) -> bool {
        let data = self.data.lock().unwrap();
        data.online && data.epoch == epoch
    }
    fn require(&self, epoch: u64) -> Result<()> {
        if self.valid(epoch) {
            Ok(())
        } else {
            Err("Reconnect before changing contact access".into())
        }
    }
    pub fn state(&self, peer: &str) -> Result<State> {
        if !valid_peer(&self.owner, peer) {
            return Err("A contact's bare XMPP address is required".into());
        }
        let data = self.data.lock().unwrap();
        let item = data.items.get(peer);
        Ok(State {
            listed: item.is_some(),
            sees_their_presence: item.is_some_and(Item::sees_their_presence),
            shares_own_presence: item.is_some_and(Item::shares_own_presence),
            outgoing_pending: item.is_some_and(|item| item.ask),
            pre_approved: item.is_some_and(|item| item.approved),
            incoming_pending: data.pending.contains(peer)
                && !item.is_some_and(Item::shares_own_presence),
            allow_requests: data.allowed.contains(peer),
            loaded: data.online && data.loaded,
        })
    }
    fn lock_for(&self, peer: &str) -> Arc<AsyncMutex<()>> {
        let mut data = self.data.lock().unwrap();
        data.jobs.retain(|_, lock| lock.strong_count() > 0);
        if let Some(lock) = data.jobs.get(peer).and_then(Weak::upgrade) {
            return lock;
        }
        let lock = Arc::new(AsyncMutex::new(()));
        data.jobs.insert(peer.into(), Arc::downgrade(&lock));
        lock
    }
    pub fn begin_full(&self, epoch: u64) -> Result<u64> {
        self.require(epoch)?;
        let mut data = self.data.lock().unwrap();
        if data.full_request.is_some() {
            return Err("A contact list refresh is already running".into());
        }
        data.full_serial = data.full_serial.wrapping_add(1);
        let serial = data.full_serial;
        data.full_request = Some(FullRequest {
            serial,
            pushes: BTreeMap::new(),
        });
        Ok(serial)
    }
    pub fn clear_full(&self, epoch: u64, serial: Option<u64>) {
        let mut data = self.data.lock().unwrap();
        if data.epoch == epoch
            && serial.is_none_or(|serial| {
                data.full_request
                    .as_ref()
                    .is_some_and(|request| request.serial == serial)
            })
        {
            data.full_request = None;
        }
    }
    pub fn finish_full(&self, epoch: u64, serial: u64, update: RosterUpdate) -> Result<()> {
        self.apply_update(epoch, update, Some(serial))
    }
    pub fn apply_roster(&self, epoch: u64, update: RosterUpdate) -> Result<()> {
        self.apply_update(epoch, update, None)
    }
    fn apply_update(
        &self,
        epoch: u64,
        mut update: RosterUpdate,
        serial: Option<u64>,
    ) -> Result<()> {
        self.require(epoch)?;
        let presence = {
            let mut data = self.data.lock().unwrap();
            if !data.online || data.epoch != epoch {
                return Err("Connection changed".into());
            }
            if let Some(serial) = serial {
                if data
                    .full_request
                    .as_ref()
                    .is_none_or(|request| request.serial != serial)
                {
                    return Ok(());
                }
                let request = data.full_request.take().unwrap();
                let mut items: BTreeMap<_, _> = update
                    .items
                    .into_iter()
                    .map(|item| (item.jid.clone(), item))
                    .collect();
                for item in request.pushes.into_values() {
                    if item.removed() {
                        items.remove(&item.jid);
                    } else {
                        items.insert(item.jid.clone(), item);
                    }
                }
                update.items = items.into_values().collect();
            } else if !update.full
                && let Some(request) = data.full_request.as_mut()
            {
                for item in &update.items {
                    request.pushes.insert(item.jid.clone(), item.clone());
                }
            }
            let mut changed: BTreeSet<String> =
                update.items.iter().map(|item| item.jid.clone()).collect();
            if update.full {
                changed.extend(data.items.keys().cloned());
                data.items.clear();
                data.loaded = true;
            }
            for item in &update.items {
                if item.removed() {
                    data.items.remove(&item.jid);
                } else {
                    data.items.insert(item.jid.clone(), item.clone());
                }
                if item.shares_own_presence() {
                    data.pending.remove(&item.jid);
                }
            }
            let allowed: HashSet<_> = data
                .items
                .values()
                .filter(|item| item.sees_their_presence())
                .map(|item| item.jid.clone())
                .collect();
            if data.loaded {
                data.resources.retain(|peer, _| allowed.contains(peer));
            }
            changed
                .into_iter()
                .map(|peer| {
                    let visible = data.loaded && allowed.contains(&peer);
                    let online = visible
                        && data
                            .resources
                            .get(&peer)
                            .is_some_and(|resources| !resources.is_empty());
                    (peer, visible, online)
                })
                .collect::<Vec<_>>()
        };
        self.emit(BackendEvent::Roster(update));
        for (peer, visible, online) in presence {
            self.emit(BackendEvent::Presence {
                peer,
                visible,
                online,
            });
        }
        Ok(())
    }
    pub fn presence(&self, root: &Element) {
        let from = root.attr("from").unwrap_or("");
        let Ok(jid) = from.parse::<Jid>() else {
            return;
        };
        let peer = jid.to_bare().to_string();
        if !valid_peer(&self.owner, &peer)
            || jid.to_string() != from
            || root.attr("to").is_some_and(|to| {
                to.parse::<Jid>()
                    .ok()
                    .is_none_or(|jid| jid.to_bare().as_str() != self.owner)
            })
            || root.children().any(|child| {
                matches!(
                    child.ns().as_str(),
                    "urn:xmpp:delay"
                        | "jabber:x:delay"
                        | "urn:xmpp:carbons:2"
                        | "urn:xmpp:forward:0"
                )
            })
        {
            return;
        }
        let kind = root.attr("type").unwrap_or("");
        if !matches!(kind, "" | "unavailable") {
            return;
        }
        let (visible, online) = {
            let mut data = self.data.lock().unwrap();
            if !data.online {
                return;
            }
            let visible =
                data.loaded && data.items.get(&peer).is_some_and(Item::sees_their_presence);
            if !data.loaded || visible {
                let resources = data.resources.entry(peer.clone()).or_default();
                if kind.is_empty() {
                    resources.insert(from.into());
                } else {
                    resources.remove(from);
                }
            } else {
                data.resources.remove(&peer);
            }
            (
                visible,
                visible
                    && data
                        .resources
                        .get(&peer)
                        .is_some_and(|resources| !resources.is_empty()),
            )
        };
        self.emit(BackendEvent::Presence {
            peer,
            visible,
            online,
        });
    }
    pub fn roster_write(&self, payload: Element) -> Element {
        let peer = payload
            .get_child("item", ROSTER)
            .and_then(|item| item.attr("jid"));
        peer.and_then(|peer| self.data.lock().unwrap().items.get(peer).cloned())
            .map_or(payload, |item| item.stanza())
    }
    pub fn revoked_by_contact(&self, peer: &str) {
        let item = self.data.lock().unwrap().items.get(peer).cloned();
        if let Some(mut item) = item {
            item.subscription = if item.shares_own_presence() {
                "from"
            } else {
                "none"
            }
            .into();
            item.ask = false;
            let _ = self.apply_roster(
                self.epoch(),
                RosterUpdate {
                    items: vec![item],
                    full: false,
                },
            );
        }
    }
    pub fn subscription(&self, peer: &str, incoming: bool) {
        let mut data = self.data.lock().unwrap();
        if incoming {
            data.pending.insert(peer.into());
        } else {
            data.pending.remove(peer);
        }
    }
    fn clear_pending(&self, peer: &str) {
        self.data.lock().unwrap().pending.remove(peer);
        self.emit(BackendEvent::RequestCancelled(peer.into()));
    }
    async fn request(
        &self,
        network: &channel::UnboundedSender<Command>,
        epoch: u64,
        kind: iq::Kind,
        payload: Element,
    ) -> Result<Option<Element>> {
        self.require(epoch)?;
        let (reply, received) = oneshot::channel();
        network
            .send(Command::ContactIq {
                epoch,
                kind,
                payload,
                reply,
            })
            .map_err(|_| "Connection closed")?;
        tokio::time::timeout(Duration::from_secs(25), received)
            .await
            .map_err(|_| "Contact request timed out")?
            .map_err(|_| "Connection closed")?
            .map_err(|error| match error {
                iq::Error::Closed => {
                    "The connection closed before the contact request was confirmed".into()
                }
                iq::Error::Timeout => {
                    "The server did not confirm the contact request in time".into()
                }
                iq::Error::Busy => "The request queue is busy; try again".into(),
                iq::Error::Invalid => "The server returned an invalid contact response".into(),
                iq::Error::Rejected { condition, .. } => {
                    format!("The server rejected the contact request: {condition}")
                }
            })
    }
    pub async fn refresh(
        &self,
        network: &channel::UnboundedSender<Command>,
        epoch: u64,
    ) -> Result<()> {
        let _gate = self.refresh_gate.clone().lock_owned().await;
        self.require(epoch)?;
        let _cleanup = RefreshCleanup {
            manager: self.clone(),
            epoch,
        };
        let response = self
            .request(
                network,
                epoch,
                iq::Kind::Get,
                Element::builder("query", ROSTER).build(),
            )
            .await?
            .ok_or("Missing contact list response")?;
        parse_roster(&response, true)?;
        self.require(epoch)
    }
    async fn wire(
        &self,
        network: &channel::UnboundedSender<Command>,
        epoch: u64,
        peer: &str,
        kind: &str,
    ) -> Result<()> {
        self.require(epoch)?;
        let (reply, received) = oneshot::channel();
        network
            .send(Command::ContactWire {
                epoch,
                peer: peer.into(),
                kind: kind.into(),
                reply,
            })
            .map_err(|_| "Connection closed")?;
        received
            .await
            .map_err(|_| "Connection closed")?
            .then_some(())
            .ok_or_else(|| "Contact request delivery was not confirmed".into())
    }
    async fn ensure_entry(
        &self,
        network: &channel::UnboundedSender<Command>,
        epoch: u64,
        peer: &str,
    ) -> Result<()> {
        if !self.data.lock().unwrap().loaded {
            self.refresh(network, epoch).await?;
        }
        if !self.state(peer)?.listed {
            let item = Item {
                jid: peer.into(),
                ..Default::default()
            };
            self.request(network, epoch, iq::Kind::Set, item.stanza())
                .await?;
            self.refresh(network, epoch).await?;
        }
        Ok(())
    }
    async fn policy(&self, peer: &str, allowed: bool) -> Result<()> {
        let (owner, peer) = (self.owner.clone(), peer.to_string());
        let stored_peer = peer.clone();
        tokio::task::spawn_blocking(move || {
            Store::open(false)?.set_contact_allow(&owner, &stored_peer, allowed)
        })
        .await
        .map_err(|_| "Contact preference could not be saved")?
        .map_err(|_| "Contact preference could not be saved")?;
        let mut data = self.data.lock().unwrap();
        if allowed {
            data.allowed.insert(peer);
        } else {
            data.allowed.remove(&peer);
        }
        data.store_error = None;
        Ok(())
    }
    pub async fn act(
        &self,
        network: &channel::UnboundedSender<Command>,
        epoch: u64,
        peer: &str,
        action: Action,
    ) -> Result<State> {
        if !valid_peer(&self.owner, peer) {
            return Err("A contact's bare XMPP address is required".into());
        }
        let _guard = self.lock_for(peer).lock_owned().await;
        self.require(epoch)?;
        if matches!(action, Action::Allow | Action::Deny | Action::Revoke) {
            self.policy(peer, action == Action::Allow).await?;
            self.require(epoch)?;
        }
        if matches!(action, Action::Request | Action::Allow | Action::Accept) {
            self.ensure_entry(network, epoch, peer).await?;
        }
        match action {
            Action::Allow => {
                if self.state(peer)?.incoming_pending {
                    self.wire(network, epoch, peer, "subscribed").await?;
                    self.clear_pending(peer);
                }
            }
            Action::Accept => {
                self.wire(network, epoch, peer, "subscribed").await?;
                self.clear_pending(peer);
                let state = self.state(peer)?;
                if !state.sees_their_presence && !state.outgoing_pending {
                    self.wire(network, epoch, peer, "subscribe").await?;
                }
            }
            Action::Request => {
                let state = self.state(peer)?;
                if !state.sees_their_presence && !state.outgoing_pending {
                    self.wire(network, epoch, peer, "subscribe").await?;
                }
            }
            Action::Deny | Action::Revoke => {
                self.wire(network, epoch, peer, "unsubscribed").await?;
                self.clear_pending(peer);
                if action == Action::Revoke {
                    self.refresh(network, epoch).await?;
                    let item = self.data.lock().unwrap().items.get(peer).cloned();
                    if let Some(item) = item {
                        self.request(network, epoch, iq::Kind::Set, item.stanza())
                            .await?;
                    }
                }
            }
            Action::Cancel => self.wire(network, epoch, peer, "unsubscribe").await?,
        }
        self.refresh(network, epoch).await?;
        self.state(peer)
    }
    pub async fn approve_if_allowed(
        &self,
        network: &channel::UnboundedSender<Command>,
        epoch: u64,
        peer: &str,
    ) -> Result<()> {
        let _guard = self.lock_for(peer).lock_owned().await;
        self.require(epoch)?;
        let state = self.state(peer)?;
        if !state.allow_requests || !state.incoming_pending {
            return Ok(());
        }
        self.ensure_entry(network, epoch, peer).await?;
        let state = self.state(peer)?;
        if !state.allow_requests || !state.incoming_pending {
            return Ok(());
        }
        self.wire(network, epoch, peer, "subscribed").await?;
        self.clear_pending(peer);
        self.refresh(network, epoch).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn xml(source: &str) -> Element {
        source.parse().unwrap()
    }
    #[test]
    fn roster_authority_policy_isolation_and_presence_visibility() {
        let update = parse_roster(&xml("<query xmlns='jabber:iq:roster'><item jid='peer@example.org' name='Alias' subscription='both' ask='subscribe' approved='true'><group>Friends</group></item></query>"), true).unwrap();
        assert!(update.items[0].sees_their_presence());
        assert!(update.items[0].shares_own_presence());
        assert!(update.items[0].ask && update.items[0].approved);
        let set = update.items[0].stanza();
        let item = set.get_child("item", ROSTER).unwrap();
        assert_eq!(item.attr("name"), Some("Alias"));
        assert_eq!(item.get_child("group", ROSTER).unwrap().text(), "Friends");
        assert!(item.attr("subscription").is_none() && item.attr("approved").is_none());
        for source in [
            "<query xmlns='jabber:iq:roster'><item jid='peer@example.org/resource'/></query>",
            "<query xmlns='jabber:iq:roster'><item jid='peer@example.org'/><item jid='peer@example.org'/></query>",
            "<query xmlns='jabber:iq:roster'><item jid='peer@example.org' ask='anything'/></query>",
            "<query xmlns='jabber:iq:roster'><item jid='peer@example.org' subscription='remove'/></query>",
        ] {
            assert!(parse_roster(&xml(source), true).is_err());
        }
        assert!(parse_roster(&xml("<query xmlns='jabber:iq:roster'/>"), false).is_err());
        let store = Store::open(true).unwrap();
        for owner in ["owner@example.org", "other@example.org"] {
            store
                .save_account(&crate::model::Account {
                    jid: owner.into(),
                    host: String::new(),
                    port: 5222,
                    direct_tls: false,
                })
                .unwrap();
            store
                .set_contact_allow(owner, "peer@example.org", true)
                .unwrap();
        }
        assert!(
            store
                .set_contact_allow("owner@example.org", "owner@example.org", true)
                .is_err()
        );
        assert!(
            store
                .set_contact_allow("owner@example.org", "peer@example.org/resource", true)
                .is_err()
        );
        store.forget("owner@example.org").unwrap();
        assert!(
            !store
                .contact_allowed("owner@example.org", "peer@example.org")
                .unwrap()
        );
        assert!(
            store
                .contact_allowed("other@example.org", "peer@example.org")
                .unwrap()
        );
        let (events, receive) = mpsc::channel();
        let manager = Manager {
            owner: "owner@example.org".into(),
            generation: 1,
            events,
            data: Arc::new(Mutex::new(Data::default())),
            refresh_gate: Arc::new(AsyncMutex::new(())),
        };
        let epoch = manager.online();
        let presence = |resource: &str, kind: &str| {
            xml(&format!(
                "<presence xmlns='jabber:client' from='peer@example.org/{resource}' to='owner@example.org/desktop'{kind}/>"
            ))
        };
        manager.presence(&presence("one", ""));
        assert!(matches!(
            receive.recv().unwrap().2,
            BackendEvent::Presence {
                visible: false,
                online: false,
                ..
            }
        ));
        manager.apply_roster(epoch, update).unwrap();
        assert!(manager.state("peer@example.org").unwrap().listed);
        assert!(matches!(receive.recv().unwrap().2, BackendEvent::Roster(_)));
        assert!(matches!(
            receive.recv().unwrap().2,
            BackendEvent::Presence {
                visible: true,
                online: true,
                ..
            }
        ));
        manager.presence(&presence("two", ""));
        let _ = receive.recv().unwrap();
        manager.presence(&presence("one", " type='unavailable'"));
        assert!(matches!(
            receive.recv().unwrap().2,
            BackendEvent::Presence {
                visible: true,
                online: true,
                ..
            }
        ));
        manager.presence(&presence("two", " type='unavailable'"));
        assert!(matches!(
            receive.recv().unwrap().2,
            BackendEvent::Presence {
                visible: true,
                online: false,
                ..
            }
        ));
        manager.revoked_by_contact("peer@example.org");
        let _ = receive.recv().unwrap();
        assert!(matches!(
            receive.recv().unwrap().2,
            BackendEvent::Presence {
                visible: false,
                online: false,
                ..
            }
        ));
        let serial = manager.begin_full(epoch).unwrap();
        manager.apply_roster(epoch, parse_roster(&xml("<query xmlns='jabber:iq:roster'><item jid='peer@example.org' subscription='remove'/></query>"), false).unwrap()).unwrap();
        manager.apply_roster(epoch, parse_roster(&xml("<query xmlns='jabber:iq:roster'><item jid='new@example.org' name='Fresh' subscription='none'/></query>"), false).unwrap()).unwrap();
        manager.finish_full(epoch, serial, parse_roster(&xml("<query xmlns='jabber:iq:roster'><item jid='peer@example.org' name='Stale' subscription='both'/></query>"), true).unwrap()).unwrap();
        assert!(!manager.state("peer@example.org").unwrap().listed);
        assert!(manager.state("new@example.org").unwrap().listed);
        let canonical = receive
            .try_iter()
            .filter_map(|(_, _, event)| match event {
                BackendEvent::Roster(update) if update.full => Some(update),
                _ => None,
            })
            .last()
            .unwrap();
        assert_eq!(canonical.items.len(), 1);
        assert_eq!(canonical.items[0].name, "Fresh");
        let old = manager.begin_full(epoch).unwrap();
        manager.clear_full(epoch, Some(old));
        let current = manager.begin_full(epoch).unwrap();
        manager
            .finish_full(
                epoch,
                old,
                parse_roster(&xml("<query xmlns='jabber:iq:roster'/>"), true).unwrap(),
            )
            .unwrap();
        assert!(
            manager
                .data
                .lock()
                .unwrap()
                .full_request
                .as_ref()
                .is_some_and(|request| request.serial == current)
        );
        manager.clear_full(epoch, Some(current));
        manager.disconnect();
        assert!(!manager.valid(epoch));
        assert!(!manager.state("peer@example.org").unwrap().loaded);
    }
}
