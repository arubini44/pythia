//! PL/SQL + SQL lexer.
//!
//! The lexer is lossless: every byte of the input belongs to exactly one token,
//! so it can drive syntax highlighting as well as statement splitting. Errors
//! (unterminated constructs, stray characters) never abort lexing.

use std::ops::Range;

use logos::Logos;

use crate::keywords::is_keyword;

#[derive(Logos, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TokenKind {
    #[regex(r"[ \t\r\f]+")]
    Whitespace,
    #[token("\n")]
    Newline,
    #[regex(r"--[^\n]*", allow_greedy = true)]
    LineComment,
    #[token("/*", block_comment)]
    BlockComment,
    /// Optimizer hint `/*+ ... */`.
    #[token("/*+", block_comment)]
    Hint,
    /// `'...'` and `N'...'` string literals.
    #[token("'", string)]
    #[regex("[nN]'", string)]
    String,
    /// Alternative quoting `q'[...]'`, `nq'{...}'`.
    #[regex("[nN]?[qQ]'", q_string)]
    QString,
    #[regex(r"[0-9]+(\.[0-9]+)?([eE][+-]?[0-9]+)?[fFdD]?")]
    #[regex(r"\.[0-9]+([eE][+-]?[0-9]+)?[fFdD]?")]
    Number,
    /// Unquoted identifier. Keywords are reclassified by [`tokenize`].
    #[regex(r"[\p{L}][\p{L}\p{N}_$#]*")]
    Ident,
    #[regex(r#""[^"\n]*"?"#)]
    QuotedIdent,
    #[regex(r":[\p{L}][\p{L}\p{N}_$#]*")]
    #[regex(r":[0-9]+")]
    BindVar,
    /// SQL*Plus substitution variable `&var` / `&&var`.
    #[regex(r"&&?[\p{L}_][\p{L}\p{N}_$#]*\.?")]
    SubstVar,
    #[regex(r":=|=>|\|\||<=|>=|<>|!=|\^=|~=|\.\.|\*\*|<<|>>|[+\-*/=<>@%|!^~?&:]")]
    Operator,
    #[token(";")]
    Semicolon,
    #[token(",")]
    Comma,
    #[token("(")]
    LParen,
    #[token(")")]
    RParen,
    #[token(".")]
    Dot,

    /// Reserved word (produced by [`tokenize`], never by the raw lexer).
    Keyword,
    /// Input the lexer could not make sense of.
    Error,
}

impl TokenKind {
    /// Whitespace and comments: tokens without meaning for the parser.
    pub fn is_trivia(self) -> bool {
        matches!(
            self,
            TokenKind::Whitespace
                | TokenKind::Newline
                | TokenKind::LineComment
                | TokenKind::BlockComment
        )
    }

    pub fn is_word(self) -> bool {
        matches!(self, TokenKind::Ident | TokenKind::Keyword)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    pub kind: TokenKind,
    pub span: Range<usize>,
}

/// Lexes `src` into a lossless token stream.
pub fn tokenize(src: &str) -> Vec<Token> {
    let mut lexer = TokenKind::lexer(src);
    let mut tokens = Vec::new();
    while let Some(result) = lexer.next() {
        let span = lexer.span();
        let kind = match result {
            Ok(TokenKind::Ident) if is_keyword(&src[span.clone()]) => TokenKind::Keyword,
            Ok(kind) => kind,
            Err(()) => TokenKind::Error,
        };
        // Merge consecutive error bytes into one token.
        if kind == TokenKind::Error
            && let Some(last) = tokens
                .last_mut()
                .filter(|t: &&mut Token| t.kind == TokenKind::Error && t.span.end == span.start)
        {
            last.span.end = span.end;
            continue;
        }
        tokens.push(Token { kind, span });
    }
    tokens
}

/// Consumes a block comment body up to and including `*/` (or end of input).
fn block_comment(lex: &mut logos::Lexer<TokenKind>) {
    let rest = lex.remainder();
    let len = rest.find("*/").map_or(rest.len(), |i| i + 2);
    lex.bump(len);
}

/// Consumes a string body after the opening quote, honouring `''` escapes.
fn string(lex: &mut logos::Lexer<TokenKind>) {
    let rest = lex.remainder().as_bytes();
    let mut i = 0;
    while i < rest.len() {
        if rest[i] == b'\'' {
            if rest.get(i + 1) == Some(&b'\'') {
                i += 2;
                continue;
            }
            lex.bump(i + 1);
            return;
        }
        i += 1;
    }
    lex.bump(rest.len());
}

/// Consumes a `q'<delim>...<delim>'` body after the `q'` prefix.
fn q_string(lex: &mut logos::Lexer<TokenKind>) {
    let rest = lex.remainder();
    let Some(open) = rest.chars().next() else {
        return;
    };
    let close = match open {
        '[' => ']',
        '{' => '}',
        '(' => ')',
        '<' => '>',
        c => c,
    };
    let body = &rest[open.len_utf8()..];
    let mut terminator = [0u8; 5];
    let terminator = {
        let n = close.encode_utf8(&mut terminator).len();
        terminator[n] = b'\'';
        std::str::from_utf8(&terminator[..=n]).expect("valid utf8")
    };
    let len = body
        .find(terminator)
        .map_or(rest.len(), |i| open.len_utf8() + i + terminator.len());
    lex.bump(len);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(src: &str) -> Vec<(TokenKind, &str)> {
        tokenize(src)
            .into_iter()
            .filter(|t| !t.kind.is_trivia())
            .map(|t| (t.kind, &src[t.span]))
            .collect()
    }

    #[test]
    fn lossless() {
        let src = "select /*+ full(t) */ t.* from \"My Tab\" t -- c\nwhere x = :b1 and y = &&v. || q'[it's]' ; §";
        let tokens = tokenize(src);
        let mut pos = 0;
        for t in &tokens {
            assert_eq!(t.span.start, pos);
            pos = t.span.end;
        }
        assert_eq!(pos, src.len());
    }

    #[test]
    fn literals_and_comments() {
        insta::assert_debug_snapshot!(kinds(
            "SELECT 'it''s', q'{a}'' b}', nq'!x!', N'n', 1.5e3, .5, 1..10 FROM dual /* c */ /*+ hint */"
        ));
    }

    #[test]
    fn binds_and_operators() {
        insta::assert_debug_snapshot!(kinds("x := :new.id || &var; y => a <> b"));
    }

    #[test]
    fn unterminated() {
        assert_eq!(kinds("'abc"), vec![(TokenKind::String, "'abc")]);
        assert_eq!(
            tokenize("/* abc"),
            vec![Token {
                kind: TokenKind::BlockComment,
                span: 0..6
            }]
        );
        assert_eq!(kinds("q'[abc"), vec![(TokenKind::QString, "q'[abc")]);
    }
}
