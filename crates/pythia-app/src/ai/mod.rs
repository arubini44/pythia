//! The AI assistant: a conversation in a side panel, started from the panel,
//! editor commands and the places slow SQL shows up.
//!
//! A request runs in two steps: dictionary lookups on the connection's main
//! session (the tables a statement uses, tables matching a description,
//! plans), then the model call on a worker thread. What is sent is built by
//! [`pythia_ai::prompt`], which applies the data policy from Settings → AI.

pub mod context;
pub mod lookup;
mod panel;

use std::ops::Range;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use pythia_ai::prompt::Rendered;
use pythia_ai::{ChatRequest, Part, Policy, Task, Turn};
use pythia_config::{AiBackend, AiSettings};

pub use lookup::Lookup;
pub use panel::{PanelAction, PanelContext};

use crate::documents::DocId;

/// A request for the assistant from a document, e.g. slow SQL in the monitor.
#[derive(Debug, Clone)]
pub struct AiRequest {
    pub task: Task,
    /// Context the document has at hand.
    pub parts: Vec<Part>,
    /// Dictionary lookups to add before asking.
    pub lookup: Lookup,
}

/// The editor a request came from, where code from the answer can go.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Origin {
    pub doc: DocId,
    /// Editor part of a program window (0 in SQL windows).
    pub part: usize,
    /// The statement or selection asked about, and its text at the time;
    /// "Replace" swaps it if it is unchanged.
    pub target: Option<(Range<usize>, String)>,
}

/// Answers requests instead of a backend (UI tests).
pub type Responder = Arc<dyn Fn(&ChatRequest) -> Result<String, String> + Send + Sync>;

/// A model call running on a worker thread.
pub struct Job {
    rx: mpsc::Receiver<Result<String, String>>,
    cancel: Arc<AtomicBool>,
    pub started: Instant,
}

impl Job {
    /// The answer, once it is in.
    pub fn poll(&self) -> Option<Result<String, String>> {
        match self.rx.try_recv() {
            Ok(result) => Some(result),
            Err(mpsc::TryRecvError::Empty) => None,
            Err(mpsc::TryRecvError::Disconnected) => {
                Some(Err("The request ended unexpectedly".into()))
            }
        }
    }

    /// Stops a command line tool; HTTP requests are left to finish and
    /// their answer is dropped.
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

/// Sends `request` to `backend` on a worker thread (reading its API key
/// from the keychain there); `repaint` is called when the answer is in.
pub fn spawn(
    backend: AiBackend,
    request: ChatRequest,
    timeout: Duration,
    repaint: impl Fn() + Send + 'static,
    responder: Option<Responder>,
) -> Job {
    let (tx, rx) = mpsc::channel();
    let cancel = Arc::new(AtomicBool::new(false));
    let stop = cancel.clone();
    std::thread::spawn(move || {
        let result = match responder {
            Some(answer) => answer(&request),
            None => {
                let key = if backend.kind.uses_key() {
                    crate::secrets::load_api_key(&backend.id)
                } else {
                    None
                };
                pythia_ai::complete(&backend, key.as_deref(), &request, timeout, &stop)
                    .map_err(|e| e.to_string())
            }
        };
        let _ = tx.send(result);
        repaint();
    });
    Job {
        rx,
        cancel,
        started: Instant::now(),
    }
}

/// A button asking the assistant; shown only when a model is set up.
pub fn button(ui: &mut egui::Ui, ai: &AiSettings, label: &str, hint: &str) -> bool {
    !ai.backends.is_empty()
        && pythia_ui::icons::icon_text_button(ui, pythia_ui::icons::Icon::Sparkle, label)
            .on_hover_text(hint)
            .clicked()
}

/// Code from an answer, fitted to replace `original`: statement ranges end
/// before their terminator (`;`, `/`), which stays in the editor.
pub fn fit_terminator(original: &str, code: &str) -> String {
    let original = original.trim_end();
    let mut code = code.trim();
    if !original.ends_with('/')
        && let Some(rest) = code.strip_suffix('/').filter(|r| r.ends_with('\n'))
    {
        code = rest.trim_end();
    }
    let ends_in_sql = pythia_plsql::split_statements(code)
        .last()
        .is_some_and(|s| s.kind == pythia_plsql::StatementKind::Sql);
    if ends_in_sql && !original.ends_with(';') {
        code = code.strip_suffix(';').unwrap_or(code).trim_end();
    }
    code.to_owned()
}

/// One request in the conversation.
struct Entry {
    turn: Turn,
    origin: Option<Origin>,
    /// Where the context came from, e.g. "SQL 1 · statement at line 3".
    source: String,
    /// Name of the backend asked.
    backend: String,
    status: Status,
    /// The message as sent, with what the policy kept back.
    sent: Option<Rendered>,
}

#[derive(Debug, Clone, PartialEq)]
enum Status {
    LookingUp,
    Asking,
    Done(Duration),
    Failed(String),
    Stopped,
}

enum Pending {
    Lookup { id: u64, entry: usize },
    Answer { job: Job, entry: usize },
}

/// What the panel's input does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    #[default]
    Ask,
    Generate,
}

