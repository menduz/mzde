#![recursion_limit = "512"]
//! `sandbox run` with a container of its own (SANDBOX_NO_SCOPE), the
//! commands of the sandboxes, `host` and the claude of yolo. The tests with
//! a sandbox do not run where bwrap cannot make one.

mod common;
use common::*;
use serde_json::json;
use std::fs;

fn dir(tag: &str) -> Dir {
    let d = Dir::new(&format!("run-{tag}"));
    fs::create_dir_all(d.path("home/work/a")).unwrap();
    d.config(
        json!({
            "s": {"paths": [d.path("home/work")]},
            "tmp": {"persistentHome": false},
            "keep": {"cleanTmp": false},
            "hostnet": {"disableNetworking": false, "shareHostNetwork": true},
        }),
        json!({}),
    );
    d
}

fn run_sh(d: &Dir, env: &str, script: &str) -> std::process::Output {
    d.run(&["run", env, "--", "sh", "-c", script])
}

#[test]
fn the_exit_code_and_the_labels() {
    if !bwrap_works() {
        return;
    }
    let d = dir("labels");
    let o = run_sh(&d, "s", "echo $SANDBOX_NAME $SANDBOX_COLOR $SANDBOX_BACKEND; cat /proc/sys/kernel/hostname; exit 9");
    assert_eq!(o.status.code(), Some(9), "{}", stderr(&o));
    assert_eq!(stdout(&o), "s #008080 bwrap\ns\n");
}

#[test]
fn the_persistent_home_keeps_its_files() {
    if !bwrap_works() {
        return;
    }
    let d = dir("home");
    assert!(run_sh(&d, "s", "echo kept > ~/f").status.success());
    assert_eq!(fs::read_to_string(d.path("state/s/home/f")).unwrap(), "kept\n");
    assert_eq!(stdout(&run_sh(&d, "s", "cat ~/f")), "kept\n");
    // The home of the host is not there.
    fs::write(d.path("home/secret"), "x").unwrap();
    assert_eq!(stdout(&run_sh(&d, "s", "test -e ~/secret || echo hidden")), "hidden\n");
    // A disposable home loses its files.
    assert!(run_sh(&d, "tmp", "echo x > ~/gone").status.success());
    assert_eq!(stdout(&run_sh(&d, "tmp", "test -e ~/gone || echo gone")), "gone\n");
}

#[test]
fn the_working_directory() {
    if !bwrap_works() {
        return;
    }
    let d = dir("cwd");
    let o = d.cmd(&["run", "s", "--", "pwd"]).current_dir(d.path("home/work/a")).output().unwrap();
    assert_eq!(stdout(&o).trim(), d.path("home/work/a"), "{}", stderr(&o));
    // Outside of the paths: the home directory.
    let o = d.cmd(&["run", "s", "--", "pwd"]).current_dir(d.path("bin")).output().unwrap();
    assert_eq!(stdout(&o).trim(), d.path("home"));
    // --need-cwd stops there.
    let o = d.cmd(&["run", "--need-cwd", "s", "--", "pwd"]).current_dir(d.path("bin")).output().unwrap();
    assert_eq!(o.status.code(), Some(1));
    assert!(stderr(&o).contains("does not have the current directory"), "{}", stderr(&o));
    let o = d.cmd(&["run", "--need-cwd", "s", "--", "true"]).current_dir(d.path("home/work")).output().unwrap();
    assert!(o.status.success(), "{}", stderr(&o));
}

#[test]
fn the_files_of_a_run() {
    if !bwrap_works() {
        return;
    }
    let d = dir("clean");
    let runs = || fs::read_dir(d.path("run")).unwrap().filter_map(|e| e.ok()).filter(|e| e.file_name().to_string_lossy().starts_with("sandbox-")).count();
    assert!(run_sh(&d, "s", "true").status.success());
    assert_eq!(runs(), 0, "cleanTmp removes the directory of the run");
    let o = run_sh(&d, "keep", "true");
    assert!(stderr(&o).contains("the files of the run are in"), "{}", stderr(&o));
    assert_eq!(runs(), 1);
}

#[test]
fn the_host_network() {
    if !bwrap_works() {
        return;
    }
    let d = dir("hostnet");
    // The interfaces of the host, not only lo.
    let o = run_sh(&d, "hostnet", "n=0; while read -r l; do n=$((n+1)); done < /proc/net/dev; echo $n");
    let host = fs::read_to_string("/proc/net/dev").unwrap().lines().count();
    assert_eq!(stdout(&o).trim(), host.to_string(), "{}", stderr(&o));
}

/// The path of sh on this host.
fn sh_path() -> String {
    std::env::var("PATH").unwrap().split(':').map(|d| format!("{d}/sh")).find(|p| std::path::Path::new(p).exists()).unwrap()
}

