//! A small client of D-Bus: the wire protocol itself (no libdbus), as
//! wayland.rs does for Wayland. It is for the notifications of the sandboxes
//! (notify_bridge.rs and notify_host.rs): it connects to a bus with the
//! authentication EXTERNAL, sends method calls, replies and signals, and
//! reads messages. It does not pass file descriptors.
//!
//! Only little-endian messages are read. A message that is not valid is
//! skipped; only a broken connection is an error. The limits (size of a
//! message, depth of the types) stop a bad peer from using much memory.

use std::collections::VecDeque;
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::net::UnixStream;

use crate::util::Result;

/// The maximum size of a message that this client reads or sends.
pub const MAX_MESSAGE: usize = 16 << 20;
/// The maximum depth of nested types (the specification allows 64).
const MAX_DEPTH: u32 = 32;

pub const METHOD_CALL: u8 = 1;
pub const METHOD_RETURN: u8 = 2;
pub const ERROR: u8 = 3;
pub const SIGNAL: u8 = 4;

/// The flag of a method call that does not want a reply.
pub const NO_REPLY_EXPECTED: u8 = 1;

/// A type of the D-Bus type system, without the file descriptor "h".
#[derive(Debug, Clone, PartialEq)]
pub enum Type {
    Byte,
    Bool,
    I16,
    U16,
    I32,
    U32,
    I64,
    U64,
    Double,
    Str,
    Path,
    Sig,
    Array(Box<Type>),
    Struct(Vec<Type>),
    Dict(Box<Type>, Box<Type>),
    Variant,
}

/// A value. An array of bytes ("ay") is `Bytes`, not an array of `Byte`.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Byte(u8),
    Bool(bool),
    I16(i16),
    U16(u16),
    I32(i32),
    U32(u32),
    I64(i64),
    U64(u64),
    Double(f64),
    Str(String),
    Path(String),
    Sig(String),
    Bytes(Vec<u8>),
    /// The type of the elements, and the elements.
    Array(Type, Vec<Value>),
    Struct(Vec<Value>),
    /// The types of the keys and of the values, and the entries.
    Dict(Type, Type, Vec<(Value, Value)>),
    Variant(Box<Value>),
}

impl Type {
    /// All types of the signature `sig`.
    pub fn parse(sig: &str) -> Result<Vec<Type>> {
        let b = sig.as_bytes();
        if b.len() > 255 {
            return Err("the signature is too long".into());
        }
        let mut pos = 0;
        let mut out = Vec::new();
        while pos < b.len() {
            let (t, n) = Type::one(&b[pos..], 0)?;
            out.push(t);
            pos += n;
        }
        Ok(out)
    }

    /// The first complete type of `b`, and its length in `b`.
    fn one(b: &[u8], depth: u32) -> Result<(Type, usize)> {
        if depth > MAX_DEPTH {
            return Err("the signature is too deep".into());
        }
        let Some(&c) = b.first() else {
            return Err("the signature ends in a type".into());
        };
        let basic = match c {
            b'y' => Some(Type::Byte),
            b'b' => Some(Type::Bool),
            b'n' => Some(Type::I16),
            b'q' => Some(Type::U16),
            b'i' => Some(Type::I32),
            b'u' => Some(Type::U32),
            b'x' => Some(Type::I64),
            b't' => Some(Type::U64),
            b'd' => Some(Type::Double),
            b's' => Some(Type::Str),
            b'o' => Some(Type::Path),
            b'g' => Some(Type::Sig),
            b'v' => Some(Type::Variant),
            _ => None,
        };
        if let Some(t) = basic {
            return Ok((t, 1));
        }
        match c {
            b'a' if b.get(1) == Some(&b'{') => {
                let (k, nk) = Type::one(&b[2..], depth + 1)?;
                if matches!(k, Type::Array(_) | Type::Struct(_) | Type::Dict(..) | Type::Variant) {
                    return Err("the key of a dictionary is not a basic type".into());
                }
                let (v, nv) = Type::one(&b[2 + nk..], depth + 1)?;
                if b.get(2 + nk + nv) != Some(&b'}') {
                    return Err("a dictionary entry has not two types".into());
                }
                Ok((Type::Dict(Box::new(k), Box::new(v)), 3 + nk + nv))
            }
            b'a' => {
                let (t, n) = Type::one(&b[1..], depth + 1)?;
                Ok((Type::Array(Box::new(t)), 1 + n))
            }
            b'(' => {
                let mut pos = 1;
                let mut fields = Vec::new();
                while b.get(pos) != Some(&b')') {
                    let (t, n) = Type::one(&b[pos..], depth + 1)?;
                    fields.push(t);
                    pos += n;
                }
                if fields.is_empty() {
                    return Err("an empty struct".into());
                }
                Ok((Type::Struct(fields), pos + 1))
            }
            _ => Err(format!("the type '{}' is not known", c as char)),
        }
    }

