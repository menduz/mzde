//! The small subcommands: list, show, pick, ps, stop, host, current, home,
//! backends.

use serde_json::Value;
use std::io::{IsTerminal, Write};
use std::process::{Command, Stdio};

use crate::config::{Config, Net};
use crate::plan::{self, Mode, Request};
use crate::process;
use crate::run::{container_dir, mktemp_dir};
use crate::systemd;
use crate::util;
use crate::die;

pub const USAGE: &str = "Usage: sandbox COMMAND [ARGUMENTS]

  list [--names|--json]   Show the sandboxes.
  show NAME               Show the plan and the backend command of NAME.
  run [--need-cwd] NAME [--] [CMD...]
                          Run CMD in the sandbox NAME. The default is $SHELL.
                          CMD starts in the current directory if the sandbox
                          has it, else in the home directory. With
                          --need-cwd, the command stops if the sandbox does
                          not have the current directory.
  pick [--host] [--gui]   Select a sandbox in the menu and print its name.
                          With --host, the menu also shows \"host\". With --gui,
                          the menu shows only the sandboxes with gui.
  ps                      Show the programs that run in sandboxes, and the
                          containers.
  stop NAME               Stop all programs of the sandbox NAME.
  logs NAME [ARGS...]     Show the log of the container of NAME (journalctl
                          --user -u sandbox@NAME, with ARGS, for example -f).
  host [--] [CMD...]      Run CMD on the host, not in a sandbox, with the label
                          of the host (SANDBOX_NAME=host), in its own scope
                          below app.slice. The default is $SHELL.
  current                 Print the sandbox of this process, or \"host\".
                          SANDBOX_NAME gives the sandbox. On a host where a
                          program set SANDBOX_NAME, PID 1 is systemd: then
                          the command prints \"host\".
  home NAME               Print the home directory of NAME on the host.
  backends                Show the backends.

Environment:
  SANDBOX_CONFIG          The configuration file (default /etc/sandboxes/config.json).
  SANDBOX_BACKEND         Use this backend in place of the backend of the sandbox.
  SANDBOX_NO_SCOPE=1      Do not start a systemd scope.
";

fn load() -> Config {
    Config::load().unwrap_or_else(|e| die!("{e}"))
}

/// The configuration as JSON values, for list.
fn load_raw() -> Value {
    let path = util::env("SANDBOX_CONFIG").unwrap_or_else(|| crate::config::DEFAULT_PATH.into());
    let text = std::fs::read(&path).unwrap_or_else(|e| die!("cannot read {path}: {e}"));
    serde_json::from_slice(&text).unwrap_or_else(|e| die!("{path}: {e}"))
}

/// The sandbox of this process. A program on the host can set SANDBOX_NAME,
/// thus PID 1 must not be systemd: in a sandbox, PID 1 is bwrap (bwrap) or
/// the first program of the sandbox (gVisor).
pub fn current() -> String {
    current_from(util::env("SANDBOX_NAME"), std::fs::read_to_string("/proc/1/comm").ok())
}

/// The sandbox from SANDBOX_NAME and the name of process 1.
fn current_from(name: Option<String>, pid1: Option<String>) -> String {
    let name = name.filter(|s| !s.is_empty()).unwrap_or_else(|| "host".into());
    if name != "host" && pid1.is_some_and(|c| c.trim_end() == "systemd") {
        return "host".into();
    }
    name
}

/// Stop with an error in a sandbox: a sandbox must not start, join or stop
/// sandboxes (nested sandboxes are forbidden), and `host` there would give
/// the label of the host to a program of the sandbox. Nested bwraps and
/// namespaces stay possible: they can only remove access.
///
/// In a sandbox of bwrap, PID 1 is bwrap, and the programs of the sandbox
/// cannot change that. In gVisor, SANDBOX_NAME tells it (`by_label`). The
/// helpers of `run` on the host (the backends, __netns) get SANDBOX_NAME
/// from `run`, and where PID 1 is not systemd (a Nix build) the label does
/// not tell a sandbox: for them, `by_label` is false.
///
/// This is not a security boundary: a sandbox that goes around this check
/// gets a sandbox in itself, with no more access than it has.
pub fn refuse_in_sandbox(what: &str, by_label: bool) {
    let label = util::env("SANDBOX_NAME").filter(|s| !s.is_empty() && s != "host");
    if let Some(found) = in_sandbox_from(label.clone().filter(|_| by_label), std::fs::read_to_string("/proc/1/comm").ok()) {
        // The label only names the sandbox in the message.
        let name = label.unwrap_or(found);
        die!("{what}: this process is in the sandbox {name}, and nested sandboxes are forbidden. Start it on the host.");
    }
}

