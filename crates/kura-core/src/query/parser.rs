//! 検索言語の構文解析（docs/search-query.md §2）。
//!
//! ```text
//! query      := or_expr
//! or_expr    := and_expr ( "|" and_expr )*
//! and_expr   := unary ( unary )*
//! unary      := "-" unary | atom
//! atom       := "(" query ")" | field_expr | term
//! ```
//!
//! `:` の左がフィールド名（§3）ならフィールドの条件、そうでなければ名前空間付きのタグ。

use super::ast::{Expr, Op, TagTerm};
use super::fields::{build, find_field};
use super::lexer::{Span, Tok, Token, lex};
use crate::text::nfc;

/// 解析の結果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Parsed {
    pub expr: Expr,
    /// いちばん外側の AND の 1 つずつ（パレットのチップ）。
    pub clauses: Vec<Clause>,
}

/// チップに分けた条件 1 つ。位置は入力の UTF-16 の文字位置。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Clause {
    pub start: usize,
    pub end: usize,
    /// 入力のその部分。
    pub text: String,
    /// `-` で除外しているか。
    pub negated: bool,
    /// フィールドの条件なら、その英語名（`type` など）。
    pub field: Option<&'static str>,
}

/// 解析のエラー（位置は入力の UTF-16 の文字位置）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    pub start: usize,
    pub end: usize,
    pub message: String,
}

fn error(span: Span, message: impl Into<String>) -> ParseError {
    ParseError {
        start: span.start,
        end: span.end,
        message: message.into(),
    }
}

/// 解析中の式（位置と、チップに出す情報付き）。
#[derive(Debug, Clone)]
struct Node {
    expr: Expr,
    span: Span,
    negated: bool,
    field: Option<&'static str>,
}

/// 検索の文字列を解析する。空なら全件（`Expr::And([])`）。
pub fn parse(input: &str) -> Result<Parsed, ParseError> {
    let tokens = lex(input)?;
    if tokens.is_empty() {
        return Ok(Parsed {
            expr: Expr::all(),
            clauses: Vec::new(),
        });
    }
    let mut parser = Parser { tokens, pos: 0 };
    let (node, items) = parser.parse_or()?;
    if let Some(extra) = parser.next() {
        return Err(match extra.tok {
            Tok::RParen => error(extra.span, "対応する「(」がありません"),
            _ => error(extra.span, "ここで式が終わっていません"),
        });
    }
    let clause = |n: &Node| Clause {
        start: n.span.start,
        end: n.span.end,
        text: input
            .get(n.span.byte_start..n.span.byte_end)
            .unwrap_or_default()
            .to_string(),
        negated: n.negated,
        field: n.field,
    };
    let clauses = match &items {
        Some(items) => items.iter().map(clause).collect(),
        None => vec![clause(&node)],
    };
    Ok(Parsed {
        expr: node.expr,
        clauses,
    })
}

struct Parser {
    tokens: Vec<Token>,
    pos: usize,
}

