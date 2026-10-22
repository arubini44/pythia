//! Learning formatter options from existing code: whatever the code shows
//! clearly (case of keywords, indentation, comma style, alignment, …)
//! replaces the base options; the rest stays as it was.

use super::{Case, CommaStyle, FormatOptions, Wrap};
use crate::lexer::{TokenKind, tokenize};

/// Least number of observations before a choice counts.
const MIN_EVIDENCE: usize = 2;

/// The options `src` is written in, starting from `base`.
pub fn infer(src: &str, base: &FormatOptions) -> FormatOptions {
    let mut o = base.clone();
    let words = Words::of(src);
    if let Some(case) = words.keyword_case() {
        o.keyword_case = case;
    }
    if let Some(case) = words.identifier_case() {
        o.identifier_case = case;
    }
    let lines: Vec<&str> = src.lines().collect();
    if let Some(indent) = indent_step(&lines) {
        o.indent = indent;
    }
    if let Some(width) = line_width(&lines) {
        o.max_width = width;
    }
    if let Some(comma) = comma_style(&lines) {
        o.comma = comma;
    }
    if lines.len() >= 20 {
        o.blank_lines = usize::from(lines.iter().any(|l| l.trim().is_empty()));
    }
    if let Some(on) = majority(
        count(&lines, |l| starts_with_word(l.trim_start(), "on")),
        count(&lines, |l| {
            let l = l.to_ascii_lowercase();
            l.contains(" join ") && l.contains(" on ")
        }),
    ) {
        o.on_new_line = on;
    }
    if let Some(align) = aligned(&lines, declaration_columns) {
        o.align_declarations = align;
    }
    if let Some(align) = aligned(&lines, |l| split_at(l, ":=")) {
        o.align_assignments = align;
    }
    if let Some(align) = aligned(&lines, |l| split_at(&l.to_ascii_lowercase(), " as ")) {
        o.align_aliases = align;
    }
    let tokens = Tokens::of(src);
    let width = o.max_width;
    if let Some(wrap) = tokens.wrap_after("CASE", "WHEN", "END", width) {
        o.case_arms = wrap;
    }
    if let Some(wrap) = tokens.subquery_wrap(width) {
        o.subqueries = wrap;
    }
    if let Some(wrap) = tokens.list_wrap(&["PROCEDURE", "FUNCTION", "CURSOR"], width) {
        o.parameters = wrap;
    }
    if let Some(wrap) = tokens.list_wrap(&["INTO"], width) {
        o.insert_lists = wrap;
    }
    if let Some(own_line) = tokens.then_on_new_line() {
        o.then_on_new_line = own_line;
    }
    o
}

fn count(lines: &[&str], f: impl Fn(&str) -> bool) -> usize {
    lines.iter().filter(|l| f(l)).count()
}

fn starts_with_word(s: &str, word: &str) -> bool {
    s.len() > word.len()
        && s[..word.len()].eq_ignore_ascii_case(word)
        && s[word.len()..].starts_with(' ')
}

/// `Some(true)` when `yes` clearly outnumbers `no` (and the other way round),
/// `None` without enough evidence.
fn majority(yes: usize, no: usize) -> Option<bool> {
    if yes + no < MIN_EVIDENCE {
        None
    } else if yes > no * 2 {
        Some(true)
    } else if no > yes * 2 {
        Some(false)
    } else {
        None
    }
}

struct Words {
    keywords: [usize; 3],
    identifiers: [usize; 3],
}

impl Words {
    fn of(src: &str) -> Self {
        let mut words = Words {
            keywords: [0; 3],
            identifiers: [0; 3],
        };
        for t in tokenize(src) {
            let text = &src[t.span.clone()];
            if !text.chars().any(|c| c.is_alphabetic()) {
                continue;
            }
            let class = if text == text.to_uppercase() {
                0
            } else if text == text.to_lowercase() {
                1
            } else {
                2
            };
            match t.kind {
                TokenKind::Keyword => words.keywords[class] += 1,
                TokenKind::Ident => words.identifiers[class] += 1,
                _ => {}
            }
        }
        words
    }

    fn keyword_case(&self) -> Option<Case> {
        case_of(self.keywords)
    }

