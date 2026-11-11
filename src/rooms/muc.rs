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
    },
    Failed(String),
}
pub struct Rooms {
    owner: String,
    bookmarks: BTreeMap<String, Bookmark>,
    membership: BTreeMap<String, Membership>,
    stable_ids: BTreeSet<String>,
    identities: members::Registry,
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
            membership: BTreeMap::new(),
            stable_ids: BTreeSet::new(),
            identities: members::Registry::default(),
        }
    }
    /// Refresh bookmark metadata without discarding current verified membership.
    pub fn import(&mut self, bookmarks: Vec<Bookmark>) -> Vec<BookmarkInfo> {
        let info = bookmarks.iter().map(|entry| entry.info().clone()).collect();
        self.bookmarks = bookmarks
            .into_iter()
            .map(|entry| (entry.info().jid.clone(), entry))
            .collect();
        info
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
        join.append_child(attr(el("history", MUC), "maxstanzas", "30"));
        if let Some(password) = bookmark.and_then(Bookmark::password) {
            join.append_child(Element::builder("password", MUC).append(password).build());
        }
        let mut stanza = attr(el("presence", CLIENT), "to", occupant.to_string());
        stanza.append_child(join);
        self.identities.reset(&jid);
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
        let (expected, locked) = match previous {
            Membership::Joining { occupant, .. } => {
                (address == *occupant || code(user, "210"), false)
            }
            Membership::Joined {
                occupant, locked, ..
            } => (address == *occupant, *locked),
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
            let body = child(xml, "body", CLIENT).ok().flatten()?;
            if body.children().next().is_some() {
                return None;
            }
            body.text()
        };
        if text.trim().is_empty() || text.len() > 256 * 1024 {
            return None;
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
        let message = Message {
            text,
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
        if message.text.trim().is_empty()
            || message.text.len() > 256 * 1024
            || message.wire_id.is_empty()
            || message.wire_id.len() > 4096
            || message.unavailable
            || message.kind != "text"
            || message.attachment.is_some()
            || message.upload.is_some()
            || message.poll.is_some()
            || message.action.is_some()
            || message.forwarded.is_some()
            || !message.reply_id.is_empty()
            || !quote.is_empty()
        {
            return Err(Error::UnsupportedMessage);
        }
        Ok(Element::builder("message", CLIENT)
            .attr("type".try_into().unwrap(), "groupchat")
            .attr("to".try_into().unwrap(), &jid)
            .attr("id".try_into().unwrap(), &message.wire_id)
            .append(
                Element::builder("body", CLIENT)
                    .append(message.text.as_str())
                    .build(),
            )
            .append(attr(el("origin-id", SID), "id", &message.wire_id))
            .build())
    }
}
