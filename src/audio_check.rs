//! Strict checks on an audio file before anything decodes it.
//!
//! Receiving a message can start preparing its audio without a click, so a crafted file must be
//! refused as early and as cheaply as possible, and what is accepted must be exactly what the
//! decoders are meant to see. This module reads only the start of the file (never decodes) and
//! accepts six containers: Ogg (Vorbis or Opus only), MP3, WAV (plain PCM or float), M4A (MP4
//! audio) and WebM. Everything else is refused: other Ogg codecs and video, MPEG layers I and II,
//! compressed WAV, free-format or absurdly sized headers, and a file whose extension names a
//! different container than its bytes. The sandboxed helper repeats these checks on the bytes it
//! actually receives and adds the ones that need decoding (duration, channels, no video track).
//!
//! The same functions are compiled into the helper (`#[path]`), so there is one definition.
use std::fmt;

/// Largest file that is prepared at all.
pub const MAX_FILE_BYTES: u64 = 64 * 1024 * 1024;
/// Longest audio that is decoded, however long the header claims it is.
pub const MAX_DURATION_MS: u64 = 30 * 60 * 1000;
/// How much of the start of the file `check` needs.
pub const HEAD_BYTES: usize = 64 * 1024;
pub const MIN_RATE: u32 = 8_000;
pub const MAX_RATE: u32 = 96_000;
pub const MAX_CHANNELS: u32 = 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Container {
    Ogg,
    Mp3,
    Wav,
    M4a,
    Webm,
}
impl Container {
    pub fn name(self) -> &'static str {
        match self {
            Self::Ogg => "ogg",
            Self::Mp3 => "mp3",
            Self::Wav => "wav",
            Self::M4a => "m4a",
            Self::Webm => "webm",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    Empty,
    TooLarge,
    /// Not one of the accepted containers or codecs.
    Unsupported,
    /// The file name says one container and the bytes are another.
    ExtensionMismatch,
    /// A header field is out of range or inconsistent.
    Malformed(&'static str),
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Empty => "This audio file is empty",
            Self::TooLarge => "This audio file is too large to preview",
            Self::Unsupported => "This audio format is not supported",
            Self::ExtensionMismatch => "The file name does not match its contents",
            Self::Malformed(_) => "This audio file is not valid",
        })
    }
}
impl std::error::Error for Error {}

/// The container a file name's extension claims, if it is one of ours.
pub fn extension_container(name: &str) -> Option<Container> {
    let (_, extension) = name.rsplit_once('.')?;
    Some(match extension.to_ascii_lowercase().as_str() {
        "ogg" | "oga" | "opus" => Container::Ogg,
        "mp3" => Container::Mp3,
        "wav" | "wave" => Container::Wav,
        "m4a" => Container::M4a,
        "webm" => Container::Webm,
        _ => return None,
    })
}

fn bad<T>(why: &'static str) -> Result<T, Error> {
    Err(Error::Malformed(why))
}
fn le16(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from(u16::from_le_bytes(b.get(at..at.checked_add(2)?)?.try_into().ok()?)))
}
fn le32(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(at..at.checked_add(4)?)?.try_into().ok()?))
}
fn be32(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(b.get(at..at.checked_add(4)?)?.try_into().ok()?))
}
fn rate_ok(rate: u32) -> bool {
    (MIN_RATE..=MAX_RATE).contains(&rate)
}

/// Identify and validate `head` (the first bytes of a file of `file_len` bytes) and require that
/// the file name's extension, when it is one of ours, agrees with the content.
pub fn check(name: &str, head: &[u8], file_len: u64) -> Result<Container, Error> {
    if file_len == 0 || head.is_empty() {
        return Err(Error::Empty);
    }
    if file_len > MAX_FILE_BYTES {
        return Err(Error::TooLarge);
    }
    let head = &head[..head.len().min(HEAD_BYTES)];
    let container = sniff(head, file_len)?;
    match extension_container(name) {
        Some(claimed) if claimed != container => Err(Error::ExtensionMismatch),
        _ => Ok(container),
    }
}

/// Which container `head` is, after validating its structure.
pub fn sniff(head: &[u8], file_len: u64) -> Result<Container, Error> {
    if head.starts_with(b"OggS") {
        ogg(head)?;
        Ok(Container::Ogg)
    } else if head.starts_with(b"RIFF") {
        wav(head, file_len)?;
        Ok(Container::Wav)
    } else if head.get(4..8) == Some(b"ftyp") {
        m4a(head, file_len)?;
        Ok(Container::M4a)
    } else if head.starts_with(&[0x1a, 0x45, 0xdf, 0xa3]) {
        webm(head)?;
        Ok(Container::Webm)
    } else if head.starts_with(b"ID3") || (head.len() >= 2 && head[0] == 0xff && head[1] & 0xe0 == 0xe0) {
        mp3(head, file_len)?;
        Ok(Container::Mp3)
    } else {
        Err(Error::Unsupported)
    }
}

