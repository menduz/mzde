//! The Wayland socket of a sandbox: a security context of the compositor
//! (the protocol wp_security_context_v1). The programs of the sandbox do not
//! get the socket of the compositor. They get a socket that this process
//! makes and gives to the compositor with
//! `wp_security_context_manager_v1.create_listener`. The compositor knows
//! the clients of that socket, and does not give them the privileged
//! globals: in river, the virtual keyboard and pointer, the capture of the
//! screen, the data control of the clipboard, the window manager, the layer
//! shell, the lock of the session and the security context manager itself
//! (refer to Server.zig of river).
//!
//! The compositor accepts connections on the socket until the other end of
//! the close pipe closes: `Context` keeps it open. When the process stops,
//! the kernel closes it.
//!
//! The client speaks the wire protocol of Wayland itself (no libwayland):
//! it sends some requests and reads only the events of wl_display and
//! wl_registry.

use std::io::{ErrorKind, Read};
use std::os::fd::{AsRawFd, OwnedFd, RawFd};
use std::os::unix::net::{UnixListener, UnixStream};

use crate::util::Result;

/// The name of the sandbox engine for the compositor.
pub const ENGINE: &str = "mzde.sandboxes";

const MANAGER: &str = "wp_security_context_manager_v1";

/// A security context. While it exists, the compositor accepts connections
/// on its socket.
pub struct Context {
    _close: OwnedFd,
}

/// The socket of the compositor: WAYLAND_DISPLAY (a path, or a name in
/// `runtime`), else wayland-0.
pub fn compositor_socket(display: Option<String>, runtime: &str) -> String {
    let wl = display.filter(|s| !s.is_empty()).unwrap_or_else(|| "wayland-0".into());
    if wl.starts_with('/') { wl } else { format!("{runtime}/{wl}") }
}

/// Make the socket `path` and give it to the compositor of the socket
/// `compositor` as a security context with the app ID `app_id` and the
/// instance ID `instance_id`.
pub fn listen(compositor: &str, path: &str, app_id: &str, instance_id: &str) -> Result<Context> {
    let conn = UnixStream::connect(compositor).map_err(|e| format!("the Wayland compositor at {compositor}: {e}"))?;
    let _ = std::fs::remove_file(path);
    let listener = UnixListener::bind(path).map_err(|e| format!("cannot make the Wayland socket {path}: {e}"))?;
    let (close_r, close_w) = crate::process::pipe()?;
    let r = register(&conn, listener.as_raw_fd(), close_r.as_raw_fd(), app_id, instance_id);
    if r.is_err() {
        let _ = std::fs::remove_file(path);
    }
    r.map_err(|e| format!("the security context of Wayland at {compositor}: {e}"))?;
    // The compositor has its own copies of the socket and of the read end.
    Ok(Context { _close: close_w })
}

/// The requests of the protocol, on the connection `conn`.
fn register(conn: &UnixStream, listen_fd: RawFd, close_fd: RawFd, app_id: &str, instance_id: &str) -> Result<()> {
    const DISPLAY: u32 = 1;
    const REGISTRY: u32 = 2;
    const SYNC1: u32 = 3;
    const MANAGER_ID: u32 = 4;
    const CONTEXT: u32 = 5;
    const SYNC2: u32 = 6;
    // wl_display.get_registry, wl_display.sync.
    send(conn, DISPLAY, 1, &Args::new().uint(REGISTRY), &[])?;
    send(conn, DISPLAY, 0, &Args::new().uint(SYNC1), &[])?;
    let mut events = Events::default();
    let globals = events.until_done(conn, SYNC1)?;
    let Some((name, version)) = globals.iter().find(|g| g.1 == MANAGER).map(|g| (g.0, g.2)) else {
        return Err(format!("the compositor does not have {MANAGER}"));
    };
    // wl_registry.bind with a new_id of no fixed interface: the interface,
    // the version and the ID.
    send(conn, REGISTRY, 0, &Args::new().uint(name).string(MANAGER).uint(version.min(1)).uint(MANAGER_ID), &[])?;
    // create_listener(id, listen_fd, close_fd): the descriptors go with the
    // message, not in it.
    send(conn, MANAGER_ID, 1, &Args::new().uint(CONTEXT), &[listen_fd, close_fd])?;
    send(conn, CONTEXT, 1, &Args::new().string(ENGINE), &[])?;
    send(conn, CONTEXT, 2, &Args::new().string(app_id), &[])?;
    send(conn, CONTEXT, 3, &Args::new().string(instance_id), &[])?;
    // commit, then destroy of the context and of the manager. The context
    // stays in the compositor after commit.
    send(conn, CONTEXT, 4, &Args::new(), &[])?;
    send(conn, CONTEXT, 0, &Args::new(), &[])?;
    send(conn, MANAGER_ID, 0, &Args::new(), &[])?;
    // The compositor did all requests before the done of this sync: an
    // error comes before it.
    send(conn, DISPLAY, 0, &Args::new().uint(SYNC2), &[])?;
    events.until_done(conn, SYNC2)?;
    Ok(())
}