impl Parser {
    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.pos)
    }

    fn next(&mut self) -> Option<Token> {
        let token = self.tokens.get(self.pos).cloned();
        if token.is_some() {
            self.pos += 1;
        }
        token
    }

    /// OR の式。OR がなければ、AND の項目も返す（チップに分けるため）。
    fn parse_or(&mut self) -> Result<(Node, Option<Vec<Node>>), ParseError> {
        let first = self.parse_and()?;
        if first.is_empty() {
            return Err(match self.peek() {
                Some(t) if t.tok == Tok::Pipe => error(t.span, "「|」の前に条件がありません"),
                Some(t) if t.tok == Tok::RParen => error(t.span, "対応する「(」がありません"),
                Some(t) => error(t.span, "条件がありません"),
                None => ParseError {
                    start: 0,
                    end: 0,
                    message: "条件がありません".into(),
                },
            });
        }
        let mut groups = vec![first];
        while let Some(pipe) = self.peek().filter(|t| t.tok == Tok::Pipe).cloned() {
            self.pos += 1;
            let group = self.parse_and()?;
            if group.is_empty() {
                return Err(error(pipe.span, "「|」の後に条件がありません"));
            }
            groups.push(group);
        }
        if groups.len() == 1 {
            let items = groups.pop().unwrap_or_default();
            let node = and_node(items.clone());
            return Ok((node, Some(items)));
        }
        let nodes: Vec<Node> = groups.into_iter().map(and_node).collect();
        let span = match (nodes.first(), nodes.last()) {
            (Some(a), Some(b)) => a.span.to(b.span),
            _ => unreachable_span(),
        };
        Ok((
            Node {
                expr: Expr::Or(nodes.into_iter().map(|n| n.expr).collect()),
                span,
                negated: false,
                field: None,
            },
            None,
        ))
    }

    fn parse_and(&mut self) -> Result<Vec<Node>, ParseError> {
        let mut items = Vec::new();
        while let Some(token) = self.peek() {
            if matches!(token.tok, Tok::RParen | Tok::Pipe) {
                break;
            }
            items.push(self.parse_unary()?);
        }
        Ok(items)
    }

    fn parse_unary(&mut self) -> Result<Node, ParseError> {
        if let Some(minus) = self.peek().filter(|t| t.tok == Tok::Minus).cloned() {
            self.pos += 1;
            if self
                .peek()
                .is_none_or(|t| matches!(t.tok, Tok::RParen | Tok::Pipe))
            {
                return Err(error(minus.span, "除外する条件がありません"));
            }
            let inner = self.parse_unary()?;
            return Ok(Node {
                expr: Expr::Not(Box::new(inner.expr)),
                span: minus.span.to(inner.span),
                negated: true,
                field: inner.field,
            });
        }
        self.parse_atom()
    }

    fn parse_atom(&mut self) -> Result<Node, ParseError> {
        let Some(token) = self.next() else {
            return Err(ParseError {
                start: 0,
                end: 0,
                message: "条件がありません".into(),
            });
        };
        match token.tok {
            Tok::LParen => {
                match self.peek() {
                    None => return Err(error(token.span, "かっこが閉じていません")),
                    Some(t) if t.tok == Tok::RParen => {
                        let span = token.span.to(t.span);
                        return Err(error(span, "かっこの中に条件がありません"));
                    }
                    Some(_) => {}
                }
                let (inner, _) = self.parse_or()?;
                match self.next() {
                    Some(close) if close.tok == Tok::RParen => Ok(Node {
                        expr: inner.expr,
                        span: token.span.to(close.span),
                        negated: false,
                        field: None,
                    }),
                    _ => Err(error(token.span, "かっこが閉じていません")),
                }
            }
            Tok::RParen => Err(error(token.span, "対応する「(」がありません")),
            Tok::Pipe => Err(error(token.span, "「|」の前に条件がありません")),
            Tok::Minus => Err(error(token.span, "除外する条件がありません")),
            Tok::Quoted(text) => {
                let text = nfc(text.trim());
                if text.is_empty() {
                    return Err(error(token.span, "引用符の中が空です"));
                }
                Ok(Node {
                    expr: Expr::Text(text),
                    span: token.span,
                    negated: false,
                    field: None,
                })
            }
            Tok::Word(word) => {
                // `名前:"…"` のように、値の引用符が語のすぐ後ろに続くことがある
                let quoted = match self.peek() {
                    Some(Token {
                        tok: Tok::Quoted(text),
                        span,
                    }) if span.start == token.span.end && ends_with_op(&word) => {
                        let value = (text.clone(), *span);
                        self.pos += 1;
                        Some(value)
                    }
                    _ => None,
                };
                term(&word, token.span, quoted)
            }
        }
    }
}

fn unreachable_span() -> Span {
    Span {
        start: 0,
        end: 0,
        byte_start: 0,
        byte_end: 0,
    }
}

