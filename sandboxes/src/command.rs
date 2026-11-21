//! The programs that run only in sandboxes (`commands` of a sandbox), and
//! the `claude` of `yolo`.
//!
//! The module installs a wrapper for each command: `sandbox __command NAME`.
//! In a sandbox with the command, it starts the program; elsewhere it stops
//! with an error.

use std::fs;

use crate::cli::current;
use crate::config::Config;
use crate::process;
use crate::tools;
use crate::util;

fn fail(name: &str, msg: &str) -> ! {
    eprintln!("{name}: {msg}");
    std::process::exit(1)
}

/// The targets of the `paths` of a sandbox ("~" is $HOME), without empty
/// segments, as `expand` makes them: the tests of the current directory
/// compare the strings.
fn path_targets(paths: &[String], home: &str) -> Vec<String> {
    paths
        .iter()
        .map(|p| {
            let p = p.strip_prefix("rw:").unwrap_or(p);
            let p = p.strip_prefix("ro:").unwrap_or(p);
            let target = p.rsplit(':').next().unwrap_or(p);
            util::expand(target, home)
        })
        .collect()
}

/// The kind of a path on the host, for cwdOnly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Dir,
    File,
    Missing,
}

fn kind_of(p: &str) -> Kind {
    match fs::metadata(p) {
        Ok(m) if m.is_dir() => Kind::Dir,
        Ok(_) => Kind::File,
        Err(_) => Kind::Missing,
    }
}

/// The program to start for the command `name` in the sandbox `here`
/// ("host" on the host), or the message of the error.
pub fn argv_for(config: &Config, name: &str, here: &str, args: &[String], home: &str, pwd: &str, kind: &dyn Fn(&str) -> Kind) -> Result<Vec<String>, String> {
    let uses = config.commands.get(name).ok_or_else(|| format!("{name} is not a command of the sandboxes"))?;
    let Some(c) = uses.get(here) else {
        let envs: Vec<&str> = uses.keys().map(String::as_str).collect();
        let where_ = if envs.len() == 1 { format!("the sandbox {}", envs[0]) } else { format!("the sandboxes {}", envs.join(", ")) };
        let not = if here == "host" { "on the host".to_string() } else { format!("in the sandbox {here}") };
        return Err(format!("{name} runs only in {where_}, not {not}. Start it in a terminal of a sandbox (sandbox run NAME)."));
    };
    let mut argv = vec![c.program.clone()];
    argv.extend(args.iter().cloned());
    if !c.cwd_only && c.bwrap_args.is_empty() {
        return Ok(argv);
    }

    // The program in a nested bwrap.
    let mut bw: Vec<String> = vec![tools::bwrap().into(), "--dev-bind".into(), "/".into(), "/".into()];
    if c.cwd_only {
        // Hide the paths of the sandbox: an empty tmpfs on a directory, an
        // empty file on a file. Then mount the current directory again. A
        // path that has the configuration of a program (the options claude,
        // codex, git, ...) would hide it: then the command does not start.
        let e = config.environments.get(here).ok_or_else(|| format!("no sandbox has the name '{here}'"))?;
        let mut has_cwd = false;
        for d in path_targets(&e.paths, home) {
            for m in &e.config_mounts {
                let cm = format!("{home}/{}", m.path.trim_end_matches('/'));
                if d == "/" || cm == d || cm.starts_with(&format!("{d}/")) {
                    return Err(format!("the path {d} of the sandbox {here} has the configuration {cm}, and cwdOnly would hide it."));
                }
            }
            match kind(&d) {
                Kind::Dir => {
                    bw.extend(["--tmpfs".into(), d.clone()]);
                    if d == "/" || pwd == d || pwd.starts_with(&format!("{d}/")) {
                        has_cwd = true;
                    }
                }
                Kind::File => bw.extend(["--ro-bind".into(), "/dev/null".into(), d.clone()]),
                Kind::Missing => {}
            }
        }
        if has_cwd {
            bw.extend(["--bind".into(), pwd.to_string(), pwd.to_string()]);
        }
    }
    bw.extend(c.bwrap_args.iter().cloned());
    bw.extend(["--die-with-parent".into(), "--chdir".into(), pwd.to_string(), "--".into()]);
    bw.extend(argv);
    Ok(bw)
}

/// `sandbox __command NAME [ARGS...]`.
pub fn main(args: &[String]) -> ! {
    let Some(name) = args.first() else {
        eprintln!("usage: sandbox __command NAME [ARGS...]");
        std::process::exit(2);
    };
    let config = Config::load().unwrap_or_else(|e| fail(name, &e));
    match argv_for(&config, name, &current(), &args[1..], &util::home(), &util::logical_pwd(), &kind_of) {
        Ok(argv) => fail(name, &process::exec(&argv)),
        Err(msg) => fail(name, &msg),
    }
}

