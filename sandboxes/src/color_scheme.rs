//! The color scheme of the host in a sandbox (`followColorScheme`). One
//! value goes into the sandbox, and nothing comes out of it:
//!
//! - On the host, the container starts `sandbox __color_scheme_host DIR`
//!   (run.rs). It reads the key color-scheme of the settings portal of the
//!   host (org.freedesktop.appearance), and writes it to DIR/color-scheme:
//!   "default", "prefer-dark" or "prefer-light". It writes the file again
//!   after each SettingChanged of that key, and when the portal starts
//!   again.
//! - The plan mounts DIR read-only in the sandbox. In the sandbox,
//!   sandbox-init starts `sandbox __color_scheme FILE` (init.rs). It reads
//!   the file each second, and writes a new value to the GSettings of the
//!   sandbox (`dconf write`, on the private bus of the sandbox). The
//!   settings portal of the sandbox then sends SettingChanged, and GTK4,
//!   libadwaita and the other programs that read the portal change their
//!   scheme.
//!
//! The host reads nothing from the sandbox: the mount is read-only, and the
//! host side does not connect to the bus of the sandbox. The sandbox side
//! accepts only the three values. It reads the file each second, because
//! gVisor gives no inotify event for a change on the host.

use std::fs;
use std::io::Read;
use std::process::{Command, Stdio};
use std::time::Duration;

use crate::dbus::{Conn, Message, Value};
use crate::tools;
use crate::util::{self, Result};
use crate::{die, warn};

/// The name of the file in the directory of the container.
pub const FILE: &str = "color-scheme";

/// The GSettings key that the sandbox side writes.
const KEY: &str = "/org/gnome/desktop/interface/color-scheme";

const DBUS: &str = "org.freedesktop.DBus";
const PORTAL: &str = "org.freedesktop.portal.Desktop";
const PORTAL_PATH: &str = "/org/freedesktop/portal/desktop";
const SETTINGS: &str = "org.freedesktop.portal.Settings";
const APPEARANCE: &str = "org.freedesktop.appearance";

/// The GSettings value of a color scheme of the portal: 1 is dark, 2 is
/// light, and 0 (or an unknown value) is no preference.
pub fn nick(scheme: u32) -> &'static str {
    match scheme {
        1 => "prefer-dark",
        2 => "prefer-light",
        _ => "default",
    }
}

/// The value of the text of the file, or None if the text is not one of the
/// three values (with or without a newline).
pub fn parse(text: &str) -> Option<&'static str> {
    match text.strip_suffix('\n').unwrap_or(text) {
        "default" => Some("default"),
        "prefer-dark" => Some("prefer-dark"),
        "prefer-light" => Some("prefer-light"),
        _ => None,
    }
}

/// The number in a value of the portal. Read gives the value in two
/// variants, ReadOne in one.
fn scheme_of(v: &Value) -> Option<u32> {
    match v {
        Value::Variant(inner) => scheme_of(inner),
        Value::U32(n) => Some(*n),
        _ => None,
    }
}

/// The color scheme of the settings portal on the bus of `c`. ReadOne is in
/// version 2 of the portal; an older portal has only Read.
fn read_portal(c: &mut Conn) -> Result<u32> {
    let call = |member: &str| Message::call(PORTAL, PORTAL_PATH, SETTINGS, member, vec![Value::str(APPEARANCE), Value::str("color-scheme")]);
    let r = c.call(call("ReadOne")).or_else(|_| c.call(call("Read")))?;
    r.body.first().and_then(scheme_of).ok_or_else(|| "the portal gave a color scheme that is not a number".into())
}

/// Write `value` to DIR/color-scheme in one step: the sandbox does not see
/// half of the file.
fn publish(dir: &str, value: &str) -> Result<()> {
    let path = format!("{dir}/{FILE}");
    let tmp = format!("{dir}/.{FILE}.tmp");
    fs::write(&tmp, format!("{value}\n"))
        .and_then(|_| fs::rename(&tmp, &path))
        .map_err(|e| format!("cannot write {path}: {e}"))
}

