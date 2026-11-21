//! Processes: signals, pidfds, waiting, exit codes, and the start of a
//! program with an explicit environment.

use std::ffi::CString;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::process::ExitStatus;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::util::Result;

/// The signals that came since the last `take_signals`, one bit each.
static PENDING: AtomicU64 = AtomicU64::new(0);

extern "C" fn on_signal(sig: libc::c_int) {
    PENDING.fetch_or(1 << sig, Ordering::SeqCst);
}

/// Catch these signals: they stop a blocking system call (no SA_RESTART),
/// and `take_signals` gives them.
pub fn catch_signals(sigs: &[libc::c_int]) {
    for &sig in sigs {
        // SAFETY: the handler only changes an atomic value.
        unsafe {
            let mut sa: libc::sigaction = std::mem::zeroed();
            sa.sa_sigaction = on_signal as *const () as usize;
            libc::sigemptyset(&mut sa.sa_mask);
            sa.sa_flags = 0;
            libc::sigaction(sig, &sa, std::ptr::null_mut());
        }
    }
}

/// The default handling of all signals, before exec. An ignored signal
/// stays ignored after exec, and the Rust runtime ignores SIGPIPE: a program
/// must not get that.
pub fn default_signals() {
    for sig in 1..32 {
        if sig != libc::SIGKILL && sig != libc::SIGSTOP {
            // SAFETY: SIG_DFL is a valid disposition.
            unsafe {
                libc::signal(sig, libc::SIG_DFL);
            }
        }
    }
}

/// The signals that came, and forget them.
pub fn take_signals() -> Vec<libc::c_int> {
    let bits = PENDING.swap(0, Ordering::SeqCst);
    (1..64).filter(|s| bits & (1 << s) != 0).collect()
}

/// The exit code of a shell for a status: the code, or 128 + the signal.
pub fn code(status: ExitStatus) -> i32 {
    use std::os::unix::process::ExitStatusExt;
    status.code().unwrap_or_else(|| 128 + status.signal().unwrap_or(0))
}

/// The exit code of a shell for a raw wait status.
pub fn code_raw(status: libc::c_int) -> i32 {
    if libc::WIFEXITED(status) {
        libc::WEXITSTATUS(status)
    } else if libc::WIFSIGNALED(status) {
        128 + libc::WTERMSIG(status)
    } else {
        1
    }
}

/// Wait for a child. A signal stops the wait: then the result is None.
pub fn wait_pid(pid: libc::pid_t) -> Option<libc::c_int> {
    let mut status = 0;
    // SAFETY: status is a valid pointer.
    let r = unsafe { libc::waitpid(pid, &mut status, 0) };
    if r == pid { Some(status) } else { None }
}

/// Wait for any child. A signal stops the wait: then the result is None. No
/// child: Some((-1, 0)).
pub fn wait_any() -> Option<(libc::pid_t, libc::c_int)> {
    let mut status = 0;
    // SAFETY: status is a valid pointer.
    let r = unsafe { libc::waitpid(-1, &mut status, 0) };
    if r > 0 {
        Some((r, status))
    } else if std::io::Error::last_os_error().raw_os_error() == Some(libc::ECHILD) {
        Some((-1, 0))
    } else {
        None
    }
}

/// Wait for the child `pid`, and give it the signals TERM and HUP of this
/// process. The result is the exit code of a shell.
pub fn wait_forwarding(pid: libc::pid_t) -> i32 {
    catch_signals(&[libc::SIGTERM, libc::SIGHUP]);
    wait_child(pid, &mut |s| {
        if s == libc::SIGTERM || s == libc::SIGHUP {
            kill(pid, s);
        }
    })
}

