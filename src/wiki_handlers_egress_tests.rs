// wiki_handlers_egress_tests — criteria 1 and 2 of issue #196, asserted on
// the tool's own path: `export_wiki_skeleton` opens no socket (no network
// call) and creates no process (no model client, in process or as a
// program), from the graph read to the artifact write.
//
// How. The test runs itself again as a child process (the same test binary,
// this test only), passing a mode in the environment. The child installs a
// seccomp filter on all its threads that kills the whole process with
// SIGSYS on `socket`, `connect`, `io_uring_setup`, `execve`, `execveat`,
// on a `clone` without CLONE_THREAD (a new process; threads are allowed),
// and on `fork`/`vfork` where they exist; `clone3` answers ENOSYS so that
// glibc falls back to `clone`, whose flags the filter can read. Then it
// does what its mode says. The parent prepares the graph before, outside
// the filter, and reads the child's exit.
//
// Falsifiability. Two controls run the same child with a TCP connect and
// with a program spawn instead of the export: both must die by SIGSYS. A
// filter that let either through would fail them, so a passing export run
// means the path made none of those calls, not that the filter is inert.
//
// Linux on x86_64 and aarch64 only (the filter checks the audit arch); the
// CI test jobs run there.

use super::tests::{export_args, fixture};
use std::os::unix::process::ExitStatusExt;
use std::process::Command;

const MODE_ENV: &str = "AP_WIKI_EGRESS_CHILD_MODE";
const GRAPH_ENV: &str = "AP_WIKI_EGRESS_CHILD_GRAPH";
const CHILD_DONE: &str = "AP_WIKI_EGRESS_CHILD_DONE";

/// `AUDIT_ARCH_X86_64` or `AUDIT_ARCH_AARCH64` (the module is built on those
/// two arches only).
// source: include/uapi/linux/audit.h, AUDIT_ARCH_X86_64 = EM_X86_64 (62) |
// __AUDIT_ARCH_64BIT | __AUDIT_ARCH_LE; AUDIT_ARCH_AARCH64 = EM_AARCH64 (183) | the same.
const AUDIT_ARCH: u32 = if cfg!(target_arch = "x86_64") {
    0xC000_003E
} else {
    0xC000_00B7
};

/// `CLONE_THREAD`.
// source: include/uapi/linux/sched.h, CLONE_THREAD 0x00010000.
const CLONE_THREAD: u32 = 0x0001_0000;
/// `BPF_JSET`, linux/bpf_common.h (jump if A & k != 0).
const BPF_JSET: u16 = 0x40;
/// `BPF_JGE`, linux/bpf_common.h.
const BPF_JGE: u16 = 0x30;
/// `struct seccomp_data` offsets: `nr`, `arch`, low word of `args[0]`
/// (little-endian on both arches).
const OFF_NR: u32 = 0;
const OFF_ARCH: u32 = 4;
const OFF_ARG0_LO: u32 = 16;
/// The x32 ABI bit of an x86_64 syscall number.
// source: arch/x86/include/uapi/asm/unistd.h, __X32_SYSCALL_BIT 0x40000000.
#[cfg(target_arch = "x86_64")]
const X32_SYSCALL_BIT: u32 = 0x4000_0000;

fn stmt(code: u32, k: u32) -> libc::sock_filter {
    libc::sock_filter {
        code: code as u16,
        jt: 0,
        jf: 0,
        k,
    }
}

fn jump(code: u16, k: u32, jt: u8, jf: u8) -> libc::sock_filter {
    libc::sock_filter { code, jt, jf, k }
}

/// The syscalls that kill: a socket, a connection, an io_uring (which can
/// open sockets without `socket`), a program image, a forked process.
fn killed_syscalls() -> Vec<u32> {
    let mut nrs = vec![
        libc::SYS_socket,
        libc::SYS_connect,
        libc::SYS_io_uring_setup,
        libc::SYS_execve,
        libc::SYS_execveat,
    ];
    #[cfg(target_arch = "x86_64")]
    nrs.extend([libc::SYS_fork, libc::SYS_vfork]);
    nrs.into_iter().map(|n| n as u32).collect()
}

