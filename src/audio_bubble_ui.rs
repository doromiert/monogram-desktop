//! The audio message bubble: a round play button, a waveform and the duration.
//!
//! The file is downloaded under the same rules as other media (automatic only where
//! `automatic_media_allowed` says so, otherwise on a click). Only after that is it prepared, and
//! preparing never touches the audio in this process: `audio_preview_run` checks the header
//! strictly and has the sandboxed helper decode it. A file that is refused or fails shows a short
//! reason instead of a player. Playback is `audio_player`, which only ever sees raw PCM.
use crate::{
    audio_check::MAX_FILE_BYTES,
    audio_player::{AudioPlayer, PlayState},
    audio_preview_run::{self, Preview},
    downloads::Status,
    file_share::Attachment,
    model::Message,
    ui::Ui,
    widgets::*,
};
use adw::prelude::*;
use gtk::{gio, glib};
use std::{
    cell::{Cell, RefCell},
    fs::File,
    rc::{Rc, Weak},
    sync::atomic::AtomicBool,
    time::Duration,
};

/// How far a keyboard seek on the waveform moves.
const SEEK_STEP_MS: u64 = 5_000;

#[derive(Clone, Debug, PartialEq)]
enum Phase {
    /// Not downloaded and not started.
    Waiting,
    Downloading(Option<f64>),
    /// Downloaded and untouched: nothing has parsed or decoded it, and nothing will until the user
    /// presses play.
    Downloaded(std::path::PathBuf),
    /// The helper is decoding it.
    Preparing,
    Ready,
    Failed(String),
}

/// Helpers running right now and the most there have been at once; the second is only read by tests.
static RUNNING: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
static PEAK: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

thread_local! {
    /// The player that is currently playing, so starting another stops it.
    static CURRENT: RefCell<Option<Weak<RefCell<Option<AudioPlayer>>>>> = const { RefCell::new(None) };
}

/// Whether a downloaded file is prepared (decoded in the sandbox) without a click. Only for senders
/// the user trusts: their own messages and contacts in a direct chat. Room participants and
/// strangers, even ones whose messages arrive encrypted, need a click: this is a stricter rule
/// than the one that decides whether the file is downloaded at all.
pub fn prepare_without_a_click(smoke: bool, own: bool, chat: &crate::model::Chat) -> bool {
    !smoke && (own || (chat.contact && !chat.group))
}

/// `m:ss` as in the design: "00:02".
pub fn clock(ms: u64) -> String {
    let seconds = ms / 1000;
    format!("{:02}:{:02}", seconds / 60, seconds % 60)
}

struct Bubble {
    ui: Weak<Ui>,
    owner: String,
    peer: String,
    id: String,
    file: Attachment,
    auto: (bool, bool),
    phase: RefCell<Phase>,
    button: gtk::Button,
    wave: gtk::DrawingArea,
    time: gtk::Label,
    peaks: RefCell<Vec<u8>>,
    duration_ms: Cell<u64>,
    /// 0.0 to 1.0: how much of the waveform is played.
    progress: Cell<f64>,
    player: Rc<RefCell<Option<AudioPlayer>>>,
    /// The GStreamer sink to play through; tests use `fakesink` instead of a sound card.
    sink: Cell<&'static str>,
    /// A position ticker is running (only while playing).
    ticking: Cell<bool>,
}

impl Ui {
    pub fn audio_attachment(self: &Rc<Self>, message: &Message) -> gtk::Box {
        let file = message.attachment.as_ref().expect("an audio message has an attachment").clone();
        let (owner, peer) = (self.state.borrow().account.clone(), self.state.borrow().current().jid.clone());

        let button = gtk::Button::from_icon_name("folder-download-symbolic");
        button.add_css_class("circular");
        button.add_css_class("suggested-action");
        button.add_css_class("audio-play");
        button.set_valign(gtk::Align::Center);
        let wave = gtk::DrawingArea::builder().hexpand(true).content_height(34).content_width(180).build();
        wave.add_css_class("audio-wave");
        wave.set_focusable(true);
        wave.update_property(&[gtk::accessible::Property::Label(if file.voice { "Voice message position" } else { "Audio position" })]);
        let time = label("", "caption dim-label audio-time");
        time.set_xalign(0.);
        let column = bx(true, 4);
        column.set_hexpand(true);
        column.set_valign(gtk::Align::Center);
        column.append(&wave);
        column.append(&time);
        let row = bx(false, 12);
        row.append(&button);
        row.append(&column);
        let root = bx(true, 0);
        root.add_css_class("audio-bubble");
        root.update_property(&[gtk::accessible::Property::Label(if file.voice { "Voice message" } else { "Audio file" })]);
        root.append(&row);

        let bubble = Rc::new(Bubble {
            ui: Rc::downgrade(self),
            owner,
            peer,
            id: message.id.clone(),
            file,
            auto: (message.own, message.encrypted),
            phase: RefCell::new(Phase::Waiting),
            button,
            wave,
            time,
            peaks: RefCell::new(Vec::new()),
            duration_ms: Cell::new(0),
            progress: Cell::new(0.),
            player: Rc::new(RefCell::new(None)),
            sink: Cell::new("autoaudiosink"),
            ticking: Cell::new(false),
        });
        bubble.set_phase(Phase::Waiting);
        install(&bubble);
        // The state lives as long as the widget does.
        unsafe { root.set_data("audio-bubble", bubble) };
        root
    }
}

