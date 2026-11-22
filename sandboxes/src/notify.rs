//! The notifications of the container of a sandbox. The service
//! sandbox@NAME runs `sandbox __notify started NAME` when the container is
//! ready (ExecStartPost) and `sandbox __notify stopped NAME` when it stops
//! (ExecStopPost). systemd runs ExecStopPost also when the container stops
//! with an error, or when a signal or the OOM killer kills it. Then
//! SERVICE_RESULT, EXIT_CODE and EXIT_STATUS tell why (systemd.exec(5)).
//!
//! notify-send sends the notification on the session bus of the host. A
//! notification of a sandbox replaces the last notification of the same
//! sandbox (the stack tag of dunst, and the synchronous hint of other
//! servers). A notification never makes the service fail: an error is only
//! a message in the journal.

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::util::{self, Result};
use crate::{die, tools, warn};

/// A notification.
#[derive(Debug, PartialEq)]
pub struct Message {
    pub urgency: &'static str,
    pub summary: String,
    pub body: String,
}

/// The notification of the event `event` ("started" or "stopped") of the
/// container of `name`. `result`, `code` and `status` are SERVICE_RESULT,
/// EXIT_CODE and EXIT_STATUS of systemd. None: the event is not known.
pub fn message(event: &str, name: &str, result: Option<&str>, code: Option<&str>, status: Option<&str>) -> Option<Message> {
    let normal = |summary: String| Message { urgency: "normal", summary, body: String::new() };
    let critical = |summary: String, why: String| Message {
        urgency: "critical",
        summary,
        body: format!("{why}\nsandbox logs {name}"),
    };
    match event {
        "started" => Some(normal(format!("Sandbox {name} started"))),
        "stopped" => Some(match result.unwrap_or("success") {
            // The last run stopped, or `sandbox stop`: systemd sent SIGTERM.
            "success" => normal(format!("Sandbox {name} stopped")),
            "signal" | "core-dump" => critical(format!("Sandbox {name} killed"), format!("signal {}", status.unwrap_or("?"))),
            "oom-kill" => critical(format!("Sandbox {name} killed"), "out of memory (OOM killer)".into()),
            "exit-code" => critical(format!("Sandbox {name} stopped with an error"), format!("exit code {}", status.unwrap_or("?"))),
            other => {
                let detail = match (code, status) {
                    (Some(c), Some(s)) => format!(" ({c} {s})"),
                    _ => String::new(),
                };
                critical(format!("Sandbox {name} stopped"), format!("{other}{detail}"))
            }
        }),
        _ => None,
    }
}

/// The arguments of notify-send for `m`.
fn args(name: &str, m: &Message) -> Vec<String> {
    vec![
        "--app-name=sandbox".into(),
        format!("--urgency={}", m.urgency),
        format!("--hint=string:x-dunst-stack-tag:sandbox-{name}"),
        format!("--hint=string:x-canonical-private-synchronous:sandbox-{name}"),
        "--".into(),
        m.summary.clone(),
        m.body.clone(),
    ]
}

/// `sandbox __notify started|stopped NAME`. It always exits with 0.
pub fn main(args: &[String]) -> ! {
    let [event, name] = args else {
        die!("usage: sandbox __notify started|stopped NAME");
    };
    let env = |k: &str| util::env(k).filter(|v| !v.is_empty());
    let m = message(event, name, env("SERVICE_RESULT").as_deref(), env("EXIT_CODE").as_deref(), env("EXIT_STATUS").as_deref())
        .unwrap_or_else(|| die!("usage: sandbox __notify started|stopped NAME"));
    if let Err(e) = send(name, &m) {
        warn!("the notification \"{}\": {e}", m.summary);
    }
    std::process::exit(0)
}

/// Run notify-send. A notification server that does not answer does not
/// stop the service (a stop at the end of the session): after 5 s, kill
/// notify-send.
fn send(name: &str, m: &Message) -> Result<()> {
    let mut child = Command::new(tools::notify_send())
        .args(args(name, m))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .spawn()
        .map_err(|e| format!("{}: {e}", tools::notify_send()))?;
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(st)) if st.success() => return Ok(()),
            Ok(Some(st)) => return Err(format!("notify-send: {st}")),
            Ok(None) if start.elapsed() > Duration::from_secs(5) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("notify-send did not stop in 5 s".into());
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(e) => return Err(format!("notify-send: {e}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn started_and_stopped() {
        let m = message("started", "code", None, None, None).unwrap();
        assert_eq!((m.urgency, m.summary.as_str(), m.body.as_str()), ("normal", "Sandbox code started", ""));
        // The last run stopped, or `sandbox stop`.
        let m = message("stopped", "code", Some("success"), Some("exited"), Some("0")).unwrap();
        assert_eq!((m.urgency, m.summary.as_str(), m.body.as_str()), ("normal", "Sandbox code stopped", ""));
        assert_eq!(message("stopped", "code", None, None, None).unwrap().urgency, "normal");
        assert_eq!(message("opened", "code", None, None, None), None);
    }

    #[test]
    fn killed_or_failed() {
        let m = message("stopped", "work", Some("signal"), Some("killed"), Some("KILL")).unwrap();
        assert_eq!(m.urgency, "critical");
        assert_eq!(m.summary, "Sandbox work killed");
        assert_eq!(m.body, "signal KILL\nsandbox logs work");
        let m = message("stopped", "work", Some("oom-kill"), Some("killed"), Some("KILL")).unwrap();
        assert_eq!(m.summary, "Sandbox work killed");
        assert!(m.body.starts_with("out of memory"));
        let m = message("stopped", "work", Some("exit-code"), Some("exited"), Some("1")).unwrap();
        assert_eq!(m.summary, "Sandbox work stopped with an error");
        assert!(m.body.starts_with("exit code 1\n"));
        let m = message("stopped", "work", Some("timeout"), Some("killed"), Some("TERM")).unwrap();
        assert_eq!((m.urgency, m.summary.as_str()), ("critical", "Sandbox work stopped"));
        assert!(m.body.starts_with("timeout (killed TERM)\n"));
    }

    #[test]
    fn the_arguments_of_notify_send() {
        let m = message("started", "code", None, None, None).unwrap();
        let a = args("code", &m);
        assert!(a.contains(&"--urgency=normal".to_string()));
        assert!(a.contains(&"--hint=string:x-dunst-stack-tag:sandbox-code".to_string()));
        // The summary and the body come after "--": a name cannot be an option.
        assert_eq!(&a[a.len() - 3..], ["--", "Sandbox code started", ""]);
    }
}