    pub fn write_sig(&self, out: &mut String) {
        let c = match self {
            Type::Byte => 'y',
            Type::Bool => 'b',
            Type::I16 => 'n',
            Type::U16 => 'q',
            Type::I32 => 'i',
            Type::U32 => 'u',
            Type::I64 => 'x',
            Type::U64 => 't',
            Type::Double => 'd',
            Type::Str => 's',
            Type::Path => 'o',
            Type::Sig => 'g',
            Type::Variant => 'v',
            Type::Array(t) => {
                out.push('a');
                return t.write_sig(out);
            }
            Type::Struct(fields) => {
                out.push('(');
                for f in fields {
                    f.write_sig(out);
                }
                out.push(')');
                return;
            }
            Type::Dict(k, v) => {
                out.push_str("a{");
                k.write_sig(out);
                v.write_sig(out);
                out.push('}');
                return;
            }
        };
        out.push(c);
    }

    fn align(&self) -> usize {
        match self {
            Type::Byte | Type::Sig | Type::Variant => 1,
            Type::I16 | Type::U16 => 2,
            Type::Bool | Type::I32 | Type::U32 | Type::Str | Type::Path | Type::Array(_) | Type::Dict(..) => 4,
            Type::I64 | Type::U64 | Type::Double | Type::Struct(_) => 8,
        }
    }
}

impl Value {
    pub fn ty(&self) -> Type {
        match self {
            Value::Byte(_) => Type::Byte,
            Value::Bool(_) => Type::Bool,
            Value::I16(_) => Type::I16,
            Value::U16(_) => Type::U16,
            Value::I32(_) => Type::I32,
            Value::U32(_) => Type::U32,
            Value::I64(_) => Type::I64,
            Value::U64(_) => Type::U64,
            Value::Double(_) => Type::Double,
            Value::Str(_) => Type::Str,
            Value::Path(_) => Type::Path,
            Value::Sig(_) => Type::Sig,
            Value::Bytes(_) => Type::Array(Box::new(Type::Byte)),
            Value::Array(t, _) => Type::Array(Box::new(t.clone())),
            Value::Struct(f) => Type::Struct(f.iter().map(Value::ty).collect()),
            Value::Dict(k, v, _) => Type::Dict(Box::new(k.clone()), Box::new(v.clone())),
            Value::Variant(_) => Type::Variant,
        }
    }

    pub fn str(s: &str) -> Value {
        Value::Str(s.to_string())
    }

    /// The string of a Str, Path or Sig.
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::Str(s) | Value::Path(s) | Value::Sig(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_u32(&self) -> Option<u32> {
        match self {
            Value::U32(v) => Some(*v),
            _ => None,
        }
    }
}

/// The signature of the values `body`.
pub fn signature(body: &[Value]) -> String {
    let mut s = String::new();
    for v in body {
        v.ty().write_sig(&mut s);
    }
    s
}

/// The encoder of the little-endian wire format. Alignment is relative to
/// the start of the buffer: a message body starts at a multiple of 8.
struct Enc {
    buf: Vec<u8>,
}

impl Enc {
    fn pad(&mut self, align: usize) {
        while self.buf.len() % align != 0 {
            self.buf.push(0);
        }
    }

