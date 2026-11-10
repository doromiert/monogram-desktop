use super::*;
use base64::{Engine as _, engine::general_purpose::STANDARD};

const A: &str = "alice@example.invalid";
const B: &str = "bob@example.invalid";
fn fixture(owner: &str, id: u32) -> (Store, Journal, Bundle) {
    let mut store = Store::open(None, owner, id, MasterKey::new(random().unwrap())).unwrap();
    let bundle = device::prepare_for(&mut store, 1, true).unwrap();
    let journal = Journal::new(&mut store).unwrap();
    (store, journal, bundle)
}
fn approve(store: &mut Store, peer: &str, id: u32, bundle: &Bundle) {
    store
        .transaction(|tx| tx.save(SEEN, peer, id, Some(&bundle.identity)))
        .unwrap();
    device::decide(
        store,
        &BTreeMap::from([(peer.into(), BTreeMap::from([(id, bundle.identity)]))]),
        true,
    )
    .unwrap();
}
fn device(peer: &str, id: u32, bundle: &Bundle) -> Device {
    Device {
        peer: peer.into(),
        id,
        key: bundle.identity,
        trust: Trust::Trusted,
    }
}
#[test]
fn authenticated_body_and_other_device_copy_survive_failed_gcm_and_archive_redelivery() {
    let (mut alice, alice_journal, alice_bundle) = fixture(A, 101);
    let (mut bob, bob_journal, bob_bundle) = fixture(B, 202);
    // Old OMEMO encodes the device ID without its JID. Both valid device keys must work.
    let (mut other, other_journal, other_bundle) = fixture(A, 202);
    let peer = wire::parse_bundle(&wire::bundle_xml(&bob_bundle)).unwrap();
    assert_eq!(peer.identity, bob_bundle.identity);
    let mut bad = bob_bundle.clone();
    bad.signature[0] ^= 1;
    assert!(wire::parse_bundle(&wire::bundle_xml(&bad)).is_err());
    approve(&mut alice, B, 202, &peer);
    approve(&mut alice, A, 202, &other_bundle);
    approve(&mut bob, A, 101, &alice_bundle);
    approve(&mut other, A, 101, &alice_bundle);
    let mut sent = encrypt_for(
        &mut alice,
        B,
        "A legacy message 🌿",
        "msg1",
        &[
            (device(B, 202, &peer), peer),
            (device(A, 202, &other_bundle), other_bundle),
        ],
    )
    .unwrap();
    let mut corrupt = sent.clone();
    let ciphertext = corrupt
        .get_child_mut("encrypted", NS)
        .unwrap()
        .get_child_mut("payload", NS)
        .unwrap();
    let mut bytes = STANDARD.decode(ciphertext.text()).unwrap();
    bytes[0] ^= 1;
    *ciphertext = Element::builder("payload", NS)
        .append(STANDARD.encode(bytes))
        .build();
    let corrupt_envelope = Envelope::parse(&corrupt, B, 202).unwrap();
    assert!(receive_for(&mut bob, &bob_journal, &corrupt, &corrupt_envelope, 1, None).is_err());
    assert!(bob_journal.pending(&mut bob).unwrap().is_empty());
    assert_eq!(
        bob.transaction(|tx| Ok(tx.ids(PRE_KEY)?.len())).unwrap(),
        100
    );
    sent.append_child(
        Element::builder("body", payload::CLIENT)
            .append("attacker text")
            .build(),
    );
    sent.append_child(element("replace", "urn:xmpp:message-correct:0"));
    let bob_envelope = Envelope::parse(&sent, B, 202).unwrap();
    let other_envelope = Envelope::parse(&sent, A, 202).unwrap();
    assert_eq!(bob_envelope.keys.len(), 2);
    let bob_decoded = receive_for(&mut bob, &bob_journal, &sent, &bob_envelope, 2, None).unwrap();
    let other_decoded =
        receive_for(&mut other, &other_journal, &sent, &other_envelope, 2, None).unwrap();
    for decoded in [&bob_decoded, &other_decoded] {
        let content = decoded.content.as_ref().unwrap();
        assert_eq!(
            content.get_child("body", payload::CLIENT).unwrap().text(),
            "A legacy message 🌿"
        );
        assert!(
            content
                .get_child("replace", "urn:xmpp:message-correct:0")
                .is_none()
        );
        assert_eq!(decoded.fingerprint, alice_bundle.identity);
    }
    let recovered = bob_journal.pending(&mut bob).unwrap().pop().unwrap();
    assert_eq!(recovered.token, bob_decoded.token);
    assert_eq!(recovered.content, bob_decoded.content);
    let ack = recovered.acknowledgement.unwrap();
    let acknowledgement = receive_for(
        &mut alice,
        &alice_journal,
        &ack,
        &Envelope::parse(&ack, A, 101).unwrap(),
        3,
        None,
    )
    .unwrap();
    assert!(acknowledgement.content.is_none());
    assert!(acknowledgement.acknowledgement.is_none());
    assert!(
        bob_journal
            .accept(&mut bob, recovered.token.as_ref().unwrap())
            .unwrap()
    );
    bob_journal
        .acknowledge(&mut bob, recovered.token.as_ref().unwrap())
        .unwrap();
    assert!(bob_journal.pending(&mut bob).unwrap().is_empty());
}

