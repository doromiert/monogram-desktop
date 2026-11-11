//! Membership and sending permissions come from the room's XEP-0045 self presence.
use super::*;
use crate::{encryption_policy::Mode, model::Message};
use chrono::{DateTime, Local, SecondsFormat, Utc};
use std::{
    collections::{BTreeMap, BTreeSet},
    time::{Duration, Instant},
};

const MUC: &str = "http://jabber.org/protocol/muc";
const USER: &str = "http://jabber.org/protocol/muc#user";
const SID: &str = "urn:xmpp:sid:0";
const DELAY: &str = "urn:xmpp:delay";
const STANZAS: &str = "urn:ietf:params:xml:ns:xmpp-stanzas";

#[derive(Clone, Debug)]
pub struct Presence {
    pub jid: String,
    pub joined: bool,
    pub can_send: bool,
    pub detail: String,
}
#[derive(Debug)]
enum Membership {
    Joining {
        occupant: Jid,
        started: Instant,
    },
    Joined {
        occupant: Jid,
        can_send: bool,
        locked: bool,
        created: bool,
        owner: bool,
    },
    Failed(String),
}
pub struct Rooms {
    owner: String,
    bookmarks: BTreeMap<String, Bookmark>,
    server_bookmarks: BTreeMap<String, Bookmark>,
    membership: BTreeMap<String, Membership>,
    stable_ids: BTreeSet<String>,
    identities: members::Registry,
    choices: BTreeMap<String, LocalBookmark>,
    join_ids: BTreeMap<String, u64>,
    join_serial: u64,
}
pub(super) fn occupant(room: &str, nick: &str) -> Result<Jid> {
    if nick.is_empty() || nick.len() > 1023 {
        return Err(Error::Invalid);
    }
    let jid: Jid = format!("{room}/{nick}")
        .parse()
        .map_err(|_| Error::Invalid)?;
    if jid.to_bare().as_str() != room || jid.resource().is_none() {
        return Err(Error::Invalid);
    }
    Ok(jid)
}
fn code(user: &Element, code: &str) -> bool {
    user.children()
        .any(|status| status.is("status", USER) && status.attr("code") == Some(code))
}
fn error_detail(stanza: &Element) -> &'static str {
    let error = stanza.get_child("error", CLIENT);
    let has = |name| error.is_some_and(|e| e.get_child(name, STANZAS).is_some());
    if has("not-authorized") {
        "The room requires a valid password"
    } else if has("registration-required") {
        "This room is members only"
    } else if has("forbidden") {
        "You are not allowed to join this room"
    } else if has("conflict") {
        "This room nickname is already in use"
    } else if has("item-not-found") || has("gone") {
        "The room could not be found"
    } else if has("service-unavailable") {
        "The room is full or unavailable"
    } else {
        "Could not join this room"
    }
}
fn left_detail(user: &Element) -> &'static str {
    if code(user, "301") {
        "You were banned from this room"
    } else if code(user, "307") {
        "You were removed from this room"
    } else if code(user, "321") || code(user, "322") {
        "Room membership is no longer available"
    } else if code(user, "332") {
        "The room service shut down"
    } else {
        "Room is disconnected"
    }
}
impl Rooms {
    pub fn new(owner: String) -> Self {
        Self {
            owner,
            bookmarks: BTreeMap::new(),
            server_bookmarks: BTreeMap::new(),
            membership: BTreeMap::new(),
            stable_ids: BTreeSet::new(),
            identities: members::Registry::default(),
            choices: BTreeMap::new(),
            join_ids: BTreeMap::new(),
            join_serial: 0,
        }
    }
    /// Refresh bookmark metadata without discarding current verified membership.
    pub fn import(&mut self, bookmarks: Vec<Bookmark>) -> Vec<BookmarkInfo> {
        self.server_bookmarks = bookmarks
            .into_iter()
            .map(|entry| (entry.info().jid.clone(), entry))
            .collect();
        self.apply_choices(false);
        self.bookmarks
            .values()
            .map(|entry| entry.info().clone())
            .collect()
    }
    pub fn update_choices(&mut self, choices: Vec<LocalBookmark>) {
        self.choices = choices
            .into_iter()
            .filter(|entry| bare(&entry.info.jid).is_ok())
            .take(1024)
            .map(|entry| (entry.info.jid.clone(), entry))
            .collect();
        self.apply_choices(false);
    }
    pub fn cached_choices(&mut self) -> Vec<BookmarkInfo> {
        self.apply_choices(true);
        self.bookmarks
            .values()
            .map(|entry| entry.info().clone())
            .collect()
    }
    fn apply_choices(&mut self, offline: bool) {
        self.bookmarks = self.server_bookmarks.clone();
        for (room, choice) in &self.choices {
            if !choice.dirty && !offline {
                continue;
            }
            let password = self
                .bookmarks
                .get(room)
                .and_then(Bookmark::password)
                .map(|value| zeroize::Zeroizing::new(value.into()));
            if let Ok(entry) = Bookmark::new(choice.info.clone(), password) {
                self.bookmarks.insert(room.clone(), entry);
            }
        }
    }
    pub fn preferred_nick(&self, room: &str) -> String {
        self.bookmarks
            .get(room)
            .and_then(|entry| entry.info().nick.clone())
            .unwrap_or_else(|| {
                self.owner
                    .split_once('@')
                    .map(|(name, _)| name.to_string())
                    .unwrap_or_default()
            })
    }
    pub fn imported_password(&self, room: &str) -> Option<manage::Password> {
        self.bookmarks
            .get(room)
            .and_then(Bookmark::password)
            .map(|value| manage::Password(zeroize::Zeroizing::new(value.into())))
    }
    pub fn known(&self, jid: &str) -> bool {
        self.bookmarks.contains_key(jid) || self.membership.contains_key(jid)
    }
    pub fn autojoin(&self) -> Vec<String> {
        self.bookmarks
            .values()
            .filter(|entry| {
                entry.info().autojoin && !self.membership.contains_key(&entry.info().jid)
            })
            .map(|entry| entry.info().jid.clone())
            .collect()
    }
    pub fn join(&mut self, jid: &str, nick: Option<&str>) -> Result<Element> {
        self.join_with(jid, nick, None, 30)
    }
    pub fn join_with(
        &mut self,
        jid: &str,
        nick: Option<&str>,
        password: Option<&str>,
        history: u32,
    ) -> Result<Element> {
        let jid = bare(jid)?;
        match self.membership.get(&jid) {
            Some(Membership::Joining { .. }) => return Err(Error::Joining),
            Some(Membership::Joined { .. }) => return Err(Error::AlreadyJoined),
            _ => {}
        }
        let bookmark = self.bookmarks.get(&jid);
        let nick = nick
            .filter(|nick| !nick.is_empty())
            .or_else(|| bookmark.and_then(|entry| entry.info().nick.as_deref()))
            .or_else(|| self.owner.split_once('@').map(|(local, _)| local))
            .ok_or(Error::Invalid)?;
        let occupant = occupant(&jid, nick)?;
        let mut join = el("x", MUC);
        join.append_child(attr(
            el("history", MUC),
            "maxstanzas",
            history.min(50).to_string(),
        ));
        if let Some(password) = password.or_else(|| bookmark.and_then(Bookmark::password)) {
            if password.encode_utf16().count() > 4096 {
                return Err(Error::Invalid);
            }
            join.append_child(Element::builder("password", MUC).append(password).build());
        }
        let mut stanza = attr(el("presence", CLIENT), "to", occupant.to_string());
        stanza.append_child(join);
        self.identities.reset(&jid);
        self.join_serial = self.join_serial.wrapping_add(1);
        self.join_ids.insert(jid.clone(), self.join_serial);
        self.membership.insert(
            jid,
            Membership::Joining {
                occupant,
                started: Instant::now(),
            },
        );
        Ok(stanza)
    }
    pub fn disconnect(&mut self) {
        self.membership.clear();
        self.stable_ids.clear();
        self.identities.clear();
        self.join_ids.clear();
    }

