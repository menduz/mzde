//! The notifications of the programs of a sandbox, on the host side. The
//! container (`sandbox __container`) starts `sandbox __notify_host NAME
//! COLOR` with a listening socket on fd 0, and the plan mounts that socket
//! in the sandbox. In the sandbox, `sandbox __notify_bridge` has the name
//! org.freedesktop.Notifications on the private bus (notify_bridge.rs), and
//! sends each call as one line of JSON on the socket.
//!
//! This side does not trust the sandbox: the sandbox can write anything on
//! the socket. It sends each notification to the notification server of the
//! host (dunst) with these changes:
//!
//! - The summary starts with the name of the sandbox, and the app name is
//!   "NAME: app": a sandbox cannot show a notification of the host. The
//!   border has the color of the sandbox (the hint frcolor of dunst).
//! - Only some hints go to the host: urgency, category, transient,
//!   resident, value and image-data (checked). No other hint, so a sandbox
//!   cannot use the stack tag of a notification of the host. An icon is
//!   only a name of the icon theme, never a path.
//! - No actions: a click on a notification of a sandbox runs nothing in
//!   the sandbox, it only closes the notification. The sandbox gets no
//!   ActionInvoked and no ActivationToken.
//! - replaces_id and CloseNotification work only for the notifications of
//!   this sandbox, and go to the unique name of the server that gave the ID,
//!   not to org.freedesktop.Notifications: after a change of the server,
//!   the bus refuses them, and they never reach another server with the
//!   same ID. NotificationClosed goes to the sandbox only for its
//!   notifications, and only from the server of the ID. When the
//!   notification server changes, the IDs of the sandbox are forgotten.
//! - The text has limits of length, and a sandbox can send 20
//!   notifications at once, then 5 each second.
//!
//! The protocol: requests {"call": "notify", "seq": N, "notify": {...}} and
//! {"call": "close", "seq": N, "id": ID}; replies {"seq": N, "id": ID} or
//! {"seq": N, "error": TEXT}; the signal {"signal": "closed", "id": ID,
//! "reason": R}.

use std::collections::{HashMap, VecDeque};
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::net::{UnixListener, UnixStream};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{Value as Json, json};

use crate::dbus::{self, Conn, Message, Type, Value};
use crate::util::{self, Result};
use crate::{die, warn};

pub const NOTIFICATIONS: &str = "org.freedesktop.Notifications";
pub const PATH: &str = "/org/freedesktop/Notifications";

/// The longest line of the protocol: a notification with an image of
/// 512 × 512 pixels (2 MiB in hexadecimal).
pub const MAX_LINE: usize = 4 << 20;
/// The largest width and height of an image.
const MAX_IMAGE: i32 = 512;
/// The number of connections of the bridge at the same time.
const MAX_CLIENTS: usize = 4;
/// The number of notifications of a sandbox that the host remembers.
const MAX_OWNED: usize = 256;
/// The rate limit: a burst, then a number each second.
const BURST: f64 = 20.0;
const RATE: f64 = 5.0;

/// A notification, as the bridge sends it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Notify {
    pub app: String,
    pub replaces: u32,
    pub icon: String,
    pub summary: String,
    pub body: String,
    pub hints: Hints,
    pub timeout: i32,
}