/// The sandbox of this process, if it is in a sandbox.
fn in_sandbox_from(name: Option<String>, pid1: Option<String>) -> Option<String> {
    let bwrap = pid1.as_deref().is_some_and(|c| c.trim_end() == "bwrap");
    match current_from(name, pid1) {
        n if n != "host" => Some(n),
        _ if bwrap => Some("(unknown)".into()),
        _ => None,
    }
}

pub fn cmd_current() {
    println!("{}", current());
}

/// A block in the color, for a terminal.
fn swatch(color: &str) -> String {
    swatch_on(color, std::io::stdout().is_terminal())
}

fn swatch_on(color: &str, terminal: bool) -> String {
    let c = color.trim_start_matches('#');
    if c.len() != 6 || !terminal {
        return String::new();
    }
    let h = |i: usize| u8::from_str_radix(&c[i..i + 2], 16).unwrap_or(0);
    format!("\x1b[48;2;{};{};{}m  \x1b[0m ", h(0), h(2), h(4))
}

pub fn cmd_list(args: &[String]) {
    // The same checks as for the other commands; the output reads the JSON.
    load();
    let raw = load_raw();
    let envs = raw["environments"].as_object().cloned().unwrap_or_default();
    match args.first().map(String::as_str) {
        Some("--names") => {
            for k in envs.keys() {
                println!("{k}");
            }
            return;
        }
        Some("--json") => {
            println!("{}", serde_json::to_string_pretty(&raw["environments"]).unwrap());
            return;
        }
        _ => {}
    }
    for (name, e) in &envs {
        let (net, caps) = summary(e);
        let color = e["color"].as_str().unwrap_or("");
        println!(
            "{}{:<12} {:<9} {:<7} {:<10} {:<36} {}",
            swatch(color),
            name,
            color,
            e["backend"].as_str().unwrap_or(""),
            net,
            caps,
            e["description"].as_str().unwrap_or("")
        );
    }
}

/// The network and the capabilities of a sandbox, for `sandbox list`.
fn summary(e: &Value) -> (&'static str, String) {
    const CAPS: &[&str] = &[
        "gui", "x11", "audio", "ssh", "gpg", "git", "claude", "codex", "gemini", "hermes", "opencode", "zed", "direnv", "docker", "dbus",
        "nix", "fuse", "kvm", "nvidia", "libvirt", "allowHome", "privacyFilter", "yolo",
    ];
    let b = |k: &str| e[k].as_bool().unwrap_or(false);
    let net = if b("disableNetworking") {
        "none"
    } else if b("shareHostNetwork") {
        "host"
    } else if !e["socksProxy"].is_null() {
        "socks"
    } else if !b("internetAccess") {
        "lan-only"
    } else if !b("lanAccess") {
        "inet-only"
    } else {
        "internet"
    };
    let caps: Vec<String> = CAPS
        .iter()
        .filter_map(|&k| match &e[k] {
            Value::Bool(true) => Some(k.to_string()),
            Value::String(s) if s == "private" || s == "read" => Some(k.to_string()),
            Value::String(s) if s == "host" => Some(format!("{k}:host")),
            Value::String(s) if s == "write" => Some(format!("{k}:rw")),
            _ => None,
        })
        .collect();
    (net, caps.join(","))
}

