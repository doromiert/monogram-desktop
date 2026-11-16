//! Participants panel for rooms: a right-hand utility pane with an invite button, a search field
//! and the occupants grouped into Owners, Administrators and Participants.
use crate::{rooms::members::Participant, ui::Ui, widgets::*};
use adw::prelude::*;
use gtk::{gio, glib, subclass::prelude::*};
use std::{
    cell::{Cell, RefCell},
    rc::{Rc, Weak},
};

#[derive(Clone, Debug, PartialEq, Eq)]
enum Row {
    Heading(String),
    Person(Participant),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Section {
    Owners,
    Administrators,
    Participants,
}
impl Section {
    fn of(person: &Participant) -> Self {
        match person.affiliation.as_str() {
            "owner" => Self::Owners,
            "admin" => Self::Administrators,
            _ => Self::Participants,
        }
    }
    fn title(self) -> &'static str {
        match self {
            Self::Owners => "Owners",
            Self::Administrators => "Administrators",
            Self::Participants => "Participants",
        }
    }
}

/// Rows for the list: sections in a fixed order, people sorted by name with the user first,
/// each heading showing "visible/total" so a search shows how much of the room it covers.
fn rows(all: &[Participant], query: &str) -> Vec<Row> {
    let query = query.trim().to_lowercase();
    // Names are matched and sorted as they are shown, with invisible characters removed.
    let name = |person: &Participant| crate::text_safety::display_nick(&person.nick, person.own);
    let matches = |person: &Participant| {
        query.is_empty()
            || name(person).to_lowercase().contains(&query)
            || person
                .jid
                .as_ref()
                .is_some_and(|jid| jid.to_lowercase().contains(&query))
    };
    let mut rows = Vec::new();
    for section in [Section::Owners, Section::Administrators, Section::Participants] {
        let mut people: Vec<&Participant> = all
            .iter()
            .filter(|person| Section::of(person) == section && matches(person))
            .collect();
        if people.is_empty() {
            continue;
        }
        people.sort_by_cached_key(|person| (!person.own, name(person).to_lowercase()));
        rows.push(Row::Heading(format!(
            "{} ({}/{})",
            section.title(),
            people.len(),
            all.len()
        )));
        rows.extend(people.into_iter().cloned().map(Row::Person));
    }
    rows
}

/// Addresses typed into the invite dialog: split on spaces and commas, anything without a
/// local and a domain part is ignored, duplicates collapse.
fn parse_addresses(text: &str) -> Vec<String> {
    let mut members: Vec<String> = text
        .split(|c: char| c.is_whitespace() || c == ',')
        .filter(|part| {
            part.split_once('@')
                .is_some_and(|(local, domain)| !local.is_empty() && !domain.is_empty())
        })
        .map(str::to_owned)
        .collect();
    members.sort();
    members.dedup();
    members
}

fn presence_label(show: &str) -> &'static str {
    match show {
        "away" | "xa" => "Away",
        "dnd" => "Busy",
        _ => "Online",
    }
}

pub struct ParticipantsView {
    pub room: String,
    root: adw::ToolbarView,
    model: gio::ListStore,
    search: gtk::SearchEntry,
    stack: gtk::Stack,
    empty: adw::StatusPage,
    all: RefCell<Vec<Participant>>,
    pending: Cell<bool>,
}
impl ParticipantsView {
    pub fn update(self: &Rc<Self>, people: Vec<Participant>) {
        self.all.replace(people);
        // A join burst sends many updates; rebuild at most once per frame.
        if self.pending.replace(true) {
            return;
        }
        let weak = Rc::downgrade(self);
        glib::idle_add_local_once(move || {
            if let Some(view) = weak.upgrade() {
                view.pending.set(false);
                view.rebuild();
            }
        });
    }
    fn rebuild(&self) {
        let query = self.search.text();
        let rows = rows(&self.all.borrow(), &query);
        let items: Vec<glib::BoxedAnyObject> =
            rows.iter().cloned().map(glib::BoxedAnyObject::new).collect();
        self.model.splice(0, self.model.n_items(), &items);
        if rows.is_empty() {
            if self.all.borrow().is_empty() {
                self.empty.set_title("No Participants");
                self.empty
                    .set_description(Some("People in this room appear here once you have joined."));
                self.empty.set_icon_name(Some("system-users-symbolic"));
            } else {
                self.empty.set_title("No Results");
                self.empty.set_description(Some("No participant matches your search."));
                self.empty.set_icon_name(Some("system-search-symbolic"));
            }
            self.stack.set_visible_child_name("empty");
        } else {
            self.stack.set_visible_child_name("list");
        }
    }
}