impl Default for Notify {
    fn default() -> Notify {
        Notify {
            app: String::new(),
            replaces: 0,
            icon: String::new(),
            summary: String::new(),
            body: String::new(),
            hints: Hints::default(),
            timeout: -1,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Hints {
    pub urgency: Option<u8>,
    pub category: Option<String>,
    pub transient: Option<bool>,
    pub resident: Option<bool>,
    pub value: Option<i32>,
    pub image: Option<Image>,
}

/// The hint image-data: raw pixels, with the data in hexadecimal.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Image {
    pub width: i32,
    pub height: i32,
    pub rowstride: i32,
    pub alpha: bool,
    pub bits: i32,
    pub channels: i32,
    pub data: String,
}

#[derive(Debug, Deserialize)]
struct Request {
    call: String,
    seq: u64,
    #[serde(default)]
    id: u32,
    #[serde(default)]
    notify: Option<Notify>,
}

pub fn hex(b: &[u8]) -> String {
    const D: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(b.len() * 2);
    for &x in b {
        s.push(D[(x >> 4) as usize] as char);
        s.push(D[(x & 15) as usize] as char);
    }
    s
}

pub fn unhex(s: &str) -> Option<Vec<u8>> {
    let b = s.as_bytes();
    if b.len() % 2 != 0 {
        return None;
    }
    let d = |c: u8| (c as char).to_digit(16).map(|v| v as u8);
    b.chunks(2).map(|p| Some(d(p[0])? << 4 | d(p[1])?)).collect()
}

/// The first `max` bytes of `s`, cut at a character.
fn cut(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut n = max;
    while !s.is_char_boundary(n) {
        n -= 1;
    }
    &s[..n]
}

/// One line: no control characters (a new line is a space).
fn line(s: &str, max: usize) -> String {
    let s: String = s.chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
    cut(s.trim(), max).trim_end().to_string()
}

/// A text: new lines and tabs stay, no other control characters.
fn text(s: &str, max: usize) -> String {
    let s: String = s.chars().filter(|&c| !c.is_control() || c == '\n' || c == '\t').collect();
    cut(&s, max).to_string()
}

/// A name of an icon of the icon theme ("dialog-information"). A path or
/// a URL is not a name: the host would read a file of its own.
fn icon_name(s: &str) -> bool {
    !s.is_empty() && s.len() <= 128 && s.bytes().all(|c| c.is_ascii_alphanumeric() || b"._-+".contains(&c)) && !s.starts_with('.')
}

/// The hint image-data for the host, if `i` is a valid image.
fn image(i: &Image) -> Option<Value> {
    let sizes = (1..=MAX_IMAGE).contains(&i.width) && (1..=MAX_IMAGE).contains(&i.height);
    let format = i.bits == 8 && matches!((i.channels, i.alpha), (3, false) | (4, true));
    if !sizes || !format {
        return None;
    }
    let row = (i.width * i.channels) as usize;
    let stride = i.rowstride as usize;
    if i.rowstride < 0 || stride < row || stride > row + 64 {
        return None;
    }
    let data = unhex(&i.data)?;
    let need = stride * (i.height as usize - 1) + row;
    if data.len() < need || data.len() > stride * i.height as usize {
        return None;
    }
    Some(Value::Struct(vec![
        Value::I32(i.width),
        Value::I32(i.height),
        Value::I32(i.rowstride),
        Value::Bool(i.alpha),
        Value::I32(i.bits),
        Value::I32(i.channels),
        Value::Bytes(data),
    ]))
}

/// The arguments of Notify on the host for the notification `n` of the
/// sandbox `name` with the color `color` ("#rrggbb"). `replaces` is the ID
/// that the notification replaces, only if it is a notification of the
/// sandbox (else 0). The notification has no actions.
pub fn host_args(name: &str, color: &str, n: &Notify, replaces: u32) -> Vec<Value> {
    let app = line(&n.app, 64);
    let app = if app.is_empty() { name.to_string() } else { format!("{name}: {app}") };
    let summary = line(&n.summary, 256);
    let summary = if summary.is_empty() { name.to_string() } else { format!("{name}: {summary}") };
    let icon = if icon_name(&n.icon) { n.icon.clone() } else { String::new() };

    let h = &n.hints;
    let mut hints: Vec<(Value, Value)> = Vec::new();
    let mut hint = |k: &str, v: Value| hints.push((Value::str(k), Value::Variant(Box::new(v))));
    if let Some(u) = h.urgency {
        hint("urgency", Value::Byte(u.min(2)));
    }
    if let Some(c) = &h.category {
        let c = line(c, 64);
        if !c.is_empty() && c.bytes().all(|b| b.is_ascii_alphanumeric() || b".-_".contains(&b)) {
            hint("category", Value::Str(c));
        }
    }
    if let Some(b) = h.transient {
        hint("transient", Value::Bool(b));
    }
    if let Some(b) = h.resident {
        hint("resident", Value::Bool(b));
    }
    if let Some(v) = h.value {
        hint("value", Value::I32(v.clamp(0, 100)));
    }
    if let Some(img) = h.image.as_ref().and_then(image) {
        hint("image-data", img);
    }
    hint("frcolor", Value::str(color));

    let timeout = if n.timeout < 0 { -1 } else { n.timeout.min(3_600_000) };
    vec![
        Value::Str(app),
        Value::U32(replaces),
        Value::Str(icon),
        Value::Str(summary),
        Value::Str(text(&n.body, 8192)),
        Value::Array(Type::Str, vec![]),
        Value::Dict(Type::Str, Type::Variant, hints),
        Value::I32(timeout),
    ]
}

/// The rate limit of the notifications of a sandbox.
struct Bucket {
    tokens: f64,
    last: Instant,
}

impl Bucket {
    fn take(&mut self, now: Instant) -> bool {
        let dt = now.saturating_duration_since(self.last).as_secs_f64();
        self.tokens = (self.tokens + dt * RATE).min(BURST);
        self.last = now;
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }
}

enum Pending {
    Notify,
    Close,
}

struct Client {
    id: u64,
    stream: UnixStream,
    buf: Vec<u8>,
}

struct Host<'a> {
    name: &'a str,
    color: &'a str,
    bus: Conn,
    clients: Vec<Client>,
    next_client: u64,
    /// The notifications of the sandbox on the host: the unique name of the
    /// server that gave the ID, and the ID.
    owned: VecDeque<(String, u32)>,
    /// The calls on the host bus: serial → client, seq.
    calls: HashMap<u32, (u64, u64, Pending)>,
    bucket: Bucket,
}

impl Host<'_> {
    fn send_to(&mut self, client: u64, msg: &Json) {
        let mut bytes = msg.to_string().into_bytes();
        bytes.push(b'\n');
        if let Some(c) = self.clients.iter_mut().find(|c| c.id == client) {
            // The socket has a send timeout: a sandbox that does not read
            // loses its connection.
            if c.stream.write_all(&bytes).is_err() {
                self.clients.retain(|c| c.id != client);
            }
        }
    }