/// Text for Pango markup.
fn escape_markup(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// The lines of the menu: one for each sandbox (with --gui, only the
/// sandboxes with gui), and "host" first with --host.
fn menu_lines(config: &Config, host: bool, gui: bool) -> String {
    let markup = config.menu.markup;
    let mut lines = String::new();
    if host {
        if markup {
            lines += &format!("<span background=\"{}\" foreground=\"#ffffff\"> host </span>  no sandbox\n", config.host.color);
        } else {
            lines += "host  no sandbox\n";
        }
    }
    for (name, e) in &config.environments {
        if gui && !e.gui {
            continue;
        }
        if markup {
            lines += &format!(
                "<span background=\"{}\" foreground=\"#ffffff\"> {name} </span>  {}\n",
                e.color,
                escape_markup(&e.description)
            );
        } else {
            lines += &format!("{name}  {}\n", e.description);
        }
    }
    lines
}

/// The name in the line that the menu wrote: the first word without the
/// markup.
fn choice_of(text: &str) -> Option<String> {
    let mut plain = String::new();
    let mut in_tag = false;
    for c in text.chars() {
        match c {
            '<' => in_tag = true,
            '>' if in_tag => in_tag = false,
            _ if !in_tag => plain.push(c),
            _ => {}
        }
    }
    plain.split_whitespace().next().map(str::to_string)
}

pub fn cmd_pick(args: &[String]) {
    let (mut host, mut gui) = (false, false);
    for a in args {
        match a.as_str() {
            "--host" => host = true,
            "--gui" => gui = true,
            _ => die!("usage: sandbox pick [--host] [--gui]"),
        }
    }
    let config = load();
    let lines = menu_lines(&config, host, gui);
    // The menu command of the configuration.
    let mut child = Command::new("/bin/sh")
        .args(["-c", &config.menu.command])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap_or_else(|e| die!("the menu: {e}"));
    let _ = child.stdin.take().unwrap().write_all(lines.as_bytes());
    let out = child.wait_with_output().unwrap_or_else(|e| die!("the menu: {e}"));
    if !out.status.success() {
        std::process::exit(1);
    }
    let Some(choice) = choice_of(&String::from_utf8_lossy(&out.stdout)) else {
        std::process::exit(1);
    };
    let choice = choice.as_str();
    if choice != "host" && !config.environments.contains_key(choice) {
        die!("no sandbox has the name '{choice}'");
    }
    println!("{choice}");
}

/// The value of a property of a user unit.
fn unit_property(config: &Config, unit: &str, prop: &str) -> String {
    systemd::systemctl(config)
        .args(["show", "-p", prop, "--value", unit])
        .stderr(Stdio::null())
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim_end().to_string())
        .unwrap_or_default()
}

/// The running units of a type with names that match a pattern.
fn units(config: &Config, kind: &str, pattern: &str) -> Vec<String> {
    let out = systemd::systemctl(config)
        .args(["list-units", &format!("--type={kind}"), "--state=running", "--no-legend", "--plain", pattern])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default();
    out.lines().filter_map(|l| l.split_whitespace().next().map(str::to_string)).collect()
}

pub fn cmd_ps() {
    let config = load();
    let config_now = util::readlink_f(&util::env("SANDBOX_CONFIG").unwrap_or_else(|| crate::config::DEFAULT_PATH.into()));
    println!("{:<12} {:<8} COMMAND", "SANDBOX", "PID");
    // The containers. A container that started with a different
    // configuration has the mounts of that configuration.
    for unit in units(&config, "service", "sandbox@*") {
        let name = container_of(&unit);
        let pid = unit_property(&config, &unit, "MainPID");
        let started = std::fs::read_to_string(format!("{}/config", container_dir(name))).unwrap_or_default();
        let stale = if stale(&started, &config_now) { " (stale: the configuration changed; sandbox stop to restart it)" } else { "" };
        println!("{name:<12} {pid:<8} [container]{stale}");
    }
    for unit in units(&config, "scope", "sandbox-*") {
        let name = sandbox_of_scope(&unit);
        let pid = unit_property(&config, &unit, "MainPID");
        let desc = unit_property(&config, &unit, "Description");
        println!("{name:<12} {pid:<8} {}", command_of(name, &desc));
    }
}

/// The sandbox of the service sandbox@NAME.service.
fn container_of(unit: &str) -> &str {
    unit.trim_start_matches("sandbox@").trim_end_matches(".service")
}

/// The sandbox of the scope sandbox-NAME-PID-RANDOM.scope. A name has no
/// "-" (module.nix asserts it).
fn sandbox_of_scope(unit: &str) -> &str {
    let name = unit.trim_start_matches("sandbox-");
    name.split('-').next().unwrap_or(name)
}

/// The command in the description "sandbox NAME: CMD" of a scope.
fn command_of<'a>(name: &str, desc: &'a str) -> &'a str {
    desc.strip_prefix(&format!("sandbox {name}: ")).unwrap_or(desc)
}

/// Whether a container started with a different configuration file (the
/// files after links).
fn stale(started: &str, now: &str) -> bool {
    !started.is_empty() && started != now
}

pub fn cmd_stop(args: &[String]) {
    if args.len() != 1 {
        die!("usage: sandbox stop NAME");
    }
    let config = load();
    let st = systemd::systemctl(&config)
        .args(["stop", &format!("sandbox-{}.slice", args[0])])
        .status()
        .unwrap_or_else(|e| die!("systemctl: {e}"));
    std::process::exit(process::code(st));
}

/// Show the log of the container of a sandbox: journalctl with the
/// arguments after NAME.
pub fn cmd_logs(args: &[String]) -> ! {
    let Some(name) = args.first() else {
        die!("usage: sandbox logs NAME [JOURNALCTL-ARGS...]");
    };
    let config = load();
    config.env(name).unwrap_or_else(|e| die!("{e}"));
    let err = process::exec(&systemd::logs_args(&config, name, &args[1..]));
    die!("{err}");
}

