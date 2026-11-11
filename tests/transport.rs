//! Disposable loopback-only transport checks; launched by xmpp_fixture.py.
#[path = "../src/action_store.rs"]
mod action_store;
#[path = "../src/avatar.rs"]
mod avatar;
#[path = "../src/contact_access.rs"]
mod contact_access;
#[path = "../src/credentials.rs"]
mod credentials;
#[path = "../src/crypto/mod.rs"]
mod crypto;
#[path = "../src/encryption_policy.rs"]
mod encryption_policy;
#[path = "../src/file_share.rs"]
mod file_share;
#[path = "../src/forwarding.rs"]
mod forwarding;
#[path = "../src/history_window.rs"]
mod history_window;
#[path = "../src/iq.rs"]
mod iq;
#[path = "../src/legacy_polls.rs"]
mod legacy_polls;
#[path = "../src/media_store.rs"]
mod media_store;
#[path = "../src/message_actions.rs"]
mod message_actions;
#[path = "../src/model.rs"]
mod model;
#[path = "../src/polls.rs"]
mod polls;
#[path = "../src/protocol.rs"]
mod protocol;
#[path = "../src/rooms/mod.rs"]
mod rooms;
#[path = "../src/storage.rs"]
mod storage;
#[path = "../src/transport.rs"]
mod transport;
use model::*;
use protocol::*;
use std::{
    sync::mpsc::{self, Receiver},
    time::{Duration, Instant},
};

