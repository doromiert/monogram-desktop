//! Recording a voice message from the microphone to an Ogg Opus file.
//!
//! The pipeline is `source ! audioconvert ! audioresample ! mono 48 kHz ! level ! opusenc !
//! oggmux ! filesink`. Everything runs on GStreamer's own threads; the main loop only receives
//! about twenty small level messages a second (which also say how long the recording is), so
//! recording cannot make the interface stutter.
//!
//! The file is Ogg Opus, mono: what the app's own player and validator accept, and what voice
//! notes elsewhere use.
use super::meter::bar_height;
use gstreamer as gst;
use gstreamer::prelude::*;
use gtk::{gio, glib};
use std::{
    cell::RefCell,
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    rc::Rc,
    time::Duration,
};

/// A recording stops by itself at this length.
pub const MAX_LENGTH: Duration = Duration::from_secs(600);
/// How long to wait for the file to be closed properly before giving up on it.
const FINISH_WITHIN: Duration = Duration::from_secs(2);
/// Level messages come this often, in nanoseconds.
const LEVEL_INTERVAL_NS: u64 = 50_000_000;

/// Why recording could not start or carry on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Failure {
    /// The sound server refused this program the microphone.
    Denied,
    NoMicrophone,
    /// Another program has the microphone to itself.
    Busy,
    /// Anything else, in the system's words.
    Other(String),
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Failure::Denied => f.write_str("Microphone access was refused"),
            Failure::NoMicrophone => f.write_str("No microphone was found"),
            Failure::Busy => f.write_str("The microphone is in use by another program"),
            Failure::Other(text) => f.write_str(text),
        }
    }
}

/// Sort a GStreamer error message into something a person can act on.
pub fn classify(message: &str) -> Failure {
    let lower = message.to_lowercase();
    let has = |words: &[&str]| words.iter().any(|word| lower.contains(word));
    if has(&["permission denied", "not allowed", "access denied", "not permitted", "refused"]) {
        Failure::Denied
    } else if has(&["busy", "in use", "exclusive"]) {
        Failure::Busy
    } else if has(&["no such", "not found", "no target", "does not exist", "no devices", "no device", "no element", "no suitable"]) {
        Failure::NoMicrophone
    } else {
        Failure::Other(crate::text_safety::visible(message, 160))
    }
}

#[derive(Debug)]
pub enum Event {
    /// The loudness of the latest 50 ms and how much has been recorded so far.
    Level { bar: f32, at: Duration },
    /// The maximum length was reached; the recording goes on until it is stopped.
    Limit,
    /// The file is complete. `length` is what was recorded.
    Finished { length: Duration },
    /// The microphone stopped working part way through; the file holds what came before.
    Interrupted { failure: Failure, length: Duration },
    /// Recording never started.
    Failed(Failure),
}

/// Where the sound comes from.
pub enum Source {
    /// The microphone, through whichever sound server is available.
    Microphone,
    /// `gst-launch` descriptions of an audio source, tried in order. Tests use `audiotestsrc`.
    #[cfg_attr(not(test), allow(dead_code))]
    Candidates(Vec<String>),
}

/// The microphone source elements to try, best first, among those installed.
pub fn microphone_candidates(has: impl Fn(&str) -> bool) -> Vec<String> {
    [("pipewiresrc", "pipewiresrc do-timestamp=true"), ("pulsesrc", "pulsesrc"), ("autoaudiosrc", "autoaudiosrc")]
        .into_iter()
        .filter(|(name, _)| has(name))
        .map(|(_, description)| description.to_owned())
        .collect()
}

pub struct Settings {
    pub max_length: Duration,
}
impl Default for Settings {
    fn default() -> Self {
        Self { max_length: MAX_LENGTH }
    }
}

struct Inner {
    pipeline: Option<gst::Pipeline>,
    watch: Option<gst::bus::BusWatchGuard>,
    path: PathBuf,
    /// The sources to try; filled in on the worker thread, which is also where GStreamer starts.
    candidates: Vec<String>,
    candidate: usize,
    /// Bumped for every pipeline started, so a slow worker's late answer can be told apart.
    attempt: u64,
    got_level: bool,
    last_at: Duration,
    stopping: bool,
    done: bool,
    /// Thrown away on purpose: whatever a late pipeline writes is deleted again.
    cancelled: bool,
    limit_sent: bool,
    max_length: Duration,
    timer: Option<glib::Source>,
    on_event: Rc<dyn Fn(Event)>,
}