// ---- Ogg -------------------------------------------------------------------------------------

/// Walk the leading pages: every stream that begins must be Vorbis or Opus (no FLAC, Speex or
/// video), and there is at least one.
fn ogg(head: &[u8]) -> Result<(), Error> {
    let (mut at, mut streams) = (0usize, 0u32);
    for _ in 0..16 {
        if head.get(at..at + 4) != Some(b"OggS") {
            // Pages after the start may lie beyond the bytes we have.
            break;
        }
        let (version, flags) = (*head.get(at + 4).ok_or(Error::Malformed("short page"))?, *head.get(at + 5).ok_or(Error::Malformed("short page"))?);
        let segments = usize::from(*head.get(at + 26).ok_or(Error::Malformed("short page"))?);
        if version != 0 || flags & !0x07 != 0 {
            return bad("ogg page header");
        }
        let table = head.get(at + 27..at + 27 + segments).ok_or(Error::Malformed("short segment table"))?;
        let body: usize = table.iter().map(|b| usize::from(*b)).sum();
        let start = at + 27 + segments;
        if flags & 0x02 != 0 {
            // A beginning-of-stream page: a continued packet makes no sense here, and the granule
            // position of an identification header is zero.
            if flags & 0x01 != 0 || head.get(at + 6..at + 14) != Some(&[0u8; 8]) {
                return bad("ogg first page");
            }
            let first = head.get(start..(start + body).min(head.len())).ok_or(Error::Malformed("short packet"))?;
            if first.starts_with(b"OpusHead") {
                // Version 1, one or two channels, channel mapping family 0 (no surround tables).
                if first.len() < 19 || first[8] != 1 || !(1..=2).contains(&first[9]) || first[18] != 0 {
                    return bad("opus header");
                }
            } else if first.starts_with(b"\x01vorbis") {
                let rate = le32(first, 12).ok_or(Error::Malformed("vorbis header"))?;
                // 30 bytes: version, channels, rate, three bitrates, block sizes and the framing bit.
                if first.len() < 30 || le32(first, 7) != Some(0) || !(1..=2).contains(&u32::from(first[11])) || !rate_ok(rate) || first[28] == 0 || first[29] & 1 != 1 {
                    return bad("vorbis header");
                }
            } else {
                return Err(Error::Unsupported);
            }
            streams += 1;
        } else if streams == 0 {
            return bad("ogg does not begin a stream");
        } else if flags & 0x02 == 0 {
            // First page after the stream headers: nothing more to learn without decoding.
            break;
        }
        at = start + body;
        if at > head.len() {
            break;
        }
    }
    if streams == 0 {
        return bad("no audio stream");
    }
    // One stream only: a second beginning-of-stream page with another codec is how video or a
    // chained stream is smuggled in, and the page loop above already refused unknown codecs.
    if streams > 1 {
        return Err(Error::Unsupported);
    }
    Ok(())
}

// ---- WAV -------------------------------------------------------------------------------------

fn wav(head: &[u8], file_len: u64) -> Result<(), Error> {
    // Whether `head` is the whole file: running out of bytes then means it is truncated.
    let complete = file_len <= head.len() as u64;
    if head.get(8..12) != Some(b"WAVE") {
        return Err(Error::Unsupported);
    }
    let (mut at, mut fmt_seen) = (12usize, false);
    for _ in 0..32 {
        let (Some(id), Some(size)) = (head.get(at..at + 4), le32(head, at + 4)) else {
            // Out of bytes before the data chunk: fine if the file goes on past what we read, a
            // truncated file if this is all of it.
            return if fmt_seen && !complete { Ok(()) } else { bad("truncated before the data chunk") };
        };
        let body = at as u64 + 8;
        // A chunk larger than what remains of the file is how a huge allocation is requested.
        if u64::from(size) > file_len.saturating_sub(body) {
            return bad("chunk larger than the file");
        }
        match id {
            b"fmt " => {
                if !matches!(size, 16 | 18 | 40) {
                    return bad("format chunk size");
                }
                let chunk = head.get(at + 8..at + 8 + size as usize).ok_or(Error::Malformed("short format chunk"))?;
                let (tag, channels, rate, byte_rate, align, bits) =
                    (le16(chunk, 0), le16(chunk, 2), le32(chunk, 4), le32(chunk, 8), le16(chunk, 12), le16(chunk, 14));
                let (Some(tag), Some(channels), Some(rate), Some(byte_rate), Some(align), Some(bits)) = (tag, channels, rate, byte_rate, align, bits) else {
                    return bad("short format chunk");
                };
                // 1 = PCM, 3 = IEEE float, 0xFFFE = extensible (its sub-format is checked below).
                let kind = if tag == 0xfffe {
                    if size != 40 || le16(chunk, 16) != Some(22) {
                        return bad("extensible format");
                    }
                    le16(chunk, 24).ok_or(Error::Malformed("extensible format"))?
                } else {
                    tag
                };
                if !matches!(kind, 1 | 3) {
                    // ADPCM, A-law, MP3-in-WAV and the rest are decoders we do not expose.
                    return Err(Error::Unsupported);
                }
                if !(1..=MAX_CHANNELS).contains(&channels) || !rate_ok(rate) || !matches!(bits, 8 | 16 | 24 | 32) || (kind == 3 && bits != 32) {
                    return bad("format values");
                }
                let frame = channels * bits / 8;
                if align != frame || u64::from(byte_rate) != u64::from(rate) * u64::from(frame) {
                    return bad("inconsistent format");
                }
                fmt_seen = true;
            }
            b"data" => return if fmt_seen { Ok(()) } else { bad("data before format") },
            _ => {}
        }
        at = at + 8 + size as usize + (size as usize & 1);
    }
    bad("too many chunks before the data")
}