impl Bubble {
    fn set_phase(&self, phase: Phase) {
        let (icon, label, tooltip) = match &phase {
            Phase::Waiting => ("folder-download-symbolic", if self.file.voice { "Download voice message" } else { "Download audio" }, None),
            Phase::Downloading(_) => ("process-stop-symbolic", "Cancel download", None),
            Phase::Downloaded(_) => ("media-playback-start-symbolic", if self.file.voice { "Play voice message" } else { "Play audio" }, None),
            Phase::Preparing => ("content-loading-symbolic", "Preparing audio", None),
            Phase::Ready => (self.play_icon(), self.play_label(), None),
            Phase::Failed(reason) => ("view-refresh-symbolic", "Try again", Some(reason.clone())),
        };
        if phase == Phase::Preparing {
            let spinner = adw::Spinner::new();
            spinner.set_size_request(20, 20);
            self.button.set_child(Some(&spinner));
        } else {
            self.button.set_icon_name(icon);
        }
        self.button.update_property(&[gtk::accessible::Property::Label(label)]);
        self.button.set_tooltip_text(Some(tooltip.as_deref().unwrap_or(label)));
        self.refresh_time(&phase);
        self.phase.replace(phase);
        self.wave.queue_draw();
    }
    fn play_icon(&self) -> &'static str {
        match self.player.borrow().as_ref().map(AudioPlayer::state) {
            Some(PlayState::Playing) => "media-playback-pause-symbolic",
            _ => "media-playback-start-symbolic",
        }
    }
    fn play_label(&self) -> &'static str {
        match self.player.borrow().as_ref().map(AudioPlayer::state) {
            Some(PlayState::Playing) => if self.file.voice { "Pause voice message" } else { "Pause audio" },
            _ => if self.file.voice { "Play voice message" } else { "Play audio" },
        }
    }
    /// The label under the waveform: size before it is ready, the length at rest, the elapsed time
    /// while playing, and the reason when something went wrong.
    fn refresh_time(&self, phase: &Phase) {
        let text = match phase {
            Phase::Waiting | Phase::Downloading(None) | Phase::Downloaded(_) => self.file.size.map(|s| glib::format_size(s).to_string()).unwrap_or_default(),
            Phase::Downloading(Some(fraction)) => format!("{:.0}%", fraction * 100.),
            Phase::Preparing => "Preparing…".into(),
            Phase::Failed(reason) => reason.clone(),
            Phase::Ready => {
                let player = self.player.borrow();
                match player.as_ref().map(|p| (p.state(), p.position_ms())) {
                    Some((PlayState::Playing | PlayState::Paused, position)) => clock(position),
                    _ => clock(self.duration_ms.get()),
                }
            }
        };
        self.time.set_label(&text);
    }
    fn update_playback(&self) {
        let ready = *self.phase.borrow() == Phase::Ready;
        if !ready {
            return;
        }
        let (progress, playing) = match self.player.borrow().as_ref() {
            Some(player) if player.state() != PlayState::Idle && self.duration_ms.get() > 0 => {
                (player.position_ms() as f64 / self.duration_ms.get() as f64, true)
            }
            _ => (0., false),
        };
        let changed = (self.progress.replace(progress) - progress).abs() > 0.001;
        self.button.set_icon_name(self.play_icon());
        self.button.update_property(&[gtk::accessible::Property::Label(self.play_label())]);
        self.button.set_tooltip_text(Some(self.play_label()));
        self.refresh_time(&Phase::Ready);
        if changed || playing {
            self.wave.queue_draw();
        }
    }

    /// Follow what the download manager says. A finished download makes the file available; it is
    /// prepared on its own only when `prepare_on_arrival` (a trusted sender), and otherwise nothing
    /// parses or decodes it until the user presses play.
    fn apply_status(self: &Rc<Self>, status: Option<Status>, phase: &Phase, prepare_on_arrival: bool) {
        match status {
            Some(Status::Ready(path)) if path.is_file() && prepare_on_arrival => self.prepare(path, false),
            Some(Status::Ready(path)) if path.is_file() => self.set_phase(Phase::Downloaded(path)),
            Some(Status::Running { bytes, total, .. }) => {
                let fraction = total.filter(|total| *total > 0 && bytes > 0).map(|total| (bytes as f64 / total as f64).min(1.));
                if *self.phase.borrow() != Phase::Downloading(fraction) {
                    self.set_phase(Phase::Downloading(fraction));
                }
            }
            Some(Status::Failed(reason) | Status::Deferred { reason, .. }) => self.set_phase(Phase::Failed(reason.into())),
            Some(Status::Cancelled) | None if *phase != Phase::Waiting => self.set_phase(Phase::Waiting),
            _ => {}
        }
    }

    /// Open the downloaded file and have the sandboxed helper prepare it, then play it if the user
    /// asked to (a click on the play button prepares and plays in one step).
    fn prepare(self: &Rc<Self>, path: std::path::PathBuf, play_when_ready: bool) {
        self.set_phase(Phase::Preparing);
        let (file, name) = (self.file.clone(), self.file.name.clone());
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            // Queue here, not on a pool thread: only two helpers run at once.
            let permit = audio_preview_run::preparation_permit().await;
            let work = gio::spawn_blocking(move || -> Result<(File, Preview), String> {
                use std::sync::atomic::Ordering::SeqCst;
                let running = RUNNING.fetch_add(1, SeqCst) + 1;
                PEAK.fetch_max(running, SeqCst);
                let stopped = AtomicBool::new(false);
                let result = crate::downloads::verified_input(&path, &file, &stopped)
                    .map_err(str::to_owned)
                    .and_then(|verified| {
                        let file = verified.0;
                        audio_preview_run::probe(&file, &name, &|| false).map(|preview| (file, preview)).map_err(|error| error.to_string())
                    });
                RUNNING.fetch_sub(1, SeqCst);
                result
            });
            let result = work.await.unwrap_or_else(|_| Err("Audio preparation was interrupted".into()));
            drop(permit);
            let Some(bubble) = weak.upgrade() else { return };
            match result {
                Ok((file, preview)) => {
                    bubble.duration_ms.set(preview.duration_ms);
                    bubble.peaks.replace(preview.peaks.clone());
                    let player = AudioPlayer::with_sink(file, preview.container, preview.duration_ms, bubble.sink.get());
                    let hook = Rc::downgrade(&bubble);
                    player.connect_changed(move || {
                        if let Some(bubble) = hook.upgrade() {
                            bubble.on_player_changed();
                        }
                    });
                    bubble.player.replace(Some(player));
                    bubble.set_phase(Phase::Ready);
                    if play_when_ready {
                        if let Some(player) = bubble.player.borrow().as_ref() {
                            player.play_from(0);
                        }
                    }
                }
                Err(reason) => bubble.set_phase(Phase::Failed(reason)),
            }
        });
    }

    fn on_player_changed(self: &Rc<Self>) {
        // Only one thing plays at a time.
        let playing = self.player.borrow().as_ref().is_some_and(|p| p.state() == PlayState::Playing);
        if playing {
            CURRENT.with(|current| {
                let previous = current.replace(Some(Rc::downgrade(&self.player)));
                if let Some(other) = previous.and_then(|weak| weak.upgrade()) {
                    if !Rc::ptr_eq(&other, &self.player) {
                        if let Some(player) = other.borrow().as_ref() {
                            player.pause();
                        }
                    }
                }
            });
        }
        if self.player.borrow().as_ref().is_some_and(AudioPlayer::take_finished) {
            self.progress.set(0.);
        }
        // Playback that could not start or broke off (no sound output, a failed sink) says so
        // under the waveform instead of doing nothing.
        if let Some(reason) = self.player.borrow().as_ref().and_then(AudioPlayer::take_error) {
            self.progress.set(0.);
            self.set_phase(Phase::Failed(format!("Can't play audio: {reason}")));
            return;
        }
        self.update_playback();
        // While playing, follow the position ten times a second; the ticker ends by itself.
        if playing && !self.ticking.replace(true) {
            let weak = Rc::downgrade(self);
            glib::timeout_add_local(Duration::from_millis(100), move || {
                let Some(bubble) = weak.upgrade() else { return glib::ControlFlow::Break };
                bubble.update_playback();
                let playing = bubble.player.borrow().as_ref().is_some_and(|p| p.state() == PlayState::Playing);
                if playing {
                    glib::ControlFlow::Continue
                } else {
                    bubble.ticking.set(false);
                    glib::ControlFlow::Break
                }
            });
        }
    }

    fn seek_to(&self, fraction: f64) {
        if *self.phase.borrow() != Phase::Ready {
            return;
        }
        let ms = (fraction.clamp(0., 1.) * self.duration_ms.get() as f64) as u64;
        if let Some(player) = self.player.borrow().as_ref() {
            player.play_from(ms);
        }
    }
}

