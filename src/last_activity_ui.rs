//! XEP-0012 in the interface: "Last seen 15 minutes ago · Sat July 4 2015 23:25" in place of a
//! bare "Offline" in the conversation header and the profile card.
//!
//! Nothing is shown unless the account's server advertises `jabber:iq:last` and the contact's
//! server answers with a number of seconds. A `forbidden` answer (no presence subscription), any
//! other error, a timeout and a malformed reply all leave the plain "Offline" in place, and the
//! contact is not asked again for a few minutes.
use crate::{last_activity, ui::Ui};
use chrono::{DateTime, Utc};
use gtk::glib;
use std::{
    collections::HashMap,
    rc::Rc,
    time::{Duration, Instant},
};

/// An answer is reused this long; opening the same chat again inside it asks nothing.
const FRESH: Duration = Duration::from_secs(60);
/// After no usable answer the contact is left alone this long.
const RETRY: Duration = Duration::from_secs(300);
/// The server's support is checked again after this long.
const SUPPORT: Duration = Duration::from_secs(600);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Outcome {
    Asking,
    /// `None`: online now. `Some`: the moment they were last connected.
    Known(Option<DateTime<Utc>>),
    Unavailable,
}

#[derive(Default)]
pub struct Cache {
    /// Whether the account's server advertises the protocol, and when that was learned.
    support: Option<(bool, Instant)>,
    probing: bool,
    entries: HashMap<(String, String), (Outcome, Instant)>,
}

impl Cache {
    fn outcome(&self, owner: &str, peer: &str) -> Option<Outcome> {
        self.entries.get(&(owner.into(), peer.into())).map(|(outcome, _)| *outcome)
    }
    /// Whether to ask now, and if so, mark the question as under way.
    fn begin(&mut self, owner: &str, peer: &str, now: Instant) -> bool {
        let key = (owner.to_owned(), peer.to_owned());
        let due = match self.entries.get(&key) {
            None => true,
            Some((Outcome::Asking, started)) => now.duration_since(*started) > Duration::from_secs(40),
            Some((Outcome::Known(_), at)) => now.duration_since(*at) >= FRESH,
            Some((Outcome::Unavailable, at)) => now.duration_since(*at) >= RETRY,
        };
        if due {
            self.entries.insert(key, (Outcome::Asking, now));
        }
        due
    }
    fn finish(&mut self, owner: &str, peer: &str, outcome: Outcome, now: Instant) {
        self.entries.insert((owner.into(), peer.into()), (outcome, now));
    }
    fn supported(&self, now: Instant) -> Option<bool> {
        self.support.filter(|(_, at)| now.duration_since(*at) < SUPPORT).map(|(yes, _)| yes)
    }
}

/// A contact worth asking about: a plain account address, not a room or a resource.
fn askable(peer: &str) -> bool {
    peer.parse::<tokio_xmpp::jid::Jid>()
        .ok()
        .is_some_and(|jid| jid.node().is_some() && jid.resource().is_none() && jid.to_string() == peer)
}

impl Ui {
    /// The line for this contact, once the server has answered.
    pub(crate) fn last_seen_line(&self, peer: &str) -> Option<String> {
        if !self.shows_last_seen() {
            return None;
        }
        let owner = self.state.borrow().account.clone();
        match self.last_activity.borrow().outcome(&owner, peer)? {
            Outcome::Known(seen) => Some(last_activity::line(seen, Utc::now())),
            Outcome::Asking | Outcome::Unavailable => None,
        }
    }

    /// The text for a direct contact's status: the known presence, except that "Offline" gives
    /// way to when they were last seen.
    pub(crate) fn status_text(&self, peer: &str, base: &str) -> String {
        if base == "Online" && self.shows_contact_status() {
            // Away or do-not-disturb, and how long they have been idle (XEP-0319), when known.
            let detail = self.contact_status.borrow().get(peer).copied();
            let light = crate::contact_status::light(true, true, detail);
            return crate::contact_status::describe(light, detail, Utc::now());
        }
        if base == "Offline" || base.is_empty() {
            if let Some(line) = self.last_seen_line(peer) {
                return line;
            }
        }
        base.into()
    }

    /// The colour of the status circle for a contact. Anything but a known online contact is
    /// grey: offline, presence not shared with us, or not heard from yet.
    pub(crate) fn contact_presence(&self, chat: &crate::model::Chat) -> crate::components::Presence {
        use crate::{components::Presence, contact_status};
        if chat.group || chat.subtitle != "Online" || !self.shows_contact_status() {
            return Presence::Unknown;
        }
        let detail = self.contact_status.borrow().get(&chat.jid).copied();
        Presence::from_light(contact_status::light(true, true, detail))
    }

    /// Ask about the open conversation, if it is a contact and the server supports this.
    pub(crate) fn refresh_last_activity_current(self: &Rc<Self>) {
        let (peer, group) = {
            let state = self.state.borrow();
            let chat = state.current();
            (chat.jid.clone(), chat.group)
        };
        if !group {
            self.refresh_last_activity(&peer);
        }
    }

