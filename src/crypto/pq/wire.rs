// SPDX-License-Identifier: AGPL-3.0-only
// Monocles PQ wire/profile adapter, matching mobile PqOmemoMessage and journal.
use super::{
    bundle::{self, Bundle, NS},
    device::Device,
    native, payload,
    store::Tx,
};
use crate::crypto::{Error, Result, Secret, journal::Context};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use pq_signal::{IdentityKey, KexAuthEntry, PreKeySignalMessage};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use tokio_xmpp::minidom::Element;

pub const MAX_WIRE: usize = 4 * 1024 * 1024;
const MAX_WRAPPED: usize = 65536;
const MAX_TARGETS: usize = 500;
const JOURNAL_ENTRY: usize = 2 * 1024 * 1024;
const JOURNAL_BYTES: usize = 16 * 1024 * 1024;
#[derive(Clone)]
pub struct Target {
    pub peer: String,
    pub device: u32,
    pub bundle: Option<Bundle>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Token {
    owner: String,
    device: u32,
    epoch: [u8; 16],
    digest: String,
}
impl Token {
    pub fn digest(&self) -> &str {
        &self.digest
    }
}
#[derive(Clone)]
pub struct Received {
    pub outer: Element,
    pub content: Option<Element>,
    pub fingerprint: String,
    pub token: Token,
    pub timestamp: i64,
    pub context: Context,
}
#[derive(Serialize, Deserialize)]
struct Saved {
    version: u32,
    owner: String,
    device: u32,
    epoch: [u8; 16],
    digest: String,
    outer: String,
    content: Option<String>,
    fingerprint: String,
    timestamp: i64,
    context: Context,
}
fn invalid() -> Error {
    Error::Invalid("PQ encrypted envelope")
}
fn one<'a>(root: &'a Element, name: &str) -> Result<&'a Element> {
    bundle::single(root, name, NS)
}
fn bytes(root: &Element, max: usize) -> Result<Vec<u8>> {
    if root.children().next().is_some() {
        return Err(invalid());
    }
    let data = bundle::decode(&root.text(), max)?;
    if data.is_empty() {
        return Err(invalid());
    }
    Ok(data)
}
fn fixed<const N: usize>(root: &Element) -> Result<[u8; N]> {
    bytes(root, N)?.try_into().map_err(|_| invalid())
}
fn element(name: &str, bytes: &[u8]) -> Element {
    Element::builder(name, NS)
        .append(STANDARD.encode(bytes))
        .build()
}
fn epoch(tx: &Tx<'_>) -> Result<[u8; 16]> {
    if let Some(value) = tx.get("meta/incoming-epoch")? {
        return value.as_slice().try_into().map_err(|_| Error::Incomplete);
    }
    let value = crate::crypto::random::<16>()?;
    tx.put("meta/incoming-epoch", Some(&value))?;
    Ok(value)
}
fn journal_key(digest: &str) -> Result<String> {
    if digest.len() != 64
        || !digest
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    {
        return Err(Error::Incomplete);
    }
    Ok(format!("incoming/{digest}"))
}
fn decode_saved(tx: &Tx<'_>, bytes: &[u8]) -> Result<Received> {
    if bytes.len() > JOURNAL_ENTRY {
        return Err(Error::Incomplete);
    }
    let saved: Saved = serde_json::from_slice(bytes).map_err(|_| Error::Incomplete)?;
    if saved.version != 1
        || saved.owner != tx.owner
        || saved.device != tx.device
        || saved.epoch != epoch(tx)?
        || saved.fingerprint.len() != 128
        || !saved.fingerprint.bytes().all(|b| b.is_ascii_hexdigit())
    {
        return Err(Error::Incomplete);
    }
    journal_key(&saved.digest)?;
    Ok(Received {
        outer: bundle::document(&saved.outer, MAX_WIRE)?,
        content: saved
            .content
            .as_ref()
            .map(|s| bundle::document(s, payload::MAX))
            .transpose()?,
        fingerprint: saved.fingerprint,
        token: Token {
            owner: saved.owner,
            device: saved.device,
            epoch: saved.epoch,
            digest: saved.digest,
        },
        timestamp: saved.timestamp,
        context: saved.context,
    })
}
fn journal_space(tx: &Tx<'_>, extra: usize) -> Result<()> {
    let keys = tx.keys("incoming/")?;
    if keys.len() >= 128 || extra > JOURNAL_ENTRY {
        return Err(Error::Unavailable(
            "PQ incoming journal is full; persist pending messages before retrying",
        ));
    }
    let mut total = extra;
    for key in keys {
        total = total
            .checked_add(tx.get(&key)?.ok_or(Error::Incomplete)?.len())
            .ok_or(Error::Incomplete)?;
        if total > JOURNAL_BYTES {
            return Err(Error::Unavailable(
                "PQ incoming journal is full; persist pending messages before retrying",
            ));
        }
    }
    Ok(())
}
pub fn pending(device: &mut Device) -> Result<Vec<Received>> {
    let tx = device.begin()?;
    let keys = tx.keys("incoming/")?;
    if keys.len() > 128 {
        return Err(Error::Incomplete);
    }
    let mut total = 0;
    let mut result = Vec::new();
    for key in keys {
        let value = tx.get(&key)?.ok_or(Error::Incomplete)?;
        total += value.len();
        if total > JOURNAL_BYTES {
            return Err(Error::Incomplete);
        }
        result.push(decode_saved(&tx, &value)?);
    }
    tx.commit()?;
    Ok(result)
}
pub fn accepted(device: &mut Device, token: &Token) -> Result<()> {
    let tx = device.begin()?;
    if tx.owner != token.owner || tx.device != token.device || epoch(&tx)? != token.epoch {
        return Err(Error::Changed);
    }
    tx.put(&journal_key(&token.digest)?, None)?;
    tx.commit()
}
pub fn digest(outer: &Element, sender: &str, destination: &str) -> Result<String> {
    crate::crypto::address(sender, 1)?;
    crate::crypto::address(destination, 1)?;
    let encrypted = one(outer, "encrypted")?;
    let encrypted = bundle::xml(encrypted, MAX_WIRE)?;
    let mut hash = openssl::sha::Sha256::new();
    if outer.attr("type") == Some("groupchat") {
        hash.update(b"groupchat\0");
    }
    for value in [
        NS.as_bytes(),
        sender.as_bytes(),
        destination.as_bytes(),
        encrypted.as_slice(),
    ] {
        hash.update(&(value.len() as u32).to_be_bytes());
        hash.update(value);
    }
    Ok(crate::crypto::lifecycle::hex(&hash.finish()))
}
struct Wrapped {
    peer: String,
    device: u32,
    prekey: bool,
    data: Vec<u8>,
}
fn auth_entries(wrapped: &[Wrapped]) -> Vec<KexAuthEntry<'_>> {
    wrapped
        .iter()
        .filter(|key| key.prekey)
        .map(|key| KexAuthEntry {
            recipient_jid: &key.peer,
            recipient_device_id: key.device,
            prekey_message: &key.data,
        })
        .collect()
}
pub async fn encrypt(
    device: &mut Device,
    plaintext: &Element,
    targets: &[Target],
    transport: bool,
) -> Result<Element> {
    let (sender, destination) = payload::routing(plaintext)?;
    if sender != device.owner || targets.is_empty() || targets.len() > MAX_TARGETS {
        return Err(invalid());
    }
    let key = Secret::new(crate::crypto::random::<32>()?.to_vec());
    let tx = device.begin()?;
    let mut addresses = BTreeSet::new();
    let mut wrapped = Vec::new();
    for target in targets {
        crate::crypto::address(&target.peer, target.device)?;
        if !addresses.insert((target.peer.clone(), target.device))
            || target.peer == tx.owner && target.device == tx.device
        {
            return Err(invalid());
        }
        if let Some(bundle) = &target.bundle {
            native::authorize(
                &tx,
                &target.peer,
                target.device,
                &bundle.identity,
                &bundle.pq_identity,
            )?;
        } else {
            let pin = native::trust_record(&tx, &target.peer, target.device)?
                .ok_or(Error::TrustRequired)?;
            if pin.len() != 2625 {
                return Err(Error::Incomplete);
            }
            native::authorize(
                &tx,
                &target.peer,
                target.device,
                pin[..33].try_into().map_err(|_| Error::Incomplete)?,
                &pin[33..],
            )?;
        }
        if !native::has_session(&tx, &target.peer, target.device)? {
            let bundle = target.bundle.as_ref().ok_or(Error::InvalidBundle)?;
            native::process_bundle(
                &tx,
                &target.peer,
                target.device,
                &bundle.session_bundle(target.device)?,
            )
            .await?;
        }
        let (prekey, data) = native::encrypt(&tx, &target.peer, target.device, &key).await?;
        if data.is_empty() || data.len() > MAX_WRAPPED {
            return Err(invalid());
        }
        wrapped.push(Wrapped {
            peer: target.peer.clone(),
            device: target.device,
            prekey,
            data,
        });
    }
    let mut header = Element::builder("header", NS)
        .attr("sid".try_into().unwrap(), tx.device.to_string())
        .build();
    let entries = auth_entries(&wrapped);
    if !entries.is_empty() {
        let identity = native::identity(&tx)?;
        let pq = native::pq_identity(&tx)?;
        let transcript = pq_signal::pq_kex_auth_transcript(
            tx.owner,
            tx.device,
            identity.identity_key(),
            &pq.public_key(),
            &entries,
        );
        let (public, pq_signature, classic_signature) = native::sign_auth(&tx, &transcript)?;
        header.append_child(
            Element::builder("pq-auth", NS)
                .append(
                    Element::builder("pq-ik", NS)
                        .attr("type".try_into().unwrap(), "ML-DSA-87")
                        .append(STANDARD.encode(public))
                        .build(),
                )
                .append(element("pq-sig", &pq_signature))
                .append(element("ik-sig", &classic_signature))
                .build(),
        );
    }
    let mut groups = BTreeMap::<String, Vec<&Wrapped>>::new();
    for key in &wrapped {
        groups.entry(key.peer.clone()).or_default().push(key);
    }
    for (peer, keys) in groups {
        if keys.len() > 128 {
            return Err(invalid());
        }
        let mut group = Element::builder("keys", NS)
            .attr("jid".try_into().unwrap(), peer)
            .build();
        for wrapped in keys {
            let mut key = Element::builder("key", NS)
                .attr("rid".try_into().unwrap(), wrapped.device.to_string())
                .append(STANDARD.encode(&wrapped.data))
                .build();
            if wrapped.prekey {
                key.set_attr("".into(), "kex".try_into().unwrap(), "true");
            }
            group.append_child(key);
        }
        header.append_child(group);
    }
    let mut encrypted = Element::builder("encrypted", NS).append(header).build();
    if !transport {
        let plain = payload::envelope(plaintext)?;
        let binding = payload::binding(&sender, &destination, tx.device)?;
        encrypted.append_child(element("payload", &payload::seal(&plain, &key, &binding)?));
        encrypted.append_child(element("commit", &payload::commitment(&key, &binding)));
    }
    bundle::xml(&encrypted, MAX_WIRE)?;
    tx.commit()?;
    Ok(encrypted)
}
struct Envelope {
    encrypted: Element,
    sender_device: u32,
    wrapped: Vec<Wrapped>,
    own: usize,
}
impl Envelope {
    fn parse(outer: &Element, owner: &str, device: u32) -> Result<Self> {
        let encrypted = one(outer, "encrypted")?;
        bundle::xml(encrypted, MAX_WIRE)?;
        let header = one(encrypted, "header")?;
        let sender_device = bundle::positive(header.attr("sid"))?;
        let mut addresses = BTreeSet::new();
        let mut wrapped = Vec::new();
        let mut own = None;
        for group in header.children().filter(|group| group.is("keys", NS)) {
            let peer = group.attr("jid").ok_or_else(invalid)?;
            crate::crypto::address(peer, 1)?;
            let mut count = 0;
            for key in group.children() {
                count += 1;
                if !key.is("key", NS) || count > 128 || wrapped.len() >= MAX_TARGETS {
                    return Err(invalid());
                }
                let id = bundle::positive(key.attr("rid"))?;
                if !addresses.insert((peer.to_owned(), id)) {
                    return Err(invalid());
                }
                let prekey = match key.attr("kex") {
                    None | Some("false") => false,
                    Some("true") => true,
                    _ => return Err(invalid()),
                };
                if peer == owner && id == device {
                    own = Some(wrapped.len());
                }
                wrapped.push(Wrapped {
                    peer: peer.into(),
                    device: id,
                    prekey,
                    data: bytes(key, MAX_WRAPPED)?,
                });
            }
        }
        Ok(Self {
            encrypted: encrypted.clone(),
            sender_device,
            wrapped,
            own: own.ok_or(Error::Unavailable(
                "This PQ message is not encrypted for this device",
            ))?,
        })
    }
    fn authenticated_identity(&self, sender: &str) -> Result<Option<([u8; 33], Vec<u8>)>> {
        let own = &self.wrapped[self.own];
        if !own.prekey {
            return Ok(None);
        }
        let prekey = PreKeySignalMessage::try_from(own.data.as_slice()).map_err(|_| invalid())?;
        let header = one(&self.encrypted, "header")?;
        let auth = one(header, "pq-auth")?;
        let pq = one(auth, "pq-ik")?;
        if pq.attr("type") != Some("ML-DSA-87") {
            return Err(invalid());
        }
        let public = fixed::<2592>(pq)?;
        if !pq_signal::pq_kex_auth_verify(
            sender,
            self.sender_device,
            prekey.identity_key(),
            &public,
            &fixed::<4627>(one(auth, "pq-sig")?)?,
            &fixed::<64>(one(auth, "ik-sig")?)?,
            &auth_entries(&self.wrapped),
        ) {
            return Err(invalid());
        }
        Ok(Some((
            prekey
                .identity_key()
                .serialize()
                .as_ref()
                .try_into()
                .map_err(|_| invalid())?,
            public.to_vec(),
        )))
    }
}
pub async fn receive(
    device: &mut Device,
    outer: &Element,
    sender: &str,
    destination: &str,
    timestamp: i64,
    context: Context,
    blind: bool,
) -> Result<Received> {
    crate::crypto::address(sender, 1)?;
    crate::crypto::address(destination, 1)?;
    let digest = digest(outer, sender, destination)?;
    {
        let tx = device.begin()?;
        if let Some(saved) = tx.get(&journal_key(&digest)?)? {
            return decode_saved(&tx, &saved);
        }
    }
    let envelope = Envelope::parse(outer, &device.owner, device.id)?;
    if let Some((identity, pq)) = envelope.authenticated_identity(sender)? {
        device.observe_hybrid(sender, envelope.sender_device, &identity, &pq, blind)?;
    }
    let tx = device.begin()?;
    journal_space(&tx, 0)?;
    let pin = native::trust_record(&tx, sender, envelope.sender_device)?.ok_or_else(|| {
        if sender == tx.owner {
            Error::OwnDeviceTrustRequired
        } else {
            Error::TrustRequired
        }
    })?;
    if pin.len() != 2625 {
        return Err(Error::Incomplete);
    }
    native::authorize(
        &tx,
        sender,
        envelope.sender_device,
        pin[..33].try_into().map_err(|_| Error::Incomplete)?,
        &pin[33..],
    )?;
    let own = &envelope.wrapped[envelope.own];
    let key = native::decrypt(&tx, sender, envelope.sender_device, &own.data, own.prekey).await?;
    if key.len() != 32 {
        return Err(invalid());
    }
    let content = if let Some(encrypted) = bundle::optional(&envelope.encrypted, "payload")? {
        let ciphertext = bytes(encrypted, payload::MAX + 16)?;
        let commitment = fixed::<64>(one(&envelope.encrypted, "commit")?)?;
        let plain = payload::open(
            &ciphertext,
            &key,
            &payload::binding(sender, destination, envelope.sender_device)?,
            &commitment,
        )?;
        let mut route = Element::builder("message", payload::CLIENT)
            .attr("from".try_into().unwrap(), sender)
            .attr("to".try_into().unwrap(), destination)
            .attr(
                "type".try_into().unwrap(),
                if outer.attr("type") == Some("groupchat") {
                    "groupchat"
                } else {
                    "chat"
                },
            )
            .build();
        if let Some(id) = outer.attr("id") {
            route.set_attr("".into(), "id".try_into().unwrap(), id);
        }
        let mut result = payload::content(&route, &plain, timestamp)?;
        for name in ["from", "to", "type"] {
            if let Some(value) = outer.attr(name) {
                result.set_attr("".into(), name.try_into().unwrap(), value);
            }
        }
        Some(result)
    } else {
        if bundle::optional(&envelope.encrypted, "commit")?.is_some() {
            return Err(invalid());
        }
        None
    };
    let epoch = epoch(&tx)?;
    let fingerprint = bundle::fingerprint(&pin[..33], &pin[33..])?;
    let saved = Saved {
        version: 1,
        owner: tx.owner.into(),
        device: tx.device,
        epoch,
        digest: digest.clone(),
        outer: String::from_utf8(bundle::xml(outer, MAX_WIRE)?).map_err(|_| invalid())?,
        content: content
            .as_ref()
            .map(|root| String::from_utf8(bundle::xml(root, payload::MAX)?).map_err(|_| invalid()))
            .transpose()?,
        fingerprint: fingerprint.clone(),
        timestamp,
        context: context.clone(),
    };
    let encoded = Secret::new(serde_json::to_vec(&saved).map_err(|_| Error::Incomplete)?);
    journal_space(&tx, encoded.len())?;
    tx.put(&journal_key(&digest)?, Some(&encoded))?;
    let token = Token {
        owner: tx.owner.into(),
        device: tx.device,
        epoch,
        digest,
    };
    tx.commit()?;
    Ok(Received {
        outer: outer.clone(),
        content,
        fingerprint,
        token,
        timestamp,
        context,
    })
}

