//! The session list of the databases read through a client (MySQL,
//! PostgreSQL, SQL Server): every session, idle ones too, in one shape for
//! the Sessions tab, and who blocks whom.
//!
//! Each database reads its own ([`crate::mysql::connections`],
//! [`crate::postgres::connections`], [`crate::sqlserver::connections`]).

use std::collections::{HashMap, HashSet};

/// A session: a MySQL thread, a PostgreSQL backend, a SQL Server session.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Connection {
    pub id: i64,
    pub user: Option<String>,
    /// The client's host (MySQL without its port), as the dashboard groups it.
    pub host: Option<String>,
    pub database: Option<String>,
    /// PostgreSQL's application, SQL Server's program; none on MySQL.
    pub program: Option<String>,
    /// MySQL's command, PostgreSQL's state, SQL Server's status.
    pub status: String,
    /// Running a statement (not sleeping or idle).
    pub active: bool,
    /// What it does or waits for: MySQL's state, PostgreSQL's wait event,
    /// SQL Server's wait type.
    pub wait: Option<String>,
    /// Seconds in the statement it runs, or since its last one ended.
    pub seconds: f64,
    /// How long its open transaction has been open.
    pub transaction_secs: Option<f64>,
    /// The session it waits for on a lock.
    pub blocked_by: Option<i64>,
    /// How long it has waited for the lock.
    pub wait_secs: f64,
    /// The statement it runs, or (idle) ran last; none when hidden.
    pub query: Option<String>,
    /// The server's own: replication, scheduler, background workers.
    pub background: bool,
}

impl Connection {
    pub fn is_blocked(&self) -> bool {
        self.blocked_by.is_some()
    }

    /// Idle with a transaction open: a likely cause of blocking.
    pub fn idle_in_transaction(&self) -> bool {
        !self.active && self.transaction_secs.is_some()
    }
}

/// All sessions, and what keeps some of them out of view.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ConnectionList {
    pub rows: Vec<Connection>,
    /// What's missing to see everything, e.g. "Only this user's sessions
    /// are listed: …".
    pub notes: Vec<String>,
}

/// The sessions of one user, host, database or program: what the
/// dashboard's bars open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectionFilter {
    User(String),
    Host(String),
    Database(String),
    Program(String),
}

impl ConnectionFilter {
    pub fn matches(&self, c: &Connection) -> bool {
        // The dashboards group a missing value as "(none)" or "(unknown)".
        let is = |v: &Option<String>, want: &str| match v.as_deref().filter(|v| !v.is_empty()) {
            Some(v) => v == want,
            None => matches!(want, "(none)" | "(unknown)"),
        };
        match self {
            ConnectionFilter::User(u) => is(&c.user, u),
            ConnectionFilter::Host(h) => is(&c.host, h),
            ConnectionFilter::Database(d) => is(&c.database, d),
            ConnectionFilter::Program(p) => is(&c.program, p),
        }
    }

    pub fn label(&self) -> String {
        match self {
            ConnectionFilter::User(u) => format!("user {u}"),
            ConnectionFilter::Host(h) => format!("host {h}"),
            ConnectionFilter::Database(d) => format!("database {d}"),
            ConnectionFilter::Program(p) => format!("program {p}"),
        }
    }
}

/// A blocker and the sessions waiting for it.
#[derive(Debug, Clone, PartialEq)]
pub struct BlockingNode {
    pub connection: Connection,
    /// Not in the list (gone, or not visible): only its id is known.
    pub unknown: bool,
    pub children: Vec<BlockingNode>,
}

impl BlockingNode {
    pub fn blocked_count(&self) -> usize {
        self.children.iter().map(|c| 1 + c.blocked_count()).sum()
    }

    pub fn max_wait(&self) -> f64 {
        self.children
            .iter()
            .map(|c| c.connection.wait_secs.max(c.max_wait()))
            .fold(0.0, f64::max)
    }
}

