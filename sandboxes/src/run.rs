//! `sandbox run` and the container of a sandbox (the user service
//! sandbox@NAME).
//!
//! A run in a scope starts its program in the container of the sandbox (as
//! `docker exec`): the scope requires the service, so systemd starts the
//! container before the scope, and stops it after the last scope. A run
//! without a scope (SANDBOX_NO_SCOPE=1) gets a container of its own.
//!
//! The directory of the container ($XDG_RUNTIME_DIR/sandboxes/NAME/container)
//! is only on the host:
//!
//!   plan.json     the plan of the container
//!   backend.path  the program of the backend
//!   config        the configuration file (after links) at the start
//!   resolv.conf   the name server of the private network
//!   net.json, go  the settings of the network namespace
//!   netns         "PID STARTTIME" of the process in the network namespace
//!   backend/      the state directory of the backend
//!   ready         the container can start programs

use serde_json::{Map, Value};
use std::collections::BTreeSet;
use std::fs;
use std::os::fd::{AsRawFd, OwnedFd};
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};

use crate::config::{Config, Net};
use crate::net;
use crate::plan::{self, Mode, MountType, Plan, Request};
use crate::process::{self, catch_signals, take_signals};
use crate::pty::{self, Pty};
use crate::systemd;
use crate::util::{self, Result};
use crate::{die, warn};

/// The directory of the sandbox in the runtime directory of the user.
pub fn sandbox_dir(name: &str) -> String {
    format!("{}/sandboxes/{name}", util::files_dir())
}

pub fn container_dir(name: &str) -> String {
    format!("{}/container", sandbox_dir(name))
}

/// The variables of the caller that the plan of the container reads, besides
/// passEnv (refer to plan.rs).
const CALLER_VARS: &[&str] = &[
    "HOME",
    "PATH",
    "XDG_RUNTIME_DIR",
    "WAYLAND_DISPLAY",
    "DISPLAY",
    "XAUTHORITY",
    "SSH_AUTH_SOCK",
    "DOCKER_HOST",
    "XDG_CONFIG_HOME",
    "XDG_DATA_HOME",
    "XDG_CACHE_HOME",
    "GNUPGHOME",
    "SANDBOX_CONFIG",
    "SANDBOX_BACKEND",
    "SANDBOX_RUNSC_PLATFORM",
    "SANDBOX_RUNSC_NVPROXY",
];

/// Make a directory with the mode 0700, if it is not there.
fn private_dir(p: &str) -> Result<()> {
    match fs::DirBuilder::new().mode(0o700).create(p) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(e) => Err(format!("cannot make {p}: {e}")),
    }
}

/// Write a file in one step: a reader does not see half of it.
fn write_atomic(path: &str, data: &[u8]) -> Result<()> {
    let tmp = format!("{path}.{}", std::process::id());
    fs::write(&tmp, data).map_err(|e| format!("cannot write {tmp}: {e}"))?;
    fs::rename(&tmp, path).map_err(|e| format!("cannot write {path}: {e}"))
}

/// The variables of the caller that the container needs: CALLER_VARS and
/// passEnv of the sandbox, if they are set.
fn caller_vars(config: &Config, name: &str, var: &dyn Fn(&str) -> Option<String>) -> Result<Map<String, Value>> {
    let e = config.env(name)?;
    let mut vars = Map::new();
    for k in CALLER_VARS.iter().copied().chain(e.pass_env.iter().map(String::as_str)) {
        if let Some(v) = var(k) {
            vars.insert(k.to_string(), Value::String(v));
        }
    }
    Ok(vars)
}

/// Write the variables of the caller that the container needs. The
/// container gets the environment of the run that starts it (the Wayland
/// socket, the SSH agent, the XDG directories), not the environment of
/// systemd.
fn write_caller_env(config: &Config, name: &str) -> Result<()> {
    let vars = caller_vars(config, name, &|k| std::env::var_os(k).map(|v| v.to_string_lossy().into_owned()))?;
    let base = format!("{}/sandboxes", util::files_dir());
    private_dir(&base)?;
    let dir = sandbox_dir(name);
    private_dir(&dir)?;
    write_atomic(&format!("{dir}/caller.json"), &serde_json::to_vec(&vars).unwrap())
}