/// Read the portal, and write a changed value to `dir`.
fn update(c: &mut Conn, dir: &str, last: &mut Option<&'static str>) -> Result<()> {
    let value = match read_portal(c) {
        Ok(n) => nick(n),
        // No portal: the sandbox keeps its scheme. A start of the portal
        // gives NameOwnerChanged.
        Err(e) => {
            warn!("the color scheme of the host: {e}");
            return Ok(());
        }
    };
    if *last != Some(value) {
        publish(dir, value)?;
        *last = Some(value);
    }
    Ok(())
}

/// The host side: follow the portal on the bus `address`, and write each
/// value to `dir`. It returns only when the connection breaks. A write
/// error does not stop it: `last` does not change, so the next signal
/// writes the value again.
fn follow(address: &str, dir: &str) -> Result<()> {
    let mut c = Conn::connect(address)?;
    // First the match rules, then the first read: a change between the two
    // is not lost.
    for rule in [
        format!("type='signal',interface='{SETTINGS}',member='SettingChanged',path='{PORTAL_PATH}',arg0='{APPEARANCE}',arg1='color-scheme'"),
        format!("type='signal',sender='{DBUS}',interface='{DBUS}',member='NameOwnerChanged',arg0='{PORTAL}'"),
    ] {
        c.call(Message::call(DBUS, "/org/freedesktop/DBus", DBUS, "AddMatch", vec![Value::str(&rule)]))?;
    }
    let mut last = None;
    if let Err(e) = update(&mut c, dir, &mut last) {
        warn!("the color scheme of the host: {e}; the next change tries again");
    }
    loop {
        let Some(m) = c.read()? else { continue };
        // The value of the signal is not used: the portal gives the value
        // again. NameOwnerChanged without a new owner is the stop of the
        // portal; a read then starts it again.
        let started = m.is_signal(DBUS, "NameOwnerChanged") && m.body.get(2).and_then(Value::as_str).is_some_and(|s| !s.is_empty());
        if (started || m.is_signal(SETTINGS, "SettingChanged"))
            && let Err(e) = update(&mut c, dir, &mut last)
        {
            warn!("the color scheme of the host: {e}; the next change tries again");
        }
    }
}

/// `sandbox __color_scheme_host DIR`: the host side.
pub fn host_main(args: &[String]) -> ! {
    let [dir] = args else {
        die!("usage: sandbox __color_scheme_host DIR");
    };
    let runtime = util::env("XDG_RUNTIME_DIR").unwrap_or_default();
    let addr = util::env("DBUS_SESSION_BUS_ADDRESS").filter(|s| !s.is_empty()).unwrap_or_else(|| format!("unix:path={runtime}/bus"));
    let err = follow(&addr, dir).err().unwrap_or_default();
    warn!("the color scheme of the host does not go to the sandbox now: {err}");
    std::process::exit(1)
}

/// The value in the file `path`, or None if the file is not there or has a
/// different text.
fn read_file(path: &str) -> Option<&'static str> {
    let mut text = String::new();
    fs::File::open(path).ok()?.take(64).read_to_string(&mut text).ok()?;
    parse(&text)
}

/// Write `value` to the GSettings of the sandbox. The dconf service of the
/// sandbox can hang: after 10 s, stop dconf.
fn write_dconf(value: &str) -> Result<()> {
    let mut child = Command::new(tools::dconf())
        .args(["write", KEY, &format!("'{value}'")])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .spawn()
        .map_err(|e| format!("{}: {e}", tools::dconf()))?;
    for _ in 0..100 {
        match child.try_wait() {
            Ok(Some(st)) if st.success() => return Ok(()),
            Ok(Some(st)) => return Err(format!("dconf write: {st}")),
            Ok(None) => std::thread::sleep(Duration::from_millis(100)),
            Err(e) => return Err(format!("dconf write: {e}")),
        }
    }
    let _ = child.kill();
    let _ = child.wait();
    Err("dconf write did not stop in 10 s".into())
}

