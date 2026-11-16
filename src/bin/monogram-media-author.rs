// SPDX-License-Identifier: GPL-3.0-or-later
//! Private picture authoring helper: probes a JPEG, PNG or WebP and applies a crop, rotation and
//! scale with an explicit GStreamer chain. Launched only inside bubblewrap by the main process.
//!
//! Layout inside the sandbox: the source is `/in/source` (read-only), the result goes to
//! `/out/result.png|jpg` (the only writable place besides a private `/tmp`), there is no network
//! and no other file. `probe` prints a `SourceInfo` JSON line; `run` reads a `Request` JSON from
//! stdin and prints `{"file":"result.png"}` on success.
//!
//! Nothing here builds a pipeline from a string or lets GStreamer pick a decoder: the format is
//! identified from magic bytes and its size is read from the header and bounded *before* any
//! decoder runs. Exit codes: 2 hardening failed, 3 bad request, 4 unsupported or oversize source,
//! 5 pipeline failure, 6 source mismatch, 7 output problem.
#[path = "../helper_hardening.rs"]
mod helper_hardening;
#[path = "../media_authoring.rs"]
#[allow(dead_code)]
mod media_authoring;

use gstreamer as gst;
use gst::prelude::*;
use media_authoring::{Format, Kind, Limits, Plan, Rotation, SourceInfo, SNIFF_BYTES};
use serde::Deserialize;
use std::io::Read;

const SOURCE: &str = "/in/source";
const OUTPUT_DIR: &str = "/out";
const WAIT_SECONDS: u64 = 20;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    plan: Plan,
    source: SourceInfo,
}

/// Read the source, bound its size and header dimensions, and say what it is.
fn identify(limits: &Limits) -> Result<(Format, u32, u32, u64), i32> {
    let file = std::fs::File::open(SOURCE).map_err(|_| 4)?;
    let metadata = file.metadata().map_err(|_| 4)?;
    if !metadata.is_file() || metadata.len() == 0 || metadata.len() > limits.max_input_bytes {
        return Err(4);
    }
    let mut head = Vec::new();
    file.take(SNIFF_BYTES as u64).read_to_end(&mut head).map_err(|_| 4)?;
    let (format, width, height) = media_authoring::sniff_image(&head).map_err(|_| 4)?;
    if width == 0
        || height == 0
        || width > limits.max_source_edge
        || height > limits.max_source_edge
        || u64::from(width) * u64::from(height) > limits.max_source_pixels
    {
        return Err(4);
    }
    Ok((format, width, height, metadata.len()))
}

fn make(factory: &str) -> Result<gst::Element, i32> {
    gst::ElementFactory::make(factory).build().map_err(|_| 5)
}

/// source ! decoder ! videoconvert ! videoflip(automatic), applying any EXIF orientation first so
/// every coordinate the user sees is in the upright picture.
fn decode_chain(format: Format) -> Result<Vec<gst::Element>, i32> {
    let source = make("filesrc")?;
    source.set_property("location", SOURCE);
    let mut chain = vec![source];
    match format {
        Format::Png => chain.push(make("pngdec")?),
        Format::Jpeg => {
            chain.push(make("jpegparse")?);
            chain.push(make("jpegdec")?);
        }
        Format::Webp => chain.push(make("webpdec")?),
    }
    chain.push(make("videoconvert")?);
    let orient = make("videoflip")?;
    orient.set_property_from_str("method", "automatic");
    chain.push(orient);
    Ok(chain)
}

fn assemble(chain: &[gst::Element]) -> Result<gst::Pipeline, i32> {
    let pipeline = gst::Pipeline::new();
    let refs: Vec<&gst::Element> = chain.iter().collect();
    pipeline.add_many(refs.iter().copied()).map_err(|_| 5)?;
    gst::Element::link_many(refs.iter().copied()).map_err(|_| 5)?;
    Ok(pipeline)
}

fn finish(pipeline: &gst::Pipeline, done: gst::MessageType) -> Result<(), i32> {
    let bus = pipeline.bus().ok_or(5)?;
    let message = bus.timed_pop_filtered(
        gst::ClockTime::from_seconds(WAIT_SECONDS),
        &[done, gst::MessageType::Error],
    );
    let ok = matches!(&message, Some(message) if message.type_() == done);
    let _ = pipeline.set_state(gst::State::Null);
    if ok { Ok(()) } else { Err(5) }
}