    fn broadcast(&mut self, msg: &Json) {
        for id in self.clients.iter().map(|c| c.id).collect::<Vec<_>>() {
            self.send_to(id, msg);
        }
    }

    fn own(&mut self, server: &str, id: u32) {
        if id == 0 || self.issuer(id).is_some() {
            return;
        }
        if self.owned.len() >= MAX_OWNED {
            self.owned.pop_front();
        }
        self.owned.push_back((server.to_string(), id));
    }

    /// The unique name of the server that gave the ID `id` to the sandbox.
    /// None: not a notification of the sandbox.
    fn issuer(&self, id: u32) -> Option<String> {
        self.owned.iter().find(|o| o.1 == id).map(|o| o.0.clone())
    }

    fn on_line(&mut self, client: u64, line: &[u8]) -> Result<()> {
        let req: Request = match serde_json::from_slice(line) {
            Ok(r) => r,
            Err(_) => {
                self.send_to(client, &json!({"seq": 0, "error": "a request that is not valid"}));
                return Ok(());
            }
        };
        match (req.call.as_str(), req.notify) {
            ("notify", Some(n)) => {
                if !self.bucket.take(Instant::now()) {
                    self.send_to(client, &json!({"seq": req.seq, "error": "too many notifications"}));
                    return Ok(());
                }
                // A replace goes to the server of the ID; a new
                // notification to the name (the bus can start the server).
                let (dest, replaces) = match self.issuer(n.replaces) {
                    Some(server) => (server, n.replaces),
                    None => (NOTIFICATIONS.to_string(), 0),
                };
                let m = Message::call(&dest, PATH, NOTIFICATIONS, "Notify", host_args(self.name, self.color, &n, replaces));
                let serial = self.bus.send(&m)?;
                self.calls.insert(serial, (client, req.seq, Pending::Notify));
            }
            ("close", _) => match self.issuer(req.id) {
                Some(server) => {
                    let m = Message::call(&server, PATH, NOTIFICATIONS, "CloseNotification", vec![Value::U32(req.id)]);
                    let serial = self.bus.send(&m)?;
                    self.calls.insert(serial, (client, req.seq, Pending::Close));
                }
                // Not a notification of the sandbox: nothing to close.
                None => self.send_to(client, &json!({"seq": req.seq})),
            },
            _ => self.send_to(client, &json!({"seq": req.seq, "error": "a request that is not known"})),
        }
        Ok(())
    }