    fn identifier_case(&self) -> Option<Case> {
        case_of(self.identifiers)
    }
}

/// Upper or lower when nearly all are; as written when they're mixed.
fn case_of([upper, lower, mixed]: [usize; 3]) -> Option<Case> {
    let total = upper + lower + mixed;
    if total < 10 {
        return None;
    }
    Some(if upper * 10 >= total * 9 {
        Case::Upper
    } else if lower * 10 >= total * 9 {
        Case::Lower
    } else {
        Case::Preserve
    })
}

fn indentation(line: &str) -> usize {
    line.chars()
        .take_while(|c| *c == ' ' || *c == '\t')
        .map(|c| if c == '\t' { 4 } else { 1 })
        .sum()
}

/// The most common step by which the indentation grows.
fn indent_step(lines: &[&str]) -> Option<usize> {
    let mut steps = [0usize; 9];
    let mut previous: Option<usize> = None;
    for line in lines.iter().filter(|l| !l.trim().is_empty()) {
        let indent = indentation(line);
        if let Some(p) = previous
            && indent > p
            && indent - p <= 8
        {
            steps[indent - p] += 1;
        }
        previous = Some(indent);
    }
    let (step, n) = steps
        .iter()
        .enumerate()
        .filter(|(step, _)| matches!(step, 2 | 3 | 4 | 8))
        .max_by_key(|(_, n)| **n)?;
    (*n >= MIN_EVIDENCE).then_some(step)
}

/// About the longest lines' length, in tens: the code's line width.
fn line_width(lines: &[&str]) -> Option<usize> {
    let mut lengths: Vec<usize> = lines
        .iter()
        .filter(|l| !l.trim().is_empty())
        .map(|l| l.chars().count())
        .collect();
    if lengths.len() < 20 {
        return None;
    }
    lengths.sort_unstable();
    let p95 = lengths[lengths.len() * 95 / 100];
    Some((p95.div_ceil(10) * 10).clamp(60, 160))
}

fn comma_style(lines: &[&str]) -> Option<CommaStyle> {
    let leading = count(lines, |l| l.trim_start().starts_with(','));
    let trailing = count(lines, |l| {
        let code = l.split("--").next().unwrap_or(l);
        code.trim_end().ends_with(',')
    });
    majority(leading, trailing).map(|lead| {
        if lead {
            CommaStyle::Leading
        } else {
            CommaStyle::Trailing
        }
    })
}

/// A declaration line `name   type…;`: the name's end and where the type
/// starts.
fn declaration_columns(line: &str) -> Option<(usize, usize)> {
    let indent = indentation(line);
    let rest = line.get(indent..)?;
    let name_len = rest
        .find(|c: char| !(c.is_alphanumeric() || matches!(c, '_' | '$' | '#')))
        .filter(|&n| n > 0)?;
    let name = &rest[..name_len];
    const NOT_NAMES: &[&str] = &[
        "begin",
        "end",
        "if",
        "else",
        "elsif",
        "loop",
        "for",
        "while",
        "return",
        "when",
        "then",
        "exception",
        "is",
        "as",
        "declare",
        "select",
        "from",
        "where",
        "and",
        "or",
        "procedure",
        "function",
        "cursor",
        "type",
        "subtype",
        "pragma",
    ];
    if NOT_NAMES.iter().any(|w| name.eq_ignore_ascii_case(w)) || !line.trim_end().ends_with(';') {
        return None;
    }
    let after = &rest[name_len..];
    let gap = after.len() - after.trim_start().len();
    (gap > 0 && !after.trim_start().starts_with(":="))
        .then_some((indent + name_len, indent + name_len + gap))
}

/// `line` split around `sep`: where the text before it ends (trimmed) and
/// where `sep` starts.
fn split_at(line: &str, sep: &str) -> Option<(usize, usize)> {
    let at = line.find(sep)?;
    let before = line[..at].trim_end();
    (!before.trim().is_empty()).then_some((before.len(), at))
}