fn and_node(mut items: Vec<Node>) -> Node {
    if items.len() == 1 {
        return items.remove(0);
    }
    let span = match (items.first(), items.last()) {
        (Some(a), Some(b)) => a.span.to(b.span),
        _ => unreachable_span(),
    };
    Node {
        expr: Expr::And(items.into_iter().map(|n| n.expr).collect()),
        span,
        negated: false,
        field: None,
    }
}

/// 語の中の最初の比較記号（位置・記号・長さ）。
fn find_op(word: &str) -> Option<(usize, Op, usize)> {
    let bytes = word.as_bytes();
    for (i, c) in word.char_indices() {
        let next = bytes.get(i + 1).copied();
        let found = match (c, next) {
            ('!', Some(b'=')) => Some((Op::Ne, 2)),
            ('>', Some(b'=')) => Some((Op::Ge, 2)),
            ('<', Some(b'=')) => Some((Op::Le, 2)),
            (':', _) => Some((Op::Has, 1)),
            ('=', _) => Some((Op::Eq, 1)),
            ('>', _) => Some((Op::Gt, 1)),
            ('<', _) => Some((Op::Lt, 1)),
            _ => None,
        };
        if let Some((op, len)) = found {
            return Some((i, op, len));
        }
    }
    None
}

/// 語を「フィールド名・記号・値」に分ける（記号がなければ None）。候補を出すのに使う。
pub(crate) fn split_op(word: &str) -> Option<(&str, &str, &str)> {
    let (i, _, len) = find_op(word).filter(|(i, _, _)| *i > 0)?;
    Some((&word[..i], &word[i..i + len], &word[i + len..]))
}

fn ends_with_op(word: &str) -> bool {
    find_op(word).is_some_and(|(i, _, len)| i + len == word.len() && i > 0)
}

fn utf16_len(s: &str) -> usize {
    s.encode_utf16().count()
}

/// 語を条件にする（フィールド・名前空間付きタグ・タグ）。
fn term(word: &str, span: Span, quoted: Option<(String, Span)>) -> Result<Node, ParseError> {
    let plain = |expr: Expr| Node {
        expr,
        span,
        negated: false,
        field: None,
    };
    let Some((i, op, len)) = find_op(word).filter(|(i, _, _)| *i > 0) else {
        return Ok(plain(Expr::Tag(TagTerm {
            ns: String::new(),
            name: nfc(word),
        })));
    };
    let left = nfc(&word[..i]);
    let right = &word[i + len..];
    let full_span = match quoted {
        Some((_, q)) => span.to(q),
        None => span,
    };
    // 値の位置（エラーの下線用）
    let value_span = match &quoted {
        Some((_, q)) => *q,
        None => {
            let start = span.start + utf16_len(&word[..i + len]);
            Span {
                start,
                end: span.end,
                byte_start: span.byte_start + i + len,
                byte_end: span.byte_end,
            }
        }
    };
    let value = match &quoted {
        Some((text, _)) => nfc(text),
        None => nfc(right),
    };

    if let Some(def) = find_field(&left) {
        if value.trim().is_empty() {
            return Err(error(
                full_span,
                format!("「{}」の値を入力してください", def.label),
            ));
        }
        let cond = build(def, op, &value).map_err(|message| error(value_span, message))?;
        return Ok(Node {
            expr: Expr::Field(cond),
            span: full_span,
            negated: false,
            field: Some(def.key),
        });
    }
    if op != Op::Has {
        let left_span = Span {
            start: span.start,
            end: span.start + utf16_len(&word[..i]),
            byte_start: span.byte_start,
            byte_end: span.byte_start + i,
        };
        return Err(error(
            left_span,
            format!("「{left}」というフィールドはありません"),
        ));
    }
    let name = value.trim().to_string();
    if name.is_empty() {
        return Err(error(full_span, "タグの名前を入力してください"));
    }
    Ok(Node {
        expr: Expr::Tag(TagTerm {
            ns: left.trim().to_string(),
            name,
        }),
        span: full_span,
        negated: false,
        field: None,
    })
}
