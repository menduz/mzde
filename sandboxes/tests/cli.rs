#![recursion_limit = "512"]
//! The subcommands of `sandbox` that start no sandbox: list, home,
//! backends, show, pick, __plan, the usage and the errors, and the refusal
//! to start sandboxes in a sandbox.

mod common;
use common::*;
use serde_json::json;

fn dir() -> Dir {
    let d = Dir::new(&format!("cli-{}", std::thread::current().name().unwrap_or("x").replace("::", "-")));
    d.config(
        json!({
            "web": {"gui": true, "color": "#800080", "description": "browser", "disableNetworking": false},
            "work": {"ssh": "write", "git": "read"},
        }),
        json!({}),
    );
    d
}

#[test]
fn list_names_json_and_table() {
    let d = dir();
    let o = d.run(&["list", "--names"]);
    assert!(o.status.success(), "{}", stderr(&o));
    assert_eq!(stdout(&o), "web\nwork\n");
    let o = d.run(&["list", "--json"]);
    let v: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(v["web"]["color"], "#800080");
    let o = d.run(&["list"]);
    let out = stdout(&o);
    let web = out.lines().find(|l| l.starts_with("web ")).unwrap();
    assert!(web.contains("#800080") && web.contains("bwrap") && web.contains("internet") && web.contains("gui") && web.ends_with("browser"), "{web}");
    let work = out.lines().find(|l| l.starts_with("work ")).unwrap();
    assert!(work.contains("none") && work.contains("ssh:rw,git"), "{work}");
}

/// A configuration of a newer module: a field that this version does not
/// know is a warning, and the command works.
#[test]
fn an_unknown_field_is_a_warning() {
    let d = Dir::new("cli-unknown-field");
    d.config(json!({"work": {"newerOption": true}}), json!({"newerTop": 1}));
    let o = d.run(&["list", "--names"]);
    assert!(o.status.success(), "{}", stderr(&o));
    assert_eq!(stdout(&o), "work\n");
    let err = stderr(&o);
    assert!(err.contains("does not know the fields newerTop, environments.work.newerOption; it ignores them"), "{err}");
    // A known field with a wrong value is still an error.
    d.config(json!({"work": {"gui": "sometimes"}}), json!({}));
    assert_eq!(d.run(&["list", "--names"]).status.code(), Some(1));
}

#[test]
fn home_and_backends() {
    let d = dir();
    let o = d.run(&["home", "work"]);
    assert_eq!(stdout(&o).trim(), d.path("state/work/home"));
    let o = d.run(&["backends"]);
    assert_eq!(stdout(&o), format!("bwrap\t{}\ngvisor\t{}\n", d.path("bin/sandbox-backend-bwrap"), d.path("bin/sandbox-backend-gvisor")));
    let o = d.run(&["home", "nope"]);
    assert_eq!(o.status.code(), Some(1));
    assert_eq!(stderr(&o), "sandbox: no sandbox has the name 'nope'. Run 'sandbox list'.\n");
}

#[test]
fn show_prints_the_plan_and_the_backend_command() {
    let d = dir();
    let o = d.run(&["show", "web"]);
    assert!(o.status.success(), "{}", stderr(&o));
    let out = stdout(&o);
    assert!(out.starts_with(&format!("backend: bwrap ({})\nnetwork: private\n", d.path("bin/sandbox-backend-bwrap"))), "{out}");
    assert!(out.contains("\"command\": [\n    \"CMD\"\n  ]"), "{out}");
    // The command of the backend: bwrap with the plan.
    let last = out.lines().last().unwrap();
    assert!(last.contains("--unshare-user") && last.ends_with("-- CMD"), "{last}");
    // show starts nothing and makes no home directory.
    assert!(!std::path::Path::new(&d.path("state/web/home")).exists() || std::fs::read_dir(d.path("state/web/home")).unwrap().next().is_none());
}