    fn on_bus(&mut self, m: Message) {
        match m.kind {
            dbus::METHOD_RETURN | dbus::ERROR => {
                let Some((client, seq, kind)) = m.reply_serial.and_then(|s| self.calls.remove(&s)) else {
                    return;
                };
                if m.kind == dbus::ERROR {
                    let text = m.body.first().and_then(Value::as_str).unwrap_or_default();
                    let error = format!("{}: {text}", m.error_name.as_deref().unwrap_or_default());
                    self.send_to(client, &json!({"seq": seq, "error": error}));
                    return;
                }
                match kind {
                    Pending::Notify => {
                        // The bus gives the unique name of the server.
                        let id = m.body.first().and_then(Value::as_u32).unwrap_or(0);
                        if let Some(server) = m.sender.as_deref() {
                            self.own(server, id);
                        }
                        self.send_to(client, &json!({"seq": seq, "id": id}));
                    }
                    Pending::Close => self.send_to(client, &json!({"seq": seq})),
                }
            }
            dbus::SIGNAL if m.is_signal("org.freedesktop.DBus", "NameOwnerChanged") => {
                // A new notification server: the IDs of the old one are
                // not the IDs of the sandbox.
                if m.body.first().and_then(Value::as_str) == Some(NOTIFICATIONS) {
                    self.owned.clear();
                }
            }
            // Only from the server of the ID: another connection of the host
            // bus can send the same signal.
            dbus::SIGNAL if m.is_signal(NOTIFICATIONS, "NotificationClosed") && m.path.as_deref() == Some(PATH) => {
                let id = m.body.first().and_then(Value::as_u32).unwrap_or(0);
                if id == 0 || self.issuer(id).is_none() || self.issuer(id) != m.sender {
                    return;
                }
                self.owned.retain(|o| o.1 != id);
                let reason = m.body.get(1).and_then(Value::as_u32).unwrap_or(4);
                self.broadcast(&json!({"signal": "closed", "id": id, "reason": reason}));
            }
            _ => {}
        }
    }

    /// Read from the client at `index`. False: the client is gone.
    fn on_client(&mut self, index: usize) -> Result<bool> {
        let mut chunk = vec![0u8; 64 * 1024];
        let c = &mut self.clients[index];
        // One read after poll: it does not block.
        let n = match c.stream.read(&mut chunk) {
            Ok(0) | Err(_) => return Ok(false),
            Ok(n) => n,
        };
        c.buf.extend_from_slice(&chunk[..n]);
        let id = c.id;
        let mut lines = Vec::new();
        while let Some(i) = c.buf.iter().position(|&b| b == b'\n') {
            lines.push(c.buf.drain(..=i).collect::<Vec<u8>>());
        }
        if c.buf.len() > MAX_LINE {
            return Ok(false);
        }
        for l in lines {
            self.on_line(id, &l[..l.len() - 1])?;
        }
        Ok(true)
    }
}

/// The match rules of NotificationClosed, and of a change of the server.
const RULES: [&str; 2] = [
    "type='signal',interface='org.freedesktop.Notifications',member='NotificationClosed',path='/org/freedesktop/Notifications'",
    "type='signal',sender='org.freedesktop.DBus',member='NameOwnerChanged',arg0='org.freedesktop.Notifications'",
];

