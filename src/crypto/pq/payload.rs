// SPDX-License-Identifier: AGPL-3.0-only
// Monocles payload v3, matching Monogram mobile PqOmemoPayload.
use crate::crypto::{Error, Result, Secret, bundle, payload as sce};
use openssl::{
    memcmp,
    symm::{Cipher, Crypter, Mode},
};
use tiny_keccak::{Hasher, Kmac};
use tokio_xmpp::minidom::Element;
pub const MAX: usize = 256 * 1024;
pub const CLIENT: &str = sce::CLIENT;
pub const SCE: &str = sce::SCE;
pub use sce::{allowed, routing};

fn invalid() -> Error {
    Error::Invalid("PQ payload")
}
pub fn binding(from: &str, to: &str, device: u32) -> Result<Vec<u8>> {
    crate::crypto::address(from, device)?;
    crate::crypto::address(to, 1)?;
    let mut out = b"OMEMO2\0".to_vec();
    out.extend(from.as_bytes());
    out.push(0);
    out.extend(to.as_bytes());
    out.push(0);
    out.extend(device.to_be_bytes());
    Ok(out)
}
fn kmac(key: &[u8], data: &[u8], customization: &[u8], length: usize) -> Secret {
    let mut mac = Kmac::v256(key, customization);
    mac.update(data);
    let mut out = Secret::new(vec![0; length]);
    mac.finalize(&mut out);
    out
}
pub fn commitment(key: &[u8], binding: &[u8]) -> Secret {
    kmac(key, binding, b"monocles:omemo2:key-commitment:v3", 64)
}
pub fn seal(plain: &[u8], key: &[u8], binding: &[u8]) -> Result<Vec<u8>> {
    if plain.len() > MAX || key.len() != 32 {
        return Err(invalid());
    }
    let derived = kmac(key, binding, b"monocles:omemo2:payload:v3", 44);
    let mut cipher = Crypter::new(
        Cipher::aes_256_gcm(),
        Mode::Encrypt,
        &derived[..32],
        Some(&derived[32..]),
    )?;
    cipher.aad_update(binding)?;
    let mut out = vec![0; plain.len() + 32];
    let mut count = cipher.update(plain, &mut out)?;
    count += cipher.finalize(&mut out[count..])?;
    out.truncate(count);
    let mut tag = [0; 16];
    cipher.get_tag(&mut tag)?;
    out.extend(tag);
    Ok(out)
}
pub fn open(ciphertext: &[u8], key: &[u8], binding: &[u8], committed: &[u8]) -> Result<Secret> {
    if ciphertext.len() < 16
        || ciphertext.len() > MAX + 16
        || key.len() != 32
        || committed.len() != 64
        || !memcmp::eq(&commitment(key, binding), committed)
    {
        return Err(invalid());
    }
    let derived = kmac(key, binding, b"monocles:omemo2:payload:v3", 44);
    let mut cipher = Crypter::new(
        Cipher::aes_256_gcm(),
        Mode::Decrypt,
        &derived[..32],
        Some(&derived[32..]),
    )?;
    cipher.aad_update(binding)?;
    let n = ciphertext.len() - 16;
    cipher.set_tag(&ciphertext[n..])?;
    let mut out = Secret::new(vec![0; n + 16]);
    let mut count = cipher.update(&ciphertext[..n], &mut out)?;
    count += cipher.finalize(&mut out[count..])?;
    out.truncate(count);
    Ok(out)
}
pub fn envelope(message: &Element) -> Result<Secret> {
    let (from, to) = routing(message)?;
    crate::polls::Poll::validate_protected(message).map_err(|_| invalid())?;
    let mut content = Element::builder("content", SCE).build();
    for child in message.children() {
        if !allowed(child) {
            return Err(invalid());
        }
        content.append_child(child.clone());
    }
    let base = Element::builder("envelope", SCE)
        .append(content)
        .append(
            Element::builder("from", SCE)
                .attr("jid".try_into().unwrap(), from)
                .build(),
        )
        .append(
            Element::builder("to", SCE)
                .attr("jid".try_into().unwrap(), to)
                .build(),
        )
        .append(
            Element::builder("time", SCE)
                .attr("stamp".try_into().unwrap(), chrono::Utc::now().to_rfc3339())
                .build(),
        )
        .build();
    let mut measured = base.clone();
    measured.append_child(Element::builder("rpad", SCE).append("A").build());
    let baseline = bundle::xml(&measured, MAX)?.len() - 1;
    let target = padded_length(baseline)?;
    let mut random = vec![0; target - baseline];
    openssl::rand::rand_bytes(&mut random)?;
    let alphabet = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let pad: String = random
        .iter()
        .map(|b| alphabet[(b & 63) as usize] as char)
        .collect();
    let mut root = base;
    root.append_child(Element::builder("rpad", SCE).append(pad).build());
    Ok(Secret::new(bundle::xml(&root, MAX)?))
}
fn padded_length(unpadded: usize) -> Result<usize> {
    let x = unpadded.checked_add(1).ok_or_else(invalid)?;
    let e = usize::BITS - 1 - x.leading_zeros();
    let s = if e == 0 {
        0
    } else {
        u32::BITS - e.leading_zeros()
    };
    let shift = e.saturating_sub(s);
    let mask = (1usize << shift) - 1;
    let padded = ((x + mask) & !mask).div_ceil(256) * 256;
    if padded > MAX {
        return Err(invalid());
    }
    Ok(padded)
}
pub fn content(outer: &Element, plain: &[u8], timestamp: i64) -> Result<Element> {
    let text = std::str::from_utf8(plain).map_err(|_| invalid())?;
    let root = bundle::document(text, MAX)?;
    for name in ["from", "to", "time"] {
        bundle::single(&root, name, SCE)?;
    }
    sce::content(outer, plain, timestamp)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn monocles_commitment_vector_and_bound_payload() {
        let key = [7; 32];
        let binding = binding("alice@example.com", "bob@example.com", 42).unwrap();
        assert_eq!(
            crate::crypto::lifecycle::hex(&commitment(&key, &binding)),
            "e04e685382db88563a43d2a5d55218bf917b5b57989b377636d88cf7f479bfc531b5a1a87a4eeef2909d8510a27f0b83e9f183361686ad5a3b00194794bde224"
        );
        let plain = b"PQ payload";
        let encrypted = seal(plain, &key, &binding).unwrap();
        assert_eq!(
            &*open(&encrypted, &key, &binding, &commitment(&key, &binding)).unwrap(),
            plain
        );
        let mut changed = binding;
        changed[8] ^= 1;
        assert!(open(&encrypted, &key, &changed, &commitment(&key, &changed)).is_err());
    }
}
