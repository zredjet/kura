//! AST を SQL の条件にする（docs/search-query.md §5）。値はすべてバインド変数にする。
//!
//! できる条件は `items i` に対する式（`WHERE` の中身）。ゴミ箱の扱いは呼ぶ側（一覧の種類）が決める。

use rusqlite::Connection;
use rusqlite::OptionalExtension;
use rusqlite::types::Value;

use super::ast::{
    CivilDate, DateSpec, Expr, FieldCond, KindValue, Op, Orientation, PlayedSpec, TagTerm, UnitSpec,
};
use crate::text::nfc;
use crate::time::days_from_civil;
use crate::{Result, folders, tags};

const DAY_MS: i64 = 86_400_000;

/// 検索するときの時刻と時差（相対の日付を解決する）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QueryContext {
    pub now_ms: i64,
    pub utc_offset_minutes: i32,
}

/// SQL の条件とバインドする値。
#[derive(Debug, Clone, PartialEq)]
pub struct SqlCond {
    pub sql: String,
    pub params: Vec<Value>,
}

/// ユニットの条件が指すもの。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UnitScope {
    /// set ユニット（None ならどれでも）。
    pub set_units: Option<Vec<i64>>,
    /// folder ユニットが参照するフォルダ（子孫を含めて解決済み）。
    pub folders: Vec<i64>,
}

/// 名前を ID にする（タグ・フォルダ・ユニット）。テストでは差し替えられるように trait にする。
pub trait Resolver {
    /// 名前空間なしの語：別名 → 名前空間なしの正式タグ。見つかればそのタグと子孫のタグ。
    fn word_tags(&self, word: &str) -> Result<Option<Vec<i64>>>;
    /// 名前空間付きのタグ（別名「ns:name」も見る）。見つかればそのタグと子孫のタグ。
    fn ns_tags(&self, ns: &str, name: &str) -> Result<Option<Vec<i64>>>;
    /// パスのフォルダ（`descendants` なら子孫も）。見つからなければ None。
    fn folders(&self, path: &[String], descendants: bool) -> Result<Option<Vec<i64>>>;
    /// ユニットの指定が指すもの（`UnitSpec::None` は呼ぶ側で `Any` の否定にする）。
    fn units(&self, spec: &UnitSpec) -> Result<UnitScope>;
}

/// AST を SQL の条件にする。
pub fn compile(expr: &Expr, resolver: &dyn Resolver, ctx: &QueryContext) -> Result<SqlCond> {
    let mut out = SqlCond {
        sql: String::new(),
        params: Vec::new(),
    };
    Compiler { resolver, ctx }.expr(expr, &mut out)?;
    Ok(out)
}

struct Compiler<'a> {
    resolver: &'a dyn Resolver,
    ctx: &'a QueryContext,
}

fn placeholders(out: &mut SqlCond, ids: &[i64]) -> String {
    out.params.extend(ids.iter().map(|id| Value::Integer(*id)));
    vec!["?"; ids.len()].join(", ")
}

fn push(out: &mut SqlCond, value: impl Into<Value>) -> &'static str {
    out.params.push(value.into());
    "?"
}

