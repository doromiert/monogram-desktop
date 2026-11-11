//! Account-scoped public pending requests. Secret Service owns every password.
use super::*;
use crate::rooms::invitations::{Incoming, Kind};

#[derive(Clone, Debug)]
pub struct ContactRequest {
    pub owner: String,
    pub peer: String,
    pub description: String,
    pub revision: u64,
}
#[derive(Clone, Debug)]
pub struct RoomInvitation {
    pub owner: String,
    pub room: String,
    pub inviter: String,
    pub reason: String,
    pub kind: Kind,
    pub revision: u64,
    pub secret_id: Option<String>,
    pub password_ready: bool,
    pub password_error: Option<String>,
}
#[derive(Clone, Debug, Default)]
pub struct PendingRequests {
    pub contacts: Vec<ContactRequest>,
    pub rooms: Vec<RoomInvitation>,
    pub contact_cursor: Option<u64>,
    pub contact_total: usize,
}
pub struct InvitationUpdate {
    pub record: RoomInvitation,
    pub replaced_secret: Option<String>,
}
fn room_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<RoomInvitation> {
    let kind: String = row.get(4)?;
    let kind = match kind.as_str() {
        "direct" => Kind::Direct,
        "mediated" => Kind::Mediated,
        _ => return Err(rusqlite::Error::InvalidQuery),
    };
    Ok(RoomInvitation {
        owner: row.get(0)?,
        room: row.get(1)?,
        inviter: row.get(2)?,
        reason: row.get(3)?,
        kind,
        revision: row.get::<_, i64>(5)? as u64,
        secret_id: row.get(6)?,
        password_ready: row.get(7)?,
        password_error: row.get(8)?,
    })
}
fn contact_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ContactRequest> {
    Ok(ContactRequest {
        owner: row.get(0)?,
        peer: row.get(1)?,
        description: row.get(2)?,
        revision: row.get::<_, i64>(3)? as u64,
    })
}
fn revision(tx: &rusqlite::Transaction<'_>, owner: &str) -> rusqlite::Result<u64> {
    tx.query_row("INSERT INTO pending_request_counters VALUES(?1,1) ON CONFLICT(account) DO UPDATE SET revision=revision+1 WHERE revision<9223372036854775807 RETURNING revision", [owner], |row|row.get::<_,i64>(0)).map(|value|value as u64)
}
impl Store {
    pub(super) fn init_requests(&self) -> rusqlite::Result<()> {
        self.db.execute_batch("CREATE TABLE IF NOT EXISTS pending_request_counters(account TEXT PRIMARY KEY REFERENCES accounts(jid) ON DELETE CASCADE,revision INTEGER NOT NULL CHECK(typeof(revision)='integer' AND revision>0));
        CREATE TABLE IF NOT EXISTS pending_contacts(account TEXT NOT NULL REFERENCES accounts(jid) ON DELETE CASCADE,peer TEXT NOT NULL,description TEXT NOT NULL,revision INTEGER NOT NULL,PRIMARY KEY(account,peer));
        CREATE INDEX IF NOT EXISTS pending_contact_order ON pending_contacts(account,revision DESC);
        CREATE TABLE IF NOT EXISTS pending_room_invitations(account TEXT NOT NULL REFERENCES accounts(jid) ON DELETE CASCADE,room TEXT NOT NULL,inviter TEXT NOT NULL,reason TEXT NOT NULL,kind TEXT NOT NULL,revision INTEGER NOT NULL,secret_id TEXT,password_ready INTEGER NOT NULL,password_error TEXT,PRIMARY KEY(account,room));
        CREATE TABLE IF NOT EXISTS invitation_secret_cleanup(account TEXT NOT NULL REFERENCES accounts(jid) ON DELETE CASCADE,room TEXT NOT NULL,secret_id TEXT NOT NULL,PRIMARY KEY(account,room,secret_id));")
    }
    pub fn pending_requests(&self, owner: &str) -> rusqlite::Result<PendingRequests> {
        self.pending_requests_before(owner, None)
    }
    pub fn pending_requests_before(
        &self,
        owner: &str,
        before: Option<u64>,
    ) -> rusqlite::Result<PendingRequests> {
        let before = before
            .map(i64::try_from)
            .transpose()
            .map_err(|_| rusqlite::Error::InvalidQuery)?;
        let mut contacts=self.db.prepare("SELECT account,peer,description,revision FROM pending_contacts WHERE account=?1 AND (?2 IS NULL OR revision<?2) ORDER BY revision DESC LIMIT 65")?;
        let mut rooms=self.db.prepare("SELECT account,room,inviter,reason,kind,revision,secret_id,password_ready,password_error FROM pending_room_invitations WHERE account=? ORDER BY revision DESC LIMIT 64")?;
        let mut contacts = contacts
            .query_map(params![owner, before], contact_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let more = contacts.len() > 64;
        contacts.truncate(64);
        let contact_cursor = more.then(|| contacts.last().unwrap().revision);
        Ok(PendingRequests {
            contacts,
            rooms: rooms
                .query_map([owner], room_row)?
                .collect::<rusqlite::Result<_>>()?,
            contact_cursor,
            contact_total: self
                .db
                .query_row(
                    "SELECT COUNT(*) FROM pending_contacts WHERE account=?",
                    [owner],
                    |row| row.get::<_, i64>(0),
                )
                .map(|value| value as usize)?,
        })
    }
    pub fn pending_request_count(&self, owner: &str) -> rusqlite::Result<usize> {
        self.db.query_row("SELECT (SELECT COUNT(*) FROM pending_contacts WHERE account=?1)+(SELECT COUNT(*) FROM pending_room_invitations WHERE account=?1)",[owner],|row|row.get::<_,i64>(0)).map(|value|value as usize)
    }
    pub fn contact_request(
        &self,
        owner: &str,
        peer: &str,
    ) -> rusqlite::Result<Option<ContactRequest>> {
        self.db.query_row("SELECT account,peer,description,revision FROM pending_contacts WHERE account=? AND peer=?",params![owner,peer],contact_row).optional()
    }
    pub fn room_invitation(
        &self,
        owner: &str,
        room: &str,
    ) -> rusqlite::Result<Option<RoomInvitation>> {
        self.db.query_row("SELECT account,room,inviter,reason,kind,revision,secret_id,password_ready,password_error FROM pending_room_invitations WHERE account=? AND room=?",params![owner,room],room_row).optional()
    }
    pub fn upsert_contact_request(
        &self,
        owner: &str,
        peer: &str,
    ) -> rusqlite::Result<Option<ContactRequest>> {
        if crate::protocol::bare(owner).ok().as_deref() != Some(owner)
            || crate::protocol::bare(peer).ok().as_deref() != Some(peer)
            || owner == peer
        {
            return Err(rusqlite::Error::InvalidQuery);
        }
        let tx = self.db.unchecked_transaction()?;
        let revision = revision(&tx, owner)?;
        let description = "Wants to contact you and see your presence";
        tx.execute("INSERT INTO pending_contacts VALUES(?,?,?,?) ON CONFLICT(account,peer) DO UPDATE SET description=excluded.description,revision=excluded.revision",params![owner,peer,description,revision as i64])?;
        tx.commit()?;
        Ok(Some(ContactRequest {
            owner: owner.into(),
            peer: peer.into(),
            description: description.into(),
            revision,
        }))
    }
    pub fn upsert_room_invitation(
        &self,
        owner: &str,
        incoming: &Incoming,
    ) -> rusqlite::Result<Option<InvitationUpdate>> {
        if crate::protocol::bare(owner).ok().as_deref() != Some(owner)
            || crate::protocol::bare(&incoming.room).ok().as_deref() != Some(incoming.room.as_str())
            || incoming.reason.encode_utf16().count() > 512
            || incoming.inviter.encode_utf16().count() > 3071
            || incoming
                .inviter
                .parse::<tokio_xmpp::jid::Jid>()
                .ok()
                .is_none_or(|jid| jid.to_bare().node().is_none())
        {
            return Err(rusqlite::Error::InvalidQuery);
        }
        let tx = self.db.unchecked_transaction()?;
        let old = tx.query_row("SELECT account,room,inviter,reason,kind,revision,secret_id,password_ready,password_error FROM pending_room_invitations WHERE account=? AND room=?",params![owner,incoming.room],room_row).optional()?;
        if old.is_none()
            && tx.query_row(
                "SELECT COUNT(*) FROM pending_room_invitations WHERE account=?",
                [owner],
                |row| row.get::<_, i64>(0),
            )? >= 64
        {
            return Ok(None);
        }
        let new_password = incoming
            .password
            .as_ref()
            .is_some_and(|value| !value.0.is_empty());
        let secret_id = if new_password {
            Some(uuid::Uuid::new_v4().to_string())
        } else {
            old.as_ref().and_then(|value| value.secret_id.clone())
        };
        let ready = if new_password {
            false
        } else {
            old.as_ref().is_none_or(|value| value.password_ready)
        };
        let error = if new_password {
            None
        } else {
            old.as_ref().and_then(|value| value.password_error.clone())
        };
        let revision = revision(&tx, owner)?;
        let kind = match incoming.kind {
            Kind::Direct => "direct",
            Kind::Mediated => "mediated",
        };
        tx.execute("INSERT INTO pending_room_invitations VALUES(?,?,?,?,?,?,?,?,?) ON CONFLICT(account,room) DO UPDATE SET inviter=excluded.inviter,reason=excluded.reason,kind=excluded.kind,revision=excluded.revision,secret_id=excluded.secret_id,password_ready=excluded.password_ready,password_error=excluded.password_error",params![owner,incoming.room,incoming.inviter,incoming.reason,kind,revision as i64,secret_id,ready,error])?;
        let replaced_secret = if new_password {
            old.and_then(|value| value.secret_id)
        } else {
            None
        };
        if let Some(old) = replaced_secret.as_deref() {
            tx.execute(
                "INSERT OR IGNORE INTO invitation_secret_cleanup VALUES(?,?,?)",
                params![owner, incoming.room, old],
            )?;
        }
        tx.commit()?;
        Ok(Some(InvitationUpdate {
            record: RoomInvitation {
                owner: owner.into(),
                room: incoming.room.clone(),
                inviter: incoming.inviter.clone(),
                reason: incoming.reason.clone(),
                kind: incoming.kind,
                revision,
                secret_id,
                password_ready: ready,
                password_error: error,
            },
            replaced_secret,
        }))
    }
    /// Secret identity survives metadata-only updates; late writes cannot replace a newer password.
    pub fn finish_invitation_secret(
        &self,
        owner: &str,
        room: &str,
        secret_id: &str,
        error: Option<&str>,
    ) -> rusqlite::Result<bool> {
        self.db.execute("UPDATE pending_room_invitations SET password_ready=?,password_error=? WHERE account=? AND room=? AND secret_id=?",params![error.is_none(),error,owner,room,secret_id]).map(|count|count==1)
    }
    pub fn remove_contact_request(&self, record: &ContactRequest) -> rusqlite::Result<bool> {
        self.db
            .execute(
                "DELETE FROM pending_contacts WHERE account=? AND peer=? AND revision=?",
                params![
                    record.owner,
                    record.peer,
                    i64::try_from(record.revision).map_err(|_| rusqlite::Error::InvalidQuery)?
                ],
            )
            .map(|count| count == 1)
    }
    pub fn remove_contact_requests(&self, owner: &str, peers: &[String]) -> rusqlite::Result<bool> {
        let tx = self.db.unchecked_transaction()?;
        let mut count = 0;
        {
            let mut remove =
                tx.prepare("DELETE FROM pending_contacts WHERE account=? AND peer=?")?;
            for peer in peers {
                count += remove.execute(params![owner, peer])?;
            }
        }
        tx.commit()?;
        Ok(count > 0)
    }
    pub fn remove_room_invitation(&self, record: &RoomInvitation) -> rusqlite::Result<bool> {
        let tx = self.db.unchecked_transaction()?;
        let current=tx.query_row("SELECT secret_id FROM pending_room_invitations WHERE account=? AND room=? AND revision=?",params![record.owner,record.room,i64::try_from(record.revision).map_err(|_|rusqlite::Error::InvalidQuery)?],|row|row.get::<_,Option<String>>(0)).optional()?;
        let Some(secret) = current else {
            return Ok(false);
        };
        if let Some(secret) = secret {
            tx.execute(
                "INSERT OR IGNORE INTO invitation_secret_cleanup VALUES(?,?,?)",
                params![record.owner, record.room, secret],
            )?;
        }
        let count = tx.execute(
            "DELETE FROM pending_room_invitations WHERE account=? AND room=? AND revision=?",
            params![
                record.owner,
                record.room,
                i64::try_from(record.revision).map_err(|_| rusqlite::Error::InvalidQuery)?
            ],
        )?;
        tx.commit()?;
        Ok(count == 1)
    }
    pub fn queue_invitation_secret_cleanup(
        &self,
        owner: &str,
        room: &str,
        secret: &str,
    ) -> rusqlite::Result<()> {
        self.db
            .execute(
                "INSERT OR IGNORE INTO invitation_secret_cleanup VALUES(?,?,?)",
                params![owner, room, secret],
            )
            .map(|_| ())
    }
    pub fn invitation_secret_cleanup(
        &self,
        owner: &str,
    ) -> rusqlite::Result<Vec<(String, String)>> {
        let mut query=self.db.prepare("SELECT room,secret_id FROM invitation_secret_cleanup WHERE account=? ORDER BY rowid LIMIT 32")?;
        query
            .query_map([owner], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect()
    }
    pub fn finish_invitation_secret_cleanup(
        &self,
        owner: &str,
        room: &str,
        secret: &str,
    ) -> rusqlite::Result<()> {
        self.db
            .execute(
                "DELETE FROM invitation_secret_cleanup WHERE account=? AND room=? AND secret_id=?",
                params![owner, room, secret],
            )
            .map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pending_requests_survive_load_and_keep_newer_metadata_and_secrets() {
        let store = Store::open(true).unwrap();
        let owner = "own@example.org";
        store
            .save_account(&Account {
                jid: owner.into(),
                host: String::new(),
                port: 5222,
                direct_tls: false,
            })
            .unwrap();
        let mut incoming = Incoming {
            room: "room@conference.example.org".into(),
            inviter: "friend@example.org".into(),
            reason: "Come join".into(),
            kind: Kind::Mediated,
            password: Some(crate::rooms::Password(zeroize::Zeroizing::new(
                "not-in-sqlite".into(),
            ))),
        };
        let first = store
            .upsert_room_invitation(owner, &incoming)
            .unwrap()
            .unwrap()
            .record;
        let secret = first.secret_id.clone().unwrap();
        incoming.password = None;
        incoming.reason = "Updated reason".into();
        let next = store
            .upsert_room_invitation(owner, &incoming)
            .unwrap()
            .unwrap()
            .record;
        assert!(next.revision > first.revision);
        assert_eq!(next.secret_id.as_deref(), Some(secret.as_str()));
        assert!(
            store
                .finish_invitation_secret(owner, &incoming.room, &secret, None)
                .unwrap()
        );
        assert!(!store.remove_room_invitation(&first).unwrap());
        incoming.password = Some(crate::rooms::Password(zeroize::Zeroizing::new(
            "replacement-not-in-sqlite".into(),
        )));
        let replacement = store
            .upsert_room_invitation(owner, &incoming)
            .unwrap()
            .unwrap();
        assert_eq!(
            replacement.replaced_secret.as_deref(),
            Some(secret.as_str())
        );
        assert!(
            !store
                .finish_invitation_secret(owner, &incoming.room, &secret, None)
                .unwrap()
        );
        assert_eq!(
            store
                .room_invitation(owner, &incoming.room)
                .unwrap()
                .unwrap()
                .password_ready,
            false
        );
        for index in 1..64 {
            incoming.room = format!("room-{index}@conference.example.org");
            store
                .upsert_room_invitation(owner, &incoming)
                .unwrap()
                .unwrap();
        }
        incoming.room = "overflow@conference.example.org".into();
        assert!(
            store
                .upsert_room_invitation(owner, &incoming)
                .unwrap()
                .is_none()
        );
        assert_eq!(store.pending_requests(owner).unwrap().rooms.len(), 64);
        assert!(
            store
                .pending_requests("other@example.org")
                .unwrap()
                .rooms
                .is_empty()
        );
        let contact = store
            .upsert_contact_request(owner, "friend@example.org")
            .unwrap()
            .unwrap();
        assert_eq!(store.load_windowed(owner).requests.len(), 1);
        assert!(store.remove_contact_request(&contact).unwrap());
        let newer = store
            .upsert_contact_request(owner, "friend@example.org")
            .unwrap()
            .unwrap();
        assert!(newer.revision > contact.revision);
        assert!(!store.remove_contact_request(&contact).unwrap());
        for index in 0..130 {
            store
                .upsert_contact_request(owner, &format!("contact-{index}@example.org"))
                .unwrap()
                .unwrap();
        }
        let first_page = store.pending_requests(owner).unwrap();
        assert_eq!(first_page.contact_total, 131);
        assert_eq!(first_page.contacts.len(), 64);
        let second_page = store
            .pending_requests_before(owner, first_page.contact_cursor)
            .unwrap();
        let last_page = store
            .pending_requests_before(owner, second_page.contact_cursor)
            .unwrap();
        assert_eq!(second_page.contacts.len(), 64);
        assert_eq!(last_page.contacts.len(), 3);
        assert!(last_page.contact_cursor.is_none());
        let peers: std::collections::HashSet<_> = first_page
            .contacts
            .iter()
            .chain(&second_page.contacts)
            .chain(&last_page.contacts)
            .map(|request| request.peer.clone())
            .collect();
        assert_eq!(
            peers.len(),
            131,
            "pagination never hides the 65th contact or repeats rows"
        );
        let accepted = first_page.contacts[..2]
            .iter()
            .map(|request| request.peer.clone())
            .collect::<Vec<_>>();
        assert!(store.remove_contact_requests(owner, &accepted).unwrap());
        assert_eq!(store.pending_requests(owner).unwrap().contact_total, 129);
        assert_eq!(store.pending_request_count(owner).unwrap(), 129 + 64);
        let retired = (replacement.record.room.clone(), secret.clone());
        assert!(
            store
                .invitation_secret_cleanup(owner)
                .unwrap()
                .contains(&retired)
        );
        assert!(store.remove_room_invitation(&replacement.record).unwrap());
        assert!(
            store
                .invitation_secret_cleanup(owner)
                .unwrap()
                .iter()
                .any(|(_, id)| Some(id) == replacement.record.secret_id.as_ref())
        );
        store
            .finish_invitation_secret_cleanup(owner, &retired.0, &retired.1)
            .unwrap();
        assert!(
            !store
                .invitation_secret_cleanup(owner)
                .unwrap()
                .contains(&retired)
        );
        let serialized: Vec<String> = store
            .db
            .prepare("SELECT inviter||reason||COALESCE(secret_id,'') FROM pending_room_invitations")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert!(
            serialized
                .iter()
                .all(|value| !value.contains("not-in-sqlite"))
        );
        store.forget(owner).unwrap();
        assert!(store.pending_requests(owner).unwrap().rooms.is_empty());
        assert!(store.pending_requests(owner).unwrap().contacts.is_empty());
        assert!(store.invitation_secret_cleanup(owner).unwrap().is_empty());
    }
}
