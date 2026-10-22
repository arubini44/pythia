//! A tkprof-like parser and aggregator for 10046 SQL trace files.
//!
//! The Instant Client doesn't ship `tkprof`, and traces read through
//! `V$DIAG_TRACE_FILE_CONTENTS` never touch the local disk anyway, so the
//! analysis is done here. Supported lines: `PARSING IN CURSOR` / `END OF
//! STMT`, `PARSE`, `EXEC`, `FETCH`, `WAIT`, `STAT`, `BINDS`, `CLOSE`,
//! `XCTEND` and `PARSE ERROR`. Times in traces are microseconds.

use std::collections::HashMap;
use std::fmt::Write as _;

/// Calls are aggregated per database call type, like tkprof's table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum CallKind {
    Parse,
    Execute,
    Fetch,
}

impl CallKind {
    pub const ALL: [CallKind; 3] = [CallKind::Parse, CallKind::Execute, CallKind::Fetch];

    pub fn label(self) -> &'static str {
        match self {
            CallKind::Parse => "Parse",
            CallKind::Execute => "Execute",
            CallKind::Fetch => "Fetch",
        }
    }
}

/// One row of the call table. Times in microseconds.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CallStats {
    pub count: u64,
    pub cpu: u64,
    pub elapsed: u64,
    /// Physical reads (`p`).
    pub disk: u64,
    /// Consistent gets (`cr`).
    pub query: u64,
    /// Current gets (`cu`).
    pub current: u64,
    pub rows: u64,
    /// Library cache misses (`mis`).
    pub misses: u64,
}

