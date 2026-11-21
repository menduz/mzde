//! The private network of a sandbox: a network namespace in a user
//! namespace, connected to the network of the host with slirp4netns, with a
//! firewall, an optional SOCKS proxy (tun2socks) and an optional OpenSnitch
//! daemon.

use serde::{Deserialize, Serialize};
use std::fs;
use std::io::{Read, Write};
use std::net::ToSocketAddrs;
use std::os::fd::AsRawFd;
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};

use crate::config::{Config, Env};
use crate::process::{self, catch_signals, take_signals};
use crate::tools;
use crate::util::{self, Result};
use crate::{die, warn};

/// The settings of the process in the namespace (DIR/net.json).
#[derive(Debug, Serialize, Deserialize)]
pub struct Settings {
    /// The addresses of allowHosts.
    pub ips: Vec<String>,
    pub internet: bool,
    pub lan: bool,
    /// socks5://HOST:PORT, or "".
    pub socks: String,
    pub proxy_host: String,
    /// The program of the backend.
    pub backend: String,
    pub name: String,
    pub opensnitch: bool,
    pub osn_daemon: String,
    pub osn_config: String,
    pub osn_rules: String,
    pub osn_dir: String,
}

impl Settings {
    pub fn new(config: &Config, name: &str, e: &Env, backend: &str) -> Settings {
        let mut ips: Vec<String> = Vec::new();
        for h in &e.allow_hosts {
            let found: Vec<String> = match (h.as_str(), 0).to_socket_addrs() {
                Ok(a) => a.map(|a| a.ip().to_string()).collect(),
                Err(_) => Vec::new(),
            };
            if found.is_empty() {
                warn!("cannot resolve {h}");
            }
            for ip in found {
                if !ips.contains(&ip) {
                    ips.push(ip);
                }
            }
        }
        // "HOST:PORT", as in sandboxed-agents, is a SOCKS5 proxy.
        let mut socks = e.socks_proxy.clone().unwrap_or_default();
        let mut proxy_host = String::new();
        if !socks.is_empty() {
            if !socks.contains("://") {
                socks = format!("socks5://{socks}");
            }
            let hostport = socks.split_once("://").map(|(_, r)| r).unwrap_or(&socks);
            let host = hostport.rsplit_once(':').map(|(h, _)| h).unwrap_or(hostport);
            proxy_host = host.trim_start_matches('[').trim_end_matches(']').to_string();
        }
        let osn = config.opensnitch.as_ref();
        Settings {
            ips,
            internet: e.internet_access,
            lan: e.lan_access,
            socks,
            proxy_host,
            backend: backend.into(),
            name: name.into(),
            opensnitch: e.opensnitch.enable,
            osn_daemon: osn.map(|o| o.daemon.clone()).unwrap_or_default(),
            osn_config: osn.map(|o| o.config.clone()).unwrap_or_default(),
            osn_rules: e.opensnitch.rules.clone().unwrap_or_default(),
            osn_dir: util::expand(&e.opensnitch.directory, &util::home()),
        }
    }
}

/// Run a program; an error if it fails.
fn run(prog: &str, args: &[&str]) -> Result<()> {
    let st = Command::new(prog).args(args).status().map_err(|e| format!("{prog}: {e}"))?;
    if st.success() { Ok(()) } else { Err(format!("{prog} {}: exit code {}", args.join(" "), process::code(st))) }
}

/// Run a program, and ignore an error.
fn run_quiet(prog: &str, args: &[&str]) {
    let _ = Command::new(prog).args(args).stderr(Stdio::null()).status();
}

