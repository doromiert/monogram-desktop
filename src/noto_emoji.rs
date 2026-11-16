//! Bundled Noto animated emoji (Lottie): which messages get one, and where its data comes from.
//!
//! A message is animated only when its whole text is exactly one emoji that is in a pack. Several
//! emoji, any other text, or an emoji outside the packs keep the normal static rendering.
//!
//! Two packs are read, built by `tools/pack-noto-emoji.py` (format described there):
//! `noto-animated-emoji.pack`, the curated 250 most popular emoji that ships with the app, and an
//! optional `noto-animated-emoji-full.pack` (`--all`) that is preferred when present. The second
//! one is the placeholder for anyone who wants the full set: drop the file next to the first.
//! Each entry is a gzip Lottie file, rendered by the confined renderer like any other sticker.
use std::{collections::HashMap, path::Path, sync::OnceLock};

const MAGIC: &[u8; 4] = b"MGNE";
const VERSION: u32 = 1;
const MAX_ENTRIES: u32 = 4096;
const MAX_BLOB: usize = 1024 * 1024;
const MAX_KEY: usize = 64;
const MAX_SEQUENCE: usize = 16;
pub const CURATED: &str = "noto-animated-emoji.pack";
pub const FULL: &str = "noto-animated-emoji-full.pack";

pub struct Pack {
    data: Vec<u8>,
    entries: HashMap<String, (usize, usize)>,
}

fn u32_at(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(bytes.get(at..at.checked_add(4)?)?.try_into().ok()?))
}

impl Pack {
    /// Parse and fully validate a pack. Nothing in the file is trusted: counts, key shapes,
    /// ordering and every offset are checked, so a corrupt or hostile pack is refused instead of
    /// reading outside its own bytes.
    pub fn parse(data: Vec<u8>) -> Result<Self, &'static str> {
        const BAD: &str = "Invalid animated emoji pack";
        if data.get(..4) != Some(MAGIC) || u32_at(&data, 4) != Some(VERSION) {
            return Err(BAD);
        }
        let count = u32_at(&data, 8).ok_or(BAD)?;
        if count == 0 || count > MAX_ENTRIES {
            return Err(BAD);
        }
        let mut at = 12;
        let mut parsed = Vec::with_capacity(count as usize);
        for _ in 0..count {
            let length = *data.get(at).ok_or(BAD)? as usize;
            let key = data.get(at + 1..at + 1 + length).ok_or(BAD)?;
            let valid_key = (1..=MAX_KEY).contains(&length)
                && key.iter().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase() || *b == b'-')
                && !key.starts_with(b"-")
                && !key.ends_with(b"-");
            if !valid_key {
                return Err(BAD);
            }
            let key = String::from_utf8(key.to_vec()).map_err(|_| BAD)?;
            let (offset, size) = (
                u32_at(&data, at + 1 + length).ok_or(BAD)? as usize,
                u32_at(&data, at + 5 + length).ok_or(BAD)? as usize,
            );
            parsed.push((key, offset, size));
            at += 9 + length;
        }
        let blobs_start = at;
        let mut entries = HashMap::with_capacity(parsed.len());
        let mut previous: Option<&str> = None;
        for (key, offset, size) in &parsed {
            // Sorted and unique, so duplicates cannot shadow each other.
            if previous.is_some_and(|last| last >= key.as_str()) {
                return Err(BAD);
            }
            previous = Some(key);
            let end = offset.checked_add(*size).ok_or(BAD)?;
            if *size == 0 || *size > MAX_BLOB || *offset < blobs_start || end > data.len() {
                return Err(BAD);
            }
            entries.insert(key.clone(), (*offset, *size));
        }
        Ok(Self { data, entries })
    }
    pub fn load(path: &Path) -> Result<Self, &'static str> {
        let metadata = std::fs::metadata(path).map_err(|_| "Animated emoji pack unavailable")?;
        // The full set is about 11 MB; anything far beyond that is not one of our packs.
        if !metadata.is_file() || metadata.len() > 64 * 1024 * 1024 {
            return Err("Invalid animated emoji pack");
        }
        Self::parse(std::fs::read(path).map_err(|_| "Animated emoji pack unavailable")?)
    }
    /// The gzip-compressed Lottie file for a normalized key.
    pub fn get(&self, key: &str) -> Option<&[u8]> {
        let (offset, size) = *self.entries.get(key)?;
        self.data.get(offset..offset + size)
    }
    pub fn len(&self) -> usize {
        self.entries.len()
    }
    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.entries.keys().map(String::as_str)
    }
}