    pub fn enable_stable_ids(&mut self, room: String) {
        if self.known(&room) {
            self.stable_ids.insert(room);
        }
    }
    pub fn status(&self, jid: &str) -> Presence {
        let (joined, can_send, detail) = match self.membership.get(jid) {
            Some(Membership::Joined { locked: true, .. }) => {
                (true, false, "Room requires configuration")
            }
            Some(Membership::Joined { can_send: true, .. }) => (true, true, "Joined"),
            Some(Membership::Joined {
                can_send: false, ..
            }) => (true, false, "Joined · read only"),
            Some(Membership::Joining { .. }) => (false, false, "Joining…"),
            Some(Membership::Failed(detail)) => (false, false, detail.as_str()),
            None => (false, false, "Not joined"),
        };
        Presence {
            jid: jid.into(),
            joined,
            can_send,
            detail: detail.into(),
        }
    }
    pub fn expire(&mut self) -> Vec<Presence> {
        let expired: Vec<_> = self.membership.iter().filter_map(|(jid, membership)| {
            matches!(membership, Membership::Joining { started, .. } if started.elapsed() >= Duration::from_secs(20)).then_some(jid.clone())
        }).collect();
        expired
            .into_iter()
            .map(|jid| {
                self.identities.remove(&jid);
                self.membership.insert(
                    jid.clone(),
                    Membership::Failed("The room did not confirm your join".into()),
                );
                self.status(&jid)
            })
            .collect()
    }
    pub fn presence(&mut self, xml: &Element) -> Option<Presence> {
        if !xml.is("presence", CLIENT) || !addressed_to(xml, &self.owner) {
            return None;
        }
        let address = xml.attr("from")?.parse::<Jid>().ok()?;
        let jid = address.to_bare().as_str().to_owned();
        let previous = self.membership.get(&jid)?;
        if xml.attr("type") == Some("error") {
            if matches!(previous, Membership::Joining { occupant, .. } if address == *occupant || address.resource().is_none())
            {
                self.identities.remove(&jid);
                self.membership
                    .insert(jid.clone(), Membership::Failed(error_detail(xml).into()));
                return Some(self.status(&jid));
            }
            return None;
        }
        let user = child(xml, "x", USER).ok().flatten()?;
        if address.resource().is_none()
            || matches!(previous, Membership::Failed(_))
            || !matches!(xml.attr("type"), None | Some("available" | "unavailable"))
            || xml.get_child("delay", DELAY).is_some()
            || xml.get_child("x", "jabber:x:delay").is_some()
        {
            return None;
        }
        if !code(user, "110") {
            self.identities
                .presence(&jid, &address.to_string(), xml, user);
            return None;
        }
        let item = child(user, "item", USER).ok().flatten();
        if item
            .and_then(|value| value.attr("jid"))
            .is_some_and(|real| {
                real.parse::<Jid>()
                    .ok()
                    .is_none_or(|jid| jid.to_bare().as_str() != self.owner)
            })
        {
            return None;
        }
        let (expected, locked, created) = match previous {
            Membership::Joining { occupant, .. } => {
                (address == *occupant || code(user, "210"), false, false)
            }
            Membership::Joined {
                occupant,
                locked,
                created,
                ..
            } => (address == *occupant, *locked, *created),
            Membership::Failed(_) => return None,
        };
        if !expected {
            return None;
        }
        self.identities
            .presence(&jid, &address.to_string(), xml, user);
        match xml.attr("type") {
            Some("unavailable") => {
                let replacement = if code(user, "303") {
                    item.and_then(|value| value.attr("nick"))
                        .and_then(|nick| occupant(&jid, nick).ok())
                } else {
                    None
                };
                let next = if let Some(occupant) = replacement {
                    self.join_serial = self.join_serial.wrapping_add(1);
                    self.join_ids.insert(jid.clone(), self.join_serial);
                    Membership::Joining {
                        occupant,
                        started: Instant::now(),
                    }
                } else {
                    Membership::Failed(left_detail(user).into())
                };
                if matches!(next, Membership::Failed(_)) {
                    self.identities.remove(&jid);
                }
                self.membership.insert(jid.clone(), next);
            }
            None | Some("available") => {
                let role = item?.attr("role")?;
                if !matches!(role, "moderator" | "participant" | "visitor") {
                    return None;
                }
                self.membership.insert(
                    jid.clone(),
                    Membership::Joined {
                        occupant: address,
                        can_send: matches!(role, "moderator" | "participant"),
                        locked: locked || code(user, "201"),
                        created: created || code(user, "201"),
                        owner: item?.attr("affiliation") == Some("owner"),
                    },
                );
            }
            _ => return None,
        }
        Some(self.status(&jid))
    }
    pub fn is_joined(&self, jid: &str) -> bool {
        matches!(self.membership.get(jid), Some(Membership::Joined { .. }))
    }
    pub fn joining(&self, room: &str) -> bool {
        matches!(self.membership.get(room), Some(Membership::Joining { .. }))
    }
    pub fn join_id(&self, room: &str) -> Option<u64> {
        self.join_ids.get(room).copied()
    }
    pub fn join_receipt(&self, room: &str) -> Option<manage::JoinReceipt> {
        let Membership::Joined {
            occupant,
            created,
            owner,
            ..
        } = self.membership.get(room)?
        else {
            return None;
        };
        Some(manage::JoinReceipt {
            room: room.into(),
            nick: occupant.resource()?.as_str().into(),
            id: self.join_id(room)?,
            created: *created,
            owner: *owner,
        })
    }
    pub fn configured(&mut self, room: &str, id: u64) -> Result<()> {
        if self.join_id(room) != Some(id) {
            return Err(Error::Cancelled);
        }
        let Some(Membership::Joined {
            locked,
            created,
            owner,
            ..
        }) = self.membership.get_mut(room)
        else {
            return Err(Error::NotJoined);
        };
        if !*created || !*owner {
            return Err(Error::Forbidden);
        }
        *locked = false;
        Ok(())
    }
    pub fn leave(&mut self, room: &str) -> Result<Element> {
        let room = bare(room)?;
        let address = match self.membership.get(&room) {
            Some(Membership::Joining { occupant, .. } | Membership::Joined { occupant, .. }) => {
                occupant.clone()
            }
            _ => return Err(Error::NotJoined),
        };
        self.membership.remove(&room);
        self.identities.remove(&room);
        self.join_ids.remove(&room);
        self.stable_ids.remove(&room);
        Ok(attr(
            attr(el("presence", CLIENT), "to", address.to_string()),
            "type",
            "unavailable",
        ))
    }

