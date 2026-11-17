// SPDX-License-Identifier: GPL-3.0-or-later
//! Private audio preview helper: decodes one audio file to a waveform or to raw PCM. Launched only
//! inside bubblewrap by the main process, which never decodes audio itself.
//!
//! Layout inside the sandbox: the source is `/in/source` (a read-only copy of the file the main
//! process opened and checked), there is no network, no other file and no audio device.
//!
//!   `probe <container>`          prints one JSON line: container, duration, rate, channels and
//!                                a 64-bar waveform
//!   `pcm <container> <start_ms>` writes raw S16LE, 48 kHz, stereo PCM to stdout from `start_ms`
//!   `selftest`                   proves the seccomp filter (see `helper_hardening`)
//!
//! Output is always that one fixed format, so the player needs no negotiation and no decoder.
//! The pipelines are explicit (no `decodebin`, no pipeline strings): Ogg is Vorbis or Opus only,
//! MP3 is Layer III only, WAV is plain PCM or float, M4A and WebM carry exactly one audio stream.
//! The file is re-checked here with the same `audio_check` the main process used, then limits that
//! need decoding are enforced: at most two channels, 8 to 96 kHz, 30 minutes, no video or second
//! stream. Exit codes: 2 hardening failed, 3 bad arguments, 4 file refused by the strict check,
//! 5 pipeline failure or stall, 6 limit exceeded, 7 unsupported stream (video or extra tracks),
//! 8 a needed GStreamer plugin (demuxer, parser or decoder) is not installed.
#[path = "../audio_check.rs"]
#[allow(dead_code)]
mod audio_check;
#[path = "../helper_hardening.rs"]
mod helper_hardening;

use audio_check::{Container, HEAD_BYTES, MAX_CHANNELS, MAX_DURATION_MS, MAX_FILE_BYTES, MAX_RATE, MIN_RATE};
use gst::prelude::*;
use gstreamer as gst;
use gstreamer_app as gst_app;
use std::{
    io::{Read, Write},
    sync::{
        atomic::{AtomicBool, AtomicU32, Ordering},
        Arc,
    },
};

const SOURCE: &str = "/in/source";
/// The only output format. 48 kHz stereo 16-bit is 192 bytes per millisecond.
const RATE: i32 = 48_000;
const FRAME_BYTES: usize = 4;
const BARS: usize = 64;
/// One waveform window is 20 ms, so thirty minutes is at most 90,000 of them.
const WINDOW_FRAMES: usize = 960;
const MAX_WINDOWS: usize = (MAX_DURATION_MS as usize / 20) + 2;
/// Wall-clock and CPU budgets for a probe (the whole file is decoded).
const PROBE_SECONDS: u32 = 60;
/// CPU budget while streaming; waiting on a full pipe or a paused player costs none.
const STREAM_CPU_SECONDS: u64 = 240;

fn container_from_arg(arg: &str) -> Option<Container> {
    Some(match arg {
        "ogg" => Container::Ogg,
        "mp3" => Container::Mp3,
        "wav" => Container::Wav,
        "m4a" => Container::M4a,
        "webm" => Container::Webm,
        _ => return None,
    })
}

/// Re-check the bytes this process actually received, and that they are the container the caller
/// expects. Returns the file length.
fn identify(expected: Container) -> Result<u64, i32> {
    let file = std::fs::File::open(SOURCE).map_err(|_| 4)?;
    let metadata = file.metadata().map_err(|_| 4)?;
    if !metadata.is_file() || metadata.len() == 0 || metadata.len() > MAX_FILE_BYTES {
        return Err(4);
    }
    let mut head = Vec::new();
    file.take(HEAD_BYTES as u64).read_to_end(&mut head).map_err(|_| 4)?;
    match audio_check::sniff(&head, metadata.len()) {
        Ok(found) if found == expected => Ok(metadata.len()),
        _ => Err(4),
    }
}

fn make(factory: &str) -> Result<gst::Element, i32> {
    gst::ElementFactory::make(factory).build().map_err(|_| 5)
}
/// A demuxer, parser or decoder. If it cannot be created the plugin is not installed (or was not
/// mounted), which is reported as its own failure so the user can be told what is missing.
fn make_codec(factory: &str) -> Result<gst::Element, i32> {
    gst::ElementFactory::make(factory).build().map_err(|_| MISSING)
}
/// The first of `factories` that exists (a distribution may ship one decoder and not the other).
fn make_first(factories: &[&str]) -> Result<gst::Element, i32> {
    factories.iter().find_map(|name| gst::ElementFactory::make(name).build().ok()).ok_or(MISSING)
}
const MISSING: i32 = 8;

