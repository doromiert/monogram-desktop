//! The microphone button and the recording bar.
//!
//! Hold the microphone button next to the emoji button to record a voice message; let go to send
//! it. While recording, the rest of the message box becomes a recording bar: a waveform in the
//! accent colour that moves with your voice, and the time recorded as `00:00`. Slide away from the
//! button (to the left) and let go to cancel, or press Escape. People who cannot hold a button
//! press Enter or Space on the focused button to start, and again to send.
//!
//! Anything that could surprise is kept safe rather than sent: a recording that ends any way
//! other than a clean release is put in the message box as an attachment to review, and one that
//! cannot be sent where it was made (you changed conversation) is deleted with a note.
use crate::{
    ui::Ui,
    voice::{
        meter,
        recorder::{self, Event, Failure, Recorder, Settings, Source},
        wave::VoiceWave,
    },
    widgets::{bx, icon_button, label},
};
use adw::prelude::*;
use gtk::{gdk, gio, glib};
use std::{
    cell::{Cell, RefCell},
    path::PathBuf,
    rc::Rc,
    time::{Duration, Instant},
};

/// Held for less than this, a press was an accident, not a recording.
pub const TAP: Duration = Duration::from_millis(500);
/// A recording shorter than this is thrown away: it is a click, not a message.
pub const MIN_LENGTH: Duration = Duration::from_secs(1);
/// Sliding this far from where the press began arms "cancel": the microphone turns into a trash
/// can that follows the pointer. Any drift shorter than this leaves it in its slot.
pub const SLIDE_TO_CANCEL: f64 = 20.0;
/// The red ring around the armed trash can (`.voice-chip.armed` in style.css, in pixels). The can
/// stops this far inside the hint so the ring ends where the waveform does.
const CHIP_HALO: f64 = 7.0;

/// How a press of the button works.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// The pointer or finger is down: letting go ends it.
    Held,
    /// Started from the keyboard: Enter or Space again ends it.
    Toggled,
}

/// Why a recording is not being sent as it is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Why {
    Cancelled,
    Tap,
    TooShort,
    /// The conversation or account changed, or the window closed.
    Moved,
    Limit,
    Interrupted,
}

/// What the person asked for when the recording ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Send,
    /// Keep it, in the message box, for the person to review.
    Draft(Why),
    Discard(Why),
}

/// What is actually done with the finished file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Send,
    /// Put it in the message box.
    Stage(Why),
    /// Delete it, with this reason.
    Delete(Why),
}

/// The rule for a finished recording. `here` is whether the conversation (and account) it was
/// made in is still the open one.
#[cfg(test)]
pub fn action_for(outcome: Outcome, length: Duration, here: bool) -> Action {
    action_with_minimum(outcome, length, here, MIN_LENGTH)
}

/// [`action_for`] with the shortest worthwhile recording given.
pub fn action_with_minimum(outcome: Outcome, length: Duration, here: bool, minimum: Duration) -> Action {
    match outcome {
        Outcome::Discard(why) => Action::Delete(why),
        _ if !here => Action::Delete(Why::Moved),
        _ if length < minimum => Action::Delete(Why::TooShort),
        Outcome::Send => Action::Send,
        Outcome::Draft(why) => Action::Stage(why),
    }
}

/// What to tell the person, if anything.
pub fn notice_for(action: Action) -> Option<&'static str> {
    match action {
        Action::Send => None,
        Action::Stage(Why::Limit) => Some("Recording limit reached. Review the voice message and send it when ready"),
        Action::Stage(_) => Some("The microphone stopped. What was recorded is in your message"),
        Action::Delete(Why::Tap) => Some("Hold the microphone button to record a voice message"),
        Action::Delete(Why::TooShort) => Some("The recording was too short to send"),
        Action::Delete(Why::Moved) => Some("Voice message discarded because the conversation changed"),
        Action::Delete(_) => None,
    }
}

/// Whether to refuse because the desktop's privacy settings turned the microphone off.
pub fn microphone_off(setting: Option<bool>) -> bool {
    setting == Some(true)
}

/// A file name for a voice message that sorts by time and cannot collide.
pub fn voice_name(now: chrono::DateTime<chrono::Local>, unique: &str) -> String {
    format!("voice-{}-{}.ogg", now.format("%Y%m%d-%H%M%S"), &unique[..unique.len().min(6)])
}

/// Whether a drag of `dx` pixels from the press is far enough, towards the start of the line, to
/// cancel. The start of the line is the left, or the right in right-to-left languages.
pub fn slid_to_cancel(dx: f64, rtl: bool) -> bool {
    let towards_start = if rtl { dx } else { -dx };
    towards_start >= SLIDE_TO_CANCEL
}

/// The desktop's own switch for the microphone (GNOME Settings → Privacy), if there is one.
fn system_microphone_setting() -> Option<bool> {
    let schema = gio::SettingsSchemaSource::default()?.lookup("org.gnome.desktop.privacy", true)?;
    schema.has_key("disable-microphone").then(|| {
        gio::Settings::new_full(&schema, None::<&gio::SettingsBackend>, None).boolean("disable-microphone")
    })
}

#[cfg(test)]
thread_local! {
    /// Tests replace the microphone with GStreamer test sources, a short length limit, or a
    /// privacy setting.
    pub(crate) static TEST_SOURCE: RefCell<Option<Vec<String>>> = const { RefCell::new(None) };
    pub(crate) static TEST_MAX: Cell<Option<Duration>> = const { Cell::new(None) };
    pub(crate) static TEST_MIN: Cell<Option<Duration>> = const { Cell::new(None) };
    pub(crate) static TEST_PRIVACY: Cell<Option<bool>> = const { Cell::new(None) };
}
#[cfg(test)]
fn min_length() -> Duration {
    TEST_MIN.with(Cell::get).unwrap_or(MIN_LENGTH)
}
#[cfg(not(test))]
fn min_length() -> Duration {
    MIN_LENGTH
}
#[cfg(test)]
fn source() -> Source {
    TEST_SOURCE.with(|s| s.borrow().clone()).map_or(Source::Microphone, Source::Candidates)
}
#[cfg(not(test))]
fn source() -> Source {
    Source::Microphone
}
#[cfg(test)]
fn max_length() -> Duration {
    TEST_MAX.with(Cell::get).unwrap_or(recorder::MAX_LENGTH)
}
#[cfg(not(test))]
fn max_length() -> Duration {
    recorder::MAX_LENGTH
}
#[cfg(test)]
fn privacy_setting() -> Option<bool> {
    TEST_PRIVACY.with(Cell::get).or_else(|| Some(false))
}
#[cfg(not(test))]
fn privacy_setting() -> Option<bool> {
    system_microphone_setting()
}

/// One recording, from the press to the end of its file.
struct Session {
    recorder: RefCell<Option<Recorder>>,
    path: PathBuf,
    owner: String,
    chat: String,
    generation: u64,
    started: Instant,
    mode: Mode,
    outcome: Cell<Option<Outcome>>,
    last_second: Cell<u64>,
}

/// The widgets that make way for the recording bar, and where the bar goes.
pub struct Parts {
    pub hide: Vec<gtk::Widget>,
    pub after: gtk::Widget,
}

pub struct VoiceUi {
    pub(crate) mic: gtk::Button,
    bar: gtk::Box,
    wave: VoiceWave,
    timer: gtk::Label,
    hint: gtk::Label,
    cancel: gtk::Button,
    /// The microphone glyph inside the button. Sliding to cancel carries it to the left and
    /// turns it into a trash can on red.
    chip: gtk::Box,
    chip_host: gtk::Fixed,
    chip_icons: gtk::Stack,
    parts: Parts,
    saved: RefCell<Vec<(gtk::Widget, bool)>>,
    current: RefCell<Option<Rc<Session>>>,
    finishing: RefCell<Vec<Rc<Session>>>,
    sliding: Cell<bool>,
}

