//! Timeline rows are a cache. Keep the visible viewport and roughly three screens on either side.
use crate::{model::Message, storage::Store};
use rusqlite::params;
use std::{
    cell::{Cell, RefCell},
    ops::Range,
    time::Instant,
};

pub const PAGE: u32 = 36;
pub const SCREENS: f64 = 3.0;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Side {
    Older,
    Newer,
}

#[derive(Default)]
pub struct HistoryWindow {
    revision: Cell<u64>,
    content_revision: Cell<u64>,
    pub scheduled: Cell<bool>,
    pub moving: Cell<bool>,
    pub highlighted: RefCell<Option<(String, Instant)>>,
    pending: Cell<bool>,
    older_end: Cell<bool>,
    newer_end: Cell<bool>,
}

impl HistoryWindow {
    pub fn reset(&self, newest: bool) {
        self.revision.set(self.revision.get().wrapping_add(1));
        self.pending.set(false);
        self.older_end.set(false);
        self.newer_end.set(newest);
    }

    pub fn content_revision(&self) -> u64 {
        self.content_revision.get()
    }

    pub fn content_changed(&self) {
        self.content_revision
            .set(self.content_revision.get().wrapping_add(1));
    }

    pub fn busy(&self) -> bool {
        self.pending.get()
    }

    pub fn at_end(&self, side: Side) -> bool {
        match side {
            Side::Older => self.older_end.get(),
            Side::Newer => self.newer_end.get(),
        }
    }

    pub fn newest(&self) -> bool {
        self.newer_end.get()
    }

    pub fn has_older_gap(&self) -> bool {
        !self.older_end.get()
    }

    pub fn invalidate_older(&self) {
        self.older_end.set(false);
    }

    pub fn begin(&self) -> Option<u64> {
        if self.pending.replace(true) {
            return None;
        }
        Some(self.revision.get())
    }

    pub fn finish(&self, revision: u64) -> bool {
        if self.revision.get() != revision {
            return false;
        }
        self.pending.set(false);
        true
    }

    pub fn end(&self, side: Side, value: bool) {
        match side {
            Side::Older => self.older_end.set(value),
            Side::Newer => self.newer_end.set(value),
        }
    }
}

/// Use measured GTK allocations, so a tall image counts for more than a line of text.
pub fn retained(rows: &[Range<f64>], viewport: Range<f64>) -> Range<usize> {
    let height = viewport.end - viewport.start;
    if height <= 0.0 {
        return 0..rows.len();
    }
    let lower = viewport.start - SCREENS * height;
    let upper = viewport.end + SCREENS * height;
    let first = rows.partition_point(|row| row.end < lower);
    let end = rows.partition_point(|row| row.start <= upper);
    first..end.max(first)
}

impl Store {
    pub fn history_side(
        &self,
        owner: &str,
        peer: &str,
        boundary: (&str, &str),
        side: Side,
        limit: u32,
    ) -> rusqlite::Result<Vec<Message>> {
        if side == Side::Older {
            return self.try_history(owner, peer, Some(boundary), limit);
        }
        let mut q = self.db.prepare("SELECT body FROM messages WHERE account=? AND peer=? AND (timestamp,id)>(?,?) AND COALESCE(json_extract(body,'$.retracted'),0)=0 ORDER BY timestamp,id LIMIT ?")?;
        let rows = q.query_map(params![owner, peer, boundary.0, boundary.1, limit], |r| {
            r.get::<_, String>(0)
        })?;
        Ok(rows
            .collect::<rusqlite::Result<Vec<_>>>()?
            .into_iter()
            .filter_map(|body| Message::decode(&body).ok())
            .collect())
    }

    pub fn history_around(
        &self,
        owner: &str,
        peer: &str,
        id: &str,
    ) -> rusqlite::Result<Vec<Message>> {
        let Some(target) = self
            .message(owner, peer, id)
            .filter(|message| !message.retracted)
        else {
            return Ok(Vec::new());
        };
        let boundary = (target.timestamp.as_str(), target.id.as_str());
        let mut before = self.history_side(owner, peer, boundary, Side::Older, PAGE / 2)?;
        let after = self.history_side(owner, peer, boundary, Side::Newer, PAGE / 2)?;
        before.push(target);
        before.extend(after);
        Ok(before)
    }

    pub fn history_ids_between(
        &self,
        owner: &str,
        peer: &str,
        first: &str,
        last: &str,
    ) -> rusqlite::Result<Vec<String>> {
        let Some(first) = self.message(owner, peer, first) else {
            return Ok(Vec::new());
        };
        let Some(last) = self.message(owner, peer, last) else {
            return Ok(Vec::new());
        };
        let lo = first.order_key().min(last.order_key());
        let hi = first.order_key().max(last.order_key());
        let mut q = self.db.prepare("SELECT id FROM messages WHERE account=? AND peer=? AND (timestamp,id)>=(?,?) AND (timestamp,id)<=(?,?) AND COALESCE(json_extract(body,'$.retracted'),0)=0 ORDER BY timestamp,id")?;
        q.query_map(params![owner, peer, lo.0, lo.1, hi.0, hi.1], |row| {
            row.get::<_, String>(0)
        })?
        .collect()
    }

    pub fn search_archive(
        &self,
        owner: &str,
        query: &str,
    ) -> rusqlite::Result<Vec<(String, Message)>> {
        let mut statement = self.db.prepare("SELECT peer,body FROM messages WHERE account=? AND COALESCE(json_extract(body,'$.retracted'),0)=0 ORDER BY timestamp DESC,id DESC")?;
        let mut rows = statement.query([owner])?;
        let mut results = Vec::new();
        while let Some(row) = rows.next()? {
            let body: String = row.get(1)?;
            let Ok(message) = Message::decode(&body) else {
                continue;
            };
            if message.content_text().to_lowercase().contains(query) {
                results.push((row.get(0)?, message));
                if results.len() == 100 {
                    break;
                }
            }
        }
        Ok(results)
    }

    pub fn resolve_reply(
        &self,
        owner: &str,
        peer: &str,
        to: &str,
        reference: &str,
    ) -> Option<Message> {
        if reference.is_empty() {
            return None;
        }
        let to = to.split('/').next().unwrap_or_default();
        if !to.is_empty() && to != owner && to != peer {
            return None;
        }
        // action_refs keeps multiple IDs for collisions; a legacy ambiguous reply must never jump arbitrarily.
        let mut q = self.db.prepare("SELECT DISTINCT message FROM action_refs WHERE account=?1 AND peer=?2 AND value=?3 AND (?4 IS NULL OR own=?4) UNION SELECT DISTINCT message FROM aliases WHERE account=?1 AND peer=?2 AND alias=?5 AND (?4 IS NULL OR own=?4) LIMIT 2").ok()?;
        let own = (!to.is_empty()).then_some(to == owner);
        let ids = q
            .query_map(
                params![owner, peer, reference, own, format!("server:{reference}")],
                |row| row.get::<_, String>(0),
            )
            .ok()?
            .collect::<rusqlite::Result<Vec<_>>>()
            .ok()?;
        if ids.len() == 1 {
            self.message(owner, peer, &ids[0])
        } else if ids.is_empty() {
            self.message(owner, peer, reference)
                .filter(|message| own.is_none_or(|own| own == message.own))
        } else {
            None
        }
    }
}
