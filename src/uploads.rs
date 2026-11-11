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
    let mime = gio::content_type_get_mime_type(&content)
        .map(|v| v.to_string())
        .unwrap_or_else(|| "application/octet-stream".into());
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
        *self.job.phase.lock().unwrap() = Phase::Uploading {
            bytes: self.bytes,
            total: self.size,
        };
        Ok(Some(output))
    }
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
    let opaque = mode == crate::encryption_policy::Mode::Omemo2;
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
    let done = Arc::new(AtomicBool::new(false));
    let input = Input {
        file: File::open(&prepared.path).map_err(|_| "Upload snapshot unavailable")?,
        size: prepared.size,
        bytes: 0,
        cipher,
        hash: Hasher::new(MessageDigest::sha256()).map_err(|_| "Hash unavailable")?,
        expected: prepared.hash.clone(),
        done: done.clone(),
        job: job.clone(),
    };
    let stream = futures_util::stream::try_unfold(input, |mut input| async move {
        input
            .next()
            .map(|chunk| chunk.map(|chunk| (chunk, input)))
            .map_err(std::io::Error::other)
    });
    *job.phase.lock().unwrap() = Phase::Uploading {
        bytes: 0,
        total: prepared.size,
    };
    let response = client
        .put(slot.put)
        .headers(slot.headers)
        .header(reqwest::header::CONTENT_TYPE, mime)
        .header(reqwest::header::CONTENT_LENGTH, wire_size)
        .body(reqwest::Body::wrap_stream(stream))
        .send()
        .await
        .map_err(|_| "HTTPS upload failed")?;
    job.check()?;
    if ![200, 201, 204].contains(&response.status().as_u16()) || !done.load(Ordering::Acquire) {
        return Err("Upload was not accepted");
    }
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
