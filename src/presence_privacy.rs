//! Privacy options for what this client looks up and shows about other people's presence.
//!
//! They are stored per account like the other privacy switches (`key:account`) and all default to
//! on. They only govern this client: what contacts can see of you is not changed by any of them
//! (this client does not publish an idle time, and a server answers last-activity questions on
//! your behalf).
use crate::ui::Ui;
use std::rc::Rc;

/// Look up when contacts were last online (XEP-0012).
pub const LAST_SEEN: &str = "show_last_seen";
/// Show the status circle, away/do-not-disturb and idle time from contacts' presence (XEP-0319).
pub const CONTACT_STATUS: &str = "show_contact_status";
/// Show hats (XEP-0317) in rooms that support them.
pub const HATS: &str = "show_hats";

/// Key, title and description of each switch, as listed in Preferences.
pub const OPTIONS: [(&str, &str, &str); 3] = [
    (
        LAST_SEEN,
        "Show when contacts were last seen",
        "Ask your server when a contact was last online, only for contacts who share their presence with you. Nothing is sent to the contact",
    ),
    (
        CONTACT_STATUS,
        "Show contacts’ status and idle time",
        "A colored circle for online, away, do not disturb and unknown, and how long a contact has been idle. This only changes what you see",
    ),
    (
        HATS,
        "Show hats in rooms",
        "Titles such as Host or Moderator that a room gives its members, in rooms that support them. This only changes what you see",
    ),
];

impl Ui {
    pub(crate) fn shows_last_seen(&self) -> bool {
        self.preference(LAST_SEEN, true)
    }
    pub(crate) fn shows_contact_status(&self) -> bool {
        self.preference(CONTACT_STATUS, true)
    }
    pub(crate) fn shows_hats(&self) -> bool {
        self.preference(HATS, true)
    }

    /// An option changed: redraw everything that depends on one, now, without a restart.
    pub(crate) fn presence_privacy_changed(self: &Rc<Self>) {
        self.update_chat_subtitle();
        self.refresh_profile_card();
        self.queue_chat_refresh();
        let room = self.state.borrow().current().jid.clone();
        self.refresh_message_hats(&room);
        let view = self.participants.borrow().clone();
        if let Some(view) = view {
            let people = self.room_rosters.borrow().get(&view.room).cloned().unwrap_or_default();
            view.update(people);
        }
        // Switching last seen back on asks again at once rather than waiting for the next chat.
        if self.shows_last_seen() {
            self.refresh_last_activity_current();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_options_are_distinct_described_and_say_they_only_change_what_you_see() {
        let keys: std::collections::HashSet<_> = OPTIONS.iter().map(|(key, ..)| *key).collect();
        assert_eq!(keys.len(), OPTIONS.len(), "no two switches share a key");
        assert_eq!(OPTIONS.map(|(key, ..)| key), [LAST_SEEN, CONTACT_STATUS, HATS]);
        for (key, title, description) in OPTIONS {
            assert!(!key.contains(':') && !key.contains(char::is_whitespace), "{key}: it is joined with the account");
            assert!(!title.is_empty() && !title.ends_with('.'), "{title}");
            assert!(description.len() > 40, "{key} explains itself");
        }
        // Status and hats only change the display; last seen says what is (not) sent and to whom.
        assert!(OPTIONS[1].2.contains("only changes what you see"));
        assert!(OPTIONS[2].2.contains("only changes what you see"));
        assert!(OPTIONS[0].2.contains("Nothing is sent to the contact"));
        assert!(OPTIONS[0].2.contains("share their presence with you"));
    }
}
