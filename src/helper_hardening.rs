// SPDX-License-Identifier: GPL-3.0-or-later
//! Hardening shared by the sandboxed helper binaries (`#[path]`-included, so there is one copy).
//!
//! Inside bubblewrap a helper is already in empty namespaces with no capabilities. This adds what
//! the process can do to itself: resource limits, `no_new_privs`, and a seccomp deny-list. It is
//! applied before anything untrusted is read. `selftest` proves the filter by trying the calls it
//! must refuse.
#![allow(dead_code)]

/// Set a hard and soft resource limit.
pub fn limit(resource: libc::__rlimit_resource_t, value: libc::rlim_t) -> bool {
    let v = libc::rlimit { rlim_cur: value, rlim_max: value };
    unsafe { libc::setrlimit(resource, &v) == 0 }
}
fn insn(code: u32, jt: u8, jf: u8, k: u32) -> libc::sock_filter {
    libc::sock_filter { code: code as u16, jt, jf, k }
}
/// Deny-list filter. GStreamer needs threads, so unlike the sticker renderer `clone` stays
/// allowed; processes, sockets, tracing and namespace changes are not.
pub fn restrict_calls() -> bool {
    const BPF_LD_W_ABS: u32 = 0x20;
    const BPF_JEQ_K: u32 = 0x15;
    const BPF_JGE_K: u32 = 0x35;
    const BPF_RET_K: u32 = 0x06;
    #[cfg(target_arch = "x86_64")]
    const ARCH: u32 = 0xc000_003e;
    #[cfg(target_arch = "aarch64")]
    const ARCH: u32 = 0xc000_00b7;
    const KILL: u32 = 0x8000_0000;
    const EPERM: u32 = 0x0005_0000 | libc::EPERM as u32;
    const ALLOW: u32 = 0x7fff_0000;
    // Everything a picture decoder has no business calling. A deny-list cannot be complete, so
    // it targets the kernel attack surface that sandbox escapes usually go through (io_uring,
    // bpf, key rings, userfaultfd, perf, module loading, the new mount API, kernel handles) on top
    // of processes, sockets, tracing and namespace changes. The namespaces, dropped capabilities
    // and no_new_privs set up around this process are the primary boundary.
    let mut denied = vec![
        libc::SYS_execve, libc::SYS_execveat, libc::SYS_socket, libc::SYS_ptrace,
        libc::SYS_process_vm_readv, libc::SYS_process_vm_writev, libc::SYS_setns,
        libc::SYS_unshare, libc::SYS_mount, libc::SYS_umount2,
        libc::SYS_io_uring_setup, libc::SYS_io_uring_enter, libc::SYS_io_uring_register,
        libc::SYS_bpf, libc::SYS_keyctl, libc::SYS_add_key, libc::SYS_request_key,
        libc::SYS_userfaultfd, libc::SYS_perf_event_open,
        libc::SYS_init_module, libc::SYS_finit_module, libc::SYS_delete_module,
        libc::SYS_kexec_load, libc::SYS_kexec_file_load, libc::SYS_reboot,
        libc::SYS_swapon, libc::SYS_swapoff, libc::SYS_acct, libc::SYS_pivot_root, libc::SYS_chroot,
        libc::SYS_open_by_handle_at, libc::SYS_name_to_handle_at, libc::SYS_fanotify_init,
        libc::SYS_kcmp, libc::SYS_pidfd_getfd, libc::SYS_process_madvise,
        libc::SYS_mount_setattr, libc::SYS_move_mount, libc::SYS_open_tree,
        libc::SYS_fsopen, libc::SYS_fsconfig, libc::SYS_fsmount, libc::SYS_fspick,
    ];
    #[cfg(target_arch = "x86_64")]
    denied.extend([libc::SYS_fork, libc::SYS_vfork]);
    let mut program = vec![
        insn(BPF_LD_W_ABS, 0, 0, 4), // seccomp_data.arch
        insn(BPF_JEQ_K, 1, 0, ARCH),
        insn(BPF_RET_K, 0, 0, KILL),
        insn(BPF_LD_W_ABS, 0, 0, 0), // seccomp_data.nr
        insn(BPF_JGE_K, 0, 1, 0x4000_0000), // x32 ABI
        insn(BPF_RET_K, 0, 0, KILL),
    ];
    for call in denied {
        program.push(insn(BPF_JEQ_K, 0, 1, call as u32));
        program.push(insn(BPF_RET_K, 0, 0, EPERM));
    }
    program.push(insn(BPF_RET_K, 0, 0, ALLOW));
    let filter = libc::sock_fprog { len: program.len() as u16, filter: program.as_mut_ptr() };
    unsafe { libc::prctl(libc::PR_SET_SECCOMP, libc::SECCOMP_MODE_FILTER, &filter) == 0 }
}