/// Run a program on the host with the label of the host. A shell prompt
/// reads the label as for a sandbox. A window manager finds the host from
/// the cgroup. The scope below app.slice takes the program out of the
/// cgroup of the caller (for example the window manager). The name follows
/// the XDG convention app-<launcher>-<id>-<random>.scope.
pub fn cmd_host(args: &[String]) -> ! {
    let mut args = args.to_vec();
    if args.first().map(String::as_str) == Some("--") {
        args.remove(0);
    }
    if args.is_empty() {
        args.push(util::env("SHELL").unwrap_or_else(|| "/bin/sh".into()));
    }
    let config = load();
    let scoped = util::env("SANDBOX__SCOPED").filter(|v| !v.is_empty()).is_some();
    let no_scope = util::env("SANDBOX_NO_SCOPE").filter(|v| !v.is_empty()).is_some();
    let manager = util::is_socket(&format!("{}/systemd/private", util::env("XDG_RUNTIME_DIR").unwrap_or_else(|| "/nonexistent".into())));
    if !scoped && !no_scope && manager {
        let mut inner = vec![process::self_exe(), "host".into(), "--".into()];
        inner.extend(args.iter().cloned());
        let err = systemd::exec_scope(
            &config,
            "app.slice",
            &format!("app-sandbox-host-{}-{}", std::process::id(), process::random15()),
            &format!("host: {}", args.join(" ")),
            &[],
            &inner,
        );
        die!("{err}");
    }
    // SAFETY: this process has one thread.
    unsafe {
        std::env::set_var("SANDBOX_NAME", "host");
        std::env::set_var("SANDBOX_COLOR", &config.host.color);
        // A program on the host can start runs, and each run must start its
        // scope.
        std::env::remove_var("SANDBOX_BACKEND");
        std::env::remove_var("SANDBOX__SCOPED");
    }
    die!("{}", process::exec(&args));
}

pub fn cmd_home(args: &[String]) {
    let Some(name) = args.first() else {
        die!("usage: sandbox home NAME");
    };
    let config = load();
    let e = config.env(name).unwrap_or_else(|e| die!("{e}"));
    println!("{}", util::expand(&e.home_directory, &util::home()));
}

pub fn cmd_backends() {
    let config = load();
    for (k, v) in &config.backends {
        println!("{k}\t{v}");
    }
}

