//! On-demand TGS frames from a native rlottie process confined by bubblewrap.
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
    child: Child,
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
fn program(variable: &str, fallback: &str) -> Result<PathBuf> {
    let path = std::env::var_os(variable)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(fallback));
    if !path.is_absolute() || !path.is_file() {
        return Err("Sandboxed sticker renderer is unavailable");
    }
    Ok(path)
}
fn number(bytes: &[u8]) -> u32 {
    u32::from_le_bytes(bytes.try_into().unwrap())
}
impl Decoder {
    /// Consumes a verified, retained descriptor. Caller must run this on a worker.
    pub fn open(mut input: VerifiedFile, edge: u32, cancel: &gio::Cancellable) -> Result<Self> {
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
        let helper = program(
            "MONOGRAM_LOTTIE_HELPER",
            env!("MONOGRAM_BUILT_LOTTIE_HELPER"),
        )?;
        let bwrap = program("MONOGRAM_BWRAP", env!("MONOGRAM_BUILT_BWRAP"))?;
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
            child,
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
            request.extend_from_slice(b"MGT1");
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
            if frames == 0
                || frames > 180
                || fps <= 0.
                || fps > 60.
                || f64::from(frames) / fps > 3.01
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
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
        result
    }
}
impl Drop for Decoder {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    };
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
                Decoder::open(input, 64, &cancel).expect("real sandboxed rlottie fixture");
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