/// The arguments of a message.
struct Args(Vec<u8>);

impl Args {
    fn new() -> Args {
        Args(Vec::new())
    }
    fn uint(mut self, v: u32) -> Args {
        self.0.extend_from_slice(&v.to_ne_bytes());
        self
    }
    /// A string: its length with the NUL, the bytes, the NUL, and zeros to
    /// a multiple of 4 bytes.
    fn string(mut self, s: &str) -> Args {
        self.0.extend_from_slice(&(s.len() as u32 + 1).to_ne_bytes());
        self.0.extend_from_slice(s.as_bytes());
        self.0.push(0);
        while !self.0.len().is_multiple_of(4) {
            self.0.push(0);
        }
        self
    }
}

/// A message: the object, then the size (with the header) and the opcode.
fn message(object: u32, opcode: u16, args: &Args) -> Vec<u8> {
    let size = 8 + args.0.len() as u32;
    let mut m = object.to_ne_bytes().to_vec();
    m.extend_from_slice(&((size << 16) | opcode as u32).to_ne_bytes());
    m.extend_from_slice(&args.0);
    m
}

/// Send a message, with the descriptors `fds` (SCM_RIGHTS).
fn send(conn: &UnixStream, object: u32, opcode: u16, args: &Args, fds: &[RawFd]) -> Result<()> {
    let m = message(object, opcode, args);
    let mut iov = libc::iovec { iov_base: m.as_ptr() as *mut libc::c_void, iov_len: m.len() };
    // Space for up to 4 descriptors, aligned for cmsghdr.
    let mut cbuf = [0u64; 8];
    // SAFETY: msghdr of zeros is valid; the pointers are valid during the
    // call; CMSG_* stay in cbuf, which is large enough for 4 descriptors.
    let n = unsafe {
        let mut msg: libc::msghdr = std::mem::zeroed();
        msg.msg_iov = &mut iov;
        msg.msg_iovlen = 1;
        if !fds.is_empty() {
            let len = std::mem::size_of_val(fds) as u32;
            assert!(fds.len() <= 4);
            msg.msg_control = cbuf.as_mut_ptr().cast();
            msg.msg_controllen = libc::CMSG_SPACE(len) as usize;
            let c = libc::CMSG_FIRSTHDR(&msg);
            (*c).cmsg_level = libc::SOL_SOCKET;
            (*c).cmsg_type = libc::SCM_RIGHTS;
            (*c).cmsg_len = libc::CMSG_LEN(len) as usize;
            std::ptr::copy_nonoverlapping(fds.as_ptr(), libc::CMSG_DATA(c).cast::<RawFd>(), fds.len());
        }
        libc::sendmsg(conn.as_raw_fd(), &msg, libc::MSG_NOSIGNAL)
    };
    if n < 0 {
        return Err(format!("send: {}", std::io::Error::last_os_error()));
    }
    if n as usize != m.len() {
        return Err("send: a short write".into());
    }
    Ok(())
}

/// A global of wl_registry: its name, its interface and its version.
type Global = (u32, String, u32);

/// The bytes from the compositor that are not a full message yet.
#[derive(Default)]
struct Events {
    buf: Vec<u8>,
}