/// `sandbox __color_scheme FILE`: the sandbox side. It stops with the
/// sandbox.
pub fn main(args: &[String]) -> ! {
    let [path] = args else {
        die!("usage: sandbox __color_scheme FILE");
    };
    let mut applied: Option<&str> = None;
    loop {
        let mut wait = Duration::from_secs(1);
        if let Some(value) = read_file(path)
            && applied != Some(value)
        {
            match write_dconf(value) {
                Ok(()) => applied = Some(value),
                Err(e) => {
                    warn!("the color scheme {value}: {e}");
                    // Try again later, not each second.
                    wait = Duration::from_secs(30);
                }
            }
        }
        std::thread::sleep(wait);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::bus;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
    use std::time::Instant;

    #[test]
    fn only_three_values() {
        assert_eq!(parse("prefer-dark\n"), Some("prefer-dark"));
        assert_eq!(parse("prefer-light"), Some("prefer-light"));
        assert_eq!(parse("default\n"), Some("default"));
        for bad in ["", "\n", "dark", "prefer-dark\n\n", " prefer-dark", "prefer-dark'; rm -rf /", "'prefer-dark'"] {
            assert_eq!(parse(bad), None, "{bad:?}");
        }
        assert_eq!((nick(0), nick(1), nick(2), nick(7)), ("default", "prefer-dark", "prefer-light", "default"));
        let v = Value::Variant(Box::new(Value::Variant(Box::new(Value::U32(1)))));
        assert_eq!(scheme_of(&v), Some(1));
        assert_eq!(scheme_of(&Value::str("1")), None);
    }

    #[test]
    fn the_file_is_read_with_a_limit() {
        let dir = std::env::temp_dir().join(format!("sandbox-color-scheme-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let d = dir.to_str().unwrap();
        let path = format!("{d}/{FILE}");
        assert_eq!(read_file(&path), None);
        publish(d, "prefer-dark").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "prefer-dark\n");
        assert_eq!(read_file(&path), Some("prefer-dark"));
        fs::write(&path, format!("prefer-dark\n{}", "x".repeat(100))).unwrap();
        assert_eq!(read_file(&path), None);
        let _ = fs::remove_dir_all(&dir);
    }

    /// A fake settings portal: color-scheme is `scheme`, and after each
    /// change the portal sends SettingChanged.
    fn portal(address: String, schemes: Vec<u32>) {
        let mut c = Conn::connect(&address).unwrap();
        c.call(Message::call(DBUS, "/org/freedesktop/DBus", DBUS, "RequestName", vec![Value::str(PORTAL), Value::U32(4)])).unwrap();
        let mut i = 0;
        loop {
            let Ok(m) = c.read() else { return };
            let Some(m) = m else { continue };
            if m.interface.as_deref() == Some(SETTINGS) && m.member.as_deref() == Some("ReadOne") {
                c.send(&m.reply(vec![Value::Variant(Box::new(Value::U32(schemes[i])))])).unwrap();
                if i + 1 < schemes.len() {
                    i += 1;
                    std::thread::sleep(Duration::from_millis(100));
                    // A signal of a different key changes nothing.
                    let changed = |key: &str| Message::signal(PORTAL_PATH, SETTINGS, "SettingChanged", vec![Value::str(APPEARANCE), Value::str(key), Value::Variant(Box::new(Value::U32(0)))]);
                    c.send(&changed("accent-color")).unwrap();
                    c.send(&changed("color-scheme")).unwrap();
                }
            }
        }
    }

    /// A fake settings portal: color-scheme is the number in `value`, and
    /// `reads` counts the reads.
    fn portal_of(address: String, value: Arc<AtomicU32>, reads: Arc<AtomicUsize>) {
        let mut c = Conn::connect(&address).unwrap();
        c.call(Message::call(DBUS, "/org/freedesktop/DBus", DBUS, "RequestName", vec![Value::str(PORTAL), Value::U32(4)])).unwrap();
        loop {
            let Ok(m) = c.read() else { return };
            let Some(m) = m else { continue };
            if m.interface.as_deref() == Some(SETTINGS) && m.member.as_deref() == Some("ReadOne") {
                c.send(&m.reply(vec![Value::Variant(Box::new(Value::U32(value.load(Ordering::SeqCst))))])).unwrap();
                reads.fetch_add(1, Ordering::SeqCst);
            }
        }
    }

    /// With a real dbus-daemon (finding F01 of proof-color-scheme): the
    /// first write fails, because DIR is read-only. The host side goes on,
    /// and after the next SettingChanged it writes the new value. Without
    /// dbus-daemon, or as root (a mode does not stop root), the test does
    /// not run.
    #[test]
    fn the_host_side_goes_on_after_a_write_error() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("sandbox-color-scheme-error-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let out = dir.join("out");
        fs::create_dir_all(&out).unwrap();
        let o = out.to_str().unwrap().to_string();
        let Some(host) = bus(&dir, "bus") else {
            eprintln!("no dbus-daemon: the test does not run");
            return;
        };
        fs::set_permissions(&out, fs::Permissions::from_mode(0o500)).unwrap();
        if fs::write(out.join("probe"), "").is_ok() {
            eprintln!("a read-only directory is writable (root): the test does not run");
            let _ = fs::remove_dir_all(&dir);
            return;
        }
        let value = Arc::new(AtomicU32::new(2));
        let reads = Arc::new(AtomicUsize::new(0));
        let (address, v, r) = (host.address.clone(), value.clone(), reads.clone());
        std::thread::spawn(move || portal_of(address, v, r));
        let (address, d) = (host.address.clone(), o.clone());
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || tx.send(follow(&address, &d)));
        let start = Instant::now();
        while reads.load(Ordering::SeqCst) == 0 {
            assert!(start.elapsed() < Duration::from_secs(10), "the host side does not read the portal");
            std::thread::sleep(Duration::from_millis(5));
        }
        // The first publish fails: the host side must not stop.
        if let Ok(r) = rx.recv_timeout(Duration::from_millis(300)) {
            panic!("the host side stopped after a write error: {r:?}");
        }
        fs::set_permissions(&out, fs::Permissions::from_mode(0o700)).unwrap();
        value.store(1, Ordering::SeqCst);
        let mut c = Conn::connect(&host.address).unwrap();
        c.send(&Message::signal(PORTAL_PATH, SETTINGS, "SettingChanged", vec![Value::str(APPEARANCE), Value::str("color-scheme"), Value::Variant(Box::new(Value::U32(1)))])).unwrap();
        let path = format!("{o}/{FILE}");
        while fs::read_to_string(&path).ok().as_deref() != Some("prefer-dark\n") {
            assert!(start.elapsed() < Duration::from_secs(10), "the new value is not in the file");
            std::thread::sleep(Duration::from_millis(5));
        }
        drop(host);
        let _ = fs::remove_dir_all(&dir);
    }

    /// With a real dbus-daemon: the host side writes the value of the
    /// portal, and the next value after SettingChanged. Without
    /// dbus-daemon, the test does not run.
    #[test]
    fn the_host_side_follows_the_portal() {
        let dir = std::env::temp_dir().join(format!("sandbox-color-scheme-host-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let Some(host) = bus(&dir, "bus") else {
            eprintln!("no dbus-daemon: the test does not run");
            return;
        };
        let address = host.address.clone();
        std::thread::spawn(move || portal(address, vec![2, 1, 0]));
        let address = host.address.clone();
        let d = dir.to_str().unwrap().to_string();
        let out = d.clone();
        std::thread::spawn(move || follow(&address, &out));
        let path = format!("{d}/{FILE}");
        let mut seen: Vec<String> = Vec::new();
        let start = Instant::now();
        while seen.last().map(String::as_str) != Some("default") {
            assert!(start.elapsed() < Duration::from_secs(10), "the values: {seen:?}");
            if let Ok(text) = fs::read_to_string(&path)
                && seen.last() != Some(&text.trim_end().to_string())
            {
                seen.push(text.trim_end().into());
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(seen, ["prefer-light", "prefer-dark", "default"]);
        drop(host);
        let _ = fs::remove_dir_all(&dir);
    }
}
