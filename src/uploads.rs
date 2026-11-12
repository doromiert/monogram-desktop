//! XEP-0363 uploads and mobile-compatible XEP-0454 encryption. All file I/O runs off GTK.
use crate::{
    file_share::{Attachment, Link, MAX},
    iq::{Client, Kind},
};
use gtk::{gio, prelude::*};
use openssl::{
    hash::{Hasher, MessageDigest},
    symm::{Cipher, Crypter, Mode},
};
use reqwest::{
    Url,
    header::{HeaderMap, HeaderName, HeaderValue},
};
use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio_xmpp::minidom::Element;
use zeroize::Zeroizing;
pub const NS: &str = "urn:xmpp:http:upload:0";
const INFO: &str = "http://jabber.org/protocol/disco#info";
const ITEMS: &str = "http://jabber.org/protocol/disco#items";
const DATA: &str = "jabber:x:data";
type Result<T> = std::result::Result<T, &'static str>;

pub use crate::model::UploadDraft as Draft;
#[derive(Clone, Debug)]
pub enum Phase {
    Discovering,
    Preparing,
    Waiting,
    Reconnecting,
    Uploading { bytes: u64, total: u64 },
}
pub struct Job {
    pub cancel: AtomicBool,
    pub source: gio::Cancellable,
    pub phase: Mutex<Phase>,
}
impl Default for Job {
    fn default() -> Self {
        Self {
            cancel: AtomicBool::new(false),
            source: gio::Cancellable::new(),
            phase: Mutex::new(Phase::Discovering),
        }
    }
}
impl Job {
    pub fn stop(&self) {
        self.cancel.store(true, Ordering::Release);
        self.source.cancel();
    }
    fn check(&self) -> Result<()> {
        if self.cancel.load(Ordering::Acquire) {
            Err("Upload cancelled")
        } else {
            Ok(())
        }
    }
}
struct Provider {
    jid: String,
    limit: u64,
}
fn value(field: &Element) -> Option<String> {
    let values: Vec<_> = field.children().filter(|e| e.is("value", DATA)).collect();
    (values.len() == 1 && values[0].children().next().is_none()).then(|| values[0].text())
}
fn provider(root: &Element, jid: &str) -> Result<Option<Provider>> {
    if !root.is("query", INFO) {
        return Err("Invalid upload discovery response");
    }
    if !root
        .children()
        .any(|c| c.is("feature", INFO) && c.attr("var") == Some(NS))
    {
        return Ok(None);
    }
    let mut limit = None;
    let mut forms = 0;
    for form in root.children().filter(|c| c.is("x", DATA)) {
        let types: Vec<_> = form
            .children()
            .filter(|c| c.is("field", DATA) && c.attr("var") == Some("FORM_TYPE"))
            .collect();
        if !types
            .iter()
            .any(|f| f.children().any(|v| v.is("value", DATA) && v.text() == NS))
        {
            continue;
        }
        forms += 1;
        if forms > 1
            || form.attr("type") != Some("result")
            || types.len() != 1
            || value(types[0]).as_deref() != Some(NS)
        {
            return Err("Invalid upload size limit");
        }
        let fields: Vec<_> = form
            .children()
            .filter(|c| c.is("field", DATA) && c.attr("var") == Some("max-file-size"))
            .collect();
        if fields.len() > 1 {
            return Err("Invalid upload size limit");
        }
        if let Some(field) = fields.first() {
            let text = value(field).ok_or("Invalid upload size limit")?;
            if text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit()) {
                return Err("Invalid upload size limit");
            }
            limit = Some(
                text.parse::<u64>()
                    .map_err(|_| "Invalid upload size limit")?,
            );
        }
    }
    Ok(Some(Provider {
        jid: jid.into(),
        limit: limit.unwrap_or(MAX + 16).min(MAX + 16),
    }))
}
async fn discover(iq: &Client, owner: &str) -> Result<Provider> {
    let domain = owner
        .parse::<tokio_xmpp::jid::BareJid>()
        .map_err(|_| "Invalid upload account")?
        .domain()
        .to_string();
    let search = async {
        let (own, items) = tokio::join!(
            iq.request(&domain, Kind::Get, crate::protocol::el("query", INFO)),
            iq.request(&domain, Kind::Get, crate::protocol::el("query", ITEMS))
        );
        let mut candidates = vec![];
        if let Ok(Some(root)) = own {
            if let Ok(Some(p)) = provider(&root, &domain) {
                candidates.push(p);
            }
        }
        let mut jids = vec![];
        if let Ok(Some(root)) = items {
            if root.is("query", ITEMS) {
                for item in root.children().filter(|c| c.is("item", ITEMS)).take(64) {
                    if item.attr("node").is_some() {
                        continue;
                    }
                    if let Some(jid) = item.attr("jid").filter(|v| v.len() <= 3071 && *v != domain)
                    {
                        if jid
                            .parse::<tokio_xmpp::jid::Jid>()
                            .ok()
                            .is_some_and(|j| j.to_string() == jid)
                            && !jids.iter().any(|v| v == jid)
                        {
                            jids.push(jid.to_string());
                        }
                    }
                }
            }
        }
        let replies = futures_util::future::join_all(
            jids.iter()
                .map(|jid| iq.request(jid, Kind::Get, crate::protocol::el("query", INFO))),
        )
        .await;
        for (jid, reply) in jids.iter().zip(replies) {
            if let Ok(Some(root)) = reply {
                if let Ok(Some(p)) = provider(&root, jid) {
                    candidates.push(p);
                }
            }
        }
        candidates
            .into_iter()
            .max_by_key(|p| p.limit)
            .ok_or("No HTTP upload service available")
    };
    tokio::time::timeout(Duration::from_secs(45), search)
        .await
        .map_err(|_| "Upload discovery timed out")?
}
struct Slot {
    put: Url,
    get: Url,
    headers: HeaderMap,
}
fn only<'a>(root: &'a Element, name: &str) -> Result<&'a Element> {
    let mut children = root.children().filter(|c| c.is(name, NS));
    let child = children.next().ok_or("Invalid upload slot")?;
    if children.next().is_some() {
        return Err("Invalid upload slot");
    }
    Ok(child)
}
fn slot(root: &Element) -> Result<Slot> {
    if !root.is("slot", NS) {
        return Err("Invalid upload slot");
    }
    let put = only(root, "put")?;
    let get = only(root, "get")?;
    let address = |e: &Element| -> Result<Url> {
        let link =
            Link::parse(e.attr("url").ok_or("Invalid upload URL")?).ok_or("Invalid upload URL")?;
        if link.encrypted() {
            return Err("Invalid upload URL");
        }
        Ok(link.url.clone())
    };
    let mut headers = HeaderMap::new();
    let mut bytes = 0;
    for header in put.children().filter(|c| c.is("header", NS)) {
        let name = header.attr("name").ok_or("Invalid upload headers")?;
        // Mobile rejects CR/LF rather than repairing potentially malicious credentials.
        if name.contains(['\r', '\n']) {
            return Err("Invalid upload headers");
        }
        if !["authorization", "cookie", "expires"]
            .iter()
            .any(|v| name.eq_ignore_ascii_case(v))
        {
            continue;
        }
        let text = header.text();
        bytes += name.len() + text.len();
        if header.children().next().is_some()
            || text.contains(['\r', '\n'])
            || bytes > 16384
            || headers.len() >= 32
        {
            return Err("Invalid upload headers");
        }
        let name = HeaderName::from_bytes(name.as_bytes()).map_err(|_| "Invalid upload headers")?;
        let mut value = HeaderValue::from_str(&text).map_err(|_| "Invalid upload headers")?;
        value.set_sensitive(true);
        headers.append(name, value);
    }
    Ok(Slot {
        put: address(put)?,
        get: address(get)?,
        headers,
    })
}
pub fn snapshot_path(owner: &str, id: &str) -> Result<PathBuf> {
    let hash: String = openssl::sha::sha256(id.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    Ok(crate::downloads::account_root(owner)?
        .join("uploads")
        .join(format!("{hash}.input")))
}
struct Partial(PathBuf);
impl Drop for Partial {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}
pub struct Prepared {
    pub path: PathBuf,
    pub name: String,
    pub mime: String,
    pub size: u64,
    pub hash: Vec<u8>,
}
fn name(value: &str) -> String {
    let mut units = 0;
    let text: String = value
        .chars()
        .map(|c| {
            if c.is_control() || matches!(c, '/' | '\\') {
                '_'
            } else {
                c
            }
        })
        .take_while(|c| {
            units += c.len_utf16();
            units <= 200
        })
        .collect();
    if text.trim().is_empty() || text == "." || text == ".." {
        "attachment".into()
    } else {
        text
    }
}
fn prepare(path: PathBuf, draft: &Draft, limit: u64, job: &Job) -> Result<Prepared> {
    job.check()?;
    *job.phase.lock().unwrap() = Phase::Preparing;
    if !path.exists() {
        let source = gio::File::for_uri(&draft.uri);
        let info = source
            .query_info(
                "standard::size,standard::type",
                gio::FileQueryInfoFlags::NONE,
                Some(&job.source),
            )
            .map_err(|_| "Cannot read selected file")?;
        if matches!(
            info.file_type(),
            gio::FileType::Directory | gio::FileType::Special
        ) {
            return Err("Select a regular file");
        }
        let known = info
            .has_attribute("standard::size")
            .then(|| info.size())
            .filter(|n| *n >= 0)
            .map(|n| n as u64);
        if known.is_some_and(|n| n == 0 || n > limit) {
            return Err("File exceeds the upload size limit or is empty");
        }
        let input = source
            .read(Some(&job.source))
            .map_err(|_| "Cannot read selected file")?;
        let temporary = Partial(path.with_extension(format!("{}.part", uuid::Uuid::new_v4())));
        let mut output = {
            let _guard = crate::downloads::CACHE_IO.lock().unwrap();
            job.check()?;
            let parent = path.parent().unwrap();
            fs::create_dir_all(parent).map_err(|_| "Upload cache unavailable")?;
            fs::set_permissions(parent, fs::Permissions::from_mode(0o700))
                .map_err(|_| "Upload cache unavailable")?;
            // The account root is also private when uploads are its first use.
            fs::set_permissions(parent.parent().unwrap(), fs::Permissions::from_mode(0o700))
                .map_err(|_| "Upload cache unavailable")?;
            OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&temporary.0)
                .map_err(|_| "Upload cache unavailable")?
        };
        let mut bytes = 0u64;
        let mut buffer = [0; 65536];
        loop {
            job.check()?;
            let n = input
                .read(&mut buffer, Some(&job.source))
                .map_err(|_| "Cannot read selected file")?;
            if n == 0 {
                break;
            }
            bytes += n as u64;
            if bytes > limit || known.is_some_and(|n| bytes > n) {
                return Err("Selected file changed or exceeds the upload size limit");
            }
            output
                .write_all(&buffer[..n])
                .map_err(|_| "Upload cache unavailable")?;
        }
        if bytes == 0 || known.is_some_and(|n| n != bytes) {
            return Err("Selected file changed or is empty");
        }
        output.sync_all().map_err(|_| "Upload cache unavailable")?;
        drop(output);
        let _guard = crate::downloads::CACHE_IO.lock().unwrap();
        job.check()?;
        fs::rename(&temporary.0, &path).map_err(|_| "Upload cache unavailable")?;
    }
    let mut input = File::open(&path).map_err(|_| "Upload cache unavailable")?;
    let metadata = input.metadata().map_err(|_| "Upload cache unavailable")?;
    if !metadata.is_file() || metadata.len() == 0 || metadata.len() > limit {
        return Err("File exceeds the upload size limit or is empty");
    }
    let size = metadata.len();
    let name = name(&draft.name);
    let mut hash = Hasher::new(MessageDigest::sha256()).map_err(|_| "Hash unavailable")?;
    let mut buffer = [0; 65536];
    let mut prefix = vec![];
    let mut bytes = 0;
    loop {
        job.check()?;
        let n = input
            .read(&mut buffer)
            .map_err(|_| "Upload cache unavailable")?;
        if n == 0 {
            break;
        }
        if bytes == 0 {
            prefix.extend_from_slice(&buffer[..n]);
        }
        bytes += n as u64;
        if bytes > size {
            return Err("Upload snapshot changed");
        }
        hash.update(&buffer[..n]).map_err(|_| "Hash unavailable")?;
    }
    if bytes != size {
        return Err("Upload snapshot changed");
    }
    let (content, _) = gio::content_type_guess(Some(Path::new(&name)), Some(prefix.as_slice()));
    let mut mime = gio::content_type_get_mime_type(&content)
        .map(|v| v.to_string())
        .unwrap_or_else(|| "application/octet-stream".into());
    if draft.sticker && prefix.starts_with(&[0x1f, 0x8b]) {
        // A filename is only a hint. Validate the retained snapshot in the same
        // bounded no-network renderer used by incoming TGS stickers.
        let verified = crate::downloads::VerifiedFile(input);
        let _validated = crate::lottie::Decoder::open(verified, 1, &job.source)?;
        job.check()?;
        mime = "application/x-tgsticker".into();
    }
    Ok(Prepared {
        path,
        name,
        mime,
        size,
        hash: hash.finish().map_err(|_| "Hash unavailable")?.to_vec(),
    })
}
struct Input {
    file: File,
    size: u64,
    bytes: u64,
    cipher: Option<Crypter>,
    hash: Hasher,
    expected: Vec<u8>,
    done: Arc<AtomicBool>,
    job: Arc<Job>,
    report_progress: bool,
    finished: Option<tokio::sync::oneshot::Sender<()>>,
}
impl Input {
    fn next(&mut self) -> Result<Option<Vec<u8>>> {
        self.job.check()?;
        if self.done.load(Ordering::Acquire) {
            return Ok(None);
        }
        let mut buffer = vec![0; 65536];
        let n = self
            .file
            .read(&mut buffer)
            .map_err(|_| "Upload snapshot unavailable")?;
        if n == 0 {
            return Err("Upload snapshot changed");
        }
        self.bytes += n as u64;
        if self.bytes > self.size {
            return Err("Upload snapshot changed");
        }
        self.hash
            .update(&buffer[..n])
            .map_err(|_| "Hash unavailable")?;
        let mut output = if let Some(cipher) = &mut self.cipher {
            let mut output = vec![0; n + 32];
            let len = cipher
                .update(&buffer[..n], &mut output)
                .map_err(|_| "Upload encryption failed")?;
            output.truncate(len);
            output
        } else {
            buffer.truncate(n);
            buffer
        };
        if self.bytes == self.size {
            if self
                .file
                .read(&mut [0; 1])
                .map_err(|_| "Upload snapshot unavailable")?
                != 0
                || !openssl::memcmp::eq(
                    &self.hash.finish().map_err(|_| "Hash unavailable")?,
                    &self.expected,
                )
            {
                return Err("Upload snapshot changed");
            }
            if let Some(cipher) = &mut self.cipher {
                let mut tail = [0; 32];
                let length = cipher
                    .finalize(&mut tail)
                    .map_err(|_| "Upload encryption failed")?;
                cipher
                    .get_tag(&mut tail[length..length + 16])
                    .map_err(|_| "Upload encryption failed")?;
                output.extend_from_slice(&tail[..length + 16]);
            }
            self.done.store(true, Ordering::Release);
        }
        if self.report_progress {
            *self.job.phase.lock().unwrap() = if self.done.load(Ordering::Acquire) {
                Phase::Waiting
            } else {
                Phase::Uploading {
                    bytes: self.bytes,
                    total: self.size,
                }
            };
        }
        if self.done.load(Ordering::Acquire)
            && let Some(finished) = self.finished.take()
        {
            let _ = finished.send(());
        }
        Ok(Some(output))
    }
}
// Keep one private, unlinked wire snapshot: retries never encrypt mutable plaintext again.
struct Wire {
    input: crate::downloads::VerifiedFile,
    size: u64,
    hash: Vec<u8>,
}
fn prepare_wire(prepared: &Prepared, cipher: Option<Crypter>, job: Arc<Job>) -> Result<Wire> {
    job.check()?;
    let partial = Partial(
        prepared
            .path
            .with_extension(format!("{}.wire", uuid::Uuid::new_v4())),
    );
    let mut output = {
        let _guard = crate::downloads::CACHE_IO.lock().unwrap();
        job.check()?;
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&partial.0)
            .map_err(|_| "Upload wire snapshot unavailable")?
    };
    let mut input = Input {
        file: File::open(&prepared.path).map_err(|_| "Upload snapshot unavailable")?,
        size: prepared.size,
        bytes: 0,
        cipher,
        hash: Hasher::new(MessageDigest::sha256()).map_err(|_| "Hash unavailable")?,
        expected: prepared.hash.clone(),
        done: Arc::new(AtomicBool::new(false)),
        job: job.clone(),
        report_progress: false,
        finished: None,
    };
    let mut hash = Hasher::new(MessageDigest::sha256()).map_err(|_| "Hash unavailable")?;
    let mut size = 0;
    while let Some(chunk) = input.next()? {
        output
            .write_all(&chunk)
            .map_err(|_| "Upload wire snapshot unavailable")?;
        hash.update(&chunk).map_err(|_| "Hash unavailable")?;
        size += chunk.len() as u64;
    }
    drop(output);
    job.check()?;
    let input = crate::downloads::VerifiedFile(
        File::open(&partial.0).map_err(|_| "Upload wire snapshot unavailable")?,
    );
    drop(partial);
    Ok(Wire {
        input,
        size,
        hash: hash.finish().map_err(|_| "Hash unavailable")?.to_vec(),
    })
}
async fn put_wire(
    client: &reqwest::Client,
    slot: &Slot,
    mime: &str,
    wire: &Wire,
    job: Arc<Job>,
    ack_timeout: Duration,
) -> Result<()> {
    for attempt in 0..2 {
        job.check()?;
        if attempt == 1 {
            *job.phase.lock().unwrap() = Phase::Reconnecting;
        } else {
            *job.phase.lock().unwrap() = Phase::Uploading {
                bytes: 0,
                total: wire.size,
            };
        }
        let done = Arc::new(AtomicBool::new(false));
        let (finished, consumed) = tokio::sync::oneshot::channel();
        let input = Input {
            file: wire
                .input
                .reopen()
                .map_err(|_| "Upload wire snapshot unavailable")?
                .0,
            size: wire.size,
            bytes: 0,
            cipher: None,
            hash: Hasher::new(MessageDigest::sha256()).map_err(|_| "Hash unavailable")?,
            expected: wire.hash.clone(),
            done: done.clone(),
            job: job.clone(),
            report_progress: true,
            finished: Some(finished),
        };
        let stream = futures_util::stream::try_unfold(input, |mut input| async move {
            input
                .next()
                .map(|chunk| chunk.map(|chunk| (chunk, input)))
                .map_err(std::io::Error::other)
        });
        let response = client
            .put(slot.put.clone())
            .headers(slot.headers.clone())
            .header(reqwest::header::CONTENT_TYPE, mime)
            .header(reqwest::header::CONTENT_LENGTH, wire.size)
            .body(reqwest::Body::wrap_stream(stream))
            .send();
        tokio::pin!(response);
        let result = tokio::select! {
            result = &mut response => Some(result),
            consumed = consumed => {
                if consumed.is_ok() && attempt == 0 {
                    tokio::time::timeout(ack_timeout, &mut response).await.ok()
                } else { Some(response.await) }
            }
        };
        job.check()?;
        match result {
            Some(Ok(response)) => {
                return if [200, 201, 204].contains(&response.status().as_u16())
                    && done.load(Ordering::Acquire)
                {
                    Ok(())
                } else {
                    Err("Upload was not accepted")
                };
            }
            Some(Err(error))
                if attempt == 0
                    && !error.is_body()
                    && (error.is_timeout() || error.is_connect() || error.is_request()) => {}
            None if attempt == 0 => {}
            _ => return Err("HTTPS upload failed"),
        }
        // RFC 9110 section 9.2.2: repeat this PUT once, to the same URI with identical bytes.
        eprintln!("Monogram upload response stalled; reconnecting once");
    }
    Err("HTTPS upload failed")
}