impl Events {
    /// Read the events until the wl_callback `callback` is done. The
    /// result: the globals of the registry in these events. A
    /// wl_display.error is an error.
    fn until_done(&mut self, conn: &UnixStream, callback: u32) -> Result<Vec<Global>> {
        let mut globals = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            while let Some((object, opcode, body)) = self.next() {
                match (object, opcode) {
                    // wl_display.error(object, code, message).
                    (1, 0) => {
                        let mut r = Reader(&body);
                        let (obj, code) = (r.uint()?, r.uint()?);
                        return Err(format!("protocol error {code} on object {obj}: {}", r.string()?));
                    }
                    // wl_registry.global(name, interface, version).
                    (2, 0) => {
                        let mut r = Reader(&body);
                        globals.push((r.uint()?, r.string()?, r.uint()?));
                    }
                    // wl_callback.done.
                    (o, 0) if o == callback => return Ok(globals),
                    _ => {}
                }
            }
            let mut c = conn;
            match c.read(&mut chunk) {
                Ok(0) => return Err("the compositor closed the connection".into()),
                Ok(n) => self.buf.extend_from_slice(&chunk[..n]),
                Err(e) if e.kind() == ErrorKind::Interrupted => {}
                Err(e) => return Err(format!("read: {e}")),
            }
        }
    }

    /// The next full message: the object, the opcode and the arguments.
    fn next(&mut self) -> Option<(u32, u16, Vec<u8>)> {
        if self.buf.len() < 8 {
            return None;
        }
        let word = |i: usize| u32::from_ne_bytes(self.buf[i..i + 4].try_into().unwrap());
        let (object, second) = (word(0), word(4));
        let size = (second >> 16) as usize;
        if size < 8 || self.buf.len() < size {
            return None;
        }
        let body = self.buf[8..size].to_vec();
        self.buf.drain(..size);
        Some((object, (second & 0xffff) as u16, body))
    }
}

/// Read the arguments of an event.
struct Reader<'a>(&'a [u8]);

