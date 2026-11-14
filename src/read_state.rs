use crate::ui::Ui;
use adw::prelude::*;
use std::rc::Rc;

#[derive(Default)]
pub struct ReadState {
    permission: Option<bool>,
    last: Option<(String, String, String, u64)>,
}

impl Ui {
    pub fn read_receipts_enabled(&self) -> bool {
        self.read_ui.borrow().permission.unwrap_or_else(|| {
            self.store
                .read_receipts(&self.state.borrow().account)
                .enabled
        })
    }

    pub fn set_read_receipts(
        self: &Rc<Self>,
        enabled: bool,
    ) -> impl std::future::Future<Output = Result<(), String>> + 'static {
        let owner = self.state.borrow().account.clone();
        let weak = Rc::downgrade(self);
        if !enabled {
            self.read_ui.borrow_mut().permission = Some(false);
            let policy = self.store.read_receipts(&owner);
            self.command(crate::protocol::Command::DisplayedPolicy {
                enabled: false,
                revision: policy.revision,
            });
        }
        async move {
            let Some(ui) = weak.upgrade() else {
                return Err("The account is no longer open".into());
            };
            let account = owner.clone();
            let policy = ui
                .store
                .write_snapshot(move |store| store.set_read_receipts(&account, enabled))
                .await
                .map_err(|_| "Read receipt settings could not be saved".to_owned())?;
            if ui.state.borrow().account != owner {
                return Err("The active account changed".into());
            }
            ui.read_ui.replace(ReadState {
                permission: Some(policy.enabled),
                last: None,
            });
            ui.command(crate::protocol::Command::DisplayedPolicy {
                enabled: policy.enabled,
                revision: policy.revision,
            });
            ui.queue_displayed_marker();
            Ok(())
        }
    }

    fn queue_displayed_marker(self: &Rc<Self>) {
        if self.reading_current_chat() { self.mark_current_chat_read(); }
    }
    pub fn receive_read_marker(self: &Rc<Self>, owner: &str, peer: &str, wire: &str, own: bool) {
        let (owner, peer, wire) = (owner.to_owned(), peer.to_owned(), wire.to_owned());
        let generation = self.generation.get();
        let weak = Rc::downgrade(self);
        gtk::glib::MainContext::default().spawn_local(async move {
            let Some(ui) = weak.upgrade() else { return };
            let (account, address) = (owner.clone(), peer.clone());
            let changed = ui
                .store
                .write_snapshot(move |store| {
                    if own {
                        store.received_own_displayed(&account, &address, &wire)
                    } else {
                        store.received_displayed(&account, &address, &wire)
                    }
                })
                .await;
            if ui.generation.get() != generation || ui.state.borrow().account != owner {
                return;
            }
            let Ok(changed) = changed else { return };
            if changed.is_empty() {
                return;
            }
            let mut state = ui.state.borrow_mut();
            let active = state.current().jid == peer;
            if let Some(chat) = state.chats.iter_mut().find(|chat| chat.jid == peer) {
                for message in changed {
                    if let Some(row) = chat.messages.iter_mut().find(|row| row.id == message.id) {
                        *row = message;
                    }
                }
                chat.unread = ui.store.unread_count(&owner, &peer);
                chat.remember_latest();
            }
            drop(state);
            if active {
                ui.render_messages(false);
            }
            ui.refresh_chats(None);
        });
    }
    pub fn reading_current_chat(&self) -> bool {
        self.window.is_active()
            && self.window.is_mapped()
            && self.timeline.is_mapped()
            && self.pages.visible_child_name().as_deref() == Some("chat")
            && !self.dialogs.borrow().iter().any(|dialog| dialog.is_visible())
            && self.active_media.borrow().is_none()
            && self.active_story.borrow().is_none()
    }
    pub fn mark_visible_chat_read(self: &Rc<Self>) {
        if self.reading_current_chat() { self.mark_current_chat_read(); }
    }
    /// Sending is also an explicit acknowledgement of the current conversation.
    /// The archive write and outgoing marker are coalesced and run off GTK.
    pub fn mark_current_chat_read(self: &Rc<Self>) {
        let mut state=self.state.borrow_mut();
        if state.account.is_empty() { return; }
        let owner=state.account.clone();
        let chat=state.current_mut();
        let Some((time,id))=chat.latest_key().map(|(t,i)|(t.to_owned(),i.to_owned())) else { return; };
        let peer=chat.jid.clone();
        let changed=chat.unread>0 || chat.unread_mark || chat.messages.iter().any(|m|m.unread);
        chat.unread=0;chat.unread_mark=false;
        for message in &mut chat.messages { message.unread=false; }
        drop(state);
        let revision=self.store.read_receipts(&owner).revision;
        let key=(owner.clone(),peer.clone(),id.clone(),revision);
        if !changed && self.read_ui.borrow().last.as_ref()==Some(&key) { return; }
        self.read_ui.borrow_mut().last=Some(key.clone());
        if changed { self.refresh_chats(None); }
        let weak=Rc::downgrade(self);
        gtk::glib::MainContext::default().spawn_local(async move {
            let Some(ui)=weak.upgrade() else { return; };
            let (account,address)=(owner.clone(),peer.clone());
            let result=ui.store.write_snapshot(move |store| {
                store.mark_read_through(&account,&address,&time,&id)?;
                store.mark_latest_displayed(&account,&address,&time,&id)
            }).await;
            if ui.state.borrow().account!=owner { return; }
            match result {
                Ok(Some(candidate)) if ui.read_receipts_enabled() => {
                    ui.command(crate::protocol::Command::Displayed { peer:candidate.peer,id:candidate.id });
                }
                Err(_) => {if ui.read_ui.borrow().last.as_ref()==Some(&key){ui.read_ui.borrow_mut().last=None;}ui.notice("Read status could not be saved. Retrying…");}
                _=>{}
            }
        });
    }
}

