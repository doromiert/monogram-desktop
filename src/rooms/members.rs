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

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RoomSender {
    pub room: String,
    pub occupant: String,
    pub author: String,
}

/// One room occupant as shown in the participants list. Built from presence only, so it is
/// available in public rooms too; `jid` is set only when the room discloses real addresses.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Participant {
    pub nick: String,
    pub jid: Option<String>,
    pub affiliation: String,
    pub role: String,
    /// XMPP `show` value: empty (available), away, xa or dnd.
    pub show: String,
    pub own: bool,
}

#[derive(Default)]
pub(super) struct Registry {
    serial: u64,
    rooms: BTreeMap<String, Identities>,
    /// Rooms whose roster was cleared and not yet reported.
    gone: std::collections::BTreeSet<String>,
}
#[derive(Default)]
struct Identities {
    revision: u64,
    occupants: BTreeMap<String, (Option<String>, String)>,
    roster: BTreeMap<String, Participant>,
    roster_dirty: bool,
    invalid: bool,
    pending: Option<u64>,
    attempted: Option<u64>,
    snapshot: Option<MemberSnapshot>,
}
impl Registry {
    pub fn revision(&self, room: &str) -> Option<u64> {
        self.rooms
            .get(room)
            .filter(|state| !state.invalid)
            .map(|state| state.revision)
    }
    pub fn install(&mut self, room: &str, revision: u64, snapshot: MemberSnapshot) -> Result<()> {
        let state = self.rooms.get_mut(room).ok_or(Error::NotJoined)?;
        if state.invalid || state.revision != revision || snapshot.room != room {
            return Err(Error::RecipientsUnavailable);
        }
        if state.snapshot.as_ref().is_some_and(|old| old != &snapshot) {
            self.serial = self.serial.wrapping_add(1);
            state.revision = self.serial;
        }
        state.snapshot = Some(snapshot);
        state.pending = None;
        state.attempted = Some(state.revision);
        Ok(())
    }
    pub fn reset(&mut self, room: &str) {
        self.gone.insert(room.into());
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
        if self.rooms.remove(room).is_some() {
            self.gone.insert(room.into());
        }
    }
    pub fn clear(&mut self) {
        let rooms = std::mem::take(&mut self.rooms);
        self.gone.extend(rooms.into_keys());
    }
    /// Rosters changed since the last call, as complete lists; a cleared room reports an empty one.
    pub fn take_roster_changes(&mut self) -> Vec<(String, Vec<Participant>)> {
        let mut changes = Vec::new();
        for (room, state) in self.rooms.iter_mut().filter(|(_, state)| state.roster_dirty) {
            state.roster_dirty = false;
            self.gone.remove(room);
            changes.push((room.clone(), state.roster.values().cloned().collect()));
        }
        for room in std::mem::take(&mut self.gone) {
            changes.push((room, Vec::new()));
        }
        changes
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
        let parsed = user_item(user);
        let item = parsed.as_ref().ok().and_then(|item| *item);
        let removed = xml.attr("type") == Some("unavailable");
        if let Some(item) = item {
            let nick = address
                .split_once('/')
                .map_or("", |(_, nick)| nick)
                .to_owned();
            if removed {
                state.roster_dirty |= state.roster.remove(address).is_some();
            } else if !nick.is_empty()
                && (state.roster.len() < MAX_MEMBERS || state.roster.contains_key(address))
            {
                let participant = Participant {
                    nick,
                    jid: item
                        .attr("jid")
                        .and_then(|real| real.parse::<Jid>().ok())
                        .filter(|jid| jid.node().is_some())
                        .map(|jid| jid.to_bare().to_string()),
                    affiliation: item.attr("affiliation").unwrap_or("none").into(),
                    role: item.attr("role").unwrap_or("participant").into(),
                    show: xml
                        .children()
                        .find(|child| child.name() == "show")
                        .map(|show| show.text())
                        .unwrap_or_default(),
                    own: user.children().any(|status| {
                        status.name() == "status" && status.attr("code") == Some("110")
                    }),
                };
                if state.roster.get(address) != Some(&participant) {
                    state.roster.insert(address.into(), participant);
                    state.roster_dirty = true;
                }
            }
        }
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
                    snapshot.affiliations.get(jid).is_none_or(|_| {
                        !matches!(affiliation.as_str(), "member" | "admin" | "owner")
                    })
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

/// Only a members-only, non-anonymous room discloses a complete recipient list. Anything else
/// (a public or semi-anonymous room) can never be end-to-end encrypted, so report that from the
/// room's features alone, before asking for member lists a public room refuses to give.
fn require_private(info: &Element) -> Result<()> {
    bounded(info)?;
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
    Ok(())
}

fn complete(room: &str, info: Element, lists: [Element; 3]) -> Result<MemberSnapshot> {
    require_private(&info)?;
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
    let info = request(iq, &room, el("query", DISCO)).await?;
    require_private(&info)?;
    let (member, admin, owner) = tokio::try_join!(
        list(Affiliation::Member),
        list(Affiliation::Admin),
        list(Affiliation::Owner),
    )?;
    complete(&room, info, [member, admin, owner])
}

#[cfg(test)]
mod tests {
    use super::*;
    fn occupant_presence(room: &str, nick: &str, extra: &str, kind: &str, user: &str) -> (Element, Element) {
        let xml: Element = format!(
            "<presence xmlns='jabber:client' from='{room}/{nick}'{kind}>{extra}<x xmlns='http://jabber.org/protocol/muc#user'>{user}</x></presence>"
        )
        .parse()
        .unwrap();
        let user = xml.get_child("x", "http://jabber.org/protocol/muc#user").unwrap().clone();
        (xml, user)
    }

    #[test]
    fn roster_follows_occupant_presence_and_reports_each_change_once() {
        let room = "room@conference.example.org";
        let mut registry = Registry::default();
        registry.reset(room);
        registry.take_roster_changes();
        let item = |affiliation: &str, jid: &str| {
            format!("<item affiliation='{affiliation}' role='participant'{jid}/>")
        };
        let (xml, user) = occupant_presence(room, "amy", "<show>away</show>", "", &item("admin", " jid='amy@example.org/phone'"));
        registry.presence(room, &format!("{room}/amy"), &xml, &user);
        let (xml, user) = occupant_presence(room, "me", "", "", &format!("{}<status code='110'/>", item("owner", "")));
        registry.presence(room, &format!("{room}/me"), &xml, &user);
        let changes = registry.take_roster_changes();
        assert_eq!(changes.len(), 1);
        let people = &changes[0].1;
        let amy = people.iter().find(|p| p.nick == "amy").unwrap();
        assert_eq!(
            (amy.affiliation.as_str(), amy.show.as_str(), amy.jid.as_deref(), amy.own),
            ("admin", "away", Some("amy@example.org"), false)
        );
        let me = people.iter().find(|p| p.nick == "me").unwrap();
        assert!(me.own && me.jid.is_none(), "anonymous rooms show no address");
        assert!(registry.take_roster_changes().is_empty(), "nothing changed since the last report");

        // An identical presence is not a change; leaving is.
        let (xml, user) = occupant_presence(room, "amy", "<show>away</show>", "", &item("admin", " jid='amy@example.org/phone'"));
        registry.presence(room, &format!("{room}/amy"), &xml, &user);
        assert!(registry.take_roster_changes().is_empty());
        let (xml, user) = occupant_presence(room, "amy", "", " type='unavailable'", &item("admin", ""));
        registry.presence(room, &format!("{room}/amy"), &xml, &user);
        let changes = registry.take_roster_changes();
        assert_eq!(changes[0].1.iter().map(|p| p.nick.as_str()).collect::<Vec<_>>(), ["me"]);
    }

    #[test]
    fn leaving_a_room_reports_an_empty_roster() {
        let room = "room@conference.example.org";
        let mut registry = Registry::default();
        registry.reset(room);
        let (xml, user) = occupant_presence(room, "amy", "", "", "<item affiliation='member' role='participant'/>");
        registry.presence(room, &format!("{room}/amy"), &xml, &user);
        registry.take_roster_changes();
        registry.remove(room);
        assert_eq!(registry.take_roster_changes(), vec![(room.to_owned(), vec![])]);
        assert!(registry.take_roster_changes().is_empty());
    }

    #[test]
    fn public_rooms_are_rejected_from_their_features_before_member_lists_are_requested() {
        let info = |features: &[&str]| -> Element {
            let features: String = features
                .iter()
                .map(|feature| format!("<feature var='{feature}'/>"))
                .collect();
            format!("<query xmlns='{DISCO}'>{features}</query>").parse().unwrap()
        };
        assert!(matches!(
            require_private(&info(&["muc_open", "muc_semianonymous", "muc_unmoderated"])),
            Err(Error::RecipientsUnavailable)
        ));
        assert!(matches!(
            require_private(&info(&["muc_membersonly"])),
            Err(Error::RecipientsUnavailable)
        ));
        assert!(require_private(&info(&["muc_membersonly", "muc_nonanonymous"])).is_ok());
    }

    #[test]
    fn admitted_affiliation_changes_keep_complete_recipients_but_revocation_blocks() {
        let room = "room@conference.example.org";
        let owner = "owner@example.org";
        for presence_affiliation in ["member", "admin", "owner", "none", "outcast"] {
            for listed in [Affiliation::Member, Affiliation::Admin, Affiliation::Owner] {
                let mut rooms = Rooms::new(owner.into());
                rooms.join(room, Some("own")).unwrap();
                let own: Element = format!("<presence xmlns='jabber:client' from='{room}/own'><x xmlns='http://jabber.org/protocol/muc#user'><item jid='{owner}/desktop' affiliation='member' role='participant'/><status code='110'/></x></presence>").parse().unwrap();
                rooms.presence(&own);
                let other: Element = format!("<presence xmlns='jabber:client' from='{room}/buddy'><x xmlns='http://jabber.org/protocol/muc#user'><item jid='buddy@example.org/phone' affiliation='{presence_affiliation}' role='participant'/></x></presence>").parse().unwrap();
                rooms.presence(&other);
                let revision = rooms.begin_member_refresh(room).unwrap();
                rooms.member_refresh_finished(
                    room,
                    revision,
                    Ok(MemberSnapshot {
                        room: room.into(),
                        affiliations: [
                            (owner.into(), Affiliation::Member),
                            ("buddy@example.org".into(), listed),
                        ]
                        .into(),
                    }),
                );
                assert_eq!(
                    rooms.recipients(room).is_ok(),
                    matches!(presence_affiliation, "member" | "admin" | "owner")
                );
                // A complete list still has to contain each identified occupant.
                let revision = rooms.recipient_revision(room).unwrap();
                assert!(
                    rooms
                        .install_recipients(
                            room,
                            revision,
                            MemberSnapshot {
                                room: room.into(),
                                affiliations: [(owner.into(), Affiliation::Member)].into()
                            }
                        )
                        .is_err()
                );
            }
        }
    }

    #[test]
    fn multisession_presence_confirms_join_and_keeps_recipient_identity() {
        let room = "room@conference.example.org";
        let owner = "owner@example.org";
        let mut rooms = Rooms::new(owner.into());
        rooms.join(room, Some("own")).unwrap();
        let presence = |nick: &str, author: &str, own: bool| -> Element {
            format!("<presence xmlns='jabber:client' from='{room}/{nick}' to='{owner}/desktop'><x xmlns='http://jabber.org/protocol/muc#user'><status code='100'/><item jid='{author}/phone' affiliation='member' role='participant'/><item jid='{author}/desktop' affiliation='member' role='participant'/>{}</x></presence>",if own {"<status code='110'/>"} else {""}).parse().unwrap()
        };
        assert!(
            rooms
                .presence(&presence("buddy", "buddy@example.org", false))
                .is_none()
        );
        let mut jobs = manage::Pending::default();
        let (reply, mut answer) = tokio::sync::oneshot::channel();
        jobs.insert(room.into(), rooms.join_id(room).unwrap(), reply);
        let joined = rooms
            .presence(&presence("own", owner, true))
            .expect("accept consistent resource items");
        jobs.changed(&rooms, room);
        assert!(joined.joined && joined.can_send);
        assert_eq!(answer.try_recv().unwrap().unwrap().nick, "own");
        let revision = rooms
            .begin_member_refresh(room)
            .expect("multi-item presence must not invalidate identity");
        rooms.member_refresh_finished(
            room,
            revision,
            Ok(MemberSnapshot {
                room: room.into(),
                affiliations: [
                    (owner.into(), Affiliation::Member),
                    ("buddy@example.org".into(), Affiliation::Member),
                ]
                .into(),
            }),
        );
        assert_eq!(rooms.recipients(room).unwrap().members.len(), 2);
        let message: Element =
            format!("<message xmlns='jabber:client' from='{room}/buddy' type='groupchat'/>")
                .parse()
                .unwrap();
        assert_eq!(
            rooms.sender(&message, false).unwrap().author,
            "buddy@example.org"
        );
    }
    #[test]
    fn multisession_self_presence_rejects_conflicting_items() {
        let room = "room@conference.example.org";
        let owner = "owner@example.org";
        for second in [
            "jid='attacker@example.org/phone' affiliation='member' role='participant'",
            "jid='owner@example.org/phone' affiliation='owner' role='participant'",
            "jid='owner@example.org/phone' affiliation='member' role='moderator'",
            "affiliation='member' role='participant'",
            "jid='invalid jid' affiliation='member' role='participant'",
            "jid='owner@example.org/phone' affiliation='member' role='participant' nick='other'",
        ] {
            let mut rooms = Rooms::new(owner.into());
            rooms.join(room, Some("own")).unwrap();
            let xml: Element = format!("<presence xmlns='jabber:client' from='{room}/own'><x xmlns='http://jabber.org/protocol/muc#user'><item jid='{owner}/desktop' affiliation='member' role='participant'/><item {second}/><status code='110'/></x></presence>").parse().unwrap();
            assert!(
                rooms.presence(&xml).is_none(),
                "reject inconsistent identity/permission data"
            );
            assert!(!rooms.is_joined(room));
        }
        let xml: Element = format!(
            "<x xmlns='http://jabber.org/protocol/muc#user'>{}</x>",
            format!("<item jid='{owner}/resource' role='participant' affiliation='member'/>")
                .repeat(129)
        )
        .parse()
        .unwrap();
        assert!(user_item(&xml).is_err(), "resource list must be bounded");
    }

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
            rooms.recipients(room).is_ok(),
            "an affiliation change between admitted roles does not remove membership"
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