pub struct Recorder(Rc<RefCell<Inner>>);

/// Run `work` once, `after` from now, on the main context of this thread.
fn later(after: Duration, work: impl FnOnce() + 'static) -> glib::Source {
    // The source runs on this thread only, which the guard checks.
    let work = glib::thread_guard::ThreadGuard::new(RefCell::new(Some(work)));
    let source = glib::timeout_source_new(after, None, glib::Priority::DEFAULT, move || {
        if let Some(work) = work.get_ref().borrow_mut().take() {
            work();
        }
        glib::ControlFlow::Break
    });
    source.attach(Some(&glib::MainContext::ref_thread_default()));
    source
}

fn create_private_file(path: &Path) -> Result<(), Failure> {
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .map(drop)
        .map_err(|error| Failure::Other(format!("The recording could not be saved: {error}")))
}

fn pipeline_text(source: &str) -> String {
    format!(
        "{source} ! audioconvert ! audioresample ! audio/x-raw,channels=1,rate=48000 ! \
         level name=meter interval={LEVEL_INTERVAL_NS} post-messages=true ! audioconvert ! \
         opusenc bitrate=24000 complexity=5 audio-type=voice frame-size=20 ! oggmux ! \
         filesink name=out sync=false async=false"
    )
}

fn emit(inner: &Rc<RefCell<Inner>>, event: Event) {
    // Called without any borrow held: the listener may stop or cancel the recording.
    let listener = inner.borrow().on_event.clone();
    listener(event);
}

/// Stop the pipeline and let go of its bus. Safe to repeat.
fn teardown(inner: &Rc<RefCell<Inner>>) {
    let (pipeline, watch, timer) = {
        let mut inner = inner.borrow_mut();
        (inner.pipeline.take(), inner.watch.take(), inner.timer.take())
    };
    if let Some(timer) = timer {
        timer.destroy();
    }
    drop(watch);
    if let Some(pipeline) = pipeline {
        let _ = pipeline.set_state(gst::State::Null);
    }
}

/// The recording is over; say so once.
fn finish(inner: &Rc<RefCell<Inner>>, interrupted: Option<Failure>) {
    {
        let mut state = inner.borrow_mut();
        if state.done {
            return;
        }
        state.done = true;
    }
    teardown(inner);
    let length = inner.borrow().last_at;
    emit(
        inner,
        match interrupted {
            Some(failure) => Event::Interrupted { failure, length },
            None => Event::Finished { length },
        },
    );
}

/// Build and start the pipeline for the current candidate, off the main thread: starting
/// GStreamer for the first time and connecting to the sound server can take a second or more, and
/// the window must not wait for that.
fn launch(inner: &Rc<RefCell<Inner>>, source: Option<Source>) {
    let attempt = {
        let mut state = inner.borrow_mut();
        state.attempt += 1;
        state.attempt
    };
    let (candidate, path) = {
        let state = inner.borrow();
        (state.candidates.get(state.candidate).cloned(), state.path.clone())
    };
    let work = gio::spawn_blocking(move || -> (Option<Vec<String>>, Result<Option<gst::Pipeline>, Failure>) {
        if let Err(error) = gst::init() {
            return (None, Err(Failure::Other(error.to_string())));
        }
        let resolved = source.map(|source| match source {
            Source::Candidates(list) => list,
            Source::Microphone => microphone_candidates(|name| gst::ElementFactory::find(name).is_some()),
        });
        let current = match (&resolved, candidate) {
            (Some(list), _) => list.first().cloned(),
            (None, current) => current,
        };
        let built = match current {
            Some(current) => build(&current, &path).map(Some),
            None => Ok(None),
        };
        (resolved, built)
    });
    let weak = Rc::downgrade(inner);
    glib::spawn_future_local(async move {
        let (resolved, built) = work
            .await
            .unwrap_or_else(|_| (None, Err(Failure::Other("The recording could not be started".into()))));
        let Some(inner) = weak.upgrade() else {
            if let Ok(Some(pipeline)) = built {
                let _ = pipeline.set_state(gst::State::Null);
            }
            return;
        };
        let (stale, cancelled, path) = {
            let state = inner.borrow();
            (state.done || state.attempt != attempt, state.cancelled, state.path.clone())
        };
        if stale {
            // Stopped or cancelled while the microphone was being opened: let go of it again.
            if let Ok(Some(pipeline)) = built {
                let _ = pipeline.set_state(gst::State::Null);
            }
            if cancelled {
                // Opening the pipeline made the file again.
                let _ = std::fs::remove_file(path);
            }
            return;
        }
        // Remember the list first, so a failure can move on to the next source.
        if let Some(list) = resolved {
            inner.borrow_mut().candidates = list;
        }
        match built {
            Ok(Some(pipeline)) => {
                if let Err(failure) = attach(&inner, pipeline) {
                    next_candidate(&inner, failure);
                }
            }
            Ok(None) => next_candidate(&inner, Failure::NoMicrophone),
            Err(failure) => next_candidate(&inner, failure),
        }
    });
}

