#![recursion_limit = "512"]
//! The bwrap backend with real sandboxes: run, start and exec. The tests do
//! not run where bwrap cannot make a sandbox (refer to bwrap_works).

mod common;
use common::*;
use serde_json::{Value, json};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// A plan without a network, with the base system and a tmpfs on /tmp.
fn plan(d: &Dir, command: &[&str]) -> Value {
    let mut mounts = vec![json!({"type": "bind", "source": "/nix/store", "target": "/nix/store", "mode": "ro"})];
    for p in ["/bin", "/usr", "/etc", "/run/current-system"] {
        if std::path::Path::new(p).exists() {
            mounts.push(json!({"type": "bind", "source": p, "target": p, "mode": "ro"}));
        }
    }
    mounts.push(json!({"type": "tmpfs", "source": "", "target": "/tmp", "mode": "1777"}));
    json!({
        "version": 1, "name": "t", "hostname": "box",
        "uid": unsafe { libc::getuid() }, "gid": unsafe { libc::getgid() },
        "home": "/tmp", "cwd": "/tmp", "tty": false, "network": "none",
        "state": d.path("state"),
        "mounts": mounts,
        "env": {"PATH": std::env::var("PATH").unwrap(), "FOO": "a b"},
        "command": command, "extraArgs": [],
    })
}

fn write(d: &Dir, name: &str, plan: &Value) -> String {
    let p = d.path(name);
    std::fs::write(&p, serde_json::to_vec(plan).unwrap()).unwrap();
    p
}

fn backend(d: &Dir, mode: &str, plan: &str) -> Command {
    let mut c = Command::new(d.path("bin/sandbox-backend-bwrap"));
    c.args([mode, plan]).env("SANDBOX_BACKEND_TEST_SECRET", "x").stdin(Stdio::null());
    c
}

fn sh(d: &Dir, script: &str) -> (i32, String, String) {
    let p = write(d, "run.json", &plan(d, &["sh", "-c", script]));
    let o = backend(d, "run", &p).output().unwrap();
    (o.status.code().unwrap_or(-1), stdout(&o), stderr(&o))
}

/// A container of `start` that stops with the test.
struct Container(Child);

impl Drop for Container {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn start(d: &Dir) -> Container {
    let p = write(d, "container.json", &plan(d, &["sleep", "60"]));
    let child = backend(d, "start", &p).spawn().unwrap();
    let t = Instant::now();
    while !std::path::Path::new(&d.path("state/ready")).exists() {
        assert!(t.elapsed() < Duration::from_secs(20), "the container is not ready");
        std::thread::sleep(Duration::from_millis(20));
    }
    Container(child)
}

fn exec(d: &Dir, script: &str) -> (i32, String, String) {
    let p = write(d, "exec.json", &plan(d, &["sh", "-c", script]));
    let o = backend(d, "exec", &p).output().unwrap();
    (o.status.code().unwrap_or(-1), stdout(&o), stderr(&o))
}

/// /proc/self/status of a program of its own: a shell (bash) ignores
/// SIGQUIT for itself, not for its programs.
const STATUS: &str = "exec cat /proc/self/status";

/// The value of a field of /proc/PID/status.
fn field<'a>(status: &'a str, name: &str) -> &'a str {
    status.lines().find_map(|l| l.strip_prefix(&format!("{name}:"))).map(str::trim).unwrap_or("")
}

#[test]
fn run_gives_the_environment_the_name_and_the_exit_code() {
    if !bwrap_works() {
        return;
    }
    let d = Dir::new("bwrap-run");
    let (rc, out, err) = sh(&d, "echo \"$FOO\"; echo \"${SANDBOX_BACKEND_TEST_SECRET:-none}\"; cat /proc/sys/kernel/hostname; pwd; exit 5");
    assert_eq!(rc, 5, "{err}");
    assert_eq!(out, "a b\nnone\nbox\n/tmp\n");
}

