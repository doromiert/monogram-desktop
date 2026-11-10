//! Read XEP-0402 before either historical XEP-0048 transport.
use super::*;
use std::collections::BTreeSet;
use zeroize::Zeroizing;

const NS: &str = "urn:xmpp:bookmarks:1";
const LEGACY: &str = "storage:bookmarks";
const PUBSUB: &str = "http://jabber.org/protocol/pubsub";
const EVENT: &str = "http://jabber.org/protocol/pubsub#event";
const DISCO: &str = "http://jabber.org/protocol/disco#info";
const PRIVATE: &str = "jabber:iq:private";
const MAX_BOOKMARKS: usize = 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BookmarkInfo {
    pub jid: String,
    pub name: String,
    pub nick: Option<String>,
    pub autojoin: bool,
}
pub struct Bookmark {
    info: BookmarkInfo,
    password: Option<Zeroizing<String>>,
}
impl fmt::Debug for Bookmark {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.info.fmt(f)
    }
}
impl Bookmark {
    pub fn info(&self) -> &BookmarkInfo {
        &self.info
    }
    pub(super) fn password(&self) -> Option<&str> {
        self.password.as_ref().map(|secret| secret.as_str())
    }
    fn parse(jid: &str, conference: &Element, ns: &str) -> Result<Self> {
        let jid = bare(jid)?;
        let name = conference.attr("name").unwrap_or("");
        if name.len() > 1024 {
            return Err(Error::Invalid);
        }
        let autojoin = match conference.attr("autojoin") {
            None | Some("0" | "false") => false,
            Some("1" | "true") => true,
            _ => return Err(Error::Invalid),
        };
        let nick = simple_text(conference, "nick", ns)?
            .map(|nick| {
                super::muc::occupant(&jid, &nick)
                    .map(|address| address.resource().unwrap().as_str().to_owned())
            })
            .transpose()?;
        let password = simple_text(conference, "password", ns)?.map(Zeroizing::new);
        Ok(Self {
            info: BookmarkInfo {
                jid,
                name: name.into(),
                nick,
                autojoin,
            },
            password,
        })
    }
}
fn simple_text(xml: &Element, name: &str, ns: &str) -> Result<Option<String>> {
    let Some(value) = child(xml, name, ns)? else {
        return Ok(None);
    };
    if value.children().next().is_some() {
        return Err(Error::Invalid);
    }
    let text = value.text();
    if text.len() > 1023 {
        return Err(Error::Invalid);
    }
    Ok((!text.is_empty()).then_some(text))
}
fn unique(entries: Vec<Bookmark>) -> Result<Vec<Bookmark>> {
    let mut jids = BTreeSet::new();
    if entries.len() > MAX_BOOKMARKS || entries.iter().any(|item| !jids.insert(&item.info.jid)) {
        return Err(Error::Invalid);
    }
    Ok(entries)
}
fn items<'a>(response: &'a Element, node: &str) -> Result<impl Iterator<Item = &'a Element>> {
    if !response.is("pubsub", PUBSUB) {
        return Err(Error::Invalid);
    }
    bounded(response)?;
    let items = child(response, "items", PUBSUB)?.ok_or(Error::Invalid)?;
    if items.attr("node") != Some(node) {
        return Err(Error::Invalid);
    }
    Ok(items.children().filter(|item| item.is("item", PUBSUB)))
}
fn modern(response: &Element) -> Result<Vec<Bookmark>> {
    let mut found = vec![];
    for item in items(response, NS)? {
        if found.len() >= MAX_BOOKMARKS {
            return Err(Error::Invalid);
        }
        let jid = item.attr("id").ok_or(Error::Invalid)?;
        let payload = child(item, "conference", NS)?.ok_or(Error::Invalid)?;
        found.push(Bookmark::parse(jid, payload, NS)?);
    }
    unique(found)
}
fn legacy(storage: &Element) -> Result<Vec<Bookmark>> {
    if !storage.is("storage", LEGACY) {
        return Err(Error::Invalid);
    }
    bounded(storage)?;
    let mut found = vec![];
    for value in storage.children().filter(|c| c.is("conference", LEGACY)) {
        if found.len() >= MAX_BOOKMARKS {
            return Err(Error::Invalid);
        }
        found.push(Bookmark::parse(
            value.attr("jid").ok_or(Error::Invalid)?,
            value,
            LEGACY,
        )?);
    }
    unique(found)
}
fn unavailable(error: &iq::Error, legacy: bool) -> bool {
    matches!(error, iq::Error::Rejected { condition, pubsub: None }
        if matches!(condition.as_str(), "item-not-found" | "feature-not-implemented")
            || legacy && condition == "service-unavailable")
}
async fn get(
    iq: &iq::Client,
    owner: &str,
    payload: Element,
    legacy: bool,
) -> Result<Option<Element>> {
    match iq.request(owner, iq::Kind::Get, payload).await {
        Ok(Some(xml)) => Ok(Some(xml)),
        Ok(None) => Err(Error::Invalid),
        Err(error) if unavailable(&error, legacy) => Ok(None),
        Err(error) => Err(error.into()),
    }
}
fn get_items(node: &str) -> Element {
    let mut items = attr(el("items", PUBSUB), "node", node);
    if node == LEGACY {
        items = attr(items, "max_items", "1");
    }
    Element::builder("pubsub", PUBSUB).append(items).build()
}
/// Never switch bookmark stores after an access denial or a transport failure.
pub async fn load_bookmarks(iq: &iq::Client, owner: &str) -> Result<Vec<Bookmark>> {
    bare(owner)?;
    if let Some(response) = get(iq, owner, get_items(NS), false).await? {
        let bookmarks = modern(&response)?;
        if !bookmarks.is_empty() {
            return Ok(bookmarks);
        }
    }
    if let Some(disco) = get(iq, owner, el("query", DISCO), false).await? {
        if !disco.is("query", DISCO) {
            return Err(Error::Invalid);
        }
        bounded(&disco)?;
        if disco.children().any(|feature| {
            feature.is("feature", DISCO)
                && matches!(
                    feature.attr("var"),
                    Some("urn:xmpp:bookmarks:1#compat" | "urn:xmpp:bookmarks:1#compat-pep")
                )
        }) {
            // A unified store's empty current set is authoritative, even if an old list remains.
            return Ok(vec![]);
        }
    }
    if let Some(response) = get(iq, owner, get_items(LEGACY), true).await? {
        let mut found = items(&response, LEGACY)?;
        if let Some(item) = found.next() {
            if found.next().is_some() {
                return Err(Error::Invalid);
            }
            let storage = child(item, "storage", LEGACY)?.ok_or(Error::Invalid)?;
            let bookmarks = legacy(storage)?;
            if !bookmarks.is_empty() {
                return Ok(bookmarks);
            }
        }
    }
    let query = Element::builder("query", PRIVATE)
        .append(el("storage", LEGACY))
        .build();
    let Some(response) = get(iq, owner, query, true).await? else {
        return Ok(vec![]);
    };
    if !response.is("query", PRIVATE) {
        return Err(Error::Invalid);
    }
    legacy(child(&response, "storage", LEGACY)?.ok_or(Error::Invalid)?)
}
/// Best effort: servers that reject self-subscriptions can still deliver PEP +notify.
pub async fn subscribe_bookmarks(iq: &iq::Client, owner: &str) {
    let subscribe = attr(attr(el("subscribe", PUBSUB), "node", NS), "jid", owner);
    let _ = iq
        .request(
            owner,
            iq::Kind::Set,
            Element::builder("pubsub", PUBSUB).append(subscribe).build(),
        )
        .await;
}
pub fn bookmark_changed(xml: &Element, owner: &str) -> bool {
    if !xml.is("message", CLIENT) || xml.attr("from") != Some(owner) || !addressed_to(xml, owner) {
        return false;
    }
    child(xml, "event", EVENT)
        .ok()
        .flatten()
        .is_some_and(|event| {
            event.children().any(|value| {
                value.ns() == EVENT
                    && matches!(value.name(), "items" | "purge" | "delete")
                    && matches!(value.attr("node"), Some(NS | LEGACY))
            })
        })
}