/// The conversation and the request in flight.
#[derive(Default)]
pub struct Assistant {
    entries: Vec<Entry>,
    pending: Option<Pending>,
    pub input: String,
    pub mode: Mode,
    /// Backend picked in the panel (None: the default one).
    backend: Option<String>,
    focus_input: bool,
    /// The question input, once drawn.
    input_id: Option<egui::Id>,
    /// A note for the user, e.g. why code couldn't be replaced.
    notice: Option<String>,
    /// Answers instead of the backends (UI tests).
    pub responder: Option<Responder>,
}

impl Assistant {
    pub fn is_busy(&self) -> bool {
        self.pending.is_some()
    }

    /// The backend requests go to.
    pub fn backend<'a>(&self, ai: &'a AiSettings) -> Option<&'a AiBackend> {
        self.backend
            .as_deref()
            .and_then(|id| ai.backend(id))
            .or_else(|| ai.default_backend())
    }

    /// Puts the keyboard focus into the input, in `mode`.
    pub fn focus(&mut self, mode: Mode) {
        self.mode = mode;
        self.focus_input = true;
    }

    /// The user types a question: keys are for the input, not shortcuts
    /// (Ctrl+Enter sends instead of executing the statement).
    pub fn typing(&self, ctx: &egui::Context) -> bool {
        self.input_id
            .is_some_and(|id| ctx.memory(|m| m.has_focus(id)))
    }

    pub fn notice(&mut self, text: impl Into<String>) {
        self.notice = Some(text.into());
    }

    /// Starts a request. Parts sent earlier in the conversation aren't sent
    /// again. Returns its index for [`Self::look_up`] or [`Self::ask`].
    pub fn add(
        &mut self,
        task: Task,
        mut parts: Vec<Part>,
        origin: Option<Origin>,
        source: String,
        backend: &AiBackend,
    ) -> usize {
        parts.retain(|p| {
            !self
                .entries
                .iter()
                .filter(|e| e.turn.answer.is_some())
                .any(|e| e.turn.parts.contains(p))
        });
        self.notice = None;
        self.entries.push(Entry {
            turn: Turn::new(task, parts),
            origin,
            source,
            backend: backend.name.clone(),
            status: Status::Asking,
            sent: None,
        });
        self.entries.len() - 1
    }

    /// The request waits for dictionary lookup `id`.
    pub fn look_up(&mut self, entry: usize, id: u64) {
        self.entries[entry].status = Status::LookingUp;
        self.pending = Some(Pending::Lookup { id, entry });
    }

    pub fn is_lookup(&self, id: u64) -> bool {
        matches!(self.pending, Some(Pending::Lookup { id: pending, .. }) if pending == id)
    }

    /// Adds what the lookup found and asks the model.
    pub fn on_lookup(
        &mut self,
        result: Result<Vec<Part>, String>,
        ai: &AiSettings,
        repaint: impl Fn() + Send + 'static,
    ) {
        let Some(Pending::Lookup { entry, .. }) = self.pending.take() else {
            return;
        };
        let parts = match result {
            Ok(parts) => parts,
            Err(e) => vec![Part::text(
                "Dictionary lookup",
                format!("Looking up tables and plans failed: {e}"),
            )],
        };
        let turn = &mut self.entries[entry].turn;
        // Definitions and plans go after the statement and before data.
        let at = turn
            .parts
            .iter()
            .position(|p| p.sensitivity != pythia_ai::Sensitivity::Metadata)
            .unwrap_or(turn.parts.len());
        turn.parts.splice(at..at, parts);
        self.ask(entry, ai, repaint);
    }

    /// Sends the conversation up to `entry` to the backend.
    pub fn ask(&mut self, entry: usize, ai: &AiSettings, repaint: impl Fn() + Send + 'static) {
        let Some(backend) = self.backend(ai).cloned() else {
            self.entries[entry].status = Status::Failed("No AI backend is set up".into());
            return;
        };
        let turns: Vec<Turn> = self.entries[..=entry]
            .iter()
            .map(|e| e.turn.clone())
            .collect();
        let (request, rendered) =
            pythia_ai::prompt::request(&turns, Policy::new(ai), backend.context_chars.max(2000));
        let e = &mut self.entries[entry];
        e.sent = Some(rendered);
        e.status = Status::Asking;
        e.backend = backend.name.clone();
        let timeout = Duration::from_secs(ai.timeout_secs.max(5));
        let job = spawn(backend, request, timeout, repaint, self.responder.clone());
        self.pending = Some(Pending::Answer { job, entry });
    }

    /// Takes the answer of the running request, if it is in.
    pub fn poll(&mut self) {
        let Some(Pending::Answer { job, entry }) = &self.pending else {
            return;
        };
        let Some(result) = job.poll() else {
            return;
        };
        let elapsed = job.started.elapsed();
        let entry = *entry;
        self.pending = None;
        let e = &mut self.entries[entry];
        match result {
            Ok(answer) => {
                e.turn.answer = Some(answer);
                e.status = Status::Done(elapsed);
            }
            Err(error) => e.status = Status::Failed(error),
        }
    }

    /// Stops waiting for the running request.
    pub fn stop(&mut self) {
        match self.pending.take() {
            Some(Pending::Answer { job, entry }) => {
                job.cancel();
                self.entries[entry].status = Status::Stopped;
            }
            Some(Pending::Lookup { entry, .. }) => self.entries[entry].status = Status::Stopped,
            None => {}
        }
    }

    /// Starts a new conversation.
    pub fn clear(&mut self) {
        self.stop();
        self.entries.clear();
        self.notice = None;
    }

    /// Code from entry `entry` replaced its target: the new text is the
    /// target now, so another suggestion can replace it again.
    pub fn replaced(&mut self, entry: usize, range: Range<usize>, text: String) {
        if let Some(origin) = self.entries.get_mut(entry).and_then(|e| e.origin.as_mut()) {
            origin.target = Some((range, text));
        }
    }

    /// Answers received (tests).
    #[cfg(test)]
    pub fn answers(&self) -> usize {
        self.entries
            .iter()
            .filter(|e| e.turn.answer.is_some())
            .count()
    }

    /// The last answer (tests).
    #[cfg(test)]
    pub fn last_answer(&self) -> Option<&str> {
        self.entries.last()?.turn.answer.as_deref()
    }

    /// Why the last request failed (tests).
    #[cfg(test)]
    pub fn last_error(&self) -> Option<&str> {
        match &self.entries.last()?.status {
            Status::Failed(e) => Some(e),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn backend() -> AiBackend {
        AiBackend::new(pythia_config::AiKind::Cli, "fake".into())
    }

    fn settings() -> AiSettings {
        AiSettings {
            backends: vec![backend()],
            ..Default::default()
        }
    }

    fn wait(assistant: &mut Assistant) {
        let start = Instant::now();
        while assistant.is_busy() {
            assert!(start.elapsed() < Duration::from_secs(10));
            assistant.poll();
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn conversation_with_a_fake_backend() {
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let log = seen.clone();
        let mut assistant = Assistant {
            responder: Some(Arc::new(move |r: &ChatRequest| {
                log.lock().unwrap().push(r.clone());
                Ok(format!("answer {}", r.messages.len()))
            })),
            ..Default::default()
        };
        let ai = settings();
        let statement = Part::code("Statement", "select 1 from dual");
        let first = assistant.add(
            Task::Explain,
            vec![statement.clone()],
            None,
            "SQL 1".into(),
            &backend(),
        );
        assistant.ask(first, &ai, || {});
        wait(&mut assistant);
        assert_eq!(assistant.last_answer(), Some("answer 1"));

        // A follow-up doesn't repeat the statement, and carries the history.
        let second = assistant.add(
            Task::Ask("Why?".into()),
            vec![statement, Part::text("Database", "Oracle")],
            None,
            "SQL 1".into(),
            &backend(),
        );
        assert_eq!(assistant.entries[second].turn.parts.len(), 1);
        assistant.ask(second, &ai, || {});
        wait(&mut assistant);
        assert_eq!(assistant.last_answer(), Some("answer 3"));
        let requests = seen.lock().unwrap();
        assert_eq!(requests[1].messages[1].content, "answer 1");
        assert!(requests[1].messages[2].content.ends_with("Why?"));
    }

    #[test]
    fn lookups_join_the_request() {
        let mut assistant = Assistant {
            responder: Some(Arc::new(|r: &ChatRequest| {
                Ok(r.messages.last().unwrap().content.clone())
            })),
            ..Default::default()
        };
        let ai = settings();
        let entry = assistant.add(
            Task::Explain,
            vec![
                Part::code("Statement", "select * from emp"),
                Part::bind_values("Bind variable values", ":x = 1"),
            ],
            None,
            String::new(),
            &backend(),
        );
        assistant.look_up(entry, 42);
        assert!(assistant.is_lookup(42) && !assistant.is_lookup(1));
        assistant.on_lookup(
            Ok(vec![Part::text("TABLE SCOTT.EMP", "EMPNO NUMBER")]),
            &ai,
            || {},
        );
        let titles: Vec<&str> = assistant.entries[entry]
            .turn
            .parts
            .iter()
            .map(|p| p.title.as_str())
            .collect();
        assert_eq!(
            titles,
            ["Statement", "TABLE SCOTT.EMP", "Bind variable values"]
        );
        wait(&mut assistant);
        let sent = assistant.last_answer().unwrap();
        assert!(sent.find("## Statement").unwrap() < sent.find("## TABLE SCOTT.EMP").unwrap());
        assert!(!sent.contains(":x = 1"));
        let rendered = assistant.entries[entry].sent.as_ref().unwrap();
        assert_eq!(rendered.withheld, ["Bind variable values"]);

        assistant.responder = Some(Arc::new(|_: &ChatRequest| Err("no model here".into())));
        let entry = assistant.add(Task::Fix, Vec::new(), None, String::new(), &backend());
        assistant.ask(entry, &ai, || {});
        wait(&mut assistant);
        assert_eq!(assistant.last_error(), Some("no model here"));
    }

    #[test]
    fn replacement_keeps_the_editors_terminator() {
        assert_eq!(
            fit_terminator("select 1 from dual", "select 2 from dual;\n"),
            "select 2 from dual"
        );
        assert_eq!(
            fit_terminator("select 1 from dual;", "select 2 from dual;"),
            "select 2 from dual;"
        );
        assert_eq!(
            fit_terminator("begin\n  null;\nend;", "begin\n  p;\nend;\n/\n"),
            "begin\n  p;\nend;"
        );
        assert_eq!(
            fit_terminator(
                "select 1 from dual",
                "create index i on t (a);\nselect a from t;"
            ),
            "create index i on t (a);\nselect a from t"
        );
    }

    #[test]
    fn stopping_and_clearing() {
        let mut assistant = Assistant::default();
        let ai = AiSettings::default();
        let entry = assistant.add(Task::Explain, Vec::new(), None, String::new(), &backend());
        assistant.look_up(entry, 7);
        assistant.stop();
        assert!(!assistant.is_busy() && !assistant.is_lookup(7));
        assert_eq!(assistant.entries[entry].status, Status::Stopped);
        assistant.ask(entry, &ai, || {});
        assert_eq!(assistant.last_error(), Some("No AI backend is set up"));
        assistant.clear();
        assert!(assistant.entries.is_empty());
    }
}