#[test]
#[ignore = "requires the mobile Smack 4.4.8/Signal 2.6.2 JVM dependencies"]
fn smack_signal_interoperability() {
    use std::{
        io::{BufRead, BufReader, Write},
        process::{Command, Stdio},
    };
    let classpath = std::env::var_os("MONOGRAM_LEGACY_JAVA_CP")
        .expect("set classpath to mobile's Smack and Signal dependencies");
    let source =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/legacy-signal/Peer.java");
    let mut child = Command::new("java")
        .arg("--class-path")
        .arg(classpath)
        .arg(source)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
    let keys = lines.next().unwrap().unwrap();
    let keys: Vec<_> = keys.split_ascii_whitespace().collect();
    assert_eq!(keys.len(), 4);
    let key_xml = |name: &str, index: usize| Element::builder(name, NS).append(keys[index]).build();
    let bundle_xml = Element::builder("bundle", NS)
        .append(attr(
            key_xml("signedPreKeyPublic", 2),
            "signedPreKeyId",
            "1",
        ))
        .append(key_xml("signedPreKeySignature", 3))
        .append(key_xml("identityKey", 0))
        .append(
            Element::builder("prekeys", NS)
                .append(attr(key_xml("preKeyPublic", 1), "preKeyId", "1"))
                .build(),
        )
        .build();
    let bob = wire::parse_bundle(&bundle_xml).unwrap();
    let (mut alice, journal, _) = fixture(A, 101);
    approve(&mut alice, B, 202, &bob);
    let rust = encrypt_for(
        &mut alice,
        B,
        "From Rust 🌿",
        "wire",
        &[(device(B, 202, &bob), bob)],
    )
    .unwrap();
    let frame = Envelope::parse(&rust, B, 202).unwrap();
    assert!(frame.keys[0].prekey);
    let mut input = child.stdin.take().unwrap();
    writeln!(
        input,
        "{} {} {}",
        STANDARD.encode(&frame.keys[0].bytes),
        STANDARD.encode(&frame.iv),
        STANDARD.encode(frame.ciphertext.as_ref().unwrap())
    )
    .unwrap();
    drop(input);
    let reply = lines.next().unwrap().unwrap();
    assert!(child.wait().unwrap().success());
    let reply: Vec<_> = reply.split_ascii_whitespace().collect();
    assert_eq!(reply.len(), 4);
    let key = wire::key_xml(101, reply[0] == "true", &STANDARD.decode(reply[1]).unwrap());
    let java = wire::stanza(
        B,
        A,
        "reply",
        202,
        vec![key],
        &STANDARD.decode(reply[2]).unwrap(),
        Some(&STANDARD.decode(reply[3]).unwrap()),
    );
    let frame = Envelope::parse(&java, A, 101).unwrap();
    let result = receive_for(&mut alice, &journal, &java, &frame, 2, None).unwrap();
    assert_eq!(
        result
            .content
            .unwrap()
            .get_child("body", payload::CLIENT)
            .unwrap()
            .text(),
        "From Smack 🌙"
    );
}

