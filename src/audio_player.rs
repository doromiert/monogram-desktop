//! Plays the raw PCM stream from the sandboxed audio helper.
//!
//! The pipeline here is `appsrc ! audioconvert ! audioresample ! <audio sink>`: no demuxer, no
//! decoder, no URI handling. Everything that parses an audio file runs in the helper; the only
//! thing this process hands to GStreamer is signed 16-bit, 48 kHz, stereo samples, which cannot
//! be anything but samples. Seeking restarts the helper at the new position instead of seeking
//! inside a container.
//!
//! Main-thread only. `PlayState` changes are reported through `connect_changed`.
use crate::audio_check::Container;
use crate::audio_preview_run::{PcmStream, PCM_BYTES_PER_MS, PCM_CHANNELS, PCM_RATE};
use gst::prelude::*;
use gstreamer as gst;
use gstreamer_app as gst_app;
use gtk::{gio, glib};
use std::{
    cell::{Cell, RefCell},
    fs::File,
    io::Read,
    rc::Rc,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};

/// How much PCM is read and queued at a time: 100 ms.
const CHUNK: usize = PCM_BYTES_PER_MS as usize * 100;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlayState {
    Idle,
    /// The helper is starting.
    Starting,
    Playing,
    Paused,
}

struct Session {
    pipeline: gst::Pipeline,
    stop: Arc<AtomicBool>,
    _watch: gst::bus::BusWatchGuard,
}
impl Drop for Session {
    fn drop(&mut self) {
        // Telling the feeder first lets it stop reading; the helper ends when its stream drops.
        self.stop.store(true, Ordering::SeqCst);
        let _ = self.pipeline.set_state(gst::State::Null);
    }
}

struct Shared {
    file: Arc<File>,
    container: Container,
    duration_ms: u64,
    sink: &'static str,
    session: RefCell<Option<Session>>,
    state: Cell<PlayState>,
    /// Where the current session started, in milliseconds into the file.
    start_ms: Cell<u64>,
    /// Bumped on every start and stop, so a late helper start from an older request is ignored.
    generation: Cell<u64>,
    changed: RefCell<Option<Box<dyn Fn()>>>,
    /// Set when the last session ended by reaching the end of the audio.
    finished: Cell<bool>,
    /// Why the last session ended abnormally (no sound device, a failed sink...), if it did.
    error: RefCell<Option<String>>,
}

#[derive(Clone)]
pub struct AudioPlayer(Rc<Shared>);

impl AudioPlayer {
    pub fn new(file: File, container: Container, duration_ms: u64) -> Self {
        Self::with_sink(file, container, duration_ms, "autoaudiosink")
    }
    /// For tests and unusual setups: a different sink element (for example `fakesink`).
    pub fn with_sink(file: File, container: Container, duration_ms: u64, sink: &'static str) -> Self {
        Self(Rc::new(Shared {
            file: Arc::new(file),
            container,
            duration_ms,
            sink,
            session: RefCell::new(None),
            state: Cell::new(PlayState::Idle),
            start_ms: Cell::new(0),
            generation: Cell::new(0),
            changed: RefCell::new(None),
            finished: Cell::new(false),
            error: RefCell::new(None),
        }))
    }

    pub fn state(&self) -> PlayState {
        self.0.state.get()
    }
    pub fn duration_ms(&self) -> u64 {
        self.0.duration_ms
    }
    /// True once, right after the audio played to its end.
    pub fn take_finished(&self) -> bool {
        self.0.finished.replace(false)
    }
    /// Why playback stopped on its own, once: for example that no audio output could be opened.
    pub fn take_error(&self) -> Option<String> {
        self.0.error.take()
    }
    pub fn connect_changed(&self, callback: impl Fn() + 'static) {
        self.0.changed.replace(Some(Box::new(callback)));
    }
    fn notify(&self) {
        if let Some(callback) = self.0.changed.borrow().as_ref() {
            callback();
        }
    }
    fn set_state(&self, state: PlayState) {
        if self.0.state.replace(state) != state {
            self.notify();
        }
    }

    /// Playback position in milliseconds into the file.
    pub fn position_ms(&self) -> u64 {
        let played = self
            .0
            .session
            .borrow()
            .as_ref()
            .and_then(|session| session.pipeline.query_position::<gst::ClockTime>())
            .map_or(0, |position| position.mseconds());
        (self.0.start_ms.get() + played).min(self.0.duration_ms)
    }

