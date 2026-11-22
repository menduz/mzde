//! sandbox-init: the first program in a sandbox. It starts the private
//! services that the plan asks for, and then it replaces itself with the
//! command.
//!
//!   SANDBOX_INIT_DBUS=1          a private session bus
//!   SANDBOX_INIT_NOTIFY=PATH     the socket of the notifications of the
//!                                host (notify_bridge.rs)
//!   SANDBOX_INIT_X11=1           a private X server (xwayland-satellite)
//!   SANDBOX_INIT_X11_DISPLAY=N   the first display number to try
//!   SANDBOX_INIT_HOLD=1          the first process of the container of a
//!                                sandbox: start the services, write their
//!                                variables to $XDG_RUNTIME_DIR/sandbox-init.env,
//!                                and stay until the container stops
//!   SANDBOX_INIT_JOIN=1          a run in the container: read the variables
//!                                of the services from that file, and then
//!                                start the command
//!
//! The services stop with the sandbox: the backend stops all processes of
//! the sandbox when the command (or, in a container, this process) stops.
//! The services do not get the terminal: a terminal stays open while a
//! process has it, and they must not read from it.

use std::fs;
use std::process::{Command, Stdio};
use std::time::Duration;

use crate::process::{self, catch_signals, take_signals};
use crate::tools;
use crate::util;

fn fail(msg: &str) -> ! {
    eprintln!("sandbox-init: {msg}");
    std::process::exit(1)
}

/// Whether an environment variable is set and not empty.
fn on(name: &str) -> bool {
    util::env(name).is_some_and(|v| !v.is_empty())
}

/// Read the KEY=VALUE lines of the file of the container.
fn read_env(path: &str) -> Vec<(String, String)> {
    parse_env(&fs::read_to_string(path).unwrap_or_default())
}

/// The KEY=VALUE lines of a text. A line without "=" is not a variable.
fn parse_env(text: &str) -> Vec<(String, String)> {
    text.lines()
        .filter_map(|l| l.split_once('=').filter(|(k, _)| !k.is_empty()).map(|(k, v)| (k.to_string(), v.to_string())))
        .collect()
}

/// The text of the file of the container.
fn env_text(vars: &[(&str, String)]) -> String {
    vars.iter().map(|(k, v)| format!("{k}={v}\n")).collect()
}

/// The first display number from `start` that no X server has.
fn free_display(start: u32, taken: &dyn Fn(u32) -> bool) -> u32 {
    let mut n = start;
    while taken(n) {
        n += 1;
    }
    n
}

/// The configuration of the private bus and its service file of
/// org.freedesktop.Notifications. The service directory of the sandbox is
/// before the directories of the system (the first file of a name wins), so
/// the bus starts the bridge (notify_bridge.rs) and never the notification
/// server of the host (dunst) in the sandbox. `socket` is the socket of the
/// notifications of the host, or "": then the bridge stops with an error.
fn bus_files(services: &str, exe: &str, socket: &str, session_conf: &str) -> (String, String) {
    let xml = |s: &str| s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;");
    let config = format!(
        "<!DOCTYPE busconfig PUBLIC \"-//freedesktop//DTD D-BUS Bus Configuration 1.0//EN\"\n \"http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd\">\n<busconfig>\n  <servicedir>{}</servicedir>\n  <include>{}</include>\n</busconfig>\n",
        xml(services),
        xml(session_conf)
    );
    let service = format!("[D-BUS Service]\nName=org.freedesktop.Notifications\nExec={exe} __notify_bridge {socket}\n");
    (config, service.replace(" \n", "\n"))
}

/// Write the files of `bus_files` in `dir`, and return the configuration
/// file. On an error, the configuration of the system.
fn write_bus_files(dir: &str, socket: &str) -> String {
    let services = format!("{dir}/services");
    let (config, service) = bus_files(&services, &process::self_exe(), socket, tools::dbus_session_conf());
    let path = format!("{dir}/session.conf");
    let written = fs::create_dir_all(&services)
        .and_then(|_| fs::write(format!("{services}/org.freedesktop.Notifications.service"), service))
        .and_then(|_| fs::write(&path, config));
    match written {
        Ok(()) => path,
        Err(e) => {
            eprintln!("sandbox-init: cannot write the configuration of the bus in {dir}: {e}");
            tools::dbus_session_conf().to_string()
        }
    }
}