/// Make the pipeline for a source and start it. Runs on a worker thread.
fn build(source: &str, path: &Path) -> Result<gst::Pipeline, Failure> {
    let pipeline = gst::parse::launch(&pipeline_text(source))
        .map_err(|error| classify(&error.to_string()))?
        .downcast::<gst::Pipeline>()
        .map_err(|_| Failure::Other("The recording pipeline was not a pipeline".into()))?;
    let sink = pipeline.by_name("out").ok_or_else(|| Failure::Other("The recording has no output".into()))?;
    sink.set_property("location", path.to_string_lossy().as_ref());
    pipeline.set_state(gst::State::Playing).map_err(|error| {
        let _ = pipeline.set_state(gst::State::Null);
        classify(&error.to_string())
    })?;
    Ok(pipeline)
}

/// Listen to a started pipeline. Runs on the main loop and is quick: messages that arrived
/// before this are waiting on the bus.
fn attach(inner: &Rc<RefCell<Inner>>, pipeline: gst::Pipeline) -> Result<(), Failure> {
    let bus = pipeline.bus().ok_or_else(|| Failure::Other("The recording has no bus".into()))?;
    let weak = Rc::downgrade(inner);
    let watch = bus
        .add_watch_local(move |_, message| {
            if let Some(inner) = weak.upgrade() {
                handle(&inner, message);
            }
            glib::ControlFlow::Continue
        })
        .map_err(|error| Failure::Other(error.to_string()))?;
    let mut state = inner.borrow_mut();
    state.pipeline = Some(pipeline);
    state.watch = Some(watch);
    state.got_level = false;
    Ok(())
}

/// Try the next source, or report that none worked.
fn next_candidate(inner: &Rc<RefCell<Inner>>, failure: Failure) {
    teardown(inner);
    let more = {
        let mut state = inner.borrow_mut();
        state.candidate += 1;
        state.candidate < state.candidates.len()
    };
    if more {
        // Not from inside the bus callback that reported the error.
        let weak = glib::thread_guard::ThreadGuard::new(Rc::downgrade(inner));
        let idle = glib::idle_source_new(None, glib::Priority::DEFAULT_IDLE, move || {
            if let Some(inner) = weak.get_ref().upgrade() {
                launch(&inner, None);
            }
            glib::ControlFlow::Break
        });
        idle.attach(Some(&glib::MainContext::ref_thread_default()));
        inner.borrow_mut().timer = Some(idle);
    } else {
        let path = {
            let mut state = inner.borrow_mut();
            state.done = true;
            state.path.clone()
        };
        // Nothing was recorded: the empty file made for it is not worth keeping.
        let _ = std::fs::remove_file(path);
        emit(inner, Event::Failed(failure));
    }
}

fn handle(inner: &Rc<RefCell<Inner>>, message: &gst::Message) {
    use gst::MessageView;
    match message.view() {
        MessageView::Element(element) => {
            let Some(structure) = element.structure().filter(|s| s.name() == "level") else { return };
            let at = structure
                .get::<u64>("endtime")
                .or_else(|_| structure.get::<u64>("timestamp"))
                .map(Duration::from_nanos)
                .unwrap_or_default();
            let peak_db = structure
                .get::<glib::ValueArray>("peak")
                .ok()
                .and_then(|peaks| peaks.first().and_then(|value| value.get::<f64>().ok()))
                .unwrap_or(f64::NEG_INFINITY);
            let over = {
                let mut state = inner.borrow_mut();
                if state.done {
                    return;
                }
                state.got_level = true;
                state.last_at = at;
                let over = at >= state.max_length && !state.limit_sent;
                state.limit_sent |= over;
                over
            };
            emit(inner, Event::Level { bar: bar_height(peak_db), at });
            if over {
                emit(inner, Event::Limit);
            }
        }
        MessageView::Error(error) => {
            let failure = classify(&error.error().to_string());
            let (started, more) = {
                let state = inner.borrow();
                (state.got_level, state.candidate + 1 < state.candidates.len())
            };
            if state_done(inner) {
                return;
            }
            if !started && more {
                next_candidate(inner, failure);
            } else if started {
                finish(inner, Some(failure));
            } else {
                next_candidate(inner, failure);
            }
        }
        MessageView::Eos(_) => finish(inner, None),
        _ => {}
    }
}

