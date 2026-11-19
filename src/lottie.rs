//! On-demand TGS frames from a pure-Rust (tlottie) helper process confined by bubblewrap.
//! Open and frame calls belong on workers; no JSON/native parsing occurs in GTK.
use crate::downloads::VerifiedFile;
use gtk::{gdk, gio, glib, prelude::*};
use std::{
    fs::File,
    io::{Read, Seek, Write},
    os::fd::{AsRawFd, RawFd},
    path::PathBuf,
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
    time::{Duration, Instant},
};
pub type Result<T> = std::result::Result<T, &'static str>;
const INPUT_LIMIT: usize = 20 * 1024 * 1024;
const INITIAL_TIMEOUT: Duration = Duration::from_secs(8);
const FRAME_TIMEOUT: Duration = Duration::from_secs(2);
pub struct Frame {
    pub texture: gdk::Texture,
    pub delay: Duration,
}
pub struct Decoder {
    child: Option<Child>,
    input: ChildStdin,
    output: ChildStdout,
    cancel: gio::Cancellable,
    pub width: u32,
    pub height: u32,
    pub frames: u32,
    pub fps: f64,
    started: Instant,
    failed: bool,
}
#[repr(C)]
struct PollFd {
    fd: i32,
    events: i16,
    revents: i16,
}
unsafe extern "C" {
    fn poll(fds: *mut PollFd, nfds: usize, timeout: i32) -> i32;
}
fn ready(fd: RawFd, events: i16, deadline: Instant, cancel: &gio::Cancellable) -> Result<()> {
    loop {
        if cancel.is_cancelled() {
            return Err("Sticker loading cancelled");
        }
        if Instant::now() >= deadline {
            return Err("Sticker renderer timed out");
        }
        let mut p = PollFd {
            fd,
            events,
            revents: 0,
        };
        let value = unsafe { poll(&mut p, 1, 20) };
        if value < 0 {
            if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err("Sticker renderer unavailable");
        }
        if value > 0 {
            if p.revents & events != 0 {
                return Ok(());
            }
            return Err("Sticker renderer closed");
        }
    }
}
fn write_pipe(
    pipe: &mut ChildStdin,
    mut bytes: &[u8],
    deadline: Instant,
    cancel: &gio::Cancellable,
) -> Result<()> {
    while !bytes.is_empty() {
        ready(pipe.as_raw_fd(), 4, deadline, cancel)?;
        let take = bytes.len().min(4096);
        match pipe.write(&bytes[..take]) {
            Ok(0) => return Err("Sticker renderer closed"),
            Ok(n) => bytes = &bytes[n..],
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => return Err("Sticker renderer closed"),
        }
    }
    Ok(())
}
fn read_pipe(
    pipe: &mut ChildStdout,
    mut bytes: &mut [u8],
    deadline: Instant,
    cancel: &gio::Cancellable,
) -> Result<()> {
    while !bytes.is_empty() {
        ready(pipe.as_raw_fd(), 1, deadline, cancel)?;
        match pipe.read(bytes) {
            Ok(0) => return Err("Sticker renderer closed"),
            Ok(n) => bytes = &mut bytes[n..],
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => return Err("Sticker renderer closed"),
        }
    }
    Ok(())
}
/// The renderer is built beside this executable (or one level up for test binaries).
fn helper() -> Result<PathBuf> {
    const NAME: &str = "monogram-sticker-renderer";
    let beside = std::env::current_exe().ok().and_then(|exe| {
        let dir = exe.parent()?;
        [dir.join(NAME), dir.parent()?.join(NAME)].into_iter().find(|p| p.is_file())
    });
    program("MONOGRAM_LOTTIE_HELPER", beside)
}
fn program(variable: &str, fallback: Option<PathBuf>) -> Result<PathBuf> {
    let path = std::env::var_os(variable)
        .map(PathBuf::from)
        .or(fallback)
        .ok_or("Sandboxed sticker renderer is unavailable")?;
    if !path.is_absolute() || !path.is_file() {
        return Err("Sandboxed sticker renderer is unavailable");
    }
    Ok(path)
}
fn number(bytes: &[u8]) -> u32 {
    u32::from_le_bytes(bytes.try_into().unwrap())
}
/// Bundled animation bytes as an anonymous in-memory file, so they travel the same descriptor
/// path as a downloaded sticker without ever touching the disk.
/// How much a frame draws: the sum of its alpha channel. Zero for an empty frame.
pub fn ink(texture: &gdk::Texture) -> u64 {
    let (width, height) = (texture.width() as usize, texture.height() as usize);
    let mut pixels = vec![0u8; width * height * 4];
    texture.download(&mut pixels, width * 4);
    pixels.chunks(4).map(|px| u64::from(px[3])).sum()
}
pub fn memory_source(bytes: &[u8]) -> Result<VerifiedFile> {
    use std::os::fd::FromRawFd;
    let fd = unsafe { libc::memfd_create(c"monogram-emoji".as_ptr(), libc::MFD_CLOEXEC) };
    if fd < 0 {
        return Err("Animated emoji are unavailable");
    }
    let mut file = unsafe { File::from_raw_fd(fd) };
    file.write_all(bytes)
        .and_then(|_| file.rewind())
        .map_err(|_| "Animated emoji are unavailable")?;
    Ok(VerifiedFile(file))
}
impl Decoder {
    /// Consumes a verified, retained descriptor. Caller must run this on a worker.
    pub fn open(input: VerifiedFile, edge: u32, cancel: &gio::Cancellable) -> Result<Self> {
        Self::open_with(input, edge, cancel, false)
    }
    /// A bundled Noto emoji animation: larger and longer than a sticker, rendered with the
    /// relaxed limits the helper applies to its `MGE1` requests.
    pub fn open_emoji(input: VerifiedFile, edge: u32, cancel: &gio::Cancellable) -> Result<Self> {
        Self::open_with(input, edge, cancel, true)
    }
    fn open_with(
        mut input: VerifiedFile,
        edge: u32,
        cancel: &gio::Cancellable,
        emoji: bool,
    ) -> Result<Self> {
        if !(1..=512).contains(&edge) || cancel.is_cancelled() {
            return Err("Sticker loading cancelled");
        }
        let metadata = input
            .0
            .metadata()
            .map_err(|_| "Sticker artwork is unavailable")?;
        if !metadata.is_file() || metadata.len() == 0 || metadata.len() > INPUT_LIMIT as u64 {
            return Err("Sticker exceeds the safe size limit");
        }
        input
            .0
            .rewind()
            .map_err(|_| "Sticker artwork is unavailable")?;
        let mut bytes = Vec::with_capacity(metadata.len() as usize);
        let mut buffer = [0; 16 * 1024];
        loop {
            if cancel.is_cancelled() {
                return Err("Sticker loading cancelled");
            }
            let n = input
                .0
                .read(&mut buffer)
                .map_err(|_| "Sticker artwork is unavailable")?;
            if n == 0 {
                break;
            }
            if bytes.len() + n > INPUT_LIMIT {
                return Err("Sticker exceeds the safe size limit");
            }
            bytes.extend_from_slice(&buffer[..n]);
        }
        if !bytes.starts_with(&[0x1f, 0x8b]) {
            return Err("Invalid TGS sticker");
        }
        let helper = helper()?;
        let bwrap = program("MONOGRAM_BWRAP", Some(env!("MONOGRAM_BUILT_BWRAP").into()))?;
        let mut command = Command::new(bwrap);
        crate::renderer_sandbox::runtime(&mut command);
        command
            .env_clear()
            .args([
                "--unshare-all",
                "--die-with-parent",
                "--new-session",
                "--cap-drop",
                "ALL",
                "--clearenv",
                "--proc",
                "/proc",
                "--dev",
                "/dev",
                "--tmpfs",
                "/tmp",
                "--dir",
                "/tmp/empty",
                "--chdir",
                "/tmp/empty",
                "--setenv",
                "HOME",
                "/nonexistent",
                "--ro-bind",
            ])
            .arg(helper)
            .arg("/renderer")
            .arg("/renderer")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let mut child = command
            .spawn()
            .map_err(|_| "Sandboxed sticker renderer could not start")?;
        let pipes = child.stdin.take().zip(child.stdout.take());
        let Some((writer, reader)) = pipes else {
            let _ = child.kill();
            let _ = child.wait();
            return Err("Sticker renderer unavailable");
        };
        let mut decoder = Self {
            child: Some(child),
            input: writer,
            output: reader,
            cancel: cancel.clone(),
            width: edge,
            height: edge,
            frames: 0,
            fps: 0.,
            started: Instant::now(),
            failed: false,
        };
        let result = (|| {
            let deadline = Instant::now() + INITIAL_TIMEOUT;
            let mut request = Vec::with_capacity(12);
            request.extend_from_slice(if emoji { b"MGE1" } else { b"MGT1" });
            request.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
            request.extend_from_slice(&edge.to_le_bytes());
            write_pipe(&mut decoder.input, &request, deadline, cancel)?;
            write_pipe(&mut decoder.input, &bytes, deadline, cancel)?;
            let mut header = [0; 20];
            read_pipe(&mut decoder.output, &mut header, deadline, cancel)?;
            if &header[..4] != b"MGF1"
                || number(&header[4..8]) != edge
                || number(&header[8..12]) != edge
            {
                return Err("Invalid sticker renderer response");
            }
            let frames = number(&header[12..16]);
            let fps = f64::from(number(&header[16..20])) / 1000.;
            let (max_frames, max_seconds) = if emoji { (360, 6.11) } else { (180, 3.01) };
            if frames == 0
                || frames > max_frames
                || fps <= 0.
                || fps > 60.
                || f64::from(frames) / fps > max_seconds
            {
                return Err("Invalid TGS animation dimensions or duration");
            }
            decoder.frames = frames;
            decoder.fps = fps;
            decoder.started = Instant::now();
            Ok(())
        })();
        result?;
        Ok(decoder)
    }
    /// Pull exactly the current clock frame. Missed frames are skipped, never queued.
    pub fn frame(&mut self) -> Result<Frame> {
        let index = ((self.started.elapsed().as_secs_f64() * self.fps) as u64
            % u64::from(self.frames)) as u32;
        self.frame_at(index)
    }
    /// Where an emoji comes to rest when it has played. Noto animations enter, loop and then leave,
    /// so the last frame is often empty: freezing there would make the emoji vanish. This walks back
    /// from the end to the last frame that is fully visible (at least 90% of `peak`, the most ink
    /// any frame showed), or keeps the last frame when it is plainly visible already. `None` when
    /// the animation never rests (it only appears and leaves), so the ordinary emoji should show.
    pub fn resting_frame(&mut self, peak: u64) -> Result<Option<Frame>> {
        let last = self.frames.saturating_sub(1);
        let entrance = self.frames / 4;
        let mut index = last;
        loop {
            let frame = self.frame_at(index)?;
            let seen = ink(&frame.texture);
            let kept_ending = index == last && (peak == 0 || seen * 10 >= peak * 3);
            if kept_ending || seen * 10 >= peak * 9 {
                return Ok((kept_ending || index >= entrance).then_some(frame));
            }
            if index == 0 {
                return Ok(None);
            }
            index -= 1;
        }
    }
    pub fn frame_at(&mut self, index: u32) -> Result<Frame> {
        if self.failed || index >= self.frames {
            return Err("Sticker renderer unavailable");
        }
        let result = (|| {
            let deadline = Instant::now() + FRAME_TIMEOUT;
            write_pipe(
                &mut self.input,
                &index.to_le_bytes(),
                deadline,
                &self.cancel,
            )?;
            let mut header = [0; 8];
            read_pipe(&mut self.output, &mut header, deadline, &self.cancel)?;
            let length = self.width as usize * self.height as usize * 4;
            if number(&header[..4]) != index || number(&header[4..]) as usize != length {
                return Err("Invalid sticker frame response");
            }
            let mut pixels = vec![0; length];
            read_pipe(&mut self.output, &mut pixels, deadline, &self.cancel)?;
            let bytes = glib::Bytes::from_owned(pixels);
            let texture = gdk::MemoryTexture::new(
                self.width as i32,
                self.height as i32,
                gdk::MemoryFormat::R8g8b8a8Premultiplied,
                &bytes,
                self.width as usize * 4,
            )
            .upcast();
            let delay = Duration::from_secs_f64(1. / self.fps);
            Ok(Frame { texture, delay })
        })();
        if result.is_err() {
            self.failed = true;
            if let Some(child)=self.child.as_mut(){let _=child.kill();let _=child.wait();}
        }
        result
    }
}
impl Drop for Decoder {
    fn drop(&mut self) {
        if let Some(mut child)=self.child.take(){
            let _=child.kill();
            // Timeline eviction can happen on GTK. Reaping a sandbox must never
            // make scrolling wait for a child process to finish exiting.
            gio::spawn_blocking(move || {let _=child.wait();});
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    };
    /// A moving red circle with the given canvas size, frame rate and length.
    fn fixture(width: u32, height: u32, fps: u32, frames: u32) -> Vec<u8> {
        let json = format!(
            r#"{{"v":"5.7.4","fr":{fps},"ip":0,"op":{frames},"w":{width},"h":{height},"nm":"fixture","ddd":0,"assets":[],"layers":[{{"ddd":0,"ind":1,"ty":4,"nm":"c","sr":1,"ks":{{"o":{{"a":0,"k":100}},"r":{{"a":0,"k":0}},"p":{{"a":1,"k":[{{"t":0,"s":[{a},{m},0],"e":[{b},{m},0],"i":{{"x":1,"y":1}},"o":{{"x":0,"y":0}}}},{{"t":{last},"s":[{b},{m},0]}}]}},"a":{{"a":0,"k":[0,0,0]}},"s":{{"a":0,"k":[100,100,100]}}}},"ao":0,"shapes":[{{"ty":"el","d":1,"s":{{"a":0,"k":[{d},{d}]}},"p":{{"a":0,"k":[0,0]}},"nm":"e"}},{{"ty":"fl","c":{{"a":0,"k":[1,0,0,1]}},"o":{{"a":0,"k":100}},"r":1,"nm":"r"}}],"ip":0,"op":{frames},"st":0,"bm":0}}]}}"#,
            a = width / 4,
            b = width * 3 / 4,
            m = height / 2,
            d = width / 5,
            last = frames - 1,
        );
        let mut gzip = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        gzip.write_all(json.as_bytes()).unwrap();
        gzip.finish().unwrap()
    }
    fn alpha_sum(texture: &gdk::Texture) -> u64 {
        ink(texture)
    }

    #[test]
    #[ignore = "requires the built sandboxed renderer and bubblewrap"]
    fn emoji_mode_allows_larger_longer_animations_but_stickers_stay_strict() {
        let open = |emoji: bool, bytes: Vec<u8>| {
            let source = memory_source(&bytes).unwrap();
            let cancel = gio::Cancellable::new();
            if emoji { Decoder::open_emoji(source, 64, &cancel) } else { Decoder::open(source, 64, &cancel) }
        };
        // A Noto-shaped animation: 1024x1024, 60 fps, 140 frames (2.3 s).
        assert!(open(true, fixture(1024, 1024, 60, 140)).is_ok());
        assert!(open(false, fixture(1024, 1024, 60, 140)).is_err(), "a sticker must be exactly 512x512");
        assert!(open(false, fixture(512, 512, 30, 30)).is_ok(), "ordinary stickers are unchanged");
        assert!(open(true, fixture(512, 512, 60, 300)).is_ok(), "5 s fits the emoji limit");
        for (name, bytes) in [
            ("larger than 1024", fixture(2048, 2048, 30, 30)),
            ("not square", fixture(1024, 512, 30, 30)),
            ("over 6 seconds", fixture(512, 512, 30, 200)),
            ("over 360 frames", fixture(512, 512, 60, 361)),
        ] {
            assert!(open(true, bytes).is_err(), "{name} must be refused even for emoji");
        }
        assert!(open(true, b"not gzip".to_vec()).is_err());
    }

    #[test]
    #[ignore = "timing probe: prints per-frame render cost in the sandbox"]
    fn emoji_frame_cost_probe() {
        let pack = crate::noto_emoji::curated().unwrap();
        let cancel = gio::Cancellable::new();
        for key in ["1f604", "1f602", "2764", "1f525"] {
            for edge in [96u32, 128, 192] {
                let source = memory_source(pack.get(key).unwrap()).unwrap();
                let mut decoder = Decoder::open_emoji(source, edge, &cancel).unwrap();
                let frames = decoder.frames.min(30);
                let began = Instant::now();
                for index in 0..frames {
                    decoder.frame_at(index).unwrap();
                }
                eprintln!("{key} edge {edge}: {} frames at {} fps, {:.1} ms/frame", decoder.frames, decoder.fps, began.elapsed().as_secs_f64() * 1000. / f64::from(frames));
            }
        }
    }

    /// Noto animations enter, loop and leave: many end on an empty frame. Whatever the emoji rests on
    /// must be visible, and one that never rests is left to the ordinary emoji.
    #[test]
    #[ignore = "requires the built sandboxed renderer and bubblewrap"]
    fn every_bundled_emoji_rests_on_a_visible_frame() {
        let pack = crate::noto_emoji::curated().expect("bundled pack");
        let cancel = gio::Cancellable::new();
        let (mut blank_ending, mut without_rest) = (0, vec![]);
        for key in pack.keys().map(str::to_owned).collect::<Vec<_>>() {
            let source = memory_source(pack.get(&key).unwrap()).unwrap();
            let mut decoder = Decoder::open_emoji(source, 64, &cancel).unwrap();
            let inks: Vec<u64> = (0..decoder.frames).map(|i| ink(&decoder.frame_at(i).unwrap().texture)).collect();
            let peak = *inks.iter().max().unwrap();
            if inks[inks.len() - 1] * 10 < peak * 3 {
                blank_ending += 1;
            }
            match decoder.resting_frame(peak).unwrap_or_else(|e| panic!("{key}: {e}")) {
                Some(frame) => assert!(ink(&frame.texture) * 10 >= peak * 3, "{key} rests on a nearly empty frame"),
                None => without_rest.push(key),
            }
        }
        assert!(blank_ending > 10, "the pack is expected to contain animations that end empty");
        assert!(without_rest.len() <= 5, "only a few may never rest: {without_rest:?}");
    }

    #[test]
    #[ignore = "requires the built sandboxed renderer and bubblewrap"]
    fn every_bundled_emoji_renders_and_moves_in_the_sandbox() {
        let pack = crate::noto_emoji::curated().expect("bundled pack");
        let cancel = gio::Cancellable::new();
        let mut still = Vec::new();
        for key in pack.keys().map(str::to_owned).collect::<Vec<_>>() {
            let source = memory_source(pack.get(&key).unwrap()).unwrap();
            let mut decoder = Decoder::open_emoji(source, 128, &cancel).unwrap_or_else(|e| panic!("{key}: {e}"));
            let first = decoder.frame_at(0).unwrap_or_else(|e| panic!("{key} first frame: {e}")).texture;
            let middle = decoder.frame_at(decoder.frames / 2).unwrap_or_else(|e| panic!("{key} middle frame: {e}")).texture;
            assert!(alpha_sum(&first) > 0 || alpha_sum(&middle) > 0, "{key} draws something");
            if first.save_to_png_bytes() == middle.save_to_png_bytes() {
                still.push(key);
            }
        }
        // A few emoji hold still at their midpoint; nearly all must animate.
        assert!(still.len() * 10 < pack.len(), "{} of {} do not move: {still:?}", still.len(), pack.len());
    }

    #[test]
    fn sandboxed_tgs_pulls_moving_frames_without_blocking_the_main_context() {
        let json=br#"{"v":"5.7.4","fr":30,"ip":0,"op":30,"w":512,"h":512,"nm":"Private fixture","ddd":0,"assets":[],"layers":[{"ddd":0,"ind":1,"ty":4,"nm":"Moving circle","sr":1,"ks":{"o":{"a":0,"k":100},"r":{"a":0,"k":0},"p":{"a":1,"k":[{"t":0,"s":[128,256,0],"e":[384,256,0],"i":{"x":1,"y":1},"o":{"x":0,"y":0}},{"t":29,"s":[384,256,0]}]},"a":{"a":0,"k":[0,0,0]},"s":{"a":0,"k":[100,100,100]}},"ao":0,"shapes":[{"ty":"el","d":1,"s":{"a":0,"k":[100,100]},"p":{"a":0,"k":[0,0]},"nm":"Circle"},{"ty":"fl","c":{"a":0,"k":[1,0,0,1]},"o":{"a":0,"k":100},"r":1,"nm":"Red"}],"ip":0,"op":30,"st":0,"bm":0}]}"#;
        let mut gzip = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        gzip.write_all(json).unwrap();
        let data = gzip.finish().unwrap();
        let path = std::env::temp_dir().join(format!("monogram-tgs-{}.tgs", uuid::Uuid::new_v4()));
        std::fs::write(&path, data).unwrap();
        let input = VerifiedFile(File::open(&path).unwrap());
        let done = Arc::new(AtomicBool::new(false));
        let finished = done.clone();
        let worker = std::thread::spawn(move || {
            let cancel = gio::Cancellable::new();
            let mut decoder =
                Decoder::open(input, 64, &cancel).expect("real sandboxed sticker fixture (run `cargo build --bins` first)");
            assert_eq!(decoder.frames, 30);
            let first = decoder.frame_at(0).unwrap().texture.save_to_png_bytes();
            let second = decoder.frame_at(15).unwrap().texture.save_to_png_bytes();
            assert_ne!(first.as_ref(), second.as_ref());
            let began = Instant::now();
            cancel.cancel();
            assert!(decoder.frame().is_err());
            assert!(began.elapsed() < Duration::from_millis(400));
            drop(decoder);
            finished.store(true, Ordering::Release);
        });
        let context = glib::MainContext::new();
        let beats = Arc::new(AtomicUsize::new(0));
        let heartbeat = beats.clone();
        let source = glib::timeout_source_new(
            Duration::from_millis(2),
            None,
            glib::Priority::DEFAULT,
            move || {
                heartbeat.fetch_add(1, Ordering::Relaxed);
                glib::ControlFlow::Continue
            },
        );
        source.attach(Some(&context));
        let start = Instant::now();
        while !done.load(Ordering::Acquire)
            && !worker.is_finished()
            && start.elapsed() < Duration::from_secs(12)
        {
            context.iteration(false);
            std::thread::sleep(Duration::from_millis(1));
        }
        source.destroy();
        worker.join().unwrap();
        assert!(done.load(Ordering::Acquire));
        assert!(beats.load(Ordering::Relaxed) > 0);
        std::fs::remove_file(path).unwrap();
    }
}
