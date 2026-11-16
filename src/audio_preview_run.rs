//! Preparing and playing audio through the confined `monogram-audio-preview` helper.
//!
//! The main process never parses or decodes audio. For a file it has opened it
//!   1. checks the header with the strict `audio_check` (cheap, and refuses most crafted files),
//!   2. starts the helper in bubblewrap with the file passed as a descriptor (the helper sees a
//!      copy of exactly these bytes, and re-checks them),
//!   3. treats everything the helper says as untrusted: its reply is bounded JSON that is
//!      validated again, and playback data is raw PCM in one fixed format.
//! Callers run this on worker threads; it blocks until the helper answers.
use crate::{
    audio_check::{self, Container, MAX_DURATION_MS},
    confined::{self, Input},
};
use std::{
    fs::File,
    io::Read,
    os::{fd::AsRawFd, unix::fs::FileExt},
    path::PathBuf,
    process::{Child, ChildStdout},
    time::{Duration, Instant},
};

const HELPER: &str = "monogram-audio-preview";
/// A probe decodes the whole file; the helper stops itself at 60 s, this is the outside deadline.
const PROBE_DEADLINE: Duration = Duration::from_secs(75);
pub const BARS: usize = 64;
/// Output format of the PCM stream: signed 16-bit little-endian, 48 kHz, two channels.
pub const PCM_RATE: u32 = 48_000;
pub const PCM_CHANNELS: u32 = 2;
pub const PCM_BYTES_PER_MS: u64 = 192;

/// Plugin files the audio helper needs. `libgstlibav.so` (FFmpeg) is only mounted when the
/// smaller MP3 or AAC decoders are not installed, to keep the decoder surface minimal.
const PLUGINS: &[&str] = &[
    "libgstcoreelements.so",
    "libgstapp.so",
    "libgstogg.so",
    "libgstvorbis.so",
    "libgstopus.so",
    "libgstaudioparsers.so",
    "libgstmpg123.so",
    "libgstfaad.so",
    "libgstwavparse.so",
    "libgstisomp4.so",
    "libgstmatroska.so",
    "libgstaudioconvert.so",
    "libgstaudioresample.so",
];
const FALLBACK_DECODERS: &str = "libgstlibav.so";

/// At most this many files are prepared at once. Preparing decodes a whole file in its own helper
/// process (CPU, and a memory copy of the file), and the work runs on the shared I/O thread pool,
/// so a flood of audio messages scrolling into view must queue rather than start a crowd.
const PREPARE_AT_ONCE: usize = 2;

/// Wait for one of the preparation places. Hold the permit for as long as the helper runs, and wait
/// for it *before* taking a pool thread, so queued files do not occupy the pool.
pub(crate) async fn preparation_permit() -> tokio::sync::OwnedSemaphorePermit {
    use std::sync::{Arc, OnceLock};
    static PLACES: OnceLock<Arc<tokio::sync::Semaphore>> = OnceLock::new();
    PLACES
        .get_or_init(|| Arc::new(tokio::sync::Semaphore::new(PREPARE_AT_ONCE)))
        .clone()
        .acquire_owned()
        .await
        .expect("the preparation semaphore stays open")
}

#[derive(Debug, PartialEq, Eq)]
pub enum Error {
    /// The helper or bubblewrap is not installed.
    Unavailable,
    /// The strict check refused the file.
    Refused(audio_check::Error),
    /// The helper refused or could not decode the file.
    Failed,
    TimedOut,
    Cancelled,
    /// The helper's reply was malformed or out of range.
    BadOutput,
    /// A GStreamer plugin this format needs is not installed. FFmpeg (gst-libav) is used only when
    /// the system already has it; nothing is bundled for this.
    MissingDecoder(&'static str),
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unavailable => f.write_str("Audio preview is unavailable"),
            Self::Refused(error) => error.fmt(f),
            Self::Failed | Self::BadOutput => f.write_str("This audio file could not be played"),
            Self::MissingDecoder(needs) => write!(f, "Can't play this: it needs {needs}, which is not installed"),
            Self::TimedOut => f.write_str("Preparing this audio took too long"),
            Self::Cancelled => f.write_str("Audio preparation was cancelled"),
        }
    }
}
impl std::error::Error for Error {}

