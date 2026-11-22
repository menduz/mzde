//! The notification server of a sandbox: `sandbox __notify_bridge SOCKET`.
//! The private bus of the sandbox starts it (D-Bus activation) for the name
//! org.freedesktop.Notifications: sandbox-init gives the bus a service file
//! for that name before the service files of the system, so the bus never
//! starts the notification server of the host (dunst) in the sandbox.
//!
//! The bridge sends each Notify and CloseNotification on SOCKET to
//! `sandbox __notify_host` on the host (notify_host.rs), and sends
//! NotificationClosed of the host on the private bus. There are no actions. The host checks all values; this
//! side only changes the types of D-Bus into JSON. Without SOCKET (the
//! sandbox has `notifications = false`, or a run without a container), the
//! bridge stops with an error, and the programs get an error for each
//! notification.
//!
//! The bridge stops when the host closes the socket or the bus stops. The
//! bus starts it again for the next notification.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;

use serde_json::{Value as Json, json};

use crate::dbus::{self, Conn, Message, Type, Value};
use crate::notify_host::{Hints, Image, MAX_LINE, NOTIFICATIONS, Notify, PATH, hex};
use crate::util::{self, Result};
use crate::die;

const DBUS: &str = "org.freedesktop.DBus";
const FAILED: &str = "org.freedesktop.DBus.Error.Failed";

/// The capabilities of the notification server: those that the host
/// forwards. No "actions": the host gives no actions to the notifications
/// of a sandbox (notify_host.rs).
const CAPABILITIES: &[&str] = &["body", "body-markup", "icon-static", "persistence"];

const INTROSPECTION: &str = r#"<!DOCTYPE node PUBLIC "-//freedesktop//DTD D-BUS Object Introspection 1.0//EN" "http://www.freedesktop.org/standards/dbus/1.0/introspect.dtd">
<node>
  <interface name="org.freedesktop.Notifications">
    <method name="GetCapabilities"><arg direction="out" type="as"/></method>
    <method name="Notify">
      <arg direction="in" type="s" name="app_name"/>
      <arg direction="in" type="u" name="replaces_id"/>
      <arg direction="in" type="s" name="app_icon"/>
      <arg direction="in" type="s" name="summary"/>
      <arg direction="in" type="s" name="body"/>
      <arg direction="in" type="as" name="actions"/>
      <arg direction="in" type="a{sv}" name="hints"/>
      <arg direction="in" type="i" name="expire_timeout"/>
      <arg direction="out" type="u"/>
    </method>
    <method name="CloseNotification"><arg direction="in" type="u" name="id"/></method>
    <method name="GetServerInformation">
      <arg direction="out" type="s" name="name"/>
      <arg direction="out" type="s" name="vendor"/>
      <arg direction="out" type="s" name="version"/>
      <arg direction="out" type="s" name="spec_version"/>
    </method>
    <signal name="NotificationClosed"><arg type="u"/><arg type="u"/></signal>
  </interface>
  <interface name="org.freedesktop.DBus.Introspectable">
    <method name="Introspect"><arg direction="out" type="s"/></method>
  </interface>
  <interface name="org.freedesktop.DBus.Peer">
    <method name="Ping"/>
  </interface>
</node>
"#;

/// An integer of any integer type of D-Bus.
fn int(v: &Value) -> Option<i64> {
    Some(match v {
        Value::Byte(n) => *n as i64,
        Value::I16(n) => *n as i64,
        Value::U16(n) => *n as i64,
        Value::I32(n) => *n as i64,
        Value::U32(n) => *n as i64,
        Value::I64(n) => *n,
        Value::U64(n) => (*n).min(i64::MAX as u64) as i64,
        _ => return None,
    })
}

fn boolean(v: &Value) -> Option<bool> {
    match v {
        Value::Bool(b) => Some(*b),
        v => int(v).map(|n| n != 0),
    }
}

/// The hint image-data: (iiibiiay).
fn image(v: &Value) -> Option<Image> {
    let Value::Struct(f) = v else { return None };
    let [w, h, stride, Value::Bool(alpha), bits, channels, Value::Bytes(data)] = &f[..] else {
        return None;
    };
    let i = |v: &Value| int(v).and_then(|n| i32::try_from(n).ok());
    Some(Image {
        width: i(w)?,
        height: i(h)?,
        rowstride: i(stride)?,
        alpha: *alpha,
        bits: i(bits)?,
        channels: i(channels)?,
        data: hex(data),
    })
}

