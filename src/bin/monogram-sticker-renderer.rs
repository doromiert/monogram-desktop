// SPDX-License-Identifier: GPL-3.0-or-later
//! Private bounded frame-pull helper for TGS stickers, rendered with tlottie (the engine
//! Telegram Desktop uses). It never resolves external references. Launched only inside bwrap.
//! Protocol: `MGT1`,len,edge + gzip TGS in; `MGF1` header out; then per u32 frame
//! request one `index,len,RGBA(premultiplied)` reply.
use tlottie::{Composition, Limits, RenderOptions};
use std::io::{Read, Write};

#[path = "../renderer_hardening.rs"]
mod hardening;
use hardening::{limit, restrict_calls};

const INPUT_LIMIT: u32 = 20 * 1024 * 1024;
const JSON_LIMIT: u64 = 2 * 1024 * 1024;

fn number(bytes: &[u8]) -> u32 {
    u32::from_le_bytes(bytes.try_into().unwrap())
}
fn run() -> Result<(), i32> {
    let mut input = std::io::stdin().lock();
    let mut output = std::io::stdout().lock();
    let mut header = [0u8; 12];
    input.read_exact(&mut header).map_err(|_| 3)?;
    if &header[..4] != b"MGT1" {
        return Err(3);
    }
    let (length, edge) = (number(&header[4..8]), number(&header[8..12]));
    if length == 0 || length > INPUT_LIMIT || edge == 0 || edge > 512 {
        return Err(3);
    }
    let mut gzip = vec![0; length as usize];
    input.read_exact(&mut gzip).map_err(|_| 3)?;
    let mut json = String::new();
    // One byte past the limit distinguishes "exactly at" from "too large".
    let read = flate2::read::GzDecoder::new(&gzip[..]).take(JSON_LIMIT + 1).read_to_string(&mut json);
    if read.is_err() || json.is_empty() || json.len() as u64 > JSON_LIMIT || json.contains('\0') {
        return Err(4);
    }
    drop(gzip);
    // tlottie treats the JSON as untrusted: bounded parsing, no I/O, no external references.
    let animation = Composition::parse(json.as_bytes(), &Limits::default()).map_err(|_| 5)?;
    drop(json);
    let (fps, frames) = (animation.frame_rate, animation.frame_count());
    if animation.width != 512 || animation.height != 512 || !(1..=180).contains(&frames)
        || !fps.is_finite() || fps <= 0.0 || fps > 60.0 || (frames as f32 / fps) > 3.001
    {
        return Err(5);
    }
    let mut renderer = tlottie::CPURenderer::new(animation);
    let mut canvas = vec![0u32; (edge * edge) as usize];
    let mut meta = Vec::with_capacity(20);
    meta.extend_from_slice(b"MGF1");
    for value in [edge, edge, frames, (f64::from(fps) * 1000.0).round() as u32] {
        meta.extend_from_slice(&value.to_le_bytes());
    }
    output.write_all(&meta).and_then(|_| output.flush()).map_err(|_| 6)?;
    let mut request = [0u8; 4];
    while input.read_exact(&mut request).is_ok() {
        let index = number(&request);
        if index >= frames {
            return Err(8);
        }
        unsafe { libc::alarm(2) };
        // The frame argument is relative to the in-point (0-based), unlike the former backend.
        renderer.render(index as f32, &mut canvas, edge, edge, RenderOptions::default()).map_err(|_| 7)?;
        // Premultiplied RGBA, 0xAABBGGRR words: [R, G, B, A] bytes in little-endian memory.
        let mut pixels = Vec::with_capacity(canvas.len() * 4);
        for word in &canvas {
            pixels.extend_from_slice(&word.to_le_bytes());
        }
        let mut reply = Vec::with_capacity(8);
        reply.extend_from_slice(&index.to_le_bytes());
        reply.extend_from_slice(&(pixels.len() as u32).to_le_bytes());
        if output.write_all(&reply).and_then(|_| output.write_all(&pixels)).and_then(|_| output.flush()).is_err() {
            break;
        }
        unsafe { libc::alarm(0) };
    }
    Ok(())
}
fn main() {
    const MIB: libc::rlim_t = 1024 * 1024;
    // The address-space cap bounds hostile animations; stack covers deep shape trees.
    if !limit(libc::RLIMIT_AS, 768 * MIB) || !limit(libc::RLIMIT_STACK, 8 * MIB) || !limit(libc::RLIMIT_CORE, 0)
        || !limit(libc::RLIMIT_FSIZE, 0) || !limit(libc::RLIMIT_NOFILE, 32)
        || unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) } != 0
        || !restrict_calls()
    {
        std::process::exit(2);
    }
    unsafe { libc::umask(0o077); libc::alarm(8) };
    if let Err(code) = run() {
        std::process::exit(code);
    }
}
