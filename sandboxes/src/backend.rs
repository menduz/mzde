//! The built-in backends: bwrap and gVisor. A backend has the interface of
//! README.md:
//!
//!   <backend> run   PLAN.json   a container for the program of the plan
//!   <backend> start PLAN.json   the container of a sandbox; "ready" in .state
//!   <backend> exec  PLAN.json   the program of the plan in that container
//!   <backend> show  PLAN.json   print the command, and do not start it
//!
//! The package installs sandbox-backend-bwrap and sandbox-backend-gvisor as
//! links to this program.

use serde_json::json;
use std::fs;
use std::io::Read;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};

use crate::plan::{MountType, Plan};
use crate::process::{self, catch_signals, take_signals};
use crate::pty;
use crate::run::{mktemp_dir, open_ns};
use crate::tools;
use crate::util::{self, Result};

/// The name of the program in messages.
fn prog(backend: &str) -> String {
    format!("sandbox-backend-{backend}")
}

macro_rules! fail {
    ($b:expr, $($arg:tt)*) => {{
        eprintln!("{}: {}", prog($b), format!($($arg)*));
        std::process::exit(1)
    }};
}

/// `sandbox-backend-NAME MODE PLAN`.
pub fn main(backend: &str, args: &[String]) -> ! {
    if args.len() != 2 {
        eprintln!("usage: {} run|start|exec|show PLAN.json", prog(backend));
        std::process::exit(2);
    }
    let (mode, path) = (args[0].as_str(), args[1].as_str());
    let plan = Plan::read(path).unwrap_or_else(|e| fail!(backend, "{e}"));
    match (backend, mode) {
        ("bwrap", "run") => fail!(backend, "{}", process::exec(&bwrap_command(&plan, false))),
        ("bwrap", "start") => bwrap_start(&plan),
        ("bwrap", "exec") => bwrap_exec(&plan),
        ("bwrap", "show") => {
            println!("{}", shell_words(&bwrap_command(&plan, false)));
            std::process::exit(0)
        }
        ("gvisor", "run" | "start" | "exec" | "show") => gvisor(mode, &plan),
        _ => {
            eprintln!("usage: {} run|start|exec|show PLAN.json", prog(backend));
            std::process::exit(2)
        }
    }
}

/// A word for a shell: as it is, or in single quotes.
pub fn shell_quote(s: &str) -> String {
    if !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || "_./=:,+@%-".contains(c)) {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}

fn shell_words(args: &[String]) -> String {
    args.iter().map(|a| shell_quote(a)).collect::<Vec<_>>().join(" ")
}

/// The bwrap command of a plan. With `info`, bwrap writes the PID of
/// process 1 of the sandbox to the descriptor 3.
fn bwrap_command(p: &Plan, info: bool) -> Vec<String> {
    let mut a: Vec<String> = vec![tools::bwrap().into()];
    if info {
        a.extend(["--info-fd".into(), "3".into()]);
    }
    a.extend(
        [
            "--die-with-parent",
            "--unshare-user",
            "--unshare-ipc",
            "--unshare-pid",
            "--unshare-uts",
            "--unshare-cgroup-try",
        ]
        .map(String::from),
    );
    a.extend(["--uid".into(), p.uid.to_string(), "--gid".into(), p.gid.to_string()]);
    a.extend(["--hostname".into(), p.hostname.clone()]);
    // A program without a terminal cannot use TIOCSTI on the terminal of the
    // caller. A shell on a terminal must keep the terminal for job control.
    if !p.tty {
        a.push("--new-session".into());
    }
    if p.network == "none" {
        a.push("--unshare-net".into());
    }
    a.extend(["--proc", "/proc", "--dev", "/dev", "--tmpfs", "/dev/shm"].map(String::from));
    for m in &p.mounts {
        match m.kind {
            MountType::Bind => a.extend([if m.mode == "ro" { "--ro-bind" } else { "--bind" }.into(), m.source.clone(), m.target.clone()]),
            MountType::Dev => a.extend(["--dev-bind".into(), m.source.clone(), m.target.clone()]),
            MountType::Tmpfs => a.extend(["--perms".into(), m.mode.clone(), "--tmpfs".into(), m.target.clone()]),
        }
    }
    a.push("--clearenv".into());
    for (k, v) in &p.env {
        a.extend(["--setenv".into(), k.clone(), v.clone()]);
    }
    a.extend(["--chdir".into(), p.cwd.clone()]);
    a.extend(p.extra_args.iter().cloned());
    a.push("--".into());
    a.extend(p.command.iter().cloned());
    a
}