impl Reader<'_> {
    fn uint(&mut self) -> Result<u32> {
        if self.0.len() < 4 {
            return Err("a short event".into());
        }
        let v = u32::from_ne_bytes(self.0[..4].try_into().unwrap());
        self.0 = &self.0[4..];
        Ok(v)
    }
    fn string(&mut self) -> Result<String> {
        let len = self.uint()? as usize;
        let padded = len.div_ceil(4) * 4;
        if len == 0 || self.0.len() < padded {
            return Err("a short event".into());
        }
        let s = String::from_utf8_lossy(&self.0[..len - 1]).into_owned();
        self.0 = &self.0[padded..];
        Ok(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::os::fd::FromRawFd;

    #[test]
    fn a_string_has_its_length_a_nul_and_padding() {
        let mut want = 4u32.to_ne_bytes().to_vec();
        want.extend_from_slice(b"abc\0");
        assert_eq!(Args::new().string("abc").0, want);
        let mut want = 5u32.to_ne_bytes().to_vec();
        want.extend_from_slice(b"abcd\0\0\0\0");
        assert_eq!(Args::new().string("abcd").0, want);
        assert_eq!(Args::new().string("").0.len(), 4 + 4);
    }

    #[test]
    fn the_header_has_the_object_the_size_and_the_opcode() {
        let m = message(5, 3, &Args::new().uint(7));
        assert_eq!(m.len(), 12);
        assert_eq!(u32::from_ne_bytes(m[0..4].try_into().unwrap()), 5);
        assert_eq!(u32::from_ne_bytes(m[4..8].try_into().unwrap()), (12 << 16) | 3);
    }

    #[test]
    fn the_compositor_socket() {
        assert_eq!(compositor_socket(None, "/r"), "/r/wayland-0");
        assert_eq!(compositor_socket(Some("".into()), "/r"), "/r/wayland-0");
        assert_eq!(compositor_socket(Some("wayland-1".into()), "/r"), "/r/wayland-1");
        assert_eq!(compositor_socket(Some("/x/wl".into()), "/r"), "/x/wl");
    }

    /// One message that the fake compositor got: the object, the opcode,
    /// the arguments and the number of descriptors.
    type Got = (u32, u16, Vec<u8>, usize);

    /// Read one message and its descriptors from the client.
    fn receive(s: &UnixStream) -> Option<Got> {
        let mut head = [0u8; 8];
        let mut cbuf = [0u64; 8];
        let mut iov = libc::iovec { iov_base: head.as_mut_ptr().cast(), iov_len: 8 };
        // SAFETY: valid buffers for recvmsg; the descriptors are new.
        let (n, nfds) = unsafe {
            let mut msg: libc::msghdr = std::mem::zeroed();
            msg.msg_iov = &mut iov;
            msg.msg_iovlen = 1;
            msg.msg_control = cbuf.as_mut_ptr().cast();
            msg.msg_controllen = std::mem::size_of_val(&cbuf);
            let n = libc::recvmsg(s.as_raw_fd(), &mut msg, libc::MSG_CMSG_CLOEXEC);
            let mut nfds = 0;
            let c = libc::CMSG_FIRSTHDR(&msg);
            if !c.is_null() && (*c).cmsg_type == libc::SCM_RIGHTS {
                nfds = ((*c).cmsg_len as usize - libc::CMSG_LEN(0) as usize) / 4;
                for i in 0..nfds {
                    drop(OwnedFd::from_raw_fd(*libc::CMSG_DATA(c).cast::<RawFd>().add(i)));
                }
            }
            (n, nfds)
        };
        if n != 8 {
            return None;
        }
        let object = u32::from_ne_bytes(head[0..4].try_into().unwrap());
        let second = u32::from_ne_bytes(head[4..8].try_into().unwrap());
        let mut body = vec![0u8; (second >> 16) as usize - 8];
        (&*s).read_exact(&mut body).ok()?;
        Some((object, (second & 0xffff) as u16, body, nfds))
    }

    fn event(object: u32, opcode: u16, args: Args) -> Vec<u8> {
        message(object, opcode, &args)
    }

    /// A fake compositor: the globals, then the done of each sync. With
    /// `error`, an error after the commit.
    fn fake(server: UnixStream, globals: &[(&str, u32)], error: bool) -> Vec<Got> {
        let mut got = Vec::new();
        let mut out = &server;
        while let Some(m) = receive(&server) {
            match (m.0, m.1) {
                (1, 1) => {
                    for (i, (iface, v)) in globals.iter().enumerate() {
                        out.write_all(&event(2, 0, Args::new().uint(10 + i as u32).string(iface).uint(*v))).unwrap();
                    }
                }
                (1, 0) => {
                    let id = u32::from_ne_bytes(m.2[..4].try_into().unwrap());
                    if error && got.iter().any(|g: &Got| g.0 == 5 && g.1 == 4) {
                        out.write_all(&event(1, 0, Args::new().uint(5).uint(1).string("invalid listen_fd"))).unwrap();
                    } else {
                        out.write_all(&event(id, 0, Args::new().uint(0))).unwrap();
                    }
                }
                _ => {}
            }
            got.push(m);
        }
        got
    }

    fn with_fake(globals: &'static [(&'static str, u32)], error: bool) -> (Result<()>, Vec<Got>) {
        let (client, server) = UnixStream::pair().unwrap();
        let t = std::thread::spawn(move || fake(server, globals, error));
        let dir = std::env::temp_dir().join(format!("sandbox-wl-{}-{error}-{}", std::process::id(), globals.len()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let listener = UnixListener::bind(dir.join("s")).unwrap();
        let (r, _w) = crate::process::pipe().unwrap();
        let res = register(&client, listener.as_raw_fd(), r.as_raw_fd(), "code", "code-1");
        drop(client);
        let got = t.join().unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        (res, got)
    }

    #[test]
    fn the_requests_of_a_security_context() {
        let (res, got) = with_fake(&[("wl_compositor", 6), (MANAGER, 1), ("wl_seat", 9)], false);
        res.unwrap();
        let ops: Vec<(u32, u16, usize)> = got.iter().map(|g| (g.0, g.1, g.3)).collect();
        assert_eq!(ops, [(1, 1, 0), (1, 0, 0), (2, 0, 0), (4, 1, 2), (5, 1, 0), (5, 2, 0), (5, 3, 0), (5, 4, 0), (5, 0, 0), (4, 0, 0), (1, 0, 0)]);
        // The bind: the name of the global, the interface, version 1, ID 4.
        assert_eq!(got[2].2, Args::new().uint(11).string(MANAGER).uint(1).uint(4).0);
        assert_eq!(got[4].2, Args::new().string(ENGINE).0);
        assert_eq!(got[5].2, Args::new().string("code").0);
        assert_eq!(got[6].2, Args::new().string("code-1").0);
    }

    #[test]
    fn no_manager_is_an_error() {
        let (res, got) = with_fake(&[("wl_compositor", 6)], false);
        assert_eq!(res.unwrap_err(), format!("the compositor does not have {MANAGER}"));
        // No bind, no listener.
        assert!(!got.iter().any(|g| g.0 == 2 || g.0 == 4));
    }

    #[test]
    fn a_protocol_error_is_an_error() {
        let (res, _) = with_fake(&[(MANAGER, 1)], true);
        assert_eq!(res.unwrap_err(), "protocol error 1 on object 5: invalid listen_fd");
    }
}
