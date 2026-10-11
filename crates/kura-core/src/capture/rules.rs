//! キャプチャのルール（`capture_rules`。docs/data-model.md）と URL パターンの一致。

use rusqlite::{Connection, OptionalExtension, Row, params};
use serde::{Deserialize, Serialize};

use crate::{CoreError, Result};

/// 撮り方。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptureMode {
    /// 対象の要素の範囲だけ。
    Element,
    /// ページ全体。
    FullPage,
    /// 表示範囲。
    Viewport,
}

impl CaptureMode {
    /// DB に保存する名前。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Element => "element",
            Self::FullPage => "full_page",
            Self::Viewport => "viewport",
        }
    }

    /// DB の名前から戻す。
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "element" => Self::Element,
            "full_page" => Self::FullPage,
            "viewport" => Self::Viewport,
            _ => return None,
        })
    }
}

/// 幅の範囲（px）。
pub const WIDTH_RANGE: std::ops::RangeInclusive<i64> = 320..=3840;
/// スクロール回数の上限。
pub const MAX_SCROLLS: i64 = 30;
/// 待ち時間の上限（ms）。
pub const MAX_WAIT_MS: i64 = 60_000;

/// キャプチャのルール。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptureRule {
    pub id: i64,
    pub name: String,
    /// `*` が任意の文字列に一致する URL のパターン。
    pub url_pattern: String,
    pub mode: CaptureMode,
    /// 要素のみのときの CSS セレクタ。
    pub target_selector: Option<String>,
    /// 撮る前に隠す要素の CSS セレクタ。
    pub hide_selectors: Vec<String>,
    pub viewport_width: i64,
    pub scroll_times: i64,
    pub wait_ms: i64,
    pub dest_folder_id: Option<i64>,
    pub enabled: bool,
    pub last_run_at: Option<i64>,
    /// `ok` / `failed`。
    pub last_status: Option<String>,
}

/// 作る・変えるときの内容。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuleInput {
    pub name: String,
    pub url_pattern: String,
    pub mode: CaptureMode,
    pub target_selector: Option<String>,
    pub hide_selectors: Vec<String>,
    pub viewport_width: i64,
    pub scroll_times: i64,
    pub wait_ms: i64,
    pub dest_folder_id: Option<i64>,
    pub enabled: bool,
}

impl RuleInput {
    /// 確かめて整える（前後の空白を除く・空のセレクタを除く）。
    pub fn normalized(mut self) -> Result<Self> {
        self.name = self.name.trim().to_string();
        self.url_pattern = self.url_pattern.trim().to_string();
        self.target_selector = self
            .target_selector
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        self.hide_selectors = self
            .hide_selectors
            .iter()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        if self.name.is_empty() {
            return Err(CoreError::Invalid("ルールの名前を入れてください".into()));
        }
        if !(self.url_pattern.starts_with("https://") || self.url_pattern.starts_with("http://")) {
            return Err(CoreError::Invalid(
                "URL のパターンは http:// か https:// で始めてください".into(),
            ));
        }
        if self.mode == CaptureMode::Element && self.target_selector.is_none() {
            return Err(CoreError::Invalid(
                "要素のみで撮るときは、対象の要素（CSS セレクタ）を入れてください".into(),
            ));
        }
        if !WIDTH_RANGE.contains(&self.viewport_width) {
            return Err(CoreError::Invalid(format!(
                "幅は {}〜{} px にしてください",
                WIDTH_RANGE.start(),
                WIDTH_RANGE.end()
            )));
        }
        self.scroll_times = self.scroll_times.clamp(0, MAX_SCROLLS);
        self.wait_ms = self.wait_ms.clamp(0, MAX_WAIT_MS);
        Ok(self)
    }
}

/// URL がパターンに一致するか（`*` は `/` を含む任意の文字列。大文字小文字は区別しない）。
pub fn pattern_matches(pattern: &str, url: &str) -> bool {
    let pattern = pattern.to_lowercase();
    let url = url.to_lowercase();
    let parts: Vec<&str> = pattern.split('*').collect();
    let (Some(first), Some(last)) = (parts.first(), parts.last()) else {
        return false;
    };
    if parts.len() == 1 {
        return pattern == url;
    }
    if !url.starts_with(first) || !url.ends_with(last) || url.len() < first.len() + last.len() {
        return false;
    }
    // 先頭と末尾を除いた部分を、間の断片が順に現れるかで調べる
    let mut rest = &url[first.len()..url.len() - last.len()];
    for part in &parts[1..parts.len() - 1] {
        match rest.find(part) {
            Some(i) => rest = &rest[i + part.len()..],
            None => return false,
        }
    }
    true
}

