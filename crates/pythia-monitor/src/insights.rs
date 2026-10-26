//! Plain-language findings from a dashboard snapshot ("what needs attention").

use crate::queries::{DashboardSnapshot, Rates};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    Info,
    Warning,
    Critical,
}

/// Where clicking an insight leads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    BlockingTree,
    Session { sid: i64, serial: i64 },
    Sessions,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Insight {
    pub severity: Severity,
    pub text: String,
    pub target: Option<Target>,
    /// The session it is about, for a "Details" link next to the target.
    pub session: Option<(i64, i64)>,
}

fn secs(s: f64) -> String {
    crate::ui::style::duration(s)
}

/// Derives findings, most severe first. Empty means "all good".
pub fn insights(snap: &DashboardSnapshot, rates: Option<&Rates>) -> Vec<Insight> {
    let mut out = Vec::new();
    for b in &snap.blockers {
        let who = b.username.as_deref().unwrap_or("background");
        let state = if b.status == "INACTIVE" {
            " — idle, likely an uncommitted transaction"
        } else {
            ""
        };
        out.push(Insight {
            severity: if b.max_wait >= 10.0 {
                Severity::Critical
            } else {
                Severity::Warning
            },
            text: format!(
                "Session {} ({who}) blocks {} session{}, longest wait {}{state}",
                b.sid,
                b.blocked,
                if b.blocked == 1 { "" } else { "s" },
                secs(b.max_wait)
            ),
            target: Some(Target::BlockingTree),
            session: Some((b.sid, b.serial)),
        });
    }
    let cpus = snap.instance.as_ref().map_or(1, |i| i.cpus.max(1)) as f64;
    if let Some(r) = rates {
        if r.aas >= cpus {
            out.push(Insight {
                severity: Severity::Critical,
                text: format!(
                    "Database is saturated: {:.1} average active sessions on {cpus:.0} CPUs",
                    r.aas
                ),
                target: Some(Target::Sessions),
                session: None,
            });
        } else if r.aas >= cpus * 0.7 {
            out.push(Insight {
                severity: Severity::Warning,
                text: format!(
                    "High load: {:.1} average active sessions on {cpus:.0} CPUs",
                    r.aas
                ),
                target: Some(Target::Sessions),
                session: None,
            });
        }
        if let Some(cpu) = r.host_cpu_pct {
            if cpu >= 90.0 {
                out.push(Insight {
                    severity: Severity::Critical,
                    text: format!("Host CPU at {cpu:.0} %"),
                    target: None,
                    session: None,
                });
            } else if cpu >= 75.0 {
                out.push(Insight {
                    severity: Severity::Warning,
                    text: format!("Host CPU at {cpu:.0} %"),
                    target: None,
                    session: None,
                });
            }
        }
    }
    for t in &snap.tablespaces {
        let severity = if t.used_pct >= 90.0 {
            Severity::Critical
        } else if t.used_pct >= 80.0 {
            Severity::Warning
        } else {
            continue;
        };
        out.push(Insight {
            severity,
            text: format!("Tablespace {} is {:.0} % full", t.name, t.used_pct),
            target: None,
            session: None,
        });
    }
    if let Some(limit) = snap.open_cursors_limit.filter(|l| *l > 0) {
        for c in &snap.cursor_holders {
            if c.open as f64 >= limit as f64 * 0.85 {
                out.push(Insight {
                    severity: Severity::Warning,
                    text: format!(
                        "Session {} ({}) has {} of {limit} cursors open — possible cursor leak",
                        c.sid,
                        c.username.as_deref().unwrap_or("background"),
                        c.open
                    ),
                    target: Some(Target::Session {
                        sid: c.sid,
                        serial: c.serial,
                    }),
                    session: None,
                });
            }
        }
    }
    if !snap.long_ops.is_empty() {
        let n = snap.long_ops.len();
        out.push(Insight {
            severity: Severity::Info,
            text: format!(
                "{n} long-running operation{} in progress",
                if n == 1 { "" } else { "s" }
            ),
            target: None,
            session: None,
        });
    }
    out.sort_by_key(|i| std::cmp::Reverse(i.severity));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::queries::{Blocker, CursorHolder, InstanceInfo, Tablespace};

    #[test]
    fn healthy() {
        assert!(insights(&DashboardSnapshot::default(), Some(&Rates::default())).is_empty());
    }

    #[test]
    fn findings_sorted_by_severity() {
        let snap = DashboardSnapshot {
            instance: Some(InstanceInfo {
                cpus: 4,
                ..InstanceInfo::default()
            }),
            blockers: vec![Blocker {
                sid: 204,
                serial: 1,
                username: Some("HR".into()),
                status: "INACTIVE".into(),
                blocked: 2,
                max_wait: 47.0,
            }],
            tablespaces: vec![Tablespace {
                name: "USERS".into(),
                used_pct: 82.0,
                used_bytes: 0.0,
                max_bytes: 0.0,
            }],
            open_cursors_limit: Some(300),
            cursor_holders: vec![CursorHolder {
                sid: 9,
                serial: 2,
                username: None,
                program: None,
                open: 290,
            }],
            ..DashboardSnapshot::default()
        };
        let rates = Rates {
            aas: 5.0,
            host_cpu_pct: Some(50.0),
            ..Rates::default()
        };
        let found = insights(&snap, Some(&rates));
        let severities: Vec<Severity> = found.iter().map(|i| i.severity).collect();
        assert_eq!(
            severities,
            [
                Severity::Critical,
                Severity::Critical,
                Severity::Warning,
                Severity::Warning
            ]
        );
        assert!(
            found[0]
                .text
                .contains("Session 204 (HR) blocks 2 sessions, longest wait 47s — idle"),
            "{found:?}"
        );
        assert!(found.iter().any(|i| i.text.contains("saturated")));
        assert!(
            found
                .iter()
                .any(|i| i.target == Some(Target::Session { sid: 9, serial: 2 }))
        );
    }
}