// ---- MP4 / M4A -------------------------------------------------------------------------------

fn m4a(head: &[u8], file_len: u64) -> Result<(), Error> {
    let (mut at, mut saw_ftyp) = (0usize, false);
    for _ in 0..16 {
        let (Some(size32), Some(kind)) = (be32(head, at), head.get(at + 4..at + 8)) else { break };
        let mut size = u64::from(size32);
        let mut header = 8u64;
        if size == 1 {
            // A 64-bit size follows; it must still fit the file and not overflow.
            let large = head.get(at + 8..at + 16).ok_or(Error::Malformed("short box"))?;
            size = u64::from_be_bytes(large.try_into().unwrap());
            header = 16;
        } else if size == 0 {
            size = file_len - at as u64; // runs to the end of the file
        }
        let end = (at as u64).checked_add(size).ok_or(Error::Malformed("box size overflows"))?;
        if size < header || end > file_len {
            return bad("box size");
        }
        if !saw_ftyp {
            if kind != b"ftyp" || size < 16 || size > 1024 {
                return bad("first box is not a small ftyp");
            }
            let brand = head.get(at + 8..at + 12).ok_or(Error::Malformed("short ftyp"))?;
            if !matches!(brand, b"M4A " | b"M4B " | b"mp42" | b"isom" | b"iso2") {
                return Err(Error::Unsupported);
            }
            saw_ftyp = true;
        }
        if end as usize > head.len() {
            break;
        }
        at = end as usize;
    }
    if saw_ftyp { Ok(()) } else { bad("no ftyp") }
}

// ---- WebM ------------------------------------------------------------------------------------

/// Read an EBML variable-length integer; returns (value, length).
fn vint(b: &[u8], at: usize, keep_marker: bool) -> Option<(u64, usize)> {
    let first = *b.get(at)?;
    let len = first.leading_zeros() as usize + 1;
    if len > 8 {
        return None;
    }
    // `len` can be 8, so mask in u64: shifting a u8 by 8 would overflow.
    let mut value = if keep_marker { u64::from(first) } else { u64::from(first) & (0xffu64 >> len) };
    for i in 1..len {
        value = value << 8 | u64::from(*b.get(at + i)?);
    }
    Some((value, len))
}

fn webm(head: &[u8]) -> Result<(), Error> {
    let (_, id_len) = vint(head, 0, true).ok_or(Error::Malformed("ebml id"))?;
    let (size, size_len) = vint(head, id_len, false).ok_or(Error::Malformed("ebml size"))?;
    if size == 0 || size > 1024 {
        return bad("ebml header size");
    }
    let start = id_len + size_len;
    let end = start + size as usize;
    let body = head.get(start..end).ok_or(Error::Malformed("short ebml header"))?;
    let (mut at, mut doc_type) = (0usize, None);
    while at < body.len() {
        let (id, id_len) = vint(body, at, true).ok_or(Error::Malformed("ebml child"))?;
        let (len, len_len) = vint(body, at + id_len, false).ok_or(Error::Malformed("ebml child size"))?;
        let from = at + id_len + len_len;
        let value = body.get(from..from.checked_add(len as usize).ok_or(Error::Malformed("size"))?).ok_or(Error::Malformed("ebml child"))?;
        match id {
            0x4282 => doc_type = Some(value),                      // DocType
            0x4286 | 0x42f7 if value != [1] => return bad("ebml version"), // EBMLVersion, EBMLReadVersion
            0x42f2 if value.is_empty() || value.iter().any(|b| *b > 4) && value != [4] => return bad("max id length"),
            0x42f3 if value.is_empty() || value.iter().any(|b| *b > 8) => return bad("max size length"),
            _ => {}
        }
        at = from + len as usize;
    }
    // EBML strings may be padded with NUL bytes (GStreamer writes "webm\0"); nothing else is allowed.
    let doc_type = doc_type.map(|value| {
        let end = value.iter().rposition(|b| *b != 0).map_or(0, |last| last + 1);
        &value[..end]
    });
    if doc_type != Some(b"webm") {
        // Matroska proper may hold video, subtitles and attachments; only WebM is accepted.
        return Err(Error::Unsupported);
    }
    // The Segment follows the header.
    if head.get(end..end + 4) != Some(&[0x18, 0x53, 0x80, 0x67]) {
        return bad("no segment after the header");
    }
    Ok(())
}

