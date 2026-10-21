//! PL/SQL and SQL language support: lexing and statement splitting.
//!
//! The parser, AST, outline and formatter will live here as well (see
//! CONCEPT.md, milestones M2/M4).

mod keywords;
mod lexer;
mod splitter;

pub use keywords::is_keyword;
pub use lexer::{Token, TokenKind, tokenize};
pub use splitter::{Statement, StatementKind, split_statements, statement_at};
