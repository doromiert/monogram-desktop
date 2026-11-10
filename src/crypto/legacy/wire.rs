//! Legacy XEP-0384 uses Signal v3 and an AES-128-GCM encrypted body.
use super::{MAX_WIRE, NS};
use crate::crypto::{
    bundle::{self, Bundle},
    native, payload, *,
};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use std::collections::{BTreeMap, BTreeSet};
use tokio_xmpp::minidom::Element;

pub(super) fn attr(mut element: Element, key: &str, value: impl Into<String>) -> Element {
    element.set_attr("".into(), key.try_into().unwrap(), value.into());
    element
}
pub(super) fn element(name: &str, namespace: &str) -> Element {
    Element::builder(name, namespace).build()
}
fn data(e: &Element, max: usize) -> Result<Vec<u8>> {
    if e.children().next().is_some() {
        return Err(Error::Invalid("legacy base64 text"));
    }
    bundle::decode(&e.text(), max)
}
fn bytes<const N: usize>(e: &Element) -> Result<[u8; N]> {
    data(e, N)?
        .try_into()
        .map_err(|_| Error::Invalid("legacy field length"))
}
fn public(e: &Element) -> Result<[u8; 32]> {
    let key = bytes::<33>(e)?;
    if key[0] != 5 {
        return Err(Error::Invalid("Signal curve point encoding"));
    }
    let point = curve25519_dalek::montgomery::MontgomeryPoint(key[1..].try_into().unwrap());
    if point.mul_clamped([1; 32]).0 == [0; 32] {
        return Err(Error::Invalid("legacy low-order public key"));
    }
    Ok(point.0)
}
fn binary(name: &str, bytes: &[u8]) -> Element {
    Element::builder(name, NS)
        .append(STANDARD.encode(bytes))
        .build()
}
fn point(name: &str, bytes: &[u8; 32]) -> Element {
    let mut serialized = [5; 33];
    serialized[1..].copy_from_slice(bytes);
    binary(name, &serialized)
}
pub(super) fn parse_bundle(root: &Element) -> Result<Bundle> {
    if !root.is("bundle", NS) {
        return Err(Error::Invalid("legacy bundle namespace"));
    }
    bundle::xml(root, 256 * 1024)?;
    let signed = bundle::single(root, "signedPreKeyPublic", NS)?;
    let mut prekeys = BTreeMap::new();
    for p in bundle::single(root, "prekeys", NS)?.children() {
        if !p.is("preKeyPublic", NS) || prekeys.len() >= 500 {
            return Err(Error::Invalid("legacy prekeys"));
        }
        if prekeys
            .insert(bundle::uint(p.attr("preKeyId"))?, public(p)?)
            .is_some()
        {
            return Err(Error::Invalid("duplicate legacy prekey"));
        }
    }
    if prekeys.is_empty() {
        return Err(Error::Invalid("empty legacy prekeys"));
    }
    let bundle = Bundle {
        identity: public(bundle::single(root, "identityKey", NS)?)?,
        signed_id: bundle::uint(signed.attr("signedPreKeyId"))?,
        signed: public(signed)?,
        signature: bytes(bundle::single(root, "signedPreKeySignature", NS)?)?,
        prekeys,
    };
    native::verify_legacy(&bundle)?;
    Ok(bundle)
}
pub(super) fn bundle_xml(bundle: &Bundle) -> Element {
    let mut prekeys = element("prekeys", NS);
    for (id, key) in &bundle.prekeys {
        prekeys.append_child(attr(point("preKeyPublic", key), "preKeyId", id.to_string()));
    }
    Element::builder("bundle", NS)
        .append(attr(
            point("signedPreKeyPublic", &bundle.signed),
            "signedPreKeyId",
            bundle.signed_id.to_string(),
        ))
        .append(binary("signedPreKeySignature", &bundle.signature))
        .append(point("identityKey", &bundle.identity))
        .append(prekeys)
        .build()
}
pub(super) fn device_ids(root: Option<&Element>) -> Result<BTreeSet<u32>> {
    let mut ids = BTreeSet::new();
    let Some(root) = root else {
        return Ok(ids);
    };
    if !root.is("list", NS) {
        return Err(Error::Invalid("legacy device list namespace"));
    }
    bundle::xml(root, 65536)?;
    for child in root.children() {
        if !child.is("device", NS)
            || ids.len() >= 500
            || !ids.insert(bundle::positive(child.attr("id"))?)
        {
            return Err(Error::Invalid("legacy device list"));
        }
    }
    Ok(ids)
}

