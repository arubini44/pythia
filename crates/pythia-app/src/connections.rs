//! Open connections and their sessions.
//!
//! Every connection has a *main* session used by the object browser and other
//! metadata lookups. Documents (SQL and program windows) get their own session
//! in multi-session mode, so a long-running query never blocks the browser or
//! another window, and every window has its own transaction. Object views and
//! forms load on a *metadata* session of their own, so they don't queue
//! behind the browser, completion and compiles on the main session.

use std::collections::{BTreeMap, HashMap};

use pythia_config::ConnectionProfile;
use pythia_db::{ConnectParams, DbEvent, DbRequest, ServerInfo, Session};

use crate::documents::DocId;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SessionKey {
    Main,
    Meta,
    Doc(DocId),
}

pub enum Status {
    Connecting,
    Connected(ServerInfo),
}

pub struct DocSession {
    pub session: Session,
    pub pending_tx: bool,
    pub autocommit: bool,
}

pub struct Connection {
    pub profile: ConnectionProfile,
    params: ConnectParams,
    pub status: Status,
    pub main: Session,
    pub main_pending_tx: bool,
    /// The metadata session, opened on first use.
    meta: Option<Session>,
    /// Requests sent to it and not answered yet: true for tasks.
    meta_pending: HashMap<u64, bool>,
    pub docs: HashMap<DocId, DocSession>,
}

impl Connection {
    pub fn name(&self) -> String {
        self.profile.display_name()
    }

    pub fn is_connected(&self) -> bool {
        matches!(self.status, Status::Connected(_))
    }

    pub fn server(&self) -> Option<&ServerInfo> {
        match &self.status {
            Status::Connected(info) => Some(info),
            Status::Connecting => None,
        }
    }

    /// How sessions of this connection log in (for worker sessions).
    pub fn params(&self) -> &pythia_db::ConnectParams {
        &self.params
    }

    /// Schema the browser starts in.
    pub fn current_schema(&self) -> String {
        self.server()
            .map(|s| s.current_schema.clone())
            .unwrap_or_else(|| self.profile.username.to_uppercase())
    }

    /// The document's session, opening it on first use.
    pub fn doc_session(&mut self, doc: DocId, notify: &Notify) -> &mut DocSession {
        let params = &self.params;
        self.docs.entry(doc).or_insert_with(|| DocSession {
            session: Session::connect(params.clone(), notify.callback()),
            pending_tx: false,
            autocommit: false,
        })
    }

    pub fn any_pending_tx(&self) -> bool {
        self.main_pending_tx || self.docs.values().any(|d| d.pending_tx)
    }

    /// Replaces a lost main session with a new one.
    pub fn reconnect_main(&mut self, notify: &Notify) {
        self.main = Session::connect(self.params.clone(), notify.callback());
        self.main_pending_tx = false;
    }

    /// Sends a metadata query or task (with its request id) to the metadata
    /// session, opening it on first use.
    pub fn send_meta(&mut self, id: u64, request: DbRequest, notify: &Notify) {
        let params = &self.params;
        let session = self
            .meta
            .get_or_insert_with(|| Session::connect(params.clone(), notify.callback()));
        self.meta_pending
            .insert(id, matches!(request, DbRequest::Task { .. }));
        session.send(request);
    }

    #[cfg(test)]
    pub fn has_meta_session(&self) -> bool {
        self.meta.is_some()
    }

    /// The metadata session's events. If it is lost or can't connect, the
    /// requests waiting on it fail, and the next one opens a new session.
    fn poll_meta(&mut self) -> Vec<DbEvent> {
        let mut out = Vec::new();
        let Some(session) = &mut self.meta else {
            return out;
        };
        let mut failed = None;
        while let Some(event) = session.try_recv() {
            match event {
                DbEvent::Connected(_) | DbEvent::Notice(_) => {}
                DbEvent::ConnectFailed(e) => failed = Some(e),
                DbEvent::Meta { id, result } => {
                    if let Err(e) = &result
                        && e.is_connection_lost()
                    {
                        failed = Some(e.clone());
                    }
                    self.meta_pending.remove(&id);
                    out.push(DbEvent::Meta { id, result });
                }
                DbEvent::TaskDone { id, result } => {
                    if let Err(e) = &result
                        && e.is_connection_lost()
                    {
                        failed = Some(e.clone());
                    }
                    self.meta_pending.remove(&id);
                    out.push(DbEvent::TaskDone { id, result });
                }
                event => out.push(event),
            }
        }
        if let Some(error) = failed {
            self.meta = None;
            for (id, task) in self.meta_pending.drain() {
                let error = error.clone();
                out.push(if task {
                    DbEvent::TaskDone {
                        id,
                        result: Err(error),
                    }
                } else {
                    DbEvent::Meta {
                        id,
                        result: Err(error),
                    }
                });
            }
        }
        out
    }
}