/// `sandbox __yolo DIR [ARGS...]`: the `claude` of `yolo`, in DIR. It starts
/// the next claude in PATH with --dangerously-skip-permissions.
pub fn yolo(args: &[String]) -> ! {
    let Some(dir) = args.first() else {
        eprintln!("usage: sandbox __yolo DIR [ARGS...]");
        std::process::exit(2);
    };
    let path = util::env("PATH").unwrap_or_default();
    let path: Vec<&str> = path.split(':').filter(|d| d != dir).collect();
    // SAFETY: this process has one thread.
    unsafe { std::env::set_var("PATH", path.join(":")) };
    let mut argv = vec!["claude".to_string(), "--dangerously-skip-permissions".into()];
    argv.extend(args[1..].iter().cloned());
    fail("claude", &process::exec(&argv));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::config_top;
    use serde_json::json;

    /// Two sandboxes with the command whereami (s with cwdOnly), and t.
    fn conf(s_patch: serde_json::Value, cmd: serde_json::Value) -> Config {
        let mut t = crate::testutil::env_json();
        t["paths"] = json!(["~/work"]);
        config_top(
            s_patch,
            json!({
                "environments": {"t": t},
                "commands": {"whereami": {"s": cmd, "t": {"program": "/p/whereami", "cwdOnly": false, "bwrapArgs": []}}},
            }),
        )
    }

    fn kinds(p: &str) -> Kind {
        match p {
            "/h/work" | "/h/code" => Kind::Dir,
            "/h/notes" => Kind::File,
            _ => Kind::Missing,
        }
    }

    fn argv(c: &Config, here: &str, pwd: &str) -> Result<Vec<String>, String> {
        argv_for(c, "whereami", here, &["x".into()], "/h", pwd, &kinds)
    }

    fn plain() -> serde_json::Value {
        json!({"program": "/p/whereami", "cwdOnly": false, "bwrapArgs": []})
    }

    #[test]
    fn starts_the_program_in_its_sandboxes() {
        let c = conf(json!({}), plain());
        assert_eq!(argv(&c, "t", "/h").unwrap(), ["/p/whereami", "x"]);
        assert_eq!(argv(&c, "s", "/h").unwrap(), ["/p/whereami", "x"]);
    }

    #[test]
    fn stops_elsewhere_with_the_sandboxes_in_the_message() {
        let c = conf(json!({}), plain());
        assert_eq!(
            argv(&c, "host", "/h").unwrap_err(),
            "whereami runs only in the sandboxes s, t, not on the host. Start it in a terminal of a sandbox (sandbox run NAME)."
        );
        assert_eq!(
            argv(&c, "other", "/h").unwrap_err(),
            "whereami runs only in the sandboxes s, t, not in the sandbox other. Start it in a terminal of a sandbox (sandbox run NAME)."
        );
        let one = config_top(json!({}), json!({"commands": {"only": {"s": plain()}}}));
        assert!(argv_for(&one, "only", "host", &[], "/h", "/h", &kinds).unwrap_err().starts_with("only runs only in the sandbox s, not on the host."));
        assert_eq!(argv_for(&one, "nope", "s", &[], "/h", "/h", &kinds).unwrap_err(), "nope is not a command of the sandboxes");
    }

    #[test]
    fn cwd_only_hides_the_paths_and_mounts_the_current_directory() {
        let c = conf(json!({"paths": ["~/work", "~/code/", "ro:~/notes", "~/missing"]}), json!({"program": "/p/whereami", "cwdOnly": true, "bwrapArgs": []}));
        let a = argv(&c, "s", "/h/work/a").unwrap();
        assert_eq!(
            a,
            [
                tools::bwrap(), "--dev-bind", "/", "/",
                "--tmpfs", "/h/work",
                "--tmpfs", "/h/code",
                "--ro-bind", "/dev/null", "/h/notes",
                "--bind", "/h/work/a", "/h/work/a",
                "--die-with-parent", "--chdir", "/h/work/a", "--",
                "/p/whereami", "x",
            ]
        );
        // Outside of the paths: all paths are hidden, and nothing comes back.
        let a = argv(&c, "s", "/h").unwrap();
        assert!(!a.iter().any(|x| x == "--bind"));
        assert!(a.windows(2).any(|w| w == ["--chdir", "/h"]));
        // A directory with the name of a path as a prefix is not in it.
        assert!(!argv(&c, "s", "/h/workshop").unwrap().iter().any(|x| x == "--bind"));
    }

    #[test]
    fn cwd_only_refuses_to_hide_the_configuration_of_a_program() {
        let c = conf(
            json!({"paths": ["~"], "configMounts": [{"mode": "rw", "path": ".claude/", "remap": ""}]}),
            json!({"program": "/p/whereami", "cwdOnly": true, "bwrapArgs": []}),
        );
        assert_eq!(argv(&c, "s", "/h").unwrap_err(), "the path /h of the sandbox s has the configuration /h/.claude, and cwdOnly would hide it.");
        // A path below the configuration is not a problem.
        let c = conf(
            json!({"paths": ["~/work"], "configMounts": [{"mode": "rw", "path": ".claude/", "remap": ""}]}),
            json!({"program": "/p/whereami", "cwdOnly": true, "bwrapArgs": []}),
        );
        assert!(argv(&c, "s", "/h/work").is_ok());
        // The path "/" has every configuration (as in the generated bash).
        let c = conf(
            json!({"paths": ["/"], "configMounts": [{"mode": "ro", "path": ".gitconfig", "remap": ""}]}),
            json!({"program": "/p/whereami", "cwdOnly": true, "bwrapArgs": []}),
        );
        assert!(argv(&c, "s", "/h").unwrap_err().contains("cwdOnly would hide it"));
    }

    #[test]
    fn bwrap_args_without_cwd_only() {
        let c = conf(json!({}), json!({"program": "/p/whereami", "cwdOnly": false, "bwrapArgs": ["--unshare-uts", "--hostname", "n"]}));
        assert_eq!(
            argv(&c, "s", "/h/x").unwrap(),
            [tools::bwrap(), "--dev-bind", "/", "/", "--unshare-uts", "--hostname", "n", "--die-with-parent", "--chdir", "/h/x", "--", "/p/whereami", "x"]
        );
    }

    #[test]
    fn targets() {
        let p = ["~/code/".to_string(), "ro:/a:~/b".into(), "rw:/x".into(), "~".into()];
        assert_eq!(path_targets(&p, "/h"), ["/h/code", "/h/b", "/x", "/h"]);
    }
}