/// Set up the network namespace (as root of its user namespace): the
/// loopback interface, the SOCKS proxy and the firewall.
/// One step of the setup of the network namespace.
#[derive(Debug, PartialEq, Eq)]
enum Step {
    /// Run a program; with `must`, a failure stops the setup.
    Run { prog: &'static str, args: Vec<String>, must: bool },
    /// Start tun2socks for the proxy.
    Tun2socks,
}

fn run_step(prog: &'static str, args: &[&str]) -> Step {
    Step::Run { prog, args: args.iter().map(|a| a.to_string()).collect(), must: true }
}

fn quiet_step(prog: &'static str, args: &[&str]) -> Step {
    Step::Run { prog, args: args.iter().map(|a| a.to_string()).collect(), must: false }
}

/// The setup of the network namespace, as steps: the loopback interface,
/// the SOCKS proxy and the firewall.
fn steps(s: &Settings) -> Vec<Step> {
    let ip = tools::ip();
    let mut out = vec![run_step(ip, &["link", "set", "lo", "up"])];
    let mut ips = s.ips.clone();
    if !s.socks.is_empty() {
        out.push(run_step(ip, &["tuntap", "add", "mode", "tun", "dev", "tun0"]));
        out.push(run_step(ip, &["addr", "add", "198.18.0.1/15", "dev", "tun0"]));
        out.push(run_step(ip, &["link", "set", "dev", "tun0", "up"]));
        out.push(quiet_step(ip, &["route", "add", &s.proxy_host, "via", "10.0.2.2", "dev", "tap0"]));
        out.push(run_step(ip, &["route", "replace", "default", "dev", "tun0"]));
        out.push(Step::Tun2socks);
        // The firewall must not stop the connections to the proxy.
        ips.push(s.proxy_host.clone());
    }

    let private4 = ["10.0.0.0/8", "172.16.0.0/12", "192.168.0.0/16", "100.64.0.0/10", "169.254.0.0/16"];
    let private6 = ["fc00::/7", "fe80::/10"];
    let fw = |out: &mut Vec<Step>, cmd: &'static str, allow: &[&str], deny: &[&str]| {
        out.push(run_step(cmd, &["-A", "OUTPUT", "-o", "lo", "-j", "ACCEPT"]));
        // An address of the other family fails: that is not an error.
        for n in &ips {
            out.push(quiet_step(cmd, &["-A", "OUTPUT", "-d", n, "-j", "ACCEPT"]));
        }
        for n in allow {
            out.push(run_step(cmd, &["-A", "OUTPUT", "-d", n, "-j", "ACCEPT"]));
        }
        for n in deny {
            out.push(run_step(cmd, &["-A", "OUTPUT", "-d", n, "-j", "REJECT"]));
        }
    };
    if !s.internet {
        // Only the local networks. 10.0.2.3 is the name server of
        // slirp4netns.
        fw(&mut out, tools::iptables(), &private4, &[]);
        fw(&mut out, tools::ip6tables(), &private6, &[]);
        out.push(run_step(tools::iptables(), &["-P", "OUTPUT", "DROP"]));
        out.push(run_step(tools::ip6tables(), &["-P", "OUTPUT", "DROP"]));
    } else if !s.lan {
        // Only the internet. The name server of slirp4netns stays open.
        fw(&mut out, tools::iptables(), &["10.0.2.3/32"], &private4);
        fw(&mut out, tools::ip6tables(), &[], &private6);
    }
    out
}

/// Set up the network namespace (as root of its user namespace). The result
/// is the PID of tun2socks, or 0.
fn setup(s: &Settings) -> Result<libc::pid_t> {
    // ping uses an ICMP socket without privileges (there is no setuid and no
    // cap_net_raw in a sandbox). A new network namespace allows that socket
    // for no group. Group 0 here is the group of the user on the host.
    // slirp4netns sends the echo requests on the host with the same socket
    // type.
    let _ = fs::write("/proc/sys/net/ipv4/ping_group_range", "0 0");
    let mut t2s = 0;
    for step in steps(s) {
        match step {
            Step::Run { prog, args, must } => {
                let args: Vec<&str> = args.iter().map(String::as_str).collect();
                if must {
                    run(prog, &args)?;
                } else {
                    run_quiet(prog, &args);
                }
            }
            Step::Tun2socks => {
                t2s = spawn_child(Command::new(tools::tun2socks()).args(["-device", "tun0", "-proxy", &s.socks]).stdout(Stdio::null()).stderr(Stdio::null()))?;
            }
        }
    }
    Ok(t2s)
}