// ---- MP3 -------------------------------------------------------------------------------------

/// Parse an MPEG audio frame header at `at`: `(frame length, sample rate)` for Layer III only.
fn mp3_frame(head: &[u8], at: usize) -> Option<(usize, u32)> {
    let header = be32(head, at)?;
    if header >> 21 != 0x7ff {
        return None;
    }
    let version = (header >> 19) & 3; // 0 = MPEG 2.5, 1 = reserved, 2 = MPEG 2, 3 = MPEG 1
    let layer = (header >> 17) & 3; // 1 = Layer III
    let bitrate_index = ((header >> 12) & 15) as usize;
    let rate_index = ((header >> 10) & 3) as usize;
    let padding = ((header >> 9) & 1) as usize;
    if version == 1 || layer != 1 || bitrate_index == 0 || bitrate_index == 15 || rate_index == 3 {
        // Reserved version, Layer I or II, free-format or invalid bitrate, reserved sample rate.
        return None;
    }
    const MPEG1: [u32; 14] = [32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320];
    const MPEG2: [u32; 14] = [8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 144, 160];
    let (kbps, rate, samples) = if version == 3 {
        (MPEG1[bitrate_index - 1], [44_100, 48_000, 32_000][rate_index], 1152)
    } else {
        (MPEG2[bitrate_index - 1], [22_050, 24_000, 16_000][rate_index] / if version == 0 { 2 } else { 1 }, 576)
    };
    let length = (samples / 8 * kbps as usize * 1000) / rate as usize + padding;
    (rate_ok(rate) || rate == 11_025 || rate == 12_000).then_some((length, rate)).filter(|(len, _)| *len >= 24)
}