fn start(user: &str, password: &str) -> (Session, Receiver<Envelope>) {
    let (tx, rx) = mpsc::channel();
    let config = Account {
        jid: format!(
            "{user}@{}",
            std::env::var("MONOGRAM_IT_DOMAIN").unwrap_or_else(|_| "localhost".into())
        ),
        host: "127.0.0.1".into(),
        port: std::env::var("MONOGRAM_IT_PORT").unwrap().parse().unwrap(),
        direct_tls: false,
    };
    (Session::start(config, password.into(), 7, tx), rx)
}
fn wait(rx: &Receiver<Envelope>, predicate: impl Fn(&BackendEvent) -> bool) -> BackendEvent {
    let deadline = Instant::now() + Duration::from_secs(25);
    loop {
        let (_, _, event) = rx
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .expect("XMPP event deadline");
        if let BackendEvent::Status { text, error, .. } = &event {
            eprintln!("transport status: {text} (error={error})");
        }
        if predicate(&event) {
            return event;
        }
    }
}
fn online(e: &BackendEvent) -> bool {
    matches!(e, BackendEvent::Status { online: true, .. })
}
fn rejected(e: &BackendEvent) -> bool {
    matches!(
        e,
        BackendEvent::Status {
            error: true,
            online: false,
            ..
        }
    )
}
fn closed(rx: Receiver<Envelope>) {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        match rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            Err(mpsc::RecvTimeoutError::Disconnected) => return,
            Err(e) => panic!("transport failed to stop: {e}"),
            Ok(_) => {}
        }
    }
}
#[test]
#[ignore = "requires isolated Prosody fixture"]
fn live_roundtrip() {
    let _ = tokio_xmpp::rustls::crypto::aws_lc_rs::default_provider().install_default();
    let (alice, a) = start("alice", "fixture-password");
    let (bob, b) = start("bob", "fixture-password");
    let (other, c) = start("alice", "fixture-password");
    for rx in [&a, &b, &c] {
        wait(rx, online);
        wait(rx, |e| matches!(e, BackendEvent::Roster(_)));
    }
    std::thread::sleep(Duration::from_millis(300));
    let encrypted_id = new_id();
    alice.send(Command::Send {
        peer: "bob@localhost".into(),
        message: Message {
            text: "Must never leak as plaintext".into(),
            wire_id: encrypted_id.clone(),
            encrypted: true,
            ..Default::default()
        },
        quote: String::new(),
    });
    wait(
        &a,
        |event| matches!(event,BackendEvent::Failed{id,..} if id==&encrypted_id),
    );
    let message = Message {
        text: "Hello 🌿 from Rust".into(),
        wire_id: new_id(),
        ..Default::default()
    };
    assert!(alice.send(Command::Send {
        peer: "bob@localhost".into(),
        message: message.clone(),
        quote: String::new()
    }));
    let received = wait(&b, |e| {
        if let BackendEvent::Message { message: m, .. } = e {
            assert_ne!(
                m.wire_id, encrypted_id,
                "Encrypted selection leaked through plaintext transport"
            );
        }
        matches!(e, BackendEvent::Message{message:m,..} if m.wire_id == message.wire_id)
    });
    if let BackendEvent::Message {
        message: m,
        receipt_to,
        ..
    } = received
    {
        assert_eq!(m.text, message.text);
        assert!(!m.own && !m.encrypted);
        assert!(m.aliases.contains(&format!("origin:{}", message.wire_id)));
        bob.send(Command::Receipt {
            peer: receipt_to.unwrap(),
            id: m.wire_id,
        });
    }
    wait(
        &a,
        |e| matches!(e,BackendEvent::Receipt{peer,id} if peer=="bob@localhost" && id==&message.wire_id),
    );
    wait(
        &c,
        |e| matches!(e,BackendEvent::Message{peer,message:m,..} if peer=="bob@localhost" && m.own && m.wire_id==message.wire_id),
    );
    let reply = Message {
        text: "A Unicode reply 🦊".into(),
        wire_id: new_id(),
        reply_id: message.wire_id.clone(),
        reply_to: "alice@localhost".into(),
        ..Default::default()
    };
    bob.send(Command::Send {
        peer: "alice@localhost".into(),
        message: reply.clone(),
        quote: message.text.clone(),
    });
    let received = wait(
        &a,
        |e| matches!(e,BackendEvent::Message{message:m,..}if m.wire_id==reply.wire_id),
    );
    if let BackendEvent::Message { message: m, .. } = received {
        assert_eq!(m.text, reply.text);
        assert_eq!(m.reply_id, message.wire_id);
    }
    bob.send(Command::Typing {
        peer: "alice@localhost".into(),
        composing: true,
    });
    wait(
        &a,
        |e| matches!(e,BackendEvent::Typing{peer,composing:true} if peer=="bob@localhost"),
    );
    alice.send(Command::History {
        peer: "bob@localhost".into(),
        before: String::new(),
    });
    let mut archived = std::collections::HashSet::new();
    loop {
        let e = wait(&a, |e| {
            matches!(
                e,
                BackendEvent::Message {
                    historical: true,
                    ..
                } | BackendEvent::History { .. }
            )
        });
        match e {
            BackendEvent::Message { message: m, .. } => {
                assert!(m.aliases.iter().any(|s| s.starts_with("archive:")));
                archived.insert(m.wire_id);
            }
            BackendEvent::History { error, .. } => {
                assert!(!error);
                break;
            }
            _ => unreachable!(),
        }
    }
    assert!(archived.contains(&message.wire_id) && archived.contains(&reply.wire_id));
    // The fixture drops only its proxy sockets; the client must reconnect itself.
    std::fs::write(std::env::var("MONOGRAM_IT_RECONNECT").unwrap(), "drop").unwrap();
    wait(&a, |e| {
        matches!(e, BackendEvent::Status { online: false, .. })
    });
    wait(&a, online);
    drop(other);
    closed(c);
    let (wrong, w) = start("alice", "wrong-password");
    wait(&w, rejected);
    drop(wrong);
    closed(w);
    drop(alice);
    closed(a);
    drop(bob);
    closed(b);
    println!(
        "Rust XMPP verified TLS, roster, Unicode text/reply, receipt, carbon, typing, MAM, reconnect, bad-password rejection and cancellation passed"
    );
}
#[test]
#[ignore = "requires isolated TLS rejection fixture"]
fn tls_rejection() {
    let _ = tokio_xmpp::rustls::crypto::aws_lc_rs::default_provider().install_default();
    let (session, rx) = start("alice", "fixture-password");
    loop {
        match wait(&rx, |e| matches!(e, BackendEvent::Status { .. })) {
            BackendEvent::Status { online: true, .. } => {
                panic!("untrusted transport authenticated")
            }
            BackendEvent::Status { error: true, .. } => break,
            _ => {}
        }
    }
    drop(session);
    closed(rx);
}