/// What the helper learned by decoding the file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Preview {
    pub container: Container,
    pub duration_ms: u64,
    /// `BARS` waveform heights, 0 to 255.
    pub peaks: Vec<u8>,
}

fn have_plugin(dirs: &[PathBuf], name: &str) -> bool {
    dirs.iter().any(|dir| dir.join(name).is_file())
}

fn plugin_names(dirs: &[PathBuf]) -> Vec<&'static str> {
    let mut names = PLUGINS.to_vec();
    if !(have_plugin(dirs, "libgstmpg123.so") && have_plugin(dirs, "libgstfaad.so")) {
        names.push(FALLBACK_DECODERS);
    }
    names
}

/// What is missing for this container, if anything. MP3 and AAC have a small decoder and an FFmpeg
/// one; FFmpeg is used only if the system has it installed, and nothing is bundled for audio. The
/// other formats' decoders come with GStreamer's base plugins, and the helper reports them (exit
/// code 8) if they are absent too.
fn missing_decoder(container: Container, dirs: &[PathBuf]) -> Option<&'static str> {
    let (small, hint) = match container {
        Container::Mp3 => ("libgstmpg123.so", "the GStreamer mpg123 plugin or FFmpeg (gst-libav)"),
        Container::M4a => ("libgstfaad.so", "the GStreamer faad plugin or FFmpeg (gst-libav)"),
        _ => return None,
    };
    (!have_plugin(dirs, small) && !have_plugin(dirs, FALLBACK_DECODERS)).then_some(hint)
}
/// What the helper means by exit code 8 when the container is one with no pre-check.
const SOME_DECODER: &str = "a GStreamer audio decoder";

/// Read the start of the file without moving its offset.
fn head(file: &File) -> Result<(Vec<u8>, u64), Error> {
    let length = file.metadata().map_err(|_| Error::Failed)?.len();
    let mut head = vec![0u8; audio_check::HEAD_BYTES.min(length as usize)];
    let mut filled = 0;
    while filled < head.len() {
        match file.read_at(&mut head[filled..], filled as u64) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => return Err(Error::Failed),
        }
    }
    head.truncate(filled);
    Ok((head, length))
}

fn spawn(file: &File, args: &[&str]) -> Result<Child, Error> {
    let (Some(helper), Some(bwrap)) = (confined::helper(HELPER, "MONOGRAM_AUDIO_PREVIEW_HELPER"), confined::bwrap()) else {
        return Err(Error::Unavailable);
    };
    // Each launch gets its own open file description at offset 0. A `try_clone` would share the
    // read offset with every other launch, and two helpers started together would each receive
    // only part of the file. Reopening through /proc/self/fd gives an independent description of
    // the same, already verified inode.
    let copy = File::open(format!("/proc/self/fd/{}", file.as_raw_fd()))
        .or_else(|_| {
            let clone = file.try_clone()?;
            rewind(&clone).map_err(|_| std::io::Error::other("rewind failed"))?;
            Ok::<_, std::io::Error>(clone)
        })
        .map_err(|_| Error::Unavailable)?;
    let dirs = confined::plugin_dirs();
    let plugins = confined::plugin_files(&plugin_names(&dirs), &dirs);
    let mut command = confined::sandbox_command(&bwrap, &helper, Input::Fd(copy.as_raw_fd()), None, &plugins, args);
    command.spawn().map_err(|_| Error::Unavailable)
}
fn rewind(file: &File) -> Result<(), Error> {
    use std::io::Seek;
    (&*file).rewind().map_err(|_| Error::Failed)
}