/// Put the microphone button after the emoji button and build the recording bar.
pub fn install(ui: &Rc<Ui>, row: &gtk::Box, parts: Parts) -> Rc<VoiceUi> {
    // The same button as the emoji one: a flat, symbolic icon from the GNOME icon set.
    let mic = icon_button("audio-input-microphone-symbolic", "Hold to record a voice message");
    mic.update_property(&[gtk::accessible::Property::Label("Record voice message")]);
    mic.add_css_class("voice-button");
    let (chip_host, chip, chip_icons) = (gtk::Fixed::new(), bx(false, 0), gtk::Stack::new());
    chip.add_css_class("voice-chip");
    chip_icons.set_transition_type(gtk::StackTransitionType::Crossfade);
    chip_icons.set_transition_duration(180);
    for (icon, name) in [("audio-input-microphone-symbolic", "mic"), ("user-trash-symbolic", "trash")] {
        let image = gtk::Image::from_icon_name(icon);
        image.set_pixel_size(16);
        chip_icons.add_named(&image, Some(name));
    }
    chip.append(&chip_icons);
    // Exactly the size of the emoji button's icon, whatever the chip is doing.
    chip.set_size_request(16, 16);
    chip.set_halign(gtk::Align::Start);
    chip.set_valign(gtk::Align::Start);
    chip_host.set_size_request(16, 16);
    chip_host.set_halign(gtk::Align::Center);
    chip_host.set_valign(gtk::Align::Center);
    chip_host.put(&chip, 0.0, 0.0);
    // The microphone leaves its button when carried to the left, so nothing may clip it.
    for widget in [mic.upcast_ref::<gtk::Widget>(), chip_host.upcast_ref()] {
        widget.set_overflow(gtk::Overflow::Visible);
    }
    mic.set_child(Some(&chip_host));
    let wave = VoiceWave::new();
    let timer = label("00:00", "numeric voice-timer");
    timer.set_valign(gtk::Align::Center);
    let dot = gtk::Image::from_icon_name("media-record-symbolic");
    dot.add_css_class("voice-dot");
    dot.set_valign(gtk::Align::Center);
    dot.set_accessible_role(gtk::AccessibleRole::Presentation);
    let cancel = icon_button("user-trash-symbolic", "Cancel recording · Esc");
    cancel.update_property(&[gtk::accessible::Property::Label("Cancel recording")]);
    let hint = label("Release to send · slide left to cancel", "caption dim-label voice-hint");
    hint.set_valign(gtk::Align::Center);
    hint.set_ellipsize(gtk::pango::EllipsizeMode::End);
    hint.set_xalign(1.0);
    let bar = bx(false, 8);
    bar.add_css_class("voice-bar");
    bar.set_hexpand(true);
    bar.set_valign(gtk::Align::Center);
    bar.set_visible(false);
    for child in [cancel.upcast_ref::<gtk::Widget>(), dot.upcast_ref(), timer.upcast_ref(), wave.upcast_ref(), hint.upcast_ref()] {
        bar.append(child);
    }
    row.insert_child_after(&bar, Some(&parts.after));
    row.append(&mic);

    let voice = Rc::new(VoiceUi {
        mic: mic.clone(),
        bar,
        wave,
        timer,
        hint,
        cancel: cancel.clone(),
        chip,
        chip_host,
        chip_icons,
        parts,
        saved: RefCell::new(Vec::new()),
        current: RefCell::new(None),
        finishing: RefCell::new(Vec::new()),
        sliding: Cell::new(false),
    });

    // Press and hold, on mouse or touch. Claiming the sequence keeps the button from also
    // "clicking" when the finger lifts.
    let gesture = gtk::GestureDrag::builder().button(gdk::BUTTON_PRIMARY).propagation_phase(gtk::PropagationPhase::Capture).build();
    let (weak_ui, weak_voice) = (Rc::downgrade(ui), Rc::downgrade(&voice));
    gesture.connect_drag_begin({
        let (weak_ui, weak_voice) = (weak_ui.clone(), weak_voice.clone());
        move |gesture, _, _| {
            gesture.set_state(gtk::EventSequenceState::Claimed);
            if let (Some(ui), Some(voice)) = (weak_ui.upgrade(), weak_voice.upgrade()) {
                voice.press(&ui, Mode::Held);
            }
        }
    });
    gesture.connect_drag_update({
        let weak_voice = weak_voice.clone();
        move |gesture, dx, _| {
            if let Some(voice) = weak_voice.upgrade() {
                let rtl = gesture.widget().is_some_and(|widget| widget.direction() == gtk::TextDirection::Rtl);
                voice.carry(dx, rtl);
                voice.slide(slid_to_cancel(dx, rtl));
            }
        }
    });
    gesture.connect_drag_end({
        let (weak_ui, weak_voice) = (weak_ui.clone(), weak_voice.clone());
        move |gesture, dx, _| {
            if let (Some(ui), Some(voice)) = (weak_ui.upgrade(), weak_voice.upgrade()) {
                let rtl = gesture.widget().is_some_and(|widget| widget.direction() == gtk::TextDirection::Rtl);
                voice.release(&ui, slid_to_cancel(dx, rtl));
            }
        }
    });
    // The system took the pointer away (another window grabbed it): end the recording and keep it.
    gesture.connect_cancel({
        let (weak_ui, weak_voice) = (weak_ui.clone(), weak_voice.clone());
        move |_, _| {
            if let (Some(ui), Some(voice)) = (weak_ui.upgrade(), weak_voice.upgrade()) {
                voice.interrupted_by_system(&ui);
            }
        }
    });
    mic.add_controller(gesture);

    // Keyboard: the button is only "clicked" this way, because the pointer's press was claimed.
    let (weak_ui, weak_voice) = (Rc::downgrade(ui), Rc::downgrade(&voice));
    mic.connect_clicked(move |_| {
        if let (Some(ui), Some(voice)) = (weak_ui.upgrade(), weak_voice.upgrade()) {
            voice.toggle(&ui);
        }
    });
    let (weak_ui, weak_voice) = (Rc::downgrade(ui), Rc::downgrade(&voice));
    cancel.connect_clicked(move |_| {
        if let (Some(ui), Some(voice)) = (weak_ui.upgrade(), weak_voice.upgrade()) {
            voice.cancel_current(&ui, Why::Cancelled);
        }
    });
    // Escape cancels, wherever the focus is while recording.
    let keys = gtk::EventControllerKey::new();
    keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    let (weak_ui, weak_voice) = (Rc::downgrade(ui), Rc::downgrade(&voice));
    keys.connect_key_pressed(move |_, key, _, _| {
        if key == gdk::Key::Escape {
            if let (Some(ui), Some(voice)) = (weak_ui.upgrade(), weak_voice.upgrade()) {
                if voice.recording() {
                    voice.cancel_current(&ui, Why::Cancelled);
                    return glib::Propagation::Stop;
                }
            }
        }
        glib::Propagation::Proceed
    });
    ui.window.add_controller(keys);
    // Closing the window throws recordings away; nothing is left running.
    let (weak_ui, weak_voice) = (Rc::downgrade(ui), Rc::downgrade(&voice));
    ui.window.connect_close_request(move |_| {
        if let (Some(ui), Some(voice)) = (weak_ui.upgrade(), weak_voice.upgrade()) {
            voice.cancel_all(&ui);
        }
        glib::Propagation::Proceed
    });
    voice.refresh(ui);
    voice
}

impl VoiceUi {
    pub fn recording(&self) -> bool {
        self.current.borrow().is_some()
    }

    /// The button works only when there is an account to send from.
    pub fn refresh(&self, ui: &Ui) {
        let live = !ui.state.borrow().account.is_empty();
        if self.recording() {
            return;
        }
        self.mic.set_sensitive(live);
        self.mic.set_tooltip_text(Some(if live { "Hold to record a voice message" } else { "Sign in to send voice messages" }));
    }

