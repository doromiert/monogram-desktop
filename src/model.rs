use crate::encryption_policy::{Mode, Preference, Selection};
use chrono::{Local, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use uuid::Uuid;

pub fn new_id() -> String {
    Uuid::new_v4().simple().to_string()
}
pub fn now() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Micros, false)
}
fn yes() -> bool {
    true
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Message {
    pub formatting: Option<crate::formatting::Markup>,
    pub markable_id: String,
    #[serde(default)]
    pub inline_emoji: Option<crate::packs::InlineEmoji>,
    #[serde(default)]
    pub preview_spoilers: Option<crate::spoilers::Context>,
    pub upload: Option<UploadDraft>,
    pub upload_caption_for: String,
    pub attachment: Option<crate::file_share::Attachment>,
    pub forwarded: Option<crate::forwarding::Attribution>,
    pub unsupported_attachment: bool,
    pub action: Option<crate::message_actions::Action>,
    pub poll: Option<crate::polls::Poll>,
    pub poll_source: crate::polls::Source,
    pub poll_tally: Option<crate::polls::Tally>,
    pub poll_closed_at: String,
    pub poll_fallback: bool,
    pub sender: String,
    pub edited: bool,
    pub edit_history: Vec<String>,
    pub retracted: bool,
    pub unread: bool,
    pub id: String,
    pub author: String,
    pub text: String,
    pub time: String,
    pub own: bool,
    pub kind: String,
    pub encrypted: bool,
    pub encryption_mode: Option<Mode>,
    pub ciphertext: String,
    pub legacy_room_sender: Option<crate::rooms::members::RoomSender>,
    pub fingerprint: String,
    pub reply: String,
    pub reply_id: String,
    pub reply_to: String,
    pub reactions: Vec<String>,
    pub my_reactions: BTreeSet<String>,
    pub pinned: bool,
    pub timestamp: String,
    pub delivery: String,
    pub outbox_pending: bool,
    pub wire_id: String,
    pub aliases: Vec<String>,
    pub options: Vec<(String, u32)>,
    pub gif_index: usize,
    pub unavailable: bool,
}
impl Default for Message {
    fn default() -> Self {
        Self {
            formatting: None,
            preview_spoilers: Some(crate::spoilers::Context::clear()),
            inline_emoji: None,
            markable_id: String::new(),
            upload: None,
            upload_caption_for: String::new(),
            attachment: None,
            forwarded: None,
            unsupported_attachment: false,
            action: None,
            poll: None,
            poll_source: crate::polls::Source::None,
            poll_tally: None,
            poll_closed_at: String::new(),
            poll_fallback: false,
            sender: String::new(),
            edited: false,
            edit_history: vec![],
            retracted: false,
            unread: false,
            id: new_id(),
            author: String::new(),
            text: String::new(),
            time: Local::now().format("%H:%M").to_string(),
            own: false,
            kind: "text".into(),
            encrypted: false,
            encryption_mode: None,
            ciphertext: String::new(),
            legacy_room_sender: None,
            fingerprint: String::new(),
            reply: String::new(),
            reply_id: String::new(),
            reply_to: String::new(),
            reactions: vec![],
            my_reactions: BTreeSet::new(),
            pinned: false,
            timestamp: now(),
            delivery: "local".into(),
            outbox_pending: false,
            wire_id: String::new(),
            aliases: vec![],
            options: vec![],
            gif_index: 0,
            unavailable: false,
        }
    }
}
impl Message {
    pub fn reply_preview(&self) -> String {
        let preview = self.preview();
        let Some((end, _)) = preview.char_indices().nth(80) else {
            return preview;
        };
        let prefix = &preview[..end];
        let boundary = prefix.rfind(char::is_whitespace);
        let at_boundary = preview[end..]
            .chars()
            .next()
            .is_some_and(char::is_whitespace);
        let word_start = boundary.map_or(0, |index| {
            index + preview[index..].chars().next().unwrap().len_utf8()
        });
        let word_end = preview[end..]
            .find(char::is_whitespace)
            .map_or(preview.len(), |index| end + index);
        let long_word = preview[word_start..word_end].chars().count() > 80;
        let keep = if at_boundary || long_word {
            prefix
        } else {
            boundary.map_or(prefix, |index| &preview[..index])
        };
        format!("{}…", keep.trim_end())
    }
    pub fn mode(&self) -> Mode {
        if !self.encrypted {
            Mode::Plain
        } else if self.encryption_mode == Some(Mode::Legacy) {
            Mode::Legacy
        } else if self.encryption_mode == Some(Mode::Pq) {
            Mode::Pq
        } else {
            Mode::Omemo2
        }
    }
    pub fn set_encryption(&mut self, mode: Mode, fingerprint: String) {
        self.encrypted = mode.encrypted();
        self.encryption_mode = Some(mode);
        self.fingerprint = fingerprint;
    }
    pub fn poll_source(&self) -> crate::polls::Source {
        if self.poll_source == crate::polls::Source::None && self.poll.is_some() {
            crate::polls::Source::Structured
        } else {
            self.poll_source
        }
    }
    pub fn hydrate_compatibility(&mut self) {
        if !self.unavailable
            && !self.retracted
            && self.forwarded.is_none()
            && !matches!(
                self.action,
                Some(
                    crate::message_actions::Action::React { .. }
                        | crate::message_actions::Action::Retract { .. }
                )
            )
            && let Some((attribution, body)) = crate::forwarding::parse(&self.text)
        {
            self.forwarded = Some(attribution);
            self.text = body;
            if self.poll.is_none()
                && self.poll_source == crate::polls::Source::Text
                && !self.unsupported_attachment
            {
                self.poll_source = crate::polls::Source::None;
            }
        }
        if self.poll.is_none() && crate::forwarding::key_uri(&self.text) {
            self.unsupported_attachment = true;
        }
        if self.poll_source() != crate::polls::Source::None
            || self.unavailable
            || self.retracted
            || self.unsupported_attachment
            || !matches!(self.kind.as_str(), "text" | "correct")
            || matches!(
                self.action,
                Some(
                    crate::message_actions::Action::React { .. }
                        | crate::message_actions::Action::Retract { .. }
                )
            )
        {
            return;
        }
        self.poll_source = crate::polls::Source::Text;
        if let Some(poll) = crate::legacy_polls::parse(&self.text) {
            self.poll = Some(poll);
            self.poll_source = crate::polls::Source::Legacy;
            self.poll_fallback = true;
            self.kind = "poll".into();
        }
    }
    pub fn decode(body: &str) -> Result<Self, serde_json::Error> {
        let mut message: Self = serde_json::from_str(body)?;
        message.hydrate_compatibility();
        Ok(message)
    }
    pub fn preview(&self) -> String {
        if let Some(upload) = &self.upload {
            return if self.text.is_empty() {
                if upload.sticker { "Sticker".into() } else { format!("📎 {}", upload.name) }
            } else {
                crate::formatting::preview(self)
            };
        }
        if let Some(file) = &self.attachment {
            return if self.text.is_empty() {
                if file.sticker { "Sticker".into() } else { format!("📎 {}", file.name) }
            } else {
                crate::formatting::preview(self)
            };
        }
        self.poll
            .as_ref()
            .map(|p| format!("📊 {}", p.question))
            .unwrap_or_else(|| crate::formatting::preview(self))
    }
    pub fn content_text(&self) -> String {
        if self.poll_source() == crate::polls::Source::Legacy {
            return self.text.clone();
        }
        self.poll
            .as_ref()
            .map(|p| {
                if self.text.is_empty() {
                    p.readable_body()
                } else {
                    format!("{}\n\n{}", p.readable_body(), self.text)
                }
            })
            .unwrap_or_else(|| self.text.clone())
    }
}
#[derive(Clone, Debug)]
pub struct MessagePreview {
    pub id: String,
    pub timestamp: String,
    pub time: String,
    pub text: String,
    pub own: bool,
}
impl From<&Message> for MessagePreview {
    fn from(message: &Message) -> Self {
        Self {
            id: message.id.clone(),
            timestamp: message.timestamp.clone(),
            time: message.time.clone(),
            text: message.preview().chars().take(160).collect(),
            own: message.own,
        }
    }
}
impl Message {
    pub fn order_key(&self) -> (&str, &str) {
        (&self.timestamp, &self.id)
    }
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Chat {
    pub encryption: bool,
    /// Sticky: set the first time encryption is used here and never cleared by an automatic
    /// choice. A room that has been encrypted must not silently fall back to plaintext because a
    /// server says it no longer offers encryption.
    pub encrypted_history: bool,
    pub sending_preference: Option<Preference>,
    pub automatic_encryption: Option<Selection>,
    pub id: String,
    pub name: String,
    #[serde(skip)]
    pub local_name: String,
    pub subtitle: String,
    pub avatar: String,
    pub color: String,
    pub unread: u32,
    pub group: bool,
    #[serde(skip)]
    pub room_joined: bool,
    #[serde(skip)]
    pub room_can_send: bool,
    pub room_autojoin: Option<bool>,
    pub room_nick: Option<String>,
    pub messages: Vec<Message>,
    #[serde(skip)]
    pub latest: Option<MessagePreview>,
    #[serde(default = "yes")]
    pub contact: bool,
    pub bot: bool,
    pub channel: bool,
    pub archived: bool,
    pub notifications: String,
    pub unread_mentions: u32,
    pub unread_mark: bool,
    pub jid: String,
    pub pinned: bool,
    pub draft: String,
    pub draft_reply: String,
    pub draft_formatting: Option<crate::formatting::Markup>,
}
impl Chat {
    pub fn display_name(&self) -> &str { if self.local_name.is_empty() { &self.name } else { &self.local_name } }
    pub fn encryption_preference(&self) -> Preference {
        self.sending_preference.unwrap_or(if self.encryption {
            Preference::Omemo2
        } else {
            Preference::Automatic
        })
    }
    pub fn selected_encryption(&self) -> Option<Mode> {
        self.encryption_preference().forced().or_else(|| {
            self.automatic_encryption
                .and_then(|selection| selection.current(Utc::now().timestamp_millis()))
                // A cached "plain" must not outlive the server's say-so for a room that has used
                // encryption: probe again instead of trusting it.
                .filter(|mode| !(self.group && *mode == Mode::Plain && self.had_encryption()))
        })
    }
    /// Whether encryption was ever used in this chat: the sticky flag, the current setting, or an
    /// own encrypted message that is loaded.
    pub fn had_encryption(&self) -> bool {
        self.encrypted_history
            || self.encryption
            || self.messages.iter().any(|m| m.own && m.mode().encrypted())
    }
    /// Whether the automatic choice `mode` would be a downgrade to plaintext in a room that has
    /// used encryption. Such a send is refused; the user must choose Unencrypted explicitly.
    pub fn downgrade_blocked(&self, mode: Mode) -> bool {
        self.group && mode == Mode::Plain && self.had_encryption()
    }
    /// Record an automatic choice. Only call this for a choice that is allowed.
    pub fn remember_selection(&mut self, mode: Mode) {
        self.encrypted_history |= self.had_encryption() || mode.encrypted();
        self.sending_preference = Some(Preference::Automatic);
        self.automatic_encryption = Some(Selection {
            mode,
            time_ms: Utc::now().timestamp_millis(),
        });
        self.encryption = mode.encrypted();
    }
    pub fn latest_key(&self) -> Option<(&str, &str)> {
        let loaded = self
            .messages
            .iter()
            .filter(|m| !m.retracted)
            .map(Message::order_key)
            .max();
        loaded.max(
            self.latest
                .as_ref()
                .map(|m| (m.timestamp.as_str(), m.id.as_str())),
        )
    }
    pub fn latest_preview(&self) -> Option<MessagePreview> {
        let newest = self.latest_key()?;
        self.messages
            .iter()
            .find(|m| !m.retracted && m.order_key() == newest)
            .map(MessagePreview::from)
            .or_else(|| self.latest.clone())
    }
    pub fn remember_latest(&mut self) {
        self.latest = self.latest_preview();
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Folder {
    pub icon:String,
    pub pinned:Vec<String>,
    pub types: BTreeSet<String>,
    pub exclude_muted: bool,
    pub exclude_read: bool,
    pub exclude_archived: bool,
    pub include: BTreeSet<String>,
    pub exclude: BTreeSet<String>,
}
impl Folder {
    pub fn matches(&self, c: &Chat) -> bool {
        if self.exclude.contains(&c.id) {
            return false;
        }
        if self.include.contains(&c.id) {
            return true;
        }
        if self.exclude_archived && c.archived {
            return false;
        }
        if c.unread_mentions == 0
            && ((self.exclude_muted
                && !matches!(c.notifications.as_str(), "all" | "mentions" | ""))
                || (self.exclude_read && c.unread == 0 && !c.unread_mark))
        {
            return false;
        }
        self.types.contains(if c.channel {
            "channels"
        } else if c.group {
            "groups"
        } else if c.bot {
            "bots"
        } else if c.contact {
            "contacts"
        } else {
            "non_contacts"
        })
    }
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Story {
    pub id: String,
    pub jid: String,
    pub url: String,
    pub mime: String,
    pub published: i64,
    pub owner: String,
    pub asset: String,
    pub caption: String,
    pub seen: bool,
    pub own: bool,
    pub liked: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Account {
    pub jid: String,
    pub host: String,
    pub port: u16,
    pub direct_tls: bool,
}
#[derive(Clone, Debug, Deserialize)]
pub struct Fixtures {
    pub chats: Vec<Chat>,
    pub folders: BTreeMap<String, Folder>,
    pub folder_order: Vec<String>,
    pub stories: Vec<Story>,
    pub requests: Vec<(String, String)>,
}
#[derive(Clone, Debug)]
pub struct State {
    pub account: String,
    pub chats: Vec<Chat>,
    pub active: String,
    pub folder: String,
    pub folders: BTreeMap<String, Folder>,
    pub folder_order: Vec<String>,
    pub all_folder_hidden: bool,
    pub stories: Vec<Story>,
    pub requests: Vec<(String, String)>,
    pub wallets: Vec<String>,
    pub wallet_index: usize,
    pub mode: String,
}
impl State {
    pub fn demo() -> Self {
        let f: Fixtures =
            serde_json::from_str(include_str!("../data/fixtures.json")).expect("bundled fixtures");
        Self {
            account: String::new(),
            active: f.chats[0].id.clone(),
            chats: f.chats,
            folder: "All".into(),
            folders: f.folders,
            folder_order: f.folder_order,
            all_folder_hidden: false,
            stories: f.stories,
            requests: f.requests,
            wallets: vec!["Everyday".into(), "Savings".into()],
            wallet_index: 0,
            mode: "PQ OMEMO2".into(),
        }
    }
    pub fn live(account: &str, mut chats: Vec<Chat>) -> Self {
        let mut state = Self::demo();
        state.account = account.into();
        state.mode = "Unencrypted".into();
        state.stories.clear();
        state.requests.clear();
        if chats.is_empty() {
            chats.push(Chat {
                id: "saved".into(),
                jid: account.into(),
                name: "Saved Messages".into(),
                subtitle: account.into(),
                color: "blue".into(),
                notifications: "all".into(),
                ..Default::default()
            })
        }
        for c in &mut chats {
            if !c.group && c.jid != account {
                c.subtitle.clear();
            }
            for m in &mut c.messages {
                if m.delivery == "sending" {
                    m.delivery = "uncertain".into()
                }
            }
        }
        state.active = chats[0].id.clone();
        state.chats = chats;
        state
    }
    pub fn current(&self) -> &Chat {
        self.chats
            .iter()
            .find(|c| c.id == self.active)
            .expect("active chat")
    }
    pub fn current_mut(&mut self) -> &mut Chat {
        self.chats
            .iter_mut()
            .find(|c| c.id == self.active)
            .expect("active chat")
    }
    pub fn filtered(&self, query: &str) -> Vec<Chat> {
        let q = query.to_lowercase();
        let mut out: Vec<_> = self
            .chats
            .iter()
            .filter(|c| {
                self.folders.get(&self.folder).is_none_or(|f| f.matches(c))
                    && (q.is_empty()
                        || c.name.to_lowercase().contains(&q)
                        || c.messages
                            .iter()
                            .any(|m| m.text.to_lowercase().contains(&q)))
            })
            .cloned()
            .collect();
        let folder_pins=self.folders.get(&self.folder).map(|f|f.pinned.as_slice()).unwrap_or(&[]);
        out.sort_by(|a, b| {
            let ap=folder_pins.iter().position(|id|id==&a.id).unwrap_or(usize::MAX);let bp=folder_pins.iter().position(|id|id==&b.id).unwrap_or(usize::MAX);
            ap.cmp(&bp).then_with(||b.pinned.cmp(&a.pinned).then_with(|| {
                if self.account.is_empty() || a.pinned {
                    return std::cmp::Ordering::Equal;
                }
                b.latest_key().cmp(&a.latest_key())
            }))
        });
        out
    }
    pub fn chat_for(&mut self, peer: &str) -> &mut Chat {
        if !self.chats.iter().any(|c| c.jid == peer) {
            self.chats.push(Chat {
                id: peer.into(),
                jid: peer.into(),
                name: peer.into(),
                color: "blue".into(),
                subtitle: String::new(),
                notifications: "all".into(),
                ..Default::default()
            })
        }
        self.chats.iter_mut().find(|c| c.jid == peer).unwrap()
    }
    pub fn message(&self, id: &str) -> Option<&Message> {
        self.current().messages.iter().find(|m| m.id == id)
    }
    pub fn send(&mut self, text: &str, kind: &str) -> Option<String> {
        if text.trim().is_empty() || (!self.account.is_empty() && kind != "text") {
            return None;
        }
        let mut m = Message {
            author: "You".into(),
            sender: self.account.clone(),
            text: text.trim().into(),
            own: true,
            kind: kind.into(),
            encrypted: if self.account.is_empty() {
                self.mode != "Unencrypted"
            } else {
                self.current().encryption
            },
            ..Default::default()
        };
        if !self.account.is_empty() {
            m.delivery = "sending".into();
            m.wire_id = m.id.clone();
            m.aliases = vec![format!("wire:{}", m.id), format!("origin:{}", m.id)]
        }
        m.hydrate_compatibility();
        let id = m.id.clone();
        self.current_mut().messages.push(m);
        Some(id)
    }
    pub fn reorder(&mut self, name: &str, index: usize) {
        if let Some(old) = self.folder_order.iter().position(|n| n == name) {
            let name = self.folder_order.remove(old);
            let at = index.min(self.folder_order.len());
            self.folder_order.insert(at, name)
        }
    }
    pub fn folder_visible(&self, name: &str) -> bool {
        self.folder_order.iter().any(|folder| folder == name)
            && if name == "All" {
                !self.all_folder_hidden
            } else {
                self.folders.contains_key(name)
            }
    }
    pub fn visible_folder_names(&self) -> Vec<String> {
        self.folder_order
            .iter()
            .filter(|name| self.folder_visible(name))
            .cloned()
            .collect()
    }
    pub fn reorder_visible(&mut self, name: &str, index: usize) -> bool {
        let mut visible = self.visible_folder_names();
        let Some(old) = visible.iter().position(|folder| folder == name) else {
            return false;
        };
        let at = index.min(visible.len() - 1);
        if old == at {
            return false;
        }
        let folder = visible.remove(old);
        visible.insert(at, folder);
        let mut reordered = visible.into_iter();
        // Only replace visible slots: hidden All keeps its exact raw position.
        let slots: Vec<_> = self
            .folder_order
            .iter()
            .enumerate()
            .filter_map(|(index, folder)| self.folder_visible(folder).then_some(index))
            .collect();
        for slot in slots {
            self.folder_order[slot] = reordered.next().unwrap();
        }
        true
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn folder_visible_reordering_preserves_hidden_all_position() {
        let mut state = State::demo();
        state.folder_order = vec!["Groups".into(), "All".into(), "Unread".into()];
        assert!(state.reorder_visible("All", 2));
        assert_eq!(state.folder_order, ["Groups", "Unread", "All"]);
        state.all_folder_hidden = true;
        assert!(!state.folder_visible("All"));
        assert!(!state.reorder_visible("All", 0));
        assert!(state.reorder_visible("Unread", 0));
        assert_eq!(state.folder_order, ["Unread", "Groups", "All"]);
        assert_eq!(state.visible_folder_names(), ["Unread", "Groups"]);
        state.all_folder_hidden = false;
        assert_eq!(state.visible_folder_names(), ["Unread", "Groups", "All"]);
        state.folder_order = vec!["Groups".into(), "All".into(), "Unread".into()];
        state.all_folder_hidden = true;
        assert!(state.reorder_visible("Unread", 0));
        assert_eq!(state.folder_order, ["Unread", "All", "Groups"]);
        assert!(!state.reorder_visible("missing", 0));
        assert!(!state.reorder_visible("Unread", 0));
    }
    #[test]
    fn filters_and_order() {
        let mut s = State::demo();
        s.folder = "Groups".into();
        assert!(s.filtered("").iter().all(|c| c.group));
        s.reorder("Groups", 0);
        assert_eq!(s.folder_order[0], "Groups");
    }
    #[test]
    fn rules_override_and_mentions() {
        let s = State::demo();
        let mut c = s.chats[0].clone();
        c.notifications = "forever".into();
        c.unread = 0;
        c.unread_mentions = 1;
        let mut f = s.folders["Unread"].clone();
        f.exclude_muted = true;
        assert!(f.matches(&c));
        c.archived = true;
        assert!(!f.matches(&c));
        f.include.insert(c.id.clone());
        assert!(f.matches(&c));
        f.exclude.insert(c.id.clone());
        assert!(!f.matches(&c));
    }
    #[test]
    fn live_and_demo_isolation() {
        let mut a = State::demo();
        let b = State::demo();
        a.send("one", "text");
        assert_ne!(a.current().messages.len(), b.current().messages.len());
        let live = State::live("me@example.org", vec![]);
        assert_eq!(live.chats.len(), 1);
        assert!(live.stories.is_empty());
    }
}

#[cfg(test)]
mod encryption_downgrade_tests {
    use super::*;

    fn room() -> Chat {
        Chat { group: true, jid: "room@conference.example.org".into(), ..Default::default() }
    }
    fn own_message(mode: Mode) -> Message {
        let mut message = Message { own: true, ..Default::default() };
        message.set_encryption(mode, "fingerprint".into());
        message
    }

    #[test]
    fn a_room_that_never_used_encryption_may_be_plain_but_every_other_room_may_not_drop_to_it() {
        assert!(!room().downgrade_blocked(Mode::Plain), "a public room is plain by nature");
        let mut sticky = room();
        sticky.encrypted_history = true;
        let mut current = room();
        current.encryption = true;
        let mut sent = room();
        sent.messages.push(own_message(Mode::Pq));
        for (name, chat) in [("sticky flag", sticky), ("current setting", current), ("an own encrypted message", sent)] {
            assert!(chat.downgrade_blocked(Mode::Plain), "{name}");
            assert!(!chat.downgrade_blocked(Mode::Pq), "{name}: choosing encryption is never blocked");
            assert!(!chat.downgrade_blocked(Mode::Legacy), "{name}");
        }
        // Someone else's encrypted message says nothing about what this user expects.
        let mut other = room();
        other.messages.push(Message { own: false, encrypted: true, ..Default::default() });
        assert!(!other.downgrade_blocked(Mode::Plain));
    }

    #[test]
    fn one_to_one_chats_keep_their_opportunistic_behaviour() {
        let mut chat = Chat { group: false, encryption: true, encrypted_history: true, ..Default::default() };
        assert!(!chat.downgrade_blocked(Mode::Plain), "this change is about rooms");
        chat.automatic_encryption = Some(Selection { mode: Mode::Plain, time_ms: Utc::now().timestamp_millis() });
        chat.sending_preference = Some(Preference::Automatic);
        assert_eq!(chat.selected_encryption(), Some(Mode::Plain));
    }

    #[test]
    fn the_history_flag_is_sticky_so_a_plain_send_cannot_erase_it() {
        let mut chat = room();
        chat.remember_selection(Mode::Pq);
        assert!(chat.encrypted_history && chat.encryption);
        // Even if the encryption setting is later cleared, the history stays.
        chat.encryption = false;
        assert!(chat.downgrade_blocked(Mode::Plain));
        // Legacy data has no flag but an encryption setting: the first choice records it.
        let mut legacy = room();
        legacy.encryption = true;
        legacy.remember_selection(Mode::Plain);
        assert!(legacy.encrypted_history, "recorded before the setting was overwritten");
        assert!(legacy.downgrade_blocked(Mode::Plain));
    }

    #[test]
    fn a_cached_plain_choice_is_not_trusted_for_a_room_that_used_encryption() {
        let cached_plain = |chat: &mut Chat| {
            chat.sending_preference = Some(Preference::Automatic);
            chat.automatic_encryption = Some(Selection { mode: Mode::Plain, time_ms: Utc::now().timestamp_millis() });
        };
        let mut never = room();
        cached_plain(&mut never);
        assert_eq!(never.selected_encryption(), Some(Mode::Plain), "a never-encrypted room keeps its choice");
        let mut had = room();
        had.encrypted_history = true;
        cached_plain(&mut had);
        assert_eq!(had.selected_encryption(), None, "re-check instead of trusting the cached downgrade");
        // An explicit user choice of Unencrypted still works: that is the override.
        had.sending_preference = Some(Preference::Plain);
        assert_eq!(had.selected_encryption(), Some(Mode::Plain));
    }

    #[test]
    fn the_flag_survives_saving_and_loading_and_old_data_defaults_to_false() {
        let mut chat = room();
        chat.remember_selection(Mode::Legacy);
        let restored: Chat = serde_json::from_str(&serde_json::to_string(&chat).unwrap()).unwrap();
        assert!(restored.encrypted_history);
        let old: Chat = serde_json::from_str(r#"{"jid":"a@b","group":true}"#).unwrap();
        assert!(!old.encrypted_history && !old.had_encryption());
    }
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct UploadDraft {
    pub uri: String,
    pub name: String,
    pub error: Option<String>,
    #[serde(default)]
    pub sticker: bool,
    #[serde(default)]
    pub local_sticker: Option<i64>,
}
impl std::fmt::Debug for UploadDraft {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UploadDraft")
            .field("failed", &self.error.is_some())
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod reply_preview_tests {
    use super::*;
    #[test]
    fn truncation_marks_omission_and_keeps_whole_words_unless_unbreakable() {
        let short = Message {
            text: "already complete".into(),
            ..Default::default()
        };
        assert_eq!(short.reply_preview(), "already complete");
        let source = (0..30)
            .map(|i| format!("word{i}"))
            .collect::<Vec<_>>()
            .join(" ");
        let message = Message {
            text: source.clone(),
            ..Default::default()
        };
        let preview = message.reply_preview();
        assert!(preview.ends_with('…'));
        let kept = preview.trim_end_matches('…');
        assert!(source.starts_with(kept));
        assert!(source[kept.len()..].starts_with(' '));
        let long = Message {
            text: "é".repeat(120),
            ..Default::default()
        };
        let preview = long.reply_preview();
        assert_eq!(preview.chars().count(), 81);
        assert!(preview.ends_with('…'));
    }
}