/// Whether runs of similar lines line up a column (`columns` gives a line's
/// left part end and the column's start). Only runs whose left parts differ
/// in length say anything.
fn aligned(lines: &[&str], columns: impl Fn(&str) -> Option<(usize, usize)>) -> Option<bool> {
    let (mut yes, mut no) = (0, 0);
    let mut run: Vec<(usize, usize, usize)> = Vec::new();
    let mut judge = |run: &[(usize, usize, usize)]| {
        if run.len() < 2 || run.iter().all(|r| r.1 == run[0].1) {
            return;
        }
        // Each line of a run counts.
        if run.iter().all(|r| r.2 == run[0].2) {
            yes += run.len();
        } else if run.iter().all(|r| r.2 == r.1 + 1 || r.2 == r.1) {
            no += run.len();
        }
    };
    for line in lines {
        match columns(line) {
            Some((end, column)) if run.last().is_none_or(|r| r.0 == indentation(line)) => {
                run.push((indentation(line), end, column));
            }
            Some((end, column)) => {
                judge(&run);
                run = vec![(indentation(line), end, column)];
            }
            None => {
                judge(&run);
                run.clear();
            }
        }
    }
    judge(&run);
    majority(yes, no)
}

/// Significant tokens with whether a line break comes before them.
struct Tokens<'s> {
    src: &'s str,
    toks: Vec<(String, std::ops::Range<usize>, bool)>,
}

impl<'s> Tokens<'s> {
    fn of(src: &'s str) -> Self {
        let mut toks = Vec::new();
        let mut newline = false;
        for t in tokenize(src) {
            match t.kind {
                TokenKind::Newline => newline = true,
                k if k.is_trivia() => {}
                _ => {
                    toks.push((src[t.span.clone()].to_ascii_uppercase(), t.span, newline));
                    newline = false;
                }
            }
        }
        Self { src, toks }
    }

    /// The token closing the parenthesis opened at `open`.
    fn matching(&self, open: usize) -> Option<usize> {
        let mut depth = 0usize;
        for (i, (text, ..)) in self.toks.iter().enumerate().skip(open) {
            match text.as_str() {
                "(" => depth += 1,
                ")" => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(i);
                    }
                }
                _ => {}
            }
        }
        None
    }

    /// Whether a span would fit on a line of `width` if written flat.
    fn fits(&self, from: usize, to: usize, width: usize) -> bool {
        let start = self.toks[from].1.start;
        let end = self.toks[to].1.end;
        let flat = self.src[start..end]
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        flat.len() < width.saturating_sub(10)
    }

    /// Short constructs from `open` to `close` whose `inner` word starts a
    /// line say `Always`; ones kept on one line say `Fit`.
    fn wrap_after(&self, open: &str, inner: &str, close: &str, width: usize) -> Option<Wrap> {
        let (mut broken, mut flat) = (0, 0);
        for (i, (text, ..)) in self.toks.iter().enumerate() {
            if text != open || i > 0 && self.toks[i - 1].0 == "END" {
                continue;
            }
            let Some(first) = (i + 1..self.toks.len()).find(|&j| self.toks[j].0 == inner) else {
                continue;
            };
            let Some(end) = (first..self.toks.len()).find(|&j| self.toks[j].0 == close) else {
                continue;
            };
            if !self.fits(i, end, width) {
                continue;
            }
            if self.toks[first].2 {
                broken += 1;
            } else {
                flat += 1;
            }
        }
        majority(broken, flat).map(wrap)
    }

    /// Short subqueries on their own lines say `Always`.
    fn subquery_wrap(&self, width: usize) -> Option<Wrap> {
        let (mut broken, mut flat) = (0, 0);
        for (i, (text, ..)) in self.toks.iter().enumerate() {
            if text != "(" || self.toks.get(i + 1).is_none_or(|t| t.0 != "SELECT") {
                continue;
            }
            let Some(close) = self.matching(i) else {
                continue;
            };
            if !self.fits(i, close, width) {
                continue;
            }
            if self.toks[i + 1].2 {
                broken += 1;
            } else {
                flat += 1;
            }
        }
        majority(broken, flat).map(wrap)
    }

    /// Short parenthesized lists after `<kw> name (` with their first item
    /// on its own line say `Always`.
    fn list_wrap(&self, keywords: &[&str], width: usize) -> Option<Wrap> {
        let (mut broken, mut flat) = (0, 0);
        for (i, (text, ..)) in self.toks.iter().enumerate() {
            if !keywords.contains(&text.as_str()) {
                continue;
            }
            // The name (possibly qualified), then `(`.
            let mut j = i + 1;
            while self.toks.get(j + 1).is_some_and(|t| t.0 == ".") {
                j += 2;
            }
            let open = j + 1;
            if self.toks.get(open).is_none_or(|t| t.0 != "(")
                || self.toks.get(open + 1).is_some_and(|t| t.0 == "SELECT")
            {
                continue;
            }
            let Some(close) = self.matching(open) else {
                continue;
            };
            if close == open + 1 || !self.fits(i, close, width) {
                continue;
            }
            if self.toks[open + 1].2 {
                broken += 1;
            } else {
                flat += 1;
            }
        }
        majority(broken, flat).map(wrap)
    }

    /// `THEN` / `LOOP` of `IF`, `ELSIF`, `WHILE` and `FOR` on their own line.
    fn then_on_new_line(&self) -> Option<bool> {
        let (mut own, mut same) = (0, 0);
        let mut opener: Option<&str> = None;
        for (text, _, newline) in &self.toks {
            match text.as_str() {
                "IF" | "ELSIF" | "WHILE" | "FOR" | "WHEN" | "CASE" => opener = Some(text),
                "THEN" | "LOOP" => {
                    let block = matches!(opener, Some("IF" | "ELSIF" | "WHILE" | "FOR"));
                    if block {
                        if *newline {
                            own += 1;
                        } else {
                            same += 1;
                        }
                    }
                    opener = None;
                }
                ";" => opener = None,
                _ => {}
            }
        }
        majority(own, same)
    }
}

