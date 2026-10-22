//! Reading answers: models reply in Markdown with code in fenced blocks.

/// A piece of an answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Block<'a> {
    /// Prose between code blocks.
    Text(&'a str),
    /// A fenced code block and its language tag (may be empty).
    Code { lang: &'a str, code: &'a str },
}

/// Splits an answer into prose and fenced code blocks (```). An unclosed
/// block runs to the end.
pub fn blocks(answer: &str) -> Vec<Block<'_>> {
    let mut out = Vec::new();
    let mut text_start = 0;
    let mut pos = 0;
    let mut open: Option<(&str, usize)> = None;
    while pos < answer.len() {
        let end = answer[pos..].find('\n').map_or(answer.len(), |i| pos + i);
        let line = &answer[pos..end];
        let next = (end + 1).min(answer.len());
        if let Some(fence) = line.trim_start().strip_prefix("```") {
            match open {
                None => {
                    push_text(&mut out, &answer[text_start..pos]);
                    open = Some((fence.trim(), next));
                }
                Some((lang, start)) => {
                    let code = answer[start..pos].trim_end_matches(['\n', '\r']);
                    out.push(Block::Code { lang, code });
                    open = None;
                    text_start = next;
                }
            }
        }
        pos = if end == answer.len() { end } else { next };
    }
    match open {
        Some((lang, start)) => {
            let code = answer[start.min(answer.len())..].trim_end_matches(['\n', '\r']);
            out.push(Block::Code { lang, code });
        }
        None => push_text(&mut out, &answer[text_start.min(answer.len())..]),
    }
    out
}

fn push_text<'a>(out: &mut Vec<Block<'a>>, text: &'a str) {
    let text = text.trim_matches(['\n', '\r']);
    if !text.trim().is_empty() {
        out.push(Block::Text(text));
    }
}

/// A run of inline text with its Markdown emphasis.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Span<'a> {
    pub text: &'a str,
    /// `**bold**`
    pub strong: bool,
    /// `` `code` ``
    pub code: bool,
}

/// Splits a line at `**bold**` and `` `code` `` markers.
pub fn spans(line: &str) -> Vec<Span<'_>> {
    let mut out = Vec::new();
    let mut rest = line;
    let mut strong = false;
    while !rest.is_empty() {
        let tick = rest.find('`');
        let stars = rest.find("**");
        match (tick, stars) {
            (Some(t), s) if s.is_none_or(|s| t < s) => {
                let Some(close) = rest[t + 1..].find('`') else {
                    push_span(&mut out, rest, strong, false);
                    break;
                };
                push_span(&mut out, &rest[..t], strong, false);
                push_span(&mut out, &rest[t + 1..t + 1 + close], strong, true);
                rest = &rest[t + 2 + close..];
            }
            (_, Some(s)) => {
                push_span(&mut out, &rest[..s], strong, false);
                strong = !strong;
                rest = &rest[s + 2..];
            }
            _ => {
                push_span(&mut out, rest, strong, false);
                break;
            }
        }
    }
    out
}

fn push_span<'a>(out: &mut Vec<Span<'a>>, text: &'a str, strong: bool, code: bool) {
    if !text.is_empty() {
        out.push(Span { text, strong, code });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prose_and_code() {
        let answer =
            "Use a join:\n\n```sql\nselect *\n  from emp\n```\nThat's it.\n```\nplain\n```";
        assert_eq!(
            blocks(answer),
            [
                Block::Text("Use a join:"),
                Block::Code {
                    lang: "sql",
                    code: "select *\n  from emp"
                },
                Block::Text("That's it."),
                Block::Code {
                    lang: "",
                    code: "plain"
                },
            ]
        );
    }

    #[test]
    fn unclosed_and_empty() {
        assert_eq!(
            blocks("Here:\n```sql\nselect 1 from dual\n"),
            [
                Block::Text("Here:"),
                Block::Code {
                    lang: "sql",
                    code: "select 1 from dual"
                }
            ]
        );
        assert_eq!(blocks(""), []);
        assert_eq!(
            blocks("```sql\n```"),
            [Block::Code {
                lang: "sql",
                code: ""
            }]
        );
        assert_eq!(blocks("no code"), [Block::Text("no code")]);
    }

    #[test]
    fn inline_emphasis() {
        let s = spans("A **full scan** of `EMP` and **`x`** end");
        let parts: Vec<(&str, bool, bool)> = s.iter().map(|s| (s.text, s.strong, s.code)).collect();
        assert_eq!(
            parts,
            [
                ("A ", false, false),
                ("full scan", true, false),
                (" of ", false, false),
                ("EMP", false, true),
                (" and ", false, false),
                ("x", true, true),
                (" end", false, false),
            ]
        );
        assert_eq!(spans("a ` b").len(), 1);
    }
}