/// Wakes the UI when a session has news.
#[derive(Clone)]
pub struct Notify(pub egui::Context);

impl Notify {
    pub fn callback(&self) -> impl Fn() + Send + 'static {
        let ctx = self.0.clone();
        move || ctx.request_repaint()
    }
}

#[derive(Default)]
pub struct Connections {
    /// Keyed by display name, which is also how documents refer to them.
    pub open: BTreeMap<String, Connection>,
}

impl Connections {
    pub fn connect(
        &mut self,
        profile: ConnectionProfile,
        password: String,
        notify: &Notify,
    ) -> String {
        let params = ConnectParams {
            username: profile.username.clone(),
            password,
            connect_string: profile.connect_string.clone(),
            privilege: profile.privilege,
            init_sql: init_sql(&profile),
        };
        let name = profile.display_name();
        let main = Session::connect(params.clone(), notify.callback());
        self.open.insert(
            name.clone(),
            Connection {
                profile,
                params,
                status: Status::Connecting,
                main,
                main_pending_tx: false,
                meta: None,
                meta_pending: HashMap::new(),
                docs: HashMap::new(),
            },
        );
        name
    }

    pub fn get(&self, name: &str) -> Option<&Connection> {
        self.open.get(name)
    }

    pub fn get_mut(&mut self, name: &str) -> Option<&mut Connection> {
        self.open.get_mut(name)
    }

    /// Closing drops all sessions, which disconnects (and rolls back) them.
    pub fn close(&mut self, name: &str) -> Option<Connection> {
        self.open.remove(name)
    }

    pub fn close_doc(&mut self, doc: DocId) {
        for conn in self.open.values_mut() {
            if let Some(s) = conn.docs.remove(&doc) {
                s.session.cancel();
            }
        }
    }

    pub fn any_pending_tx(&self) -> bool {
        self.open.values().any(Connection::any_pending_tx)
    }

    /// Drains all sessions' events.
    pub fn poll(&mut self) -> Vec<(String, SessionKey, DbEvent)> {
        let mut out = Vec::new();
        for (name, conn) in &mut self.open {
            while let Some(event) = conn.main.try_recv() {
                if let DbEvent::Transaction { pending } = event {
                    conn.main_pending_tx = pending;
                }
                out.push((name.clone(), SessionKey::Main, event));
            }
            for event in conn.poll_meta() {
                out.push((name.clone(), SessionKey::Meta, event));
            }
            for (doc, s) in &mut conn.docs {
                while let Some(event) = s.session.try_recv() {
                    if let DbEvent::Transaction { pending } = event {
                        s.pending_tx = pending;
                    }
                    out.push((name.clone(), SessionKey::Doc(*doc), event));
                }
            }
        }
        out
    }

    pub fn send_main(&self, name: &str, request: DbRequest) -> bool {
        match self.open.get(name) {
            Some(conn) => {
                conn.main.send(request);
                true
            }
            None => false,
        }
    }
}

/// Statements every session of `profile` runs after connecting.
fn init_sql(profile: &ConnectionProfile) -> Vec<String> {
    let mut out = Vec::new();
    let schema = profile.default_schema.trim();
    if !schema.is_empty() {
        out.push(format!(
            "alter session set current_schema = {}",
            crate::documents::quote_ident(&schema.to_uppercase())
        ));
    }
    let script = &profile.login_script;
    for statement in pythia_plsql::split_statements(script) {
        if statement.kind != pythia_plsql::StatementKind::SqlPlus {
            out.push(statement.text(script).to_owned());
        }
    }
    out
}