fn filter_program() -> Vec<libc::sock_filter> {
    let ld = libc::BPF_LD | libc::BPF_W | libc::BPF_ABS;
    let jeq = (libc::BPF_JMP | libc::BPF_JEQ | libc::BPF_K) as u16;
    let ret = libc::BPF_RET | libc::BPF_K;
    let kill = libc::SECCOMP_RET_KILL_PROCESS;
    let allow = libc::SECCOMP_RET_ALLOW;
    let enosys = libc::SECCOMP_RET_ERRNO | (libc::ENOSYS as u32);

    let mut p = vec![
        stmt(ld, OFF_ARCH),
        jump(jeq, AUDIT_ARCH, 1, 0),
        stmt(ret, kill),
        stmt(ld, OFF_NR),
    ];
    #[cfg(target_arch = "x86_64")]
    {
        p.push(jump(
            (libc::BPF_JMP as u16) | BPF_JGE | (libc::BPF_K as u16),
            X32_SYSCALL_BIT,
            0,
            1,
        ));
        p.push(stmt(ret, kill));
    }
    for nr in killed_syscalls() {
        p.push(jump(jeq, nr, 0, 1));
        p.push(stmt(ret, kill));
    }
    p.push(jump(jeq, libc::SYS_clone3 as u32, 0, 1));
    p.push(stmt(ret, enosys));
    // clone: a thread (CLONE_THREAD set) is allowed, a process is killed.
    p.push(jump(jeq, libc::SYS_clone as u32, 0, 3));
    p.push(stmt(ld, OFF_ARG0_LO));
    p.push(jump(
        (libc::BPF_JMP as u16) | BPF_JSET | (libc::BPF_K as u16),
        CLONE_THREAD,
        1,
        0,
    ));
    p.push(stmt(ret, kill));
    p.push(stmt(ret, allow));
    p
}

/// Installs the filter on every thread of this process, for good.
fn install_filter() {
    let mut program = filter_program();
    let prog = libc::sock_fprog {
        len: program.len() as u16,
        filter: program.as_mut_ptr(),
    };
    // SAFETY: plain syscalls on a filter that outlives the call (the kernel
    // copies it); `prog` points at `program`, alive until the end of scope.
    unsafe {
        assert_eq!(
            libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0),
            0,
            "PR_SET_NO_NEW_PRIVS: {}",
            std::io::Error::last_os_error()
        );
        let rc = libc::syscall(
            libc::SYS_seccomp,
            libc::SECCOMP_SET_MODE_FILTER,
            libc::SECCOMP_FILTER_FLAG_TSYNC,
            &prog as *const libc::sock_fprog,
        );
        assert_eq!(rc, 0, "seccomp: {}", std::io::Error::last_os_error());
    }
}

/// The child: filter, then the mode's work. Prints `CHILD_DONE` when the work
/// returned, so the parent can tell a finished run from a test filtered out.
fn child(mode: &str) {
    install_filter();
    match mode {
        "export" => {
            let graph = std::env::var(GRAPH_ENV).expect("graph path");
            let response = super::do_export_wiki_skeleton(&export_args(graph.as_ref()))
                .expect("export under the filter");
            assert_eq!(response["status"], "ok", "{response}");
        }
        "socket" => {
            let _ = std::net::TcpStream::connect(("127.0.0.1", 9));
        }
        "exec" => {
            let _ = Command::new("true").status();
        }
        other => panic!("unknown child mode {other}"),
    }
    println!("{CHILD_DONE}");
}

/// Runs `test` of this binary again under `mode`; returns its exit status
/// and standard output.
fn run_child(
    test: &str,
    mode: &str,
    graph: Option<&std::path::Path>,
) -> (std::process::ExitStatus, String) {
    let mut cmd = Command::new(std::env::current_exe().expect("test binary"));
    cmd.args([test, "--exact", "--nocapture", "--test-threads=1"])
        .env(MODE_ENV, mode);
    if let Some(g) = graph {
        cmd.env(GRAPH_ENV, g);
    }
    let out = cmd.output().expect("run the child");
    (
        out.status,
        String::from_utf8_lossy(&out.stdout).into_owned(),
    )
}

const EXPORT_TEST: &str =
    "wiki_handlers::egress_tests::the_export_opens_no_socket_and_creates_no_process";
const CONTROL_TEST: &str = "wiki_handlers::egress_tests::the_filter_kills_a_connect_and_a_spawn";

#[test]
fn the_export_opens_no_socket_and_creates_no_process() {
    if let Ok(mode) = std::env::var(MODE_ENV) {
        return child(&mode);
    }
    let (_tmp, graph) = fixture(true);
    let (status, stdout) = run_child(EXPORT_TEST, "export", Some(&graph));
    assert!(
        status.success() && stdout.contains(CHILD_DONE),
        "the export under the filter did not finish: {status:?} (SIGSYS = {}): {stdout}",
        libc::SIGSYS
    );
    let artifact = graph.parent().unwrap().join(super::WIKI_SKELETON_FILE);
    assert!(artifact.is_file(), "the child wrote no artifact");
}

#[test]
fn the_filter_kills_a_connect_and_a_spawn() {
    if let Ok(mode) = std::env::var(MODE_ENV) {
        return child(&mode);
    }
    for mode in ["socket", "exec"] {
        let (status, stdout) = run_child(CONTROL_TEST, mode, None);
        assert_eq!(
            status.signal(),
            Some(libc::SIGSYS),
            "{mode}: the filter let it through: {status:?}: {stdout}"
        );
        assert!(!stdout.contains(CHILD_DONE), "{mode}: {stdout}");
    }
}
