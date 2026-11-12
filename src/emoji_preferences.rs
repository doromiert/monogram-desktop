//! Device-wide emoji preferences, shared by composition and reactions as on mobile.
use crate::{message_actions::canonical_emoji, storage::Store};
use rusqlite::{Connection, OptionalExtension, params};
use std::collections::HashSet;

const FAVORITES: &str = "emoji:favorites";
const PINNED: &str = "emoji:pinned";
const USAGE: &str = "emoji:usage";
const RECENT_LIMIT: usize = 48;
pub const DEFAULT_REACTIONS: [&str; 8] = ["👍", "❤️", "😂", "😮", "😢", "🙏", "🔥", "👎"];

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EmojiPreferences {
    pub favorites: Vec<String>,
    pub pinned: Vec<String>,
    pub recent: Vec<String>,
}
fn canonical_list(values: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut seen = HashSet::new();
    values
        .into_iter()
        .filter_map(|value| canonical_emoji(&value))
        .filter(|value| seen.insert(value.clone()))
        .collect()
}
fn read(db: &Connection, key: &str) -> rusqlite::Result<Vec<String>> {
    let value: Option<String> = db
        .query_row("SELECT value FROM preferences WHERE key=?", [key], |row| {
            row.get(0)
        })
        .optional()?;
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let values: Vec<String> =
        serde_json::from_str(&value).map_err(|_| rusqlite::Error::InvalidQuery)?;
    Ok(canonical_list(values))
}
fn usage(db: &Connection) -> rusqlite::Result<Vec<(String, u64)>> {
    let raw: Option<String> = db
        .query_row(
            "SELECT value FROM preferences WHERE key=?",
            [USAGE],
            |row| row.get(0),
        )
        .optional()?;
    let values: Vec<(String, u64)> = if let Some(raw) = raw {
        if raw.len() > 16 * 1024 {
            return Err(rusqlite::Error::InvalidQuery);
        }
        serde_json::from_str(&raw).map_err(|_| rusqlite::Error::InvalidQuery)?
    } else {
        let defaults: Vec<String> = serde_json::from_str(include_str!(
            "../assets/icons/mobile-emoji/default-recent.json"
        ))
        .map_err(|_| rusqlite::Error::InvalidQuery)?;
        let count = defaults.len();
        defaults
            .into_iter()
            .enumerate()
            .map(|(i, emoji)| (emoji, (count - i) as u64))
            .collect()
    };
    let mut seen = HashSet::new();
    let mut result: Vec<_> = values
        .into_iter()
        .filter_map(|(emoji, count)| canonical_emoji(&emoji).map(|emoji| (emoji, count)))
        .filter(|(emoji, _)| seen.insert(emoji.clone()))
        .collect();
    result.sort_by(|a, b| b.1.cmp(&a.1));
    result.truncate(RECENT_LIMIT);
    Ok(result)
}
impl EmojiPreferences {
    pub fn quick_reactions(&self, current: &[String]) -> Vec<String> {
        canonical_list(
            self.pinned
                .iter()
                .chain(&self.favorites)
                .cloned()
                .chain(DEFAULT_REACTIONS.iter().map(|value| (*value).to_owned()))
                .chain(
                    current
                        .iter()
                        .filter_map(|value| value.split_whitespace().next())
                        .map(str::to_owned),
                ),
        )
    }
}
impl Store {
    pub fn emoji_preferences(&self) -> rusqlite::Result<EmojiPreferences> {
        Ok(EmojiPreferences {
            favorites: read(&self.db, FAVORITES)?,
            pinned: read(&self.db, PINNED)?,
            recent: usage(&self.db)?
                .into_iter()
                .map(|(emoji, _)| emoji)
                .collect(),
        })
    }
    pub fn record_emoji_recent(&self, emoji: &str) -> rusqlite::Result<()> {
        let emoji = canonical_emoji(emoji).ok_or(rusqlite::Error::InvalidQuery)?;
        let tx = self.db.unchecked_transaction()?;
        let mut entries = usage(&tx)?;
        let count = entries
            .iter()
            .find(|(value, _)| *value == emoji)
            .map(|(_, n)| *n)
            .unwrap_or(0)
            .saturating_add(1);
        entries.retain(|(value, _)| *value != emoji);
        if entries.len() == RECENT_LIMIT {
            entries.pop();
        }
        entries.insert(0, (emoji, count));
        entries.sort_by(|a, b| b.1.cmp(&a.1));
        let json = serde_json::to_string(&entries).map_err(|_| rusqlite::Error::InvalidQuery)?;
        tx.execute("INSERT INTO preferences(key,value) VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value",params![USAGE,json])?;
        tx.commit()
    }
    pub fn set_emoji_favorite(&self, emoji: &str, favorite: bool) -> rusqlite::Result<()> {
        self.set_emoji_preference(FAVORITES, emoji, favorite)
    }
    pub fn set_emoji_pinned(&self, emoji: &str, pinned: bool) -> rusqlite::Result<()> {
        self.set_emoji_preference(PINNED, emoji, pinned)
    }
    fn set_emoji_preference(&self, key: &str, emoji: &str, enabled: bool) -> rusqlite::Result<()> {
        let emoji = canonical_emoji(emoji).ok_or(rusqlite::Error::InvalidQuery)?;
        let transaction = self.db.unchecked_transaction()?;
        let mut values = read(&transaction, key)?;
        if enabled {
            if !values.contains(&emoji) {
                values.push(emoji);
            }
        } else {
            values.retain(|value| value != &emoji);
        }
        let value = serde_json::to_string(&values).map_err(|_| rusqlite::Error::InvalidQuery)?;
        transaction.execute("INSERT INTO preferences(key,value) VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value", params![key,value])?;
        transaction.commit()
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn global_preferences_canonical_order_and_independent_pin_favorite_updates() {
        let db = Store::open(true).unwrap();
        db.set_emoji_favorite("❤", true).unwrap();
        db.set_emoji_favorite("❤️", true).unwrap();
        db.set_emoji_favorite("🐢", true).unwrap();
        db.set_emoji_pinned("🔥", true).unwrap();
        assert!(db.set_emoji_favorite("not an emoji", true).is_err());
        let default_recent = db.emoji_preferences().unwrap().recent;
        assert_eq!(default_recent.first().map(String::as_str), Some("😂"));
        for _ in 0..40 {
            db.record_emoji_recent("🔥").unwrap();
        }
        assert_eq!(
            db.emoji_preferences()
                .unwrap()
                .recent
                .first()
                .map(String::as_str),
            Some("🔥")
        );
        assert!(db.record_emoji_recent("not an emoji").is_err());
        for value in 0x1f600..0x1f660 {
            if let Some(value) = char::from_u32(value) {
                let _ = db.record_emoji_recent(&value.to_string());
            }
        }
        assert_eq!(db.emoji_preferences().unwrap().recent.len(), 48);
        let preferences = db.emoji_preferences().unwrap();
        assert_eq!(preferences.favorites, ["❤️", "🐢"]);
        let quick = preferences.quick_reactions(&["🦊 2".into(), "❤ 3".into()]);
        assert_eq!(&quick[..3], ["🔥", "❤️", "🐢"]);
        assert_eq!(
            quick.iter().filter(|emoji| emoji.as_str() == "❤️").count(),
            1
        );
        assert_eq!(quick.last().unwrap(), "🦊");
        db.set_emoji_favorite("❤️", false).unwrap();
        assert_eq!(db.emoji_preferences().unwrap().pinned, ["🔥"]);
        assert_eq!(db.emoji_preferences().unwrap().favorites, ["🐢"]);
        db.save_account(&crate::model::Account {
            jid: "owner@example.org".into(),
            host: String::new(),
            port: 5222,
            direct_tls: false,
        })
        .unwrap();
        db.forget("owner@example.org").unwrap();
        assert_eq!(db.emoji_preferences().unwrap().favorites, ["🐢"]);
    }
}