#[test]
fn plan_of_each_mode() {
    let d = dir();
    for mode in ["run", "container", "join"] {
        let o = d.run(&["__plan", mode, "work", "/T", "/S", "", "--", "echo", "hi"]);
        assert!(o.status.success(), "{mode}: {}", stderr(&o));
        let v: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
        assert_eq!(v["name"], "work");
        assert_eq!(v["state"], "/S");
        assert_eq!(v["network"], "none");
        let cmd: Vec<&str> = v["command"].as_array().unwrap().iter().map(|x| x.as_str().unwrap()).collect();
        match mode {
            "run" => assert_eq!(cmd, ["echo", "hi"]),
            _ => assert_eq!(cmd, [d.path("bin/sandbox-init").as_str(), "echo", "hi"]),
        }
    }
    let o = d.run(&["__plan", "other", "work", "/T", "/S", ""]);
    assert_eq!(o.status.code(), Some(1));
}

#[test]
fn pick_with_the_menu_of_the_configuration() {
    let d = dir();
    // The menu takes the first line.
    let o = d.run(&["pick"]);
    assert_eq!(stdout(&o), "web\n", "{}", stderr(&o));
    let o = d.run(&["pick", "--host"]);
    assert_eq!(stdout(&o), "host\n");
    // A menu that the user closes: exit code 1, no output.
    d.config(json!({"web": {}, "work": {}}), json!({"menu": {"command": "cat >/dev/null; exit 1"}}));
    let o = d.run(&["pick"]);
    assert_eq!((o.status.code(), stdout(&o).as_str()), (Some(1), ""));
    // A line that is no sandbox.
    d.config(json!({"web": {}}), json!({"menu": {"command": "cat >/dev/null; echo other"}}));
    let o = d.run(&["pick"]);
    assert_eq!(o.status.code(), Some(1));
    assert!(stderr(&o).contains("no sandbox has the name 'other'"));
    // --gui: the menu gets only the sandboxes with gui.
    d.config(json!({"web": {"gui": true}, "work": {}}), json!({"menu": {"command": "cat"}}));
    let o = d.run(&["pick", "--gui"]);
    assert_eq!(stdout(&o), "web\n");
    let o = d.run(&["pick", "--bad"]);
    assert_eq!(stderr(&o), "sandbox: usage: sandbox pick [--host] [--gui]\n");
}

#[test]
fn pick_with_markup() {
    let d = dir();
    d.config(json!({"web": {"color": "#800080"}}), json!({"menu": {"command": "tail -n 1", "markup": true}}));
    let o = d.run(&["pick", "--host"]);
    assert_eq!(stdout(&o), "web\n", "{}", stderr(&o));
}