fn factory(ui: Weak<Ui>, room:String) -> gtk::SignalListItemFactory {
    let factory = gtk::SignalListItemFactory::new();
    let profile_ui=ui.clone();
    factory.connect_setup(move |_, item| {
        let item = item.downcast_ref::<gtk::ListItem>().unwrap();
        item.set_activatable(false);
        item.set_selectable(false);
        let root = bx(true, 0);
        let heading = label("", "heading");
        heading.set_xalign(0.);
        heading.set_margin_top(18);
        heading.set_margin_bottom(6);
        heading.set_margin_start(12);
        heading.set_margin_end(12);
        let person = bx(false, 12);
        person.set_margin_top(6);
        person.set_margin_bottom(6);
        person.set_margin_start(12);
        person.set_margin_end(12);
        let overlay = gtk::Overlay::new();
        overlay.set_child(Some(&adw::Avatar::new(40, None, true)));
        let dot = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        dot.add_css_class("presence-dot");
        dot.set_halign(gtk::Align::End);
        dot.set_valign(gtk::Align::End);
        dot.set_can_target(false);
        overlay.add_overlay(&dot);
        person.append(&overlay);
        let name = label("", "");
        name.set_xalign(0.);
        name.set_hexpand(true);
        name.set_ellipsize(gtk::pango::EllipsizeMode::End);
        person.append(&name);
        root.append(&heading);
        let button=gtk::Button::builder().child(&person).css_classes(["flat","profile-target"]).build();
        let weak_item=item.downgrade();let weak_ui=profile_ui.clone();let room=room.clone();
        button.connect_clicked(move|button|{
            let (Some(item),Some(ui))=(weak_item.upgrade(),weak_ui.upgrade())else{return;};
            if let Some(object)=item.item().and_downcast::<glib::BoxedAnyObject>() {
                if let Row::Person(person)=object.borrow::<Row>().clone(){let peer=person.jid.unwrap_or_else(||format!("{room}/{}",person.nick));ui.profile_card(button,&peer,&person.nick);}
            }
        });
        root.append(&button);
        item.set_child(Some(&root));
    });
    factory.connect_bind(move |_, item| {
        let item = item.downcast_ref::<gtk::ListItem>().unwrap();
        let Some(row) = item
            .item()
            .and_downcast::<glib::BoxedAnyObject>()
            .map(|object| object.borrow::<Row>().clone())
        else {
            return;
        };
        let root = item.child().and_downcast::<gtk::Box>().unwrap();
        let heading = root.first_child().and_downcast::<gtk::Label>().unwrap();
        let profile=root.last_child().and_downcast::<gtk::Button>().unwrap();
        let person = profile.child().and_downcast::<gtk::Box>().unwrap();
        match row {
            Row::Heading(text) => {
                heading.set_label(&text);
                heading.set_visible(true);
                profile.set_visible(false);
                person.set_visible(false);
            }
            Row::Person(participant) => {
                heading.set_visible(false);
                profile.set_visible(true);
                person.set_visible(true);
                let overlay = person.first_child().and_downcast::<gtk::Overlay>().unwrap();
                let avatar = overlay.child().and_downcast::<adw::Avatar>().unwrap();
                avatar.set_text(Some(&participant.nick));
                let texture = participant.jid.as_ref().and_then(|jid| {
                    ui.upgrade().and_then(|ui| {
                        ui.avatars
                            .borrow()
                            .cache
                            .get(jid)
                            .and_then(|(_, texture)| texture.clone())
                    })
                });
                avatar.set_custom_image(texture.as_ref());
                let dot = overlay.last_child().unwrap();
                for class in ["presence-online", "presence-away", "presence-busy"] {
                    dot.remove_css_class(class);
                }
                let status = presence_label(&participant.show);
                dot.add_css_class(match status {
                    "Away" => "presence-away",
                    "Busy" => "presence-busy",
                    _ => "presence-online",
                });
                let name = person.last_child().and_downcast::<gtk::Label>().unwrap();
                // The "(You)" mark is added here, for the user's own entry only; other people's
                // names are cleaned so they cannot imitate it or hide characters.
                let nick = crate::text_safety::display_nick(&participant.nick, participant.own);
                let shown = if participant.own { format!("{nick} (You)") } else { nick };
                name.set_label(&shown);
                person.set_tooltip_text(Some(&match &participant.jid {
                    Some(jid) => format!("{shown} · {jid}"),
                    None => shown.clone(),
                }));
                // The coloured dot alone is not accessible; say the state too.
                person.update_property(&[gtk::accessible::Property::Label(&format!(
                    "{shown}, {status}"
                ))]);
            }
        }
    });
    factory
}

