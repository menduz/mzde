//! sandbox: start programs in the named sandboxes of programs.sandboxes.
//! Refer to README.md.
//!
//! One program with several names: `sandbox`, `sandbox-init` (the first
//! program in a sandbox), `sandbox-backend-bwrap` and
//! `sandbox-backend-gvisor` (the built-in backends).

// serde_json::json! of the tests needs more than the default.
#![recursion_limit = "512"]

mod backend;
mod cli;
mod color_scheme;
mod command;
mod config;
mod dbus;
mod host;
mod init;
mod net;
mod notify;
mod notify_bridge;
mod notify_host;
mod plan;
mod process;
mod pty;
mod run;
mod systemd;
#[cfg(test)]
mod testutil;
mod tools;
mod util;
mod wayland;

use config::Config;
use plan::{Mode, Request};

/// `sandbox __plan MODE NAME TMP STATE RESOLV [-- CMD...]`: print the plan.
fn cmd_plan(args: &[String]) {
    if args.len() < 5 {
        die!("usage: sandbox __plan MODE NAME TMP STATE RESOLV [-- CMD...]");
    }
    let mode = Mode::parse(&args[0]).unwrap_or_else(|| die!("MODE is run, container or join"));
    let mut command: Vec<String> = args[5..].to_vec();
    if command.first().map(String::as_str) == Some("--") {
        command.remove(0);
    }
    let config = Config::load().unwrap_or_else(|e| die!("{e}"));
    let resolv = if args[4].is_empty() { format!("{}/resolv.conf", args[2]) } else { args[4].clone() };
    let built = plan::build(&Request {
        config: &config,
        name: &args[1],
        mode,
        resolv,
        wayland: Some(format!("{}/wayland-0", args[2])),
        notify: None,
        color_scheme: None,
        state: args[3].clone(),
        command,
    })
    .unwrap_or_else(|e| die!("{e}"));
    println!("{}", serde_json::to_string_pretty(&built.plan).unwrap());
}

fn main() {
    let argv: Vec<String> = std::env::args().collect();
    let name = argv.first().map(|a| a.rsplit('/').next().unwrap_or(a).to_string()).unwrap_or_default();
    let args = &argv[1.min(argv.len())..];
    match name.as_str() {
        "sandbox-init" => init::main(args),
        "sandbox-backend-bwrap" => backend::main("bwrap", args),
        "sandbox-backend-gvisor" => backend::main("gvisor", args),
        _ => {}
    }
    let cmd = args.first().map(String::as_str).unwrap_or("");
    let rest = &args[1.min(args.len())..];
    // The commands that start, join or stop sandboxes, or that give the
    // label of the host, are only for the host (refer to refuse_in_sandbox).
    // The commands with "__" are helpers of `run` on the host, except
    // __command and __yolo: they start the programs of `commands` in a
    // sandbox.
    match cmd {
        "run" | "host" | "stop" => cli::refuse_in_sandbox(&format!("sandbox {cmd}"), true),
        "__container" | "__container_ns" | "__netns" => cli::refuse_in_sandbox(&format!("sandbox {cmd}"), false),
        _ => {}
    }
    match cmd {
        "run" => run::cmd_run(rest),
        "host" => cli::cmd_host(rest),
        "current" => cli::cmd_current(),
        "list" => cli::cmd_list(rest),
        "show" => cli::cmd_show(rest),
        "pick" => cli::cmd_pick(rest),
        "ps" => cli::cmd_ps(),
        "stop" => cli::cmd_stop(rest),
        "logs" => cli::cmd_logs(rest),
        "home" => cli::cmd_home(rest),
        "backends" => cli::cmd_backends(),
        "" | "-h" | "--help" | "help" => print!("{}", cli::USAGE),
        "__container" => run::cmd_container(rest),
        "__container_ns" => net::cmd_container_ns(rest.first().map(String::as_str).unwrap_or_else(|| die!("usage: sandbox __container_ns DIR"))),
        "__netns" => net::cmd_netns(rest.first().map(String::as_str).unwrap_or_else(|| die!("usage: sandbox __netns DIR"))),
        "__command" => command::main(rest),
        "__yolo" => command::yolo(rest),
        "__notify" => notify::main(rest),
        "__notify_host" => notify_host::main(rest),
        "__notify_bridge" => notify_bridge::main(rest),
        "__color_scheme_host" => color_scheme::host_main(rest),
        "__color_scheme" => color_scheme::main(rest),
        "__plan" => cmd_plan(rest),
        _ => {
            eprint!("{}", cli::USAGE);
            std::process::exit(2);
        }
    }
}
