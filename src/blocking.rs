// SPDX-License-Identifier: GPL-3.0-or-later
//! Server-enforced XEP-0191 blocking; follows mobile's acknowledged offline cache and push replay.
use crate::{
    iq,
    protocol::{BackendEvent, Command, Envelope},
    storage::Store,
};
use rusqlite::Result as SqlResult;
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, Mutex, mpsc},
};
use tokio::sync::{Mutex as AsyncMutex, mpsc as channel, oneshot};
use tokio_xmpp::{jid::Jid, minidom::Element};
pub const NS: &str = "urn:xmpp:blocking";
const CLIENT: &str = "jabber:client";
const DISCO: &str = "http://jabber.org/protocol/disco#info";
const MAX: usize = 10_000;
const BYTES: usize = 2 * 1024 * 1024;
static CACHE_IO: Mutex<BTreeMap<String, String>> = Mutex::new(BTreeMap::new());
type Result<T> = std::result::Result<T, String>;
#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    pub jids: Option<Vec<String>>,
    pub fresh: bool,
    pub supported: Option<bool>,
}
pub fn address(value: &str) -> Result<String> {
    if value.is_empty() || value.len() > 3071 || value.chars().any(char::is_control) {
        return Err("Invalid XMPP address".into());
    }
    value
        .parse::<Jid>()
        .map(|jid| jid.to_string())
        .map_err(|_| "Invalid XMPP address".into())
}
pub fn matching_rules(list: &[String], peer: &str) -> Vec<String> {
    let Ok(jid) = peer.parse::<Jid>() else {
        return vec![];
    };
    let full = jid.to_string();
    let bare = jid.to_bare().to_string();
    let domain = jid.domain().as_str();
    list.iter()
        .filter(|rule| **rule == full || **rule == bare || rule.as_str() == domain)
        .cloned()
        .collect()
}
fn bounded(xml: &Element) -> Result<()> {
    struct Budget(usize);
    impl std::io::Write for Budget {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0 = self
                .0
                .checked_add(bytes.len())
                .filter(|n| *n <= BYTES)
                .ok_or_else(|| std::io::Error::other("Block list too large"))?;
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    xml.write_to(&mut Budget(0))
        .map_err(|_| "Block list is too large".into())
}
fn items(xml: &Element) -> Result<BTreeSet<String>> {
    bounded(xml)?;
    if !xml.text().trim().is_empty() {
        return Err("Invalid block list response".into());
    }
    let mut values = BTreeSet::new();
    for item in xml.children() {
        if !item.is("item", NS)
            || item.children().next().is_some()
            || !item.text().trim().is_empty()
        {
            return Err("Invalid blocked address entry".into());
        }
        let jid = address(item.attr("jid").ok_or("Blocked address is missing")?)?;
        values.insert(jid);
        if values.len() > MAX {
            return Err("Block list is too large".into());
        }
    }
    Ok(values)
}
fn failure(error: iq::Error) -> String {
    match error {
        iq::Error::Timeout => "The server did not answer the blocking request".into(),
        iq::Error::Closed => "Reconnect before changing blocked contacts".into(),
        iq::Error::Rejected { condition, .. }
            if condition == "feature-not-implemented" || condition == "service-unavailable" =>
        {
            "This server does not support contact blocking".into()
        }
        _ => "The server could not confirm the blocking request".into(),
    }
}
#[derive(Default)]
struct Replay {
    clear: bool,
    invalid: bool,
    rules: BTreeMap<String, bool>,
}
struct Data {
    epoch: u64,
    online: bool,
    known: bool,
    loaded: bool,
    supported: Option<bool>,
    current: BTreeSet<String>,
    replay: Option<Replay>,
}
#[derive(Clone)]
pub struct Manager {
    owner: String,
    generation: u64,
    events: mpsc::Sender<Envelope>,
    cache_path: Option<std::path::PathBuf>,
    cache_token: String,
    data: Arc<Mutex<Data>>,
    gate: Arc<AsyncMutex<()>>,
}
pub struct Lifecycle(Manager);
impl Drop for Lifecycle {
    fn drop(&mut self) {
        self.0.disconnect();
    }
}
impl Manager {
    pub async fn new(owner: String, generation: u64, events: mpsc::Sender<Envelope>) -> Self {
        let account = owner.clone();
        let stored = tokio::task::spawn_blocking(move || -> SqlResult<_> {
            let store = Store::open(false)?;
            Ok((
                store.cached_blocked(&account)?,
                store.db.path().map(std::path::PathBuf::from),
            ))
        })
        .await
        .ok()
        .and_then(|v| v.ok());
        let (cached, cache_path) = stored.unwrap_or_default();
        let cache_token = crate::model::new_id();
        CACHE_IO
            .lock()
            .unwrap()
            .insert(owner.clone(), cache_token.clone());
        Self {
            owner,
            generation,
            events,
            cache_path,
            cache_token,
            data: Arc::new(Mutex::new(Data {
                epoch: 0,
                online: false,
                known: cached.is_some(),
                loaded: false,
                supported: None,
                current: cached.unwrap_or_default().into_iter().collect(),
                replay: None,
            })),
            gate: Arc::new(AsyncMutex::new(())),
        }
    }
    pub fn lifecycle(&self) -> Lifecycle {
        Lifecycle(self.clone())
    }
    pub fn snapshot(&self) -> Snapshot {
        let data = self.data.lock().unwrap();
        Snapshot {
            jids: data.known.then(|| data.current.iter().cloned().collect()),
            fresh: data.online && data.loaded,
            supported: data.supported,
        }
    }
    pub fn online(&self) -> u64 {
        let epoch = {
            let mut data = self.data.lock().unwrap();
            data.epoch = data.epoch.wrapping_add(1);
            data.online = true;
            data.loaded = false;
            data.supported = None;
            data.replay = None;
            data.epoch
        };
        self.emit();
        epoch
    }
    pub fn disconnect(&self) {
        {
            let mut data = self.data.lock().unwrap();
            data.epoch = data.epoch.wrapping_add(1);
            data.online = false;
            data.loaded = false;
            data.replay = None;
        }
        self.emit();
    }
    pub fn epoch(&self) -> u64 {
        self.data.lock().unwrap().epoch
    }
    pub fn blocked(&self, peer: &str) -> bool {
        let Ok(jid) = peer.parse::<Jid>() else {
            return false;
        };
        let data = self.data.lock().unwrap();
        data.current.contains(&jid.to_string())
            || data.current.contains(jid.to_bare().as_str())
            || data.current.contains(jid.domain().as_str())
    }
    pub fn valid(&self, epoch: u64) -> bool {
        let data = self.data.lock().unwrap();
        data.online && data.epoch == epoch
    }
    fn require(&self, epoch: u64) -> Result<()> {
        self.valid(epoch)
            .then_some(())
            .ok_or_else(|| "Blocking connection changed; refresh the list".into())
    }
    fn emit(&self) {
        let _ = self.events.send((
            self.generation,
            self.owner.clone(),
            BackendEvent::BlockingChanged(self.snapshot()),
        ));
    }
    fn publish(&self, epoch: u64) {
        if !self.valid(epoch) {
            return;
        }
        self.emit();
        let Some(path) = self.cache_path.clone() else {
            return;
        };
        let manager = self.clone();
        tokio::task::spawn_blocking(move || manager.persist(path));
    }
    fn persist(&self, path: std::path::PathBuf) {
        let current = CACHE_IO.lock().unwrap();
        // Retain acknowledged changes across disconnect, but never overwrite a newer session.
        if current.get(&self.owner) != Some(&self.cache_token) {
            return;
        }
        if let Some(list) = self.snapshot().jids {
            if let Ok(db) = rusqlite::Connection::open_with_flags(
                path,
                rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE,
            ) {
                let _ = db.busy_timeout(std::time::Duration::from_millis(250));
                let _ = (Store { db }).save_blocked(&self.owner, &list);
            }
        }
    }
    async fn request(
        &self,
        network: &channel::UnboundedSender<Command>,
        epoch: u64,
        to: String,
        kind: iq::Kind,
        payload: Element,
    ) -> Result<Option<Element>> {
        self.require(epoch)?;
        let (reply, response) = oneshot::channel();
        network
            .send(Command::BlockingIq {
                epoch,
                to,
                kind,
                payload,
                reply,
            })
            .map_err(|_| "Blocking connection closed")?;
        let result = response
            .await
            .map_err(|_| "Blocking connection closed")?
            .map_err(failure);
        self.require(epoch)?;
        result
    }
    async fn refresh_locked(
        &self,
        network: &channel::UnboundedSender<Command>,
        epoch: u64,
    ) -> Result<Snapshot> {
        self.require(epoch)?;
        if self.snapshot().supported != Some(true) {
            let domain = self
                .owner
                .parse::<Jid>()
                .map_err(|_| "Invalid account")?
                .domain()
                .as_str()
                .to_owned();
            let response = self
                .request(
                    network,
                    epoch,
                    domain,
                    iq::Kind::Get,
                    Element::builder("query", DISCO).build(),
                )
                .await?
                .ok_or("Missing server capability response")?;
            bounded(&response)?;
            if !response.is("query", DISCO) {
                return Err("Invalid server capability response".into());
            }
            let supported = response
                .children()
                .any(|c| c.is("feature", DISCO) && c.attr("var") == Some(NS));
            self.data.lock().unwrap().supported = Some(supported);
            self.emit();
            if !supported {
                return Err("This server does not support contact blocking".into());
            }
        }
        self.data.lock().unwrap().replay = Some(Replay::default());
        let result = self
            .request(
                network,
                epoch,
                self.owner.clone(),
                iq::Kind::Get,
                Element::builder("blocklist", NS).build(),
            )
            .await;
        let parsed = result.and_then(|response| {
            let xml = response.ok_or("Missing block list response")?;
            if !xml.is("blocklist", NS) {
                return Err("Invalid block list response".into());
            }
            items(&xml)
        });
        self.require(epoch)?;
        let mut data = self.data.lock().unwrap();
        let replay = data.replay.take().unwrap_or_default();
        match parsed {
            Ok(mut list) => {
                if replay.invalid {
                    data.loaded = false;
                    drop(data);
                    self.emit();
                    return Err("Block list changed too much; refresh it again".into());
                }
                if replay.clear {
                    list.clear();
                }
                for (jid, block) in replay.rules {
                    if block {
                        list.insert(jid);
                    } else {
                        list.remove(&jid);
                    }
                }
                if list.len() > MAX || list.iter().map(String::len).sum::<usize>() > BYTES {
                    data.loaded = false;
                    drop(data);
                    self.emit();
                    return Err("Block list is too large".into());
                }
                data.current = list;
                data.known = true;
                data.loaded = true;
                drop(data);
                self.publish(epoch);
                Ok(self.snapshot())
            }
            Err(error) => {
                data.loaded = false;
                drop(data);
                self.emit();
                Err(error)
            }
        }
    }
    pub async fn refresh(
        &self,
        network: &channel::UnboundedSender<Command>,
        epoch: u64,
    ) -> Result<Snapshot> {
        let _gate = self.gate.lock().await;
        self.refresh_locked(network, epoch).await
    }
    pub async fn change(
        &self,
        network: &channel::UnboundedSender<Command>,
        epoch: u64,
        peer: &str,
        blocked: bool,
    ) -> Result<Snapshot> {
        let peer = address(peer)?;
        if blocked && !crate::contact_access::valid_peer(&self.owner, &peer) {
            return Err("Choose another contact's bare XMPP address".into());
        }
        let _gate = self.gate.lock().await;
        self.require(epoch)?;
        if !self.snapshot().fresh {
            self.refresh_locked(network, epoch).await?;
        }
        let item = Element::builder("item", NS)
            .attr("jid".try_into().unwrap(), peer)
            .build();
        let payload = Element::builder(if blocked { "block" } else { "unblock" }, NS)
            .append(item)
            .build();
        match self
            .request(network, epoch, self.owner.clone(), iq::Kind::Set, payload)
            .await
        {
            Ok(None) => self.refresh_locked(network, epoch).await,
            Ok(Some(_)) => {
                self.data.lock().unwrap().loaded = false;
                self.emit();
                Err("Invalid blocking acknowledgement; refresh the list".into())
            }
            Err(error) => {
                if self.valid(epoch) {
                    self.data.lock().unwrap().loaded = false;
                    self.emit();
                }
                Err(error)
            }
        }
    }
    /// Returns an acknowledgement/error only for blocking pushes. Never trusts other resources.
    pub fn push(&self, root: &Element) -> Option<Element> {
        if !root.is("iq", CLIENT) || root.attr("type") != Some("set") {
            return None;
        }
        let payload = root.children().find(|c| c.ns() == NS)?;
        if !matches!(payload.name(), "block" | "unblock") {
            return None;
        }
        let id = root.attr("id")?;
        if id.is_empty() || id.len() > 1024 {
            return None;
        }
        let (epoch, online) = {
            let data = self.data.lock().unwrap();
            (data.epoch, data.online)
        };
        let trusted = online
            && root.attr("from").is_none_or(|from| from == self.owner)
            && root.attr("to").is_none_or(|to| {
                to.parse::<Jid>()
                    .ok()
                    .is_some_and(|jid| jid.to_bare().as_str() == self.owner)
            });
        let parsed = if !trusted {
            Err("not-allowed")
        } else if root.children().count() != 1 {
            Err("bad-request")
        } else {
            items(payload)
                .map_err(|_| "bad-request")
                .and_then(|values| {
                    if payload.name() == "block" && values.is_empty() {
                        Err("bad-request")
                    } else {
                        Ok(values)
                    }
                })
        };
        let mut reply = Element::builder("iq", CLIENT)
            .attr("id".try_into().unwrap(), id)
            .attr(
                "type".try_into().unwrap(),
                if parsed.is_ok() { "result" } else { "error" },
            );
        if let Some(from) = root.attr("from") {
            reply = reply.attr("to".try_into().unwrap(), from);
        }
        match parsed {
            Err(condition) => Some(
                reply
                    .append(payload.clone())
                    .append(
                        Element::builder("error", CLIENT)
                            .attr("type".try_into().unwrap(), "cancel")
                            .append(
                                Element::builder(condition, "urn:ietf:params:xml:ns:xmpp-stanzas")
                                    .build(),
                            )
                            .build(),
                    )
                    .build(),
            ),
            Ok(values) => {
                let mut data = self.data.lock().unwrap();
                let block = payload.name() == "block";
                if !block && values.is_empty() {
                    data.current.clear();
                    if let Some(replay) = data.replay.as_mut() {
                        replay.clear = true;
                        replay.rules.clear();
                    }
                }
                for jid in values {
                    if block {
                        data.current.insert(jid.clone());
                    } else {
                        data.current.remove(&jid);
                    }
                    if let Some(replay) = data.replay.as_mut() {
                        if !replay.invalid {
                            replay.rules.insert(jid, block);
                            if replay.rules.len() > MAX {
                                replay.invalid = true;
                                replay.rules.clear();
                            }
                        }
                    }
                }
                if data.current.len() > MAX
                    || data.current.iter().map(String::len).sum::<usize>() > BYTES
                {
                    data.loaded = false;
                    data.known = false;
                    data.current.clear();
                    if let Some(replay) = data.replay.as_mut() {
                        replay.invalid = true;
                        replay.rules.clear();
                    }
                }
                drop(data);
                self.publish(epoch);
                Some(reply.build())
            }
        }
    }
}
impl Store {
    pub fn cached_blocked(&self, owner: &str) -> SqlResult<Option<Vec<String>>> {
        let raw = self.pref(&format!("blocked-jids:{owner}"), "");
        if raw.is_empty() {
            return Ok(None);
        }
        if raw.len() > BYTES {
            return Ok(None);
        }
        let Ok(list) = serde_json::from_str::<Vec<String>>(&raw) else {
            return Ok(None);
        };
        if list.len() > MAX {
            return Ok(None);
        }
        let normalized = list
            .iter()
            .map(|s| address(s))
            .collect::<Result<BTreeSet<_>>>();
        Ok(normalized.ok().map(|set| set.into_iter().collect()))
    }
    fn save_blocked(&self, owner: &str, list: &[String]) -> SqlResult<()> {
        let json = serde_json::to_string(list).map_err(|_| rusqlite::Error::InvalidQuery)?;
        if json.len() > BYTES {
            return Err(rusqlite::Error::InvalidQuery);
        }
        self.db.execute("INSERT INTO preferences(key,value) SELECT ?1,?2 WHERE EXISTS(SELECT 1 FROM accounts WHERE jid=?3) ON CONFLICT(key) DO UPDATE SET value=excluded.value", rusqlite::params![format!("blocked-jids:{owner}"),json,owner])?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn manager() -> Manager {
        let (events, _) = mpsc::channel();
        Manager {
            owner: "owner@example.invalid".into(),
            generation: 1,
            events,
            cache_path: None,
            cache_token: "fixture".into(),
            data: Arc::new(Mutex::new(Data {
                epoch: 0,
                online: false,
                known: false,
                loaded: false,
                supported: None,
                current: BTreeSet::new(),
                replay: None,
            })),
            gate: Arc::new(AsyncMutex::new(())),
        }
    }
    fn push(from: Option<&str>, payload: &str) -> Element {
        let mut iq = Element::builder("iq", CLIENT)
            .attr("id".try_into().unwrap(), "push")
            .attr("type".try_into().unwrap(), "set");
        if let Some(from) = from {
            iq = iq.attr("from".try_into().unwrap(), from)
        }
        iq.append(payload.parse::<Element>().unwrap()).build()
    }
    async fn query(
        commands: &mut channel::UnboundedReceiver<Command>,
        name: &str,
    ) -> oneshot::Sender<iq::Reply> {
        let Command::BlockingIq { payload, reply, .. } = commands.recv().await.unwrap() else {
            panic!("Expected blocking request")
        };
        assert_eq!(payload.name(), name);
        reply
    }
    #[test]
    fn authenticated_push_replay_and_connection_changes() {
        tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
            let manager=manager();let epoch=manager.online();let (network,mut commands)=channel::unbounded_channel();
            let (m,n)=(manager.clone(),network.clone());let work=tokio::spawn(async move{m.refresh(&n,epoch).await});
            query(&mut commands,"query").await.send(Ok(Some(format!("<query xmlns='{DISCO}'><feature var='{NS}'/></query>").parse().unwrap()))).unwrap();
            let response=query(&mut commands,"blocklist").await;
            let block=format!("<block xmlns='{NS}'><item jid='push@other.invalid'/></block>");
            for from in [Some("attacker@other.invalid"),Some("owner@example.invalid/phone")] {
                assert_eq!(manager.push(&push(from,&block)).unwrap().attr("type"),Some("error"));
                assert!(manager.data.lock().unwrap().current.is_empty());
            }
            assert_eq!(manager.push(&push(None,&block)).unwrap().attr("type"),Some("result"));
            response.send(Ok(Some(format!("<blocklist xmlns='{NS}'><item jid='old@other.invalid'/></blocklist>").parse().unwrap()))).unwrap();
            assert_eq!(work.await.unwrap().unwrap().jids.unwrap(),vec!["old@other.invalid","push@other.invalid"]);
            let (m,n)=(manager.clone(),network.clone());let work=tokio::spawn(async move{m.change(&n,epoch,"target@other.invalid",true).await});
            query(&mut commands,"block").await.send(Ok(None)).unwrap();
            let response=query(&mut commands,"blocklist").await;
            manager.push(&push(Some("owner@example.invalid"),&format!("<unblock xmlns='{NS}'/>"))).unwrap();
            manager.push(&push(None,&format!("<block xmlns='{NS}'><item jid='after@other.invalid'/></block>"))).unwrap();
            response.send(Ok(Some(format!("<blocklist xmlns='{NS}'><item jid='old@other.invalid'/><item jid='target@other.invalid'/></blocklist>").parse().unwrap()))).unwrap();
            assert_eq!(work.await.unwrap().unwrap().jids.unwrap(),vec!["after@other.invalid"]);
            let (m,n)=(manager.clone(),network.clone());let work=tokio::spawn(async move{m.refresh(&n,epoch).await});
            let response=query(&mut commands,"blocklist").await;
            manager.disconnect();manager.online();
            response.send(Ok(Some(format!("<blocklist xmlns='{NS}'/>").parse().unwrap()))).unwrap();
            assert!(work.await.unwrap().is_err());
            assert_eq!(manager.snapshot().jids.unwrap(),vec!["after@other.invalid"]);
            assert!(!manager.snapshot().fresh);
        });
    }
    #[test]
    fn acknowledged_cache_survives_disconnect_without_stale_session_overwrite() {
        let store = Store::open(true).unwrap();
        let mut manager = manager();
        manager.owner = format!("{}@cache.invalid", crate::model::new_id());
        store
            .save_account(&crate::model::Account {
                jid: manager.owner.clone(),
                host: String::new(),
                port: 5222,
                direct_tls: false,
            })
            .unwrap();
        let path = std::env::temp_dir().join(format!(
            "monogram-blocking-cache-{}.sqlite",
            crate::model::new_id()
        ));
        store
            .db
            .execute("VACUUM INTO ?", [path.to_str().unwrap()])
            .unwrap();
        {
            let mut data = manager.data.lock().unwrap();
            data.known = true;
            data.current.insert("acknowledged@other.invalid".into());
        }
        CACHE_IO
            .lock()
            .unwrap()
            .insert(manager.owner.clone(), manager.cache_token.clone());
        manager.online();
        manager.disconnect();
        manager.persist(path.clone());
        let disk = Store {
            db: rusqlite::Connection::open(&path).unwrap(),
        };
        assert_eq!(
            disk.cached_blocked(&manager.owner).unwrap().unwrap(),
            vec!["acknowledged@other.invalid"]
        );
        CACHE_IO
            .lock()
            .unwrap()
            .insert(manager.owner.clone(), "newer-session".into());
        disk.save_blocked(&manager.owner, &["newer@other.invalid".into()])
            .unwrap();
        manager.persist(path.clone());
        assert_eq!(
            disk.cached_blocked(&manager.owner).unwrap().unwrap(),
            vec!["newer@other.invalid"]
        );
        disk.forget(&manager.owner).unwrap();
        CACHE_IO
            .lock()
            .unwrap()
            .insert(manager.owner.clone(), manager.cache_token.clone());
        manager.persist(path.clone());
        assert!(disk.cached_blocked(&manager.owner).unwrap().is_none());
        CACHE_IO.lock().unwrap().remove(&manager.owner);
        drop(disk);
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn matching_validation_cache_and_server_addressing() {
        let list = vec![
            "example.org".into(),
            "friend@else.org/device".into(),
            "bare@else.org".into(),
        ];
        assert_eq!(
            matching_rules(&list, "any@example.org/phone"),
            vec!["example.org"]
        );
        assert!(matching_rules(&list, "friend@else.org/other").is_empty());
        assert_eq!(
            matching_rules(&list, "friend@else.org/device"),
            vec!["friend@else.org/device"]
        );
        assert_eq!(
            matching_rules(&list, "bare@else.org/any"),
            vec!["bare@else.org"]
        );
        assert!(address("bad\n@example.org").is_err());
        assert!(
            items(
                &format!("<blocklist xmlns='{NS}'><item/></blocklist>")
                    .parse()
                    .unwrap()
            )
            .is_err()
        );
        let store = Store::open(true).unwrap();
        store
            .save_account(&crate::model::Account {
                jid: "owner@example.invalid".into(),
                host: String::new(),
                port: 5222,
                direct_tls: false,
            })
            .unwrap();
        store.save_blocked("owner@example.invalid", &list).unwrap();
        assert_eq!(
            store
                .cached_blocked("owner@example.invalid")
                .unwrap()
                .unwrap()
                .len(),
            3
        );
        assert!(
            store
                .cached_blocked("another@example.invalid")
                .unwrap()
                .is_none()
        );
        let mut pending = iq::Pending::default();
        let (reply, mut received) = oneshot::channel();
        let request = pending
            .start(
                "owner@example.invalid".into(),
                iq::Kind::Get,
                Element::builder("blocklist", NS).build(),
                reply,
            )
            .unwrap();
        assert!(request.attr("to").is_none());
        let result: Element = format!(
            "<iq xmlns='{CLIENT}' type='result' id='{}'><blocklist xmlns='{NS}'/></iq>",
            request.attr("id").unwrap()
        )
        .parse()
        .unwrap();
        assert!(pending.receive(&result, "owner@example.invalid"));
        assert!(received.try_recv().unwrap().is_ok());
    }
}
