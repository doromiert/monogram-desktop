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
const OWNER: &str = "http://jabber.org/protocol/pubsub#owner";
const FORM: &str = "jabber:x:data";
const MAX_BOOKMARKS: usize = 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BookmarkInfo {
    pub jid: String,
    pub name: String,
    pub nick: Option<String>,
    pub autojoin: bool,
}
#[derive(Clone)]
pub struct Bookmark {
    info: BookmarkInfo,
    password: Option<Zeroizing<String>>,
    metadata: Element,
}
impl fmt::Debug for Bookmark {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.info.fmt(f)
    }
}
impl Bookmark {
    /// When editing a server item, None preserves its password; Some(empty) clears it.
    pub fn new(mut info: BookmarkInfo, password: Option<Zeroizing<String>>) -> Result<Self> {
        info.jid = bare(&info.jid)?;
        if info.name.len() > 1024 || password.as_ref().is_some_and(|value| value.len() > 1023) {
            return Err(Error::Invalid);
        }
        info.nick = info
            .nick
            .filter(|nick| !nick.is_empty())
            .map(|nick| {
                super::muc::occupant(&info.jid, &nick)
                    .map(|address| address.resource().unwrap().as_str().to_owned())
            })
            .transpose()?;
        Ok(Self {
            info,
            password,
            metadata: el("conference", NS),
        })
    }
    pub fn info(&self) -> &BookmarkInfo {
        &self.info
    }
    pub(super) fn password(&self) -> Option<&str> {
        self.password.as_ref().map(|secret| secret.as_str())
    }
    fn preserve_server_metadata(&mut self, previous: &Self) {
        self.metadata = previous.metadata.clone();
        if self.password.is_none() {
            self.password = previous.password.clone();
        }
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
        let mut entry = Self::new(
            BookmarkInfo {
                jid,
                name: name.into(),
                nick,
                autojoin,
            },
            password,
        )?;
        entry.metadata = conference.clone();
        entry.metadata.remove_child("nick", ns);
        entry.metadata.remove_child("password", ns);
        Ok(entry)
    }
    fn conference(&self) -> Element {
        let mut conference = el("conference", NS);
        for ((namespace, name), value) in self.metadata.attrs().iter() {
            if namespace.as_str().is_empty() && matches!(name.as_str(), "name" | "autojoin" | "jid")
            {
                continue;
            }
            conference.set_attr(namespace.clone(), name.clone(), value.clone());
        }
        if !self.info.name.is_empty() {
            conference = attr(conference, "name", &self.info.name);
        }
        conference = attr(
            conference,
            "autojoin",
            if self.info.autojoin { "true" } else { "false" },
        );
        for (name, value) in [
            ("nick", self.info.nick.as_deref()),
            (
                "password",
                self.password().filter(|value| !value.is_empty()),
            ),
        ] {
            if let Some(value) = value {
                conference.append_child(Element::builder(name, NS).append(value).build());
            }
        }
        for extra in self.metadata.children() {
            conference.append_child(extra.clone());
        }
        conference
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
        if unified(&disco) {
            // A unified store's empty current set is authoritative, even if an old list remains.
            return Ok(vec![]);
        }
    }
    legacy_bookmarks(iq, owner).await
}
async fn legacy_bookmarks(iq: &iq::Client, owner: &str) -> Result<Vec<Bookmark>> {
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
fn supports(disco: &Element, feature: &str) -> bool {
    disco
        .children()
        .any(|child| child.is("feature", DISCO) && child.attr("var") == Some(feature))
}
fn unified(disco: &Element) -> bool {
    supports(disco, "urn:xmpp:bookmarks:1#compat")
        || supports(disco, "urn:xmpp:bookmarks:1#compat-pep")
}
fn private_form(kind: &str) -> Element {
    let mut form = attr(el("x", FORM), "type", "submit");
    for (name, value) in [
        ("FORM_TYPE", kind),
        ("pubsub#persist_items", "true"),
        ("pubsub#max_items", "max"),
        ("pubsub#send_last_published_item", "never"),
        ("pubsub#access_model", "whitelist"),
    ] {
        let mut field = attr(el("field", FORM), "var", name);
        if name == "FORM_TYPE" {
            field = attr(field, "type", "hidden");
        }
        field.append_child(Element::builder("value", FORM).append(value).build());
        form.append_child(field);
    }
    form
}
fn publish(entry: &Bookmark) -> Result<Element> {
    let mut publication = attr(el("publish", PUBSUB), "node", NS);
    let mut item = attr(el("item", PUBSUB), "id", &entry.info.jid);
    item.append_child(entry.conference());
    publication.append_child(item);
    let mut options = el("publish-options", PUBSUB);
    options.append_child(private_form(
        "http://jabber.org/protocol/pubsub#publish-options",
    ));
    let request = Element::builder("pubsub", PUBSUB)
        .append(publication)
        .append(options)
        .build();
    bounded(&request)?;
    Ok(request)
}
/// Legacy stores remain read-only, matching mobile; private options are mandatory.
pub async fn save_bookmark(iq: &iq::Client, owner: &str, entry: &Bookmark) -> Result<()> {
    bare(owner)?;
    let disco = get(iq, owner, el("query", DISCO), false)
        .await?
        .ok_or(Error::BookmarkPrivacyUnavailable)?;
    if !disco.is("query", DISCO) || disco.attr("node").is_some() {
        return Err(Error::Invalid);
    }
    bounded(&disco)?;
    if !supports(&disco, "http://jabber.org/protocol/pubsub#publish-options") {
        return Err(Error::BookmarkPrivacyUnavailable);
    }
    let current = get(iq, owner, get_items(NS), false)
        .await?
        .map(|xml| modern(&xml))
        .transpose()?
        .unwrap_or_default();
    if current.is_empty() && !unified(&disco) && !legacy_bookmarks(iq, owner).await?.is_empty() {
        return Err(Error::BookmarkPrivacyUnavailable);
    }
    let mut edited = entry.clone();
    if let Some(previous) = current
        .iter()
        .find(|value| value.info.jid == entry.info.jid)
    {
        edited.preserve_server_metadata(previous);
    }
    let request = publish(&edited)?;
    match iq.request(owner, iq::Kind::Set, request.clone()).await {
        Ok(_) => Ok(()),
        Err(iq::Error::Rejected { condition, pubsub })
            if condition == "conflict"
                && pubsub
                    .as_deref()
                    .is_none_or(|reason| reason == "precondition-not-met") =>
        {
            let mut configure = attr(el("configure", OWNER), "node", NS);
            configure.append_child(private_form(
                "http://jabber.org/protocol/pubsub#node_config",
            ));
            iq.request(
                owner,
                iq::Kind::Set,
                Element::builder("pubsub", OWNER).append(configure).build(),
            )
            .await?;
            iq.request(owner, iq::Kind::Set, request).await?;
            Ok(())
        }
        Err(error) => Err(error.into()),
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn per_item_publication_is_private_and_preserves_unknown_metadata() {
        let existing: Element = "<conference xmlns='urn:xmpp:bookmarks:1' name='Old name' custom='keep' autojoin='true'><nick>Old nick</nick><password>old-password</password><extensions><state xmlns='urn:test:bookmark' minimized='true'/></extensions><other xmlns='urn:test:unknown'/></conference>".parse().unwrap();
        let previous = Bookmark::parse("room@conference.example.org", &existing, NS).unwrap();
        let mut edited = Bookmark::new(
            BookmarkInfo {
                jid: previous.info.jid.clone(),
                name: "New & name".into(),
                nick: Some("New nick".into()),
                autojoin: false,
            },
            Some(Zeroizing::new("new-password".into())),
        )
        .unwrap();
        edited.preserve_server_metadata(&previous);
        let request = publish(&edited).unwrap();
        let publication = child(&request, "publish", PUBSUB).unwrap().unwrap();
        assert_eq!(publication.attr("node"), Some(NS));
        assert_eq!(
            publication.children().count(),
            1,
            "saving one room cannot replace other items"
        );
        let item = child(publication, "item", PUBSUB).unwrap().unwrap();
        assert_eq!(item.attr("id"), Some("room@conference.example.org"));
        let conference = child(item, "conference", NS).unwrap().unwrap();
        assert_eq!(conference.attr("name"), Some("New & name"));
        assert_eq!(conference.attr("autojoin"), Some("false"));
        assert_eq!(conference.attr("custom"), Some("keep"));
        assert_eq!(
            simple_text(conference, "nick", NS).unwrap().as_deref(),
            Some("New nick")
        );
        assert_eq!(
            simple_text(conference, "password", NS).unwrap().as_deref(),
            Some("new-password")
        );
        assert_eq!(
            conference.get_child("extensions", NS),
            existing.get_child("extensions", NS)
        );
        assert!(conference.get_child("other", "urn:test:unknown").is_some());
        let mut public_edit = Bookmark::new(edited.info.clone(), None).unwrap();
        public_edit.preserve_server_metadata(&previous);
        let public_conference = public_edit.conference();
        assert_eq!(
            public_edit.password(),
            previous.password(),
            "omitting a password during nickname/autojoin edits must retain the fresh server secret"
        );
        assert_eq!(public_conference.attr("autojoin"), Some("false"));
        assert_eq!(
            simple_text(&public_conference, "nick", NS)
                .unwrap()
                .as_deref(),
            Some("New nick")
        );
        assert_eq!(
            simple_text(&public_conference, "password", NS)
                .unwrap()
                .as_deref(),
            previous.password()
        );
        assert_eq!(public_conference.attr("custom"), Some("keep"));
        assert_eq!(
            public_conference.get_child("extensions", NS),
            existing.get_child("extensions", NS)
        );
        assert!(public_edit.metadata.get_child("password", NS).is_none());
        let mut clear =
            Bookmark::new(edited.info.clone(), Some(Zeroizing::new(String::new()))).unwrap();
        clear.preserve_server_metadata(&previous);
        assert!(clear.password().is_some_and(str::is_empty));
        assert!(
            clear.conference().get_child("password", NS).is_none(),
            "explicit empty remains a clear operation"
        );
        let options = child(&request, "publish-options", PUBSUB).unwrap().unwrap();
        let form = child(options, "x", FORM).unwrap().unwrap();
        assert_eq!(form.attr("type"), Some("submit"));
        let fields: std::collections::BTreeMap<_, _> = form
            .children()
            .map(|field| {
                (
                    field.attr("var").unwrap(),
                    child(field, "value", FORM).unwrap().unwrap().text(),
                )
            })
            .collect();
        assert_eq!(fields.len(), 5);
        for (name, value) in [
            (
                "FORM_TYPE",
                "http://jabber.org/protocol/pubsub#publish-options",
            ),
            ("pubsub#persist_items", "true"),
            ("pubsub#max_items", "max"),
            ("pubsub#send_last_published_item", "never"),
            ("pubsub#access_model", "whitelist"),
        ] {
            assert_eq!(fields[name], value);
        }
        assert!(!format!("{edited:?}").contains("new-password"));
        assert!(
            Bookmark::new(
                BookmarkInfo {
                    jid: "conference.example.org".into(),
                    name: String::new(),
                    nick: None,
                    autojoin: false
                },
                None
            )
            .is_err()
        );
        assert!(
            Bookmark::new(
                BookmarkInfo {
                    jid: "room@conference.example.org/resource".into(),
                    name: String::new(),
                    nick: None,
                    autojoin: false
                },
                None
            )
            .is_err()
        );
    }
}
