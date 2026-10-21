//! Minimal JDWP (Java Debug Wire Protocol) client.
//!
//! Oracle's `DBMS_DEBUG_JDWP` speaks JDWP: the database session under test
//! connects to the debugger, which then drives it with JDWP commands. This
//! module implements the wire format and the commands the PL/SQL debugger
//! needs. It is synchronous: replies are matched by id, events that arrive in
//! between are queued.

use std::collections::VecDeque;
use std::io::{self, Read, Write};
use std::net::TcpStream;
use std::time::Duration;

pub const HANDSHAKE: &[u8] = b"JDWP-Handshake";
const REPLY_FLAG: u8 = 0x80;

#[derive(Debug, thiserror::Error)]
pub enum JdwpError {
    #[error("connection: {0}")]
    Io(#[from] io::Error),
    #[error("JDWP error {code} ({})", error_name(*.code))]
    Reply { code: u16 },
    #[error("malformed JDWP packet: {0}")]
    Protocol(String),
    #[error("debuggee disconnected")]
    Disconnected,
}

pub type Result<T> = std::result::Result<T, JdwpError>;

/// Names of common JDWP error codes.
pub fn error_name(code: u16) -> &'static str {
    match code {
        10 => "INVALID_THREAD",
        13 => "THREAD_NOT_SUSPENDED",
        20 => "INVALID_OBJECT",
        21 => "INVALID_CLASS",
        22 => "CLASS_NOT_PREPARED",
        25 => "INVALID_FIELDID",
        23 => "INVALID_METHOD",
        24 => "INVALID_LOCATION",
        30 => "INVALID_FRAMEID",
        34 => "INVALID_SLOT",
        35 => "DUPLICATE",
        41 => "NOT_FOUND",
        99 => "NOT_IMPLEMENTED",
        100 => "NULL_POINTER",
        101 => "ABSENT_INFORMATION",
        102 => "INVALID_EVENT_TYPE",
        112 => "VM_DEAD",
        _ => "error",
    }
}

/// Sizes of the variable-length ids, from `VirtualMachine.IDSizes`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IdSizes {
    pub field: usize,
    pub method: usize,
    pub object: usize,
    pub reference_type: usize,
    pub frame: usize,
}

impl Default for IdSizes {
    fn default() -> Self {
        Self {
            field: 8,
            method: 8,
            object: 8,
            reference_type: 8,
            frame: 8,
        }
    }
}

/// Builds packet data.
#[derive(Debug, Default, Clone)]
pub struct Out {
    pub buf: Vec<u8>,
}

impl Out {
    pub fn u8(mut self, v: u8) -> Self {
        self.buf.push(v);
        self
    }

    pub fn i32(mut self, v: i32) -> Self {
        self.buf.extend_from_slice(&v.to_be_bytes());
        self
    }

    pub fn u64(mut self, v: u64) -> Self {
        self.buf.extend_from_slice(&v.to_be_bytes());
        self
    }

    pub fn string(mut self, s: &str) -> Self {
        self.buf.extend_from_slice(&(s.len() as i32).to_be_bytes());
        self.buf.extend_from_slice(s.as_bytes());
        self
    }

    /// An id of `size` bytes (big endian).
    pub fn id(mut self, v: u64, size: usize) -> Self {
        self.buf.extend_from_slice(&v.to_be_bytes()[8 - size..]);
        self
    }

    pub fn location(self, loc: &Location, sizes: &IdSizes) -> Self {
        self.u8(loc.type_tag)
            .id(loc.class, sizes.reference_type)
            .id(loc.method, sizes.method)
            .u64(loc.index)
    }
}

