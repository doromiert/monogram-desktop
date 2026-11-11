//! Complete XEP-0045 recipient lists, including members who are currently offline.
use super::*;
use std::collections::BTreeMap;

const ADMIN: &str = "http://jabber.org/protocol/muc#admin";
const DISCO: &str = "http://jabber.org/protocol/disco#info";
pub(super) const MAX_MEMBERS: usize = 4096;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Affiliation {
    Member,
    Admin,
    Owner,
}
impl Affiliation {
    fn name(self) -> &'static str {
        match self {
            Self::Member => "member",
            Self::Admin => "admin",
            Self::Owner => "owner",
        }
    }
}

/// A complete server response, not proof that membership is still current.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MemberSnapshot {
    pub room: String,
    pub affiliations: BTreeMap<String, Affiliation>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RoomRecipients {
    pub room: String,
    pub revision: u64,
    pub members: BTreeMap<String, Affiliation>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RoomSender {
    pub room: String,
    pub occupant: String,
    pub author: String,
}

#[derive(Default)]
pub(super) struct Registry {
    serial: u64,
    rooms: BTreeMap<String, Identities>,
}
#[derive(Default)]
struct Identities {
    revision: u64,
    occupants: BTreeMap<String, (Option<String>, String)>,
    invalid: bool,
    pending: Option<u64>,
    attempted: Option<u64>,
    snapshot: Option<MemberSnapshot>,
}
impl Registry {
    pub fn reset(&mut self, room: &str) {
        self.serial = self.serial.wrapping_add(1);
        self.rooms.insert(
            room.into(),
            Identities {
                revision: self.serial,
                ..Default::default()
            },
        );
    }
    pub fn remove(&mut self, room: &str) {
        self.rooms.remove(room);
    }
    pub fn clear(&mut self) {
        self.rooms.clear();
    }
    pub fn invalidate(&mut self, room: &str) {
        if let Some(state) = self.rooms.get_mut(room) {
            self.serial = self.serial.wrapping_add(1);
            state.revision = self.serial;
            state.snapshot = None;
        }
    }
    pub fn presence(&mut self, room: &str, address: &str, xml: &Element, user: &Element) {
        let Some(state) = self.rooms.get_mut(room) else {
            return;
        };
        let parsed = child(user, "item", "http://jabber.org/protocol/muc#user");
        let item = parsed.as_ref().ok().and_then(|item| *item);
        let removed = xml.attr("type") == Some("unavailable");
        let author = item.and_then(|item| item.attr("jid")).and_then(|real| {
            let jid = real.parse::<Jid>().ok()?;
            jid.node()?;
            Some(jid.to_bare().to_string())
        });
        let affiliation = item
            .and_then(|item| item.attr("affiliation"))
            .unwrap_or("")
            .to_owned();
        let valid = parsed.is_ok() && item.is_some() && bounded(xml).is_ok();
        let value = (author, affiliation);
        let changed = if !valid
            || (!removed
                && state.occupants.len() >= MAX_MEMBERS
                && !state.occupants.contains_key(address))
        {
            state.invalid = true;
            true
        } else if removed {
            state.occupants.remove(address).is_some()
        } else if state.occupants.get(address) != Some(&value) {
            state.occupants.insert(address.into(), value);
            true
        } else {
            false
        };
        if changed {
            self.invalidate(room);
        }
    }
    pub fn begin(&mut self, room: &str) -> Option<u64> {
        let state = self.rooms.get_mut(room)?;
        if state.invalid || state.pending.is_some() || state.attempted == Some(state.revision) {
            return None;
        }
        state.pending = Some(state.revision);
        state.attempted = Some(state.revision);
        Some(state.revision)
    }
    pub fn finish(&mut self, room: &str, revision: u64, result: Result<MemberSnapshot>) {
        let Some(state) = self.rooms.get_mut(room) else {
            return;
        };
        if state.pending != Some(revision) {
            return;
        }
        state.pending = None;
        if state.revision == revision && !state.invalid {
            state.snapshot = result.ok().filter(|snapshot| snapshot.room == room);
        }
    }
    pub fn recipients(&self, room: &str, owner: &str) -> Result<RoomRecipients> {
        let state = self.rooms.get(room).ok_or(Error::NotJoined)?;
        let snapshot = state
            .snapshot
            .as_ref()
            .ok_or(Error::RecipientsUnavailable)?;
        if state.invalid
            || !snapshot.affiliations.contains_key(owner)
            || state.occupants.values().any(|(author, affiliation)| {
                author.as_ref().is_none_or(|jid| {
                    snapshot
                        .affiliations
                        .get(jid)
                        .is_none_or(|listed| listed.name() != affiliation)
                })
            })
        {
            return Err(Error::RecipientsUnavailable);
        }
        Ok(RoomRecipients {
            room: room.into(),
            revision: state.revision,
            members: snapshot.affiliations.clone(),
        })
    }
    pub fn author(&self, room: &str, occupant: &str) -> Option<String> {
        let state = self.rooms.get(room)?;
        if state.invalid {
            return None;
        }
        state.occupants.get(occupant)?.0.clone()
    }
}

async fn request(iq: &iq::Client, room: &str, payload: Element) -> Result<Element> {
    iq.request(room, iq::Kind::Get, payload)
        .await?
        .ok_or(Error::Invalid)
}

fn parse_list(xml: &Element, affiliation: Affiliation) -> Result<BTreeMap<String, Affiliation>> {
    bounded(xml)?;
    if !xml.is("query", ADMIN) {
        return Err(Error::Invalid);
    }
    let mut members = BTreeMap::new();
    for item in xml.children() {
        if !item.is("item", ADMIN) || item.attr("affiliation") != Some(affiliation.name()) {
            return Err(Error::Invalid);
        }
        let jid = bare(item.attr("jid").ok_or(Error::Invalid)?)?;
        if members.len() >= MAX_MEMBERS || members.insert(jid, affiliation).is_some() {
            return Err(Error::Invalid);
        }
    }
    Ok(members)
}

fn complete(room: &str, info: Element, lists: [Element; 3]) -> Result<MemberSnapshot> {
    bounded(&info)?;
    if !info.is("query", DISCO) || info.attr("node").is_some() {
        return Err(Error::Invalid);
    }
    let has = |feature| {
        info.children()
            .any(|child| child.is("feature", DISCO) && child.attr("var") == Some(feature))
    };
    if !has("muc_nonanonymous")
        || !has("muc_membersonly")
        || has("muc_semianonymous")
        || has("muc_anonymous")
        || has("muc_open")
    {
        return Err(Error::RecipientsUnavailable);
    }
    let mut affiliations = BTreeMap::new();
    for (list, affiliation) in
        lists
            .into_iter()
            .zip([Affiliation::Member, Affiliation::Admin, Affiliation::Owner])
    {
        for (jid, affiliation) in parse_list(&list, affiliation)? {
            if affiliations.len() >= MAX_MEMBERS || affiliations.insert(jid, affiliation).is_some()
            {
                return Err(Error::Invalid);
            }
        }
    }
    Ok(MemberSnapshot {
        room: room.into(),
        affiliations,
    })
}

/// Every reply must succeed; visible occupants alone never form an encryption list.
pub async fn fetch(iq: &iq::Client, room: &str) -> Result<MemberSnapshot> {
    let room = bare(room)?;
    let list = |affiliation: Affiliation| {
        let mut query = el("query", ADMIN);
        query.append_child(attr(el("item", ADMIN), "affiliation", affiliation.name()));
        request(iq, &room, query)
    };
    let (info, member, admin, owner) = tokio::try_join!(
        request(iq, &room, el("query", DISCO)),
        list(Affiliation::Member),
        list(Affiliation::Admin),
        list(Affiliation::Owner),
    )?;
    complete(&room, info, [member, admin, owner])
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn recipient_snapshot_tracks_prejoin_occupants_and_fences_lifecycle() {
        let room = "room@conference.example.org";
        let owner = "owner@example.org";
        let mut rooms = Rooms::new(owner.into());
        rooms.join(room, Some("own")).unwrap();
        let presence = |nick, jid: Option<&str>, own| {
            let jid = jid.map(|jid| format!(" jid='{jid}'")).unwrap_or_default();
            let status = if own { "<status code='110'/>" } else { "" };
            let affiliation = if own { "owner" } else { "member" };
            format!("<presence xmlns='jabber:client' from='{room}/{nick}'><x xmlns='http://jabber.org/protocol/muc#user'><item affiliation='{affiliation}' role='participant'{jid}/>{status}</x></presence>").parse::<Element>().unwrap()
        };
        rooms.presence(&presence("buddy", Some("buddy@example.org/phone"), false));
        assert!(rooms.begin_member_refresh(room).is_none());
        rooms.presence(&presence("own", Some(owner), true));
        let snapshot = MemberSnapshot {
            room: room.into(),
            affiliations: [
                (owner.into(), Affiliation::Owner),
                ("buddy@example.org".into(), Affiliation::Member),
                ("offline@example.org".into(), Affiliation::Member),
            ]
            .into(),
        };
        let revision = rooms.begin_member_refresh(room).unwrap();
        rooms.member_refresh_finished(room, revision, Ok(snapshot.clone()));
        assert_eq!(rooms.recipients(room).unwrap().members.len(), 3);
        let mut mismatched = snapshot.clone();
        mismatched
            .affiliations
            .insert("buddy@example.org".into(), Affiliation::Admin);
        rooms.configuration_changed(&format!("<message xmlns='jabber:client' from='{room}' type='groupchat'><x xmlns='http://jabber.org/protocol/muc#user'><status code='104'/></x></message>").parse().unwrap());
        let changed = rooms.begin_member_refresh(room).unwrap();
        rooms.member_refresh_finished(room, changed, Ok(mismatched));
        assert!(
            rooms.recipients(room).is_err(),
            "occupant affiliations must agree with the lists"
        );
        let message: Element =
            format!("<message xmlns='jabber:client' from='{room}/buddy' type='groupchat'/>")
                .parse()
                .unwrap();
        assert_eq!(
            rooms.sender(&message, false).unwrap().author,
            "buddy@example.org"
        );
        assert!(
            rooms.sender(&message, true).is_err(),
            "history cannot use today's nickname map"
        );
        rooms.presence(&presence("unidentified", None, false));
        let pending = rooms.begin_member_refresh(room).unwrap();
        rooms.presence(&presence("unidentified", Some("new@example.org"), false));
        rooms.member_refresh_finished(room, pending, Ok(snapshot.clone()));
        assert!(
            rooms.recipients(room).is_err(),
            "changed membership invalidates an in-flight result"
        );
        let replacement = rooms.begin_member_refresh(room).unwrap();
        assert_ne!(replacement, pending);
        rooms.member_refresh_finished(room, replacement, Ok(snapshot.clone()));
        assert!(
            rooms.recipients(room).is_err(),
            "every visible occupant must be in all-member lists"
        );
        rooms.disconnect();
        rooms.join(room, Some("own")).unwrap();
        rooms.presence(&presence("own", Some(owner), true));
        let rejoined = rooms.begin_member_refresh(room).unwrap();
        assert_ne!(rejoined, replacement);
        rooms.member_refresh_finished(room, replacement, Ok(snapshot.clone()));
        assert!(
            rooms.recipients(room).is_err(),
            "old session results cannot attach to a rejoin"
        );
        rooms.member_refresh_finished(room, rejoined, Ok(snapshot));
        assert!(rooms.recipients(room).is_ok());
        rooms.configuration_changed(&format!("<message xmlns='jabber:client' from='{room}' type='groupchat'><delay xmlns='urn:xmpp:delay' stamp='2026-01-01T00:00:00Z'/><x xmlns='http://jabber.org/protocol/muc#user'><status code='104'/></x></message>").parse().unwrap());
        assert!(
            rooms.recipients(room).is_ok(),
            "historical configuration cannot invalidate live membership"
        );
        rooms.configuration_changed(&format!("<message xmlns='jabber:client' from='{room}' type='groupchat'><x xmlns='http://jabber.org/protocol/muc#user'><status code='104'/></x></message>").parse().unwrap());
        let renaming = rooms.begin_member_refresh(room).unwrap();
        rooms.presence(&format!("<presence xmlns='jabber:client' from='{room}/own' type='unavailable'><x xmlns='http://jabber.org/protocol/muc#user'><item affiliation='owner' role='none' jid='{owner}' nick='renamed'/><status code='110'/><status code='303'/></x></presence>").parse().unwrap());
        rooms.member_refresh_finished(room, renaming, Err(Error::Invalid));
        rooms.presence(&presence("renamed", Some(owner), true));
        assert!(
            rooms.begin_member_refresh(room).is_some(),
            "nickname changes cannot strand an old in-flight query"
        );
    }
    #[test]
    fn complete_lists_reject_ambiguous_or_incomplete_identity() {
        let info: Element = format!("<query xmlns='{DISCO}'><feature var='muc_nonanonymous'/><feature var='muc_membersonly'/></query>").parse().unwrap();
        let list = |affiliation, jid| {
            format!(
                "<query xmlns='{ADMIN}'><item affiliation='{affiliation}' jid='{jid}'/></query>"
            )
            .parse::<Element>()
            .unwrap()
        };
        let empty: Element = format!("<query xmlns='{ADMIN}'/>").parse().unwrap();
        let lists = [
            list("member", "offline@example.org"),
            empty.clone(),
            list("owner", "owner@example.org"),
        ];
        let snapshot =
            complete("room@conference.example.org", info.clone(), lists.clone()).unwrap();
        assert_eq!(snapshot.affiliations.len(), 2);
        assert!(
            complete(
                "room@conference.example.org",
                info.clone(),
                [
                    lists[0].clone(),
                    empty.clone(),
                    list("owner", "offline@example.org")
                ]
            )
            .is_err()
        );
        assert!(
            parse_list(
                &list("member", "user@example.org/resource"),
                Affiliation::Member
            )
            .is_err()
        );
        assert!(parse_list(&list("admin", "user@example.org"), Affiliation::Member).is_err());
        assert!(parse_list(&list("member", "example.org"), Affiliation::Member).is_err());
        let anonymous: Element =
            format!("<query xmlns='{DISCO}'><feature var='muc_membersonly'/></query>")
                .parse()
                .unwrap();
        assert!(complete("room@conference.example.org", anonymous, lists).is_err());
    }
}
