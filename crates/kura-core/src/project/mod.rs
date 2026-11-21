//! プロジェクトと使った記録（M15 段 2。docs/data-model.md §4「プロジェクトと使った記録（M15）」、ADR-202・204）。
//!
//! 登録したプロジェクト（Godot・Unity・ほかのフォルダ）を照らし直しのジョブ（[`scan`]。`kind = 'project'`）が見て、取り込める
//! 形式のファイルの中身をアイテムの今・元・過去の版と持ち出した出力（`item_outputs`）と照らす。使った記録は「今プロジェクトに
//! あるもの」（ADR-202 (4)）。フォルダが見つからなければ記録を残して `missing` にする（段 2 の決定 4）。

pub mod scan;
pub mod walk;

use std::path::{Path, PathBuf};

use rusqlite::{Connection, OptionalExtension, params};

use crate::error::{CoreError, Result};
use crate::jobs::{JobId, JobKind, JobManager};
use crate::text::nfc;

/// 名前の長さの上限（文字）。
pub const MAX_NAME_CHARS: usize = 200;

/// プロジェクトのエンジン。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Engine {
    Godot,
    Unity,
    /// どのエンジンでもないフォルダ。
    Folder,
}

impl Engine {
    /// DB の値。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Godot => "godot",
            Self::Unity => "unity",
            Self::Folder => "folder",
        }
    }

    /// DB の値から。
    pub fn parse(s: &str) -> Result<Self> {
        match s {
            "godot" => Ok(Self::Godot),
            "unity" => Ok(Self::Unity),
            "folder" => Ok(Self::Folder),
            other => Err(CoreError::Invalid(format!(
                "エンジンの値が違います: {other}"
            ))),
        }
    }
}

/// 登録したプロジェクト 1 つ。
#[derive(Debug, Clone, PartialEq)]
pub struct Project {
    pub id: i64,
    pub name: String,
    /// 正規化した絶対パス。
    pub root: String,
    pub engine: Engine,
    /// 最後の照らしでフォルダが見つからなかった。
    pub missing: bool,
    pub scanned_at: Option<i64>,
    /// 照らしたファイルの数。
    pub files: i64,
    /// 使っているアイテムの数（一致したファイルのアイテム。重なりは 1 つ）。
    pub items: i64,
    pub pos: f64,
    pub created_at: i64,
    pub updated_at: i64,
}

const COLUMNS: &str = "p.id, p.name, p.root, p.engine, p.missing, p.scanned_at,
     (SELECT count(*) FROM project_files f WHERE f.project_id = p.id),
     (SELECT count(DISTINCT f.item_id) FROM project_files f WHERE f.project_id = p.id AND f.item_id IS NOT NULL),
     p.pos, p.created_at, p.updated_at";

fn row(r: &rusqlite::Row<'_>) -> rusqlite::Result<(Project, String)> {
    Ok((
        Project {
            id: r.get(0)?,
            name: r.get(1)?,
            root: r.get(2)?,
            engine: Engine::Folder,
            missing: r.get::<_, i64>(4)? != 0,
            scanned_at: r.get(5)?,
            files: r.get(6)?,
            items: r.get(7)?,
            pos: r.get(8)?,
            created_at: r.get(9)?,
            updated_at: r.get(10)?,
        },
        r.get(3)?,
    ))
}

fn finish((mut p, engine): (Project, String)) -> Result<Project> {
    p.engine = Engine::parse(&engine)?;
    Ok(p)
}

/// すべてのプロジェクト（並びの順）。
pub fn list(conn: &Connection) -> Result<Vec<Project>> {
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT {COLUMNS} FROM projects p ORDER BY p.pos, p.id"
    ))?;
    let rows = stmt.query_map([], row)?;
    let mut out = Vec::new();
    for r in rows {
        out.push(finish(r?)?);
    }
    Ok(out)
}