/// Parses packet data.
pub struct In<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> In<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self.pos + n;
        let slice = self.data.get(self.pos..end).ok_or_else(|| {
            JdwpError::Protocol(format!(
                "read {n} bytes at {} of {}",
                self.pos,
                self.data.len()
            ))
        })?;
        self.pos = end;
        Ok(slice)
    }

    pub fn remaining(&self) -> usize {
        self.data.len() - self.pos
    }

    pub fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }

    pub fn bool(&mut self) -> Result<bool> {
        Ok(self.u8()? != 0)
    }

    pub fn u16(&mut self) -> Result<u16> {
        Ok(u16::from_be_bytes(
            self.take(2)?.try_into().expect("2 bytes"),
        ))
    }

    pub fn i32(&mut self) -> Result<i32> {
        Ok(i32::from_be_bytes(
            self.take(4)?.try_into().expect("4 bytes"),
        ))
    }

    pub fn i64(&mut self) -> Result<i64> {
        Ok(i64::from_be_bytes(
            self.take(8)?.try_into().expect("8 bytes"),
        ))
    }

    pub fn u64(&mut self) -> Result<u64> {
        Ok(self.i64()? as u64)
    }

    pub fn string(&mut self) -> Result<String> {
        let len = self.i32()?.max(0) as usize;
        Ok(String::from_utf8_lossy(self.take(len)?).into_owned())
    }

    pub fn id(&mut self, size: usize) -> Result<u64> {
        let bytes = self.take(size)?;
        Ok(bytes.iter().fold(0u64, |acc, b| (acc << 8) | u64::from(*b)))
    }

    pub fn location(&mut self, sizes: &IdSizes) -> Result<Location> {
        Ok(Location {
            type_tag: self.u8()?,
            class: self.id(sizes.reference_type)?,
            method: self.id(sizes.method)?,
            index: self.u64()?,
        })
    }

    /// A tagged value (`StackFrame.GetValues` & co).
    pub fn value(&mut self, sizes: &IdSizes) -> Result<Value> {
        let tag = self.u8()?;
        self.untagged_value(tag, sizes)
    }

    pub fn untagged_value(&mut self, tag: u8, sizes: &IdSizes) -> Result<Value> {
        Ok(match tag {
            b'B' => Value::Int(i64::from(self.u8()? as i8)),
            b'C' => Value::Char(char::from_u32(u32::from(self.u16()?)).unwrap_or('?')),
            b'S' => Value::Int(i64::from(self.u16()? as i16)),
            b'I' => Value::Int(i64::from(self.i32()?)),
            b'J' => Value::Int(self.i64()?),
            b'F' => Value::Float(f64::from(f32::from_bits(self.i32()? as u32))),
            b'D' => Value::Float(f64::from_bits(self.u64()?)),
            b'Z' => Value::Bool(self.bool()?),
            b'V' => Value::Void,
            b's' => Value::String(self.id(sizes.object)?),
            b'L' | b'[' | b't' | b'g' | b'l' | b'c' => Value::Object {
                tag,
                id: self.id(sizes.object)?,
            },
            other => return Err(JdwpError::Protocol(format!("unknown value tag {other}"))),
        })
    }
}

/// A code location: class (reference type), method and code index.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Location {
    pub type_tag: u8,
    pub class: u64,
    pub method: u64,
    pub index: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Int(i64),
    Float(f64),
    Bool(bool),
    Char(char),
    Void,
    /// A string object id (resolve with [`Jdwp::string_value`]).
    String(u64),
    /// Any other object (`id == 0` is null).
    Object {
        tag: u8,
        id: u64,
    },
}

#[derive(Debug)]
pub struct Packet {
    pub id: u32,
    pub flags: u8,
    /// Command set and command for commands; zero for replies.
    pub command: (u8, u8),
    pub error: u16,
    pub data: Vec<u8>,
}

impl Packet {
    pub fn encode_command(id: u32, set: u8, cmd: u8, data: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(11 + data.len());
        out.extend_from_slice(&((11 + data.len()) as u32).to_be_bytes());
        out.extend_from_slice(&id.to_be_bytes());
        out.push(0);
        out.push(set);
        out.push(cmd);
        out.extend_from_slice(data);
        out
    }

    pub fn decode(header: &[u8; 11], data: Vec<u8>) -> Self {
        let id = u32::from_be_bytes(header[4..8].try_into().expect("4 bytes"));
        let flags = header[8];
        if flags & REPLY_FLAG != 0 {
            let error = u16::from_be_bytes(header[9..11].try_into().expect("2 bytes"));
            Packet {
                id,
                flags,
                command: (0, 0),
                error,
                data,
            }
        } else {
            Packet {
                id,
                flags,
                command: (header[9], header[10]),
                error: 0,
                data,
            }
        }
    }

    pub fn is_reply(&self) -> bool {
        self.flags & REPLY_FLAG != 0
    }
}