#[test]
fn run_isolates_the_files_and_the_network() {
    if !bwrap_works() {
        return;
    }
    let d = Dir::new("bwrap-iso");
    let (rc, out, err) = sh(&d, "touch /tmp/ok && echo tmp-ok; touch /nix/store/x 2>/dev/null || echo store-ro; [ -e /home ] || echo no-home; n=0; while read -r l; do n=$((n+1)); done < /proc/net/dev; echo lines:$n");
    assert_eq!(rc, 0, "{err}");
    // Only lo in /proc/net/dev (and 2 lines of header).
    assert_eq!(out, "tmp-ok\nstore-ro\nno-home\nlines:3\n");
}

#[test]
fn run_has_no_capabilities() {
    if !bwrap_works() {
        return;
    }
    let d = Dir::new("bwrap-caps");
    let (_, out, err) = sh(&d, STATUS);
    assert_eq!(field(&out, "CapEff"), "0000000000000000", "{out}{err}");
    assert_eq!(field(&out, "SigIgn"), "0000000000000000", "{out}");
}

#[test]
fn exec_joins_the_container() {
    if !bwrap_works() {
        return;
    }
    let d = Dir::new("bwrap-exec");
    let _c = start(&d);
    // The processes of the container: process 1 is bwrap, the sleep of the
    // container is there.
    let (rc, out, err) = exec(&d, "cat /proc/1/comm; for p in /proc/[0-9]*; do read -r c < $p/comm; [ \"$c\" = sleep ] && echo has-sleep; done; exit 7");
    assert_eq!(rc, 7, "{err}");
    assert!(out.starts_with("bwrap\n") && out.contains("has-sleep"), "{out}");
    // A file of one run is there for the next run (one /tmp).
    assert_eq!(exec(&d, "echo x > /tmp/shared").0, 0);
    assert_eq!(exec(&d, "cat /tmp/shared").1, "x\n");
}

#[test]
fn exec_has_no_capabilities_and_no_new_privileges() {
    if !bwrap_works() {
        return;
    }
    let d = Dir::new("bwrap-exec-caps");
    let _c = start(&d);
    let (rc, out, err) = exec(&d, STATUS);
    assert_eq!(rc, 0, "{err}");
    assert_eq!(field(&out, "CapEff"), "0000000000000000", "{out}");
    assert_eq!(field(&out, "CapPrm"), "0000000000000000", "{out}");
    assert_eq!(field(&out, "NoNewPrivs"), "1", "{out}");
    // No ignored signals (Ctrl-C, SIGPIPE).
    assert_eq!(field(&out, "SigIgn"), "0000000000000000", "{out}");
    // The root of the container, read-only, and the user of the plan.
    let (_, out, _) = exec(&d, "touch /nix/store/x 2>/dev/null || echo ro; [ -e /home ] || echo no-home; id -u");
    assert_eq!(out, format!("ro\nno-home\n{}\n", unsafe { libc::getuid() }));
}

#[test]
fn exec_gets_only_the_environment_of_the_plan() {
    if !bwrap_works() {
        return;
    }
    let d = Dir::new("bwrap-exec-env");
    let _c = start(&d);
    let (_, out, _) = exec(&d, "echo \"$FOO\"; echo \"${SANDBOX_BACKEND_TEST_SECRET:-none}\"; pwd");
    assert_eq!(out, "a b\nnone\n/tmp\n");
}

#[test]
fn exec_without_a_terminal_is_a_new_session() {
    if !bwrap_works() {
        return;
    }
    let d = Dir::new("bwrap-exec-sid");
    let _c = start(&d);
    // Field 6 of /proc/self/stat is the session; the shell is its leader.
    let (_, out, err) = exec(&d, "read -r pid comm state ppid pgrp sid rest < /proc/self/stat; [ \"$sid\" = \"$pid\" ] && echo leader");
    assert_eq!(out, "leader\n", "{err}");
}

