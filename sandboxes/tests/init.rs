#![recursion_limit = "512"]
//! sandbox-init without a sandbox: the modes hold and join, and a private
//! session bus.

mod common;
use common::*;
use std::fs;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn init(d: &Dir) -> Command {
    let mut c = Command::new(d.path("bin/sandbox-init"));
    c.env_clear()
        .env("PATH", std::env::var("PATH").unwrap())
        .env("XDG_RUNTIME_DIR", d.path("run"))
        .stdin(Stdio::null());
    c
}

fn wait_for(path: &str) {
    let t = Instant::now();
    while !std::path::Path::new(path).exists() {
        assert!(t.elapsed() < Duration::from_secs(10), "{path} is not there");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn without_services_it_replaces_itself_with_the_command() {
    let d = Dir::new("init-exec");
    let o = init(&d).args(["sh", "-c", "echo $$ ${SANDBOX_INIT_X11:-none}; exit 6"]).env("SANDBOX_INIT_X11", "1").output().unwrap();
    assert_eq!(o.status.code(), Some(6));
    // The variables of init do not go to the command. Without
    // WAYLAND_DISPLAY there is no X server.
    assert!(stdout(&o).trim_end().ends_with(" none"), "{}", stdout(&o));
    let o = init(&d).arg("no-such-command-x").output().unwrap();
    assert_eq!(o.status.code(), Some(1));
    assert!(stderr(&o).starts_with("sandbox-init: no-such-command-x: command not found"), "{}", stderr(&o));
}

#[test]
fn hold_writes_the_variables_and_stays_until_term() {
    let d = Dir::new("init-hold");
    let mut child = init(&d).env("SANDBOX_INIT_HOLD", "1").spawn().unwrap();
    let file = d.path("run/sandbox-init.env");
    wait_for(&file);
    // No services: an empty file.
    assert_eq!(fs::read_to_string(&file).unwrap(), "");
    std::thread::sleep(Duration::from_millis(200));
    assert!(child.try_wait().unwrap().is_none(), "init stays");
    unsafe { libc::kill(child.id() as i32, libc::SIGTERM) };
    let st = child.wait().unwrap();
    assert_eq!(st.code(), Some(0));
}

#[test]
fn join_reads_the_variables_of_the_container() {
    let d = Dir::new("init-join");
    fs::write(d.path("run/sandbox-init.env"), "DISPLAY=:5\nDBUS_SESSION_BUS_ADDRESS=unix:path=/x\nBAD LINE\n").unwrap();
    let o = init(&d)
        .args(["sh", "-c", "echo $DISPLAY $DBUS_SESSION_BUS_ADDRESS ${SANDBOX_INIT_JOIN:-none}"])
        .env("SANDBOX_INIT_JOIN", "1")
        .output()
        .unwrap();
    assert_eq!(stdout(&o), ":5 unix:path=/x none\n", "{}", stderr(&o));
    // The file is read, not run by a shell.
    fs::write(d.path("run/sandbox-init.env"), format!("X=$(touch {})\n", d.path("ran"))).unwrap();
    let o = init(&d).args(["sh", "-c", "printf '%s' \"$X\""]).env("SANDBOX_INIT_JOIN", "1").output().unwrap();
    assert_eq!(stdout(&o), format!("$(touch {})", d.path("ran")));
    assert!(!std::path::Path::new(&d.path("ran")).exists());
}

#[test]
fn hold_starts_a_private_session_bus() {
    let d = Dir::new("init-dbus");
    let mut child = init(&d).env("SANDBOX_INIT_HOLD", "1").env("SANDBOX_INIT_DBUS", "1").spawn().unwrap();
    let file = d.path("run/sandbox-init.env");
    wait_for(&file);
    let text = fs::read_to_string(&file).unwrap();
    let bus = d.path("run/bus");
    if text.is_empty() {
        // No dbus-daemon here (the message is in run/dbus-daemon.log).
        eprintln!("no dbus-daemon: {}", fs::read_to_string(d.path("run/dbus-daemon.log")).unwrap_or_default());
    } else {
        assert_eq!(text, format!("DBUS_SESSION_BUS_ADDRESS=unix:path={bus}\n"));
        use std::os::unix::fs::FileTypeExt;
        assert!(fs::metadata(&bus).unwrap().file_type().is_socket());
        // The bus answers.
        let s = std::os::unix::net::UnixStream::connect(&bus);
        assert!(s.is_ok(), "{s:?}");
    }
    unsafe { libc::kill(child.id() as i32, libc::SIGTERM) };
    assert_eq!(child.wait().unwrap().code(), Some(0));
    // dbus-daemon forked: it is not a child of init. Stop it.
    let addr = format!("--address=unix:path={bus}");
    for e in fs::read_dir("/proc").unwrap().flatten() {
        if let Ok(cmd) = fs::read(e.path().join("cmdline"))
            && cmd.split(|&b| b == 0).any(|a| a == addr.as_bytes())
            && let Ok(pid) = e.file_name().to_string_lossy().parse::<i32>()
        {
            unsafe { libc::kill(pid, libc::SIGTERM) };
        }
    }
}
