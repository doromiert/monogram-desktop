//! The "is typing" indicator: who it names, the sentence, and the animated dots.
//!
//! Direct chats say `typing...` (the chat already is that person). Groups name the people:
//! `Alice is typing...`, `Alice and Bob are typing...`, `Alice, Bob and Carol are typing...`,
//! `Alice, Bob and 2 others are typing...`. The same text is the header subtitle and the preview
//! of the conversation in the sidebar, and its three dots pulse while it is shown.
use crate::model::{Chat, MessagePreview};
use crate::ui::Ui;
use gtk::{glib, pango, prelude::*};
use std::f64::consts::TAU;
use std::rc::Rc;
use std::time::Instant;

/// Every typing sentence ends with these; they are the part that animates.
const DOTS: &str = "...";
const TICK: &str = "monogram-typing-pulse";
/// How often the dots are redrawn: smooth enough for a slow pulse, light enough to be free.
const STEP: std::time::Duration = std::time::Duration::from_millis(66);
/// One full pulse of a dot, and how far behind the next dot follows.
const PERIOD: f64 = 1.2;
const LAG: f64 = 0.18;

/// What to say for the people typing in a conversation, oldest typist first.
pub fn sentence(group: bool, names: &[String]) -> String {
    if !group {
        return format!("typing{DOTS}");
    }
    match names {
        [] => format!("Someone is typing{DOTS}"),
        [a] => format!("{a} is typing{DOTS}"),
        [a, b] => format!("{a} and {b} are typing{DOTS}"),
        [a, b, c] => format!("{a}, {b} and {c} are typing{DOTS}"),
        [a, b, rest @ ..] => format!("{a}, {b} and {} others are typing{DOTS}", rest.len()),
    }
}

/// The nickname of a room occupant (`room@host/Nick`), or the whole address if it has none.
fn occupant_name(sender: &str) -> &str {
    sender.split_once('/').map(|(_, nick)| nick).filter(|nick| !nick.is_empty()).unwrap_or(sender)
}

/// The conversation's normal one-line preview: the draft if there is one, else the last message.
pub fn preview_text(chat: &Chat, latest: Option<&MessagePreview>) -> String {
    if !chat.draft.is_empty() {
        format!("Draft: {}", chat.draft.replace('\n', " "))
    } else {
        latest
            .map(|m| format!("{}{}", if m.own { "You: " } else { "" }, m.text.replace('\n', " ")))
            .unwrap_or_else(|| "No messages yet".into())
    }
}

impl Ui {
    /// Who is typing in `chat` right now, each person once, the one who began first leading.
    /// In a direct chat that is the contact however many of their devices are typing.
    pub(crate) fn typing_names(&self, chat: &Chat) -> Vec<String> {
        let typing = self.typing.borrow();
        let mut who: Vec<(Instant, &str)> = typing
            .iter()
            .filter(|((peer, _), at)| *peer == chat.jid && at.elapsed() < crate::chat_states::TIMEOUT)
            .map(|((_, sender), at)| (*at, sender.as_str()))
            .collect();
        if who.is_empty() {
            return Vec::new();
        }
        if !chat.group {
            return vec![chat.display_name().to_owned()];
        }
        who.sort();
        let mut names: Vec<String> = Vec::new();
        for (_, sender) in who {
            let name = occupant_name(sender);
            if !names.iter().any(|seen| seen == name) {
                names.push(name.to_owned());
            }
        }
        names
    }

    /// The typing sentence for `chat`, or `None` when nobody is typing there.
    pub(crate) fn typing_text(&self, chat: &Chat) -> Option<String> {
        let names = self.typing_names(chat);
        (!names.is_empty()).then(|| sentence(chat.group, &names))
    }

