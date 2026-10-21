//! The debugger side: listens for the target session, then drives it over
//! JDWP on a worker thread. The UI talks to it through channels and never
//! blocks.

use std::collections::{BTreeSet, HashMap};
use std::io;
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread;
use std::time::Duration;

use tracing::{debug, info, warn};

use crate::jdwp::{self, Event, Jdwp, JdwpError, Value};
use crate::types::{
    Breakpoint, DebugCommand, DebugEvent, Frame, Location, StopReason, Unit, Variable,
};

/// Handle to a debugger worker.
pub struct Debugger {
    commands: Sender<DebugCommand>,
    events: Receiver<DebugEvent>,
    port: u16,
}

impl Debugger {
    /// Binds a listener (e.g. `0.0.0.0:0` for any free port) and starts the
    /// worker. `notify` is called whenever an event is available.
    pub fn listen(bind: &str, notify: impl Fn() + Send + 'static) -> io::Result<Self> {
        let listener = TcpListener::bind(bind)?;
        listener.set_nonblocking(true)?;
        let port = listener.local_addr()?.port();
        let (cmd_tx, cmd_rx) = mpsc::channel();
        let (ev_tx, ev_rx) = mpsc::channel();
        let events = Events {
            tx: ev_tx,
            notify: Box::new(notify),
        };
        thread::Builder::new()
            .name(format!("debugger-{port}"))
            .spawn(move || run(listener, cmd_rx, events))
            .expect("spawning debugger thread");
        Ok(Self {
            commands: cmd_tx,
            events: ev_rx,
            port,
        })
    }

    /// Port the target session must connect to.
    pub fn port(&self) -> u16 {
        self.port
    }

    pub fn send(&self, command: DebugCommand) {
        let _ = self.commands.send(command);
    }

    pub fn try_recv(&self) -> Option<DebugEvent> {
        self.events.try_recv().ok()
    }
}

impl Drop for Debugger {
    fn drop(&mut self) {
        let _ = self.commands.send(DebugCommand::Detach);
    }
}

struct Events {
    tx: Sender<DebugEvent>,
    notify: Box<dyn Fn() + Send>,
}

impl Events {
    fn send(&self, event: DebugEvent) {
        if self.tx.send(event).is_ok() {
            (self.notify)();
        }
    }
}