/// Start a child that stops when this process stops.
fn spawn_child(cmd: &mut Command) -> Result<libc::pid_t> {
    // SAFETY: the closure only calls prctl.
    unsafe {
        cmd.pre_exec(|| {
            process::die_with_parent();
            Ok(())
        });
    }
    let child = cmd.spawn().map_err(|e| format!("{:?}: {e}", cmd.get_program()))?;
    Ok(child.id() as libc::pid_t)
}

/// The rules of the configuration are links into the store (`store_rules`,
/// a directory of JSON files). Remove the links of old rules from `rules`,
/// and then add the links of the rules of the current configuration. The
/// rules of the GUI are files, and they stay.
fn sync_rules(rules: &str, store_rules: &str) -> Result<()> {
    if let Ok(entries) = fs::read_dir(rules) {
        for e in entries.flatten() {
            if let Ok(target) = fs::read_link(e.path())
                && target.starts_with("/nix/store/")
            {
                let _ = fs::remove_file(e.path());
            }
        }
    }
    if store_rules.is_empty() {
        return Ok(());
    }
    let Ok(entries) = fs::read_dir(store_rules) else {
        return Ok(());
    };
    for e in entries.flatten() {
        let name = e.file_name();
        if !name.to_string_lossy().ends_with(".json") {
            continue;
        }
        let target = fs::canonicalize(e.path()).map_err(|err| format!("{:?}: {err}", e.path()))?;
        let link = format!("{rules}/{}", name.to_string_lossy());
        let _ = fs::remove_file(&link);
        std::os::unix::fs::symlink(target, &link).map_err(|err| format!("{link}: {err}"))?;
    }
    Ok(())
}

/// The configuration of a daemon of a sandbox: the configuration of the
/// module, with the name of the node (the GUI shows unix:/NAME), the log
/// file ("" is the standard output) and the directory of the rules.
fn opensnitch_config(mut conf: serde_json::Value, name: &str, log: Option<&str>, rules: &str) -> serde_json::Value {
    conf["Server"]["Name"] = name.into();
    conf["Server"]["LogFile"] = log.unwrap_or("").into();
    conf["Rules"]["Path"] = rules.into();
    conf
}

/// Start the OpenSnitch daemon of the sandbox in this network namespace.
/// The daemon sees each connection of the programs of the sandbox before
/// slirp4netns, and it asks the GUI of the host. With `log`, the daemon
/// writes to that file; else to the standard output.
///
/// The function returns when the queue rules of the daemon are in place, so
/// that no connection goes out before the daemon sees it.
fn opensnitch_start(s: &Settings, log: Option<&str>) -> Result<libc::pid_t> {
    let dir = &s.osn_dir;
    let rules = format!("{dir}/rules");
    fs::create_dir_all(&rules).map_err(|e| format!("cannot make {rules}: {e}"))?;
    use std::os::unix::fs::PermissionsExt;
    let _ = fs::set_permissions(dir, fs::Permissions::from_mode(0o700));

    sync_rules(&rules, &s.osn_rules)?;

    // The configuration of the module, with the name of the node, the rules
    // and the log of this sandbox. The GUI shows the node as unix:/NAME.
    let text = fs::read(&s.osn_config).map_err(|e| format!("cannot read {}: {e}", s.osn_config))?;
    let base: serde_json::Value = serde_json::from_slice(&text).map_err(|e| format!("{}: {e}", s.osn_config))?;
    let conf = opensnitch_config(base, &s.name, log, &rules);
    let conf_path = format!("{dir}/config.json");
    fs::write(&conf_path, serde_json::to_vec_pretty(&conf).unwrap()).map_err(|e| format!("{conf_path}: {e}"))?;

    // The daemon writes to the standard output until it reads the
    // configuration. For a run, the standard output is the terminal.
    let mut cmd = Command::new(&s.osn_daemon);
    cmd.args(["-config-file", &conf_path]).stdin(Stdio::null());
    if let Some(log) = log {
        let f = fs::OpenOptions::new().create(true).append(true).open(log).map_err(|e| format!("{log}: {e}"))?;
        cmd.stdout(f.try_clone().map_err(|e| e.to_string())?).stderr(f);
    }
    let pid = spawn_child(&mut cmd)?;

    for _ in 0..2000 {
        let out = Command::new(tools::nft())
            .args(["list", "chain", "inet", "opensnitch", "mangle_output"])
            .stderr(Stdio::null())
            .output();
        if let Ok(o) = out
            && String::from_utf8_lossy(&o.stdout).contains("queue")
        {
            return Ok(pid);
        }
        let mut status = 0;
        // SAFETY: status is a valid pointer.
        if unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) } == pid {
            return Err(format!("opensnitchd stopped{}", log.map(|l| format!("; refer to {l}")).unwrap_or_default()));
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    process::kill(pid, libc::SIGTERM);
    Err("opensnitchd did not set its queue rules in 20 s".into())
}

