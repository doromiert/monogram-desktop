//! Inline XEP-0084 and vcard-temp avatars. Never follow a remote image URL.
use crate::iq;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use gtk::glib;
use tokio_xmpp::{jid::Jid, minidom::Element};

const CLIENT: &str = "jabber:client";
const PUBSUB: &str = "http://jabber.org/protocol/pubsub";
const EVENT: &str = "http://jabber.org/protocol/pubsub#event";
const META: &str = "urn:xmpp:avatar:metadata";
const DATA: &str = "urn:xmpp:avatar:data";
const VCARD: &str = "vcard-temp";
const UPDATE: &str = "vcard-temp:x:update";
const MAX_BYTES: usize = 1024 * 1024;
const MAX_ENCODED: usize = MAX_BYTES.div_ceil(3) * 4;
const MAX_WIRE_BYTES: usize = MAX_ENCODED + MAX_ENCODED / 32;
const PNG_MAGIC: &[u8] = b"\x89PNG\r\n\x1a\n";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// A person's bare JID. Prefer XEP-0084, then read their vCard.
    Pep,
    /// A bare room or a full occupant JID, whose avatar belongs to its vCard.
    VCard,
}

#[derive(Debug)]
pub enum Error {
    Invalid,
    Request(iq::Error),
}

pub struct Avatar {
    pub(crate) bytes: glib::Bytes,
    hash: String,
    pub(crate) pep: bool,
}

impl std::fmt::Debug for Avatar {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Avatar")
            .field("len", &self.bytes.len())
            .field("pep", &self.pep)
            .finish_non_exhaustive()
    }
}

impl Avatar {
    pub(crate) fn new(bytes: Vec<u8>, pep: bool) -> Self {
        let hash = openssl::sha::sha1(&bytes)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        Self {
            bytes: glib::Bytes::from_owned(bytes),
            hash,
            pep,
        }
    }

    pub fn hash(&self) -> &str {
        &self.hash
    }
}

fn single<'a>(element: &'a Element, name: &str, ns: &str) -> Result<Option<&'a Element>, Error> {
    let mut matches = element.children().filter(|child| child.is(name, ns));
    let first = matches.next();
    if matches.next().is_some() {
        return Err(Error::Invalid);
    }
    Ok(first)
}

