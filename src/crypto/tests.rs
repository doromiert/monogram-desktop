use super::{bundle::Bundle, records::*, *};
use std::collections::BTreeMap;
const A: &str = "alice@example.invalid";
const B: &str = "bob@example.invalid";
fn store(owner: &str, id: u32) -> Store {
    Store::open(None, owner, id, MasterKey::new(random().unwrap())).unwrap()
}
fn approve(store: &mut Store, peer: &str, id: u32, bundle: &Bundle) {
    device::observe(store, peer, id, bundle, false).unwrap();
    device::decide(
        store,
        &BTreeMap::from([(peer.into(), BTreeMap::from([(id, bundle.identity)]))]),
        true,
    )
    .unwrap();
}
fn fixtures() -> (Store, Store, Bundle, Bundle) {
    let (mut a, mut b) = (store(A, 101), store(B, 202));
    let ab = device::prepare(&mut a, 1).unwrap();
    let bb = device::prepare(&mut b, 1).unwrap();
    approve(&mut a, B, 202, &bb);
    approve(&mut b, A, 101, &ab);
    (a, b, ab, bb)
}
fn encrypt(
    store: &mut Store,
    peer: &str,
    id: u32,
    bundle: &Bundle,
    plain: &[u8],
) -> Result<(bool, Vec<u8>)> {
    let local = store.device;
    store.transaction(|t| {
        device::session(t, local, peer, id, bundle)?;
        native::encrypt(t, local, peer, id, plain)
    })
}
fn decrypt(store: &mut Store, peer: &str, id: u32, message: &(bool, Vec<u8>)) -> Result<Secret> {
    let local = store.device;
    store.transaction(|t| native::decrypt(t, local, peer, id, &message.1, message.0))
}
#[test]
fn bundles_ratchets_and_out_of_order_roundtrip() {
    let (mut a, mut b, ab, bb) = fixtures();
    let parsed = Bundle::parse(&bb.xml()).unwrap();
    assert_eq!(parsed.identity, bb.identity);
    let first = encrypt(&mut a, B, 202, &bb, b"First from Rust").unwrap();
    assert!(first.0);
    assert_eq!(
        decrypt(&mut b, A, 101, &first).unwrap().as_slice(),
        b"First from Rust"
    );
    assert_eq!(b.transaction(|t| Ok(t.ids(PRE_KEY)?.len())).unwrap(), 99);
    let reply = encrypt(&mut b, A, 101, &ab, b"Reply").unwrap();
    assert_eq!(
        decrypt(&mut a, B, 202, &reply).unwrap().as_slice(),
        b"Reply"
    );
    let second = encrypt(&mut a, B, 202, &bb, b"Second").unwrap();
    let third = encrypt(&mut a, B, 202, &bb, b"Third").unwrap();
    assert!(!second.0);
    assert_eq!(
        decrypt(&mut b, A, 101, &third).unwrap().as_slice(),
        b"Third"
    );
    assert_eq!(
        decrypt(&mut b, A, 101, &second).unwrap().as_slice(),
        b"Second"
    );
    assert!(matches!(
        decrypt(&mut b, A, 101, &second),
        Err(Error::Native(-1001))
    ));
}
#[test]
fn persistence_and_payload_authentication_failures_rollback_ratchets() {
    let (mut a, mut b, _, bb) = fixtures();
    let plain = payload::seal(b"Authenticated fixture payload").unwrap();
    let first = encrypt(&mut a, B, 202, &bb, &plain.material).unwrap();
    b.fixture_fail_writes(true);
    assert!(decrypt(&mut b, A, 101, &first).is_err());
    b.fixture_fail_writes(false);
    assert_eq!(b.transaction(|t| Ok(t.ids(PRE_KEY)?.len())).unwrap(), 100);
    let mut bad = plain.ciphertext.clone();
    bad[0] ^= 1;
    let result = b.transaction(|t| {
        let material = native::decrypt(t, 202, A, 101, &first.1, first.0)?;
        payload::open(&bad, &material)
    });
    assert!(result.is_err());
    assert_eq!(b.transaction(|t| Ok(t.ids(PRE_KEY)?.len())).unwrap(), 100);
    let decoded = b
        .transaction(|t| {
            let material = native::decrypt(t, 202, A, 101, &first.1, first.0)?;
            payload::open(&plain.ciphertext, &material)
        })
        .unwrap();
    assert_eq!(decoded.as_slice(), b"Authenticated fixture payload");
    let before = a.transaction(|t| t.load(SESSION, B, 202)).unwrap().unwrap();
    a.fixture_fail_writes(true);
    assert!(encrypt(&mut a, B, 202, &bb, b"Must not advance").is_err());
    a.fixture_fail_writes(false);
    assert_eq!(
        a.transaction(|t| t.load(SESSION, B, 202))
            .unwrap()
            .unwrap()
            .as_slice(),
        before.as_slice()
    );
}
#[test]
fn no_implicit_trust_and_stale_batch_decisions_are_atomic() {
    let (mut a, mut b) = (store(A, 101), store(B, 202));
    device::prepare(&mut a, 1).unwrap();
    let bb = device::prepare(&mut b, 1).unwrap();
    device::observe(&mut a, B, 202, &bb, false).unwrap();
    assert!(matches!(
        encrypt(&mut a, B, 202, &bb, b"Blocked"),
        Err(Error::TrustRequired)
    ));
    let wrong = [4; 32];
    let batch = BTreeMap::from([(B.into(), BTreeMap::from([(202, bb.identity), (203, wrong)]))]);
    assert!(matches!(
        device::decide(&mut a, &batch, true),
        Err(Error::Changed)
    ));
    assert_eq!(
        a.transaction(|t| device::trust(t, B, 202, &bb.identity))
            .unwrap(),
        device::Trust::Undecided
    );
    approve(&mut a, B, 202, &bb);
    device::decide(
        &mut a,
        &BTreeMap::from([(B.into(), BTreeMap::from([(202, bb.identity)]))]),
        false,
    )
    .unwrap();
    assert_eq!(
        a.transaction(|t| device::trust(t, B, 202, &bb.identity))
            .unwrap(),
        device::Trust::Rejected
    );
    assert!(encrypt(&mut a, B, 202, &bb, b"Still blocked").is_err());
}
#[test]
fn contact_blind_trust_replaces_only_the_approved_ratchet_and_preserves_blocks() {
    let (mut a, _b, ab, old) = fixtures();
    assert!(encrypt(&mut a, B, 202, &old, b"Before rotation").unwrap().0);
    let mut replacement = store(B, 202);
    let next = device::prepare(&mut replacement, 1).unwrap();
    assert_ne!(next.identity, old.identity);
    device::observe(&mut a, B, 202, &next, true).unwrap();
    approve(&mut replacement, A, 101, &ab);
    let message = encrypt(&mut a, B, 202, &next, b"After rotation").unwrap();
    assert!(message.0);
    assert_eq!(
        decrypt(&mut replacement, A, 101, &message)
            .unwrap()
            .as_slice(),
        b"After rotation"
    );

    device::decide(
        &mut a,
        &BTreeMap::from([(B.into(), BTreeMap::from([(202, next.identity)]))]),
        false,
    )
    .unwrap();
    device::observe(&mut a, B, 202, &next, true).unwrap();
    assert!(matches!(
        encrypt(&mut a, B, 202, &next, b"Blocked"),
        Err(Error::TrustRequired)
    ));
    device::observe(&mut a, B, 202, &old, true).unwrap();
    assert!(matches!(
        encrypt(&mut a, B, 202, &old, b"Reused device ID"),
        Err(Error::TrustRequired)
    ));

    let mut own_device = store(A, 303);
    let own_copy = device::prepare(&mut own_device, 1).unwrap();
    device::observe(&mut a, A, 303, &own_copy, false).unwrap();
    assert!(matches!(
        encrypt(&mut a, A, 303, &own_copy, b"Own device needs review"),
        Err(Error::TrustRequired)
    ));
    device::observe(&mut a, A, 303, &own_copy, true).unwrap();
    approve(&mut own_device, A, 101, &ab);
    let copied = encrypt(&mut a, A, 303, &own_copy, b"Approved own device").unwrap();
    assert_eq!(
        decrypt(&mut own_device, A, 101, &copied)
            .unwrap()
            .as_slice(),
        b"Approved own device"
    );
    device::decide(
        &mut a,
        &BTreeMap::from([(A.into(), BTreeMap::from([(303, own_copy.identity)]))]),
        false,
    )
    .unwrap();
    device::observe(&mut a, A, 303, &own_copy, true).unwrap();
    assert!(matches!(
        encrypt(&mut a, A, 303, &own_copy, b"Own block wins"),
        Err(Error::TrustRequired)
    ));
}
#[test]
fn rotation_and_incomplete_state_preserve_identity() {
    let mut a = store(A, 101);
    let original = device::prepare(&mut a, 1).unwrap();
    let next = device::prepare(&mut a, 14 * 24 * 60 * 60 * 1000 + 1).unwrap();
    assert_eq!(original.identity, next.identity);
    assert_ne!(original.signed_id, next.signed_id);
    device::prepare(&mut a, 28 * 24 * 60 * 60 * 1000 + 1).unwrap();
    assert_eq!(
        a.transaction(|t| Ok(t.ids(SIGNED_PRE_KEY)?.len())).unwrap(),
        2
    );
    a.transaction(|t| t.save(IDENTITY, "", 0, None)).unwrap();
    assert!(matches!(
        device::prepare(&mut a, 29 * 24 * 60 * 60 * 1000),
        Err(Error::Incomplete)
    ));
    assert_eq!(
        a.transaction(|t| t.load(IDENTITY, "", 1))
            .unwrap()
            .unwrap()
            .as_slice(),
        original.identity
    );
}
#[test]
fn unsigned_remote_prekey_ids_and_malformed_bundle_validation() {
    for id in [0, 2147483648, u32::MAX] {
        let (mut a, mut b, _, bb) = fixtures();
        let pre_id = *bb.prekeys.keys().next().unwrap();
        b.transaction(|t| {
            let pre = t.load(PRE_KEY, "", pre_id)?.unwrap();
            let signed = t.load(SIGNED_PRE_KEY, "", bb.signed_id)?.unwrap();
            t.save(PRE_KEY, "", id, Some(&pre))?;
            t.save(SIGNED_PRE_KEY, "", id, Some(&signed))
        })
        .unwrap();
        let mut remote = bb.clone();
        remote.signed_id = id;
        remote.prekeys = BTreeMap::from([(id, bb.prekeys[&pre_id])]);
        let remote = Bundle::parse(&remote.xml()).unwrap();
        let first = encrypt(&mut a, B, 202, &remote, b"Unsigned IDs").unwrap();
        assert_eq!(
            decrypt(&mut b, A, 101, &first).unwrap().as_slice(),
            b"Unsigned IDs"
        );
    }
    let mut a = store(A, 101);
    let bundle = device::prepare(&mut a, 1).unwrap();
    let mut invalid = bundle.clone();
    invalid.signature[0] ^= 1;
    assert!(Bundle::parse(&invalid.xml()).is_err());
    invalid = bundle.clone();
    invalid.identity = [0; 32];
    assert!(invalid.validate().is_err());
    invalid = bundle.clone();
    invalid.prekeys.insert(1, [0; 32]);
    assert!(invalid.validate().is_err());
}
#[test]
fn complete_stanzas_pending_journal_and_heartbeats() {
    use engine::Engine;
    use tokio_xmpp::minidom::Element;
    let (a, b, ab, bb) = fixtures();
    let (mut a, mut b) = (Engine::new(a, 2).unwrap(), Engine::new(b, 2).unwrap());
    let plain:Element=format!("<message xmlns='jabber:client' from='{A}/desktop' to='{B}' type='chat' id='one'><body>Encrypted 🌿</body></message>").parse().unwrap();
    let encrypted = a
        .encrypt(
            &plain,
            &BTreeMap::from([(B.into(), BTreeMap::from([(202, bb)]))]),
        )
        .unwrap();
    assert!(encrypted.get_child("body", payload::CLIENT).is_none());
    let first = b.receive(&encrypted, 2).unwrap();
    assert_eq!(
        first
            .content
            .as_ref()
            .unwrap()
            .get_child("body", payload::CLIENT)
            .unwrap()
            .text(),
        "Encrypted 🌿"
    );
    assert!(first.acknowledgement.is_some());
    let cached = b.receive(&encrypted, 2).unwrap();
    assert_eq!(first.token, cached.token);
    assert_eq!(b.journal.pending(&mut b.store).unwrap().len(), 1);
    let token = first.token.as_ref().unwrap();
    assert!(b.journal.accept(&mut b.store, token).unwrap());
    let pending = b.journal.pending(&mut b.store).unwrap();
    assert!(pending[0].content.is_none() && pending[0].acknowledgement.is_some());
    let ack = a
        .receive(first.acknowledgement.as_ref().unwrap(), 2)
        .unwrap();
    assert!(ack.content.is_none());
    b.journal.acknowledge(&mut b.store, token).unwrap();
    assert!(b.journal.pending(&mut b.store).unwrap().is_empty());
    let reply:Element=format!("<message xmlns='jabber:client' from='{B}' to='{A}' type='chat' id='two'><body>Reply</body></message>").parse().unwrap();
    let ciphertext = b
        .encrypt(
            &reply,
            &BTreeMap::from([(A.into(), BTreeMap::from([(101, ab)]))]),
        )
        .unwrap();
    let message = a.receive(&ciphertext, 2).unwrap();
    assert_eq!(
        message
            .content
            .unwrap()
            .get_child("body", payload::CLIENT)
            .unwrap()
            .text(),
        "Reply"
    );
}
#[test]
#[ignore = "requires compiled mobile Java/JNI fixture"]
fn native_mobile_jvm_interoperability() {
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use std::{
        io::{BufRead, BufReader, Write},
        process::{Command, Stdio},
    };
    let dir = std::env::var("MONOGRAM_MOBILE_CRYPTO_FIXTURE")
        .expect("Build tests/mobile-reference first");
    let java = std::env::var("JAVA_HOME").unwrap() + "/bin/java";
    let mut peer = Command::new(java)
        .arg(format!("-Djava.library.path={dir}"))
        .arg("-cp")
        .arg(format!("{dir}/classes"))
        .arg("com.negzero.monogram.xmpp.Bridge")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut input = peer.stdin.take().unwrap();
    let mut output = BufReader::new(peer.stdout.take().unwrap());
    let mut request = |value: &str| -> Vec<String> {
        writeln!(input, "{value}").unwrap();
        input.flush().unwrap();
        let mut line = String::new();
        output.read_line(&mut line).unwrap();
        assert!(
            line.starts_with("OK\t"),
            "mobile fixture rejected operation"
        );
        line.trim_end()
            .split('\t')
            .skip(1)
            .map(str::to_string)
            .collect()
    };
    let fields = request("INIT");
    let decode = |i: usize| STANDARD.decode(&fields[i]).unwrap();
    let mobile = Bundle {
        identity: decode(0).try_into().unwrap(),
        prekeys: BTreeMap::from([(11, decode(1).try_into().unwrap())]),
        signed_id: 22,
        signed: decode(2).try_into().unwrap(),
        signature: decode(3).try_into().unwrap(),
    };
    mobile.validate().unwrap();
    let mut rust = store(A, 101);
    let own = device::prepare(&mut rust, 1).unwrap();
    approve(&mut rust, B, 202, &mobile);
    request(&format!(
        "TRUST\t{A}\t101\t{}",
        STANDARD.encode(own.identity)
    ));
    let encrypted = encrypt(&mut rust, B, 202, &mobile, b"Rust to actual mobile JNI").unwrap();
    let received = request(&format!(
        "DECRYPT\t{A}\t101\t{}\t{}",
        u8::from(encrypted.0),
        STANDARD.encode(&encrypted.1)
    ));
    assert_eq!(
        STANDARD.decode(&received[0]).unwrap(),
        b"Rust to actual mobile JNI"
    );
    let received = request(&format!(
        "ENCRYPT\t{A}\t101\t{}",
        STANDARD.encode("Mobile reply 🦊")
    ));
    let reply = (received[0] == "1", STANDARD.decode(&received[1]).unwrap());
    assert_eq!(
        decrypt(&mut rust, B, 202, &reply).unwrap().as_slice(),
        "Mobile reply 🦊".as_bytes()
    );
    let second = encrypt(&mut rust, B, 202, &mobile, b"Second").unwrap();
    let third = encrypt(&mut rust, B, 202, &mobile, b"Third").unwrap();
    for message in [&third, &second] {
        request(&format!(
            "DECRYPT\t{A}\t101\t{}\t{}",
            u8::from(message.0),
            STANDARD.encode(&message.1)
        ));
    }
    // Exercise the other side's independently established prekey session too.
    let fields = request("INIT");
    let decode = |i: usize| STANDARD.decode(&fields[i]).unwrap();
    let mobile = Bundle {
        identity: decode(0).try_into().unwrap(),
        prekeys: BTreeMap::from([(11, decode(1).try_into().unwrap())]),
        signed_id: 22,
        signed: decode(2).try_into().unwrap(),
        signature: decode(3).try_into().unwrap(),
    };
    let mut fresh = store(A, 101);
    let own = device::prepare(&mut fresh, 1).unwrap();
    approve(&mut fresh, B, 202, &mobile);
    request(&format!(
        "TRUST\t{A}\t101\t{}",
        STANDARD.encode(own.identity)
    ));
    let (id, key) = own.prekeys.first_key_value().unwrap();
    request(&format!(
        "BUILD\t{A}\t101\t{id}\t{}\t{}\t{}\t{}\t{}",
        STANDARD.encode(key),
        own.signed_id,
        STANDARD.encode(own.signed),
        STANDARD.encode(own.signature),
        STANDARD.encode(own.identity)
    ));
    let first = request(&format!(
        "ENCRYPT\t{A}\t101\t{}",
        STANDARD.encode("Mobile initiated 🌿")
    ));
    assert_eq!(
        decrypt(
            &mut fresh,
            B,
            202,
            &(first[0] == "1", STANDARD.decode(&first[1]).unwrap())
        )
        .unwrap()
        .as_slice(),
        "Mobile initiated 🌿".as_bytes()
    );
    drop(request);
    drop(input);
    assert!(peer.wait().unwrap().success());
}