pub fn main(args: &[String]) -> ! {
    let runtime = util::env("XDG_RUNTIME_DIR").unwrap_or_else(|| "/tmp".into());
    let envfile = format!("{runtime}/sandbox-init.env");

    if on("SANDBOX_INIT_JOIN") {
        // SAFETY: this process has one thread.
        unsafe { std::env::remove_var("SANDBOX_INIT_JOIN") };
        // The backend starts a run when the container has started this
        // program, but the services can need more time.
        for _ in 0..200 {
            if util::exists(&envfile) {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        if util::exists(&envfile) {
            for (k, v) in read_env(&envfile) {
                // SAFETY: this process has one thread.
                unsafe { std::env::set_var(k, v) };
            }
        } else {
            eprintln!("sandbox-init: the services of the sandbox are not ready; the program starts without them");
        }
        fail(&process::exec(args));
    }

    let mut vars: Vec<(&str, String)> = Vec::new();

    if on("SANDBOX_INIT_DBUS") {
        let addr = format!("unix:path={runtime}/bus");
        let log = fs::File::create(format!("{runtime}/dbus-daemon.log")).ok();
        let config = write_bus_files(&format!("{runtime}/sandbox-dbus"), &util::env("SANDBOX_INIT_NOTIFY").unwrap_or_default());
        // --fork returns when the bus is ready.
        let st = Command::new(tools::dbus_daemon())
            .args([
                &format!("--config-file={config}"),
                &format!("--address={addr}"),
                "--fork",
                "--nopidfile",
                "--print-address=1",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(log.map(Stdio::from).unwrap_or(Stdio::null()))
            .status();
        match st {
            Ok(s) if s.success() => {
                // SAFETY: this process has one thread.
                unsafe { std::env::set_var("DBUS_SESSION_BUS_ADDRESS", &addr) };
                vars.push(("DBUS_SESSION_BUS_ADDRESS", addr));
            }
            _ => eprintln!("sandbox-init: dbus-daemon did not start; refer to {runtime}/dbus-daemon.log"),
        }
    }

    if on("SANDBOX_INIT_X11") && on("WAYLAND_DISPLAY") {
        let start = util::env("SANDBOX_INIT_X11_DISPLAY").and_then(|s| s.parse().ok()).unwrap_or(0);
        let n = free_display(start, &|n| util::exists(&format!("/tmp/.X11-unix/X{n}")) || util::exists(&format!("/tmp/.X{n}-lock")));
        let log = format!("{runtime}/xwayland-satellite.log");
        let out = fs::File::create(&log).ok();
        let err = out.as_ref().and_then(|f| f.try_clone().ok());
        let started = Command::new(tools::xwayland_satellite())
            .arg(format!(":{n}"))
            .stdin(Stdio::null())
            .stdout(out.map(Stdio::from).unwrap_or(Stdio::null()))
            .stderr(err.map(Stdio::from).unwrap_or(Stdio::null()))
            .spawn();
        let sock = format!("/tmp/.X11-unix/X{n}");
        if started.is_ok() {
            for _ in 0..100 {
                if util::is_socket(&sock) {
                    break;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        }
        if util::is_socket(&sock) {
            let d = format!(":{n}");
            // SAFETY: this process has one thread.
            unsafe { std::env::set_var("DISPLAY", &d) };
            vars.push(("DISPLAY", d));
        } else {
            eprintln!("sandbox-init: xwayland-satellite did not start; refer to {log}");
        }
    }

    for v in ["SANDBOX_INIT_DBUS", "SANDBOX_INIT_NOTIFY", "SANDBOX_INIT_X11", "SANDBOX_INIT_X11_DISPLAY"] {
        // SAFETY: this process has one thread.
        unsafe { std::env::remove_var(v) };
    }

    if on("SANDBOX_INIT_HOLD") {
        // Write the file in one step: a run must not read half of it.
        let text = env_text(&vars);
        let tmp = format!("{envfile}.tmp");
        if fs::write(&tmp, text).and_then(|_| fs::rename(&tmp, &envfile)).is_err() {
            fail(&format!("cannot write {envfile}"));
        }
        // In gVisor this is process 1, and it gets the processes of the runs
        // that lose their parent. Wait for them, so they do not stay as
        // zombies.
        // SIGCHLD stops wait_any when a process without a parent comes.
        catch_signals(&[libc::SIGTERM, libc::SIGINT, libc::SIGCHLD]);
        loop {
            if let Some((-1, _)) = process::wait_any() {
                // No child now: look again in one second.
                std::thread::sleep(Duration::from_secs(1));
            }
            if take_signals().iter().any(|&s| s == libc::SIGTERM || s == libc::SIGINT) {
                std::process::exit(0);
            }
        }
    }

    fail(&process::exec(args));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_service_file_of_the_notifications() {
        let (config, service) = bus_files("/run/user/1/sandbox-dbus/services", "/s/bin/sandbox", "/run/user/1/sandbox-notify", "/d/session.conf");
        assert!(config.contains("<servicedir>/run/user/1/sandbox-dbus/services</servicedir>\n  <include>/d/session.conf</include>"));
        assert_eq!(service, "[D-BUS Service]\nName=org.freedesktop.Notifications\nExec=/s/bin/sandbox __notify_bridge /run/user/1/sandbox-notify\n");
        let (_, service) = bus_files("/x", "/s/bin/sandbox", "", "/d");
        assert!(service.ends_with("Exec=/s/bin/sandbox __notify_bridge\n"));
    }

    /// With a real dbus-daemon: the bus starts the program of the sandbox
    /// for org.freedesktop.Notifications, not the notification server of
    /// the system. Without dbus-daemon, the test does not run.
    #[test]
    fn the_bus_starts_the_bridge_and_not_dunst() {
        use crate::dbus::{Conn, Message, Value};
        let dir = std::env::temp_dir().join(format!("sandbox-init-bus-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let d = dir.to_str().unwrap().to_string();
        let system = format!("{d}/system/dbus-1/services");
        fs::create_dir_all(&system).unwrap();
        let fake = |who: &str| {
            let path = format!("{d}/{who}");
            // Exit status 1: for status 0, the bus waits for the name (a
            // program that forks), until its timeout.
            fs::write(&path, format!("#!/bin/sh\necho {who} >> {d}/started\nexit 1\n")).unwrap();
            fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
            path
        };
        let dunst = fake("dunst");
        fs::write(format!("{system}/org.knopwob.dunst.service"), format!("[D-BUS Service]\nName=org.freedesktop.Notifications\nExec={dunst}\n")).unwrap();
        let services = format!("{d}/sandbox-dbus/services");
        fs::create_dir_all(&services).unwrap();
        let (config, service) = bus_files(&services, &fake("bridge"), "", tools::dbus_session_conf());
        fs::write(format!("{services}/org.freedesktop.Notifications.service"), service).unwrap();
        fs::write(format!("{d}/session.conf"), config).unwrap();

        let address = format!("unix:path={d}/bus");
        let Ok(mut daemon) = Command::new(tools::dbus_daemon())
            .args([&format!("--config-file={d}/session.conf"), &format!("--address={address}"), "--nofork", "--nopidfile"])
            .env("XDG_DATA_DIRS", format!("{d}/system"))
            .env("XDG_DATA_HOME", format!("{d}/none"))
            .stderr(Stdio::null())
            .spawn()
        else {
            eprintln!("no dbus-daemon: the test does not run");
            return;
        };
        for _ in 0..500 {
            if util::exists(&format!("{d}/bus")) {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let mut c = Conn::connect(&address).unwrap();
        // The fake bridge stops with an error: the call is an error, but
        // the bus started it.
        let start = Message::call("org.freedesktop.DBus", "/org/freedesktop/DBus", "org.freedesktop.DBus", "StartServiceByName", vec![Value::str("org.freedesktop.Notifications"), Value::U32(0)]);
        assert!(c.call(start).is_err());
        let _ = daemon.kill();
        let _ = daemon.wait();
        assert_eq!(fs::read_to_string(format!("{d}/started")).unwrap(), "bridge\n");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn env_file_round_trip() {
        let vars = [("DBUS_SESSION_BUS_ADDRESS", "unix:path=/run/user/1000/bus".to_string()), ("DISPLAY", ":3".to_string())];
        let text = env_text(&vars);
        assert_eq!(text, "DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/1000/bus\nDISPLAY=:3\n");
        assert_eq!(
            parse_env(&text),
            [("DBUS_SESSION_BUS_ADDRESS".to_string(), "unix:path=/run/user/1000/bus".to_string()), ("DISPLAY".to_string(), ":3".to_string())]
        );
        // No shell: the text is not run, and a line without "=" is not a
        // variable.
        assert_eq!(parse_env("A=$(rm -rf /)\njunk\n=x\nB=a=b\n"), [("A".to_string(), "$(rm -rf /)".to_string()), ("B".to_string(), "a=b".to_string())]);
        assert!(parse_env("").is_empty());
    }

    #[test]
    fn free_display_skips_the_used_numbers() {
        assert_eq!(free_display(0, &|_| false), 0);
        assert_eq!(free_display(0, &|n| n < 3), 3);
        assert_eq!(free_display(534, &|n| n == 534), 535);
    }
}
