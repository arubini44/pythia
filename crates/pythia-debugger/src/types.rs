//! Debugger model: units, breakpoints, locations, frames and variables.

use std::fmt;

use serde::{Deserialize, Serialize};

/// Kind of PL/SQL unit, as Oracle's JDWP agent names its classes:
/// `$Oracle.<Kind>.<OWNER>.<NAME>` (anonymous blocks: `$Oracle.Block.<OWNER>.<hash>`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum UnitType {
    Procedure,
    Function,
    Package,
    PackageBody,
    Trigger,
    Type,
    TypeBody,
    /// Anonymous block.
    Block,
}

impl UnitType {
    pub const ALL: [UnitType; 8] = [
        UnitType::Procedure,
        UnitType::Function,
        UnitType::Package,
        UnitType::PackageBody,
        UnitType::Trigger,
        UnitType::Type,
        UnitType::TypeBody,
        UnitType::Block,
    ];

    /// The class name segment used by Oracle's JDWP agent.
    pub fn class_kind(self) -> &'static str {
        match self {
            UnitType::Procedure => "Procedure",
            UnitType::Function => "Function",
            UnitType::Package => "Package",
            UnitType::PackageBody => "PackageBody",
            UnitType::Trigger => "Trigger",
            UnitType::Type => "Type",
            UnitType::TypeBody => "TypeBody",
            UnitType::Block => "Block",
        }
    }

    /// Dictionary type name (`ALL_OBJECTS.OBJECT_TYPE`).
    pub fn dictionary_name(self) -> &'static str {
        match self {
            UnitType::Procedure => "PROCEDURE",
            UnitType::Function => "FUNCTION",
            UnitType::Package => "PACKAGE",
            UnitType::PackageBody => "PACKAGE BODY",
            UnitType::Trigger => "TRIGGER",
            UnitType::Type => "TYPE",
            UnitType::TypeBody => "TYPE BODY",
            UnitType::Block => "ANONYMOUS BLOCK",
        }
    }

    pub fn from_dictionary(name: &str) -> Option<Self> {
        UnitType::ALL
            .into_iter()
            .find(|t| t.dictionary_name().eq_ignore_ascii_case(name))
    }
}

/// A PL/SQL unit (stored object or anonymous block).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Unit {
    pub owner: String,
    pub name: String,
    pub unit_type: UnitType,
}

impl Unit {
    pub fn new(owner: impl Into<String>, name: impl Into<String>, unit_type: UnitType) -> Self {
        Self {
            owner: owner.into(),
            name: name.into(),
            unit_type,
        }
    }

    /// JDWP class signature, e.g. `L$Oracle/Procedure/HR/RAISE_SALARY;`.
    pub fn class_signature(&self) -> String {
        format!(
            "L$Oracle/{}/{}/{};",
            self.unit_type.class_kind(),
            self.owner,
            self.name
        )
    }

    /// Parses a JDWP class signature back into a unit.
    pub fn from_class_signature(signature: &str) -> Option<Self> {
        let inner = signature.strip_prefix("L$Oracle/")?.strip_suffix(';')?;
        let mut parts = inner.splitn(3, '/');
        let kind = parts.next()?;
        let owner = parts.next()?;
        let name = parts.next()?;
        let unit_type = UnitType::ALL.into_iter().find(|t| t.class_kind() == kind)?;
        Some(Unit::new(owner, name, unit_type))
    }
}

impl fmt::Display for Unit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.unit_type {
            UnitType::Block => write!(f, "anonymous block"),
            t => write!(
                f,
                "{} {}.{}",
                t.dictionary_name().to_lowercase(),
                self.owner,
                self.name
            ),
        }
    }
}

/// A line breakpoint in a unit (1-based source line).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Breakpoint {
    pub unit: Unit,
    pub line: u32,
}

/// When a breakpoint actually stops execution.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct BreakpointOptions {
    /// Stop only while this holds, e.g. `l_total > 3 and l_name is not null`.
    pub condition: Option<String>,
    /// Stop from this hit on (hits are counted while the condition holds).
    pub hit_count: Option<u32>,
}

impl BreakpointOptions {
    pub fn is_empty(&self) -> bool {
        self.condition.is_none() && self.hit_count.is_none()
    }
}

/// A watched expression and its value at the current stop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Watch {
    pub expression: String,
    pub value: Result<Variable, String>,
}

/// Where execution stopped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Location {
    pub unit: Unit,
    /// Source line, if the unit has line information.
    pub line: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub location: Location,
    /// Subprogram name (`<procedure$1>` for blocks).
    pub method: String,
}

