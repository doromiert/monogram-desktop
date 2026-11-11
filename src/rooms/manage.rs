//! XEP-0045 room lifecycle. Network work is separate from verified join state.
use super::*;
use crate::protocol::Command;
use futures_util::{StreamExt, stream};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::sync::{mpsc, oneshot};
use zeroize::Zeroizing;

const DISCO: &str = "http://jabber.org/protocol/disco#info";
const ITEMS: &str = "http://jabber.org/protocol/disco#items";
const MUC: &str = "http://jabber.org/protocol/muc";
const OWNER: &str = "http://jabber.org/protocol/muc#owner";
const ADMIN: &str = "http://jabber.org/protocol/muc#admin";
const USER: &str = "http://jabber.org/protocol/muc#user";
const DATA: &str = "jabber:x:data";

#[derive(Clone)]
pub struct Password(pub Zeroizing<String>);
impl fmt::Debug for Password {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Password([REDACTED])")
    }
}
pub type Ticket = Arc<AtomicBool>;
#[derive(Clone)]
pub struct Form(pub Element);
impl fmt::Debug for Form {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("RoomForm([REDACTED])")
    }
}
#[derive(Clone, Debug)]
pub struct RoomInfo {
    pub title: String,
    pub mam: bool,
    pub stable_ids: bool,
    pub private: bool,
}
#[derive(Clone, Debug)]
pub struct JoinReceipt {
    pub room: String,
    pub nick: String,
    pub id: u64,
    pub created: bool,
    pub owner: bool,
}
struct Waiter {
    id: u64,
    started: Instant,
    reply: oneshot::Sender<Result<JoinReceipt>>,
}
#[derive(Default)]
pub struct Pending {
    joins: BTreeMap<String, Waiter>,
    operations: BTreeMap<String, (Ticket, Option<u64>)>,
}
impl Pending {
    pub fn begin(&mut self, room: &str) -> Result<Ticket> {
        if self.operations.contains_key(room) {
            return Err(Error::OperationBusy);
        }
        let ticket = Arc::new(AtomicBool::new(false));
        self.operations.insert(room.into(), (ticket.clone(), None));
        Ok(ticket)
    }
    pub fn valid(&self, room: &str, ticket: &Ticket) -> bool {
        !ticket.load(Ordering::Acquire)
            && self
                .operations
                .get(room)
                .is_some_and(|(current, _)| Arc::ptr_eq(current, ticket))
    }
    pub fn finished(&mut self, room: &str, ticket: &Ticket) {
        if self
            .operations
            .get(room)
            .is_some_and(|(current, _)| Arc::ptr_eq(current, ticket))
        {
            self.operations.remove(room);
        }
    }
    pub fn owned_join(&self, room: &str, ticket: &Ticket) -> Option<u64> {
        self.operations
            .get(room)
            .filter(|(current, _)| Arc::ptr_eq(current, ticket))
            .and_then(|(_, id)| *id)
    }
    pub fn contains(&self, room: &str) -> bool {
        self.joins.contains_key(room)
    }
    pub fn insert(&mut self, room: String, id: u64, reply: oneshot::Sender<Result<JoinReceipt>>) {
        if let Some((_, joined)) = self.operations.get_mut(&room) {
            *joined = Some(id)
        }
        self.joins.insert(
            room,
            Waiter {
                id,
                started: Instant::now(),
                reply,
            },
        );
    }
    pub fn changed(&mut self, rooms: &Rooms, room: &str) {
        let Some(waiter) = self.joins.get(room) else {
            return;
        };
        if rooms.join_id(room) != Some(waiter.id) {
            self.cancel(room);
        } else if let Some(receipt) = rooms.join_receipt(room) {
            if let Some(waiter) = self.joins.remove(room) {
                let _ = waiter.reply.send(Ok(receipt));
            }
        } else if !rooms.joining(room) {
            if let Some(waiter) = self.joins.remove(room) {
                let _ = waiter.reply.send(Err(Error::Forbidden));
            }
        }
    }
    pub fn cancel(&mut self, room: &str) {
        if let Some((ticket, _)) = self.operations.remove(room) {
            ticket.store(true, Ordering::Release);
        }
        if let Some(waiter) = self.joins.remove(room) {
            let _ = waiter.reply.send(Err(Error::Cancelled));
        }
    }
    pub fn disconnect(&mut self) {
        for (_, (ticket, _)) in std::mem::take(&mut self.operations) {
            ticket.store(true, Ordering::Release);
        }
        for (_, waiter) in std::mem::take(&mut self.joins) {
            let _ = waiter.reply.send(Err(Error::Cancelled));
        }
    }
    pub fn expire(&mut self, rooms: &mut Rooms) -> Vec<Element> {
        let mut cleanup = vec![];
        let expired: Vec<_> = self
            .joins
            .iter()
            .filter(|(_, job)| job.started.elapsed() >= Duration::from_secs(20))
            .map(|(room, _)| room.clone())
            .collect();
        for room in expired {
            if let Some(job) = self.joins.remove(&room) {
                if rooms.join_id(&room) == Some(job.id) {
                    if let Ok(stanza) = rooms.leave(&room) {
                        cleanup.push(stanza)
                    }
                }
                let _ = job.reply.send(Err(Error::Request(iq::Error::Timeout)));
                self.cancel(&room);
            }
        }
        cleanup
    }
}