    pub fn toggle(&self) {
        match self.state() {
            PlayState::Idle => self.play_from(0),
            PlayState::Playing => self.pause(),
            PlayState::Paused => self.resume(),
            PlayState::Starting => {}
        }
    }
    pub fn pause(&self) {
        if let Some(session) = self.0.session.borrow().as_ref() {
            if session.pipeline.set_state(gst::State::Paused).is_ok() {
                self.set_state(PlayState::Paused);
            }
        }
    }
    pub fn resume(&self) {
        if let Some(session) = self.0.session.borrow().as_ref() {
            if session.pipeline.set_state(gst::State::Playing).is_ok() {
                self.set_state(PlayState::Playing);
            }
        }
    }
    pub fn stop(&self) {
        self.0.generation.set(self.0.generation.get() + 1);
        self.0.session.take();
        self.0.start_ms.set(0);
        self.set_state(PlayState::Idle);
    }

    /// Start (or restart) playback at `ms`. The helper is started on a worker thread.
    pub fn play_from(&self, ms: u64) {
        let ms = ms.min(self.0.duration_ms.saturating_sub(1));
        self.0.session.take();
        let generation = self.0.generation.get() + 1;
        self.0.generation.set(generation);
        self.0.finished.set(false);
        self.set_state(PlayState::Starting);
        let (file, container) = (self.0.file.clone(), self.0.container);
        let started = gio::spawn_blocking(move || PcmStream::start(&file, container, ms));
        let player = self.clone();
        glib::spawn_future_local(async move {
            let stream = started.await;
            if player.0.generation.get() != generation {
                return; // stopped or restarted while the helper was starting
            }
            match stream {
                Ok(Ok(stream)) => player.begin(stream, ms),
                _ => player.set_state(PlayState::Idle),
            }
        });
    }

    fn begin(&self, stream: PcmStream, start_ms: u64) {
        let Some((pipeline, source)) = self.pipeline() else {
            self.set_state(PlayState::Idle);
            return;
        };
        let stop = Arc::new(AtomicBool::new(false));
        let weak = Rc::downgrade(&self.0);
        let player = self.clone();
        let generation = self.0.generation.get();
        let watch = pipeline.bus().and_then(|bus| {
            bus.add_watch_local(move |_, message| {
                use gst::MessageView;
                match message.view() {
                    MessageView::Eos(_) => {
                        if let Some(shared) = weak.upgrade() {
                            if shared.generation.get() == generation {
                                shared.finished.set(true);
                                player.stop();
                            }
                        }
                        glib::ControlFlow::Break
                    }
                    MessageView::Error(error) => {
                        if let Some(shared) = weak.upgrade().filter(|shared| shared.generation.get() == generation) {
                            let reason = format!("{}", error.error());
                            eprintln!("Monogram audio playback error: {reason} ({})", error.debug().unwrap_or_default());
                            shared.error.replace(Some(reason));
                            player.stop();
                        }
                        glib::ControlFlow::Break
                    }
                    _ => glib::ControlFlow::Continue,
                }
            })
            .ok()
        });
        let Some(watch) = watch else {
            self.set_state(PlayState::Idle);
            return;
        };
        self.0.start_ms.set(start_ms);
        let feeder_stop = stop.clone();
        std::thread::spawn(move || feed(stream, source, feeder_stop));
        if pipeline.set_state(gst::State::Playing).is_err() {
            self.set_state(PlayState::Idle);
            return;
        }
        self.0.session.replace(Some(Session { pipeline, stop, _watch: watch }));
        self.set_state(PlayState::Playing);
    }

    /// `appsrc ! audioconvert ! audioresample ! sink`, and the appsrc to feed.
    fn pipeline(&self) -> Option<(gst::Pipeline, gst_app::AppSrc)> {
        gst::init().ok()?;
        let caps = gst::Caps::builder("audio/x-raw")
            .field("format", "S16LE")
            .field("rate", PCM_RATE as i32)
            .field("channels", PCM_CHANNELS as i32)
            .field("layout", "interleaved")
            .build();
        let source = gst::ElementFactory::make("appsrc").build().ok()?.downcast::<gst_app::AppSrc>().ok()?;
        source.set_caps(Some(&caps));
        source.set_format(gst::Format::Time);
        // Block the feeder when about a second is queued, so a paused player holds the helper.
        source.set_block(true);
        source.set_max_bytes(PCM_BYTES_PER_MS * 1000);
        let convert = gst::ElementFactory::make("audioconvert").build().ok()?;
        let resample = gst::ElementFactory::make("audioresample").build().ok()?;
        let sink = gst::ElementFactory::make(self.0.sink).build().ok()?;
        if self.0.sink == "fakesink" {
            sink.set_property("sync", true);
        }
        let pipeline = gst::Pipeline::new();
        pipeline.add_many([source.upcast_ref::<gst::Element>(), &convert, &resample, &sink]).ok()?;
        gst::Element::link_many([source.upcast_ref::<gst::Element>(), &convert, &resample, &sink]).ok()?;
        Some((pipeline, source))
    }
}