    /// The button went down (or the key was pressed): start recording.
    pub(crate) fn press(self: &Rc<Self>, ui: &Rc<Ui>, mode: Mode) {
        if self.recording() || !self.mic.is_sensitive() {
            return;
        }
        let (owner, chat, generation) = {
            let state = ui.state.borrow();
            (state.account.clone(), state.active.clone(), ui.generation.get())
        };
        if owner.is_empty() {
            return;
        }
        if microphone_off(privacy_setting()) {
            ui.notice("The microphone is turned off in your privacy settings");
            return;
        }
        let Some(dir) = crate::picture_editor_ui::temporary_copies_dir() else {
            ui.notice("The voice message could not be recorded");
            return;
        };
        let path = dir.join(voice_name(chrono::Local::now(), &uuid::Uuid::new_v4().simple().to_string()));
        let session = Rc::new(Session {
            recorder: RefCell::new(None),
            path: path.clone(),
            owner,
            chat,
            generation,
            started: Instant::now(),
            mode,
            outcome: Cell::new(None),
            last_second: Cell::new(0),
        });
        let (weak_ui, weak_voice, weak_session) = (Rc::downgrade(ui), Rc::downgrade(self), Rc::downgrade(&session));
        let started = Recorder::start(source(), &path, Settings { max_length: max_length() }, move |event| {
            if let (Some(ui), Some(voice), Some(session)) = (weak_ui.upgrade(), weak_voice.upgrade(), weak_session.upgrade()) {
                voice.on_event(&ui, &session, event);
            }
        });
        match started {
            Ok(recorder) => {
                session.recorder.replace(Some(recorder));
            }
            Err(failure) => {
                ui.notice(&failure_text(&failure));
                return;
            }
        }
        self.current.replace(Some(session));
        self.show_recording(mode);
        // Screen readers hear it start; sighted people see the bar.
        self.timer.announce("Recording voice message", gtk::AccessibleAnnouncementPriority::Medium);
    }

    /// The microphone stays in its slot until the pointer has moved far enough to arm "cancel";
    /// from then on the trash can it became is carried under the pointer, as far as the start of
    /// the hint text.
    pub(crate) fn carry(&self, dx: f64, rtl: bool) {
        if !self.recording() {
            return;
        }
        let towards_start = if rtl { dx } else { -dx };
        let travel = if towards_start < SLIDE_TO_CANCEL { 0.0 } else { towards_start.min(self.room_to_carry()) };
        self.shift_chip(if rtl { travel } else { -travel });
        // The hint makes way for the trash can once it is on its way.
        self.hint.set_opacity(if travel > 0.0 { 0.0 } else { 1.0 });
    }

    /// How far the chip can be carried towards the start of the line: to where the hint text
    /// begins, so the trash can ends up exactly at the edge of the text it replaced and never
    /// over the waveform.
    fn room_to_carry(&self) -> f64 {
        let (Some(mic), Some(hint)) = (self.mic.compute_bounds(&self.bar), self.hint.compute_bounds(&self.bar)) else {
            return f64::MAX;
        };
        if mic.width() <= 0.0 || hint.width() <= 0.0 {
            return f64::MAX;
        }
        let centre = f64::from(mic.x() + mic.width() / 2.0);
        let reach = if self.mic.direction() == gtk::TextDirection::Rtl {
            f64::from(hint.x() + hint.width()) - CHIP_HALO - centre
        } else {
            centre - (f64::from(hint.x()) + CHIP_HALO)
        };
        reach.max(0.0)
    }

    /// The pointer moved far enough from the press to arm, or disarm, "cancel".
    pub(crate) fn slide(&self, armed: bool) {
        if !self.recording() || self.sliding.replace(armed) == armed {
            return;
        }
        self.chip_icons.set_visible_child_name(if armed { "trash" } else { "mic" });
        if armed {
            self.chip.add_css_class("armed");
        } else {
            self.chip.remove_css_class("armed");
        }
        // The whole message box warns, and the slot the microphone left is not shown empty.
        if let Some(composer) = self.composer() {
            if armed {
                composer.add_css_class("voice-cancelling");
            } else {
                composer.remove_css_class("voice-cancelling");
            }
        }
        self.hint.set_label(if armed { "Release to cancel" } else { "Release to send · slide left to cancel" });
        if armed {
            self.hint.remove_css_class("dim-label");
            self.hint.add_css_class("error");
            self.cancel.add_css_class("destructive-action");
        } else {
            self.hint.add_css_class("dim-label");
            self.hint.remove_css_class("error");
            self.cancel.remove_css_class("destructive-action");
        }
    }

    /// The button came up. `slid`: far enough away to mean cancel.
    pub(crate) fn release(self: &Rc<Self>, ui: &Rc<Ui>, slid: bool) {
        let Some(session) = self.current.borrow().clone() else { return };
        if session.mode != Mode::Held {
            return;
        }
        let outcome = if slid {
            Outcome::Discard(Why::Cancelled)
        } else if session.started.elapsed() < TAP {
            Outcome::Discard(Why::Tap)
        } else {
            Outcome::Send
        };
        self.end(ui, &session, outcome);
    }

    /// Enter or Space: start, and then send.
    pub(crate) fn toggle(self: &Rc<Self>, ui: &Rc<Ui>) {
        let current = self.current.borrow().clone();
        match current {
            None => self.press(ui, Mode::Toggled),
            Some(session) if session.mode == Mode::Toggled => self.end(ui, &session, Outcome::Send),
            Some(_) => {}
        }
    }

    pub(crate) fn cancel_current(self: &Rc<Self>, ui: &Rc<Ui>, why: Why) {
        let session = self.current.borrow().clone();
        if let Some(session) = session {
            self.end(ui, &session, Outcome::Discard(why));
        }
    }

    /// The pointer was taken from us mid-press: stop, and keep the recording for review.
    fn interrupted_by_system(self: &Rc<Self>, ui: &Rc<Ui>) {
        let session = self.current.borrow().clone();
        if let Some(session) = session.filter(|s| s.mode == Mode::Held) {
            self.end(ui, &session, Outcome::Draft(Why::Interrupted));
        }
    }

    /// Throw everything away: the conversation changed, or the window is closing.
    pub(crate) fn cancel_all(self: &Rc<Self>, ui: &Rc<Ui>) {
        self.cancel_current(ui, Why::Moved);
        let finishing: Vec<_> = self.finishing.borrow_mut().drain(..).collect();
        for session in finishing {
            if let Some(recorder) = session.recorder.borrow().as_ref() {
                recorder.cancel();
            }
            crate::picture_editor_ui::discard_edited(Some(session.path.clone()));
        }
    }

    /// Stop recording now and decide what becomes of it.
    fn end(self: &Rc<Self>, ui: &Rc<Ui>, session: &Rc<Session>, outcome: Outcome) {
        self.current.borrow_mut().take();
        session.outcome.set(Some(outcome));
        self.show_idle();
        match outcome {
            Outcome::Discard(why) => {
                if let Some(recorder) = session.recorder.borrow().as_ref() {
                    recorder.cancel();
                }
                crate::picture_editor_ui::discard_edited(Some(session.path.clone()));
                if let Some(text) = notice_for(Action::Delete(why)) {
                    ui.notice(text);
                }
            }
            Outcome::Send | Outcome::Draft(_) => {
                self.finishing.borrow_mut().push(session.clone());
                if let Some(recorder) = session.recorder.borrow().as_ref() {
                    recorder.stop();
                }
            }
        }
    }

    fn on_event(self: &Rc<Self>, ui: &Rc<Ui>, session: &Rc<Session>, event: Event) {
        let is_current = self.current.borrow().as_ref().is_some_and(|current| Rc::ptr_eq(current, session));
        match event {
            Event::Level { bar, at } => {
                if is_current {
                    self.wave.push(bar);
                    // The text changes once a second, not twenty times.
                    if at.as_secs() != session.last_second.get() || at.is_zero() {
                        session.last_second.set(at.as_secs());
                        self.timer.set_label(&meter::timer_text(at));
                    }
                }
            }
            Event::Limit => {
                if is_current {
                    self.end(ui, session, Outcome::Draft(Why::Limit));
                }
            }
            Event::Finished { length } => self.complete(ui, session, length),
            Event::Interrupted { failure, length } => {
                if is_current {
                    // Still held: the microphone died under the finger. Keep what there is.
                    self.current.borrow_mut().take();
                    session.outcome.set(Some(Outcome::Draft(Why::Interrupted)));
                    self.show_idle();
                    ui.notice(&failure_text(&failure));
                }
                self.complete(ui, session, length);
            }
            Event::Failed(failure) => {
                if is_current {
                    self.current.borrow_mut().take();
                    self.show_idle();
                }
                crate::picture_editor_ui::discard_edited(Some(session.path.clone()));
                ui.notice(&failure_text(&failure));
            }
        }
    }