pub fn cmd_show(args: &[String]) {
    if args.len() != 1 {
        die!("usage: sandbox show NAME");
    }
    let config = load();
    let built = plan::build(&Request {
        config: &config,
        name: &args[0],
        mode: Mode::Run,
        resolv: "$TMP/resolv.conf".into(),
        wayland: Some("$TMP/wayland-0".into()),
        state: String::new(),
        command: vec!["CMD".into()],
    })
    .unwrap_or_else(|e| die!("{e}"));
    let (bname, bpath) = &built.backend;
    println!("backend: {bname} ({bpath})");
    println!(
        "network: {}",
        match built.net {
            Net::None => "none",
            Net::Host => "host",
            Net::Private => "private",
        }
    );
    println!("{}", serde_json::to_string_pretty(&built.plan).unwrap());
    let dir = mktemp_dir(&format!("{}/sandbox-show", util::files_dir())).unwrap_or_else(|e| die!("{e}"));
    let path = format!("{dir}/plan.json");
    built.plan.write(&path).unwrap_or_else(|e| die!("{e}"));
    let _ = Command::new(bpath).args(["show", &path]).status();
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::config_top;
    use serde_json::json;

    #[test]
    fn current_is_the_label_except_on_the_host() {
        let s = |v: &str| Some(v.to_string());
        assert_eq!(current_from(None, s("bwrap\n")), "host");
        assert_eq!(current_from(s(""), s("bwrap\n")), "host");
        assert_eq!(current_from(s("code"), s("bwrap\n")), "code");
        // gVisor: process 1 is the first program.
        assert_eq!(current_from(s("code"), s("sandbox-init\n")), "code");
        // A program on the host can set SANDBOX_NAME: process 1 is systemd.
        assert_eq!(current_from(s("code"), s("systemd\n")), "host");
        assert_eq!(current_from(s("code"), None), "code");
    }

    #[test]
    fn in_a_sandbox_by_the_label_or_by_process_1() {
        let s = |v: &str| Some(v.to_string());
        assert_eq!(in_sandbox_from(s("code"), s("bwrap\n")), s("code"));
        // bwrap: a program can remove SANDBOX_NAME, or set it to "host",
        // but PID 1 stays bwrap.
        assert_eq!(in_sandbox_from(None, s("bwrap\n")), s("(unknown)"));
        assert_eq!(in_sandbox_from(s("host"), s("bwrap\n")), s("(unknown)"));
        // gVisor: process 1 is the first program.
        assert_eq!(in_sandbox_from(s("code"), s("sh\n")), s("code"));
        // The host: process 1 is systemd, also with SANDBOX_NAME.
        assert_eq!(in_sandbox_from(s("code"), s("systemd\n")), None);
        assert_eq!(in_sandbox_from(None, s("systemd\n")), None);
        // The build of the package: process 1 is the builder.
        assert_eq!(in_sandbox_from(None, s("bash\n")), None);
    }

    fn two() -> Config {
        let mut t = crate::testutil::env_json();
        t["gui"] = json!(true);
        t["color"] = json!("#800080");
        t["description"] = json!("a <b> & c");
        config_top(json!({"description": "first"}), json!({"environments": {"t": t}}))
    }

    #[test]
    fn menu_without_markup() {
        let c = two();
        assert_eq!(menu_lines(&c, false, false), "s  first\nt  a <b> & c\n");
        assert_eq!(menu_lines(&c, true, false), "host  no sandbox\ns  first\nt  a <b> & c\n");
        // --gui: only the sandboxes with gui (s has no gui here).
        assert_eq!(menu_lines(&c, true, true), "host  no sandbox\nt  a <b> & c\n");
    }

    #[test]
    fn menu_with_markup() {
        let mut c = two();
        c.menu.markup = true;
        let l = menu_lines(&c, true, false);
        let lines: Vec<&str> = l.lines().collect();
        assert_eq!(lines[0], "<span background=\"#404040\" foreground=\"#ffffff\"> host </span>  no sandbox");
        assert_eq!(lines[2], "<span background=\"#800080\" foreground=\"#ffffff\"> t </span>  a &lt;b&gt; &amp; c");
        // The choice of each line is its name.
        assert_eq!(lines.iter().map(|l| choice_of(l).unwrap()).collect::<Vec<_>>(), ["host", "s", "t"]);
    }

    #[test]
    fn choice_of_the_menu() {
        assert_eq!(choice_of("t  a b\n").as_deref(), Some("t"));
        assert_eq!(choice_of("  <span x=\"y\"> web </span>  d\n").as_deref(), Some("web"));
        assert_eq!(choice_of(""), None);
        assert_eq!(choice_of("\n"), None);
    }

    #[test]
    fn list_summary() {
        let mut e = crate::testutil::env_json();
        assert_eq!(summary(&e), ("internet", "x11,dbus".to_string()));
        e["gui"] = json!(true);
        e["ssh"] = json!("write");
        e["git"] = json!("read");
        e["dbus"] = json!("host");
        e["yolo"] = json!(true);
        e["lanAccess"] = json!(false);
        assert_eq!(summary(&e), ("inet-only", "gui,x11,ssh:rw,git,dbus:host,yolo".to_string()));
        e["internetAccess"] = json!(false);
        assert_eq!(summary(&e).0, "lan-only");
        e["socksProxy"] = json!("h:1");
        assert_eq!(summary(&e).0, "socks");
        e["shareHostNetwork"] = json!(true);
        assert_eq!(summary(&e).0, "host");
        e["disableNetworking"] = json!(true);
        assert_eq!(summary(&e).0, "none");
    }

    #[test]
    fn units_of_ps() {
        assert_eq!(container_of("sandbox@code.service"), "code");
        assert_eq!(sandbox_of_scope("sandbox-code-123-4567.scope"), "code");
        assert_eq!(sandbox_of_scope("sandbox-my_box2-1-2.scope"), "my_box2");
        assert_eq!(command_of("code", "sandbox code: zeditor ."), "zeditor .");
        assert_eq!(command_of("code", "other"), "other");
        assert!(stale("/nix/store/a-config.json", "/nix/store/b-config.json"));
        assert!(!stale("/nix/store/a-config.json", "/nix/store/a-config.json"));
        assert!(!stale("", "/nix/store/b-config.json"));
    }

    #[test]
    fn swatch_needs_a_terminal_and_six_digits() {
        assert_eq!(swatch_on("#008080", false), "");
        assert_eq!(swatch_on("#008080", true), "\x1b[48;2;0;128;128m  \x1b[0m ");
        assert_eq!(swatch_on("#fff", true), "");
        assert_eq!(escape_markup("<a&b>"), "&lt;a&amp;b&gt;");
    }
}
