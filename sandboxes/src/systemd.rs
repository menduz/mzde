//! systemd: the scopes of the runs, the user units, the log and the
//! notification of the container service. The programs come from the systemd package of the
//! configuration (config.json "systemd"), so they agree with the systemd
//! that runs, and the PATH of the caller does not select them.

use std::os::unix::net::{SocketAddr, UnixDatagram};
use std::process::Command;

use crate::config::Config;
use crate::process;

fn program(config: &Config, name: &str) -> String {
    match &config.systemd {
        Some(dir) => format!("{dir}/bin/{name}"),
        None => name.to_string(),
    }
}

pub fn systemctl(config: &Config) -> Command {
    let mut c = Command::new(program(config, "systemctl"));
    c.arg("--user");
    c
}

/// Replace this process with `argv` in a new scope of the user manager. The
/// scope keeps the environment of the caller, and gets SANDBOX__SCOPED=1, so
/// that `argv` does not start a second scope. Returns only with an error.
pub fn exec_scope(config: &Config, slice: &str, unit: &str, description: &str, properties: &[String], argv: &[String]) -> String {
    let args = scope_args(config, slice, unit, description, properties, argv);
    // SAFETY: this process has one thread.
    unsafe { std::env::set_var("SANDBOX__SCOPED", "1") };
    process::exec(&args)
}

/// The command line of systemd-run for a scope.
fn scope_args(config: &Config, slice: &str, unit: &str, description: &str, properties: &[String], argv: &[String]) -> Vec<String> {
    let mut args = vec![
        program(config, "systemd-run"),
        "--user".into(),
        "--scope".into(),
        "--quiet".into(),
        "--collect".into(),
    ];
    for p in properties {
        args.push(format!("--property={p}"));
    }
    args.push(format!("--slice={slice}"));
    args.push(format!("--unit={unit}"));
    args.push(format!("--description={description}"));
    args.push("--".into());
    args.extend(argv.iter().cloned());
    args
}

/// The command line of journalctl for the log of the container service of
/// the sandbox `name`, with the arguments `extra` (for example -f).
pub fn logs_args(config: &Config, name: &str, extra: &[String]) -> Vec<String> {
    let mut args = vec![program(config, "journalctl"), "--user".into(), format!("--unit=sandbox@{name}.service")];
    args.extend(extra.iter().cloned());
    args
}

/// Tell systemd that the service is ready (Type=notify). NOTIFY_SOCKET is a
/// path, or an abstract name with "@".
pub fn notify_ready(socket: Option<&str>) {
    let Some(socket) = socket else {
        return;
    };
    let addr = if let Some(abs) = socket.strip_prefix('@') {
        use std::os::linux::net::SocketAddrExt;
        SocketAddr::from_abstract_name(abs.as_bytes())
    } else {
        SocketAddr::from_pathname(socket)
    };
    let sent = addr.and_then(|a| UnixDatagram::unbound()?.send_to_addr(b"READY=1", &a));
    if let Err(e) = sent {
        crate::warn!("cannot notify systemd: {e}");
    }
}

#[cfg(test)]
mod tests {
    use crate::testutil::config_top;
    use serde_json::json;

    #[test]
    fn scope_command_line() {
        let c = config_top(json!({}), json!({"systemd": "/nix/store/x-systemd"}));
        let a = super::scope_args(
            &c,
            "sandbox-s.slice",
            "sandbox-s-1-2",
            "sandbox s: sh -c x",
            &["Requires=sandbox@s.service".into(), "After=sandbox@s.service".into()],
            &["/p/sandbox".into(), "run".into(), "s".into(), "--".into(), "--weird arg".into()],
        );
        assert_eq!(
            a,
            [
                "/nix/store/x-systemd/bin/systemd-run", "--user", "--scope", "--quiet", "--collect",
                "--property=Requires=sandbox@s.service", "--property=After=sandbox@s.service",
                "--slice=sandbox-s.slice", "--unit=sandbox-s-1-2", "--description=sandbox s: sh -c x",
                "--", "/p/sandbox", "run", "s", "--", "--weird arg",
            ]
        );
        // Without the package in the configuration: PATH.
        let c = config_top(json!({}), json!({}));
        assert_eq!(super::scope_args(&c, "a", "b", "c", &[], &[])[0], "systemd-run");
        assert_eq!(super::systemctl(&c).get_program(), "systemctl");
    }

    #[test]
    fn logs_command_line() {
        let c = config_top(json!({}), json!({"systemd": "/nix/store/x-systemd"}));
        assert_eq!(
            super::logs_args(&c, "code", &["-f".into()]),
            ["/nix/store/x-systemd/bin/journalctl", "--user", "--unit=sandbox@code.service", "-f"]
        );
    }

    #[test]
    fn notify_without_a_socket_does_nothing() {
        super::notify_ready(None);
    }

    #[test]
    fn notify_reaches_socket() {
        let dir = std::env::temp_dir().join(format!("sd-notify-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("notify");
        let rx = std::os::unix::net::UnixDatagram::bind(&path).unwrap();
        super::notify_ready(Some(path.to_str().unwrap()));
        let mut buf = [0u8; 64];
        let n = rx.recv(&mut buf).unwrap();
        assert_eq!(&buf[..n], b"READY=1");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