/// Wait for the child `pid`, and give each signal that comes (refer to
/// catch_signals) to `on_signal`. The result is the exit code of a shell.
/// A signal can come just before a blocking wait: then the wait would not
/// see it. Thus wait on the pidfd with a timeout, and look at the signals
/// each time.
pub fn wait_child(pid: libc::pid_t, on_signal: &mut dyn FnMut(libc::c_int)) -> i32 {
    let fd = pidfd_open(pid).ok();
    loop {
        for s in take_signals() {
            on_signal(s);
        }
        match &fd {
            Some(fd) if !readable(fd.as_raw_fd(), 100) => continue,
            _ => {}
        }
        if let Some(st) = wait_pid(pid) {
            return code_raw(st);
        }
    }
}

/// Whether a process is alive (it can be a zombie).
pub fn alive(pid: libc::pid_t) -> bool {
    // SAFETY: signal 0 only checks the process.
    unsafe { libc::kill(pid, 0) == 0 }
}

pub fn kill(pid: libc::pid_t, sig: libc::c_int) {
    if pid > 0 {
        // SAFETY: kill has no memory effects.
        unsafe {
            libc::kill(pid, sig);
        }
    }
}

/// A pidfd of the process `pid`.
pub fn pidfd_open(pid: libc::pid_t) -> Result<OwnedFd> {
    // SAFETY: pidfd_open takes two integers.
    let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) };
    if fd < 0 {
        return Err(format!("process {pid}: {}", std::io::Error::last_os_error()));
    }
    // SAFETY: the kernel gave a new descriptor.
    Ok(unsafe { OwnedFd::from_raw_fd(fd as RawFd) })
}

/// Whether the process of a pidfd stopped: its pidfd is readable then.
pub fn pidfd_exited(fd: &OwnedFd) -> bool {
    let mut p = libc::pollfd {
        fd: fd.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    // SAFETY: one valid pollfd, no wait.
    let r = unsafe { libc::poll(&mut p, 1, 0) };
    r != 0
}

/// The process `pid` with the start time `start` (as util::starttime), as a
/// pidfd. Then a different process with the same PID cannot take its place.
pub fn pidfd_checked(pid: libc::pid_t, start: &str) -> Result<OwnedFd> {
    let fd = pidfd_open(pid)?;
    if crate::util::starttime(pid).as_deref() != Some(start) || pidfd_exited(&fd) {
        return Err(format!("the process {pid} stopped"));
    }
    Ok(fd)
}

/// A C string. The arguments of the command have no NUL byte.
pub fn cstr(s: &str) -> CString {
    CString::new(s).unwrap_or_else(|_| crate::die!("a NUL byte in {s:?}"))
}

/// Find a program in a PATH value, as execvp does.
pub fn find_program(prog: &str, path: &str) -> Option<String> {
    if prog.contains('/') {
        return Some(prog.to_string());
    }
    for dir in path.split(':') {
        let dir = if dir.is_empty() { "." } else { dir };
        let p = format!("{dir}/{prog}");
        let c = cstr(&p);
        // SAFETY: c is a valid C string.
        if unsafe { libc::access(c.as_ptr(), libc::X_OK) } == 0 {
            return Some(p);
        }
    }
    None
}

/// Replace this process with `argv`, with the environment `env` and no
/// other variable. The program is found in the PATH of `env`. Returns only
/// with an error.
pub fn exec_with_env(argv: &[String], env: &[(String, String)]) -> String {
    let path = env.iter().find(|(k, _)| k == "PATH").map(|(_, v)| v.as_str()).unwrap_or("/bin:/usr/bin");
    let Some(prog) = argv.first() else {
        return "no command".into();
    };
    let Some(file) = find_program(prog, path) else {
        return format!("{prog}: command not found");
    };
    let file_c = cstr(&file);
    let args: Vec<CString> = argv.iter().map(|a| cstr(a)).collect();
    let envs: Vec<CString> = env.iter().map(|(k, v)| cstr(&format!("{k}={v}"))).collect();
    let mut argp: Vec<*const libc::c_char> = args.iter().map(|a| a.as_ptr()).collect();
    argp.push(std::ptr::null());
    let mut envp: Vec<*const libc::c_char> = envs.iter().map(|a| a.as_ptr()).collect();
    envp.push(std::ptr::null());
    default_signals();
    // SAFETY: the pointers point into the CStrings above, the lists end with NULL.
    unsafe {
        libc::execve(file_c.as_ptr(), argp.as_ptr(), envp.as_ptr());
    }
    format!("{file}: {}", std::io::Error::last_os_error())
}

/// Replace this process with `argv`, with the current environment.
pub fn exec(argv: &[String]) -> String {
    let env: Vec<(String, String)> = std::env::vars().collect();
    exec_with_env(argv, &env)
}

/// The path of this program in the store (for the internal commands).
pub fn self_exe() -> String {
    std::fs::read_link("/proc/self/exe")
        .map(|p| crate::util::path_str(&p))
        .unwrap_or_else(|e| crate::die!("/proc/self/exe: {e}"))
}

/// A pipe: (read end, write end), both close-on-exec.
pub fn pipe() -> Result<(OwnedFd, OwnedFd)> {
    let mut fds = [0; 2];
    // SAFETY: fds has two elements.
    if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
        return Err(format!("pipe: {}", std::io::Error::last_os_error()));
    }
    // SAFETY: the kernel gave two new descriptors.
    Ok(unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) })
}