impl CallStats {
    fn add(&mut self, o: &CallStats) {
        self.count += o.count;
        self.cpu += o.cpu;
        self.elapsed += o.elapsed;
        self.disk += o.disk;
        self.query += o.query;
        self.current += o.current;
        self.rows += o.rows;
        self.misses += o.misses;
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct WaitStats {
    pub count: u64,
    /// Microseconds.
    pub total: u64,
    pub max: u64,
}

/// A row source operation from `STAT` lines.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RowSource {
    pub id: u32,
    pub parent: u32,
    pub depth: usize,
    pub rows: u64,
    pub object_id: i64,
    /// Operation including statistics, e.g. `TABLE ACCESS FULL EMP (cr=7 pr=0 pw=0 time=37 us ...)`.
    pub operation: String,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Statement {
    pub sql_id: Option<String>,
    pub text: String,
    /// Recursive depth (`dep`): 0 for statements issued by the client.
    pub depth: u32,
    /// Parsing user id (`uid`).
    pub user_id: Option<i64>,
    /// Optimizer goal (`og`).
    pub optimizer_goal: Option<String>,
    pub calls: [CallStats; 3],
    pub waits: Vec<(String, WaitStats)>,
    pub row_source: Vec<RowSource>,
    /// Values of the first captured bind set.
    pub binds: Vec<String>,
    pub parse_errors: Vec<String>,
}

impl Statement {
    pub fn total(&self) -> CallStats {
        let mut t = CallStats::default();
        for c in &self.calls {
            t.add(c);
        }
        t
    }

    pub fn call(&self, kind: CallKind) -> &CallStats {
        &self.calls[kind as usize]
    }

    pub fn wait_total(&self) -> u64 {
        self.waits.iter().map(|(_, w)| w.total).sum()
    }

    /// The statement's section of the tkprof text (bind values aren't part
    /// of it).
    pub fn to_text(&self) -> String {
        let mut out = String::new();
        if let Some(id) = &self.sql_id {
            let _ = writeln!(out, "\nSQL ID: {id}\n");
        }
        let _ = writeln!(out, "{}\n", self.text);
        call_table(&mut out, &self.calls);
        let _ = writeln!(
            out,
            "\nMisses in library cache during parse: {}",
            self.call(CallKind::Parse).misses
        );
        if self.call(CallKind::Execute).misses > 0 {
            let _ = writeln!(
                out,
                "Misses in library cache during execute: {}",
                self.call(CallKind::Execute).misses
            );
        }
        if let Some(og) = &self.optimizer_goal {
            let _ = writeln!(out, "Optimizer mode: {og}");
        }
        if let Some(uid) = self.user_id {
            let _ = writeln!(
                out,
                "Parsing user id: {uid}{}",
                if self.is_recursive() {
                    format!("     (recursive depth: {})", self.depth)
                } else {
                    String::new()
                }
            );
        }
        for e in &self.parse_errors {
            let _ = writeln!(out, "Parse error: {e}");
        }
        if !self.row_source.is_empty() {
            let _ = writeln!(out, "\n      Rows  Row Source Operation");
            let _ = writeln!(
                out,
                "----------  ---------------------------------------------------"
            );
            for r in &self.row_source {
                let _ = writeln!(
                    out,
                    "{:>10}  {}{}",
                    r.rows,
                    " ".repeat(r.depth),
                    r.operation
                );
            }
        }
        wait_table(&mut out, &self.waits);
        out.push('\n');
        out
    }

    fn wait(&mut self, event: &str, ela: u64) {
        let w = match self.waits.iter_mut().find(|(n, _)| n == event) {
            Some((_, w)) => w,
            None => {
                self.waits.push((event.to_owned(), WaitStats::default()));
                &mut self.waits.last_mut().expect("just pushed").1
            }
        };
        w.count += 1;
        w.total += ela;
        w.max = w.max.max(ela);
    }

    pub fn is_recursive(&self) -> bool {
        self.depth > 0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SortKey {
    /// Elapsed time of all calls (tkprof has no single key for this).
    #[default]
    TotalElapsed,
    TotalCpu,
    ParseElapsed,
    ExecuteElapsed,
    FetchElapsed,
    ParseCpu,
    ExecuteCpu,
    FetchCpu,
    ExecuteCount,
    Disk,
    Query,
    Rows,
    Waits,
}

impl SortKey {
    pub const ALL: [SortKey; 13] = [
        SortKey::TotalElapsed,
        SortKey::TotalCpu,
        SortKey::ParseElapsed,
        SortKey::ExecuteElapsed,
        SortKey::FetchElapsed,
        SortKey::ParseCpu,
        SortKey::ExecuteCpu,
        SortKey::FetchCpu,
        SortKey::ExecuteCount,
        SortKey::Disk,
        SortKey::Query,
        SortKey::Rows,
        SortKey::Waits,
    ];

    /// Label, with the tkprof `sort=` option name where one exists.
    pub fn label(self) -> &'static str {
        match self {
            SortKey::TotalElapsed => "elapsed (total)",
            SortKey::TotalCpu => "cpu (total)",
            SortKey::ParseElapsed => "prsela",
            SortKey::ExecuteElapsed => "exeela",
            SortKey::FetchElapsed => "fchela",
            SortKey::ParseCpu => "prscpu",
            SortKey::ExecuteCpu => "execpu",
            SortKey::FetchCpu => "fchcpu",
            SortKey::ExecuteCount => "execnt",
            SortKey::Disk => "disk (total)",
            SortKey::Query => "query (total)",
            SortKey::Rows => "rows (total)",
            SortKey::Waits => "wait time",
        }
    }

    pub fn value(self, s: &Statement) -> u64 {
        let t = s.total();
        match self {
            SortKey::TotalElapsed => t.elapsed,
            SortKey::TotalCpu => t.cpu,
            SortKey::ParseElapsed => s.call(CallKind::Parse).elapsed,
            SortKey::ExecuteElapsed => s.call(CallKind::Execute).elapsed,
            SortKey::FetchElapsed => s.call(CallKind::Fetch).elapsed,
            SortKey::ParseCpu => s.call(CallKind::Parse).cpu,
            SortKey::ExecuteCpu => s.call(CallKind::Execute).cpu,
            SortKey::FetchCpu => s.call(CallKind::Fetch).cpu,
            SortKey::ExecuteCount => s.call(CallKind::Execute).count,
            SortKey::Disk => t.disk,
            SortKey::Query => t.query + t.current,
            SortKey::Rows => t.rows,
            SortKey::Waits => s.wait_total(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Totals {
    pub statements: usize,
    pub calls: [CallStats; 3],
    pub waits: Vec<(String, WaitStats)>,
}

impl Totals {
    pub fn total(&self) -> CallStats {
        let mut t = CallStats::default();
        for c in &self.calls {
            t.add(c);
        }
        t
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Report {
    /// Header lines (trace file name, version, session id, ...).
    pub header: Vec<String>,
    pub statements: Vec<Statement>,
    pub non_recursive: Totals,
    pub recursive: Totals,
    pub commits: u64,
    pub rollbacks: u64,
    /// Lines that looked like trace records but could not be parsed.
    pub unparsed: usize,
    /// Number of trace lines read.
    pub lines: usize,
}

impl Report {
    /// Statement indexes sorted by `key`, descending.
    pub fn sorted(&self, key: SortKey) -> Vec<usize> {
        let mut idx: Vec<usize> = (0..self.statements.len()).collect();
        idx.sort_by(|a, b| {
            key.value(&self.statements[*b])
                .cmp(&key.value(&self.statements[*a]))
                .then(a.cmp(b))
        });
        idx
    }
}

/// Parses `key=value,key=value` (as in `PARSE #1:c=0,e=12,...`).
fn kv(s: &str) -> HashMap<&str, &str> {
    s.split(',')
        .filter_map(|p| p.split_once('='))
        .map(|(k, v)| (k.trim(), v.trim()))
        .collect()
}

fn num(map: &HashMap<&str, &str>, key: &str) -> u64 {
    map.get(key)
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(0)
}

/// Splits `#<cursor>:<rest>` (cursor ids can be large numbers).
fn cursor_and_rest(s: &str) -> Option<(&str, &str)> {
    let s = s.strip_prefix('#')?;
    let end = s.find(|c: char| !c.is_ascii_digit())?;
    let (cursor, rest) = s.split_at(end);
    Some((cursor, rest.trim_start_matches(':').trim_start()))
}

/// Key used to aggregate identical statements (like tkprof `aggregate=yes`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct StatementKey {
    text: String,
    depth: u32,
    user_id: Option<i64>,
}

fn optimizer_goal(og: &str) -> Option<String> {
    Some(
        match og {
            "1" => "ALL_ROWS",
            "2" => "FIRST_ROWS",
            "3" => "RULE",
            "4" => "CHOOSE",
            _ => return None,
        }
        .to_owned(),
    )
}

pub fn parse(trace: &str) -> Report {
    let mut report = Report::default();
    let mut by_key: HashMap<StatementKey, usize> = HashMap::new();
    // Cursor number → statement index.
    let mut cursors: HashMap<String, usize> = HashMap::new();
    // Row sources being collected per cursor (STAT lines arrive in a burst).
    let mut stat_burst: HashMap<String, Vec<RowSource>> = HashMap::new();
    let mut lines = trace.lines().peekable();
    let mut in_header = true;

    let statement_for = |report: &mut Report,
                         by_key: &mut HashMap<StatementKey, usize>,
                         key: StatementKey,
                         sql_id: Option<String>|
     -> usize {
        if let Some(&i) = by_key.get(&key) {
            return i;
        }
        report.statements.push(Statement {
            sql_id,
            text: key.text.clone(),
            depth: key.depth,
            user_id: key.user_id,
            ..Statement::default()
        });
        let i = report.statements.len() - 1;
        by_key.insert(key, i);
        i
    };

    while let Some(line) = lines.next() {
        report.lines += 1;
        let line = line.trim_end_matches('\r');
        if let Some(rest) = line.strip_prefix("PARSING IN CURSOR ") {
            in_header = false;
            let Some((cursor, attrs)) = cursor_and_rest(rest) else {
                report.unparsed += 1;
                continue;
            };
            let attrs: HashMap<&str, &str> = attrs
                .split_whitespace()
                .filter_map(|p| p.split_once('='))
                .map(|(k, v)| (k, v.trim_matches('\'')))
                .collect();
            let mut text = String::new();
            for l in lines.by_ref() {
                report.lines += 1;
                if l.trim_end() == "END OF STMT" {
                    break;
                }
                if !text.is_empty() {
                    text.push('\n');
                }
                text.push_str(l.trim_end_matches('\r'));
            }
            let key = StatementKey {
                text,
                depth: attrs.get("dep").and_then(|d| d.parse().ok()).unwrap_or(0),
                user_id: attrs.get("uid").and_then(|u| u.parse().ok()),
            };
            let sql_id = attrs.get("sqlid").map(|s| (*s).to_owned());
            let idx = statement_for(&mut report, &mut by_key, key, sql_id);
            cursors.insert(cursor.to_owned(), idx);
            continue;
        }

        let (kind, rest) = if let Some(r) = line.strip_prefix("PARSE ERROR ") {
            ("PARSE ERROR", r)
        } else if let Some(r) = line.strip_prefix("PARSE ") {
            ("PARSE", r)
        } else if let Some(r) = line.strip_prefix("EXEC ") {
            ("EXEC", r)
        } else if let Some(r) = line.strip_prefix("FETCH ") {
            ("FETCH", r)
        } else if let Some(r) = line.strip_prefix("WAIT ") {
            ("WAIT", r)
        } else if let Some(r) = line.strip_prefix("STAT ") {
            ("STAT", r)
        } else if let Some(r) = line.strip_prefix("BINDS ") {
            ("BINDS", r)
        } else if let Some(r) = line.strip_prefix("CLOSE ") {
            ("CLOSE", r)
        } else if let Some(r) = line.strip_prefix("XCTEND ") {
            let map = kv(r);
            if map.get("rlbk") == Some(&"1") {
                report.rollbacks += 1;
            } else {
                report.commits += 1;
            }
            continue;
        } else {
            if in_header && !line.trim().is_empty() && !line.starts_with("=====") {
                report.header.push(line.to_owned());
            }
            continue;
        };
        in_header = false;

        let Some((cursor, body)) = cursor_and_rest(rest) else {
            report.unparsed += 1;
            continue;
        };
        // A STAT burst ends at the first non-STAT line for that cursor.
        if kind != "STAT"
            && let Some(rows) = stat_burst.remove(cursor)
            && let Some(&idx) = cursors.get(cursor)
            && report.statements[idx].row_source.is_empty()
        {
            report.statements[idx].row_source = rows;
        }
        let stmt = cursors.get(cursor).copied();

        match kind {
            "PARSE" | "EXEC" | "FETCH" => {
                let map = kv(body);
                let call = CallStats {
                    count: 1,
                    cpu: num(&map, "c"),
                    elapsed: num(&map, "e"),
                    disk: num(&map, "p"),
                    query: num(&map, "cr"),
                    current: num(&map, "cu"),
                    rows: num(&map, "r"),
                    misses: num(&map, "mis"),
                };
                let call_kind = match kind {
                    "PARSE" => CallKind::Parse,
                    "EXEC" => CallKind::Execute,
                    _ => CallKind::Fetch,
                };
                let idx = match stmt {
                    Some(i) => i,
                    None => {
                        // Cursor opened before tracing started: unknown text.
                        let depth = num(&map, "dep") as u32;
                        let key = StatementKey {
                            text: format!("-- cursor #{cursor} (opened before tracing started)"),
                            depth,
                            user_id: None,
                        };
                        let i = statement_for(&mut report, &mut by_key, key, None);
                        cursors.insert(cursor.to_owned(), i);
                        i
                    }
                };
                let s = &mut report.statements[idx];
                s.calls[call_kind as usize].add(&call);
                if s.optimizer_goal.is_none() {
                    s.optimizer_goal = map.get("og").and_then(|og| optimizer_goal(og));
                }
            }
            "PARSE ERROR" => {
                let map = kv(body);
                // The failing text follows on the next lines until a blank line.
                let mut text = String::new();
                while let Some(l) = lines.peek() {
                    if l.trim().is_empty() || l.starts_with("=====") {
                        break;
                    }
                    if !text.is_empty() {
                        text.push('\n');
                    }
                    text.push_str(l);
                    lines.next();
                    report.lines += 1;
                }
                let depth = num(&map, "dep") as u32;
                let user_id = map.get("uid").and_then(|u| u.parse().ok());
                let key = StatementKey {
                    text,
                    depth,
                    user_id,
                };
                let i = statement_for(&mut report, &mut by_key, key, None);
                let err = map.get("err").copied().unwrap_or("?");
                report.statements[i]
                    .parse_errors
                    .push(format!("ORA-{err:0>5}"));
            }
            "WAIT" => {
                // nam='db file sequential read' ela= 123 file#=1 ...
                let Some(name) = body
                    .split_once("nam='")
                    .and_then(|(_, r)| r.split_once('\''))
                    .map(|(n, _)| n)
                else {
                    report.unparsed += 1;
                    continue;
                };
                let ela = body
                    .split_once("ela=")
                    .map(|(_, r)| r.trim_start())
                    .and_then(|r| r.split_whitespace().next())
                    .and_then(|v| v.parse::<u64>().ok())
                    .unwrap_or(0);
                let idx = match stmt {
                    Some(i) => i,
                    None => {
                        // WAIT #0 and waits on unknown cursors.
                        let key = StatementKey {
                            text: "-- waits not associated with a cursor".into(),
                            depth: 0,
                            user_id: None,
                        };
                        statement_for(&mut report, &mut by_key, key, None)
                    }
                };
                report.statements[idx].wait(name, ela);
            }
            "STAT" => {
                // id=1 cnt=1 pid=0 pos=1 obj=0 op='SORT AGGREGATE (cr=3 ...)'
                let Some((attrs, op)) = body.split_once("op='") else {
                    report.unparsed += 1;
                    continue;
                };
                let op = op.strip_suffix('\'').unwrap_or(op);
                let map: HashMap<&str, &str> = attrs
                    .split_whitespace()
                    .filter_map(|p| p.split_once('='))
                    .collect();
                let id = map.get("id").and_then(|v| v.parse().ok()).unwrap_or(0);
                let parent = map.get("pid").and_then(|v| v.parse().ok()).unwrap_or(0);
                let burst = stat_burst.entry(cursor.to_owned()).or_default();
                let depth = burst
                    .iter()
                    .find(|r| r.id == parent)
                    .map_or(0, |p| p.depth + 1);
                burst.push(RowSource {
                    id,
                    parent,
                    depth,
                    rows: map.get("cnt").and_then(|v| v.parse().ok()).unwrap_or(0),
                    object_id: map.get("obj").and_then(|v| v.parse().ok()).unwrap_or(0),
                    operation: op.to_owned(),
                });
            }
            "BINDS" => {
                let mut values = Vec::new();
                while let Some(l) = lines.peek() {
                    if !(l.trim().is_empty() || l.starts_with(' ')) {
                        break;
                    }
                    if let Some(v) = l.trim().strip_prefix("value=") {
                        values.push(v.to_owned());
                    } else if l.trim() == "No oacdef for this bind." {
                        values.push("(no value)".into());
                    }
                    lines.next();
                    report.lines += 1;
                }
                if let Some(i) = stmt
                    && report.statements[i].binds.is_empty()
                {
                    report.statements[i].binds = values;
                }
            }
            "CLOSE" => {
                // Cursor numbers are reused after close.
                cursors.remove(cursor);
            }
            _ => {}
        }
    }
    // Flush STAT bursts at the end of the file.
    for (cursor, rows) in stat_burst {
        if let Some(&idx) = cursors.get(&cursor)
            && report.statements[idx].row_source.is_empty()
        {
            report.statements[idx].row_source = rows;
        }
    }

    for s in &report.statements {
        let totals = if s.is_recursive() {
            &mut report.recursive
        } else {
            &mut report.non_recursive
        };
        totals.statements += 1;
        for k in CallKind::ALL {
            totals.calls[k as usize].add(s.call(k));
        }
        for (name, w) in &s.waits {
            match totals.waits.iter_mut().find(|(n, _)| n == name) {
                Some((_, t)) => {
                    t.count += w.count;
                    t.total += w.total;
                    t.max = t.max.max(w.max);
                }
                None => totals.waits.push((name.clone(), w.clone())),
            }
        }
    }
    for t in [&mut report.non_recursive, &mut report.recursive] {
        t.waits.sort_by_key(|w| std::cmp::Reverse(w.1.total));
    }
    report
}

fn secs(us: u64) -> String {
    format!("{:.2}", us as f64 / 1e6)
}

fn call_table(out: &mut String, calls: &[CallStats; 3]) {
    let _ = writeln!(
        out,
        "call     count       cpu    elapsed       disk      query    current        rows"
    );
    let _ = writeln!(
        out,
        "------- ------  -------- ---------- ---------- ---------- ----------  ----------"
    );
    let mut total = CallStats::default();
    for k in CallKind::ALL {
        let c = &calls[k as usize];
        total.add(c);
        let _ = writeln!(
            out,
            "{:<7} {:>6}  {:>8} {:>10} {:>10} {:>10} {:>10}  {:>10}",
            k.label(),
            c.count,
            secs(c.cpu),
            secs(c.elapsed),
            c.disk,
            c.query,
            c.current,
            c.rows
        );
    }
    let _ = writeln!(
        out,
        "------- ------  -------- ---------- ---------- ---------- ----------  ----------"
    );
    let _ = writeln!(
        out,
        "{:<7} {:>6}  {:>8} {:>10} {:>10} {:>10} {:>10}  {:>10}",
        "total",
        total.count,
        secs(total.cpu),
        secs(total.elapsed),
        total.disk,
        total.query,
        total.current,
        total.rows
    );
}

fn wait_table(out: &mut String, waits: &[(String, WaitStats)]) {
    if waits.is_empty() {
        return;
    }
    let _ = writeln!(out, "\nElapsed times include waiting on following events:");
    let _ = writeln!(
        out,
        "  Event waited on                             Times   Max. Wait  Total Waited"
    );
    let _ = writeln!(
        out,
        "  ----------------------------------------   Waited  ----------  ------------"
    );
    for (name, w) in waits {
        let _ = writeln!(
            out,
            "  {:<40}  {:>7}  {:>10}  {:>12}",
            name,
            w.count,
            secs(w.max),
            secs(w.total)
        );
    }
}

impl Report {
    /// Renders a text report in the familiar tkprof layout.
    pub fn to_text(&self, sort: SortKey) -> String {
        let mut out = String::new();
        let _ = writeln!(
            out,
            "Pythia trace analysis (tkprof style), sort = {}",
            sort.label()
        );
        for h in self.header.iter().take(12) {
            let _ = writeln!(out, "{h}");
        }
        let _ = writeln!(
            out,
            "\ncount    = number of times OCI procedure was executed"
        );
        let _ = writeln!(out, "cpu      = cpu time in seconds executing");
        let _ = writeln!(out, "elapsed  = elapsed time in seconds executing");
        let _ = writeln!(
            out,
            "disk     = number of physical reads of buffers from disk"
        );
        let _ = writeln!(
            out,
            "query    = number of buffers gotten for consistent read"
        );
        let _ = writeln!(
            out,
            "current  = number of buffers gotten in current mode (usually for update)"
        );
        let _ = writeln!(
            out,
            "rows     = number of rows processed by the fetch or execute call"
        );
        for i in self.sorted(sort) {
            let _ = writeln!(out, "{}", "*".repeat(80));
            out.push_str(&self.statements[i].to_text());
        }
        for (title, t) in [
            (
                "OVERALL TOTALS FOR ALL NON RECURSIVE STATEMENTS",
                &self.non_recursive,
            ),
            (
                "OVERALL TOTALS FOR ALL RECURSIVE STATEMENTS",
                &self.recursive,
            ),
        ] {
            let _ = writeln!(out, "{}\n\n{title}\n", "*".repeat(80));
            call_table(&mut out, &t.calls);
            wait_table(&mut out, &t.waits);
            out.push('\n');
        }
        let _ = writeln!(
            out,
            "{} user SQL statements, {} internal SQL statements, {} commits, {} rollbacks, {} trace lines",
            self.non_recursive.statements,
            self.recursive.statements,
            self.commits,
            self.rollbacks,
            self.lines
        );
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TRACE: &str = "\
Trace file /opt/oracle/diag/rdbms/free/FREE/trace/FREE_ora_1234.trc
Oracle Database 23ai Free Release 23.0.0.0.0
*** SESSION ID:(45.1234) 2026-10-21T18:00:00.000000+00:00

=====================
PARSING IN CURSOR #140001 len=36 dep=0 uid=105 oct=3 lid=105 tim=100 hv=1 ad='a' sqlid='abc123'
select count(*)
  from emp where deptno = :b1
END OF STMT
PARSE #140001:c=1000,e=1500,p=0,cr=0,cu=0,mis=1,r=0,dep=0,og=1,plh=0,tim=101
BINDS #140001:

 Bind#0
  oacdty=02 mxl=22(22) mxlc=00 mal=00 scl=00 pre=00
  value=10
EXEC #140001:c=200,e=250,p=0,cr=0,cu=0,mis=0,r=0,dep=0,og=1,plh=123,tim=102
WAIT #140001: nam='SQL*Net message to client' ela= 3 driver id=1650815232 #bytes=1 p3=0 obj#=-1 tim=103
FETCH #140001:c=500,e=700,p=2,cr=7,cu=0,mis=0,r=1,dep=0,og=1,plh=123,tim=104
WAIT #140001: nam='db file sequential read' ela= 120 file#=1 block#=2 blocks=1 obj#=73 tim=105
WAIT #140001: nam='db file sequential read' ela= 80 file#=1 block#=3 blocks=1 obj#=73 tim=106
STAT #140001 id=1 cnt=1 pid=0 pos=1 obj=0 op='SORT AGGREGATE (cr=7 pr=2 pw=0 str=1 time=690 us)'
STAT #140001 id=2 cnt=3 pid=1 pos=1 obj=73 op='TABLE ACCESS FULL EMP (cr=7 pr=2 pw=0 str=1 time=600 us cost=3 size=9 card=3)'
FETCH #140001:c=0,e=5,p=0,cr=0,cu=0,mis=0,r=0,dep=0,og=1,plh=123,tim=107
CLOSE #140001:c=0,e=4,dep=0,type=0,tim=108
=====================
PARSING IN CURSOR #140002 len=40 dep=1 uid=0 oct=3 lid=0 tim=110 hv=2 ad='b' sqlid='rec1'
select obj# from obj$ where name = :1
END OF STMT
PARSE #140002:c=10,e=20,p=0,cr=0,cu=0,mis=0,r=0,dep=1,og=4,plh=0,tim=111
EXEC #140002:c=10,e=20,p=0,cr=0,cu=0,mis=0,r=0,dep=1,og=4,plh=0,tim=112
FETCH #140002:c=10,e=20,p=0,cr=3,cu=0,mis=0,r=1,dep=1,og=4,plh=0,tim=113
CLOSE #140002:c=0,e=1,dep=1,type=3,tim=114
=====================
PARSING IN CURSOR #140001 len=36 dep=0 uid=105 oct=3 lid=105 tim=200 hv=1 ad='a' sqlid='abc123'
select count(*)
  from emp where deptno = :b1
END OF STMT
PARSE #140001:c=0,e=10,p=0,cr=0,cu=0,mis=0,r=0,dep=0,og=1,plh=0,tim=201
EXEC #140001:c=100,e=100,p=0,cr=0,cu=0,mis=0,r=0,dep=0,og=1,plh=123,tim=202
FETCH #140001:c=100,e=100,p=0,cr=7,cu=0,mis=0,r=1,dep=0,og=1,plh=123,tim=203
WAIT #0: nam='SQL*Net message from client' ela= 5000 driver id=1 #bytes=1 p3=0 obj#=-1 tim=204
XCTEND rlbk=0, rd_only=1, tim=205
PARSE ERROR #140003:len=22,dep=0,uid=105,oct=3,lid=105,tim=206,err=942
select * from nothere

EXEC #999:c=5,e=6,p=0,cr=0,cu=0,mis=0,r=0,dep=0,og=1,plh=0,tim=207
";

    #[test]
    fn aggregates_statements() {
        let r = parse(TRACE);
        assert_eq!(r.header.len(), 3, "{:?}", r.header);
        let main = r
            .statements
            .iter()
            .find(|s| s.sql_id.as_deref() == Some("abc123"))
            .unwrap();
        assert_eq!(main.text, "select count(*)\n  from emp where deptno = :b1");
        assert_eq!(main.call(CallKind::Parse).count, 2);
        assert_eq!(main.call(CallKind::Parse).misses, 1);
        assert_eq!(main.call(CallKind::Execute).count, 2);
        assert_eq!(main.call(CallKind::Fetch).count, 3);
        assert_eq!(main.call(CallKind::Fetch).rows, 2);
        assert_eq!(main.call(CallKind::Fetch).disk, 2);
        assert_eq!(main.call(CallKind::Fetch).query, 14);
        assert_eq!(main.total().elapsed, 1500 + 250 + 700 + 5 + 10 + 100 + 100);
        assert_eq!(main.optimizer_goal.as_deref(), Some("ALL_ROWS"));
        assert_eq!(main.binds, ["10"]);
        assert_eq!(main.user_id, Some(105));
    }

    #[test]
    fn waits_and_row_source() {
        let r = parse(TRACE);
        let main = &r.statements[0];
        let (name, w) = main
            .waits
            .iter()
            .find(|(n, _)| n == "db file sequential read")
            .unwrap();
        assert_eq!(
            (name.as_str(), w.count, w.total, w.max),
            ("db file sequential read", 2, 200, 120)
        );
        assert_eq!(main.row_source.len(), 2);
        assert_eq!(main.row_source[1].depth, 1);
        assert_eq!(main.row_source[1].rows, 3);
        assert!(
            main.row_source[1]
                .operation
                .starts_with("TABLE ACCESS FULL EMP")
        );
        // WAIT #0 goes to a synthetic entry.
        assert!(
            r.statements
                .iter()
                .any(|s| s.text.contains("not associated") && s.wait_total() == 5000)
        );
    }

    #[test]
    fn totals_and_misc() {
        let r = parse(TRACE);
        assert_eq!(r.recursive.statements, 1);
        assert_eq!(r.recursive.calls[CallKind::Fetch as usize].query, 3);
        assert_eq!(r.commits, 1);
        assert_eq!(r.rollbacks, 0);
        let err = r
            .statements
            .iter()
            .find(|s| !s.parse_errors.is_empty())
            .unwrap();
        assert_eq!(err.text, "select * from nothere");
        assert_eq!(err.parse_errors, ["ORA-00942"]);
        // Unknown cursor #999 becomes a placeholder statement.
        assert!(r.statements.iter().any(|s| s.text.contains("#999")));
        assert_eq!(r.unparsed, 0);
    }

    #[test]
    fn sorting_and_text() {
        let r = parse(TRACE);
        let order = r.sorted(SortKey::TotalElapsed);
        assert_eq!(r.statements[order[0]].sql_id.as_deref(), Some("abc123"));
        let order = r.sorted(SortKey::Waits);
        assert!(r.statements[order[0]].text.contains("not associated"));
        let text = r.to_text(SortKey::FetchElapsed);
        assert!(text.contains("OVERALL TOTALS FOR ALL NON RECURSIVE STATEMENTS"));
        assert!(text.contains("TABLE ACCESS FULL EMP"));
        assert!(text.contains("db file sequential read"));
    }

    #[test]
    fn never_panics_on_garbage() {
        let samples = [
            TRACE,
            "PARSING IN CURSOR",
            "PARSE #",
            "WAIT #1: nam='x",
            "STAT #1 id=",
            "EXEC #12x:c=a",
            "BINDS #1:\n  value=",
        ];
        for s in samples {
            for end in 0..=s.len() {
                if s.is_char_boundary(end) {
                    let _ = parse(&s[..end]);
                }
            }
        }
    }
}