/// The steps that refuse (`on`) or allow again all connections of the
/// namespace. A daemon that stops removes its queue rules: without these
/// steps, the connections would go out without the daemon until it is back.
/// REJECT, not DROP: a program gets an error at once, and TCP does not send
/// the connection again after the end of the block.
fn block_steps(on: bool) -> Vec<Step> {
    let op = if on { "-I" } else { "-D" };
    let mut out = Vec::new();
    for cmd in [tools::iptables(), tools::ip6tables()] {
        let mut args = vec![op, "OUTPUT"];
        if on {
            args.push("1");
        }
        args.extend(["-j", "REJECT"]);
        out.push(run_step(cmd, &args));
    }
    out
}

/// Stop or allow again all connections of the namespace.
fn block(on: bool) -> Result<()> {
    for step in block_steps(on) {
        if let Step::Run { prog, args, .. } = step {
            run(prog, &args.iter().map(String::as_str).collect::<Vec<_>>())?;
        }
    }
    Ok(())
}

/// Read DIR/go (the frontend writes it when slirp4netns is ready) and
/// DIR/net.json.
fn inner_start(dir: &str) -> Settings {
    let mut go = fs::File::open(format!("{dir}/go")).unwrap_or_else(|e| die!("{dir}/go: {e}"));
    let mut buf = [0u8; 8];
    let _ = go.read(&mut buf);
    let text = fs::read(format!("{dir}/net.json")).unwrap_or_else(|e| die!("{dir}/net.json: {e}"));
    serde_json::from_slice(&text).unwrap_or_else(|e| die!("{dir}/net.json: {e}"))
}