/// 1 つ。
pub fn get(conn: &Connection, id: i64) -> Result<Project> {
    let mut stmt =
        conn.prepare_cached(&format!("SELECT {COLUMNS} FROM projects p WHERE p.id = ?1"))?;
    let found = stmt.query_row([id], row).optional()?;
    finish(found.ok_or_else(|| CoreError::NotFound(format!("プロジェクト {id}")))?)
}

/// フォルダの見分け（ボリュームと inode）。読めなければ None。
fn identity(path: &Path) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(path).ok().map(|m| (m.dev(), m.ino()))
}

/// `inner` が `outer` と同じか、その中か（書き出し先がプロジェクトの中かの確かめにも使う）。パスの文字のほか、ファイルの見分け（祖先のどれかが `outer` と同じボリューム・inode）で
/// 比べる（APFS の大文字小文字を区別しない名前・ファームリンク（/System/Volumes/Data）・改名で文字が食い違っても気づく）。
pub fn within(outer: &Path, inner: &Path) -> bool {
    if inner.starts_with(outer) {
        return true;
    }
    match identity(outer) {
        Some(id) => inner.ancestors().any(|a| identity(a) == Some(id)),
        None => false,
    }
}

/// 登録するフォルダを確かめて正規化する（ファイルシステムを見るので、DB の書き手の外で呼ぶ）：あるフォルダで、パスが UTF-8 で、
/// ライブラリの中でも、ライブラリを中に含むフォルダでもなく、`existing` のプロジェクト（`except` を除く）と同じ・中・外側でも
/// ない。正規化した絶対パスとエンジンを返す。書き手の中の [`add`]・[`relocate`] もパスの文字で重なりを確かめ直す。
pub fn prepare_root(
    path: &Path,
    library_root: &Path,
    existing: &[Project],
    except: Option<i64>,
) -> Result<(String, Engine)> {
    if !path.is_absolute() {
        return Err(CoreError::Invalid(format!(
            "絶対パスを指定してください: {}",
            path.display()
        )));
    }
    let root = std::fs::canonicalize(path).map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => {
            CoreError::NotFound(format!("フォルダが見つかりません: {}", path.display()))
        }
        std::io::ErrorKind::NotADirectory => {
            CoreError::Invalid(format!("フォルダを選んでください: {}", path.display()))
        }
        std::io::ErrorKind::PermissionDenied => {
            CoreError::Invalid(format!("読めないフォルダです: {}", path.display()))
        }
        _ => CoreError::Io(e),
    })?;
    if !root.is_dir() {
        return Err(CoreError::Invalid(format!(
            "フォルダを選んでください: {}",
            root.display()
        )));
    }
    let library =
        std::fs::canonicalize(library_root).unwrap_or_else(|_| library_root.to_path_buf());
    if within(&library, &root) || within(&root, &library) {
        return Err(CoreError::Invalid(
            "ライブラリの中のフォルダや、ライブラリを含むフォルダは登録できません".into(),
        ));
    }
    for p in existing.iter().filter(|p| Some(p.id) != except) {
        let other = Path::new(&p.root);
        if within(other, &root) || within(&root, other) {
            return Err(CoreError::Conflict(format!(
                "プロジェクト「{}」と同じか、中・外側のフォルダです",
                p.name
            )));
        }
    }
    let text = root
        .to_str()
        .ok_or_else(|| {
            CoreError::Invalid(format!("名前を読めないフォルダです: {}", root.display()))
        })?
        .to_string();
    Ok((text, walk::detect(&root)))
}

/// ほかのプロジェクトと重なる（同じ・中・外側。パスの文字で）なら断る（書き手の中の確かめ直し。ファイルの見分けでの確かめは
/// [`prepare_root`]）。
fn check_overlap(conn: &Connection, root: &str, except: Option<i64>) -> Result<()> {
    let mut stmt = conn.prepare_cached("SELECT id, name, root FROM projects")?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
        ))
    })?;
    let new = Path::new(root);
    for r in rows {
        let (id, name, other) = r?;
        if Some(id) == except {
            continue;
        }
        let other = Path::new(&other);
        if new.starts_with(other) || other.starts_with(new) {
            return Err(CoreError::Conflict(format!(
                "プロジェクト「{name}」と同じか、中・外側のフォルダです"
            )));
        }
    }
    Ok(())
}