fn wrap(broken: bool) -> Wrap {
    if broken { Wrap::Always } else { Wrap::Fit }
}

#[cfg(test)]
mod tests {
    use super::*;

    const UPPER_FOUR_LEADING: &str = "\
CREATE OR REPLACE PACKAGE BODY billing AS
    PROCEDURE charge(
        p_customer IN NUMBER
    ) IS
        l_total    NUMBER;
        l_customer NUMBER;
        l_note     VARCHAR2(100);
    BEGIN
        IF p_customer > 0
        THEN
            l_total    := 0;
            l_customer := p_customer;
        END IF;
        FOR r IN (SELECT id
                       , amount
                       , currency
                    FROM invoices
                   WHERE customer_id = p_customer)
        LOOP
            l_total := l_total + r.amount;
        END LOOP;
        INSERT INTO charges (
            customer_id
          , total
        ) VALUES (p_customer, l_total);
        INSERT INTO charge_log (
            customer_id
        ) VALUES (p_customer);
    END charge;

    FUNCTION fee(
        p_amount IN NUMBER
    ) RETURN NUMBER IS
    BEGIN
        RETURN p_amount * 0.1;
    END fee;
END billing;
";

    #[test]
    fn learns_a_style() {
        let o = infer(UPPER_FOUR_LEADING, &FormatOptions::default());
        assert_eq!(o.keyword_case, Case::Upper);
        assert_eq!(o.identifier_case, Case::Lower);
        assert_eq!(o.indent, 4);
        assert_eq!(o.comma, CommaStyle::Leading);
        assert!(o.align_declarations);
        assert!(o.align_assignments);
        assert!(o.then_on_new_line);
        assert_eq!(o.parameters, Wrap::Always);
        assert_eq!(o.insert_lists, Wrap::Always);
    }

    #[test]
    fn keeps_what_the_code_doesnt_show() {
        let base = FormatOptions {
            indent: 3,
            max_width: 77,
            ..FormatOptions::default()
        };
        let o = infer("select 1 from dual;", &base);
        assert_eq!(o, base);
    }

    #[test]
    fn unaligned_declarations() {
        let src = "declare\n  l_a number;\n  l_total number;\n  l_name varchar2(10);\n  \
                   l_x date;\nbegin\n  null;\nend;";
        assert_eq!(
            aligned(&src.lines().collect::<Vec<_>>(), declaration_columns),
            Some(false)
        );
        assert!(!infer(src, &FormatOptions::default()).align_declarations);
    }
}
