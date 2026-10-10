//! 検索言語（docs/search-query.md）：字句解析 → 構文解析 → AST → SQL。

pub mod ast;
pub mod compile;
mod fields;
mod lexer;
pub mod parser;
pub mod suggest;

#[cfg(test)]
mod compile_tests;
#[cfg(test)]
mod parser_tests;

pub use ast::Expr;
pub use compile::{DbResolver, QueryContext, Resolver, SqlCond, compile};
pub use parser::{Clause, ParseError, Parsed, parse};

use rusqlite::Connection;

use crate::Result;
use crate::items::{ItemQuery, ItemView, KindCounts, count_kinds};

/// パレットに出す解析の結果（エラー・チップ・件数）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueryCheck {
    pub error: Option<ParseError>,
    pub clauses: Vec<Clause>,
    /// 一致する件数（解析できたときだけ）。
    pub counts: Option<KindCounts>,
}

/// 入力を解析し、開いている一覧の中で一致する件数を数える。
pub fn check(
    conn: &Connection,
    text: &str,
    view: ItemView,
    ctx: &QueryContext,
) -> Result<QueryCheck> {
    match parse(text) {
        Err(error) => Ok(QueryCheck {
            error: Some(error),
            clauses: Vec::new(),
            counts: None,
        }),
        Ok(parsed) => {
            let query = ItemQuery {
                view,
                filter: Some(parsed.expr),
                ..ItemQuery::default()
            };
            Ok(QueryCheck {
                error: None,
                clauses: parsed.clauses,
                counts: Some(count_kinds(conn, &query, ctx)?),
            })
        }
    }
}
