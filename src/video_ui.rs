//! Native playback of authenticated cached media. The open descriptor pins the verified bytes.
use crate::{downloads::VerifiedFile, model::Message, ui::Ui, widgets::*};
use adw::prelude::*;
use glib::translate::{from_glib_full, from_glib_none};
use gtk::{gio, glib};
use std::ffi::CString;

unsafe extern "C" {
    fn monogram_playback_init() -> i32;
    fn monogram_playback_network_blocked() -> i32;
    #[cfg(test)]
    fn monogram_playback_probe(uri: *const std::ffi::c_char, ms: u32);
    fn monogram_video_new(
        uri: *const std::ffi::c_char,
        controls: *mut *mut gtk::ffi::GtkWidget,
        replay: *mut *mut gtk::ffi::GtkWidget,
    ) -> *mut gtk::ffi::GtkMediaStream;
}
use std::{
    cell::{Cell, RefCell},
    os::fd::AsRawFd,
    rc::Rc,
    sync::{Arc, atomic::Ordering},
    time::{Duration, Instant},
};

static PLAYBACK_READY: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
// Call on a worker: Clapper scans plugins before the first player can exist.
pub(crate) fn initialize() -> bool {
    *PLAYBACK_READY.get_or_init(|| unsafe {monogram_playback_init()!=0})
}

fn round_seek_fraction(widget: &gtk::Widget, x: f64, y: f64) -> Option<f64> {
    let radius = f64::from(widget.width().min(widget.height())) / 2.;
    let dx = x - f64::from(widget.width()) / 2.;
    let dy = y - f64::from(widget.height()) / 2.;
    let distance = dx.hypot(dy);
    if radius <= 0. || distance > radius || distance < (radius - 32.).max(0.) {
        return None;
    }
    Some(
        (dy.atan2(dx) + std::f64::consts::FRAC_PI_2).rem_euclid(std::f64::consts::TAU)
            / std::f64::consts::TAU,
    )
}

fn update_round_mute_button(button: &gtk::Button, muted: bool) {
    let label = if muted { "Unmute video" } else { "Mute video" };
    button.set_icon_name(if muted {
        "audio-volume-muted-symbolic"
    } else {
        "audio-volume-high-symbolic"
    });
    button.set_tooltip_text(Some(label));
    button.update_property(&[gtk::accessible::Property::Label(label)]);
}

struct RoundProgressClock {
    displayed: Cell<f64>,
    start: Cell<f64>,
    target: Cell<i64>,
    sample_time: Cell<Instant>,
    interval: Cell<Duration>,
    running: Cell<bool>,
}
impl RoundProgressClock {
    fn new() -> Self {
        Self {
            displayed: Cell::new(0.),
            start: Cell::new(0.),
            target: Cell::new(0),
            sample_time: Cell::new(Instant::now()),
            interval: Cell::new(Duration::from_millis(100)),
            running: Cell::new(false),
        }
    }
    fn anchor(&self, timestamp: i64) {
        self.displayed.set(timestamp as f64);
        self.start.set(timestamp as f64);
        self.target.set(timestamp);
        self.sample_time.set(Instant::now());
    }
    fn advance(&self, now: Instant) {
        let elapsed = now.duration_since(self.sample_time.get()).as_secs_f64();
        let period = self.interval.get().as_secs_f64().max(0.001);
        let amount = (elapsed / period).clamp(0., 1.);
        self.displayed
            .set(self.start.get() + (self.target.get() as f64 - self.start.get()) * amount);
    }
    fn sample(&self, media: &gtk::MediaStream) {
        let now = Instant::now();
        self.advance(now);
        let timestamp = media.timestamp();
        let previous = self.target.get();
        let period = now
            .duration_since(self.sample_time.get())
            .clamp(Duration::from_millis(50), Duration::from_millis(150));
        if !media.is_playing()
            || media.is_seeking()
            || timestamp < previous
            || timestamp.saturating_sub(previous) > 500_000
        {
            self.anchor(timestamp);
            return;
        }
        // Stay one sample behind so buffering never produces invented progress.
        self.start.set(self.displayed.get());
        self.target.set(timestamp);
        self.sample_time.set(now);
        self.interval.set(period);
    }
    fn timestamp(&self, media: &gtk::MediaStream) -> f64 {
        if media.is_playing() {
            self.displayed.get()
        } else {
            media.timestamp() as f64
        }
    }
    fn start(self: &Rc<Self>, display: &gtk::DrawingArea, media: &gtk::MediaStream) {
        if !display.is_mapped() || !media.is_playing() || media.is_seeking() {
            self.anchor(media.timestamp());
            return;
        }
        if self.running.replace(true) {
            return;
        }
        let media = media.downgrade();
        let smooth = self.clone();
        display.add_tick_callback(move |display, _| {
            let Some(media) = media.upgrade() else {
                smooth.running.set(false);
                return glib::ControlFlow::Break;
            };
            if !display.is_mapped() || !media.is_playing() || media.is_seeking() {
                smooth.anchor(media.timestamp());
                smooth.running.set(false);
                display.queue_draw();
                return glib::ControlFlow::Break;
            }
            let now = Instant::now();
            smooth.advance(now);
            display.queue_draw();
            if now.duration_since(smooth.sample_time.get()) > smooth.interval.get().mul_f64(2.) {
                smooth.running.set(false);
                glib::ControlFlow::Break
            } else {
                glib::ControlFlow::Continue
            }
        });
    }
}