    fn u32(&mut self, v: u32) {
        self.pad(4);
        self.buf.extend_from_slice(&v.to_le_bytes());
    }

    fn string(&mut self, s: &str) {
        self.u32(s.len() as u32);
        self.buf.extend_from_slice(s.as_bytes());
        self.buf.push(0);
    }

    fn sig(&mut self, s: &str) {
        self.buf.push(s.len() as u8);
        self.buf.extend_from_slice(s.as_bytes());
        self.buf.push(0);
    }

    /// An array: the length, the padding to the elements, the elements.
    fn array(&mut self, elem_align: usize, f: impl FnOnce(&mut Enc)) {
        self.pad(4);
        let at = self.buf.len();
        self.buf.extend_from_slice(&[0; 4]);
        self.pad(elem_align);
        let start = self.buf.len();
        f(self);
        let len = (self.buf.len() - start) as u32;
        self.buf[at..at + 4].copy_from_slice(&len.to_le_bytes());
    }

    fn value(&mut self, v: &Value) {
        match v {
            Value::Byte(b) => self.buf.push(*b),
            Value::Bool(b) => self.u32(*b as u32),
            Value::I16(n) => {
                self.pad(2);
                self.buf.extend_from_slice(&n.to_le_bytes());
            }
            Value::U16(n) => {
                self.pad(2);
                self.buf.extend_from_slice(&n.to_le_bytes());
            }
            Value::I32(n) => {
                self.pad(4);
                self.buf.extend_from_slice(&n.to_le_bytes());
            }
            Value::U32(n) => self.u32(*n),
            Value::I64(n) => {
                self.pad(8);
                self.buf.extend_from_slice(&n.to_le_bytes());
            }
            Value::U64(n) => {
                self.pad(8);
                self.buf.extend_from_slice(&n.to_le_bytes());
            }
            Value::Double(d) => {
                self.pad(8);
                self.buf.extend_from_slice(&d.to_le_bytes());
            }
            Value::Str(s) | Value::Path(s) => self.string(s),
            Value::Sig(s) => self.sig(s),
            Value::Bytes(b) => self.array(1, |e| e.buf.extend_from_slice(b)),
            Value::Array(t, items) => self.array(t.align(), |e| {
                for i in items {
                    e.value(i);
                }
            }),
            Value::Struct(fields) => {
                self.pad(8);
                for f in fields {
                    self.value(f);
                }
            }
            Value::Dict(_, _, entries) => self.array(8, |e| {
                for (k, v) in entries {
                    e.pad(8);
                    e.value(k);
                    e.value(v);
                }
            }),
            Value::Variant(inner) => {
                let mut s = String::new();
                inner.ty().write_sig(&mut s);
                self.sig(&s);
                self.value(inner);
            }
        }
    }
}

/// The decoder of the little-endian wire format.
struct Dec<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl Dec<'_> {
    fn align(&mut self, a: usize) -> Result<()> {
        let p = self.pos.div_ceil(a) * a;
        if p > self.buf.len() {
            return Err("the message ends in a padding".into());
        }
        self.pos = p;
        Ok(())
    }