fn probe(limits: &Limits) -> Result<(), i32> {
    let (format, _, _, bytes) = identify(limits)?;
    let mut chain = decode_chain(format)?;
    let sink = make("fakesink")?;
    sink.set_property("sync", false);
    chain.push(sink.clone());
    let pipeline = assemble(&chain)?;
    pipeline.set_state(gst::State::Paused).map_err(|_| 5)?;
    let bus = pipeline.bus().ok_or(5)?;
    let message = bus.timed_pop_filtered(
        gst::ClockTime::from_seconds(WAIT_SECONDS),
        &[gst::MessageType::AsyncDone, gst::MessageType::Error],
    );
    if !matches!(&message, Some(m) if m.type_() == gst::MessageType::AsyncDone) {
        return Err(5);
    }
    let caps = sink.static_pad("sink").and_then(|pad| pad.current_caps()).ok_or(5)?;
    let structure = caps.structure(0).ok_or(5)?;
    let (width, height) = (
        structure.get::<i32>("width").map_err(|_| 5)?,
        structure.get::<i32>("height").map_err(|_| 5)?,
    );
    let _ = pipeline.set_state(gst::State::Null);
    let info = SourceInfo {
        kind: Kind::Image,
        width: u32::try_from(width).map_err(|_| 5)?,
        height: u32::try_from(height).map_err(|_| 5)?,
        duration_ms: None,
        bytes,
    };
    println!("{}", serde_json::to_string(&info).map_err(|_| 5)?);
    Ok(())
}

fn run(limits: &Limits) -> Result<(), i32> {
    let mut json = Vec::new();
    std::io::stdin()
        .lock()
        .take(limits.max_job_json as u64 + 1)
        .read_to_end(&mut json)
        .map_err(|_| 3)?;
    if json.len() > limits.max_job_json {
        return Err(3);
    }
    let request: Request = serde_json::from_slice(&json).map_err(|_| 3)?;
    let (Request { plan, source }, (format, width, height, _)) = (request, identify(limits)?);
    plan.check(&source, limits).map_err(|_| 3)?;
    // The caller probed this file; refuse if it is not the one it described (an EXIF turn swaps
    // the axes, nothing else may differ).
    let same = (width, height) == (source.width, source.height)
        || (height, width) == (source.width, source.height);
    if !same {
        return Err(6);
    }

    let mut chain = decode_chain(format)?;
    let crop = make("videocrop")?;
    crop.set_property("left", plan.crop.x as i32);
    crop.set_property("top", plan.crop.y as i32);
    crop.set_property("right", (source.width - plan.crop.x - plan.crop.width) as i32);
    crop.set_property("bottom", (source.height - plan.crop.y - plan.crop.height) as i32);
    chain.push(crop);
    let turn = make("videoflip")?;
    turn.set_property_from_str(
        "method",
        match plan.rotation {
            Rotation::None => "none",
            Rotation::Clockwise90 => "clockwise",
            Rotation::Half => "rotate-180",
            Rotation::Counterclockwise90 => "counterclockwise",
        },
    );
    chain.push(turn);
    chain.push(make("videoscale")?);
    let size = make("capsfilter")?;
    size.set_property(
        "caps",
        gst::Caps::builder("video/x-raw")
            .field("width", plan.output_width as i32)
            .field("height", plan.output_height as i32)
            .build(),
    );
    chain.push(size);
    chain.push(make("videoconvert")?);
    // Photos stay JPEG; PNG and WebP are written as lossless PNG so transparency survives.
    let (encoder, name) = if format == Format::Jpeg {
        let encoder = make("jpegenc")?;
        encoder.set_property("quality", 90i32);
        (encoder, "result.jpg")
    } else {
        (make("pngenc")?, "result.png")
    };
    chain.push(encoder);
    let output = make("filesink")?;
    output.set_property("location", format!("{OUTPUT_DIR}/{name}"));
    chain.push(output);
    let pipeline = assemble(&chain)?;
    pipeline.set_state(gst::State::Playing).map_err(|_| 5)?;
    finish(&pipeline, gst::MessageType::Eos)?;
    let written = std::fs::metadata(format!("{OUTPUT_DIR}/{name}")).map_err(|_| 7)?;
    if !written.is_file() || written.len() == 0 || written.len() > plan.max_output_bytes {
        return Err(7);
    }
    println!("{{\"file\":\"{name}\"}}");
    Ok(())
}

fn main() {
    const MIB: libc::rlim_t = 1024 * 1024;
    let limits = Limits::default();
    // GStreamer maps plugins and large frames, so the address-space cap is generous; it still
    // bounds a decoder that a crafted file drives towards unbounded allocation. FSIZE makes the
    // kernel enforce the output limit.
    if !helper_hardening::harden(6 * 1024 * MIB, 256, limits.max_output_bytes) {
        std::process::exit(2);
    }
    unsafe {
        libc::umask(0o077);
        libc::alarm(WAIT_SECONDS as u32 + 10)
    };
    if std::env::args().nth(1).as_deref() == Some("selftest") {
        std::process::exit(helper_hardening::selftest().err().unwrap_or(0));
    }
    if gst::init().is_err() {
        std::process::exit(5);
    }
    let result = match std::env::args().nth(1).as_deref() {
        Some("probe") => probe(&limits),
        Some("run") => run(&limits),
        _ => Err(3),
    };
    if let Err(code) = result {
        std::process::exit(code);
    }
}
