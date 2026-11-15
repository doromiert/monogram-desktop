//! Private, offline native fixture: only a fake command channel is used.
use crate::{
    model::{Chat, State},
    protocol::{BackendEvent, Command, Session},
    rooms::BookmarkInfo,
    ui::Ui,
};
use adw::prelude::*;
use gtk::gio;
#[test]
#[ignore = "requires private GTK display"]
fn reconnect_restores_visible_room_after_bookmarks_and_then_requests_history() {
    let app = adw::Application::builder()
        .application_id("com.negzero.MonogramRoomRestoreTest")
        .flags(gio::ApplicationFlags::NON_UNIQUE)
        .build();
    app.register(None::<&gio::Cancellable>).unwrap();
    let ui = Ui::new(&app, true);
    let owner = "owner@example.invalid";
    let peer = "room@conference.example.invalid";
    let mut state = State::live(
        owner,
        vec![Chat {
            id: "room".into(),
            jid: peer.into(),
            name: "Group".into(),
            group: true,
            room_autojoin: Some(false),
            room_nick: Some("Saved nick".into()),
            ..Default::default()
        }],
    );
    state.active = "room".into();
    ui.activate_state(state);
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    ui.session.replace(Some(Session::fixture_channel(tx)));
    let generation = ui.generation.get();
    ui.backend_event(
        generation,
        owner,
        BackendEvent::Status {
            online: true,
            error: false,
            text: "Connected".into(),
        },
    );
    assert!(
        !std::iter::from_fn(|| rx.try_recv().ok())
            .any(|c| matches!(c, Command::JoinRoom { .. } | Command::History { .. })),
        "wait for room settings and confirmed membership"
    );
    let feedback = std::time::Instant::now();
    ui.backend_event(
        generation,
        owner,
        BackendEvent::RoomBookmarks(vec![BookmarkInfo {
            jid: peer.into(),
            name: "Group".into(),
            nick: Some("Server nick".into()),
            autojoin: false,
        }]),
    );
    assert!(
        feedback.elapsed() < std::time::Duration::from_millis(400),
        "room restoration must give immediate feedback"
    );
    let commands = std::iter::from_fn(|| rx.try_recv().ok()).collect::<Vec<_>>();
    assert_eq!(commands.iter().filter(|c|matches!(c,Command::JoinRoom{peer:p,nick:Some(n)} if p==peer && n=="Server nick")).count(),1);
    assert_eq!(ui.state.borrow().current().subtitle, "Joining…");
    assert!(
        !commands
            .iter()
            .any(|c| matches!(c, Command::History { .. })),
        "do not query room MAM before self presence"
    );
    ui.backend_event(
        generation,
        owner,
        BackendEvent::RoomPresence(crate::rooms::Presence {
            jid: peer.into(),
            joined: true,
            can_send: true,
            detail: "Joined".into(),
        }),
    );
    assert!(
        std::iter::from_fn(|| rx.try_recv().ok())
            .any(|c| matches!(c,Command::History{peer:p,..} if p==peer)),
        "confirmed room join starts archive loading"
    );
    assert!(ui.state.borrow().current().room_joined);
    ui.backend_event(generation, owner, BackendEvent::RoomBookmarks(vec![]));
    assert!(
        !std::iter::from_fn(|| rx.try_recv().ok()).any(|c| matches!(c, Command::JoinRoom { .. })),
        "bookmark pushes do not join twice"
    );
    ui.window.close();
}