    fn take(&mut self, n: usize) -> Result<&[u8]> {
        if n > self.buf.len() - self.pos {
            return Err("the message ends in a value".into());
        }
        let s = &self.buf[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }

    fn fixed<const N: usize>(&mut self) -> Result<[u8; N]> {
        self.align(N)?;
        Ok(self.take(N)?.try_into().unwrap())
    }

    fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.fixed()?))
    }

    fn text(&mut self, len: usize) -> Result<String> {
        let b = self.take(len + 1)?;
        if b[len] != 0 {
            return Err("a string without its NUL".into());
        }
        let s = std::str::from_utf8(&b[..len]).map_err(|_| "a string that is not UTF-8")?;
        if s.contains('\0') {
            return Err("a NUL in a string".into());
        }
        Ok(s.to_string())
    }

    fn sig(&mut self) -> Result<String> {
        let len = self.take(1)?[0] as usize;
        self.text(len)
    }

    fn value(&mut self, t: &Type, depth: u32) -> Result<Value> {
        if depth > MAX_DEPTH {
            return Err("the value is too deep".into());
        }
        Ok(match t {
            Type::Byte => Value::Byte(self.take(1)?[0]),
            Type::Bool => match self.u32()? {
                0 => Value::Bool(false),
                1 => Value::Bool(true),
                _ => return Err("a boolean that is not 0 or 1".into()),
            },
            Type::I16 => Value::I16(i16::from_le_bytes(self.fixed()?)),
            Type::U16 => Value::U16(u16::from_le_bytes(self.fixed()?)),
            Type::I32 => Value::I32(i32::from_le_bytes(self.fixed()?)),
            Type::U32 => Value::U32(self.u32()?),
            Type::I64 => Value::I64(i64::from_le_bytes(self.fixed()?)),
            Type::U64 => Value::U64(u64::from_le_bytes(self.fixed()?)),
            Type::Double => Value::Double(f64::from_le_bytes(self.fixed()?)),
            Type::Str | Type::Path => {
                let len = self.u32()? as usize;
                let s = self.text(len)?;
                if *t == Type::Str { Value::Str(s) } else { Value::Path(s) }
            }
            Type::Sig => Value::Sig(self.sig()?),
            Type::Array(elem) => {
                let len = self.u32()? as usize;
                self.align(elem.align())?;
                if len > self.buf.len() - self.pos {
                    return Err("an array longer than the message".into());
                }
                let end = self.pos + len;
                if **elem == Type::Byte {
                    return Ok(Value::Bytes(self.take(len)?.to_vec()));
                }
                let mut items = Vec::new();
                while self.pos < end {
                    items.push(self.value(elem, depth + 1)?);
                }
                if self.pos != end {
                    return Err("an array with a wrong length".into());
                }
                Value::Array((**elem).clone(), items)
            }
            Type::Dict(k, v) => {
                let len = self.u32()? as usize;
                self.align(8)?;
                if len > self.buf.len() - self.pos {
                    return Err("an array longer than the message".into());
                }
                let end = self.pos + len;
                let mut entries = Vec::new();
                while self.pos < end {
                    self.align(8)?;
                    let key = self.value(k, depth + 1)?;
                    let val = self.value(v, depth + 1)?;
                    entries.push((key, val));
                }
                if self.pos != end {
                    return Err("an array with a wrong length".into());
                }
                Value::Dict((**k).clone(), (**v).clone(), entries)
            }
            Type::Struct(fields) => {
                self.align(8)?;
                let mut out = Vec::new();
                for f in fields {
                    out.push(self.value(f, depth + 1)?);
                }
                Value::Struct(out)
            }
            Type::Variant => {
                let sig = self.sig()?;
                let mut types = Type::parse(&sig)?;
                if types.len() != 1 {
                    return Err("a variant without one type".into());
                }
                Value::Variant(Box::new(self.value(&types.remove(0), depth + 1)?))
            }
        })
    }
}

/// A message of D-Bus.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Message {
    pub kind: u8,
    pub flags: u8,
    pub serial: u32,
    pub path: Option<String>,
    pub interface: Option<String>,
    pub member: Option<String>,
    pub error_name: Option<String>,
    pub reply_serial: Option<u32>,
    pub destination: Option<String>,
    pub sender: Option<String>,
    pub body: Vec<Value>,
}

impl Message {
    pub fn call(destination: &str, path: &str, interface: &str, member: &str, body: Vec<Value>) -> Message {
        Message {
            kind: METHOD_CALL,
            destination: Some(destination.into()),
            path: Some(path.into()),
            interface: Some(interface.into()),
            member: Some(member.into()),
            body,
            ..Default::default()
        }
    }