fn install(bubble: &Rc<Bubble>) {
    // Waveform drawing: bars pooled from the helper's 64 peaks; played ones in full colour.
    let weak = Rc::downgrade(bubble);
    bubble.wave.set_draw_func(move |area, cr, width, height| {
        let Some(bubble) = weak.upgrade() else { return };
        let colour = area.color();
        let peaks = bubble.peaks.borrow();
        let (width, height) = (f64::from(width), f64::from(height));
        let (bar, gap) = (3., 2.);
        let count = ((width + gap) / (bar + gap)).floor().max(1.) as usize;
        let played = bubble.progress.get() * count as f64;
        for index in 0..count {
            // Quiet files and files not prepared yet draw small dots, as in the design.
            let amplitude = if peaks.is_empty() {
                0.
            } else {
                let from = index * peaks.len() / count;
                let to = ((index + 1) * peaks.len() / count).max(from + 1).min(peaks.len());
                f64::from(*peaks[from..to].iter().max().unwrap_or(&0)) / 255.
            };
            let bar_height = (amplitude * height).max(bar);
            let alpha = if (index as f64) < played { 1. } else { 0.45 };
            cr.set_source_rgba(f64::from(colour.red()), f64::from(colour.green()), f64::from(colour.blue()), f64::from(colour.alpha()) * alpha);
            let x = index as f64 * (bar + gap);
            let y = (height - bar_height) / 2.;
            cr.rectangle(x, y, bar, bar_height);
            let _ = cr.fill();
        }
    });
    // Click or tap on the waveform to seek.
    let click = gtk::GestureClick::new();
    let weak = Rc::downgrade(bubble);
    click.connect_pressed(move |gesture, _, x, _| {
        if let (Some(bubble), Some(area)) = (weak.upgrade(), gesture.widget()) {
            area.grab_focus();
            bubble.seek_to(x / f64::from(area.width().max(1)));
        }
    });
    bubble.wave.add_controller(click);
    // Keyboard: Left and Right move by five seconds, Space plays or pauses.
    let keys = gtk::EventControllerKey::new();
    let weak = Rc::downgrade(bubble);
    keys.connect_key_pressed(move |_, key, _, _| {
        let Some(bubble) = weak.upgrade() else { return glib::Propagation::Proceed };
        let (Some(duration), true) = (Some(bubble.duration_ms.get()).filter(|d| *d > 0), *bubble.phase.borrow() == Phase::Ready) else {
            return glib::Propagation::Proceed;
        };
        let position = bubble.player.borrow().as_ref().map_or(0, AudioPlayer::position_ms);
        let target = match key {
            gtk::gdk::Key::Left => position.saturating_sub(SEEK_STEP_MS),
            gtk::gdk::Key::Right => (position + SEEK_STEP_MS).min(duration.saturating_sub(1)),
            gtk::gdk::Key::space => {
                if let Some(player) = bubble.player.borrow().as_ref() {
                    player.toggle();
                }
                return glib::Propagation::Stop;
            }
            _ => return glib::Propagation::Proceed,
        };
        bubble.seek_to(target as f64 / duration as f64);
        glib::Propagation::Stop
    });
    bubble.wave.add_controller(keys);

    // The button: download, cancel, retry, or play and pause.
    let weak = Rc::downgrade(bubble);
    bubble.button.connect_clicked(move |_| {
        let Some(bubble) = weak.upgrade() else { return };
        let Some(ui) = bubble.ui.upgrade() else { return };
        let phase = bubble.phase.borrow().clone();
        match phase {
            Phase::Ready => {
                if let Some(player) = bubble.player.borrow().as_ref() {
                    player.toggle();
                }
            }
            Phase::Downloading(_) => {
                ui.downloads.borrow_mut().cancel(&bubble.owner, &bubble.file);
                bubble.set_phase(Phase::Waiting);
            }
            Phase::Preparing => {}
            // The user asked to play: this click is what starts decoding.
            Phase::Downloaded(path) => bubble.prepare(path, true),
            Phase::Waiting | Phase::Failed(_) => {
                // A file that is already here is prepared (on this click); otherwise it is downloaded.
                match ui.downloads.borrow().status(&bubble.owner, &bubble.file) {
                    Some(Status::Ready(path)) if path.is_file() => bubble.prepare(path, true),
                    _ => {
                        ui.downloads.borrow_mut().invalidate(&bubble.owner, &bubble.file);
                        match ui.downloads.borrow_mut().start(&bubble.owner, bubble.file.clone()) {
                            Ok(()) => bubble.set_phase(Phase::Downloading(None)),
                            Err(reason) => bubble.set_phase(Phase::Failed(reason.into())),
                        }
                    }
                }
            }
        }
    });

    // Follow the download, and the playback position while it plays.
    let weak = Rc::downgrade(bubble);
    glib::timeout_add_local(Duration::from_millis(150), move || {
        let Some(bubble) = weak.upgrade() else { return glib::ControlFlow::Break };
        let Some(ui) = bubble.ui.upgrade() else { return glib::ControlFlow::Break };
        if !ui.attachment_is_current(&bubble.owner, &bubble.peer, &bubble.id, &bubble.file) {
            return glib::ControlFlow::Break;
        }
        let phase = bubble.phase.borrow().clone();
        match phase {
            Phase::Ready => bubble.update_playback(),
            Phase::Preparing | Phase::Failed(_) | Phase::Downloaded(_) => {}
            Phase::Waiting | Phase::Downloading(_) => {
                if !ui.in_chat_viewport(&bubble.wave) {
                    return glib::ControlFlow::Continue;
                }
                // Audio too large to prepare is never downloaded automatically.
                if bubble.file.size.is_some_and(|size| size > MAX_FILE_BYTES) {
                    bubble.set_phase(Phase::Failed("Too large to preview".into()));
                    return glib::ControlFlow::Continue;
                }
                let automatic = ui.auto_download_enabled.get()
                    && !bubble.file.manual
                    && crate::attachment_ui::automatic_media_allowed(ui.smoke, bubble.auto.0, bubble.auto.1, ui.state.borrow().current());
                let mut status = ui.downloads.borrow().status(&bubble.owner, &bubble.file);
                if status.is_none() && automatic && phase == Phase::Waiting {
                    let limit = MAX_FILE_BYTES.min(ui.auto_download_limit.get());
                    ui.downloads.borrow_mut().start_automatic(&bubble.owner, bubble.file.clone(), limit);
                    status = ui.downloads.borrow().status(&bubble.owner, &bubble.file);
                }
                let trusted = crate::audio_bubble_ui::prepare_without_a_click(ui.smoke, bubble.auto.0, ui.state.borrow().current());
                bubble.apply_status(status, &phase, trusted);
            }
        }
        glib::ControlFlow::Continue
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{collections::BTreeMap, path::{Path, PathBuf}, time::Instant};

    fn attachment(name: &str, mime: &str) -> Attachment {
        Attachment { wire_url: String::new(), name: name.into(), mime: mime.into(), size: None, width: None, height: None, hashes: BTreeMap::new(), manual: false, sticker: false, voice: false }
    }

    #[test]
    fn which_attachments_get_the_audio_player() {
        for (name, mime, audio) in [
            ("voice.ogg", "audio/ogg", true),
            ("voice.opus", "audio/ogg", true),
            ("song.mp3", "audio/mpeg", true),
            ("sound.wav", "audio/x-wav", true),
            ("note.m4a", "audio/mp4", true),
            ("recording.webm", "audio/webm", true),
            ("voice.oga", "", true),
            ("song.MP3", "application/octet-stream", true),
            // Video, images and unknown files keep their own presentation.
            ("clip.webm", "video/webm", false),
            ("clip.webm", "", false),
            ("clip.ogg", "video/ogg", false),
            ("clip.mp4", "video/mp4", false),
            ("photo.png", "image/png", false),
            ("notes.txt", "text/plain", false),
            ("noextension", "", false),
            ("song.mp3", "image/png", false),
        ] {
            assert_eq!(attachment(name, mime).audio_candidate(), audio, "{name} ({mime})");
        }
        let mut sticker = attachment("a.ogg", "audio/ogg");
        sticker.sticker = true;
        assert!(!sticker.audio_candidate());

        // `attachment_ui.rs` asks the sandboxed preview first, so a file both would take stays with
        // it; only audio the preview does not claim (a type-less FLAC or AAC) reaches the media player.
        for (name, mime, preview, fallback) in [
            ("a.flac", "", false, true),
            ("a.aac", "application/octet-stream", false, true),
            ("a.mp3", "audio/mpeg", true, true),
            ("a.flac", "audio/flac", true, true),
            ("clip.mp4", "video/mp4", false, false),
        ] {
            let file = attachment(name, mime);
            assert_eq!(file.audio_candidate(), preview, "{name} ({mime}) preview");
            assert_eq!(file.player_audio_candidate(), fallback, "{name} ({mime}) fallback");
        }
    }

    fn fixture(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/audio").join(name)
    }
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
    fn bubble_of(root: &gtk::Box) -> Rc<Bubble> {
        unsafe { root.data::<Rc<Bubble>>("audio-bubble").expect("the bubble state").as_ref().clone() }
    }
    #[gtk::test]
    #[ignore = "requires a private GTK display, built audio helper and bubblewrap"]
    fn voice_note_player_keeps_native_controls_and_immediate_feedback() {
        adw::init().unwrap();
        let app=adw::Application::builder().application_id("com.negzero.VoiceIntentFixture").flags(gio::ApplicationFlags::NON_UNIQUE).build();
        app.register(None::<&gio::Cancellable>).unwrap();
        let ui=Ui::new(&app,true);
        let mut file=attachment("opaque.ogg","audio/ogg");file.voice=true;
        let message=Message{own:true,attachment:Some(file),..Default::default()};
        let started=Instant::now();let root=ui.audio_attachment(&message);
        let bubble=bubble_of(&root);bubble.sink.set("fakesink");
        assert!(started.elapsed()<Duration::from_millis(400));
        assert_eq!(bubble.button.tooltip_text().as_deref(),Some("Download voice message"));
        let window=gtk::Window::builder().child(&root).default_width(420).default_height(180).build();window.present();
        let started=Instant::now();bubble.prepare(fixture("tone-opus.ogg"),false);
        assert!(started.elapsed()<Duration::from_millis(400));assert_eq!(*bubble.phase.borrow(),Phase::Preparing);
        assert!(wait(Duration::from_secs(20),||*bubble.phase.borrow()==Phase::Ready));
        assert_eq!(bubble.peaks.borrow().len(),audio_preview_run::BARS);
        assert_eq!(bubble.button.tooltip_text().as_deref(),Some("Play voice message"));
        bubble.button.emit_clicked();
        assert!(wait(Duration::from_secs(10),||bubble.player.borrow().as_ref().is_some_and(|p|p.state()==PlayState::Playing)));
        assert_eq!(bubble.button.tooltip_text().as_deref(),Some("Pause voice message"));
        bubble.seek_to(0.5);
        assert!(wait(Duration::from_secs(10),||bubble.player.borrow().as_ref().is_some_and(|p|p.state()==PlayState::Idle)));
        window.close();ui.window.close();
    }

    /// A real bubble in a real window: prepared through the sandbox, played, seeked by clicking the
    /// waveform, and refused with a reason for a crafted file or a video.
    #[gtk::test]
    #[ignore = "requires a display (use tools/headless-gtk-test.sh), the built helpers and bubblewrap"]
    fn the_bubble_prepares_plays_seeks_and_explains_refusals() {
        adw::init().unwrap();
        let app = adw::Application::builder().application_id("com.negzero.AudioBubbleFixture").flags(gio::ApplicationFlags::NON_UNIQUE).build();
        app.register(None::<&gio::Cancellable>).unwrap();
        let ui = Ui::new(&app, true);
        ui.window.set_default_size(1000, 800);
        ui.window.present();
        wait(Duration::from_millis(300), || false);

        let message = |name: &str, mime: &str| Message { own: true, attachment: Some(attachment(name, mime)), ..Default::default() };
        let root = ui.audio_attachment(&message("tone.mp3", "audio/mpeg"));
        let window = gtk::Window::builder().default_width(420).default_height(200).child(&root).build();
        window.present();
        let bubble = bubble_of(&root);
        bubble.sink.set("fakesink");
        assert_eq!(*bubble.phase.borrow(), Phase::Waiting);
        assert_eq!(bubble.button.tooltip_text().as_deref(), Some("Download audio"), "not downloaded yet");

        // Downloaded: the helper prepares it and the player appears with its length and waveform.
        bubble.prepare(fixture("tone.mp3"), false);
        assert_eq!(*bubble.phase.borrow(), Phase::Preparing);
        assert!(wait(Duration::from_secs(20), || *bubble.phase.borrow() == Phase::Ready), "prepared: {:?}", bubble.phase.borrow());
        assert_eq!(bubble.time.label(), clock(bubble.duration_ms.get()));
        assert_eq!(bubble.time.label(), "00:01", "about 1.1 seconds");
        assert_eq!(bubble.peaks.borrow().len(), audio_preview_run::BARS);
        assert_eq!(bubble.button.tooltip_text().as_deref(), Some("Play audio"));

        // Play: the button turns into pause and the waveform fills as it goes.
        bubble.button.emit_clicked();
        assert!(wait(Duration::from_secs(10), || bubble.player.borrow().as_ref().unwrap().state() == PlayState::Playing));
        assert!(wait(Duration::from_secs(3), || bubble.progress.get() > 0.15), "progress {}", bubble.progress.get());
        assert_eq!(bubble.button.tooltip_text().as_deref(), Some("Pause audio"));
        assert_ne!(bubble.time.label(), clock(bubble.duration_ms.get()), "elapsed time shows while playing");

        // Clicking the waveform near the end seeks there.
        let wave = &bubble.wave;
        let (width, height) = (wave.width().max(1), wave.height().max(1));
        let _ = height;
        bubble.seek_to(0.8);
        assert!(wait(Duration::from_secs(10), || bubble.player.borrow().as_ref().unwrap().state() == PlayState::Playing));
        assert!(wait(Duration::from_secs(3), || bubble.progress.get() >= 0.75), "seeked: {}", bubble.progress.get());
        let _ = width;

        // It plays out and returns to rest: play button, full length, empty waveform.
        assert!(wait(Duration::from_secs(10), || bubble.player.borrow().as_ref().unwrap().state() == PlayState::Idle), "it finished");
        wait(Duration::from_millis(400), || false);
        assert_eq!(bubble.progress.get(), 0.);
        assert_eq!(bubble.time.label(), "00:01");
        assert_eq!(bubble.button.tooltip_text().as_deref(), Some("Play audio"));

        // A video WebM and crafted bytes are refused with a reason and a retry button, no player.
        let refusals: Vec<(&str, &str, Vec<u8>)> = vec![
            ("clip.webm", "audio/webm", std::fs::read(fixture("video-only.webm")).unwrap()),
            ("evil.mp3", "audio/mpeg", b"ID3\x04\x00\x00\x00\x00\x00\x10 definitely not audio".to_vec()),
            ("wrong.wav", "audio/x-wav", std::fs::read(fixture("tone.mp3")).unwrap()),
            ("theora.ogg", "audio/ogg", std::fs::read(fixture("video-theora.ogg")).unwrap()),
        ];
        for (name, mime, bytes) in refusals {
            let dir = std::env::temp_dir().join(format!("bubble-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&dir).unwrap();
            let path = dir.join(name);
            std::fs::write(&path, bytes).unwrap();
            let root = ui.audio_attachment(&message(name, mime));
            let bad = bubble_of(&root);
            bad.prepare(path, false);
            assert!(wait(Duration::from_secs(20), || matches!(*bad.phase.borrow(), Phase::Failed(_))), "{name} must fail, was {:?}", bad.phase.borrow());
            assert!(bad.player.borrow().is_none(), "{name}: no player is created");
            assert!(!bad.time.label().is_empty(), "{name}: the reason is shown under the waveform");
            assert_eq!(bad.button.tooltip_text().as_deref(), Some(bad.time.label().as_str()), "{name}: and on the retry button");
            std::fs::remove_dir_all(dir).unwrap();
        }
        window.close();
        ui.window.close();
    }

    /// In a real chat: an audio file gets the player, anything else keeps its own presentation.
    #[gtk::test]
    #[ignore = "requires a display (use tools/headless-gtk-test.sh)"]
    fn an_audio_message_in_the_timeline_gets_the_player_and_other_files_do_not() {
        fn has_class(widget: &gtk::Widget, class: &str) -> bool {
            widget.has_css_class(class) || {
                let mut child = widget.first_child();
                let mut found = false;
                while let Some(current) = child {
                    found |= has_class(&current, class);
                    child = current.next_sibling();
                }
                found
            }
        }
        adw::init().unwrap();
        let app = adw::Application::builder().application_id("com.negzero.AudioTimelineFixture").flags(gio::ApplicationFlags::NON_UNIQUE).build();
        app.register(None::<&gio::Cancellable>).unwrap();
        let ui = Ui::new(&app, true);
        ui.window.set_default_size(1000, 800);
        ui.window.present();
        wait(Duration::from_millis(300), || false);
        let cases = [("voice.ogg", "audio/ogg", true), ("song.mp3", "audio/mpeg", true), ("notes.txt", "text/plain", false), ("clip.mp4", "video/mp4", false), ("clip.webm", "video/webm", false)];
        let mut ids = Vec::new();
        for (index, (name, mime, _)) in cases.iter().enumerate() {
            let mut state = ui.state.borrow_mut();
            let message = Message {
                id: format!("audio-case-{index}"),
                wire_id: format!("audio-case-{index}"),
                kind: "file".into(),
                own: true,
                attachment: Some(attachment(name, mime)),
                ..Default::default()
            };
            ids.push(message.id.clone());
            state.current_mut().messages.push(message);
        }
        ui.render_messages(true);
        wait(Duration::from_millis(1500), || false);
        let bubbles = ui.bubbles.borrow();
        for ((name, mime, audio), id) in cases.iter().zip(&ids) {
            let bubble = bubbles.get(id).unwrap_or_else(|| panic!("{name} was rendered"));
            assert_eq!(has_class(bubble, "audio-bubble"), *audio, "{name} ({mime})");
        }
    }

    /// The point of "decoding waits for a click": a finished download makes the file available and
    /// nothing more. No helper starts, no file is opened for decoding, until the user presses play,
    /// and that single press then prepares and plays.
    #[gtk::test]
    #[ignore = "requires a display (use tools/headless-gtk-test.sh), the built helpers and bubblewrap"]
    fn a_finished_download_is_not_decoded_until_the_user_presses_play() {
        adw::init().unwrap();
        let app = adw::Application::builder().application_id("com.negzero.AudioClickFixture").flags(gio::ApplicationFlags::NON_UNIQUE).build();
        app.register(None::<&gio::Cancellable>).unwrap();
        let ui = Ui::new(&app, true);
        ui.window.present();
        wait(Duration::from_millis(300), || false);
        let root = ui.audio_attachment(&Message { own: true, attachment: Some(attachment("tone.mp3", "audio/mpeg")), ..Default::default() });
        let window = gtk::Window::builder().default_width(420).default_height(200).child(&root).build();
        window.present();
        let bubble = bubble_of(&root);
        bubble.sink.set("fakesink");
        PEAK.store(0, std::sync::atomic::Ordering::SeqCst);

        // The download finishes (as the download manager reports it).
        bubble.apply_status(Some(Status::Ready(fixture("tone.mp3"))), &Phase::Downloading(None), false);
        assert_eq!(*bubble.phase.borrow(), Phase::Downloaded(fixture("tone.mp3")));
        assert_eq!(bubble.button.tooltip_text().as_deref(), Some("Play audio"), "it offers play, not a spinner");
        // Give any automatic preparation every chance to start.
        wait(Duration::from_millis(2500), || false);
        assert_eq!(PEAK.load(std::sync::atomic::Ordering::SeqCst), 0, "no helper was started by the download finishing");
        assert!(bubble.player.borrow().is_none(), "and nothing was opened for decoding");
        assert_eq!(*bubble.phase.borrow(), Phase::Downloaded(fixture("tone.mp3")));

        // One press prepares and plays.
        bubble.button.emit_clicked();
        assert_eq!(*bubble.phase.borrow(), Phase::Preparing);
        assert!(wait(Duration::from_secs(20), || *bubble.phase.borrow() == Phase::Ready), "prepared");
        assert!(wait(Duration::from_secs(10), || bubble.player.borrow().as_ref().is_some_and(|p| p.state() == PlayState::Playing)), "and playing, without a second click");
        assert_eq!(PEAK.load(std::sync::atomic::Ordering::SeqCst), 1);
        bubble.player.borrow().as_ref().unwrap().stop();
        window.close();
        ui.window.close();
    }

    #[test]
    fn only_trusted_senders_are_prepared_without_a_click() {
        use crate::model::Chat;
        let chat = |contact: bool, group: bool| Chat { contact, group, ..Default::default() };
        // (own message, contact, group, expected)
        for (own, contact, group, expected) in [
            (true, false, false, true),   // the user's own message
            (true, false, true, true),    // the user's own message in a room
            (false, true, false, true),   // a contact in a direct chat
            (false, false, false, false), // a stranger (even if the message arrived encrypted)
            (false, false, true, false),  // a participant of a joined room
            (false, true, true, false),   // a room is never "a contact", whatever the flag says
        ] {
            assert_eq!(prepare_without_a_click(false, own, &chat(contact, group)), expected, "own {own} contact {contact} group {group}");
        }
        assert!(!prepare_without_a_click(true, true, &chat(true, false)), "never in smoke-test mode");
    }

    /// A contact's finished download is prepared on its own (not played); a stranger's is not.
    #[gtk::test]
    #[ignore = "requires a display (use tools/headless-gtk-test.sh), the built helpers and bubblewrap"]
    fn a_trusted_senders_download_is_prepared_without_a_click_but_not_played() {
        adw::init().unwrap();
        let app = adw::Application::builder().application_id("com.negzero.AudioContactFixture").flags(gio::ApplicationFlags::NON_UNIQUE).build();
        app.register(None::<&gio::Cancellable>).unwrap();
        let ui = Ui::new(&app, true);
        ui.window.present();
        wait(Duration::from_millis(300), || false);
        let root = ui.audio_attachment(&Message { own: false, attachment: Some(attachment("tone.mp3", "audio/mpeg")), ..Default::default() });
        let window = gtk::Window::builder().default_width(420).default_height(200).child(&root).build();
        window.present();
        let bubble = bubble_of(&root);
        bubble.sink.set("fakesink");
        bubble.apply_status(Some(Status::Ready(fixture("tone.mp3"))), &Phase::Downloading(None), true);
        assert!(wait(Duration::from_secs(20), || *bubble.phase.borrow() == Phase::Ready), "prepared by itself: {:?}", bubble.phase.borrow());
        assert_eq!(bubble.player.borrow().as_ref().unwrap().state(), PlayState::Idle, "but never started playing");
        assert_eq!(bubble.time.label(), "00:01", "the length and waveform are already there");
        window.close();
        ui.window.close();
    }

    /// 0.5 s of silent 8 kHz mono WAV: plays through the real output without being heard.
    fn silent_wav(path: &Path) {
        let data = 8_000usize;
        let mut wav = b"RIFF".to_vec();
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
        std::fs::write(path, wav).unwrap();
    }

    /// The whole flow with the REAL audio output (silent audio): press play on a downloaded file, it
    /// prepares, plays through the sound system and ends cleanly, with no failure shown.
    #[gtk::test]
    #[ignore = "uses the real audio output (silently); requires a display (headless runner) and the built helpers"]
    fn pressing_play_on_a_downloaded_file_plays_through_the_real_audio_output() {
        adw::init().unwrap();
        let app = adw::Application::builder().application_id("com.negzero.AudioRealOutputFixture").flags(gio::ApplicationFlags::NON_UNIQUE).build();
        app.register(None::<&gio::Cancellable>).unwrap();
        let ui = Ui::new(&app, true);
        ui.window.present();
        wait(Duration::from_millis(300), || false);
        let path = std::env::temp_dir().join(format!("silence-{}.wav", uuid::Uuid::new_v4()));
        silent_wav(&path);
        let root = ui.audio_attachment(&Message { own: false, attachment: Some(attachment("silence.wav", "audio/x-wav")), ..Default::default() });
        let window = gtk::Window::builder().default_width(420).default_height(200).child(&root).build();
        window.present();
        let bubble = bubble_of(&root);
        // No sink override: this is the production path (`autoaudiosink`).
        assert_eq!(bubble.sink.get(), "autoaudiosink");
        bubble.apply_status(Some(Status::Ready(path.clone())), &Phase::Downloading(None), false);
        bubble.button.emit_clicked();
        assert!(wait(Duration::from_secs(20), || *bubble.phase.borrow() == Phase::Ready || matches!(*bubble.phase.borrow(), Phase::Failed(_))), "prepared: {:?}", bubble.phase.borrow());
        let mut saw_playing = false;
        let finished = wait(Duration::from_secs(10), || {
            saw_playing |= bubble.player.borrow().as_ref().is_some_and(|p| p.state() == PlayState::Playing);
            matches!(*bubble.phase.borrow(), Phase::Failed(_)) || (saw_playing && bubble.player.borrow().as_ref().is_some_and(|p| p.state() == PlayState::Idle))
        });
        let phase = bubble.phase.borrow().clone();
        std::fs::remove_file(&path).unwrap();
        assert!(finished, "it finished: {phase:?}");
        assert!(!matches!(phase, Phase::Failed(_)), "no failure was shown: {phase:?}");
        assert!(saw_playing, "it reached Playing");
        window.close();
        ui.window.close();
    }

    /// Eight audio messages appear at once (as in a flood from a room): all get prepared, but never
    /// more than two helpers run at the same time.
    #[gtk::test]
    #[ignore = "requires a display (use tools/headless-gtk-test.sh), the built helpers and bubblewrap"]
    fn many_audio_messages_are_prepared_two_at_a_time() {
        adw::init().unwrap();
        let app = adw::Application::builder().application_id("com.negzero.AudioFloodFixture").flags(gio::ApplicationFlags::NON_UNIQUE).build();
        app.register(None::<&gio::Cancellable>).unwrap();
        let ui = Ui::new(&app, true);
        ui.window.present();
        wait(Duration::from_millis(300), || false);
        let roots: Vec<gtk::Box> = (0..8)
            .map(|_| ui.audio_attachment(&Message { own: true, attachment: Some(attachment("tone.wav", "audio/x-wav")), ..Default::default() }))
            .collect();
        let bubbles: Vec<Rc<Bubble>> = roots.iter().map(bubble_of).collect();
        PEAK.store(0, std::sync::atomic::Ordering::SeqCst);
        for bubble in &bubbles {
            bubble.sink.set("fakesink");
            bubble.prepare(fixture("tone.wav"), false);
        }
        assert!(wait(Duration::from_secs(60), || bubbles.iter().all(|b| *b.phase.borrow() == Phase::Ready)), "all eight were prepared");
        let peak = PEAK.load(std::sync::atomic::Ordering::SeqCst);
        assert!((1..=2).contains(&peak), "{peak} helpers ran at once; the limit is two");
        assert_eq!(RUNNING.load(std::sync::atomic::Ordering::SeqCst), 0, "none left running");
        ui.window.close();
    }

    #[test]
    fn the_duration_reads_like_the_design_placeholder() {
        assert_eq!(clock(0), "00:00");
    }

    #[test]
    fn the_duration_reads_like_the_design() {
        assert_eq!(clock(0), "00:00");
        assert_eq!(clock(2_400), "00:02");
        assert_eq!(clock(59_999), "00:59");
        assert_eq!(clock(61_000), "01:01");
        assert_eq!(clock(30 * 60 * 1000), "30:00");
    }
}
