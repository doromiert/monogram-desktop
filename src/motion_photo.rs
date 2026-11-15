//! Google/Android Motion Photo JPEG container; transport stays ordinary XEP-0447/0454.
use crate::downloads::VerifiedFile;
use std::{
    io::{Read, Seek, SeekFrom, Write},
    sync::atomic::{AtomicBool, Ordering},
};
use tokio_xmpp::minidom::Element;

const CAMERA: &str = "http://ns.google.com/photos/1.0/camera/";
const CONTAINER: &str = "http://ns.google.com/photos/1.0/container/";
const ITEM: &str = "http://ns.google.com/photos/1.0/container/item/";
const RDF: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";
const XMP: &[u8] = b"http://ns.adobe.com/xap/1.0/\0";

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Clip {
    pub offset: u64,
    pub length: u64,
}

fn descendants<'a>(root: &'a Element, out: &mut Vec<&'a Element>) {
    out.push(root);
    for child in root.children() {
        descendants(child, out);
    }
}
fn xmp_clip(packet: &[u8], size: u64) -> Option<Clip> {
    let mut text = std::str::from_utf8(packet).ok()?.trim_matches('\0').trim();
    // minidom expects an element, whereas XMP commonly wraps it in xpacket PIs.
    while text.starts_with("<?") {
        text = text[text.find("?>")? + 2..].trim_start();
    }
    while text.ends_with("?>") {
        text = text[..text.rfind("<?")?].trim_end();
    }
    if text.matches('<').count() > 512 || text.contains("<!DOCTYPE") || text.contains("<!ENTITY") {
        return None;
    }
    let root: Element = text.parse().ok()?;
    let mut elements = vec![];
    descendants(&root, &mut elements);
    let descriptions: Vec<_> = elements
        .iter()
        .copied()
        .filter(|e| e.is("Description", RDF))
        .collect();
    let modern: Vec<_> = descriptions
        .iter()
        .filter_map(|e| e.attr_ns(CAMERA, "MotionPhoto"))
        .collect();
    let length = if modern == ["1"] {
        let directories: Vec<_> = elements
            .iter()
            .copied()
            .filter(|e| e.is("Directory", CONTAINER))
            .collect();
        if directories.len() != 1 {
            return None;
        }
        let mut nodes = vec![];
        descendants(directories[0], &mut nodes);
        let items: Vec<_> = nodes
            .into_iter()
            .filter(|e| e.is("Item", CONTAINER))
            .collect();
        if !(2..=16).contains(&items.len())
            || items.first()?.attr_ns(ITEM, "Semantic")? != "Primary"
        {
            return None;
        }
        let video = items.last()?;
        if video.attr_ns(ITEM, "Semantic")? != "MotionPhoto"
            || video.attr_ns(ITEM, "Mime")? != "video/mp4"
            || items[..items.len() - 1]
                .iter()
                .any(|e| e.attr_ns(ITEM, "Semantic") == Some("MotionPhoto"))
        {
            return None;
        }
        video.attr_ns(ITEM, "Length")?.parse::<u64>().ok()?
    } else if modern.is_empty() {
        // Legacy Google MicroVideo v1 is still produced by older cameras.
        let legacy: Vec<_> = descriptions
            .iter()
            .filter(|e| e.attr_ns(CAMERA, "MicroVideo") == Some("1"))
            .collect();
        if legacy.len() != 1 {
            return None;
        }
        legacy[0]
            .attr_ns(CAMERA, "MicroVideoOffset")?
            .parse::<u64>()
            .ok()?
    } else {
        return None;
    };
    let offset = size.checked_sub(length)?;
    (length >= 16 && length <= 512 * 1024 * 1024 && offset > 4).then_some(Clip { offset, length })
}

/// Inspect only JPEG metadata, never scan arbitrary image pixels for XML/video signatures.
pub fn inspect(input: &mut std::fs::File) -> Option<Clip> {
    let result = (|| {
        let size = input.metadata().ok()?.len();
        input.rewind().ok()?;
        let mut magic = [0; 2];
        input.read_exact(&mut magic).ok()?;
        if magic != [0xff, 0xd8] {
            return None;
        }
        let mut found = None;
        for _ in 0..256 {
            input.read_exact(&mut magic).ok()?;
            if magic[0] != 0xff || matches!(magic[1], 0xda | 0xd9 | 0x00 | 0xd8) {
                break;
            }
            if magic[1] == 0xff {
                input.seek(SeekFrom::Current(-1)).ok()?;
                continue;
            }
            let marker = magic[1];
            input.read_exact(&mut magic).ok()?;
            let length = u16::from_be_bytes(magic).checked_sub(2)? as usize;
            if input.stream_position().ok()?.checked_add(length as u64)? > 2 * 1024 * 1024 {
                return None;
            }
            if marker == 0xe1 {
                let mut bytes = vec![0; length];
                input.read_exact(&mut bytes).ok()?;
                if let Some(packet) = bytes.strip_prefix(XMP) {
                    if let Some(clip) = xmp_clip(packet, size) {
                        if found.replace(clip).is_some() {
                            return None;
                        }
                    }
                }
            } else {
                input.seek(SeekFrom::Current(length as i64)).ok()?;
            }
        }
        let clip = found?;
        if clip.offset < input.stream_position().ok()? {
            return None;
        }
        input.seek(SeekFrom::Start(clip.offset)).ok()?;
        let mut header = [0; 16];
        input.read_exact(&mut header).ok()?;
        let box_size = u32::from_be_bytes(header[..4].try_into().ok()?) as u64;
        (header[4..8] == *b"ftyp" && box_size >= 16 && box_size <= clip.length).then_some(clip)
    })();
    let _ = input.rewind();
    result
}