pub(crate) struct Playback {
    pub(crate) media: gtk::MediaStream,
    pub(crate) controls: gtk::Widget,
    pub(crate) replay: gtk::Button,
    _input: VerifiedFile,
    pub(crate) expanded: Cell<bool>,
    inline_interactive: Cell<bool>,
    inline_users: Cell<usize>,
    /// When a preview was first seen ready, for [`Playback::keep_preview_silent`].
    prepared_at: Cell<Option<Instant>>,
}

/// How long a ready preview is kept muted against Clapper's late initial state.
const SETTLE: Duration = Duration::from_millis(1500);
/// True when `head` starts like a binary media container. Text such as an HLS or DASH manifest,
/// a playlist or an SDP file is refused even if the sender labelled it `video/*`.
fn media_container(head: &[u8]) -> bool {
    let at = |offset: usize, magic: &[u8]| head.get(offset..offset + magic.len()) == Some(magic);
    at(0, &[0x1a, 0x45, 0xdf, 0xa3]) // Matroska and WebM
        || [b"ftyp", b"moov", b"mdat", b"free", b"skip", b"wide", b"pnot"]
            .iter()
            .any(|atom| at(4, *atom)) // ISO BMFF and QuickTime
        || (at(0, b"RIFF") && at(8, b"AVI "))
        || at(0, b"OggS")
        || at(0, b"FLV\x01")
        || at(0, &[0x00, 0x00, 0x01, 0xba]) // MPEG program stream
        || at(0, &[0x30, 0x26, 0xb2, 0x75, 0x8e, 0x66, 0xcf, 0x11]) // ASF and WMV
        || (at(0, &[0x47]) && at(188, &[0x47])) // MPEG transport stream
        || (at(4, &[0x47]) && at(196, &[0x47])) // M2TS
}
fn audio_container(head:&[u8])->bool {
    head.starts_with(b"ID3") && head.get(3).is_some_and(|v|(2..=4).contains(v))
        || head.starts_with(b"fLaC")
        || head.starts_with(b"RIFF") && head.get(8..12)==Some(b"WAVE")
        || head.len()>=4 && head[0]==0xff && head[1]&0xe0==0xe0 && head[1]&0x18!=0x08
}
impl Playback {
    /// Keep an inline preview silent while it settles. Called for every preview on each poll.
    ///
    /// Previews play muted. Clapper can publish its own "not muted" state shortly after the
    /// stream is ready, so for [`SETTLE`] after that the mute is put back. After that it is left
    /// alone for good: whoever unmutes the player later, the viewer opened on it or the person in
    /// the system mixer, means it, and muting it again every 150 ms is what made the sound
    /// impossible to turn on.
    pub(crate) fn keep_preview_silent(&self) {
        self.keep_preview_silent_at(Instant::now());
    }
    fn keep_preview_silent_at(&self, now: Instant) {
        // Someone is listening: the viewer is showing this player, or a round video was tapped.
        if self.inline_interactive.get() || self.expanded.get() {
            return;
        }
        let ready = match self.prepared_at.get() {
            Some(ready) => ready,
            None => {
                self.prepared_at.set(Some(now));
                now
            }
        };
        if now.saturating_duration_since(ready) <= SETTLE && !self.media.is_muted() {
            self.media.set_muted(true);
        }
    }
    pub(crate) fn new(input: VerifiedFile) -> Option<Rc<Self>> {Self::new_kind(input,false)}
    pub(crate) fn new_audio(input:VerifiedFile)->Option<Rc<Self>> {Self::new_kind(input,true)}
    fn new_kind(input:VerifiedFile,audio:bool)->Option<Rc<Self>> {
        if PLAYBACK_READY.get()!=Some(&true) {return None;}
        {
            use std::os::unix::fs::FileExt;
            let mut head = [0u8; 512];
            let read = input.0.read_at(&mut head, 0).ok()?;
            if !media_container(&head[..read]) && !(audio && audio_container(&head[..read])) {return None;}
        }
        let file = gio::File::for_path(format!("/proc/self/fd/{}", input.0.as_raw_fd()));
        let uri = CString::new(file.uri().as_str()).ok()?;
        let mut controls = std::ptr::null_mut();
        let mut replay = std::ptr::null_mut();
        let pointer = unsafe { monogram_video_new(uri.as_ptr(), &mut controls, &mut replay) };
        if pointer.is_null() {
            return None;
        }
        let media: gtk::MediaStream = unsafe { from_glib_full(pointer) };
        let controls: gtk::Widget = unsafe { from_glib_none(controls) };
        let replay: gtk::Button = unsafe { from_glib_none(replay.cast::<gtk::ffi::GtkButton>()) };
        media.set_muted(!audio);
        media.set_loop(!audio);
        Some(Rc::new(Self {
            media,
            controls,
            replay,
            _input: input,
            expanded: Cell::new(false),
            inline_interactive: Cell::new(false),
            prepared_at: Cell::new(None),
            inline_users: Cell::new(0),
        }))
    }
}
impl Drop for Playback {
    fn drop(&mut self) {
        self.media.pause();
    }
}
struct InlinePlayback {
    player: Rc<Playback>,
    signals: Vec<glib::SignalHandlerId>,
    visible: Cell<bool>,
}
impl InlinePlayback {
    fn new(player: Rc<Playback>, image: &AspectImage) -> Self {
        let picture = image.downgrade();
        let contents = player.media.connect_invalidate_contents(move |_| {
            if let Some(image) = picture.upgrade() {
                image.queue_draw();
            }
        });
        let picture = image.downgrade();
        let size = player.media.connect_invalidate_size(move |media| {
            if let Some(image) = picture.upgrade() {
                image.set_paintable(Some(media.clone().upcast()));
            }
        });
        Self {
            player,
            signals: vec![contents, size],
            visible: Cell::new(false),
        }
    }
    fn visible(&self, visible: bool) {
        if self.visible.replace(visible) != visible {
            let users = self.player.inline_users.get();
            self.player.inline_users.set(if visible {
                users + 1
            } else {
                users.saturating_sub(1)
            });
        }
        if !self.player.expanded.get() {
            let playing = self.player.inline_users.get() > 0;
            if playing && self.player.inline_interactive.get() {
                return;
            }
            if self.player.media.is_playing() != playing {
                self.player.media.set_playing(playing);
            }
        }
    }
}
impl Drop for InlinePlayback {
    fn drop(&mut self) {
        self.visible(false);
        for signal in self.signals.drain(..) {
            self.player.media.disconnect(signal);
        }
    }
}
impl Ui {
    pub(crate) fn video(
        self: &Rc<Self>,
        file: &crate::file_share::Attachment,
        path: &std::path::Path,
    ) -> Option<Rc<Playback>> {
        let key = file.key();
        let owner = self.state.borrow().account.clone();
        let cancel = self.downloads.borrow().token(&owner, file)?;
        {
            let mut images = self.images.borrow_mut();
            if let Some(player) = images.video(&key) {
                return Some(player);
            }
            images
                .video_tasks
                .retain(|_, token| !token.load(Ordering::Acquire));
            if images.video_failures.contains_key(&key)
                || images.video_tasks.contains_key(&key)
                || images.video_tasks.len() >= 2
            {
                return None;
            }
            images.video_tasks.insert(key.clone(), cancel.clone());
        }
        let source = file.clone();
        let path = path.to_path_buf();
        let weak = Rc::downgrade(self);
        glib::MainContext::default().spawn_local(async move {
            let worker_cancel = cancel.clone();
            let verified = gio::spawn_blocking(move || {
                let input = crate::downloads::verified_input(&path, &source, &worker_cancel)?;
                // Plugin scanning and Clapper initialization must never block GTK input.
                if !initialize() {
                    return Err("Video playback initialization failed");
                }
                Ok(input)
            })
            .await;
            let Some(ui) = weak.upgrade() else {
                return;
            };
            {
                let mut images = ui.images.borrow_mut();
                if images
                    .video_tasks
                    .get(&key)
                    .is_none_or(|token| !Arc::ptr_eq(token, &cancel))
                {
                    return;
                }
                images.video_tasks.remove(&key);
            }
            // A network reconnect does not invalidate authenticated local bytes.
            if ui.state.borrow().account != owner || cancel.load(Ordering::Acquire) {
                return;
            }
            match verified {
                Ok(Ok(input)) => {
                    if let Some(player) = Playback::new(input) {
                        ui.images.borrow_mut().remember_video(key, player);
                    } else {
                        ui.images
                            .borrow_mut()
                            .video_failures
                            .insert(key, "Video playback unavailable");
                    }
                }
                _ => {
                    ui.images
                        .borrow_mut()
                        .video_failures
                        .insert(key, "Video failed integrity verification");
                }
            }
        });
        None
    }
    pub fn attach_video(
        self: &Rc<Self>,
        message: &Message,
        content: &gtk::Box,
        controls: &gtk::Box,
    ) {
        let file = message.attachment.as_ref().unwrap().clone();
        let owner = self.state.borrow().account.clone();
        let peer = self.state.borrow().current().jid.clone();
        let id = message.id.clone();
        let image = AspectImage::thumbnail(320, false);
        let button = gtk::Button::builder()
            .child(&image)
            .visible(false)
            .halign(gtk::Align::Start)
            .tooltip_text("Play video")
            .build();
        button.set_widget_name(&id);
        button.add_css_class("shared-media-tile");
        button.add_css_class("message-media-tile");
        let stage = gtk::Overlay::builder().child(&button).build();
        stage.set_halign(gtk::Align::Start);
        let progress = gtk::DrawingArea::new();
        progress.set_hexpand(true);
        progress.set_vexpand(true);
        progress.set_can_target(false);
        progress.set_visible(false);
        progress.add_css_class("round-video-progress");
        stage.add_overlay(&progress);
        let mute = gtk::Button::from_icon_name("audio-volume-muted-symbolic");
        mute.set_halign(gtk::Align::Start);
        mute.set_valign(gtk::Align::Start);
        mute.set_margin_start(8);
        mute.set_margin_top(8);
        mute.set_visible(false);
        mute.add_css_class("circular");
        mute.add_css_class("round-video-mute");
        update_round_mute_button(&mute, true);
        stage.add_overlay(&mute);

        let round_player = Rc::new(RefCell::new(None::<Rc<Playback>>));
        let seek_preview = Rc::new(Cell::new(None::<f64>));
        let seeking = Rc::new(Cell::new(false));
        let seek_origin = Rc::new(Cell::new((0., 0.)));
        let resume_after_seek = Rc::new(Cell::new(false));
        let last_seek = Rc::new(Cell::new(Instant::now()));
        let smooth_progress = Rc::new(RoundProgressClock::new());
        let draw_player = round_player.clone();
        let draw_preview = seek_preview.clone();
        let draw_seeking = seeking.clone();
        let draw_smooth = smooth_progress.clone();
        progress.set_draw_func(move |_, ctx, width, height| {
            let Some(player) = draw_player.borrow().as_ref().cloned() else {
                return;
            };
            let duration = player.media.duration();
            if duration <= 0 {
                return;
            }
            let amount = draw_preview
                .get()
                .unwrap_or_else(|| draw_smooth.timestamp(&player.media) / duration as f64)
                .clamp(0., 1.);
            let paused = !player.media.is_playing();
            let center_x = f64::from(width) / 2.;
            let center_y = f64::from(height) / 2.;
            let inset = if paused { 18. } else { 3. };
            let radius = center_x.min(center_y) - inset;
            if radius <= 0. {
                return;
            }
            ctx.set_line_width(if paused { 4.5 } else { 3. });
            ctx.set_line_cap(gtk::cairo::LineCap::Round);
            if paused {
                ctx.set_source_rgba(1., 1., 1., 0.3);
                ctx.arc(center_x, center_y, radius, 0., std::f64::consts::TAU);
                let _ = ctx.stroke();
            }
            let start = -std::f64::consts::FRAC_PI_2;
            let end = start + amount * std::f64::consts::TAU;
            ctx.set_source_rgba(1., 1., 1., 0.82);
            ctx.arc(center_x, center_y, radius, start, end);
            let _ = ctx.stroke();
            if paused || draw_seeking.get() {
                ctx.arc(
                    center_x + end.cos() * radius,
                    center_y + end.sin() * radius,
                    if draw_seeking.get() { 11. } else { 8. },
                    0.,
                    std::f64::consts::TAU,
                );
                let _ = ctx.fill();
            }
        });

        let drag = gtk::GestureDrag::new();
        drag.set_button(1);
        drag.set_propagation_phase(gtk::PropagationPhase::Capture);
        let drag_stage = stage.downgrade();
        let drag_player = round_player.clone();
        let drag_preview = seek_preview.clone();
        let drag_seeking = seeking.clone();
        let drag_origin = seek_origin.clone();
        let drag_resume = resume_after_seek.clone();
        let drag_last_seek = last_seek.clone();
        let drag_progress = progress.downgrade();
        drag.connect_drag_begin(move |gesture, x, y| {
            let Some(stage) = drag_stage.upgrade() else {
                return;
            };
            let Some(player) = drag_player.borrow().as_ref().cloned() else {
                return;
            };
            let Some(amount) = round_seek_fraction(stage.upcast_ref(), x, y)
                .filter(|_| player.media.is_seekable())
            else {
                gesture.set_state(gtk::EventSequenceState::Denied);
                return;
            };
            gesture.set_state(gtk::EventSequenceState::Claimed);
            drag_origin.set((x, y));
            drag_resume.set(player.media.is_playing());
            drag_seeking.set(true);
            player.inline_interactive.set(true);
            player.media.pause();
            drag_preview.set(Some(amount));
            player
                .media
                .seek((player.media.duration() as f64 * amount) as i64);
            drag_last_seek.set(Instant::now());
            if let Some(progress) = drag_progress.upgrade() {
                progress.queue_draw();
            }
        });
        let drag_stage = stage.downgrade();
        let drag_player = round_player.clone();
        let drag_preview = seek_preview.clone();
        let drag_seeking = seeking.clone();
        let drag_origin = seek_origin.clone();
        let drag_last_seek = last_seek.clone();
        let drag_progress = progress.downgrade();
        drag.connect_drag_update(move |_, dx, dy| {
            if !drag_seeking.get() {
                return;
            }
            let (Some(stage), Some(player)) =
                (drag_stage.upgrade(), drag_player.borrow().as_ref().cloned())
            else {
                return;
            };
            let (x, y) = drag_origin.get();
            let amount = round_seek_fraction(stage.upcast_ref(), x + dx, y + dy)
                .unwrap_or_else(|| drag_preview.get().unwrap_or(0.));
            drag_preview.set(Some(amount));
            if drag_last_seek.get().elapsed() >= Duration::from_millis(100) {
                player
                    .media
                    .seek((player.media.duration() as f64 * amount) as i64);
                drag_last_seek.set(Instant::now());
            }
            if let Some(progress) = drag_progress.upgrade() {
                progress.queue_draw();
            }
        });
        let drag_stage = stage.downgrade();
        let drag_player = round_player.clone();
        let drag_preview = seek_preview.clone();
        let drag_seeking = seeking.clone();
        let drag_origin = seek_origin.clone();
        let drag_resume = resume_after_seek.clone();
        let drag_progress = progress.downgrade();
        drag.connect_drag_end(move |_, dx, dy| {
            if !drag_seeking.replace(false) {
                return;
            }
            let (Some(stage), Some(player)) =
                (drag_stage.upgrade(), drag_player.borrow().as_ref().cloned())
            else {
                return;
            };
            let (x, y) = drag_origin.get();
            let amount = round_seek_fraction(stage.upcast_ref(), x + dx, y + dy)
                .unwrap_or_else(|| drag_preview.get().unwrap_or(0.));
            drag_preview.set(Some(amount));
            player
                .media
                .seek((player.media.duration() as f64 * amount) as i64);
            if drag_resume.get() {
                player.media.play();
            }
            if let Some(progress) = drag_progress.upgrade() {
                progress.queue_draw();
            }
        });
        stage.add_controller(drag);

        let weak = Rc::downgrade(self);
        if !file.sticker {
            let image = image.downgrade();
            let key = file.key();
            button.connect_clicked(move |button| {
                if let Some(ui) = weak.upgrade() {
                    if image.upgrade().is_some_and(|image| image.is_circular()) {
                        let mut images = ui.images.borrow_mut();
                        if let Some(player) = images.video(&key) {
                            for (_, other) in &images.videos {
                                if !Rc::ptr_eq(other, &player) && !other.media.is_muted() {
                                    other.media.pause();
                                    other.media.set_muted(true);
                                    other.media.set_loop(true);
                                    other.inline_interactive.set(false);
                                }
                            }
                            let playing = player.media.is_muted() || !player.media.is_playing();
                            player.inline_interactive.set(true);
                            player.media.set_muted(false);
                            player.media.set_loop(false);
                            if player.media.is_ended() {
                                player.media.seek(0);
                            }
                            player.media.set_playing(playing);
                        }
                    } else {
                        ui.media_viewer(Some(button.clone().upcast()));
                    }
                }
            });
        }
        let mute_player = round_player.clone();
        let mute_weak = Rc::downgrade(self);
        mute.connect_clicked(move |button| {
            let Some(player) = mute_player.borrow().as_ref().cloned() else {
                return;
            };
            let muted = !player.media.is_muted();
            if !muted && let Some(ui) = mute_weak.upgrade() {
                let images = ui.images.borrow_mut();
                for (_, other) in &images.videos {
                    if !Rc::ptr_eq(other, &player) && !other.media.is_muted() {
                        other.media.pause();
                        other.media.set_muted(true);
                        other.media.set_loop(true);
                        other.inline_interactive.set(false);
                    }
                }
            }
            player.inline_interactive.set(true);
            if !muted {
                player.media.set_loop(false);
            }
            player.media.set_muted(muted);
            update_round_mute_button(button, muted);
        });
        content.append(&stage);
        let weak = Rc::downgrade(self);
        let view = image.downgrade();
        let tile = button.downgrade();
        let fallback = controls.downgrade();
        let location = content.downgrade();
        let mut attached: Option<InlinePlayback> = None;
        let captionless = message.text.is_empty();
        let load = move || {
            let (Some(ui), Some(image), Some(button), Some(controls), Some(location)) = (
                weak.upgrade(),
                view.upgrade(),
                tile.upgrade(),
                fallback.upgrade(),
                location.upgrade(),
            ) else {
                return glib::ControlFlow::Break;
            };
            if !ui.attachment_is_current(&owner, &peer, &id, &file) {
                return glib::ControlFlow::Break;
            }
            if let Some(binding) = attached.as_ref()
                && binding.player.media.error().is_some()
            {
                controls.add_css_class("media-preview-error");
                controls.set_tooltip_text(Some("Video playback unavailable"));
                controls.set_visible(true);
                button.set_visible(false);
                attached.take();
                return glib::ControlFlow::Continue;
            }
            if let Some(binding) = attached.as_mut() {
                let player = &binding.player;
                if player.media.is_prepared() {
                    player.keep_preview_silent();
                    if !player.media.has_video() {
                        controls.add_css_class("media-preview-error");
                        controls.set_tooltip_text(Some("This attachment has no video track"));
                        controls.set_visible(true);
                        return glib::ControlFlow::Continue;
                    }
                    let width = player.media.intrinsic_width().max(0) as u32;
                    let height = player.media.intrinsic_height().max(0) as u32;
                    let round = file.round_video(width, height, player.media.duration());
                    if round && !image.is_circular() {
                        image.set_circular(true);
                        button.add_css_class("round-video");
                        button.set_tooltip_text(Some("Play or pause round video"));
                        if let Some(overlay) = controls.parent().and_downcast::<gtk::Overlay>() {
                            overlay.add_css_class("round-video");
                            if let Some(placeholder) = overlay.child().and_downcast::<AspectImage>()
                            {
                                placeholder.set_circular(true);
                            }
                        }
                        let mut parent = location.parent();
                        while let Some(widget) = parent {
                            if widget.has_css_class("message-bubble") {
                                if captionless {
                                    widget.add_css_class("round-video-bubble");
                                }
                                break;
                            }
                            parent = widget.parent();
                        }
                        round_player.replace(Some(player.clone()));
                        smooth_progress.anchor(player.media.timestamp());
                        progress.set_visible(true);
                        mute.set_visible(player.media.has_audio());
                        update_round_mute_button(&mute, player.media.is_muted());
                        let display = progress.downgrade();
                        let preview = seek_preview.clone();
                        let dragging = seeking.clone();
                        let smooth = smooth_progress.clone();
                        binding
                            .signals
                            .push(player.media.connect_timestamp_notify(move |media| {
                                if !dragging.get() && !media.is_seeking() {
                                    preview.set(None);
                                }
                                if let Some(display) = display.upgrade() {
                                    if preview.get().is_some() {
                                        smooth.anchor(media.timestamp());
                                    } else {
                                        smooth.sample(media);
                                    }
                                    smooth.start(&display, media);
                                    display.queue_draw();
                                }
                            }));
                        let display = progress.downgrade();
                        let smooth = smooth_progress.clone();
                        binding
                            .signals
                            .push(player.media.connect_playing_notify(move |media| {
                                if let Some(display) = display.upgrade() {
                                    smooth.anchor(media.timestamp());
                                    smooth.start(&display, media);
                                    display.queue_draw();
                                }
                            }));
                        let display = progress.downgrade();
                        let preview = seek_preview.clone();
                        let dragging = seeking.clone();
                        let smooth = smooth_progress.clone();
                        binding
                            .signals
                            .push(player.media.connect_seeking_notify(move |media| {
                                if !media.is_seeking() && !dragging.get() {
                                    preview.set(None);
                                }
                                smooth.anchor(media.timestamp());
                                if let Some(display) = display.upgrade() {
                                    smooth.start(&display, media);
                                    display.queue_draw();
                                }
                            }));
                        let button = mute.downgrade();
                        binding
                            .signals
                            .push(player.media.connect_muted_notify(move |media| {
                                if let Some(button) = button.upgrade() {
                                    update_round_mute_button(&button, media.is_muted());
                                }
                            }));
                    }
                    if !button.is_visible() {
                        image.set_paintable(Some(player.media.clone().upcast()));
                        button.set_visible(true);
                        reveal_media(&controls, &player.media);
                        if width > 0 && height > 0 {
                            let mime = if file.mime.starts_with("video/") {
                                file.mime.as_str()
                            } else {
                                "video/unknown"
                            };
                            ui.remember_media_metadata(
                                &owner, &peer, &id, &file, mime, width, height, true,
                            );
                        }
                    }
                    binding.visible(ui.in_chat_viewport(&location));
                }
                return glib::ControlFlow::Continue;
            }
            if !ui.in_chat_prefetch(&location) {
                return glib::ControlFlow::Continue;
            }
            if let Some(reason) = ui.images.borrow().video_failures.get(&file.key()) {
                controls.add_css_class("media-preview-error");
                controls.set_tooltip_text(Some(reason));
                controls.set_visible(true);
            }
            if let Some(path) = ui.cached_image_source(&file)
                && let Some(player) = ui.video(&file, &path)
            {
                attached = Some(InlinePlayback::new(player, &image));
            }
            glib::ControlFlow::Continue
        };
        let mut load = load;
        if load() == glib::ControlFlow::Continue {
            glib::timeout_add_local(Duration::from_millis(150), load);
        }
    }
}