pub(super) struct Key {
    pub prekey: bool,
    pub bytes: Vec<u8>,
}
pub(super) struct Envelope {
    pub sender: u32,
    pub peer: String,
    pub iv: Vec<u8>,
    pub keys: Vec<Key>,
    pub ciphertext: Option<Vec<u8>>,
    pub digest: [u8; 32],
}
impl Envelope {
    pub fn parse(message: &Element, owner: &str, device: u32) -> Result<Self> {
        bundle::xml(message, MAX_WIRE)?;
        let (from, to) = payload::routing(message)?;
        if (from != owner && to != owner)
            || message.attr("type") == Some("groupchat")
            || message
                .get_child("x", "http://jabber.org/protocol/muc#user")
                .is_some()
        {
            return Err(Error::Invalid("legacy direct routing"));
        }
        let root = bundle::single(message, "encrypted", NS)?;
        if message
            .children()
            .filter(|child| child.name() == "encrypted")
            .count()
            != 1
        {
            return Err(Error::Invalid("multiple encrypted envelopes"));
        }
        let header = bundle::single(root, "header", NS)?;
        let sender = bundle::positive(header.attr("sid"))?;
        if from == owner && sender == device {
            return Err(Error::Unavailable("Message was encrypted by this device"));
        }
        let iv = data(bundle::single(header, "iv", NS)?, 16)?;
        if !matches!(iv.len(), 12 | 16) {
            return Err(Error::Invalid("legacy GCM nonce"));
        }
        let mut keys = Vec::new();
        let mut count = 0;
        for key in header.children().filter(|c| c.is("key", NS)) {
            count += 1;
            if count > 1000 {
                return Err(Error::Invalid("legacy recipient limit"));
            }
            if bundle::positive(key.attr("rid"))? != device {
                continue;
            }
            let prekey = match key.attr("prekey") {
                None | Some("false" | "0") => false,
                Some("true" | "1") => true,
                _ => return Err(Error::Invalid("legacy prekey flag")),
            };
            let bytes = data(key, 16384)?;
            if bytes.is_empty() || keys.len() >= 2 {
                return Err(Error::Invalid("legacy recipient key"));
            }
            keys.push(Key { prekey, bytes });
        }
        if keys.is_empty() {
            return Err(Error::Unavailable(
                "Encrypted message does not include this device",
            ));
        }
        let mut payloads = root.children().filter(|c| c.is("payload", NS));
        let ciphertext = payloads
            .next()
            .map(|value| data(value, payload::MAX))
            .transpose()?;
        if payloads.next().is_some() || ciphertext.as_ref().is_some_and(Vec::is_empty) {
            return Err(Error::Invalid("legacy payload shape"));
        }
        let mut digest = openssl::sha::Sha256::new();
        let mut add = |bytes: &[u8]| {
            digest.update(&(bytes.len() as u32).to_be_bytes());
            digest.update(bytes);
        };
        add(NS.as_bytes());
        add(from.as_bytes());
        add(to.as_bytes());
        add(&sender.to_be_bytes());
        add(&device.to_be_bytes());
        add(&iv);
        if let Some(ciphertext) = &ciphertext {
            add(ciphertext);
        }
        keys.sort_by(|a, b| a.prekey.cmp(&b.prekey).then_with(|| a.bytes.cmp(&b.bytes)));
        for key in &keys {
            add(&[key.prekey.into()]);
            add(&key.bytes);
        }
        Ok(Self {
            sender,
            peer: from,
            iv,
            keys,
            ciphertext,
            digest: digest.finish(),
        })
    }
}
pub(super) fn key_xml(id: u32, prekey: bool, bytes: &[u8]) -> Element {
    let mut key = attr(binary("key", bytes), "rid", id.to_string());
    if prekey {
        key = attr(key, "prekey", "true");
    }
    key
}
pub(super) fn stanza(
    owner: &str,
    peer: &str,
    id: &str,
    local: u32,
    keys: Vec<Element>,
    iv: &[u8],
    ciphertext: Option<&[u8]>,
) -> Element {
    let mut header = attr(element("header", NS), "sid", local.to_string());
    for key in keys {
        header.append_child(key);
    }
    header.append_child(binary("iv", iv));
    let mut encrypted = element("encrypted", NS);
    encrypted.append_child(header);
    let mut message = Element::builder("message", payload::CLIENT)
        .attr("from".try_into().unwrap(), owner)
        .attr("to".try_into().unwrap(), peer)
        .attr("id".try_into().unwrap(), id)
        .attr("type".try_into().unwrap(), "chat")
        .build();
    if let Some(ciphertext) = ciphertext {
        encrypted.append_child(binary("payload", ciphertext));
        message.append_child(element("store", "urn:xmpp:hints"));
        message.append_child(attr(element("origin-id", "urn:xmpp:sid:0"), "id", id));
    }
    message.append_child(encrypted);
    message
}