/// The pack key for a message that is exactly one emoji: lowercase hex code points joined by `-`,
/// without presentation selectors, so "❤" and "❤️" find the same animation.
pub fn key_for(text: &str) -> Option<String> {
    let text = text.trim();
    let mut points = Vec::new();
    for c in text.chars() {
        if matches!(c, '\u{fe0e}' | '\u{fe0f}') {
            continue;
        }
        points.push(c);
        if points.len() > MAX_SEQUENCE {
            return None;
        }
    }
    let first = *points.first()?;
    // Letters, digits, ©, ® and ™ are only emoji when written with the emoji selector.
    if (first as u32) < 0x2000 && !text.contains('\u{fe0f}') {
        return None;
    }
    Some(points.iter().map(|c| format!("{:x}", *c as u32)).collect::<Vec<_>>().join("-"))
}

pub fn curated() -> Result<Pack, &'static str> {
    Pack::load(Path::new(&crate::widgets::asset(CURATED)))
}

fn packs() -> &'static Vec<Pack> {
    static PACKS: OnceLock<Vec<Pack>> = OnceLock::new();
    PACKS.get_or_init(|| {
        // The full pack, when installed, wins over the curated one.
        [FULL, CURATED]
            .into_iter()
            .filter_map(|name| Pack::load(Path::new(&crate::widgets::asset(name))).ok())
            .collect()
    })
}