fn state_done(inner: &Rc<RefCell<Inner>>) -> bool {
    inner.borrow().done
}

impl Recorder {
    /// Start recording into a new file at `path` (it must not exist; it is made private to this
    /// user). Returns at once: the microphone is opened in the background, and `on_event` (called
    /// on the main loop) hears about it, whether that works (levels follow) or not (`Failed`).
    pub fn start(source: Source, path: &Path, settings: Settings, on_event: impl Fn(Event) + 'static) -> Result<Self, Failure> {
        if matches!(&source, Source::Candidates(list) if list.is_empty()) {
            return Err(Failure::NoMicrophone);
        }
        create_private_file(path)?;
        let inner = Rc::new(RefCell::new(Inner {
            pipeline: None,
            watch: None,
            path: path.to_owned(),
            candidates: Vec::new(),
            candidate: 0,
            attempt: 0,
            got_level: false,
            last_at: Duration::ZERO,
            stopping: false,
            done: false,
            cancelled: false,
            limit_sent: false,
            max_length: settings.max_length,
            timer: None,
            on_event: Rc::new(on_event),
        }));
        launch(&inner, Some(source));
        Ok(Self(inner))
    }

    /// Finish the recording properly: the file is closed and `Finished` follows. Stopping again,
    /// or after the end, does nothing.
    pub fn stop(&self) {
        let (nothing_yet, pipeline) = {
            let mut state = self.0.borrow_mut();
            if state.done || state.stopping {
                return;
            }
            state.stopping = true;
            (!state.got_level, state.pipeline.clone())
        };
        if nothing_yet {
            // Let go before any sound arrived: there is nothing to close properly.
            finish(&self.0, None);
            return;
        }
        if let Some(pipeline) = pipeline {
            pipeline.send_event(gst::event::Eos::new());
        }
        let weak = Rc::downgrade(&self.0);
        let timer = later(FINISH_WITHIN, move || {
            if let Some(inner) = weak.upgrade() {
                finish(&inner, None);
            }
        });
        self.0.borrow_mut().timer = Some(timer);
    }

    /// Throw the recording away: no more events, and the file is deleted.
    pub fn cancel(&self) {
        let path = {
            let mut state = self.0.borrow_mut();
            state.done = true;
            state.cancelled = true;
            state.path.clone()
        };
        teardown(&self.0);
        let _ = std::fs::remove_file(path);
    }

}

