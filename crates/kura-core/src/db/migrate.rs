//! スキーマのマイグレーション。`PRAGMA user_version` に適用済みの数を記録する（docs/data-model.md §5）。
//!
//! - `Plain`：1 つのトランザクションで当てる。
//! - `Rebuild`：ほかの表から外部キーで参照される表を作り直す。トランザクションの外で `foreign_keys` を切り、
//!   1 つのトランザクションで当て、`PRAGMA foreign_key_check` に新しい違反がないことを確かめてからコミットし、
//!   成功しても失敗しても `foreign_keys` を必ず戻す。違反があったときだけ、いったん戻して「当てる前からあった違反」を数え、
//!   新しい違反がなければ当て直す（前からの違反は許して警告を出す）。

use std::time::Instant;

use rusqlite::{Connection, TransactionBehavior};

use crate::{CoreError, Result};

/// マイグレーションの当て方。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// 1 つのトランザクションで当てる。
    Plain,
    /// 参照される表の作り直し（外部キーを切って当て、違反を確かめる）。
    Rebuild,
}

/// マイグレーション 1 つ。
#[derive(Debug, Clone, Copy)]
pub struct Migration {
    pub name: &'static str,
    pub sql: &'static str,
    pub kind: Kind,
}

const fn plain(name: &'static str, sql: &'static str) -> Migration {
    Migration {
        name,
        sql,
        kind: Kind::Plain,
    }
}

const fn rebuild(name: &'static str, sql: &'static str) -> Migration {
    Migration {
        name,
        sql,
        kind: Kind::Rebuild,
    }
}

/// マイグレーションの一覧（連番順）。適用済みのファイルは編集しない。
pub const MIGRATIONS: &[Migration] = &[
    plain("0001_init", include_str!("../../migrations/0001_init.sql")),
    plain(
        "0002_thumb_pending_index",
        include_str!("../../migrations/0002_thumb_pending_index.sql"),
    ),
    plain(
        "0003_proxy_todo_index",
        include_str!("../../migrations/0003_proxy_todo_index.sql"),
    ),
    plain(
        "0004_playlist_generation",
        include_str!("../../migrations/0004_playlist_generation.sql"),
    ),
    plain(
        "0005_export_job",
        include_str!("../../migrations/0005_export_job.sql"),
    ),
    plain(
        "0006_unit_order_reverse",
        include_str!("../../migrations/0006_unit_order_reverse.sql"),
    ),
    rebuild(
        "0007_blobs_and_kinds",
        include_str!("../../migrations/0007_blobs_and_kinds.sql"),
    ),
    plain(
        "0008_jobs_named_checks",
        include_str!("../../migrations/0008_jobs_named_checks.sql"),
    ),
    plain(
        "0009_analysis",
        include_str!("../../migrations/0009_analysis.sql"),
    ),
];

/// 当てたマイグレーション 1 つ。
#[derive(Debug, Clone, PartialEq)]
pub struct AppliedMigration {
    pub name: &'static str,
    /// かかった時間（ミリ秒）。
    pub ms: f64,
}

/// マイグレーションを当てた結果。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MigrationReport {
    /// 当てる前の版（user_version）。
    pub from: usize,
    /// 当てたあとの版。
    pub to: usize,
    /// 当てたもの（順に）。
    pub applied: Vec<AppliedMigration>,
    /// Rebuild で許した、当てる前からあった外部キーの違反の数。
    pub tolerated_violations: usize,
    /// 当てる前のフック（バックアップ）にかかった時間（ミリ秒。`Db::open_with` だけが入れる）。
    pub before_ms: f64,
    /// 当てたあとに WAL を空にする（`wal_checkpoint(TRUNCATE)`）のにかかった時間（ミリ秒。`Db::open_with` だけが入れる）。
    pub checkpoint_ms: f64,
}

impl MigrationReport {
    /// 既存の DB（版 1 以上）を新しい版にした（更新後の初回。NFR-01 の外として記録する）。
    pub fn upgraded(&self) -> bool {
        self.from > 0 && !self.applied.is_empty()
    }

    /// バックアップ・各マイグレーション・WAL を空にするまでの時間の合計（ミリ秒）。
    pub fn total_ms(&self) -> f64 {
        self.before_ms + self.applied.iter().map(|a| a.ms).sum::<f64>() + self.checkpoint_ms
    }
}

/// 適用済みの数（user_version）。
pub fn applied(conn: &Connection) -> Result<usize> {
    let v: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
    Ok(usize::try_from(v).unwrap_or(0))
}

/// 未適用のマイグレーションがあるか。DB の方が新しければ Conflict。
pub fn pending(conn: &Connection) -> Result<bool> {
    let current = applied(conn)?;
    newer_check(current, MIGRATIONS.len())?;
    Ok(current < MIGRATIONS.len())
}

fn newer_check(current: usize, latest: usize) -> Result<()> {
    if current > latest {
        return Err(CoreError::Conflict(format!(
            "このライブラリは新しい版の Kura で作られています（スキーマ {current}、対応 {latest}）"
        )));
    }
    Ok(())
}

/// 未適用のマイグレーションを順に適用する。DB の方が新しければエラー。
pub fn migrate(conn: &mut Connection) -> Result<MigrationReport> {
    migrate_until(conn, MIGRATIONS.len())
}

/// `count` 番目までのマイグレーションを適用する（テストと kura-devtools の bench で前の版のスキーマを作るのにも使う）。
#[doc(hidden)]
pub fn migrate_until(conn: &mut Connection, count: usize) -> Result<MigrationReport> {
    run(conn, MIGRATIONS, count).map_err(|(_, err)| err)
}