/// Replace the environment of this process with the variables of
/// caller.json. Before threads: set_var is safe then.
fn load_caller_env(dir: &str) {
    let Ok(text) = fs::read(format!("{dir}/caller.json")) else {
        return;
    };
    let Ok(vars) = serde_json::from_slice::<Map<String, Value>>(&text) else {
        warn!("{dir}/caller.json is not valid");
        return;
    };
    for (k, _) in std::env::vars_os() {
        // SAFETY: this process has one thread.
        unsafe { std::env::remove_var(k) };
    }
    for (k, v) in vars {
        if let Value::String(v) = v {
            // SAFETY: this process has one thread.
            unsafe { std::env::set_var(k, v) };
        }
    }
}

/// `sandbox __container NAME`: the service sandbox@NAME (Type=notify). It
/// makes the network namespace (slirp4netns, the firewall, the OpenSnitch
/// daemon) and starts the backend with sandbox-init, which starts the
/// private X server and the session bus and stays. It tells systemd that it
/// is ready when the backend can start programs in the container.
pub fn cmd_container(args: &[String]) -> ! {
    if args.len() != 1 {
        die!("usage: sandbox __container NAME");
    }
    let name = &args[0];
    let notify = util::env("NOTIFY_SOCKET");
    load_caller_env(&sandbox_dir(name));
    let home = util::home();
    let _ = std::env::set_current_dir(&home);
    let config = Config::load().unwrap_or_else(|e| die!("{e}"));
    let e = config.env(name).unwrap_or_else(|e| die!("{e}"));

    let dir = container_dir(name);
    let _ = fs::remove_dir_all(&dir);
    private_dir(&sandbox_dir(name)).and_then(|_| private_dir(&dir)).unwrap_or_else(|e| die!("{e}"));
    fs::write(format!("{dir}/resolv.conf"), "nameserver 10.0.2.3\n").unwrap_or_else(|e| die!("{e}"));
    let config_path = util::env("SANDBOX_CONFIG").unwrap_or_else(|| crate::config::DEFAULT_PATH.into());
    let _ = fs::write(format!("{dir}/config"), util::readlink_f(&config_path));

    let state = format!("{dir}/backend");
    let built = plan::build(&Request {
        config: &config,
        name,
        mode: Mode::Container,
        resolv: format!("{dir}/resolv.conf"),
        state: state.clone(),
        command: Vec::new(),
    })
    .unwrap_or_else(|e| die!("{e}"));
    built.plan.write(&format!("{dir}/plan.json")).unwrap_or_else(|e| die!("{e}"));
    let backend = built.backend.1.clone();
    fs::write(format!("{dir}/backend.path"), &backend).unwrap_or_else(|e| die!("{e}"));

    // slirp4netns gets the label in its environment. A rule of the daemon of
    // the host can then find the slirp4netns of this sandbox (allowOnHost).
    // SAFETY: this process has one thread.
    unsafe {
        std::env::set_var("SANDBOX_NAME", name);
        std::env::set_var("SANDBOX_COLOR", &e.color);
    }

    catch_signals(&[libc::SIGTERM, libc::SIGINT]);
    let mut pids: Vec<libc::pid_t> = Vec::new();
    let mut netns = None;
    if built.net == Net::Private {
        let settings = net::Settings::new(&config, name, e, &backend);
        let ns = net::start(&dir, "__container_ns", &settings, e.allow_host_loopback, Stdio::null(), None).unwrap_or_else(|e| die!("{e}"));
        pids.extend([ns.ns, ns.slirp]);
        netns = Some(ns.ns);
    } else {
        let child = Command::new(&backend)
            .args(["start", &format!("{dir}/plan.json")])
            .stdin(Stdio::null())
            .spawn()
            .unwrap_or_else(|e| die!("{backend}: {e}"));
        pids.push(child.id() as libc::pid_t);
    }
    let stop = |pids: &[libc::pid_t], dir: &str| {
        for &p in pids {
            process::kill(p, libc::SIGTERM);
        }
        let _ = fs::remove_dir_all(dir);
    };

    let mut ready = false;
    for _ in 0..3000 {
        if !take_signals().is_empty() {
            stop(&pids, &dir);
            std::process::exit(0);
        }
        if util::exists(&format!("{state}/ready")) {
            ready = true;
            break;
        }
        if let Some(&p) = pids.iter().find(|&&p| {
            let mut st = 0;
            // SAFETY: status is a valid pointer.
            unsafe { libc::waitpid(p, &mut st, libc::WNOHANG) == p }
        }) {
            stop(&pids, &dir);
            die!("the container of {name} stopped (process {p})");
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    if !ready {
        stop(&pids, &dir);
        die!("the container of {name} was not ready in 30 s");
    }
    // A run in a private network enters the network namespace of this
    // process first.
    if let Some(ns) = netns {
        let start = util::starttime(ns).unwrap_or_default();
        fs::write(format!("{dir}/netns"), format!("{ns} {start}\n")).unwrap_or_else(|e| die!("{e}"));
    }
    fs::write(format!("{dir}/ready"), "").unwrap_or_else(|e| die!("{e}"));
    systemd::notify_ready(notify.as_deref());
    println!("sandbox: the container of {name} is ready");

    // Stop when the container, the namespace process or slirp4netns stops.
    loop {
        let w = process::wait_any();
        if !take_signals().is_empty() {
            stop(&pids, &dir);
            std::process::exit(0);
        }
        match w {
            Some((p, status)) if pids.contains(&p) => {
                stop(&pids, &dir);
                die!("the container of {name} stopped (exit code {})", process::code_raw(status));
            }
            Some((-1, _)) => {
                stop(&pids, &dir);
                die!("the container of {name} stopped");
            }
            _ => {}
        }
    }
}

/// The comparison of the mounts of a run with the mounts of the container.
/// The access of the container is smaller or the same: it can only warn.
/// The access of the container is larger (a mount that the run does not
/// have, rw that is now ro, a different source): the run stops, because the
/// configuration of the sandbox gives less access now.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Difference {
    /// The targets where the container has more access.
    pub larger: Vec<String>,
    /// The other targets that differ.
    pub other: Vec<String>,
}

/// Compare the mounts of the container `c` and of a run `r`, by target (the
/// last mount on a target counts). A tmpfs of the container is no access to
/// the host. A mount of a socket comes from the environment of the caller
/// (the Wayland socket, the SSH agent): a different socket is not a larger
/// access. A different store path (after a rebuild) is not a larger access
/// either.
pub fn compare(c: &Plan, r: &Plan, is_socket: &dyn Fn(&str) -> bool) -> Difference {
    let mut larger = BTreeSet::new();
    let mut other = BTreeSet::new();
    let last = |p: &'_ Plan, target: &str| p.mounts.iter().rev().find(|n| n.target == target).cloned();
    let mut targets: Vec<&str> = c.mounts.iter().map(|m| m.target.as_str()).collect();
    targets.dedup();
    for target in targets {
        let m = last(c, target).unwrap();
        let n = last(r, target);
        if n.as_ref() == Some(&m) {
            continue;
        }
        let more = match &n {
            _ if m.kind == MountType::Tmpfs || is_socket(&m.source) => false,
            None => true,
            Some(n) if n.kind == MountType::Tmpfs => true,
            Some(n) => {
                let store = m.source.starts_with("/nix/store/") && n.source.starts_with("/nix/store/");
                (m.mode == "rw" && n.mode == "ro") || (m.source != n.source && !store)
            }
        };
        if more {
            larger.insert(target.to_string());
        } else {
            other.insert(target.to_string());
        }
    }
    for n in &r.mounts {
        if !c.mounts.iter().any(|m| m.target == n.target) {
            other.insert(n.target.clone());
        }
    }
    Difference {
        larger: larger.into_iter().collect(),
        other: other.into_iter().collect(),
    }
}

/// Whether a run in a scope starts a scope now: not with SANDBOX_NO_SCOPE,
/// not in a scope, and only with a systemd user manager.
fn use_scope() -> bool {
    use_scope_from(&|k| util::env(k), &util::is_socket)
}

fn use_scope_from(var: &dyn Fn(&str) -> Option<String>, is_socket: &dyn Fn(&str) -> bool) -> bool {
    let set = |k: &str| var(k).is_some_and(|v| !v.is_empty());
    !set("SANDBOX_NO_SCOPE")
        && !set("SANDBOX__SCOPED")
        && is_socket(&format!("{}/systemd/private", var("XDG_RUNTIME_DIR").unwrap_or_else(|| "/nonexistent".into())))
}

/// "PID STARTTIME" of a state file.
pub fn parse_pid_file(text: &str) -> Option<(libc::pid_t, String)> {
    let mut it = text.split_whitespace();
    let pid = it.next()?.parse().ok().filter(|&p: &libc::pid_t| p > 0)?;
    let start = it.next()?.to_string();
    if it.next().is_some() || !start.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    Some((pid, start))
}

/// The namespaces of the process in the network namespace of the
/// container: its user namespace and its network namespace.
fn netns_fds(dir: &str) -> Result<Option<(OwnedFd, OwnedFd)>> {
    let Ok(text) = fs::read_to_string(format!("{dir}/netns")) else {
        return Ok(None);
    };
    let (pid, start) = parse_pid_file(&text).ok_or("the file netns is not valid")?;
    let pidfd = process::pidfd_checked(pid, &start).map_err(|_| "the network of the container stopped".to_string())?;
    let user = open_ns(pid, "user")?;
    let net = open_ns(pid, "net")?;
    if process::pidfd_exited(&pidfd) {
        return Err("the network of the container stopped".into());
    }
    Ok(Some((user, net)))
}

/// A namespace of a process.
pub fn open_ns(pid: libc::pid_t, ns: &str) -> Result<OwnedFd> {
    let p = format!("/proc/{pid}/ns/{ns}");
    fs::File::open(&p).map(OwnedFd::from).map_err(|e| format!("{p}: {e}"))
}

/// Start `backend exec PLAN` in the network namespace of the container.
fn exec_joined(backend: &str, plan: &str, ns: Option<(OwnedFd, OwnedFd)>, pty: Option<Pty>) -> Result<i32> {
    let mut cmd = Command::new(backend);
    cmd.args(["exec", plan]);
    let fds = ns.as_ref().map(|(u, n)| (u.as_raw_fd(), n.as_raw_fd()));
    // The namespace has no setgroups, so the credentials stay: the user is
    // root in the namespace.
    let enter = move || {
        if let Some((u, n)) = fds {
            // SAFETY: setns with descriptors that stay open until the spawn.
            unsafe {
                if libc::setns(u, libc::CLONE_NEWUSER) != 0 || libc::setns(n, libc::CLONE_NEWNET) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
            }
        }
        Ok(())
    };
    start_backend(cmd, pty, enter)
}

/// Start a backend and wait for it. With a pty, the backend gets the slave,
/// and this process copies the bytes of the terminal (refer to pty.rs).
fn start_backend(mut cmd: Command, pty: Option<Pty>, before: impl Fn() -> std::io::Result<()> + Send + Sync + 'static) -> Result<i32> {
    let prog = cmd.get_program().to_string_lossy().into_owned();
    match pty {
        Some(pty) => {
            let child = pty::spawn(&mut cmd, &pty, before)?;
            Ok(pty::proxy(pty, child))
        }
        None => {
            // SAFETY: the closure calls only system calls.
            unsafe { cmd.pre_exec(before) };
            let child = cmd.spawn().map_err(|e| format!("{prog}: {e}"))?;
            Ok(process::wait_forwarding(child.id() as libc::pid_t))
        }
    }
}

pub fn cmd_run(args: &[String]) -> ! {
    let mut args = args.to_vec();
    let need_cwd = args.first().map(String::as_str) == Some("--need-cwd");
    if need_cwd {
        args.remove(0);
    }
    if args.is_empty() {
        die!("usage: sandbox run [--need-cwd] NAME [--] [CMD...]");
    }
    let name = args.remove(0);
    if args.first().map(String::as_str) == Some("--") {
        args.remove(0);
    }
    if args.is_empty() {
        args.push(util::env("SHELL").unwrap_or_else(|| "/bin/sh".into()));
    }
    let config = Config::load().unwrap_or_else(|e| die!("{e}"));
    config.env(&name).unwrap_or_else(|e| die!("{e}"));

    // Put each run in a scope below sandbox-NAME.slice. A scope keeps the
    // environment of the caller. The run requires the container of the
    // sandbox: systemd starts sandbox@NAME before the scope, and stops it
    // after the last scope. The container gets the environment of this run.
    if use_scope() {
        write_caller_env(&config, &name).unwrap_or_else(|e| die!("{e}"));
        let mut inner = vec![process::self_exe(), "run".into()];
        if need_cwd {
            inner.push("--need-cwd".into());
        }
        inner.push(name.clone());
        inner.push("--".into());
        inner.extend(args.iter().cloned());
        let err = systemd::exec_scope(
            &config,
            &format!("sandbox-{name}.slice"),
            &format!("sandbox-{name}-{}-{}", std::process::id(), process::random15()),
            &format!("sandbox {name}: {}", args.join(" ")),
            &[format!("Requires=sandbox@{name}.service"), format!("After=sandbox@{name}.service")],
            &inner,
        );
        die!("{err}");
    }

    let e = config.env(&name).unwrap();
    let tmp_base = util::files_dir();
    let tmp = mktemp_dir(&format!("{tmp_base}/sandbox-{name}")).unwrap_or_else(|e| die!("{e}"));
    let cleanup = |tmp: &str| {
        if e.clean_tmp {
            let _ = fs::remove_dir_all(tmp);
        } else {
            warn!("the files of the run are in {tmp} (cleanTmp is false)");
        }
    };

    // A run in a scope starts its program in the container of the sandbox.
    // A run without a scope has no service: it gets a container of its own.
    let scoped = util::env("SANDBOX__SCOPED").filter(|v| !v.is_empty()).is_some();
    let cdir = container_dir(&name);
    let req = if scoped {
        if !util::exists(&format!("{cdir}/ready")) {
            let _ = fs::remove_dir_all(&tmp);
            die!("the container of {name} is not ready; refer to: journalctl --user -u sandbox@{name}");
        }
        Request {
            config: &config,
            name: &name,
            mode: Mode::Join,
            resolv: format!("{cdir}/resolv.conf"),
            state: format!("{cdir}/backend"),
            command: args.clone(),
        }
    } else {
        fs::write(format!("{tmp}/resolv.conf"), "nameserver 10.0.2.3\n").unwrap_or_else(|e| die!("{e}"));
        Request {
            config: &config,
            name: &name,
            mode: Mode::Run,
            resolv: format!("{tmp}/resolv.conf"),
            state: String::new(),
            command: args.clone(),
        }
    };
    let built = match plan::build(&req) {
        Ok(b) => b,
        Err(err) => {
            let _ = fs::remove_dir_all(&tmp);
            die!("{err}");
        }
    };
    if need_cwd && !built.has_cwd {
        let _ = fs::remove_dir_all(&tmp);
        die!("the sandbox {name} does not have the current directory {}", util::logical_pwd());
    }
    let plan_path = format!("{tmp}/plan.json");
    built.plan.write(&plan_path).unwrap_or_else(|e| die!("{e}"));

    // The label is also in the environment of the backend and of
    // slirp4netns. The OpenSnitch rule of the host (allowOnHost) finds
    // slirp4netns by SANDBOX_NAME. A window manager reads the cgroup, not
    // this environment.
    // SAFETY: this process has one thread.
    unsafe {
        std::env::set_var("SANDBOX_NAME", &name);
        std::env::set_var("SANDBOX_COLOR", &e.color);
    }

    // A run with a terminal gets a terminal of its own (refer to pty.rs).
    let term = if built.plan.tty {
        match pty::open() {
            Ok(t) => Some(t),
            Err(err) => {
                cleanup(&tmp);
                die!("{err}");
            }
        }
    } else {
        None
    };

    // Ctrl-C goes to the program in the sandbox. This process stays to
    // remove the files of the run after the program. A handler, not
    // SIG_IGN: an ignored signal stays ignored in the programs that this
    // process starts.
    catch_signals(&[libc::SIGINT]);
    let rc = if scoped {
        match run_joined(&name, &cdir, &built.plan, &plan_path, term) {
            Ok(rc) => rc,
            Err(err) => {
                cleanup(&tmp);
                die!("{err}");
            }
        }
    } else if built.net == Net::Private {
        let settings = net::Settings::new(&config, &name, e, &built.backend.1);
        match net::start(&tmp, "__netns", &settings, e.allow_host_loopback, Stdio::inherit(), term.as_ref()) {
            Ok(ns) => {
                let rc = match term {
                    Some(t) => pty::proxy(t, ns.ns),
                    None => process::wait_forwarding(ns.ns),
                };
                process::kill(ns.slirp, libc::SIGTERM);
                rc
            }
            Err(err) => {
                warn!("{err}");
                1
            }
        }
    } else {
        let mut cmd = Command::new(&built.backend.1);
        cmd.args(["run", &plan_path]);
        match start_backend(cmd, term, || Ok(())) {
            Ok(rc) => rc,
            Err(err) => {
                warn!("{err}");
                1
            }
        }
    };
    cleanup(&tmp);
    std::process::exit(rc)
}

/// Start the program of the plan in the container of the sandbox.
fn run_joined(name: &str, cdir: &str, plan: &Plan, plan_path: &str, term: Option<Pty>) -> Result<i32> {
    let backend = fs::read_to_string(format!("{cdir}/backend.path")).map_err(|e| format!("{cdir}/backend.path: {e}"))?;
    let container = Plan::read(&format!("{cdir}/plan.json"))?;
    let d = compare(&container, plan, &|p| util::is_socket(p));
    if !d.larger.is_empty() {
        return Err(format!(
            "the container of {name} has more access than its configuration gives now ({}). 'sandbox stop {name}' stops it and its programs; the next run starts it again.",
            d.larger.join(", ")
        ));
    }
    if !d.other.is_empty() {
        warn!(
            "the container of {name} started with different mounts ({}): a different configuration or environment. 'sandbox stop {name}' stops it and its programs; the next run starts it again.",
            d.other.join(", ")
        );
    }
    let ns = netns_fds(cdir)?;
    exec_joined(backend.trim_end(), plan_path, ns, term)
}

/// A new directory PREFIX.XXXXXX with the mode 0700.
pub fn mktemp_dir(prefix: &str) -> Result<String> {
    let mut t = process::cstr(&format!("{prefix}.XXXXXX")).into_bytes_with_nul();
    // SAFETY: t is a NUL-terminated template.
    let p = unsafe { libc::mkdtemp(t.as_mut_ptr().cast()) };
    if p.is_null() {
        return Err(format!("cannot make {prefix}.XXXXXX: {}", std::io::Error::last_os_error()));
    }
    t.pop();
    let s = String::from_utf8(t).map_err(|e| e.to_string())?;
    let _ = fs::set_permissions(&s, fs::Permissions::from_mode(0o700));
    Ok(s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plan::Mount;

    fn plan(mounts: Vec<Mount>) -> Plan {
        Plan {
            version: 1,
            name: "a".into(),
            hostname: "a".into(),
            uid: 1,
            gid: 1,
            home: "/h".into(),
            cwd: "/h".into(),
            tty: false,
            network: "inherit".into(),
            state: String::new(),
            mounts,
            env: Default::default(),
            command: vec![],
            extra_args: vec![],
        }
    }

    fn bind(src: &str, dst: &str, mode: &str) -> Mount {
        Mount {
            kind: MountType::Bind,
            source: src.into(),
            target: dst.into(),
            mode: mode.into(),
        }
    }

    #[test]
    fn same_mounts() {
        let p = plan(vec![bind("/a", "/a", "rw")]);
        assert_eq!(compare(&p, &p, &|_| false), Difference::default());
    }

    #[test]
    fn removed_path_is_larger() {
        let c = plan(vec![bind("/a", "/a", "rw"), bind("/s", "/s", "ro")]);
        let r = plan(vec![bind("/a", "/a", "rw")]);
        assert_eq!(compare(&c, &r, &|_| false).larger, vec!["/s"]);
    }

    #[test]
    fn rw_to_ro_is_larger() {
        let c = plan(vec![bind("/a", "/a", "rw")]);
        let r = plan(vec![bind("/a", "/a", "ro")]);
        assert_eq!(compare(&c, &r, &|_| false).larger, vec!["/a"]);
        // The other way: the container has less access.
        let d = compare(&r, &c, &|_| false);
        assert!(d.larger.is_empty());
        assert_eq!(d.other, vec!["/a"]);
    }

    #[test]
    fn store_paths_and_sockets_only_warn() {
        let c = plan(vec![bind("/nix/store/x-a", "/h/.f", "ro"), bind("/run/1/agent", "/run/1/agent", "rw")]);
        let r = plan(vec![bind("/nix/store/y-a", "/h/.f", "ro"), bind("/run/1/other", "/run/1/other", "rw")]);
        let d = compare(&c, &r, &|p| p.starts_with("/run/1/"));
        assert!(d.larger.is_empty(), "{d:?}");
        assert_eq!(d.other, vec!["/h/.f", "/run/1/agent", "/run/1/other"]);
    }

    #[test]
    fn bind_on_tmpfs_only_warns() {
        let tmp = Mount {
            kind: MountType::Tmpfs,
            source: String::new(),
            target: "/tmp".into(),
            mode: "1777".into(),
        };
        let c = plan(vec![tmp.clone()]);
        let r = plan(vec![tmp.clone(), bind("/tmp", "/tmp", "rw")]);
        let d = compare(&c, &r, &|_| false);
        assert!(d.larger.is_empty(), "{d:?}");
        assert_eq!(d.other, vec!["/tmp"]);
        // A bind that becomes a tmpfs: the container has more access.
        assert_eq!(compare(&r, &c, &|_| false).larger, vec!["/tmp"]);
    }

    #[test]
    fn caller_vars_are_only_the_variables_of_the_plan() {
        let c = crate::testutil::config(serde_json::json!({"passEnv": ["LANG"]}));
        let env: std::collections::BTreeMap<&str, &str> =
            [("HOME", "/h"), ("WAYLAND_DISPLAY", "wayland-1"), ("LANG", "C"), ("GITHUB_TOKEN", "secret"), ("SSH_AUTH_SOCK", "/a")].into_iter().collect();
        let v = caller_vars(&c, "s", &|k| env.get(k).map(|v| v.to_string())).unwrap();
        let keys: Vec<&str> = v.keys().map(String::as_str).collect();
        assert_eq!(keys, ["HOME", "LANG", "SSH_AUTH_SOCK", "WAYLAND_DISPLAY"]);
        assert!(caller_vars(&c, "nope", &|_| None).is_err());
    }

    #[test]
    fn a_scope_only_with_a_user_manager() {
        let sock = |p: &str| p == "/run/user/1000/systemd/private";
        let vars = |pairs: &'static [(&'static str, &'static str)]| move |k: &str| pairs.iter().find(|(n, _)| *n == k).map(|(_, v)| v.to_string());
        assert!(use_scope_from(&vars(&[("XDG_RUNTIME_DIR", "/run/user/1000")]), &sock));
        assert!(!use_scope_from(&vars(&[("XDG_RUNTIME_DIR", "/run/user/1000"), ("SANDBOX_NO_SCOPE", "1")]), &sock));
        assert!(!use_scope_from(&vars(&[("XDG_RUNTIME_DIR", "/run/user/1000"), ("SANDBOX__SCOPED", "1")]), &sock));
        // An empty value is no value.
        assert!(use_scope_from(&vars(&[("XDG_RUNTIME_DIR", "/run/user/1000"), ("SANDBOX_NO_SCOPE", "")]), &sock));
        assert!(!use_scope_from(&vars(&[]), &sock));
        assert!(!use_scope_from(&vars(&[("XDG_RUNTIME_DIR", "/run/user/2000")]), &sock));
    }

    #[test]
    fn pid_files() {
        assert_eq!(parse_pid_file("123 4567\n"), Some((123, "4567".to_string())));
        assert_eq!(parse_pid_file("123"), None);
        assert_eq!(parse_pid_file("0 1"), None);
        assert_eq!(parse_pid_file("-1 1"), None);
        assert_eq!(parse_pid_file("x 1"), None);
        assert_eq!(parse_pid_file("1 2 3"), None);
        assert_eq!(parse_pid_file("1 2x"), None);
        assert_eq!(parse_pid_file(""), None);
    }

    #[test]
    fn temporary_directories_and_atomic_files() {
        let base = std::env::temp_dir().join(format!("sandbox-run-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&base).unwrap();
        let base = base.to_str().unwrap().to_string();
        let a = mktemp_dir(&format!("{base}/x")).unwrap();
        let b = mktemp_dir(&format!("{base}/x")).unwrap();
        assert_ne!(a, b);
        assert!(a.starts_with(&format!("{base}/x.")) && a.len() == base.len() + 9);
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(fs::metadata(&a).unwrap().permissions().mode() & 0o777, 0o700);
        assert!(mktemp_dir(&format!("{base}/missing/x")).is_err());
        let f = format!("{base}/f.json");
        write_atomic(&f, b"one").unwrap();
        write_atomic(&f, b"two").unwrap();
        assert_eq!(fs::read_to_string(&f).unwrap(), "two");
        // No temporary file stays.
        assert_eq!(fs::read_dir(&base).unwrap().count(), 3);
        private_dir(&format!("{base}/p")).unwrap();
        private_dir(&format!("{base}/p")).unwrap();
        assert_eq!(fs::metadata(format!("{base}/p")).unwrap().permissions().mode() & 0o777, 0o700);
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn new_mount_only_warns() {
        let c = plan(vec![]);
        let r = plan(vec![bind("/a", "/a", "rw")]);
        let d = compare(&c, &r, &|_| false);
        assert!(d.larger.is_empty());
        assert_eq!(d.other, vec!["/a"]);
    }
}