/// An event from an `Event.Composite` packet.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    VmStart {
        thread: u64,
    },
    VmDeath,
    ThreadStart {
        thread: u64,
    },
    ThreadDeath {
        thread: u64,
    },
    ClassPrepare {
        request: i32,
        thread: u64,
        type_tag: u8,
        class: u64,
        signature: String,
        status: i32,
    },
    Breakpoint {
        request: i32,
        thread: u64,
        location: Location,
    },
    SingleStep {
        request: i32,
        thread: u64,
        location: Location,
    },
    MethodEntry {
        request: i32,
        thread: u64,
        location: Location,
    },
    MethodExit {
        request: i32,
        thread: u64,
        location: Location,
    },
    Other {
        kind: u8,
    },
}

pub mod kind {
    pub const SINGLE_STEP: u8 = 1;
    pub const BREAKPOINT: u8 = 2;
    pub const THREAD_START: u8 = 6;
    pub const THREAD_DEATH: u8 = 7;
    pub const CLASS_PREPARE: u8 = 8;
    pub const METHOD_ENTRY: u8 = 40;
    pub const METHOD_EXIT: u8 = 41;
    pub const VM_START: u8 = 90;
    pub const VM_DEATH: u8 = 99;
}

pub mod suspend {
    pub const NONE: u8 = 0;
    pub const THREAD: u8 = 1;
    pub const ALL: u8 = 2;
}

pub mod step {
    pub const LINE: i32 = 1;
    pub const INTO: i32 = 0;
    pub const OVER: i32 = 1;
    pub const OUT: i32 = 2;
}

/// Parses an `Event.Composite` packet: (suspend policy, events).
pub fn parse_events(data: &[u8], sizes: &IdSizes) -> Result<(u8, Vec<Event>)> {
    let mut r = In::new(data);
    let policy = r.u8()?;
    let count = r.i32()?;
    let mut events = Vec::new();
    for _ in 0..count {
        let kind = r.u8()?;
        let event = match kind {
            kind::VM_START => {
                r.i32()?;
                Event::VmStart {
                    thread: r.id(sizes.object)?,
                }
            }
            kind::VM_DEATH => {
                r.i32()?;
                Event::VmDeath
            }
            kind::THREAD_START | kind::THREAD_DEATH => {
                r.i32()?;
                let thread = r.id(sizes.object)?;
                if kind == kind::THREAD_START {
                    Event::ThreadStart { thread }
                } else {
                    Event::ThreadDeath { thread }
                }
            }
            kind::CLASS_PREPARE => Event::ClassPrepare {
                request: r.i32()?,
                thread: r.id(sizes.object)?,
                type_tag: r.u8()?,
                class: r.id(sizes.reference_type)?,
                signature: r.string()?,
                status: r.i32()?,
            },
            kind::BREAKPOINT | kind::SINGLE_STEP | kind::METHOD_ENTRY | kind::METHOD_EXIT => {
                let request = r.i32()?;
                let thread = r.id(sizes.object)?;
                let location = r.location(sizes)?;
                match kind {
                    kind::BREAKPOINT => Event::Breakpoint {
                        request,
                        thread,
                        location,
                    },
                    kind::SINGLE_STEP => Event::SingleStep {
                        request,
                        thread,
                        location,
                    },
                    kind::METHOD_ENTRY => Event::MethodEntry {
                        request,
                        thread,
                        location,
                    },
                    _ => Event::MethodExit {
                        request,
                        thread,
                        location,
                    },
                }
            }
            // Unknown layouts: stop parsing this packet.
            other => {
                events.push(Event::Other { kind: other });
                break;
            }
        };
        events.push(event);
    }
    Ok((policy, events))
}

#[derive(Debug, Clone)]
pub struct MethodInfo {
    pub id: u64,
    pub name: String,
    pub signature: String,
    pub modifiers: i32,
}

#[derive(Debug, Clone)]
pub struct LineTable {
    pub start: i64,
    pub end: i64,
    /// (code index, line number)
    pub lines: Vec<(u64, i32)>,
}

#[derive(Debug, Clone)]
pub struct VariableInfo {
    pub code_index: u64,
    pub name: String,
    pub signature: String,
    pub length: i32,
    pub slot: i32,
}

