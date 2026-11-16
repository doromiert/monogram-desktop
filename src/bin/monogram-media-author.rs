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

fn limit(resource: libc::__rlimit_resource_t, value: libc::rlim_t) -> bool {
    let v = libc::rlimit { rlim_cur: value, rlim_max: value };
    unsafe { libc::setrlimit(resource, &v) == 0 }
}
fn insn(code: u32, jt: u8, jf: u8, k: u32) -> libc::sock_filter {
    libc::sock_filter { code: code as u16, jt, jf, k }
}
/// Deny-list filter. GStreamer needs threads, so unlike the sticker renderer `clone` stays
/// allowed; processes, sockets, tracing and namespace changes are not.
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
    // Everything a picture decoder has no business calling. A deny-list cannot be complete, so
    // it targets the kernel attack surface that sandbox escapes usually go through (io_uring,
    // bpf, key rings, userfaultfd, perf, module loading, the new mount API, kernel handles) on top
    // of processes, sockets, tracing and namespace changes. The namespaces, dropped capabilities
    // and no_new_privs set up around this process are the primary boundary.
    let mut denied = vec![
        libc::SYS_execve, libc::SYS_execveat, libc::SYS_socket, libc::SYS_ptrace,
        libc::SYS_process_vm_readv, libc::SYS_process_vm_writev, libc::SYS_setns,
        libc::SYS_unshare, libc::SYS_mount, libc::SYS_umount2,
        libc::SYS_io_uring_setup, libc::SYS_io_uring_enter, libc::SYS_io_uring_register,
        libc::SYS_bpf, libc::SYS_keyctl, libc::SYS_add_key, libc::SYS_request_key,
        libc::SYS_userfaultfd, libc::SYS_perf_event_open,
        libc::SYS_init_module, libc::SYS_finit_module, libc::SYS_delete_module,
        libc::SYS_kexec_load, libc::SYS_kexec_file_load, libc::SYS_reboot,
        libc::SYS_swapon, libc::SYS_swapoff, libc::SYS_acct, libc::SYS_pivot_root, libc::SYS_chroot,
        libc::SYS_open_by_handle_at, libc::SYS_name_to_handle_at, libc::SYS_fanotify_init,
        libc::SYS_kcmp, libc::SYS_pidfd_getfd, libc::SYS_process_madvise,
        libc::SYS_mount_setattr, libc::SYS_move_mount, libc::SYS_open_tree,
        libc::SYS_fsopen, libc::SYS_fsconfig, libc::SYS_fsmount, libc::SYS_fspick,
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

/// `selftest`: after the same hardening as a real run, try each forbidden call and report what the
/// kernel said. Exits 0 only if every one was refused with EPERM and an allowed call still works.
fn selftest() -> Result<(), i32> {
    let null = std::ptr::null::<libc::c_void>();
    // The error code must be read right after its own call, before anything else can change it.
    fn attempt(name: &'static str, call: impl FnOnce() -> i64) -> (&'static str, i64, Option<i32>) {
        let result = call();
        (name, result, std::io::Error::last_os_error().raw_os_error())
    }
    let attempts = [
        attempt("socket", || unsafe { libc::socket(libc::AF_INET, libc::SOCK_STREAM, 0) } as i64),
        attempt("io_uring_setup", || unsafe { libc::syscall(libc::SYS_io_uring_setup, 1, null) }),
        attempt("bpf", || unsafe { libc::syscall(libc::SYS_bpf, 0, null, 0) }),
        attempt("keyctl", || unsafe { libc::syscall(libc::SYS_keyctl, 0, 0, 0, 0, 0) }),
        attempt("userfaultfd", || unsafe { libc::syscall(libc::SYS_userfaultfd, 0) }),
        attempt("perf_event_open", || unsafe { libc::syscall(libc::SYS_perf_event_open, null, 0, -1, -1, 0) }),
        attempt("execve", || unsafe { libc::execve(c"/bin/true".as_ptr(), std::ptr::null(), std::ptr::null()) } as i64),
        attempt("mount", || unsafe { libc::mount(std::ptr::null(), std::ptr::null(), std::ptr::null(), 0, null) } as i64),
    ];
    let mut failed = false;
    for (name, result, errno) in attempts {
        // EPERM is the filter's answer; any other failure (ENOSYS, EINVAL…) means the call reached
        // the kernel, so it was not refused by the filter.
        let refused = result == -1 && errno == Some(libc::EPERM);
        println!("{name}: {}", if refused { "refused".to_owned() } else { format!("NOT REFUSED ({result}, errno {errno:?})") });
        failed |= !refused;
    }
    // The filter must not be a blanket ban: ordinary calls and threads still work.
    let alive = unsafe { libc::getpid() } > 0 && std::thread::spawn(|| 7).join().ok() == Some(7);
    println!("ordinary calls and threads: {}", if alive { "work" } else { "BROKEN" });
    if failed || !alive { Err(9) } else { Ok(()) }
}

fn main() {
    const MIB: libc::rlim_t = 1024 * 1024;
    let limits = Limits::default();
    // GStreamer maps plugins and large frames, so the address-space cap is generous; it still
    // bounds a decoder that a crafted file drives towards unbounded allocation. FSIZE makes the
    // kernel enforce the output limit.
    if !limit(libc::RLIMIT_AS, 6 * 1024 * MIB)
        || !limit(libc::RLIMIT_CORE, 0)
        || !limit(libc::RLIMIT_NOFILE, 256)
        || !limit(libc::RLIMIT_FSIZE, limits.max_output_bytes)
        || unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) } != 0
        || !restrict_calls()
    {
        std::process::exit(2);
    }
    unsafe {
        libc::umask(0o077);
        libc::alarm(WAIT_SECONDS as u32 + 10)
    };
    if std::env::args().nth(1).as_deref() == Some("selftest") {
        std::process::exit(selftest().err().unwrap_or(0));
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
