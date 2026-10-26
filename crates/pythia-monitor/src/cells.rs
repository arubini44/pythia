//! Reading the text cells of [`pythia_db::Client`] rows, and counting, for
//! the dashboards that read through a client (MySQL, PostgreSQL, SQL
//! Server).

use std::collections::HashMap;

use pythia_db::Cell;

pub(crate) fn opt(row: &[Cell], i: usize) -> Option<String> {
    row.get(i).cloned().flatten()
}

pub(crate) fn text(row: &[Cell], i: usize) -> String {
    opt(row, i).unwrap_or_default()
}

pub(crate) fn num(row: &[Cell], i: usize) -> f64 {
    row.get(i)
        .and_then(|c| c.as_deref()?.parse().ok())
        .unwrap_or(0.0)
}

pub(crate) fn int(row: &[Cell], i: usize) -> i64 {
    num(row, i) as i64
}

pub(crate) fn plural(n: i64, one: &str) -> String {
    if n == 1 {
        format!("1 {one}")
    } else {
        format!("{n} {one}s")
    }
}

/// Counts of each value, most frequent first, at most 8.
pub(crate) fn top_counts(values: impl Iterator<Item = String>) -> Vec<(String, i64)> {
    let mut counts = all_counts(values);
    counts.truncate(8);
    counts
}

pub(crate) fn all_counts(values: impl Iterator<Item = String>) -> Vec<(String, i64)> {
    let mut counts: HashMap<String, i64> = HashMap::new();
    for v in values {
        *counts.entry(v).or_default() += 1;
    }
    let mut counts: Vec<(String, i64)> = counts.into_iter().collect();
    counts.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    counts
}