fn sha1_id(value: &str) -> bool {
    value.len() == 40 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// A valid XEP-0153 update. A present empty photo clears the vCard hint only.
pub fn presence_hash(root: &Element) -> Option<String> {
    if !root.is("presence", CLIENT) || !matches!(root.attr("type"), None | Some("available")) {
        return None;
    }
    let update = single(root, "x", UPDATE).ok()??;
    let photo = single(update, "photo", UPDATE).ok()??;
    if photo.children().next().is_some() || photo.texts().map(str::len).sum::<usize>() > 256 {
        return None;
    }
    let value = photo.text();
    let value = value.trim();
    (value.is_empty() || sha1_id(value)).then(|| value.to_ascii_lowercase())
}

/// A PEP notification is a freshness hint. Retrieve the current item from the authenticated IQ target.
pub fn pep_changed(root: &Element) -> bool {
    root.is("message", CLIENT)
        && root.attr("type") != Some("error")
        && single(root, "event", EVENT)
            .ok()
            .flatten()
            .is_some_and(|event| {
                event.children().any(|child| {
                    child.ns() == EVENT
                        && matches!(child.name(), "items" | "purge" | "delete")
                        && child.attr("node") == Some(META)
                })
            })
}

fn inline(element: &Element) -> Result<Vec<u8>, Error> {
    if element.children().next().is_some() {
        return Err(Error::Invalid);
    }
    let mut encoded = Vec::new();
    let mut length = 0;
    for chunk in element.texts() {
        length += chunk.len();
        if length > MAX_WIRE_BYTES {
            return Err(Error::Invalid);
        }
        encoded.extend(chunk.bytes().filter(|byte| !byte.is_ascii_whitespace()));
    }
    if encoded.len() > MAX_ENCODED {
        return Err(Error::Invalid);
    }
    let bytes = STANDARD.decode(encoded).map_err(|_| Error::Invalid)?;
    if bytes.len() > MAX_BYTES {
        return Err(Error::Invalid);
    }
    Ok(bytes)
}

async fn optional(iq: &iq::Client, jid: &str, payload: Element) -> Result<Option<Element>, Error> {
    match iq.request(jid, iq::Kind::Get, payload).await {
        Ok(result) => Ok(result),
        Err(iq::Error::Rejected { condition, .. })
            if matches!(
                condition.as_str(),
                "item-not-found" | "feature-not-implemented"
            ) =>
        {
            Ok(None)
        }
        Err(error) => Err(Error::Request(error)),
    }
}

async fn item(
    iq: &iq::Client,
    jid: &str,
    node: &str,
    id: Option<&str>,
) -> Result<Option<Element>, Error> {
    let mut items = Element::builder("items", PUBSUB).attr("node".try_into().unwrap(), node);
    items = if let Some(id) = id {
        items.append(
            Element::builder("item", PUBSUB)
                .attr("id".try_into().unwrap(), id)
                .build(),
        )
    } else {
        items.attr("max_items".try_into().unwrap(), "1")
    };
    let query = Element::builder("pubsub", PUBSUB)
        .append(items.build())
        .build();
    let Some(mut reply) = optional(iq, jid, query).await? else {
        return Ok(None);
    };
    if !reply.is("pubsub", PUBSUB) {
        return Err(Error::Invalid);
    }
    single(&reply, "items", PUBSUB)?.ok_or(Error::Invalid)?;
    let mut items = reply.remove_child("items", PUBSUB).ok_or(Error::Invalid)?;
    if items.attr("node") != Some(node) {
        return Err(Error::Invalid);
    }
    if single(&items, "item", PUBSUB)?.is_none() {
        return Ok(None);
    }
    let mut item = items.remove_child("item", PUBSUB).ok_or(Error::Invalid)?;
    if id.is_some_and(|requested| item.attr("id") != Some(requested)) {
        return Err(Error::Invalid);
    }
    let data = item.unshift_child().ok_or(Error::Invalid)?;
    if item.children().next().is_some() {
        return Err(Error::Invalid);
    }
    Ok(Some(data))
}

enum Pep {
    Missing,
    Disabled,
    Avatar(Avatar),
}

async fn pep(iq: &iq::Client, jid: &str) -> Result<Pep, Error> {
    let Some(meta) = item(iq, jid, META, None).await? else {
        return Ok(Pep::Missing);
    };
    if !meta.is("metadata", META) || meta.texts().any(|text| !text.trim().is_empty()) {
        return Err(Error::Invalid);
    }
    if meta.children().next().is_none() {
        return Ok(Pep::Disabled);
    }
    let (id, size) = meta
        .children()
        .take(16)
        .find_map(|info| {
            if !info.is("info", META)
                || info.attr("type") != Some("image/png")
                || info.attr("url").is_some()
            {
                return None;
            }
            let id = info.attr("id").filter(|id| sha1_id(id))?;
            let size = info.attr("bytes")?.parse::<usize>().ok()?;
            (1..=MAX_BYTES).contains(&size).then_some((id, size))
        })
        .ok_or(Error::Invalid)?;
    let data = item(iq, jid, DATA, Some(id)).await?.ok_or(Error::Invalid)?;
    if !data.is("data", DATA) {
        return Err(Error::Invalid);
    }
    let bytes = inline(&data)?;
    if bytes.len() != size || !bytes.starts_with(PNG_MAGIC) {
        return Err(Error::Invalid);
    }
    let avatar = Avatar::new(bytes, true);
    if !avatar.hash().eq_ignore_ascii_case(id) {
        return Err(Error::Invalid);
    }
    Ok(Pep::Avatar(avatar))
}

async fn vcard(iq: &iq::Client, jid: &str) -> Result<Option<Avatar>, Error> {
    let Some(card) = optional(iq, jid, Element::builder("vCard", VCARD).build()).await? else {
        return Ok(None);
    };
    if !card.is("vCard", VCARD) {
        return Err(Error::Invalid);
    }
    let Some(photo) = single(&card, "PHOTO", VCARD)? else {
        return Ok(None);
    };
    if let Some(data) = single(photo, "BINVAL", VCARD)? {
        let bytes = inline(data)?;
        if !bytes.is_empty() {
            // XEP-0153 treats TYPE as a hint. The sandbox identifies the actual raster format.
            return Ok(Some(Avatar::new(bytes, false)));
        }
    }
    if single(photo, "EXTVAL", VCARD)?.is_some_and(|url| {
        url.children().next().is_some() || url.texts().any(|text| !text.trim().is_empty())
    }) {
        return Err(Error::Invalid);
    }
    Ok(None)
}

/// Fetch only for a requested chat. `Ok(None)` is confirmed absent; retain any cached
/// avatar on an error. No network input enters a GTK image widget without sandbox decoding.
pub async fn fetch(
    iq: &iq::Client,
    address: &str,
    source: Source,
) -> Result<Option<Avatar>, Error> {
    let jid = address.parse::<Jid>().map_err(|_| Error::Invalid)?;
    if jid.node().is_none()
        || jid.to_string() != address
        || (source == Source::Pep && jid.is_full())
    {
        return Err(Error::Invalid);
    }
    if source == Source::VCard {
        return vcard(iq, address).await;
    }
    match pep(iq, address).await {
        Ok(Pep::Avatar(avatar)) => Ok(Some(avatar)),
        Ok(Pep::Disabled) => Ok(None),
        Ok(Pep::Missing) => vcard(iq, address).await,
        Err(error @ Error::Request(iq::Error::Closed | iq::Error::Timeout | iq::Error::Busy)) => {
            Err(error)
        }
        Err(error) => vcard(iq, address)
            .await
            .and_then(|avatar| avatar.map(Some).ok_or(error)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::Command;
    use tokio::sync::mpsc;

    const PERSON: &str = "person@example.org";
    const ROOM: &str = "room@conference.example.org";
    const OCCUPANT: &str = "room@conference.example.org/nickname";
    const PNG: &[u8] = b"\x89PNG\r\n\x1a\nfixture";

    fn xml(text: &str) -> Element {
        text.parse().unwrap()
    }

    fn node_reply(node: &str, id: &str, value: &str) -> iq::Reply {
        Ok(Some(xml(&format!(
            "<pubsub xmlns='{PUBSUB}'><items node='{node}'><item id='{id}'>{value}</item></items></pubsub>"
        ))))
    }

    fn iq_fixture(jid: &'static str, replies: Vec<(Option<String>, iq::Reply)>) -> iq::Client {
        let (tx, mut rx) = mpsc::unbounded_channel();
        tokio::spawn(async move {
            for (node, answer) in replies {
                let Some(Command::Iq {
                    to,
                    kind: iq::Kind::Get,
                    payload,
                    reply,
                }) = rx.recv().await
                else {
                    panic!("expected an IQ GET");
                };
                assert_eq!(to, jid);
                if let Some(node) = node {
                    let items = payload.get_child("items", PUBSUB).unwrap();
                    let (node, id) = node.split_once(' ').unwrap_or((&node, ""));
                    assert_eq!(items.attr("node"), Some(node));
                    if id.is_empty() {
                        assert_eq!(items.attr("max_items"), Some("1"));
                    } else {
                        assert_eq!(
                            items.get_child("item", PUBSUB).unwrap().attr("id"),
                            Some(id)
                        );
                    }
                } else {
                    assert!(payload.is("vCard", VCARD));
                }
                let _ = reply.send(answer);
            }
        });
        iq::Client::new(tx)
    }

    #[tokio::test]
    async fn pep_checks_exact_item_bytes_and_does_not_clear_on_failed_refresh() {
        let hash = Avatar::new(PNG.to_vec(), true).hash;
        let meta = format!(
            "<metadata xmlns='{META}'><info type='image/png' id='{hash}' bytes='{}'/></metadata>",
            PNG.len()
        );
        let make_data =
            |data: &[u8]| format!("<data xmlns='{DATA}'>\n{}\n</data>", STANDARD.encode(data));
        let good = iq_fixture(
            PERSON,
            vec![
                (Some(META.into()), node_reply(META, &hash, &meta)),
                (
                    Some(format!("{DATA} {hash}")),
                    node_reply(DATA, &hash, &make_data(PNG)),
                ),
            ],
        );
        let fetched = fetch(&good, PERSON, Source::Pep).await.unwrap().unwrap();
        assert_eq!(fetched.hash(), hash);
        assert_eq!(fetched.bytes.as_ref(), PNG);

        let mut tampered = PNG.to_vec();
        *tampered.last_mut().unwrap() ^= 1;
        let bad = iq_fixture(
            PERSON,
            vec![
                (Some(META.into()), node_reply(META, &hash, &meta)),
                (
                    Some(format!("{DATA} {hash}")),
                    node_reply(DATA, &hash, &make_data(&tampered)),
                ),
                (None, Ok(Some(xml("<vCard xmlns='vcard-temp'/>")))),
            ],
        );
        assert!(matches!(
            fetch(&bad, PERSON, Source::Pep).await,
            Err(Error::Invalid)
        ));
    }

    #[tokio::test]
    async fn absent_pep_falls_back_but_explicit_removal_skips_the_card() {
        let card = xml(&format!(
            "<vCard xmlns='{VCARD}'><PHOTO><BINVAL>{}</BINVAL></PHOTO></vCard>",
            STANDARD.encode(PNG)
        ));
        let fallback = iq_fixture(
            PERSON,
            vec![
                (Some(META.into()), Ok(None)),
                (None, Ok(Some(card.clone()))),
            ],
        );
        assert_eq!(
            fetch(&fallback, PERSON, Source::Pep)
                .await
                .unwrap()
                .unwrap()
                .bytes
                .as_ref(),
            PNG
        );
        let disabled = iq_fixture(
            PERSON,
            vec![(
                Some(META.into()),
                node_reply(META, "old", &format!("<metadata xmlns='{META}'/>")),
            )],
        );
        assert!(
            fetch(&disabled, PERSON, Source::Pep)
                .await
                .unwrap()
                .is_none()
        );
        let room = iq_fixture(ROOM, vec![(None, Ok(Some(card.clone())))]);
        assert_eq!(
            fetch(&room, ROOM, Source::VCard)
                .await
                .unwrap()
                .unwrap()
                .bytes
                .as_ref(),
            PNG
        );
        let occupant = iq_fixture(OCCUPANT, vec![(None, Ok(Some(card)))]);
        assert!(
            fetch(&occupant, OCCUPANT, Source::VCard)
                .await
                .unwrap()
                .is_some()
        );
        let remote_only = iq_fixture(
            ROOM,
            vec![(
                None,
                Ok(Some(xml(
                    "<vCard xmlns='vcard-temp'><PHOTO><EXTVAL>https://example.org/avatar.png</EXTVAL></PHOTO></vCard>",
                ))),
            )],
        );
        assert!(matches!(
            fetch(&remote_only, ROOM, Source::VCard).await,
            Err(Error::Invalid)
        ));
    }

    #[test]
    fn ignores_invalid_update_hints() {
        assert_eq!(
            presence_hash(&xml(
                "<presence xmlns='jabber:client'><x xmlns='vcard-temp:x:update'><photo/></x></presence>"
            )),
            Some(String::new())
        );
        assert!(
            presence_hash(&xml(
                "<presence xmlns='jabber:client'><x xmlns='vcard-temp:x:update'/></presence>"
            ))
            .is_none()
        );
        assert!(presence_hash(&xml("<presence xmlns='jabber:client'><x xmlns='vcard-temp:x:update'><photo>bad</photo></x></presence>")).is_none());
        assert!(pep_changed(&xml(
            "<message xmlns='jabber:client'><event xmlns='http://jabber.org/protocol/pubsub#event'><items node='urn:xmpp:avatar:metadata'/></event></message>"
        )));
        assert!(!pep_changed(&xml(
            "<message xmlns='jabber:client'><event xmlns='http://jabber.org/protocol/pubsub#event'><items node='urn:xmpp:avatar:data'/></event></message>"
        )));
    }

    #[test]
    #[ignore = "requires the disposable loopback Prosody fixture"]
    fn reads_an_avatar_published_by_another_account_over_tls() {
        use crate::{
            model::Account,
            protocol::{BackendEvent, Session},
        };
        use std::{
            sync::mpsc,
            time::{Duration, Instant},
        };

        assert_eq!(
            std::env::var("MONOGRAM_KEYRING_FIXTURE").as_deref(),
            Ok("1")
        );
        let _ = tokio_xmpp::rustls::crypto::aws_lc_rs::default_provider().install_default();
        let port = std::env::var("MONOGRAM_IT_PORT").unwrap().parse().unwrap();
        let connect = |jid: &str| {
            let (tx, rx) = mpsc::channel();
            let session = Session::start(
                Account {
                    jid: jid.into(),
                    host: "127.0.0.1".into(),
                    port,
                    direct_tls: false,
                },
                "fixture-password".into(),
                1,
                tx,
            );
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                let (_, _, event) = rx
                    .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                    .unwrap();
                if matches!(event, BackendEvent::Status { online: true, .. }) {
                    return (session, rx);
                }
            }
        };
        let (publisher, _publisher_events) = connect("alice@localhost");
        let (viewer, _viewer_events) = connect("bob@localhost");
        let publisher = iq::Client::new(publisher.tx.clone());
        let viewer = iq::Client::new(viewer.tx.clone());
        let png = STANDARD.decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGNgYGD4DwABBAEAX+XDSwAAAABJRU5ErkJggg==").unwrap();
        let hash = Avatar::new(png.clone(), true).hash;
        let data = format!("<data xmlns='{DATA}'>{}</data>", STANDARD.encode(&png));
        let meta = format!(
            "<metadata xmlns='{META}'><info id='{hash}' type='image/png' bytes='{}'/></metadata>",
            png.len()
        );
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();
        runtime.block_on(async {
            for (node, content) in [(DATA, &data), (META, &meta)] {
                let publish = xml(&format!("<pubsub xmlns='{PUBSUB}'><publish node='{node}'><item id='{hash}'>{content}</item></publish><publish-options><x xmlns='jabber:x:data' type='submit'><field var='FORM_TYPE' type='hidden'><value>{PUBSUB}#publish-options</value></field><field var='pubsub#access_model'><value>open</value></field></x></publish-options></pubsub>"));
                publisher.request("alice@localhost", iq::Kind::Set, publish).await.unwrap();
            }
            let avatar = fetch(&viewer, "alice@localhost", Source::Pep).await.unwrap().unwrap();
            assert_eq!(avatar.hash(), hash);
            assert_eq!(avatar.bytes.as_ref(), &png);
        });
    }
}