/// The notification of the arguments of Notify (susssasa{sv}i).
pub fn notify_of(body: &[Value]) -> Option<Notify> {
    // The actions are not forwarded (notify_host.rs).
    let [Value::Str(app), Value::U32(replaces), Value::Str(icon), Value::Str(summary), Value::Str(text), Value::Array(Type::Str, _), Value::Dict(Type::Str, Type::Variant, hints), Value::I32(timeout)] = body else {
        return None;
    };
    let mut h = Hints::default();
    for (k, v) in hints {
        let (Some(k), Value::Variant(v)) = (k.as_str(), v) else { continue };
        match k {
            "urgency" => h.urgency = int(v).map(|n| n.clamp(0, 2) as u8),
            "category" => h.category = v.as_str().map(String::from),
            "transient" => h.transient = boolean(v),
            "resident" => h.resident = boolean(v),
            "value" => h.value = int(v).map(|n| n.clamp(0, 100) as i32),
            // The names of the versions of the specification, newest first.
            "image-data" => h.image = image(v),
            "image_data" | "icon_data" if h.image.is_none() => h.image = image(v),
            _ => {}
        }
    }
    Some(Notify {
        app: app.clone(),
        replaces: *replaces,
        icon: icon.clone(),
        summary: summary.clone(),
        body: text.clone(),
        hints: h,
        timeout: *timeout,
    })
}

enum Kind {
    Notify,
    Close,
}

struct Bridge {
    bus: Conn,
    host: UnixStream,
    buf: Vec<u8>,
    seq: u64,
    /// The calls on the host: seq → the method call on the bus.
    calls: HashMap<u64, (Message, Kind)>,
}

impl Bridge {
    fn reply(&mut self, call: &Message, m: Message) -> Result<()> {
        if call.flags & dbus::NO_REPLY_EXPECTED == 0 {
            self.bus.send(&m)?;
        }
        Ok(())
    }

    fn to_host(&mut self, call: Message, kind: Kind, mut msg: Json) -> Result<()> {
        self.seq += 1;
        msg["seq"] = json!(self.seq);
        let mut bytes = msg.to_string().into_bytes();
        bytes.push(b'\n');
        self.host.write_all(&bytes).map_err(|e| format!("the host: {e}"))?;
        self.calls.insert(self.seq, (call, kind));
        Ok(())
    }

    fn on_bus(&mut self, m: Message) -> Result<()> {
        if m.kind != dbus::METHOD_CALL {
            return Ok(());
        }
        let iface = m.interface.as_deref().unwrap_or(NOTIFICATIONS);
        let member = m.member.as_deref().unwrap_or_default();
        let at_path = m.path.as_deref() == Some(PATH);
        match (iface, member) {
            ("org.freedesktop.DBus.Introspectable", "Introspect") => {
                let xml = if at_path { INTROSPECTION } else { "<node/>" };
                let r = m.reply(vec![Value::str(xml)]);
                self.reply(&m, r)
            }
            ("org.freedesktop.DBus.Peer", "Ping") => {
                let r = m.reply(vec![]);
                self.reply(&m, r)
            }
            _ if !at_path => {
                let r = m.error("org.freedesktop.DBus.Error.UnknownObject", "no object at this path");
                self.reply(&m, r)
            }
            (NOTIFICATIONS, "Notify") => match notify_of(&m.body) {
                Some(n) => self.to_host(m, Kind::Notify, json!({"call": "notify", "notify": n})),
                None => {
                    let r = m.error("org.freedesktop.DBus.Error.InvalidArgs", "Notify takes susssasa{sv}i");
                    self.reply(&m, r)
                }
            },
            (NOTIFICATIONS, "CloseNotification") => match m.body.first().and_then(Value::as_u32) {
                Some(id) => self.to_host(m, Kind::Close, json!({"call": "close", "id": id})),
                None => {
                    let r = m.error("org.freedesktop.DBus.Error.InvalidArgs", "CloseNotification takes u");
                    self.reply(&m, r)
                }
            },
            (NOTIFICATIONS, "GetCapabilities") => {
                let caps = CAPABILITIES.iter().map(|c| Value::str(c)).collect();
                let r = m.reply(vec![Value::Array(Type::Str, caps)]);
                self.reply(&m, r)
            }
            (NOTIFICATIONS, "GetServerInformation") => {
                let r = m.reply(["sandbox", "mzde", env!("CARGO_PKG_VERSION"), "1.2"].iter().map(|s| Value::str(s)).collect());
                self.reply(&m, r)
            }
            _ => {
                let r = m.error("org.freedesktop.DBus.Error.UnknownMethod", &format!("no method {iface}.{member}"));
                self.reply(&m, r)
            }
        }
    }

    fn on_host(&mut self, line: &[u8]) -> Result<()> {
        let Ok(msg) = serde_json::from_slice::<Json>(line) else {
            return Ok(());
        };
        let id = msg["id"].as_u64().and_then(|n| u32::try_from(n).ok()).unwrap_or(0);
        match msg["signal"].as_str() {
            Some("closed") => {
                let reason = msg["reason"].as_u64().and_then(|n| u32::try_from(n).ok()).unwrap_or(4);
                self.bus.send(&Message::signal(PATH, NOTIFICATIONS, "NotificationClosed", vec![Value::U32(id), Value::U32(reason)]))?;
            }
            Some(_) => {}
            None => {
                let Some((call, kind)) = msg["seq"].as_u64().and_then(|s| self.calls.remove(&s)) else {
                    return Ok(());
                };
                let r = match (msg["error"].as_str(), kind) {
                    (Some(e), _) => call.error(FAILED, e),
                    (None, Kind::Notify) => call.reply(vec![Value::U32(id)]),
                    (None, Kind::Close) => call.reply(vec![]),
                };
                self.reply(&call, r)?;
            }
        }
        Ok(())
    }
}