fn command_dir(tag: &str, cwd_only: bool) -> Dir {
    let d = Dir::new(&format!("cmd-{tag}"));
    fs::create_dir_all(d.path("home/work/a")).unwrap();
    fs::write(d.path("home/notes"), "secret").unwrap();
    d.config(
        json!({"s": {"paths": [d.path("home/work"), d.path("home/notes")]}, "t": {}}),
        json!({"commands": {"probe": {"s": {"program": sh_path(), "cwdOnly": cwd_only, "bwrapArgs": []}}}}),
    );
    d
}

fn pid1_is_systemd() -> bool {
    fs::read_to_string("/proc/1/comm").map(|c| c.trim() == "systemd").unwrap_or(false)
}

#[test]
fn a_command_stops_outside_of_its_sandboxes() {
    let d = command_dir("outside", false);
    let o = d.run(&["__command", "probe", "-c", "echo started"]);
    assert_eq!(o.status.code(), Some(1));
    assert_eq!(stderr(&o), "probe: probe runs only in the sandbox s, not on the host. Start it in a terminal of a sandbox (sandbox run NAME).\n");
    if !pid1_is_systemd() {
        let o = d.cmd(&["__command", "probe", "-c", "echo started"]).env("SANDBOX_NAME", "t").output().unwrap();
        assert!(stderr(&o).contains("not in the sandbox t."), "{}", stderr(&o));
    }
    let o = d.run(&["__command", "nope"]);
    assert_eq!(stderr(&o), "nope: nope is not a command of the sandboxes\n");
}

#[test]
fn a_command_starts_in_its_sandbox() {
    if pid1_is_systemd() {
        return;
    }
    let d = command_dir("inside", false);
    let o = d.cmd(&["__command", "probe", "-c", "echo started $0 $1", "x"]).env("SANDBOX_NAME", "s").output().unwrap();
    // `sh -c SCRIPT x`: $0 is x.
    assert_eq!(stdout(&o), "started x\n", "{}", stderr(&o));
}

#[test]
fn a_cwd_only_command_sees_only_the_current_directory() {
    if pid1_is_systemd() || !bwrap_works() {
        return;
    }
    let d = command_dir("cwdonly", true);
    fs::write(d.path("home/work/b"), "").unwrap();
    let o = d
        .cmd(&["__command", "probe", "-c", "ls ~/work; echo [$(cat ~/notes)]; touch written; pwd"])
        .env("SANDBOX_NAME", "s")
        .current_dir(d.path("home/work/a"))
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", stderr(&o));
    // Only "a" of ~/work, an empty ~/notes, and a writable current directory.
    assert_eq!(stdout(&o), format!("a\n[]\n{}\n", d.path("home/work/a")));
    assert!(std::path::Path::new(&d.path("home/work/a/written")).exists());
}

#[test]
fn host_has_the_label_of_the_host() {
    // In a sandbox, `host` stops with an error (tests/cli.rs).
    if in_a_sandbox() {
        return;
    }
    let d = dir("host");
    let o = d
        .cmd(&["host", "--", "sh", "-c", "echo $SANDBOX_NAME $SANDBOX_COLOR ${SANDBOX_BACKEND:-none} ${SANDBOX__SCOPED:-none}"])
        .env("SANDBOX_BACKEND", "gvisor")
        .env("SANDBOX__SCOPED", "1")
        .output()
        .unwrap();
    assert_eq!(stdout(&o), "host #404040 none none\n", "{}", stderr(&o));
}

#[test]
fn yolo_starts_the_next_claude_with_the_flag() {
    let d = Dir::new("yolo");
    fs::create_dir_all(d.path("wrap")).unwrap();
    fs::create_dir_all(d.path("real")).unwrap();
    let claude = d.path("real/claude");
    fs::write(&claude, format!("#!{}\necho \"$@\"\n", sh_path())).unwrap();
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&claude, fs::Permissions::from_mode(0o755)).unwrap();
    // The wrapper is first in PATH; it must not start itself again.
    std::os::unix::fs::symlink(BIN, d.path("wrap/claude")).unwrap();
    let path = format!("{}:{}:{}", d.path("wrap"), d.path("real"), std::env::var("PATH").unwrap());
    let o = d.cmd(&["__yolo", &d.path("wrap"), "-p", "hi"]).env("PATH", path).output().unwrap();
    assert_eq!(stdout(&o), "--dangerously-skip-permissions -p hi\n", "{}", stderr(&o));
}

#[test]
fn term_to_the_run_stops_the_program() {
    if !bwrap_works() {
        return;
    }
    let d = dir("term");
    for sig in [libc::SIGTERM, libc::SIGHUP] {
        let mut child = d.cmd(&["run", "s", "--", "sleep", "30"]).spawn().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(700));
        unsafe { libc::kill(child.id() as i32, sig) };
        let t = std::time::Instant::now();
        let st = child.wait().unwrap();
        assert!(t.elapsed() < std::time::Duration::from_secs(5), "{sig}: {:?}", t.elapsed());
        assert_eq!(st.code(), Some(128 + sig), "{sig}");
    }
}