#[cfg(test)]
mod container_tests {
    use super::*;

    #[test]
    fn only_binary_media_containers_are_played() {
        let mp4 = [&[0, 0, 0, 0x18][..], b"ftypmp42", &[0; 16]].concat();
        let quicktime = [&[0, 0, 0, 8][..], b"wide", &[0; 8]].concat();
        let webm = [0x1a, 0x45, 0xdf, 0xa3, 0x9f, 0x42, 0x86, 0x81, 0x01];
        let avi = [&b"RIFF"[..], &[0; 4], b"AVI LIST"].concat();
        let mut transport = vec![0u8; 400];
        transport[0] = 0x47;
        transport[188] = 0x47;
        for (name, bytes) in [("mp4", &mp4[..]), ("quicktime", &quicktime), ("webm", &webm), ("avi", &avi), ("ogg", b"OggS\0\x02"), ("flv", b"FLV\x01\x05"), ("ts", &transport)] {
            assert!(media_container(bytes), "{name} is a media container");
        }
        // Manifests and playlists make GStreamer fetch the URLs written inside them.
        for (name, bytes) in [
            ("hls", &b"#EXTM3U\n#EXT-X-VERSION:3\nhttp://127.0.0.1/a.ts\n"[..]),
            ("dash", b"<?xml version=\"1.0\"?><MPD xmlns=\"urn:mpeg:dash:schema:mpd:2011\">"),
            ("smooth", b"<SmoothStreamingMedia MajorVersion=\"2\">"),
            ("sdp", b"v=0\r\no=- 0 0 IN IP4 127.0.0.1\r\n"),
            ("asx", b"<asx version=\"3.0\"><entry><ref href=\"http://127.0.0.1/a\"/>"),
            ("html", b"<html><video src=http://127.0.0.1/>"),
            ("empty", b""),
            ("short", b"ftyp"),
        ] {
            assert!(!media_container(bytes), "{name} must not be played");
        }
    }
}
#[cfg(test)]
mod cold_playback_tests {
    use super::*;
    #[test]
    #[ignore = "requires isolated native display and media plugins"]
    fn first_pack_video_initializes_before_constructing_a_player() {
        gtk::init().unwrap();assert!(PLAYBACK_READY.get().is_none());
        let path=std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/reaction-video.mp4");
        let input=||VerifiedFile(std::fs::File::open(&path).unwrap());
        assert!(Playback::new(input()).is_none(),"Cold callers must not enter uninitialized Clapper");
        glib::MainContext::default().block_on(async {
            assert!(gio::spawn_blocking(initialize).await.unwrap());
            assert_eq!(unsafe { monogram_playback_network_blocked() }, 1, "HLS, DASH and network sources must stay disabled");
            // End to end: a manifest saved as an .mp4 must not make GStreamer contact the address inside it.
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            listener.set_nonblocking(true).unwrap();
            let port = listener.local_addr().unwrap().port();
            let dir = std::env::temp_dir().join(format!("monogram-manifest-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&dir).unwrap();
            let manifest = dir.join("not-a-video.mp4");
            std::fs::write(&manifest, format!("#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:4\n#EXTINF:4,\nhttp://127.0.0.1:{port}/probe.ts\n#EXT-X-ENDLIST\n")).unwrap();
            let uri = std::ffi::CString::new(format!("file://{}", manifest.display())).unwrap();
            gio::spawn_blocking(move || unsafe { monogram_playback_probe(uri.as_ptr(), 4000) }).await.unwrap();
            assert!(listener.accept().is_err(), "GStreamer followed a URL found inside a media file");
            let _ = std::fs::remove_dir_all(dir);
            let player=Playback::new(input()).expect("First sticker/video player must construct after worker initialization");
            player.media.play();glib::timeout_future(Duration::from_millis(300)).await;player.media.pause();
            assert!(player.media.error().is_none(),"Native media pipeline failed: {:?}",player.media.error());
        });
    }
}

#[cfg(test)]
mod video_audio_tests {
    use super::*;

    /// The videos with a sound track: what is common in chats (H.264 and AAC in MP4) and WebM.
    const FIXTURES: [&str; 2] = ["video-with-aac.mp4", "video-with-audio.webm"];

    /// A fresh player for a video-with-sound fixture.
    fn player_for(name: &str) -> Rc<Playback> {
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name);
        Playback::new(VerifiedFile(std::fs::File::open(path).unwrap())).expect("the player is built once playback is initialized")
    }
    fn player() -> Rc<Playback> {
        player_for(FIXTURES[1])
    }
    async fn wait(limit: Duration, done: impl Fn() -> bool) -> bool {
        let end = Instant::now() + limit;
        while Instant::now() < end {
            if done() {
                return true;
            }
            glib::timeout_future(Duration::from_millis(10)).await;
        }
        done()
    }

    /// A video with a sound track must say it has sound once it is ready. GTK silences a stream
    /// that was marked prepared without audio and never asks again, so a wrong answer here means
    /// a video that stays silent however often it is unmuted.
    #[test]
    #[ignore = "requires isolated native display and media plugins"]
    fn a_video_with_a_sound_track_reports_audio_whenever_it_is_prepared() {
        gtk::init().unwrap();
        glib::MainContext::default().block_on(async {
            assert!(gio::spawn_blocking(initialize).await.unwrap(), "playback initializes");
            for name in FIXTURES {
                let (mut silent, mut ready) = (0, 0);
                for round in 0..4 {
                    let player = player_for(name);
                    player.media.play();
                    if !wait(Duration::from_secs(25), || player.media.is_prepared()).await {
                        // The pipeline of a player occasionally never starts in the test setup;
                        // that is not what is being checked here, so it is not counted.
                        eprintln!("{name} round {round}: never became ready, not counted");
                        continue;
                    }
                    ready += 1;
                    if !player.media.has_audio() {
                        silent += 1;
                    }
                    assert!(player.media.has_video(), "{name} round {round}");
                    assert!(player.media.error().is_none(), "{name}: {:?}", player.media.error());
                    player.media.pause();
                    drop(player);
                    // Let the finished player's pipeline be taken down before the next one starts.
                    glib::timeout_future(Duration::from_millis(400)).await;
                }
                assert!(ready >= 2, "{name}: only {ready} of 4 players became ready");
                assert_eq!(silent, 0, "{name}: {silent} of {ready} players were prepared without audio and would stay silent");
            }
        });
    }

    /// Unmuting and changing the volume of a video with sound must reach the player and stay.
    #[test]
    #[ignore = "requires isolated native display and media plugins"]
    fn unmuting_and_the_volume_reach_the_player_of_a_video_with_sound() {
        gtk::init().unwrap();
        glib::MainContext::default().block_on(async {
            assert!(gio::spawn_blocking(initialize).await.unwrap());
            let player = player();
            assert!(player.media.is_muted(), "inline videos start silent");
            player.media.play();
            assert!(wait(Duration::from_secs(10), || player.media.is_prepared()).await);
            assert!(player.media.has_audio());
            // Unmute, as opening the viewer does.
            player.media.set_muted(false);
            glib::timeout_future(Duration::from_millis(400)).await;
            assert!(!player.media.is_muted(), "unmuting stuck");
            assert!((player.media.volume() - 1.0).abs() < 0.01, "full volume: {}", player.media.volume());
            // The volume follows what is asked, and the sound stays on.
            player.media.set_volume(0.4);
            glib::timeout_future(Duration::from_millis(400)).await;
            assert!((player.media.volume() - 0.4).abs() < 0.02, "{}", player.media.volume());
            assert!(!player.media.is_muted(), "changing the volume must not mute");
            // Muting and unmuting again, repeatedly, keeps working.
            for _ in 0..3 {
                player.media.set_muted(true);
                glib::timeout_future(Duration::from_millis(150)).await;
                assert!(player.media.is_muted());
                player.media.set_muted(false);
                glib::timeout_future(Duration::from_millis(150)).await;
                assert!(!player.media.is_muted());
            }
            assert!(player.media.error().is_none(), "the pipeline did not fail: {:?}", player.media.error());
            player.media.pause();
        });
    }

    /// The report: "the app has muted itself, and if I unmute it, it keeps muting". The chat tile
    /// polls every 150 ms; something (the viewer, or the system mixer) unmutes the player; the tile
    /// must leave it alone.
    #[test]
    #[ignore = "requires isolated native display and media plugins"]
    fn a_player_someone_unmuted_is_not_muted_again_by_the_chat_tile_behind_it() {
        gtk::init().unwrap();
        glib::MainContext::default().block_on(async {
            assert!(gio::spawn_blocking(initialize).await.unwrap());
            let player = player();
            assert!(player.media.is_muted(), "a preview starts silent");
            player.media.play();
            assert!(wait(Duration::from_secs(10), || player.media.is_prepared()).await);
            // The tile's own timer.
            let tile = player.clone();
            let poll = glib::timeout_add_local(Duration::from_millis(150), move || {
                tile.keep_preview_silent();
                glib::ControlFlow::Continue
            });
            glib::timeout_future(Duration::from_millis(2500)).await;
            assert!(player.media.is_muted(), "still silent while only previewing");
            // Someone listens: the viewer unmutes it, or the person does in the system mixer.
            player.media.set_muted(false);
            glib::timeout_future(Duration::from_millis(1200)).await;
            poll.remove();
            assert!(!player.media.is_muted(), "the tile behind muted it again");
            player.media.pause();
        });
    }

    /// The settling window and the exemptions, with chosen clock readings.
    #[test]
    #[ignore = "requires isolated native display and media plugins"]
    fn previews_are_kept_silent_only_while_settling_and_never_while_someone_listens() {
        gtk::init().unwrap();
        glib::MainContext::default().block_on(async {
            assert!(gio::spawn_blocking(initialize).await.unwrap());
            let start = Instant::now();
            let at = |ms: u64| start + Duration::from_millis(ms);
            // Inside the window an unmuted preview is muted again (Clapper's late initial state).
            let preview = player();
            preview.keep_preview_silent_at(at(0));
            preview.media.set_muted(false);
            preview.keep_preview_silent_at(at(1000));
            assert!(preview.media.is_muted(), "inside the window it is put back");
            preview.media.set_muted(false);
            preview.keep_preview_silent_at(at(1500));
            assert!(preview.media.is_muted(), "the window includes its last instant");
            // After it, never again, however often it is polled.
            preview.media.set_muted(false);
            for ms in [1501, 1700, 5000, 60_000, 3_600_000] {
                preview.keep_preview_silent_at(at(ms));
                assert!(!preview.media.is_muted(), "left alone at {ms} ms");
            }
            // The window runs from the first look, and a clock that goes backwards is harmless.
            let late = player();
            late.keep_preview_silent_at(at(10_000));
            late.media.set_muted(false);
            late.keep_preview_silent_at(at(11_000));
            assert!(late.media.is_muted());
            late.media.set_muted(false);
            late.keep_preview_silent_at(at(9_000));
            assert!(late.media.is_muted(), "an earlier reading counts as no time having passed");
            // Someone listening: never muted, even inside the window.
            for shown in [true, false] {
                let heard = player();
                heard.keep_preview_silent_at(at(0));
                heard.media.set_muted(false);
                if shown {
                    heard.expanded.set(true); // the viewer is showing it
                } else {
                    heard.inline_interactive.set(true); // a round video was tapped
                }
                heard.keep_preview_silent_at(at(100));
                assert!(!heard.media.is_muted(), "listened to (viewer: {shown})");
                // When the viewer is closed the app mutes it itself, as it always did.
                heard.expanded.set(false);
                heard.inline_interactive.set(false);
            }
        });
    }
}