/// A variable with its value as text (`None` = NULL) and, for records and
/// collections, its fields/elements.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Variable {
    pub name: String,
    pub type_name: String,
    pub value: Option<String>,
    pub children: Vec<Variable>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopReason {
    Breakpoint,
    Step,
    Pause,
    Exception,
    /// Entered the subprogram of [`DebugCommand::BreakOnEntry`].
    Entry,
    /// Reached the line of [`DebugCommand::RunTo`].
    Cursor,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DebugCommand {
    SetBreakpoint(Breakpoint),
    ClearBreakpoint(Breakpoint),
    /// Condition and hit count of a breakpoint (evaluated by the debugger).
    SetBreakpointOptions(Breakpoint, BreakpointOptions),
    /// Expressions evaluated at every stop (and right away when stopped):
    /// variables, `record.field`, `collection(n)`, `package.variable`.
    /// They are evaluated in the selected frame (see [`DebugCommand::SelectFrame`]).
    SetWatches(Vec<String>),
    /// While stopped, selects a frame of the call stack (0 = innermost):
    /// reports its variables and evaluates the watches there, with its
    /// unit's globals, until the next stop selects the innermost frame with
    /// source again. Breakpoint conditions are always evaluated in the
    /// breakpoint's frame.
    SelectFrame(usize),
    /// Stop when an exception is raised.
    BreakOnException(bool),
    /// Stop once, when execution enters a subprogram of a unit (any
    /// overload), at its first line with code. A package's subprograms are
    /// in its body; a standalone procedure or function is its own
    /// subprogram. Replaces an entry not reached yet.
    BreakOnEntry {
        unit: Unit,
        subprogram: String,
    },
    /// While stopped, changes a scalar of the selected frame: a variable,
    /// `record.field`, `collection(n)` or `package.variable`, as watches
    /// name them; `None` sets NULL. Dates, timestamps and intervals are
    /// written as shown (the session's NLS format). Answered by
    /// [`DebugEvent::ValueSet`], and when the value changed by
    /// [`DebugEvent::FrameSelected`] with the frame read again.
    SetValue {
        expression: String,
        value: Option<String>,
    },
    Resume,
    /// While stopped, continues to a line (the first with code at or after
    /// it) and stops there once, even if the unit isn't loaded yet. Any
    /// stop before (a breakpoint, an exception, Pause) drops it.
    RunTo(Breakpoint),
    StepOver,
    StepInto,
    StepOut,
    Pause,
    /// Detach: the target session runs on without the debugger.
    Detach,
    /// Abort the target session's call (ORA-30687 in the target).
    Terminate,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DebugEvent {
    /// Listening for the target session to connect.
    Listening {
        port: u16,
    },
    /// The target session connected.
    Connected {
        version: String,
    },
    /// A breakpoint was set (`line` may move to the next line with code)
    /// or could not be set.
    BreakpointBound {
        breakpoint: Breakpoint,
        line: Option<u32>,
        error: Option<String>,
    },
    Suspended {
        reason: StopReason,
        location: Location,
        stack: Vec<Frame>,
        /// The frame `variables` and `watches` belong to: the innermost one
        /// with source (code without debug information has neither lines
        /// nor variables).
        frame: usize,
        variables: Vec<Variable>,
        watches: Vec<Watch>,
        /// The raised exception, for [`StopReason::Exception`].
        exception: Option<String>,
    },
    /// Watches were changed while stopped: their values.
    Watches(Vec<Watch>),
    /// Answers [`DebugCommand::SetValue`]: `error` says why the value
    /// stayed as it was.
    ValueSet {
        expression: String,
        error: Option<String>,
    },
    /// Another frame was selected ([`DebugCommand::SelectFrame`]): its
    /// variables and the watches evaluated there.
    FrameSelected {
        frame: usize,
        variables: Vec<Variable>,
        watches: Vec<Watch>,
    },
    /// Units without debug information that were loaded while stepping
    /// into: the step went over their code, as it can't stop there. Units
    /// are loaded on first use in the session, so later calls go unnoticed.
    SteppedOver(Vec<Unit>),
    /// A breakpoint was reached with its condition true; `hits` counts
    /// these since the session started.
    BreakpointHit {
        breakpoint: Breakpoint,
        hits: u32,
    },
    Resumed,
    /// The target finished, detached or was terminated.
    Terminated,
    Error(String),
}