/// Wait until `fd` is readable, at most `ms` milliseconds.
pub fn readable(fd: RawFd, ms: i32) -> bool {
    let mut p = libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    };
    // SAFETY: one valid pollfd.
    unsafe { libc::poll(&mut p, 1, ms) > 0 }
}

/// A random number for the names of units ($RANDOM of a shell: 0 to 32767).
pub fn random15() -> u32 {
    let mut b = [0u8; 2];
    // SAFETY: the buffer has 2 bytes.
    unsafe { libc::getrandom(b.as_mut_ptr().cast(), 2, 0) };
    u16::from_ne_bytes(b) as u32 & 0x7fff
}

/// The children of a process: the processes with this parent PID.
pub fn children(pid: libc::pid_t) -> Vec<libc::pid_t> {
    let mut out = Vec::new();
    let Ok(dir) = std::fs::read_dir("/proc") else {
        return out;
    };
    for entry in dir.flatten() {
        let Some(n) = entry.file_name().to_str().and_then(|s| s.parse::<libc::pid_t>().ok()) else {
            continue;
        };
        let Ok(stat) = std::fs::read(format!("/proc/{n}/stat")) else {
            continue;
        };
        let Some(close) = stat.iter().rposition(|&c| c == b')') else {
            continue;
        };
        let rest = String::from_utf8_lossy(&stat[close + 1..]);
        // After the name: the state, then the parent PID.
        if rest.split_whitespace().nth(1).and_then(|s| s.parse::<libc::pid_t>().ok()) == Some(pid) {
            out.push(n);
        }
    }
    out
}