/// Anonymous disk-backed descriptor: no persistent decrypted clip or attacker-controlled path.
pub fn extract(mut input: VerifiedFile, cancel: &AtomicBool) -> Result<VerifiedFile, &'static str> {
    let clip = inspect(&mut input.0).ok_or("This photo has no playable motion clip")?;
    let path = std::env::temp_dir().join(format!(".monogram-motion-{}", uuid::Uuid::new_v4()));
    use std::os::unix::fs::OpenOptionsExt;
    let mut out = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)
        .map_err(|_| "Motion photo playback unavailable")?;
    std::fs::remove_file(&path).map_err(|_| "Motion photo playback unavailable")?;
    input
        .0
        .seek(SeekFrom::Start(clip.offset))
        .map_err(|_| "Motion photo unavailable")?;
    let mut left = clip.length;
    let mut bytes = [0; 65536];
    while left > 0 {
        if cancel.load(Ordering::Acquire) {
            return Err("Motion photo playback cancelled");
        }
        let amount = (left as usize).min(bytes.len());
        input
            .0
            .read_exact(&mut bytes[..amount])
            .map_err(|_| "Motion photo unavailable")?;
        out.write_all(&bytes[..amount])
            .map_err(|_| "Motion photo playback unavailable")?;
        left -= amount as u64;
    }
    out.rewind()
        .map_err(|_| "Motion photo playback unavailable")?;
    Ok(VerifiedFile(out))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    pub(crate) fn container(xmp: &str, video: &[u8]) -> Vec<u8> {
        let mut bytes = vec![0xff, 0xd8, 0xff, 0xe1];
        bytes.extend_from_slice(&((XMP.len() + xmp.len() + 2) as u16).to_be_bytes());
        bytes.extend_from_slice(XMP);
        bytes.extend_from_slice(xmp.as_bytes());
        bytes.extend_from_slice(&[0xff, 0xd9]);
        bytes.extend_from_slice(video);
        bytes
    }
    pub(crate) fn packet(motion: &str, length: &str) -> String {
        format!(
            r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><r:RDF xmlns:r="{RDF}"><r:Description xmlns:c="{CAMERA}" c:MotionPhoto="{motion}"><g:Directory xmlns:g="{CONTAINER}"><r:Seq><r:li><g:Item xmlns:i="{ITEM}" i:Semantic="Primary" i:Mime="image/jpeg"/></r:li><r:li><g:Item xmlns:i="{ITEM}" i:Semantic="MotionPhoto" i:Mime="video/mp4" i:Length="{length}"/></r:li></r:Seq></g:Directory></r:Description></r:RDF></x:xmpmeta>"#
        )
    }
    #[test]
    fn mobile_writer_fixture_survives_plain_and_encrypted_sfs_persistence() {
        use crate::file_share::Attachment;
        let bytes = include_bytes!("../tests/fixtures/motion-photoMP.jpg");
        let path = std::env::temp_dir().join(uuid::Uuid::new_v4().to_string());
        std::fs::write(&path, bytes).unwrap();
        let mut input = std::fs::File::open(&path).unwrap();
        std::fs::remove_file(path).unwrap();
        let clip = inspect(&mut input).expect("actual Android MediaController writer output");
        assert_eq!(
            clip.length,
            include_bytes!("../tests/fixtures/reaction-video.mp4").len() as u64
        );
        for encrypted in [false, true] {
            let url = if encrypted {
                format!("aesgcm://fixture.invalid/photo.jpg#{}", "00".repeat(44))
            } else {
                "https://fixture.invalid/photo.jpg".into()
            };
            let mut file = Attachment::bare(&url).unwrap();
            file.mime = "image/jpeg".into();
            file.size = Some(bytes.len() as u64);
            file.hashes
                .insert("sha-256".into(), openssl::sha::sha256(bytes).to_vec());
            assert!(file.valid_outgoing(encrypted));
            let mut root = Element::builder("message", "jabber:client")
                .append(
                    Element::builder("body", "jabber:client")
                        .append(url.as_str())
                        .build(),
                )
                .build();
            file.add_to(&mut root);
            let incoming = Attachment::incoming(&root, &url, encrypted).unwrap();
            assert_eq!(incoming, file);
            let persisted: Attachment =
                serde_json::from_str(&serde_json::to_string(&incoming).unwrap()).unwrap();
            assert_eq!(persisted, file);
        }
    }
    #[test]
    fn verified_container_extracts_exact_clip_and_rejects_stale_or_forged_metadata() {
        let video = include_bytes!("../tests/fixtures/reaction-video.mp4");
        let run = |bytes: &[u8]| {
            let path = std::env::temp_dir().join(uuid::Uuid::new_v4().to_string());
            std::fs::write(&path, bytes).unwrap();
            let mut input = std::fs::File::open(&path).unwrap();
            std::fs::remove_file(path).unwrap();
            let clip = inspect(&mut input);
            if clip.is_some() {
                let mut out = extract(VerifiedFile(input), &AtomicBool::new(false)).unwrap();
                let mut actual = vec![];
                out.0.read_to_end(&mut actual).unwrap();
                assert_eq!(actual, video);
            }
            clip
        };
        assert_eq!(
            run(&container(&packet("1", &video.len().to_string()), video))
                .unwrap()
                .length,
            video.len() as u64
        );
        assert!(run(&container(&packet("0", &video.len().to_string()), video)).is_none());
        assert!(run(&container(&packet("1", "18446744073709551615"), video)).is_none());
        assert!(
            run(&container(
                &packet("1", &video.len().to_string()),
                b"not an mp4"
            ))
            .is_none()
        );
        assert!(run(b"\xff\xd8\xff\xd9").is_none());
    }
}
