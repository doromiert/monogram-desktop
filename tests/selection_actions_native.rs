//! Private native acceptance. No session, credentials, network or contact sends.
use super::*;
use adw::prelude::*;
use std::{
    cell::Cell,
    time::{Duration, Instant},
};

async fn until(mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(12);
    while !condition() {
        assert!(
            Instant::now() < deadline,
            "selection fixture did not settle"
        );
        glib::timeout_future(Duration::from_millis(10)).await;
    }
}

#[test]
#[ignore = "requires private X11/D-Bus/XDG roots"]
fn stored_day_selection_and_deletion_keep_native_ui_responsive() {
    const OWNER: &str = "day-owner@fixture.invalid";
    const PEER: &str = "day-peer@fixture.invalid";
    let app = adw::Application::builder()
        .application_id("com.negzero.SelectionActionsAcceptance")
        .flags(gtk::gio::ApplicationFlags::NON_UNIQUE)
        .build();
    let passed = Rc::new(Cell::new(false));
    let result = passed.clone();
    app.connect_activate(move |app| {
        // File-backed private storage exercises real worker snapshots, not memory fallback.
        let ui=Ui::new(app,false);
        ui.store.save_account(&Account{jid:OWNER.into(),host:String::new(),port:5222,direct_tls:false}).unwrap();
        let mut state=State::live(OWNER,vec![]);
        for n in 0..5000 {
            let id=format!("day-{n:05}");
            let mut m=Message{id:id.clone(),wire_id:format!("wire-{id}"),aliases:vec![format!("wire:wire-{id}")],sender:PEER.into(),text:format!("Stored row {n}"),timestamp:"2026-11-12T12:00:00Z".into(),delivery:"received".into(),..Default::default()};
            m.hydrate_compatibility();state.chat_for(PEER).messages.push(m);
        }
        state.active=state.chat_for(PEER).id.clone();ui.store.save(&state).unwrap();
        state.chat_for(PEER).messages.drain(..4980);ui.activate_state(state);ui.show_main();
        ui.window.set_title(Some("Selection actions acceptance"));ui.window.present();
        let app=app.clone();let result=result.clone();
        glib::spawn_future_local(async move {
            println!("SETUP private file-backed store ready");
            let checks=Cell::new(0u32);
            until(||{checks.set(checks.get()+1);if checks.get()%100==0{println!("SETTLE rendering={} bubbles={} mapped={} scroll_mapped={} welcome={} page={:?} loaded={}",ui.rendering_messages(),ui.bubbles.borrow().len(),ui.window.is_mapped(),ui.scroll.is_mapped(),ui.on_welcome(),ui.pages.visible_child_name(),ui.state.borrow().current().messages.len());}!ui.rendering_messages() && !ui.bubbles.borrow().is_empty()}).await;
            println!("SETTLED initial history");
            glib::timeout_future(Duration::from_millis(150)).await;
            let frames=Rc::new(Cell::new(0u32));let gap=Rc::new(Cell::new(Duration::ZERO));let previous=Rc::new(Cell::new(Instant::now()));
            let frame_count=frames.clone();let maxgap=gap.clone();let last=previous.clone();
            ui.window.add_tick_callback(move|_,_|{let now=Instant::now();maxgap.set(maxgap.get().max(now-last.get()));last.set(now);frame_count.set(frame_count.get()+1);glib::ControlFlow::Continue});

            ui.selected.replace(HashSet::from(["day-04999".into()]));ui.anchor.replace(Some("day-04999".into()));ui.update_selection();
            ui.selection_action("select-day");
            ui.selected.replace(HashSet::from(["day-04998".into()]));ui.anchor.replace(Some("day-04998".into()));ui.update_selection();
            glib::timeout_future(Duration::from_millis(700)).await;
            assert_eq!(*ui.selected.borrow(),HashSet::from(["day-04998".into()]),"late day query overwrote newer manual selection");
            // Missing selected anchor falls back deterministically to a valid selected ID.
            ui.selected.borrow_mut().insert("missing-anchor".into());ui.anchor.replace(Some("missing-anchor".into()));
            let begin=Instant::now();let frame=frames.get();ui.selection_action("select-day");
            until(||frames.get()>frame).await;let feedback=begin.elapsed();
            println!("QUERY awaiting5000 current={}",ui.selected.borrow().len());
            until(||ui.selected.borrow().len()==5000).await;
            assert!(feedback<Duration::from_millis(400));assert!(ui.state.borrow().current().messages.len()<=200,"day selection must retain the bounded viewport cache");assert!(ui.bubbles.borrow().len()<=200,"day selection mounted unloaded rows");
            println!("DAY selected=5000 loaded={} mounted={} first_frame_ms={} max_frame_gap_ms={} stale_selection_preserved=true",ui.state.borrow().current().messages.len(),ui.bubbles.borrow().len(),feedback.as_millis(),gap.get().as_millis());
            let ids=ui.selected.borrow().iter().cloned().collect();
            let prep=ui.prepare_deletion(ids).await.unwrap();assert_eq!(prep.local_count,5000);assert_eq!(prep.own_count,0);
            // A dialog prepared in another account generation must never apply.
            ui.generation.set(ui.generation.get()+1);ui.apply_deletion(prep,false);
            glib::timeout_future(Duration::from_millis(50)).await;assert!(ui.store.message(OWNER,PEER,"day-00000").is_some());
            let prep=ui.prepare_deletion(ui.selected.borrow().iter().cloned().collect()).await.unwrap();
            let begin=Instant::now();let frame=frames.get();ui.apply_deletion(prep,false);
            until(||frames.get()>frame).await;let feedback=begin.elapsed();
            until(||ui.state.borrow().current().messages.is_empty()).await;
            assert!(ui.store.history(OWNER,PEER,None,1).is_empty());assert!(feedback<Duration::from_millis(400));assert!(gap.get()<Duration::from_millis(400));
            println!("DELETE local=5000 first_frame_ms={} max_frame_gap_ms={} context_fence_preserved=true no_transport=true",feedback.as_millis(),gap.get().as_millis());
            result.set(true);app.quit();
        });
    });
    app.run_with_args::<&str>(&[]);
    assert!(passed.get());
}