/// `sandbox __netns DIR`: the process in the network namespace of a run
/// without a scope. It sets up the namespace, and then starts the backend.
pub fn cmd_netns(dir: &str) -> ! {
    let s = inner_start(dir);
    let t2s = setup(&s).unwrap_or_else(|e| die!("{e}"));
    let mut osn = 0;
    if s.opensnitch {
        osn = opensnitch_start(&s, Some(&format!("{}/opensnitchd.log", s.osn_dir))).unwrap_or_else(|e| die!("{e}"));
    }
    // TERM and HUP go to the backend, so that it stops its sandbox. If the
    // daemon stops, the run has no network until it stops: a run has no
    // service that starts the daemon again.
    let rc = match Command::new(&s.backend).args(["run", &format!("{dir}/plan.json")]).spawn() {
        Ok(child) => {
            let backend = child.id() as libc::pid_t;
            catch_signals(&[libc::SIGTERM, libc::SIGHUP]);
            loop {
                for sig in take_signals() {
                    process::kill(backend, sig);
                }
                let mut st = 0;
                // SAFETY: status is a valid pointer.
                let pid = unsafe { libc::waitpid(-1, &mut st, libc::WNOHANG) };
                if pid == backend {
                    break process::code_raw(st);
                }
                if pid == osn && osn > 0 {
                    osn = 0;
                    warn!("opensnitchd stopped: the sandbox has no network until the program stops");
                    if let Err(e) = block(true) {
                        warn!("{e}");
                        process::kill(backend, libc::SIGTERM);
                    }
                }
                if pid <= 0 {
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
            }
        }
        Err(e) => {
            warn!("{}: {e}", s.backend);
            1
        }
    };
    process::kill(osn, libc::SIGTERM);
    process::kill(t2s, libc::SIGTERM);
    std::process::exit(rc)
}

/// `sandbox __container_ns DIR`: the process in the network namespace of
/// the container (the service sandbox@NAME). It sets up the namespace,
/// starts the OpenSnitch daemon and then the container. If the daemon stops,
/// it starts the daemon again; if that fails, it stops the container. It
/// stops when the container stops.
pub fn cmd_container_ns(dir: &str) -> ! {
    let s = inner_start(dir);
    catch_signals(&[libc::SIGTERM, libc::SIGINT]);
    let t2s = setup(&s).unwrap_or_else(|e| die!("{e}"));
    let mut osn = 0;
    if s.opensnitch {
        osn = opensnitch_start(&s, None).unwrap_or_else(|e| die!("{e}"));
    }
    let backend = spawn_child(Command::new(&s.backend).args(["start", &format!("{dir}/plan.json")]).stdin(Stdio::null()))
        .unwrap_or_else(|e| die!("{e}"));
    loop {
        let w = process::wait_any();
        if !take_signals().is_empty() {
            // Stop the daemon, tun2socks and the container with this
            // process. (systemd stops all processes of the service, but a
            // manual start has no cgroup.)
            for p in [osn, t2s, backend] {
                process::kill(p, libc::SIGTERM);
            }
            std::process::exit(0);
        }
        match w {
            Some((pid, status)) if pid == backend => {
                process::kill(osn, libc::SIGTERM);
                process::kill(t2s, libc::SIGTERM);
                std::process::exit(process::code_raw(status));
            }
            Some((pid, _)) if pid == osn && osn > 0 => {
                // No connection goes out while there is no daemon.
                warn!("opensnitchd stopped: no connections until it is back");
                let blocked = block(true);
                if let Err(e) = &blocked {
                    warn!("{e}");
                }
                std::thread::sleep(std::time::Duration::from_secs(1));
                let started = opensnitch_start(&s, None).and_then(|p| {
                    if blocked.is_ok() {
                        block(false)?;
                    }
                    Ok(p)
                });
                match started {
                    Ok(p) => osn = p,
                    Err(e) => {
                        warn!("{e}");
                        process::kill(backend, libc::SIGTERM);
                        process::kill(t2s, libc::SIGTERM);
                        std::process::exit(1);
                    }
                }
            }
            Some((-1, _)) => std::process::exit(1),
            _ => {}
        }
    }
}

/// A network namespace with slirp4netns. `ns` is the process in the
/// namespace, `slirp` is slirp4netns.
pub struct Namespace {
    pub ns: libc::pid_t,
    pub slirp: libc::pid_t,
}

/// Start `sandbox WHAT DIR` in a new network namespace (in a new user
/// namespace, where the user is root), and connect the namespace to the
/// network of the host with slirp4netns. The settings go to DIR/net.json.
pub fn start(dir: &str, what: &str, settings: &Settings, host_loopback: bool, stdin: Stdio, pty: Option<&crate::pty::Pty>) -> Result<Namespace> {
    fs::write(format!("{dir}/net.json"), serde_json::to_vec(settings).unwrap()).map_err(|e| format!("{dir}/net.json: {e}"))?;
    let go = format!("{dir}/go");
    let go_c = process::cstr(&go);
    // SAFETY: a valid C string.
    if unsafe { libc::mkfifo(go_c.as_ptr(), 0o600) } != 0 {
        return Err(format!("{go}: {}", std::io::Error::last_os_error()));
    }

    // The maps of the new user namespace: the user is root in it, as
    // `unshare --map-root-user`. The child writes them itself, after
    // unshare and before exec; the data is ready before the fork.
    let uid_map = format!("0 {} 1", util::uid()).into_bytes();
    let gid_map = format!("0 {} 1", util::gid()).into_bytes();
    let mut cmd = Command::new(process::self_exe());
    cmd.args([what, dir]).stdin(stdin);
    let unshare = move || {
        // SAFETY: system calls only.
        unsafe {
            if libc::unshare(libc::CLONE_NEWUSER | libc::CLONE_NEWNET) != 0 {
                return Err(std::io::Error::last_os_error());
            }
        }
        write_file(c"/proc/self/setgroups", b"deny")?;
        write_file(c"/proc/self/uid_map", &uid_map)?;
        write_file(c"/proc/self/gid_map", &gid_map)?;
        Ok(())
    };
    let ns = match pty {
        Some(pty) => crate::pty::spawn(&mut cmd, pty, unshare).map_err(|e| format!("the network namespace did not start: {e}"))?,
        None => {
            // SAFETY: the closure calls only unshare, open, write and close.
            unsafe { cmd.pre_exec(unshare) };
            cmd.spawn().map_err(|e| format!("the network namespace did not start: {e}"))?.id() as libc::pid_t
        }
    };

    let (r, w) = process::pipe()?;
    let wfd = w.as_raw_fd();
    let mut slirp = Command::new(tools::slirp4netns());
    slirp.args(["--configure", "--mtu=65520"]);
    if !host_loopback {
        slirp.arg("--disable-host-loopback");
    }
    slirp.args(["--ready-fd=3", &ns.to_string(), "tap0"]).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    // SAFETY: the closure calls only dup2 and prctl.
    unsafe {
        slirp.pre_exec(move || {
            if libc::dup2(wfd, 3) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            process::die_with_parent();
            Ok(())
        });
    }
    let slirp_child = slirp.spawn().map_err(|e| format!("slirp4netns: {e}"))?;
    drop(w);
    // slirp4netns writes "1" when the interface is ready.
    let mut ready = fs::File::from(r);
    let mut one = [0u8; 1];
    if !(process::readable(ready.as_raw_fd(), 5000) && ready.read(&mut one).unwrap_or(0) == 1) {
        warn!("slirp4netns did not start: the sandbox has no network");
    }
    let mut f = fs::OpenOptions::new().write(true).open(&go).map_err(|e| format!("{go}: {e}"))?;
    let _ = f.write_all(b"go\n");
    Ok(Namespace {
        ns,
        slirp: slirp_child.id() as libc::pid_t,
    })
}

/// Write a file in a child before exec: only system calls.
fn write_file(path: &std::ffi::CStr, data: &[u8]) -> std::io::Result<()> {
    // SAFETY: path is a C string, data a valid buffer.
    unsafe {
        let fd = libc::open(path.as_ptr(), libc::O_WRONLY | libc::O_CLOEXEC);
        if fd < 0 {
            return Err(std::io::Error::last_os_error());
        }
        let n = libc::write(fd, data.as_ptr().cast(), data.len());
        libc::close(fd);
        if n != data.len() as isize {
            return Err(std::io::Error::last_os_error());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{config, config_top};
    use serde_json::json;

    fn settings(patch: serde_json::Value) -> Settings {
        let c = config_top(patch, json!({"opensnitch": {"daemon": "/o/opensnitchd", "config": "/o/config.json"}}));
        Settings::new(&c, "s", &c.environments["s"], "/b/bwrap")
    }

    /// The steps as "PROG ARGS" lines; a quiet step ends with " (quiet)".
    fn lines(s: &Settings) -> Vec<String> {
        steps(s)
            .into_iter()
            .map(|st| match st {
                Step::Run { prog, args, must } => {
                    let prog = prog.rsplit('/').next().unwrap();
                    format!("{prog} {}{}", args.join(" "), if must { "" } else { " (quiet)" })
                }
                Step::Tun2socks => "tun2socks".into(),
            })
            .collect()
    }

    #[test]
    fn socks_proxy_forms() {
        let s = settings(json!({"socksProxy": "10.0.2.2:1080"}));
        assert_eq!((s.socks.as_str(), s.proxy_host.as_str()), ("socks5://10.0.2.2:1080", "10.0.2.2"));
        let s = settings(json!({"socksProxy": "socks5h://proxy.lan:9050"}));
        assert_eq!((s.socks.as_str(), s.proxy_host.as_str()), ("socks5h://proxy.lan:9050", "proxy.lan"));
        let s = settings(json!({"socksProxy": "[fd00::1]:1080"}));
        assert_eq!((s.socks.as_str(), s.proxy_host.as_str()), ("socks5://[fd00::1]:1080", "fd00::1"));
        let s = settings(json!({}));
        assert_eq!((s.socks.as_str(), s.proxy_host.as_str()), ("", ""));
    }

    #[test]
    fn allow_hosts_are_addresses_without_duplicates() {
        let s = settings(json!({"allowHosts": ["203.0.113.10", "203.0.113.10", "2001:db8::1"]}));
        assert_eq!(s.ips, ["203.0.113.10", "2001:db8::1"]);
        // A name that does not resolve gives no address.
        let s = settings(json!({"allowHosts": ["does-not-exist.invalid"]}));
        assert!(s.ips.is_empty());
    }

    #[test]
    fn the_fields_for_the_namespace() {
        let s = settings(json!({"internetAccess": false, "lanAccess": false, "opensnitch": {"enable": true, "directory": "/d/osn", "rules": "/nix/store/r"}}));
        assert!(!s.internet && !s.lan && s.opensnitch);
        assert_eq!((s.osn_daemon.as_str(), s.osn_config.as_str()), ("/o/opensnitchd", "/o/config.json"));
        assert_eq!((s.osn_rules.as_str(), s.osn_dir.as_str()), ("/nix/store/r", "/d/osn"));
        assert_eq!((s.name.as_str(), s.backend.as_str()), ("s", "/b/bwrap"));
        // Without OpenSnitch in the configuration: no daemon.
        let c = config(json!({}));
        let s = Settings::new(&c, "s", &c.environments["s"], "/b/bwrap");
        assert_eq!(s.osn_daemon, "");
    }

    #[test]
    fn full_network_has_no_firewall() {
        assert_eq!(lines(&settings(json!({}))), ["ip link set lo up"]);
    }

    #[test]
    fn internet_only_rejects_the_local_networks() {
        let l = lines(&settings(json!({"lanAccess": false})));
        assert_eq!(
            l,
            [
                "ip link set lo up",
                "iptables -A OUTPUT -o lo -j ACCEPT",
                "iptables -A OUTPUT -d 10.0.2.3/32 -j ACCEPT",
                "iptables -A OUTPUT -d 10.0.0.0/8 -j REJECT",
                "iptables -A OUTPUT -d 172.16.0.0/12 -j REJECT",
                "iptables -A OUTPUT -d 192.168.0.0/16 -j REJECT",
                "iptables -A OUTPUT -d 100.64.0.0/10 -j REJECT",
                "iptables -A OUTPUT -d 169.254.0.0/16 -j REJECT",
                "ip6tables -A OUTPUT -o lo -j ACCEPT",
                "ip6tables -A OUTPUT -d fc00::/7 -j REJECT",
                "ip6tables -A OUTPUT -d fe80::/10 -j REJECT",
            ]
        );
    }

    #[test]
    fn local_only_drops_the_rest() {
        let l = lines(&settings(json!({"internetAccess": false, "allowHosts": ["203.0.113.10"]})));
        assert_eq!(l[1], "iptables -A OUTPUT -o lo -j ACCEPT");
        // The allowed hosts come before the policy, for both families (one
        // of them fails: quiet).
        assert!(l.contains(&"iptables -A OUTPUT -d 203.0.113.10 -j ACCEPT (quiet)".to_string()));
        assert!(l.contains(&"ip6tables -A OUTPUT -d 203.0.113.10 -j ACCEPT (quiet)".to_string()));
        assert!(l.contains(&"iptables -A OUTPUT -d 192.168.0.0/16 -j ACCEPT".to_string()));
        assert!(l.contains(&"ip6tables -A OUTPUT -d fe80::/10 -j ACCEPT".to_string()));
        assert!(!l.iter().any(|x| x.contains("REJECT")));
        assert_eq!(&l[l.len() - 2..], ["iptables -P OUTPUT DROP", "ip6tables -P OUTPUT DROP"]);
        // internetAccess = false wins over lanAccess = false.
        let l = lines(&settings(json!({"internetAccess": false, "lanAccess": false})));
        assert!(l.contains(&"iptables -P OUTPUT DROP".to_string()));
        assert!(!l.iter().any(|x| x.contains("REJECT")));
    }

    #[test]
    fn socks_proxy_routes_all_traffic_through_tun2socks() {
        let l = lines(&settings(json!({"socksProxy": "192.168.1.2:1080", "internetAccess": false})));
        assert_eq!(
            &l[..7],
            [
                "ip link set lo up",
                "ip tuntap add mode tun dev tun0",
                "ip addr add 198.18.0.1/15 dev tun0",
                "ip link set dev tun0 up",
                "ip route add 192.168.1.2 via 10.0.2.2 dev tap0 (quiet)",
                "ip route replace default dev tun0",
                "tun2socks",
            ]
        );
        // The firewall lets the connections to the proxy through.
        assert!(l.contains(&"iptables -A OUTPUT -d 192.168.1.2 -j ACCEPT (quiet)".to_string()));
    }

    #[test]
    fn block_and_allow_all_connections() {
        let l = |on| {
            block_steps(on)
                .into_iter()
                .map(|st| match st {
                    Step::Run { prog, args, must } => format!("{} {}{}", prog.rsplit('/').next().unwrap(), args.join(" "), if must { "" } else { " (quiet)" }),
                    Step::Tun2socks => "tun2socks".into(),
                })
                .collect::<Vec<_>>()
        };
        // First in OUTPUT: before the rules of the firewall.
        assert_eq!(l(true), ["iptables -I OUTPUT 1 -j REJECT", "ip6tables -I OUTPUT 1 -j REJECT"]);
        assert_eq!(l(false), ["iptables -D OUTPUT -j REJECT", "ip6tables -D OUTPUT -j REJECT"]);
    }

    #[test]
    fn opensnitch_config_names_the_node() {
        let base = json!({"Server": {"Address": "unix:///tmp/osui.sock", "Name": "x"}, "Rules": {"Path": "/etc"}, "DefaultAction": "allow"});
        let c = opensnitch_config(base.clone(), "web", Some("/d/log"), "/d/rules");
        assert_eq!(c["Server"], json!({"Address": "unix:///tmp/osui.sock", "Name": "web", "LogFile": "/d/log"}));
        assert_eq!(c["Rules"]["Path"], "/d/rules");
        assert_eq!(c["DefaultAction"], "allow");
        // No log: the standard output.
        assert_eq!(opensnitch_config(base, "web", None, "/d/rules")["Server"]["LogFile"], "");
    }

    #[test]
    fn rules_of_the_configuration_replace_the_old_links_and_keep_the_files_of_the_gui() {
        let d = std::env::temp_dir().join(format!("sandbox-rules-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        let rules = d.join("rules");
        let store = d.join("store");
        fs::create_dir_all(&rules).unwrap();
        fs::create_dir_all(&store).unwrap();
        fs::write(store.join("allow-a.json"), "{}").unwrap();
        fs::write(store.join("README"), "").unwrap();
        // An old rule of the configuration, a rule of the GUI, and a link of
        // the user (not into the store).
        std::os::unix::fs::symlink("/nix/store/old-rule.json", rules.join("old.json")).unwrap();
        fs::write(rules.join("gui.json"), "{}").unwrap();
        std::os::unix::fs::symlink("/elsewhere/mine.json", rules.join("mine.json")).unwrap();
        let (r, st) = (rules.to_str().unwrap(), store.to_str().unwrap());
        sync_rules(r, st).unwrap();
        let mut names: Vec<String> = fs::read_dir(&rules).unwrap().map(|e| e.unwrap().file_name().into_string().unwrap()).collect();
        names.sort();
        assert_eq!(names, ["allow-a.json", "gui.json", "mine.json"]);
        assert_eq!(fs::read_link(rules.join("allow-a.json")).unwrap(), fs::canonicalize(store.join("allow-a.json")).unwrap());
        // Again: the same result.
        sync_rules(r, st).unwrap();
        assert_eq!(fs::read_dir(&rules).unwrap().count(), 3);
        // No rules of the configuration: only the old links go.
        sync_rules(r, "").unwrap();
        assert!(rules.join("allow-a.json").exists());
        let _ = fs::remove_dir_all(&d);
    }
}