/// `list` の `count` 番目までを当てる。失敗したら、失敗したマイグレーションの番号とエラーを返す。
pub(super) fn run(
    conn: &mut Connection,
    list: &[Migration],
    count: usize,
) -> std::result::Result<MigrationReport, (Option<usize>, CoreError)> {
    let current = applied(conn).map_err(|e| (None, e))?;
    newer_check(current, list.len()).map_err(|e| (None, e))?;
    let mut report = MigrationReport {
        from: current,
        to: current,
        ..MigrationReport::default()
    };
    for (index, m) in list.iter().enumerate().take(count) {
        let version = index + 1;
        if version <= current {
            continue;
        }
        let started = Instant::now();
        let result = match m.kind {
            Kind::Plain => apply_plain(conn, m, version).map(|()| 0),
            Kind::Rebuild => apply_rebuild(conn, m, version),
        };
        report.tolerated_violations += result.map_err(|e| (Some(index), e))?;
        let ms = started.elapsed().as_secs_f64() * 1000.0;
        tracing::info!(migration = m.name, ms, "マイグレーションを適用しました");
        report.applied.push(AppliedMigration { name: m.name, ms });
        report.to = version;
    }
    Ok(report)
}

fn user_version(version: usize) -> i64 {
    i64::try_from(version).unwrap_or(i64::MAX)
}

fn apply_plain(conn: &mut Connection, m: &Migration, version: usize) -> Result<()> {
    let tx = conn.transaction()?;
    tx.execute_batch(m.sql)?;
    tx.pragma_update(None, "user_version", user_version(version))?;
    tx.commit()?;
    Ok(())
}

/// `PRAGMA foreign_key_check` の行（表, rowid, 親の表, 外部キーの番号）。
pub type Violation = (String, Option<i64>, String, i64);

/// 外部キーの違反の一覧（並べたもの）。
pub fn violations(conn: &Connection) -> Result<Vec<Violation>> {
    let mut stmt = conn.prepare("PRAGMA foreign_key_check")?;
    let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?;
    let mut v: Vec<Violation> = rows.collect::<std::result::Result<_, _>>()?;
    v.sort();
    Ok(v)
}

/// 参照される表を作り直す（モジュールの冒頭）。許した前からの違反の数を返す。
pub(crate) fn apply_rebuild(conn: &mut Connection, m: &Migration, version: usize) -> Result<usize> {
    if !conn.is_autocommit() {
        return Err(CoreError::Invalid(format!(
            "{}：トランザクションの中では作り直せません",
            m.name
        )));
    }
    // 古い名前を参照し続ける古い ALTER の振る舞いでは、子の REFERENCES が書き換わってしまう
    let legacy: bool = conn.pragma_query_value(None, "legacy_alter_table", |r| r.get(0))?;
    if legacy {
        return Err(CoreError::Invalid(format!(
            "{}：legacy_alter_table が有効なので作り直せません",
            m.name
        )));
    }
    let was_on: bool = conn.pragma_query_value(None, "foreign_keys", |r| r.get(0))?;
    conn.pragma_update(None, "foreign_keys", false)?;
    let result = rebuild_without_foreign_keys(conn, m, version);
    // 成功しても失敗しても戻す（失敗したトランザクションはロールバック済み）
    let restored = conn.pragma_update(None, "foreign_keys", was_on);
    let tolerated = result?;
    restored?;
    Ok(tolerated)
}

fn rebuild_without_foreign_keys(
    conn: &mut Connection,
    m: &Migration,
    version: usize,
) -> Result<usize> {
    let off: bool = conn.pragma_query_value(None, "foreign_keys", |r| r.get(0))?;
    if off {
        return Err(CoreError::Invalid(format!(
            "{}：外部キーを切れませんでした",
            m.name
        )));
    }
    let after = {
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute_batch(m.sql)?;
        let after = violations(&tx)?;
        if after.is_empty() {
            tx.pragma_update(None, "user_version", user_version(version))?;
            tx.commit()?;
            return Ok(0);
        }
        // 違反があるときだけ：いったん戻して、当てる前からあったものかを数える
        tx.rollback()?;
        after
    };
    let new = new_violations(&violations(conn)?, &after);
    if let Some(first) = new.first() {
        return Err(CoreError::Conflict(format!(
            "{}：外部キーの食い違いが {} 件できるので当てません（例：{} の行 {:?} が {} を参照）",
            m.name,
            new.len(),
            first.0,
            first.1,
            first.2
        )));
    }
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    tx.execute_batch(m.sql)?;
    tx.pragma_update(None, "user_version", user_version(version))?;
    tx.commit()?;
    tracing::warn!(
        migration = m.name,
        violations = after.len(),
        "当てる前からあった外部キーの食い違いを残したまま当てました"
    );
    Ok(after.len())
}

/// `after` のうち `before` になかったもの（同じ行が複数あれば数で比べる。どちらも並べたもの）。
fn new_violations<'a>(before: &[Violation], after: &'a [Violation]) -> Vec<&'a Violation> {
    let mut rest = before.iter().peekable();
    let mut new = Vec::new();
    for v in after {
        while rest.next_if(|b| *b < v).is_some() {}
        if rest.next_if(|b| *b == v).is_none() {
            new.push(v);
        }
    }
    new
}

#[cfg(test)]
#[path = "migrate_tests.rs"]
mod tests;