fn clean_name(name: &str) -> Result<String> {
    let name = nfc(name.trim());
    if name.is_empty() {
        return Err(CoreError::Invalid(
            "プロジェクトの名前を入力してください".into(),
        ));
    }
    if name.chars().count() > MAX_NAME_CHARS {
        return Err(CoreError::Invalid(format!(
            "プロジェクトの名前は {MAX_NAME_CHARS} 文字までです"
        )));
    }
    Ok(name)
}

/// プロジェクトを登録する（並びの最後。`name` がなければフォルダの名前）。`root` と `engine` は [`prepare_root`] の結果。
/// ID を返す。
pub fn add(
    conn: &Connection,
    root: &str,
    engine: Engine,
    name: Option<&str>,
    now: i64,
) -> Result<i64> {
    check_overlap(conn, root, None)?;
    let name = match name {
        Some(name) => clean_name(name)?,
        // 名前を省けばフォルダの名前（長ければ上限で切る）
        None => {
            let folder = Path::new(root)
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or(root);
            clean_name(&nfc(folder).chars().take(MAX_NAME_CHARS).collect::<String>())?
        }
    };
    conn.prepare_cached(
        "INSERT INTO projects (name, root, engine, settings, pos, missing, created_at, updated_at)
         VALUES (?1, ?2, ?3, '{}', (SELECT ifnull(max(pos), 0) + 1 FROM projects), 0, ?4, ?4)",
    )?
    .execute(params![name, root, engine.as_str(), now])?;
    Ok(conn.last_insert_rowid())
}

/// 名前を変える。
pub fn rename(conn: &Connection, id: i64, name: &str, now: i64) -> Result<()> {
    let name = clean_name(name)?;
    let n = conn
        .prepare_cached("UPDATE projects SET name = ?2, updated_at = ?3 WHERE id = ?1")?
        .execute(params![id, name, now])?;
    if n == 0 {
        return Err(CoreError::NotFound(format!("プロジェクト {id}")));
    }
    Ok(())
}

/// 場所を選び直す（見つからなくなったときなど）。使った記録は残し、次の照らし直しで新しい場所のファイルと照らす。`root` と
/// `engine` は [`prepare_root`] の結果。
pub fn relocate(conn: &Connection, id: i64, root: &str, engine: Engine, now: i64) -> Result<()> {
    get(conn, id)?;
    check_overlap(conn, root, Some(id))?;
    conn.prepare_cached(
        "UPDATE projects SET root = ?2, engine = ?3, missing = 0, updated_at = ?4 WHERE id = ?1",
    )?
    .execute(params![id, root, engine.as_str(), now])?;
    Ok(())
}

/// 登録を外す（使った記録も消える。プロジェクトのフォルダには触れない）。
pub fn remove(conn: &Connection, id: i64) -> Result<()> {
    let n = conn
        .prepare_cached("DELETE FROM projects WHERE id = ?1")?
        .execute([id])?;
    if n == 0 {
        return Err(CoreError::NotFound(format!("プロジェクト {id}")));
    }
    Ok(())
}

/// プロジェクトで使っているアイテム（一致したファイルのアイテム。ID の順）。
pub fn used_items(conn: &Connection, id: i64) -> Result<Vec<i64>> {
    get(conn, id)?;
    let mut stmt = conn.prepare_cached(
        "SELECT DISTINCT item_id FROM project_files WHERE project_id = ?1 AND item_id IS NOT NULL ORDER BY item_id",
    )?;
    let rows = stmt.query_map([id], |r| r.get(0))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// アイテムを使っているプロジェクト 1 つ。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItemUse {
    pub project_id: i64,
    pub name: String,
    pub missing: bool,
    /// そのアイテムと一致したファイル（ルートからの相対パスの順）。
    pub paths: Vec<String>,
}

