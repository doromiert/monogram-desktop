// SPDX-License-Identifier: AGPL-3.0-only
// Adapted from Monogram's Monocles PQ profile adapter; see native/NOTICE.md.
use crate::crypto::{Error, Result};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use openssl::hash::{Hasher, MessageDigest};
use pq_signal::{IdentityKey, PqIdentityKey, PqSignature, PreKeyBundle, PublicKey};
use std::{collections::BTreeMap, ops::Deref};
use tokio_xmpp::minidom::Element;

pub use crate::crypto::bundle::{decode, document, positive, single, uint, xml};
pub const NS: &str = "urn:monocles:omemo-pq:1";
pub const MAX: usize = 512 * 1024;

#[derive(Clone, Debug)]
pub struct KemKey {
    pub id: u32,
    pub public: Vec<u8>,
    pub signature: [u8; 64],
}

#[derive(Clone, Debug)]
pub struct PublicBundle {
    pub identity: [u8; 33],
    pub pq_identity: Vec<u8>,
    pub pq_signature: Vec<u8>,
    pub classic_signature: [u8; 64],
    pub signed_id: u32,
    pub signed: [u8; 33],
    pub signed_signature: [u8; 64],
    pub prekeys: BTreeMap<u32, [u8; 33]>,
    pub last_resort: KemKey,
    pub kem_prekeys: BTreeMap<u32, KemKey>,
}