const COLUMNS: &str = "id, name, url_pattern, mode, target_selector, hide_selectors, viewport_width, \
                       scroll_times, wait_ms, dest_folder_id, enabled, last_run_at, last_status";

fn from_row(row: &Row<'_>) -> rusqlite::Result<CaptureRule> {
    let mode: String = row.get(3)?;
    let hide: Option<String> = row.get(5)?;
    Ok(CaptureRule {
        id: row.get(0)?,
        name: row.get(1)?,
        url_pattern: row.get(2)?,
        mode: CaptureMode::parse(&mode).unwrap_or(CaptureMode::Viewport),
        target_selector: row.get(4)?,
        hide_selectors: hide
            .and_then(|h| serde_json::from_str(&h).ok())
            .unwrap_or_default(),
        viewport_width: row.get(6)?,
        scroll_times: row.get(7)?,
        wait_ms: row.get(8)?,
        dest_folder_id: row.get(9)?,
        enabled: row.get(10)?,
        last_run_at: row.get(11)?,
        last_status: row.get(12)?,
    })
}

/// 並び順（pos）に一覧する。
pub fn list(conn: &Connection) -> Result<Vec<CaptureRule>> {
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT {COLUMNS} FROM capture_rules ORDER BY pos, id"
    ))?;
    let rows = stmt.query_map([], from_row)?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

/// 1 件読む。
pub fn get(conn: &Connection, id: i64) -> Result<CaptureRule> {
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT {COLUMNS} FROM capture_rules WHERE id = ?1"
    ))?;
    stmt.query_row([id], from_row)
        .optional()?
        .ok_or_else(|| CoreError::NotFound(format!("キャプチャのルール {id}")))
}

/// URL に一致する最初の有効なルール。
pub fn find_for_url(conn: &Connection, url: &str) -> Result<Option<CaptureRule>> {
    Ok(list(conn)?
        .into_iter()
        .find(|r| r.enabled && pattern_matches(&r.url_pattern, url)))
}

/// 作る（いちばん下に）。
pub fn create(conn: &Connection, input: RuleInput) -> Result<CaptureRule> {
    let input = input.normalized()?;
    let mut stmt = conn.prepare_cached(
        "INSERT INTO capture_rules (name, url_pattern, mode, target_selector, hide_selectors,
                                    viewport_width, scroll_times, wait_ms, dest_folder_id, enabled, pos)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10,
                 (SELECT coalesce(max(pos), 0) + 1 FROM capture_rules))",
    )?;
    stmt.execute(params![
        input.name,
        input.url_pattern,
        input.mode.as_str(),
        input.target_selector,
        serde_json::to_string(&input.hide_selectors)?,
        input.viewport_width,
        input.scroll_times,
        input.wait_ms,
        input.dest_folder_id,
        input.enabled,
    ])?;
    get(conn, conn.last_insert_rowid())
}

/// 変える。
pub fn update(conn: &Connection, id: i64, input: RuleInput) -> Result<CaptureRule> {
    let input = input.normalized()?;
    let mut stmt = conn.prepare_cached(
        "UPDATE capture_rules SET name = ?2, url_pattern = ?3, mode = ?4, target_selector = ?5,
                hide_selectors = ?6, viewport_width = ?7, scroll_times = ?8, wait_ms = ?9,
                dest_folder_id = ?10, enabled = ?11
         WHERE id = ?1",
    )?;
    let changed = stmt.execute(params![
        id,
        input.name,
        input.url_pattern,
        input.mode.as_str(),
        input.target_selector,
        serde_json::to_string(&input.hide_selectors)?,
        input.viewport_width,
        input.scroll_times,
        input.wait_ms,
        input.dest_folder_id,
        input.enabled,
    ])?;
    if changed == 0 {
        return Err(CoreError::NotFound(format!("キャプチャのルール {id}")));
    }
    get(conn, id)
}

/// 消す。
pub fn delete(conn: &Connection, id: i64) -> Result<()> {
    let mut stmt = conn.prepare_cached("DELETE FROM capture_rules WHERE id = ?1")?;
    stmt.execute([id])?;
    Ok(())
}

/// 並べ替える（`ids` の順にする。含まれないものは後ろに今の順で）。
pub fn reorder(conn: &Connection, ids: &[i64]) -> Result<()> {
    let mut order: Vec<i64> = ids.to_vec();
    for rule in list(conn)? {
        if !order.contains(&rule.id) {
            order.push(rule.id);
        }
    }
    let mut stmt = conn.prepare_cached("UPDATE capture_rules SET pos = ?2 WHERE id = ?1")?;
    for (i, id) in order.iter().enumerate() {
        stmt.execute(params![id, (i + 1) as f64])?;
    }
    Ok(())
}