/// アイテムを使っているプロジェクト（並びの順）。
pub fn item_projects(conn: &Connection, item: i64) -> Result<Vec<ItemUse>> {
    let mut stmt = conn.prepare_cached(
        "SELECT p.id, p.name, p.missing, f.path FROM project_files f JOIN projects p ON p.id = f.project_id
          WHERE f.item_id = ?1 ORDER BY p.pos, p.id, f.path",
    )?;
    let rows = stmt.query_map([item], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, i64>(2)? != 0,
            r.get::<_, String>(3)?,
        ))
    })?;
    let mut out: Vec<ItemUse> = Vec::new();
    for r in rows {
        let (project_id, name, missing, path) = r?;
        match out.last_mut() {
            Some(last) if last.project_id == project_id => last.paths.push(path),
            _ => out.push(ItemUse {
                project_id,
                name,
                missing,
                paths: vec![path],
            }),
        }
    }
    Ok(out)
}

/// 持ち出しの既定（`projects.settings`。M15 段 3。空ならエンジンごとの既定）。
pub fn settings(conn: &Connection, id: i64) -> Result<crate::export::ExportPreset> {
    let p = get(conn, id)?;
    let json: String = conn
        .prepare_cached("SELECT settings FROM projects WHERE id = ?1")?
        .query_row([id], |r| r.get(0))?;
    Ok(crate::export::ExportPreset::from_json(&json, p.engine))
}

/// 持ち出しの既定を書く（名前のテンプレートと形式を確かめる）。
pub fn set_settings(
    conn: &Connection,
    id: i64,
    preset: &crate::export::ExportPreset,
    now: i64,
) -> Result<()> {
    get(conn, id)?;
    preset.options(id).check()?;
    let json = serde_json::to_string(preset)?;
    conn.prepare_cached("UPDATE projects SET settings = ?2, updated_at = ?3 WHERE id = ?1")?
        .execute(params![id, json, now])?;
    Ok(())
}

/// 書き出し先 `dest` がプロジェクト `p` のフォルダの中か確かめる（ファイルシステムを見るので書き手の外で）。
pub fn check_dest(p: &Project, dest: &Path) -> Result<()> {
    if within(Path::new(&p.root), dest) {
        Ok(())
    } else {
        Err(CoreError::Invalid(format!(
            "プロジェクト「{}」のフォルダの中を選んでください",
            p.name
        )))
    }
}

/// 照らし直しのジョブを積む（登録したプロジェクト全部を照らす。待っているものがあれば積まない）。
pub fn queue_scan(jobs: &JobManager) -> Result<Option<JobId>> {
    jobs.ensure_queued(JobKind::Project, "{}")
}

/// ライブラリを開いたとき：登録したプロジェクトがあれば照らし直しを積む（段 2 の決定 2。読みの接続の外で積む）。
pub fn queue_on_open(db: &crate::db::Db, jobs: &JobManager) -> Result<Option<JobId>> {
    queue_if_registered(db, jobs)
}

/// 登録したプロジェクトがあれば照らし直しを積む（開いたとき・ドラッグアウトで渡したあと。M15 段 5）。
pub fn queue_if_registered(db: &crate::db::Db, jobs: &JobManager) -> Result<Option<JobId>> {
    let any: Option<i64> = db.read(|c| {
        Ok(c.prepare_cached("SELECT id FROM projects LIMIT 1")?
            .query_row([], |r| r.get(0))
            .optional()?)
    })?;
    if any.is_some() {
        queue_scan(jobs)
    } else {
        Ok(None)
    }
}

/// `root` からの相対パス（`/` 区切り）を絶対パスにする。
pub fn absolute(root: &str, rel: &str) -> PathBuf {
    rel.split('/').fold(PathBuf::from(root), |p, c| p.join(c))
}

#[cfg(test)]
mod tests;
