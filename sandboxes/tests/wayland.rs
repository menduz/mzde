#![recursion_limit = "512"]
//! The Wayland socket of a sandbox with a real compositor: a headless river.
//! A client on the socket of river gets the privileged globals; a client in
//! the sandbox does not (the security context, refer to src/wayland.rs).
//!
//! The tests run only with SANDBOX_TEST_RIVER (the program river) and
//! SANDBOX_TEST_WAYLAND_INFO (the program wayland-info), and where bwrap can
//! make a sandbox. The test of X11 also needs SANDBOX_TEST_XDPYINFO, and
//! xwayland-satellite (with Xwayland).

mod common;
use common::*;
use serde_json::json;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// The privileged globals of river (blocklist in Server.zig of river).
const PRIVILEGED: &[&str] = &[
    "wp_security_context_manager_v1",
    "river_window_manager_v1",
    "zwlr_layer_shell_v1",
    "zwp_virtual_keyboard_manager_v1",
    "zwlr_virtual_pointer_manager_v1",
    "zwlr_screencopy_manager_v1",
    "ext_data_control_manager_v1",
    "zwlr_data_control_manager_v1",
    "zwp_input_method_manager_v2",
    "ext_session_lock_manager_v1",
];

struct River(Child);

impl Drop for River {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// The interfaces in the output of wayland-info.
fn interfaces(out: &str) -> Vec<String> {
    out.lines().filter_map(|l| l.split("interface: '").nth(1)).filter_map(|r| r.split('\'').next()).map(String::from).collect()
}

#[test]
fn a_sandbox_does_not_get_the_privileged_globals() {
    let (Some(river), Some(info)) = (option_env_or("SANDBOX_TEST_RIVER"), option_env_or("SANDBOX_TEST_WAYLAND_INFO")) else {
        eprintln!("no SANDBOX_TEST_RIVER and SANDBOX_TEST_WAYLAND_INFO: the test does not run");
        return;
    };
    if !bwrap_works() {
        return;
    }
    let d = Dir::new("wayland");
    d.config(json!({"g": {"gui": true}}), json!({}));
    let (river, socket) = start_river(&d, &river);
    let run = d.path("run");
    // On the socket of river: the privileged globals are there.
    let o = Command::new(&info).env("XDG_RUNTIME_DIR", &run).env("WAYLAND_DISPLAY", &socket).output().unwrap();
    let host = interfaces(&stdout(&o));
    assert!(host.iter().any(|i| i == "wl_compositor"), "{host:?} {}", stderr(&o));
    for p in ["wp_security_context_manager_v1", "zwp_virtual_keyboard_manager_v1", "zwlr_screencopy_manager_v1"] {
        assert!(host.iter().any(|i| i == p), "river has no {p}: {host:?}");
    }

    // In the sandbox: the usual globals, but no privileged global.
    let o = d.cmd(&["run", "g", "--", &info]).env("WAYLAND_DISPLAY", &socket).output().unwrap();
    assert!(o.status.success(), "{}", stderr(&o));
    let sandbox = interfaces(&stdout(&o));
    for i in ["wl_compositor", "wl_shm", "xdg_wm_base", "wl_seat", "wl_data_device_manager"] {
        assert!(sandbox.iter().any(|s| s == i), "the sandbox has no {i}: {sandbox:?}");
    }
    for p in PRIVILEGED {
        assert!(!sandbox.iter().any(|s| s == p), "the sandbox has {p}: {sandbox:?}");
    }
    // The files of the run, with its socket, stop with the run.
    assert!(!std::fs::read_dir(&run).unwrap().filter_map(|e| e.ok()).any(|e| e.file_name().to_string_lossy().starts_with("sandbox-g")));
    drop(river);
}

/// A headless river with no outputs and no input devices (-c: no init
/// program), in the runtime directory of `d`, and the name of its socket.
fn start_river(d: &Dir, river: &str) -> (River, String) {
    let run = d.path("run");
    let river = River(
        Command::new(river)
            .args(["-no-xwayland", "-c", "sleep 600"])
            .env("XDG_RUNTIME_DIR", &run)
            .env("WLR_BACKENDS", "headless")
            .env("WLR_RENDERER", "pixman")
            .env("WLR_LIBINPUT_NO_DEVICES", "1")
            .env_remove("WAYLAND_DISPLAY")
            .env_remove("DISPLAY")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let t = Instant::now();
    loop {
        let found = std::fs::read_dir(&run).unwrap().filter_map(|e| e.ok()).map(|e| e.file_name().to_string_lossy().into_owned()).find(|n| n.starts_with("wayland-") && !n.ends_with(".lock"));
        if let Some(n) = found {
            return (river, n);
        }
        assert!(t.elapsed() < Duration::from_secs(20), "river did not make its socket");
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn the_x_server_of_a_sandbox_works_on_the_socket_of_the_sandbox() {
    let (Some(river), Some(xdpyinfo)) = (option_env_or("SANDBOX_TEST_RIVER"), option_env_or("SANDBOX_TEST_XDPYINFO")) else {
        eprintln!("no SANDBOX_TEST_RIVER and SANDBOX_TEST_XDPYINFO: the test does not run");
        return;
    };
    if !bwrap_works() {
        return;
    }
    let d = Dir::new("wayland-x11");
    // sandbox-init starts xwayland-satellite in the sandbox: the sandbox
    // gets the directory of the links, with a copy of the program (the
    // target of the links is not in the sandbox).
    let init = d.path("bin/sandbox-init");
    std::fs::remove_file(&init).unwrap();
    std::fs::copy(BIN, &init).unwrap();
    d.config(json!({"x": {"gui": true, "x11": "private", "paths": [d.path("bin")]}}), json!({}));
    let (_river, socket) = start_river(&d, &river);
    // xwayland-satellite is a client of the socket of the sandbox: with no
    // privileged global, it starts its X server.
    let o = d.cmd(&["run", "x", "--", "sh", "-c", &format!("echo $DISPLAY; {xdpyinfo} | grep -m1 'name of display'")]).env("WAYLAND_DISPLAY", &socket).output().unwrap();
    assert!(o.status.success(), "{}{}", stdout(&o), stderr(&o));
    let out = stdout(&o);
    let display = out.lines().next().unwrap_or("");
    assert!(display.starts_with(':'), "{out}{}", stderr(&o));
    assert!(out.contains(&format!("name of display:    {display}")), "{out}{}", stderr(&o));
}

/// An environment variable of the build, or of the test.
fn option_env_or(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|s| !s.is_empty())
}
