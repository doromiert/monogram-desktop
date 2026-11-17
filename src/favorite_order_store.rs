//! Favorite ordering storage and sync merge, free of any widget code so the transport test can reuse it.
use rusqlite::{Connection, OptionalExtension, params};
use std::{
    collections::{HashMap, HashSet},
};

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum Kind {
    Emoji,
    Stickers,
    Gifs,
}
impl Kind {
    fn preference(self) -> &'static str {
        match self {
            Self::Emoji => "picker:favorites:emoji",
            Self::Stickers => "picker:favorites:stickers",
            Self::Gifs => "picker:favorites:gifs",
        }
    }
}
#[derive(Default)]
pub(crate) struct Order {
    keys: Vec<String>,
    ranks: HashMap<String, usize>,
}
impl Order {
    fn new(keys: Vec<String>) -> Self {
        let ranks = keys
            .iter()
            .enumerate()
            .map(|(i, key)| (key.clone(), i))
            .collect();
        Self { keys, ranks }
    }
    pub(crate) fn sort<T>(&self, items: &mut [T], key: impl Fn(&T) -> String) {
        items.sort_by_cached_key(|item| self.ranks.get(&key(item)).copied().unwrap_or(usize::MAX));
    }
}
pub(crate) fn emoji_key(emoji: &str) -> String {
    format!(
        "emoji:{}",
        crate::message_actions::canonical_emoji(emoji).unwrap_or_else(|| emoji.into())
    )
}
pub(crate) fn pack_key(owner: &str, id: i64) -> String {
    format!("pack:{owner}:{id}")
}
pub(crate) fn gif_key(owner: &str, id: &str) -> String {
    format!("gif:{owner}\n{id}")
}
pub(crate) fn read(db: &Connection, kind: Kind) -> rusqlite::Result<Order> {
    let saved: Option<String> = db
        .query_row(
            "SELECT value FROM preferences WHERE key=?",
            [kind.preference()],
            |row| row.get(0),
        )
        .optional()?;
    let keys = match saved {
        None => Vec::new(),
        Some(value) => {
            if value.len() > 4 * 1024 * 1024 {
                return Err(rusqlite::Error::InvalidQuery);
            }
            serde_json::from_str::<Vec<String>>(&value)
                .map_err(|_| rusqlite::Error::InvalidQuery)?
        }
    };
    let mut seen = HashSet::new();
    Ok(Order::new(
        keys.into_iter()
            .filter(|key| !key.is_empty() && key.len() <= 16384 && seen.insert(key.clone()))
            .collect(),
    ))
}
pub(crate) fn sort_saved<T>(db:&Connection,kind:Kind,items:&mut [T],key:impl Fn(&T)->String)->rusqlite::Result<()> {
    read(db,kind)?.sort(items,key);Ok(())
}
pub(crate) fn import_synced_order(db:&Connection,kind:Kind,owner:&str,keys:Vec<String>)->rusqlite::Result<()> {import_synced_order_at(db,kind,owner,keys,None).map(|_|())}
pub(crate) fn import_synced_order_at(db:&Connection,kind:Kind,owner:&str,keys:Vec<String>,stamp:Option<u64>)->rusqlite::Result<bool> {
    let tx=rusqlite::Transaction::new_unchecked(db,rusqlite::TransactionBehavior::Immediate)?;
    if !crate::emoji_preferences::picker_import_allowed(&tx,stamp)?{return Ok(false);}
    let old=read(&tx,kind)?;let prefix=format!("pack:{owner}:");let mut incoming=keys.into_iter();
    let mut merged=old.keys.into_iter().filter_map(|key|if key.starts_with("emoji:")||key.starts_with(&prefix){incoming.next()}else{Some(key)}).collect::<Vec<_>>();merged.extend(incoming);
    let mut seen=HashSet::new();merged.retain(|key|seen.insert(key.clone()));
    let json=serde_json::to_string(&merged).map_err(|_|rusqlite::Error::InvalidQuery)?;
    tx.execute("INSERT INTO preferences(key,value) VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value",params![kind.preference(),json])?;tx.commit()?;Ok(true)
}
pub(crate) fn load(db: &Connection) -> rusqlite::Result<HashMap<Kind, Order>> {
    [Kind::Emoji, Kind::Stickers, Kind::Gifs]
        .into_iter()
        .map(|kind| read(db, kind).map(|order| (kind, order)))
        .collect()
}
pub(crate) fn save_move(
    db: &Connection,
    kind: Kind,
    mut actual: Vec<String>,
    source: &str,
    target: &str,
) -> rusqlite::Result<Order> {
    let transaction = db.unchecked_transaction()?;
    let old = read(&transaction, kind)?;
    let mut unique = HashSet::new();
    actual.retain(|key| key.len() <= 16384 && unique.insert(key.clone()));
    old.sort(&mut actual, Clone::clone);
    let from = actual
        .iter()
        .position(|key| key == source)
        .ok_or(rusqlite::Error::InvalidQuery)?;
    let to = actual
        .iter()
        .position(|key| key == target)
        .ok_or(rusqlite::Error::InvalidQuery)?;
    let moved = actual.remove(from);
    actual.insert(to, moved);
    // Keep slots belonging to other accounts; their favorites are absent from this view.
    let mut ordered = actual.iter();
    let mut keys: Vec<_> = old
        .keys
        .into_iter()
        .map(|key| {
            if unique.contains(&key) {
                ordered.next().unwrap().clone()
            } else {
                key
            }
        })
        .collect();
    keys.extend(ordered.cloned());
    let value = serde_json::to_string(&keys).map_err(|_| rusqlite::Error::InvalidQuery)?;
    if value.len() > 4 * 1024 * 1024 {
        return Err(rusqlite::Error::InvalidQuery);
    }
    transaction.execute("INSERT INTO preferences(key,value) VALUES(?,?) ON CONFLICT(key) DO UPDATE SET value=excluded.value",params![kind.preference(),value])?;
    crate::emoji_preferences::touch_picker(&transaction)?;
    transaction.commit()?;
    Ok(Order::new(keys))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn synced_mixed_order_preserves_other_accounts_and_remaps_local_items() {
        let db=Connection::open_in_memory().unwrap();db.execute_batch("CREATE TABLE preferences(key TEXT PRIMARY KEY,value TEXT)").unwrap();
        import_synced_order(&db,Kind::Emoji,"other",vec!["emoji:❤️".into(),"pack:other:2".into(),"pack:mine:1".into()]).unwrap();
        import_synced_order(&db,Kind::Emoji,"mine",vec!["pack:mine:999".into(),"emoji:☠️".into()]).unwrap();
        assert_eq!(read(&db,Kind::Emoji).unwrap().keys,["pack:mine:999","pack:other:2","emoji:☠️"]);
    }
    #[test]
    fn order_moves_preserve_hidden_accounts_and_never_change_membership() {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE preferences(key TEXT PRIMARY KEY,value TEXT)")
            .unwrap();
        let items = vec![
            "emoji:❤️".into(),
            "pack:first:1".into(),
            "pack:other:2".into(),
            "emoji:😅".into(),
        ];
        let first = save_move(&db, Kind::Emoji, items.clone(), &items[0], &items[3]).unwrap();
        assert_eq!(
            first.keys,
            ["pack:first:1", "pack:other:2", "emoji:😅", "emoji:❤️"]
        );
        let second = save_move(
            &db,
            Kind::Emoji,
            vec![items[0].clone(), items[1].clone(), items[3].clone()],
            &items[0],
            &items[1],
        )
        .unwrap();
        assert_eq!(
            second.keys,
            ["emoji:❤️", "pack:other:2", "pack:first:1", "emoji:😅"]
        );
        assert!(save_move(&db, Kind::Emoji, items.clone(), "not-a-favorite", &items[0]).is_err());
        assert_eq!(read(&db, Kind::Emoji).unwrap().keys, second.keys);
        assert!(read(&db, Kind::Stickers).unwrap().keys.is_empty());
    }
}
