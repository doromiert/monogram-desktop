//! One-time upgrade to the interaction defaults requested for both Monogram clients.
use crate::storage::Store;
use rusqlite::params;
impl Store {
    fn migrate_room_trust_defaults(&self, owner: &str) -> rusqlite::Result<()> {
        let marker = format!("room-trust-defaults-v1:{owner}");
        if self.pref(&marker, "") == "1" { return Ok(()); }
        let tx = self.db.unchecked_transaction()?;
        let prefix = format!("blind-trust:{owner}\n");
        tx.execute("UPDATE preferences SET value='true' WHERE substr(key,1,?)=? AND EXISTS(SELECT 1 FROM chats WHERE account=? AND peer=substr(key,?) AND COALESCE(json_extract(body,'$.group'),0)=1)",params![prefix.len() as i64,&prefix,owner,prefix.len() as i64+1])?;
        self.set_pref(&marker, "1")?;
        tx.commit()
    }
    pub fn migrate_chat_defaults(&self,owner:&str)->rusqlite::Result<()> {
        if owner.is_empty() {return Ok(());}
        self.migrate_room_trust_defaults(owner)?;
        if self.pref(&format!("chat-defaults-v1:{owner}"),"")=="1" {return Ok(());}
        let tx=self.db.unchecked_transaction()?;
        for key in [format!("read_receipts:{owner}"),format!("link-previews-enabled:{owner}"),format!("link-previews-encrypted:{owner}"),format!("typing:{owner}")] {self.set_pref(&key,"true")?;}
        let prefix=format!("blind-trust:{owner}\n");
        tx.execute("UPDATE preferences SET value='true' WHERE substr(key,1,?)=? AND NOT EXISTS(SELECT 1 FROM chats WHERE account=? AND peer=substr(key,?) AND COALESCE(json_extract(body,'$.group'),0)=1)",params![prefix.len() as i64,&prefix,owner,prefix.len() as i64+1])?;
        let revision=self.read_receipts(owner).revision.saturating_add(1);
        self.set_pref(&format!("read-receipts-revision:{owner}"),&revision.to_string())?;
        tx.execute("DELETE FROM displayed_pending WHERE account=?",[owner])?;
        self.set_pref(&format!("chat-defaults-v1:{owner}"),"1")?;
        tx.commit()
    }
}
#[cfg(test)] mod tests {
 use super::*;
 #[test]fn room_trust_upgrade_is_independent_and_keeps_later_opt_outs(){
  let store=Store::open(true).unwrap();let owner="alice@example.invalid";let other="other@example.invalid";let room="friends@conference.example.invalid";let contact="bob@example.invalid";
  store.save_account(&crate::model::Account{jid:owner.into(),host:String::new(),port:5222,direct_tls:false}).unwrap();
  let state=crate::model::State::live(owner,vec![crate::model::Chat{id:"room".into(),jid:room.into(),group:true,..Default::default()}]);store.save(&state).unwrap();
  store.set_pref(&format!("chat-defaults-v1:{owner}"),"1").unwrap();
  store.set_blind_trust(owner,room,false).unwrap();store.set_blind_trust(owner,contact,false).unwrap();store.set_blind_trust(other,room,false).unwrap();
  store.migrate_chat_defaults(owner).unwrap();assert!(store.blind_trust(owner,room).unwrap());assert!(!store.blind_trust(owner,contact).unwrap());assert!(!store.blind_trust(other,room).unwrap());assert!(store.blind_trust(owner,"new@conference.example.invalid").unwrap());
  store.set_blind_trust(owner,room,false).unwrap();store.migrate_chat_defaults(owner).unwrap();assert!(!store.blind_trust(owner,room).unwrap());
 }
 #[test]fn upgrades_once_and_then_keeps_user_choices(){
  let store=Store::open(true).unwrap();let owner="alice@example.invalid";let peer="bob@example.invalid";
  store.set_pref(&format!("read_receipts:{owner}"),"false").unwrap();store.set_pref(&format!("link-previews-enabled:{owner}"),"false").unwrap();store.set_blind_trust(owner,peer,false).unwrap();
  store.migrate_chat_defaults(owner).unwrap();assert!(store.read_receipts(owner).enabled);assert!(store.blind_trust(owner,peer).unwrap());assert_eq!(store.pref(&format!("link-previews-enabled:{owner}"),""),"true");
  store.set_read_receipts(owner,false).unwrap();store.set_blind_trust(owner,peer,false).unwrap();store.set_pref(&format!("typing:{owner}"),"false").unwrap();store.migrate_chat_defaults(owner).unwrap();
  assert!(!store.read_receipts(owner).enabled);assert!(!store.blind_trust(owner,peer).unwrap());assert_eq!(store.pref(&format!("typing:{owner}"),""),"false");
 }
}
