//! Local, bounded failure history; never part of the transmitted message.
use crate::storage::Store;
use rusqlite::params;

pub struct Failure {
    pub time: String,
    pub stage: String,
    pub detail: String,
}

pub(crate) fn safe_log_detail(value:&str)->String {
    safe_detail(value).split_whitespace().map(|word|{
        if word.contains('@') {"[address]"} else if word.contains('/') || word.starts_with('~') {"[path]"} else {word}
    }).collect::<Vec<_>>().join(" ").chars().take(512).collect()
}

pub fn safe_detail(value: &str) -> String {
    let bounded: String = value.chars().take(4096).collect();
    let visible = crate::text_safety::visible(&bounded, 2048);
    let mut credentials = false;
    let redacted = visible.split_whitespace().map(|word| {
        let lower = word.to_ascii_lowercase();
        credentials |= ["authorization:", "cookie:", "password=", "password:", "token=", "token:", "secret=", "secret:", "key=", "key:", "bearer"]
            .iter().any(|label| lower.contains(label));
        if credentials || word.contains("://") || word.contains(['<', '>'])
            || (word.len() >= 64 && word.bytes().all(|b| b.is_ascii_hexdigit()))
        {
            "[redacted]"
        } else { word }
    }).collect::<Vec<_>>().join(" ");
    crate::text_safety::visible(&redacted, 2048)
}

impl Store {
    pub fn init_message_failures(&self) -> rusqlite::Result<()> {
        self.db.execute_batch("CREATE TABLE IF NOT EXISTS message_failures(seq INTEGER PRIMARY KEY,account TEXT NOT NULL,peer TEXT NOT NULL,id TEXT NOT NULL,time TEXT NOT NULL,stage TEXT NOT NULL,detail TEXT NOT NULL,FOREIGN KEY(account,peer,id) REFERENCES messages(account,peer,id) ON DELETE CASCADE);CREATE INDEX IF NOT EXISTS message_failure_lookup ON message_failures(account,peer,id,seq);")
    }
    pub fn record_message_failure(&self, owner: &str, peer: &str, wire: &str, stage: &str, detail: &str) -> rusqlite::Result<bool> {
        let Some(id) = self.own_wire_id(owner, peer, wire)? else { return Ok(false) };
        let tx = self.db.unchecked_transaction()?;
        tx.execute("INSERT INTO message_failures(account,peer,id,time,stage,detail) VALUES(?,?,?,?,?,?)",
            params![owner, peer, id, crate::model::now(), crate::text_safety::visible(stage, 80), safe_detail(detail)])?;
        tx.execute("DELETE FROM message_failures WHERE account=?1 AND peer=?2 AND id=?3 AND seq NOT IN(SELECT seq FROM message_failures WHERE account=?1 AND peer=?2 AND id=?3 ORDER BY seq DESC LIMIT 8)", params![owner, peer, id])?;
        tx.commit()?;
        Ok(true)
    }
    pub fn message_failures(&self, owner: &str, peer: &str, id: &str) -> rusqlite::Result<Vec<Failure>> {
        let mut query = self.db.prepare("SELECT time,stage,detail FROM message_failures WHERE account=? AND peer=? AND id=? ORDER BY seq DESC LIMIT 8")?;
        query.query_map(params![owner,peer,id], |row| Ok(Failure { time:row.get(0)?, stage:row.get(1)?, detail:row.get(2)? }))?.collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn private_transport_values_are_redacted_but_status_and_condition_survive() {
        let value = safe_detail("HTTP 403 forbidden https://files.invalid/a?token=private#key Authorization: Bearer hidden");
        assert!(value.contains("HTTP 403 forbidden"));
        assert!(!value.contains("private") && !value.contains("hidden") && !value.contains("files.invalid"));
        assert_eq!(safe_detail("remote-server-timeout: unavailable [invalid: PQ attachment]"), "remote-server-timeout: unavailable [invalid: PQ attachment]");
        assert!(!safe_detail(&"f".repeat(96)).contains(&"f".repeat(64)));
        assert!(safe_detail(&"a ".repeat(5000)).chars().count() <= 2048);
    }
    #[test]
    fn failures_survive_retry_are_bounded_and_are_deleted_with_the_message() {
        use crate::model::*;
        let store = Store::open(true).unwrap();
        let owner = "owner@fixture.invalid";
        let peer = "peer@fixture.invalid";
        store.save_account(&Account { jid:owner.into(), host:String::new(), port:5222, direct_tls:false }).unwrap();
        let mut state = State::live(owner, vec![]);
        let message = Message { id:"local".into(), wire_id:"wire".into(), own:true, ..Default::default() };
        state.chat_for(peer).messages.push(message);
        store.save(&state).unwrap();
        for index in 0..12 {
            assert!(store.record_message_failure(owner,peer,"wire","XMPP response",&format!("forbidden attempt {index}")).unwrap());
        }
        let records = store.message_failures(owner,peer,"local").unwrap();
        assert_eq!(records.len(),8);
        assert_eq!(records[0].detail,"forbidden attempt 11");
        store.save(&state).unwrap();
        store.update_delivery(owner,peer,"wire","sent").unwrap();
        assert_eq!(store.message_failures(owner,peer,"local").unwrap().len(),8);
        assert!(!store.record_message_failure(owner,peer,"unknown","Send","unknown").unwrap());
        store.delete(owner,peer,"local").unwrap();
        assert!(store.message_failures(owner,peer,"local").unwrap().is_empty());
    }
}