struct Built {
    pipeline: gst::Pipeline,
    sink: gst_app::AppSink,
    convert: gst::Element,
    /// Set when a demuxer offers a stream we do not accept (video, a second track, text...).
    rejected: Arc<AtomicBool>,
    /// Set when the stream is acceptable but its decoder plugin is not installed.
    missing: Arc<AtomicBool>,
}

/// Why no decoder chain was made for a demuxed stream.
enum Why {
    Unsupported,
    Missing,
}

fn output_caps() -> gst::Caps {
    gst::Caps::builder("audio/x-raw")
        .field("format", "S16LE")
        .field("rate", RATE)
        .field("channels", 2i32)
        .field("layout", "interleaved")
        .build()
}

/// The decoder for a demuxed stream, by its caps, from a fixed list. Anything else is refused.
fn decoder_for(caps: &gst::CapsRef) -> Result<Vec<gst::Element>, Why> {
    let structure = caps.structure(0).ok_or(Why::Unsupported)?;
    let missing = |_| Why::Missing;
    match structure.name().as_str() {
        "audio/x-opus" => Ok(vec![make_codec("opusdec").map_err(missing)?]),
        "audio/x-vorbis" => Ok(vec![make_codec("vorbisdec").map_err(missing)?]),
        "audio/mpeg" if structure.get::<i32>("mpegversion") == Ok(4) => {
            Ok(vec![make_codec("aacparse").map_err(missing)?, make_first(&["faad", "avdec_aac"]).map_err(missing)?])
        }
        _ => Err(Why::Unsupported),
    }
}

fn build(container: Container) -> Result<Built, i32> {
    let pipeline = gst::Pipeline::new();
    let rejected = Arc::new(AtomicBool::new(false));
    let missing = Arc::new(AtomicBool::new(false));
    let source = make("filesrc")?;
    source.set_property("location", SOURCE);
    let convert = make("audioconvert")?;
    let resample = make("audioresample")?;
    let filter = make("capsfilter")?;
    filter.set_property("caps", output_caps());
    let sink = make("appsink")?.downcast::<gst_app::AppSink>().map_err(|_| 5)?;
    sink.set_caps(Some(&output_caps()));
    // Decode no faster than the consumer reads: a few buffers ahead at most.
    sink.set_max_buffers(8);
    sink.set_property("sync", false);
    let tail = [&convert, &resample, &filter, sink.upcast_ref::<gst::Element>()];
    pipeline.add_many([&source]).map_err(|_| 5)?;
    pipeline.add_many(tail).map_err(|_| 5)?;
    gst::Element::link_many(tail).map_err(|_| 5)?;

    match container {
        Container::Wav | Container::Mp3 => {
            let chain: Vec<gst::Element> = if container == Container::Wav {
                vec![make_codec("wavparse")?]
            } else {
                vec![make_codec("mpegaudioparse")?, make_first(&["mpg123audiodec", "avdec_mp3float", "avdec_mp3"])?]
            };
            let refs: Vec<&gst::Element> = chain.iter().collect();
            pipeline.add_many(refs.iter().copied()).map_err(|_| 5)?;
            let mut line = vec![&source];
            line.extend(refs.iter().copied());
            line.push(&convert);
            gst::Element::link_many(line).map_err(|_| 5)?;
        }
        Container::Ogg | Container::M4a | Container::Webm => {
            let demux = make_codec(match container {
                Container::Ogg => "oggdemux",
                Container::M4a => "qtdemux",
                _ => "matroskademux",
            })?;
            pipeline.add(&demux).map_err(|_| 5)?;
            source.link(&demux).map_err(|_| 5)?;
            let pads = Arc::new(AtomicU32::new(0));
            let (weak, flag, absent, convert_weak) = (pipeline.downgrade(), rejected.clone(), missing.clone(), convert.downgrade());
            demux.connect_pad_added(move |_, pad| {
                // Exactly one stream, and it must be audio we have a decoder for.
                if pads.fetch_add(1, Ordering::SeqCst) > 0 {
                    flag.store(true, Ordering::SeqCst);
                    return;
                }
                let caps = pad.current_caps().unwrap_or_else(|| pad.query_caps(None));
                let chain = decoder_for(&caps);
                if matches!(chain, Err(Why::Missing)) {
                    absent.store(true, Ordering::SeqCst);
                }
                let (Some(pipeline), Some(convert), Ok(chain)) = (weak.upgrade(), convert_weak.upgrade(), chain) else {
                    flag.store(true, Ordering::SeqCst);
                    return;
                };
                let linked = (|| {
                    for element in &chain {
                        pipeline.add(element).ok()?;
                    }
                    let mut previous = pad.clone();
                    for element in &chain {
                        previous.link(&element.static_pad("sink")?).ok()?;
                        previous = element.static_pad("src")?;
                    }
                    previous.link(&convert.static_pad("sink")?).ok()?;
                    for element in &chain {
                        element.sync_state_with_parent().ok()?;
                    }
                    Some(())
                })();
                if linked.is_none() {
                    flag.store(true, Ordering::SeqCst);
                }
            });
        }
    }
    Ok(Built { pipeline, sink, convert, rejected, missing })
}