#[derive(Clone, Debug)]
pub struct Bundle {
    pub public: PublicBundle,
    pub kem_binding: [u8; 64],
    // Monocles signs the manifest in XML document order, not numeric key order.
    kem_order: Vec<u32>,
}
impl Deref for Bundle {
    type Target = PublicBundle;
    fn deref(&self) -> &PublicBundle {
        &self.public
    }
}
fn invalid() -> Error {
    Error::Invalid("PQ bundle")
}
fn signal<T, E>(value: std::result::Result<T, E>) -> Result<T> {
    value.map_err(|_| invalid())
}
pub fn optional<'a>(root: &'a Element, name: &str) -> Result<Option<&'a Element>> {
    let mut children = root.children().filter(|child| child.is(name, NS));
    let result = children.next();
    if children.next().is_some() {
        return Err(invalid());
    }
    Ok(result)
}
fn data<const N: usize>(root: &Element) -> Result<[u8; N]> {
    if root.children().next().is_some() {
        return Err(invalid());
    }
    decode(&root.text(), N)?.try_into().map_err(|_| invalid())
}
fn ec(root: &Element) -> Result<[u8; 33]> {
    let raw = data::<32>(root)?;
    let mut scalar = [0; 32];
    scalar[0] = 1;
    if curve25519_dalek::montgomery::MontgomeryPoint(raw)
        .mul_clamped(scalar)
        .0
        == [0; 32]
    {
        return Err(invalid());
    }
    let mut serialized = [5; 33];
    serialized[1..].copy_from_slice(&raw);
    signal(PublicKey::deserialize(&serialized))?;
    Ok(serialized)
}
fn kem(public: Vec<u8>, id: u32, signature: [u8; 64]) -> Result<KemKey> {
    if public.len() != 1569 || public[0] != 10 {
        return Err(invalid());
    }
    signal(pq_signal::kem::PublicKey::deserialize(&public))?;
    Ok(KemKey {
        id,
        public,
        signature,
    })
}
fn binding(bundle: &PublicBundle) -> Result<[u8; 64]> {
    binding_ordered(
        bundle,
        &bundle.kem_prekeys.keys().copied().collect::<Vec<_>>(),
    )
}
fn binding_ordered(bundle: &PublicBundle, order: &[u32]) -> Result<[u8; 64]> {
    if bundle.prekeys.is_empty()
        || bundle.prekeys.len() > 100
        || bundle.kem_prekeys.len() > 100
        || bundle.pq_identity.len() != 2592
        || bundle.last_resort.public.len() != 1569
        || bundle.kem_prekeys.contains_key(&bundle.last_resort.id)
        || order.len() != bundle.kem_prekeys.len()
        || order
            .iter()
            .copied()
            .collect::<std::collections::BTreeSet<_>>()
            .len()
            != order.len()
    {
        return Err(invalid());
    }
    let keys = order
        .iter()
        .map(|id| {
            let key = bundle.kem_prekeys.get(id).ok_or_else(invalid)?;
            if key.id != *id {
                return Err(invalid());
            }
            Ok(((*id).into(), key.public.as_slice()))
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(pq_signal::pq_kem_binding(
        bundle.last_resort.id.into(),
        &bundle.last_resort.public,
        &keys,
    ))
}
pub fn transcript(bundle: &PublicBundle) -> Result<Vec<u8>> {
    Ok(pq_signal::pq_bundle_transcript(
        &signal(IdentityKey::decode(&bundle.identity))?,
        &signal(PqIdentityKey::deserialize(&bundle.pq_identity))?,
        bundle.signed_id.into(),
        &signal(PublicKey::deserialize(&bundle.signed))?,
        &binding(bundle)?,
    ))
}
pub fn fingerprint(classic: &[u8], pq: &[u8]) -> Result<String> {
    if classic.len() != 33 || pq.len() != 2592 {
        return Err(invalid());
    }
    let mut hash = Hasher::new(MessageDigest::sha3_512())?;
    hash.update(b"monocles:omemo2:ik:v2")?;
    for field in [classic, pq] {
        hash.update(&(field.len() as u32).to_be_bytes())?;
        hash.update(field)?;
    }
    Ok(crate::crypto::lifecycle::hex(&hash.finish()?))
}
impl Bundle {
    pub fn from_parts(public: PublicBundle) -> Result<Self> {
        let kem_binding = binding(&public)?;
        let bundle = Self {
            kem_order: public.kem_prekeys.keys().copied().collect(),
            public,
            kem_binding,
        };
        bundle.validate()?;
        Ok(bundle)
    }
    pub fn parse(root: &Element) -> Result<Self> {
        if !root.is("bundle", NS) {
            return Err(invalid());
        }
        xml(root, MAX)?;
        let signed = single(root, "spk", NS)?;
        let pq = single(root, "pq-ik", NS)?;
        if pq.attr("type") != Some("ML-DSA-87") {
            return Err(invalid());
        }
        let mut prekeys = BTreeMap::new();
        for key in single(root, "prekeys", NS)?.children() {
            if !key.is("pk", NS)
                || prekeys.len() >= 100
                || prekeys.insert(uint(key.attr("id"))?, ec(key)?).is_some()
            {
                return Err(invalid());
            }
        }
        let last = single(root, "kem-spk", NS)?;
        let last_resort = kem(
            data::<1569>(last)?.to_vec(),
            uint(last.attr("id"))?,
            data(single(root, "kem-spks", NS)?)?,
        )?;
        let mut kem_prekeys = BTreeMap::new();
        let mut kem_order = Vec::new();
        if let Some(keys) = optional(root, "kem-prekeys")? {
            for key in keys.children() {
                if !key.is("kem-pk", NS) || kem_prekeys.len() >= 100 {
                    return Err(invalid());
                }
                let id = uint(key.attr("id"))?;
                let signature = decode(key.attr("sig").ok_or_else(invalid)?, 64)?
                    .try_into()
                    .map_err(|_| invalid())?;
                if kem_prekeys
                    .insert(id, kem(data::<1569>(key)?.to_vec(), id, signature)?)
                    .is_some()
                {
                    return Err(invalid());
                }
                kem_order.push(id);
            }
        }
        let public = PublicBundle {
            identity: ec(single(root, "ik", NS)?)?,
            pq_identity: data::<2592>(pq)?.to_vec(),
            pq_signature: data::<4627>(single(root, "pq-sig", NS)?)?.to_vec(),
            classic_signature: data(single(root, "ik-sig", NS)?)?,
            signed_id: uint(signed.attr("id"))?,
            signed: ec(signed)?,
            signed_signature: data(single(root, "spks", NS)?)?,
            prekeys,
            last_resort,
            kem_prekeys,
        };
        let kem_binding = binding_ordered(&public, &kem_order)?;
        let bundle = Self {
            public,
            kem_binding,
            kem_order,
        };
        bundle.validate()?;
        Ok(bundle)
    }
    pub fn validate(&self) -> Result<()> {
        let binding = binding_ordered(self, &self.kem_order)?;
        if binding != self.kem_binding {
            return Err(invalid());
        }
        let identity = signal(IdentityKey::decode(&self.identity))?;
        if !identity
            .public_key()
            .verify_signature(&self.signed, &self.signed_signature)
        {
            return Err(invalid());
        }
        for key in std::iter::once(&self.last_resort).chain(self.kem_prekeys.values()) {
            if key.public.len() != 1569
                || key.public[0] != 10
                || !identity
                    .public_key()
                    .verify_signature(&key.public, &key.signature)
            {
                return Err(invalid());
            }
            signal(pq_signal::kem::PublicKey::deserialize(&key.public))?;
        }
        if !pq_signal::pq_bundle_verify(
            &identity,
            &signal(PqIdentityKey::deserialize(&self.pq_identity))?,
            &signal(PqSignature::deserialize(&self.pq_signature))?,
            &self.classic_signature,
            self.signed_id.into(),
            &signal(PublicKey::deserialize(&self.signed))?,
            &binding,
        ) {
            return Err(invalid());
        }
        Ok(())
    }
    pub fn fingerprint(&self) -> Result<String> {
        fingerprint(&self.identity, &self.pq_identity)
    }
    pub fn session_bundle(&self, device: u32) -> Result<PreKeyBundle> {
        self.validate()?;
        crate::crypto::address("peer@example.invalid", device)?;
        fn choose(len: usize) -> Result<usize> {
            let bound = (u64::from(u32::MAX) + 1) / len as u64 * len as u64;
            loop {
                let n = u64::from(u32::from_ne_bytes(crate::crypto::random::<4>()?));
                if n < bound {
                    return Ok((n % len as u64) as usize);
                }
            }
        }
        let (ec_id, ec) = self
            .prekeys
            .iter()
            .nth(choose(self.prekeys.len())?)
            .ok_or_else(invalid)?;
        let kem = if self.kem_prekeys.is_empty() {
            &self.last_resort
        } else {
            self.kem_prekeys
                .values()
                .nth(choose(self.kem_prekeys.len())?)
                .ok_or_else(invalid)?
        };
        Ok(signal(PreKeyBundle::new(
            0,
            device.try_into().map_err(|_| invalid())?,
            Some(((*ec_id).into(), signal(PublicKey::deserialize(ec))?)),
            self.signed_id.into(),
            signal(PublicKey::deserialize(&self.signed))?,
            self.signed_signature.to_vec(),
            kem.id.into(),
            signal(pq_signal::kem::PublicKey::deserialize(&kem.public))?,
            kem.signature.to_vec(),
            signal(IdentityKey::decode(&self.identity))?,
        ))?
        .with_pq_identity(
            signal(PqIdentityKey::deserialize(&self.pq_identity))?,
            signal(PqSignature::deserialize(&self.pq_signature))?,
            self.classic_signature.to_vec(),
            self.kem_binding.to_vec(),
        ))
    }
    pub fn xml(&self) -> Element {
        fn binary(name: &str, bytes: &[u8]) -> Element {
            Element::builder(name, NS)
                .append(STANDARD.encode(bytes))
                .build()
        }
        fn key(name: &str, id: u32, bytes: &[u8]) -> Element {
            Element::builder(name, NS)
                .attr("id".try_into().unwrap(), id.to_string())
                .append(STANDARD.encode(bytes))
                .build()
        }
        let mut root = Element::builder("bundle", NS)
            .attr("rev".try_into().unwrap(), "4")
            .append(key("spk", self.signed_id, &self.signed[1..]))
            .append(binary("spks", &self.signed_signature))
            .append(binary("ik", &self.identity[1..]))
            .build();
        let mut prekeys = Element::builder("prekeys", NS).build();
        for (id, public) in &self.prekeys {
            prekeys.append_child(key("pk", *id, &public[1..]));
        }
        root.append_child(prekeys);
        root.append_child(key(
            "kem-spk",
            self.last_resort.id,
            &self.last_resort.public,
        ));
        root.append_child(binary("kem-spks", &self.last_resort.signature));
        let mut keys = Element::builder("kem-prekeys", NS).build();
        for id in &self.kem_order {
            let value = &self.kem_prekeys[id];
            let mut element = key("kem-pk", *id, &value.public);
            element.set_attr(
                "".into(),
                "sig".try_into().unwrap(),
                STANDARD.encode(value.signature),
            );
            keys.append_child(element);
        }
        root.append_child(keys);
        root.append_child(
            Element::builder("pq-ik", NS)
                .attr("type".try_into().unwrap(), "ML-DSA-87")
                .append(STANDARD.encode(&self.pq_identity))
                .build(),
        );
        root.append_child(binary("pq-sig", &self.pq_signature));
        root.append_child(binary("ik-sig", &self.classic_signature));
        root
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn monocles_hybrid_fingerprint_vector() {
        let mut classic = [0x11; 33];
        classic[0] = 5;
        assert_eq!(
            fingerprint(&classic, &[0x22; 2592]).unwrap(),
            "6b6ea370b7cbc0078f992487b235ab384a7f272b232e508a2d27da9b42f1def7f2def0daffbdfd91a33065c1e383473a1eacce6e5709833d286c5e399e19c77a"
        );
    }
    #[test]
    fn signed_kem_manifest_preserves_document_order() {
        let mut device =
            super::super::device::Device::memory("fixture@example.invalid", 71).unwrap();
        let mut public = device.prepare().unwrap();
        let order: Vec<_> = public.kem_prekeys.keys().rev().copied().collect();
        let kem_binding = binding_ordered(&public, &order).unwrap();
        let transcript = pq_signal::pq_bundle_transcript(
            &IdentityKey::decode(&public.identity).unwrap(),
            &PqIdentityKey::deserialize(&public.pq_identity).unwrap(),
            public.signed_id.into(),
            &PublicKey::deserialize(&public.signed).unwrap(),
            &kem_binding,
        );
        let tx = device.begin().unwrap();
        let (_, pq_signature, classic_signature) =
            super::super::native::sign_auth(&tx, &transcript).unwrap();
        public.pq_signature = pq_signature;
        public.classic_signature = classic_signature;
        let bundle = Bundle {
            public,
            kem_binding,
            kem_order: order.clone(),
        };
        bundle.validate().unwrap();
        let parsed = Bundle::parse(&bundle.xml()).unwrap();
        assert_eq!(parsed.kem_order, order);
        assert_eq!(parsed.kem_binding, kem_binding);
        assert!(
            Bundle::from_parts(parsed.public).is_err(),
            "sorting a signed remote manifest must invalidate its signature"
        );
    }
}