fn mp3(head: &[u8], file_len: u64) -> Result<(), Error> {
    let mut at = 0usize;
    if head.starts_with(b"ID3") {
        let (major, flags) = (*head.get(3).ok_or(Error::Malformed("short tag"))?, *head.get(5).ok_or(Error::Malformed("short tag"))?);
        let size = head.get(6..10).ok_or(Error::Malformed("short tag"))?;
        // The tag size is four "syncsafe" bytes (7 bits each) and must leave room for audio.
        if !(2..=4).contains(&major) || size.iter().any(|b| b & 0x80 != 0) || flags & 0x0f != 0 {
            return bad("id3 header");
        }
        let tag = size.iter().fold(0u64, |value, b| value << 7 | u64::from(*b)) + 10;
        if tag >= file_len {
            return bad("id3 tag fills the file");
        }
        at = tag as usize;
        if at >= head.len() {
            // The audio starts beyond what we read; the sandboxed decoder will judge it.
            return Ok(());
        }
    }
    // The first frame must start right here (at the start, or straight after the tag), and the
    // next one must follow exactly where its length says: a few random bytes that look like a sync
    // word do not make an MP3, and junk in front of the audio is not skipped over.
    let Some((length, _)) = mp3_frame(head, at) else { return bad("no mp3 frame where the audio starts") };
    match head.get(at + length..at + length + 4) {
        Some(_) if mp3_frame(head, at + length).is_some() => Ok(()),
        // The next frame lies beyond the bytes we have: acceptable only if the file continues.
        None if ((at + length) as u64) <= file_len => Ok(()),
        _ => bad("mp3 frames do not chain"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- minimal valid files, built by hand so the crafted variants can change one field ----

    fn ogg_page(flags: u8, granule: u64, packet: &[u8]) -> Vec<u8> {
        let mut page = b"OggS".to_vec();
        page.extend_from_slice(&[0, flags]);
        page.extend_from_slice(&granule.to_le_bytes());
        page.extend_from_slice(&[1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        // 1 segment (a packet under 255 bytes).
        page.push(1);
        page.push(packet.len() as u8);
        page.extend_from_slice(packet);
        page
    }
    fn opus_head(channels: u8, family: u8) -> Vec<u8> {
        let mut head = b"OpusHead".to_vec();
        head.extend_from_slice(&[1, channels, 0x38, 0x01]);
        head.extend_from_slice(&48_000u32.to_le_bytes());
        head.extend_from_slice(&[0, 0, family]);
        head
    }
    fn vorbis_id(channels: u8, rate: u32) -> Vec<u8> {
        let mut id = b"\x01vorbis".to_vec();
        id.extend_from_slice(&0u32.to_le_bytes());
        id.push(channels);
        id.extend_from_slice(&rate.to_le_bytes());
        id.extend_from_slice(&[0; 12]); // three bitrates
        id.extend_from_slice(&[0xb8, 1]); // block sizes, framing bit
        id
    }
    fn wav_file(tag: u16, channels: u16, rate: u32, bits: u16, data: u32) -> Vec<u8> {
        let frame = channels * bits / 8;
        let mut f = b"RIFF".to_vec();
        f.extend_from_slice(&(36 + data).to_le_bytes());
        f.extend_from_slice(b"WAVEfmt ");
        f.extend_from_slice(&16u32.to_le_bytes());
        for v in [tag, channels] { f.extend_from_slice(&v.to_le_bytes()); }
        f.extend_from_slice(&rate.to_le_bytes());
        f.extend_from_slice(&(rate * u32::from(frame)).to_le_bytes());
        for v in [frame, bits] { f.extend_from_slice(&v.to_le_bytes()); }
        f.extend_from_slice(b"data");
        f.extend_from_slice(&data.to_le_bytes());
        f.resize(f.len() + data as usize, 0);
        f
    }
    fn mp3_frames(count: usize) -> Vec<u8> {
        // MPEG-1 Layer III, 128 kbps, 44.1 kHz, no padding: 417 bytes per frame.
        let mut out = Vec::new();
        for _ in 0..count {
            out.extend_from_slice(&[0xff, 0xfb, 0x90, 0x00]);
            out.resize(out.len() + 413, 0);
        }
        out
    }
    fn mp4(brand: &[u8; 4], extra: &[u8]) -> Vec<u8> {
        let mut f = 24u32.to_be_bytes().to_vec();
        f.extend_from_slice(b"ftyp");
        f.extend_from_slice(brand);
        f.extend_from_slice(&[0, 0, 0, 0]);
        f.extend_from_slice(brand);
        f.extend_from_slice(b"isom");
        f.extend_from_slice(extra);
        f
    }
    fn webm_file(doc: &[u8]) -> Vec<u8> {
        let mut header = vec![0x42, 0x82, 0x80 | doc.len() as u8];
        header.extend_from_slice(doc);
        header.extend_from_slice(&[0x42, 0x86, 0x81, 1, 0x42, 0xf7, 0x81, 1]);
        let mut f = vec![0x1a, 0x45, 0xdf, 0xa3, 0x80 | header.len() as u8];
        f.extend(header);
        f.extend_from_slice(&[0x18, 0x53, 0x80, 0x67, 0x01, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff]);
        f
    }
    fn ok(name: &str, bytes: &[u8]) -> Result<Container, Error> {
        check(name, bytes, bytes.len() as u64)
    }

    #[test]
    fn each_accepted_container_is_recognised_and_its_extension_agrees() {
        let mut opus = ogg_page(0x02, 0, &opus_head(1, 0));
        opus.extend(ogg_page(0, 0, b"OpusTags"));
        assert_eq!(ok("voice.ogg", &opus), Ok(Container::Ogg));
        assert_eq!(ok("voice.opus", &opus), Ok(Container::Ogg), ".opus is Ogg Opus");
        assert_eq!(ok("a.oga", &ogg_page(0x02, 0, &vorbis_id(2, 44_100))), Ok(Container::Ogg));
        assert_eq!(ok("a.wav", &wav_file(1, 2, 44_100, 16, 4410)), Ok(Container::Wav));
        assert_eq!(ok("a.wav", &wav_file(3, 1, 48_000, 32, 4800)), Ok(Container::Wav), "float");
        assert_eq!(ok("a.mp3", &mp3_frames(3)), Ok(Container::Mp3));
        let mut tagged = b"ID3\x04\x00\x00\x00\x00\x00\x10".to_vec();
        tagged.resize(26, 0);
        tagged.extend(mp3_frames(3));
        assert_eq!(ok("a.mp3", &tagged), Ok(Container::Mp3), "after an ID3v2 tag");
        assert_eq!(ok("a.m4a", &mp4(b"M4A ", &[])), Ok(Container::M4a));
        assert_eq!(ok("a.webm", &webm_file(b"webm")), Ok(Container::Webm));
        // A name with no known extension is judged by its bytes alone.
        assert_eq!(ok("voice-note", &mp3_frames(3)), Ok(Container::Mp3));
        assert_eq!(ok("clip.bin", &wav_file(1, 1, 16_000, 16, 320)), Ok(Container::Wav));
    }

    #[test]
    fn a_file_name_that_disagrees_with_the_bytes_is_refused() {
        assert_eq!(ok("song.mp3", &wav_file(1, 1, 8_000, 16, 160)), Err(Error::ExtensionMismatch));
        assert_eq!(ok("song.wav", &mp3_frames(3)), Err(Error::ExtensionMismatch));
        assert_eq!(ok("song.m4a", &webm_file(b"webm")), Err(Error::ExtensionMismatch));
        assert_eq!(ok("voice.ogg", &mp4(b"M4A ", &[])), Err(Error::ExtensionMismatch));
    }

    #[test]
    fn empty_oversize_and_unknown_files_are_refused_first() {
        assert_eq!(check("a.mp3", &[], 0), Err(Error::Empty));
        assert_eq!(check("a.mp3", &mp3_frames(3), MAX_FILE_BYTES + 1), Err(Error::TooLarge));
        for bytes in [&b"GIF89a......"[..], b"\x7fELF\x02\x01\x01\x00", b"MZ\x90\x00", b"%PDF-1.7", b"<html>", b"PK\x03\x04", b"fLaC\x00\x00\x00\x22", b"MThd\x00\x00\x00\x06"] {
            assert_eq!(ok("a.ogg", bytes), Err(Error::Unsupported), "{bytes:?}");
        }
        assert_eq!(ok("a.mp3", &[0u8; 64]), Err(Error::Unsupported), "silence is not a container");
    }

    #[test]
    fn ogg_must_be_a_single_vorbis_or_opus_stream_with_a_sane_first_page() {
        let page = |packet: Vec<u8>| ogg_page(0x02, 0, &packet);
        assert_eq!(ok("a.ogg", &page(b"\x7fFLAC\x01\x00".to_vec())), Err(Error::Unsupported), "FLAC");
        assert_eq!(ok("a.ogg", &page(b"\x80theora\x03\x02\x01".to_vec())), Err(Error::Unsupported), "video");
        assert_eq!(ok("a.ogg", &page(b"Speex   \x00".to_vec())), Err(Error::Unsupported), "Speex");
        assert!(matches!(ok("a.ogg", &page(opus_head(6, 1))), Err(Error::Malformed(_))), "surround Opus");
        assert!(matches!(ok("a.ogg", &page(opus_head(1, 1))), Err(Error::Malformed(_))), "mapping family 1");
        assert!(matches!(ok("a.ogg", &page(vorbis_id(2, 0))), Err(Error::Malformed(_))), "zero sample rate");
        assert!(matches!(ok("a.ogg", &page(vorbis_id(0, 44_100))), Err(Error::Malformed(_))), "no channels");
        assert!(matches!(ok("a.ogg", &page(vorbis_id(2, 4_000_000_000))), Err(Error::Malformed(_))), "absurd rate");
        assert!(matches!(ok("a.ogg", &ogg_page(0x02, 5, &opus_head(1, 0))), Err(Error::Malformed(_))), "granule must be zero");
        assert!(matches!(ok("a.ogg", &ogg_page(0x03, 0, &opus_head(1, 0))), Err(Error::Malformed(_))), "a first page cannot continue");
        assert!(matches!(ok("a.ogg", &ogg_page(0x00, 0, &opus_head(1, 0))), Err(Error::Malformed(_))), "no beginning of stream");
        // Audio plus a second stream (the way video is muxed in) is refused.
        let mut two = ogg_page(0x02, 0, &opus_head(1, 0));
        two.extend(ogg_page(0x02, 0, &vorbis_id(2, 44_100)));
        assert_eq!(ok("a.ogg", &two), Err(Error::Unsupported));
        // Truncated and corrupt page headers.
        let full = ogg_page(0x02, 0, &opus_head(1, 0));
        for cut in [5, 20, 27, 30, full.len() - 3] {
            assert!(ok("a.ogg", &full[..cut]).is_err(), "cut at {cut}");
        }
        let mut version = full.clone();
        version[4] = 1;
        assert!(matches!(ok("a.ogg", &version), Err(Error::Malformed(_))));
    }

    #[test]
    fn wav_must_be_plain_pcm_or_float_with_consistent_numbers() {
        assert_eq!(ok("a.wav", &wav_file(2, 1, 8_000, 16, 160)), Err(Error::Unsupported), "ADPCM");
        assert_eq!(ok("a.wav", &wav_file(0x55, 2, 44_100, 16, 400)), Err(Error::Unsupported), "MP3 inside WAV");
        for (channels, rate, bits) in [(0, 44_100, 16), (6, 44_100, 16), (2, 0, 16), (2, 7_999, 16), (2, 96_001, 16), (2, 44_100, 12), (2, 44_100, 0)] {
            assert!(matches!(ok("a.wav", &wav_file(1, channels, rate, bits, 64)), Err(Error::Malformed(_))), "{channels} ch {rate} Hz {bits} bit");
        }
        assert!(matches!(ok("a.wav", &wav_file(3, 1, 44_100, 16, 64)), Err(Error::Malformed(_))), "float must be 32 bit");
        // Block align and byte rate must agree with the rest.
        let mut skewed = wav_file(1, 2, 44_100, 16, 64);
        skewed[32] = 9;
        assert!(matches!(ok("a.wav", &skewed), Err(Error::Malformed(_))));
        // A data chunk that claims far more than the file holds (a classic allocation attack).
        let mut huge = wav_file(1, 2, 44_100, 16, 64);
        huge[40..44].copy_from_slice(&0xffff_fff0u32.to_le_bytes());
        assert!(matches!(ok("a.wav", &huge), Err(Error::Malformed(_))));
        // Format chunk sizes other than the three real ones, and truncation.
        let mut odd = wav_file(1, 2, 44_100, 16, 64);
        odd[16..20].copy_from_slice(&17u32.to_le_bytes());
        assert!(ok("a.wav", &odd).is_err());
        for cut in [4, 11, 20, 30, 43] {
            assert!(ok("a.wav", &wav_file(1, 2, 44_100, 16, 64)[..cut]).is_err(), "cut at {cut}");
        }
        assert_eq!(ok("a.wav", b"RIFF\x24\x00\x00\x00AVI LIST"), Err(Error::Unsupported), "RIFF but not WAVE");
    }

    #[test]
    fn mp3_must_be_layer_iii_with_frames_that_chain() {
        assert_eq!(ok("a.mp3", &mp3_frames(1)), Ok(Container::Mp3), "a single frame is a valid file");
        // Layer II (0xFD) and Layer I (0xFF), a reserved version, free format, bad bitrate and rate.
        for (label, header) in [("layer II", [0xff, 0xfd, 0x90, 0x00]), ("layer I", [0xff, 0xff, 0x90, 0x00]), ("reserved version", [0xff, 0xeb, 0x90, 0x00]), ("free format", [0xff, 0xfb, 0x00, 0x00]), ("bad bitrate", [0xff, 0xfb, 0xf0, 0x00]), ("reserved rate", [0xff, 0xfb, 0x9c, 0x00])] {
            let mut bytes = mp3_frames(3);
            bytes[..4].copy_from_slice(&header);
            assert!(ok("a.mp3", &bytes).is_err(), "{label}");
        }
        // A sync word followed by garbage where the next frame should be.
        let mut broken = mp3_frames(3);
        broken[417] = 0;
        assert!(matches!(ok("a.mp3", &broken), Err(Error::Malformed(_))));
        // Tags: bad version, a non-syncsafe size, unknown flags, and one that swallows the file.
        for (label, tag) in [
            ("version", b"ID3\x09\x00\x00\x00\x00\x00\x10".to_vec()),
            ("syncsafe", b"ID3\x04\x00\x00\x80\x00\x00\x10".to_vec()),
            ("flags", b"ID3\x04\x00\x0f\x00\x00\x00\x10".to_vec()),
            ("whole file", b"ID3\x04\x00\x00\x00\x7f\x7f\x7f".to_vec()),
        ] {
            let mut bytes = tag;
            bytes.extend(mp3_frames(3));
            assert!(matches!(ok("a.mp3", &bytes), Err(Error::Malformed(_))), "{label}");
        }
        assert!(matches!(ok("a.mp3", &[0xff, 0xfb]), Err(Error::Malformed(_))), "a lone sync word");
        // Random bytes that begin with a sync word.
        let noise: Vec<u8> = (0..4000u32).map(|i| (i.wrapping_mul(2_654_435_761) >> 13) as u8).collect();
        let mut bytes = vec![0xff, 0xfb, 0x90, 0x00];
        bytes.extend(noise);
        assert!(ok("a.mp3", &bytes).is_err());
    }

    #[test]
    fn mp4_boxes_must_fit_the_file_and_the_brand_must_be_an_audio_one() {
        assert_eq!(ok("a.m4a", &mp4(b"M4A ", &[])), Ok(Container::M4a));
        assert_eq!(ok("a.m4a", &mp4(b"mp42", &[])), Ok(Container::M4a));
        assert_eq!(ok("a.m4a", &mp4(b"heic", &[])), Err(Error::Unsupported), "an image brand");
        assert_eq!(ok("a.m4a", &mp4(b"qt  ", &[])), Err(Error::Unsupported), "QuickTime video");
        // A box whose size runs past the file, a 64-bit size that overflows, and a size below header.
        let mut over = mp4(b"M4A ", &[]);
        over.extend_from_slice(&u32::MAX.to_be_bytes());
        over.extend_from_slice(b"moov");
        assert!(matches!(ok("a.m4a", &over), Err(Error::Malformed(_))));
        let mut wide = mp4(b"M4A ", &[]);
        wide.extend_from_slice(&1u32.to_be_bytes());
        wide.extend_from_slice(b"mdat");
        wide.extend_from_slice(&u64::MAX.to_be_bytes());
        assert!(matches!(ok("a.m4a", &wide), Err(Error::Malformed(_))), "overflowing 64-bit size");
        let mut tiny = mp4(b"M4A ", &[]);
        tiny.extend_from_slice(&4u32.to_be_bytes());
        tiny.extend_from_slice(b"free");
        assert!(matches!(ok("a.m4a", &tiny), Err(Error::Malformed(_))), "a box smaller than its own header");
        let mut huge_ftyp = mp4(b"M4A ", &[]);
        huge_ftyp[..4].copy_from_slice(&4000u32.to_be_bytes());
        assert!(ok("a.m4a", &huge_ftyp).is_err(), "an enormous ftyp");
        assert!(ok("a.m4a", &mp4(b"M4A ", &[])[..10]).is_err());
    }

    #[test]
    fn webm_must_declare_webm_and_sane_ebml_limits() {
        assert_eq!(ok("a.webm", &webm_file(b"webm")), Ok(Container::Webm));
        assert_eq!(ok("a.webm", &webm_file(b"matroska")), Err(Error::Unsupported), "Matroska can hold video and attachments");
        assert_eq!(ok("a.webm", &webm_file(b"webm\0")), Ok(Container::Webm), "GStreamer pads the DocType with a NUL byte");
        assert_eq!(ok("a.webm", &webm_file(b"webm\0\0\0")), Ok(Container::Webm));
        assert_eq!(ok("a.webm", &webm_file(b"web\0m")), Err(Error::Unsupported), "a NUL inside the string is not padding");
        assert_eq!(ok("a.webm", &webm_file(b"\0")), Err(Error::Unsupported));
        assert!(ok("a.webm", &webm_file(b"webm")[..9]).is_err());
        let mut no_segment = webm_file(b"webm");
        let at = no_segment.len() - 12;
        no_segment[at] = 0x00;
        assert!(matches!(ok("a.webm", &no_segment), Err(Error::Malformed(_))));
        // An EBML header that claims to be enormous.
        let mut big = vec![0x1a, 0x45, 0xdf, 0xa3, 0x01, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff];
        big.extend_from_slice(&[0u8; 32]);
        assert!(matches!(ok("a.webm", &big), Err(Error::Malformed(_))));
        let mut bad_version = webm_file(b"webm");
        let pos = bad_version.windows(3).position(|w| w == [0x42, 0x86, 0x81]).unwrap();
        bad_version[pos + 3] = 2;
        assert!(matches!(ok("a.webm", &bad_version), Err(Error::Malformed(_))));
    }

    #[test]
    fn every_possible_ebml_length_prefix_is_handled() {
        // The first byte of an EBML integer decides its length (1 to 8 bytes, or invalid); a crafted
        // file can use any of them for the header size, the id and the child sizes.
        for first in 0..=255u8 {
            for fill in [0x00u8, 0x01, 0x7f, 0xff] {
                let mut sized = vec![0x1a, 0x45, 0xdf, 0xa3, first];
                sized.resize(64, fill);
                let _ = check("a.webm", &sized, 64);
                let mut child = vec![0x1a, 0x45, 0xdf, 0xa3, 0x8f, 0x42, 0x82, first];
                child.resize(64, fill);
                let _ = check("a.webm", &child, 64);
                let mut id = vec![0x1a, 0x45, 0xdf, 0xa3, 0x8f, first, 0x81];
                id.resize(64, fill);
                let _ = check("a.webm", &id, 64);
            }
        }
    }

    #[test]
    fn no_input_makes_the_checks_panic() {
        // Every prefix of every valid file, with every single byte corrupted in turn: refused or
        // accepted, never a panic or an out-of-range read.
        let files: Vec<(&str, Vec<u8>)> = vec![
            ("a.ogg", [ogg_page(0x02, 0, &opus_head(2, 0)), ogg_page(0, 0, b"OpusTags")].concat()),
            ("a.ogg", ogg_page(0x02, 0, &vorbis_id(2, 44_100))),
            ("a.wav", wav_file(1, 2, 44_100, 16, 64)),
            ("a.mp3", mp3_frames(3)),
            ("a.m4a", mp4(b"M4A ", &[])),
            ("a.webm", webm_file(b"webm")),
        ];
        for (name, bytes) in &files {
            for cut in 0..=bytes.len() {
                let _ = check(name, &bytes[..cut], bytes.len() as u64);
            }
            for at in 0..bytes.len() {
                for value in [0x00, 0x7f, 0x80, 0xff] {
                    let mut mutated = bytes.clone();
                    mutated[at] = value;
                    let _ = check(name, &mutated, bytes.len() as u64);
                }
            }
        }
        // And arbitrary bytes behind each magic number.
        for magic in [&b"OggS"[..], b"RIFF", b"\x00\x00\x00\x20ftyp", b"\x1a\x45\xdf\xa3", b"ID3", b"\xff\xfb"] {
            for fill in [0x00u8, 0xff, 0x55, 0x80] {
                let mut bytes = magic.to_vec();
                bytes.resize(HEAD_BYTES + 100, fill);
                let _ = check("x", &bytes, bytes.len() as u64);
            }
        }
    }
}