#[cfg(test)]
mod interop_tests {
    use super::*;
    use std::{
        io::{BufRead, BufReader, Write},
        process::{Child, ChildStdin, ChildStdout, Command, Stdio},
    };
    struct Java {
        child: Child,
        input: ChildStdin,
        output: BufReader<ChildStdout>,
    }
    impl Drop for Java {
        fn drop(&mut self) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
    impl Java {
        fn start(path: &str) -> Self {
            let mut child = Command::new(path)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::inherit())
                .spawn()
                .unwrap();
            let input = child.stdin.take().unwrap();
            let output = BufReader::new(child.stdout.take().unwrap());
            Self {
                child,
                input,
                output,
            }
        }
        fn command(&mut self, name: &str, fields: &[String]) {
            writeln!(self.input, "{name}\t{}", fields.join("\t")).unwrap();
            self.input.flush().unwrap();
        }
        fn reply(&mut self, expected: &str) -> String {
            for _ in 0..128 {
                let mut line = String::new();
                assert!(
                    self.output.read_line(&mut line).unwrap() > 0,
                    "Java bridge exited"
                );
                if let Some(value) = line.strip_prefix("PQBRIDGE\t") {
                    let (kind, value) = value
                        .trim_end_matches(['\r', '\n'])
                        .split_once('\t')
                        .unwrap_or((value.trim_end(), ""));
                    assert_eq!(kind, expected, "Java fixture operation failed");
                    return value.into();
                }
            }
            panic!("Java bridge protocol unavailable");
        }
        fn bundle(&mut self) -> (u32, Bundle) {
            let reply = self.reply("BUNDLE");
            let (id, xml) = reply.split_once('\t').unwrap();
            let xml = String::from_utf8(STANDARD.decode(xml).unwrap()).unwrap();
            (
                id.parse().unwrap(),
                Bundle::parse(&bundle::document(&xml, bundle::MAX).unwrap()).unwrap(),
            )
        }
        fn pin(&mut self, id: u32, bundle: &Bundle) {
            self.command(
                "BUNDLE",
                &[
                    id.to_string(),
                    STANDARD.encode(bundle::xml(&bundle.xml(), bundle::MAX).unwrap()),
                ],
            );
            self.reply("READY");
        }
        fn message(&mut self) -> Element {
            let xml = STANDARD.decode(self.reply("MESSAGE")).unwrap();
            bundle::document(std::str::from_utf8(&xml).unwrap(), MAX_WIRE).unwrap()
        }
    }
    fn plain(from: &str, to: &str, body: &str) -> Element {
        Element::builder("message", payload::CLIENT)
            .attr("from".try_into().unwrap(), from)
            .attr("to".try_into().unwrap(), to)
            .attr("type".try_into().unwrap(), "chat")
            .attr("id".try_into().unwrap(), uuid::Uuid::new_v4().to_string())
            .append(
                Element::builder("body", payload::CLIENT)
                    .append(body)
                    .build(),
            )
            .build()
    }
    async fn send(device: &mut Device, target: Target, own: Option<Target>, body: &str) -> Element {
        let mut root = plain(&device.owner, &target.peer, body);
        let mut targets = vec![target];
        if let Some(own) = own {
            targets.push(own);
        }
        let encrypted = encrypt(device, &root, &targets, false).await.unwrap();
        root = Element::builder("message", payload::CLIENT)
            .attr("from".try_into().unwrap(), root.attr("from").unwrap())
            .attr("to".try_into().unwrap(), root.attr("to").unwrap())
            .attr("type".try_into().unwrap(), "chat")
            .attr("id".try_into().unwrap(), root.attr("id").unwrap())
            .append(encrypted)
            .build();
        root
    }
    #[test]
    #[ignore = "Requires the private keyring fixture and actual cached mobile Java/JNI bridge"]
    fn actual_mobile_wire_both_initiators_restart_own_copy_and_tamper() {
        assert_eq!(
            std::env::var("MONOGRAM_KEYRING_FIXTURE").as_deref(),
            Ok("1")
        );
        let bridge =
            std::env::var("MONOGRAM_PQ_JAVA_BRIDGE").expect("compiled actual mobile bridge");
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                let root = std::path::PathBuf::from(std::env::var_os("XDG_STATE_HOME").unwrap())
                    .join("pq-wire-interop");
                let desktop = "desktop@example.invalid";
                let phone = "phone@example.invalid";
                let mut java = Java::start(&bridge);
                let (phone_id, phone_bundle) = java.bundle();
                let mut local = Device::open(&root, desktop, true).await.unwrap();
                let local_id = local.id;
                let local_bundle = Bundle::from_parts(local.prepare().unwrap()).unwrap();
                local
                    .observe(phone, phone_id, &phone_bundle, false)
                    .unwrap();
                local
                    .decide(phone, phone_id, &phone_bundle.fingerprint().unwrap(), true)
                    .unwrap();
                java.pin(local.id, &local_bundle);
                let mut own = Device::memory(desktop, 303).unwrap();
                let own_bundle = Bundle::from_parts(own.prepare().unwrap()).unwrap();
                local.observe(desktop, own.id, &own_bundle, false).unwrap();
                local
                    .decide(desktop, own.id, &own_bundle.fingerprint().unwrap(), true)
                    .unwrap();
                own.observe(desktop, local.id, &local_bundle, false)
                    .unwrap();
                own.decide(
                    desktop,
                    local.id,
                    &local_bundle.fingerprint().unwrap(),
                    true,
                )
                .unwrap();
                let target = Target {
                    peer: phone.into(),
                    device: phone_id,
                    bundle: Some(phone_bundle.clone()),
                };
                let message = send(
                    &mut local,
                    target.clone(),
                    Some(Target {
                        peer: desktop.into(),
                        device: own.id,
                        bundle: Some(own_bundle),
                    }),
                    "Rust to mobile",
                )
                .await;
                let copied = receive(
                    &mut own,
                    &message,
                    desktop,
                    phone,
                    chrono::Utc::now().timestamp_millis(),
                    Context::default(),
                    false,
                )
                .await
                .unwrap();
                assert_eq!(
                    copied
                        .content
                        .unwrap()
                        .get_child("body", payload::CLIENT)
                        .unwrap()
                        .text(),
                    "Rust to mobile"
                );
                java.command(
                    "MESSAGE",
                    &[STANDARD.encode(bundle::xml(&message, MAX_WIRE).unwrap())],
                );
                assert_eq!(
                    STANDARD.decode(java.reply("DECRYPTED")).unwrap(),
                    b"Rust to mobile"
                );
                let reply = java.message();
                let timestamp = chrono::Utc::now().timestamp_millis();
                let encrypted = one(&reply, "encrypted").unwrap();
                let committed = one(encrypted, "commit").unwrap().text();
                let bad = bundle::xml(&reply, MAX_WIRE).unwrap();
                let bad = String::from_utf8(bad)
                    .unwrap()
                    .replace(&committed, &STANDARD.encode([0; 64]));
                assert!(
                    receive(
                        &mut local,
                        &bundle::document(&bad, MAX_WIRE).unwrap(),
                        phone,
                        desktop,
                        timestamp,
                        Context::default(),
                        false
                    )
                    .await
                    .is_err()
                );
                let good = receive(
                    &mut local,
                    &reply,
                    phone,
                    desktop,
                    timestamp,
                    Context::default(),
                    false,
                )
                .await
                .unwrap();
                assert_eq!(
                    good.content
                        .unwrap()
                        .get_child("body", payload::CLIENT)
                        .unwrap()
                        .text(),
                    "Mobile reply"
                );
                accepted(&mut local, &good.token).unwrap();
                drop(local);
                let mut local = Device::open(&root, desktop, false).await.unwrap();
                assert_eq!(local.id, local_id);
                let message = send(&mut local, target, None, "Rust after restart").await;
                java.command(
                    "MESSAGE",
                    &[STANDARD.encode(bundle::xml(&message, MAX_WIRE).unwrap())],
                );
                assert_eq!(
                    STANDARD.decode(java.reply("DECRYPTED")).unwrap(),
                    b"Rust after restart"
                );
                let reply = java.message();
                let good = receive(
                    &mut local,
                    &reply,
                    phone,
                    desktop,
                    chrono::Utc::now().timestamp_millis(),
                    Context::default(),
                    false,
                )
                .await
                .unwrap();
                assert_eq!(
                    good.content
                        .unwrap()
                        .get_child("body", payload::CLIENT)
                        .unwrap()
                        .text(),
                    "Mobile reply"
                );
                accepted(&mut local, &good.token).unwrap();
                java.command("RESET", &[]);
                let (phone_id, phone_bundle) = java.bundle();
                let mut fresh = Device::memory(desktop, 404).unwrap();
                let fresh_bundle = Bundle::from_parts(fresh.prepare().unwrap()).unwrap();
                fresh
                    .observe(phone, phone_id, &phone_bundle, false)
                    .unwrap();
                fresh
                    .decide(phone, phone_id, &phone_bundle.fingerprint().unwrap(), true)
                    .unwrap();
                java.pin(fresh.id, &fresh_bundle);
                java.command("INITIATE", &[]);
                let first = java.message();
                let good = receive(
                    &mut fresh,
                    &first,
                    phone,
                    desktop,
                    chrono::Utc::now().timestamp_millis(),
                    Context::default(),
                    false,
                )
                .await
                .unwrap();
                assert_eq!(
                    good.content
                        .unwrap()
                        .get_child("body", payload::CLIENT)
                        .unwrap()
                        .text(),
                    "Mobile initiates"
                );
                java.command("QUIT", &[]);
                java.reply("DONE");
                assert!(java.child.wait().unwrap().success());
            });
    }
}
