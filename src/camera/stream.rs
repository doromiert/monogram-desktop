//! A live camera stream: PipeWire in, bounded RGBA pictures out.
//!
//! `pipewiresrc` speaks the PipeWire protocol on the connection the portal handed over (after the
//! person said yes), so it can only see the cameras that connection allows. The pictures go
//! through `videoconvert ! videoscale ! videorate` into an `appsink` that holds RGBA at most
//! 1920x1080 and 30 frames a second, whatever the camera offers.
//!
//! Pictures reach the interface through a one-slot channel: if the interface is slow, the camera's
//! thread drops a picture instead of queueing it, so memory stays flat. Receiving needs no Tokio
//! runtime; it can be awaited on the GTK main loop.
use super::frame::{Frame, MAX_HEIGHT, MAX_WIDTH};
use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use std::os::fd::{AsRawFd, OwnedFd};
use tokio::sync::mpsc;

/// What a stream tells the interface.
#[derive(Debug)]
pub enum Event {
    Frame(Frame),
    /// The camera stopped working; the text is GStreamer's.
    Failed(String),
    /// The source ended by itself (a test source, or the camera was unplugged cleanly).
    Ended,
}

/// Where the pictures come from.
pub enum Source {
    /// The portal's PipeWire connection. The stream keeps the descriptor until it stops.
    PipeWire(OwnedFd),
    /// A `gst-launch` description of a raw video source. Tests use this with `videotestsrc`; no
    /// real camera is ever opened that way.
    Description(String),
}

pub struct Stream {
    pipeline: gst::Pipeline,
    _fd: Option<OwnedFd>,
    _watch: gst::bus::BusWatchGuard,
}

fn limits() -> String {
    format!("video/x-raw,format=RGBA,width=[1,{MAX_WIDTH}],height=[1,{MAX_HEIGHT}]")
}

/// The pipeline text for a source. The descriptor is a number, so nothing from outside is spliced
/// into it.
fn description(source: &Source) -> String {
    let head = match source {
        Source::PipeWire(fd) => format!("pipewiresrc fd={} do-timestamp=true", fd.as_raw_fd()),
        Source::Description(text) => text.clone(),
    };
    format!(
        "{head} ! videoconvert ! videoscale ! videorate drop-only=true max-rate=30 ! {} ! \
         appsink name=sink sync=false max-buffers=1 drop=true",
        limits()
    )
}

fn frame_of(sample: &gst::Sample) -> Option<Frame> {
    let caps = sample.caps()?;
    let structure = caps.structure(0)?;
    let width = u32::try_from(structure.get::<i32>("width").ok()?).ok()?;
    let height = u32::try_from(structure.get::<i32>("height").ok()?).ok()?;
    let map = sample.buffer()?.map_readable().ok()?;
    Frame::new(width, height, map.as_slice().to_vec()).ok()
}

/// The message when the GStreamer plugin that speaks PipeWire is not installed.
pub const NO_PLUGIN: &str = "Camera support is not installed: the GStreamer PipeWire plugin is missing";

/// Whether the elements a source needs exist; `has` says whether a named element is installed.
fn require(source: &Source, has: impl Fn(&str) -> bool) -> Result<(), String> {
    match source {
        Source::PipeWire(_) if !has("pipewiresrc") => Err(NO_PLUGIN.into()),
        _ => Ok(()),
    }
}