/// Read PCM from the helper and queue it, with timestamps from the byte count, until the end of
/// the stream or until asked to stop. Runs on its own thread; `push_buffer` blocks while the
/// player holds a second of audio, which in turn holds the helper.
fn feed(mut stream: PcmStream, source: gst_app::AppSrc, stop: Arc<AtomicBool>) {
    let (mut chunk, mut bytes_pushed) = (vec![0u8; CHUNK], 0u64);
    let mut have = 0usize;
    loop {
        if stop.load(Ordering::SeqCst) {
            return;
        }
        match stream.read(&mut chunk[have..]) {
            Ok(0) => break,
            Ok(read) => have += read,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        }
        // Push whole 4-byte frames; keep a partial one for the next read.
        let whole = have - have % 4;
        if whole == 0 || (whole < CHUNK && have < CHUNK) {
            continue;
        }
        let mut buffer = gst::Buffer::from_slice(chunk[..whole].to_vec());
        {
            let buffer = buffer.get_mut().expect("a new buffer is writable");
            buffer.set_pts(gst::ClockTime::from_nseconds(bytes_pushed * 1_000_000_000 / (PCM_RATE as u64 * 4)));
            buffer.set_duration(gst::ClockTime::from_nseconds(whole as u64 * 1_000_000_000 / (PCM_RATE as u64 * 4)));
        }
        if source.push_buffer(buffer).is_err() {
            return; // the pipeline was stopped
        }
        bytes_pushed += whole as u64;
        chunk.copy_within(whole..have, 0);
        have -= whole;
    }
    // Whatever whole frames remain, then the end of the stream.
    let whole = have - have % 4;
    if whole > 0 && !stop.load(Ordering::SeqCst) {
        let mut buffer = gst::Buffer::from_slice(chunk[..whole].to_vec());
        buffer.get_mut().expect("a new buffer is writable").set_pts(gst::ClockTime::from_nseconds(bytes_pushed * 1_000_000_000 / (PCM_RATE as u64 * 4)));
        let _ = source.push_buffer(buffer);
    }
    let _ = source.end_of_stream();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn fixture(name: &str) -> File {
        File::open(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/audio").join(name)).unwrap()
    }
    /// Run the GLib main context until `done` or the timeout.
    fn wait(timeout: Duration, mut done: impl FnMut() -> bool) -> bool {
        let context = glib::MainContext::default();
        let until = Instant::now() + timeout;
        while Instant::now() < until {
            while context.iteration(false) {}
            if done() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        false
    }
    fn player(name: &str, container: Container, duration: u64) -> AudioPlayer {
        // `fakesink` stands in for the sound card: same clock, no device.
        AudioPlayer::with_sink(fixture(name), container, duration, "fakesink")
    }

    #[test]
    #[ignore = "requires the built audio helper, GStreamer plugins and bubblewrap"]
    fn it_plays_to_the_end_reports_progress_and_goes_back_to_idle() {
        let player = player("tone-opus.ogg", Container::Ogg, 1000);
        let seen = Rc::new(RefCell::new(Vec::new()));
        let log = seen.clone();
        let watcher = player.clone();
        player.connect_changed(move || log.borrow_mut().push(watcher.state()));
        player.play_from(0);
        assert_eq!(player.state(), PlayState::Starting);
        assert!(wait(Duration::from_secs(10), || player.state() == PlayState::Playing), "it starts");
        let began = Instant::now();
        let mut last = 0;
        wait(Duration::from_secs(3), || {
            let now = player.position_ms();
            // While playing the position only moves forward; at the end it resets to the start.
            if player.state() == PlayState::Playing {
                assert!(now >= last, "the position never goes backwards while playing ({last} then {now})");
                last = now;
            }
            player.state() == PlayState::Idle
        });
        assert_eq!(player.state(), PlayState::Idle, "it finished by itself");
        assert!(player.take_finished(), "and says it finished");
        assert!(!player.take_finished(), "only once");
        assert!(began.elapsed() > Duration::from_millis(500), "it ran in real time ({:?})", began.elapsed());
        assert_eq!(player.position_ms(), 0, "back at the start");
        assert_eq!(*seen.borrow(), [PlayState::Starting, PlayState::Playing, PlayState::Idle]);
    }

    /// Silent audio through the real sound system (`autoaudiosink`), not `fakesink`: the only place a
    /// missing or unusable output shows up. Silence, so nothing is heard.
    #[test]
    #[ignore = "uses the real audio output (silently); requires the built helper and bubblewrap"]
    fn playing_through_the_real_audio_output_starts_and_ends_cleanly() {
        let mut wav = b"RIFF".to_vec();
        let data = 8_000 * 2 / 2; // 0.5 s of 8 kHz mono 16-bit silence
        wav.extend_from_slice(&(36 + data as u32).to_le_bytes());
        wav.extend_from_slice(b"WAVEfmt ");
        wav.extend_from_slice(&16u32.to_le_bytes());
        for v in [1u16, 1] { wav.extend_from_slice(&v.to_le_bytes()); }
        wav.extend_from_slice(&8_000u32.to_le_bytes());
        wav.extend_from_slice(&16_000u32.to_le_bytes());
        for v in [2u16, 16] { wav.extend_from_slice(&v.to_le_bytes()); }
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&(data as u32).to_le_bytes());
        wav.resize(wav.len() + data, 0);
        let path = std::env::temp_dir().join(format!("silence-{}.wav", uuid::Uuid::new_v4()));
        std::fs::write(&path, &wav).unwrap();
        let player = AudioPlayer::new(File::open(&path).unwrap(), Container::Wav, 500);
        std::fs::remove_file(&path).unwrap();
        player.play_from(0);
        assert!(wait(Duration::from_secs(10), || player.state() != PlayState::Starting), "it left the starting state");
        let mut saw_playing = player.state() == PlayState::Playing;
        wait(Duration::from_secs(5), || {
            saw_playing |= player.state() == PlayState::Playing;
            player.state() == PlayState::Idle
        });
        let error = player.take_error();
        assert!(error.is_none(), "playback through the real output failed: {error:?}");
        assert!(saw_playing, "it reached Playing");
        assert!(player.take_finished(), "and played to its end");
    }

    #[test]
    #[ignore = "requires the built audio helper, GStreamer plugins and bubblewrap"]
    fn pausing_holds_the_position_resuming_continues_and_seeking_restarts_the_helper() {
        let player = player("tone.mp3", Container::Mp3, 1100);
        player.play_from(0);
        assert!(wait(Duration::from_secs(10), || player.state() == PlayState::Playing));
        assert!(wait(Duration::from_secs(2), || player.position_ms() >= 150));
        player.pause();
        assert_eq!(player.state(), PlayState::Paused);
        let held = player.position_ms();
        wait(Duration::from_millis(500), || false);
        assert!(player.position_ms().abs_diff(held) <= 60, "paused: {held} then {}", player.position_ms());
        player.resume();
        assert_eq!(player.state(), PlayState::Playing);
        // Seeking restarts at the new place and the position follows it.
        player.play_from(700);
        assert!(wait(Duration::from_secs(10), || player.state() == PlayState::Playing));
        assert!(player.position_ms() >= 700, "after seeking to 700 ms the position is {}", player.position_ms());
        assert!(wait(Duration::from_secs(3), || player.state() == PlayState::Idle), "the rest plays out");
    }

    #[test]
    #[ignore = "requires the built audio helper, GStreamer plugins and bubblewrap"]
    fn stopping_while_the_helper_is_starting_never_leaves_it_playing() {
        let player = player("tone.wav", Container::Wav, 300);
        player.play_from(0);
        player.stop();
        assert_eq!(player.state(), PlayState::Idle);
        wait(Duration::from_secs(2), || false);
        assert_eq!(player.state(), PlayState::Idle, "the late start was ignored");
        // A new request after a stop works, and a request that replaces another wins.
        player.play_from(0);
        player.play_from(50);
        assert!(wait(Duration::from_secs(10), || player.state() == PlayState::Playing));
        assert!(player.position_ms() >= 50);
        player.stop();
    }

    #[test]
    fn toggling_an_idle_player_never_panics_and_a_missing_file_ends_idle() {
        // A file the helper cannot use (not audio): it must end idle, not hang or crash.
        let path = std::env::temp_dir().join(format!("player-{}", uuid::Uuid::new_v4()));
        std::fs::write(&path, b"not audio").unwrap();
        let player = AudioPlayer::with_sink(File::open(&path).unwrap(), Container::Ogg, 1000, "fakesink");
        std::fs::remove_file(&path).unwrap();
        player.pause();
        player.resume();
        player.stop();
        player.toggle();
        wait(Duration::from_secs(20), || player.state() == PlayState::Idle);
        assert_eq!(player.state(), PlayState::Idle);
    }
}
