#![recursion_limit = "512"]
//! The subcommands of `sandbox` that start no sandbox: list, home,
//! backends, show, pick, __plan, the usage and the errors.

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
    let o = d.run(&["run"]);
    assert_eq!(stderr(&o), "sandbox: usage: sandbox run [--need-cwd] NAME [--] [CMD...]\n");
    let o = d.run(&["run", "nope", "--", "true"]);
    assert_eq!(stderr(&o), "sandbox: no sandbox has the name 'nope'. Run 'sandbox list'.\n");
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
