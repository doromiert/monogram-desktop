// SPDX-License-Identifier: GPL-3.0-or-later
//! Shared self-sandboxing for the private renderer helpers (included with `#[path]`).
//! The helpers also run inside bwrap; this is the second layer.

pub fn limit(resource: libc::__rlimit_resource_t, value: libc::rlim_t) -> bool {
    let v = libc::rlimit { rlim_cur: value, rlim_max: value };
    unsafe { libc::setrlimit(resource, &v) == 0 }
}
fn insn(code: u32, jt: u8, jf: u8, k: u32) -> libc::sock_filter {
    libc::sock_filter { code: code as u16, jt, jf, k }
}
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
    let mut denied = vec![
        libc::SYS_clone, libc::SYS_clone3, libc::SYS_execve, libc::SYS_execveat,
        libc::SYS_socket, libc::SYS_socketpair, libc::SYS_ptrace,
        libc::SYS_process_vm_readv, libc::SYS_process_vm_writev, libc::SYS_setns,
        libc::SYS_unshare, libc::SYS_mount, libc::SYS_umount2,
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
