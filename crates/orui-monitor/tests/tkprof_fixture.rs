//! Parses a real (sanitized) 10046 trace from Oracle Free 23ai.

use orui_monitor::tkprof::{self, CallKind, SortKey};

const TRACE: &str = include_str!("fixtures/free23_10046.trc");

#[test]
fn real_trace() {
    let report = tkprof::parse(TRACE);
    assert_eq!(report.unparsed, 0);
    assert!(
        report
            .header
            .iter()
            .any(|h| h.starts_with("Oracle AI Database"))
    );

    let marker = report
        .statements
        .iter()
        .find(|s| s.text.contains("orui_trace_marker"))
        .expect("marker");
    assert_eq!(marker.depth, 0);
    // The client statement cache parses once and re-executes.
    assert_eq!(marker.call(CallKind::Parse).count, 1);
    assert_eq!(marker.call(CallKind::Execute).count, 3);
    assert_eq!(marker.call(CallKind::Fetch).rows, 3);
    assert_eq!(marker.optimizer_goal.as_deref(), Some("ALL_ROWS"));
    assert_eq!(marker.binds.first().map(String::as_str), Some("1000"));
    assert!(
        marker
            .row_source
            .first()
            .is_some_and(|r| r.depth == 0 && r.operation.starts_with("SORT AGGREGATE"))
    );
    assert!(marker.row_source.iter().any(|r| r.depth > 0));

    let plsql = report
        .statements
        .iter()
        .find(|s| s.text.contains("orui_trace_plsql"))
        .expect("plsql");
    assert_eq!(plsql.call(CallKind::Execute).count, 1);

    // Dictionary access by ALL_OBJECTS shows up as recursive SQL, if any was parsed.
    assert_eq!(
        report.non_recursive.statements + report.recursive.statements,
        report.statements.len()
    );
    let nr = report.non_recursive.total();
    assert!(nr.elapsed >= marker.total().elapsed);

    let text = report.to_text(SortKey::ExecuteElapsed);
    assert!(text.contains("Row Source Operation"));
    assert!(text.contains("OVERALL TOTALS FOR ALL NON RECURSIVE STATEMENTS"));
}
