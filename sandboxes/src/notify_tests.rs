//! The notifications of a sandbox from end to end, with two real
//! dbus-daemons: the bus of the "host" with a fake notification server, and
//! the private bus of the "sandbox" with the bridge. A client on the private
//! bus sends notifications; the test checks what the server of the host
//! gets and which signals come back. Without dbus-daemon, the test does not
//! run.

use std::os::unix::net::UnixListener;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use crate::dbus::{Conn, Message, Type, Value};
use crate::notify_host::{NOTIFICATIONS, PATH};
use crate::tools;

const DBUS: &str = "org.freedesktop.DBus";

struct Bus {
    child: Child,
    address: String,
}

impl Drop for Bus {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// A dbus-daemon with the session configuration, or None.
fn bus(dir: &std::path::Path, name: &str) -> Option<Bus> {
    let path = dir.join(name);
    let address = format!("unix:path={}", path.display());
    let child = Command::new(tools::dbus_daemon())
        .args([&format!("--config-file={}", tools::dbus_session_conf()), &format!("--address={address}"), "--nofork", "--nopidfile"])
        // No service directories of the user: the bus must not start a
        // notification server of the system.
        .env("XDG_DATA_HOME", dir.join("none"))
        .env("XDG_DATA_DIRS", dir.join("none"))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let start = Instant::now();
    while !path.exists() {
        if start.elapsed() > Duration::from_secs(10) {
            return None;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    Some(Bus { child, address })
}

fn dbus_call(c: &mut Conn, member: &str, body: Vec<Value>) -> Message {
    c.call(Message::call(DBUS, "/org/freedesktop/DBus", DBUS, member, body)).unwrap()
}

/// Wait until a program has the name `name` on the bus of `c`.
fn wait_owner(c: &mut Conn, name: &str) {
    let start = Instant::now();
    while dbus_call(c, "NameHasOwner", vec![Value::str(name)]).body[0] != Value::Bool(true) {
        assert!(start.elapsed() < Duration::from_secs(10), "nobody has {name}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// The next signal of the notification server on `c`, or None after 2 s.
fn next_signal(c: &mut Conn) -> Option<Message> {
    let fd = c.fd();
    let end = Instant::now() + Duration::from_secs(2);
    loop {
        if !c.has_pending() {
            let left = end.saturating_duration_since(Instant::now()).as_millis() as i32;
            let mut p = libc::pollfd { fd, events: libc::POLLIN, revents: 0 };
            // SAFETY: p is a valid pollfd.
            if left == 0 || unsafe { libc::poll(&mut p, 1, left) } <= 0 {
                return None;
            }
        }
        if let Some(m) = c.read().unwrap() {
            if m.interface.as_deref() == Some(NOTIFICATIONS) && m.kind == crate::dbus::SIGNAL {
                return Some(m);
            }
        }
    }
}

/// The fake notification server of the host: it gives the IDs 41, 42, ...
/// and sends the arguments of each Notify to `tx`. 300 ms after the first
/// one, it sends a NotificationClosed of a notification of the host (7),
/// then ActionInvoked and NotificationClosed of 41.
fn server(address: String, tx: mpsc::Sender<Vec<Value>>) {
    let mut c = Conn::connect(&address).unwrap();
    dbus_call(&mut c, "RequestName", vec![Value::str(NOTIFICATIONS), Value::U32(4)]);
    let mut next = 41;
    loop {
        // The bus stops at the end of the test.
        let Ok(m) = c.read() else { return };
        let Some(m) = m else { continue };
        if m.is_call(NOTIFICATIONS, "Notify") {
            let id = next;
            next += 1;
            c.send(&m.reply(vec![Value::U32(id)])).unwrap();
            tx.send(m.body.clone()).unwrap();
            if id == 41 {
                // The test sends a false signal in this time.
                std::thread::sleep(Duration::from_millis(300));
                let signal = |member: &str, v: Value| Message::signal(PATH, NOTIFICATIONS, member, vec![Value::U32(0), v]);
                let mut host = signal("NotificationClosed", Value::U32(1));
                host.body[0] = Value::U32(7);
                c.send(&host).unwrap();
                let mut action = signal("ActionInvoked", Value::str("default"));
                action.body[0] = Value::U32(41);
                c.send(&action).unwrap();
                let mut closed = signal("NotificationClosed", Value::U32(2));
                closed.body[0] = Value::U32(41);
                c.send(&closed).unwrap();
            }
        } else if m.is_call(NOTIFICATIONS, "CloseNotification") {
            c.send(&m.reply(vec![])).unwrap();
            tx.send(vec![Value::str("closed"), m.body[0].clone()]).unwrap();
        }
    }
}

fn notify(c: &mut Conn, replaces: u32, summary: &str, hints: Vec<(&str, Value)>) -> std::result::Result<Message, String> {
    let hints = hints.into_iter().map(|(k, v)| (Value::str(k), Value::Variant(Box::new(v)))).collect();
    c.call(Message::call(
        NOTIFICATIONS,
        PATH,
        NOTIFICATIONS,
        "Notify",
        vec![
            Value::str("app"),
            Value::U32(replaces),
            Value::str("/etc/passwd"),
            Value::str(summary),
            Value::str("body"),
            Value::Array(Type::Str, vec![Value::str("default"), Value::str("Open")]),
            Value::Dict(Type::Str, Type::Variant, hints),
            Value::I32(5000),
        ],
    ))
}

fn hint<'a>(args: &'a [Value], key: &str) -> Option<&'a Value> {
    let Value::Dict(_, _, entries) = &args[6] else { panic!() };
    entries.iter().find(|(k, _)| k.as_str() == Some(key)).map(|(_, v)| v)
}

#[test]
fn notifications_go_to_the_host_with_the_name_of_the_sandbox() {
    let dir = std::env::temp_dir().join(format!("sandbox-notify-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let (Some(host_bus), Some(sandbox_bus)) = (bus(&dir, "host-bus"), bus(&dir, "sandbox-bus")) else {
        eprintln!("no dbus-daemon: the test does not run");
        return;
    };

    let (tx, rx) = mpsc::channel();
    let address = host_bus.address.clone();
    std::thread::spawn(move || server(address, tx));
    let mut client = Conn::connect(&sandbox_bus.address).unwrap();
    let mut host_client = Conn::connect(&host_bus.address).unwrap();
    wait_owner(&mut host_client, NOTIFICATIONS);

    let socket = dir.join("notify");
    let listener = UnixListener::bind(&socket).unwrap();
    let address = host_bus.address.clone();
    std::thread::spawn(move || crate::notify_host::serve(listener, "work", "#008080", &address));
    let (address, path) = (sandbox_bus.address.clone(), socket.to_str().unwrap().to_string());
    std::thread::spawn(move || crate::notify_bridge::run(&address, &path));
    wait_owner(&mut client, NOTIFICATIONS);

    let rule = format!("type='signal',interface='{NOTIFICATIONS}'");
    dbus_call(&mut client, "AddMatch", vec![Value::str(&rule)]);

    // The first notification: the name of the sandbox, only some hints, no
    // path as the icon.
    let r = notify(&mut client, 0, "Hello\nworld", vec![("urgency", Value::Byte(1)), ("x-dunst-stack-tag", Value::str("host"))]).unwrap();
    assert_eq!(r.body, [Value::U32(41)]);
    let args = rx.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(args[0], Value::str("work: app"));
    assert_eq!(args[1], Value::U32(0));
    assert_eq!(args[2], Value::str(""));
    assert_eq!(args[3], Value::str("work: Hello world"));
    assert_eq!(args[7], Value::I32(5000));
    assert_eq!(hint(&args, "urgency"), Some(&Value::Variant(Box::new(Value::Byte(1)))));
    assert_eq!(hint(&args, "frcolor"), Some(&Value::Variant(Box::new(Value::str("#008080")))));
    assert_eq!(hint(&args, "x-dunst-stack-tag"), None);
    // No actions.
    assert_eq!(args[5], Value::Array(Type::Str, vec![]));

    // Another connection of the host bus sends a NotificationClosed of 41:
    // it does not come to the sandbox. Then only the NotificationClosed of
    // the server comes: not its ActionInvoked, not the signal of the
    // notification 7 of the host.
    host_client.send(&Message::signal(PATH, NOTIFICATIONS, "NotificationClosed", vec![Value::U32(41), Value::U32(1)])).unwrap();
    let s = next_signal(&mut client).expect("NotificationClosed");
    assert_eq!((s.member.as_deref(), &s.body[..]), (Some("NotificationClosed"), &[Value::U32(41), Value::U32(2)][..]));

    // A notification of the host (7) and a closed one (41) cannot be
    // replaced or closed.
    let r = notify(&mut client, 7, "again", vec![]).unwrap();
    assert_eq!(r.body, [Value::U32(42)]);
    assert_eq!(rx.recv_timeout(Duration::from_secs(5)).unwrap()[1], Value::U32(0));
    let r = notify(&mut client, 41, "again", vec![]).unwrap();
    assert_eq!(r.body, [Value::U32(43)]);
    assert_eq!(rx.recv_timeout(Duration::from_secs(5)).unwrap()[1], Value::U32(0));
    client.call(Message::call(NOTIFICATIONS, PATH, NOTIFICATIONS, "CloseNotification", vec![Value::U32(7)])).unwrap();
    // 43 is of the sandbox: it can replace and close it.
    notify(&mut client, 43, "replace", vec![]).unwrap();
    assert_eq!(rx.recv_timeout(Duration::from_secs(5)).unwrap()[1], Value::U32(43));
    client.call(Message::call(NOTIFICATIONS, PATH, NOTIFICATIONS, "CloseNotification", vec![Value::U32(43)])).unwrap();
    assert_eq!(rx.recv_timeout(Duration::from_secs(5)).unwrap(), [Value::str("closed"), Value::U32(43)]);
    // The close of 7 never reached the host.
    assert!(rx.try_recv().is_err());

    // The other methods of the server.
    let caps = client.call(Message::call(NOTIFICATIONS, PATH, NOTIFICATIONS, "GetCapabilities", vec![])).unwrap();
    assert!(matches!(&caps.body[0], Value::Array(Type::Str, c) if c.contains(&Value::str("body")) && !c.contains(&Value::str("actions"))));
    let info = client.call(Message::call(NOTIFICATIONS, PATH, NOTIFICATIONS, "GetServerInformation", vec![])).unwrap();
    assert_eq!(info.body[0], Value::str("sandbox"));
    assert!(client.call(Message::call(NOTIFICATIONS, "/x", NOTIFICATIONS, "Notify", vec![])).is_err());

    // The rate limit: 20 at once (the 4 above count), then errors.
    let errors = (0..30).filter(|_| notify(&mut client, 0, "many", vec![]).is_err()).count();
    assert!(errors >= 10, "{errors} errors");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Start the host side of the sandbox "work" on the bus `host`, and the
/// bridge on the bus `sandbox`, with the socket in `dir`.
fn start_forwarding(dir: &std::path::Path, host: &str, sandbox: &str) {
    let socket = dir.join("notify");
    let listener = UnixListener::bind(&socket).unwrap();
    let address = host.to_string();
    std::thread::spawn(move || crate::notify_host::serve(listener, "work", "#008080", &address));
    let (address, path) = (sandbox.to_string(), socket.to_str().unwrap().to_string());
    std::thread::spawn(move || crate::notify_bridge::run(&address, &path));
}

/// A notification server that answers each Notify with 41, 42, ... and
/// sends (label, member, ID) of each Notify with replaces_id and each
/// CloseNotification to `tx`. With `hand_over`, it gives the name to the
/// next server in the queue during its first Notify, then answers it.
fn named_server(address: String, label: &'static str, queue: bool, hand_over: bool, tx: mpsc::Sender<(&'static str, &'static str, u32)>) {
    let mut c = Conn::connect(&address).unwrap();
    // 0: wait in the queue of the name; 4: DO_NOT_QUEUE.
    dbus_call(&mut c, "RequestName", vec![Value::str(NOTIFICATIONS), Value::U32(if queue { 0 } else { 4 })]);
    let mut next = 41;
    let mut handed = false;
    loop {
        let Ok(m) = c.read() else { return };
        let Some(m) = m else { continue };
        if m.is_call(NOTIFICATIONS, "Notify") {
            if hand_over && !handed {
                handed = true;
                dbus_call(&mut c, "ReleaseName", vec![Value::str(NOTIFICATIONS)]);
            }
            if let Some(&Value::U32(r)) = m.body.get(1)
                && r != 0
            {
                tx.send((label, "Notify", r)).unwrap();
            }
            c.send(&m.reply(vec![Value::U32(next)])).unwrap();
            next += 1;
        } else if m.is_call(NOTIFICATIONS, "CloseNotification") {
            tx.send((label, "CloseNotification", m.body[0].as_u32().unwrap())).unwrap();
            c.send(&m.reply(vec![])).unwrap();
        }
    }
}

/// Server A gives ID 41 to the sandbox, but gives the name to server B
/// before its reply. A replace and a close of 41 must go
/// to A, the server of 41, never to B, which has its own notification 41.
#[test]
fn an_id_goes_back_to_its_server_after_a_change_of_the_server() {
    let dir = std::env::temp_dir().join(format!("sandbox-notify-owner-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let (Some(host_bus), Some(sandbox_bus)) = (bus(&dir, "host-bus"), bus(&dir, "sandbox-bus")) else {
        eprintln!("no dbus-daemon: the test does not run");
        return;
    };
    let (tx, rx) = mpsc::channel();
    let mut probe = Conn::connect(&host_bus.address).unwrap();
    let (address, t) = (host_bus.address.clone(), tx.clone());
    std::thread::spawn(move || named_server(address, "A", false, true, t));
    wait_owner(&mut probe, NOTIFICATIONS);
    let (address, t) = (host_bus.address.clone(), tx);
    std::thread::spawn(move || named_server(address, "B", true, false, t));
    // B waits in the queue of the name.
    let start = Instant::now();
    loop {
        let queue = dbus_call(&mut probe, "ListQueuedOwners", vec![Value::str(NOTIFICATIONS)]);
        if matches!(&queue.body[0], Value::Array(_, owners) if owners.len() == 2) {
            break;
        }
        assert!(start.elapsed() < Duration::from_secs(10), "B is not in the queue");
        std::thread::sleep(Duration::from_millis(10));
    }

    let mut client = Conn::connect(&sandbox_bus.address).unwrap();
    start_forwarding(&dir, &host_bus.address, &sandbox_bus.address);
    wait_owner(&mut client, NOTIFICATIONS);

    let r = notify(&mut client, 0, "first", vec![]).unwrap();
    assert_eq!(r.body, [Value::U32(41)]);
    let _ = notify(&mut client, 41, "replace", vec![]);
    let _ = client.call(Message::call(NOTIFICATIONS, PATH, NOTIFICATIONS, "CloseNotification", vec![Value::U32(41)]));
    let mut got = Vec::new();
    while let Ok(x) = rx.recv_timeout(Duration::from_secs(1)) {
        got.push(x);
    }
    assert!(!got.iter().any(|g| g.0 == "B"), "server B got an ID of server A: {got:?}");
    assert_eq!(got, [("A", "Notify", 41), ("A", "CloseNotification", 41)]);
    let _ = std::fs::remove_dir_all(&dir);
}