/// `LIKE` の特別な文字を逃がす（ESCAPE '\'）。
fn like_contains(s: &str) -> String {
    let mut out = String::from("%");
    for c in s.chars() {
        if matches!(c, '\\' | '%' | '_') {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('%');
    out
}

fn bound(column: &str, cmp: &str, value: i64, out: &mut SqlCond) -> String {
    let mark = push(out, value);
    format!("{column} {cmp} {mark}")
}

/// 期間の中（[start, end)。どちらかがなければ開いた区間）。
fn within(column: &str, start: Option<i64>, end: Option<i64>, out: &mut SqlCond) -> String {
    let mut parts = Vec::new();
    if let Some(s) = start {
        parts.push(bound(column, ">=", s, out));
    }
    if let Some(e) = end {
        parts.push(bound(column, "<", e, out));
    }
    if parts.is_empty() {
        format!("{column} IS NOT NULL")
    } else {
        format!("({})", parts.join(" AND "))
    }
}

/// 日付の指定が表す期間（[start, end)。どちらかがなければ開いた区間）。今日・昨日・日付は端末の時差で区切る。
pub fn date_range(spec: &DateSpec, ctx: &QueryContext) -> (Option<i64>, Option<i64>) {
    let offset = i64::from(ctx.utc_offset_minutes) * 60_000;
    let start_of = |date: CivilDate| -> i64 {
        days_from_civil(i64::from(date.y), i64::from(date.m), i64::from(date.d)) * DAY_MS - offset
    };
    let today = (ctx.now_ms + offset).div_euclid(DAY_MS) * DAY_MS - offset;
    match spec {
        DateSpec::Today => (Some(today), Some(today + DAY_MS)),
        DateSpec::Yesterday => (Some(today - DAY_MS), Some(today)),
        DateSpec::WithinDays(n) => (Some(ctx.now_ms - i64::from(*n) * DAY_MS), None),
        DateSpec::OlderThanDays(n) => (None, Some(ctx.now_ms - i64::from(*n) * DAY_MS)),
        DateSpec::Range { from, to } => (Some(start_of(*from)), Some(start_of(*to) + DAY_MS)),
    }
}

/// 日付の条件を値で判定する（SQL にした `date` と同じ意味。スマート × ユニットで使う）。
pub fn date_matches(value: Option<i64>, op: Op, spec: &DateSpec, ctx: &QueryContext) -> bool {
    let (start, end) = date_range(spec, ctx);
    let inside = |v: i64| start.is_none_or(|s| v >= s) && end.is_none_or(|e| v < e);
    match (op, value) {
        (Op::Has | Op::Eq, Some(v)) => inside(v),
        (Op::Ne, Some(v)) => !inside(v),
        (Op::Ne, None) => true,
        (Op::Gt, Some(v)) => end.is_some_and(|e| v >= e),
        (Op::Ge, Some(v)) => start.is_none_or(|s| v >= s),
        (Op::Lt, Some(v)) => start.is_some_and(|s| v < s),
        (Op::Le, Some(v)) => end.is_none_or(|e| v < e),
        (_, None) => false,
    }
}

fn sql_op(op: Op) -> &'static str {
    match op {
        Op::Has | Op::Eq => "=",
        Op::Ne => "<>",
        Op::Gt => ">",
        Op::Ge => ">=",
        Op::Lt => "<",
        Op::Le => "<=",
    }
}

impl Compiler<'_> {
    fn expr(&self, expr: &Expr, out: &mut SqlCond) -> Result<()> {
        match expr {
            Expr::And(items) => self.and(items, out),
            Expr::Or(items) => {
                if items.is_empty() {
                    out.sql.push('0');
                    return Ok(());
                }
                out.sql.push('(');
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        out.sql.push_str(" OR ");
                    }
                    self.expr(item, out)?;
                }
                out.sql.push(')');
                Ok(())
            }
            Expr::Not(inner) => {
                out.sql.push_str("NOT coalesce(");
                self.expr(inner, out)?;
                out.sql.push_str(", 0)");
                Ok(())
            }
            // 語（引用符で囲んだ語も）：別名 → 正式タグ。名前空間なしで同名のタグがなければ全文検索、
            // 名前空間付きなら一致なし
            Expr::Tag(_) | Expr::Text(_) => {
                let Some(term) = expr.tag_term() else {
                    return Ok(());
                };
                match self.tag_ids(&term)? {
                    Some(ids) => {
                        let marks = placeholders(out, &ids);
                        out.sql.push_str(&format!(
                            "EXISTS (SELECT 1 FROM item_tags t WHERE t.item_id = i.id AND t.tag_id IN ({marks}))"
                        ));
                        Ok(())
                    }
                    None if term.ns.is_empty() => {
                        self.full_text(&term.name, out);
                        Ok(())
                    }
                    None => {
                        out.sql.push('0');
                        Ok(())
                    }
                }
            }
            Expr::Field(cond) => self.field(cond, out),
        }
    }

    fn tag_ids(&self, term: &TagTerm) -> Result<Option<Vec<i64>>> {
        if term.ns.is_empty() {
            self.resolver.word_tags(&term.name)
        } else {
            self.resolver.ns_tags(&term.ns, &term.name)
        }
    }

    /// AND。タグの条件が 2 つ以上あれば、`INTERSECT` でまとめて先に絞る（§5）。
    fn and(&self, items: &[Expr], out: &mut SqlCond) -> Result<()> {
        if items.is_empty() {
            out.sql.push('1');
            return Ok(());
        }
        let mut tag_sets: Vec<Vec<i64>> = Vec::new();
        let mut rest: Vec<&Expr> = Vec::new();
        for item in items {
            match item.tag_term() {
                Some(term) => match self.tag_ids(&term)? {
                    Some(ids) => tag_sets.push(ids),
                    None => rest.push(item),
                },
                None => rest.push(item),
            }
        }
        if tag_sets.len() < 2 {
            rest = items.iter().collect();
            tag_sets.clear();
        }
        out.sql.push('(');
        let mut first = true;
        if !tag_sets.is_empty() {
            let selects: Vec<String> = tag_sets
                .iter()
                .map(|ids| {
                    let marks = placeholders(out, ids);
                    format!("SELECT item_id FROM item_tags WHERE tag_id IN ({marks})")
                })
                .collect();
            out.sql
                .push_str(&format!("i.id IN ({})", selects.join(" INTERSECT ")));
            first = false;
        }
        for item in rest {
            if !first {
                out.sql.push_str(" AND ");
            }
            first = false;
            self.expr(item, out)?;
        }
        out.sql.push(')');
        Ok(())
    }

    /// 全文検索：3 文字以上は FTS5 trigram、1〜2 文字は LIKE（§5）。
    fn full_text(&self, text: &str, out: &mut SqlCond) {
        if text.chars().count() >= 3 {
            let phrase = format!("\"{}\"", text.replace('"', "\"\""));
            let mark = push(out, phrase);
            out.sql.push_str(&format!(
                "i.id IN (SELECT rowid FROM items_fts WHERE items_fts MATCH {mark})"
            ));
        } else {
            let pattern = like_contains(text);
            let columns = [
                "i.title",
                "i.original_name",
                "ifnull(i.note, '')",
                "ifnull(i.source_url, '')",
            ];
            let parts: Vec<String> = columns
                .iter()
                .map(|c| {
                    let mark = push(out, pattern.clone());
                    format!("{c} LIKE {mark} ESCAPE '\\'")
                })
                .collect();
            out.sql.push_str(&format!("({})", parts.join(" OR ")));
        }
    }

    fn compare(&self, column: &str, op: Op, value: i64, out: &mut SqlCond) {
        let mark = push(out, value);
        out.sql.push_str(&format!(
            "({column} IS NOT NULL AND {column} {} {mark})",
            sql_op(op)
        ));
    }

    fn range(&self, spec: &DateSpec) -> (Option<i64>, Option<i64>) {
        date_range(spec, self.ctx)
    }

    fn date(&self, column: &str, op: Op, spec: &DateSpec, out: &mut SqlCond) {
        let (start, end) = self.range(spec);
        let any = format!("{column} IS NOT NULL");
        let sql = match op {
            Op::Has | Op::Eq => within(column, start, end, out),
            Op::Ne => {
                let inside = within(column, start, end, out);
                format!("({column} IS NULL OR NOT {inside})")
            }
            // 期間より後・前（期間の終わり・始まりと比べる）
            Op::Gt => end.map_or("0".to_string(), |e| bound(column, ">=", e, out)),
            Op::Ge => start.map_or(any, |s| bound(column, ">=", s, out)),
            Op::Lt => start.map_or("0".to_string(), |s| bound(column, "<", s, out)),
            Op::Le => end.map_or(any, |e| bound(column, "<", e, out)),
        };
        out.sql.push_str(&sql);
    }

    /// 文字列の条件（`:` は含む、`=` は一致、`!=` は含まない）。
    fn text(&self, columns: &[&str], op: Op, value: &str, out: &mut SqlCond) {
        let value = nfc(value);
        let parts: Vec<String> = columns
            .iter()
            .map(|c| match op {
                Op::Eq => {
                    let mark = push(out, value.clone());
                    format!("lower({c}) = lower({mark})")
                }
                _ => {
                    let mark = push(out, like_contains(&value));
                    format!("{c} LIKE {mark} ESCAPE '\\'")
                }
            })
            .collect();
        let any = format!("({})", parts.join(" OR "));
        if op == Op::Ne {
            out.sql.push_str(&format!("NOT {any}"));
        } else {
            out.sql.push_str(&any);
        }
    }

    fn negate_if(&self, op: Op, sql: String, out: &mut SqlCond) {
        if op == Op::Ne {
            out.sql.push_str(&format!("NOT coalesce({sql}, 0)"));
        } else {
            out.sql.push_str(&sql);
        }
    }

    fn field(&self, cond: &FieldCond, out: &mut SqlCond) -> Result<()> {
        match cond {
            FieldCond::Type { op, value } => {
                let kinds: Vec<Value> = value
                    .iter()
                    .map(|k: &KindValue| Value::Text(k.as_str().into()))
                    .collect();
                let marks = vec!["?"; kinds.len()].join(", ");
                out.params.extend(kinds);
                let not = if *op == Op::Ne { "NOT " } else { "" };
                out.sql.push_str(&format!("i.kind {not}IN ({marks})"));
            }
            FieldCond::Rating { op, value } => self.compare("i.rating", *op, *value, out),
            FieldCond::Dur { op, value } => self.compare("i.duration_ms", *op, *value, out),
            FieldCond::Plays { op, value } => self.compare("i.play_count", *op, *value, out),
            FieldCond::W { op, value } => self.compare("i.width", *op, *value, out),
            FieldCond::H { op, value } => self.compare("i.height", *op, *value, out),
            FieldCond::Tags { op, value } => self.compare(
                "(SELECT count(*) FROM item_tags t WHERE t.item_id = i.id)",
                *op,
                *value,
                out,
            ),
            FieldCond::Added { op, value } => self.date("i.imported_at", *op, value, out),
            FieldCond::Taken { op, value } => self.date("i.captured_at", *op, value, out),
            FieldCond::Played { op, value } => match value {
                PlayedSpec::Never => out.sql.push_str(if *op == Op::Ne {
                    "i.last_played_at IS NOT NULL"
                } else {
                    "i.last_played_at IS NULL"
                }),
                PlayedSpec::Date(spec) => self.date("i.last_played_at", *op, spec, out),
            },
            FieldCond::Folder { op, value } => {
                let sql = match self.resolver.folders(value, *op != Op::Eq)? {
                    Some(ids) if !ids.is_empty() => {
                        let marks = placeholders(out, &ids);
                        format!(
                            "EXISTS (SELECT 1 FROM item_folders f WHERE f.item_id = i.id AND f.folder_id IN ({marks}))"
                        )
                    }
                    _ => "0".to_string(),
                };
                self.negate_if(*op, sql, out);
            }
            FieldCond::Unit { op, value } => {
                let (spec, negate) = match value {
                    UnitSpec::None => (&UnitSpec::Any, *op != Op::Ne),
                    other => (other, *op == Op::Ne),
                };
                let scope = self.resolver.units(spec)?;
                let mut parts = Vec::new();
                match &scope.set_units {
                    None => parts.push(
                        "EXISTS (SELECT 1 FROM unit_items u WHERE u.item_id = i.id)".to_string(),
                    ),
                    Some(ids) if !ids.is_empty() => {
                        let marks = placeholders(out, ids);
                        parts.push(format!(
                            "EXISTS (SELECT 1 FROM unit_items u WHERE u.item_id = i.id AND u.unit_id IN ({marks}))"
                        ));
                    }
                    Some(_) => {}
                }
                if !scope.folders.is_empty() {
                    let marks = placeholders(out, &scope.folders);
                    parts.push(format!(
                        "EXISTS (SELECT 1 FROM item_folders f WHERE f.item_id = i.id AND f.folder_id IN ({marks}))"
                    ));
                }
                let sql = if parts.is_empty() {
                    "0".to_string()
                } else {
                    format!("({})", parts.join(" OR "))
                };
                if negate {
                    out.sql.push_str(&format!("NOT {sql}"));
                } else {
                    out.sql.push_str(&sql);
                }
            }
            FieldCond::Source { op, value } => {
                let mark = push(out, nfc(value));
                let sql = format!("lower(ifnull(i.source_site, '')) = lower({mark})");
                self.negate_if(*op, sql, out);
            }
            FieldCond::Name { op, value } => {
                self.text(&["i.title", "i.original_name"], *op, value, out)
            }
            FieldCond::Url { op, value } => {
                self.text(&["ifnull(i.source_url, '')"], *op, value, out)
            }
            FieldCond::Note { op, value } => self.text(&["ifnull(i.note, '')"], *op, value, out),
            FieldCond::Orient { op, value } => {
                let square = "abs(i.width - i.height) * 100 <= max(i.width, i.height)";
                let sql = match value {
                    Orientation::Square => format!("(i.width > 0 AND {square})"),
                    Orientation::Portrait => format!("(i.height > i.width AND NOT ({square}))"),
                    Orientation::Landscape => format!("(i.width > i.height AND NOT ({square}))"),
                };
                self.negate_if(*op, sql, out);
            }
        }
        Ok(())
    }
}

