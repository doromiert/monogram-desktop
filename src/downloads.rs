//! Account-owned downloads. Each retry owns its own progress and partial file.
use crate::file_share::{Attachment, MAX};
use blake2::{
    Blake2b,
    digest::{Digest, consts::U32},
};
use openssl::{
    hash::{Hasher, MessageDigest},
    symm::{Cipher, Crypter, Mode},
};
use std::{
    collections::HashMap,
    fs::{self, File, OpenOptions},
    io::{Read, Seek, Write},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    Running {
        bytes: u64,
        total: Option<u64>,
    },
    Ready(PathBuf),
    Failed(&'static str),
    Deferred {
        reason: &'static str,
        total: Option<u64>,
    },
    Cancelled,
}
const AUTO_MAX: u64 = 10 * 1024 * 1024;
const AUTO_SIZE: &str = "Media exceeds the automatic download limit or its size is unknown";
const AUTO_TYPE: &str = "The server did not identify this as media";

#[derive(Clone, Copy, PartialEq, Eq)]
enum Policy {
    Cache,
    Manual,
    Automatic,
}
struct Task {
    policy: Policy,
    state: Arc<Mutex<Status>>,
    cancel: Arc<AtomicBool>,
    signal: Option<tokio::sync::oneshot::Sender<()>>,
}
impl Drop for Task {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Release);
        if let Some(signal) = self.signal.take() {
            let _ = signal.send(());
        }
    }
}
#[derive(Default)]
pub struct Downloads {
    tasks: HashMap<(String, String), Task>,
}
// Account removal must fence cache creation/commit; network and hashing stay outside this lock.
pub(crate) static CACHE_IO: Mutex<()> = Mutex::new(());
impl Downloads {
    pub fn token(&self, account: &str, file: &Attachment) -> Option<Arc<AtomicBool>> {
        self.tasks
            .get(&(account.into(), file.key()))
            .map(|task| task.cancel.clone())
    }
    pub fn invalidate(&mut self, account: &str, file: &Attachment) {
        self.tasks.remove(&(account.into(), file.key()));
    }
    pub fn status(&self, account: &str, file: &Attachment) -> Option<Status> {
        self.tasks
            .get(&(account.into(), file.key()))
            .and_then(|task| {
                let status = task.state.lock().unwrap().clone();
                if task.policy == Policy::Cache && matches!(status, Status::Failed(_)) {
                    None
                } else {
                    Some(status)
                }
            })
    }
    pub fn cancel(&mut self, account: &str, file: &Attachment) {
        if let Some(task) = self.tasks.get_mut(&(account.into(), file.key())) {
            task.cancel.store(true, Ordering::Release);
            if let Some(signal) = task.signal.take() {
                let _ = signal.send(());
            }
            *task.state.lock().unwrap() = Status::Cancelled;
        }
    }
    pub fn clear(&mut self) {
        self.tasks.clear();
    }
    pub fn start(&mut self, account: &str, file: Attachment) -> Result<(), &'static str> {
        self.start_with_policy(account, file, Policy::Manual)
    }
    pub fn restore_cached(&mut self, account: &str, file: Attachment) -> Result<(), &'static str> {
        self.start_with_policy(account, file, Policy::Cache)
    }
    pub fn start_automatic(&mut self, account: &str, file: Attachment) {
        if !file.manual && file.media_candidate() && self.status(account, &file).is_none() {
            let _ = self.start_with_policy(account, file, Policy::Automatic);
        }
    }
    fn start_with_policy(
        &mut self,
        account: &str,
        file: Attachment,
        policy: Policy,
    ) -> Result<(), &'static str> {
        if account.is_empty() || file.link().is_none() {
            return Err("Invalid attachment");
        }
        let key = (account.to_string(), file.key());
        if policy == Policy::Cache && self.tasks.contains_key(&key) {
            return Ok(());
        }
        if self
            .tasks
            .get(&key)
            .is_some_and(|t| matches!(*t.state.lock().unwrap(), Status::Running { .. }))
        {
            return Ok(());
        }
        if self
            .tasks
            .values()
            .filter(|t| matches!(*t.state.lock().unwrap(), Status::Running { .. }))
            .count()
            >= 4
        {
            return Err("Wait for another download to finish");
        }
        let root = account_root(account)?;
        let state = Arc::new(Mutex::new(Status::Running {
            bytes: 0,
            total: file.size,
        }));
        let cancel = Arc::new(AtomicBool::new(false));
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.tasks.insert(
            key,
            Task {
                policy,
                state: state.clone(),
                cancel: cancel.clone(),
                signal: Some(tx),
            },
        );
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build();
            let result = match runtime {
                Ok(rt) => rt.block_on(async {
                    tokio::select! {biased; _=rx=>Err("Cancelled"),result=async {
                        if policy == Policy::Cache {
                            let path = root.join(file.key());
                            verify(&path, &file, &cancel)?;
                            Ok(path)
                        } else {
                            let client = client()?;
                            download(&client, &file, &root, &cancel, &state, policy).await
                        }
                    }=>result}
                }),
                Err(_) => Err("Download runtime unavailable"),
            };
            let mut current = state.lock().unwrap();
            if cancel.load(Ordering::Acquire) {
                *current = Status::Cancelled;
            } else {
                *current = match result {
                    Ok(path) => Status::Ready(path),
                    Err(reason @ (AUTO_SIZE | AUTO_TYPE)) => Status::Deferred {
                        reason,
                        total: match *current {
                            Status::Running { total, .. } => total,
                            _ => None,
                        },
                    },
                    Err(reason) => Status::Failed(reason),
                };
            }
        });
        Ok(())
    }
}
pub fn account_root(account: &str) -> Result<PathBuf, &'static str> {
    let root = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|v| PathBuf::from(v).join(".cache")))
        .ok_or("Attachment cache unavailable")?;
    let id: String = openssl::sha::sha256(account.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    Ok(root.join("monogram-desktop").join("attachments").join(id))
}
pub fn forget(account: &str) -> Result<(), &'static str> {
    let _guard = CACHE_IO.lock().unwrap();
    let root = account_root(account)?;
    if root.exists() {
        fs::remove_dir_all(root).map_err(|_| "Attachment cache could not be removed")?;
    }
    Ok(())
}
fn client() -> Result<reqwest::Client, &'static str> {
    client_builder().build().map_err(|_| "HTTPS unavailable")
}
pub(crate) fn client_builder() -> reqwest::ClientBuilder {
    reqwest::Client::builder()
        .https_only(true)
        .redirect(reqwest::redirect::Policy::none())
        .referer(false)
        .no_gzip()
        .no_brotli()
        .no_deflate()
        .no_zstd()
        .connect_timeout(Duration::from_secs(15))
        .read_timeout(Duration::from_secs(30))
}
fn transfer_error(error: &reqwest::Error, body: bool) -> &'static str {
    // Match mobile's safe exception categories; private URLs and keys stay out of logs.
    eprintln!(
        "Monogram attachment HTTPS failure: body={body} timeout={} connect={} request={}",
        error.is_timeout(),
        error.is_connect(),
        error.is_request()
    );
    if error.is_timeout() {
        if body {
            "Download stalled: server timed out"
        } else {
            "Download server timed out before responding"
        }
    } else if error.is_connect() {
        "HTTPS connection failed"
    } else if body {
        "Download interrupted"
    } else {
        "HTTPS download failed"
    }
}
struct Partial(PathBuf);
impl Drop for Partial {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}
fn checked(cancel: &AtomicBool) -> Result<(), &'static str> {
    if cancel.load(Ordering::Acquire) {
        Err("Cancelled")
    } else {
        Ok(())
    }
}
pub fn verify(path: &Path, file: &Attachment, cancel: &AtomicBool) -> Result<(), &'static str> {
    verified_input(path, file, cancel).map(|_| ())
}
pub struct VerifiedFile(pub(crate) File);
impl VerifiedFile {
    pub fn reopen(&self) -> Result<Self, &'static str> {
        use std::os::fd::AsRawFd;
        File::open(format!("/proc/self/fd/{}", self.0.as_raw_fd()))
            .map(Self)
            .map_err(|_| "Verified artwork is unavailable")
    }
}
pub fn verified_input(
    path: &Path,
    file: &Attachment,
    cancel: &AtomicBool,
) -> Result<VerifiedFile, &'static str> {
    checked(cancel)?;
    let mut input = File::open(path).map_err(|_| "Attachment cache unavailable")?;
    let metadata = input
        .metadata()
        .map_err(|_| "Attachment cache unavailable")?;
    if !metadata.is_file()
        || metadata.len() > MAX
        || file.size.is_some_and(|size| size != metadata.len())
    {
        return Err("Attachment integrity check failed");
    }
    let mut sha = Hasher::new(MessageDigest::sha256()).map_err(|_| "Hash unavailable")?;
    let mut sha3 = Hasher::new(MessageDigest::sha3_256()).map_err(|_| "Hash unavailable")?;
    let mut blake = Blake2b::<U32>::new();
    let mut buffer = [0; 65536];
    loop {
        checked(cancel)?;
        let size = input
            .read(&mut buffer)
            .map_err(|_| "Attachment cache unavailable")?;
        if size == 0 {
            break;
        }
        sha.update(&buffer[..size])
            .map_err(|_| "Hash unavailable")?;
        sha3.update(&buffer[..size])
            .map_err(|_| "Hash unavailable")?;
        blake.update(&buffer[..size]);
    }
    let sha = sha.finish().map_err(|_| "Hash unavailable")?;
    let sha3 = sha3.finish().map_err(|_| "Hash unavailable")?;
    let blake = blake.finalize();
    for (name, expected) in &file.hashes {
        let actual: &[u8] = match name.as_str() {
            "sha-256" => &sha,
            "sha3-256" => &sha3,
            "id-blake2b256" => blake.as_slice(),
            _ => return Err("Unsupported attachment hash"),
        };
        if expected.len() != 32 || !openssl::memcmp::eq(actual, expected) {
            return Err("Attachment integrity check failed");
        }
    }
    checked(cancel)?;
    input.rewind().map_err(|_| "Attachment cache unavailable")?;
    Ok(VerifiedFile(input))
}
async fn download(
    client: &reqwest::Client,
    file: &Attachment,
    root: &Path,
    cancel: &AtomicBool,
    state: &Mutex<Status>,
    policy: Policy,
) -> Result<PathBuf, &'static str> {
    checked(cancel)?;
    if file.size.is_some_and(|size| size > MAX) {
        return Err("Attachment exceeds size limit");
    }
    let destination = root.join(file.key());
    if destination.exists() && verify(&destination, file, cancel).is_ok() {
        return Ok(destination);
    }
    let link = file.link().ok_or("Invalid attachment")?;
    let mut url = link.url.clone();
    let mut response = None;
    for _ in 0..=5 {
        checked(cancel)?;
        let reply = client
            .get(url.clone())
            .header(reqwest::header::ACCEPT_ENCODING, "identity")
            .send()
            .await
            .map_err(|error| transfer_error(&error, false))?;
        if [301, 302, 303, 307, 308].contains(&reply.status().as_u16()) {
            let target = reply
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|h| h.to_str().ok())
                .ok_or("Invalid download redirect")?;
            let next = url.join(target).map_err(|_| "Invalid download redirect")?;
            url = crate::file_share::Link::parse(next.as_str())
                .ok_or("Unsafe download redirect")?
                .url
                .clone();
        } else {
            response = Some(reply);
            break;
        }
    }
    let mut response = response.ok_or("Too many download redirects")?;
    if response.status() != reqwest::StatusCode::OK {
        return Err("Attachment server refused download");
    }
    if response
        .headers()
        .get(reqwest::header::CONTENT_ENCODING)
        .is_some_and(|v| v.as_bytes() != b"identity")
    {
        return Err("Unsupported download encoding");
    }
    let expected = response
        .headers()
        .get(reqwest::header::CONTENT_LENGTH)
        .map(|v| {
            v.to_str()
                .ok()
                .filter(|v| !v.is_empty() && v.bytes().all(|c| c.is_ascii_digit()))
                .and_then(|v| v.parse::<u64>().ok())
                .ok_or("Invalid download size")
        })
        .transpose()?;
    let overhead = if link.encrypted() { 16 } else { 0 };
    if expected.is_some_and(|size| size > MAX + overhead || size < overhead) {
        return Err("Invalid download size");
    }
    let total = expected.map(|size| size - overhead).or(file.size);
    *state.lock().unwrap() = Status::Running { bytes: 0, total };
    let limit = if policy == Policy::Automatic {
        let mime = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(|value| {
                value
                    .split(';')
                    .next()
                    .unwrap_or_default()
                    .trim()
                    .to_ascii_lowercase()
            });
        if !mime.as_deref().is_some_and(|mime| {
            mime.starts_with("image/")
                || mime.starts_with("video/")
                || mime == "application/octet-stream"
                    && (link.encrypted() || file.image_extension() || file.video_extension())
        }) {
            return Err(AUTO_TYPE);
        }
        if total.is_none_or(|total| total > AUTO_MAX)
            || file.size.is_some_and(|size| size > AUTO_MAX)
        {
            return Err(AUTO_SIZE);
        }
        AUTO_MAX
    } else {
        MAX
    };
    if let (Some(expected), Some(size)) = (expected, file.size)
        && expected - overhead != size
    {
        return Err("Download size mismatch");
    }
    let partial = Partial(root.join(format!("{}.part", crate::model::new_id())));
    let mut output = {
        let _guard = CACHE_IO.lock().unwrap();
        checked(cancel)?;
        fs::create_dir_all(root).map_err(|_| "Attachment cache unavailable")?;
        fs::set_permissions(root, fs::Permissions::from_mode(0o700))
            .map_err(|_| "Attachment cache unavailable")?;
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&partial.0)
            .map_err(|_| "Attachment cache unavailable")?
    };
    let mut cipher = if link.encrypted() {
        Some(
            Crypter::new(
                Cipher::aes_256_gcm(),
                Mode::Decrypt,
                &link.key,
                Some(&link.iv),
            )
            .map_err(|_| "File decryption unavailable")?,
        )
    } else {
        None
    };
    let mut count = 0;
    let mut tail = Vec::new();
    let mut last = Instant::now();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| transfer_error(&error, true))?
    {
        checked(cancel)?;
        count += chunk.len() as u64;
        if count > limit + overhead {
            return Err("Attachment exceeds size limit");
        }
        for chunk in chunk.chunks(32768) {
            if let Some(cipher) = cipher.as_mut() {
                tail.extend_from_slice(chunk);
                if tail.len() <= 16 {
                    continue;
                }
                let length = tail.len() - 16;
                let mut plain = vec![0; length + 16];
                let written = cipher
                    .update(&tail[..length], &mut plain)
                    .map_err(|_| "File authentication failed")?;
                output
                    .write_all(&plain[..written])
                    .map_err(|_| "Attachment cache unavailable")?;
                use zeroize::Zeroize;
                plain.zeroize();
                tail.drain(..length);
            } else {
                output
                    .write_all(chunk)
                    .map_err(|_| "Attachment cache unavailable")?;
            }
        }
        if last.elapsed() > Duration::from_millis(80) {
            *state.lock().unwrap() = Status::Running {
                bytes: count.saturating_sub(overhead),
                total,
            };
            last = Instant::now();
        }
    }
    checked(cancel)?;
    if expected.is_some_and(|size| size != count) {
        return Err("Download size mismatch");
    }
    if let Some(cipher) = cipher.as_mut() {
        if tail.len() != 16 {
            return Err("File authentication failed");
        }
        cipher
            .set_tag(&tail)
            .map_err(|_| "File authentication failed")?;
        let mut final_bytes = [0; 16];
        let written = cipher
            .finalize(&mut final_bytes)
            .map_err(|_| "File authentication failed")?;
        output
            .write_all(&final_bytes[..written])
            .map_err(|_| "Attachment cache unavailable")?;
    }
    output
        .sync_all()
        .map_err(|_| "Attachment cache unavailable")?;
    drop(output);
    verify(&partial.0, file, cancel)?;
    {
        let _guard = CACHE_IO.lock().unwrap();
        checked(cancel)?;
        fs::rename(&partial.0, &destination).map_err(|_| "Cannot commit attachment")?;
    }
    Ok(destination)
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::{Engine as _, engine::general_purpose::STANDARD};
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
    use std::{net::TcpListener, thread};
    struct Server {
        url: String,
        cert: Vec<u8>,
        stop: Arc<AtomicBool>,
        requests: Arc<Mutex<Vec<String>>>,
        thread: Option<thread::JoinHandle<()>>,
    }
    impl Server {
        fn new(bytes: Vec<u8>) -> Self {
            let key = PKey::from_rsa(Rsa::generate(2048).unwrap()).unwrap();
            let mut name = X509NameBuilder::new().unwrap();
            name.append_entry_by_text("CN", "localhost").unwrap();
            let name = name.build();
            let mut cert = X509::builder().unwrap();
            cert.set_version(2).unwrap();
            let serial = BigNum::from_u32(1).unwrap().to_asn1_integer().unwrap();
            cert.set_serial_number(&serial).unwrap();
            cert.set_subject_name(&name).unwrap();
            cert.set_issuer_name(&name).unwrap();
            cert.set_pubkey(&key).unwrap();
            cert.set_not_before(&Asn1Time::days_from_now(0).unwrap())
                .unwrap();
            cert.set_not_after(&Asn1Time::days_from_now(1).unwrap())
                .unwrap();
            cert.append_extension(BasicConstraints::new().critical().ca().build().unwrap())
                .unwrap();
            let san = SubjectAlternativeName::new()
                .dns("localhost")
                .ip("127.0.0.1")
                .build(&cert.x509v3_context(None, None))
                .unwrap();
            cert.append_extension(san).unwrap();
            cert.sign(&key, MessageDigest::sha256()).unwrap();
            let cert = cert.build();
            let mut acceptor = SslAcceptor::mozilla_intermediate(SslMethod::tls()).unwrap();
            acceptor.set_certificate(&cert).unwrap();
            acceptor.set_private_key(&key).unwrap();
            let acceptor = acceptor.build();
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let url = format!(
                "https://localhost:{}",
                listener.local_addr().unwrap().port()
            );
            listener.set_nonblocking(true).unwrap();
            let stop = Arc::new(AtomicBool::new(false));
            let stopped = stop.clone();
            let requests = Arc::new(Mutex::new(Vec::new()));
            let seen = requests.clone();
            let base = url.clone();
            let thread = thread::spawn(move || {
                let mut workers = Vec::new();
                while !stopped.load(Ordering::Acquire) {
                    let socket = match listener.accept() {
                        Ok((s, _)) => s,
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(5));
                            continue;
                        }
                        Err(e) => panic!("{e}"),
                    };
                    socket
                        .set_read_timeout(Some(Duration::from_secs(3)))
                        .unwrap();
                    socket
                        .set_write_timeout(Some(Duration::from_secs(3)))
                        .unwrap();
                    let acceptor = acceptor.clone();
                    let bytes = bytes.clone();
                    let seen = seen.clone();
                    let stopped = stopped.clone();
                    let base = base.clone();
                    workers.push(thread::spawn(move||{
                        let Ok(mut socket)=acceptor.accept(socket)else{return;};let mut header=Vec::new();let mut byte=[0];
                        while header.len()<16384 && !header.ends_with(b"\r\n\r\n"){if socket.read(&mut byte).ok()!=Some(1){return;}header.push(byte[0]);}
                        let header=String::from_utf8(header).unwrap();assert!(!header.contains('#'));assert!(!header.contains("aesgcm"));
                        let path=header.split_whitespace().nth(1).unwrap().to_string();seen.lock().unwrap().push(path.clone());
                        if path=="/redirect"||path=="/downgrade" {let location=if path=="/redirect"{format!("{base}/file")}else{base.replace("https:","http:")+"/file"};let _=write!(socket,"HTTP/1.1 302 Found\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");return;}
                        if path=="/headers-slow" {while !stopped.load(Ordering::Acquire){thread::sleep(Duration::from_millis(5));}return;}
                        let mut body=bytes;if path=="/bad"{if let Some(b)=body.last_mut(){*b^=1;}}
                        let length=if path=="/huge"{MAX+17}else{body.len() as u64+if path=="/short"{1}else{0}};
                        let encoding=if path=="/encoding"{"Content-Encoding: gzip\r\n"}else{""};
                        let _=write!(socket,"HTTP/1.1 200 OK\r\nContent-Length: {length}\r\n{encoding}Connection: close\r\n\r\n");
                        if path=="/slow"{let _=socket.write_all(&body[..1]);while !stopped.load(Ordering::Acquire){thread::sleep(Duration::from_millis(5));}}
                        else {for part in body.chunks(7){if socket.write_all(part).is_err(){break;}}}
                    }));
                }
                for worker in workers {
                    worker.join().unwrap();
                }
            });
            Self {
                url,
                cert: cert.to_pem().unwrap(),
                stop,
                requests,
                thread: Some(thread),
            }
        }
        fn client(&self) -> reqwest::Client {
            client_builder()
                .no_proxy()
                .add_root_certificate(reqwest::Certificate::from_pem(&self.cert).unwrap())
                .build()
                .unwrap()
        }
    }
    impl Drop for Server {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Release);
            self.thread.take().unwrap().join().unwrap();
        }
    }
    #[test]
    fn stalled_headers_report_timeout_without_committing_a_file() {
        let server = Server::new(b"untrusted".to_vec());
        let client = client_builder()
            .no_proxy()
            .add_root_certificate(reqwest::Certificate::from_pem(&server.cert).unwrap())
            .read_timeout(Duration::from_millis(200))
            .build()
            .unwrap();
        let file = Attachment::bare(&format!("{}/headers-slow", server.url)).unwrap();
        let root = std::env::temp_dir().join(crate::model::new_id());
        let progress = Mutex::new(Status::Running {
            bytes: 0,
            total: None,
        });
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        assert_eq!(
            runtime.block_on(download(
                &client,
                &file,
                &root,
                &AtomicBool::new(false),
                &progress,
                Policy::Manual
            )),
            Err("Download server timed out before responding")
        );
        assert!(
            !root.exists(),
            "a request with no headers must not commit media"
        );
    }

    #[test]
    fn mobile_stream_cipher_and_hashes_commit_only_verified_https_bytes() {
        let cases: Vec<serde_json::Value> = serde_json::from_str(include_str!(
            "../tests/mobile-reference/file-cipher-vectors.json"
        ))
        .unwrap();
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        for value in cases {
            let bytes = |key: &str| STANDARD.decode(value[key].as_str().unwrap()).unwrap();
            let plain = bytes("plain");
            let encrypted = bytes("ciphertext");
            let mut material = bytes("iv");
            material.extend(bytes("key"));
            let secret: String = material.iter().map(|b| format!("{b:02x}")).collect();
            let server = Server::new(encrypted);
            let client = server.client();
            let mut file = Attachment::bare(&format!(
                "aesgcm{}#{}",
                &format!("{}/file", server.url)[5..],
                secret
            ))
            .unwrap();
            file.size = Some(plain.len() as u64);
            file.hashes = value["hashes"]
                .as_object()
                .unwrap()
                .iter()
                .map(|(k, v)| (k.clone(), STANDARD.decode(v.as_str().unwrap()).unwrap()))
                .collect();
            let root = std::env::temp_dir().join(crate::model::new_id());
            let cancel = AtomicBool::new(false);
            let progress = Mutex::new(Status::Running {
                bytes: 0,
                total: None,
            });
            let path = rt
                .block_on(download(
                    &client,
                    &file,
                    &root,
                    &cancel,
                    &progress,
                    Policy::Manual,
                ))
                .unwrap();
            assert_eq!(fs::read(&path).unwrap(), plain);
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
            let requests = server.requests.lock().unwrap().len();
            rt.block_on(download(
                &client,
                &file,
                &root,
                &cancel,
                &progress,
                Policy::Manual,
            ))
            .unwrap();
            assert_eq!(server.requests.lock().unwrap().len(), requests);
            fs::write(&path, b"corrupted cache").unwrap();
            rt.block_on(download(
                &client,
                &file,
                &root,
                &cancel,
                &progress,
                Policy::Manual,
            ))
            .unwrap();
            assert_eq!(fs::read(&path).unwrap(), plain);
            for route in [
                "bad",
                "short",
                "huge",
                "encoding",
                "downgrade",
                "hash",
                "size",
            ] {
                let mut wrong = file.clone();
                if route == "hash" {
                    wrong.hashes.insert("sha-256".into(), vec![255; 32]);
                }
                if route == "size" {
                    wrong.size = Some(plain.len() as u64 + 1);
                }
                wrong.wire_url = wrong
                    .wire_url
                    .replace("/file?", "/unused?")
                    .replace("/file#", &format!("/{route}#"));
                let result = rt.block_on(download(
                    &client,
                    &wrong,
                    &root,
                    &cancel,
                    &progress,
                    Policy::Manual,
                ));
                assert!(result.is_err(), "route {route}");
                assert!(!root.join(wrong.key()).exists());
                assert!(
                    fs::read_dir(&root).unwrap().all(|e| !e
                        .unwrap()
                        .path()
                        .extension()
                        .is_some_and(|s| s == "part"))
                );
            }
            let mut redirect = file.clone();
            redirect.wire_url = redirect.wire_url.replace("/file#", "/redirect#");
            let result = rt
                .block_on(download(
                    &client,
                    &redirect,
                    &root,
                    &cancel,
                    &progress,
                    Policy::Manual,
                ))
                .unwrap();
            assert_eq!(fs::read(result).unwrap(), plain);
            let mut slow = file.clone();
            slow.wire_url = slow.wire_url.replace("/file#", "/slow#");
            let timeout = rt.block_on(async {
                tokio::time::timeout(
                    Duration::from_millis(100),
                    download(&client, &slow, &root, &cancel, &progress, Policy::Manual),
                )
                .await
            });
            assert!(timeout.is_err());
            assert!(!root.join(slow.key()).exists());
            assert!(
                fs::read_dir(&root).unwrap().all(|e| !e
                    .unwrap()
                    .path()
                    .extension()
                    .is_some_and(|s| s == "part"))
            );
            let mut untrusted = file.clone();
            untrusted.wire_url = untrusted.wire_url.replace("/file#", "/untrusted#");
            assert!(
                rt.block_on(download(
                    &super::client().unwrap(),
                    &untrusted,
                    &root,
                    &cancel,
                    &progress,
                    Policy::Manual,
                ))
                .is_err()
            );
            fs::remove_dir_all(root).unwrap();
        }
    }
}