impl Stream {
    /// Start streaming. Events arrive on the returned receiver; events are posted by the thread
    /// default main context of the caller (it must be iterated for errors to be seen).
    pub fn start(source: Source) -> Result<(Self, mpsc::Receiver<Event>), String> {
        gst::init().map_err(|error| error.to_string())?;
        require(&source, |name| gst::ElementFactory::find(name).is_some())?;
        let text = description(&source);
        let pipeline = gst::parse::launch(&text)
            .map_err(|error| format!("The camera could not be set up: {error}"))?
            .downcast::<gst::Pipeline>()
            .map_err(|_| "The camera pipeline was not a pipeline".to_string())?;
        let sink = pipeline
            .by_name("sink")
            .and_then(|element| element.downcast::<gst_app::AppSink>().ok())
            .ok_or("The camera pipeline has no output")?;
        let (events, received) = mpsc::channel(1);
        let frames = events.clone();
        sink.set_callbacks(
            gst_app::AppSinkCallbacks::builder()
                .new_sample(move |sink| {
                    let sample = sink.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                    if let Some(frame) = frame_of(&sample) {
                        // A full slot means the interface is behind: this picture is dropped.
                        let _ = frames.try_send(Event::Frame(frame));
                    }
                    Ok(gst::FlowSuccess::Ok)
                })
                .build(),
        );
        let bus = pipeline.bus().ok_or("The camera pipeline has no bus")?;
        let watch = bus
            .add_watch_local(move |_, message| {
                use gst::MessageView;
                match message.view() {
                    MessageView::Error(error) => {
                        let _ = events.try_send(Event::Failed(error.error().to_string()));
                    }
                    MessageView::Eos(_) => {
                        let _ = events.try_send(Event::Ended);
                    }
                    _ => {}
                }
                gtk::glib::ControlFlow::Continue
            })
            .map_err(|error| error.to_string())?;
        pipeline
            .set_state(gst::State::Playing)
            .map_err(|error| format!("The camera could not be started: {error}"))?;
        let fd = match source {
            Source::PipeWire(fd) => Some(fd),
            Source::Description(_) => None,
        };
        Ok((Self { pipeline, _fd: fd, _watch: watch }, received))
    }

    /// Stop the camera now. Dropping the stream does the same, so only tests need to say it.
    #[cfg(test)]
    pub fn stop(&self) {
        let _ = self.pipeline.set_state(gst::State::Null);
    }
}