    /// Update the sidebar rows in place. A rebuild of the whole list for every start or stop of
    /// typing would cost far more than the one label that changes.
    pub(crate) fn update_typing_rows(self: &Rc<Self>) {
        let Some(list) = self.chat_stack.visible_child().and_downcast::<gtk::ListBox>() else {
            return;
        };
        let state = self.state.borrow();
        let mut child = list.first_child();
        while let Some(widget) = child {
            child = widget.next_sibling();
            let Some(row) = widget.downcast_ref::<crate::components::ChatRow>() else {
                continue;
            };
            let id = row.widget_name();
            let Some(chat) = state.chats.iter().find(|chat| chat.id == id.as_str()) else {
                continue;
            };
            let typing = self.typing_text(chat);
            let shown = typing.clone().unwrap_or_else(|| preview_text(chat, chat.latest_preview().as_ref()));
            row.set_preview(shown.as_str());
            row.set_typing(typing.is_some());
        }
    }
}

/// Start or stop the pulsing of the trailing dots of `label`. A timer drives it, only while a
/// typing sentence is on screen: a frame-clock tick would wake and repaint the whole window
/// sixty times a second for a few pixels of dots.
pub fn animate(label: &gtk::Label, on: bool) {
    // SAFETY: the key is only ever used with this one type, here.
    let running = unsafe { label.data::<glib::SourceId>(TICK).is_some() };
    if on && !running {
        let weak = label.downgrade();
        let id = glib::timeout_add_local(STEP, move || {
            let Some(label) = weak.upgrade() else { return glib::ControlFlow::Break };
            pulse(&label, glib::monotonic_time());
            glib::ControlFlow::Continue
        });
        unsafe { label.set_data(TICK, id) };
        pulse(label, glib::monotonic_time());
    } else if !on && running {
        if let Some(id) = unsafe { label.steal_data::<glib::SourceId>(TICK) } {
            id.remove();
        }
        label.set_attributes(None);
    }
}

/// Whether `label` is currently pulsing its dots.
#[cfg(test)]
fn animating(label: &gtk::Label) -> bool {
    unsafe { label.data::<glib::SourceId>(TICK).is_some() }
}

/// Set how opaque each of the last three characters is for this moment. Without animations
/// (the desktop's setting) the dots simply stay solid.
fn pulse(label: &gtk::Label, micros: i64) {
    let text = label.text();
    if !text.ends_with(DOTS) || !label.settings().is_gtk_enable_animations() {
        label.set_attributes(None);
        return;
    }
    let attributes = pango::AttrList::new();
    let first = text.len() - DOTS.len();
    for dot in 0..DOTS.len() {
        let mut attribute = pango::AttrInt::new_foreground_alpha(dot_alpha(micros as f64 / 1e6, dot));
        attribute.set_start_index((first + dot) as u32);
        attribute.set_end_index((first + dot + 1) as u32);
        attributes.insert(attribute);
    }
    label.set_attributes(Some(&attributes));
}