    pub fn signal(path: &str, interface: &str, member: &str, body: Vec<Value>) -> Message {
        Message {
            kind: SIGNAL,
            path: Some(path.into()),
            interface: Some(interface.into()),
            member: Some(member.into()),
            body,
            ..Default::default()
        }
    }

    /// The reply to the method call `self`.
    pub fn reply(&self, body: Vec<Value>) -> Message {
        Message {
            kind: METHOD_RETURN,
            reply_serial: Some(self.serial),
            destination: self.sender.clone(),
            body,
            ..Default::default()
        }
    }

    /// The error reply to the method call `self`.
    pub fn error(&self, name: &str, text: &str) -> Message {
        Message {
            kind: ERROR,
            error_name: Some(name.into()),
            reply_serial: Some(self.serial),
            destination: self.sender.clone(),
            body: vec![Value::str(text)],
            ..Default::default()
        }
    }

    #[cfg(test)]
    pub fn is_call(&self, interface: &str, member: &str) -> bool {
        self.kind == METHOD_CALL && self.interface.as_deref() == Some(interface) && self.member.as_deref() == Some(member)
    }

    pub fn is_signal(&self, interface: &str, member: &str) -> bool {
        self.kind == SIGNAL && self.interface.as_deref() == Some(interface) && self.member.as_deref() == Some(member)
    }

    /// The bytes of the message, with the serial `serial`.
    pub fn encode(&self, serial: u32) -> Result<Vec<u8>> {
        let mut body = Enc { buf: Vec::new() };
        for v in &self.body {
            body.value(v);
        }
        let mut fields: Vec<Value> = Vec::new();
        let mut field = |code: u8, v: Value| fields.push(Value::Struct(vec![Value::Byte(code), Value::Variant(Box::new(v))]));
        if let Some(p) = &self.path {
            field(1, Value::Path(p.clone()));
        }
        if let Some(s) = &self.interface {
            field(2, Value::str(s));
        }
        if let Some(s) = &self.member {
            field(3, Value::str(s));
        }
        if let Some(s) = &self.error_name {
            field(4, Value::str(s));
        }
        if let Some(n) = self.reply_serial {
            field(5, Value::U32(n));
        }
        if let Some(s) = &self.destination {
            field(6, Value::str(s));
        }
        if let Some(s) = &self.sender {
            field(7, Value::str(s));
        }
        if !self.body.is_empty() {
            field(8, Value::Sig(signature(&self.body)));
        }
        let mut e = Enc { buf: vec![b'l', self.kind, self.flags, 1] };
        e.buf.extend_from_slice(&(body.buf.len() as u32).to_le_bytes());
        e.buf.extend_from_slice(&serial.to_le_bytes());
        let field_type = Type::Struct(vec![Type::Byte, Type::Variant]);
        e.value(&Value::Array(field_type, fields));
        e.pad(8);
        e.buf.extend_from_slice(&body.buf);
        if e.buf.len() > MAX_MESSAGE {
            return Err("the message is too large".into());
        }
        Ok(e.buf)
    }

    /// The message of the bytes `b` (one whole message).
    pub fn decode(b: &[u8]) -> Result<Message> {
        if b.len() < 16 || b[0] != b'l' {
            return Err("not a little-endian message".into());
        }
        if b[3] != 1 {
            return Err("not a message of version 1".into());
        }
        let mut d = Dec { buf: b, pos: 12 };
        let field_type = Type::Array(Box::new(Type::Struct(vec![Type::Byte, Type::Variant])));
        let Value::Array(_, fields) = d.value(&field_type, 0)? else {
            unreachable!()
        };
        d.align(8)?;
        let body_len = u32::from_le_bytes(b[4..8].try_into().unwrap()) as usize;
        if b.len() - d.pos != body_len {
            return Err("the body has a wrong length".into());
        }
        let mut m = Message {
            kind: b[1],
            flags: b[2],
            serial: u32::from_le_bytes(b[8..12].try_into().unwrap()),
            ..Default::default()
        };
        let mut sig = String::new();
        for f in fields {
            let Value::Struct(f) = f else { continue };
            let (Value::Byte(code), Value::Variant(v)) = (&f[0], &f[1]) else { continue };
            let s = v.as_str().map(String::from);
            match code {
                1 => m.path = s,
                2 => m.interface = s,
                3 => m.member = s,
                4 => m.error_name = s,
                5 => m.reply_serial = v.as_u32(),
                6 => m.destination = s,
                7 => m.sender = s,
                8 => sig = s.unwrap_or_default(),
                _ => {}
            }
        }
        for t in Type::parse(&sig)? {
            m.body.push(d.value(&t, 0)?);
        }
        if d.pos != b.len() {
            return Err("the body is longer than its signature".into());
        }
        Ok(m)
    }
}

