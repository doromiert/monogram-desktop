//! One-time upgrade to the interaction defaults requested for both Monogram clients.
use crate::storage::Store;
use rusqlite::params;
impl Store {
    pub fn migrate_chat_defaults(&self,owner:&str)->rusqlite::Result<()> {
        if owner.is_empty() || self.pref(&format!("chat-defaults-v1:{owner}"),"")=="1" {return Ok(());}
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
 #[test]fn upgrades_once_and_then_keeps_user_choices(){
  let store=Store::open(true).unwrap();let owner="alice@example.invalid";let peer="bob@example.invalid";
  store.set_pref(&format!("read_receipts:{owner}"),"false").unwrap();store.set_pref(&format!("link-previews-enabled:{owner}"),"false").unwrap();store.set_blind_trust(owner,peer,false).unwrap();
  store.migrate_chat_defaults(owner).unwrap();assert!(store.read_receipts(owner).enabled);assert!(store.blind_trust(owner,peer).unwrap());assert_eq!(store.pref(&format!("link-previews-enabled:{owner}"),""),"true");
  store.set_read_receipts(owner,false).unwrap();store.set_blind_trust(owner,peer,false).unwrap();store.set_pref(&format!("typing:{owner}"),"false").unwrap();store.migrate_chat_defaults(owner).unwrap();
  assert!(!store.read_receipts(owner).enabled);assert!(!store.blind_trust(owner,peer).unwrap());assert_eq!(store.pref(&format!("typing:{owner}"),""),"false");
 }
}