fn run(listener: TcpListener, commands: Receiver<DebugCommand>, events: Events) {
    let port = listener.local_addr().map(|a| a.port()).unwrap_or_default();
    events.send(DebugEvent::Listening { port });
    let mut early: Vec<DebugCommand> = Vec::new();
    // Wait for the target, collecting breakpoints set meanwhile.
    let stream = loop {
        match listener.accept() {
            Ok((stream, addr)) => {
                info!(%addr, "debug target connected");
                break stream;
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
            Err(e) => {
                events.send(DebugEvent::Error(format!("accepting debug target: {e}")));
                return;
            }
        }
        match commands.recv_timeout(Duration::from_millis(50)) {
            Ok(DebugCommand::Detach | DebugCommand::Terminate)
            | Err(mpsc::RecvTimeoutError::Disconnected) => {
                events.send(DebugEvent::Terminated);
                return;
            }
            Ok(cmd) => early.push(cmd),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
    };
    let result = stream
        .set_nonblocking(false)
        .map_err(JdwpError::from)
        .and_then(|()| Session::start(stream, &events));
    let mut session = match result {
        Ok(s) => s,
        Err(e) => {
            events.send(DebugEvent::Error(format!("debug handshake failed: {e}")));
            events.send(DebugEvent::Terminated);
            return;
        }
    };
    for cmd in early {
        session.command(cmd, &events);
    }
    if let Err(e) = session.event_loop(&commands, &events)
        && !matches!(e, JdwpError::Disconnected)
    {
        events.send(DebugEvent::Error(e.to_string()));
    }
    events.send(DebugEvent::Terminated);
}

/// JDWP state of one debug session.
struct Session {
    jdwp: Jdwp,
    /// Requested breakpoints.
    breakpoints: BTreeSet<Breakpoint>,
    /// Bound breakpoints → JDWP request ids.
    bound: HashMap<Breakpoint, i32>,
    /// Loaded classes by unit.
    classes: HashMap<Unit, u64>,
    /// Thread that is currently suspended at a breakpoint/step.
    stopped: Option<u64>,
    /// Classes whose line tables were cached: (class, method) → lines.
    lines: HashMap<(u64, u64), Vec<(u64, i32)>>,
    method_names: HashMap<(u64, u64), String>,
}

#[derive(Debug, PartialEq, Eq)]
enum Flow {
    Continue,
    Stop,
}

impl Session {
    fn start(stream: TcpStream, events: &Events) -> Result<Self, JdwpError> {
        let mut jdwp = Jdwp::handshake(stream)?;
        let version = jdwp.version().unwrap_or_else(|e| format!("unknown ({e})"));
        events.send(DebugEvent::Connected { version });
        let mut session = Session {
            jdwp,
            breakpoints: BTreeSet::new(),
            bound: HashMap::new(),
            classes: HashMap::new(),
            stopped: None,
            lines: HashMap::new(),
            method_names: HashMap::new(),
        };
        for (_, id, signature) in session.jdwp.all_classes()? {
            if let Some(unit) = Unit::from_class_signature(&signature) {
                session.classes.insert(unit, id);
            }
        }
        // Every PL/SQL unit loaded later is announced, so deferred
        // breakpoints can be bound.
        session
            .jdwp
            .set_event(jdwp::kind::CLASS_PREPARE, jdwp::suspend::ALL, vec![])?;
        Ok(session)
    }

    /// Handles a UI command. Returns whether the target should be resumed.
    fn command(&mut self, command: DebugCommand, events: &Events) -> bool {
        let result: Result<bool, JdwpError> = (|| {
            Ok(match command {
                DebugCommand::SetBreakpoint(bp) => {
                    self.breakpoints.insert(bp.clone());
                    if let Some(class) = self.classes.get(&bp.unit).copied() {
                        self.bind(&bp, class, events)?;
                    }
                    false
                }
                DebugCommand::ClearBreakpoint(bp) => {
                    self.breakpoints.remove(&bp);
                    if let Some(request) = self.bound.remove(&bp) {
                        self.jdwp.clear_event(jdwp::kind::BREAKPOINT, request)?;
                    }
                    false
                }
                DebugCommand::Resume => self.stopped.is_some(),
                DebugCommand::StepOver | DebugCommand::StepInto | DebugCommand::StepOut => {
                    let Some(thread) = self.stopped else {
                        return Ok(false);
                    };
                    let depth = match command {
                        DebugCommand::StepOver => jdwp::step::OVER,
                        DebugCommand::StepInto => jdwp::step::INTO,
                        _ => jdwp::step::OUT,
                    };
                    let step = self.jdwp.modifier_step(thread, depth);
                    self.jdwp.set_event(
                        jdwp::kind::SINGLE_STEP,
                        jdwp::suspend::ALL,
                        vec![step, Jdwp::modifier_count(1)],
                    )?;
                    true
                }
                DebugCommand::Pause => {
                    if self.stopped.is_none() {
                        self.jdwp.suspend_vm()?;
                        if let Some(thread) = self.jdwp.all_threads()?.first().copied() {
                            self.suspended(thread, StopReason::Pause, events)?;
                        }
                    }
                    false
                }
                DebugCommand::Detach | DebugCommand::Terminate => false,
            })
        })();
        match result {
            Ok(resume) => resume,
            Err(e) => {
                events.send(DebugEvent::Error(e.to_string()));
                false
            }
        }
    }

    fn resume(&mut self, events: &Events) -> Result<(), JdwpError> {
        self.stopped = None;
        self.jdwp.resume_vm()?;
        events.send(DebugEvent::Resumed);
        Ok(())
    }

    fn event_loop(
        &mut self,
        commands: &Receiver<DebugCommand>,
        events: &Events,
    ) -> Result<(), JdwpError> {
        // The target is suspended after VM_START; the queued VM_START event
        // batch below resumes it like any other non-stopping event.
        loop {
            loop {
                match commands.try_recv() {
                    Ok(DebugCommand::Detach) => {
                        let _ = self.jdwp.dispose();
                        return Ok(());
                    }
                    Ok(DebugCommand::Terminate) => {
                        let _ = self.jdwp.exit(1);
                        return Ok(());
                    }
                    Ok(cmd) => {
                        if self.command(cmd, events) {
                            self.resume(events)?;
                        }
                    }
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        let _ = self.jdwp.dispose();
                        return Ok(());
                    }
                }
            }
            let Some((policy, batch)) = self.jdwp.next_events(Duration::from_millis(50))? else {
                continue;
            };
            let mut flow = Flow::Continue;
            // A breakpoint and a step can report the same stop; handle it once.
            let mut stopped_here = false;
            for event in batch {
                debug!(?event, "debug event");
                match event {
                    Event::VmDeath => return Ok(()),
                    Event::ClassPrepare {
                        class, signature, ..
                    } => {
                        if let Some(unit) = Unit::from_class_signature(&signature) {
                            self.classes.insert(unit.clone(), class);
                            let pending: Vec<Breakpoint> = self
                                .breakpoints
                                .iter()
                                .filter(|b| b.unit == unit)
                                .cloned()
                                .collect();
                            for bp in pending {
                                self.bind(&bp, class, events)?;
                            }
                        }
                    }
                    Event::Breakpoint { thread, .. } | Event::SingleStep { thread, .. }
                        if !stopped_here =>
                    {
                        let reason = if matches!(event, Event::Breakpoint { .. }) {
                            StopReason::Breakpoint
                        } else {
                            StopReason::Step
                        };
                        stopped_here = true;
                        self.suspended(thread, reason, events)?;
                        flow = Flow::Stop;
                    }
                    _ => {}
                }
            }
            if flow == Flow::Continue && policy != jdwp::suspend::NONE {
                self.jdwp.resume_vm()?;
            }
        }
    }

    /// Sets a JDWP breakpoint for `bp` in a loaded class.
    fn bind(&mut self, bp: &Breakpoint, class: u64, events: &Events) -> Result<(), JdwpError> {
        if let Some(old) = self.bound.remove(bp) {
            let _ = self.jdwp.clear_event(jdwp::kind::BREAKPOINT, old);
        }
        // The first line at or after the requested one that has code.
        let mut best: Option<(i32, u64, u64)> = None;
        for method in self.jdwp.methods(class)? {
            for (index, line) in self.line_table(class, method.id)? {
                if line >= bp.line as i32 && best.is_none_or(|(l, ..)| line < l) {
                    best = Some((line, method.id, index));
                }
            }
        }
        let Some((line, method, index)) = best else {
            events.send(DebugEvent::BreakpointBound {
                breakpoint: bp.clone(),
                line: None,
                error: Some("no code at or after this line (compiled with debug info?)".into()),
            });
            return Ok(());
        };
        let location = jdwp::Location {
            type_tag: 1,
            class,
            method,
            index,
        };
        let modifier = self.jdwp.modifier_location(&location);
        let request =
            self.jdwp
                .set_event(jdwp::kind::BREAKPOINT, jdwp::suspend::ALL, vec![modifier])?;
        self.bound.insert(bp.clone(), request);
        events.send(DebugEvent::BreakpointBound {
            breakpoint: bp.clone(),
            line: Some(line as u32),
            error: None,
        });
        Ok(())
    }

    fn line_table(&mut self, class: u64, method: u64) -> Result<Vec<(u64, i32)>, JdwpError> {
        if let Some(lines) = self.lines.get(&(class, method)) {
            return Ok(lines.clone());
        }
        let lines = match self.jdwp.line_table(class, method) {
            Ok(t) => t.lines,
            Err(JdwpError::Reply { .. }) => Vec::new(),
            Err(e) => return Err(e),
        };
        self.lines.insert((class, method), lines.clone());
        Ok(lines)
    }

    fn line_of(&mut self, loc: &jdwp::Location) -> Result<Option<u32>, JdwpError> {
        let lines = self.line_table(loc.class, loc.method)?;
        Ok(lines
            .iter()
            .filter(|(i, _)| *i <= loc.index)
            .max_by_key(|(i, _)| *i)
            .map(|(_, l)| *l as u32))
    }

    fn method_name(&mut self, class: u64, method: u64) -> Result<String, JdwpError> {
        if let Some(n) = self.method_names.get(&(class, method)) {
            return Ok(n.clone());
        }
        for m in self.jdwp.methods(class)? {
            self.method_names.insert((class, m.id), m.name);
        }
        Ok(self
            .method_names
            .get(&(class, method))
            .cloned()
            .unwrap_or_default())
    }

    fn unit_of(&mut self, class: u64) -> Result<Unit, JdwpError> {
        if let Some((unit, _)) = self.classes.iter().find(|(_, c)| **c == class) {
            return Ok(unit.clone());
        }
        let signature = self.jdwp.signature(class)?;
        let unit = Unit::from_class_signature(&signature).unwrap_or_else(|| {
            Unit::new(
                "",
                signature.trim_start_matches('L').trim_end_matches(';'),
                crate::types::UnitType::Block,
            )
        });
        self.classes.insert(unit.clone(), class);
        Ok(unit)
    }

    /// Reports a stop with stack and the top frame's variables.
    fn suspended(
        &mut self,
        thread: u64,
        reason: StopReason,
        events: &Events,
    ) -> Result<(), JdwpError> {
        self.stopped = Some(thread);
        let frames = self.jdwp.frames(thread)?;
        let mut stack = Vec::new();
        for (_, loc) in &frames {
            let unit = self.unit_of(loc.class)?;
            let line = self.line_of(loc)?;
            let method = self.method_name(loc.class, loc.method)?;
            stack.push(Frame {
                location: Location { unit, line },
                method,
            });
        }
        let variables = match frames.first() {
            Some((frame, loc)) => self.frame_variables(thread, *frame, loc)?,
            None => Vec::new(),
        };
        let location = stack
            .first()
            .map(|f| f.location.clone())
            .unwrap_or(Location {
                unit: Unit::new("", "?", crate::types::UnitType::Block),
                line: None,
            });
        events.send(DebugEvent::Suspended {
            reason,
            location,
            stack,
            variables,
        });
        Ok(())
    }

    fn frame_variables(
        &mut self,
        thread: u64,
        frame: u64,
        loc: &jdwp::Location,
    ) -> Result<Vec<Variable>, JdwpError> {
        let vars = match self.jdwp.variable_table(loc.class, loc.method) {
            Ok((_, vars)) => vars,
            Err(JdwpError::Reply { .. }) => return Ok(Vec::new()),
            Err(e) => return Err(e),
        };
        // Only variables in scope at the current code index.
        let vars: Vec<_> = vars
            .into_iter()
            .filter(|v| {
                v.code_index <= loc.index && loc.index < v.code_index + v.length.max(0) as u64
            })
            .collect();
        if vars.is_empty() {
            return Ok(Vec::new());
        }
        let slots: Vec<(i32, u8)> = vars
            .iter()
            .map(|v| {
                (
                    v.slot,
                    v.signature.as_bytes().first().copied().unwrap_or(b'L'),
                )
            })
            .collect();
        let values = match self.jdwp.frame_values(thread, frame, &slots) {
            Ok(v) => v,
            Err(JdwpError::Reply { code }) => {
                warn!("reading frame values failed: {}", jdwp::error_name(code));
                return Ok(Vec::new());
            }
            Err(e) => return Err(e),
        };
        let mut out = Vec::new();
        for (var, value) in vars.iter().zip(values) {
            out.push(self.variable(var.name.clone(), &var.signature, &value, 0)?);
        }
        Ok(out)
    }

    /// Converts a JDWP value into a variable; PL/SQL scalars are objects of
    /// `$Oracle/Builtin/<TYPE>` with a string `_value` field.
    fn variable(
        &mut self,
        name: String,
        signature: &str,
        value: &Value,
        depth: usize,
    ) -> Result<Variable, JdwpError> {
        let mut var = Variable {
            name,
            type_name: type_name(signature),
            value: None,
            children: Vec::new(),
        };
        match value {
            Value::Object { id: 0, .. } | Value::String(0) => {}
            Value::String(id) => var.value = Some(self.jdwp.string_value(*id)?),
            Value::Int(i) => var.value = Some(i.to_string()),
            Value::Float(f) => var.value = Some(f.to_string()),
            Value::Bool(b) => var.value = Some(b.to_string()),
            Value::Char(c) => var.value = Some(c.to_string()),
            Value::Void => {}
            Value::Object { tag: b'[', id } if depth < MAX_DEPTH => {
                let len = self.jdwp.array_length(*id)?;
                let values = self.jdwp.array_values(*id, 0, len.min(MAX_ELEMENTS))?;
                for (i, v) in values.iter().enumerate() {
                    let mut element = self.variable(format!("({})", i + 1), "", v, depth + 1)?;
                    // Elements are wrappers; show their value directly.
                    if element.children.len() == 1 && element.value.is_none() {
                        let inner = element.children.remove(0);
                        element.value = inner.value;
                        element.type_name = inner.type_name;
                        element.children = inner.children;
                    }
                    var.children.push(element);
                }
                var.value = Some(if len > MAX_ELEMENTS {
                    format!("{len} elements (first {MAX_ELEMENTS} shown)")
                } else {
                    format!("{len} element(s)")
                });
            }
            Value::Object { id, .. } if depth < MAX_DEPTH => {
                let (_, class) = self.jdwp.object_type(*id)?;
                if var.type_name.is_empty() {
                    var.type_name = type_name(&self.jdwp.signature(class)?);
                }
                let fields = self.jdwp.fields(class)?;
                let ids: Vec<u64> = fields.iter().map(|f| f.id).collect();
                let values: Vec<Option<Value>> = match self.jdwp.object_values(*id, &ids) {
                    _ if ids.is_empty() => Vec::new(),
                    Ok(values) => values.into_iter().map(Some).collect(),
                    // Some fields (e.g. statics) can't be read through the
                    // object; read one by one and skip those.
                    Err(JdwpError::Reply { .. }) => ids
                        .iter()
                        .map(|f| {
                            self.jdwp
                                .object_values(*id, &[*f])
                                .ok()
                                .and_then(|mut v| v.pop())
                        })
                        .collect(),
                    Err(e) => return Err(e),
                };
                for (field, v) in fields.iter().zip(values) {
                    let Some(v) = v else { continue };
                    if field.name == "_values" {
                        // Collections: `_values` is an array of elements.
                        let collection =
                            self.variable(String::new(), &field.signature, &v, depth)?;
                        var.children = collection.children;
                        var.value = collection.value;
                    } else if field.name == "_value" {
                        var.value = match v {
                            Value::String(0) | Value::Object { id: 0, .. } => None,
                            Value::String(sid) => Some(self.jdwp.string_value(sid)?),
                            other => {
                                self.variable(String::new(), &field.signature, &other, depth + 1)?
                                    .value
                            }
                        };
                    } else if !field.name.starts_with('_') {
                        var.children.push(self.variable(
                            field.name.clone(),
                            &field.signature,
                            &v,
                            depth + 1,
                        )?);
                    }
                }
            }
            Value::Object { .. } => var.value = Some("…".into()),
        }
        Ok(var)
    }
}

/// Nesting limit for records/collections (each PL/SQL level is ~2 objects).
const MAX_DEPTH: usize = 8;
/// Collection elements read per variable.
const MAX_ELEMENTS: i32 = 200;

/// `L$Oracle/Builtin/VARCHAR2;` → `VARCHAR2`; user types keep owner.name;
/// types declared inside a unit (`…/DBG_DEMO/DBG_DEMO/T_ARR`) keep their
/// own name.
fn type_name(signature: &str) -> String {
    let inner = signature.trim_start_matches('L').trim_end_matches(';');
    if let Some(builtin) = inner.strip_prefix("$Oracle/Builtin/") {
        return builtin.to_owned();
    }
    if let Some(unit) = Unit::from_class_signature(signature) {
        if unit.name.contains('/') {
            return unit.name.rsplit('/').next().unwrap_or_default().to_owned();
        }
        return format!("{}.{}", unit.owner, unit.name);
    }
    inner.rsplit('/').next().unwrap_or(inner).to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn type_names() {
        assert_eq!(type_name("L$Oracle/Builtin/VARCHAR2;"), "VARCHAR2");
        assert_eq!(type_name("L$Oracle/Type/HR/T_REC;"), "HR.T_REC");
        assert_eq!(
            type_name("L$Oracle/Procedure/ORUI/DBG_DEMO/DBG_DEMO/T_ARR;"),
            "T_ARR"
        );
        assert_eq!(type_name(""), "");
    }
}