#[derive(Debug, Clone)]
pub struct FieldInfo {
    pub id: u64,
    pub name: String,
    pub signature: String,
    pub modifiers: i32,
}

/// A JDWP connection to one debuggee.
pub struct Jdwp {
    stream: TcpStream,
    next_id: u32,
    pub sizes: IdSizes,
    /// Raw `Event.Composite` payloads, parsed when consumed (the first one,
    /// VM_START, arrives before the id sizes are known).
    events: VecDeque<Vec<u8>>,
    /// Partially read packet bytes (reads may time out mid-packet).
    pending: Vec<u8>,
}

impl Jdwp {
    /// Performs the handshake on an accepted connection.
    pub fn handshake(mut stream: TcpStream) -> Result<Self> {
        stream.set_nodelay(true).ok();
        stream.set_read_timeout(Some(Duration::from_secs(30)))?;
        stream.write_all(HANDSHAKE)?;
        let mut reply = [0u8; 14];
        stream.read_exact(&mut reply)?;
        if reply != HANDSHAKE {
            return Err(JdwpError::Protocol(format!(
                "bad handshake {:?}",
                String::from_utf8_lossy(&reply)
            )));
        }
        let mut jdwp = Self {
            stream,
            next_id: 1,
            sizes: IdSizes::default(),
            events: VecDeque::new(),
            pending: Vec::new(),
        };
        jdwp.sizes = jdwp.id_sizes()?;
        Ok(jdwp)
    }

