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
    pub upload: Option<UploadDraft>,
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
    pub fingerprint: String,
    pub reply: String,
    pub reply_id: String,
    pub reply_to: String,
    pub reactions: Vec<String>,
    pub my_reactions: BTreeSet<String>,
    pub pinned: bool,
    pub timestamp: String,
    pub delivery: String,
    pub wire_id: String,
    pub aliases: Vec<String>,
    pub options: Vec<(String, u32)>,
    pub gif_index: usize,
    pub unavailable: bool,
}
impl Default for Message {
    fn default() -> Self {
        Self {
            upload: None,
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
            fingerprint: String::new(),
            reply: String::new(),
            reply_id: String::new(),
            reply_to: String::new(),
            reactions: vec![],
            my_reactions: BTreeSet::new(),
            pinned: false,
            timestamp: now(),
            delivery: "local".into(),
            wire_id: String::new(),
            aliases: vec![],
            options: vec![],
            gif_index: 0,
            unavailable: false,
        }
    }
}
impl Message {
    pub fn mode(&self) -> Mode {
        if !self.encrypted {
            Mode::Plain
        } else if self.encryption_mode == Some(Mode::Legacy) {
            Mode::Legacy
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
                format!("📎 {}", upload.name)
            } else {
                self.text.clone()
            };
        }
        if let Some(file) = &self.attachment {
            return if self.text.is_empty() {
                format!("📎 {}", file.name)
            } else {
                self.text.clone()
            };
        }
        self.poll
            .as_ref()
            .map(|p| format!("📊 {}", p.question))
            .unwrap_or_else(|| self.text.clone())
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
    pub sending_preference: Option<Preference>,
    pub automatic_encryption: Option<Selection>,
    pub id: String,
    pub name: String,
    pub subtitle: String,
    pub avatar: String,
    pub color: String,
    pub unread: u32,
    pub group: bool,
    #[serde(skip)]
    pub room_joined: bool,
    #[serde(skip)]
    pub room_can_send: bool,
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
}
impl Chat {
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
        })
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
        out.sort_by(|a, b| {
            b.pinned.cmp(&a.pinned).then_with(|| {
                if self.account.is_empty() || a.pinned {
                    return std::cmp::Ordering::Equal;
                }
                b.latest_key().cmp(&a.latest_key())
            })
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
                subtitle: "Offline".into(),
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
}
#[cfg(test)]
mod tests {
    use super::*;
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

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct UploadDraft {
    pub uri: String,
    pub name: String,
    pub error: Option<String>,
}
impl std::fmt::Debug for UploadDraft {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UploadDraft")
            .field("failed", &self.error.is_some())
            .finish_non_exhaustive()
    }
}
