//! jobs テーブルの読み書き。

use rusqlite::types::{FromSql, FromSqlError, FromSqlResult, ValueRef};
use rusqlite::{Connection, OptionalExtension, Row, params};

use super::{JobId, JobKind, JobState};
use crate::Result;

/// jobs テーブルの 1 行。
#[derive(Debug, Clone, PartialEq)]
pub struct JobRecord {
    pub id: JobId,
    pub kind: JobKind,
    pub state: JobState,
    pub priority: i64,
    pub input_json: String,
    pub progress: Option<f64>,
    pub message: Option<String>,
    pub error_code: Option<String>,
    pub log_path: Option<String>,
    pub result_json: Option<String>,
    pub created_at: i64,
    pub started_at: Option<i64>,
    pub finished_at: Option<i64>,
}

/// 一覧の絞り込み。空なら絞り込まない。
#[derive(Debug, Clone, Default)]
pub struct JobFilter {
    pub kinds: Vec<JobKind>,
    pub states: Vec<JobState>,
    pub limit: Option<u32>,
}

const COLUMNS: &str = "id, kind, state, priority, input_json, progress, message, error_code, \
                       log_path, result_json, created_at, started_at, finished_at";

impl FromSql for JobKind {
    fn column_result(value: ValueRef<'_>) -> FromSqlResult<Self> {
        let s = value.as_str()?;
        JobKind::parse(s).ok_or_else(|| FromSqlError::Other(format!("不明な種類: {s}").into()))
    }
}

impl FromSql for JobState {
    fn column_result(value: ValueRef<'_>) -> FromSqlResult<Self> {
        let s = value.as_str()?;
        JobState::parse(s).ok_or_else(|| FromSqlError::Other(format!("不明な状態: {s}").into()))
    }
}

fn from_row(row: &Row<'_>) -> rusqlite::Result<JobRecord> {
    Ok(JobRecord {
        id: row.get(0)?,
        kind: row.get(1)?,
        state: row.get(2)?,
        priority: row.get(3)?,
        input_json: row.get(4)?,
        progress: row.get(5)?,
        message: row.get(6)?,
        error_code: row.get(7)?,
        log_path: row.get(8)?,
        result_json: row.get(9)?,
        created_at: row.get(10)?,
        started_at: row.get(11)?,
        finished_at: row.get(12)?,
    })
}

/// ジョブを `queued` で追加する。
pub fn insert(conn: &Connection, kind: JobKind, input_json: &str, now: i64) -> Result<JobId> {
    let mut stmt = conn.prepare_cached(
        "INSERT INTO jobs (kind, state, input_json, created_at) VALUES (?1, 'queued', ?2, ?3)",
    )?;
    stmt.execute(params![kind.as_str(), input_json, now])?;
    Ok(conn.last_insert_rowid())
}

/// 1 件読む。
pub fn get(conn: &Connection, id: JobId) -> Result<Option<JobRecord>> {
    let mut stmt = conn.prepare_cached(&format!("SELECT {COLUMNS} FROM jobs WHERE id = ?1"))?;
    Ok(stmt.query_row([id], from_row).optional()?)
}