    /// The file is closed: do what was decided.
    fn complete(self: &Rc<Self>, ui: &Rc<Ui>, session: &Rc<Session>, length: Duration) {
        self.finishing.borrow_mut().retain(|s| !Rc::ptr_eq(s, session));
        let outcome = session.outcome.get().unwrap_or(Outcome::Discard(Why::Cancelled));
        let here = {
            let state = ui.state.borrow();
            state.account == session.owner && state.active == session.chat && ui.generation.get() == session.generation
        };
        let action = action_with_minimum(outcome, length, here, min_length());
        let file = gio::File::for_path(&session.path);
        // Keep explicit recorder intent if the user must review this file before sending.
        unsafe { file.set_data("monogram-voice-message", true); }
        match action {
            Action::Send => {
                if !ui.queue_voice_message(file.clone()) {
                    // It could not be queued (a blocked contact, a full queue): never lose it.
                    if ui.stage_attachment_files(vec![file]) {
                        ui.notice("The voice message could not be sent yet. It is in your message");
                    } else {
                        crate::picture_editor_ui::discard_edited(Some(session.path.clone()));
                        ui.notice("The voice message could not be sent");
                    }
                }
            }
            Action::Stage(_) => {
                if !ui.stage_attachment_files(vec![file]) {
                    crate::picture_editor_ui::discard_edited(Some(session.path.clone()));
                    ui.notice("The voice message could not be added to your message");
                    return;
                }
            }
            Action::Delete(_) => crate::picture_editor_ui::discard_edited(Some(session.path.clone())),
        }
        if let Some(text) = notice_for(action) {
            ui.notice(text);
        }
    }

    fn show_recording(&self, mode: Mode) {
        self.wave.clear();
        self.timer.set_label("00:00");
        self.sliding.set(false);
        self.hint.set_label(match mode {
            Mode::Held => "Release to send · slide left to cancel",
            Mode::Toggled => "Press Enter to send · Esc to cancel",
        });
        self.hint.add_css_class("dim-label");
        self.hint.remove_css_class("error");
        self.cancel.remove_css_class("destructive-action");
        // The hint swaps between texts of different lengths. Holding its width at the longest
        // one, as drawn, keeps the waveform beside it from growing and shrinking as you slide.
        let widest = ["Release to send · slide left to cancel", "Release to cancel"]
            .iter()
            .map(|text| self.hint.create_pango_layout(Some(text)).pixel_size().0)
            .max()
            .unwrap_or(-1);
        self.hint.set_size_request(widest, -1);
        self.reset_chip();
        // While the button is held, the microphone itself becomes the way to cancel, so the trash
        // button only shows without a held button (tap to record). It keeps its place either way,
        // so the waveform never moves.
        let shown = mode == Mode::Toggled;
        self.cancel.set_opacity(if shown { 1.0 } else { 0.0 });
        self.cancel.set_can_target(shown);
        self.cancel.set_focusable(shown);
        self.cancel.update_state(&[gtk::accessible::State::Hidden(!shown)]);
        {
            let mut saved = self.saved.borrow_mut();
            saved.clear();
            for widget in &self.parts.hide {
                saved.push((widget.clone(), widget.is_visible()));
                widget.set_visible(false);
            }
        }
        self.bar.set_visible(true);
        // The whole message box turns accent coloured; the microphone stays a flat button in it.
        if let Some(composer) = self.composer() {
            composer.add_css_class("voice-recording");
        }
        self.mic.set_tooltip_text(Some(match mode {
            Mode::Held => "Release to send",
            Mode::Toggled => "Send voice message",
        }));
    }

    /// The message box the microphone button sits in.
    fn composer(&self) -> Option<gtk::Widget> {
        self.mic.parent()?.parent()
    }

    fn reset_chip(&self) {
        self.chip.remove_css_class("armed");
        if let Some(composer) = self.composer() {
            composer.remove_css_class("voice-cancelling");
        }
        self.chip_icons.set_visible_child_name("mic");
        self.shift_chip(0.0);
        self.hint.set_opacity(1.0);
    }

    fn shift_chip(&self, x: f64) {
        let to = gtk::graphene::Point::new(x as f32, 0.0);
        self.chip_host.set_child_transform(&self.chip, Some(&gtk::gsk::Transform::new().translate(&to)));
    }

    #[cfg(test)]
    fn chip_shift(&self) -> f64 {
        self.chip_host.child_transform(&self.chip).map_or(0.0, |t| f64::from(t.to_translate().0))
    }

    fn show_idle(&self) {
        self.bar.set_visible(false);
        for (widget, was_visible) in self.saved.borrow_mut().drain(..) {
            widget.set_visible(was_visible);
        }
        self.reset_chip();
        if let Some(composer) = self.composer() {
            composer.remove_css_class("voice-recording");
        }
        self.mic.set_tooltip_text(Some("Hold to record a voice message"));
        self.sliding.set(false);
    }
}

/// What to tell the person when the microphone could not be used, with the way out.
pub fn failure_text(failure: &Failure) -> String {
    match failure {
        Failure::Denied => "Microphone access was refused. Allow it in your system’s sound or privacy settings".into(),
        Failure::NoMicrophone => "No microphone was found. Connect one and try again".into(),
        Failure::Busy => "The microphone is in use by another program".into(),
        Failure::Other(text) => format!("Recording failed: {text}"),
    }
}