/// Forward the notifications of the bridges that connect to `listener` to
/// the bus `bus_address`. Returns only with an error of the bus.
pub fn serve(listener: UnixListener, name: &str, color: &str, bus_address: &str) -> Result<()> {
    listener.local_addr().map_err(|e| format!("not a listening socket: {e}"))?;
    let mut bus = Conn::connect(bus_address)?;
    for rule in RULES {
        bus.call(Message::call("org.freedesktop.DBus", "/org/freedesktop/DBus", "org.freedesktop.DBus", "AddMatch", vec![Value::str(rule)]))?;
    }
    let mut h = Host {
        name,
        color,
        bus,
        clients: Vec::new(),
        next_client: 0,
        owned: VecDeque::new(),
        calls: HashMap::new(),
        bucket: Bucket { tokens: BURST, last: Instant::now() },
    };
    loop {
        while h.bus.has_pending() {
            if let Some(m) = h.bus.read()? {
                h.on_bus(m);
            }
        }
        let mut fds = vec![
            libc::pollfd { fd: listener.as_raw_fd(), events: libc::POLLIN, revents: 0 },
            libc::pollfd { fd: h.bus.fd(), events: libc::POLLIN, revents: 0 },
        ];
        fds.extend(h.clients.iter().map(|c| libc::pollfd { fd: c.stream.as_raw_fd(), events: libc::POLLIN, revents: 0 }));
        // SAFETY: fds is a valid array of fds.len() entries.
        if unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, -1) } < 0 {
            if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err(format!("poll: {}", std::io::Error::last_os_error()));
        }
        if fds[1].revents != 0 {
            if let Some(m) = h.bus.read()? {
                h.on_bus(m);
            }
        }
        // The clients from the last to the first: a removal does not move
        // the next ones.
        let ids: Vec<u64> = h.clients.iter().map(|c| c.id).collect();
        for (i, id) in ids.iter().enumerate().rev() {
            if fds[2 + i].revents == 0 {
                continue;
            }
            let Some(index) = h.clients.iter().position(|c| c.id == *id) else { continue };
            if !h.on_client(index)? {
                h.clients.retain(|c| c.id != *id);
            }
        }
        if fds[0].revents != 0 {
            if let Ok((stream, _)) = listener.accept() {
                if h.clients.len() < MAX_CLIENTS {
                    let _ = stream.set_write_timeout(Some(Duration::from_secs(1)));
                    h.next_client += 1;
                    h.clients.push(Client { id: h.next_client, stream, buf: Vec::new() });
                }
            }
        }
    }
}

/// `sandbox __notify_host NAME COLOR`, with the listening socket on fd 0.
pub fn main(args: &[String]) -> ! {
    let [name, color] = args else {
        die!("usage: sandbox __notify_host NAME COLOR (a listening socket on fd 0)");
    };
    // SAFETY: the container gives the listening socket on fd 0 (run.rs),
    // and nothing else in this process owns fd 0.
    let listener = unsafe { UnixListener::from_raw_fd(0) };
    let runtime = util::env("XDG_RUNTIME_DIR").unwrap_or_default();
    let addr = util::env("DBUS_SESSION_BUS_ADDRESS").filter(|s| !s.is_empty()).unwrap_or_else(|| format!("unix:path={runtime}/bus"));
    let err = serve(listener, name, color, &addr).err().unwrap_or_default();
    warn!("the notifications of {name} stop: {err}");
    std::process::exit(1)
}

#[cfg(test)]
#[path = "notify_tests.rs"]
mod end_to_end;

#[cfg(test)]
mod tests {
    use super::*;