#[test]
fn usage_and_errors() {
    let d = dir();
    let o = d.run(&[]);
    assert!(o.status.success());
    assert!(stdout(&o).starts_with("Usage: sandbox COMMAND"));
    let o = d.run(&["frobnicate"]);
    assert_eq!(o.status.code(), Some(2));
    assert!(stderr(&o).starts_with("Usage: sandbox COMMAND"));
    // In a sandbox, `run` stops before it reads its arguments.
    if !in_a_sandbox() {
        let o = d.run(&["run"]);
        assert_eq!(stderr(&o), "sandbox: usage: sandbox run [--need-cwd] NAME [--] [CMD...]\n");
        let o = d.run(&["run", "nope", "--", "true"]);
        assert_eq!(stderr(&o), "sandbox: no sandbox has the name 'nope'. Run 'sandbox list'.\n");
    }
    // A configuration with a wrong field: the name of the field.
    std::fs::write(d.path("config.json"), r#"{"menu": 1}"#).unwrap();
    let o = d.run(&["list", "--names"]);
    assert_eq!(o.status.code(), Some(1));
    let o = d.run(&["home", "web"]);
    assert!(stderr(&o).contains("config.json") && stderr(&o).to_lowercase().contains("menu"), "{}", stderr(&o));
    // No configuration.
    std::fs::remove_file(d.path("config.json")).unwrap();
    let o = d.run(&["backends"]);
    assert!(stderr(&o).starts_with(&format!("sandbox: cannot read {}", d.path("config.json"))), "{}", stderr(&o));
}

#[test]
fn current_needs_no_configuration() {
    let d = dir();
    std::fs::remove_file(d.path("config.json")).unwrap();
    let o = d.run(&["current"]);
    assert_eq!(stdout(&o), "host\n");
    let pid1 = std::fs::read_to_string("/proc/1/comm").unwrap_or_default();
    let o = d.cmd(&["current"]).env("SANDBOX_NAME", "work").output().unwrap();
    // On a host with systemd as process 1, SANDBOX_NAME is not the sandbox.
    let want = if pid1.trim() == "systemd" { "host\n" } else { "work\n" };
    assert_eq!(stdout(&o), want);
}

/// `PROG ARGS` (PROG: the program or one of its links) with the
/// environment of `d.cmd`, in a sandbox: here if this process is in one,
/// else in a bwrap with a PID namespace (PID 1 is bwrap). None if bwrap
/// cannot make a sandbox here.
fn in_sandbox(d: &Dir, prog: &str, args: &[&str]) -> Option<std::process::Output> {
    let inner = d.cmd(args);
    let mut c = if in_a_sandbox() {
        std::process::Command::new(prog)
    } else {
        let mut c = std::process::Command::new(option_env!("SANDBOX_TOOL_BWRAP").unwrap_or("bwrap"));
        c.args(["--unshare-user", "--unshare-pid", "--dev", "/dev", "--proc", "/proc", "--ro-bind", "/", "/", "--", prog]);
        c
    };
    c.args(args).current_dir(d.path("home")).stdin(std::process::Stdio::null());
    for (k, v) in inner.get_envs() {
        match v {
            Some(v) => c.env(k, v),
            None => c.env_remove(k),
        };
    }
    if !in_a_sandbox() {
        // bwrap cannot make a sandbox here (no user namespaces), or its
        // /proc does not show its PID namespace (in a PID namespace of
        // `unshare --mount-proc`): then the test cannot make a sandbox.
        let probe = std::process::Command::new(c.get_program()).args(["--unshare-user", "--unshare-pid", "--dev", "/dev", "--proc", "/proc", "--ro-bind", "/", "/", "--", "cat", "/proc/1/comm"]).output().ok()?;
        if stdout(&probe) != "bwrap\n" {
            return None;
        }
    }
    c.output().ok()
}

#[test]
fn nested_sandboxes_are_forbidden() {
    let d = dir();
    let plan = d.path("plan.json");
    std::fs::write(&plan, "{}").unwrap();
    let refused = |prog: &str, args: &[&str], what: &str| -> bool {
        let Some(o) = in_sandbox(&d, prog, args) else {
            eprintln!("bwrap cannot make a sandbox here: the test does not run");
            return false;
        };
        assert_eq!(o.status.code(), Some(1), "{args:?}: {}", stderr(&o));
        let err = stderr(&o);
        assert!(err.contains(&format!("{what}: this process is in the sandbox ")) && err.contains("nested sandboxes are forbidden"), "{args:?}: {err}");
        true
    };
    // The commands that start, join or stop sandboxes, and host. The name
    // of the sandbox is not important: also its own sandbox, or no sandbox
    // of that name.
    for args in [&["run", "work", "--", "true"][..], &["run", "nope"], &["host", "--", "true"], &["stop", "work"], &["__container", "work"], &["__netns", "/x"]] {
        if !refused(BIN, args, &format!("sandbox {}", args[0])) {
            return;
        }
    }
    // The backends: run, start and exec.
    let backend = d.path("bin/sandbox-backend-bwrap");
    for mode in ["run", "start", "exec"] {
        refused(&backend, &[mode, &plan], &format!("sandbox-backend-bwrap {mode}"));
    }
    // The commands that start no sandbox work in a sandbox.
    let o = in_sandbox(&d, BIN, &["list", "--names"]).unwrap();
    assert!(o.status.success(), "{}", stderr(&o));
    assert_eq!(stdout(&o), "web\nwork\n");
}