/// Pull decoded samples, calling `consume` with each buffer's bytes, until the end of the stream.
/// Enforces the decoded-format and length limits and refuses anything that stalls.
fn run(built: &Built, mut consume: impl FnMut(&[u8]) -> Result<(), i32>) -> Result<u64, i32> {
    built.pipeline.set_state(gst::State::Playing).map_err(|_| 5)?;
    let (mut frames, mut checked) = (0u64, false);
    let max_frames = MAX_DURATION_MS * RATE as u64 / 1000;
    let result = loop {
        if built.rejected.load(Ordering::SeqCst) {
            // Acceptable audio whose decoder is not installed is reported as that, not as refused.
            break Err(if built.missing.load(Ordering::SeqCst) { MISSING } else { 7 });
        }
        // A stalled pipeline (a demuxer waiting for data that never comes) is a failure, not a hang.
        let Some(sample) = built.sink.try_pull_sample(gst::ClockTime::from_seconds(12)) else {
            // No more samples: the reason a stream was refused or a decoder is missing wins over a
            // generic failure, so the caller can say what is wrong.
            break if built.rejected.load(Ordering::SeqCst) {
                Err(if built.missing.load(Ordering::SeqCst) { MISSING } else { 7 })
            } else if built.sink.is_eos() {
                Ok(())
            } else {
                Err(5)
            };
        };
        if !checked {
            // The caps the decoder actually produced, before conversion to our fixed format.
            let caps = built.convert.static_pad("sink").and_then(|pad| pad.current_caps()).ok_or(5);
            let structure = caps.as_ref().ok().and_then(|caps| caps.structure(0));
            let (channels, rate) = structure
                .map(|s| (s.get::<i32>("channels").unwrap_or(0), s.get::<i32>("rate").unwrap_or(0)))
                .unwrap_or((0, 0));
            if !(1..=MAX_CHANNELS as i32).contains(&channels) || !(MIN_RATE as i32..=MAX_RATE as i32).contains(&rate) {
                break Err(6);
            }
            checked = true;
        }
        let Some(buffer) = sample.buffer() else { continue };
        let map = buffer.map_readable().map_err(|_| 5)?;
        frames += (map.len() / FRAME_BYTES) as u64;
        if frames > max_frames {
            break Err(6);
        }
        if let Err(code) = consume(map.as_slice()) {
            break Err(code);
        }
    };
    let error = built.pipeline.bus().and_then(|bus| bus.pop_filtered(&[gst::MessageType::Error]));
    let _ = built.pipeline.set_state(gst::State::Null);
    match (result, error) {
        (Ok(()), None) if frames > 0 => Ok(frames),
        (Ok(()), _) => Err(5),
        (Err(code), _) => Err(code),
    }
}

