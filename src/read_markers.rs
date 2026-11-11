//! XEP-0333 public displayed markers, following mobile MessageReceipt/MonogramReads.
//! Reading history is local until an explicit display action queues a consented marker.
use crate::{model::Message, storage::Store};
use rusqlite::{OptionalExtension, params};
use tokio_xmpp::{jid::Jid, minidom::Element};
pub const NS: &str = "urn:xmpp:chat-markers:0";
const CLIENT: &str = "jabber:client";
const RECEIPTS: &str = "urn:xmpp:receipts";
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Notice {
    pub peer: String,
    pub id: String,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Policy {
    pub enabled: bool,
    pub revision: u64,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Candidate {
    pub peer: String,
    pub id: String,
    pub wire: String,
    pub timestamp: String,
    pub revision: u64,
}
pub fn valid_id(id: &str) -> bool {
    !id.trim().is_empty()
        && id.encode_utf16().count() <= 4096
        && id.chars().all(|c| {
            matches!(c, '\t' | '\n' | '\r') || c >= ' ' && !matches!(c, '\u{fffe}' | '\u{ffff}')
        })
}
fn bare(address: &str) -> Option<String> {
    let jid = address.parse::<Jid>().ok()?;
    let bare = jid.to_bare().to_string();
    (crate::protocol::bare(&bare).ok().as_deref() == Some(&bare)).then_some(bare)
}
fn direct(xml: &Element) -> bool {
    xml.is("message", CLIENT)
        && matches!(xml.attr("type").unwrap_or("normal"), "chat" | "normal")
        && !xml.children().any(|c| {
            c.is("x", "http://jabber.org/protocol/muc#user")
                || c.is("result", "urn:xmpp:mam:2")
                || c.ns() == "urn:xmpp:carbons:2"
        })
}
fn marker_id(xml: &Element, name: &str, ns: &str) -> Option<String> {
    let mut children = xml.children().filter(|c| c.is(name, ns));
    let marker = children.next()?;
    if children.next().is_some()
        || marker.children().next().is_some()
        || !marker.text().trim().is_empty()
        || marker.attrs().len() != 1
    {
        return None;
    }
    let id = marker.attr("id")?;
    valid_id(id).then(|| id.into())
}
pub fn incoming(xml: &Element, owner: &str) -> Option<Notice> {
    if !direct(xml) || xml.get_child("delay", "urn:xmpp:delay").is_some() {
        return None;
    }
    let peer = bare(xml.attr("from")?)?;
    if peer == owner
        || xml
            .attr("to")
            .is_some_and(|to| bare(to).as_deref() != Some(owner))
    {
        return None;
    }
    Some(Notice {
        peer,
        id: marker_id(xml, "displayed", NS)?,
    })
}
pub fn incoming_receipt(xml: &Element, owner: &str) -> Option<Notice> {
    if !direct(xml) || xml.get_child("delay", "urn:xmpp:delay").is_some() {
        return None;
    }
    let peer = bare(xml.attr("from")?)?;
    if peer == owner
        || xml
            .attr("to")
            .is_some_and(|to| bare(to).as_deref() != Some(owner))
    {
        return None;
    }
    Some(Notice {
        peer,
        id: marker_id(xml, "received", RECEIPTS)?,
    })
}
/// Only call after the sent-carbon wrapper has been authenticated as our bare account.
pub fn own_displayed(xml: &Element, owner: &str) -> Option<Notice> {
    if !direct(xml)
        || bare(xml.attr("from")?).as_deref() != Some(owner)
        || xml.get_child("body", CLIENT).is_some()
        || xml.get_child("subject", CLIENT).is_some()
    {
        return None;
    }
    let peer = bare(xml.attr("to")?)?;
    let id = marker_id(xml, "displayed", NS)?;
    (peer != owner).then_some(Notice { peer, id })
}
pub fn markable_id(xml: &Element) -> Option<String> {
    if !direct(xml) || xml.get_child("received", RECEIPTS).is_some() {
        return None;
    }
    let id = xml.attr("id").filter(|id| valid_id(id))?;
    let mut markers = xml.children().filter(|c| c.ns() == NS);
    let markable = markers.next()?;
    if markers.next().is_some()
        || !markable.is("markable", NS)
        || !markable.attrs().is_empty()
        || markable.children().next().is_some()
        || !markable.text().trim().is_empty()
    {
        return None;
    }
    Some(id.into())
}
pub fn decorate(xml: &mut Element, message: &Message) {
    if !matches!(xml.attr("type").unwrap_or("normal"), "chat" | "normal")
        || message.action.is_some()
        || message.retracted
        || message.text.is_empty() && message.attachment.is_none() && message.poll.is_none()
        || !xml.attr("id").is_some_and(valid_id)
        || xml
            .children()
            .any(|c| c.ns() == NS || c.is("received", RECEIPTS))
    {
        return;
    }
    xml.append_child(Element::builder("markable", NS).build());
}
pub fn stanza(peer: &str, id: &str) -> Option<Element> {
    if crate::protocol::bare(peer).ok().as_deref() != Some(peer) || !valid_id(id) {
        return None;
    }
    Some(
        Element::builder("message", CLIENT)
            .attr("to".try_into().unwrap(), peer)
            .attr("type".try_into().unwrap(), "chat")
            .append(
                Element::builder("displayed", NS)
                    .attr("id".try_into().unwrap(), id)
                    .build(),
            )
            .append(Element::builder("store", "urn:xmpp:hints").build())
            .build(),
    )
}
impl Store {
    pub(crate) fn init_read_markers(&self) -> rusqlite::Result<()> {
        self.db.execute_batch("CREATE TABLE IF NOT EXISTS displayed_sent(account TEXT NOT NULL,peer TEXT NOT NULL,id TEXT NOT NULL,timestamp TEXT NOT NULL,wire TEXT NOT NULL,revision INTEGER NOT NULL,PRIMARY KEY(account,peer),FOREIGN KEY(account,peer,id) REFERENCES messages(account,peer,id) ON DELETE CASCADE);CREATE TABLE IF NOT EXISTS displayed_pending(account TEXT NOT NULL,peer TEXT NOT NULL,id TEXT NOT NULL,timestamp TEXT NOT NULL,wire TEXT NOT NULL,revision INTEGER NOT NULL,PRIMARY KEY(account,peer),FOREIGN KEY(account,peer,id) REFERENCES messages(account,peer,id) ON DELETE CASCADE);CREATE TABLE IF NOT EXISTS displayed_receipts(account TEXT NOT NULL,peer TEXT NOT NULL,id TEXT NOT NULL,PRIMARY KEY(account,peer,id),FOREIGN KEY(account,peer,id) REFERENCES messages(account,peer,id) ON DELETE CASCADE);")
    }
    pub fn read_receipts(&self, owner: &str) -> Policy {
        Policy {
            enabled: self.pref(&format!("read_receipts:{owner}"), "false") == "true",
            revision: self
                .pref(&format!("read-receipts-revision:{owner}"), "0")
                .parse::<u64>()
                .ok()
                .filter(|revision| *revision < i64::MAX as u64)
                .unwrap_or(0),
        }
    }
    pub fn set_read_receipts(&self, owner: &str, enabled: bool) -> rusqlite::Result<Policy> {
        if crate::protocol::bare(owner).ok().as_deref() != Some(owner) {
            return Err(rusqlite::Error::InvalidQuery);
        }
        self.init_read_markers()?;
        let tx = rusqlite::Transaction::new_unchecked(
            &self.db,
            rusqlite::TransactionBehavior::Immediate,
        )?;
        let current = self.read_receipts(owner);
        let revision = current
            .revision
            .checked_add(1)
            .filter(|v| *v < i64::MAX as u64)
            .ok_or(rusqlite::Error::InvalidQuery)?;
        self.set_pref(
            &format!("read_receipts:{owner}"),
            if enabled { "true" } else { "false" },
        )?;
        self.set_pref(
            &format!("read-receipts-revision:{owner}"),
            &revision.to_string(),
        )?;
        tx.execute("DELETE FROM displayed_pending WHERE account=?", [owner])?;
        tx.commit()?;
        Ok(Policy { enabled, revision })
    }
    fn direct_chat(&self, owner: &str, peer: &str) -> bool {
        peer!=owner&&crate::protocol::bare(peer).ok().as_deref()==Some(peer)&&self.db.query_row("SELECT COALESCE(json_extract(body,'$.group'),0)=0 FROM chats WHERE account=? AND peer=?",params![owner,peer],|r|r.get(0)).unwrap_or(false)
    }
    fn incoming_anchor(
        &self,
        owner: &str,
        peer: &str,
        id: &str,
    ) -> rusqlite::Result<Option<Message>> {
        if !self.direct_chat(owner, peer) {
            return Ok(None);
        }
        let Some(m) = self.message(owner, peer, id).filter(|m| {
            !m.own
                && !m.unavailable
                && !m.retracted
                && m.action.is_none()
                && m.upload.is_none()
                && (!m.text.is_empty() || m.attachment.is_some() || m.poll.is_some())
                && !m.markable_id.is_empty()
                && m.markable_id == m.wire_id
                && valid_id(&m.markable_id)
                && bare(&m.sender).as_deref() == Some(peer)
        }) else {
            return Ok(None);
        };
        let count:i64=self.db.query_row("SELECT count(*) FROM messages WHERE account=? AND peer=? AND json_extract(body,'$.own')=0 AND json_extract(body,'$.wire_id')=?",params![owner,peer,m.markable_id],|r|r.get(0))?;
        Ok((count == 1).then_some(m))
    }
    /// Queue only IDs explicitly displayed locally, after their unread flags have been persisted.
    pub fn mark_displayed(
        &self,
        owner: &str,
        peer: &str,
        ids: &[String],
    ) -> rusqlite::Result<Option<Candidate>> {
        self.init_read_markers()?;
        let tx = rusqlite::Transaction::new_unchecked(
            &self.db,
            rusqlite::TransactionBehavior::Immediate,
        )?;
        let policy = self.read_receipts(owner);
        if !policy.enabled {
            return Ok(None);
        }
        let mut anchor: Option<Message> = None;
        for id in ids {
            if let Some(m) = self.incoming_anchor(owner, peer, id)?.filter(|m| !m.unread) {
                if anchor
                    .as_ref()
                    .is_none_or(|a| m.order_key() > a.order_key())
                {
                    anchor = Some(m);
                }
            }
        }
        let Some(m) = anchor else {
            return Ok(None);
        };
        for table in ["displayed_sent", "displayed_pending"] {
            let previous: Option<(String, String)> = self
                .db
                .query_row(
                    &format!("SELECT timestamp,id FROM {table} WHERE account=? AND peer=?"),
                    params![owner, peer],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?;
            if previous.is_some_and(|(time, id)| (time.as_str(), id.as_str()) >= m.order_key()) {
                return Ok(None);
            }
        }
        let candidate = Candidate {
            peer: peer.into(),
            id: m.id.clone(),
            wire: m.markable_id.clone(),
            timestamp: m.timestamp.clone(),
            revision: policy.revision,
        };
        tx.execute(
            "INSERT OR REPLACE INTO displayed_pending VALUES(?,?,?,?,?,?)",
            params![
                owner,
                peer,
                candidate.id,
                candidate.timestamp,
                candidate.wire,
                candidate.revision as i64
            ],
        )?;
        tx.commit()?;
        Ok(Some(candidate))
    }
    pub fn pending_displayed(&self, owner: &str) -> rusqlite::Result<Vec<Candidate>> {
        self.init_read_markers()?;
        let mut query=self.db.prepare("SELECT peer,id,wire,timestamp,revision FROM displayed_pending WHERE account=? ORDER BY timestamp,id")?;
        query
            .query_map([owner], |r| {
                Ok(Candidate {
                    peer: r.get(0)?,
                    id: r.get(1)?,
                    wire: r.get(2)?,
                    timestamp: r.get(3)?,
                    revision: r.get::<_, i64>(4)?.max(0) as u64,
                })
            })?
            .collect()
    }
    pub fn displayed_candidate(
        &self,
        owner: &str,
        peer: &str,
        id: &str,
    ) -> rusqlite::Result<Option<Candidate>> {
        let policy = self.read_receipts(owner);
        if !policy.enabled {
            return Ok(None);
        }
        self.init_read_markers()?;
        let candidate:Option<Candidate>=self.db.query_row("SELECT peer,id,wire,timestamp,revision FROM displayed_pending WHERE account=? AND peer=? AND id=?",params![owner,peer,id],|r|Ok(Candidate{peer:r.get(0)?,id:r.get(1)?,wire:r.get(2)?,timestamp:r.get(3)?,revision:r.get::<_,i64>(4)?.max(0) as u64})).optional()?;
        let Some(candidate) = candidate.filter(|c| c.revision == policy.revision) else {
            return Ok(None);
        };
        let Some(m) = self.incoming_anchor(owner, peer, id)?.filter(|m| !m.unread) else {
            return Ok(None);
        };
        Ok(
            (m.timestamp == candidate.timestamp && m.markable_id == candidate.wire)
                .then_some(candidate),
        )
    }
    pub fn acknowledge_displayed(
        &self,
        owner: &str,
        candidate: &Candidate,
    ) -> rusqlite::Result<bool> {
        self.init_read_markers()?;
        let tx = rusqlite::Transaction::new_unchecked(
            &self.db,
            rusqlite::TransactionBehavior::Immediate,
        )?;
        if self
            .displayed_candidate(owner, &candidate.peer, &candidate.id)?
            .as_ref()
            != Some(candidate)
        {
            return Ok(false);
        }
        tx.execute(
            "INSERT OR REPLACE INTO displayed_sent VALUES(?,?,?,?,?,?)",
            params![
                owner,
                candidate.peer,
                candidate.id,
                candidate.timestamp,
                candidate.wire,
                candidate.revision as i64
            ],
        )?;
        tx.execute(
            "DELETE FROM displayed_pending WHERE account=? AND peer=? AND id=? AND revision=?",
            params![
                owner,
                candidate.peer,
                candidate.id,
                candidate.revision as i64
            ],
        )?;
        tx.commit()?;
        Ok(true)
    }
    /// Resolve only an unambiguous original stanza ID of our own real message in this peer's chat.
    pub fn received_displayed(
        &self,
        owner: &str,
        peer: &str,
        wire: &str,
    ) -> rusqlite::Result<Vec<Message>> {
        self.init_read_markers()?;
        if !valid_id(wire) || !self.direct_chat(owner, peer) {
            return Ok(Vec::new());
        }
        let tx = rusqlite::Transaction::new_unchecked(
            &self.db,
            rusqlite::TransactionBehavior::Immediate,
        )?;
        let rows = {
            let mut q=tx.prepare("SELECT body FROM messages WHERE account=? AND peer=? AND json_extract(body,'$.own')=1 AND json_extract(body,'$.wire_id')=? AND COALESCE(json_extract(body,'$.retracted'),0)=0")?;
            q.query_map(params![owner, peer, wire], |r| r.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?
        };
        if rows.len() != 1 {
            return Ok(Vec::new());
        }
        let Some(anchor) = rows
            .first()
            .and_then(|s| Message::decode(s).ok())
            .filter(|m| {
                m.action.is_none()
                    && matches!(
                        m.delivery.as_str(),
                        "sending" | "sent" | "delivered" | "read" | "uncertain"
                    )
                    && bare(&m.sender).as_deref() == Some(owner)
            })
        else {
            return Ok(Vec::new());
        };
        let rows = {
            let mut q=tx.prepare("SELECT body FROM messages WHERE account=? AND peer=? AND json_extract(body,'$.own')=1 AND COALESCE(json_extract(body,'$.retracted'),0)=0 AND (timestamp<? OR (timestamp=? AND id<=?)) AND COALESCE(json_extract(body,'$.delivery'),'')!='read' ORDER BY timestamp,id")?;
            q.query_map(
                params![owner, peer, anchor.timestamp, anchor.timestamp, anchor.id],
                |r| r.get::<_, String>(0),
            )?
            .collect::<rusqlite::Result<Vec<_>>>()?
        };
        let mut changed = Vec::new();
        for row in rows {
            if let Ok(mut m) = Message::decode(&row) {
                if m.action.is_some()
                    || !matches!(
                        m.delivery.as_str(),
                        "sending" | "sent" | "delivered" | "read" | "uncertain"
                    )
                    || bare(&m.sender).as_deref() != Some(owner)
                {
                    continue;
                }
                m.delivery = "read".into();
                tx.execute(
                    "INSERT OR IGNORE INTO displayed_receipts VALUES(?,?,?)",
                    params![owner, peer, m.id],
                )?;
                for table in ["messages", "message_bases"] {
                    tx.execute(&format!("UPDATE {table} SET body=json_set(body,'$.delivery','read') WHERE account=? AND peer=? AND id=?"),params![owner,peer,m.id])?;
                }
                changed.push(m);
            }
        }
        tx.commit()?;
        Ok(changed)
    }
    /// A sent carbon is already public; applying it locally must never publish another marker.
    pub fn received_own_displayed(
        &self,
        owner: &str,
        peer: &str,
        wire: &str,
    ) -> rusqlite::Result<Vec<Message>> {
        if !valid_id(wire) || !self.direct_chat(owner, peer) {
            return Ok(Vec::new());
        }
        let rows = {
            let mut q=self.db.prepare("SELECT id FROM messages WHERE account=? AND peer=? AND json_extract(body,'$.own')=0 AND json_extract(body,'$.markable_id')=? AND COALESCE(json_extract(body,'$.retracted'),0)=0")?;
            q.query_map(params![owner, peer, wire], |r| r.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?
        };
        if rows.len() != 1 {
            return Ok(Vec::new());
        }
        let Some(anchor) = self.incoming_anchor(owner, peer, &rows[0])? else {
            return Ok(Vec::new());
        };
        let tx = rusqlite::Transaction::new_unchecked(
            &self.db,
            rusqlite::TransactionBehavior::Immediate,
        )?;
        let rows = {
            let mut q=tx.prepare("SELECT body FROM messages WHERE account=? AND peer=? AND json_extract(body,'$.own')=0 AND json_extract(body,'$.unread')=1 AND (timestamp<? OR(timestamp=? AND id<=?)) ORDER BY timestamp,id")?;
            q.query_map(
                params![owner, peer, anchor.timestamp, anchor.timestamp, anchor.id],
                |r| r.get::<_, String>(0),
            )?
            .collect::<rusqlite::Result<Vec<_>>>()?
        };
        let mut changed = Vec::new();
        for row in rows {
            if let Ok(mut m) = Message::decode(&row) {
                m.unread = false;
                for table in ["messages", "message_bases"] {
                    tx.execute(&format!("UPDATE {table} SET body=json_set(body,'$.unread',json('false')) WHERE account=? AND peer=? AND id=?"),params![owner,peer,m.id])?;
                }
                changed.push(m);
            }
        }
        tx.execute("UPDATE chats SET body=json_set(body,'$.unread',(SELECT count(*) FROM messages WHERE account=?1 AND peer=?2 AND json_extract(body,'$.own')=0 AND json_extract(body,'$.unread')=1 AND COALESCE(json_extract(body,'$.retracted'),0)=0)) WHERE account=?1 AND peer=?2",params![owner,peer])?;
        tx.commit()?;
        Ok(changed)
    }
    pub(crate) fn confirmed_read(&self, owner: &str, peer: &str, id: &str) -> bool {
        self.db.query_row("SELECT EXISTS(SELECT 1 FROM displayed_receipts WHERE account=? AND peer=? AND id=?)",params![owner,peer,id],|r|r.get(0)).unwrap_or(false)
    }
}

pub async fn load_policy(owner: &str) -> Policy {
    let owner = owner.to_owned();
    tokio::task::spawn_blocking(move || {
        Store::open(false)
            .map(|s| s.read_receipts(&owner))
            .unwrap_or_default()
    })
    .await
    .unwrap_or_default()
}
pub fn prepare(
    owner: String,
    peer: String,
    id: String,
    epoch: u64,
    network: tokio::sync::mpsc::UnboundedSender<crate::protocol::Command>,
) {
    tokio::spawn(async move {
        let resolved = tokio::task::spawn_blocking(move || {
            Store::open(false)?.displayed_candidate(&owner, &peer, &id)
        })
        .await;
        if let Ok(Ok(Some(candidate))) = resolved {
            let _ = network.send(crate::protocol::Command::DisplayedReady { candidate, epoch });
        }
    });
}
pub fn flush(
    owner: String,
    epoch: u64,
    network: tokio::sync::mpsc::UnboundedSender<crate::protocol::Command>,
    active: std::sync::Arc<std::sync::atomic::AtomicBool>,
) {
    use std::sync::atomic::Ordering;
    if active.swap(true, Ordering::AcqRel) {
        return;
    }
    tokio::spawn(async move {
        let resolved = tokio::task::spawn_blocking(move || {
            let store = Store::open(false)?;
            let mut candidates = Vec::new();
            for c in store.pending_displayed(&owner)? {
                if store.displayed_candidate(&owner, &c.peer, &c.id)?.as_ref() == Some(&c) {
                    candidates.push(c);
                }
            }
            Ok::<_, rusqlite::Error>(candidates)
        })
        .await;
        if let Ok(Ok(candidates)) = resolved {
            for candidate in candidates {
                if network
                    .send(crate::protocol::Command::DisplayedReady { candidate, epoch })
                    .is_err()
                {
                    break;
                }
            }
        }
        active.store(false, Ordering::Release);
    });
}
pub fn acknowledge(owner: String, candidate: Candidate) {
    tokio::task::spawn_blocking(move || {
        if let Ok(store) = Store::open(false) {
            let _ = store.acknowledge_displayed(&owner, &candidate);
        }
    });
}
impl Store {
    pub fn restore_markable(
        &self,
        owner: &str,
        peer: &str,
        id: &str,
        wire: &str,
    ) -> rusqlite::Result<bool> {
        if !valid_id(wire) || !self.direct_chat(owner, peer) {
            return Ok(false);
        }
        let Some(m) = self.message(owner, peer, id).filter(|m| {
            !m.own
                && !m.retracted
                && m.action.is_none()
                && m.wire_id == wire
                && bare(&m.sender).as_deref() == Some(peer)
        }) else {
            return Ok(false);
        };
        if !m.markable_id.is_empty() {
            return Ok(false);
        }
        let tx = rusqlite::Transaction::new_unchecked(
            &self.db,
            rusqlite::TransactionBehavior::Immediate,
        )?;
        let mut changed = false;
        for table in ["messages", "message_bases"] {
            changed|=tx.execute(&format!("UPDATE {table} SET body=json_set(body,'$.markable_id',?1) WHERE account=?2 AND peer=?3 AND id=?4 AND COALESCE(json_extract(body,'$.markable_id'),'')=''"),params![wire,owner,peer,id])?>0;
        }
        tx.commit()?;
        Ok(changed)
    }
    pub(crate) fn own_wire_id(
        &self,
        owner: &str,
        peer: &str,
        wire: &str,
    ) -> rusqlite::Result<Option<String>> {
        if !valid_id(wire) {
            return Ok(None);
        }
        let ids = {
            let mut q=self.db.prepare("SELECT id FROM messages WHERE account=? AND peer=? AND json_extract(body,'$.own')=1 AND json_extract(body,'$.wire_id')=? AND COALESCE(json_extract(body,'$.retracted'),0)=0 LIMIT 2")?;
            q.query_map(params![owner, peer, wire], |r| r.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?
        };
        Ok((ids.len() == 1).then(|| ids[0].clone()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Account, Chat};
    #[test]
    fn public_reads_require_consent_marked_unique_rows_and_never_downgrade() {
        let owner = "reader@example.invalid";
        let peer = "friend@example.invalid";
        let store = Store::open(true).unwrap();
        store
            .save_account(&Account {
                jid: owner.into(),
                host: String::new(),
                port: 5222,
                direct_tls: false,
            })
            .unwrap();
        let chat = Chat {
            jid: peer.into(),
            name: "Friend".into(),
            ..Default::default()
        };
        store.save_chat_message(owner, &chat, None).unwrap();
        let received_message = Message {
            id: "incoming-local".into(),
            wire_id: "incoming-wire".into(),
            markable_id: "incoming-wire".into(),
            sender: format!("{peer}/phone"),
            timestamp: "2026-11-11T10:00:00.000000+00:00".into(),
            text: "Seen on screen".into(),
            unread: false,
            own: false,
            ..Default::default()
        };
        store
            .save_chat_message(owner, &chat, Some(&received_message))
            .unwrap();
        let ids = vec![received_message.id.clone()];
        assert!(!store.read_receipts(owner).enabled);
        assert!(store.mark_displayed(owner, peer, &ids).unwrap().is_none());
        let enabled = store.set_read_receipts(owner, true).unwrap();
        let queued = store.mark_displayed(owner, peer, &ids).unwrap().unwrap();
        assert_eq!(queued.wire, "incoming-wire");
        assert_eq!(queued.revision, enabled.revision);
        let disabled = store.set_read_receipts(owner, false).unwrap();
        assert!(disabled.revision > enabled.revision);
        assert!(
            store
                .displayed_candidate(owner, peer, &queued.id)
                .unwrap()
                .is_none()
        );
        assert!(store.pending_displayed(owner).unwrap().is_empty());
        assert!(!store.acknowledge_displayed(owner, &queued).unwrap());
        store.set_read_receipts(owner, true).unwrap();
        let queued = store.mark_displayed(owner, peer, &ids).unwrap().unwrap();
        assert!(store.acknowledge_displayed(owner, &queued).unwrap());
        assert!(store.mark_displayed(owner, peer, &ids).unwrap().is_none());
        let first = Message {
            id: "sent-one".into(),
            wire_id: "wire-one".into(),
            sender: owner.into(),
            own: true,
            delivery: "sent".into(),
            timestamp: "2026-11-11T11:00:00.000000+00:00".into(),
            text: "First".into(),
            ..Default::default()
        };
        let mut second = first.clone();
        second.id = "sent-two".into();
        second.wire_id = "wire-two".into();
        second.timestamp = "2026-11-11T11:01:00.000000+00:00".into();
        store.save_chat_message(owner, &chat, Some(&first)).unwrap();
        store
            .save_chat_message(owner, &chat, Some(&second))
            .unwrap();
        let mut ambiguous = second.clone();
        ambiguous.id = "other-resource".into();
        ambiguous.sender = format!("{owner}/phone");
        store
            .save_chat_message(owner, &chat, Some(&ambiguous))
            .unwrap();
        assert!(
            store
                .received_displayed(owner, peer, "wire-two")
                .unwrap()
                .is_empty()
        );
        ambiguous.wire_id = "wire-three".into();
        ambiguous.timestamp = "2026-11-11T11:02:00.000000+00:00".into();
        store
            .save_chat_message(owner, &chat, Some(&ambiguous))
            .unwrap();
        let changed = store.received_displayed(owner, peer, "wire-two").unwrap();
        assert_eq!(changed.len(), 2);
        assert!(changed.iter().all(|m| m.delivery == "read"));
        assert_eq!(
            store
                .update_delivery(owner, peer, "wire-one", "delivered")
                .unwrap()
                .unwrap()
                .delivery,
            "read"
        );
        store.save_chat_message(owner, &chat, Some(&first)).unwrap();
        assert_eq!(
            store.message(owner, peer, &first.id).unwrap().delivery,
            "read"
        );
        assert!(
            store
                .received_displayed(owner, "foreign@example.invalid", "wire-three")
                .unwrap()
                .is_empty()
        );
        let xml:Element=format!("<message xmlns='jabber:client' from='{peer}/phone' to='{owner}' type='chat'><displayed xmlns='{NS}' id='wire-two'/></message>").parse().unwrap();
        assert_eq!(incoming(&xml, owner).unwrap().id, "wire-two");
        assert!(incoming(&xml, peer).is_none());
        let own:Element=format!("<message xmlns='jabber:client' from='{owner}/phone' to='{peer}'><displayed xmlns='{NS}' id='incoming-wire'/></message>").parse().unwrap();
        assert_eq!(own_displayed(&own, owner).unwrap().peer, peer);
        assert!(incoming(&own, owner).is_none());
        let mut outgoing = crate::protocol::outgoing(peer, &first, "");
        assert_eq!(markable_id(&outgoing).as_deref(), Some("wire-one"));
        outgoing.append_child(
            Element::builder("displayed", NS)
                .attr("id".try_into().unwrap(), "other")
                .build(),
        );
        assert!(markable_id(&outgoing).is_none());
    }
}
