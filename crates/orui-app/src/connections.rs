//! Open connections and their sessions.
//!
//! Every connection has a *main* session used by the object browser and other
//! metadata lookups. Documents (SQL and program windows) get their own session
//! in multi-session mode, so a long-running query never blocks the browser or
//! another window, and every window has its own transaction.

use std::collections::{BTreeMap, HashMap};

use orui_config::ConnectionProfile;
use orui_db::{ConnectParams, DbEvent, DbRequest, ServerInfo, Session};

use crate::documents::DocId;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SessionKey {
    Main,
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