#[test]
#[ignore = "requires disposable Prosody, private D-Bus and keyring"]
fn live_legacy_service_and_independent_send_preference() {
    use crate::{
        encryption_policy::Mode,
        model::{Account, Message, State, new_id},
        protocol::{BackendEvent, Command, Envelope, Session},
    };
    use std::{
        sync::mpsc,
        time::{Duration, Instant},
    };
    const SENDER: &str = "grace@localhost";
    const RECEIVER: &str = "hank@localhost";
    assert_eq!(
        std::env::var("MONOGRAM_KEYRING_FIXTURE").as_deref(),
        Ok("1")
    );
    let _ = tokio_xmpp::rustls::crypto::aws_lc_rs::default_provider().install_default();
    let history = crate::storage::Store::open(false).unwrap();
    fn wait(
        rx: &mpsc::Receiver<Envelope>,
        predicate: impl Fn(&BackendEvent) -> bool,
    ) -> BackendEvent {
        let deadline = Instant::now() + Duration::from_secs(25);
        loop {
            let (_, _, event) = rx
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .unwrap();
            if let BackendEvent::LegacyStatus(status) = &event {
                eprintln!("fixture Legacy OMEMO status: {}", status.text);
            }
            if predicate(&event) {
                return event;
            }
        }
    }
    let start = |jid: &str| {
        let account = Account {
            jid: jid.into(),
            host: "127.0.0.1".into(),
            port: std::env::var("MONOGRAM_IT_PORT").unwrap().parse().unwrap(),
            direct_tls: false,
        };
        history.save_account(&account).unwrap();
        let (tx, rx) = mpsc::channel();
        let session = Session::start(account, "fixture-password".into(), 17, tx);
        let BackendEvent::LegacyStatus(status) = wait(
            &rx,
            |event| matches!(event, BackendEvent::LegacyStatus(s) if s.ready),
        ) else {
            unreachable!()
        };
        (session, rx, status.fingerprint)
    };
    let (sender, sent, fingerprint) = start(SENDER);
    let (receiver, received, _) = start(RECEIVER);
    history.set_blind_trust(RECEIVER, SENDER, true).unwrap();
    for (session, rx) in [(&sender, &sent), (&receiver, &received)] {
        session.send(Command::Crypto(crate::crypto::service::Request::Enable));
        wait(
            rx,
            |event| matches!(event, BackendEvent::CryptoStatus(s) if s.ready),
        );
    }
    sender.send(Command::Legacy(service::Request::Review(RECEIVER.into())));
    let BackendEvent::LegacyReview { rows, errors, .. } = wait(
        &sent,
        |event| matches!(event, BackendEvent::LegacyReview { peer, .. } if peer == RECEIVER),
    ) else {
        unreachable!()
    };
    assert!(errors.is_empty(), "{errors:?}");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].trust, "Undecided");
    sender.send(Command::Legacy(service::Request::Decide {
        keys: BTreeMap::from([(
            RECEIVER.into(),
            BTreeMap::from([(rows[0].device, rows[0].key)]),
        )]),
        approve: true,
    }));
    wait(
        &sent,
        |event| matches!(event, BackendEvent::LegacyReview { peer, rows, .. } if peer == RECEIVER && rows.iter().any(|row| row.trust == "Trusted")),
    );
    let (tx, mut result) = tokio::sync::oneshot::channel();
    sender.send(Command::ChooseEncryption {
        peer: RECEIVER.into(),
        reply: tx,
    });
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        match result.try_recv() {
            Ok(probed) => {
                assert_eq!(probed.unwrap(), Mode::Legacy);
                break;
            }
            Err(tokio::sync::oneshot::error::TryRecvError::Empty) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(10))
            }
            _ => panic!("legacy probe timed out"),
        }
    }
    let id = new_id();
    let mut message = Message {
        wire_id: id.clone(),
        text: "Legacy encrypted through Prosody 🌿".into(),
        ..Default::default()
    };
    message.set_encryption(Mode::Legacy, fingerprint.clone());
    sender.send(Command::Send {
        peer: RECEIVER.into(),
        message,
        quote: String::new(),
    });
    wait(
        &sent,
        |e| matches!(e, BackendEvent::Sent { id: wire, .. } if wire == &id),
    );
    let event = wait(&received, |e| {
        matches!(e, BackendEvent::LegacyEncryptedMessage { .. })
    });
    let BackendEvent::LegacyEncryptedMessage {
        peer,
        message,
        token,
        ..
    } = event
    else {
        unreachable!()
    };
    assert_eq!(peer, SENDER);
    assert_eq!(message.text, "Legacy encrypted through Prosody 🌿");
    assert_eq!(message.wire_id, id);
    assert_eq!(message.fingerprint, fingerprint);
    assert_eq!(message.mode(), Mode::Legacy);
    let mut state = State::live(RECEIVER, vec![]);
    state.chat_for(SENDER).messages.push(message);
    history.save(&state).unwrap();
    receiver.send(Command::Legacy(service::Request::Accepted(token)));
    receiver.send(Command::History {
        peer: SENDER.into(),
        before: String::new(),
    });
    wait(
        &received,
        |e| matches!(e, BackendEvent::CryptoAliases { peer, aliases, .. } if peer == SENDER && aliases.iter().any(|id| id.starts_with("archive:"))),
    );
}