/// Apply the limits every helper shares, then the seccomp filter. `address_space` and
/// `max_file_bytes` differ per helper. Returns false if any step failed, in which case the helper
/// must not continue.
pub fn harden(address_space: libc::rlim_t, open_files: libc::rlim_t, max_file_bytes: libc::rlim_t) -> bool {
    limit(libc::RLIMIT_AS, address_space)
        && limit(libc::RLIMIT_CORE, 0)
        && limit(libc::RLIMIT_NOFILE, open_files)
        && limit(libc::RLIMIT_FSIZE, max_file_bytes)
        && unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) } == 0
        && restrict_calls()
}

/// `selftest`: after the same hardening as a real run, try each forbidden call and report what the
/// kernel said. Exits 0 only if every one was refused with EPERM and an allowed call still works.
pub fn selftest() -> Result<(), i32> {
    let null = std::ptr::null::<libc::c_void>();
    // The error code must be read right after its own call, before anything else can change it.
    fn attempt(name: &'static str, call: impl FnOnce() -> i64) -> (&'static str, i64, Option<i32>) {
        let result = call();
        (name, result, std::io::Error::last_os_error().raw_os_error())
    }
    let attempts = [
        attempt("socket", || unsafe { libc::socket(libc::AF_INET, libc::SOCK_STREAM, 0) } as i64),
        attempt("io_uring_setup", || unsafe { libc::syscall(libc::SYS_io_uring_setup, 1, null) }),
        attempt("bpf", || unsafe { libc::syscall(libc::SYS_bpf, 0, null, 0) }),
        attempt("keyctl", || unsafe { libc::syscall(libc::SYS_keyctl, 0, 0, 0, 0, 0) }),
        attempt("userfaultfd", || unsafe { libc::syscall(libc::SYS_userfaultfd, 0) }),
        attempt("perf_event_open", || unsafe { libc::syscall(libc::SYS_perf_event_open, null, 0, -1, -1, 0) }),
        attempt("execve", || unsafe { libc::execve(c"/bin/true".as_ptr(), std::ptr::null(), std::ptr::null()) } as i64),
        attempt("mount", || unsafe { libc::mount(std::ptr::null(), std::ptr::null(), std::ptr::null(), 0, null) } as i64),
    ];
    let mut failed = false;
    for (name, result, errno) in attempts {
        // EPERM is the filter's answer; any other failure (ENOSYS, EINVAL…) means the call reached
        // the kernel, so it was not refused by the filter.
        let refused = result == -1 && errno == Some(libc::EPERM);
        println!("{name}: {}", if refused { "refused".to_owned() } else { format!("NOT REFUSED ({result}, errno {errno:?})") });
        failed |= !refused;
    }
    // What this process inherited: nothing above stdio may be open. (Informational here; the
    // sandbox tests assert it, because a test harness legitimately holds descriptors.)
    let names: Vec<String> = std::fs::read_dir("/proc/self/fd")
        .map(|dir| dir.flatten().map(|entry| entry.file_name().to_string_lossy().into_owned()).collect())
        .unwrap_or_default();
    // The directory handle used for the listing is closed by now, so it is skipped here.
    let inherited: Vec<i32> = names
        .iter()
        .filter_map(|name| name.parse::<i32>().ok())
        .filter(|fd| *fd > 2 && unsafe { libc::fcntl(*fd, libc::F_GETFD) } >= 0)
        .collect();
    if inherited.is_empty() {
        println!("inherited descriptors: none");
    } else {
        println!("INHERITED DESCRIPTORS: {inherited:?}");
    }
    // The filter must not be a blanket ban: ordinary calls and threads still work.
    let alive = unsafe { libc::getpid() } > 0 && std::thread::spawn(|| 7).join().ok() == Some(7);
    println!("ordinary calls and threads: {}", if alive { "work" } else { "BROKEN" });
    if failed || !alive { Err(9) } else { Ok(()) }
}