async fn query(iq: &iq::Client, to: &str, ns: &str) -> Result<Element> {
    iq.request(to, iq::Kind::Get, el("query", ns))
        .await?
        .ok_or(Error::Invalid)
}
fn info(xml: &Element) -> Result<RoomInfo> {
    bounded(xml)?;
    if !xml.is("query", DISCO) || xml.attr("node").is_some() {
        return Err(Error::Invalid);
    }
    let identity = xml
        .children()
        .find(|c| {
            c.is("identity", DISCO)
                && c.attr("category") == Some("conference")
                && c.attr("type") == Some("text")
        })
        .ok_or(Error::Invalid)?;
    let has = |name| {
        xml.children()
            .any(|c| c.is("feature", DISCO) && c.attr("var") == Some(name))
    };
    if !has(MUC) {
        return Err(Error::Invalid);
    }
    let title = identity.attr("name").unwrap_or("");
    if title.len() > 1024 {
        return Err(Error::Invalid);
    }
    Ok(RoomInfo {
        title: title.into(),
        mam: has("urn:xmpp:mam:2"),
        stable_ids: has("urn:xmpp:sid:0"),
        private: has("muc_membersonly")
            && has("muc_nonanonymous")
            && !has("muc_open")
            && !has("muc_anonymous")
            && !has("muc_semianonymous"),
    })
}
pub async fn services(iq: &iq::Client, account: &str) -> Result<Vec<String>> {
    let own = account.parse::<Jid>().map_err(|_| Error::Invalid)?;
    let domain = own.domain().as_str();
    let items = query(iq, domain, ITEMS).await?;
    bounded(&items)?;
    if !items.is("query", ITEMS) || items.attr("node").is_some() {
        return Err(Error::Invalid);
    }
    let mut candidates = BTreeSet::new();
    for item in items.children() {
        if !item.is("item", ITEMS) {
            return Err(Error::Invalid);
        }
        if item.attr("node").is_some() {
            continue;
        }
        let Some(address) = item.attr("jid") else {
            return Err(Error::Invalid);
        };
        let address = address.parse::<Jid>().map_err(|_| Error::Invalid)?;
        if address.node().is_none() && address.resource().is_none() {
            candidates.insert(address.to_string());
        }
        if candidates.len() > 128 {
            return Err(Error::Invalid);
        }
    }
    let mut checks = stream::iter(candidates.into_iter().map(|candidate| async move {
        query(iq, &candidate, DISCO)
            .await
            .as_ref()
            .is_ok_and(|xml| info(xml).is_ok())
            .then_some(candidate)
    }))
    .buffer_unordered(8);
    let mut found = vec![];
    while let Some(value) = checks.next().await {
        if let Some(value) = value {
            found.push(value)
        }
    }
    found.sort();
    Ok(found)
}
pub async fn room_info(iq: &iq::Client, room: &str) -> Result<RoomInfo> {
    info(&query(iq, &bare(room)?, DISCO).await?)
}