impl Ui {
    /// The conversation changed or the account was switched.
    pub(crate) fn voice_cancel(self: &Rc<Self>) {
        let voice = self.voice.borrow().clone();
        if let Some(voice) = voice {
            voice.cancel_all(self);
        }
    }
    pub(crate) fn voice_refresh(&self) {
        let voice = self.voice.borrow().clone();
        if let Some(voice) = voice {
            voice.refresh(self);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Account, Message, State};
    use std::os::unix::fs::PermissionsExt;

    // ---- the rules: no window needed ----------------------------------------------------

    const LONG: Duration = Duration::from_secs(5);

    #[test]
    fn a_clean_release_of_a_real_recording_is_sent() {
        assert_eq!(action_for(Outcome::Send, LONG, true), Action::Send);
        assert_eq!(action_for(Outcome::Send, MIN_LENGTH, true), Action::Send, "exactly the minimum is enough");
        assert_eq!(action_for(Outcome::Send, MIN_LENGTH - Duration::from_millis(1), true), Action::Delete(Why::TooShort));
        assert_eq!(action_for(Outcome::Send, Duration::ZERO, true), Action::Delete(Why::TooShort));
    }

    #[test]
    fn anything_but_a_clean_release_is_kept_for_review_not_sent() {
        for why in [Why::Limit, Why::Interrupted] {
            assert_eq!(action_for(Outcome::Draft(why), LONG, true), Action::Stage(why));
            assert_eq!(action_for(Outcome::Draft(why), Duration::from_millis(300), true), Action::Delete(Why::TooShort), "a fragment is not worth keeping");
        }
    }

    #[test]
    fn a_cancel_or_a_tap_always_deletes_whatever_the_length() {
        for why in [Why::Cancelled, Why::Tap, Why::Moved] {
            for length in [Duration::ZERO, LONG, Duration::from_secs(3600)] {
                assert_eq!(action_for(Outcome::Discard(why), length, true), Action::Delete(why));
                assert_eq!(action_for(Outcome::Discard(why), length, false), Action::Delete(why));
            }
        }
    }

    #[test]
    fn a_recording_never_goes_to_a_conversation_it_was_not_made_in() {
        assert_eq!(action_for(Outcome::Send, LONG, false), Action::Delete(Why::Moved));
        assert_eq!(action_for(Outcome::Draft(Why::Limit), LONG, false), Action::Delete(Why::Moved));
        assert_eq!(action_for(Outcome::Draft(Why::Interrupted), LONG, false), Action::Delete(Why::Moved));
    }

    #[test]
    fn what_is_said_matches_what_happened() {
        assert_eq!(notice_for(Action::Send), None);
        assert_eq!(notice_for(Action::Delete(Why::Cancelled)), None, "cancelling is silent: you did it");
        assert!(notice_for(Action::Delete(Why::Tap)).unwrap().contains("Hold the microphone button"));
        assert!(notice_for(Action::Delete(Why::TooShort)).unwrap().contains("too short"));
        assert!(notice_for(Action::Delete(Why::Moved)).unwrap().contains("conversation changed"));
        assert!(notice_for(Action::Stage(Why::Limit)).unwrap().contains("limit"));
        assert!(notice_for(Action::Stage(Why::Interrupted)).unwrap().contains("in your message"));
        for action in [Action::Delete(Why::Tap), Action::Delete(Why::TooShort), Action::Delete(Why::Moved), Action::Stage(Why::Limit), Action::Stage(Why::Interrupted)] {
            let text = notice_for(action).unwrap();
            assert!(!text.ends_with('.'), "toasts have no full stop: {text}");
        }
    }

    #[test]
    fn sliding_far_enough_towards_the_start_of_the_line_cancels() {
        assert!(!slid_to_cancel(0.0, false));
        assert!(!slid_to_cancel(-SLIDE_TO_CANCEL + 1.0, false));
        assert!(slid_to_cancel(-SLIDE_TO_CANCEL, false));
        assert!(slid_to_cancel(-500.0, false));
        assert!(!slid_to_cancel(500.0, false), "away from the start is not a cancel");
        // Mirrored for right-to-left languages.
        assert!(slid_to_cancel(SLIDE_TO_CANCEL, true));
        assert!(!slid_to_cancel(-500.0, true));
        assert!(!slid_to_cancel(f64::NAN, false) && !slid_to_cancel(f64::NAN, true));
    }

    #[test]
    fn the_system_microphone_switch_only_blocks_when_it_is_on() {
        assert!(microphone_off(Some(true)));
        assert!(!microphone_off(Some(false)));
        assert!(!microphone_off(None), "no such setting on this desktop: allowed");
    }

    #[test]
    fn voice_files_are_named_by_time_and_are_safe_ogg_names_the_app_accepts() {
        use chrono::TimeZone;
        let early = chrono::Local.with_ymd_and_hms(2025, 1, 2, 3, 4, 5).unwrap();
        let late = chrono::Local.with_ymd_and_hms(2025, 11, 22, 19, 30, 59).unwrap();
        let (a, b) = (voice_name(early, "abcdef123"), voice_name(late, "abcdef123"));
        assert_eq!(a, "voice-20250102-030405-abcdef.ogg");
        assert!(a < b);
        assert_ne!(voice_name(early, "aaaaaa"), voice_name(early, "bbbbbb"));
        assert_eq!(voice_name(early, "ab"), "voice-20250102-030405-ab.ogg");
        assert_eq!(crate::audio_check::extension_container(&a), Some(crate::audio_check::Container::Ogg));
        assert!(!a.contains(['/', ' ', ':', '\\']));
    }

    #[test]
    fn every_failure_says_what_to_do_about_it() {
        assert!(failure_text(&Failure::Denied).contains("settings"));
        assert!(failure_text(&Failure::NoMicrophone).contains("Connect one"));
        assert!(failure_text(&Failure::Busy).contains("another program"));
        assert!(failure_text(&Failure::Other("boom".into())).contains("boom"));
        for failure in [Failure::Denied, Failure::NoMicrophone, Failure::Busy, Failure::Other("x".into())] {
            assert!(!failure_text(&failure).ends_with('.'));
        }
    }

    // ---- the real interface ---------------------------------------------------------------

    const OWNER: &str = "alice@example.invalid";
    const PEER: &str = "bob@example.invalid";
    const OTHER: &str = "carol@example.invalid";
    const TONE: &str = "audiotestsrc is-live=true wave=sine freq=330 volume=0.7";

    fn ui() -> Rc<Ui> {
        adw::init().unwrap();
        let app = adw::Application::builder().application_id("com.negzero.VoiceFixture").flags(gio::ApplicationFlags::NON_UNIQUE).build();
        app.register(None::<&gio::Cancellable>).unwrap();
        let ui = Ui::new(&app, true);
        ui.window.set_default_size(1000, 700);
        ui.window.present();
        TEST_SOURCE.with(|s| *s.borrow_mut() = Some(vec![TONE.into()]));
        ui
    }
    fn live(ui: &Rc<Ui>) {
        ui.store.save_account(&Account { jid: OWNER.into(), host: String::new(), port: 5222, direct_tls: false }).unwrap();
        let mut state = State::live(OWNER, vec![]);
        for peer in [PEER, OTHER] {
            let chat = state.chat_for(peer);
            chat.messages = vec![Message { id: format!("hi-{peer}"), text: "hello".into(), author: "Them".into(), sender: peer.into(), time: "12:00".into(), ..Default::default() }];
        }
        state.active = state.chat_for(PEER).id.clone();
        ui.store.save(&state).unwrap();
        ui.activate_state(state);
        ui.show_main();
        settle(300);
    }
    fn settle(ms: u64) {
        let context = glib::MainContext::default();
        let end = Instant::now() + Duration::from_millis(ms);
        while Instant::now() < end {
            context.iteration(false);
            std::thread::sleep(Duration::from_millis(2));
        }
    }
    fn wait_until(limit: Duration, done: impl Fn() -> bool) -> bool {
        let context = glib::MainContext::default();
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
    /// Let the window finish its own start-up work (laying out, first drawing, background loading):
    /// until a whole second goes by with no pause in the main loop over 60 ms. In the software
    /// rendered test setup that first work alone stalls the loop for seconds, recording or not.
    fn warm_up() {
        let context = glib::MainContext::default();
        let give_up = Instant::now() + Duration::from_secs(30);
        while Instant::now() < give_up {
            let window = Instant::now();
            let mut last = Instant::now();
            let mut longest = Duration::ZERO;
            while window.elapsed() < Duration::from_secs(1) {
                context.iteration(false);
                longest = longest.max(last.elapsed());
                last = Instant::now();
                std::thread::sleep(Duration::from_millis(1));
            }
            if longest < Duration::from_millis(60) {
                return;
            }
        }
    }
    fn voice(ui: &Rc<Ui>) -> Rc<VoiceUi> {
        ui.voice.borrow().clone().expect("the voice controls are installed")
    }
    fn descendants(root: &gtk::Widget, found: &mut Vec<gtk::Widget>) {
        found.push(root.clone());
        let mut child = root.first_child();
        while let Some(widget) = child {
            child = widget.next_sibling();
            descendants(&widget, found);
        }
    }
    fn labels(ui: &Rc<Ui>) -> Vec<String> {
        let mut all = vec![];
        descendants(ui.toast.upcast_ref(), &mut all);
        all.iter().filter_map(|w| w.downcast_ref::<gtk::Label>().map(|l| l.label().to_string())).collect()
    }
    fn says(ui: &Rc<Ui>, text: &str) -> bool {
        labels(ui).iter().any(|label| label.contains(text))
    }
    fn gesture(ui: &Rc<Ui>) -> gtk::GestureDrag {
        let controllers = voice(ui).mic.observe_controllers();
        (0..controllers.n_items()).filter_map(|i| controllers.item(i)).find_map(|c| c.downcast::<gtk::GestureDrag>().ok()).expect("the hold gesture")
    }
    fn press(ui: &Rc<Ui>) {
        gesture(ui).emit_by_name::<()>("drag-begin", &[&0.0f64, &0.0f64]);
    }
    fn drag(ui: &Rc<Ui>, dx: f64) {
        gesture(ui).emit_by_name::<()>("drag-update", &[&dx, &0.0f64]);
    }
    fn let_go(ui: &Rc<Ui>, dx: f64) {
        gesture(ui).emit_by_name::<()>("drag-end", &[&dx, &0.0f64]);
    }
    fn files(ui: &Rc<Ui>) -> Vec<PathBuf> {
        let dir = crate::picture_editor_ui::temporary_copies_dir().unwrap();
        std::fs::read_dir(dir).unwrap().flatten().map(|e| e.path()).filter(|p| p.file_name().is_some_and(|n| n.to_string_lossy().starts_with("voice-"))).collect()
    }
    fn sent(ui: &Rc<Ui>) -> Vec<Message> {
        ui.state.borrow().current().messages.iter().filter(|m| m.upload.as_ref().is_some_and(|u| u.name.starts_with("voice-"))).cloned().collect()
    }
    fn composer_row(ui: &Rc<Ui>) -> gtk::Box {
        voice(ui).mic.parent().and_downcast::<gtk::Box>().unwrap()
    }
    fn timer(ui: &Rc<Ui>) -> String {
        voice(ui).timer.label().to_string()
    }
    fn recording_ui(ui: &Rc<Ui>) -> bool {
        voice(ui).bar.is_visible()
    }

    #[gtk::test]
    #[ignore = "requires a private GTK display and D-Bus"]
    fn the_microphone_button_follows_the_emoji_button_in_the_same_style() {
        let ui = ui();
        live(&ui);
        let row = composer_row(&ui);
        let mic = voice(&ui).mic.clone();
        let emoji = mic.prev_sibling().and_downcast::<gtk::Button>().expect("the emoji button is just before it");
        assert_eq!(emoji.icon_name().as_deref(), Some("face-smile-symbolic"));
        let glyph = voice(&ui).chip_icons.child_by_name("mic").and_downcast::<gtk::Image>().unwrap();
        assert_eq!(glyph.icon_name().as_deref(), Some("audio-input-microphone-symbolic"), "a GNOME icon");
        assert_eq!(voice(&ui).chip_icons.visible_child_name().as_deref(), Some("mic"));
        for class in ["flat"] {
            assert!(mic.has_css_class(class) && emoji.has_css_class(class), "{class}");
        }
        assert_eq!(mic.valign(), emoji.valign());
        assert!(mic.tooltip_text().unwrap().contains("Hold to record"));
        assert!(mic.is_sensitive() && mic.is_visible());
        assert_eq!(mic.parent().as_ref(), Some(row.upcast_ref()));
        // The recording bar sits in the same row, hidden until needed.
        assert!(!voice(&ui).bar.is_visible() && voice(&ui).bar.parent().as_ref() == Some(row.upcast_ref()));
        ui.window.close();
    }

    #[gtk::test]
    #[ignore = "requires a private GTK display and D-Bus"]
    fn without_an_account_the_button_is_off_and_says_why() {
        let ui = ui();
        let mic = voice(&ui).mic.clone();
        assert!(!mic.is_sensitive());
        assert!(mic.tooltip_text().unwrap().contains("Sign in"));
        press(&ui);
        assert!(!recording_ui(&ui) && files(&ui).is_empty());
        live(&ui);
        assert!(mic.is_sensitive(), "on once there is an account");
        ui.window.close();
    }

    #[gtk::test]
    #[ignore = "requires a private GTK display and D-Bus"]
    fn holding_shows_the_recording_bar_and_letting_go_sends_a_voice_message() {
        let ui = ui();
        live(&ui);
        ui.composer.buffer().set_text("a draft I am typing");
        let before = ui.composer_text();
        press(&ui);
        assert!(recording_ui(&ui), "the bar appears at once");
        let v = voice(&ui);
        // The message box makes way for it; the microphone button stays, now in the accent style.
        assert!(v.parts.hide.iter().all(|w| !w.is_visible()), "the rest of the box is out of the way");
        assert!(v.mic.is_visible() && v.mic.has_css_class("flat") && !v.mic.has_css_class("suggested-action"), "no blue circle");
        assert!(v.composer().unwrap().has_css_class("voice-recording"), "the box itself turns accent coloured");
        assert!(v.cancel.opacity() == 0.0 && !v.cancel.can_target(), "no trash button beside a held microphone");
        assert!(v.cancel.is_visible(), "but its place is kept, so the waveform does not move");
        assert_eq!(v.chip_shift(), 0.0, "the microphone sits exactly where it did before recording");
        assert_eq!(timer(&ui), "00:00");
        assert!(wait_until(Duration::from_secs(10), || timer(&ui) == "00:01"), "the timer counts: {}", timer(&ui));
        assert!(v.wave.len() > 10, "the waveform has been filling");
        assert!(v.timer.has_css_class("voice-timer") && v.wave.has_css_class("voice-wave"), "styled by the bar");
        let_go(&ui, 0.0);
        assert!(!recording_ui(&ui), "the box comes back as soon as the button is let go");
        assert!(v.parts.hide.iter().all(|w| w.is_visible() || w == &v.parts.hide[3]), "everything that was there is back");
        assert!(v.mic.has_css_class("flat") && !v.composer().unwrap().has_css_class("voice-recording"));
        assert!(wait_until(Duration::from_secs(10), || !sent(&ui).is_empty()), "the voice message was sent");
        let message = sent(&ui).remove(0);
        assert!(message.upload.as_ref().unwrap().voice, "recorded intent survives the durable outbox");
        let restored: crate::model::UploadDraft = serde_json::from_str(&serde_json::to_string(message.upload.as_ref().unwrap()).unwrap()).unwrap();
        assert!(restored.voice);
        let name = message.upload.as_ref().unwrap().name.clone();
        assert!(name.starts_with("voice-") && name.ends_with(".ogg"), "{name}");
        assert_eq!(ui.composer_text(), before, "the draft being typed was not touched");
        assert!(ui.attachments.borrow().is_empty());
        // The file is a private Ogg Opus the app's own checker accepts.
        let path = files(&ui).remove(0);
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(crate::audio_check::check(&name, &bytes[..bytes.len().min(65536)], bytes.len() as u64), Ok(crate::audio_check::Container::Ogg));
        ui.window.close();
    }

    #[gtk::test]
    #[ignore = "requires a private GTK display and D-Bus"]
    fn a_quick_tap_is_an_accident_not_a_recording() {
        let ui = ui();
        live(&ui);
        warm_up();
        press(&ui);
        assert!(recording_ui(&ui));
        let_go(&ui, 0.0);
        assert!(!recording_ui(&ui));
        assert!(wait_until(Duration::from_secs(5), || says(&ui, "Hold the microphone button")), "it explains how it works");
        settle(300);
        assert!(sent(&ui).is_empty() && files(&ui).is_empty(), "nothing sent, nothing left behind");
        assert!(ui.attachments.borrow().is_empty());
        ui.window.close();
    }

    #[gtk::test]
    #[ignore = "requires a private GTK display and D-Bus"]
    fn a_recording_held_past_the_tap_time_but_shorter_than_the_minimum_is_too_short() {
        let ui = ui();
        live(&ui);
        // Make "worth sending" longer than this test holds, so the outcome does not depend on timing.
        TEST_MIN.with(|m| m.set(Some(Duration::from_secs(60))));
        warm_up();
        press(&ui);
        assert!(wait_until(Duration::from_secs(10), || voice(&ui).wave.len() >= 4), "some sound arrived");
        settle(300);
        assert!(voice(&ui).current.borrow().as_ref().unwrap().started.elapsed() > TAP, "held past the tap time");
        let_go(&ui, 0.0);
        assert!(wait_until(Duration::from_secs(5), || says(&ui, "too short")), "{:?}", labels(&ui).iter().filter(|l| l.contains("short")).collect::<Vec<_>>());
        settle(300);
        assert!(sent(&ui).is_empty() && files(&ui).is_empty());
        ui.window.close();
    }

    #[gtk::test]
    #[ignore = "requires a private GTK display and D-Bus"]
    fn sliding_away_arms_cancel_and_letting_go_there_discards_but_sliding_back_still_sends() {
        let ui = ui();
        live(&ui);
        press(&ui);
        assert!(wait_until(Duration::from_secs(10), || timer(&ui) == "00:01"));
        let v = voice(&ui);
        assert!(v.hint.label().contains("slide left"));
        assert_eq!(v.chip_icons.visible_child_name().as_deref(), Some("mic"));
        // Drifting a little while holding leaves the microphone in its slot, hint and all.
        drag(&ui, -SLIDE_TO_CANCEL + 5.0);
        assert_eq!(v.chip_shift(), 0.0, "short of the cancel distance the microphone does not move");
        assert_eq!(v.hint.opacity(), 1.0);
        assert!(!v.chip.has_css_class("armed") && !v.composer().unwrap().has_css_class("voice-cancelling"));
        drag(&ui, -SLIDE_TO_CANCEL - 10.0);
        assert_eq!(v.chip_shift(), -SLIDE_TO_CANCEL - 10.0, "past it, the trash can is carried under the pointer");
        assert_eq!(v.chip_icons.visible_child_name().as_deref(), Some("trash"), "it became a trash can");
        assert!(v.chip.has_css_class("armed"), "on red");
        assert!(v.composer().unwrap().has_css_class("voice-cancelling"), "the whole message box turns red");
        assert_eq!(v.hint.label(), "Release to cancel");
        assert!(v.hint.has_css_class("error") && v.cancel.has_css_class("destructive-action"));
        drag(&ui, -60.0);
        assert_eq!(v.chip_shift(), -60.0, "and keeps following it, beyond the cancel distance");
        // Far past the hint it stops where the hint text begins, clear of the waveform.
        drag(&ui, -2000.0);
        settle(300);
        let (bar, chip) = (v.bar.clone(), v.chip.clone());
        let centre = |w: &gtk::Widget| { let b = w.compute_bounds(&bar).unwrap(); f64::from(b.x() + b.width() / 2.0) };
        let hint_start = f64::from(v.hint.compute_bounds(&bar).unwrap().x());
        assert!((centre(chip.upcast_ref()) - hint_start - CHIP_HALO).abs() <= 1.5, "the trash can stops at the start of the hint: {} vs {hint_start}", centre(chip.upcast_ref()));
        let wave = v.wave.compute_bounds(&bar).unwrap();
        assert!(f64::from(chip.compute_bounds(&bar).unwrap().x()) - CHIP_HALO >= f64::from(wave.x() + wave.width()) - 1.5, "and its ring never covers the waveform");
        // Back again: sending is possible again.
        drag(&ui, -10.0);
        assert_eq!(v.chip_icons.visible_child_name().as_deref(), Some("mic"), "the microphone returns");
        assert_eq!(v.chip_shift(), 0.0, "to its slot");
        assert!(!v.chip.has_css_class("armed") && !v.composer().unwrap().has_css_class("voice-cancelling"));
        assert!(v.hint.label().contains("Release to send") && !v.cancel.has_css_class("destructive-action"));
        drag(&ui, -SLIDE_TO_CANCEL - 10.0);
        let_go(&ui, -SLIDE_TO_CANCEL - 10.0);
        settle(600);
        assert!(sent(&ui).is_empty() && files(&ui).is_empty(), "cancelled: nothing sent, file deleted");
        assert!(!recording_ui(&ui));
        assert_eq!(v.chip_shift(), 0.0, "the microphone is back in its button");
        assert_eq!(v.chip_icons.visible_child_name().as_deref(), Some("mic"));
        // And the other way: slid out, slid back, then let go.
        press(&ui);
        assert!(wait_until(Duration::from_secs(10), || timer(&ui) == "00:01"));
        drag(&ui, -SLIDE_TO_CANCEL - 10.0);
        drag(&ui, -5.0);
        let_go(&ui, -5.0);
        assert!(wait_until(Duration::from_secs(10), || !sent(&ui).is_empty()), "sent after sliding back");
        ui.window.close();
    }

    #[gtk::test]
    #[ignore = "requires a private GTK display and D-Bus"]
    fn escape_and_the_bin_button_cancel_a_recording() {
        let ui = ui();
        live(&ui);
        for use_escape in [true, false] {
            press(&ui);
            assert!(wait_until(Duration::from_secs(10), || voice(&ui).wave.len() >= 5));
            if use_escape {
                let controllers = ui.window.observe_controllers();
                for i in 0..controllers.n_items() {
                    if let Some(keys) = controllers.item(i).and_downcast::<gtk::EventControllerKey>() {
                        let handled = keys.emit_by_name::<bool>("key-pressed", &[&gdk::Key::Escape, &0u32, &gdk::ModifierType::empty()]);
                        if handled {
                            break;
                        }
                    }
                }
            } else {
                voice(&ui).cancel.emit_clicked();
            }
            assert!(!recording_ui(&ui), "cancelled (escape: {use_escape})");
            // The press is still down when escape is used: letting go afterwards changes nothing.
            let_go(&ui, 0.0);
            settle(500);
            assert!(sent(&ui).is_empty() && files(&ui).is_empty(), "escape: {use_escape}");
        }
        ui.window.close();
    }

    #[gtk::test]
    #[ignore = "requires a private GTK display and D-Bus"]
    fn escape_does_nothing_when_not_recording() {
        let ui = ui();
        live(&ui);
        let controllers = ui.window.observe_controllers();
        for i in 0..controllers.n_items() {
            if let Some(keys) = controllers.item(i).and_downcast::<gtk::EventControllerKey>() {
                let ours = keys.emit_by_name::<bool>("key-pressed", &[&gdk::Key::Escape, &0u32, &gdk::ModifierType::empty()]);
                assert!(!ours || !voice(&ui).recording(), "escape is left for everything else when not recording");
            }
        }
        ui.window.close();
    }

    #[gtk::test]
    #[ignore = "requires a private GTK display and D-Bus"]
    fn the_keyboard_can_start_and_send_without_holding_anything() {
        let ui = ui();
        live(&ui);
        let v = voice(&ui);
        v.mic.emit_clicked();
        assert!(recording_ui(&ui));
        assert!(v.hint.label().contains("Enter"), "it says how to finish");
        // A pointer release means nothing in this mode.
        let_go(&ui, 0.0);
        assert!(recording_ui(&ui), "still recording");
        assert!(wait_until(Duration::from_secs(10), || timer(&ui) == "00:01"));
        v.mic.emit_clicked();
        assert!(!recording_ui(&ui));
        assert!(wait_until(Duration::from_secs(10), || !sent(&ui).is_empty()));
        // And Escape cancels this kind too.
        v.mic.emit_clicked();
        v.cancel.emit_clicked();
        settle(500);
        assert_eq!(sent(&ui).len(), 1);
        ui.window.close();
    }

    #[gtk::test]
    #[ignore = "requires a private GTK display and D-Bus"]
    fn pressing_again_while_recording_changes_nothing() {
        let ui = ui();
        live(&ui);
        press(&ui);
        press(&ui);
        press(&ui);
        voice(&ui).mic.emit_clicked();
        assert!(voice(&ui).recording(), "a held press is not toggled off by a click");
        assert!(wait_until(Duration::from_secs(10), || timer(&ui) == "00:01"));
        let_go(&ui, 0.0);
        assert!(wait_until(Duration::from_secs(10), || !sent(&ui).is_empty()));
        settle(300);
        assert_eq!(sent(&ui).len(), 1, "one recording, one message");
        ui.window.close();
    }

    #[gtk::test]
    #[ignore = "requires a private GTK display and D-Bus"]
    fn changing_conversation_while_recording_throws_the_recording_away() {
        let ui = ui();
        live(&ui);
        press(&ui);
        assert!(wait_until(Duration::from_secs(10), || timer(&ui) == "00:01"));
        let other = ui.state.borrow().chats.iter().find(|c| c.jid == OTHER).unwrap().id.clone();
        ui.select_chat(&other);
        assert!(!recording_ui(&ui) && !voice(&ui).recording(), "the bar is gone");
        let_go(&ui, 0.0);
        settle(600);
        assert!(sent(&ui).is_empty() && files(&ui).is_empty(), "not sent to either conversation");
        // Leaving a chat empties its in-memory list, so look at what was saved.
        let saved = ui.store.history(OWNER, PEER, None, 50);
        assert!(saved.iter().all(|m| !m.upload.as_ref().is_some_and(|u| u.name.starts_with("voice-"))), "the first conversation got nothing");
        ui.window.close();
    }

    #[gtk::test]
    #[ignore = "requires a private GTK display and D-Bus"]
    fn changing_conversation_right_after_letting_go_never_sends_to_the_wrong_one() {
        let ui = ui();
        live(&ui);
        press(&ui);
        assert!(wait_until(Duration::from_secs(10), || timer(&ui) == "00:01"));
        let_go(&ui, 0.0);
        // Before the file has even finished closing.
        let other = ui.state.borrow().chats.iter().find(|c| c.jid == OTHER).unwrap().id.clone();
        ui.select_chat(&other);
        settle(800);
        for chat in ui.state.borrow().chats.iter() {
            assert!(!chat.messages.iter().any(|m| m.upload.as_ref().is_some_and(|u| u.name.starts_with("voice-"))), "{}", chat.jid);
        }
        assert!(files(&ui).is_empty(), "and no file is left");
        ui.window.close();
    }

    #[gtk::test]
    #[ignore = "requires a private GTK display and D-Bus"]
    fn closing_the_window_leaves_no_recording_running_or_on_disk() {
        let ui = ui();
        live(&ui);
        press(&ui);
        assert!(wait_until(Duration::from_secs(10), || voice(&ui).wave.len() >= 5));
        ui.window.close();
        settle(500);
        assert!(!voice(&ui).recording() && files(&ui).is_empty());
    }

    #[gtk::test]
    #[ignore = "requires a private GTK display and D-Bus"]
    fn when_the_desktop_has_the_microphone_off_nothing_records_and_it_says_so() {
        let ui = ui();
        live(&ui);
        TEST_PRIVACY.with(|p| p.set(Some(true)));
        press(&ui);
        assert!(!recording_ui(&ui) && files(&ui).is_empty());
        assert!(wait_until(Duration::from_secs(5), || says(&ui, "turned off in your privacy settings")));
        TEST_PRIVACY.with(|p| p.set(Some(false)));
        press(&ui);
        assert!(recording_ui(&ui), "works again when allowed");
        let_go(&ui, -500.0);
        ui.window.close();
    }

    #[gtk::test]
    #[ignore = "requires a private GTK display and D-Bus"]
    fn a_missing_or_broken_microphone_says_so_and_leaves_the_box_as_it_was() {
        let ui = ui();
        live(&ui);
        TEST_SOURCE.with(|s| *s.borrow_mut() = Some(vec!["audiotestsrc ! identity error-after=1".into(), "no-such-element".into()]));
        press(&ui);
        assert!(wait_until(Duration::from_secs(10), || !voice(&ui).recording() && !recording_ui(&ui) && says(&ui, "Recording failed") || says(&ui, "No microphone")), "{:?}", labels(&ui).iter().filter(|l| l.contains("ecord") || l.contains("icrophone")).collect::<Vec<_>>());
        assert!(voice(&ui).parts.hide.iter().take(3).all(|w| w.is_visible()), "the message box is back");
        settle(300);
        assert!(files(&ui).is_empty() && sent(&ui).is_empty());
        // It can be tried again.
        TEST_SOURCE.with(|s| *s.borrow_mut() = Some(vec![TONE.into()]));
        press(&ui);
        assert!(recording_ui(&ui));
        let_go(&ui, -500.0);
        ui.window.close();
    }

    #[gtk::test]
    #[ignore = "requires a private GTK display and D-Bus"]
    fn hitting_the_length_limit_keeps_the_recording_for_review_instead_of_sending() {
        let ui = ui();
        live(&ui);
        TEST_MAX.with(|m| m.set(Some(Duration::from_millis(1200))));
        press(&ui);
        assert!(wait_until(Duration::from_secs(15), || !voice(&ui).recording()), "it stopped by itself");
        assert!(!recording_ui(&ui));
        assert!(wait_until(Duration::from_secs(10), || ui.attachments.borrow().len() == 1), "it is in the message box");
        assert!(says(&ui, "limit") || wait_until(Duration::from_secs(3), || says(&ui, "limit")));
        assert!(sent(&ui).is_empty(), "nothing was sent without being looked at");
        let_go(&ui, 0.0);
        settle(300);
        assert_eq!(ui.attachments.borrow().len(), 1, "letting go later changes nothing");
        let files=ui.attachments.borrow().clone();
        assert!(ui.queue_local_files(files,"",false,false));
        assert!(sent(&ui).iter().all(|m|m.upload.as_ref().is_some_and(|u|u.voice)));
        ui.window.close();
    }

    #[gtk::test]
    #[ignore = "requires a private GTK display and D-Bus"]
    fn a_microphone_that_dies_part_way_keeps_what_was_recorded_for_review() {
        let ui = ui();
        live(&ui);
        TEST_SOURCE.with(|s| *s.borrow_mut() = Some(vec!["audiotestsrc is-live=true volume=0.7 ! identity error-after=70".into()]));
        press(&ui);
        assert!(wait_until(Duration::from_secs(15), || ui.attachments.borrow().len() == 1), "kept: {:?}", labels(&ui).iter().filter(|l| l.contains("icrophone")).collect::<Vec<_>>());
        assert!(!recording_ui(&ui) && sent(&ui).is_empty());
        ui.window.close();
    }

    #[gtk::test]
    #[ignore = "requires a private GTK display and D-Bus"]
    fn recording_again_straight_after_sending_works_and_many_rapid_goes_leave_nothing_behind() {
        let ui = ui();
        live(&ui);
        for _ in 0..12 {
            press(&ui);
            let_go(&ui, 0.0);
        }
        // Each tap's microphone is opened in the background and then let go of again; wait for that.
        assert!(wait_until(Duration::from_secs(20), || files(&ui).is_empty() && voice(&ui).finishing.borrow().is_empty()), "left over: {:?}", files(&ui));
        assert!(!voice(&ui).recording() && sent(&ui).is_empty(), "twelve accidental taps left nothing");
        // A real one right after a send.
        press(&ui);
        assert!(wait_until(Duration::from_secs(10), || timer(&ui) == "00:01"));
        let_go(&ui, 0.0);
        press(&ui);
        assert!(recording_ui(&ui), "a new recording can start while the last one is being closed");
        assert!(wait_until(Duration::from_secs(10), || timer(&ui) == "00:01"));
        let_go(&ui, 0.0);
        assert!(wait_until(Duration::from_secs(10), || sent(&ui).len() == 2));
        ui.window.close();
    }

    #[gtk::test]
    #[ignore = "requires a private GTK display and D-Bus"]
    fn the_interface_stays_smooth_while_recording() {
        let ui = ui();
        live(&ui);
        warm_up();
        press(&ui);
        let started = Instant::now();
        let (mut longest, mut last) = (Duration::ZERO, Instant::now());
        let context = glib::MainContext::default();
        while started.elapsed() < Duration::from_secs(4) {
            context.iteration(false);
            let now = Instant::now();
            if started.elapsed() > Duration::from_secs(1) {
                longest = longest.max(now - last);
            }
            last = now;
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(voice(&ui).wave.len() > 40, "the waveform kept up: {}", voice(&ui).wave.len());
        assert!(longest < Duration::from_millis(40), "the longest pause between loop turns was {longest:?}");
        let_go(&ui, -500.0);
        ui.window.close();
    }
}
