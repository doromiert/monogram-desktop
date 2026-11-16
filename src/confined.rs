//! Launching a sandboxed helper process: bubblewrap with no network, a read-only input, optional
//! one writable directory, and only the GStreamer plugin files that helper needs.
//!
//! Shared by the picture and audio helpers so the sandbox is defined once. The input is either a
//! path or an already-open file descriptor. A descriptor is the stricter form: bubblewrap copies
//! its contents into the sandbox, so the bytes the helper sees are the bytes that were opened and
//! checked, and nothing can swap the file for another in between.
use std::{
    os::{
        fd::RawFd,
        unix::process::CommandExt,
    },
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

/// A helper binary: from `variable` if set, else beside this executable (or one level up, which is
/// where test binaries run from). Must be an absolute path to a file.
pub fn helper(name: &str, variable: &str) -> Option<PathBuf> {
    let beside = std::env::current_exe().ok().and_then(|exe| {
        let dir = exe.parent()?;
        [dir.join(name), dir.parent()?.join(name)].into_iter().find(|path| path.is_file())
    });
    program(variable, beside)
}
pub fn program(variable: &str, fallback: Option<PathBuf>) -> Option<PathBuf> {
    let path = std::env::var_os(variable).map(PathBuf::from).or(fallback)?;
    (path.is_absolute() && path.is_file()).then_some(path)
}
pub fn bwrap() -> Option<PathBuf> {
    program("MONOGRAM_BWRAP", Some(env!("MONOGRAM_BUILT_BWRAP").into()))
}

/// Directories GStreamer plugins live in: an explicit search path first (Nix splits plugins over
/// several store paths), then the usual distribution locations and the AppImage.
pub fn plugin_dirs() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = ["GST_PLUGIN_SYSTEM_PATH_1_0", "GST_PLUGIN_SYSTEM_PATH"]
        .iter()
        .filter_map(std::env::var_os)
        .flat_map(|value| std::env::split_paths(&value).collect::<Vec<_>>())
        .collect();
    if let Some(appdir) = std::env::var_os("APPDIR") {
        dirs.push(Path::new(&appdir).join("lib/gstreamer-1.0"));
    }
    for dir in [
        "/usr/lib/gstreamer-1.0",
        "/usr/lib64/gstreamer-1.0",
        "/usr/lib/x86_64-linux-gnu/gstreamer-1.0",
        "/usr/lib/aarch64-linux-gnu/gstreamer-1.0",
    ] {
        dirs.push(dir.into());
    }
    dirs
}
/// The first file found for each of `names`. Missing ones are skipped: the helper then fails to
/// build its chain and the feature is reported unavailable.
pub fn plugin_files(names: &[&str], dirs: &[PathBuf]) -> Vec<PathBuf> {
    names
        .iter()
        .filter_map(|name| dirs.iter().map(|dir| dir.join(name)).find(|path| path.is_file()))
        .collect()
}

/// What the helper reads, mounted at `/in/source`.
pub enum Input<'a> {
    Path(&'a Path),
    /// An open descriptor positioned at the start; it is kept open in the child for bubblewrap.
    Fd(RawFd),
}

/// The bubblewrap command line. Separate from spawning so tests can inspect exactly what is
/// exposed to the helper. `args` follow the helper's path.
pub fn sandbox_command(
    bwrap: &Path,
    helper: &Path,
    input: Input,
    out_dir: Option<&Path>,
    plugins: &[PathBuf],
    args: &[&str],
) -> Command {
    let mut command = Command::new(bwrap);
    crate::renderer_sandbox::runtime(&mut command);
    command.env_clear().args([
        "--unshare-all",
        "--die-with-parent",
        "--new-session",
        "--cap-drop",
        "ALL",
        "--clearenv",
        "--proc",
        "/proc",
        "--dev",
        "/dev",
        "--tmpfs",
        "/tmp",
        "--dir",
        "/in",
        "--chdir",
        "/tmp",
    ]);
    match input {
        Input::Path(path) => {
            command.arg("--ro-bind").arg(path).arg("/in/source");
        }
        Input::Fd(fd) => {
            command.arg("--ro-bind-data").arg(fd.to_string()).arg("/in/source");
            // Rust opens descriptors close-on-exec; bubblewrap needs this one to survive the exec.
            unsafe {
                command.pre_exec(move || {
                    let flags = libc::fcntl(fd, libc::F_GETFD);
                    if flags < 0 || libc::fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) < 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
        }
    }
    if let Some(out_dir) = out_dir {
        command.arg("--bind").arg(out_dir).arg("/out");
    }
    for plugin in plugins {
        if let Some(name) = plugin.file_name() {
            command.arg("--ro-bind").arg(plugin).arg(Path::new("/plugins").join(name));
        }
    }
    for (key, value) in [
        ("HOME", "/nonexistent"),
        ("XDG_CACHE_HOME", "/tmp"),
        ("GST_REGISTRY", "/tmp/gst-registry.bin"),
        ("GST_REGISTRY_FORK", "no"),
        ("GST_PLUGIN_SCANNER", "/nonexistent"),
        ("GST_PLUGIN_SYSTEM_PATH_1_0", "/plugins"),
        ("GST_PLUGIN_PATH_1_0", ""),
    ] {
        command.args(["--setenv", key, value]);
    }
    command
        .arg("--ro-bind")
        .arg(helper)
        .arg("/author")
        .arg("/author")
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    command
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(command: &Command) -> Vec<String> {
        command.get_args().map(|arg| arg.to_string_lossy().into_owned()).collect()
    }

    #[test]
    fn a_descriptor_input_is_copied_in_and_a_path_input_is_bound_read_only() {
        let by_fd = sandbox_command(Path::new("/usr/bin/bwrap"), Path::new("/opt/h"), Input::Fd(9), None, &[], &["probe", "ogg"]);
        let a = args(&by_fd);
        let has = |needle: &[&str]| a.windows(needle.len()).any(|w| w == needle);
        assert!(has(&["--ro-bind-data", "9", "/in/source"]));
        assert!(!a.iter().any(|x| x == "--bind"), "no writable directory unless asked for");
        assert!(a.iter().any(|x| x == "--unshare-all") && !a.iter().any(|x| x == "--share-net"));
        assert_eq!(&a[a.len() - 2..], ["probe", "ogg"], "helper arguments come last");
        let by_path = sandbox_command(Path::new("/usr/bin/bwrap"), Path::new("/opt/h"), Input::Path(Path::new("/home/me/a.png")), Some(Path::new("/tmp/out")), &[], &["run"]);
        let b = args(&by_path);
        assert!(b.windows(3).any(|w| w == ["--ro-bind", "/home/me/a.png", "/in/source"]));
        assert!(b.windows(3).any(|w| w == ["--bind", "/tmp/out", "/out"]));
    }

    #[test]
    fn plugin_files_are_found_by_name_and_only_the_named_ones() {
        let root = std::env::temp_dir().join(format!("confined-test-{}", uuid::Uuid::new_v4()));
        let (a, b) = (root.join("a"), root.join("b"));
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        std::fs::write(a.join("libgstogg.so"), b"x").unwrap();
        std::fs::write(b.join("libgstopus.so"), b"x").unwrap();
        std::fs::write(b.join("libgstevil.so"), b"x").unwrap();
        let found = plugin_files(&["libgstogg.so", "libgstopus.so", "libgstmissing.so"], &[a.clone(), b.clone()]);
        std::fs::remove_dir_all(&root).unwrap();
        assert_eq!(found, [a.join("libgstogg.so"), b.join("libgstopus.so")]);
    }
}
