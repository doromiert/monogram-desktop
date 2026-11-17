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

/// Runs in the forked child just before the exec. bubblewrap does not close descriptors it
/// inherits, so anything this process holds open without close-on-exec (a socket, a database, a
/// file a library opened carelessly) would be readable inside the sandbox. Every descriptor above
/// stdio is marked close-on-exec so it vanishes at the exec, then `keep` (the one the helper
/// should receive) is switched back. Marking instead of closing leaves Rust's own exec-error pipe
/// working. Only async-signal-safe calls are used.
fn confine_descriptors(keep: Option<RawFd>) -> std::io::Result<()> {
    // Linux 5.11 and later: the whole range in one call.
    const CLOSE_RANGE_CLOEXEC: libc::c_uint = 4;
    let marked = unsafe { libc::syscall(libc::SYS_close_range, 3 as libc::c_uint, libc::c_uint::MAX, CLOSE_RANGE_CLOEXEC) } == 0;
    if !marked {
        // Older kernels: one by one, up to the descriptor limit.
        let mut limit = libc::rlimit { rlim_cur: 0, rlim_max: 0 };
        let top = if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit) } == 0 { limit.rlim_cur.min(65_536) as i32 } else { 1024 };
        for fd in 3..top {
            let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
            if flags >= 0 {
                unsafe { libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) };
            }
        }
    }
    if let Some(fd) = keep {
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
        if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) } < 0 {
            return Err(std::io::Error::last_os_error());
        }
    }
    Ok(())
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
        }
    }
    // Nothing but the input descriptor (when there is one) may reach the sandbox.
    let keep = match input {
        Input::Fd(fd) => Some(fd),
        Input::Path(_) => None,
    };
    unsafe {
        command.pre_exec(move || confine_descriptors(keep));
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

    /// Closing every descriptor in the child would also close Rust's exec-error pipe and make a
    /// failed exec look like success. Marking them close-on-exec keeps the error visible.
    #[test]
    fn a_missing_launcher_is_still_reported_as_a_spawn_error() {
        for input in [Input::Fd(-1), Input::Path(Path::new("/nonexistent"))] {
            let mut command = sandbox_command(Path::new("/nonexistent/bwrap"), Path::new("/opt/h"), input, None, &[], &["probe"]);
            let error = command.spawn().expect_err("a launcher that does not exist must not spawn");
            // The input descriptor may itself be invalid (-1): either way it is an error, not a child.
            assert!(matches!(error.kind(), std::io::ErrorKind::NotFound | std::io::ErrorKind::InvalidInput | std::io::ErrorKind::Other) || error.raw_os_error().is_some(), "{error:?}");
        }
    }

    #[test]
    fn descriptors_above_stdio_are_marked_close_on_exec_except_the_one_kept() {
        // Run the same routine in a forked child so this process is untouched, and report back.
        let plain = unsafe { libc::open(c"/etc/hostname".as_ptr(), libc::O_RDONLY) };
        let kept = unsafe { libc::open(c"/etc/hostname".as_ptr(), libc::O_RDONLY) };
        assert!(plain > 2 && kept > 2);
        let mut fds = [0i32; 2];
        assert_eq!(unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) }, 0);
        match unsafe { libc::fork() } {
            0 => unsafe {
                let ok = confine_descriptors(Some(kept)).is_ok();
                let cloexec = |fd| libc::fcntl(fd, libc::F_GETFD) & libc::FD_CLOEXEC != 0;
                let result = [ok, cloexec(plain), !cloexec(kept)].iter().all(|v| *v);
                libc::write(fds[1], &[u8::from(result)] as *const u8 as *const libc::c_void, 1);
                libc::_exit(0)
            },
            pid if pid > 0 => {
                unsafe { libc::close(fds[1]) };
                let mut answer = [0u8; 1];
                let read = unsafe { libc::read(fds[0], answer.as_mut_ptr() as *mut libc::c_void, 1) };
                let mut status = 0;
                unsafe { libc::waitpid(pid, &mut status, 0) };
                for fd in [plain, kept, fds[0]] {
                    unsafe { libc::close(fd) };
                }
                assert_eq!((read, answer[0]), (1, 1), "a stray descriptor is close-on-exec and the kept one is inheritable");
            }
            _ => panic!("fork failed"),
        }
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