impl Drop for Recorder {
    fn drop(&mut self) {
        // Nobody is left to hear about it; the file is the owner's to keep or delete.
        self.0.borrow_mut().done = true;
        teardown(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{os::unix::fs::PermissionsExt, time::Instant};

    fn available() -> bool {
        gst::init().unwrap();
        ["audiotestsrc", "audioconvert", "audioresample", "level", "opusenc", "oggmux", "filesink", "identity"]
            .iter()
            .all(|name| gst::ElementFactory::find(name).is_some())
    }
    const TONE: &str = "audiotestsrc is-live=true wave=sine freq=440 volume=0.8";
    const SILENCE: &str = "audiotestsrc is-live=true wave=silence";

    /// A directory that is removed again, so nothing is left in the real cache.
    struct Scratch(PathBuf);
    impl Scratch {
        fn new() -> Self {
            let dir = std::env::temp_dir().join(format!("voice-test-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
        fn file(&self, name: &str) -> PathBuf {
            self.0.join(name)
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    type Log = Rc<RefCell<Vec<Event>>>;
    fn with_loop<T>(body: impl FnOnce(&glib::MainContext) -> T) -> T {
        let context = glib::MainContext::new();
        context.with_thread_default(|| body(&context)).unwrap()
    }
    fn record(source: &str, path: &Path, settings: Settings) -> (Result<Recorder, Failure>, Log) {
        let log: Log = Default::default();
        let sink = log.clone();
        let recorder = Recorder::start(Source::Candidates(vec![source.into()]), path, settings, move |event| sink.borrow_mut().push(event));
        (recorder, log)
    }
    fn run_until(context: &glib::MainContext, limit: Duration, done: impl Fn() -> bool) -> bool {
        let end = Instant::now() + limit;
        while Instant::now() < end {
            context.iteration(false);
            if done() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        done()
    }
    fn levels(log: &Log) -> Vec<(f32, Duration)> {
        log.borrow().iter().filter_map(|e| if let Event::Level { bar, at } = e { Some((*bar, *at)) } else { None }).collect()
    }
    fn finished(log: &Log) -> Option<Duration> {
        log.borrow().iter().find_map(|e| if let Event::Finished { length } = e { Some(*length) } else { None })
    }
    fn count(log: &Log, matches: impl Fn(&Event) -> bool) -> usize {
        log.borrow().iter().filter(|e| matches(e)).count()
    }

    // ---- pure ---------------------------------------------------------------------------

    #[test]
    fn errors_are_sorted_into_what_a_person_can_do_about_them() {
        for (text, want) in [
            ("Failed to connect: Permission denied", Failure::Denied),
            ("Operation not allowed", Failure::Denied),
            ("access denied by policy", Failure::Denied),
            ("Device 'default' is busy", Failure::Busy),
            ("The device is in use", Failure::Busy),
            ("no such file or directory", Failure::NoMicrophone),
            ("No target node found", Failure::NoMicrophone),
            ("Could not find device: does not exist", Failure::NoMicrophone),
            ("no element \"pulsesrc\"", Failure::NoMicrophone),
        ] {
            assert_eq!(classify(text), want, "{text}");
        }
        assert_eq!(classify("something odd\u{202e} happened"), Failure::Other("something odd happened".into()));
        let Failure::Other(long) = classify(&"x".repeat(1000)) else { panic!() };
        assert_eq!(long.chars().count(), 160);
        assert_eq!(classify("PERMISSION DENIED"), Failure::Denied, "case does not matter");
    }

    #[test]
    fn every_failure_has_a_plain_sentence() {
        for failure in [Failure::Denied, Failure::NoMicrophone, Failure::Busy, Failure::Other("because".into())] {
            let text = failure.to_string();
            assert!(!text.is_empty() && !text.ends_with('.'), "{text}");
        }
    }

    #[test]
    fn the_microphone_sources_are_tried_in_order_among_those_installed() {
        let all = microphone_candidates(|_| true);
        assert_eq!(all.len(), 3);
        assert!(all[0].starts_with("pipewiresrc") && all[1] == "pulsesrc" && all[2] == "autoaudiosrc");
        assert_eq!(microphone_candidates(|name| name == "pulsesrc"), ["pulsesrc"]);
        assert!(microphone_candidates(|_| false).is_empty());
    }

    #[test]
    fn the_pipeline_is_mono_48k_opus_in_ogg_with_levels() {
        let text = pipeline_text("audiotestsrc");
        for needed in ["channels=1", "rate=48000", "level", "post-messages=true", "opusenc", "oggmux", "filesink", "audio-type=voice"] {
            assert!(text.contains(needed), "{needed} in {text}");
        }
        assert!(!text.contains("/dev/"), "no device is named");
    }

    // ---- recording ----------------------------------------------------------------------

    #[test]
    fn a_recording_is_a_private_ogg_opus_file_the_app_accepts_with_about_twenty_levels_a_second() {
        if !available() {
            eprintln!("skipped: GStreamer plugins missing");
            return;
        }
        with_loop(|context| {
            let dir = Scratch::new();
            let path = dir.file("note.ogg");
            let (recorder, log) = record(TONE, &path, Settings::default());
            let recorder = recorder.unwrap();
            assert!(run_until(context, Duration::from_secs(8), || levels(&log).last().is_some_and(|(_, at)| *at >= Duration::from_millis(1500))));
            recorder.stop();
            assert!(run_until(context, Duration::from_secs(5), || finished(&log).is_some()), "finished after stop");
            let length = finished(&log).unwrap();
            assert!(length >= Duration::from_millis(1400) && length < Duration::from_secs(4), "{length:?}");
            // The file.
            assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600, "private");
            let bytes = std::fs::read(&path).unwrap();
            assert_eq!(&bytes[..4], b"OggS");
            assert!(bytes.windows(8).any(|w| w == b"OpusHead"), "Opus");
            assert!(bytes.len() > 2000 && bytes.len() < 60_000, "24 kbit/s for about 2 s: {} bytes", bytes.len());
            let head = &bytes[..bytes.len().min(64 * 1024)];
            assert_eq!(crate::audio_check::check("note.ogg", head, bytes.len() as u64), Ok(crate::audio_check::Container::Ogg));
            // The levels.
            let seen = levels(&log);
            let per_second = seen.len() as f64 / length.as_secs_f64();
            assert!((12.0..=28.0).contains(&per_second), "about 20 a second, got {per_second:.1}");
            assert!(seen.windows(2).all(|pair| pair[0].1 <= pair[1].1), "time only moves forward");
            assert!(seen.iter().all(|(bar, _)| (0.0..=1.0).contains(bar)));
            assert!(seen.iter().skip(2).all(|(bar, _)| *bar > 0.4), "a loud tone gives tall bars");
            assert_eq!(count(&log, |e| matches!(e, Event::Finished { .. })), 1, "said once");
        });
    }

    #[test]
    fn silence_gives_flat_bars_and_a_valid_file() {
        if !available() {
            return;
        }
        with_loop(|context| {
            let dir = Scratch::new();
            let path = dir.file("quiet.ogg");
            let (recorder, log) = record(SILENCE, &path, Settings::default());
            let recorder = recorder.unwrap();
            run_until(context, Duration::from_secs(8), || levels(&log).len() >= 15);
            recorder.stop();
            assert!(run_until(context, Duration::from_secs(5), || finished(&log).is_some()));
            assert!(levels(&log).iter().all(|(bar, _)| *bar == 0.0));
            assert_eq!(&std::fs::read(&path).unwrap()[..4], b"OggS");
        });
    }

    #[test]
    fn stopping_before_any_sound_finishes_at_once_with_nothing() {
        if !available() {
            return;
        }
        with_loop(|context| {
            let dir = Scratch::new();
            let (recorder, log) = record(TONE, &dir.file("early.ogg"), Settings::default());
            let recorder = recorder.unwrap();
            recorder.stop();
            recorder.stop();
            recorder.stop();
            run_until(context, Duration::from_millis(500), || false);
            assert_eq!(count(&log, |e| matches!(e, Event::Finished { .. })), 1, "once, however often it was stopped");
            assert_eq!(finished(&log), Some(Duration::ZERO));
        });
    }

    #[test]
    fn cancelling_deletes_the_file_and_nothing_more_is_heard() {
        if !available() {
            return;
        }
        with_loop(|context| {
            let dir = Scratch::new();
            let path = dir.file("gone.ogg");
            let (recorder, log) = record(TONE, &path, Settings::default());
            let recorder = recorder.unwrap();
            assert!(run_until(context, Duration::from_secs(8), || levels(&log).len() >= 4));
            recorder.cancel();
            let heard = log.borrow().len();
            assert!(!path.exists(), "the file is deleted");
            run_until(context, Duration::from_millis(600), || false);
            assert_eq!(log.borrow().len(), heard, "silent after cancel");
            // Cancelling or stopping afterwards is harmless.
            recorder.cancel();
            recorder.stop();
        });
    }

    #[test]
    fn the_length_limit_is_announced_once_and_recording_goes_on_until_stopped() {
        if !available() {
            return;
        }
        with_loop(|context| {
            let dir = Scratch::new();
            let (recorder, log) = record(TONE, &dir.file("long.ogg"), Settings { max_length: Duration::from_millis(600) });
            let recorder = recorder.unwrap();
            assert!(run_until(context, Duration::from_secs(8), || count(&log, |e| matches!(e, Event::Limit)) >= 1));
            run_until(context, Duration::from_millis(500), || false);
            assert_eq!(count(&log, |e| matches!(e, Event::Limit)), 1, "only once");
            assert!(levels(&log).last().unwrap().1 > Duration::from_millis(600), "it carried on");
            recorder.stop();
            assert!(run_until(context, Duration::from_secs(5), || finished(&log).is_some()));
        });
    }

    #[test]
    fn a_source_that_cannot_be_built_is_reported_once_and_leaves_no_file() {
        if !available() {
            return;
        }
        with_loop(|context| {
            let dir = Scratch::new();
            for (name, source) in [("a.ogg", "no-such-element-at-all"), ("b.ogg", ""), ("c.ogg", "videotestsrc ! ! !")] {
                let path = dir.file(name);
                let (recorder, log) = record(source, &path, Settings::default());
                let _recorder = recorder.expect("starting never blocks or fails for this: the answer comes as an event");
                assert!(run_until(context, Duration::from_secs(10), || count(&log, |e| matches!(e, Event::Failed(_))) >= 1), "{name}");
                run_until(context, Duration::from_millis(200), || false);
                assert_eq!(count(&log, |e| matches!(e, Event::Failed(_))), 1, "{name}: told once");
                assert!(!path.exists(), "{name}: the empty file was removed");
            }
            let none = Recorder::start(Source::Candidates(vec![]), &dir.file("d.ogg"), Settings::default(), |_| {});
            assert_eq!(none.err(), Some(Failure::NoMicrophone));
            assert!(!dir.file("d.ogg").exists());
        });
    }

    #[test]
    fn stopping_or_cancelling_before_the_microphone_has_opened_leaves_nothing_running() {
        if !available() {
            return;
        }
        with_loop(|context| {
            let dir = Scratch::new();
            for stop_instead_of_cancel in [false, true] {
                let path = dir.file(if stop_instead_of_cancel { "stopped.ogg" } else { "cancelled.ogg" });
                let (recorder, log) = record(TONE, &path, Settings::default());
                let recorder = recorder.unwrap();
                // Straight away: the background start has not finished.
                if stop_instead_of_cancel {
                    recorder.stop();
                } else {
                    recorder.cancel();
                }
                // Long enough for the late pipeline to appear, and be torn down.
                run_until(context, Duration::from_millis(1500), || false);
                assert!(levels(&log).is_empty(), "no sound was ever taken (stop: {stop_instead_of_cancel})");
                assert_eq!(count(&log, |e| matches!(e, Event::Failed(_) | Event::Interrupted { .. })), 0);
                if stop_instead_of_cancel {
                    assert_eq!(finished(&log), Some(Duration::ZERO));
                    assert_eq!(std::fs::metadata(&path).unwrap().len(), 0, "an empty file, nothing recorded");
                } else {
                    assert!(!path.exists());
                    assert!(log.borrow().is_empty());
                }
            }
        });
    }

    #[test]
    fn dropping_before_the_microphone_has_opened_is_safe() {
        if !available() {
            return;
        }
        with_loop(|context| {
            let dir = Scratch::new();
            let (recorder, log) = record(TONE, &dir.file("dropped-early.ogg"), Settings::default());
            drop(recorder.unwrap());
            run_until(context, Duration::from_millis(1200), || false);
            assert!(log.borrow().is_empty());
        });
    }

    /// The reason for starting in the background. In a process where GStreamer has not been used
    /// yet, loading its plugins takes a second or more; starting must not wait for that.
    #[test]
    #[ignore = "needs GStreamer not yet started in this process: run it alone (--exact)"]
    fn starting_returns_at_once_even_when_gstreamer_is_cold() {
        with_loop(|context| {
            let dir = Scratch::new();
            let started = Instant::now();
            let (recorder, log) = record(TONE, &dir.file("cold.ogg"), Settings::default());
            let took = started.elapsed();
            let recorder = recorder.unwrap();
            assert!(took < Duration::from_millis(50), "start() took {took:?}");
            assert!(run_until(context, Duration::from_secs(20), || !levels(&log).is_empty()), "and it did start");
            recorder.cancel();
        });
    }

    #[test]
    fn a_file_that_cannot_be_created_is_reported_not_overwritten() {
        if !available() {
            return;
        }
        with_loop(|_| {
            let dir = Scratch::new();
            let path = dir.file("exists.ogg");
            std::fs::write(&path, b"precious").unwrap();
            let (recorder, _log) = record(TONE, &path, Settings::default());
            assert!(matches!(recorder.err(), Some(Failure::Other(_))), "an existing file is never overwritten");
            assert_eq!(std::fs::read(&path).unwrap(), b"precious");
            let missing = dir.file("no-such-folder/x.ogg");
            let (recorder, _log) = record(TONE, &missing, Settings::default());
            assert!(matches!(recorder.err(), Some(Failure::Other(_))));
        });
    }

    #[test]
    fn if_the_first_source_fails_the_next_is_used() {
        if !available() {
            return;
        }
        with_loop(|context| {
            let dir = Scratch::new();
            let path = dir.file("fallback.ogg");
            let log: Log = Default::default();
            let sink = log.clone();
            // The first cannot be built; the second builds but fails before any sound; the third works.
            let recorder = Recorder::start(
                Source::Candidates(vec![
                    "no-such-element-at-all".into(),
                    "audiotestsrc is-live=true ! identity error-after=1".into(),
                    TONE.into(),
                ]),
                &path,
                Settings::default(),
                move |event| sink.borrow_mut().push(event),
            )
            .unwrap();
            assert!(run_until(context, Duration::from_secs(10), || levels(&log).len() >= 3), "{:?}", log.borrow());
            assert_eq!(count(&log, |e| matches!(e, Event::Failed(_))), 0, "the failures on the way were not reported");
            recorder.stop();
            assert!(run_until(context, Duration::from_secs(5), || finished(&log).is_some()));
        });
    }

    #[test]
    fn when_every_source_fails_the_person_hears_one_failure() {
        if !available() {
            return;
        }
        with_loop(|context| {
            let dir = Scratch::new();
            let log: Log = Default::default();
            let sink = log.clone();
            let _recorder = Recorder::start(
                Source::Candidates(vec!["audiotestsrc ! identity error-after=1".into(), "audiotestsrc ! identity error-after=1".into()]),
                &dir.file("fails.ogg"),
                Settings::default(),
                move |event| sink.borrow_mut().push(event),
            )
            .unwrap();
            assert!(run_until(context, Duration::from_secs(10), || count(&log, |e| matches!(e, Event::Failed(_))) >= 1));
            run_until(context, Duration::from_millis(300), || false);
            assert_eq!(count(&log, |e| matches!(e, Event::Failed(_))), 1);
        });
    }

    #[test]
    fn a_microphone_that_stops_part_way_keeps_what_was_recorded() {
        if !available() {
            return;
        }
        with_loop(|context| {
            let dir = Scratch::new();
            let path = dir.file("cut.ogg");
            // The "microphone" dies after a while, as an unplugged one would.
            let (recorder, log) = record("audiotestsrc is-live=true volume=0.8 ! identity error-after=60", &path, Settings::default());
            let _recorder = recorder.unwrap();
            assert!(run_until(context, Duration::from_secs(10), || count(&log, |e| matches!(e, Event::Interrupted { .. })) >= 1), "{:?}", log.borrow().len());
            let length = log.borrow().iter().find_map(|e| if let Event::Interrupted { length, .. } = e { Some(*length) } else { None }).unwrap();
            assert!(length > Duration::from_millis(300), "{length:?}");
            assert_eq!(count(&log, |e| matches!(e, Event::Finished { .. } | Event::Failed(_))), 0, "told once, as an interruption");
            let bytes = std::fs::read(&path).unwrap();
            assert_eq!(&bytes[..4], b"OggS", "what came before is a usable file");
        });
    }

    #[test]
    fn a_source_that_ends_by_itself_finishes_the_file_properly() {
        if !available() {
            return;
        }
        with_loop(|context| {
            let dir = Scratch::new();
            let path = dir.file("ended.ogg");
            let (recorder, log) = record("audiotestsrc num-buffers=40 volume=0.8", &path, Settings::default());
            let _recorder = recorder.unwrap();
            assert!(run_until(context, Duration::from_secs(10), || finished(&log).is_some()));
            let bytes = std::fs::read(&path).unwrap();
            assert_eq!(&bytes[..4], b"OggS");
            assert!(crate::audio_check::check("ended.ogg", &bytes, bytes.len() as u64).is_ok());
        });
    }

    #[test]
    fn dropping_the_recorder_stops_the_microphone_without_a_word() {
        if !available() {
            return;
        }
        with_loop(|context| {
            let dir = Scratch::new();
            let (recorder, log) = record(TONE, &dir.file("dropped.ogg"), Settings::default());
            let recorder = recorder.unwrap();
            assert!(run_until(context, Duration::from_secs(8), || levels(&log).len() >= 3));
            drop(recorder);
            let heard = log.borrow().len();
            run_until(context, Duration::from_millis(500), || false);
            assert_eq!(log.borrow().len(), heard);
        });
    }

    #[test]
    fn the_main_loop_never_stalls_while_recording() {
        if !available() {
            return;
        }
        with_loop(|context| {
            let dir = Scratch::new();
            let (recorder, log) = record(TONE, &dir.file("smooth.ogg"), Settings::default());
            let recorder = recorder.unwrap();
            let started = Instant::now();
            let (mut longest, mut last) = (Duration::ZERO, Instant::now());
            while started.elapsed() < Duration::from_secs(4) {
                context.iteration(false);
                let now = Instant::now();
                if started.elapsed() > Duration::from_secs(1) {
                    longest = longest.max(now - last);
                }
                last = now;
                std::thread::sleep(Duration::from_millis(1));
            }
            recorder.cancel();
            assert!(levels(&log).len() > 40, "it really was recording");
            assert!(longest < Duration::from_millis(30), "the longest pause between loop turns was {longest:?}");
        });
    }
}