#[test]
fn journal_survives_restart_and_storage_failures_without_losing_plaintext() {
    use engine::Engine;
    use tokio_xmpp::minidom::Element;
    let directory =
        std::env::temp_dir().join(format!("monogram-crypto-{}", crate::model::new_id()));
    struct Cleanup(std::path::PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let _cleanup = Cleanup(directory.clone());
    let path = directory.join("keys.sqlite3");
    let master = [9; 32];
    let mut a = store(A, 101);
    let mut b = Store::open(Some(&path), B, 202, MasterKey::new(master)).unwrap();
    let ab = device::prepare(&mut a, 1).unwrap();
    let bb = device::prepare(&mut b, 1).unwrap();
    approve(&mut a, B, 202, &bb);
    approve(&mut b, A, 101, &ab);
    let (mut a, mut b) = (Engine::new(a, 1).unwrap(), Engine::new(b, 1).unwrap());
    let plain:Element=format!("<message xmlns='jabber:client' from='{A}' to='{B}' type='chat'><body>Keep through a crash</body></message>").parse().unwrap();
    let encrypted = a
        .encrypt(
            &plain,
            &BTreeMap::from([(B.into(), BTreeMap::from([(202, bb)]))]),
        )
        .unwrap();
    let first = b.receive(&encrypted, 123).unwrap();
    let token = first.token.unwrap();
    drop(b);
    let mut wrong = Store::open(Some(&path), B, 202, MasterKey::new([8; 32])).unwrap();
    assert!(device::prepare(&mut wrong, 2).is_err());
    drop(wrong);
    let mut b = Engine::new(
        Store::open(Some(&path), B, 202, MasterKey::new(master)).unwrap(),
        2,
    )
    .unwrap();
    let recovered = b.journal.pending(&mut b.store).unwrap();
    assert_eq!(recovered.len(), 1);
    assert_eq!(recovered[0].token.as_ref(), Some(&token));
    assert_eq!(recovered[0].sent_time, 123);
    assert_eq!(recovered[0].fingerprint, ab.identity);
    assert!(recovered[0].outer.is_some());
    assert!(recovered[0].acknowledgement.is_some());
    assert!(recovered[0].content.is_some());
    b.store.fixture_fail_writes(true);
    assert!(b.journal.accept(&mut b.store, &token).is_err());
    b.store.fixture_fail_writes(false);
    assert!(
        b.journal.pending(&mut b.store).unwrap()[0]
            .content
            .is_some()
    );
    b.journal.acknowledge(&mut b.store, &token).unwrap();
    assert!(
        b.journal.pending(&mut b.store).unwrap()[0]
            .content
            .is_some()
    );
    assert_eq!(
        b.receive(&encrypted, 999).unwrap().token,
        Some(token.clone())
    );
    assert_eq!(
        b.store.transaction(|t| Ok(t.ids(PRE_KEY)?.len())).unwrap(),
        100 // Startup replenishes; replay must not consume another prekey.
    );
    assert!(b.journal.accept(&mut b.store, &token).unwrap());
    drop(b);
    let mut b = Engine::new(
        Store::open(Some(&path), B, 202, MasterKey::new(master)).unwrap(),
        3,
    )
    .unwrap();
    let recovered = b.journal.pending(&mut b.store).unwrap();
    assert!(recovered[0].content.is_none() && recovered[0].acknowledgement.is_some());
    b.journal.acknowledge(&mut b.store, &token).unwrap();
    assert!(b.journal.pending(&mut b.store).unwrap().is_empty());
    let mut other = Store::open(Some(&path), A, 202, MasterKey::new(master)).unwrap();
    assert!(
        other
            .transaction(|t| t.load(IDENTITY, "", 1))
            .unwrap()
            .is_none()
    );
}

#[test]
fn full_incoming_journal_rolls_back_ratchet_and_consumed_prekey() {
    use engine::Engine;
    use tokio_xmpp::minidom::Element;
    let (a, b, _, bb) = fixtures();
    let (mut a, mut b) = (Engine::new(a, 1).unwrap(), Engine::new(b, 1).unwrap());
    let plain:Element=format!("<message xmlns='jabber:client' from='{A}' to='{B}' type='chat'><body>Retry after freeing capacity</body></message>").parse().unwrap();
    let encrypted = a
        .encrypt(
            &plain,
            &BTreeMap::from([(B.into(), BTreeMap::from([(202, bb)]))]),
        )
        .unwrap();
    let journal = &b.journal;
    let tokens: Vec<_> = b
        .store
        .transaction(|tx| {
            (0..128u8)
                .map(|i| {
                    journal
                        .append(
                            tx,
                            [i; 32],
                            &plain,
                            Some(&plain),
                            None,
                            [1; 32],
                            false,
                            0,
                            None,
                        )
                        .map(|r| r.token.unwrap())
                })
                .collect()
        })
        .unwrap();
    assert!(b.receive(&encrypted, 0).is_err());
    assert_eq!(
        b.store.transaction(|t| Ok(t.ids(PRE_KEY)?.len())).unwrap(),
        100
    );
    assert!(
        b.store
            .transaction(|t| t.load(SESSION, A, 101))
            .unwrap()
            .is_none()
    );
    assert!(b.journal.accept(&mut b.store, &tokens[0]).unwrap());
    assert!(b.receive(&encrypted, 0).unwrap().content.is_some());
    // Reusing a freed slot must not let its old token delete a newer message.
    assert!(!b.journal.accept(&mut b.store, &tokens[0]).unwrap());
    assert_eq!(b.journal.pending(&mut b.store).unwrap().len(), 128);
    b.store
        .transaction(|t| t.save(INCOMING, "", 0, Some(&[0; 20])))
        .unwrap();
    assert!(b.journal.pending(&mut b.store).is_err());
}

#[test]
#[ignore = "requires isolated Prosody fixture"]
fn live_pep_encrypted_roundtrip() {
    use crate::{
        iq,
        model::Account,
        protocol::Command,
        transport::{Client, Event},
    };
    use engine::Engine;
    use std::time::Duration;
    use tokio_xmpp::{minidom::Element, parsers::stanza::Stanza};
    let _ = tokio_xmpp::rustls::crypto::aws_lc_rs::default_provider().install_default();
    let config = |user: &str| Account {
        jid: format!("{user}@localhost"),
        host: "127.0.0.1".into(),
        port: std::env::var("MONOGRAM_IT_PORT").unwrap().parse().unwrap(),
        direct_tls: false,
    };
    async fn iq_transport(config: Account) -> iq::Client {
        // This fixture publishes manually generated keys, so its IQ client has no automatic crypto setup.
        let owner = config.jid.clone();
        let mut client = Client::new(config, "fixture-password".into());
        assert!(matches!(
            tokio::time::timeout(Duration::from_secs(20), client.next())
                .await
                .unwrap(),
            Some(Event::Online)
        ));
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        tokio::task::spawn_local(async move {
            let mut pending = iq::Pending::default();
            loop {
                tokio::select! {
                    command = rx.recv() => {
                        let Some(Command::Iq { to, kind, payload, reply }) = command else { break };
                        if let Some(xml) = pending.start(to, kind, payload, reply) {
                            let stanza = Stanza::try_from(xml).unwrap();
                            if client.send_stanza(stanza).await.is_err() { break; }
                        }
                    }
                    event = client.next() => match event {
                        Some(Event::Stanza(stanza)) => { pending.receive(&stanza.into(), &owner); }
                        _ => break,
                    }
                }
            }
            pending.disconnect();
        });
        iq::Client::new(tx)
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    tokio::task::LocalSet::new().block_on(&runtime, async {
        let aiq = iq_transport(config("alice")).await;
        let biq = iq_transport(config("bob")).await;
        let(mut a,mut b,mut auxiliary)=(store("alice@localhost",101),store("bob@localhost",202),store("alice@localhost",303));
        let ab=device::prepare(&mut a,1).unwrap();let bb=device::prepare(&mut b,1).unwrap();let aux=device::prepare(&mut auxiliary,1).unwrap();
        let ap=pep::Pep::new(aiq.clone(),"alice@localhost",101).unwrap();let bp=pep::Pep::new(biq,"bob@localhost",202).unwrap();let auxp=pep::Pep::new(aiq.clone(),"alice@localhost",303).unwrap();
        auxp.initialize(&aux).await.unwrap();
        // Preserve a different client's labels/unknown children when adding our device.
        let list:Element="<pubsub xmlns='http://jabber.org/protocol/pubsub'><publish node='urn:xmpp:omemo:2:devices'><item id='current'><devices xmlns='urn:xmpp:omemo:2' marker='retain'><device id='303' label='Mobile'/><metadata xmlns='urn:fixture:metadata'>keep</metadata></devices></item></publish></pubsub>".parse().unwrap();
        aiq.request("alice@localhost",iq::Kind::Set,list).await.unwrap();
        let announced=ap.initialize(&ab).await.unwrap();assert_eq!(pep::ids(Some(&announced)).unwrap(),std::collections::BTreeSet::from([101,303]));assert_eq!(announced.attr("marker"),Some("retain"));assert!(announced.get_child("metadata","urn:fixture:metadata").is_some());
        bp.initialize(&bb).await.unwrap();
        let remote=ap.discover("bob@localhost").await.unwrap();let own=ap.discover("alice@localhost").await.unwrap();let bremote=bp.discover("alice@localhost").await.unwrap();assert_eq!(remote[&202].identity,bb.identity);assert_eq!(own.keys().copied().collect::<Vec<_>>(),vec![303]);assert_eq!(bremote.len(),2);
        approve(&mut a,"bob@localhost",202,&remote[&202]);approve(&mut a,"alice@localhost",303,&own[&303]);approve(&mut b,"alice@localhost",101,&bremote[&101]);approve(&mut auxiliary,"alice@localhost",101,&bremote[&101]);
        let(mut a,mut b,mut auxiliary)=(Engine::new(a,2).unwrap(),Engine::new(b,2).unwrap(),Engine::new(auxiliary,2).unwrap());
        let plain:Element="<message xmlns='jabber:client' from='alice@localhost' to='bob@localhost' type='chat' id='encrypted-fixture'><body>Encrypted over verified TLS 🌿</body><reply xmlns='urn:xmpp:reply:0' to='bob@localhost' id='previous'/></message>".parse().unwrap();
        let encrypted=a.encrypt(&plain,&BTreeMap::from([("bob@localhost".into(),remote),("alice@localhost".into(),own)])).unwrap();assert!(encrypted.get_child("body",payload::CLIENT).is_none());assert!(!String::from_utf8(bundle::xml(&encrypted,1024*1024).unwrap()).unwrap().contains("Encrypted over verified"));
        let mut sender=Client::new(config("alice"),"fixture-password".into());let mut receiver=Client::new(config("bob"),"fixture-password".into());
        assert!(matches!(tokio::time::timeout(Duration::from_secs(20),sender.next()).await.unwrap(),Some(Event::Online)));assert!(matches!(tokio::time::timeout(Duration::from_secs(20),receiver.next()).await.unwrap(),Some(Event::Online)));
        receiver.send_stanza(Stanza::try_from("<presence xmlns='jabber:client'><priority>10</priority></presence>".parse::<Element>().unwrap()).unwrap()).await.unwrap();
        // Wait for the server to process priority before sending to the bare JID.
        let probe=Element::builder("iq",payload::CLIENT).attr("type".try_into().unwrap(),"get").attr("id".try_into().unwrap(),"barrier").append(Element::builder("ping","urn:xmpp:ping").build()).build();receiver.send_stanza(Stanza::try_from(probe).unwrap()).await.unwrap();
        loop {if let Some(Event::Stanza(stanza))=tokio::time::timeout(Duration::from_secs(20),receiver.next()).await.unwrap(){let root:Element=stanza.into();if root.is("iq",payload::CLIENT)&&root.attr("id")==Some("barrier"){break}}}
        sender.send_stanza(Stanza::try_from(encrypted.clone()).unwrap()).await.unwrap();
        let wire=loop {if let Some(Event::Stanza(stanza))=tokio::time::timeout(Duration::from_secs(20),receiver.next()).await.unwrap(){let root:Element=stanza.into();if root.get_child("encrypted",bundle::NS).is_some(){break root}}};
        assert!(wire.get_child("body",payload::CLIENT).is_none());let received=b.receive(&wire,2).unwrap();let decoded=received.content.as_ref().unwrap();assert_eq!(decoded.get_child("body",payload::CLIENT).unwrap().text(),"Encrypted over verified TLS 🌿");assert!(decoded.get_child("reply","urn:xmpp:reply:0").is_some());assert!(auxiliary.receive(&encrypted,2).unwrap().content.is_some());
        b.journal.accept(&mut b.store,received.token.as_ref().unwrap()).unwrap();
        let mut ack=received.acknowledgement.unwrap();ack.set_attr("".into(),"to".try_into().unwrap(),wire.attr("from").unwrap());receiver.send_stanza(Stanza::try_from(ack).unwrap()).await.unwrap();
        loop {if let Some(Event::Stanza(stanza))=tokio::time::timeout(Duration::from_secs(20),sender.next()).await.unwrap(){let root:Element=stanza.into();if root.get_child("encrypted",bundle::NS).is_some(){assert!(a.receive(&root,2).unwrap().content.is_none());break}}}
        b.journal.acknowledge(&mut b.store,received.token.as_ref().unwrap()).unwrap();assert!(b.journal.pending(&mut b.store).unwrap().is_empty());
        sender.send_end().await.unwrap();receiver.send_end().await.unwrap();
    });
}
