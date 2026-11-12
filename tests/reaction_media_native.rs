//! Private GTK/X11 acceptance; all commands are captured in an in-memory sink.
use super::*;
use crate::{downloads, file_share, history_window, message_actions, model, protocol, ui, widgets};
use gtk::{gio, glib};
use std::time::Instant;

fn descendants(root: &impl IsA<gtk::Widget>) -> Vec<gtk::Widget> {
    let mut result = Vec::new();
    let mut pending = vec![root.clone().upcast::<gtk::Widget>()];
    while let Some(widget) = pending.pop() {
        let mut child = widget.first_child();
        while let Some(value) = child {
            child = value.next_sibling();
            pending.push(value);
        }
        result.push(widget);
    }
    result
}
fn pointer(ui: &ui::Ui, widget: &impl IsA<gtk::Widget>, button: &str) {
    let b = widget.compute_bounds(&ui.window).unwrap();
    let x = (b.x() + b.width() / 2.).round().to_string();
    let y = (b.y() + b.height() / 2.).round().to_string();
    assert!(
        std::process::Command::new("xdotool")
            .args([
                "search",
                "--name",
                "Reaction media acceptance",
                "mousemove",
                "--sync",
                "--window",
                "%@",
                &x,
                &y,
                "click",
                button
            ])
            .status()
            .unwrap()
            .success()
    );
}
async fn wait(mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(8);
    while !condition() {
        assert!(
            Instant::now() < deadline,
            "reaction media fixture did not settle"
        );
        glib::timeout_future(Duration::from_millis(10)).await;
    }
}
fn attachment(
    owner: &str,
    url: &str,
    name: &str,
    mime: &str,
    bytes: &[u8],
) -> file_share::Attachment {
    use std::os::unix::fs::PermissionsExt;
    let mut file = file_share::Attachment::bare(url).unwrap();
    file.name = name.into();
    file.mime = mime.into();
    file.size = Some(bytes.len() as u64);
    file.manual = true;
    file.hashes
        .insert("sha-256".into(), openssl::sha::sha256(bytes).to_vec());
    let root = downloads::account_root(owner).unwrap();
    std::fs::create_dir_all(&root).unwrap();
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
    let path = root.join(file.key());
    std::fs::write(&path, bytes).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
    file
}
fn point(ui: &ui::Ui, id: &str) -> f64 {
    ui.bubbles.borrow()[id]
        .compute_bounds(&ui.timeline)
        .unwrap()
        .y() as f64
        - ui.scroll.vadjustment().value()
}
fn feedback(ui: &ui::Ui, button: &impl IsA<gtk::Button>) -> Rc<Cell<Option<Duration>>> {
    let start = Rc::new(Cell::new(None::<Instant>));
    let clicked = start.clone();
    button.connect_clicked(move |_| clicked.set(Some(Instant::now())));
    let measured = Rc::new(Cell::new(None));
    let result = measured.clone();
    ui.window
        .frame_clock()
        .unwrap()
        .connect_after_paint(move |_| {
            if result.get().is_none() {
                if let Some(start) = start.get() {
                    result.set(Some(start.elapsed()));
                }
            }
        });
    measured
}
fn reaction_button(ui: &ui::Ui, id: &str) -> gtk::ToggleButton {
    descendants(&ui.bubbles.borrow()[id])
        .into_iter()
        .filter_map(|widget| widget.downcast::<gtk::ToggleButton>().ok())
        .find(|button| button.has_css_class("reaction"))
        .unwrap()
}
fn remote(ui: &Rc<ui::Ui>, id: &str, emojis: &[&str]) {
    let target = ui.state.borrow().message(id).unwrap().wire_id.clone();
    let wire = model::new_id();
    assert!(ui.receive_message(
        "owner@fixture.invalid",
        "peer@fixture.invalid".into(),
        model::Message {
            wire_id: wire.clone(),
            aliases: vec![format!("wire:{wire}")],
            sender: "peer@fixture.invalid/one".into(),
            action: Some(message_actions::Action::React {
                target,
                emojis: emojis.iter().map(|emoji| (*emoji).to_string()).collect()
            }),
            delivery: "received".into(),
            ..Default::default()
        },
        false,
        None
    ));
}
fn sent(commands: &mut tokio::sync::mpsc::UnboundedReceiver<protocol::Command>) -> usize {
    let mut count = 0;
    while let Ok(command) = commands.try_recv() {
        if matches!(command, protocol::Command::Send { .. }) {
            count += 1;
        }
    }
    count
}
#[test]
#[ignore = "requires a private X11 display, D-Bus, xdotool and media decoders"]
fn reactions_preserve_media_downloads_and_history_state() {
    let app = adw::Application::builder()
        .application_id("com.negzero.ReactionMediaAcceptance")
        .flags(gio::ApplicationFlags::NON_UNIQUE)
        .build();
    let passed = Rc::new(Cell::new(false));
    let result = passed.clone();
    app.connect_activate(move|app|{
 const OWNER:&str="owner@fixture.invalid";const PEER:&str="peer@fixture.invalid";
 let ui=ui::Ui::new(app,true);ui.store.save_account(&model::Account{jid:OWNER.into(),host:"".into(),port:5222,direct_tls:false}).unwrap();
 let pixels=gtk::gdk_pixbuf::Pixbuf::new(gtk::gdk_pixbuf::Colorspace::Rgb,true,8,32,20).unwrap();pixels.fill(0x4488aaff);let png=pixels.save_to_bufferv("png",&[]).unwrap();
 let image=attachment(OWNER,"https://fixture.invalid/image.png","image.png","image/png",&png);let mp4=include_bytes!("fixtures/reaction-video.mp4").to_vec();let video=attachment(OWNER,"https://fixture.invalid/video.mp4","video.mp4","video/mp4",&mp4);
 let mut state=model::State::live(OWNER,vec![]);for n in 0..40 {let file=if n==32{Some(image.clone())}else if n==33{Some(video.clone())}else{None};state.chat_for(PEER).messages.push(model::Message{id:format!("message-{n:02}"),wire_id:format!("wire-{n:02}"),aliases:vec![format!("wire:wire-{n:02}")],sender:format!("{PEER}/one"),author:"Fixture".into(),kind:if file.is_some(){"file"}else{"text"}.into(),text:if file.is_some(){String::new()}else{format!("Text message {n}\nAnother line")},attachment:file,timestamp:format!("2026-11-12T10:{n:02}:00Z"),reactions:vec![],..Default::default()});}for message in &mut state.chat_for(PEER).messages {message.hydrate_compatibility();}state.active=state.chat_for(PEER).id.clone();ui.store.save(&state).unwrap();ui.activate_state(state);ui.downloads.borrow_mut().restore_cached(OWNER,image.clone()).unwrap();ui.downloads.borrow_mut().restore_cached(OWNER,video.clone()).unwrap();
 let (events,_events)=std::sync::mpsc::channel();let mut session=protocol::Session::start(model::Account{jid:"invalid diagnostic jid".into(),host:"".into(),port:5222,direct_tls:false},String::new(),0,events);let(tx,commands)=tokio::sync::mpsc::unbounded_channel();session.tx=tx;ui.session.borrow_mut().replace(session);ui.online.set(true);
 let css=gtk::CssProvider::new();css.load_from_path(crate::widgets::asset("style.css"));gtk::style_context_add_provider_for_display(&gtk::prelude::WidgetExt::display(&ui.window),&css,gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);ui.window.set_title(Some("Reaction media acceptance"));ui.window.set_default_size(1000,1000);ui.window.present();let app=app.clone();let result=result.clone();
 glib::spawn_future_local(async move {
  let mut commands=commands;
  wait(||!ui.rendering_messages()&&ui.bubbles.borrow().contains_key("message-33")&&ui.message_scroll.borrow().is_none()).await;
  ui.history_window.end(history_window::Side::Older,true);ui.history_window.end(history_window::Side::Newer,true);ui.history_pending.borrow_mut().clear();ui.update_history_spinner();
  let top=ui.bubbles.borrow()["message-32"].compute_bounds(&ui.timeline).unwrap().y() as f64;ui.scroll.vadjustment().set_value((top-70.).max(0.));
  wait(||ui.images.borrow_mut().video(&video.key()).is_some_and(|player|player.media.is_prepared())&&descendants(&ui.bubbles.borrow()["message-32"]).iter().any(|w|w.downcast_ref::<widgets::AspectImage>().is_some_and(|image|image.paintable().is_some()))).await;
  let player=ui.images.borrow_mut().video(&video.key()).unwrap();player.expanded.set(true);player.media.pause();player.media.seek(500_000);glib::timeout_future(Duration::from_millis(250)).await;let top=ui.bubbles.borrow()["message-32"].compute_bounds(&ui.timeline).unwrap().y() as f64;ui.scroll.vadjustment().set_value((top-70.).max(0.));glib::timeout_future(Duration::from_millis(150)).await;
  let image_body=ui.bubbles.borrow()["message-32"].clone();let video_body=ui.bubbles.borrow()["message-33"].clone();let image_view=descendants(&image_body).into_iter().find(|w|w.downcast_ref::<widgets::AspectImage>().is_some_and(|image|image.paintable().is_some())).unwrap();let video_view=descendants(&video_body).into_iter().find(|w|w.downcast_ref::<widgets::AspectImage>().is_some_and(|image|image.paintable().is_some())).unwrap();let image_token=ui.downloads.borrow().token(OWNER,&image).unwrap();let video_token=ui.downloads.borrow().token(OWNER,&video).unwrap();let position=player.media.timestamp();
  let flashed=Rc::new(Cell::new(false));let visible=flashed.clone();ui.history.connect_visible_notify(move |spinner|{if spinner.is_visible(){visible.set(true);}});
  println!("TRACE before action bottom_distance={} busy={} pending={} rendering={}",ui.scroll.vadjustment().upper()-ui.scroll.vadjustment().page_size()-ui.scroll.vadjustment().value(),ui.history_window.busy(),ui.history_pending.borrow().len(),ui.rendering_messages());let render_serial=ui.rendered_rows.borrow().serial;let baseline=point(&ui,"message-32");pointer(&ui,&video_body,"3");glib::timeout_future(Duration::from_millis(200)).await;let quick=descendants(&ui.window).into_iter().filter_map(|w|w.downcast::<gtk::Button>().ok()).find(|b|b.has_css_class("reaction-choice")&&b.is_mapped()&&b.label().as_deref()==Some("👍")).unwrap();let painted=feedback(&ui,&quick);let start=Instant::now();pointer(&ui,&quick,"1");wait(||ui.state.borrow().message("message-33").is_some_and(|m|m.my_reactions.contains("👍"))).await;glib::timeout_future(Duration::from_millis(350)).await;
  println!("Local pointer add: elapsed_ms={} spinner_flashed={} body_same={} media_same={} paused={} timestamp_delta={} anchor_delta={:.2}",start.elapsed().as_millis(),flashed.get(),ui.bubbles.borrow()["message-33"]==video_body,descendants(&ui.bubbles.borrow()["message-33"]).contains(&video_view),!player.media.is_playing(),player.media.timestamp()-position,point(&ui,"message-32")-baseline);
  assert_eq!(sent(&mut commands),1,"one actual pointer reaction must enqueue one command");assert!(painted.get().unwrap()<Duration::from_millis(400));println!("ADD first_paint_ms={}",painted.get().unwrap().as_millis());
  let chip=reaction_button(&ui,"message-33");let painted=feedback(&ui,&chip);pointer(&ui,&chip,"1");wait(||ui.state.borrow().message("message-33").unwrap().my_reactions.is_empty()).await;glib::timeout_future(Duration::from_millis(350)).await;assert_eq!(sent(&mut commands),1);assert!(painted.get().unwrap()<Duration::from_millis(400));println!("REMOVE first_paint_ms={} anchor_delta={:.2}",painted.get().unwrap().as_millis(),point(&ui,"message-32")-baseline);
  ui.react("message-33","👍");let chip=reaction_button(&ui,"message-33");glib::timeout_future(Duration::from_millis(60)).await;ui.react("message-33","👍");glib::timeout_future(Duration::from_millis(60)).await;ui.react("message-33","👍");glib::timeout_future(Duration::from_millis(350)).await;assert_eq!(sent(&mut commands),3);assert_eq!(reaction_button(&ui,"message-33"),chip,"rapid reversal must reuse the chip");assert!(ui.state.borrow().message("message-33").unwrap().my_reactions.contains("👍"));println!("RAPID reversal chip identity preserved");
  remote(&ui,"message-33",&["👍"]);glib::timeout_future(Duration::from_millis(350)).await;assert_eq!(reaction_button(&ui,"message-33"),chip);assert!(ui.state.borrow().message("message-33").unwrap().reactions.iter().any(|value|value=="👍 2"));assert!(descendants(&chip).iter().any(|widget|widget.downcast_ref::<gtk::Label>().is_some_and(|label|label.has_css_class("reaction-count")&&label.text()=="2")));assert_eq!(sent(&mut commands),0);println!("REMOTE count update reuses same chip");
  remote(&ui,"message-32",&["❤️"]);glib::timeout_future(Duration::from_millis(350)).await;assert_eq!(ui.bubbles.borrow()["message-32"],image_body);assert_eq!(sent(&mut commands),0);
  let top=ui.scroll.vadjustment().value();let above=ui.state.borrow().current().messages.iter().filter(|message|!matches!(message.id.as_str(),"message-32"|"message-33")).filter_map(|message|{let bounds=ui.bubbles.borrow().get(&message.id)?.compute_bounds(&ui.timeline)?;let end=bounds.y() as f64+bounds.height() as f64;(end<top-5.&&end>top-ui.scroll.vadjustment().page_size()*1.9).then_some(message.id.clone())}).last().expect("cached row above viewport");
  remote(&ui,&above,&["🔥"]);glib::timeout_future(Duration::from_millis(350)).await;ui.selected.borrow_mut().insert("message-32".into());ui.update_selection();glib::timeout_future(Duration::from_millis(350)).await;let anchor=point(&ui,"message-32");remote(&ui,&above,&[]);glib::timeout_future(Duration::from_millis(450)).await;let drift=point(&ui,"message-32")-anchor;assert!(drift.abs()<2.,"delayed exit above viewport moved anchor by {drift}");assert!(ui.selected.borrow().contains("message-32"));let status=descendants(&ui.bubbles.borrow()[&above]).into_iter().find(|widget|widget.has_css_class("message-meta")).unwrap();assert!(!status.parent().is_some_and(|parent|parent.has_css_class("reaction-status")),"timestamp must return from the emptied reaction row");println!("WRAPPED timestamp restored after final reaction removal");println!("REMOTE delayed last-chip exit above viewport anchor_delta={drift:.2} selection_preserved=true");
  remote(&ui,&above,&["🔥"]);glib::timeout_future(Duration::from_millis(350)).await;let before_wheel=point(&ui,"message-32");remote(&ui,&above,&[]);glib::timeout_future(Duration::from_millis(80)).await;let reaction_spinner=flashed.get();assert!(!reaction_spinner,"reaction phases must not flash history loading");assert_eq!(ui.rendered_rows.borrow().serial,render_serial,"reaction phases must not schedule a full preparation pass");pointer(&ui,&ui.scroll,"5");glib::timeout_future(Duration::from_millis(450)).await;let wheel_delta=point(&ui,"message-32")-before_wheel;assert!(wheel_delta< -2.&&wheel_delta> -400.,"exit animation must preserve real wheel movement: {wheel_delta}");println!("WHEEL during delayed exit anchor_delta={wheel_delta:.2}");
  let identity=ui.bubbles.borrow()["message-32"]==image_body&&ui.bubbles.borrow()["message-33"]==video_body&&descendants(&ui.bubbles.borrow()["message-32"]).contains(&image_view)&&descendants(&ui.bubbles.borrow()["message-33"]).contains(&video_view);let tokens=std::sync::Arc::ptr_eq(&image_token,&ui.downloads.borrow().token(OWNER,&image).unwrap())&&std::sync::Arc::ptr_eq(&video_token,&ui.downloads.borrow().token(OWNER,&video).unwrap());let same_player=Rc::ptr_eq(&player,&ui.images.borrow_mut().video(&video.key()).unwrap());let ready=matches!(ui.downloads.borrow().status(OWNER,&image),Some(downloads::Status::Ready(_)))&&matches!(ui.downloads.borrow().status(OWNER,&video),Some(downloads::Status::Ready(_)));
  println!("FINAL identity={identity} downloads_same={tokens} ready={ready} player_same={same_player} paused={} timestamp_delta={} spinner_during_reactions={reaction_spinner} spinner_after_intentional_wheel={}",!player.media.is_playing(),player.media.timestamp()-position,flashed.get());
  result.set(identity&&tokens&&ready&&same_player&&!reaction_spinner&&!ui.rendering_messages()&&!player.media.is_playing()&&(player.media.timestamp()-position).abs()<1000);app.quit();
 });});
    app.run_with_args::<&str>(&[]);
    assert!(
        passed.get(),
        "reaction replaced media or started history rendering"
    );
}
