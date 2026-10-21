//! PL/SQL and SQL language support: lexing, statement splitting and an
//! error-tolerant parser for navigation, outline and completion.

pub mod ast;
mod header;
mod keywords;
mod lexer;
mod parser;
mod splitter;

pub use header::{UnitHeader, unit_header};
pub use keywords::is_keyword;
pub use lexer::{Token, TokenKind, tokenize};
pub use parser::{normalize_ident, parse, parse_tokens};
pub use splitter::{Statement, StatementKind, split_statements, statement_at};