    pub(crate) fn refresh_last_activity(self: &Rc<Self>, peer: &str) {
        let owner = self.state.borrow().account.clone();
        if owner.is_empty() || !askable(peer) || peer == owner || !self.shows_last_seen() {
            return;
        }
        if self.blocking_snapshot.borrow().jids.as_ref().is_some_and(|blocked| blocked.iter().any(|jid| jid == peer)) {
            return;
        }
        let Some(iq) = self.session.borrow().as_ref().map(|session| crate::iq::Client::new(session.tx.clone())) else {
            return;
        };
        let now = Instant::now();
        let supported = self.last_activity.borrow().supported(now);
        if supported == Some(false) {
            return;
        }
        if supported.is_none() && self.last_activity.borrow().probing {
            return;
        }
        if !self.last_activity.borrow_mut().begin(&owner, peer, now) {
            return;
        }
        let generation = self.generation.get();
        // This runs on the GTK main loop, which has no Tokio runtime: `Client::request` would panic
        // there ("there is no reactor running"), so it must be `request_on_main_loop`. A test pins this.
        let (peer, domain) = (peer.to_owned(), owner.split_once('@').map_or(owner.clone(), |(_, d)| d.to_owned()));
        let weak = Rc::downgrade(self);
        glib::MainContext::default().spawn_local(async move {
            let current = |ui: &Rc<Ui>| ui.generation.get() == generation && ui.state.borrow().account == owner;
            if supported.is_none() {
                if let Some(ui) = weak.upgrade() {
                    ui.last_activity.borrow_mut().probing = true;
                }
                let disco = tokio_xmpp::minidom::Element::builder("query", "http://jabber.org/protocol/disco#info").build();
                let yes = matches!(
                    iq.request_on_main_loop(&domain, crate::iq::Kind::Get, disco).await,
                    Ok(Some(info)) if last_activity::advertised(&info)
                );
                let Some(ui) = weak.upgrade().filter(|ui| current(ui)) else { return };
                {
                    let mut cache = ui.last_activity.borrow_mut();
                    cache.probing = false;
                    cache.support = Some((yes, Instant::now()));
                    if !yes {
                        cache.entries.remove(&(owner.clone(), peer.clone()));
                    }
                }
                if !yes {
                    return;
                }
            }
            let outcome = match iq.request_on_main_loop(&peer, crate::iq::Kind::Get, last_activity::request()).await {
                Ok(Some(query)) => match last_activity::parse(&query) {
                    Some(activity) => Outcome::Known(last_activity::when(activity, Utc::now())),
                    None => Outcome::Unavailable,
                },
                _ => Outcome::Unavailable,
            };
            let Some(ui) = weak.upgrade().filter(|ui| current(ui)) else { return };
            ui.last_activity.borrow_mut().finish(&owner, &peer, outcome, Instant::now());
            if outcome != Outcome::Unavailable {
                ui.update_chat_subtitle();
                ui.refresh_profile_card();
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_plain_contact_addresses_are_asked_about() {
        assert!(askable("bob@example.org"));
        assert!(!askable("example.org"), "a server is asked about uptime, which means something else");
        assert!(!askable("bob@example.org/phone"), "a resource's answer is idle time, not last seen");
        assert!(!askable("room@conference.example.org/nick"));
        assert!(!askable(""));
        assert!(!askable("not an address"));
        assert!(!askable("Bob@EXAMPLE.org"), "only the normalised form");
    }

    #[test]
    fn a_contact_is_asked_once_then_again_only_after_the_answer_goes_stale() {
        let mut cache = Cache::default();
        let start = Instant::now();
        assert!(cache.begin("me@x", "bob@x", start));
        assert!(!cache.begin("me@x", "bob@x", start), "already under way");
        assert_eq!(cache.outcome("me@x", "bob@x"), Some(Outcome::Asking));
        cache.finish("me@x", "bob@x", Outcome::Known(None), start);
        assert!(!cache.begin("me@x", "bob@x", start + Duration::from_secs(59)));
        assert!(cache.begin("me@x", "bob@x", start + Duration::from_secs(60)));
        // A stuck question is eventually asked again.
        assert!(cache.begin("me@x", "carol@x", start));
        assert!(!cache.begin("me@x", "carol@x", start + Duration::from_secs(39)));
        assert!(cache.begin("me@x", "carol@x", start + Duration::from_secs(41)));
    }

    #[test]
    fn a_refused_contact_is_left_alone_for_five_minutes() {
        let mut cache = Cache::default();
        let start = Instant::now();
        assert!(cache.begin("me@x", "bob@x", start));
        cache.finish("me@x", "bob@x", Outcome::Unavailable, start);
        assert!(!cache.begin("me@x", "bob@x", start + Duration::from_secs(299)));
        assert!(cache.begin("me@x", "bob@x", start + Duration::from_secs(300)));
    }

    #[test]
    fn answers_are_kept_per_account_and_contact() {
        let mut cache = Cache::default();
        let start = Instant::now();
        cache.finish("me@x", "bob@x", Outcome::Known(None), start);
        assert_eq!(cache.outcome("me@x", "bob@x"), Some(Outcome::Known(None)));
        assert_eq!(cache.outcome("other@x", "bob@x"), None, "another account has its own answers");
        assert_eq!(cache.outcome("me@x", "carol@x"), None);
        assert!(cache.begin("other@x", "bob@x", start), "so it is asked separately");
    }

    #[test]
    fn server_support_is_remembered_and_expires() {
        let mut cache = Cache::default();
        let start = Instant::now();
        assert_eq!(cache.supported(start), None);
        cache.support = Some((true, start));
        assert_eq!(cache.supported(start + Duration::from_secs(599)), Some(true));
        assert_eq!(cache.supported(start + Duration::from_secs(600)), None, "checked again");
        cache.support = Some((false, start));
        assert_eq!(cache.supported(start), Some(false));
    }

    fn ui() -> Rc<Ui> {
        adw::init().unwrap();
        let app = adw::Application::builder()
            .application_id("com.negzero.LastSeenFixture")
            .flags(gtk::gio::ApplicationFlags::NON_UNIQUE)
            .build();
        app.register(None::<&gtk::gio::Cancellable>).unwrap();
        Ui::new(&app, true)
    }

    #[gtk::test]
    #[ignore = "requires a private GTK display and D-Bus"]
    fn offline_gives_way_to_the_last_seen_line_and_nothing_else_does() {
        let ui = ui();
        ui.state.borrow_mut().account = "me@example.org".into();
        let peer = "bob@example.org";
        // Before any answer, and while asking, the plain status stays.
        assert_eq!(ui.status_text(peer, "Offline"), "Offline");
        ui.last_activity.borrow_mut().begin("me@example.org", peer, Instant::now());
        assert_eq!(ui.status_text(peer, "Offline"), "Offline");
        // Refused or failed: still the plain status.
        ui.last_activity.borrow_mut().finish("me@example.org", peer, Outcome::Unavailable, Instant::now());
        assert_eq!(ui.status_text(peer, "Offline"), "Offline");
        // An answer 15 minutes and 3 seconds old.
        let then = Utc::now() - chrono::Duration::try_seconds(903).unwrap();
        ui.last_activity.borrow_mut().finish("me@example.org", peer, Outcome::Known(Some(then)), Instant::now());
        let line = ui.status_text(peer, "Offline");
        assert!(line.starts_with("Last seen 15 minutes ago · "), "{line}");
        assert!(line.ends_with(&last_activity::absolute(&then.with_timezone(&chrono::Local))), "{line}");
        assert_eq!(ui.status_text(peer, ""), line, "an empty status counts as unknown too");
        // Live presence, a typing or joining state and other people are never overwritten.
        assert_eq!(ui.status_text(peer, "Online"), "Online");
        assert_eq!(ui.status_text(peer, "Joining…"), "Joining…");
        assert_eq!(ui.status_text("carol@example.org", "Offline"), "Offline");
        // Online right now.
        ui.last_activity.borrow_mut().finish("me@example.org", peer, Outcome::Known(None), Instant::now());
        assert_eq!(ui.status_text(peer, "Offline"), "Online");
        // Another account does not see this account's answers.
        ui.state.borrow_mut().account = "other@example.org".into();
        assert_eq!(ui.status_text(peer, "Offline"), "Offline");
    }

    #[gtk::test]
    #[ignore = "requires a private GTK display and D-Bus"]
    fn nothing_is_asked_without_an_account_a_session_or_a_real_contact() {
        let ui = ui();
        // Preview mode (no account): no question, no cache entry.
        ui.refresh_last_activity("bob@example.org");
        assert!(ui.last_activity.borrow().entries.is_empty());
        ui.state.borrow_mut().account = "me@example.org".into();
        // No connection: nothing is asked and nothing is left marked as under way.
        ui.refresh_last_activity("bob@example.org");
        assert!(ui.last_activity.borrow().entries.is_empty());
        for peer in ["me@example.org", "example.org", "room@conference.example.org/nick", "", "bob@example.org/phone"] {
            ui.refresh_last_activity(peer);
        }
        assert!(ui.last_activity.borrow().entries.is_empty());
    }

    // ---- the status circle and text on a contact ----------------------------------------

    use adw::prelude::*;
    use crate::{
        components::{Presence, PresenceAvatar},
        contact_status::{Detail, Show},
        model::{Account, State},
        protocol::BackendEvent,
    };

    const OWNER: &str = "alice@example.invalid";
    const LAST_SEEN_KEY: &str = crate::presence_privacy::LAST_SEEN;
    const STATUS_KEY: &str = crate::presence_privacy::CONTACT_STATUS;
    const HATS_KEY: &str = crate::presence_privacy::HATS;
    const PEER: &str = "bob@example.invalid";

    fn pump() {
        let context = glib::MainContext::default();
        let deadline = Instant::now() + Duration::from_millis(250);
        while Instant::now() < deadline {
            context.iteration(false);
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    fn open_contact(ui: &Rc<Ui>) {
        ui.store
            .save_account(&Account { jid: OWNER.into(), host: String::new(), port: 5222, direct_tls: false })
            .unwrap();
        let mut state = State::live(OWNER, vec![]);
        state.active = state.chat_for(PEER).id.clone();
        ui.activate_state(state);
        let id = ui.state.borrow().chats.iter().find(|c| c.jid == PEER).unwrap().id.clone();
        ui.select_chat(&id);
        ui.window.set_default_size(1100, 700);
        ui.window.present();
        pump();
    }
    fn event(ui: &Rc<Ui>, event: BackendEvent) {
        ui.backend_event(ui.generation.get(), OWNER, event);
    }
    fn presence(ui: &Rc<Ui>, visible: bool, online: bool) {
        event(ui, BackendEvent::Presence { peer: PEER.into(), online, visible });
    }
    fn status(ui: &Rc<Ui>, show: Show, idle_minutes: Option<i64>) {
        let idle_since = idle_minutes.map(|m| Utc::now() - chrono::Duration::try_minutes(m).unwrap());
        event(ui, BackendEvent::ContactStatus { peer: PEER.into(), detail: Some(Detail { show, idle_since }) });
    }
    fn header_dot(ui: &Rc<Ui>) -> gtk::Widget {
        let dot = ui.header_presence.borrow().clone().expect("a contact's header has a status circle");
        dot_of(&dot)
    }
    fn dot_of(avatar: &PresenceAvatar) -> gtk::Widget {
        let mut stack = vec![avatar.clone().upcast::<gtk::Widget>()];
        while let Some(widget) = stack.pop() {
            if widget.has_css_class("presence-dot") {
                return widget;
            }
            let mut child = widget.first_child();
            while let Some(c) = child {
                child = c.next_sibling();
                stack.push(c);
            }
        }
        panic!("no status circle");
    }
    fn colour(dot: &gtk::Widget) -> &'static str {
        ["presence-online", "presence-away", "presence-busy", "presence-unknown"]
            .into_iter()
            .find(|class| dot.has_css_class(class))
            .unwrap()
    }
    fn header_text(ui: &Rc<Ui>) -> String {
        ui.header_subtitle.borrow().as_ref().unwrap().text().to_string()
    }

    #[gtk::test]
    #[ignore = "requires a private GTK display and D-Bus"]
    fn the_header_circle_is_green_yellow_red_or_grey_as_the_contacts_presence_says() {
        let ui = ui();
        open_contact(&ui);
        // Presence not shared with us yet: grey, and no word beyond that.
        presence(&ui, false, false);
        assert_eq!(colour(&header_dot(&ui)), "presence-unknown");
        // Offline: grey.
        presence(&ui, true, false);
        assert_eq!(colour(&header_dot(&ui)), "presence-unknown");
        assert_eq!(header_text(&ui), "Offline");
        // Online with no detail yet: green.
        presence(&ui, true, true);
        assert_eq!(colour(&header_dot(&ui)), "presence-online");
        assert_eq!(header_text(&ui), "Online");
        // Away: yellow. Do not disturb: red. Back: green.
        status(&ui, Show::Away, None);
        presence(&ui, true, true);
        assert_eq!(colour(&header_dot(&ui)), "presence-away");
        assert_eq!(header_text(&ui), "Away");
        status(&ui, Show::DoNotDisturb, None);
        presence(&ui, true, true);
        assert_eq!(colour(&header_dot(&ui)), "presence-busy");
        assert_eq!(header_text(&ui), "Do not disturb");
        status(&ui, Show::Available, None);
        presence(&ui, true, true);
        assert_eq!(colour(&header_dot(&ui)), "presence-online");
        // Going offline wins over any stale detail that was left behind.
        status(&ui, Show::DoNotDisturb, None);
        presence(&ui, true, false);
        assert_eq!(colour(&header_dot(&ui)), "presence-unknown");
        assert_eq!(header_text(&ui), "Offline");
        // And so does losing the right to see them.
        presence(&ui, false, true);
        assert_eq!(colour(&header_dot(&ui)), "presence-unknown");
        ui.window.close();
    }

    #[gtk::test]
    #[ignore = "requires a private GTK display and D-Bus"]
    fn idle_time_is_added_to_the_text_but_does_not_change_the_colour() {
        let ui = ui();
        open_contact(&ui);
        presence(&ui, true, true);
        status(&ui, Show::Away, Some(12));
        presence(&ui, true, true);
        let text = header_text(&ui);
        assert!(text.starts_with("Away · idle for 12 minutes · since "), "{text}");
        assert_eq!(colour(&header_dot(&ui)), "presence-away");
        // Available but idle: still green, with the idle time said in words.
        status(&ui, Show::Available, Some(90));
        presence(&ui, true, true);
        assert!(header_text(&ui).starts_with("Online · idle for 1 hour · since "), "{}", header_text(&ui));
        assert_eq!(colour(&header_dot(&ui)), "presence-online");
        // Idle for under a minute is not worth a sentence.
        status(&ui, Show::Available, Some(0));
        presence(&ui, true, true);
        assert_eq!(header_text(&ui), "Online");
        // The profile card says the same.
        let chat = ui.state.borrow().chats.iter().find(|c| c.jid == PEER).cloned().unwrap();
        status(&ui, Show::Away, Some(30));
        assert!(ui.contact_presence(&chat) == Presence::Away);
        ui.window.close();
    }

    #[gtk::test]
    #[ignore = "requires a private GTK display and D-Bus"]
    fn only_a_known_online_person_ever_gets_a_colour() {
        let ui = ui();
        let chat = |subtitle: &str, group: bool, jid: &str| crate::model::Chat {
            jid: jid.into(),
            subtitle: subtitle.into(),
            group,
            ..Default::default()
        };
        ui.contact_status.borrow_mut().insert(PEER.into(), Detail { show: Show::DoNotDisturb, idle_since: None });
        ui.contact_status.borrow_mut().insert("room@conference.example.invalid".into(), Detail { show: Show::Away, idle_since: None });
        assert_eq!(ui.contact_presence(&chat("Online", false, PEER)), Presence::Busy);
        assert_eq!(ui.contact_presence(&chat("Online", false, "carol@example.invalid")), Presence::Online, "no detail yet");
        for subtitle in ["Offline", "", "Disconnected", "Joining…", "online", "ONLINE", "Online "] {
            assert_eq!(ui.contact_presence(&chat(subtitle, false, PEER)), Presence::Unknown, "{subtitle:?}");
        }
        assert_eq!(
            ui.contact_presence(&chat("Online", true, "room@conference.example.invalid")),
            Presence::Unknown,
            "a room has no single presence"
        );
    }

    #[gtk::test]
    #[ignore = "requires a private GTK display and D-Bus"]
    fn the_chat_list_and_a_new_account_show_the_right_circles() {
        let ui = ui();
        open_contact(&ui);
        status(&ui, Show::Away, None);
        presence(&ui, true, true);
        ui.refresh_chats(None);
        pump();
        let list = ui.chat_stack.visible_child().and_downcast::<gtk::ListBox>().expect("a list of chats");
        let id = ui.state.borrow().chats.iter().find(|c| c.jid == PEER).unwrap().id.clone();
        let mut row = None;
        let mut child = list.first_child();
        while let Some(widget) = child {
            child = widget.next_sibling();
            if widget.widget_name() == id.as_str() {
                row = widget.downcast::<crate::components::ChatRow>().ok();
            }
        }
        let row = row.expect("the contact's chat row");
        let mut dots = vec![];
        let mut stack = vec![row.upcast::<gtk::Widget>()];
        while let Some(widget) = stack.pop() {
            if let Ok(avatar) = widget.clone().downcast::<PresenceAvatar>() {
                dots.push(avatar);
            }
            let mut child = widget.first_child();
            while let Some(c) = child {
                child = c.next_sibling();
                stack.push(c);
            }
        }
        assert_eq!(dots.len(), 1, "one circle on the row's avatar");
        assert_eq!(colour(&dot_of(&dots[0])), "presence-away");
        // Another account must not inherit the previous account's contacts.
        assert!(!ui.contact_status.borrow().is_empty());
        ui.contact_status.borrow_mut().clear();
        ui.window.close();
    }

    // ---- privacy options ----------------------------------------------------------------

    fn set_option(ui: &Rc<Ui>, key: &str, on: bool) {
        let owner = ui.state.borrow().account.clone();
        ui.store.set_pref(&format!("{key}:{owner}"), if on { "true" } else { "false" }).unwrap();
        ui.presence_privacy_changed();
    }

    #[gtk::test]
    #[ignore = "requires a private GTK display and D-Bus"]
    fn every_option_starts_on_and_is_kept_per_account() {
        let ui = ui();
        open_contact(&ui);
        assert!(ui.shows_last_seen() && ui.shows_contact_status() && ui.shows_hats(), "all default to on");
        set_option(&ui, crate::presence_privacy::HATS, false);
        assert!(!ui.shows_hats() && ui.shows_last_seen() && ui.shows_contact_status(), "one switch, one option");
        set_option(&ui, crate::presence_privacy::LAST_SEEN, false);
        set_option(&ui, crate::presence_privacy::CONTACT_STATUS, false);
        assert!(!ui.shows_last_seen() && !ui.shows_contact_status() && !ui.shows_hats());
        // Another account has its own choices.
        ui.state.borrow_mut().account = "other@example.invalid".into();
        assert!(ui.shows_last_seen() && ui.shows_contact_status() && ui.shows_hats());
        ui.state.borrow_mut().account = OWNER.into();
        assert!(!ui.shows_hats(), "and this one kept its own");
        // Turning them back on works.
        for (key, _, _) in crate::presence_privacy::OPTIONS {
            set_option(&ui, key, true);
        }
        assert!(ui.shows_last_seen() && ui.shows_contact_status() && ui.shows_hats());
        ui.window.close();
    }

    #[gtk::test]
    #[ignore = "requires a private GTK display and D-Bus"]
    fn turning_status_off_removes_the_circle_and_the_extra_words_at_once_and_on_restores_them() {
        let ui = ui();
        open_contact(&ui);
        status(&ui, Show::Away, Some(12));
        presence(&ui, true, true);
        let text = header_text(&ui);
        assert!(text.starts_with("Away · idle for 12 minutes"), "{text}");
        assert!(ui.header_presence.borrow().as_ref().unwrap().dot_visible());
        set_option(&ui, crate::presence_privacy::CONTACT_STATUS, false);
        // Live, with no new presence event: the plain status is back and the circle is gone.
        assert_eq!(header_text(&ui), "Online");
        assert!(!ui.header_presence.borrow().as_ref().unwrap().dot_visible(), "hidden, not grey");
        let chat = ui.state.borrow().chats.iter().find(|c| c.jid == PEER).cloned().unwrap();
        assert_eq!(ui.contact_presence(&chat), Presence::Unknown);
        // Presence updates that arrive while it is off change nothing on screen.
        status(&ui, Show::DoNotDisturb, None);
        presence(&ui, true, true);
        assert_eq!(header_text(&ui), "Online");
        assert!(!ui.header_presence.borrow().as_ref().unwrap().dot_visible());
        // The ordinary online/offline text is not part of this option.
        presence(&ui, true, false);
        assert_eq!(header_text(&ui), "Offline");
        presence(&ui, true, true);
        set_option(&ui, crate::presence_privacy::CONTACT_STATUS, true);
        assert_eq!(header_text(&ui), "Do not disturb");
        assert!(ui.header_presence.borrow().as_ref().unwrap().dot_visible());
        assert_eq!(colour(&header_dot(&ui)), "presence-busy");
        ui.window.close();
    }

    #[gtk::test]
    #[ignore = "requires a private GTK display and D-Bus"]
    fn turning_last_seen_off_hides_it_stops_asking_and_on_brings_it_back() {
        let ui = ui();
        open_contact(&ui);
        presence(&ui, true, false);
        assert_eq!(header_text(&ui), "Offline");
        let then = Utc::now() - chrono::Duration::try_seconds(903).unwrap();
        ui.last_activity.borrow_mut().finish(OWNER, PEER, Outcome::Known(Some(then)), Instant::now());
        ui.update_chat_subtitle();
        assert!(header_text(&ui).starts_with("Last seen 15 minutes ago · "), "{}", header_text(&ui));
        set_option(&ui, crate::presence_privacy::LAST_SEEN, false);
        assert_eq!(header_text(&ui), "Offline", "gone immediately");
        assert_eq!(ui.last_seen_line(PEER), None);
        // Nothing is asked while it is off, even for a contact never asked about.
        let before = ui.last_activity.borrow().entries.len();
        ui.refresh_last_activity("carol@example.invalid");
        ui.refresh_last_activity_current();
        assert_eq!(ui.last_activity.borrow().entries.len(), before, "no question was started");
        set_option(&ui, crate::presence_privacy::LAST_SEEN, true);
        assert!(header_text(&ui).starts_with("Last seen 15 minutes ago · "), "kept answers return");
        ui.window.close();
    }

    #[gtk::test]
    #[ignore = "requires a private GTK display and D-Bus"]
    fn turning_hats_off_hides_them_in_the_roster_and_on_messages_and_on_shows_them_again() {
        use crate::rooms::{hats::Hat, members::Participant};
        const ROOM: &str = "room@conference.example.invalid";
        let ui = ui();
        open_contact(&ui);
        {
            let mut state = ui.state.borrow_mut();
            let chat = state.chat_for(ROOM);
            chat.group = true;
            let id = chat.id.clone();
            state.active = id;
        }
        ui.room_rosters.borrow_mut().insert(
            ROOM.into(),
            vec![Participant {
                nick: "amy".into(),
                jid: None,
                affiliation: "member".into(),
                role: "participant".into(),
                show: String::new(),
                own: false,
                hats: vec![Hat::new("Host", "urn:h", Some(120.0)).unwrap()],
            }],
        );
        let message = crate::model::Message { id: "m".into(), author: "amy".into(), text: "hi".into(), ..Default::default() };
        let titles = |ui: &Rc<Ui>| ui.hats_of(&message).into_iter().map(|h| h.title).collect::<Vec<_>>();
        assert_eq!(titles(&ui), ["Host"]);
        set_option(&ui, crate::presence_privacy::HATS, false);
        assert!(titles(&ui).is_empty(), "no hats on messages while off");
        set_option(&ui, crate::presence_privacy::HATS, true);
        assert_eq!(titles(&ui), ["Host"]);
        ui.window.close();
    }

    /// Every `SwitchRow` under a widget, with its title.
    fn switches(root: &gtk::Widget, found: &mut Vec<adw::SwitchRow>) {
        if let Some(row) = root.downcast_ref::<adw::SwitchRow>() {
            found.push(row.clone());
        }
        let mut child = root.first_child();
        while let Some(widget) = child {
            child = widget.next_sibling();
            switches(&widget, found);
        }
    }

    #[gtk::test]
    #[ignore = "requires a private GTK display and D-Bus"]
    fn preferences_lists_the_three_options_and_flipping_them_takes_effect_and_is_saved() {
        let ui = ui();
        open_contact(&ui);
        status(&ui, Show::Away, None);
        presence(&ui, true, true);
        assert_eq!(header_text(&ui), "Away");
        ui.preferences();
        pump();
        let dialog = ui.dialogs.borrow().iter().rev().find_map(|d| d.clone().downcast::<adw::PreferencesDialog>().ok()).expect("the preferences dialog");
        let mut rows = vec![];
        switches(dialog.upcast_ref(), &mut rows);
        let row = |title: &str| rows.iter().find(|row| row.title() == title).unwrap_or_else(|| panic!("no row {title:?}")).clone();
        // All three are there with their explanations, and start on.
        for (key, title, description) in crate::presence_privacy::OPTIONS {
            let r = row(title);
            assert!(r.is_active(), "{key} starts on");
            assert_eq!(r.subtitle().as_deref(), Some(description), "{key}");
        }
        // Flipping the status switch changes the open conversation at once, and is saved.
        row(crate::presence_privacy::OPTIONS[1].1).set_active(false);
        assert_eq!(header_text(&ui), "Online");
        assert_eq!(ui.store.pref(&format!("{}:{OWNER}", crate::presence_privacy::CONTACT_STATUS), "true"), "false");
        assert!(ui.shows_last_seen() && ui.shows_hats(), "the others are untouched");
        row(crate::presence_privacy::OPTIONS[1].1).set_active(true);
        assert_eq!(header_text(&ui), "Away");
        row(crate::presence_privacy::OPTIONS[0].1).set_active(false);
        row(crate::presence_privacy::OPTIONS[2].1).set_active(false);
        assert_eq!(
            [LAST_SEEN_KEY, STATUS_KEY, HATS_KEY].map(|key| ui.store.pref(&format!("{key}:{OWNER}"), "true")),
            ["false", "true", "false"]
        );
        // A freshly opened Preferences shows the saved choices.
        dialog.force_close();
        ui.preferences();
        pump();
        let dialog = ui.dialogs.borrow().iter().rev().find_map(|d| d.clone().downcast::<adw::PreferencesDialog>().ok()).unwrap();
        let mut rows = vec![];
        switches(dialog.upcast_ref(), &mut rows);
        let active = |title: &str| rows.iter().find(|row| row.title() == title).unwrap().is_active();
        assert_eq!(
            crate::presence_privacy::OPTIONS.map(|(_, title, _)| active(title)),
            [false, true, false]
        );
        dialog.force_close();
        ui.window.close();
    }

    // ---- the lookup on the GTK main loop (no Tokio runtime) ------------------------------

    use crate::protocol::Command;
    use tokio::sync::mpsc;

    /// A session whose server is this test: commands the app sends arrive on the receiver.
    fn connect(ui: &Rc<Ui>) -> mpsc::UnboundedReceiver<Command> {
        let (tx, rx) = mpsc::unbounded_channel();
        ui.session.replace(Some(crate::protocol::Session::fixture_channel(tx)));
        rx
    }
    /// Run the main loop until the app sends an IQ, and take it.
    fn next_iq(rx: &mut mpsc::UnboundedReceiver<Command>) -> (String, tokio_xmpp::minidom::Element, tokio::sync::oneshot::Sender<crate::iq::Reply>) {
        let context = glib::MainContext::default();
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline {
            context.iteration(false);
            if let Ok(Command::Iq { to, payload, reply, .. }) = rx.try_recv() {
                return (to, payload, reply);
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        panic!("the app sent no IQ");
    }
    fn no_more_iq(rx: &mut mpsc::UnboundedReceiver<Command>) {
        pump();
        assert!(rx.try_recv().is_err(), "nothing else was sent");
    }
    fn disco(feature: Option<&str>) -> tokio_xmpp::minidom::Element {
        let inner = feature.map(|f| format!("<feature var='{f}'/>")).unwrap_or_default();
        format!("<query xmlns='http://jabber.org/protocol/disco#info'>{inner}</query>").parse().unwrap()
    }
    fn last(seconds: &str) -> tokio_xmpp::minidom::Element {
        format!("<query xmlns='jabber:iq:last' seconds='{seconds}'/>").parse().unwrap()
    }
    fn signed_in(ui: &Rc<Ui>) {
        open_contact(ui);
        presence(ui, true, false);
    }

    /// Regression: `thread 'main' panicked at src/iq.rs:63:9: there is no reactor running, must be
    /// called from the context of a Tokio 1.x runtime`. The lookup used `Client::request`, which
    /// needs a Tokio timer; the GTK main loop has none.
    #[gtk::test]
    #[ignore = "requires a private GTK display and D-Bus"]
    fn the_last_seen_lookup_runs_on_the_gtk_main_loop_without_a_tokio_runtime() {
        assert!(tokio::runtime::Handle::try_current().is_err(), "this thread has no Tokio runtime, like the real main loop");
        let ui = ui();
        signed_in(&ui);
        let mut server = connect(&ui);
        ui.refresh_last_activity(PEER);
        // First the server is asked what it supports, then the contact is asked.
        let (to, payload, reply) = next_iq(&mut server);
        assert_eq!(to, "example.invalid");
        assert!(payload.is("query", "http://jabber.org/protocol/disco#info"));
        reply.send(Ok(Some(disco(Some("jabber:iq:last"))))).unwrap();
        let (to, payload, reply) = next_iq(&mut server);
        assert_eq!(to, PEER);
        assert!(payload.is("query", "jabber:iq:last"), "asked in the XEP-0012 namespace");
        reply.send(Ok(Some(last("903")))).unwrap();
        pump();
        let line = ui.last_seen_line(PEER).expect("the answer is shown");
        assert!(line.starts_with("Last seen 15 minutes ago · "), "{line}");
        assert!(header_text(&ui).starts_with("Last seen 15 minutes ago · "), "{}", header_text(&ui));
        no_more_iq(&mut server);
        ui.window.close();
    }

    #[gtk::test]
    #[ignore = "requires a private GTK display and D-Bus"]
    fn a_server_without_last_activity_is_asked_once_and_the_contact_never() {
        let ui = ui();
        signed_in(&ui);
        let mut server = connect(&ui);
        ui.refresh_last_activity(PEER);
        let (_, _, reply) = next_iq(&mut server);
        reply.send(Ok(Some(disco(Some("urn:other"))))).unwrap();
        pump();
        assert_eq!(ui.last_seen_line(PEER), None);
        assert_eq!(header_text(&ui), "Offline");
        // Other contacts are not asked either, and the server is not asked again for a while.
        ui.refresh_last_activity("carol@example.invalid");
        ui.refresh_last_activity(PEER);
        no_more_iq(&mut server);
        assert_eq!(ui.last_activity.borrow().supported(Instant::now()), Some(false));
        ui.window.close();
    }

    #[gtk::test]
    #[ignore = "requires a private GTK display and D-Bus"]
    fn refused_failed_and_garbled_answers_show_nothing_and_do_not_crash() {
        use crate::iq::Error;
        let answers: [(&str, crate::iq::Reply); 7] = [
            ("forbidden", Err(Error::Rejected { condition: "forbidden".into(), pubsub: None })),
            ("service-unavailable", Err(Error::Rejected { condition: "service-unavailable".into(), pubsub: None })),
            ("timeout", Err(Error::Timeout)),
            ("session closed", Err(Error::Closed)),
            ("empty result", Ok(None)),
            ("not a number", Ok(Some(last("soon")))),
            ("wrong namespace", Ok(Some("<query xmlns='urn:other' seconds='5'/>".parse().unwrap()))),
        ];
        for (name, answer) in answers {
            let ui = ui_for(name);
            signed_in(&ui);
            let mut server = connect(&ui);
            // The server's support is already known, so only the contact is asked.
            ui.last_activity.borrow_mut().support = Some((true, Instant::now()));
            ui.refresh_last_activity(PEER);
            let (to, _, reply) = next_iq(&mut server);
            assert_eq!(to, PEER, "{name}");
            reply.send(answer).unwrap();
            pump();
            assert_eq!(ui.last_seen_line(PEER), None, "{name}");
            assert_eq!(header_text(&ui), "Offline", "{name}");
            // And it is not asked again straight away.
            ui.refresh_last_activity(PEER);
            no_more_iq(&mut server);
            ui.window.close();
        }
    }

    /// A fresh fixture per case, but GTK may only be initialised once per process.
    fn ui_for(_case: &str) -> Rc<Ui> {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        adw::init().unwrap();
        let app = adw::Application::builder()
            .application_id(format!("com.negzero.LastSeenLoop{}", NEXT.fetch_add(1, Ordering::Relaxed)))
            .flags(gtk::gio::ApplicationFlags::NON_UNIQUE)
            .build();
        app.register(None::<&gtk::gio::Cancellable>).unwrap();
        Ui::new(&app, true)
    }

    #[gtk::test]
    #[ignore = "requires a private GTK display and D-Bus"]
    fn a_lookup_is_dropped_quietly_when_the_session_ends_or_the_account_changes_mid_flight() {
        let ui = ui();
        signed_in(&ui);
        let mut server = connect(&ui);
        ui.last_activity.borrow_mut().support = Some((true, Instant::now()));
        ui.refresh_last_activity(PEER);
        let (_, _, reply) = next_iq(&mut server);
        // The account changes before the answer arrives: it must not be shown for the new one.
        ui.state.borrow_mut().account = "other@example.invalid".into();
        reply.send(Ok(Some(last("60")))).unwrap();
        pump();
        assert_eq!(ui.last_seen_line(PEER), None);
        // A session that goes away while a question is open answers "closed": no panic, no text.
        ui.state.borrow_mut().account = OWNER.into();
        ui.last_activity.borrow_mut().entries.clear();
        ui.refresh_last_activity(PEER);
        let (_, _, reply) = next_iq(&mut server);
        drop(reply);
        pump();
        assert_eq!(ui.last_seen_line(PEER), None);
        ui.window.close();
    }
}
