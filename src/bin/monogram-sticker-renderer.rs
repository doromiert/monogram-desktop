// SPDX-License-Identifier: GPL-3.0-or-later
//! Private bounded frame-pull helper for TGS stickers, rendered with tlottie (the engine
//! Telegram Desktop uses). It never resolves external references. Launched only inside bwrap.
//! Protocol: `MGT1`,len,edge + gzip TGS in; `MGF1` header out; then per u32 frame
//! request one `index,len,RGBA(premultiplied)` reply. `MGE2`/`MGF2` add a rest-frame index for bundled Noto
//! emoji, whose animations are larger and longer than a sticker (square up to 1024, 360
//! frames, 6 seconds); stickers keep their exact 512x512, 180 frame, 3 second limits.
use tlottie::{Composition, Limits, RenderOptions};
use std::io::{Read, Write};

const INPUT_LIMIT: u32 = 20 * 1024 * 1024;
const JSON_LIMIT: u64 = 2 * 1024 * 1024;

fn limit(resource: libc::__rlimit_resource_t, value: libc::rlim_t) -> bool {
    let v = libc::rlimit { rlim_cur: value, rlim_max: value };
    unsafe { libc::setrlimit(resource, &v) == 0 }
}
fn insn(code: u32, jt: u8, jf: u8, k: u32) -> libc::sock_filter {
    libc::sock_filter { code: code as u16, jt, jf, k }
}
fn restrict_calls() -> bool {
    const BPF_LD_W_ABS: u32 = 0x20;
    const BPF_JEQ_K: u32 = 0x15;
    const BPF_JGE_K: u32 = 0x35;
    const BPF_RET_K: u32 = 0x06;
    #[cfg(target_arch = "x86_64")]
    const ARCH: u32 = 0xc000_003e;
    #[cfg(target_arch = "aarch64")]
    const ARCH: u32 = 0xc000_00b7;
    const KILL: u32 = 0x8000_0000;
    const EPERM: u32 = 0x0005_0000 | libc::EPERM as u32;
    const ALLOW: u32 = 0x7fff_0000;
    let mut denied = vec![
        libc::SYS_clone, libc::SYS_clone3, libc::SYS_execve, libc::SYS_execveat,
        libc::SYS_socket, libc::SYS_socketpair, libc::SYS_ptrace,
        libc::SYS_process_vm_readv, libc::SYS_process_vm_writev, libc::SYS_setns,
        libc::SYS_unshare, libc::SYS_mount, libc::SYS_umount2,
    ];
    #[cfg(target_arch = "x86_64")]
    denied.extend([libc::SYS_fork, libc::SYS_vfork]);
    let mut program = vec![
        insn(BPF_LD_W_ABS, 0, 0, 4), // seccomp_data.arch
        insn(BPF_JEQ_K, 1, 0, ARCH),
        insn(BPF_RET_K, 0, 0, KILL),
        insn(BPF_LD_W_ABS, 0, 0, 0), // seccomp_data.nr
        insn(BPF_JGE_K, 0, 1, 0x4000_0000), // x32 ABI
        insn(BPF_RET_K, 0, 0, KILL),
    ];
    for call in denied {
        program.push(insn(BPF_JEQ_K, 0, 1, call as u32));
        program.push(insn(BPF_RET_K, 0, 0, EPERM));
    }
    program.push(insn(BPF_RET_K, 0, 0, ALLOW));
    let filter = libc::sock_fprog { len: program.len() as u16, filter: program.as_mut_ptr() };
    unsafe { libc::prctl(libc::PR_SET_SECCOMP, libc::SECCOMP_MODE_FILTER, &filter) == 0 }
}
fn number(bytes: &[u8]) -> u32 {
    u32::from_le_bytes(bytes.try_into().unwrap())
}
// Noto's static pose is a marker, not the last frame of its looping timeline.
fn rest_frame(json: &str, in_point: f32, frames: u32) -> Option<u32> {
    let value: serde_json::Value = serde_json::from_str(json).ok()?;
    let mut markers = value.get("markers")?.as_array()?.iter().filter(|marker|
        marker.get("cm").and_then(|name| name.as_str())
            .is_some_and(|name| name.trim().eq_ignore_ascii_case("rest")));
    let marker = markers.next()?;
    if markers.next().is_some() { return None; }
    let relative = marker.get("tm")?.as_f64()? - f64::from(in_point);
    (relative.is_finite() && relative >= 0. && relative < f64::from(frames))
        .then(|| relative.floor() as u32)
}

fn run() -> Result<(), i32> {
    let mut input = std::io::stdin().lock();
    let mut output = std::io::stdout().lock();
    let mut header = [0u8; 12];
    input.read_exact(&mut header).map_err(|_| 3)?;
    let emoji = match &header[..4] {
        b"MGT1" => false,
        b"MGE2" => true,
        _ => return Err(3),
    };
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
    let (fps, frames) = (animation.frame_rate, animation.frame_count());
    let rest = if emoji { rest_frame(&json, animation.in_point, frames) } else { None };
    drop(json);
    let (square_ok, max_frames, max_seconds) = if emoji {
        (animation.width == animation.height && (1..=1024).contains(&animation.width), 360, 6.1)
    } else {
        (animation.width == 512 && animation.height == 512, 180, 3.001)
    };
    if !square_ok || !(1..=max_frames).contains(&frames)
        || !fps.is_finite() || fps <= 0.0 || fps > 60.0 || (frames as f32 / fps) > max_seconds
    {
        return Err(5);
    }
    let mut renderer = tlottie::CPURenderer::new(animation);
    let mut canvas = vec![0u32; (edge * edge) as usize];
    let mut meta = Vec::with_capacity(if emoji { 24 } else { 20 });
    meta.extend_from_slice(if emoji { b"MGF2" } else { b"MGF1" });
    for value in [edge, edge, frames, (f64::from(fps) * 1000.0).round() as u32] {
        meta.extend_from_slice(&value.to_le_bytes());
    }
    if emoji { meta.extend_from_slice(&rest.unwrap_or(u32::MAX).to_le_bytes()); }
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

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rest_markers_are_case_insensitive_relative_and_bounded() {
        for name in ["rest", "Rest", " REST "] {
            let json = format!(r#"{{"markers":[{{"tm":110,"cm":"{name}","dr":0}}]}}"#);
            assert_eq!(rest_frame(&json, 0., 143), Some(110));
            assert_eq!(rest_frame(&json, 10., 133), Some(100));
        }
        for json in [r#"{}"#, r#"{"markers":[]}"#, r#"{"markers":[{"tm":110,"cm":"other"}]}"#,
            r#"{"markers":[{"tm":-1,"cm":"rest"}]}"#, r#"{"markers":[{"tm":143,"cm":"rest"}]}"#,
            r#"{"markers":[{"tm":"110","cm":"rest"}]}"#, r#"{"markers":[{"cm":"rest"}]}"#,
            r#"{"markers":[{"tm":1,"cm":"rest"},{"tm":2,"cm":"Rest"}]}"#, "invalid"] {
            assert_eq!(rest_frame(json, 0., 143), None, "{json}");
        }
        assert_eq!(rest_frame(r#"{"markers":[{"tm":10,"cm":"rest"}]}"#, 11., 143), None);
    }
}