    /// Reads one packet, waiting at most `timeout` (`None`: no wait limit).
    fn read_packet(&mut self, timeout: Option<Duration>) -> Result<Option<Packet>> {
        self.stream
            .set_read_timeout(timeout.or(Some(Duration::from_secs(3600))))?;
        loop {
            if self.pending.len() >= 4 {
                let len =
                    u32::from_be_bytes(self.pending[..4].try_into().expect("4 bytes")) as usize;
                if len < 11 {
                    return Err(JdwpError::Protocol(format!("packet length {len}")));
                }
                if self.pending.len() >= len {
                    let rest = self.pending.split_off(len);
                    let packet = std::mem::replace(&mut self.pending, rest);
                    let header: [u8; 11] = packet[..11].try_into().expect("11 bytes");
                    return Ok(Some(Packet::decode(&header, packet[11..].to_vec())));
                }
            }
            let mut buf = [0u8; 8192];
            match self.stream.read(&mut buf) {
                Ok(0) => return Err(JdwpError::Disconnected),
                Ok(n) => self.pending.extend_from_slice(&buf[..n]),
                Err(e)
                    if matches!(
                        e.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                    ) =>
                {
                    if timeout.is_some() {
                        return Ok(None);
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::ConnectionReset => {
                    return Err(JdwpError::Disconnected);
                }
                Err(e) => return Err(e.into()),
            }
        }
    }

    fn queue_event_packet(&mut self, packet: Packet) -> Result<()> {
        if packet.command == (64, 100) {
            self.events.push_back(packet.data);
        } else {
            tracing::debug!(command = ?packet.command, "unexpected command from debuggee");
        }
        Ok(())
    }

    /// Sends a command and waits for its reply data.
    pub fn command(&mut self, set: u8, cmd: u8, data: Out) -> Result<Vec<u8>> {
        let id = self.next_id;
        self.next_id += 1;
        self.stream
            .write_all(&Packet::encode_command(id, set, cmd, &data.buf))?;
        loop {
            let Some(packet) = self.read_packet(Some(Duration::from_secs(60)))? else {
                return Err(JdwpError::Protocol(format!(
                    "no reply to command {set}/{cmd}"
                )));
            };
            if packet.is_reply() && packet.id == id {
                if packet.error != 0 {
                    return Err(JdwpError::Reply { code: packet.error });
                }
                return Ok(packet.data);
            }
            if !packet.is_reply() {
                self.queue_event_packet(packet)?;
            }
        }
    }

    /// Next event batch, waiting up to `timeout`.
    pub fn next_events(&mut self, timeout: Duration) -> Result<Option<(u8, Vec<Event>)>> {
        if self.events.is_empty() {
            while let Some(packet) = self.read_packet(Some(timeout))? {
                if !packet.is_reply() {
                    self.queue_event_packet(packet)?;
                    break;
                }
            }
        }
        match self.events.pop_front() {
            Some(data) => {
                let parsed = parse_events(&data, &self.sizes)?;
                tracing::debug!(?parsed, "jdwp events");
                Ok(Some(parsed))
            }
            None => Ok(None),
        }
    }

    fn out(&self) -> Out {
        Out::default()
    }

    // ----- VirtualMachine (1) -------------------------------------------------

    pub fn version(&mut self) -> Result<String> {
        let data = self.command(1, 1, Out::default())?;
        let mut r = In::new(&data);
        let description = r.string()?;
        let _major = r.i32()?;
        let _minor = r.i32()?;
        let vm_version = r.string()?;
        let vm_name = r.string()?;
        Ok(format!("{description} / {vm_name} {vm_version}"))
    }

    pub fn id_sizes(&mut self) -> Result<IdSizes> {
        let data = self.command(1, 7, Out::default())?;
        let mut r = In::new(&data);
        Ok(IdSizes {
            field: r.i32()? as usize,
            method: r.i32()? as usize,
            object: r.i32()? as usize,
            reference_type: r.i32()? as usize,
            frame: r.i32()? as usize,
        })
    }

    /// All loaded classes: (type tag, id, signature).
    pub fn all_classes(&mut self) -> Result<Vec<(u8, u64, String)>> {
        let data = self.command(1, 3, Out::default())?;
        let mut r = In::new(&data);
        let n = r.i32()?;
        let mut out = Vec::new();
        for _ in 0..n {
            let tag = r.u8()?;
            let id = r.id(self.sizes.reference_type)?;
            let sig = r.string()?;
            let _status = r.i32()?;
            out.push((tag, id, sig));
        }
        Ok(out)
    }

    pub fn classes_by_signature(&mut self, signature: &str) -> Result<Vec<(u8, u64)>> {
        let data = self.command(1, 2, self.out().string(signature))?;
        let mut r = In::new(&data);
        let n = r.i32()?;
        let mut out = Vec::new();
        for _ in 0..n {
            let tag = r.u8()?;
            let id = r.id(self.sizes.reference_type)?;
            let _status = r.i32()?;
            out.push((tag, id));
        }
        Ok(out)
    }

    pub fn all_threads(&mut self) -> Result<Vec<u64>> {
        let data = self.command(1, 4, Out::default())?;
        let mut r = In::new(&data);
        let n = r.i32()?;
        (0..n).map(|_| r.id(self.sizes.object)).collect()
    }

    pub fn suspend_vm(&mut self) -> Result<()> {
        self.command(1, 8, Out::default()).map(drop)
    }

    pub fn resume_vm(&mut self) -> Result<()> {
        self.command(1, 9, Out::default()).map(drop)
    }

    pub fn dispose(&mut self) -> Result<()> {
        self.command(1, 6, Out::default()).map(drop)
    }

    pub fn exit(&mut self, code: i32) -> Result<()> {
        self.command(1, 10, self.out().i32(code)).map(drop)
    }

    // ----- ReferenceType (2) ----------------------------------------------------

    pub fn signature(&mut self, class: u64) -> Result<String> {
        let data = self.command(2, 1, self.out().id(class, self.sizes.reference_type))?;
        In::new(&data).string()
    }

    pub fn fields(&mut self, class: u64) -> Result<Vec<FieldInfo>> {
        let data = self.command(2, 4, self.out().id(class, self.sizes.reference_type))?;
        let mut r = In::new(&data);
        let n = r.i32()?;
        let mut out = Vec::new();
        for _ in 0..n {
            out.push(FieldInfo {
                id: r.id(self.sizes.field)?,
                name: r.string()?,
                signature: r.string()?,
                modifiers: r.i32()?,
            });
        }
        Ok(out)
    }

    pub fn methods(&mut self, class: u64) -> Result<Vec<MethodInfo>> {
        let data = self.command(2, 5, self.out().id(class, self.sizes.reference_type))?;
        let mut r = In::new(&data);
        let n = r.i32()?;
        let mut out = Vec::new();
        for _ in 0..n {
            out.push(MethodInfo {
                id: r.id(self.sizes.method)?,
                name: r.string()?,
                signature: r.string()?,
                modifiers: r.i32()?,
            });
        }
        Ok(out)
    }

    // ----- Method (6) -------------------------------------------------------------

    pub fn line_table(&mut self, class: u64, method: u64) -> Result<LineTable> {
        let data = self.command(
            6,
            1,
            self.out()
                .id(class, self.sizes.reference_type)
                .id(method, self.sizes.method),
        )?;
        let mut r = In::new(&data);
        let start = r.i64()?;
        let end = r.i64()?;
        let n = r.i32()?;
        let mut lines = Vec::new();
        for _ in 0..n {
            lines.push((r.u64()?, r.i32()?));
        }
        Ok(LineTable { start, end, lines })
    }

    pub fn variable_table(&mut self, class: u64, method: u64) -> Result<(i32, Vec<VariableInfo>)> {
        let data = self.command(
            6,
            2,
            self.out()
                .id(class, self.sizes.reference_type)
                .id(method, self.sizes.method),
        )?;
        let mut r = In::new(&data);
        let args = r.i32()?;
        let n = r.i32()?;
        let mut vars = Vec::new();
        for _ in 0..n {
            vars.push(VariableInfo {
                code_index: r.u64()?,
                name: r.string()?,
                signature: r.string()?,
                length: r.i32()?,
                slot: r.i32()?,
            });
        }
        Ok((args, vars))
    }

    // ----- ObjectReference (9) / StringReference (10) ---------------------------

    pub fn object_type(&mut self, object: u64) -> Result<(u8, u64)> {
        let data = self.command(9, 1, self.out().id(object, self.sizes.object))?;
        let mut r = In::new(&data);
        Ok((r.u8()?, r.id(self.sizes.reference_type)?))
    }

    pub fn object_values(&mut self, object: u64, fields: &[u64]) -> Result<Vec<Value>> {
        let mut out = self
            .out()
            .id(object, self.sizes.object)
            .i32(fields.len() as i32);
        for f in fields {
            out = out.id(*f, self.sizes.field);
        }
        let data = self.command(9, 2, out)?;
        let mut r = In::new(&data);
        let n = r.i32()?;
        (0..n).map(|_| r.value(&self.sizes)).collect()
    }

    pub fn string_value(&mut self, object: u64) -> Result<String> {
        let data = self.command(10, 1, self.out().id(object, self.sizes.object))?;
        In::new(&data).string()
    }

    // ----- ThreadReference (11) ---------------------------------------------------

    pub fn thread_name(&mut self, thread: u64) -> Result<String> {
        let data = self.command(11, 1, self.out().id(thread, self.sizes.object))?;
        In::new(&data).string()
    }

    pub fn resume_thread(&mut self, thread: u64) -> Result<()> {
        self.command(11, 3, self.out().id(thread, self.sizes.object))
            .map(drop)
    }

    /// Stack frames, innermost first: (frame id, location).
    pub fn frames(&mut self, thread: u64) -> Result<Vec<(u64, Location)>> {
        let data = self.command(
            11,
            6,
            self.out().id(thread, self.sizes.object).i32(0).i32(-1),
        )?;
        let mut r = In::new(&data);
        let n = r.i32()?;
        let mut out = Vec::new();
        for _ in 0..n {
            let frame = r.id(self.sizes.frame)?;
            let loc = r.location(&self.sizes)?;
            out.push((frame, loc));
        }
        Ok(out)
    }

    // ----- ArrayReference (13) ----------------------------------------------------

    pub fn array_length(&mut self, array: u64) -> Result<i32> {
        let data = self.command(13, 1, self.out().id(array, self.sizes.object))?;
        In::new(&data).i32()
    }

    pub fn array_values(&mut self, array: u64, first: i32, length: i32) -> Result<Vec<Value>> {
        let data = self.command(
            13,
            2,
            self.out()
                .id(array, self.sizes.object)
                .i32(first)
                .i32(length),
        )?;
        let mut r = In::new(&data);
        let tag = r.u8()?;
        let n = r.i32()?;
        let primitive = !matches!(tag, b'L' | b'[' | b's' | b't' | b'g' | b'l' | b'c');
        (0..n)
            .map(|_| {
                if primitive {
                    r.untagged_value(tag, &self.sizes)
                } else {
                    r.value(&self.sizes)
                }
            })
            .collect()
    }

    // ----- EventRequest (15) ------------------------------------------------------

    /// Sets an event request; returns its id. `modifiers` are pre-encoded.
    pub fn set_event(&mut self, kind: u8, suspend: u8, modifiers: Vec<Out>) -> Result<i32> {
        let mut out = self.out().u8(kind).u8(suspend).i32(modifiers.len() as i32);
        for m in modifiers {
            out.buf.extend_from_slice(&m.buf);
        }
        let data = self.command(15, 1, out)?;
        In::new(&data).i32()
    }

    pub fn clear_event(&mut self, kind: u8, request: i32) -> Result<()> {
        self.command(15, 2, self.out().u8(kind).i32(request))
            .map(drop)
    }

    pub fn modifier_class_match(pattern: &str) -> Out {
        Out::default().u8(5).string(pattern)
    }

    pub fn modifier_location(&self, loc: &Location) -> Out {
        Out::default().u8(7).location(loc, &self.sizes)
    }

    pub fn modifier_step(&self, thread: u64, depth: i32) -> Out {
        Out::default()
            .u8(10)
            .id(thread, self.sizes.object)
            .i32(step::LINE)
            .i32(depth)
    }

    pub fn modifier_count(count: i32) -> Out {
        Out::default().u8(1).i32(count)
    }

    // ----- StackFrame (16) --------------------------------------------------------

    /// Values of `slots` (slot, signature tag byte) in a frame.
    pub fn frame_values(
        &mut self,
        thread: u64,
        frame: u64,
        slots: &[(i32, u8)],
    ) -> Result<Vec<Value>> {
        let mut out = self
            .out()
            .id(thread, self.sizes.object)
            .id(frame, self.sizes.frame)
            .i32(slots.len() as i32);
        for (slot, tag) in slots {
            out = out.i32(*slot).u8(*tag);
        }
        let data = self.command(16, 1, out)?;
        let mut r = In::new(&data);
        let n = r.i32()?;
        (0..n).map(|_| r.value(&self.sizes)).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_encoding() {
        let bytes = Packet::encode_command(7, 1, 7, &[]);
        assert_eq!(bytes, [0, 0, 0, 11, 0, 0, 0, 7, 0, 1, 7]);
        let data = Out::default().i32(5).string("ab").id(0x0102, 4).buf;
        assert_eq!(data, [0, 0, 0, 5, 0, 0, 0, 2, b'a', b'b', 0, 0, 1, 2]);
    }

    #[test]
    fn reply_decoding() {
        let header = [0, 0, 0, 13, 0, 0, 0, 9, 0x80, 0, 41];
        let p = Packet::decode(&header, vec![1, 2]);
        assert!(p.is_reply());
        assert_eq!((p.id, p.error), (9, 41));
    }

    #[test]
    fn values_and_ids() {
        let sizes = IdSizes {
            object: 4,
            ..IdSizes::default()
        };
        let data = [
            b'I', 0, 0, 0, 42, b's', 0, 0, 0, 9, b'Z', 1, b'L', 0, 0, 0, 0,
        ];
        let mut r = In::new(&data);
        assert_eq!(r.value(&sizes).unwrap(), Value::Int(42));
        assert_eq!(r.value(&sizes).unwrap(), Value::String(9));
        assert_eq!(r.value(&sizes).unwrap(), Value::Bool(true));
        assert_eq!(r.value(&sizes).unwrap(), Value::Object { tag: b'L', id: 0 });
        assert_eq!(r.remaining(), 0);
    }

    #[test]
    fn breakpoint_event() {
        let sizes = IdSizes {
            object: 4,
            reference_type: 4,
            method: 4,
            ..IdSizes::default()
        };
        let mut data = Out::default()
            .u8(suspend::ALL)
            .i32(1)
            .u8(kind::BREAKPOINT)
            .i32(3)
            .id(5, 4);
        data = data.location(
            &Location {
                type_tag: 1,
                class: 6,
                method: 7,
                index: 8,
            },
            &sizes,
        );
        let (policy, events) = parse_events(&data.buf, &sizes).unwrap();
        assert_eq!(policy, suspend::ALL);
        assert_eq!(
            events,
            [Event::Breakpoint {
                request: 3,
                thread: 5,
                location: Location {
                    type_tag: 1,
                    class: 6,
                    method: 7,
                    index: 8
                }
            }]
        );
    }
}