/// 最後に動かした結果を記録する（`ok` / `failed`）。
pub fn record_run(conn: &Connection, id: i64, ok: bool, at: i64) -> Result<()> {
    let mut stmt = conn.prepare_cached(
        "UPDATE capture_rules SET last_run_at = ?2, last_status = ?3 WHERE id = ?1",
    )?;
    stmt.execute(params![id, at, if ok { "ok" } else { "failed" }])?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_conn;

    pub(crate) fn input(name: &str, pattern: &str, mode: CaptureMode) -> RuleInput {
        RuleInput {
            name: name.into(),
            url_pattern: pattern.into(),
            mode,
            target_selector: (mode == CaptureMode::Element).then(|| "article".into()),
            hide_selectors: vec![" header ".into(), "".into(), ".cookie-banner".into()],
            viewport_width: 1200,
            scroll_times: 99,
            wait_ms: 1500,
            dest_folder_id: None,
            enabled: true,
        }
    }

    #[test]
    fn matches_wildcards() {
        let p = "https://example.com/*/status/*";
        assert!(pattern_matches(p, "https://example.com/someone/status/123"));
        assert!(pattern_matches(p, "HTTPS://Example.com/a/b/status/1?x=2"));
        assert!(!pattern_matches(p, "https://example.com/someone/likes/1"));
        assert!(!pattern_matches(
            p,
            "https://evil.example/https://example.com/a/status/1"
        ));
        assert!(pattern_matches(
            "https://news.example.jp/articles/*",
            "https://news.example.jp/articles/"
        ));
        assert!(pattern_matches("https://a.test/x", "https://a.test/x"));
        assert!(!pattern_matches("https://a.test/x", "https://a.test/xy"));
        assert!(pattern_matches("*", "https://anything"));
        // 先頭と末尾が重なる短い URL は一致しない
        assert!(!pattern_matches("https://a*a", "https://a"));
    }

    #[test]
    fn validates_and_normalizes_rules() {
        let ok = input("SNS", "https://example.com/*", CaptureMode::Element)
            .normalized()
            .unwrap();
        assert_eq!(ok.hide_selectors, vec!["header", ".cookie-banner"]);
        assert_eq!(ok.scroll_times, MAX_SCROLLS);
        let mut bad = input("", "https://x/*", CaptureMode::Viewport);
        assert!(bad.clone().normalized().is_err());
        bad.name = "x".into();
        bad.url_pattern = "javascript:*".into();
        assert!(bad.clone().normalized().is_err());
        bad.url_pattern = "https://x/*".into();
        bad.viewport_width = 10;
        assert!(bad.clone().normalized().is_err());
        let mut element = input("e", "https://x/*", CaptureMode::Element);
        element.target_selector = Some("  ".into());
        assert!(element.normalized().is_err());
    }

    #[test]
    fn stores_orders_and_finds_rules() {
        let conn = test_conn();
        let a = create(
            &conn,
            input(
                "記事",
                "https://news.example.jp/articles/*",
                CaptureMode::FullPage,
            ),
        )
        .unwrap();
        let b = create(
            &conn,
            input(
                "SNS 投稿",
                "https://example.com/*/status/*",
                CaptureMode::Element,
            ),
        )
        .unwrap();
        assert_eq!(b.hide_selectors, vec!["header", ".cookie-banner"]);
        assert_eq!(
            list(&conn)
                .unwrap()
                .iter()
                .map(|r| r.id)
                .collect::<Vec<_>>(),
            [a.id, b.id]
        );
        reorder(&conn, &[b.id]).unwrap();
        assert_eq!(
            list(&conn)
                .unwrap()
                .iter()
                .map(|r| r.id)
                .collect::<Vec<_>>(),
            [b.id, a.id]
        );

        let found = find_for_url(&conn, "https://example.com/u/status/9").unwrap();
        assert_eq!(found.map(|r| r.id), Some(b.id));
        let mut disabled = input(
            "SNS 投稿",
            "https://example.com/*/status/*",
            CaptureMode::Element,
        );
        disabled.enabled = false;
        update(&conn, b.id, disabled).unwrap();
        assert_eq!(
            find_for_url(&conn, "https://example.com/u/status/9").unwrap(),
            None
        );

        record_run(&conn, a.id, true, 50).unwrap();
        let a2 = get(&conn, a.id).unwrap();
        assert_eq!(
            (a2.last_run_at, a2.last_status.as_deref()),
            (Some(50), Some("ok"))
        );
        delete(&conn, a.id).unwrap();
        assert!(get(&conn, a.id).is_err());
        assert!(
            update(
                &conn,
                a.id,
                input("x", "https://x/*", CaptureMode::Viewport)
            )
            .is_err()
        );
    }
}
