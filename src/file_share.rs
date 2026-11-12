//! Mobile FileLink/FileShare receive profile (GPL-3.0-or-later), XEP-0447/0454.
use base64::{Engine as _, engine::general_purpose::STANDARD};
use reqwest::Url;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use tokio_xmpp::minidom::Element;
pub const NS: &str = "urn:xmpp:sfs:0";
pub const META: &str = "urn:xmpp:file:metadata:0";
pub const HASH: &str = "urn:xmpp:hashes:2";
pub const OOB: &str = "jabber:x:oob";
pub const MAX: u64 = 4000 * 1024 * 1024;
const CLIENT: &str = "jabber:client";
const FALLBACK: &str = "urn:xmpp:fallback:0";
#[derive(Clone)]
pub struct Link {
    pub url: Url,
    pub iv: Vec<u8>,
    pub key: Vec<u8>,
}
impl Link {
    pub fn parse(wire: &str) -> Option<Self> {
        let encrypted = wire.starts_with("aesgcm://");
        if !encrypted && !wire.starts_with("https://")
            || wire.chars().any(|c| {
                c.is_control()
                    || c.is_whitespace()
                    || matches!(c, '\\' | '<' | '>' | '"' | '{' | '}' | '|' | '^' | '`')
            })
        {
            return None;
        }
        for (i, byte) in wire.bytes().enumerate() {
            if byte == b'%'
                && !wire
                    .as_bytes()
                    .get(i + 1..i + 3)
                    .is_some_and(|v| v.iter().all(u8::is_ascii_hexdigit))
            {
                return None;
            }
        }
        let authority = wire.split_once("://")?.1.split(['/', '?', '#']).next()?;
        if authority.contains('@') || !authority.is_ascii() {
            return None;
        }
        let address = if encrypted {
            format!("https{}", &wire[6..])
        } else {
            wire.into()
        };
        let mut url = Url::parse(&address).ok()?;
        if url.host_str().is_none() || !url.username().is_empty() || url.password().is_some() {
            return None;
        }
        let (iv, key) = if encrypted {
            let hex = url.fragment()?;
            if ![88, 96].contains(&hex.len()) || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
                return None;
            }
            let bytes: Vec<_> = (0..hex.len())
                .step_by(2)
                .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
                .collect();
            (
                bytes[..bytes.len() - 32].to_vec(),
                bytes[bytes.len() - 32..].to_vec(),
            )
        } else {
            if url.fragment().is_some() {
                return None;
            }
            (vec![], vec![])
        };
        url.set_fragment(None);
        Some(Self { url, iv, key })
    }
    pub fn encrypted(&self) -> bool {
        !self.key.is_empty()
    }
    pub fn name(&self) -> String {
        let name = self.url.path().rsplit('/').next().unwrap_or("");
        let decoded = percent_encoding::percent_decode_str(name).decode_utf8_lossy();
        let name = safe_name(&decoded);
        if name.is_empty() || name == "." || name == ".." {
            "attachment".into()
        } else {
            name.chars()
                .scan(0, |length, c| {
                    *length += c.len_utf16();
                    (*length <= 200).then_some(c)
                })
                .collect()
        }
    }
}
impl Drop for Link {
    fn drop(&mut self) {
        use zeroize::Zeroize;
        self.key.zeroize();
        self.iv.zeroize();
    }
}
fn safe_name(value: &str) -> String {
    value
        .chars()
        .map(|c| {
            if c.is_control() || matches!(c, '/' | '\\') {
                '_'
            } else {
                c
            }
        })
        .collect()
}
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Attachment {
    pub wire_url: String,
    pub name: String,
    pub mime: String,
    pub size: Option<u64>,
    #[serde(default)]
    pub width: Option<u32>,
    #[serde(default)]
    pub height: Option<u32>,
    pub hashes: BTreeMap<String, Vec<u8>>,
    pub manual: bool,
    pub sticker: bool,
}
// URLs can contain bearer tokens and file keys; never include them in diagnostics.
impl std::fmt::Debug for Attachment {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Attachment")
            .field("size", &self.size)
            .field("encrypted", &self.link().is_some_and(|l| l.encrypted()))
            .finish_non_exhaustive()
    }
}
fn only<'a>(root: &'a Element, name: &str, ns: &str) -> Option<&'a Element> {
    let mut values = root.children().filter(|c| c.is(name, ns));
    let value = values.next()?;
    if values.next().is_some() {
        None
    } else {
        Some(value)
    }
}
fn field(root: &Element, name: &str) -> Option<String> {
    let values: Vec<_> = root.children().filter(|c| c.is(name, META)).collect();
    match values.as_slice() {
        [] => Some(String::new()),
        [value] => Some(value.text()),
        _ => None,
    }
}
fn xml_size(root: &Element) -> usize {
    let mut bytes = Vec::new();
    if root.write_to(&mut bytes).is_err() {
        return usize::MAX;
    }
    std::str::from_utf8(&bytes)
        .map(|text| text.encode_utf16().count())
        .unwrap_or(usize::MAX)
}
impl Attachment {
    pub fn element(&self) -> Element {
        let mut metadata = Element::builder("file", META)
            .append(
                Element::builder("name", META)
                    .append(self.name.as_str())
                    .build(),
            )
            .append(
                Element::builder("media-type", META)
                    .append(self.mime.as_str())
                    .build(),
            )
            .build();
        if let Some(size) = self.size {
            metadata.append_child(
                Element::builder("size", META)
                    .append(size.to_string())
                    .build(),
            );
        }
        for (name, dimension) in [("width", self.width), ("height", self.height)] {
            if let Some(dimension) = dimension {
                metadata.append_child(
                    Element::builder(name, META)
                        .append(dimension.to_string())
                        .build(),
                );
            }
        }
        for (algorithm, hash) in &self.hashes {
            metadata.append_child(
                Element::builder("hash", HASH)
                    .attr("algo".try_into().unwrap(), algorithm)
                    .append(STANDARD.encode(hash))
                    .build(),
            );
        }
        Element::builder("file-sharing", NS)
            .attr(
                "disposition".try_into().unwrap(),
                if self.manual { "attachment" } else { "inline" },
            )
            .append(metadata)
            .append(
                Element::builder("sources", NS)
                    .append(
                        Element::builder("url-data", "http://jabber.org/protocol/url-data")
                            .attr("target".try_into().unwrap(), &self.wire_url)
                            .build(),
                    )
                    .build(),
            )
            .build()
    }
    pub fn valid_outgoing(&self, encrypted: bool) -> bool {
        self.link()
            .is_some_and(|link| link.encrypted() == encrypted)
            && Self::description(&self.element(), self.sticker).as_ref() == Some(self)
    }
    pub fn add_to(&self, message: &mut Element) {
        message.append_child(self.element());
        if self.sticker {
            message.append_child(Element::builder("sticker", "urn:xmpp:stickers:0").build());
        }
        message.append_child(
            Element::builder("x", OOB)
                .append(
                    Element::builder("url", OOB)
                        .append(self.wire_url.as_str())
                        .build(),
                )
                .build(),
        );
        let body = message.get_child("body", CLIENT).unwrap().text();
        let end = body.chars().count();
        let mut start = end - self.wire_url.chars().count();
        if start > 0 && body.chars().nth(start - 1) == Some('\n') {
            start -= 1;
        }
        for namespace in [NS, OOB] {
            message.append_child(
                Element::builder("fallback", FALLBACK)
                    .attr("for".try_into().unwrap(), namespace)
                    .append(
                        Element::builder("body", FALLBACK)
                            .attr("start".try_into().unwrap(), start.to_string())
                            .attr("end".try_into().unwrap(), end.to_string())
                            .build(),
                    )
                    .build(),
            );
        }
    }
    pub fn media_candidate(&self) -> bool {
        self.image_candidate() || self.video_candidate()
    }
    pub fn round_video(&self, width: u32, height: u32, duration_us: i64) -> bool {
        let name = self.name.to_ascii_lowercase();
        !self.sticker
            && name.starts_with("round_video_")
            && name.ends_with(".mp4")
            && width > 0
            && width == height
            && width <= 1280
            && duration_us > 0
    }
    pub fn video_candidate(&self) -> bool {
        self.mime.starts_with("video/") || self.video_extension()
    }
    pub fn video_extension(&self) -> bool {
        self.name.rsplit_once('.').is_some_and(|(_, extension)| {
            matches!(
                extension.to_ascii_lowercase().as_str(),
                "mp4" | "m4v" | "webm" | "mkv" | "mov" | "ogv"
            )
        })
    }
    pub fn image_candidate(&self) -> bool {
        self.sticker || self.mime.starts_with("image/") || self.image_extension()
    }
    pub fn image_extension(&self) -> bool {
        self.name.rsplit_once('.').is_some_and(|(_, ext)| {
            matches!(
                ext.to_ascii_lowercase().as_str(),
                "avif" | "gif" | "heic" | "jpeg" | "jpg" | "png" | "webp"
            )
        })
    }
    pub fn image_label(&self) -> &'static str {
        if self.video_candidate() {
            "Video"
        } else if self.sticker {
            "Sticker"
        } else if self.mime == "image/gif" || self.name.to_ascii_lowercase().ends_with(".gif") {
            "GIF"
        } else {
            "Image"
        }
    }
    pub fn link(&self) -> Option<Link> {
        Link::parse(&self.wire_url)
    }
    pub fn key(&self) -> String {
        let mut value = format!("{}\n{:?}", self.wire_url, self.size);
        for (algorithm, hash) in &self.hashes {
            value.push_str(&format!("\n{algorithm}:{}", STANDARD.encode(hash)));
        }
        openssl::sha::sha256(value.as_bytes())
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }
    pub fn bare(wire: &str) -> Option<Self> {
        let link = Link::parse(wire)?;
        Some(Self {
            wire_url: wire.into(),
            name: link.name(),
            mime: "application/octet-stream".into(),
            size: None,
            width: None,
            height: None,
            hashes: BTreeMap::new(),
            manual: false,
            sticker: false,
        })
    }
    fn description(element: &Element, sticker: bool) -> Option<Self> {
        if xml_size(element) > 65536 {
            return None;
        }
        let disposition = element.attr("disposition");
        if disposition.is_some_and(|v| !matches!(v, "inline" | "attachment")) {
            return None;
        }
        let file = only(element, "file", META)?;
        let sources = only(element, "sources", NS)?;
        let url =
            only(sources, "url-data", "http://jabber.org/protocol/url-data")?.attr("target")?;
        let mut value = Self::bare(url)?;
        let name = field(file, "name")?;
        if !name.is_empty() {
            value.name = safe_name(&name);
        }
        value.mime = field(file, "media-type")?
            .trim_matches(|c| c <= '\u{20}')
            .into();
        if value.mime.is_empty() {
            value.mime = "application/octet-stream".into();
        }
        if value.name.encode_utf16().count() > 200
            || matches!(value.name.as_str(), "." | "..")
            || value.mime.encode_utf16().count() > 200
        {
            return None;
        }
        let size = field(file, "size")?;
        let size = size.trim_matches(|c| c <= '\u{20}');
        if !size.is_empty() {
            value.size = Some(size.parse::<u64>().ok()?);
            if value.size? > i64::MAX as u64 {
                return None;
            }
        }
        let dimension = |name| {
            field(file, name)
                .and_then(|value| value.trim().parse::<u32>().ok())
                .filter(|value| (1..=65535).contains(value))
        };
        value.width = dimension("width");
        value.height = dimension("height");
        if value
            .width
            .zip(value.height)
            .is_some_and(|(w, h)| u64::from(w) * u64::from(h) > 64_000_000)
        {
            value.width = None;
            value.height = None;
        }
        for hash in file.children().filter(|c| c.is("hash", HASH)) {
            let algorithm = hash.attr("algo").unwrap_or("");
            if !matches!(algorithm, "sha-256" | "sha3-256" | "id-blake2b256") {
                continue;
            }
            let text = hash.text();
            if text.encode_utf16().count() > 128 {
                return None;
            }
            let text: String = text
                .chars()
                .filter(|c| !matches!(c, '\t' | '\n' | '\r' | ' '))
                .collect();
            let bytes = STANDARD.decode(text).ok()?;
            if bytes.len() != 32 || value.hashes.insert(algorithm.into(), bytes).is_some() {
                return None;
            }
        }
        value.manual = disposition == Some("attachment");
        value.sticker = sticker;
        Some(value)
    }
    /// Protected content is supplied by the caller; never inspect exposed encrypted XML.
    pub fn incoming(root: &Element, body: &str, encrypted: bool) -> Option<Self> {
        let unquoted = crate::protocol::reply_content(root, body).3;
        let payload = crate::forwarding::parse(&unquoted)
            .map(|(_, body)| body)
            .unwrap_or(unquoted);
        let descriptions: Vec<_> = root
            .children()
            .filter(|c| c.is("file-sharing", NS))
            .collect();
        let sticker = Self::sticker_marker(root);
        let mut value = if descriptions.is_empty() {
            let references: Vec<_> = root.children().filter(|c| c.is("x", OOB)).collect();
            if references.len() == 1 {
                Self::bare(&only(references[0], "url", OOB)?.text())?
            } else if encrypted {
                Self::bare(&payload)?
            } else {
                return None;
            }
        } else if descriptions.len() == 1 {
            Self::description(descriptions[0], sticker)?
        } else {
            return None;
        };
        value.sticker = sticker;
        (value.link()?.encrypted() == encrypted).then_some(value)
    }
    pub fn sticker_marker(root: &Element) -> bool {
        let mut markers = root
            .children()
            .filter(|c| c.is("sticker", "urn:xmpp:stickers:0"));
        markers.next().is_some_and(|marker| {
            marker.attrs().iter().next().is_none()
                && marker.children().next().is_none()
                && marker.text().trim().is_empty()
        }) && markers.next().is_none()
    }
    /// Only validated file URL spans disappear; malformed fallbacks keep caption text.
    pub fn caption(&self, root: &Element, body: &str) -> String {
        let reply_clean = crate::protocol::reply_content(root, body).3;
        let chars: Vec<_> = body.chars().collect();
        let length = chars.len();
        let mut ranges = Vec::new();
        for fallback in root.children().filter(|c| {
            c.is("fallback", FALLBACK) && c.attr("for").is_some_and(|ns| ns == NS || ns == OOB)
        }) {
            for part in fallback.children().filter(|c| {
                c.is("body", FALLBACK)
                    && c.attr_ns("http://www.w3.org/XML/1998/namespace", "lang")
                        .is_none()
            }) {
                let span = match (part.attr("start"), part.attr("end")) {
                    (None, None) => Some((0, length)),
                    (Some(a), Some(b)) => a.parse::<usize>().ok().zip(b.parse::<usize>().ok()),
                    _ => None,
                };
                let Some((a, b)) = span.filter(|(a, b)| a <= b && *b <= length) else {
                    return reply_clean;
                };
                if ranges.len() >= 64
                    || chars[a..b]
                        .iter()
                        .collect::<String>()
                        .trim_matches(|c| c <= '\u{20}')
                        != self.wire_url
                {
                    return reply_clean;
                }
                ranges.push((a, b));
            }
        }
        if ranges.is_empty() {
            if reply_clean == self.wire_url {
                return String::new();
            }
            if let Some((attribution, payload)) = crate::forwarding::parse(&reply_clean)
                && payload == self.wire_url
            {
                return attribution.wire_body("");
            }
        }
        if reply_clean != body {
            for fallback in root
                .children()
                .filter(|c| c.is("fallback", FALLBACK) && c.attr("for") == Some("urn:xmpp:reply:0"))
            {
                if fallback.children().next().is_none() {
                    ranges.push((0, length));
                }
                for part in fallback.children().filter(|c| {
                    c.is("body", FALLBACK)
                        && c.attr_ns("http://www.w3.org/XML/1998/namespace", "lang")
                            .is_none()
                }) {
                    let span = match (part.attr("start"), part.attr("end")) {
                        (None, None) => Some((0, length)),
                        (Some(a), Some(b)) => a.parse::<usize>().ok().zip(b.parse::<usize>().ok()),
                        _ => None,
                    };
                    if let Some((a, b)) = span.filter(|(a, b)| a <= b && *b <= length) {
                        ranges.push((a, b));
                    }
                }
            }
        }
        let mut masked = vec![false; length];
        for (a, b) in ranges {
            masked[a..b].fill(true);
        }
        chars
            .into_iter()
            .enumerate()
            .filter_map(|(i, c)| (!masked[i]).then_some(c))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn round_marker_requires_verified_square_video_metadata() {
        let mut file = Attachment::bare("https://example.org/opaque").unwrap();
        file.name = "ROUND_VIDEO_capture.MP4".into();
        assert!(file.round_video(640, 640, 1_000_000));
        for (width, height, duration) in [(0, 0, 1), (640, 480, 1), (1920, 1920, 1), (640, 640, 0)]
        {
            assert!(!file.round_video(width, height, duration));
        }
        file.name = "ordinary.mp4".into();
        assert!(!file.round_video(640, 640, 1_000_000));
    }

    #[test]
    fn actual_mobile_file_models_and_protected_captions_match() {
        let cases: Vec<serde_json::Value> =
            serde_json::from_str(include_str!("../tests/mobile-reference/file-vectors.json"))
                .unwrap();
        for (index, value) in cases.iter().enumerate() {
            let decode = |key: &str| STANDARD.decode(value[key].as_str().unwrap()).unwrap();
            let string = |key: &str| String::from_utf8(decode(key)).unwrap();
            let outer: Element = string("xml").parse().unwrap();
            let encrypted = value["encrypted"].as_bool().unwrap();
            let message = if encrypted {
                let content = crate::crypto::payload::content(&outer, &decode("sce"), 0).unwrap();
                crate::protocol::normalize_protected(&content, "bob@example.org", None)
                    .unwrap()
                    .1
            } else {
                crate::protocol::normalize(&outer, "bob@example.org", None)
                    .unwrap()
                    .1
            };
            assert_eq!(
                message.attachment.is_some(),
                value["attachment"].as_bool().unwrap(),
                "mobile vector {index}"
            );
            if let Some(file) = &message.attachment {
                assert_eq!(file.wire_url, string("url"), "mobile vector {index}");
                assert_eq!(file.name, string("name"));
                assert_eq!(file.mime, string("mime"));
                assert_eq!(
                    file.size.map(|n| n as i64).unwrap_or(-1),
                    value["size"].as_i64().unwrap()
                );
                assert_eq!(file.manual, value["manual"].as_bool().unwrap());
                assert_eq!(file.sticker, value["sticker"].as_bool().unwrap());
                assert_eq!(message.text, string("caption"), "caption vector {index}");
                assert_eq!(message.kind, "file");
                assert_eq!(message.encrypted, encrypted);
                assert!(file.valid_outgoing(encrypted));
                assert!(!file.valid_outgoing(!encrypted));
                let mut outgoing =
                    crate::protocol::outgoing("bob@example.org", &message, &message.reply);
                outgoing.set_attr(
                    "".into(),
                    "from".try_into().unwrap(),
                    "alice@example.org/phone",
                );
                let roundtrip = if encrypted {
                    crate::protocol::normalize_protected(&outgoing, "bob@example.org", None)
                } else {
                    crate::protocol::normalize(&outgoing, "bob@example.org", None)
                }
                .unwrap()
                .1;
                assert_eq!(roundtrip.attachment, message.attachment);
                assert_eq!(roundtrip.text, message.text);
                assert_eq!(roundtrip.forwarded, message.forwarded);
                assert!(!format!("{file:?}").contains(&file.wire_url));
                let restored =
                    crate::model::Message::decode(&serde_json::to_string(&message).unwrap())
                        .unwrap();
                assert_eq!(restored.attachment, message.attachment);
                assert_eq!(restored.text, message.text);
            }
        }
    }
    #[test]
    fn file_urls_never_forward_the_secret_fragment_or_user_info() {
        let secret = "00".repeat(44);
        let link = Link::parse(&format!(
            "aesgcm://example.org/tr%C3%A8s+cool.bin?token=fixture#{secret}"
        ))
        .unwrap();
        assert_eq!(
            link.url.as_str(),
            "https://example.org/tr%C3%A8s+cool.bin?token=fixture"
        );
        assert_eq!(link.name(), "très+cool.bin");
        assert_eq!(link.iv.len(), 12);
        assert_eq!(link.key.len(), 32);
        for invalid in [
            "http://example.org/file",
            "file:///etc/passwd",
            "https://user:pass@example.org/file",
            "https://@example.org/file",
            "https://example.org/file#secret",
            "aesgcm://example.org/file#abcd",
            "https://example.org:99999/file",
            "https://example.org/file\nother",
            "https://example.org/bad path",
            "https://éxample.org/file",
        ] {
            assert!(Link::parse(invalid).is_none(), "{invalid}");
        }
        let file = Attachment::bare("https://example.org/a").unwrap();
        let mut changed = file.clone();
        changed.size = Some(3);
        assert_ne!(file.key(), changed.key());
        changed.hashes.insert("sha-256".into(), vec![0; 32]);
        let key = changed.key();
        changed.hashes.insert("sha-256".into(), vec![1; 32]);
        assert_ne!(key, changed.key());
    }
    #[test]
    fn exposed_file_extensions_cannot_supply_private_urls() {
        let outer:Element="<message xmlns='jabber:client' from='alice@example.org/phone' to='bob@example.org' type='chat'><body>Private body</body><x xmlns='jabber:x:oob'><url>https://files.example.org/forged</url></x></message>".parse().unwrap();
        let mut clear = outer.clone();
        clear.remove_child("x", OOB);
        let plain = crate::crypto::payload::envelope(&clear).unwrap();
        let content = crate::crypto::payload::content(&outer, &plain, 0).unwrap();
        let (_, message, _) =
            crate::protocol::normalize_protected(&content, "bob@example.org", None).unwrap();
        assert!(message.attachment.is_none());
        assert_eq!(message.text, "Private body");
    }
}
