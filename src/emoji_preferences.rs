//! Device-wide emoji preferences, shared by composition and reactions as on mobile.
use crate::{message_actions::canonical_emoji, storage::Store};
use rusqlite::{Connection, OptionalExtension, params};
use std::collections::HashSet;

const FAVORITES: &str = "emoji:favorites";
const PINNED: &str = "emoji:pinned";
pub const DEFAULT_REACTIONS: [&str; 8] = ["👍", "❤️", "😂", "😮", "😢", "🙏", "🔥", "👎"];

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EmojiPreferences {
    pub favorites: Vec<String>,
    pub pinned: Vec<String>,
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
        })
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