impl Drop for Stream {
    fn drop(&mut self) {
        // Null releases the PipeWire connection; only then is the descriptor closed.
        let _ = self.pipeline.set_state(gst::State::Null);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gtk::glib;
    use std::time::{Duration, Instant};

    fn available() -> bool {
        gst::init().unwrap();
        ["videotestsrc", "videoconvert", "videoscale", "videorate", "appsink"]
            .iter()
            .all(|name| gst::ElementFactory::find(name).is_some())
    }

    /// Run `body` with a main context of its own so tests do not share the default one.
    fn with_loop<T>(body: impl FnOnce(&glib::MainContext) -> T) -> T {
        let context = glib::MainContext::new();
        context.with_thread_default(|| body(&context)).unwrap()
    }

    /// Collect events until `done` says stop or the time runs out.
    fn collect(context: &glib::MainContext, receiver: &mut mpsc::Receiver<Event>, limit: Duration, done: impl Fn(&[Event]) -> bool) -> Vec<Event> {
        let mut seen = Vec::new();
        let end = Instant::now() + limit;
        while Instant::now() < end && !done(&seen) {
            context.iteration(false);
            while let Ok(event) = receiver.try_recv() {
                seen.push(event);
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        seen
    }
    fn open_fds() -> usize {
        std::fs::read_dir("/proc/self/fd").map(|dir| dir.count()).unwrap_or(0)
    }
    fn frames(events: &[Event]) -> Vec<&Frame> {
        events.iter().filter_map(|event| if let Event::Frame(frame) = event { Some(frame) } else { None }).collect()
    }

    #[test]
    fn a_missing_pipewire_plugin_is_explained_not_a_raw_gstreamer_error() {
        let (reader, _writer) = std::os::unix::net::UnixStream::pair().unwrap();
        let source = Source::PipeWire(OwnedFd::from(reader));
        assert_eq!(require(&source, |_| false), Err(NO_PLUGIN.to_string()));
        assert_eq!(require(&source, |name| name == "pipewiresrc"), Ok(()));
        // A test description never needs the plugin.
        assert_eq!(require(&Source::Description("videotestsrc".into()), |_| false), Ok(()));
        assert!(!NO_PLUGIN.contains("pipewiresrc"), "no jargon element names in what people read");
    }

    #[test]
    fn the_pipeline_text_is_bounded_and_only_a_number_reaches_it() {
        let (reader, _writer) = std::os::unix::net::UnixStream::pair().unwrap();
        let fd = OwnedFd::from(reader);
        let number = fd.as_raw_fd();
        let text = description(&Source::PipeWire(fd));
        assert!(text.starts_with(&format!("pipewiresrc fd={number} do-timestamp=true ! ")), "{text}");
        for needed in ["videoconvert", "videoscale", "max-rate=30", "format=RGBA", "width=[1,1920]", "height=[1,1080]", "max-buffers=1", "drop=true"] {
            assert!(text.contains(needed), "{needed} in {text}");
        }
        assert!(!text.contains("/dev/video"), "no device is named");
    }

    #[test]
    fn a_test_source_delivers_rgba_frames_of_the_requested_size_then_ends() {
        if !available() {
            eprintln!("skipped: GStreamer base plugins are not installed");
            return;
        }
        with_loop(|context| {
            let (stream, mut events) = Stream::start(Source::Description(
                "videotestsrc num-buffers=10 ! video/x-raw,width=320,height=240,framerate=30/1".into(),
            ))
            .unwrap();
            let seen = collect(context, &mut events, Duration::from_secs(5), |seen| seen.iter().any(|e| matches!(e, Event::Ended)));
            let pictures = frames(&seen);
            assert!(!pictures.is_empty(), "frames arrived: {seen:?}");
            for frame in &pictures {
                assert_eq!((frame.width(), frame.height()), (320, 240));
                assert_eq!(frame.data().len(), 320 * 240 * 4);
            }
            assert!(seen.iter().any(|e| matches!(e, Event::Ended)), "the source ended");
            stream.stop();
        });
    }

    #[test]
    fn a_picture_bigger_than_the_limit_is_scaled_down_keeping_its_shape() {
        if !available() {
            return;
        }
        with_loop(|context| {
            let (_stream, mut events) = Stream::start(Source::Description(
                "videotestsrc num-buffers=4 ! video/x-raw,width=3840,height=2160,framerate=30/1".into(),
            ))
            .unwrap();
            let seen = collect(context, &mut events, Duration::from_secs(8), |seen| !frames(seen).is_empty());
            let frame = frames(&seen).into_iter().next().expect("a frame").clone();
            assert!(frame.width() <= MAX_WIDTH && frame.height() <= MAX_HEIGHT);
            // 16:9 stays 16:9.
            assert_eq!(u64::from(frame.width()) * 9, u64::from(frame.height()) * 16, "{}x{}", frame.width(), frame.height());
        });
    }

    #[test]
    fn a_small_odd_shaped_picture_passes_through_unchanged() {
        if !available() {
            return;
        }
        with_loop(|context| {
            let (_stream, mut events) = Stream::start(Source::Description(
                "videotestsrc num-buffers=3 ! video/x-raw,width=17,height=9,framerate=30/1".into(),
            ))
            .unwrap();
            let seen = collect(context, &mut events, Duration::from_secs(5), |seen| !frames(seen).is_empty());
            let frame = frames(&seen).into_iter().next().expect("a frame").clone();
            assert_eq!((frame.width(), frame.height()), (17, 9));
        });
    }

    #[test]
    fn the_frame_rate_is_capped_at_thirty() {
        if !available() {
            return;
        }
        with_loop(|context| {
            let (stream, mut events) = Stream::start(Source::Description(
                "videotestsrc is-live=true ! video/x-raw,width=64,height=48,framerate=120/1".into(),
            ))
            .unwrap();
            let started = Instant::now();
            let mut count = 0;
            while started.elapsed() < Duration::from_millis(1500) {
                context.iteration(false);
                while let Ok(event) = events.try_recv() {
                    count += usize::from(matches!(event, Event::Frame(_)));
                }
                std::thread::sleep(Duration::from_millis(1));
            }
            stream.stop();
            assert!(count > 0, "some frames");
            assert!(count <= 50, "at most about 30 a second, got {count} in 1.5 s");
        });
    }

    #[test]
    fn a_bad_source_is_an_error_not_a_crash() {
        if !available() {
            return;
        }
        with_loop(|_| {
            assert!(Stream::start(Source::Description("no-such-element-at-all".into())).is_err());
            assert!(Stream::start(Source::Description("videotestsrc ! ! !".into())).is_err());
            assert!(Stream::start(Source::Description(String::new())).is_err());
        });
    }

    #[test]
    fn a_descriptor_that_is_not_a_pipewire_connection_never_produces_pictures() {
        if gst::init().is_err() || gst::ElementFactory::find("pipewiresrc").is_none() {
            eprintln!("skipped: the PipeWire GStreamer plugin is not installed");
            return;
        }
        with_loop(|context| {
            // /dev/null is not a PipeWire socket. This also proves no camera is opened: the
            // descriptor is all the source has.
            let null = std::fs::File::open("/dev/null").unwrap();
            let started = Stream::start(Source::PipeWire(OwnedFd::from(null)));
            if let Ok((_stream, mut events)) = started {
                let seen = collect(context, &mut events, Duration::from_secs(4), |seen| {
                    seen.iter().any(|e| matches!(e, Event::Failed(_) | Event::Ended))
                });
                assert!(frames(&seen).is_empty(), "no picture from a non-connection");
                assert!(seen.iter().any(|e| matches!(e, Event::Failed(_) | Event::Ended)), "it reports failure: {seen:?}");
            }
        });
    }

    #[test]
    #[ignore = "counts the whole process's file descriptors: run it alone (--exact)"]
    fn stopping_ends_the_events_and_releases_descriptors_however_often_it_is_done() {
        if !available() {
            return;
        }
        with_loop(|context| {
            let before = open_fds();
            for _ in 0..5 {
                let (stream, mut events) = Stream::start(Source::Description(
                    "videotestsrc is-live=true ! video/x-raw,width=64,height=48,framerate=30/1".into(),
                ))
                .unwrap();
                collect(context, &mut events, Duration::from_secs(3), |seen| !frames(seen).is_empty());
                stream.stop();
                stream.stop();
                // Drain what was already queued, then nothing more arrives.
                while events.try_recv().is_ok() {}
                std::thread::sleep(Duration::from_millis(80));
                context.iteration(false);
                assert!(events.try_recv().is_err(), "silent after stop");
                drop(stream);
            }
            let after = open_fds();
            assert!(after <= before + 2, "descriptors leaked: {before} -> {after}");
        });
    }

    #[test]
    fn the_portal_descriptor_is_closed_when_the_stream_is() {
        if !available() {
            return;
        }
        let (kept, peer) = std::os::unix::net::UnixStream::pair().unwrap();
        drop(peer);
        let raw = kept.as_raw_fd();
        let owned = OwnedFd::from(kept);
        let probe = || unsafe { libc::fcntl(raw, libc::F_GETFD) } != -1;
        assert!(probe());
        // A source built from a description never owns the descriptor, so use the real kind:
        // the stream takes it, and closing the stream closes it.
        if gst::ElementFactory::find("pipewiresrc").is_some() {
            with_loop(|_| {
                if let Ok((stream, _events)) = Stream::start(Source::PipeWire(owned)) {
                    assert!(probe(), "held while the stream lives");
                    drop(stream);
                    assert!(!probe(), "closed with the stream");
                }
            });
        }
    }

    #[test]
    fn a_slow_reader_costs_no_memory_because_pictures_are_dropped_not_queued() {
        if !available() {
            return;
        }
        with_loop(|context| {
            let (stream, mut events) = Stream::start(Source::Description(
                "videotestsrc is-live=true ! video/x-raw,width=640,height=480,framerate=30/1".into(),
            ))
            .unwrap();
            // Do not read for a while; the channel holds at most one picture.
            let end = Instant::now() + Duration::from_millis(800);
            while Instant::now() < end {
                context.iteration(false);
                std::thread::sleep(Duration::from_millis(5));
            }
            let mut queued = 0;
            while events.try_recv().is_ok() {
                queued += 1;
            }
            stream.stop();
            assert!(queued <= 2, "at most one waiting picture (plus one event), got {queued}");
        });
    }
}