/// The trees of blocked sessions under the sessions that block them and
/// wait for nobody, longest wait first.
pub fn blocking_tree(rows: &[Connection]) -> Vec<BlockingNode> {
    let by_id: HashMap<i64, &Connection> = rows.iter().map(|c| (c.id, c)).collect();
    let mut children: HashMap<i64, Vec<i64>> = HashMap::new();
    for c in rows {
        if let Some(b) = c.blocked_by {
            children.entry(b).or_default().push(c.id);
        }
    }
    // Roots: blockers that aren't blocked themselves, or not listed. A
    // cycle (a deadlock about to be broken) gets its lowest id as root.
    let mut roots: Vec<i64> = children
        .keys()
        .copied()
        .filter(|b| by_id.get(b).is_none_or(|c| c.blocked_by.is_none()))
        .collect();
    let mut seen = HashSet::new();
    fn build(
        id: i64,
        by_id: &HashMap<i64, &Connection>,
        children: &HashMap<i64, Vec<i64>>,
        seen: &mut HashSet<i64>,
    ) -> BlockingNode {
        seen.insert(id);
        let ids: Vec<i64> = children
            .get(&id)
            .into_iter()
            .flatten()
            .filter(|c| !seen.contains(c))
            .copied()
            .collect();
        let mut kids: Vec<BlockingNode> = ids
            .into_iter()
            .map(|c| build(c, by_id, children, seen))
            .collect();
        kids.sort_by(|a, b| b.connection.wait_secs.total_cmp(&a.connection.wait_secs));
        BlockingNode {
            connection: by_id.get(&id).map_or_else(
                || Connection {
                    id,
                    ..Connection::default()
                },
                |c| (*c).clone(),
            ),
            unknown: !by_id.contains_key(&id),
            children: kids,
        }
    }
    roots.sort_unstable();
    let mut trees: Vec<BlockingNode> = roots
        .into_iter()
        .map(|r| build(r, &by_id, &children, &mut seen))
        .collect();
    let mut cyclic: Vec<i64> = rows
        .iter()
        .filter(|c| c.blocked_by.is_some() && !seen.contains(&c.id))
        .map(|c| c.id)
        .collect();
    cyclic.sort_unstable();
    for id in cyclic {
        if !seen.contains(&id) {
            trees.push(build(id, &by_id, &children, &mut seen));
        }
    }
    trees.sort_by(|a, b| b.max_wait().total_cmp(&a.max_wait()));
    trees
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conn(id: i64, blocked_by: Option<i64>, wait_secs: f64) -> Connection {
        Connection {
            id,
            blocked_by,
            wait_secs,
            ..Connection::default()
        }
    }

    #[test]
    fn tree() {
        let rows = [
            conn(1, None, 0.0),
            conn(2, Some(1), 5.0),
            conn(3, Some(2), 9.0),
            conn(4, Some(1), 1.0),
            // Its blocker isn't listed.
            conn(5, Some(99), 2.0),
            conn(6, None, 0.0),
        ];
        let trees = blocking_tree(&rows);
        assert_eq!(trees.len(), 2);
        let root = &trees[0];
        assert_eq!(root.connection.id, 1);
        assert!(!root.unknown);
        assert_eq!(root.blocked_count(), 3);
        assert_eq!(root.max_wait(), 9.0);
        // Longest wait first.
        assert_eq!(root.children[0].connection.id, 2);
        assert_eq!(root.children[0].children[0].connection.id, 3);
        assert_eq!(trees[1].connection.id, 99);
        assert!(trees[1].unknown);
    }

    #[test]
    fn deadlock() {
        let rows = [conn(7, Some(8), 3.0), conn(8, Some(7), 4.0)];
        let trees = blocking_tree(&rows);
        assert_eq!(trees.len(), 1);
        assert_eq!(trees[0].connection.id, 7);
        assert_eq!(trees[0].blocked_count(), 1);
    }

    #[test]
    fn filters() {
        let c = Connection {
            user: Some("app".into()),
            host: Some("10.0.0.5".into()),
            database: None,
            program: Some(String::new()),
            ..Connection::default()
        };
        assert!(ConnectionFilter::User("app".into()).matches(&c));
        assert!(!ConnectionFilter::User("root".into()).matches(&c));
        assert!(ConnectionFilter::Host("10.0.0.5".into()).matches(&c));
        assert!(ConnectionFilter::Database("(none)".into()).matches(&c));
        assert!(ConnectionFilter::Database("(unknown)".into()).matches(&c));
        assert!(ConnectionFilter::Program("(none)".into()).matches(&c));
    }
}