    pub fn begin_member_refresh(&mut self, room: &str) -> Option<u64> {
        if !self.status(room).can_send {
            return None;
        }
        self.identities.begin(room)
    }
    pub fn member_refresh_finished(
        &mut self,
        room: &str,
        revision: u64,
        result: Result<members::MemberSnapshot>,
    ) {
        self.identities.finish(room, revision, result);
    }
    pub fn recipients(&self, room: &str) -> Result<members::RoomRecipients> {
        if !self.is_joined(room) {
            return Err(Error::NotJoined);
        }
        if !self.status(room).can_send {
            return Err(Error::Forbidden);
        }
        self.identities.recipients(room, &self.owner)
    }
    pub fn recipient_revision(&self, room: &str) -> Result<u64> {
        if !self.is_joined(room) {
            return Err(Error::NotJoined);
        }
        if !self.status(room).can_send {
            return Err(Error::Forbidden);
        }
        self.identities
            .revision(room)
            .ok_or(Error::RecipientsUnavailable)
    }
    pub fn install_recipients(
        &mut self,
        room: &str,
        revision: u64,
        snapshot: members::MemberSnapshot,
    ) -> Result<members::RoomRecipients> {
        if self.recipient_revision(room)? != revision {
            return Err(Error::RecipientsUnavailable);
        }
        self.identities.install(room, revision, snapshot)?;
        self.recipients(room)
    }
    /// Capture this before crypto waits; historical nicknames never use today's map.
    pub fn sender(&self, xml: &Element, archived: bool) -> Result<members::RoomSender> {
        if !xml.is("message", CLIENT)
            || xml.attr("type") != Some("groupchat")
            || !addressed_to(xml, &self.owner)
        {
            return Err(Error::Invalid);
        }
        bounded(xml)?;
        let address = xml
            .attr("from")
            .ok_or(Error::Invalid)?
            .parse::<Jid>()
            .map_err(|_| Error::Invalid)?;
        let room = address.to_bare().to_string();
        if address.resource().is_none() || !self.is_joined(&room) {
            return Err(Error::NotJoined);
        }
        let occupant = address.to_string();
        let author = if archived
            || xml.get_child("delay", DELAY).is_some()
            || xml.get_child("x", "jabber:x:delay").is_some()
            || xml.get_child("forwarded", "urn:xmpp:forward:0").is_some()
            || xml.children().any(|child| {
                matches!(
                    child.ns().as_str(),
                    "urn:xmpp:carbons:2" | "urn:xmpp:mam:2" | "urn:xmpp:mam:1"
                )
            }) {
            let user = child(xml, "x", USER)?.ok_or(Error::Invalid)?;
            let item = child(user, "item", USER)?.ok_or(Error::Invalid)?;
            let real = item
                .attr("jid")
                .ok_or(Error::Invalid)?
                .parse::<Jid>()
                .map_err(|_| Error::Invalid)?;
            if real.node().is_none() {
                return Err(Error::Invalid);
            }
            real.to_bare().to_string()
        } else {
            self.identities
                .author(&room, &occupant)
                .ok_or(Error::RecipientsUnavailable)?
        };
        Ok(members::RoomSender {
            room,
            occupant,
            author,
        })
    }
    pub fn configuration_changed(&mut self, xml: &Element) {
        if !xml.is("message", CLIENT)
            || xml.attr("type") != Some("groupchat")
            || !addressed_to(xml, &self.owner)
            || xml.get_child("delay", DELAY).is_some()
            || xml.get_child("x", "jabber:x:delay").is_some()
            || xml.children().any(|child| {
                matches!(
                    child.ns().as_str(),
                    "urn:xmpp:forward:0"
                        | "urn:xmpp:carbons:2"
                        | "urn:xmpp:mam:2"
                        | "urn:xmpp:mam:1"
                )
            })
            || bounded(xml).is_err()
        {
            return;
        }
        let Some(room) = xml.attr("from").filter(|room| self.is_joined(room)) else {
            return;
        };
        if let Ok(Some(user)) = child(xml, "x", USER)
            && ["104", "172", "173"].iter().any(|value| code(user, value))
        {
            self.identities.invalidate(room);
        }
    }