pub struct Uploaded {
    pub attachment: Attachment,
    pub snapshot: PathBuf,
}
pub async fn run(
    iq: &Client,
    owner: &str,
    id: &str,
    draft: &Draft,
    mode: crate::encryption_policy::Mode,
    job: Arc<Job>,
) -> Result<Uploaded> {
    let encrypted = mode.encrypted();
    let opaque = matches!(mode, crate::encryption_policy::Mode::Omemo2 | crate::encryption_policy::Mode::Pq);
    job.check()?;
    let provider = discover(iq, owner).await?;
    job.check()?;
    let limit = provider
        .limit
        .saturating_sub(if encrypted { 16 } else { 0 })
        .min(MAX);
    let prepared = prepare(snapshot_path(owner, id)?, draft, limit, &job)?;
    let filename = if opaque {
        format!("{}.bin", uuid::Uuid::new_v4())
    } else {
        prepared.name.clone()
    };
    let mime = if opaque {
        "application/octet-stream"
    } else {
        &prepared.mime
    };
    let wire_size = prepared.size + if encrypted { 16 } else { 0 };
    let mut secret = Zeroizing::new([0u8; 44]);
    let cipher = if encrypted {
        openssl::rand::rand_bytes(secret.as_mut()).map_err(|_| "Secure randomness unavailable")?;
        Some(
            Crypter::new(
                Cipher::aes_256_gcm(),
                Mode::Encrypt,
                &secret[12..],
                Some(&secret[..12]),
            )
            .map_err(|_| "Upload encryption unavailable")?,
        )
    } else {
        None
    };
    let wire = prepare_wire(&prepared, cipher, job.clone())?;
    if wire.size != wire_size {
        return Err("Upload size mismatch");
    }
    let request = Element::builder("request", NS)
        .attr("filename".try_into().unwrap(), &filename)
        .attr("size".try_into().unwrap(), wire_size.to_string())
        .attr("content-type".try_into().unwrap(), mime)
        .build();
    let root = iq
        .request(&provider.jid, Kind::Get, request)
        .await
        .map_err(|_| "Upload slot request failed")?
        .ok_or("Invalid upload slot")?;
    let slot = slot(&root)?;
    job.check()?;
    let client = crate::downloads::client_builder()
        .build()
        .map_err(|_| "HTTPS unavailable")?;
    put_wire(
        &client,
        &slot,
        mime,
        &wire,
        job.clone(),
        Duration::from_secs(3),
    )
    .await?;
    let wire_url = if encrypted {
        let hex: String = secret.iter().map(|b| format!("{b:02x}")).collect();
        format!("aesgcm{}#{hex}", &slot.get.as_str()[5..])
    } else {
        slot.get.to_string()
    };
    let attachment = Attachment {
        wire_url,
        name: prepared.name,
        mime: prepared.mime,
        size: Some(prepared.size),
        width: None,
        height: None,
        hashes: BTreeMap::from([("sha-256".into(), prepared.hash)]),
        manual: false,
        sticker: draft.sticker,
    };
    if !attachment.valid_outgoing(encrypted) {
        return Err("Invalid uploaded attachment");
    }
    Ok(Uploaded {
        attachment,
        snapshot: prepared.path,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    struct Directory(PathBuf);
    impl Directory {
        fn new() -> Self {
            let path =
                std::env::temp_dir().join(format!("monogram-upload-{}", uuid::Uuid::new_v4()));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Directory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    fn xml(text: &str) -> Element {
        text.parse().unwrap()
    }
    struct Https {
        url: String,
        cert: Vec<u8>,
        requests: Arc<Mutex<Vec<(String, Vec<u8>)>>>,
        stop: Arc<AtomicBool>,
        thread: Option<std::thread::JoinHandle<()>>,
    }
    impl Https {
        fn new() -> Self {
            use openssl::{
                asn1::Asn1Time,
                bn::BigNum,
                pkey::PKey,
                rsa::Rsa,
                ssl::{SslAcceptor, SslMethod},
                x509::{
                    X509, X509NameBuilder,
                    extension::{BasicConstraints, SubjectAlternativeName},
                },
            };
            let key = PKey::from_rsa(Rsa::generate(2048).unwrap()).unwrap();
            let mut name = X509NameBuilder::new().unwrap();
            name.append_entry_by_text("CN", "localhost").unwrap();
            let name = name.build();
            let mut cert = X509::builder().unwrap();
            cert.set_version(2).unwrap();
            cert.set_serial_number(&BigNum::from_u32(1).unwrap().to_asn1_integer().unwrap())
                .unwrap();
            cert.set_subject_name(&name).unwrap();
            cert.set_issuer_name(&name).unwrap();
            cert.set_pubkey(&key).unwrap();
            cert.set_not_before(&Asn1Time::days_from_now(0).unwrap())
                .unwrap();
            cert.set_not_after(&Asn1Time::days_from_now(1).unwrap())
                .unwrap();
            cert.append_extension(BasicConstraints::new().critical().ca().build().unwrap())
                .unwrap();
            cert.append_extension(
                SubjectAlternativeName::new()
                    .dns("localhost")
                    .build(&cert.x509v3_context(None, None))
                    .unwrap(),
            )
            .unwrap();
            cert.sign(&key, MessageDigest::sha256()).unwrap();
            let cert = cert.build();
            let mut tls = SslAcceptor::mozilla_intermediate(SslMethod::tls()).unwrap();
            tls.set_certificate(&cert).unwrap();
            tls.set_private_key(&key).unwrap();
            let tls = tls.build();
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            listener.set_nonblocking(true).unwrap();
            let url = format!(
                "https://localhost:{}",
                listener.local_addr().unwrap().port()
            );
            let requests = Arc::new(Mutex::new(Vec::new()));
            let seen = requests.clone();
            let stop = Arc::new(AtomicBool::new(false));
            let stopped = stop.clone();
            let thread = std::thread::spawn(move || {
                let mut workers = Vec::new();
                while !stopped.load(Ordering::Acquire) {
                    let socket = match listener.accept() {
                        Ok((s, _)) => s,
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                            std::thread::sleep(Duration::from_millis(2));
                            continue;
                        }
                        Err(e) => panic!("{e}"),
                    };
                    socket
                        .set_read_timeout(Some(Duration::from_secs(2)))
                        .unwrap();
                    socket
                        .set_write_timeout(Some(Duration::from_secs(2)))
                        .unwrap();
                    let tls = tls.clone();
                    let seen = seen.clone();
                    let stopped = stopped.clone();
                    workers.push(std::thread::spawn(move||{
                        let Ok(mut socket)=tls.accept(socket) else{return;};let mut header=Vec::new();let mut byte=[0];
                        while header.len()<16384 && !header.ends_with(b"\r\n\r\n"){if socket.read(&mut byte).ok()!=Some(1){return;}header.push(byte[0]);}
                        let header=String::from_utf8(header).unwrap();let length=header.lines().find_map(|line|line.to_ascii_lowercase().strip_prefix("content-length:").map(|v|v.trim().parse::<usize>().unwrap())).unwrap();if header.starts_with("PUT /slow-body "){std::thread::sleep(Duration::from_millis(250));}let mut body=vec![0;length];socket.read_exact(&mut body).unwrap();
                        let attempt={let mut requests=seen.lock().unwrap();requests.push((header.clone(),body));requests.len()};
                        if header.starts_with("PUT /reject "){let _=write!(socket,"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");}
                        else if header.starts_with("PUT /slow-body ") || header.starts_with("PUT /recover ") && attempt==2 {let _=write!(socket,"HTTP/1.1 201 Created\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");}
                        else {while !stopped.load(Ordering::Acquire){std::thread::sleep(Duration::from_millis(2));}}
                    }));
                }
                for worker in workers {
                    worker.join().unwrap();
                }
            });
            Self {
                url,
                cert: cert.to_pem().unwrap(),
                requests,
                stop,
                thread: Some(thread),
            }
        }
        fn client(&self) -> reqwest::Client {
            crate::downloads::client_builder()
                .no_proxy()
                .add_root_certificate(reqwest::Certificate::from_pem(&self.cert).unwrap())
                .build()
                .unwrap()
        }
        fn slot(&self, path: &str) -> Slot {
            let mut headers = HeaderMap::new();
            headers.insert(
                "authorization",
                HeaderValue::from_static("Bearer synthetic"),
            );
            headers.insert("cookie", HeaderValue::from_static("fixture=1"));
            Slot {
                put: Url::parse(&format!("{}{path}", self.url)).unwrap(),
                get: Url::parse(&format!("{}/object", self.url)).unwrap(),
                headers,
            }
        }
    }
    impl Drop for Https {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Release);
            self.thread.take().unwrap().join().unwrap();
        }
    }
    #[test]
    fn one_recovery_replays_identical_private_ciphertext_and_preserves_slot_headers() {
        let directory = Directory::new();
        let path = directory.0.join("plain");
        let plain = b"synthetic plaintext only";
        fs::write(&path, plain).unwrap();
        let prepared = Prepared {
            path: path.clone(),
            name: "fixture.bin".into(),
            mime: "application/octet-stream".into(),
            size: plain.len() as u64,
            hash: openssl::sha::sha256(plain).to_vec(),
        };
        let cipher = Crypter::new(
            Cipher::aes_256_gcm(),
            Mode::Encrypt,
            &[7; 32],
            Some(&[3; 12]),
        )
        .unwrap();
        let wire = prepare_wire(&prepared, Some(cipher), Arc::new(Job::default())).unwrap();
        assert_eq!(
            fs::read_dir(&directory.0).unwrap().count(),
            1,
            "wire file must be unlinked before networking"
        );
        let mut expected = Vec::new();
        wire.input
            .reopen()
            .unwrap()
            .0
            .read_to_end(&mut expected)
            .unwrap();
        assert_ne!(expected, plain);
        assert_eq!(expected.len(), plain.len() + 16);
        fs::write(&path, b"mutable plaintext changed after encryption").unwrap();
        assert!(
            prepare_wire(&prepared, None, Arc::new(Job::default())).is_err(),
            "changed snapshot cannot be encrypted or published"
        );
        let server = Https::new();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime
            .block_on(put_wire(
                &server.client(),
                &server.slot("/recover"),
                "application/octet-stream",
                &wire,
                Arc::new(Job::default()),
                Duration::from_millis(80),
            ))
            .unwrap();
        let requests = server.requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert_eq!(
            requests[0].0, requests[1].0,
            "same slot, length and allowed headers"
        );
        assert!(
            requests[0]
                .0
                .to_ascii_lowercase()
                .contains("authorization: bearer synthetic")
        );
        assert!(
            requests[0]
                .0
                .to_ascii_lowercase()
                .contains("cookie: fixture=1")
        );
        assert_eq!(requests[0].1, expected);
        assert_eq!(requests[1].1, expected);
    }
    #[test]
    fn failed_recovery_is_bounded_business_errors_and_cancellation_do_not_replay() {
        let directory = Directory::new();
        let path = directory.0.join("plain");
        fs::write(&path, b"fixture").unwrap();
        let prepared = Prepared {
            path,
            name: "fixture.bin".into(),
            mime: "application/octet-stream".into(),
            size: 7,
            hash: openssl::sha::sha256(b"fixture").to_vec(),
        };
        let wire = prepare_wire(&prepared, None, Arc::new(Job::default())).unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        for (route, count) in [("/blackhole", 2), ("/reject", 1)] {
            let server = Https::new();
            let client = crate::downloads::client_builder()
                .no_proxy()
                .add_root_certificate(reqwest::Certificate::from_pem(&server.cert).unwrap())
                .read_timeout(Duration::from_millis(150))
                .build()
                .unwrap();
            assert!(
                runtime
                    .block_on(put_wire(
                        &client,
                        &server.slot(route),
                        "application/octet-stream",
                        &wire,
                        Arc::new(Job::default()),
                        Duration::from_millis(50)
                    ))
                    .is_err()
            );
            assert_eq!(server.requests.lock().unwrap().len(), count);
        }
        let server = Https::new();
        let job = Arc::new(Job::default());
        let stopped = job.clone();
        let requests = server.requests.clone();
        let cancel = std::thread::spawn(move || {
            while requests.lock().unwrap().is_empty() {
                std::thread::sleep(Duration::from_millis(2));
            }
            stopped.stop();
        });
        assert_eq!(
            runtime.block_on(put_wire(
                &server.client(),
                &server.slot("/blackhole"),
                "application/octet-stream",
                &wire,
                job,
                Duration::from_millis(80)
            )),
            Err("Upload cancelled")
        );
        cancel.join().unwrap();
        assert_eq!(server.requests.lock().unwrap().len(), 1);
    }

    #[test]
    fn acknowledgement_deadline_does_not_limit_a_healthy_slow_body() {
        let directory = Directory::new();
        let path = directory.0.join("large");
        let plain = vec![42; 8 * 1024 * 1024];
        fs::write(&path, &plain).unwrap();
        let prepared = Prepared {
            path,
            name: "large.bin".into(),
            mime: "application/octet-stream".into(),
            size: plain.len() as u64,
            hash: openssl::sha::sha256(&plain).to_vec(),
        };
        let wire = prepare_wire(&prepared, None, Arc::new(Job::default())).unwrap();
        let server = Https::new();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let start = std::time::Instant::now();
        runtime
            .block_on(put_wire(
                &server.client(),
                &server.slot("/slow-body"),
                "application/octet-stream",
                &wire,
                Arc::new(Job::default()),
                Duration::from_millis(80),
            ))
            .unwrap();
        assert!(start.elapsed() >= Duration::from_millis(250));
        assert_eq!(
            server.requests.lock().unwrap().len(),
            1,
            "slow body must not reconnect before consumption"
        );
    }

    #[test]
    fn strict_provider_limits_and_slot_authority() {
        let root = xml(
            "<query xmlns='http://jabber.org/protocol/disco#info'><feature var='urn:xmpp:http:upload:0'/><x xmlns='jabber:x:data' type='result'><field var='FORM_TYPE'><value>urn:xmpp:http:upload:0</value></field><field var='max-file-size'><value>100</value></field></x></query>",
        );
        assert_eq!(
            provider(&root, "upload.example.org")
                .unwrap()
                .unwrap()
                .limit,
            100
        );
        for bad in ["-1", "100x", "18446744073709551616", ""] {
            assert!(
                provider(
                    &xml(&String::from(&root)
                        .replace("<value>100</value>", &format!("<value>{bad}</value>"))),
                    "example.org"
                )
                .is_err()
            );
        }
        let valid = "<slot xmlns='urn:xmpp:http:upload:0'><put url='https://put.example.org/file'><header name='Authorization'>Bearer fixture</header><header name='Cookie'>first=1</header><header name='Cookie'>second=2</header><header name='Host'>evil.example.org</header></put><get url='https://get.example.org/file'/></slot>";
        let parsed = slot(&xml(valid)).unwrap();
        assert_eq!(parsed.put.host_str(), Some("put.example.org"));
        assert_eq!(parsed.headers.get_all("cookie").iter().count(), 2);
        assert!(parsed.headers.get("host").is_none());
        for bad in [
            "http://put.example.org/file",
            "https://user:pass@put.example.org/file",
            "https://put.example.org/file#secret",
            "aesgcm://put.example.org/file#0000",
        ] {
            assert!(slot(&xml(&valid.replace("https://put.example.org/file", bad))).is_err());
        }
        assert!(slot(&xml(&valid.replace("Bearer fixture", "Bearer&#10;fixture"))).is_err());
        assert!(
            slot(&xml(&valid.replace(
                "</slot>",
                "<get url='https://evil.example.org'/></slot>"
            )))
            .is_err()
        );
    }
    #[test]
    fn private_snapshot_survives_source_changes_and_retry() {
        let directory = Directory::new();
        let source = directory.0.join("source.txt");
        fs::write(&source, b"original bytes").unwrap();
        let draft = Draft {
            uri: gio::File::for_path(&source).uri().into(),
            name: "file.txt".into(),
            error: None,
            sticker: false,
        };
        let target = directory.0.join("account/uploads/snapshot.input");
        let prepared = prepare(target.clone(), &draft, 100, &Job::default()).unwrap();
        assert_eq!(prepared.size, 14);
        assert_eq!(prepared.hash, openssl::sha::sha256(b"original bytes"));
        fs::write(&source, b"replaced source").unwrap();
        let retry = prepare(target.clone(), &draft, 100, &Job::default()).unwrap();
        assert_eq!(fs::read(&retry.path).unwrap(), b"original bytes");
        assert_eq!(
            fs::metadata(&target).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert!(prepare(target.clone(), &draft, 13, &Job::default()).is_err());
        let cancelled = Job::default();
        cancelled.stop();
        assert!(prepare(target, &draft, 100, &cancelled).is_err());
        let small = directory.0.join("account/uploads/small.input");
        assert!(prepare(small.clone(), &draft, 2, &Job::default()).is_err());
        assert!(!small.exists());
        assert!(
            fs::read_dir(small.parent().unwrap()).unwrap().all(|e| !e
                .unwrap()
                .path()
                .extension()
                .is_some_and(|e| e == "part"))
        );
        assert_eq!(name("../file\n.txt"), ".._file_.txt");
        assert!(name(&"🦀".repeat(101)).encode_utf16().count() <= 200);
    }
    fn input(path: &Path, plain: &[u8], cipher: Option<Crypter>) -> Input {
        Input {
            file: File::open(path).unwrap(),
            size: plain.len() as u64,
            bytes: 0,
            cipher,
            hash: Hasher::new(MessageDigest::sha256()).unwrap(),
            expected: openssl::sha::sha256(plain).to_vec(),
            done: Arc::new(AtomicBool::new(false)),
            job: Arc::new(Job::default()),
            report_progress: true,
            finished: None,
        }
    }
    #[test]
    fn streamed_cipher_matches_actual_mobile_vectors_and_checks_mutation() {
        let directory = Directory::new();
        let path = directory.0.join("input");
        let vectors: Vec<serde_json::Value> = serde_json::from_str(include_str!(
            "../tests/mobile-reference/file-cipher-vectors.json"
        ))
        .unwrap();
        let mut tested = 0;
        for vector in vectors {
            let decode = |key: &str| STANDARD.decode(vector[key].as_str().unwrap()).unwrap();
            let plain = decode("plain");
            // Mobile rejects empty upload inputs before constructing a cipher.
            if plain.is_empty() {
                continue;
            }
            fs::write(&path, &plain).unwrap();
            let cipher = Crypter::new(
                Cipher::aes_256_gcm(),
                Mode::Encrypt,
                &decode("key"),
                Some(&decode("iv")),
            )
            .unwrap();
            let mut input = input(&path, &plain, Some(cipher));
            let mut actual = vec![];
            while let Some(chunk) = input.next().unwrap() {
                assert!(chunk.len() <= 65568);
                actual.extend(chunk);
            }
            assert!(input.done.load(Ordering::Acquire));
            assert_eq!(actual, decode("ciphertext"));
            tested += 1;
            let mut plain_input = super::tests::input(&path, &plain, None);
            let mut actual = vec![];
            while let Some(chunk) = plain_input.next().unwrap() {
                actual.extend(chunk);
            }
            assert_eq!(actual, plain);
        }
        assert_eq!(tested, 2);
        fs::write(&path, b"changed").unwrap();
        let mut changed = input(&path, b"initial", None);
        assert_eq!(changed.next().unwrap_err(), "Upload snapshot changed");
        assert!(!changed.done.load(Ordering::Acquire));
        fs::write(&path, b"initial-more").unwrap();
        let mut changed = input(&path, b"initial", None);
        assert!(changed.next().is_err());
        fs::write(&path, b"short").unwrap();
        let mut changed = input(&path, b"initial", None);
        assert!(changed.next().is_ok());
        assert!(changed.next().is_err());
        let mut stopped = input(&path, b"short", None);
        stopped.job.stop();
        assert_eq!(stopped.next().unwrap_err(), "Upload cancelled");
    }
}