fn configuration(xml: &Element, title: &str) -> Result<Element> {
    bounded(xml)?;
    if !xml.is("query", OWNER) {
        return Err(Error::Invalid);
    }
    let form = child(xml, "x", DATA)?
        .filter(|form| form.attr("type") == Some("form"))
        .ok_or(Error::Invalid)?;
    let mut values = BTreeMap::new();
    let mut options = BTreeMap::new();
    for field in form.children().filter(|child| child.is("field", DATA)) {
        if field.attr("type") == Some("fixed") {
            continue;
        }
        let var = field
            .attr("var")
            .filter(|var| !var.is_empty() && var.len() <= 256)
            .ok_or(Error::Invalid)?;
        if values.len() >= 256 || values.contains_key(var) {
            return Err(Error::Invalid);
        }
        let mut answers = vec![];
        for value in field.children().filter(|child| child.is("value", DATA)) {
            if value.children().next().is_some() {
                return Err(Error::Invalid);
            }
            answers.push(value.text());
        }
        let allowed = field
            .children()
            .filter(|child| child.is("option", DATA))
            .map(|option| {
                child(option, "value", DATA)?
                    .map(|value| value.text())
                    .ok_or(Error::Invalid)
            })
            .collect::<Result<Vec<_>>>()?;
        options.insert(var.to_string(), allowed);
        values.insert(var.to_string(), answers);
    }
    if values.get("FORM_TYPE") != Some(&vec!["http://jabber.org/protocol/muc#roomconfig".into()])
        || !values.contains_key("muc#roomconfig_membersonly")
        || !values.contains_key("muc#roomconfig_whois")
    {
        return Err(Error::Invalid);
    }
    if options
        .get("muc#roomconfig_whois")
        .is_some_and(|values| !values.is_empty() && !values.iter().any(|value| value == "anyone"))
    {
        return Err(Error::Invalid);
    }
    for (var, value) in [
        ("muc#roomconfig_membersonly", "1"),
        ("muc#roomconfig_whois", "anyone"),
        ("muc#roomconfig_persistentroom", "1"),
        ("muc#roomconfig_publicroom", "0"),
        ("muc#roomconfig_roomname", title),
    ] {
        if let Some(values) = values.get_mut(var) {
            *values = vec![value.into()];
        }
    }
    let mut submit = attr(el("x", DATA), "type", "submit");
    for (var, answers) in values {
        let mut field = attr(el("field", DATA), "var", var);
        for value in answers {
            field.append_child(Element::builder("value", DATA).append(value).build());
        }
        submit.append_child(field);
    }
    Ok(Element::builder("query", OWNER).append(submit).build())
}
async fn checked_iq(
    network: &mpsc::UnboundedSender<Command>,
    epoch: u64,
    room: &str,
    id: u64,
    ticket: &Ticket,
    kind: iq::Kind,
    payload: Element,
) -> iq::Reply {
    let (reply, answer) = oneshot::channel();
    network
        .send(Command::ManagedIq {
            epoch,
            peer: room.into(),
            id,
            ticket: ticket.clone(),
            kind,
            payload: Form(payload),
            reply,
        })
        .map_err(|_| iq::Error::Closed)?;
    answer.await.map_err(|_| iq::Error::Closed)?
}
async fn configure(
    iq: &iq::Client,
    network: &mpsc::UnboundedSender<Command>,
    epoch: u64,
    receipt: &JoinReceipt,
    ticket: &Ticket,
    title: &str,
) -> Result<RoomInfo> {
    let room = &receipt.room;
    let form = checked_iq(
        network,
        epoch,
        room,
        receipt.id,
        ticket,
        iq::Kind::Get,
        el("query", OWNER),
    )
    .await?
    .ok_or(Error::Invalid)?;
    let form = configuration(&form, title)?;
    checked_iq(
        network,
        epoch,
        room,
        receipt.id,
        ticket,
        iq::Kind::Set,
        form,
    )
    .await?;
    let info = room_info(iq, room).await?;
    if !info.private {
        return Err(Error::RecipientsUnavailable);
    }
    Ok(info)
}
async fn enter(
    network: &mpsc::UnboundedSender<Command>,
    epoch: u64,
    peer: String,
    nick: String,
    password: Option<Password>,
    history: u32,
    ticket: &Ticket,
) -> Result<JoinReceipt> {
    let (reply, answer) = oneshot::channel();
    network
        .send(Command::ManagedJoin {
            epoch,
            peer,
            nick,
            password,
            history,
            ticket: ticket.clone(),
            reply,
        })
        .map_err(|_| Error::Cancelled)?;
    tokio::time::timeout(Duration::from_secs(22), answer)
        .await
        .map_err(|_| Error::Request(iq::Error::Timeout))?
        .map_err(|_| Error::Cancelled)?
}
async fn cleanup(
    network: &mpsc::UnboundedSender<Command>,
    epoch: u64,
    receipt: &JoinReceipt,
    ticket: &Ticket,
) {
    let (reply, answer) = oneshot::channel();
    let _ = network.send(Command::ManagedLeave {
        epoch,
        peer: receipt.room.clone(),
        id: receipt.id,
        ticket: ticket.clone(),
        reply,
    });
    let _ = tokio::time::timeout(Duration::from_secs(3), answer).await;
}
pub fn creation_address(service: &str) -> Result<String> {
    let service = service.parse::<Jid>().map_err(|_| Error::Invalid)?;
    if service.node().is_some() || service.resource().is_some() {
        return Err(Error::Invalid);
    }
    Ok(format!("{}@{}", crate::model::new_id(), service))
}
pub async fn create(
    network: &mpsc::UnboundedSender<Command>,
    owner: &str,
    epoch: u64,
    room: &str,
    service: &str,
    title: &str,
    nick: &str,
    ticket: &Ticket,
) -> Result<BookmarkInfo> {
    let service = service.parse::<Jid>().map_err(|_| Error::Invalid)?;
    if service.node().is_some()
        || service.resource().is_some()
        || title.trim().is_empty()
        || title.len() > 1024
    {
        return Err(Error::Invalid);
    }
    let iq = iq::Client::new(network.clone());
    // The server must actually advertise this service; no guessed conference hostname.
    if !services(&iq, owner).await?.contains(&service.to_string()) {
        return Err(Error::Invalid);
    }
    let room = bare(room)?;
    let receipt = enter(network, epoch, room.clone(), nick.into(), None, 0, ticket).await?;
    let configured = async {
        if !receipt.created || !receipt.owner {
            return Err(Error::Forbidden);
        }
        let info = configure(&iq, network, epoch, &receipt, ticket, title.trim()).await?;
        if info.stable_ids {
            let _ = network.send(Command::RoomStableIds {
                epoch,
                peer: room.clone(),
            });
        }
        let (reply, answer) = oneshot::channel();
        network
            .send(Command::ManagedUnlock {
                epoch,
                peer: room.clone(),
                id: receipt.id,
                ticket: ticket.clone(),
                reply,
            })
            .map_err(|_| Error::Cancelled)?;
        answer.await.map_err(|_| Error::Cancelled)??;
        Ok(BookmarkInfo {
            jid: room.clone(),
            name: if info.title.is_empty() {
                title.trim().into()
            } else {
                info.title
            },
            nick: Some(receipt.nick.clone()),
            autojoin: true,
        })
    }
    .await;
    match configured {
        Ok(info) => Ok(info),
        Err(_) => {
            cleanup(network, epoch, &receipt, ticket).await;
            Err(Error::PartialCreated { room })
        }
    }
}
pub async fn join(
    network: &mpsc::UnboundedSender<Command>,
    owner: &str,
    epoch: u64,
    room: &str,
    nick: &str,
    password: Option<Password>,
    imported: Option<Password>,
    ticket: &Ticket,
) -> Result<BookmarkInfo> {
    let room = bare(room)?;
    let iq = iq::Client::new(network.clone());
    let info = room_info(&iq, &room).await?;
    let supplied = password.is_some();
    let password = match password {
        Some(value) => Some(value),
        None => match crate::credentials::load_room(owner, &room).await {
            Ok(secret) => Some(Password(secret)),
            Err(crate::credentials::Error::Missing) => imported,
            Err(_) => return Err(Error::RoomSecretUnavailable),
        },
    };
    let receipt = enter(
        network,
        epoch,
        room.clone(),
        nick.into(),
        password.clone(),
        if info.mam { 0 } else { 50 },
        ticket,
    )
    .await?;
    if receipt.created {
        cleanup(network, epoch, &receipt, ticket).await;
        return Err(Error::PartialCreated { room });
    }
    if info.stable_ids {
        let _ = network.send(Command::RoomStableIds {
            epoch,
            peer: room.clone(),
        });
    }
    if supplied || password.is_some() {
        if let Some(password) = password {
            if crate::credentials::store_room(owner, &room, password.0)
                .await
                .is_err()
            {
                return Err(Error::JoinedSecretUnsaved { room });
            }
        }
    }
    Ok(BookmarkInfo {
        jid: room,
        name: info.title,
        nick: Some(receipt.nick),
        autojoin: true,
    })
}
pub async fn invite(
    network: &mpsc::UnboundedSender<Command>,
    room: &str,
    members: Vec<String>,
    epoch: u64,
    id: u64,
    ticket: &Ticket,
) -> Result<Vec<String>> {
    bare(room)?;
    let mut unique = BTreeSet::new();
    if members.len() > 256 {
        return Err(Error::Invalid);
    }
    for member in members {
        unique.insert(bare(&member)?);
    }
    let iq = iq::Client::new(network.clone());
    let private = room_info(&iq, room).await?.private;
    let existing = if private {
        members::fetch(&iq, room).await?.affiliations
    } else {
        BTreeMap::new()
    };
    let mut failures = vec![];
    for member in unique {
        if ticket.load(Ordering::Acquire) {
            return Err(Error::Cancelled);
        }
        if private && !existing.contains_key(&member) {
            let item = attr(
                attr(el("item", ADMIN), "jid", &member),
                "affiliation",
                "member",
            );
            if checked_iq(
                network,
                epoch,
                room,
                id,
                ticket,
                iq::Kind::Set,
                Element::builder("query", ADMIN).append(item).build(),
            )
            .await
            .is_err()
            {
                failures.push(member);
                continue;
            }
        }
        let invitation = Element::builder("message", CLIENT)
            .attr("to".try_into().unwrap(), room)
            .append(
                Element::builder("x", USER)
                    .append(attr(el("invite", USER), "to", &member))
                    .build(),
            )
            .build();
        let (reply, answer) = oneshot::channel();
        if network
            .send(Command::ManagedRoomStanza {
                epoch,
                peer: room.into(),
                id,
                ticket: ticket.clone(),
                xml: invitation,
                reply,
            })
            .is_err()
            || !answer.await.unwrap_or(false)
        {
            failures.push(member);
        }
    }
    Ok(failures)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn private_form_join_ownership_and_local_choice_lifecycle() {
        let room = "group@conference.example.invalid";
        let owner = "owner@example.invalid";
        let form:Element=format!("<query xmlns='{OWNER}'><x xmlns='{DATA}' type='form'><field var='FORM_TYPE' type='hidden'><value>http://jabber.org/protocol/muc#roomconfig</value></field><field var='muc#roomconfig_membersonly'><value>0</value></field><field var='muc#roomconfig_whois'><value>moderators</value><option><value>anyone</value></option></field><field var='provider-custom'><value>keep-default</value></field></x></query>").parse().unwrap();
        let submit = configuration(&form, "A new group").unwrap();
        let submitted = submit.get_child("x", DATA).unwrap();
        let answer = |name| {
            submitted
                .children()
                .find(|field| field.attr("var") == Some(name))
                .unwrap()
                .get_child("value", DATA)
                .unwrap()
                .text()
        };
        assert_eq!(answer("muc#roomconfig_membersonly"), "1");
        assert_eq!(answer("muc#roomconfig_whois"), "anyone");
        assert_eq!(answer("provider-custom"), "keep-default");
        assert!(configuration(&el("query", OWNER), "Group").is_err());
        let mut rooms = Rooms::new(owner.into());
        let choice = BookmarkInfo {
            jid: room.into(),
            name: "Local title".into(),
            nick: Some("Local nick".into()),
            autojoin: true,
        };
        rooms.update_choices(vec![LocalBookmark {
            info: choice.clone(),
            revision: 1,
            dirty: true,
        }]);
        let server = BookmarkInfo {
            jid: room.into(),
            name: "Server title".into(),
            nick: Some("Server nick".into()),
            autojoin: false,
        };
        let imported =
            Bookmark::new(server.clone(), Some(Zeroizing::new("server-secret".into()))).unwrap();
        assert_eq!(rooms.import(vec![imported])[0], choice);
        assert_eq!(
            rooms.imported_password(room).unwrap().0.as_str(),
            "server-secret"
        );
        rooms.update_choices(vec![LocalBookmark {
            info: choice,
            revision: 2,
            dirty: false,
        }]);
        assert!(rooms.autojoin().is_empty());
        assert!(
            rooms.import(vec![]).is_empty(),
            "absent clean server bookmarks never reappear as autojoin"
        );
        let mut jobs = Pending::default();
        let ticket = jobs.begin(room).unwrap();
        assert_eq!(jobs.owned_join(room, &ticket), None);
        let join = rooms.join_with(room, Some("Own"), Some(""), 0).unwrap();
        let join_x = join.get_child("x", MUC).unwrap();
        assert_eq!(
            join_x.get_child("history", MUC).unwrap().attr("maxstanzas"),
            Some("0")
        );
        assert_eq!(join_x.get_child("password", MUC).unwrap().text(), "");
        let id = rooms.join_id(room).unwrap();
        let (reply, mut received) = oneshot::channel();
        jobs.insert(room.into(), id, reply);
        let self_presence:Element=format!("<presence xmlns='jabber:client' from='{room}/Own'><x xmlns='{USER}'><item jid='{owner}' affiliation='owner' role='moderator'/><status code='110'/><status code='201'/></x></presence>").parse().unwrap();
        rooms.presence(&self_presence);
        jobs.changed(&rooms, room);
        let receipt = received.try_recv().unwrap().unwrap();
        assert!(receipt.created && receipt.owner);
        assert!(
            !rooms.status(room).can_send,
            "201 stays locked until owner configuration is verified"
        );
        assert!(rooms.configured(room, id + 1).is_err());
        rooms.configured(room, id).unwrap();
        assert!(rooms.status(room).can_send);
        assert_eq!(jobs.owned_join(room, &ticket), Some(id));
        jobs.cancel(room);
        assert!(!jobs.valid(room, &ticket));
        let leave = rooms.leave(room).unwrap();
        assert_eq!(leave.attr("type"), Some("unavailable"));
        rooms.join_with(room, Some("Own"), None, 50).unwrap();
        assert_ne!(rooms.join_id(room), Some(id));
        assert!(rooms.configured(room, id).is_err());
        let password = Password(Zeroizing::new("do-not-log-this".into()));
        let (reply, _) = oneshot::channel();
        let command = Command::JoinExistingRoom {
            peer: room.into(),
            nick: "Own".into(),
            password: Some(password),
            reply,
        };
        assert!(!format!("{command:?}").contains("do-not-log-this"));
    }
}
