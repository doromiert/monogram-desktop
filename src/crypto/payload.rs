//! XEP-0384 HKDF/AES/HMAC and XEP-0420 authenticated XML content, matching mobile.
use super::{
    bundle::{single, xml},
    *,
};
use openssl::{
    hash::MessageDigest,
    memcmp,
    pkey::{Id, PKey},
    pkey_ctx::PkeyCtx,
    sign::Signer,
    symm::{Cipher, decrypt, encrypt},
};
use std::collections::HashMap;
use tokio_xmpp::{jid::Jid, minidom::Element};
pub const SCE: &str = "urn:xmpp:sce:1";
pub const CLIENT: &str = "jabber:client";
pub const MAX: usize = 256 * 1024;
pub struct Sealed {
    pub ciphertext: Vec<u8>,
    pub material: Secret,
}
fn derive(key: &[u8]) -> Result<zeroize::Zeroizing<[u8; 80]>> {
    let mut ctx = PkeyCtx::new_id(Id::HKDF)?;
    ctx.derive_init()?;
    ctx.set_hkdf_md(openssl::md::Md::sha256())?;
    ctx.set_hkdf_salt(&[0; 32])?;
    ctx.set_hkdf_key(key)?;
    ctx.add_hkdf_info(b"OMEMO Payload")?;
    let mut out = zeroize::Zeroizing::new([0; 80]);
    ctx.derive(Some(out.as_mut()))?;
    Ok(out)
}
fn mac(expanded: &[u8], ciphertext: &[u8]) -> Result<Secret> {
    let key = PKey::hmac(&expanded[32..64])?;
    let mut signer = Signer::new(MessageDigest::sha256(), &key)?;
    signer.update(ciphertext)?;
    Ok(Secret::new(signer.sign_to_vec()?))
}
pub fn seal(plain: &[u8]) -> Result<Sealed> {
    let key = MasterKey::new(random::<32>()?);
    seal_with_key(plain, &key)
}
fn seal_with_key(plain: &[u8], key: &[u8; 32]) -> Result<Sealed> {
    if plain.len() > MAX {
        return Err(Error::Invalid("payload limit"));
    }
    let expanded = derive(key)?;
    let ciphertext = encrypt(
        Cipher::aes_256_cbc(),
        &expanded[..32],
        Some(&expanded[64..]),
        plain,
    )?;
    let tag = mac(&*expanded, &ciphertext)?;
    let mut material = Secret::new(Vec::with_capacity(48));
    material.extend_from_slice(key);
    material.extend_from_slice(&tag[..16]);
    Ok(Sealed {
        ciphertext,
        material,
    })
}
pub fn open(ciphertext: &[u8], material: &[u8]) -> Result<Secret> {
    if material.len() != 48
        || ciphertext.is_empty()
        || ciphertext.len() > MAX + 16
        || ciphertext.len() % 16 != 0
    {
        return Err(Error::Invalid("payload shape"));
    }
    let expanded = derive(&material[..32])?;
    let tag = mac(&*expanded, ciphertext)?;
    if !memcmp::eq(&tag[..16], &material[32..]) {
        return Err(Error::Invalid("payload authentication"));
    }
    Ok(Secret::new(decrypt(
        Cipher::aes_256_cbc(),
        &expanded[..32],
        Some(&expanded[64..]),
        ciphertext,
    )?))
}
fn bare(value: Option<&str>) -> Result<String> {
    let jid = value
        .ok_or(Error::Invalid("missing routing"))?
        .parse::<Jid>()
        .map_err(|_| Error::Invalid("routing"))?;
    if jid.node().is_none() {
        return Err(Error::Invalid("entity routing"));
    }
    Ok(jid.to_bare().to_string())
}
pub fn routing(message: &Element) -> Result<(String, String)> {
    if !message.is("message", CLIENT)
        || !matches!(
            message.attr("type"),
            None | Some("chat" | "normal" | "groupchat")
        )
        || message
            .get_child("x", "http://jabber.org/protocol/muc#user")
            .is_some()
    {
        return Err(Error::Invalid("message routing"));
    }
    Ok((bare(message.attr("from"))?, bare(message.attr("to"))?))
}
pub fn allowed(child: &Element) -> bool {
    matches!(
        (child.name(), child.ns().as_str()),
        ("body" | "subject" | "thread", CLIENT)
            | ("file-sharing", "urn:xmpp:sfs:0")
            | ("sticker", "urn:xmpp:stickers:0")
            | ("html", "http://jabber.org/protocol/xhtml-im")
            | ("data", "urn:xmpp:bob")
            | ("x", "jabber:x:oob")
            | ("reply", "urn:xmpp:reply:0")
            | ("fallback", "urn:xmpp:fallback:0")
            | ("replace", "urn:xmpp:message-correct:0")
            | ("reactions", "urn:xmpp:reactions:0")
            | ("poll", "urn:xmpp:polls:0")
            | ("forwarded", "urn:xmpp:forward:0")
            | ("reference", "urn:xmpp:reference:0")
            | ("content", "urn:xmpp:content")
            | ("markup", "urn:xmpp:markup:0")
            | ("spoiler", "urn:xmpp:spoiler:0")
            | ("checklist" | "checklist-update", "urn:monogram:checklist:1")
            | ("unstyled", "urn:xmpp:styling:0")
            | ("retract", "urn:xmpp:message-retract:1")
            | ("apply-to", "urn:xmpp:fasten:0")
    )
}
fn wrap(content: Element, from: &str, to: &str, padding: &str) -> Element {
    Element::builder("envelope", SCE)
        .append(content)
        .append(Element::builder("rpad", SCE).append(padding).build())
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
        .build()
}
pub fn envelope(message: &Element) -> Result<Secret> {
    let (from, to) = routing(message)?;
    crate::polls::Poll::validate_protected(message)
        .map_err(|_| Error::Invalid("structured poll"))?;
    let mut content = Element::builder("content", SCE).build();
    for child in message.children() {
        if !allowed(child) {
            return Err(Error::Invalid("unsupported protected content"));
        }
        content.append_child(child.clone());
    }
    let baseline = xml(&wrap(content.clone(), &from, &to, ""), MAX)?.len();
    let extra = loop {
        let b = random::<1>()?[0];
        if b <= 200 {
            break b as usize;
        }
    };
    let count = 512usize.saturating_sub(baseline) + extra;
    let mut bytes = vec![0; count];
    openssl::rand::rand_bytes(&mut bytes)?;
    let alphabet = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let padding: String = bytes
        .iter()
        .map(|b| alphabet[(*b & 63) as usize] as char)
        .collect();
    Ok(Secret::new(xml(&wrap(content, &from, &to, &padding), MAX)?))
}
pub fn content(outer: &Element, plain: &[u8], sent_time: i64) -> Result<Element> {
    let (from, to) = routing(outer)?;
    if plain.len() > MAX {
        return Err(Error::Invalid("content limit"));
    }
    let text = std::str::from_utf8(plain).map_err(|_| Error::Invalid("UTF-8"))?;
    if text.contains("<!") {
        return Err(Error::Invalid("XML declarations"));
    }
    let root = super::bundle::document(text, MAX)?;
    if !root.is("envelope", SCE) {
        return Err(Error::Invalid("SCE namespace"));
    }
    let inner = single(&root, "content", SCE)?;
    single(&root, "rpad", SCE)?;
    if outer.attr("type") == Some("groupchat") {
        single(&root, "to", SCE)?;
    }
    for affix in root.children() {
        if affix.ns() != SCE {
            return Err(Error::Invalid("SCE affix namespace"));
        }
        match affix.name() {
            "from" | "to" => {
                single(&root, affix.name(), SCE)?;
                let value = affix.attr("jid").ok_or(Error::Invalid("affix routing"))?;
                if crate::protocol::bare(value).ok().as_deref() != Some(value)
                    || value != if affix.name() == "from" { &from } else { &to }
                {
                    return Err(Error::Invalid("affix mismatch"));
                }
            }
            "time" => {
                single(&root, "time", SCE)?;
                let stamp = affix.attr("stamp").ok_or(Error::Invalid("time"))?;
                if stamp.len() > 100 {
                    return Err(Error::Invalid("time"));
                }
                let time = chrono::DateTime::parse_from_rfc3339(stamp)
                    .map_err(|_| Error::Invalid("time"))?
                    .timestamp_millis();
                if sent_time < 0 || time.abs_diff(sent_time) > 600000 {
                    return Err(Error::Invalid("time mismatch"));
                }
            }
            "content" | "rpad" => {}
            _ => return Err(Error::Invalid("unknown SCE affix")),
        }
    }
    let mut message = Element::builder("message", CLIENT).build();
    for name in ["from", "to", "type", "id"] {
        if let Some(value) = outer.attr(name) {
            message.set_attr("".into(), name.try_into().unwrap(), value);
        }
    }
    let mut texts = HashMap::new();
    for child in inner.children() {
        if !allowed(child) {
            return Err(Error::Invalid("unsupported protected extension"));
        }
        if child.is("body", CLIENT) || child.is("subject", CLIENT) || child.is("thread", CLIENT) {
            let language = child
                .attr_ns("http://www.w3.org/XML/1998/namespace", "lang")
                .unwrap_or("");
            if child.children().next().is_some() {
                return Err(Error::Invalid("nested text"));
            }
            let key = (child.name().to_string(), language.to_string());
            if let Some(previous) = texts.insert(key, child) {
                // Older mobile encoders copied typed text again from Smack's extension list.
                if previous == child {
                    continue;
                }
                return Err(Error::Invalid("conflicting text"));
            }
        }
        message.append_child(child.clone());
    }
    Ok(message)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn actual_mobile_payload_vectors() {
        use base64::{Engine as _, engine::general_purpose::STANDARD};
        let vectors: Vec<serde_json::Value> = serde_json::from_str(include_str!(
            "../../tests/mobile-reference/payload-vectors.json"
        ))
        .unwrap();
        for vector in vectors {
            let plain = STANDARD
                .decode(vector["plaintext"].as_str().unwrap())
                .unwrap();
            let material = STANDARD
                .decode(vector["material"].as_str().unwrap())
                .unwrap();
            let ciphertext = STANDARD
                .decode(vector["ciphertext"].as_str().unwrap())
                .unwrap();
            let sealed = seal_with_key(&plain, material[..32].try_into().unwrap()).unwrap();
            assert_eq!(sealed.material.as_slice(), material);
            assert_eq!(sealed.ciphertext, ciphertext);
            assert_eq!(open(&ciphertext, &material).unwrap().as_slice(), plain);
            if plain.starts_with(b"<envelope") {
                let outer:Element="<message xmlns='jabber:client' from='alice@example.invalid/mobile' to='bob@example.invalid' type='chat'/>".parse().unwrap();
                let message = content(&outer, &plain, 0).unwrap();
                assert_eq!(
                    message.get_child("body", CLIENT).unwrap().text(),
                    "Authenticated 🌿"
                );
                assert!(message.get_child("reply", "urn:xmpp:reply:0").is_some());
            }
        }
    }
    #[test]
    fn authenticated_payload_and_sce_routing() {
        let message:Element="<message xmlns='jabber:client' from='a@example.org/r' to='b@example.org' type='chat' id='one'><body>Hi 🌿</body></message>".parse().unwrap();
        let plain = envelope(&message).unwrap();
        assert!(plain.len() >= 512);
        let sealed = seal(&plain).unwrap();
        assert_eq!(
            open(&sealed.ciphertext, &sealed.material)
                .unwrap()
                .as_slice(),
            plain.as_slice()
        );
        let decoded = content(&message, &plain, 0).unwrap();
        assert_eq!(decoded.get_child("body", CLIENT).unwrap().text(), "Hi 🌿");
        let mut wrong = message.clone();
        wrong.set_attr("".into(), "to".try_into().unwrap(), "c@example.org");
        assert!(content(&wrong, &plain, 0).is_err());
        let mut corrupt = sealed.ciphertext.clone();
        corrupt[0] ^= 1;
        assert!(open(&corrupt, &sealed.material).is_err());
        let mut material = sealed.material.clone();
        material[0] ^= 1;
        assert!(open(&sealed.ciphertext, &material).is_err());
    }
    #[test]
    fn rejects_unsafe_xml_and_duplicate_affixes() {
        let outer: Element =
            "<message xmlns='jabber:client' from='a@example.org' to='b@example.org'/>"
                .parse()
                .unwrap();
        for plain in [
            "<!DOCTYPE x><envelope xmlns='urn:xmpp:sce:1'/>",
            "<envelope xmlns='urn:xmpp:sce:1'><content/><rpad/><from jid='a@example.org'/><from jid='a@example.org'/></envelope>",
            "<envelope xmlns='urn:xmpp:sce:1'><content><evil xmlns='not-supported'/></content><rpad/></envelope>",
        ] {
            assert!(content(&outer, plain.as_bytes(), 0).is_err());
        }
    }
    #[test]
    fn historical_identical_text_is_coalesced_but_conflicts_are_rejected() {
        let outer: Element =
            "<message xmlns='jabber:client' from='a@example.org' to='b@example.org'/>"
                .parse()
                .unwrap();
        let text = "<body xmlns='jabber:client'>Text 😀</body><body xmlns='jabber:client' xml:lang='fr'>Bonjour</body><subject xmlns='jabber:client'>Subject</subject><thread xmlns='jabber:client'>thread-id</thread>";
        let envelope = |content: &str| {
            format!(
                "<envelope xmlns='urn:xmpp:sce:1'><content>{content}</content><rpad/></envelope>"
            )
        };
        let decoded = content(&outer, envelope(&format!("{text}{text}")).as_bytes(), 0).unwrap();
        assert_eq!(decoded.children().count(), 4);
        assert_eq!(decoded.get_child("body", CLIENT).unwrap().text(), "Text 😀");
        for duplicate in [
            "<body xmlns='jabber:client'>Conflicting text</body>",
            "<body xmlns='jabber:client' xml:lang='fr'>Different translation</body>",
            "<body xmlns='jabber:client' other='attribute'>Text 😀</body>",
            "<subject xmlns='jabber:client'>Other subject</subject>",
            "<thread xmlns='jabber:client'>other-thread</thread>",
            "<body xmlns='jabber:client'><nested/></body>",
        ] {
            assert!(
                content(
                    &outer,
                    envelope(&format!("{text}{duplicate}")).as_bytes(),
                    0
                )
                .is_err()
            );
        }
    }
    #[test]
    fn bounded_depth_and_complete_document() {
        let outer: Element =
            "<message xmlns='jabber:client' from='a@example.org' to='b@example.org'/>"
                .parse()
                .unwrap();
        let valid = "<envelope xmlns='urn:xmpp:sce:1'><content/><rpad/></envelope>";
        assert!(content(&outer, format!("{valid} \n\t").as_bytes(), 0).is_ok());
        for tail in ["trailing text", "<second xmlns='other'/>", "<broken"] {
            assert!(content(&outer, format!("{valid}{tail}").as_bytes(), 0).is_err());
        }
        let nested = format!(
            "<envelope xmlns='urn:xmpp:sce:1'><content>{}{}</content><rpad/></envelope>",
            "<x>".repeat(10000),
            "</x>".repeat(10000)
        );
        assert!(content(&outer, nested.as_bytes(), 0).is_err());
    }
}
