//! XEP-0501 Atom stories, matching mobile XmppStories (GPL-3.0-or-later).
//! Media is HTTPS and not OMEMO encrypted; publication requires presence access.
use crate::iq;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
};
use tokio_xmpp::{jid::Jid, minidom::Element};

pub const NODE: &str = "urn:xmpp:pubsub-social-feed:stories:0";
pub const SOCIAL: &str = "urn:xmpp:pubsub-social-feed:1";
pub const ATOM: &str = "http://www.w3.org/2005/Atom";
pub const LIFETIME: i64 = 86_400_000;
const PUBSUB: &str = "http://jabber.org/protocol/pubsub";
const OWNER: &str = "http://jabber.org/protocol/pubsub#owner";
const EVENT: &str = "http://jabber.org/protocol/pubsub#event";
const FORM: &str = "jabber:x:data";
const MAX_ITEMS: usize = 100;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub id: String,
    pub owner: String,
    pub url: String,
    pub mime: String,
    pub caption: String,
    pub published: i64,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    Invalid,
    Privacy,
    Expired,
    NotOwn,
    Request(iq::Error),
}
impl From<iq::Error> for Error {
    fn from(value: iq::Error) -> Self {
        Self::Request(value)
    }
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Invalid => "The story data is invalid or its media type is unsupported. Choose a JPEG, PNG, WebP, GIF, MP4 or WebM file.",
            Self::Privacy => "The server could not confirm contact-only story access. Check that your XMPP server supports presence-access PEP nodes.",
            Self::Expired => "This story expired. Publish a new story instead.",
            Self::NotOwn => "Only stories belonging to this account can be changed.",
            Self::Request(iq::Error::Closed) => "Reconnect to load or publish stories.",
            Self::Request(iq::Error::Timeout) => "The story server did not answer. Check your connection and try again.",
            Self::Request(_) => "The story server rejected the request. Check its PEP support and try again.",
        })
    }
}
impl std::error::Error for Error {}
pub type Result<T> = std::result::Result<T, Error>;
fn el(name: &str, namespace: &str) -> Element {
    Element::builder(name, namespace).build()
}
fn attr(mut xml: Element, key: &str, value: impl Into<String>) -> Element {
    xml.set_attr("".into(), key.try_into().unwrap(), value.into());
    xml
}
fn only<'a>(xml: &'a Element, name: &str, namespace: &str) -> Result<Option<&'a Element>> {
    let mut found = xml.children().filter(|c| c.is(name, namespace));
    let first = found.next();
    if found.next().is_some() {
        return Err(Error::Invalid);
    }
    Ok(first)
}
fn text(xml: &Element, name: &str, namespace: &str) -> Result<Option<String>> {
    only(xml, name, namespace)?
        .map(|c| {
            if c.children().next().is_none() {
                Ok(c.text())
            } else {
                Err(Error::Invalid)
            }
        })
        .transpose()
}
fn bare(value: &str) -> Result<()> {
    let jid = value.parse::<Jid>().map_err(|_| Error::Invalid)?;
    if jid.node().is_none() || jid.resource().is_some() || jid.to_string() != value {
        return Err(Error::Invalid);
    }
    Ok(())
}
fn bounded(xml: &Element, size: usize) -> Result<()> {
    crate::crypto::bundle::xml(xml, size)
        .map(|_| ())
        .map_err(|_| Error::Invalid)
}
impl Entry {
    pub fn validate(&self, now: i64) -> Result<()> {
        bare(&self.owner)?;
        let url = reqwest::Url::parse(&self.url).map_err(|_| Error::Invalid)?;
        if self.id.is_empty()
            || self.id.len() > 1024
            || self.id.chars().any(char::is_control)
            || self.url.len() > 8192
            || url.scheme() != "https"
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.fragment().is_some()
            || !matches!(
                self.mime.as_str(),
                "image/jpeg"
                    | "image/png"
                    | "image/webp"
                    | "image/gif"
                    | "video/mp4"
                    | "video/webm"
            )
            || self.caption.encode_utf16().count() > 32768
            || self.published <= 0
            || self.published > now.saturating_add(300_000)
        {
            return Err(Error::Invalid);
        }
        Ok(())
    }
    pub fn expired(&self, now: i64) -> bool {
        now.saturating_sub(self.published) >= LIFETIME
    }
    pub fn atom(&self) -> Result<Element> {
        let stamp = chrono::DateTime::from_timestamp_millis(self.published)
            .ok_or(Error::Invalid)?
            .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        let node = |name: &str, value: &str| Element::builder(name, ATOM).append(value).build();
        let xml = Element::builder("entry", ATOM)
            .append(node("id", &format!("urn:uuid:{}", self.id)))
            .append(node(
                "title",
                if self.caption.is_empty() {
                    "Story"
                } else {
                    &self.caption
                },
            ))
            .append(node("published", &stamp))
            .append(node("updated", &stamp))
            .append(
                Element::builder("author", ATOM)
                    .append(node("uri", &format!("xmpp:{}", self.owner)))
                    .build(),
            )
            .append(attr(
                attr(
                    attr(
                        attr(el("link", ATOM), "rel", "enclosure"),
                        "href",
                        &self.url,
                    ),
                    "type",
                    &self.mime,
                ),
                "title",
                &self.caption,
            ))
            .build();
        bounded(&xml, 65536)?;
        Ok(xml)
    }
}
pub fn parse(owner: &str, id: &str, xml: &Element, now: i64) -> Result<Entry> {
    bounded(xml, 65536)?;
    if !xml.is("entry", ATOM) {
        return Err(Error::Invalid);
    }
    if let Some(author) = only(xml, "author", ATOM)? {
        if let Some(uri) = text(author, "uri", ATOM)? {
            let address = percent_encoding::percent_decode_str(
                uri.strip_prefix("xmpp:").ok_or(Error::Invalid)?,
            )
            .decode_utf8()
            .map_err(|_| Error::Invalid)?;
            bare(&address)?;
            if address != owner {
                return Err(Error::Invalid);
            }
        }
    }
    let mut links = xml
        .children()
        .filter(|c| c.is("link", ATOM) && c.attr("rel") == Some("enclosure"));
    let link = links.next().ok_or(Error::Invalid)?;
    if links.next().is_some() {
        return Err(Error::Invalid);
    }
    let stamp = text(xml, "published", ATOM)?
        .or(text(xml, "updated", ATOM)?)
        .ok_or(Error::Invalid)?;
    let entry = Entry {
        id: id.into(),
        owner: owner.into(),
        url: link.attr("href").ok_or(Error::Invalid)?.into(),
        mime: link.attr("type").ok_or(Error::Invalid)?.into(),
        caption: text(xml, "title", ATOM)?.unwrap_or_default(),
        published: chrono::DateTime::parse_from_rfc3339(&stamp)
            .map_err(|_| Error::Invalid)?
            .timestamp_millis(),
    };
    entry.validate(now)?;
    if entry.expired(now) {
        return Err(Error::Expired);
    }
    Ok(entry)
}
pub async fn fetch(iq: &iq::Client, owner: &str) -> Result<Vec<Entry>> {
    bare(owner)?;
    let request = Element::builder("pubsub", PUBSUB)
        .append(attr(
            attr(el("items", PUBSUB), "node", NODE),
            "max_items",
            "100",
        ))
        .build();
    let response = match iq.request(owner, iq::Kind::Get, request).await {
        Err(iq::Error::Rejected { condition, .. })
            if matches!(condition.as_str(), "item-not-found" | "forbidden") =>
        {
            return Ok(vec![]);
        }
        value => value?.ok_or(Error::Invalid)?,
    };
    parse_items(owner, &response, chrono::Utc::now().timestamp_millis())
}
fn parse_items(owner: &str, response: &Element, now: i64) -> Result<Vec<Entry>> {
    bounded(response, 7 * 1024 * 1024)?;
    if !response.is("pubsub", PUBSUB) {
        return Err(Error::Invalid);
    }
    let items = only(response, "items", PUBSUB)?.ok_or(Error::Invalid)?;
    if items.attr("node") != Some(NODE) || items.children().count() > MAX_ITEMS {
        return Err(Error::Invalid);
    }
    let mut ids = BTreeSet::new();
    let mut entries = vec![];
    for item in items.children() {
        if !item.is("item", PUBSUB) {
            return Err(Error::Invalid);
        }
        let id = item.attr("id").ok_or(Error::Invalid)?;
        if !ids.insert(id) {
            return Err(Error::Invalid);
        }
        if let Some(entry) = only(item, "entry", ATOM)? {
            // Malformed/expired remote items never become usable media.
            if let Ok(entry) = parse(owner, id, entry, now) {
                entries.push(entry);
            }
        }
    }
    entries.sort_by(|a, b| (a.published, &a.id).cmp(&(b.published, &b.id)));
    Ok(entries)
}
fn configuration(response: &Element) -> Result<Element> {
    bounded(response, 65536)?;
    if !response.is("pubsub", OWNER) {
        return Err(Error::Invalid);
    }
    let config = only(response, "configure", OWNER)?.ok_or(Error::Invalid)?;
    if config.attr("node") != Some(NODE) {
        return Err(Error::Invalid);
    }
    let form = only(config, "x", FORM)?.ok_or(Error::Privacy)?;
    if form.attr("type") != Some("form") {
        return Err(Error::Privacy);
    }
    Ok(form.clone())
}
fn fields(form: &Element) -> Result<BTreeMap<String, Element>> {
    let mut fields = BTreeMap::new();
    for field in form.children().filter(|f| f.is("field", FORM)) {
        if field.attr("type") == Some("fixed") {
            continue;
        }
        let name = field
            .attr("var")
            .filter(|n| !n.is_empty() && n.len() <= 256)
            .ok_or(Error::Invalid)?;
        if fields.insert(name.into(), field.clone()).is_some() {
            return Err(Error::Invalid);
        }
    }
    Ok(fields)
}
fn presence_form(form: Option<&Element>) -> Result<Element> {
    let mut fields = if let Some(form) = form {
        fields(form)?
    } else {
        BTreeMap::new()
    };
    for (name, value) in [
        ("FORM_TYPE", "http://jabber.org/protocol/pubsub#node_config"),
        ("pubsub#access_model", "presence"),
        ("pubsub#persist_items", "true"),
        ("pubsub#max_items", "100"),
        ("pubsub#type", NODE),
    ] {
        let mut field = attr(el("field", FORM), "var", name);
        if name == "FORM_TYPE" {
            field = attr(field, "type", "hidden");
        }
        field.append_child(Element::builder("value", FORM).append(value).build());
        fields.insert(name.into(), field);
    }
    if fields.contains_key("pubsub#item_expire") {
        fields.insert(
            "pubsub#item_expire".into(),
            attr(el("field", FORM), "var", "pubsub#item_expire"),
        );
        fields
            .get_mut("pubsub#item_expire")
            .unwrap()
            .append_child(Element::builder("value", FORM).append("86400").build());
    }
    Ok(Element::builder("x", FORM)
        .attr("type".try_into().unwrap(), "submit")
        .append_all(fields.into_values())
        .build())
}
async fn get_configuration(iq: &iq::Client, owner: &str) -> Result<Element> {
    let payload = Element::builder("pubsub", OWNER)
        .append(attr(el("configure", OWNER), "node", NODE))
        .build();
    configuration(
        &iq.request(owner, iq::Kind::Get, payload)
            .await?
            .ok_or(Error::Invalid)?,
    )
}
fn verify_presence(form: &Element) -> Result<()> {
    let fields = fields(form)?;
    let value = |name| {
        fields
            .get(name)
            .and_then(|f| text(f, "value", FORM).ok().flatten())
    };
    if value("pubsub#access_model").as_deref() != Some("presence")
        || !matches!(value("pubsub#persist_items").as_deref(), Some("true" | "1"))
        || value("pubsub#type").as_deref() != Some(NODE)
    {
        return Err(Error::Privacy);
    }
    Ok(())
}
pub async fn publish(iq: &iq::Client, owner: &str, entry: Entry) -> Result<Entry> {
    let now = chrono::Utc::now().timestamp_millis();
    entry.validate(now)?;
    if entry.owner != owner {
        return Err(Error::NotOwn);
    }
    if entry.expired(now) {
        return Err(Error::Expired);
    }
    let current = match get_configuration(iq, owner).await {
        Ok(form) => form,
        Err(Error::Request(iq::Error::Rejected { condition, .. }))
            if condition == "item-not-found" =>
        {
            let create = Element::builder("pubsub", PUBSUB)
                .append(attr(el("create", PUBSUB), "node", NODE))
                .append(
                    Element::builder("configure", PUBSUB)
                        .append(presence_form(None)?)
                        .build(),
                )
                .build();
            match iq.request(owner, iq::Kind::Set, create).await {
                Ok(_) => {}
                Err(iq::Error::Rejected { condition, .. }) if condition == "conflict" => {}
                Err(e) => return Err(e.into()),
            }
            get_configuration(iq, owner).await?
        }
        Err(e) => return Err(e),
    };
    let config = Element::builder("pubsub", OWNER)
        .append(
            Element::builder("configure", OWNER)
                .attr("node".try_into().unwrap(), NODE)
                .append(presence_form(Some(&current))?)
                .build(),
        )
        .build();
    iq.request(owner, iq::Kind::Set, config).await?;
    verify_presence(&get_configuration(iq, owner).await?)?;
    // The atomic publish precondition also protects against a concurrent access change.
    let option = Element::builder("x", FORM)
        .attr("type".try_into().unwrap(), "submit")
        .append(
            Element::builder("field", FORM)
                .attr("var".try_into().unwrap(), "FORM_TYPE")
                .attr("type".try_into().unwrap(), "hidden")
                .append(
                    Element::builder("value", FORM)
                        .append("http://jabber.org/protocol/pubsub#publish-options")
                        .build(),
                )
                .build(),
        )
        .append(
            Element::builder("field", FORM)
                .attr("var".try_into().unwrap(), "pubsub#access_model")
                .append(Element::builder("value", FORM).append("presence").build())
                .build(),
        )
        .build();
    let payload = Element::builder("pubsub", PUBSUB)
        .append(
            Element::builder("publish", PUBSUB)
                .attr("node".try_into().unwrap(), NODE)
                .append(
                    Element::builder("item", PUBSUB)
                        .attr("id".try_into().unwrap(), &entry.id)
                        .append(entry.atom()?)
                        .build(),
                )
                .build(),
        )
        .append(
            Element::builder("publish-options", PUBSUB)
                .append(option)
                .build(),
        )
        .build();
    iq.request(owner, iq::Kind::Set, payload).await?;
    Ok(entry)
}
pub async fn retract(iq: &iq::Client, owner: &str, id: &str) -> Result<()> {
    bare(owner)?;
    if id.is_empty() || id.len() > 1024 {
        return Err(Error::Invalid);
    }
    let payload = Element::builder("pubsub", PUBSUB)
        .append(
            Element::builder("retract", PUBSUB)
                .attr("node".try_into().unwrap(), NODE)
                .attr("notify".try_into().unwrap(), "true")
                .append(attr(el("item", PUBSUB), "id", id))
                .build(),
        )
        .build();
    match iq.request(owner, iq::Kind::Set, payload).await {
        Ok(_) => Ok(()),
        Err(iq::Error::Rejected { condition, .. }) if condition == "item-not-found" => Ok(()),
        Err(e) => Err(e.into()),
    }
}
/// Only direct authenticated PEP events prompt a refresh, never forwarded copies.
pub fn changed(xml: &Element, owner: &str) -> Option<String> {
    if !xml.is("message", "jabber:client")
        || !matches!(xml.attr("type"), None | Some("headline" | "normal"))
        || xml.attr("to").is_some_and(|to| {
            to.parse::<Jid>()
                .ok()
                .is_none_or(|j| j.to_bare().as_str() != owner)
        })
        || xml.children().any(|c| {
            matches!(
                c.ns().as_str(),
                "urn:xmpp:delay" | "jabber:x:delay" | "urn:xmpp:carbons:2" | "urn:xmpp:forward:0"
            )
        })
    {
        return None;
    }
    bounded(xml, 65536).ok()?;
    let peer = xml.attr("from")?;
    bare(peer).ok()?;
    let event = only(xml, "event", EVENT).ok()??;
    event
        .children()
        .any(|i| {
            matches!(i.name(), "items" | "purge" | "delete")
                && i.ns() == EVENT
                && i.attr("node") == Some(NODE)
        })
        .then(|| peer.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn entry() -> Entry {
        Entry {
            id: "mobile-one".into(),
            owner: "alice@example.org".into(),
            url: "https://example.org/story.jpg".into(),
            mime: "image/jpeg".into(),
            caption: "A <caption> & text".into(),
            published: chrono::Utc::now().timestamp_millis(),
        }
    }
    fn form(access: &str) -> Element {
        let mut form = presence_form(None).unwrap();
        form.set_attr("".into(), "type".try_into().unwrap(), "form");
        let fields = fields(&form).unwrap();
        let mut result = attr(el("x", FORM), "type", "form");
        for (name, field) in fields {
            if name == "pubsub#access_model" {
                result.append_child(
                    Element::builder("field", FORM)
                        .attr("var".try_into().unwrap(), name)
                        .append(Element::builder("value", FORM).append(access).build())
                        .build(),
                );
            } else {
                result.append_child(field);
            }
        }
        result.append_child(
            Element::builder("field", FORM)
                .attr("var".try_into().unwrap(), "provider-option")
                .append(Element::builder("value", FORM).append("keep").build())
                .build(),
        );
        result
    }
    #[test]
    fn mobile_atom_roundtrip_expiry_and_unsafe_enclosures() {
        let entry = entry();
        let now = entry.published;
        let xml = entry.atom().unwrap();
        // Independent literal follows mobile XmppStories.Story.entry(), rather
        // than generating the input with the desktop encoder under test.
        let stamp = chrono::DateTime::from_timestamp_millis(now)
            .unwrap()
            .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        let mobile:Element=format!("<entry xmlns='http://www.w3.org/2005/Atom'><id>urn:uuid:mobile-one</id><title>A &lt;caption&gt; &amp; text</title><published>{stamp}</published><updated>{stamp}</updated><author><uri>xmpp:alice@example.org</uri></author><link rel='enclosure' href='https://example.org/story.jpg' type='image/jpeg' title='A &lt;caption&gt; &amp; text'/></entry>").parse().unwrap();
        assert_eq!(parse(&entry.owner, &entry.id, &mobile, now).unwrap(), entry);
        assert_eq!(parse(&entry.owner, &entry.id, &xml, now).unwrap(), entry);
        assert_eq!(
            parse(&entry.owner, &entry.id, &xml, now + LIFETIME),
            Err(Error::Expired)
        );
        assert!(parse("mallory@example.org", &entry.id, &xml, now).is_err());
        let mut duplicate = xml.clone();
        duplicate.append_child(xml.get_child("link", ATOM).unwrap().clone());
        assert!(parse(&entry.owner, &entry.id, &duplicate, now).is_err());
        for url in [
            "file:///secret",
            "http://example.org/a.jpg",
            "https://user@example.org/a.jpg",
            "https://example.org/a.jpg#secret",
        ] {
            let mut invalid = entry.clone();
            invalid.url = url.into();
            assert!(invalid.validate(now).is_err());
        }
        let mut invalid = entry.clone();
        invalid.mime = "text/html".into();
        assert!(invalid.validate(now).is_err());
        invalid = entry.clone();
        invalid.caption = "a".repeat(32769);
        assert!(invalid.validate(now).is_err());
        invalid = entry;
        invalid.published = now + 300001;
        assert!(invalid.validate(now).is_err());
    }
    #[test]
    fn only_bounded_unique_items_and_authenticated_direct_notifications() {
        let entry = entry();
        let item = Element::builder("item", PUBSUB)
            .attr("id".try_into().unwrap(), &entry.id)
            .append(entry.atom().unwrap())
            .build();
        let response = Element::builder("pubsub", PUBSUB)
            .append(
                Element::builder("items", PUBSUB)
                    .attr("node".try_into().unwrap(), NODE)
                    .append(item.clone())
                    .build(),
            )
            .build();
        assert_eq!(
            parse_items(&entry.owner, &response, entry.published).unwrap(),
            vec![entry.clone()]
        );
        let duplicated = Element::builder("pubsub", PUBSUB)
            .append(
                Element::builder("items", PUBSUB)
                    .attr("node".try_into().unwrap(), NODE)
                    .append(item.clone())
                    .append(item)
                    .build(),
            )
            .build();
        assert!(parse_items(&entry.owner, &duplicated, entry.published).is_err());
        let xml:Element=format!("<message xmlns='jabber:client' from='bob@example.org' to='alice@example.org/desktop' type='headline'><event xmlns='{EVENT}'><items node='{NODE}'><retract id='one'/></items></event></message>").parse().unwrap();
        assert_eq!(
            changed(&xml, &entry.owner).as_deref(),
            Some("bob@example.org")
        );
        let mut other = xml.clone();
        other.set_attr("".into(), "to".try_into().unwrap(), "other@example.org");
        assert!(changed(&other, &entry.owner).is_none());
        let mut forwarded = xml;
        forwarded.append_child(el("forwarded", "urn:xmpp:forward:0"));
        assert!(changed(&forwarded, &entry.owner).is_none());
    }
    #[tokio::test]
    async fn publication_confirms_contact_access_and_preserves_original_expiry() {
        for confirmed in [false, true] {
            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
            let client = iq::Client::new(tx);
            let expected = entry();
            let story = expected.clone();
            let task =
                tokio::spawn(async move { publish(&client, "alice@example.org", story).await });
            for step in 0..if confirmed { 4 } else { 3 } {
                let command = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv())
                    .await
                    .unwrap()
                    .unwrap();
                let crate::protocol::Command::Iq {
                    to,
                    kind,
                    payload,
                    reply,
                } = command
                else {
                    panic!("unexpected command")
                };
                assert_eq!(to, expected.owner);
                if step == 0 || step == 2 {
                    assert!(matches!(kind, iq::Kind::Get));
                    assert!(payload.is("pubsub", OWNER));
                    let access = if step == 2 && confirmed {
                        "presence"
                    } else {
                        "open"
                    };
                    reply
                        .send(Ok(Some(
                            Element::builder("pubsub", OWNER)
                                .append(
                                    Element::builder("configure", OWNER)
                                        .attr("node".try_into().unwrap(), NODE)
                                        .append(form(access))
                                        .build(),
                                )
                                .build(),
                        )))
                        .unwrap();
                } else if step == 1 {
                    assert!(matches!(kind, iq::Kind::Set));
                    let config = payload.get_child("configure", OWNER).unwrap();
                    let submitted = config.get_child("x", FORM).unwrap();
                    assert_eq!(submitted.attr("type"), Some("submit"));
                    verify_presence(submitted).unwrap();
                    assert!(fields(submitted).unwrap().contains_key("provider-option"));
                    reply.send(Ok(None)).unwrap();
                } else {
                    assert!(matches!(kind, iq::Kind::Set));
                    let publish = payload.get_child("publish", PUBSUB).unwrap();
                    let item = publish.get_child("item", PUBSUB).unwrap();
                    assert_eq!(item.attr("id"), Some(expected.id.as_str()));
                    let parsed = parse(
                        &expected.owner,
                        &expected.id,
                        item.get_child("entry", ATOM).unwrap(),
                        expected.published,
                    )
                    .unwrap();
                    assert_eq!(parsed.published, expected.published);
                    assert_eq!(parsed.caption, expected.caption);
                    let options = payload
                        .get_child("publish-options", PUBSUB)
                        .unwrap()
                        .get_child("x", FORM)
                        .unwrap();
                    assert_eq!(
                        text(
                            fields(options).unwrap().get("pubsub#access_model").unwrap(),
                            "value",
                            FORM
                        )
                        .unwrap()
                        .as_deref(),
                        Some("presence")
                    );
                    reply.send(Ok(None)).unwrap();
                }
            }
            let result = task.await.unwrap();
            if confirmed {
                assert_eq!(result.unwrap(), expected)
            } else {
                assert_eq!(result, Err(Error::Privacy));
                assert!(
                    rx.try_recv().is_err(),
                    "never publish if access did not change"
                );
            }
        }
    }
}