/// How opaque dot `dot` is `seconds` into the animation: a wave that runs along the three dots.
pub fn dot_alpha(seconds: f64, dot: usize) -> u16 {
    let wave = 0.5 + 0.5 * ((seconds / PERIOD - dot as f64 * LAG) * TAU).sin();
    ((0.3 + 0.7 * wave) * f64::from(u16::MAX)) as u16
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|name| name.to_string()).collect()
    }

    #[test]
    fn direct_chats_say_typing_and_groups_name_the_people() {
        assert_eq!(sentence(false, &names(&["Bob"])), "typing...");
        assert_eq!(sentence(true, &names(&[])), "Someone is typing...");
        assert_eq!(sentence(true, &names(&["Alice"])), "Alice is typing...");
        assert_eq!(sentence(true, &names(&["Alice", "Bob"])), "Alice and Bob are typing...");
        assert_eq!(sentence(true, &names(&["Alice", "Bob", "Carol"])), "Alice, Bob and Carol are typing...");
        assert_eq!(sentence(true, &names(&["Alice", "Bob", "Carol", "Dan"])), "Alice, Bob and 2 others are typing...");
        assert_eq!(sentence(true, &names(&["A", "B", "C", "D", "E", "F"])), "A, B and 4 others are typing...");
    }

    #[test]
    fn every_sentence_ends_in_the_dots_that_animate() {
        for group in [false, true] {
            for count in 0..6 {
                let list: Vec<String> = (0..count).map(|n| format!("P{n}")).collect();
                assert!(sentence(group, &list).ends_with(DOTS));
            }
        }
    }

    #[test]
    fn a_room_occupant_is_named_by_its_nickname() {
        assert_eq!(occupant_name("room@conference.example.org/Alice"), "Alice");
        assert_eq!(occupant_name("room@conference.example.org/Alice B/C"), "Alice B/C");
        assert_eq!(occupant_name("room@conference.example.org/"), "room@conference.example.org/");
        assert_eq!(occupant_name("bob@example.org"), "bob@example.org");
    }

    #[test]
    fn the_dots_pulse_one_after_another_and_never_vanish() {
        let samples: Vec<[u16; 3]> = (0..120).map(|step| {
            let t = f64::from(step) * 0.01;
            [dot_alpha(t, 0), dot_alpha(t, 1), dot_alpha(t, 2)]
        }).collect();
        for frame in &samples {
            assert!(frame.iter().all(|alpha| *alpha >= (0.29 * 65535.0) as u16), "a dot is never invisible: {frame:?}");
        }
        let lowest = samples.iter().map(|f| f[0]).min().unwrap();
        let highest = samples.iter().map(|f| f[0]).max().unwrap();
        assert!(highest - lowest > 30000, "a dot really pulses");
        assert!(samples.iter().any(|f| f[0] != f[1] && f[1] != f[2]), "the dots are out of step with each other");
    }

    fn rows(ui: &Rc<Ui>) -> Vec<crate::components::ChatRow> {
        let list = ui.chat_stack.visible_child().and_downcast::<gtk::ListBox>().expect("the chat list");
        let mut found = Vec::new();
        let mut child = list.first_child();
        while let Some(widget) = child {
            child = widget.next_sibling();
            if let Ok(row) = widget.downcast::<crate::components::ChatRow>() {
                found.push(row);
            }
        }
        found
    }

    fn snippet(row: &crate::components::ChatRow) -> gtk::Label {
        fn find(widget: &gtk::Widget) -> Option<gtk::Label> {
            if let Some(label) = widget.downcast_ref::<gtk::Label>().filter(|l| l.has_css_class("chat-preview")) {
                return Some(label.clone());
            }
            let mut child = widget.first_child();
            while let Some(next) = child {
                child = next.next_sibling();
                if let Some(found) = find(&next) {
                    return Some(found);
                }
            }
            None
        }
        find(row.upcast_ref()).expect("the preview label")
    }

    #[test]
    #[ignore = "requires a private GTK display and D-Bus"]
    fn the_sidebar_names_who_is_typing_updates_in_place_and_only_animates_while_it_shows() {
        use crate::model::{Account, State};
        use std::time::Duration;
        let app = adw::Application::builder().application_id("com.negzero.MonogramTypingRows").flags(gtk::gio::ApplicationFlags::NON_UNIQUE).build();
        app.register(None::<&gtk::gio::Cancellable>).unwrap();
        let ui = Ui::new(&app, true);
        let (owner, friend, room) = ("alice@example.invalid", "bob@example.invalid", "room@conference.example.invalid");
        ui.store.save_account(&Account { jid: owner.into(), host: String::new(), port: 5222, direct_tls: false }).unwrap();
        let mut state = State::live(owner, vec![]);
        state.active = state.chat_for(friend).id.clone();
        let group = state.chat_for(room);
        group.group = true;
        group.room_joined = true;
        ui.activate_state(state);
        ui.window.set_default_size(1000, 600);
        ui.window.present();
        let settle = |ms: u64| {
            let end = std::time::Instant::now() + Duration::from_millis(ms);
            while std::time::Instant::now() < end {
                glib::MainContext::default().iteration(false);
                std::thread::sleep(Duration::from_millis(2));
            }
        };
        settle(300);
        let by_id = |ui: &Rc<Ui>, jid: &str| {
            let id = ui.state.borrow().chats.iter().find(|c| c.jid == jid).unwrap().id.clone();
            rows(ui).into_iter().find(|r| r.widget_name() == id.as_str()).expect("the row")
        };
        let (friend_row, room_row) = (by_id(&ui, friend), by_id(&ui, room));
        let resting = room_row.preview();
        let typing = |who: &str, peer: &str, composing: bool| ui.backend_event(ui.generation.get(), owner, crate::protocol::BackendEvent::Typing { peer: peer.into(), sender: who.into(), composing });
        // Names in a group; the contact's own chat just says typing.
        typing(&format!("{room}/Alice"), room, true);
        typing(&format!("{friend}/phone"), friend, true);
        typing(&format!("{friend}/laptop"), friend, true);
        assert_eq!(room_row.preview(), "Alice is typing...");
        assert_eq!(friend_row.preview(), "typing...", "one contact on two devices is still one person");
        typing(&format!("{room}/Bob"), room, true);
        assert_eq!(room_row.preview(), "Alice and Bob are typing...");
        typing(&format!("{room}/Carol"), room, true);
        typing(&format!("{room}/Dan"), room, true);
        assert_eq!(room_row.preview(), "Alice, Bob and 2 others are typing...");
        // The rows were updated where they are, not rebuilt.
        assert!(by_id(&ui, room) == room_row && by_id(&ui, friend) == friend_row, "typing must not rebuild the chat list");
        // Styled and animated while shown.
        let label = snippet(&room_row);
        assert!(label.has_css_class("typing") && animating(&label));
        settle(250);
        // The first step of the animation comes within a few frames.
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        while label.attributes().is_none() && std::time::Instant::now() < deadline {
            settle(50);
        }
        let dots = label.attributes().map(|a| a.to_string()).unwrap_or_default();
        assert_eq!(dots.matches("foreground-alpha").count(), 3, "the three trailing dots carry their own opacity: {dots:?}");

        // Everyone stops: back to the normal preview, no styling, no ticking.
        for who in ["Alice", "Bob", "Carol", "Dan"] {
            typing(&format!("{room}/{who}"), room, false);
        }
        assert_eq!(room_row.preview(), resting);
        assert!(!label.has_css_class("typing") && !animating(&label) && label.attributes().is_none());
        // A full rebuild keeps showing it for whoever is still typing.
        typing(&format!("{room}/Eve"), room, true);
        ui.refresh_chats(None);
        settle(200);
        assert_eq!(by_id(&ui, room).preview(), "Eve is typing...");
        ui.window.close();
    }

    #[test]
    #[ignore = "requires a private GTK display and D-Bus"]
    fn composing_is_sent_once_until_the_state_changes() {
        use crate::protocol::Command;
        let app = adw::Application::builder().application_id("com.negzero.MonogramTypingSend").flags(gtk::gio::ApplicationFlags::NON_UNIQUE).build();
        app.register(None::<&gtk::gio::Cancellable>).unwrap();
        let ui = Ui::new(&app, true);
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        ui.session.replace(Some(crate::protocol::Session::fixture_channel(tx)));
        ui.online.set(true);
        let mut sent = || {
            let mut states = Vec::new();
            while let Ok(command) = rx.try_recv() {
                if let Command::Typing { composing, .. } = command {
                    states.push(composing);
                }
            }
            states
        };
        for _ in 0..4 {
            ui.notify_typing(true);
        }
        assert_eq!(sent(), vec![true], "typing on and on says composing once");
        // Even after a long time of typing, nothing is repeated.
        ui.typing_input.borrow_mut().as_mut().unwrap().1 = std::time::Instant::now() - std::time::Duration::from_secs(60);
        ui.notify_typing(true);
        assert!(sent().is_empty());
        ui.notify_typing(false);
        assert_eq!(sent(), vec![false]);
        ui.notify_typing(false);
        assert!(sent().is_empty(), "paused is not repeated either");
        ui.notify_typing(true);
        assert_eq!(sent(), vec![true], "typing again after a pause says composing again");
    }
}