/// Check the file strictly, then have the helper decode it to learn its duration and waveform.
pub fn probe(file: &File, name: &str, cancel: &dyn Fn() -> bool) -> Result<Preview, Error> {
    let (head, length) = head(file)?;
    let container = audio_check::check(name, &head, length).map_err(Error::Refused)?;
    // Say what is missing before starting anything.
    if let Some(needs) = missing_decoder(container, &confined::plugin_dirs()) {
        return Err(Error::MissingDecoder(needs));
    }
    let mut child = spawn(file, &["probe", container.name()])?;
    drop(child.stdin.take());
    let mut stdout = child.stdout.take().ok_or(Error::Failed)?;
    let reader = std::thread::spawn(move || {
        let mut reply = Vec::new();
        // One short JSON line; anything beyond this is not read.
        let _ = (&mut stdout).take(8 * 1024).read_to_end(&mut reply);
        reply
    });
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().map_err(|_| Error::Failed)? {
            break status;
        }
        let stop = if cancel() {
            Some(Error::Cancelled)
        } else if started.elapsed() > PROBE_DEADLINE {
            Some(Error::TimedOut)
        } else {
            None
        };
        if let Some(error) = stop {
            let _ = child.kill();
            let _ = child.wait();
            return Err(error);
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let reply = reader.join().map_err(|_| Error::Failed)?;
    match status.code() {
        Some(8) => return Err(Error::MissingDecoder(SOME_DECODER)),
        _ if !status.success() => return Err(Error::Failed),
        _ => {}
    }
    parse_probe(&reply, container)
}

/// Validate the helper's reply. It is untrusted data: bounded, no unknown fields, every number
/// checked against the limits, and it must describe the container that was checked.
pub fn parse_probe(reply: &[u8], expected: Container) -> Result<Preview, Error> {
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Reply {
        container: String,
        duration_ms: u64,
        rate: u32,
        channels: u32,
        peaks: Vec<u8>,
    }
    let reply: Reply = serde_json::from_slice(reply).map_err(|_| Error::BadOutput)?;
    let valid = reply.container == expected.name()
        && (1..=MAX_DURATION_MS).contains(&reply.duration_ms)
        && (audio_check::MIN_RATE..=audio_check::MAX_RATE).contains(&reply.rate)
        && (1..=audio_check::MAX_CHANNELS).contains(&reply.channels)
        && reply.peaks.len() == BARS;
    if !valid {
        return Err(Error::BadOutput);
    }
    Ok(Preview { container: expected, duration_ms: reply.duration_ms, peaks: reply.peaks })
}

/// Raw PCM from the helper, from `start_ms` to the end. Dropping it stops the helper.
pub struct PcmStream {
    child: Option<Child>,
    out: ChildStdout,
}
impl PcmStream {
    pub fn start(file: &File, container: Container, start_ms: u64) -> Result<Self, Error> {
        if start_ms > MAX_DURATION_MS {
            return Err(Error::BadOutput);
        }
        let start = start_ms.to_string();
        let mut child = spawn(file, &["pcm", container.name(), &start])?;
        drop(child.stdin.take());
        let out = child.stdout.take().ok_or(Error::Failed)?;
        Ok(Self { child: Some(child), out })
    }
}
impl Read for PcmStream {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        self.out.read(buffer)
    }
}
impl Drop for PcmStream {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            // Reaping must never make the GTK thread wait.
            std::thread::spawn(move || {
                let _ = child.wait();
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reply(extra: &str) -> Vec<u8> {
        let peaks: Vec<u8> = (0..BARS as u8).collect();
        format!(r#"{{"container":"ogg","duration_ms":1000,"rate":48000,"channels":2,"peaks":{peaks:?}{extra}}}"#).into_bytes()
    }

    #[test]
    fn a_good_reply_is_accepted() {
        let preview = parse_probe(&reply(""), Container::Ogg).unwrap();
        assert_eq!((preview.duration_ms, preview.peaks.len(), preview.container), (1000, BARS, Container::Ogg));
    }

    #[test]
    fn a_reply_from_a_misbehaving_helper_is_refused_field_by_field() {
        let good = String::from_utf8(reply("")).unwrap();
        for (label, bad) in [
            ("wrong container", good.replace("\"ogg\"", "\"mp3\"")),
            ("zero duration", good.replace("1000", "0")),
            ("over the limit", good.replace("1000", &(MAX_DURATION_MS + 1).to_string())),
            ("negative duration", good.replace("1000", "-5")),
            ("duration beyond u64", good.replace("1000", "99999999999999999999999")),
            ("zero rate", good.replace("48000", "0")),
            ("huge rate", good.replace("48000", "4000000000")),
            ("six channels", good.replace("\"channels\":2", "\"channels\":6")),
            ("short waveform", good.replace(&format!("{:?}", (0..BARS as u8).collect::<Vec<_>>()), "[1,2,3]")),
            ("unknown field", good.replace('}', ",\"cmd\":\"x\"}")),
            ("not JSON", "hello".into()),
            ("empty", String::new()),
            ("array", "[]".into()),
            ("peak out of byte range", good.replace("\"peaks\":[0,", "\"peaks\":[999,")),
        ] {
            assert_eq!(parse_probe(bad.as_bytes(), Container::Ogg), Err(Error::BadOutput), "{label}");
        }
    }

    #[test]
    fn the_ffmpeg_decoder_plugin_is_mounted_only_when_the_small_ones_are_missing() {
        let root = std::env::temp_dir().join(format!("audio-plugins-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let dirs = [root.clone()];
        assert!(plugin_names(&dirs).contains(&FALLBACK_DECODERS), "neither decoder installed");
        std::fs::write(root.join("libgstmpg123.so"), b"x").unwrap();
        assert!(plugin_names(&dirs).contains(&FALLBACK_DECODERS), "AAC decoder still missing");
        std::fs::write(root.join("libgstfaad.so"), b"x").unwrap();
        assert!(!plugin_names(&dirs).contains(&FALLBACK_DECODERS), "both small decoders present");
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn mp3_and_aac_need_a_decoder_but_ffmpeg_is_only_used_when_it_is_already_installed() {
        let root = std::env::temp_dir().join(format!("audio-decoders-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let dirs = [root.clone()];
        let say = |container| missing_decoder(container, &dirs);
        // Nothing installed: MP3 and M4A name what is missing, and FFmpeg is one of the answers.
        let mp3 = say(Container::Mp3).expect("MP3 needs a decoder");
        assert!(mp3.contains("mpg123") && mp3.contains("FFmpeg"), "{mp3}");
        assert!(say(Container::M4a).is_some_and(|hint| hint.contains("faad") && hint.contains("FFmpeg")));
        // Ogg, WAV and WebM need no FFmpeg or MP3 decoder.
        for container in [Container::Ogg, Container::Wav, Container::Webm] {
            assert_eq!(say(container), None, "{container:?}");
        }
        // The small decoder alone is enough for its own format, not for the other.
        std::fs::write(root.join("libgstmpg123.so"), b"x").unwrap();
        assert_eq!((say(Container::Mp3), say(Container::M4a).is_some()), (None, true));
        // FFmpeg, if the system has it, covers both.
        std::fs::write(root.join("libgstlibav.so"), b"x").unwrap();
        assert_eq!((say(Container::Mp3), say(Container::M4a)), (None, None));
        std::fs::remove_dir_all(&root).unwrap();
        // The message names the missing piece in plain words.
        assert_eq!(
            Error::MissingDecoder("FFmpeg (gst-libav)").to_string(),
            "Can't play this: it needs FFmpeg (gst-libav), which is not installed"
        );
    }

    #[test]
    fn the_head_is_read_without_moving_the_files_offset() {
        use std::io::{Read as _, Seek};
        let path = std::env::temp_dir().join(format!("audio-head-{}", uuid::Uuid::new_v4()));
        std::fs::write(&path, vec![7u8; 100_000]).unwrap();
        let mut file = File::open(&path).unwrap();
        file.seek(std::io::SeekFrom::Start(40)).unwrap();
        let (bytes, length) = head(&file).unwrap();
        assert_eq!((bytes.len(), length), (audio_check::HEAD_BYTES, 100_000));
        let mut next = [0u8; 1];
        file.read_exact(&mut next).unwrap();
        assert_eq!(file.stream_position().unwrap(), 41, "reading the head did not move the offset");
        std::fs::remove_file(path).unwrap();
    }

    // ---- the real helper, in the real sandbox: build it first with `cargo build --bins` ----

    const NEVER: &dyn Fn() -> bool = &|| false;
    fn fixture(name: &str) -> File {
        File::open(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/audio").join(name)).unwrap()
    }

    #[test]
    #[ignore = "requires the built helper, GStreamer plugins and real bubblewrap namespaces"]
    fn every_supported_container_is_probed_in_the_sandbox() {
        // (file, name, container, expected duration in ms)
        for (file, container, expected) in [
            ("tone-opus.ogg", Container::Ogg, 1000),
            ("tone-vorbis.oga", Container::Ogg, 1000),
            // MP3 encoders add padding (here about 120 ms) and the decoder reports it faithfully.
            ("tone.mp3", Container::Mp3, 1060),
            ("tone.wav", Container::Wav, 300),
            ("tone.m4a", Container::M4a, 1000),
            ("tone-opus.webm", Container::Webm, 1000),
        ] {
            let preview = probe(&fixture(file), file, NEVER).unwrap_or_else(|e| panic!("{file}: {e:?}"));
            assert_eq!(preview.container, container, "{file}");
            assert!(preview.duration_ms.abs_diff(expected) <= 120, "{file}: {} ms, expected about {expected}", preview.duration_ms);
            assert!(preview.duration_ms >= 250, "{file}: a real tone has a real length");
            assert_eq!(preview.peaks.len(), BARS);
            assert!(preview.peaks.iter().any(|p| *p > 200), "{file}: a tone must reach the top of the waveform");
        }
    }

    #[test]
    #[ignore = "requires the built helper, GStreamer plugins and real bubblewrap namespaces"]
    fn pcm_is_the_fixed_format_starts_where_asked_and_carries_the_signal() {
        let file = fixture("tone.wav");
        let read_all = |start| {
            let mut bytes = Vec::new();
            PcmStream::start(&file, Container::Wav, start).unwrap().read_to_end(&mut bytes).unwrap();
            bytes
        };
        let whole = read_all(0);
        // 0.3 s of 48 kHz stereo 16-bit.
        assert!(whole.len().abs_diff(300 * PCM_BYTES_PER_MS as usize) <= 2 * 960 * 4, "{} bytes", whole.len());
        assert_eq!(whole.len() % 4, 0, "whole frames only");
        let loud = whole.chunks_exact(2).map(|s| i16::from_le_bytes([s[0], s[1]]).unsigned_abs()).max().unwrap();
        assert!(loud > 3000, "the tone is audible: peak {loud}");
        let later = read_all(100);
        assert!(later.len() < whole.len() && whole.len() - later.len() >= 90 * PCM_BYTES_PER_MS as usize, "starting at 100 ms drops about 100 ms: {} vs {}", later.len(), whole.len());
        assert_eq!(read_all(10_000).len(), 0, "starting past the end yields nothing");
    }

    #[test]
    #[ignore = "requires the built helper, GStreamer plugins and real bubblewrap namespaces"]
    fn video_second_streams_other_codecs_and_surround_are_refused() {
        // Refused by the strict check, before any helper starts.
        for (file, why) in [("video-theora.ogg", "Ogg video"), ("tone-flac.ogg", "Ogg FLAC"), ("surround.wav", "6 channels")] {
            assert!(matches!(probe(&fixture(file), file, NEVER), Err(Error::Refused(_))), "{file}: {why}");
        }
        // WebM with only video passes the container check and is refused by the helper.
        assert_eq!(probe(&fixture("video-only.webm"), "video-only.webm", NEVER), Err(Error::Failed), "WebM without audio");
        // The name must agree with the bytes.
        assert_eq!(probe(&fixture("tone.mp3"), "tone.wav", NEVER), Err(Error::Refused(audio_check::Error::ExtensionMismatch)));
    }

    /// Write `bytes` to a temp file and open it.
    fn temp(bytes: &[u8]) -> File {
        let path = std::env::temp_dir().join(format!("audio-{}", uuid::Uuid::new_v4()));
        std::fs::write(&path, bytes).unwrap();
        let file = File::open(&path).unwrap();
        std::fs::remove_file(path).unwrap();
        file
    }

    #[test]
    #[ignore = "requires the built helper, GStreamer plugins and real bubblewrap namespaces"]
    fn crafted_files_are_refused_or_fail_quickly_never_hang_or_succeed() {
        let started = Instant::now();
        let tone = std::fs::read(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/audio/tone-opus.ogg")).unwrap();
        let mp3 = std::fs::read(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/audio/tone.mp3")).unwrap();
        let wav = std::fs::read(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/audio/tone.wav")).unwrap();
        let mut cases: Vec<(&str, Vec<u8>)> = vec![
            ("empty.ogg", vec![]),
            ("text.mp3", b"this is not audio at all".to_vec()),
            ("zeros.wav", vec![0u8; 10_000]),
            ("noise.ogg", (0..20_000u32).map(|i| (i.wrapping_mul(2_654_435_761) >> 11) as u8).collect()),
            ("truncated.ogg", tone[..60].to_vec()),
            ("truncated2.mp3", mp3[..600].to_vec()),
            ("cut.wav", wav[..30].to_vec()),
        ];
        // Valid header, then garbage where the audio should be.
        let mut hollow = tone[..tone.len().min(80)].to_vec();
        hollow.extend(vec![0xa5u8; 4000]);
        cases.push(("hollow.ogg", hollow));
        let mut hollow_mp3 = mp3[..417].to_vec();
        hollow_mp3.extend(vec![0xffu8; 3000]);
        cases.push(("hollow.mp3", hollow_mp3));
        // Layer II: the first header changed.
        let mut layer2 = mp3.clone();
        layer2[1] = 0xfd;
        cases.push(("layer2.mp3", layer2));
        // A WAV whose data chunk claims more than the file holds.
        let mut greedy = wav.clone();
        greedy[40..44].copy_from_slice(&u32::MAX.to_le_bytes());
        cases.push(("greedy.wav", greedy));
        for (name, bytes) in cases {
            let result = probe(&temp(&bytes), name, NEVER);
            assert!(result.is_err(), "{name} must not be accepted, got {result:?}");
        }
        assert!(started.elapsed() < Duration::from_secs(60), "crafted files must fail fast");
    }

    /// Corrupt each real fixture in many deterministic ways and push every variant through the
    /// whole pipeline (strict check, sandbox, helper). Whatever the bytes, the answer is a clean
    /// result or a refusal, quickly: never a timeout, a hang or a crash of the caller.
    #[test]
    #[ignore = "requires the built helper, GStreamer plugins and real bubblewrap namespaces"]
    fn mutated_real_files_always_end_in_a_result_or_a_refusal_never_a_hang() {
        let started = Instant::now();
        let (mut accepted, mut refused) = (0u32, 0u32);
        let mut state = 0x2545_f491_4f6c_dd1du64;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        for (name, container_name) in [("tone-opus.ogg", "a.ogg"), ("tone-vorbis.oga", "a.oga"), ("tone.mp3", "a.mp3"), ("tone.wav", "a.wav"), ("tone.m4a", "a.m4a"), ("tone-opus.webm", "a.webm")] {
            let original = std::fs::read(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/audio").join(name)).unwrap();
            for round in 0..14u32 {
                let mut bytes = original.clone();
                match round % 7 {
                    // A few single bytes anywhere, a few in the first kilobyte (the headers).
                    0 | 1 => {
                        for _ in 0..4 {
                            let at = (next() as usize) % bytes.len();
                            bytes[at] = next() as u8;
                        }
                    }
                    2 | 3 => {
                        for _ in 0..4 {
                            let at = (next() as usize) % bytes.len().min(1024);
                            bytes[at] ^= 1 << (next() % 8);
                        }
                    }
                    // A run of zeros or of 0xFF, and a cut.
                    4 => {
                        let at = (next() as usize) % bytes.len();
                        let end = (at + 64 + (next() as usize) % 512).min(bytes.len());
                        bytes[at..end].fill(0);
                    }
                    5 => {
                        let at = (next() as usize) % bytes.len();
                        let end = (at + 64 + (next() as usize) % 512).min(bytes.len());
                        bytes[at..end].fill(0xff);
                    }
                    _ => bytes.truncate(1 + (next() as usize) % bytes.len()),
                }
                let one = Instant::now();
                let result = probe(&temp(&bytes), container_name, NEVER);
                assert_ne!(result, Err(Error::TimedOut), "{name} round {round}: must not run into the deadline");
                assert!(one.elapsed() < Duration::from_secs(30), "{name} round {round}: took {:?}", one.elapsed());
                match result {
                    Ok(preview) => {
                        // Whatever was accepted still obeys the limits.
                        assert!((1..=MAX_DURATION_MS).contains(&preview.duration_ms) && preview.peaks.len() == BARS, "{name} round {round}");
                        accepted += 1;
                    }
                    Err(_) => refused += 1,
                }
            }
        }
        eprintln!("mutation run: {accepted} still decoded, {refused} refused, in {:?}", started.elapsed());
        assert!(refused > 0 && accepted + refused == 6 * 14);
        assert!(started.elapsed() < Duration::from_secs(240));
    }

    #[test]
    #[ignore = "requires the built helper, GStreamer plugins and real bubblewrap namespaces"]
    fn a_file_longer_than_thirty_minutes_is_refused_after_decoding_not_trusted_from_its_header() {
        // 8 kHz mono 16-bit silence, 64 MiB minus a little: about 69 minutes, within the file-size
        // limit but far beyond the duration limit. The header is perfectly honest, so only the
        // decoder-side limit catches it.
        let mut wav = Vec::new();
        let data = 60 * 1024 * 1024u32;
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&(36 + data).to_le_bytes());
        wav.extend_from_slice(b"WAVEfmt ");
        wav.extend_from_slice(&16u32.to_le_bytes());
        for v in [1u16, 1] { wav.extend_from_slice(&v.to_le_bytes()); }
        wav.extend_from_slice(&8_000u32.to_le_bytes());
        wav.extend_from_slice(&16_000u32.to_le_bytes());
        for v in [2u16, 16] { wav.extend_from_slice(&v.to_le_bytes()); }
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&data.to_le_bytes());
        wav.resize(wav.len() + data as usize, 0);
        let started = Instant::now();
        assert_eq!(probe(&temp(&wav), "long.wav", NEVER), Err(Error::Failed));
        assert!(started.elapsed() < Duration::from_secs(70), "refused within the deadline: {:?}", started.elapsed());
    }

    /// The helper hardens itself exactly as in a real run, then tries calls it must never make.
    #[test]
    #[ignore = "requires the built helper (cargo build --bins)"]
    fn the_audio_helper_filter_really_refuses_dangerous_calls_but_not_ordinary_ones() {
        let helper = confined::helper(HELPER, "MONOGRAM_AUDIO_PREVIEW_HELPER").expect("built helper");
        let output = std::process::Command::new(helper).arg("selftest").output().unwrap();
        let report = String::from_utf8_lossy(&output.stdout);
        assert!(output.status.success(), "the filter let something through:\n{report}");
        for call in ["socket", "io_uring_setup", "bpf", "keyctl", "userfaultfd", "perf_event_open", "execve", "mount"] {
            assert!(report.contains(&format!("{call}: refused")), "{call} must be refused:\n{report}");
        }
        assert!(report.contains("ordinary calls and threads: work"), "{report}");
    }

    /// Helpers started at the same moment on one file each get the whole file. A duplicated
    /// descriptor shares its read offset with the original, so two launches reading the copy at once
    /// would each receive part of it.
    #[test]
    #[ignore = "requires the built helper, GStreamer plugins and real bubblewrap namespaces"]
    fn helpers_started_together_on_one_file_each_get_all_of_it() {
        let file = std::sync::Arc::new(fixture("tone.wav"));
        let lengths: Vec<usize> = (0..6)
            .map(|_| {
                let file = file.clone();
                std::thread::spawn(move || {
                    let mut bytes = Vec::new();
                    let mut stream = PcmStream::start(&file, Container::Wav, 0).expect("starts");
                    stream.read_to_end(&mut bytes).expect("reads");
                    bytes.len()
                })
            })
            .collect::<Vec<_>>()
            .into_iter()
            .map(|thread| thread.join().unwrap())
            .collect();
        assert!(lengths.iter().all(|len| *len > 40_000), "every helper produced the whole 0.3 s: {lengths:?}");
        assert!(lengths.windows(2).all(|pair| pair[0] == pair[1]), "and the same amount: {lengths:?}");
    }

    /// A format whose decoder plugin is not available inside the sandbox is reported as a missing
    /// decoder (exit code 8), not as a corrupt file. Here MP3 gets every plugin except a decoder.
    #[test]
    #[ignore = "requires the built helper and real bubblewrap namespaces"]
    fn a_missing_decoder_plugin_is_reported_as_missing_not_as_a_bad_file() {
        use std::os::fd::AsRawFd;
        let (helper, bwrap) = (confined::helper(HELPER, "MONOGRAM_AUDIO_PREVIEW_HELPER").unwrap(), confined::bwrap().unwrap());
        let dirs = confined::plugin_dirs();
        // Everything an MP3 probe needs except the decoders (no mpg123, no libav).
        let names = ["libgstcoreelements.so", "libgstapp.so", "libgstaudioparsers.so", "libgstaudioconvert.so", "libgstaudioresample.so"];
        let plugins = confined::plugin_files(&names, &dirs);
        assert_eq!(plugins.len(), names.len(), "this machine has the base plugins");
        let input = fixture("tone.mp3");
        let output = confined::sandbox_command(&bwrap, &helper, Input::Fd(input.as_raw_fd()), None, &plugins, &["probe", "mp3"]).output().unwrap();
        assert_eq!(output.status.code(), Some(8), "no MP3 decoder available: exit code 8");
        // With the decoder mounted the same file is fine, so the code really means "missing".
        let mut with_decoder = names.to_vec();
        with_decoder.push("libgstmpg123.so");
        let plugins = confined::plugin_files(&with_decoder, &dirs);
        let input = fixture("tone.mp3");
        let output = confined::sandbox_command(&bwrap, &helper, Input::Fd(input.as_raw_fd()), None, &plugins, &["probe", "mp3"]).output().unwrap();
        assert_eq!(output.status.code(), Some(0), "{}", String::from_utf8_lossy(&output.stdout));
    }

    /// bubblewrap does not close descriptors it inherits: a file opened without close-on-exec in
    /// this process is readable inside the sandbox unless the launcher marks it close-on-exec.
    #[test]
    #[ignore = "requires the built helper and real bubblewrap namespaces"]
    fn a_descriptor_leaked_in_this_process_does_not_reach_the_sandbox() {
        use std::os::fd::AsRawFd;
        // Open a file the way a careless library would: no O_CLOEXEC.
        let leaked = unsafe { libc::open(c"/etc/hostname".as_ptr(), libc::O_RDONLY) };
        assert!(leaked > 2 && unsafe { libc::fcntl(leaked, libc::F_GETFD) } & libc::FD_CLOEXEC == 0, "the test leaks a descriptor on purpose");
        let (helper, bwrap) = (confined::helper(HELPER, "MONOGRAM_AUDIO_PREVIEW_HELPER").unwrap(), confined::bwrap().unwrap());
        let input = fixture("tone.wav");
        let output = confined::sandbox_command(&bwrap, &helper, Input::Fd(input.as_raw_fd()), None, &[], &["selftest"]).output().unwrap();
        unsafe { libc::close(leaked) };
        let report = String::from_utf8_lossy(&output.stdout);
        assert!(report.contains("ordinary calls and threads: work"), "the selftest ran in the sandbox:\n{report}");
        assert!(report.contains("inherited descriptors: none"), "a leaked descriptor reached the sandbox:\n{report}");
    }

    #[test]
    #[ignore = "requires the built helper, GStreamer plugins and real bubblewrap namespaces"]
    fn probing_can_be_cancelled_and_dropping_a_stream_stops_its_helper() {
        let file = fixture("tone.mp3");
        assert_eq!(probe(&file, "tone.mp3", &|| true), Err(Error::Cancelled));
        // A stream that is dropped without being read to the end leaves no running helper.
        let stream = PcmStream::start(&file, Container::Mp3, 0).unwrap();
        let pid = stream.child.as_ref().unwrap().id();
        drop(stream);
        std::thread::sleep(Duration::from_millis(500));
        assert!(!std::path::Path::new(&format!("/proc/{pid}")).exists(), "the helper (pid {pid}) is gone");
    }
}
