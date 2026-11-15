//! Regenerable sticker thumbnails (still frames and WebM posters) kept under
//! `$XDG_CACHE_HOME/monogram-desktop/pack-previews`. The pack library itself stays in the data
//! directory because it holds packs that cannot be downloaded again; everything here can be
//! rebuilt from it, so deleting this directory only costs a re-decode.
use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicUsize, Ordering},
    time::SystemTime,
};

const MAX_ENTRY: u64 = 2 * 1024 * 1024;
pub(crate) const MAX_TOTAL: u64 = 128 * 1024 * 1024;
const PRUNE_EVERY: usize = 64;

fn default_root() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache")))?;
    Some(base.join("monogram-desktop").join("pack-previews"))
}

/// One directory per account, named by a hash of the account, so removing an account can
/// remove exactly its previews and the directory names reveal nothing.
fn account_dir(base: &Path, account: &str) -> PathBuf {
    let id: String = openssl::sha::sha256(account.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    base.join(id)
}

pub(crate) fn account_root(account: &str) -> Option<PathBuf> {
    default_root().map(|base| account_dir(&base, account))
}

/// Deletes every cached preview of `account`. A missing directory is not an error.
pub(crate) fn forget(account: &str) -> Result<(), &'static str> {
    let Some(base) = default_root() else {
        return Ok(());
    };
    forget_in(&base, account)
}

fn forget_in(base: &Path, account: &str) -> Result<(), &'static str> {
    match fs::remove_dir_all(account_dir(base, account)) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err("Sticker preview cache could not be removed"),
    }
}

