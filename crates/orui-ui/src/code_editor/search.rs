//! Find / replace.

use std::ops::Range;

use regex::{Regex, RegexBuilder};

use super::text::is_word_char;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SearchQuery {
    pub text: String,
    pub case_sensitive: bool,
    pub whole_word: bool,
    pub regex: bool,
}

/// A compiled query.
pub struct Matcher {
    regex: Regex,
    whole_word: bool,
    is_regex: bool,
}

impl Matcher {
    /// Returns `None` for an empty query, `Err` for an invalid regex.
    pub fn new(query: &SearchQuery) -> Option<Result<Self, regex::Error>> {
        if query.text.is_empty() {
            return None;
        }
        let pattern = if query.regex {
            query.text.clone()
        } else {
            regex::escape(&query.text)
        };
        let regex = RegexBuilder::new(&pattern)
            .case_insensitive(!query.case_sensitive)
            .multi_line(true)
            .build();
        Some(regex.map(|regex| Self {
            regex,
            whole_word: query.whole_word,
            is_regex: query.regex,
        }))
    }

    /// Byte ranges of all non-empty matches within `scope` of `haystack`.
    pub fn find_all(&self, haystack: &str, scope: Option<Range<usize>>) -> Vec<Range<usize>> {
        let scope = scope.unwrap_or(0..haystack.len());
        let slice = &haystack[scope.clone()];
        self.regex
            .find_iter(slice)
            .map(|m| m.start() + scope.start..m.end() + scope.start)
            .filter(|r| !r.is_empty())
            .filter(|r| !self.whole_word || is_whole_word(haystack, r))
            .collect()
    }

    /// The replacement text for the match at `range` (expands `$1` in regex mode).
    pub fn replacement(&self, haystack: &str, range: &Range<usize>, replace: &str) -> String {
        if !self.is_regex {
            return replace.to_owned();
        }
        // Re-run the regex anchored at the match to get its captures. Use the
        // whole haystack so look-around-like anchors (`^`, `$`) stay right.
        match self.regex.captures_at(haystack, range.start) {
            Some(caps) if caps.get(0).is_some_and(|m| m.start() == range.start) => {
                let mut out = String::new();
                caps.expand(replace, &mut out);
                out
            }
            _ => replace.to_owned(),
        }
    }
}

fn is_whole_word(haystack: &str, r: &Range<usize>) -> bool {
    let before = haystack[..r.start].chars().next_back();
    let after = haystack[r.end..].chars().next();
    !before.is_some_and(is_word_char) && !after.is_some_and(is_word_char)
}

/// Index of the first match at or after `pos` (wrapping), or the last match
/// before `pos` when searching backwards.
pub fn next_match(matches: &[Range<usize>], pos: usize, forward: bool) -> Option<usize> {
    if matches.is_empty() {
        return None;
    }
    if forward {
        Some(matches.iter().position(|m| m.start >= pos).unwrap_or(0))
    } else {
        Some(
            matches
                .iter()
                .rposition(|m| m.end <= pos)
                .unwrap_or(matches.len() - 1),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q(text: &str) -> SearchQuery {
        SearchQuery {
            text: text.into(),
            ..Default::default()
        }
    }

    #[test]
    fn literal_case_and_word() {
        let hay = "Emp emp employees EMP";
        let m = Matcher::new(&q("emp")).unwrap().unwrap();
        assert_eq!(m.find_all(hay, None).len(), 4);
        let m = Matcher::new(&SearchQuery {
            case_sensitive: true,
            ..q("emp")
        })
        .unwrap()
        .unwrap();
        assert_eq!(m.find_all(hay, None), vec![4..7, 8..11]);
        let m = Matcher::new(&SearchQuery {
            whole_word: true,
            ..q("emp")
        })
        .unwrap()
        .unwrap();
        assert_eq!(m.find_all(hay, None), vec![0..3, 4..7, 18..21]);
        assert_eq!(m.find_all(hay, Some(2..12)), vec![4..7]);
        let m = Matcher::new(&q("a.b")).unwrap().unwrap();
        assert!(m.find_all("axb", None).is_empty());
    }

    #[test]
    fn regex_and_replacement() {
        let hay = "x := 1;\ny := 22;";
        let query = SearchQuery {
            regex: true,
            ..q(r"(\w) := (\d+)")
        };
        let m = Matcher::new(&query).unwrap().unwrap();
        let found = m.find_all(hay, None);
        assert_eq!(found, vec![0..6, 8..15]);
        assert_eq!(m.replacement(hay, &found[1], "$2 => $1"), "22 => y");
        assert!(
            Matcher::new(&SearchQuery {
                regex: true,
                ..q("(")
            })
            .unwrap()
            .is_err()
        );
        assert!(Matcher::new(&q("")).is_none());
        let line_start = Matcher::new(&SearchQuery {
            regex: true,
            ..q("^y")
        })
        .unwrap()
        .unwrap();
        assert_eq!(line_start.find_all(hay, None), vec![8..9]);
    }

    #[test]
    fn navigation() {
        let m = vec![2..4, 10..12, 20..22];
        assert_eq!(next_match(&m, 5, true), Some(1));
        assert_eq!(next_match(&m, 25, true), Some(0));
        assert_eq!(next_match(&m, 10, false), Some(0));
        assert_eq!(next_match(&m, 1, false), Some(2));
    }
}
