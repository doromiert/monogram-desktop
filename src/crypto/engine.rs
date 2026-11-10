//! Complete OMEMO 2 stanza encryption/decryption over prepared, reviewed devices.
use super::{
    bundle::{self, Bundle, NS},
    device,
    journal::{Journal, Received},
    native, payload,
    records::*,
    *,
};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use std::collections::BTreeMap;
use tokio_xmpp::minidom::Element;
pub struct Engine {
    pub store: Store,
    pub journal: Journal,
}
#[derive(Clone)]
struct Key {
    rid: u32,
    kex: bool,
    ciphertext: Vec<u8>,
}
struct Envelope {
    sender: u32,
    peer: String,
    digest: [u8; 32],
    key: Key,
    ciphertext: Option<Vec<u8>>,
}
impl Engine {
    pub fn new(mut store: Store, now: u64) -> Result<Self> {
        device::prepare(&mut store, now)?;
        let journal = Journal::new(&mut store)?;
        Ok(Self { store, journal })
    }
    pub fn encrypt(
        &mut self,
        plain: &Element,
        recipients: &BTreeMap<String, BTreeMap<u32, Bundle>>,
    ) -> Result<Element> {
        let (from, to) = payload::routing(plain)?;
        if from != self.store.owner
            || plain.attr("type") == Some("groupchat")
            || recipients.keys().any(|jid| jid != &to && jid != &from)
        {
            return Err(Error::Invalid("encrypted recipients"));
        }
        let count: usize = recipients.values().map(|v| v.len()).sum();
        if count == 0 || count > 1000 || recipients.get(&to).is_none_or(|v| v.is_empty()) {
            return Err(Error::Invalid("no recipient devices"));
        }
        let envelope = payload::envelope(plain)?;
        let sealed = payload::seal(&envelope)?;
        let local = self.store.device;
        self.store.transaction(|tx| {
            let mut header = Element::builder("header", NS)
                .attr("sid".try_into().unwrap(), local.to_string())
                .build();
            for (peer, devices) in recipients {
                if devices.is_empty() {
                    continue;
                }
                let mut keys = Element::builder("keys", NS)
                    .attr("jid".try_into().unwrap(), peer)
                    .build();
                for (id, bundle) in devices {
                    address(peer, *id)?;
                    if peer == &from && *id == local {
                        return Err(Error::Invalid("self device target"));
                    }
                    device::session(tx, local, peer, *id, bundle)?;
                    let (kex, ciphertext) =
                        native::encrypt(tx, local, peer, *id, &sealed.material)?;
                    keys.append_child(key_xml(*id, kex, &ciphertext));
                }
                header.append_child(keys);
            }
            let encrypted = Element::builder("encrypted", NS)
                .append(header)
                .append(
                    Element::builder("payload", NS)
                        .append(STANDARD.encode(&sealed.ciphertext))
                        .build(),
                )
                .build();
            let mut result = outer(plain)?;
            result.append_child(encrypted);
            result.append_child(Element::builder("store", "urn:xmpp:hints").build());
            Ok(result)
        })
    }
    pub fn receive(&mut self, message: &Element, sent_time: i64) -> Result<Received> {
        self.receive_with_context(message, sent_time, None)
    }
    pub fn receive_with_context(
        &mut self,
        message: &Element,
        sent_time: i64,
        context: Option<super::journal::Context>,
    ) -> Result<Received> {
        if context.as_ref().is_some_and(|v| {
            v.archive
                .as_ref()
                .is_some_and(|(id, stamp)| id.len() > 4096 || stamp.len() > 100)
                || v.receipt_to.as_ref().is_some_and(|v| v.len() > 4096)
        }) {
            return Err(Error::Invalid("incoming context bounds"));
        }
        let envelope = parse(message, &self.store.owner, self.store.device)?;
        let local = self.store.device;
        let owner = self.store.owner.clone();
        let journal = &self.journal;
        self.store.transaction(|tx| {
            if let Some(cached) = journal.find(tx, &envelope.digest)? {
                return Ok(cached);
            }
            let material = native::decrypt(
                tx,
                local,
                &envelope.peer,
                envelope.sender,
                &envelope.key.ciphertext,
                envelope.key.kex,
            )?;
            let content = if let Some(ciphertext) = &envelope.ciphertext {
                let plain = payload::open(ciphertext, &material)?;
                Some(payload::content(message, &plain, sent_time)?)
            } else {
                if material.len() != 32 || material.iter().any(|b| *b != 0) {
                    return Err(Error::Invalid("heartbeat material"));
                }
                None
            };
            let acknowledgement = if envelope.key.kex {
                let (kex, bytes) =
                    native::encrypt(tx, local, &envelope.peer, envelope.sender, &[0; 32])?;
                let keys = Element::builder("keys", NS)
                    .attr("jid".try_into().unwrap(), &envelope.peer)
                    .append(key_xml(envelope.sender, kex, &bytes))
                    .build();
                let header = Element::builder("header", NS)
                    .attr("sid".try_into().unwrap(), local.to_string())
                    .append(keys)
                    .build();
                Some(
                    Element::builder("message", payload::CLIENT)
                        .attr("from".try_into().unwrap(), &owner)
                        .attr("to".try_into().unwrap(), message.attr("from").unwrap())
                        .attr("type".try_into().unwrap(), "chat")
                        .attr("id".try_into().unwrap(), crate::model::new_id())
                        .append(Element::builder("encrypted", NS).append(header).build())
                        .build(),
                )
            } else {
                None
            };
            let fingerprint = tx
                .load(SESSION_IDENTITY, &envelope.peer, envelope.sender)?
                .ok_or(Error::Incomplete)?;
            let fingerprint = fingerprint
                .as_slice()
                .try_into()
                .map_err(|_| Error::Incomplete)?;
            let replenish = tx.ids(PRE_KEY)?.len() < 50;
            if content.is_some() || acknowledgement.is_some() {
                journal.append(
                    tx,
                    envelope.digest,
                    message,
                    content.as_ref(),
                    acknowledgement.as_ref(),
                    fingerprint,
                    replenish,
                    sent_time,
                    context.clone(),
                )
            } else {
                Ok(Received {
                    content: None,
                    outer: None,
                    acknowledgement: None,
                    fingerprint,
                    replenish,
                    token: None,
                    sent_time,
                    context,
                })
            }
        })
    }
    pub fn digest(message: &Element, owner: &str, device: u32) -> Result<[u8; 32]> {
        Ok(parse(message, owner, device)?.digest)
    }
}
fn key_xml(id: u32, kex: bool, ciphertext: &[u8]) -> Element {
    Element::builder("key", NS)
        .attr("rid".try_into().unwrap(), id.to_string())
        .attr(
            "kex".try_into().unwrap(),
            if kex { "true" } else { "false" },
        )
        .append(STANDARD.encode(ciphertext))
        .build()
}
fn outer(plain: &Element) -> Result<Element> {
    let mut b = Element::builder("message", payload::CLIENT);
    for name in ["from", "to", "type", "id"] {
        if let Some(value) = plain.attr(name) {
            b = b.attr(name.try_into().unwrap(), value)
        }
    }
    Ok(b.build())
}
fn parse(message: &Element, owner: &str, device: u32) -> Result<Envelope> {
    let (from, to) = payload::routing(message)?;
    if message.attr("type") == Some("groupchat") || (&from != owner && &to != owner) {
        return Err(Error::Invalid("direct encryption scope"));
    }
    let encrypted = bundle::single(message, "encrypted", NS)?;
    bundle::xml(encrypted, 1024 * 1024)?;
    let header = bundle::single(encrypted, "header", NS)?;
    let sender = bundle::positive(header.attr("sid"))?;
    let mut groups = BTreeMap::new();
    let mut count = 0;
    let mut selected = None;
    for group in header.children() {
        if !group.is("keys", NS) {
            return Err(Error::Invalid("header group"));
        }
        let jid = group.attr("jid").ok_or(Error::Invalid("group jid"))?;
        address(jid, 1)?;
        if groups.contains_key(jid) {
            return Err(Error::Invalid("duplicate key group"));
        }
        let mut keys = BTreeMap::new();
        for key in group.children() {
            count += 1;
            if !key.is("key", NS) || count > 1000 {
                return Err(Error::Invalid("key count"));
            }
            let rid = bundle::positive(key.attr("rid"))?;
            let kex = match key.attr("kex") {
                Some("true" | "1") => true,
                None | Some("false" | "0") => false,
                _ => return Err(Error::Invalid("key exchange flag")),
            };
            if key.children().next().is_some() {
                return Err(Error::Invalid("key content"));
            }
            let ciphertext = bundle::decode(&key.text(), 16384)?;
            if ciphertext.is_empty() {
                return Err(Error::Invalid("empty ratchet key"));
            }
            let value = Key {
                rid,
                kex,
                ciphertext,
            };
            if jid == owner && rid == device {
                selected = Some(value.clone())
            }
            if keys.insert(rid, value).is_some() {
                return Err(Error::Invalid("duplicate device key"));
            }
        }
        groups.insert(jid.to_string(), keys);
    }
    let mut payloads = encrypted.children().filter(|c| c.is("payload", NS));
    let ciphertext = if let Some(payload) = payloads.next() {
        if payloads.next().is_some() || payload.children().next().is_some() {
            return Err(Error::Invalid("duplicate payload"));
        }
        let bytes = bundle::decode(&payload.text(), payload::MAX + 16)?;
        if bytes.is_empty() {
            return Err(Error::Invalid("empty payload"));
        }
        Some(bytes)
    } else {
        None
    };
    // Stable across carbons/MAM, key ordering, equivalent boolean flags and base64 whitespace.
    let canonical: Vec<_> = groups
        .iter()
        .map(|(jid, keys)| {
            (
                jid,
                keys.values()
                    .map(|key| (key.rid, key.kex, STANDARD.encode(&key.ciphertext)))
                    .collect::<Vec<_>>(),
            )
        })
        .collect();
    let bytes = serde_json::to_vec(&(
        "Monogram OMEMO2 incoming v1",
        &from,
        &to,
        message.attr("type").unwrap_or("normal"),
        sender,
        canonical,
        &ciphertext,
    ))
    .map_err(|_| Error::Invalid("envelope digest"))?;
    Ok(Envelope {
        sender,
        peer: from,
        digest: openssl::sha::sha256(&bytes),
        key: selected.ok_or(Error::Invalid("not addressed to this device"))?,
        ciphertext,
    })
}
