use super::*;
use base64::{Engine, engine::general_purpose::STANDARD};
use curve25519_dalek::{edwards::CompressedEdwardsY, traits::IsIdentity};
use openssl::{
    derive::Deriver,
    pkey::{Id, PKey},
    sign::Verifier,
};
use std::{collections::BTreeMap, io::Write};
use tokio_xmpp::minidom::Element;
pub const NS: &str = "urn:xmpp:omemo:2";
/// Bound DOM depth before constructing a recursive tree, and consume the whole document.
pub fn document(text: &str, limit: usize) -> Result<Element> {
    use tokio_xmpp::minidom::{
        rxml::{RawEvent, RawReader},
        tree_builder::TreeBuilder,
    };
    if text.len() > limit || text.contains("<!") {
        return Err(Error::Invalid("XML bounds/declarations"));
    }
    let mut reader = RawReader::new(text.as_bytes());
    let mut tree = TreeBuilder::new();
    let mut root = None;
    let mut nodes = 0;
    while let Some(event) = reader.read().map_err(|_| Error::Invalid("XML document"))? {
        match &event {
            RawEvent::ElementHeadOpen(..) => {
                nodes += 1;
                if root.is_some() || tree.depth() >= 64 || nodes > 16384 {
                    return Err(Error::Invalid("XML depth/document bounds"));
                }
            }
            RawEvent::Text(_, text) if tree.depth() == 0 && !text.trim().is_empty() => {
                return Err(Error::Invalid("XML trailing text"));
            }
            _ => {}
        }
        tree.process_event(event)
            .map_err(|_| Error::Invalid("XML tree"))?;
        if let Some(finished) = tree.root.take() {
            root = Some(finished);
        }
    }
    if tree.depth() != 0 {
        return Err(Error::Invalid("incomplete XML"));
    }
    root.ok_or(Error::Invalid("empty XML"))
}
#[derive(Clone, Debug)]
pub struct Bundle {
    pub identity: [u8; 32],
    pub signed_id: u32,
    pub signed: [u8; 32],
    pub signature: [u8; 64],
    pub prekeys: BTreeMap<u32, [u8; 32]>,
}
pub fn single<'a>(root: &'a Element, name: &str, ns: &str) -> Result<&'a Element> {
    let mut children = root.children().filter(|c| c.is(name, ns));
    let result = children.next().ok_or(Error::Invalid("missing element"))?;
    if children.next().is_some() {
        return Err(Error::Invalid("duplicate element"));
    }
    Ok(result)
}
pub fn xml(root: &Element, limit: usize) -> Result<Vec<u8>> {
    struct Bounded {
        bytes: Vec<u8>,
        limit: usize,
    }
    impl Write for Bounded {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if bytes.len() > self.limit.saturating_sub(self.bytes.len()) {
                return Err(std::io::ErrorKind::FileTooLarge.into());
            }
            self.bytes.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut writer = Bounded {
        bytes: Vec::new(),
        limit,
    };
    root.write_to(&mut writer)
        .map_err(|_| Error::Invalid("XML limit"))?;
    Ok(writer.bytes)
}
pub fn decode(text: &str, limit: usize) -> Result<Vec<u8>> {
    if text.len() > limit.saturating_mul(2).max(128) {
        return Err(Error::Invalid("base64 limit"));
    }
    let text: Vec<_> = text
        .bytes()
        .filter(|b| !matches!(b, b' ' | b'\t' | b'\r' | b'\n'))
        .collect();
    let value = STANDARD
        .decode(text)
        .map_err(|_| Error::Invalid("base64"))?;
    if value.len() > limit {
        return Err(Error::Invalid("decoded limit"));
    }
    Ok(value)
}
fn bytes<const N: usize>(root: &Element) -> Result<[u8; N]> {
    if root.children().next().is_some() {
        return Err(Error::Invalid("text element"));
    }
    decode(&root.text(), N)?
        .try_into()
        .map_err(|_| Error::Invalid("key length"))
}
pub fn uint(value: Option<&str>) -> Result<u32> {
    let value = value.ok_or(Error::Invalid("missing ID"))?;
    if value.is_empty() || value.len() > 10 || !value.bytes().all(|b| b.is_ascii_digit()) {
        return Err(Error::Invalid("ID syntax"));
    }
    value.parse().map_err(|_| Error::Invalid("ID range"))
}
pub fn positive(value: Option<&str>) -> Result<u32> {
    let id = uint(value)?;
    if id == 0 || id > i32::MAX as u32 {
        Err(Error::Invalid("device ID"))
    } else {
        Ok(id)
    }
}
fn point(key: &[u8; 32]) -> Result<()> {
    let scalar = PKey::private_key_from_raw_bytes(&[1; 32], Id::X25519)?;
    let peer = PKey::public_key_from_raw_bytes(key, Id::X25519)?;
    let mut derive = Deriver::new(&scalar)?;
    derive.set_peer(&peer)?;
    let shared = Secret::new(derive.derive_to_vec()?);
    if shared.iter().all(|b| *b == 0) {
        return Err(Error::Invalid("low-order key"));
    }
    Ok(())
}
impl Bundle {
    pub fn validate(&self) -> Result<()> {
        let point = CompressedEdwardsY(self.identity)
            .decompress()
            .ok_or(Error::Invalid("identity point"))?;
        if point.is_identity()
            || !point.is_torsion_free()
            || point.compress().to_bytes() != self.identity
        {
            return Err(Error::Invalid("identity subgroup"));
        }
        let key = PKey::public_key_from_raw_bytes(&self.identity, Id::ED25519)?;
        if !Verifier::new_without_digest(&key)?.verify_oneshot(&self.signature, &self.signed)? {
            return Err(Error::Invalid("signed prekey signature"));
        }
        point_key(&self.signed)?;
        if self.prekeys.is_empty() || self.prekeys.len() > 1000 {
            return Err(Error::Invalid("prekey count"));
        }
        for key in self.prekeys.values() {
            point_key(key)?;
        }
        Ok(())
    }
    pub fn parse(root: &Element) -> Result<Self> {
        if !root.is("bundle", NS) {
            return Err(Error::Invalid("bundle namespace"));
        }
        xml(root, 256 * 1024)?;
        let signed = single(root, "spk", NS)?;
        let keys = single(root, "prekeys", NS)?;
        let mut prekeys = BTreeMap::new();
        let mut count = 0;
        for key in keys.children().filter(|c| c.is("pk", NS)) {
            count += 1;
            if count > 1000 {
                return Err(Error::Invalid("prekey count"));
            }
            let Ok(id) = uint(key.attr("id")) else {
                continue;
            };
            if prekeys.insert(id, bytes(key)?).is_some() {
                return Err(Error::Invalid("duplicate prekey"));
            }
        }
        let bundle = Self {
            identity: bytes(single(root, "ik", NS)?)?,
            signed_id: uint(signed.attr("id"))?,
            signed: bytes(signed)?,
            signature: bytes(single(root, "spks", NS)?)?,
            prekeys,
        };
        bundle.validate()?;
        Ok(bundle)
    }
    pub fn xml(&self) -> Element {
        let mut keys = Element::builder("prekeys", NS).build();
        for (id, key) in &self.prekeys {
            keys.append_child(
                Element::builder("pk", NS)
                    .attr("id".try_into().unwrap(), id.to_string())
                    .append(STANDARD.encode(key))
                    .build(),
            );
        }
        Element::builder("bundle", NS)
            .append(
                Element::builder("spk", NS)
                    .attr("id".try_into().unwrap(), self.signed_id.to_string())
                    .append(STANDARD.encode(self.signed))
                    .build(),
            )
            .append(
                Element::builder("spks", NS)
                    .append(STANDARD.encode(self.signature))
                    .build(),
            )
            .append(
                Element::builder("ik", NS)
                    .append(STANDARD.encode(self.identity))
                    .build(),
            )
            .append(keys)
            .build()
    }
}
fn point_key(key: &[u8; 32]) -> Result<()> {
    point(key)
}
