//! Statement history, persisted per connection.

use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

const MAX_ENTRIES: usize = 2000;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entry {
    pub connection: String,
    pub sql: String,
    /// Unix seconds of the last execution.
    pub at: u64,
    pub count: u32,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct History {
    #[serde(rename = "entry")]
    pub entries: Vec<Entry>,
    #[serde(skip)]
    pub dirty: bool,
}

impl History {
    /// Records an execution; re-running a statement moves it to the top.
    pub fn add(&mut self, connection: &str, sql: &str) {
        let sql = sql.trim();
        if sql.is_empty() {
            return;
        }
        let at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        let count = match self
            .entries
            .iter()
            .position(|e| e.connection == connection && e.sql == sql)
        {
            Some(i) => self.entries.remove(i).count + 1,
            None => 1,
        };
        self.entries.push(Entry {
            connection: connection.to_owned(),
            sql: sql.to_owned(),
            at,
            count,
        });
        if self.entries.len() > MAX_ENTRIES {
            let excess = self.entries.len() - MAX_ENTRIES;
            self.entries.drain(..excess);
        }
        self.dirty = true;
    }

    /// Newest first, optionally limited to one connection and a filter text.
    pub fn search<'a>(
        &'a self,
        connection: Option<&'a str>,
        filter: &'a str,
    ) -> impl Iterator<Item = &'a Entry> {
        let filter = filter.to_lowercase();
        self.entries.iter().rev().filter(move |e| {
            connection.is_none_or(|c| e.connection == c)
                && (filter.is_empty() || e.sql.to_lowercase().contains(&filter))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dedupes_and_orders() {
        let mut h = History::default();
        h.add("a", "select 1 from dual");
        h.add("a", "select 2 from dual");
        h.add("a", " select 1 from dual ");
        h.add("b", "select 3 from dual");
        let a: Vec<_> = h
            .search(Some("a"), "")
            .map(|e| (e.sql.as_str(), e.count))
            .collect();
        assert_eq!(a, [("select 1 from dual", 2), ("select 2 from dual", 1)]);
        assert_eq!(h.search(None, "3").count(), 1);
    }
}