/// 新しい順に一覧する。
pub fn list(conn: &Connection, filter: &JobFilter) -> Result<Vec<JobRecord>> {
    let mut sql = format!("SELECT {COLUMNS} FROM jobs");
    let mut values: Vec<String> = Vec::new();
    let mut conditions = Vec::new();
    if !filter.kinds.is_empty() {
        let marks = vec!["?"; filter.kinds.len()].join(", ");
        conditions.push(format!("kind IN ({marks})"));
        values.extend(filter.kinds.iter().map(|k| k.as_str().to_string()));
    }
    if !filter.states.is_empty() {
        let marks = vec!["?"; filter.states.len()].join(", ");
        conditions.push(format!("state IN ({marks})"));
        values.extend(filter.states.iter().map(|s| s.as_str().to_string()));
    }
    if !conditions.is_empty() {
        sql.push_str(" WHERE ");
        sql.push_str(&conditions.join(" AND "));
    }
    sql.push_str(" ORDER BY id DESC LIMIT ?");
    values.push(filter.limit.unwrap_or(200).to_string());

    let mut stmt = conn.prepare_cached(&sql)?;
    let rows = stmt.query_map(rusqlite::params_from_iter(values.iter()), from_row)?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

/// 種類ごとに次の `queued` を 1 件取り出して `running` にする。
pub fn claim_next(
    conn: &Connection,
    kind: JobKind,
    now: i64,
    log_path: impl Fn(JobId) -> String,
) -> Result<Option<JobRecord>> {
    let id: Option<JobId> = {
        let mut stmt = conn.prepare_cached(
            "SELECT id FROM jobs WHERE state = 'queued' AND kind = ?1
             ORDER BY priority DESC, created_at, id LIMIT 1",
        )?;
        stmt.query_row([kind.as_str()], |row| row.get(0))
            .optional()?
    };
    let Some(id) = id else {
        return Ok(None);
    };
    let mut stmt = conn.prepare_cached(
        "UPDATE jobs SET state = 'running', started_at = coalesce(started_at, ?2),
                         log_path = coalesce(log_path, ?3), finished_at = NULL
         WHERE id = ?1",
    )?;
    stmt.execute(params![id, now, log_path(id)])?;
    get(conn, id)
}

/// 状態を `from` のどれかから `to` に変える。変えたら true。
pub fn transition(conn: &Connection, id: JobId, from: &[JobState], to: JobState) -> Result<bool> {
    let Some(current) = get(conn, id)?.map(|r| r.state) else {
        return Ok(false);
    };
    if !from.contains(&current) {
        return Ok(false);
    }
    let mut stmt = conn.prepare_cached("UPDATE jobs SET state = ?2 WHERE id = ?1")?;
    stmt.execute(params![id, to.as_str()])?;
    Ok(true)
}

/// 終了状態（done / failed / canceled）にする。
#[allow(clippy::too_many_arguments)]
pub fn finish(
    conn: &Connection,
    id: JobId,
    state: JobState,
    progress: Option<f64>,
    message: Option<&str>,
    error_code: Option<&str>,
    result_json: Option<&str>,
    now: i64,
) -> Result<()> {
    let mut stmt = conn.prepare_cached(
        "UPDATE jobs SET state = ?2, progress = coalesce(?3, progress), message = coalesce(?4, message),
                         error_code = ?5, result_json = coalesce(?6, result_json), finished_at = ?7
         WHERE id = ?1",
    )?;
    stmt.execute(params![
        id,
        state.as_str(),
        progress,
        message,
        error_code,
        result_json,
        now
    ])?;
    Ok(())
}

/// 途中経過を保存する（再開に使う result_json と表示用の進捗）。ジョブ側のトランザクションの中で呼ぶ。
pub fn save_partial(
    conn: &Connection,
    id: JobId,
    result_json: &str,
    progress: f64,
    message: &str,
) -> Result<()> {
    let mut stmt = conn.prepare_cached(
        "UPDATE jobs SET result_json = ?2, progress = ?3, message = ?4 WHERE id = ?1",
    )?;
    stmt.execute(params![id, result_json, progress, message])?;
    Ok(())
}

/// 前回 `running` のまま終わったジョブを `queued` に戻す。戻した数を返す。
pub fn recover_running(conn: &Connection) -> Result<usize> {
    let mut stmt =
        conn.prepare_cached("UPDATE jobs SET state = 'queued' WHERE state = 'running'")?;
    Ok(stmt.execute([])?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{query_plan, test_conn};

    #[test]
    fn insert_claim_finish() {
        let conn = test_conn();
        let a = insert(&conn, JobKind::Import, "{}", 10).unwrap();
        let b = insert(&conn, JobKind::Import, "{}", 20).unwrap();
        insert(&conn, JobKind::Thumbnail, "{}", 5).unwrap();

        let claimed = claim_next(&conn, JobKind::Import, 30, |id| format!("job-{id}.log"))
            .unwrap()
            .unwrap();
        assert_eq!(claimed.id, a);
        assert_eq!(claimed.state, JobState::Running);
        assert_eq!(claimed.started_at, Some(30));
        assert_eq!(
            claimed.log_path.as_deref(),
            Some(format!("job-{a}.log").as_str())
        );

        finish(
            &conn,
            a,
            JobState::Done,
            Some(1.0),
            Some("ok"),
            None,
            Some("{\"n\":1}"),
            40,
        )
        .unwrap();
        let done = get(&conn, a).unwrap().unwrap();
        assert_eq!(done.state, JobState::Done);
        assert_eq!(done.result_json.as_deref(), Some("{\"n\":1}"));
        assert_eq!(done.finished_at, Some(40));

        let next = claim_next(&conn, JobKind::Import, 50, |_| String::new())
            .unwrap()
            .unwrap();
        assert_eq!(next.id, b);
        assert!(
            claim_next(&conn, JobKind::Import, 60, |_| String::new())
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn transition_only_from_allowed_states() {
        let conn = test_conn();
        let id = insert(&conn, JobKind::Import, "{}", 0).unwrap();
        assert!(transition(&conn, id, &[JobState::Queued], JobState::Paused).unwrap());
        assert!(!transition(&conn, id, &[JobState::Queued], JobState::Canceled).unwrap());
        assert!(transition(&conn, id, &[JobState::Paused], JobState::Queued).unwrap());
        assert!(!transition(&conn, 999, &[JobState::Queued], JobState::Paused).unwrap());
    }

    #[test]
    fn recovers_running_jobs() {
        let conn = test_conn();
        let id = insert(&conn, JobKind::Import, "{}", 0).unwrap();
        claim_next(&conn, JobKind::Import, 1, |_| String::new()).unwrap();
        assert_eq!(recover_running(&conn).unwrap(), 1);
        assert_eq!(get(&conn, id).unwrap().unwrap().state, JobState::Queued);
    }

    #[test]
    fn list_filters_by_kind_and_state() {
        let conn = test_conn();
        let a = insert(&conn, JobKind::Import, "{}", 0).unwrap();
        insert(&conn, JobKind::Download, "{}", 0).unwrap();
        let filter = JobFilter {
            kinds: vec![JobKind::Import],
            states: vec![JobState::Queued],
            limit: None,
        };
        let rows = list(&conn, &filter).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, a);
        assert_eq!(list(&conn, &JobFilter::default()).unwrap().len(), 2);
    }

    #[test]
    fn queue_lookup_uses_index() {
        let conn = test_conn();
        let plan = query_plan(
            &conn,
            "SELECT id FROM jobs WHERE state = 'queued' AND kind = 'import'
             ORDER BY priority DESC, created_at, id LIMIT 1",
        );
        assert!(plan.contains("idx_jobs_queue"), "{plan}");
    }
}