/// The state directory of a plan for start and exec.
fn state(backend: &str, p: &Plan) -> String {
    if p.state.is_empty() {
        fail!(backend, "the plan has no state directory");
    }
    p.state.clone()
}

/// start: the container. bwrap writes the PID of process 1 of the sandbox
/// before process 1 makes the root of the sandbox: before that, its root is
/// the root of the host. Process 1 starts the command after it made the
/// root and all namespaces, thus the container is ready when process 1 has
/// a child. Then `exec` can enter it: state/pid has "PID STARTTIME".
fn bwrap_start(p: &Plan) -> ! {
    let state = state("bwrap", p);
    fs::create_dir_all(&state).unwrap_or_else(|e| fail!("bwrap", "{state}: {e}"));
    let (r, w) = process::pipe().unwrap_or_else(|e| fail!("bwrap", "{e}"));
    let wfd = w.as_raw_fd();
    let argv = bwrap_command(p, true);
    let mut cmd = Command::new(&argv[0]);
    cmd.args(&argv[1..]).stdin(Stdio::null());
    // SAFETY: the closure calls only dup2.
    unsafe {
        cmd.pre_exec(move || {
            if libc::dup2(wfd, 3) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    catch_signals(&[libc::SIGTERM, libc::SIGINT, libc::SIGHUP]);
    let child = cmd.spawn().unwrap_or_else(|e| fail!("bwrap", "bwrap: {e}"));
    let bwrap = child.id() as libc::pid_t;
    drop(w);
    let mut info = String::new();
    let _ = fs::File::from(r).read_to_string(&mut info);
    let pid = serde_json::from_str::<serde_json::Value>(&info)
        .ok()
        .and_then(|v| v["child-pid"].as_i64())
        .map(|p| p as libc::pid_t);
    let Some(pid) = pid else {
        let rc = process::wait_pid(bwrap).map(process::code_raw).unwrap_or(1);
        fail!("bwrap", "bwrap stopped (exit code {rc})");
    };
    let mut n = 0;
    while process::children(pid).is_empty() {
        if !take_signals().is_empty() {
            process::kill(bwrap, libc::SIGTERM);
            std::process::exit(1);
        }
        let mut st = 0;
        // SAFETY: status is a valid pointer.
        if unsafe { libc::waitpid(bwrap, &mut st, libc::WNOHANG) } == bwrap {
            fail!("bwrap", "bwrap stopped (exit code {})", process::code_raw(st));
        }
        n += 1;
        if n >= 3000 {
            process::kill(bwrap, libc::SIGTERM);
            fail!("bwrap", "the container did not start in 30 s");
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let start = util::starttime(pid).unwrap_or_default();
    let tmp = format!("{state}/pid.tmp");
    fs::write(&tmp, format!("{pid} {start}\n")).unwrap_or_else(|e| fail!("bwrap", "{tmp}: {e}"));
    fs::rename(&tmp, format!("{state}/pid")).unwrap_or_else(|e| fail!("bwrap", "{state}/pid: {e}"));
    fs::write(format!("{state}/ready"), "").unwrap_or_else(|e| fail!("bwrap", "{state}/ready: {e}"));
    std::process::exit(process::wait_child(bwrap, &mut |_| process::kill(bwrap, libc::SIGTERM)));
}

/// The parent of a user namespace (ioctl NS_GET_PARENT).
fn ns_parent(fd: &OwnedFd) -> Result<OwnedFd> {
    const NS_GET_PARENT: libc::c_ulong = 0xb702;
    // SAFETY: the ioctl takes no argument and returns a new descriptor.
    let r = unsafe { libc::ioctl(fd.as_raw_fd(), NS_GET_PARENT) };
    if r < 0 {
        return Err(format!("the parent user namespace: {}", std::io::Error::last_os_error()));
    }
    // SAFETY: the kernel gave a new descriptor (close-on-exec).
    Ok(unsafe { OwnedFd::from_raw_fd(r) })
}

fn setns(fd: &OwnedFd, kind: libc::c_int, what: &str) -> Result<()> {
    // SAFETY: setns with a valid descriptor.
    if unsafe { libc::setns(fd.as_raw_fd(), kind) } != 0 {
        return Err(format!("setns {what}: {}", std::io::Error::last_os_error()));
    }
    Ok(())
}

/// exec: start the command of the plan in the container that `start` made
/// for the same state directory. The program gets the environment and the
/// directory of the plan, and the namespaces and the root of the container.
/// With a terminal, the program gets a new terminal of the container, as
/// `docker exec -t` (refer to the end of this function).
///
/// With --dev, bwrap makes a second user namespace in the first one, and
/// the first one has the other namespaces. This process enters the parent
/// user namespace (where the user is root), then the other namespaces, then
/// the user namespace of the container (where the user is the user of the
/// plan). The child gets no_new_privs and the parent death signal, and
/// execve removes its capabilities: its user ID is not 0.
fn bwrap_exec(p: &Plan) -> ! {
    let state = state("bwrap", p);
    // No ptrace and no /proc access of this process from the sandbox while it
    // has capabilities in the namespaces of the container.
    // SAFETY: prctl with integer arguments.
    unsafe { libc::prctl(libc::PR_SET_DUMPABLE, 0, 0, 0, 0) };
    let text = fs::read_to_string(format!("{state}/pid")).unwrap_or_else(|_| fail!("bwrap", "the container of {state} is not ready"));
    let (pid, start) = crate::run::parse_pid_file(&text).unwrap_or_else(|| fail!("bwrap", "{state}/pid is not valid"));
    let pidfd = process::pidfd_checked(pid, &start).unwrap_or_else(|_| fail!("bwrap", "the container of {state} stopped"));
    let open = |ns: &str| open_ns(pid, ns).unwrap_or_else(|e| fail!("bwrap", "{e}"));
    let user = open("user");
    let parent = ns_parent(&user).unwrap_or_else(|e| fail!("bwrap", "{e}"));
    let others = [
        (open("ipc"), libc::CLONE_NEWIPC, "ipc"),
        (open("uts"), libc::CLONE_NEWUTS, "uts"),
        (open("cgroup"), libc::CLONE_NEWCGROUP, "cgroup"),
        (open("pid"), libc::CLONE_NEWPID, "pid"),
    ];
    // With "inherit", the caller is in the network namespace of the
    // container.
    let net = (p.network == "none").then(|| open("net"));
    let mnt = open("mnt");
    let root = fs::File::open(format!("/proc/{pid}/root")).unwrap_or_else(|e| fail!("bwrap", "/proc/{pid}/root: {e}"));
    if process::pidfd_exited(&pidfd) {
        fail!("bwrap", "the container of {state} stopped");
    }

    let enter = || -> Result<()> {
        setns(&parent, libc::CLONE_NEWUSER, "user (parent)")?;
        for (fd, kind, what) in &others {
            setns(fd, *kind, what)?;
        }
        if let Some(fd) = &net {
            setns(fd, libc::CLONE_NEWNET, "net")?;
        }
        setns(&mnt, libc::CLONE_NEWNS, "mnt")?;
        setns(&user, libc::CLONE_NEWUSER, "user")?;
        // SAFETY: fchdir and chroot with a valid descriptor and path.
        unsafe {
            if libc::fchdir(root.as_raw_fd()) != 0 || libc::chroot(c".".as_ptr()) != 0 {
                return Err(format!("chroot: {}", std::io::Error::last_os_error()));
            }
        }
        Ok(())
    };
    enter().unwrap_or_else(|e| fail!("bwrap", "{e}"));
    drop((user, parent, others, net, mnt, root, pidfd));

    // The terminal of the caller is on a devpts of the host, and the
    // container has a devpts of its own (--dev): in the container, ttyname()
    // does not find a name for the terminal of the caller (tty and GPG_TTY
    // fail). Thus, with a terminal, this process opens a terminal on the
    // /dev/ptmx of the container, after the chroot. The program gets it as
    // its controlling terminal in a new session, and this process copies the
    // bytes between the two terminals (refer to pty.rs).
    let term = p.tty.then(|| pty::open().unwrap_or_else(|e| fail!("bwrap", "a terminal in the container: {e}")));
    let slave = term.as_ref().map(|t| t.slave.as_raw_fd());

    // The program is in a new session: this process gives it the signals.
    // With a terminal, Ctrl-C comes as a byte, and the terminal of the
    // container gives the signal to the program.
    // The child sets the default handlers again before exec.
    catch_signals(&[libc::SIGINT, libc::SIGQUIT, libc::SIGTERM, libc::SIGHUP]);
    let env: Vec<(String, String)> = p.env.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
    let cwd = process::cstr(&p.cwd);
    // SAFETY: this process has one thread; the child only calls system calls
    // and execve before it replaces itself.
    let child = unsafe { libc::fork() };
    if child < 0 {
        fail!("bwrap", "fork: {}", std::io::Error::last_os_error());
    }
    if child == 0 {
        // SAFETY: system calls in the child.
        unsafe {
            // In the PID namespace of the container, the parent is not
            // visible (getppid is 0), so there is no test of the parent.
            libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL, 0, 0, 0);
            libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0);
            match slave {
                Some(fd) => {
                    if let Err(e) = pty::attach(fd) {
                        eprintln!("{}: the terminal of the container: {e}", prog("bwrap"));
                        libc::_exit(1);
                    }
                }
                None => {
                    libc::setsid();
                }
            }
            if libc::chdir(cwd.as_ptr()) != 0 {
                eprintln!("{}: {}: {}", prog("bwrap"), p.cwd, std::io::Error::last_os_error());
                libc::_exit(1);
            }
        }
        let err = process::exec_with_env(&p.command, &env);
        eprintln!("{}: {err}", prog("bwrap"));
        // SAFETY: exit the child without the handlers of the parent.
        unsafe { libc::_exit(127) };
    }
    match term {
        Some(t) => std::process::exit(pty::proxy(t, child)),
        None => std::process::exit(process::wait_child(child, &mut |s| process::kill(child, s))),
    }
}

/// The gVisor backend: runsc, with an OCI bundle from the plan.
///
/// gVisor gives the program a kernel in user space (the Sentry). Only a
/// small set of system calls goes to the kernel of the host. Limits: no
/// /dev/dri, /dev/snd, /dev/kvm or /dev/fuse (the backend ignores device
/// mounts); NVIDIA through nvproxy (SANDBOX_RUNSC_NVPROXY=1); its own /sys;
/// the program is root in gVisor; no Wayland or X11 client can draw.
fn gvisor(mode: &str, p: &Plan) -> ! {
    let b = "gvisor";
    // Not util::runtime_dir: in the user namespace of a private network the
    // user ID is 0.
    let runtime = util::files_dir();
    // The bundle (config.json, rootfs) and the state of runsc. For "run" and
    // "show", a directory for this run; for "start" and "exec", the state
    // directory of the container. "exec" uses a directory of its own for its
    // process.json.
    let (dir, temp) = match mode {
        "start" | "exec" => (state(b, p), false),
        _ => (mktemp_dir(&format!("{runtime}/sandbox-runsc-{}", p.name)).unwrap_or_else(|e| fail!(b, "{e}")), true),
    };
    let _ = fs::create_dir_all(format!("{dir}/rootfs"));
    let _ = fs::create_dir_all(format!("{dir}/state"));
    let work = if mode == "exec" {
        mktemp_dir(&format!("{runtime}/sandbox-runsc-exec-{}", p.name)).unwrap_or_else(|e| fail!(b, "{e}"))
    } else {
        dir.clone()
    };
    let cleanup = || {
        if temp {
            let _ = fs::remove_dir_all(&dir);
        }
        if mode == "exec" {
            let _ = fs::remove_dir_all(&work);
        }
    };
    if mode != "show" && mode != "exec" {
        for m in p.mounts.iter().filter(|m| m.kind == MountType::Dev) {
            eprintln!("{}: gVisor cannot use {}; the sandbox does not get it", prog(b), m.source);
        }
    }

    // With a terminal, runsc gives the terminal of the host to the program.
    // The terminal of the host then sends the signals of Ctrl-C and of a
    // resize to runsc, not to the program, and Ctrl-C stops the whole
    // sandbox. Thus `script` makes a terminal in gVisor and copies the bytes
    // between the two terminals. The terminal of gVisor sends the signals to
    // the program. The backend sends SIGWINCH to all processes of the
    // sandbox when the terminal of the host changes its size.
    let process_json = gvisor_process(p);
    if mode == "exec" {
        fs::write(format!("{work}/process.json"), process_json.to_string()).unwrap_or_else(|e| fail!(b, "{e}"));
    } else {
        let config = gvisor_config(p, process_json);
        fs::write(format!("{dir}/config.json"), serde_json::to_vec_pretty(&config).unwrap()).unwrap_or_else(|e| fail!(b, "{e}"));
    }

    let flags = runsc_flags(
        p,
        &dir,
        util::env("SANDBOX_RUNSC_PLATFORM").filter(|s| !s.is_empty()),
        util::env("SANDBOX_RUNSC_NVPROXY").is_some_and(|s| !s.is_empty()),
    );

    // A run has a container of its own. The container of a sandbox has one
    // name, and the state directory of the sandbox.
    let id = if mode == "start" || mode == "exec" { format!("sandbox-{}", p.name) } else { format!("sandbox-{}-{}", p.name, std::process::id()) };
    let runsc = |args: &[&str]| {
        let mut c = Command::new(tools::runsc());
        c.args(&flags).args(args);
        c
    };

    let mut cmd = match mode {
        "show" => {
            let mut a = vec![tools::runsc().to_string()];
            a.extend(flags.iter().cloned());
            a.extend(["run".into(), "--bundle".into(), dir.clone(), id.clone()]);
            println!("{}", shell_words(&a));
            println!("{}", serde_json::to_string_pretty(&serde_json::from_slice::<serde_json::Value>(&fs::read(format!("{dir}/config.json")).unwrap()).unwrap()).unwrap());
            cleanup();
            std::process::exit(0);
        }
        "exec" => runsc(&["exec", "--process", &format!("{work}/process.json"), "--internal-pid-file", &format!("{work}/pid"), &id]),
        _ => runsc(&["run", "--bundle", &dir, &id]),
    };
    if mode == "start" {
        let _ = fs::remove_file(format!("{dir}/ready"));
        cmd.stdin(Stdio::null());
    }
    // runsc stays in the process group of the terminal.
    catch_signals(&[libc::SIGTERM, libc::SIGHUP, libc::SIGWINCH]);
    let child = cmd.spawn().unwrap_or_else(|e| fail!(b, "runsc: {e}"));
    let pid = child.id() as libc::pid_t;

    // Stop the processes of this backend: all processes of the sandbox, or
    // for "exec" the process of the run (its PID in gVisor). runsc does not
    // stop the sandbox when it gets SIGTERM or SIGHUP. A sandbox that stays
    // uses CPU and memory, and no window or scope shows it.
    let stop = || {
        if mode == "exec" {
            if let Ok(ipid) = fs::read_to_string(format!("{work}/pid")) {
                let _ = runsc(&["kill", "--pid", ipid.trim(), &id, "SIGKILL"]).stderr(Stdio::null()).status();
            }
        } else {
            let _ = runsc(&["kill", "--all", &id, "SIGKILL"]).stderr(Stdio::null()).status();
        }
    };
    let handle = |sigs: Vec<libc::c_int>| {
        for s in sigs {
            if s == libc::SIGWINCH {
                if p.tty {
                    let _ = runsc(&["kill", "--all", &id, "SIGWINCH"]).stderr(Stdio::null()).status();
                }
            } else {
                stop();
            }
        }
    };

    if mode == "start" {
        // `exec` can start programs when the first process runs.
        let mut n = 0;
        loop {
            let out = runsc(&["state", &id]).stderr(Stdio::null()).output();
            let running = out
                .ok()
                .and_then(|o| serde_json::from_slice::<serde_json::Value>(&o.stdout).ok())
                .is_some_and(|v| v["status"] == "running");
            if running {
                break;
            }
            handle(take_signals());
            n += 1;
            if !process::alive(pid) || n >= 600 {
                eprintln!("{}: the container did not start", prog(b));
                stop();
                cleanup();
                std::process::exit(1);
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        let _ = fs::write(format!("{dir}/ready"), "");
    }

    let rc = process::wait_child(pid, &mut |s| handle(vec![s]));
    cleanup();
    std::process::exit(rc)
}

/// The command of a plan in gVisor. With a terminal, runsc gives the
/// terminal of the host to the program. The terminal of the host then sends
/// the signals of Ctrl-C and of a resize to runsc, not to the program, and
/// Ctrl-C stops the whole sandbox. Thus `script` makes a terminal in gVisor
/// and copies the bytes between the two terminals. The terminal of gVisor
/// sends the signals to the program. The backend sends SIGWINCH to all
/// processes of the sandbox when the terminal of the host changes its size.
fn gvisor_command(p: &Plan) -> Vec<String> {
    if !p.tty {
        return p.command.clone();
    }
    // `script -c` gives the command to $SHELL as one string. As process 1
    // of gVisor, `script` does not stop after the command, so a shell is
    // process 1. The "exit" prevents that the shell replaces itself with
    // `script`.
    vec![
        "/bin/sh".into(),
        "-c".into(),
        "\"$@\"; exit $?".into(),
        "sh".into(),
        tools::script().into(),
        "-qef".into(),
        "-c".into(),
        shell_words_always(&p.command),
        "/dev/null".into(),
    ]
}

/// The OCI process of a plan. The program runs as root (uid 0) in gVisor.
/// runsc --rootless maps that root to the user that starts runsc, so the
/// files of the program belong to the user on the host. A user ID other than
/// 0 in gVisor sees the files of the host as files of root.
fn gvisor_process(p: &Plan) -> serde_json::Value {
    json!({
        "terminal": p.tty,
        "user": {"uid": 0, "gid": 0},
        "args": gvisor_command(p),
        "env": p.env.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>(),
        "cwd": p.cwd,
        "capabilities": {"bounding": [], "effective": [], "permitted": [], "inheritable": [], "ambient": []},
        "rlimits": [{"type": "RLIMIT_NOFILE", "hard": 65536, "soft": 65536}],
        "noNewPrivileges": true,
    })
}

/// The OCI runtime configuration of a plan: an empty, read-only root, the
/// file systems of the kernel of gVisor, and the mounts of the plan without
/// the devices and without /sys (gVisor has its own).
fn gvisor_config(p: &Plan, process: serde_json::Value) -> serde_json::Value {
    let mut mounts = vec![
        json!({"destination": "/proc", "type": "proc", "source": "proc"}),
        json!({"destination": "/dev", "type": "tmpfs", "source": "tmpfs", "options": ["nosuid", "strictatime", "mode=755"]}),
        json!({"destination": "/dev/shm", "type": "tmpfs", "source": "shm", "options": ["nosuid", "nodev", "mode=1777"]}),
        json!({"destination": "/dev/pts", "type": "devpts", "source": "devpts",
               "options": ["nosuid", "noexec", "newinstance", "ptmxmode=0666", "mode=0620"]}),
        json!({"destination": "/sys", "type": "sysfs", "source": "sysfs", "options": ["nosuid", "noexec", "nodev", "ro"]}),
    ];
    for m in p.mounts.iter().filter(|m| m.kind != MountType::Dev && m.target != "/sys") {
        mounts.push(match m.kind {
            MountType::Bind => {
                let mut o = vec!["rbind", "rprivate"];
                if m.mode == "ro" {
                    o.push("ro");
                }
                json!({"destination": m.target, "type": "bind", "source": m.source, "options": o})
            }
            _ => json!({"destination": m.target, "type": "tmpfs", "source": "tmpfs",
                        "options": ["nosuid", "nodev", format!("mode={}", m.mode)]}),
        });
    }
    json!({
        "ociVersion": "1.0.2",
        "process": process,
        "root": {"path": "rootfs", "readonly": true},
        "hostname": p.hostname,
        "mounts": mounts,
        "linux": {"namespaces": [{"type": "pid"}, {"type": "ipc"}, {"type": "uts"}, {"type": "mount"}]},
    })
}

/// The flags of runsc for a plan, with the state in DIR/state.
fn runsc_flags(p: &Plan, dir: &str, platform: Option<String>, nvproxy: bool) -> Vec<String> {
    let mut flags: Vec<String> = vec![
        format!("--root={dir}/state"),
        "--rootless".into(),
        "--ignore-cgroups".into(),
        format!("--platform={}", platform.unwrap_or_else(|| "systrap".into())),
        // The root file system is an empty directory. The overlay in memory
        // takes the mount points.
        "--overlay2=root:memory".into(),
        // The program can connect to the sockets of the host that the plan
        // mounts (Wayland, SSH agent, Docker).
        "--host-uds=open".into(),
    ];
    // "inherit" uses the network namespace of the caller. `sandbox` makes
    // that namespace with slirp4netns and the firewall.
    flags.push(if p.network == "none" { "--network=none" } else { "--network=host" }.into());
    if nvproxy {
        flags.push("--nvproxy".into());
    }
    flags.extend(p.extra_args.iter().cloned());
    flags
}

/// The words of a command, each in quotes, for `script -c` (as @sh of jq).
fn shell_words_always(args: &[String]) -> String {
    args.iter().map(|a| format!("'{}'", a.replace('\'', "'\\''"))).collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plan::Mount;

    fn plan() -> Plan {
        Plan {
            version: 1,
            name: "s".into(),
            hostname: "box".into(),
            uid: 1000,
            gid: 100,
            home: "/home/a".into(),
            cwd: "/home/a/code".into(),
            tty: false,
            network: "inherit".into(),
            state: "/S".into(),
            mounts: vec![
                Mount { kind: MountType::Bind, source: "/nix/store".into(), target: "/nix/store".into(), mode: "ro".into() },
                Mount { kind: MountType::Bind, source: "/h/s".into(), target: "/home/a".into(), mode: "rw".into() },
                Mount { kind: MountType::Dev, source: "/dev/dri".into(), target: "/dev/dri".into(), mode: "rw".into() },
                Mount { kind: MountType::Tmpfs, source: String::new(), target: "/tmp".into(), mode: "1777".into() },
                Mount { kind: MountType::Bind, source: "/sys".into(), target: "/sys".into(), mode: "ro".into() },
            ],
            env: [("PATH".to_string(), "/bin".to_string()), ("A".to_string(), "x y".to_string())].into_iter().collect(),
            command: vec!["sh".into(), "-c".into(), "echo 'hi'".into()],
            extra_args: vec!["--cap-drop".into(), "ALL".into()],
        }
    }

    /// The position of `seq` in `a`, as a sequence of words.
    fn find(a: &[String], seq: &[&str]) -> Option<usize> {
        a.windows(seq.len()).position(|w| w.iter().zip(seq).all(|(x, y)| x == y))
    }

    #[test]
    fn bwrap_isolates_and_maps_the_user() {
        let a = bwrap_command(&plan(), false);
        assert_eq!(a[0], tools::bwrap());
        for f in ["--die-with-parent", "--unshare-user", "--unshare-ipc", "--unshare-pid", "--unshare-uts", "--unshare-cgroup-try", "--clearenv"] {
            assert!(a.iter().any(|x| x == f), "{f}");
        }
        assert!(find(&a, &["--uid", "1000", "--gid", "100"]).is_some());
        assert!(find(&a, &["--hostname", "box"]).is_some());
        assert!(find(&a, &["--proc", "/proc", "--dev", "/dev", "--tmpfs", "/dev/shm"]).is_some());
        assert!(!a.iter().any(|x| x == "--info-fd"));
    }

    #[test]
    fn bwrap_mounts_in_order() {
        let a = bwrap_command(&plan(), false);
        let p = [
            find(&a, &["--ro-bind", "/nix/store", "/nix/store"]).unwrap(),
            find(&a, &["--bind", "/h/s", "/home/a"]).unwrap(),
            find(&a, &["--dev-bind", "/dev/dri", "/dev/dri"]).unwrap(),
            find(&a, &["--perms", "1777", "--tmpfs", "/tmp"]).unwrap(),
            find(&a, &["--ro-bind", "/sys", "/sys"]).unwrap(),
        ];
        assert!(p.windows(2).all(|w| w[0] < w[1]), "{p:?}");
        // The base mounts of bwrap come first: a mount of the plan can cover
        // them.
        assert!(find(&a, &["--proc", "/proc"]).unwrap() < p[0]);
    }

    #[test]
    fn bwrap_environment_directory_and_command() {
        let a = bwrap_command(&plan(), false);
        let clear = a.iter().position(|x| x == "--clearenv").unwrap();
        assert!(find(&a, &["--setenv", "A", "x y"]).unwrap() > clear);
        assert!(find(&a, &["--setenv", "PATH", "/bin"]).unwrap() > clear);
        assert!(find(&a, &["--chdir", "/home/a/code"]).is_some());
        // The extra arguments come before the command, the command at the end.
        let dashes = a.iter().position(|x| x == "--").unwrap();
        assert!(find(&a, &["--cap-drop", "ALL"]).unwrap() < dashes);
        assert_eq!(&a[dashes + 1..], ["sh", "-c", "echo 'hi'"]);
    }

    #[test]
    fn bwrap_terminal_network_and_info() {
        let mut p = plan();
        assert!(bwrap_command(&p, false).iter().any(|x| x == "--new-session"));
        assert!(!bwrap_command(&p, false).iter().any(|x| x == "--unshare-net"));
        p.tty = true;
        p.network = "none".into();
        let a = bwrap_command(&p, true);
        // A shell on a terminal keeps the terminal for job control.
        assert!(!a.iter().any(|x| x == "--new-session"));
        assert!(a.iter().any(|x| x == "--unshare-net"));
        assert_eq!(&a[1..3], ["--info-fd", "3"]);
    }

    #[test]
    fn quoting() {
        assert_eq!(shell_quote("abc-1.2/x=y:z,+@%_"), "abc-1.2/x=y:z,+@%_");
        assert_eq!(shell_quote(""), "''");
        assert_eq!(shell_quote("a b"), "'a b'");
        assert_eq!(shell_quote("it's"), "'it'\\''s'");
        assert_eq!(shell_quote("$HOME"), "'$HOME'");
        assert_eq!(shell_words(&["a".into(), "b c".into()]), "a 'b c'");
        assert_eq!(shell_words_always(&["a".into(), "it's".into()]), "'a' 'it'\\''s'");
    }

    #[test]
    fn gvisor_command_with_and_without_a_terminal() {
        let mut p = plan();
        assert_eq!(gvisor_command(&p), p.command);
        p.tty = true;
        let c = gvisor_command(&p);
        assert_eq!(&c[..4], ["/bin/sh", "-c", "\"$@\"; exit $?", "sh"]);
        assert_eq!(c[4], tools::script());
        assert_eq!(&c[5..7], ["-qef", "-c"]);
        assert_eq!(c[7], "'sh' '-c' 'echo '\\''hi'\\'''");
        assert_eq!(c[8], "/dev/null");
    }

    #[test]
    fn gvisor_process_is_root_without_capabilities() {
        let v = gvisor_process(&plan());
        assert_eq!(v["user"], json!({"uid": 0, "gid": 0}));
        assert_eq!(v["noNewPrivileges"], true);
        for k in ["bounding", "effective", "permitted", "inheritable", "ambient"] {
            assert_eq!(v["capabilities"][k], json!([]), "{k}");
        }
        assert_eq!(v["cwd"], "/home/a/code");
        assert_eq!(v["env"], json!(["A=x y", "PATH=/bin"]));
        assert_eq!(v["terminal"], false);
    }

    #[test]
    fn gvisor_config_mounts() {
        let p = plan();
        let v = gvisor_config(&p, gvisor_process(&p));
        assert_eq!(v["root"], json!({"path": "rootfs", "readonly": true}));
        assert_eq!(v["hostname"], "box");
        let m = v["mounts"].as_array().unwrap();
        let dests: Vec<&str> = m.iter().map(|x| x["destination"].as_str().unwrap()).collect();
        // Devices are not in gVisor, and /sys is the one of gVisor.
        assert_eq!(dests, ["/proc", "/dev", "/dev/shm", "/dev/pts", "/sys", "/nix/store", "/home/a", "/tmp"]);
        assert_eq!(m[5]["options"], json!(["rbind", "rprivate", "ro"]));
        assert_eq!(m[6]["options"], json!(["rbind", "rprivate"]));
        assert_eq!(m[7], json!({"destination": "/tmp", "type": "tmpfs", "source": "tmpfs", "options": ["nosuid", "nodev", "mode=1777"]}));
        assert_eq!(m[4]["source"], "sysfs");
    }

    #[test]
    fn runsc_flags_for_the_network_and_the_options() {
        let mut p = plan();
        let f = runsc_flags(&p, "/S", None, false);
        assert_eq!(f[0], "--root=/S/state");
        assert!(f.contains(&"--rootless".to_string()));
        assert!(f.contains(&"--platform=systrap".to_string()));
        assert!(f.contains(&"--network=host".to_string()));
        assert!(!f.contains(&"--nvproxy".to_string()));
        assert_eq!(&f[f.len() - 2..], ["--cap-drop", "ALL"]);
        p.network = "none".into();
        let f = runsc_flags(&p, "/S", Some("ptrace".into()), true);
        assert!(f.contains(&"--network=none".to_string()));
        assert!(f.contains(&"--platform=ptrace".to_string()));
        assert!(f.contains(&"--nvproxy".to_string()));
    }
}