    pub fn archived_message(
        &self,
        room: &str,
        archive: archive::ResultStanza<'_>,
    ) -> Option<Message> {
        let address = archive.xml.attr("from")?.parse::<Jid>().ok()?;
        if address.to_bare().as_str() != room || archive.xml.attr("to").is_some() {
            return None;
        }
        let (_, message, _) =
            self.decode_message(archive.xml, Some((archive.uid, archive.stamp)))?;
        Some(message)
    }

    pub fn message(&self, xml: &Element) -> Option<(String, Message, bool)> {
        self.decode_message(xml, None)
    }

    fn decode_message(
        &self,
        xml: &Element,
        archive: Option<(&str, &str)>,
    ) -> Option<(String, Message, bool)> {
        if !xml.is("message", CLIENT)
            || xml.attr("type") != Some("groupchat")
            || !addressed_to(xml, &self.owner)
        {
            return None;
        }
        let address = xml.attr("from")?.parse::<Jid>().ok()?;
        let jid = address.to_bare().as_str().to_owned();
        let Membership::Joined { occupant, .. } = self.membership.get(&jid)? else {
            return None;
        };
        let nick = address.resource()?.as_str();
        bounded(xml).ok()?;
        let delay = child(xml, "delay", DELAY).ok()?;
        let historical =
            archive.is_some() || delay.is_some() || xml.get_child("x", "jabber:x:delay").is_some();
        // Only the archive's sanitized MUC user element can establish historical ownership.
        let own = if archive.is_some() {
            child(xml, "x", USER)
                .ok()?
                .and_then(|user| child(user, "item", USER).ok().flatten())
                .and_then(|item| item.attr("jid"))
                .and_then(|real| real.parse::<Jid>().ok())
                .is_some_and(|real| real.to_bare().as_str() == self.owner)
        } else {
            !historical && address == *occupant
        };
        let stamp = archive
            .map(|(_, stamp)| stamp)
            .or_else(|| delay.and_then(|delay| delay.attr("stamp")));
        let date = stamp
            .and_then(|stamp| DateTime::parse_from_rfc3339(stamp).ok())
            .map(|date| date.with_timezone(&Utc))
            .unwrap_or_else(Utc::now);
        let unavailable = xml
            .children()
            .any(|child| child.name() == "encrypted" || child.is("encryption", "urn:xmpp:eme:0"));
        let text = if unavailable {
            "Encrypted room message · this encryption is unavailable on desktop".into()
        } else {
            match child(xml, "body", CLIENT).ok().flatten() {
                Some(body) if body.children().next().is_none() => body.text(),
                None => String::new(),
                _ => return None,
            }
        };
        let attachment = (!unavailable)
            .then(|| crate::file_share::Attachment::incoming(xml, &text, false))
            .flatten();
        if (text.trim().is_empty() && attachment.is_none()) || text.len() > 256 * 1024 {
            return None;
        }
        let caption = attachment.as_ref().map(|file| file.caption(xml, &text));
        let (reply_id, mut reply_to, reply, text) = if unavailable {
            (String::new(), String::new(), String::new(), text)
        } else {
            crate::protocol::reply_content(xml, &text)
        };
        let text = caption.unwrap_or(text);
        if !reply_id.is_empty()
            && let Some(address) = xml
                .get_child("reply", "urn:xmpp:reply:0")
                .and_then(|reply| reply.attr("to"))
                .and_then(|to| to.parse::<Jid>().ok())
            && address.to_bare().as_str() == jid
        {
            reply_to = address.to_string();
        }
        let wire_id = xml
            .attr("id")
            .filter(|id| !id.is_empty() && id.len() <= 4096)
            .unwrap_or_default()
            .to_owned();
        let mut aliases = vec![];
        let uid = if let Some((uid, _)) = archive {
            Some(uid)
        } else if self.stable_ids.contains(&jid) {
            let mut ids = xml
                .children()
                .filter(|id| id.is("stanza-id", SID) && id.attr("by") == Some(&jid));
            let id = ids
                .next()
                .filter(|id| id.children().next().is_none() && id.text().is_empty());
            id.filter(|_| ids.next().is_none())
                .and_then(|id| id.attr("id"))
                .filter(|uid| archive::valid_uid(uid))
        } else {
            None
        };
        if let Some(uid) = uid {
            aliases.push(format!("room-archive:{uid}"));
        }
        if !wire_id.is_empty() {
            aliases.push(format!("wire:{wire_id}"));
        }
        if own {
            if let Some(id) = child(xml, "origin-id", SID)
                .ok()
                .flatten()
                .and_then(|root| root.attr("id"))
                .filter(|id| !id.is_empty() && id.len() <= 4096)
            {
                aliases.push(format!("origin:{id}"));
            }
        }
        let mut message = Message {
            kind: if attachment.is_some() { "file" } else { "text" }.into(),
            attachment,
            unsupported_attachment: xml
                .get_child("file-sharing", crate::file_share::NS)
                .is_some()
                || xml.get_child("x", crate::file_share::OOB).is_some(),
            text,
            reply_id,
            reply_to,
            reply,
            wire_id,
            aliases,
            own,
            unavailable,
            sender: address.to_string(),
            author: if own { "You".into() } else { nick.into() },
            timestamp: date.to_rfc3339_opts(SecondsFormat::Micros, false),
            time: date.with_timezone(&Local).format("%H:%M").to_string(),
            delivery: "delivered".into(),
            ..Default::default()
        };
        message.hydrate_compatibility();
        message.inline_emoji = (!unavailable && message.attachment.is_none())
            .then(|| crate::packs::normalized_inline(xml, &message.text)).flatten();
        message.preview_spoilers = if unavailable {
            None
        } else {
            crate::spoilers::normalized(xml, &message.text)
        };
        Some((jid, message, historical))
    }
    pub fn outgoing(&self, peer: &str, message: &Message, quote: &str) -> Result<Element> {
        let jid = bare(peer)?;
        if message.mode() != Mode::Plain {
            return Err(Error::Encryption);
        }
        match self.membership.get(&jid) {
            Some(Membership::Joined { locked: true, .. }) => return Err(Error::Locked),
            Some(Membership::Joined {
                can_send: false, ..
            }) => return Err(Error::Forbidden),
            Some(Membership::Joined { .. }) => {}
            _ => return Err(Error::NotJoined),
        }
        if (message.text.trim().is_empty() && message.attachment.is_none())
            || message.text.len() > 256 * 1024
            || message.wire_id.is_empty()
            || message.wire_id.len() > 4096
            || message.unavailable
            || !matches!(message.kind.as_str(), "text" | "file")
            || message
                .attachment
                .as_ref()
                .is_some_and(|file| !file.valid_outgoing(false))
            || message.upload.is_some()
            || message.poll.is_some()
            || message.action.is_some()
            || message.forwarded.is_some()
            || (!message.reply_id.is_empty()
                && !crate::message_actions::valid_id(&message.reply_id))
        {
            return Err(Error::UnsupportedMessage);
        }
        let clear = crate::protocol::outgoing(&jid, message, quote);
        let mut root = Element::builder("message", CLIENT)
            .attr("type".try_into().unwrap(), "groupchat")
            .attr("to".try_into().unwrap(), &jid)
            .attr("id".try_into().unwrap(), &message.wire_id)
            .build();
        for child in clear
            .children()
            .filter(|child| !child.is("request", "urn:xmpp:receipts"))
        {
            root.append_child(child.clone());
        }
        Ok(root)
    }
}