/// Have the name org.freedesktop.Notifications on the bus `bus_address`
/// and forward the calls to the host at `socket`. Returns when the host or
/// the bus closes the connection.
pub fn run(bus_address: &str, socket: &str) -> Result<()> {
    let host = UnixStream::connect(socket).map_err(|e| format!("the notifications of the host at {socket}: {e}"))?;
    let mut bus = Conn::connect(bus_address)?;
    // DO_NOT_QUEUE: if another program has the name, stop.
    let r = bus.call(Message::call(DBUS, "/org/freedesktop/DBus", DBUS, "RequestName", vec![Value::str(NOTIFICATIONS), Value::U32(4)]))?;
    if !matches!(r.body.first().and_then(Value::as_u32), Some(1 | 4)) {
        return Err(format!("another program has the name {NOTIFICATIONS}"));
    }
    let mut b = Bridge { bus, host, buf: Vec::new(), seq: 0, calls: HashMap::new() };
    let mut chunk = vec![0u8; 64 * 1024];
    loop {
        while b.bus.has_pending() {
            if let Some(m) = b.bus.read()? {
                b.on_bus(m)?;
            }
        }
        let mut fds = [
            libc::pollfd { fd: b.bus.fd(), events: libc::POLLIN, revents: 0 },
            libc::pollfd { fd: b.host.as_raw_fd(), events: libc::POLLIN, revents: 0 },
        ];
        // SAFETY: fds is a valid array of 2 entries.
        if unsafe { libc::poll(fds.as_mut_ptr(), 2, -1) } < 0 {
            if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err(format!("poll: {}", std::io::Error::last_os_error()));
        }
        if fds[0].revents != 0 {
            if let Some(m) = b.bus.read()? {
                b.on_bus(m)?;
            }
        }
        if fds[1].revents != 0 {
            let n = match b.host.read(&mut chunk) {
                Ok(0) | Err(_) => return Ok(()),
                Ok(n) => n,
            };
            b.buf.extend_from_slice(&chunk[..n]);
            while let Some(i) = b.buf.iter().position(|&c| c == b'\n') {
                let line: Vec<u8> = b.buf.drain(..=i).collect();
                b.on_host(&line[..line.len() - 1])?;
            }
            if b.buf.len() > MAX_LINE {
                return Err("the host sent a line that is too long".into());
            }
        }
    }
}

pub fn main(args: &[String]) -> ! {
    let Some(socket) = args.first() else {
        die!("the notifications of this sandbox do not go to the host (notifications = false, or a run without a container)");
    };
    let Some(addr) = util::env("DBUS_STARTER_ADDRESS").or_else(|| util::env("DBUS_SESSION_BUS_ADDRESS")).filter(|s| !s.is_empty()) else {
        die!("no bus: DBUS_STARTER_ADDRESS and DBUS_SESSION_BUS_ADDRESS are not set");
    };
    if let Err(e) = run(&addr, socket) {
        die!("{e}");
    }
    std::process::exit(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn variant(v: Value) -> Value {
        Value::Variant(Box::new(v))
    }

    #[test]
    fn the_arguments_of_notify() {
        let hints = Value::Dict(
            Type::Str,
            Type::Variant,
            vec![
                (Value::str("urgency"), variant(Value::Byte(2))),
                (Value::str("transient"), variant(Value::I32(1))),
                (Value::str("x-dunst-stack-tag"), variant(Value::str("tag"))),
                (
                    Value::str("icon_data"),
                    variant(Value::Struct(vec![
                        Value::I32(1),
                        Value::I32(1),
                        Value::I32(3),
                        Value::Bool(false),
                        Value::I32(8),
                        Value::I32(3),
                        Value::Bytes(vec![1, 2, 255]),
                    ])),
                ),
            ],
        );
        let body = vec![
            Value::str("app"),
            Value::U32(4),
            Value::str("icon"),
            Value::str("s"),
            Value::str("b"),
            Value::Array(Type::Str, vec![Value::str("default"), Value::str("Open")]),
            hints,
            Value::I32(5000),
        ];
        let n = notify_of(&body).unwrap();
        assert_eq!((n.app.as_str(), n.replaces, n.icon.as_str(), n.timeout), ("app", 4, "icon", 5000));
        assert_eq!((n.hints.urgency, n.hints.transient), (Some(2), Some(true)));
        assert_eq!(n.hints.image.as_ref().unwrap().data, "0102ff");
        // The JSON of the notification reads back the same.
        let back: Notify = serde_json::from_value(serde_json::to_value(&n).unwrap()).unwrap();
        assert_eq!(back, n);
        assert!(notify_of(&body[..7]).is_none());
    }
}