/// Opaque file name for one preview. Only the stored artwork name (a random UUID) is hashed.
pub(crate) fn key(kind: &str, edge: u32, artwork: &str) -> String {
    openssl::sha::sha256(format!("{kind}\0{edge}\0{artwork}").as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// A cached preview, unless the artwork file changed after it was written.
pub(crate) fn load(root: &Path, key: &str, source_modified: Option<SystemTime>) -> Option<Vec<u8>> {
    let path = root.join(key);
    let meta = fs::metadata(&path).ok()?;
    if !meta.is_file() || meta.len() == 0 || meta.len() > MAX_ENTRY {
        return None;
    }
    if let (Some(source), Ok(written)) = (source_modified, meta.modified())
        && written < source
    {
        return None;
    }
    fs::read(path).ok()
}

pub(crate) fn store(root: &Path, key: &str, bytes: &[u8]) {
    if bytes.is_empty() || bytes.len() as u64 > MAX_ENTRY {
        return;
    }
    static WRITES: AtomicUsize = AtomicUsize::new(0);
    if fs::create_dir_all(root).is_err() {
        return;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(root, fs::Permissions::from_mode(0o700));
    }
    let temp = root.join(format!(".{key}.{}.tmp", std::process::id()));
    let written = {
        use std::{io::Write, os::unix::fs::OpenOptionsExt};
        fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&temp)
            .and_then(|mut file| file.write_all(bytes))
            .is_ok()
    };
    if !written || fs::rename(&temp, root.join(key)).is_err() {
        let _ = fs::remove_file(&temp);
    }
    if WRITES.fetch_add(1, Ordering::Relaxed) % PRUNE_EVERY == PRUNE_EVERY - 1 {
        prune(root, MAX_TOTAL);
    }
}

/// Deletes the oldest previews until the directory is back under three quarters of `max`.
pub(crate) fn prune(root: &Path, max: u64) {
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    let mut files: Vec<_> = entries
        .flatten()
        .filter_map(|entry| {
            let meta = entry.metadata().ok()?;
            if !meta.is_file() {
                return None;
            }
            Some((meta.modified().ok()?, meta.len(), entry.path()))
        })
        .collect();
    let mut total: u64 = files.iter().map(|(_, len, _)| len).sum();
    if total <= max {
        return;
    }
    files.sort_by_key(|(modified, _, _)| *modified);
    for (_, len, path) in files {
        if total <= max / 4 * 3 {
            break;
        }
        if fs::remove_file(path).is_ok() {
            total -= len;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn temp() -> PathBuf {
        std::env::temp_dir().join(format!("monogram-preview-cache-{}", uuid::Uuid::new_v4()))
    }

    #[test]
    fn keys_are_opaque_stable_and_separate_kinds_and_sizes() {
        let a = key("still", 192, "0f0e0d0c-0000-4000-8000-000000000000.webp");
        assert_eq!(a, key("still", 192, "0f0e0d0c-0000-4000-8000-000000000000.webp"));
        assert_ne!(a, key("poster", 192, "0f0e0d0c-0000-4000-8000-000000000000.webp"));
        assert_ne!(a, key("still", 96, "0f0e0d0c-0000-4000-8000-000000000000.webp"));
        assert_eq!(a.len(), 64);
        assert!(a.bytes().all(|b| b.is_ascii_hexdigit()));
        assert!(!a.contains("webp") && !a.contains("0f0e"));
    }

    #[test]
    fn stored_previews_round_trip_and_stale_ones_are_ignored() {
        let root = temp();
        let name = key("still", 192, "artwork.webp");
        assert!(load(&root, &name, None).is_none());
        store(&root, &name, b"png bytes");
        assert_eq!(load(&root, &name, None).as_deref(), Some(&b"png bytes"[..]));
        let before = SystemTime::now() - Duration::from_secs(3600);
        assert!(load(&root, &name, Some(before)).is_some());
        let after = SystemTime::now() + Duration::from_secs(3600);
        assert!(load(&root, &name, Some(after)).is_none(), "artwork newer than the preview");
        assert!(!fs::read_dir(&root).unwrap().flatten().any(|e| e.file_name().to_string_lossy().ends_with(".tmp")));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(fs::metadata(&root).unwrap().permissions().mode() & 0o777, 0o700);
        }
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn previews_are_private_and_scoped_to_one_account_until_it_is_removed() {
        let base = temp();
        let alice = account_dir(&base, "alice@example.invalid");
        let bob = account_dir(&base, "bob@example.invalid");
        assert_ne!(alice, bob);
        assert!(!alice.to_string_lossy().contains("alice"), "directory names are hashed");
        let name = key("still", 192, "artwork.webp");
        store(&alice, &name, b"alice preview");
        store(&bob, &name, b"bob preview");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(fs::metadata(alice.join(&name)).unwrap().permissions().mode() & 0o777, 0o600);
        }
        forget_in(&base, "alice@example.invalid").unwrap();
        assert!(load(&alice, &name, None).is_none());
        assert_eq!(load(&bob, &name, None).as_deref(), Some(&b"bob preview"[..]), "other accounts are kept");
        forget_in(&base, "alice@example.invalid").unwrap();
        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn empty_and_oversized_previews_are_not_stored_or_read() {
        let root = temp();
        store(&root, "empty", b"");
        store(&root, "huge", &vec![0u8; MAX_ENTRY as usize + 1]);
        assert!(!root.exists() || fs::read_dir(&root).unwrap().next().is_none());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn prune_removes_the_oldest_files_until_under_three_quarters() {
        let root = temp();
        fs::create_dir_all(&root).unwrap();
        for index in 0..10u8 {
            let path = root.join(format!("{index}"));
            fs::write(&path, [index; 100]).unwrap();
            let file = fs::File::options().write(true).open(&path).unwrap();
            file.set_modified(SystemTime::now() - Duration::from_secs(100 - u64::from(index))).unwrap();
        }
        prune(&root, 2000);
        assert_eq!(fs::read_dir(&root).unwrap().count(), 10, "under the limit is left alone");
        prune(&root, 800);
        let mut left: Vec<_> = fs::read_dir(&root)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().parse::<u8>().unwrap())
            .collect();
        left.sort_unstable();
        assert_eq!(left, [4, 5, 6, 7, 8, 9], "newest six kept, 600 bytes <= 3/4 of 800");
        let _ = fs::remove_dir_all(root);
    }
}