#[test]
fn exec_with_a_terminal_gives_a_terminal_of_the_container() {
    if !bwrap_works() {
        return;
    }
    let d = Dir::new("bwrap-exec-tty");
    let _c = start(&d);
    let mut plan = plan(&d, &["sh", "-c", "tty; [ -e \"$(tty)\" ] && echo has-node; read -r pid comm state ppid pgrp sid rest < /proc/self/stat; [ \"$sid\" = \"$pid\" ] && echo leader; read l; echo got:$l; exit 3"]);
    plan["tty"] = json!(true);
    let p = write(&d, "exec.json", &plan);
    // The terminal of the caller is not a terminal here (a pipe): the
    // program gets a terminal all the same, and this process the bytes.
    let mut child = backend(&d, "exec", &p).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    {
        use std::io::Write;
        child.stdin.take().unwrap().write_all(b"hello\n").unwrap();
    }
    let o = child.wait_with_output().unwrap();
    let out = stdout(&o);
    assert_eq!(o.status.code(), Some(3), "{out}{}", stderr(&o));
    // ttyname() finds the terminal in the devpts of the container. The
    // terminal echoes the input, maybe before this line.
    assert!(out.lines().any(|l| l.starts_with("/dev/pts/")), "{out}");
    assert!(out.contains("has-node") && out.contains("leader") && out.contains("got:hello"), "{out}");
}

#[test]
fn exec_gives_term_to_the_program() {
    if !bwrap_works() {
        return;
    }
    let d = Dir::new("bwrap-exec-term");
    let _c = start(&d);
    let p = write(&d, "exec.json", &plan(&d, &["sleep", "60"]));
    let mut child = backend(&d, "exec", &p).spawn().unwrap();
    std::thread::sleep(Duration::from_millis(500));
    unsafe { libc::kill(child.id() as i32, libc::SIGTERM) };
    let t = Instant::now();
    let st = child.wait().unwrap();
    assert!(t.elapsed() < Duration::from_secs(5));
    // The program stopped with SIGTERM: 128 + 15.
    assert_eq!(st.code(), Some(143));
}

#[test]
fn exec_refuses_a_stopped_container_or_a_different_process() {
    if !bwrap_works() {
        return;
    }
    let d = Dir::new("bwrap-exec-stopped");
    let c = start(&d);
    let pidfile = d.path("state/pid");
    let good = std::fs::read_to_string(&pidfile).unwrap();
    // The same PID with a different start time: not the container.
    let pid = good.split_whitespace().next().unwrap();
    std::fs::write(&pidfile, format!("{pid} 1\n")).unwrap();
    let (rc, _, err) = exec(&d, "true");
    assert_eq!(rc, 1);
    assert!(err.contains("stopped"), "{err}");
    std::fs::write(&pidfile, "nonsense").unwrap();
    assert!(exec(&d, "true").2.contains("is not valid"));
    std::fs::write(&pidfile, &good).unwrap();
    assert_eq!(exec(&d, "true").0, 0);
    drop(c);
    let t = Instant::now();
    loop {
        let (rc, _, err) = exec(&d, "true");
        if rc == 1 && err.contains("stopped") {
            break;
        }
        assert!(t.elapsed() < Duration::from_secs(5), "{rc} {err}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn exec_before_start() {
    if in_a_sandbox() {
        return;
    }
    let d = Dir::new("bwrap-exec-none");
    let (rc, _, err) = exec(&d, "true");
    assert_eq!(rc, 1);
    assert!(err.contains("is not ready"), "{err}");
}

#[test]
fn show_and_usage() {
    let d = Dir::new("bwrap-show");
    let p = write(&d, "show.json", &plan(&d, &["echo", "it's"]));
    let o = backend(&d, "show", &p).output().unwrap();
    let out = stdout(&o);
    assert!(out.contains("--hostname box") && out.contains("--unshare-net") && out.trim_end().ends_with("-- echo 'it'\\''s'"), "{out}");
    let o = backend(&d, "bogus", &p).output().unwrap();
    assert_eq!(o.status.code(), Some(2));
    let o = Command::new(d.path("bin/sandbox-backend-bwrap")).output().unwrap();
    assert_eq!(o.status.code(), Some(2));
    assert_eq!(stderr(&o), "usage: sandbox-backend-bwrap run|start|exec|show PLAN.json\n");
}