/// What the automatic participants pane should do given the window and chat state.
#[derive(Debug, PartialEq, Eq)]
enum Auto {
    Open,
    Close,
    Keep,
}
#[derive(Default)]
struct AutoState {
    /// The utility pane is pinned beside the chat (window wide enough), not an overlay.
    wide: bool,
    group: bool,
    joined: bool,
    live: bool,
    /// The user has not closed the panel; they can bring it back from the menu.
    enabled: bool,
    sidebar_shown: bool,
    showing_participants: bool,
    showing_auto: bool,
    /// The pane lists a different room than the one in view.
    other_room: bool,
}
fn auto_action(state: &AutoState) -> Auto {
    if !state.wide || !state.group {
        return if state.showing_participants && state.showing_auto {
            Auto::Close
        } else {
            Auto::Keep
        };
    }
    if !state.live || !state.joined || !state.enabled {
        return Auto::Keep;
    }
    // Never replace Conversation Info, Shared Media or the media pickers.
    if !state.sidebar_shown || (state.showing_participants && state.other_room) {
        Auto::Open
    } else {
        Auto::Keep
    }
}

impl Ui {
    fn participants_auto_enabled(&self) -> bool {
        self.participants_pref.get().unwrap_or_else(|| {
            let on = self.store.pref("participants-auto", "true") != "false";
            self.participants_pref.set(Some(on));
            on
        })
    }
    fn set_participants_auto(&self, on: bool) {
        self.participants_pref.set(Some(on));
        let _ = self.store.set_pref("participants-auto", if on { "true" } else { "false" });
    }

    /// Show the participants beside a joined room when the window is wide enough, and hide the
    /// automatic pane again when it is not or the chat is not a room.
    pub fn sync_participants(self: &Rc<Self>) {
        let state = self.state.borrow();
        let chat = state.current();
        let showing_participants =
            self.utility.shows_sidebar() && self.media_mode.borrow().as_str() == "participants";
        let current = AutoState {
            wide: !self.utility.is_collapsed(),
            group: chat.group,
            joined: chat.room_joined,
            live: !state.account.is_empty() && !self.on_welcome(),
            enabled: self.participants_auto_enabled(),
            sidebar_shown: self.utility.shows_sidebar(),
            showing_participants,
            showing_auto: self.participants_auto.get(),
            other_room: self
                .participants
                .borrow()
                .as_ref()
                .is_some_and(|view| view.room != chat.jid),
        };
        drop(state);
        match auto_action(&current) {
            Auto::Open => self.open_participants(true),
            Auto::Close => {
                self.participants_auto.set(false);
                self.utility.set_show_sidebar(false);
            }
            Auto::Keep => {}
        }
    }

    /// Opened by the user (menu): it also turns automatic showing back on.
    pub fn show_participants(self: &Rc<Self>) {
        self.set_participants_auto(true);
        self.open_participants(false);
    }