#[cfg(test)] mod native_tests {
 use super::*;use crate::model::{Account,Message,State};use gtk::{gio,glib};use std::time::{Duration,Instant};
 fn pump(){let context=glib::MainContext::default();let deadline=Instant::now()+Duration::from_millis(350);while Instant::now()<deadline{context.iteration(false);std::thread::sleep(Duration::from_millis(1));}}
 #[test]#[ignore="requires private GTK display, xdotool and D-Bus"]
 fn focused_history_arrivals_send_acknowledgement_and_typing_subtitle(){
  let app=adw::Application::builder().application_id("com.negzero.MonogramReadTest").flags(gio::ApplicationFlags::NON_UNIQUE).build();app.register(None::<&gio::Cancellable>).unwrap();
  let ui=Ui::new(&app,true);let owner="alice@example.invalid";let peer="bob@example.invalid";
  ui.store.save_account(&Account{jid:owner.into(),host:String::new(),port:5222,direct_tls:false}).unwrap();
  let mut state=State::live(owner,vec![]);state.active=state.chat_for(peer).id.clone();
  for n in 0..100 {state.current_mut().messages.push(Message{id:format!("r{n:03}"),wire_id:format!("w{n}"),markable_id:format!("w{n}"),sender:peer.into(),text:format!("History row {n}\nSecond line"),timestamp:format!("2026-11-14T10:{:02}:{:02}Z",n/60,n%60),..Default::default()});}
  ui.store.save(&state).unwrap();ui.activate_state(ui.store.load_windowed(owner));ui.select_chat(peer);ui.window.set_default_size(900,520);ui.window.present();pump();
  let ids=std::process::Command::new("xdotool").args(["search","--name","^Monogram$"]).output().unwrap();let id=String::from_utf8(ids.stdout).unwrap().lines().last().unwrap().to_owned();assert!(std::process::Command::new("xdotool").args(["windowfocus","--sync",&id]).status().unwrap().success());pump();assert!(ui.window.is_active());
  ui.scroll.vadjustment().set_value(0.);pump();assert!(!ui.at_message_end());assert!(ui.reading_current_chat());
  assert!(ui.receive_message(owner,peer.into(),Message{id:"fresh".into(),wire_id:"fresh-wire".into(),markable_id:"fresh-wire".into(),sender:peer.into(),text:"New while reading older history".into(),timestamp:"2026-11-14T12:00:00Z".into(),..Default::default()},false,None));pump();
  assert_eq!(ui.state.borrow().current().unread,0);assert_eq!(ui.store.unread_count(owner,peer),0);
  assert!(ui.store.pending_displayed(owner).unwrap().iter().any(|c|c.wire=="fresh-wire"));
  ui.backend_event(ui.generation.get(),owner,crate::protocol::BackendEvent::Typing{peer:peer.into(),composing:true});pump();
  fn has_typing(w:&gtk::Widget)->bool{if let Some(label)=w.downcast_ref::<gtk::Label>(){if label.text()=="Typing…"{return true;}}let mut c=w.first_child();while let Some(w)=c{c=w.next_sibling();if has_typing(&w){return true;}}false}
  assert!(has_typing(ui.title.upcast_ref()));
  ui.backend_event(ui.generation.get(),owner,crate::protocol::BackendEvent::Typing{peer:peer.into(),composing:false});pump();assert!(!has_typing(ui.title.upcast_ref()));
  // Sending acknowledges a conversation without requiring the newest row to be on screen.
  ui.state.borrow_mut().current_mut().unread=1;ui.mark_current_chat_read();assert_eq!(ui.state.borrow().current().unread,0);pump();
  eprintln!("Native focused history: incoming cleared immediately, latest marker queued, typing replaced subtitle and restored it");ui.window.close();
 }
}