/// The animation for a message, if its entire text is one bundled emoji.
pub fn animation_for(text: &str) -> Option<Vec<u8>> {
    let key = key_for(text)?;
    packs().iter().find_map(|pack| pack.get(&key)).map(<[u8]>::to_vec)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    fn pack(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let header = 12 + entries.iter().map(|(key, _)| 9 + key.len()).sum::<usize>();
        let (mut index, mut blobs, mut offset) = (Vec::new(), Vec::new(), header);
        for (key, blob) in entries {
            index.push(key.len() as u8);
            index.extend_from_slice(key.as_bytes());
            index.extend_from_slice(&(offset as u32).to_le_bytes());
            index.extend_from_slice(&(blob.len() as u32).to_le_bytes());
            blobs.extend_from_slice(blob);
            offset += blob.len();
        }
        let mut bytes = b"MGNE".to_vec();
        bytes.extend_from_slice(&1u32.to_le_bytes());
        bytes.extend_from_slice(&(entries.len() as u32).to_le_bytes());
        bytes.extend(index);
        bytes.extend(blobs);
        bytes
    }

    #[test]
    fn exactly_one_emoji_maps_to_its_key_and_anything_else_does_not() {
        assert_eq!(key_for("😄").as_deref(), Some("1f604"));
        assert_eq!(key_for("  😄\n").as_deref(), Some("1f604"), "surrounding whitespace is ignored");
        assert_eq!(key_for("❤").as_deref(), Some("2764"));
        assert_eq!(key_for("❤️").as_deref(), Some("2764"), "with or without the emoji selector");
        assert_eq!(key_for("👨‍👩‍👧").as_deref(), Some("1f468-200d-1f469-200d-1f467"), "a ZWJ family is one emoji");
        // The same text with letters, digits or a second emoji is a different key, never a hit.
        for text in ["", "   ", "a", "7", "©", "™", "Hi 😄", "😄!"] {
            assert!(
                key_for(text).is_none_or(|key| !["1f604", "2764"].contains(&key.as_str())),
                "{text:?}"
            );
        }
        assert_eq!(key_for("©️").as_deref(), Some("a9"), "emoji-selector form is an emoji");
        assert_eq!(key_for("©"), None, "bare symbol is text");
        assert_eq!(key_for("7"), None);
        assert_eq!(key_for(&"😄".repeat(40)), None, "bounded work for long input");
    }

    #[test]
    fn several_emoji_never_match_a_single_emoji_entry() {
        let pack = Pack::parse(pack(&[("1f602", b"a"), ("1f604", b"b")])).unwrap();
        for text in ["😄😄", "😄😂", "😄 😄"] {
            let key = key_for(text).unwrap();
            assert!(pack.get(&key).is_none(), "{text:?} -> {key}");
        }
        assert_eq!(pack.get(&key_for("😄").unwrap()), Some(&b"b"[..]));
    }

    #[test]
    fn a_valid_pack_is_indexed_by_key() {
        let parsed = Pack::parse(pack(&[("1f468-200d-1f469", b"zwj"), ("1f602", b"first"), ("1f604", b"second")])).unwrap();
        assert_eq!(parsed.len(), 3);
        assert_eq!(parsed.get("1f604"), Some(&b"second"[..]));
        assert_eq!(parsed.get("1f468-200d-1f469"), Some(&b"zwj"[..]));
        assert_eq!(parsed.get("1f605"), None);
    }

    #[test]
    fn corrupt_or_hostile_packs_are_refused_without_panicking() {
        let good = pack(&[("1f602", b"first"), ("1f604", b"second")]);
        assert!(Pack::parse(good.clone()).is_ok());
        let mut cases: Vec<(&str, Vec<u8>)> = vec![
            ("empty", vec![]),
            ("wrong magic", [b"XXXX".as_slice(), &good[4..]].concat()),
            ("wrong version", { let mut b = good.clone(); b[4] = 2; b }),
            ("no entries", { let mut b = good.clone(); b[8..12].copy_from_slice(&0u32.to_le_bytes()); b }),
            ("too many entries", { let mut b = good.clone(); b[8..12].copy_from_slice(&u32::MAX.to_le_bytes()); b }),
            ("truncated blobs", good[..good.len() - 3].to_vec()),
            ("truncated index", good[..20].to_vec()),
            ("unsorted", pack(&[("1f604", b"a"), ("1f602", b"b")])),
            ("duplicate", pack(&[("1f604", b"a"), ("1f604", b"b")])),
            ("empty blob", pack(&[("1f604", b"")])),
            ("uppercase key", pack(&[("1F604", b"a")])),
            ("path-like key", pack(&[("../x", b"a")])),
            ("trailing dash", pack(&[("1f604-", b"a")])),
            ("oversize blob", pack(&[("1f604", &vec![0u8; MAX_BLOB + 1])])),
        ];
        // An offset that points back into the index, and one that overflows.
        let mut back = good.clone();
        let at = 12 + 1 + 5;
        back[at..at + 4].copy_from_slice(&0u32.to_le_bytes());
        cases.push(("offset into the index", back));
        let mut overflow = good.clone();
        overflow[at..at + 4].copy_from_slice(&u32::MAX.to_le_bytes());
        cases.push(("offset overflow", overflow));
        for (name, bytes) in cases {
            assert!(Pack::parse(bytes).is_err(), "{name} must be refused");
        }
    }

    /// The pack that ships with the app: parses, has the popular emoji, and every entry is a
    /// Lottie file inside the limits the renderer enforces for emoji.
    #[test]
    fn the_bundled_pack_is_valid_and_every_animation_is_within_renderer_limits() {
        let pack = Pack::load(Path::new(&crate::widgets::asset(CURATED))).expect("bundled pack");
        assert!(pack.len() >= 200, "{} emoji", pack.len());
        for key in ["1f604", "1f602", "2764", "1f44d", "1f525"] {
            assert!(pack.get(key).is_some(), "{key} is bundled");
        }
        for (key, (offset, size)) in &pack.entries {
            let mut json = String::new();
            flate2::read::GzDecoder::new(&pack.data[*offset..offset + size])
                .take(8 * 1024 * 1024)
                .read_to_string(&mut json)
                .unwrap_or_else(|_| panic!("{key} is gzip"));
            let value: serde_json::Value = serde_json::from_str(&json).unwrap_or_else(|_| panic!("{key} is JSON"));
            let (w, h) = (value["w"].as_u64().unwrap(), value["h"].as_u64().unwrap());
            let (rate, frames) = (value["fr"].as_f64().unwrap(), value["op"].as_f64().unwrap() - value["ip"].as_f64().unwrap());
            assert!(w == h && (1..=1024).contains(&w), "{key}: {w}x{h}");
            assert!(rate > 0. && rate <= 60. && frames > 0. && frames <= 360. && frames / rate <= 6.1, "{key}: {frames} frames at {rate}");
        }
    }
}