    pub(crate) fn open_participants(self: &Rc<Self>, auto: bool) {
        let chat = self.state.borrow().current().clone();
        if !chat.group || chat.jid.is_empty() {
            return;
        }
        let model = gio::ListStore::new::<glib::BoxedAnyObject>();
        let list = gtk::ListView::new(
            Some(gtk::NoSelection::new(Some(model.clone()))),
            Some(factory(Rc::downgrade(self),chat.jid.clone())),
        );
        list.add_css_class("navigation-sidebar");
        list.set_can_focus(true);
        let search = gtk::SearchEntry::builder()
            .placeholder_text("Search Participants…")
            .margin_start(12)
            .margin_end(12)
            .build();
        search.update_property(&[gtk::accessible::Property::Label("Search participants")]);
        let invite = gtk::Button::new();
        let content = adw::ButtonContent::builder()
            .icon_name("contact-new-symbolic")
            .label("Invite to This Chat")
            .build();
        invite.set_child(Some(&content));
        invite.set_halign(gtk::Align::Center);
        invite.set_margin_top(12);
        invite.set_sensitive(chat.room_joined && self.online.get());
        let peer = chat.jid.clone();
        self.weak_button(&invite, move |ui| ui.invite_to_room(&peer));
        let empty = adw::StatusPage::new();
        empty.add_css_class("compact");
        let stack = gtk::Stack::new();
        stack.add_named(&scroll(&list), Some("list"));
        stack.add_named(&empty, Some("empty"));
        stack.set_vexpand(true);
        let body = bx(true, 12);
        body.append(&invite);
        body.append(&search);
        body.append(&stack);

        let pane = adw::ToolbarView::new();
        pane.add_css_class("utility-sidebar");
        pane.set_top_bar_style(adw::ToolbarStyle::Flat);
        let header = adw::HeaderBar::builder()
            .show_start_title_buttons(false)
            .title_widget(&label("Participants", "heading"))
            .build();
        let close = icon_button("sidebar-show-symbolic", "Close participants");
        let weak = Rc::downgrade(self);
        close.connect_clicked(move |_| {
            if let Some(ui) = weak.upgrade() {
                // Closing is a choice: do not reopen by itself until asked from the menu.
                ui.set_participants_auto(false);
                ui.participants_auto.set(false);
                ui.utility.set_show_sidebar(false);
            }
        });
        header.pack_start(&close);
        pane.add_top_bar(&header);
        pane.set_content(Some(&body));

        let view = Rc::new(ParticipantsView {
            room: chat.jid.clone(),
            root: pane.clone(),
            model,
            search: search.clone(),
            stack,
            empty,
            all: RefCell::new(
                self.room_rosters
                    .borrow()
                    .get(&chat.jid)
                    .cloned()
                    .unwrap_or_default(),
            ),
            pending: Cell::new(false),
        });
        let weak = Rc::downgrade(&view);
        search.connect_search_changed(move |_| {
            if let Some(view) = weak.upgrade() {
                view.rebuild();
            }
        });
        view.rebuild();
        self.participants.replace(Some(view));
        self.participants_auto.set(auto);

        self.track_sidebar(&pane);
        self.utility.set_sidebar(gtk::Widget::NONE);
        self.utility.set_sidebar(Some(&pane));
        pane.set_visible(true);
        self.media_mode.replace("participants".into());
        self.utility.set_show_sidebar(true);
    }

    pub fn room_roster_changed(self: &Rc<Self>, room: &str, people: Vec<Participant>) {
        if people.is_empty() {
            self.room_rosters.borrow_mut().remove(room);
        } else {
            self.room_rosters
                .borrow_mut()
                .insert(room.into(), people.clone());
        }
        let view = self.participants.borrow().clone();
        if let Some(view) = view.filter(|view| view.room == room) {
            view.update(people);
        }
    }