/// Set a signal to the parent death signal of this process (in a child).
pub fn die_with_parent() {
    // SAFETY: prctl with integer arguments.
    unsafe {
        libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL, 0, 0, 0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_codes_of_a_shell() {
        // A raw wait status: the exit code in the second byte, the signal in
        // the first.
        assert_eq!(code_raw(7 << 8), 7);
        assert_eq!(code_raw(0), 0);
        assert_eq!(code_raw(libc::SIGKILL), 128 + 9);
        use std::os::unix::process::ExitStatusExt;
        assert_eq!(code(ExitStatus::from_raw(3 << 8)), 3);
        assert_eq!(code(ExitStatus::from_raw(libc::SIGTERM)), 128 + 15);
    }

    #[test]
    fn programs_in_path() {
        let d = std::env::temp_dir().join(format!("sandbox-path-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("a")).unwrap();
        std::fs::create_dir_all(d.join("b")).unwrap();
        use std::os::unix::fs::PermissionsExt;
        // Not executable in a, executable in b.
        std::fs::write(d.join("a/prog"), "").unwrap();
        std::fs::write(d.join("b/prog"), "").unwrap();
        std::fs::set_permissions(d.join("b/prog"), std::fs::Permissions::from_mode(0o755)).unwrap();
        let (a, b) = (d.join("a").to_str().unwrap().to_string(), d.join("b").to_str().unwrap().to_string());
        assert_eq!(find_program("prog", &format!("{a}:{b}")).unwrap(), format!("{b}/prog"));
        assert_eq!(find_program("prog", &a), None);
        // A name with "/" is not searched.
        assert_eq!(find_program("./x/prog", &a).unwrap(), "./x/prog");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn exec_errors() {
        assert_eq!(exec_with_env(&[], &[]), "no command");
        assert_eq!(exec_with_env(&["no-such-program-x".into()], &[("PATH".into(), "/nonexistent".into())]), "no-such-program-x: command not found");
    }

    #[test]
    fn pidfd_of_a_checked_process() {
        let me = std::process::id() as libc::pid_t;
        let start = crate::util::starttime(me).unwrap();
        let fd = pidfd_checked(me, &start).unwrap();
        assert!(!pidfd_exited(&fd));
        // A different start time: a different process with the same PID.
        assert!(pidfd_checked(me, "1").is_err());
        assert!(pidfd_open(0x3fff_ffff).is_err());
    }

    #[test]
    fn children_and_exit_of_a_child() {
        let mut child = std::process::Command::new("sleep").arg("5").spawn().unwrap();
        let pid = child.id() as libc::pid_t;
        let me = std::process::id() as libc::pid_t;
        assert!(children(me).contains(&pid));
        let fd = pidfd_open(pid).unwrap();
        assert!(!pidfd_exited(&fd));
        assert!(alive(pid));
        kill(pid, libc::SIGKILL);
        let st = loop {
            if let Some(st) = wait_pid(pid) {
                break st;
            }
        };
        assert_eq!(code_raw(st), 128 + 9);
        assert!(pidfd_exited(&fd));
        let _ = child.try_wait();
        // kill of PID 0 or less does nothing (no process group).
        kill(0, libc::SIGKILL);
    }

    #[test]
    fn pipes_and_readable() {
        use std::io::Write;
        let (r, w) = pipe().unwrap();
        assert!(!readable(r.as_raw_fd(), 0));
        std::fs::File::from(w).write_all(b"1").unwrap();
        assert!(readable(r.as_raw_fd(), 0));
    }

    #[test]
    fn wait_forwarding_gives_term_to_the_child() {
        let child = std::process::Command::new("sleep").arg("30").spawn().unwrap();
        let pid = child.id() as libc::pid_t;
        // A TERM before the wait: it must not stay in the flags.
        catch_signals(&[libc::SIGTERM]);
        // SAFETY: a signal to this process; the handler only sets a flag.
        unsafe { libc::raise(libc::SIGTERM) };
        let t = std::time::Instant::now();
        assert_eq!(wait_forwarding(pid), 128 + libc::SIGTERM);
        assert!(t.elapsed() < std::time::Duration::from_secs(2));
    }

    #[test]
    fn random_numbers_of_units() {
        assert!((0..100).map(|_| random15()).all(|n| n < 32768));
    }

    #[test]
    fn signals_come_once() {
        catch_signals(&[libc::SIGUSR2]);
        // SAFETY: a signal to this process, with a handler.
        unsafe { libc::raise(libc::SIGUSR2) };
        assert!(take_signals().contains(&libc::SIGUSR2));
        assert!(!take_signals().contains(&libc::SIGUSR2));
    }

    /// A child of Command does not get the ignored SIGPIPE of the Rust
    /// runtime.
    #[test]
    fn command_child_has_default_sigpipe() {
        let out = std::process::Command::new("sh").args(["-c", "grep SigIgn /proc/self/status"]).output().unwrap();
        let text = String::from_utf8_lossy(&out.stdout);
        let mask = u64::from_str_radix(text.split_whitespace().nth(1).unwrap(), 16).unwrap();
        assert_eq!(mask & 0x1000, 0, "{text}");
    }
}