/// The socket of a bus address: the first "unix:path=" or "unix:abstract="
/// of `address`. Values with %-escapes are decoded.
pub fn socket_of(address: &str) -> Option<(bool, String)> {
    for a in address.split(';') {
        let Some(params) = a.strip_prefix("unix:") else { continue };
        for p in params.split(',') {
            if let Some(v) = p.strip_prefix("path=") {
                return Some((false, unescape(v)));
            }
            if let Some(v) = p.strip_prefix("abstract=") {
                return Some((true, unescape(v)));
            }
        }
    }
    None
}

fn unescape(v: &str) -> String {
    let b = v.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            let hex = std::str::from_utf8(&b[i + 1..i + 3]).unwrap_or("");
            if let Ok(n) = u8::from_str_radix(hex, 16) {
                out.push(n);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// A connection to a bus.
pub struct Conn {
    stream: UnixStream,
    serial: u32,
    /// The messages that `call` read while it waited for its reply.
    pending: VecDeque<Message>,
    /// The unique name of this connection.
    pub name: String,
}

impl Conn {
    /// Connect to the bus `address`, authenticate, and say Hello.
    pub fn connect(address: &str) -> Result<Conn> {
        let (abstract_, path) = socket_of(address).ok_or_else(|| format!("the bus address {address} has no unix path"))?;
        let stream = if abstract_ {
            use std::os::linux::net::SocketAddrExt;
            let addr = std::os::unix::net::SocketAddr::from_abstract_name(path.as_bytes()).map_err(|e| e.to_string())?;
            UnixStream::connect_addr(&addr)
        } else {
            UnixStream::connect(&path)
        }
        .map_err(|e| format!("the bus {address}: {e}"))?;
        let mut c = Conn { stream, serial: 0, pending: VecDeque::new(), name: String::new() };
        c.auth().map_err(|e| format!("the bus {address}: {e}"))?;
        let r = c.call(Message::call("org.freedesktop.DBus", "/org/freedesktop/DBus", "org.freedesktop.DBus", "Hello", vec![]))?;
        c.name = r.body.first().and_then(Value::as_str).unwrap_or_default().to_string();
        Ok(c)
    }

    fn auth(&mut self) -> Result<()> {
        // SAFETY: getuid has no failure.
        let uid = unsafe { libc::getuid() }.to_string();
        let hex: String = uid.bytes().map(|b| format!("{b:02x}")).collect();
        self.stream.write_all(format!("\0AUTH EXTERNAL {hex}\r\n").as_bytes()).map_err(|e| e.to_string())?;
        let line = self.auth_line()?;
        if !line.starts_with("OK ") {
            return Err(format!("the authentication failed: {line}"));
        }
        self.stream.write_all(b"BEGIN\r\n").map_err(|e| e.to_string())
    }

    /// One line of the authentication, without "\r\n". Byte by byte: the
    /// messages after it stay in the socket.
    fn auth_line(&mut self) -> Result<String> {
        let mut line = Vec::new();
        let mut b = [0u8; 1];
        while !line.ends_with(b"\r\n") {
            if line.len() > 1024 {
                return Err("the authentication line is too long".into());
            }
            match self.stream.read(&mut b) {
                Ok(0) => return Err("the bus closed the connection".into()),
                Ok(_) => line.push(b[0]),
                Err(e) => return Err(e.to_string()),
            }
        }
        line.truncate(line.len() - 2);
        Ok(String::from_utf8_lossy(&line).into_owned())
    }

    pub fn fd(&self) -> RawFd {
        self.stream.as_raw_fd()
    }

    /// Whether `read` has a message without a read from the socket.
    pub fn has_pending(&self) -> bool {
        !self.pending.is_empty()
    }

    /// Send `m` with the next serial, and return the serial.
    pub fn send(&mut self, m: &Message) -> Result<u32> {
        self.serial = self.serial.wrapping_add(1).max(1);
        let bytes = m.encode(self.serial)?;
        self.stream.write_all(&bytes).map_err(|e| format!("the bus: {e}"))?;
        Ok(self.serial)
    }

    /// The next message. It blocks until a whole message is there. None: a
    /// message that is not valid (skipped). An error: the connection broke.
    pub fn read(&mut self) -> Result<Option<Message>> {
        if let Some(m) = self.pending.pop_front() {
            return Ok(Some(m));
        }
        let mut head = [0u8; 16];
        self.stream.read_exact(&mut head).map_err(|e| format!("the bus: {e}"))?;
        let le = head[0] == b'l';
        let n = |b: &[u8]| {
            let a: [u8; 4] = b.try_into().unwrap();
            if le { u32::from_le_bytes(a) } else { u32::from_be_bytes(a) }
        } as usize;
        let body_len = n(&head[4..8]);
        let fields_len = n(&head[12..16]);
        let total = (16 + fields_len).div_ceil(8) * 8 + body_len;
        if total > MAX_MESSAGE {
            return Err("the bus sent a message that is too large".into());
        }
        let mut b = head.to_vec();
        b.resize(total, 0);
        self.stream.read_exact(&mut b[16..]).map_err(|e| format!("the bus: {e}"))?;
        Ok(Message::decode(&b).ok())
    }

    /// Send the method call `m` and wait for its reply. The other messages
    /// stay for `read`. An error reply is an error.
    pub fn call(&mut self, m: Message) -> Result<Message> {
        let serial = self.send(&m)?;
        let mut other = VecDeque::new();
        let r = loop {
            let Some(r) = self.read_stream()? else { continue };
            if r.reply_serial == Some(serial) && (r.kind == METHOD_RETURN || r.kind == ERROR) {
                break r;
            }
            other.push_back(r);
        };
        self.pending.extend(other);
        if r.kind == ERROR {
            let name = r.error_name.clone().unwrap_or_default();
            let text = r.body.first().and_then(Value::as_str).unwrap_or_default();
            return Err(format!("{}: {name}: {text}", m.member.unwrap_or_default()));
        }
        Ok(r)
    }

    /// `read`, but never from `pending`.
    fn read_stream(&mut self) -> Result<Option<Message>> {
        let saved = std::mem::take(&mut self.pending);
        let r = self.read();
        self.pending = saved;
        r
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip(m: &Message) -> Message {
        Message::decode(&m.encode(7).unwrap()).unwrap()
    }

    #[test]
    fn signatures() {
        let t = Type::parse("susssasa{sv}i").unwrap();
        assert_eq!(t.len(), 8);
        assert_eq!(t[6], Type::Dict(Box::new(Type::Str), Box::new(Type::Variant)));
        let mut s = String::new();
        for x in &t {
            x.write_sig(&mut s);
        }
        assert_eq!(s, "susssasa{sv}i");
        assert_eq!(Type::parse("(iiibiiay)").unwrap()[0].align(), 8);
        for bad in ["a", "(", "()", "a{vs}", "a{s}", "z", "a{sss}", &"a".repeat(40)] {
            assert!(Type::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn a_notification_round_trip() {
        let image = Value::Struct(vec![
            Value::I32(2),
            Value::I32(1),
            Value::I32(8),
            Value::Bool(true),
            Value::I32(8),
            Value::I32(4),
            Value::Bytes(vec![1, 2, 3, 4, 5, 6, 7, 8]),
        ]);
        let hints = Value::Dict(
            Type::Str,
            Type::Variant,
            vec![
                (Value::str("urgency"), Value::Variant(Box::new(Value::Byte(2)))),
                (Value::str("image-data"), Value::Variant(Box::new(image))),
                (Value::str("x"), Value::Variant(Box::new(Value::Double(1.5)))),
            ],
        );
        let mut m = Message::call(
            "org.freedesktop.Notifications",
            "/org/freedesktop/Notifications",
            "org.freedesktop.Notifications",
            "Notify",
            vec![
                Value::str("app"),
                Value::U32(0),
                Value::str(""),
                Value::str("Summary é"),
                Value::str("Body"),
                Value::Array(Type::Str, vec![Value::str("default"), Value::str("Open")]),
                hints,
                Value::I32(-1),
            ],
        );
        m.sender = Some(":1.4".into());
        let r = round_trip(&m);
        assert_eq!(r.serial, 7);
        assert_eq!(r, Message { serial: 7, ..m.clone() });
        assert_eq!(signature(&r.body), "susssasa{sv}i");
        // The reply goes to the sender of the call.
        let reply = round_trip(&m.reply(vec![Value::U32(3)]));
        assert_eq!((reply.kind, reply.reply_serial, reply.destination.as_deref()), (METHOD_RETURN, Some(0), Some(":1.4")));
        let err = round_trip(&m.error("org.x.Error", "no"));
        assert_eq!((err.kind, err.error_name.as_deref(), err.body[0].as_str()), (ERROR, Some("org.x.Error"), Some("no")));
    }

    #[test]
    fn empty_arrays_and_alignment() {
        let m = Message::signal(
            "/a",
            "org.x",
            "S",
            vec![
                Value::Byte(1),
                Value::Array(Type::U64, vec![]),
                Value::Dict(Type::Str, Type::Variant, vec![]),
                Value::Array(Type::U64, vec![Value::U64(u64::MAX)]),
                Value::I16(-2),
                Value::Path("/o".into()),
                Value::Sig("a{sv}".into()),
            ],
        );
        assert_eq!(round_trip(&m), Message { serial: 7, ..m });
    }

    #[test]
    fn bad_messages_are_errors() {
        let m = Message::signal("/a", "org.x", "S", vec![Value::str("hello"), Value::U32(1)]);
        let good = m.encode(1).unwrap();
        // Each cut of the message is an error, never a panic.
        for n in 0..good.len() {
            assert!(Message::decode(&good[..n]).is_err(), "{n}");
        }
        // Random changes are errors or other messages, never a panic.
        for i in 0..good.len() {
            for v in [0u8, 0xff, 0x7f, 1] {
                let mut b = good.clone();
                b[i] = v;
                let _ = Message::decode(&b);
            }
        }
        let mut big_endian = good.clone();
        big_endian[0] = b'B';
        assert!(Message::decode(&big_endian).is_err());
        // An array length beyond the message.
        let a = Message::signal("/a", "org.x", "S", vec![Value::Array(Type::U32, vec![Value::U32(1)])]).encode(1).unwrap();
        let mut b = a.clone();
        let at = b.len() - 8;
        b[at..at + 4].copy_from_slice(&0xffff_fff0u32.to_le_bytes());
        assert!(Message::decode(&b).is_err());
    }

    #[test]
    fn addresses() {
        assert_eq!(socket_of("unix:path=/run/user/1000/bus"), Some((false, "/run/user/1000/bus".into())));
        assert_eq!(socket_of("unix:path=/tmp/a%20b,guid=123"), Some((false, "/tmp/a b".into())));
        assert_eq!(socket_of("tcp:host=x;unix:abstract=/tmp/dbus-1"), Some((true, "/tmp/dbus-1".into())));
        assert_eq!(socket_of("tcp:host=x,port=1"), None);
    }
}
