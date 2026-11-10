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
    pub id: String,
    pub author: String,
    pub text: String,
    pub time: String,
    pub own: bool,
    pub kind: String,
    pub encrypted: bool,
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
            id: new_id(),
            author: String::new(),
            text: String::new(),
            time: Local::now().format("%H:%M").to_string(),
            own: false,
            kind: "text".into(),
            encrypted: false,
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
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Chat {
    pub encryption: bool,
    pub id: String,
    pub name: String,
    pub subtitle: String,
    pub avatar: String,
    pub color: String,
    pub unread: u32,
    pub group: bool,
    pub messages: Vec<Message>,
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
        out.sort_by_key(|c| !c.pinned);
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