/// DB を見て名前を ID にする。
pub struct DbResolver<'a> {
    pub conn: &'a Connection,
}

impl DbResolver<'_> {
    fn with_descendants(&self, ids: Vec<i64>) -> Result<Option<Vec<i64>>> {
        if ids.is_empty() {
            return Ok(None);
        }
        let mut all = ids.clone();
        for id in ids {
            for child in tags::descendants(self.conn, id)? {
                if !all.contains(&child) {
                    all.push(child);
                }
            }
        }
        Ok(Some(all))
    }

    fn alias(&self, alias: &str) -> Result<Option<i64>> {
        let mut stmt = self
            .conn
            .prepare_cached("SELECT tag_id FROM tag_aliases WHERE alias = ?1")?;
        Ok(stmt.query_row([alias], |r| r.get(0)).optional()?)
    }

    fn tags_named(&self, ns: &str, name: &str) -> Result<Vec<i64>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT id FROM tags WHERE namespace = ?1 COLLATE NOCASE AND name = ?2 COLLATE NOCASE",
        )?;
        let rows = stmt.query_map([ns, name], |r| r.get(0))?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }
}

impl Resolver for DbResolver<'_> {
    fn word_tags(&self, word: &str) -> Result<Option<Vec<i64>>> {
        let word = nfc(word);
        if let Some(id) = self.alias(&word)? {
            return self.with_descendants(vec![id]);
        }
        let ids = self.tags_named("", &word)?;
        self.with_descendants(ids)
    }

    fn ns_tags(&self, ns: &str, name: &str) -> Result<Option<Vec<i64>>> {
        let (ns, name) = (nfc(ns), nfc(name));
        if let Some(id) = self.alias(&format!("{ns}:{name}"))? {
            return self.with_descendants(vec![id]);
        }
        let ids = self.tags_named(&ns, &name)?;
        self.with_descendants(ids)
    }

    fn folders(&self, path: &[String], descendants: bool) -> Result<Option<Vec<i64>>> {
        let mut parent: Option<i64> = None;
        let mut stmt = self.conn.prepare_cached(
            "SELECT id FROM folders WHERE ifnull(parent_id, 0) = ?1 AND name = ?2",
        )?;
        for name in path {
            let found: Option<i64> = stmt
                .query_row(rusqlite::params![parent.unwrap_or(0), nfc(name)], |r| {
                    r.get(0)
                })
                .optional()?;
            match found {
                Some(id) => parent = Some(id),
                None => return Ok(None),
            }
        }
        let Some(id) = parent else {
            return Ok(None);
        };
        if descendants {
            Ok(Some(folders::descendants(self.conn, id)?))
        } else {
            Ok(Some(vec![id]))
        }
    }

    fn units(&self, spec: &UnitSpec) -> Result<UnitScope> {
        let rows: Vec<(i64, String)> = match spec {
            UnitSpec::Name(name) => {
                let mut stmt = self
                    .conn
                    .prepare_cached("SELECT id, kind FROM units WHERE name = ?1")?;
                let rows = stmt.query_map([nfc(name)], |r| Ok((r.get(0)?, r.get(1)?)))?;
                rows.collect::<std::result::Result<_, _>>()?
            }
            _ => {
                let mut stmt = self
                    .conn
                    .prepare_cached("SELECT id, kind FROM units WHERE kind = 'folder'")?;
                let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
                rows.collect::<std::result::Result<_, _>>()?
            }
        };
        let mut scope = UnitScope {
            set_units: match spec {
                UnitSpec::Name(_) => Some(Vec::new()),
                _ => None,
            },
            folders: Vec::new(),
        };
        let mut refs = self
            .conn
            .prepare_cached("SELECT folder_id, recursive FROM unit_folders WHERE unit_id = ?1")?;
        for (id, kind) in rows {
            if kind == "set" {
                if let Some(ids) = scope.set_units.as_mut() {
                    ids.push(id);
                }
                continue;
            }
            let folder_refs: Vec<(i64, bool)> = refs
                .query_map([id], |r| Ok((r.get(0)?, r.get::<_, i64>(1)? != 0)))?
                .collect::<std::result::Result<_, _>>()?;
            for (folder, recursive) in folder_refs {
                let ids = if recursive {
                    folders::descendants(self.conn, folder)?
                } else {
                    vec![folder]
                };
                for f in ids {
                    if !scope.folders.contains(&f) {
                        scope.folders.push(f);
                    }
                }
            }
        }
        Ok(scope)
    }
}