    fn hint<'a>(args: &'a [Value], key: &str) -> Option<&'a Value> {
        let Value::Dict(_, _, entries) = &args[6] else { panic!() };
        entries.iter().find(|(k, _)| k.as_str() == Some(key)).map(|(_, v)| match v {
            Value::Variant(v) => &**v,
            _ => panic!(),
        })
    }

    #[test]
    fn the_name_of_the_sandbox_in_the_notification() {
        let n = Notify { app: "Firefox".into(), summary: "Hello\nthere\u{7}".into(), body: "a\nb\u{1b}[31m".into(), ..Default::default() };
        let a = host_args("work", "#008080", &n, 0);
        assert_eq!(a[0], Value::str("work: Firefox"));
        assert_eq!(a[3], Value::str("work: Hello there"));
        assert_eq!(a[4], Value::str("a\nb[31m"));
        assert_eq!(a[7], Value::I32(-1));
        assert_eq!(hint(&a, "frcolor"), Some(&Value::str("#008080")));
        // No app name and no summary: the name of the sandbox.
        let a = host_args("work", "#008080", &Notify::default(), 0);
        assert_eq!((&a[0], &a[3]), (&Value::str("work"), &Value::str("work")));
    }

    #[test]
    fn only_some_hints_and_icon_names() {
        let n = Notify {
            icon: "/home/user/.ssh/id_ed25519".into(),
            hints: Hints { urgency: Some(9), category: Some("im.received".into()), value: Some(150), ..Default::default() },
            timeout: i32::MAX,
            ..Default::default()
        };
        let a = host_args("w", "#000000", &n, 3);
        assert_eq!(a[1], Value::U32(3));
        assert_eq!(a[2], Value::str(""));
        assert_eq!(hint(&a, "urgency"), Some(&Value::Byte(2)));
        assert_eq!(hint(&a, "category"), Some(&Value::str("im.received")));
        assert_eq!(hint(&a, "value"), Some(&Value::I32(100)));
        // No actions.
        assert_eq!(a[5], Value::Array(Type::Str, vec![]));
        assert_eq!(a[7], Value::I32(3_600_000));
        for (icon, ok) in [("dialog-information", true), ("file:///etc/passwd", false), ("../x", false), (".hidden", false)] {
            let a = host_args("w", "#000000", &Notify { icon: icon.into(), ..Default::default() }, 0);
            assert_eq!(a[2] == Value::str(icon), ok, "{icon}");
        }
    }

    #[test]
    fn images() {
        let img = |w: i32, h: i32, stride: i32, bytes: usize| Image {
            width: w,
            height: h,
            rowstride: stride,
            alpha: true,
            bits: 8,
            channels: 4,
            data: hex(&vec![7u8; bytes]),
        };
        assert!(image(&img(2, 2, 8, 16)).is_some());
        // The last row can be without its padding.
        assert!(image(&img(2, 2, 12, 20)).is_some());
        assert!(image(&img(2, 2, 8, 15)).is_none());
        assert!(image(&img(2, 2, 8, 17)).is_none());
        assert!(image(&img(2, 2, 4, 16)).is_none());
        assert!(image(&img(0, 2, 8, 0)).is_none());
        assert!(image(&img(512, 1, 2048, 2048)).is_some());
        assert!(image(&img(513, 1, 2052, 2052)).is_none());
        assert!(image(&Image { data: "zz".into(), ..img(1, 1, 4, 4) }).is_none());
        assert!(image(&Image { channels: 3, ..img(2, 2, 8, 16) }).is_none());
    }

    #[test]
    fn hexadecimal_and_cuts() {
        assert_eq!(unhex(&hex(&[0, 1, 0xab, 0xff])), Some(vec![0, 1, 0xab, 0xff]));
        assert_eq!(unhex("abc"), None);
        assert_eq!(unhex("+1"), None);
        assert_eq!(cut("ééé", 3), "é");
        assert_eq!(line(&"x".repeat(300), 256).len(), 256);
    }

    #[test]
    fn the_rate_limit() {
        let t = Instant::now();
        let mut b = Bucket { tokens: BURST, last: t };
        for _ in 0..20 {
            assert!(b.take(t));
        }
        assert!(!b.take(t));
        assert!(b.take(t + Duration::from_millis(200)));
        assert!(!b.take(t + Duration::from_millis(200)));
    }
}