    fn invite_to_room(self: &Rc<Self>, peer: &str) {
        let entry = adw::EntryRow::builder()
            .title("Addresses, separated by spaces")
            .build();
        let group = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .css_classes(["boxed-list"])
            .build();
        group.append(&entry);
        let alert = adw::AlertDialog::builder()
            .heading("Invite to This Chat")
            .body("Enter the addresses of the people to invite.")
            .extra_child(&group)
            .build();
        alert.add_response("cancel", "Cancel");
        alert.add_response("invite", "Invite");
        alert.set_response_appearance("invite", adw::ResponseAppearance::Suggested);
        alert.set_default_response(Some("invite"));
        alert.set_close_response("cancel");
        alert.set_response_enabled("invite", false);
        let alert_weak = alert.downgrade();
        entry.connect_changed(move |entry| {
            if let Some(alert) = alert_weak.upgrade() {
                alert.set_response_enabled("invite", !parse_addresses(&entry.text()).is_empty());
            }
        });
        let weak = Rc::downgrade(self);
        let peer = peer.to_owned();
        alert.connect_response(Some("invite"), move |_, _| {
            if let Some(ui) = weak.upgrade() {
                let members = parse_addresses(&entry.text());
                ui.invite_room_people(&peer, members, "Invitations Incomplete", true);
            }
        });
        alert.present(Some(&self.window));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn person(nick: &str, affiliation: &str, own: bool) -> Participant {
        Participant {
            nick: nick.into(),
            jid: None,
            affiliation: affiliation.into(),
            role: "participant".into(),
            show: String::new(),
            own,
        }
    }

    #[test]
    fn sections_are_ordered_counted_against_the_room_and_sorted_with_the_user_first() {
        let all = vec![
            person("zed", "none", false),
            person("Romeo", "member", true),
            person("bot", "admin", false),
            person("asterix", "owner", false),
            person("amy", "member", false),
        ];
        let rows = rows(&all, "");
        let text: Vec<String> = rows
            .iter()
            .map(|row| match row {
                Row::Heading(title) => title.clone(),
                Row::Person(person) => person.nick.clone(),
            })
            .collect();
        assert_eq!(
            text,
            [
                "Owners (1/5)",
                "asterix",
                "Administrators (1/5)",
                "bot",
                "Participants (3/5)",
                "Romeo",
                "amy",
                "zed"
            ]
        );
    }

    fn ready() -> AutoState {
        AutoState { wide: true, group: true, joined: true, live: true, enabled: true, ..Default::default() }
    }

    #[test]
    fn the_pane_opens_by_itself_for_a_joined_room_on_a_wide_window() {
        assert_eq!(auto_action(&ready()), Auto::Open);
        for (name, state) in [
            ("narrow window", AutoState { wide: false, ..ready() }),
            ("not a room", AutoState { group: false, ..ready() }),
            ("not joined yet", AutoState { joined: false, ..ready() }),
            ("signed out", AutoState { live: false, ..ready() }),
            ("closed by the user", AutoState { enabled: false, ..ready() }),
        ] {
            assert_eq!(auto_action(&state), Auto::Keep, "{name}");
        }
    }

    #[test]
    fn other_panes_are_never_replaced_but_a_stale_room_list_is_refreshed() {
        let busy = AutoState { sidebar_shown: true, ..ready() };
        assert_eq!(auto_action(&busy), Auto::Keep, "Conversation Info or Shared Media stays open");
        let same_room = AutoState { showing_participants: true, ..busy };
        assert_eq!(auto_action(&same_room), Auto::Keep);
        let other_room = AutoState { sidebar_shown: true, showing_participants: true, other_room: true, ..ready() };
        assert_eq!(auto_action(&other_room), Auto::Open, "follow the chat to the new room");
    }

    #[test]
    fn only_an_automatic_pane_hides_when_the_window_narrows_or_the_chat_is_not_a_room() {
        let shown = |auto| AutoState { sidebar_shown: true, showing_participants: true, showing_auto: auto, ..ready() };
        assert_eq!(auto_action(&AutoState { wide: false, ..shown(true) }), Auto::Close);
        assert_eq!(auto_action(&AutoState { group: false, ..shown(true) }), Auto::Close);
        assert_eq!(auto_action(&AutoState { wide: false, ..shown(false) }), Auto::Keep, "the user opened it");
        let other = AutoState { wide: false, sidebar_shown: true, showing_auto: true, ..ready() };
        assert_eq!(auto_action(&other), Auto::Keep, "a different pane is not ours to close");
    }

    #[test]
    fn hostile_names_cannot_hide_reorder_or_pose_as_the_user() {
        let mut own = person("Romeo", "member", true);
        own.nick = "Romeo".into();
        let all = vec![person("Mallory (You)", "member", false), person("zed\u{202e}", "member", false), own];
        let rows = rows(&all, "");
        let names: Vec<String> = rows
            .iter()
            .filter_map(|row| match row {
                Row::Person(p) => Some(crate::text_safety::display_nick(&p.nick, p.own)),
                _ => None,
            })
            .collect();
        assert_eq!(names, ["Romeo", "Mallory [You]", "zed"], "the user first, lookalikes neutralised, direction marks gone");
        // Searching for "you" does not surface the lookalike as if it were the user's own row.
        assert!(super::rows(&all, "(you)").is_empty());
        assert_eq!(super::rows(&all, "[you]").len(), 2, "heading and the one person who really has that name");
    }

    #[test]
    fn invite_addresses_are_split_validated_and_deduplicated() {
        assert_eq!(
            parse_addresses("b@example.org, a@example.org  b@example.org\n@bad nodomain c@"),
            ["a@example.org", "b@example.org"]
        );
        assert!(parse_addresses("  ,  ").is_empty());
    }

    #[test]
    fn availability_is_named_for_assistive_technology() {
        assert_eq!(presence_label(""), "Online");
        assert_eq!(presence_label("away"), "Away");
        assert_eq!(presence_label("xa"), "Away");
        assert_eq!(presence_label("dnd"), "Busy");
    }

    #[test]
    fn search_matches_a_disclosed_address_too() {
        let mut who = person("amy", "member", false);
        who.jid = Some("amy.smith@example.org".into());
        let all = vec![who, person("bob", "member", false)];
        assert_eq!(rows(&all, "smith").len(), 2);
        assert!(rows(&all, "smith").iter().any(|row| matches!(row, Row::Person(p) if p.nick == "amy")));
    }

    #[test]
    fn search_hides_empty_sections_but_keeps_the_room_total() {
        let all = vec![person("asterix", "owner", false), person("amy", "member", false)];
        let rows = rows(&all, " AM ");
        assert_eq!(
            rows,
            vec![
                Row::Heading("Participants (1/2)".into()),
                Row::Person(person("amy", "member", false))
            ]
        );
        assert!(super::rows(&all, "nobody").is_empty());
    }
}