fn probe(container: Container) -> Result<(), i32> {
    identify(container)?;
    let built = build(container)?;
    let (mut windows, mut peak, mut in_window) = (Vec::<u16>::new(), 0u16, 0usize);
    let (mut rate, mut channels) = (RATE as u32, 2u32);
    let frames = run(&built, |bytes| {
        for frame in bytes.chunks_exact(FRAME_BYTES) {
            let left = i16::from_le_bytes([frame[0], frame[1]]).unsigned_abs();
            let right = i16::from_le_bytes([frame[2], frame[3]]).unsigned_abs();
            peak = peak.max(left).max(right);
            in_window += 1;
            if in_window == WINDOW_FRAMES {
                if windows.len() >= MAX_WINDOWS {
                    return Err(6);
                }
                windows.push(peak);
                (peak, in_window) = (0, 0);
            }
        }
        Ok(())
    })?;
    if in_window > 0 {
        windows.push(peak);
    }
    // The source's own rate and channels were checked in `run`; report the output format.
    (rate, channels) = (rate.min(MAX_RATE), channels.min(2));
    // Pool the 20 ms windows into BARS bars and scale so the loudest bar is full height. Quiet
    // files keep their shape (a floor stops silence from dividing by zero).
    let mut bars = [0u16; BARS];
    for (index, value) in windows.iter().enumerate() {
        let bar = (index * BARS / windows.len().max(1)).min(BARS - 1);
        bars[bar] = bars[bar].max(*value);
    }
    let loudest = (*bars.iter().max().unwrap_or(&1)).max(256);
    let scaled: Vec<u8> = bars.iter().map(|v| ((u32::from(*v) * 255) / u32::from(loudest)) as u8).collect();
    let duration_ms = frames * 1000 / RATE as u64;
    println!(
        "{}",
        serde_json::json!({"container": container.name(), "duration_ms": duration_ms, "rate": rate, "channels": channels, "peaks": scaled})
    );
    Ok(())
}

fn pcm(container: Container, start_ms: u64) -> Result<(), i32> {
    identify(container)?;
    let built = build(container)?;
    let mut skip = (start_ms * RATE as u64 / 1000) as usize * FRAME_BYTES;
    let mut out = std::io::stdout().lock();
    run(&built, |bytes| {
        let bytes = if skip >= bytes.len() {
            skip -= bytes.len();
            return Ok(());
        } else {
            let rest = &bytes[skip..];
            skip = 0;
            rest
        };
        // The reader going away (playback stopped) is not an error.
        out.write_all(bytes).and_then(|()| out.flush()).map_err(|_| 0)
    })
    .map(|_| ())
    .or_else(|code| if code == 0 { Ok(()) } else { Err(code) })
}

fn main() {
    const MIB: libc::rlim_t = 1024 * 1024;
    let mode = std::env::args().nth(1).unwrap_or_default();
    // Which build this is, so the app can tell when it is paired with an out-of-date helper.
    if mode == "build" {
        println!("{}", env!("CARGO_PKG_VERSION").to_owned() + option_env!("MONOGRAM_GIT_COMMIT").unwrap_or("") + option_env!("MONOGRAM_GIT_DIRTY").unwrap_or(""));
        return;
    }
    // Address space for GStreamer and its plugins; the kernel enforces the rest. No file may be
    // written beyond a small registry, and a probe is bounded in wall-clock time as well as CPU.
    if !helper_hardening::harden(4 * 1024 * MIB, 128, 16 * MIB) {
        std::process::exit(2);
    }
    let cpu = if mode == "probe" { u64::from(PROBE_SECONDS) } else { STREAM_CPU_SECONDS };
    if !helper_hardening::limit(libc::RLIMIT_CPU, cpu) {
        std::process::exit(2);
    }
    unsafe {
        libc::umask(0o077);
        if mode == "probe" {
            libc::alarm(PROBE_SECONDS + 5);
        }
    }
    if mode == "selftest" {
        std::process::exit(helper_hardening::selftest().err().unwrap_or(0));
    }
    let args: Vec<String> = std::env::args().skip(2).collect();
    let container = args.first().and_then(|name| container_from_arg(name));
    let result = match (mode.as_str(), container, args.get(1).map(|s| s.parse::<u64>())) {
        ("probe", Some(container), None) => gst::init().map_err(|_| 5).and_then(|()| probe(container)),
        ("pcm", Some(container), Some(Ok(start))) if start <= MAX_DURATION_MS && args.len() == 2 => {
            gst::init().map_err(|_| 5).and_then(|()| pcm(container, start))
        }
        _ => Err(3),
    };
    if let Err(code) = result {
        std::process::exit(code);
    }
}
