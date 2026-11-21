//! ドラッグアウト（M15 段 5。ADR-202・207）：アプリの中のドラッグが窓の外へ出たら OS のドラッグに切り替え、渡すファイルを
//! ライブラリの一時置き場（`staging/dragout/<回>/`）に作る。ファイルの URL で渡す（Godot 4 の macOS 版はファイルプロミスを
//! 受けないため。ADR-202 (3)）。
//!
//! - 名前と形式は「持ち出し先のプロジェクト」の既定（[`target`]。選んでいなければ、登録が 1 つならそれ、なければ名前も形式も
//!   そのまま）。書き出し（段 3）と同じ名前の規則・形式の変換（エンジンで描き、版を足さず、出力を `item_outputs` に記録する）。
//!   ユニットはユニット名のフォルダにする。同じ回の中で同じ名前は番号を付ける（落とす先の同じ名前は落とす先が扱う）。
//! - ドラッグを始めたら裏で作り始め（写しを先、形式を変えるものを後に）、落とす先がデータを求めたら（AppKit の data provider。
//!   メインスレッド）その項目ができるまで待つ（回の最初に求められてから [`WAIT_LIMIT`] まで。過ぎたら渡さない）。
//! - 終わったら、1 つでも渡した回は [`SCAN_DELAY`] 後に登録したプロジェクトの照らし直しを積み（使った記録を付ける。段 5 の決定 2）、
//!   [`KEEP_AFTER`] 後に一時置き場を消す（Finder は落としたあとに裏でコピーする。段 5 の決定 3）。何も渡さなかった回は作るのを
//!   やめてすぐ消す。ライブラリを閉じるときと開くとき（staging の掃除）にも消える。
//! - ジョブにはしない（利用者がその場で待つ用意。CLAUDE.md の「長い処理はジョブ」の例外。ADR-207 (2)）。

use std::collections::BTreeSet;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError, Weak};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use rusqlite::{Connection, OptionalExtension};

use super::formats::{ExportOptions, Plan, plan};
use super::job::{ItemRow, item_row, record_output, render_converted, resolve, writable_formats};
use super::names::{NameParts, numbered_dir, numbered_file, rule_folder, rule_name, safe_name};
use crate::edit::save::{Flow, UnitCx};
use crate::edit::services::EngineServices;
use crate::jobs::{Control, JobManager};
use crate::library::{Library, ThumbFormat, ThumbSize, hex};
use crate::{CoreError, Result, settings};

/// 一時置き場の中のドラッグアウトのフォルダ（`staging/dragout`）。
pub const DRAGOUT_DIR: &str = "dragout";

/// 持ち出し先のプロジェクトを覚える設定の鍵（値はプロジェクトの ID）。
pub const TARGET_KEY: &str = "project.dragout";

/// 落とす先がデータを求めてから待つ長さ（回ごと。最初に求められたときから数える）。過ぎた項目は渡さない。
pub const WAIT_LIMIT: Duration = Duration::from_secs(20);

/// 渡した回の一時置き場を残す長さ（Finder は落としたあとに裏でコピーする）。
pub const KEEP_AFTER: Duration = Duration::from_secs(600);

/// 渡した回の、照らし直しを積むまでの間（落とす先がコピーし終えるのを待つ）。
pub const SCAN_DELAY: Duration = Duration::from_secs(2);

/// 1 回のドラッグで作るファイルの上限（それより多ければ書き出しを使う）。
pub const MAX_FILES: usize = 1000;

/// 渡した回の照らし直しと片付けの時（試験が短くする）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Timing {
    keep: Duration,
    scan: Duration,
}

impl Default for Timing {
    fn default() -> Self {
        Self {
            keep: KEEP_AFTER,
            scan: SCAN_DELAY,
        }
    }
}

/// 持ち出し先のプロジェクト。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Target {
    /// 選んだプロジェクト（登録を外したものは None）。
    pub chosen: Option<i64>,
    /// 名前と形式に使うプロジェクト（選んでいなければ、登録が 1 つならそれ）。None なら名前も形式もそのまま。
    pub effective: Option<i64>,
}

/// 持ち出し先のプロジェクト。
pub fn target(conn: &Connection) -> Result<Target> {
    let chosen = match settings::get::<i64>(conn, TARGET_KEY)? {
        Some(id) => conn
            .prepare_cached("SELECT id FROM projects WHERE id = ?1")?
            .query_row([id], |r| r.get::<_, i64>(0))
            .optional()?,
        None => None,
    };
    let effective = match chosen {
        Some(id) => Some(id),
        None => {
            let mut stmt = conn.prepare_cached("SELECT id FROM projects LIMIT 2")?;
            let ids: Vec<i64> = stmt
                .query_map([], |r| r.get(0))?
                .collect::<rusqlite::Result<_>>()?;
            match ids.as_slice() {
                [only] => Some(*only),
                _ => None,
            }
        }
    };
    Ok(Target { chosen, effective })
}

/// 持ち出し先のプロジェクトを選ぶ（None で選ばない）。ないプロジェクトは NotFound。
pub fn set_target(conn: &Connection, project: Option<i64>) -> Result<()> {
    match project {
        Some(id) => {
            crate::project::get(conn, id)?;
            settings::set(conn, TARGET_KEY, &id)
        }
        None => settings::remove(conn, TARGET_KEY),
    }
}

/// ドラッグアウトの名前と形式（持ち出し先のプロジェクトの既定。なければ名前も形式もそのまま）。
pub fn options(conn: &Connection) -> Result<ExportOptions> {
    Ok(match target(conn)?.effective {
        Some(id) => crate::project::settings(conn, id)?.options(id),
        None => ExportOptions::default(),
    })
}

/// 落とす先に渡す 1 項目（ばらのアイテムは 1 ファイル、ユニットはフォルダ）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DragEntry {
    /// 渡すパス（絶対）。
    pub path: PathBuf,
    /// フォルダか（ユニット）。
    pub folder: bool,
    /// ドラッグの見た目に使うサムネイル（あれば。ばらのアイテムだけ）。
    pub image: Option<PathBuf>,
    /// 作るファイル（[`DragSession`] の中の番号）。
    files: Range<usize>,
}

/// 作る 1 ファイル。
#[derive(Debug, Clone)]
struct Slot {
    item_id: i64,
    path: PathBuf,
    row_hash: [u8; 32],
    ext: String,
    plan: Plan,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SlotState {
    Pending,
    Ready,
    Failed,
}

/// 回の今。
#[derive(Debug)]
struct SessionState {
    slots: Vec<SlotState>,
    /// 項目ごとに、落とす先が求めたか。
    asked: Vec<bool>,
    /// 項目ごとに、渡したか。
    handed: Vec<bool>,
    /// 項目ごとに、待ちきれずに渡さなかったか。
    late: Vec<bool>,
    /// 最初に求められてから [`WAIT_LIMIT`] 後。
    deadline: Option<Instant>,
    /// ドラッグが終わったか。
    ended: bool,
    /// 片付けた（一時置き場を消した。もう渡さない）。
    retired: bool,
    /// この時刻を過ぎたら片付ける。
    expires: Option<Instant>,
    /// この時刻を過ぎたら照らし直しを積む。
    scan_at: Option<Instant>,
}

/// ドラッグが終わったときのまとめ（画面への知らせ）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DragOutcome {
    /// 渡した項目の数。
    pub delivered: u64,
    /// 作れなかったファイルの数。
    pub failed: u64,
    /// 待ちきれずに渡さなかった項目の数。
    pub late: u64,
    /// 落とす先が求めたのに渡さなかった項目の数（作れなかった・待ちきれなかった）。
    pub missed: u64,
}

/// 1 回のドラッグ。
pub struct DragSession {
    id: u64,
    dir: PathBuf,
    entries: Vec<DragEntry>,
    slots: Vec<Slot>,
    /// 作る順（写しを先、形式を変えるものを後に）。
    order: Vec<usize>,
    /// 飛ばしたアイテムの数（ゴミ箱の中・見つからない）。
    skipped: u64,
    state: Mutex<SessionState>,
    cond: Condvar,
    cancel: AtomicBool,
    worker: Mutex<Option<JoinHandle<()>>>,
    owner: Weak<Inner>,
    timing: Timing,
}

impl std::fmt::Debug for DragSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DragSession")
            .field("id", &self.id)
            .field("dir", &self.dir)
            .field("entries", &self.entries.len())
            .field("files", &self.slots.len())
            .finish()
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

impl DragSession {
    /// 回の番号。
    pub fn id(&self) -> u64 {
        self.id
    }

    /// 一時置き場のこの回のフォルダ。
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// 落とす先に渡す項目（ドラッグの項目の順）。
    pub fn entries(&self) -> &[DragEntry] {
        &self.entries
    }

    /// 飛ばしたアイテムの数（ゴミ箱の中・見つからない）。
    pub fn skipped(&self) -> u64 {
        self.skipped
    }

    /// 落とす先がデータを求めたとき：`index` の項目ができるまで待つ（回の最初に求められてから [`WAIT_LIMIT`] まで）。
    /// 渡してよければ true（フォルダは中のどれかができたら）。メインスレッドから呼ばれる。
    pub fn wait(&self, index: usize) -> bool {
        self.wait_for(index, WAIT_LIMIT)
    }

    fn wait_for(&self, index: usize, limit: Duration) -> bool {
        let Some(range) = self.entries.get(index).map(|e| e.files.clone()) else {
            return false;
        };
        let mut st = lock(&self.state);
        if st.retired {
            return false;
        }
        st.asked[index] = true;
        let deadline = *st.deadline.get_or_insert_with(|| Instant::now() + limit);
        loop {
            let states = &st.slots[range.clone()];
            let pending = states.contains(&SlotState::Pending);
            let ready = states.contains(&SlotState::Ready);
            if !pending {
                if ready && !st.retired {
                    st.handed[index] = true;
                    return true;
                }
                return false;
            }
            let now = Instant::now();
            if now >= deadline || st.retired {
                st.late[index] = true;
                return false;
            }
            st = self
                .cond
                .wait_timeout(st, deadline - now)
                .map(|(g, _)| g)
                .unwrap_or_else(|e| e.into_inner().0);
        }
    }

    /// ドラッグが終わった（落ちた・やめた。AppKit の source が知らせる）。1 つでも渡した回は少し後に照らし直しを積み、
    /// [`KEEP_AFTER`] 後に片付ける。渡さなかった回は作るのをやめてすぐ片付ける。2 回目からは何もしない。
    pub fn end(&self) -> DragOutcome {
        let outcome = {
            let mut st = lock(&self.state);
            let outcome = DragOutcome {
                delivered: st.handed.iter().filter(|h| **h).count() as u64,
                failed: st.slots.iter().filter(|s| **s == SlotState::Failed).count() as u64,
                late: st.late.iter().filter(|l| **l).count() as u64,
                missed: st
                    .asked
                    .iter()
                    .zip(&st.handed)
                    .filter(|(a, h)| **a && !**h)
                    .count() as u64,
            };
            if st.ended {
                return outcome;
            }
            st.ended = true;
            let now = Instant::now();
            if outcome.delivered > 0 {
                st.expires = Some(now + self.timing.keep);
                st.scan_at = Some(now + self.timing.scan);
            } else {
                st.expires = Some(now);
            }
            outcome
        };
        // もう求められないので、まだ作っていないものはやめる（渡したものはできている）
        self.cancel.store(true, Ordering::SeqCst);
        self.cond.notify_all();
        self.wake_janitor();
        outcome
    }

    /// 片付けの係を起こす（印を立ててから。待つ前に見るので、起こし損ねない）。
    fn wake_janitor(&self) {
        if let Some(inner) = self.owner.upgrade() {
            lock(&inner.reg).dirty = true;
            inner.cond.notify_all();
        }
    }

    /// OS のドラッグを始められなかった（ボタンが離れていた）：作るのをやめて片付ける。
    pub fn abort(&self) {
        {
            let mut st = lock(&self.state);
            st.ended = true;
            st.expires = Some(Instant::now());
        }
        self.cancel.store(true, Ordering::SeqCst);
        self.cond.notify_all();
        self.wake_janitor();
    }

    /// 片付けた（もう渡さない。ドラッグの係が登録から外してよい）。
    pub fn retired(&self) -> bool {
        lock(&self.state).retired
    }

    /// 作るのをやめ、作り終えるのを待ってから一時置き場のこの回のフォルダを消す。
    fn retire(&self) {
        self.cancel.store(true, Ordering::SeqCst);
        {
            let mut st = lock(&self.state);
            st.retired = true;
            st.expires = None;
            st.scan_at = None;
        }
        self.cond.notify_all();
        let worker = lock(&self.worker).take();
        if let Some(worker) = worker
            && worker.join().is_err()
        {
            tracing::error!(session = self.id, "ドラッグアウトの用意が異常終了しました");
        }
        match std::fs::remove_dir_all(&self.dir) {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => {
                tracing::warn!(%err, dir = %self.dir.display(), "ドラッグアウトの一時置き場を消せませんでした（次に開くときに消します）");
            }
        }
    }

    fn set(&self, slot: usize, state: SlotState) {
        lock(&self.state).slots[slot] = state;
        self.cond.notify_all();
    }
}

/// 形式を変えて描く 1 件の場（ジョブの外。止めるのは回の中止だけ、印は書かない）。
struct DragUnit<'a> {
    library: &'a Library,
    cancel: &'a AtomicBool,
    tag: String,
}

impl UnitCx for DragUnit<'_> {
    fn library(&self) -> &Library {
        self.library
    }

    fn control(&self) -> Control {
        if self.cancel.load(Ordering::SeqCst) {
            Control::Cancel
        } else {
            Control::Continue
        }
    }

    fn progress(&self, _progress: f64, _message: &str) {}

    fn log(&self, line: &str) {
        tracing::info!("{line}");
    }

    // OS のデコーダで読む前後の印は書かない（ジョブでないので、落ちても読み直さない）
    fn mark(&self, _result_json: &str, _progress: f64, _message: &str) -> Result<()> {
        Ok(())
    }

    // コミットしない（transient）ので持ち主のジョブはない
    fn job(&self) -> i64 {
        0
    }

    fn version_job(&self) -> Option<i64> {
        None
    }

    // 一時置き場の写しで、置いたあとで捨てるので同期しない
    fn sync_file(&self, _file: &std::fs::File) -> std::io::Result<()> {
        Ok(())
    }

    fn tag(&self) -> String {
        self.tag.clone()
    }
}

/// 開いているライブラリのドラッグアウトの係（[`crate::library::LibrarySession::dragout`]）。
pub struct DragOuts {
    inner: Arc<Inner>,
}

struct Inner {
    library: Arc<Library>,
    engine: Option<Arc<EngineServices>>,
    jobs: Arc<JobManager>,
    reg: Mutex<Registry>,
    cond: Condvar,
    timing: Timing,
}

#[derive(Default)]
struct Registry {
    next: u64,
    sessions: Vec<Arc<DragSession>>,
    closing: bool,
    /// 回が終わった（片付けの係が見直す）。
    dirty: bool,
    janitor: Option<JoinHandle<()>>,
}

impl std::fmt::Debug for DragOuts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DragOuts").finish_non_exhaustive()
    }
}

/// まとまりの中の 1 件（名前を決める前）。
struct Planned {
    item_id: i64,
    row: ItemRow,
    plan: Plan,
    /// 書き出す順（1 から。テンプレートの {番号}）。
    index: usize,
}

fn too_many() -> CoreError {
    CoreError::Invalid(format!(
        "一度にドラッグできるのは {MAX_FILES} 件までです（多いときは書き出しを使ってください）"
    ))
}

/// 同じフォルダの中で同じ名前を避ける（大文字小文字を問わない。APFS は区別しないことが多い）。
fn unique(used: &mut BTreeSet<String>, name: &str, dir: bool) -> String {
    for n in 1..=u32::MAX {
        let candidate = if dir {
            numbered_dir(name, n)
        } else {
            numbered_file(name, n)
        };
        if used.insert(candidate.to_lowercase()) {
            return candidate;
        }
    }
    name.to_string()
}

impl DragOuts {
    /// 係を作る（裏の片付けの係は最初のドラッグで動かす）。
    pub fn new(
        library: Arc<Library>,
        engine: Option<Arc<EngineServices>>,
        jobs: Arc<JobManager>,
    ) -> Self {
        Self::with_timing(library, engine, jobs, Timing::default())
    }

    fn with_timing(
        library: Arc<Library>,
        engine: Option<Arc<EngineServices>>,
        jobs: Arc<JobManager>,
        timing: Timing,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                library,
                engine,
                jobs,
                reg: Mutex::new(Registry::default()),
                cond: Condvar::new(),
                timing,
            }),
        }
    }

    /// 一時置き場のドラッグアウトのフォルダ。
    pub fn root(&self) -> PathBuf {
        self.inner.library.layout().staging_dir().join(DRAGOUT_DIR)
    }

    /// ドラッグを始める：選んだもの（一覧の ID。ユニットは `LIST_FLAG | unit_id`。選んだ順）の名前と形式を決め、裏で
    /// ファイルを作り始める。作るものがなければ Invalid。
    pub fn begin(&self, entries: &[i64]) -> Result<Arc<DragSession>> {
        let inner = &self.inner;
        if lock(&inner.reg).closing {
            return Err(CoreError::Invalid("ライブラリを閉じています".into()));
        }
        let library = &inner.library;
        let (options, groups) = library.db().read(|c| {
            let options = options(c)?;
            Ok((options, resolve(c, entries)?))
        })?;
        options.check()?;
        // 多すぎる選択は行を読む前に断る（ユニットのメンバーを含めた数。飛ばすものも数える）
        if groups.iter().map(|g| g.items.len()).sum::<usize>() > MAX_FILES {
            return Err(too_many());
        }
        let writable = writable_formats(inner.engine.as_ref());
        let writable_fn = |f: &str| writable.as_ref().is_some_and(|w| w.contains(f));
        let writable_ref: Option<&dyn Fn(&str) -> bool> = writable
            .as_ref()
            .map(|_| &writable_fn as &dyn Fn(&str) -> bool);

        // まとまりごとに行を読み、ゴミ箱の中・見つからないものを飛ばす
        let mut skipped = 0u64;
        let mut index = 0usize;
        let mut planned: Vec<(usize, Vec<Planned>)> = Vec::new();
        for (g, group) in groups.iter().enumerate() {
            let mut items = Vec::new();
            for &item_id in &group.items {
                index += 1;
                let row = match item_row(library, item_id) {
                    Ok(row) if !row.trashed => row,
                    Ok(_) | Err(CoreError::NotFound(_)) => {
                        skipped += 1;
                        continue;
                    }
                    Err(err) => return Err(err),
                };
                let plan = plan(
                    row.kind,
                    &row.ext,
                    row.codec.as_deref(),
                    options.target(row.kind),
                    writable_ref,
                );
                items.push(Planned {
                    item_id,
                    row,
                    plan,
                    index,
                });
            }
            if !items.is_empty() {
                planned.push((g, items));
            }
        }
        let files: usize = planned.iter().map(|(_, items)| items.len()).sum();
        if files == 0 {
            return Err(CoreError::Invalid(
                "ドラッグできるアイテムがありません（ゴミ箱の中は持ち出せません）".into(),
            ));
        }

        let id = {
            let mut reg = lock(&inner.reg);
            reg.next += 1;
            reg.next
        };
        let dir = self.root().join(id.to_string());
        // 前の回の同じ番号の残り（開き直したライブラリ）は消してから作る
        match std::fs::remove_dir_all(&dir) {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => return Err(err.into()),
        }
        std::fs::create_dir_all(&dir)?;

        // 名前を決める（ばらのアイテムは回のフォルダに、ユニットはユニットのフォルダに。同じ名前は番号を付ける）
        let named = (|| -> Result<(Vec<DragEntry>, Vec<Slot>)> {
            let mut top = BTreeSet::new();
            let mut drag_entries = Vec::new();
            let mut slots = Vec::new();
            for (g, items) in planned {
                let group = &groups[g];
                let unit_dir = match (&group.unit, group.unit_id) {
                    (Some(unit), Some(uid)) => {
                        let fallback = format!("unit-{uid}");
                        let named =
                            safe_name(&rule_folder(&options.name, unit, &fallback), &fallback);
                        let folder = unique(&mut top, &named, true);
                        let path = dir.join(&folder);
                        std::fs::create_dir_all(&path)?;
                        Some(path)
                    }
                    _ => None,
                };
                let mut inside = BTreeSet::new();
                let start = slots.len();
                for p in items {
                    let ext = match &p.plan {
                        Plan::Copy => p.row.ext.clone(),
                        Plan::Convert { format } => {
                            crate::edit::save::output_ext(format, &p.row.ext, None)
                                .unwrap_or_else(|| format.clone())
                        }
                    };
                    let parts = NameParts {
                        item_id: p.item_id,
                        title: &p.row.title,
                        original_name: &p.row.original_name,
                        index: p.index,
                        unit: group.unit.as_deref(),
                        width: p.row.width,
                        height: p.row.height,
                    };
                    let name = rule_name(&options.name, &parts, &ext);
                    let path = match &unit_dir {
                        Some(folder) => folder.join(unique(&mut inside, &name, false)),
                        None => dir.join(unique(&mut top, &name, false)),
                    };
                    if unit_dir.is_none() {
                        drag_entries.push(DragEntry {
                            path: path.clone(),
                            folder: false,
                            image: thumb_of(library, &p.row.hash),
                            files: slots.len()..slots.len() + 1,
                        });
                    }
                    slots.push(Slot {
                        item_id: p.item_id,
                        path,
                        row_hash: p.row.hash,
                        ext: p.row.ext.clone(),
                        plan: p.plan,
                    });
                }
                if let Some(folder) = unit_dir {
                    drag_entries.push(DragEntry {
                        path: folder,
                        folder: true,
                        image: None,
                        files: start..slots.len(),
                    });
                }
            }
            Ok((drag_entries, slots))
        })();
        let (drag_entries, slots) = match named {
            Ok(named) => named,
            Err(err) => {
                let _ = std::fs::remove_dir_all(&dir);
                return Err(err);
            }
        };
        // 写しを先に（すぐできる）、形式を変えるものを後に
        let mut order: Vec<usize> = (0..slots.len()).collect();
        order.sort_by_key(|&k| matches!(slots[k].plan, Plan::Convert { .. }));

        let n_entries = drag_entries.len();
        let session = Arc::new(DragSession {
            id,
            dir,
            entries: drag_entries,
            state: Mutex::new(SessionState {
                slots: vec![SlotState::Pending; slots.len()],
                asked: vec![false; n_entries],
                handed: vec![false; n_entries],
                late: vec![false; n_entries],
                deadline: None,
                ended: false,
                retired: false,
                expires: None,
                scan_at: None,
            }),
            slots,
            order,
            skipped,
            cond: Condvar::new(),
            cancel: AtomicBool::new(false),
            worker: Mutex::new(None),
            owner: Arc::downgrade(inner),
            timing: inner.timing,
        });
        {
            let mut reg = lock(&inner.reg);
            if reg.closing {
                drop(reg);
                session.retire();
                return Err(CoreError::Invalid("ライブラリを閉じています".into()));
            }
            reg.sessions.push(session.clone());
            if reg.janitor.is_none() {
                let weak = Arc::downgrade(inner);
                reg.janitor = Some(
                    std::thread::Builder::new()
                        .name("kura-dragout-janitor".into())
                        .spawn(move || janitor(&weak))?,
                );
            }
        }
        let (work, inner2) = (session.clone(), inner.clone());
        let worker = std::thread::Builder::new()
            .name("kura-dragout".into())
            .spawn(move || prepare(&inner2, &work));
        match worker {
            Ok(handle) => *lock(&session.worker) = Some(handle),
            Err(err) => {
                session.abort();
                return Err(err.into());
            }
        }
        Ok(session)
    }

    /// 閉じる：作るのをやめ、片付けの係を止め、一時置き場のドラッグアウトのフォルダを消す（何度呼んでもよい）。
    pub fn close(&self) {
        let (sessions, janitor) = {
            let mut reg = lock(&self.inner.reg);
            reg.closing = true;
            (std::mem::take(&mut reg.sessions), reg.janitor.take())
        };
        self.inner.cond.notify_all();
        for session in &sessions {
            session.retire();
        }
        if let Some(janitor) = janitor
            && janitor.join().is_err()
        {
            tracing::error!("ドラッグアウトの片付けの係が異常終了しました");
        }
        match std::fs::remove_dir_all(self.root()) {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => {
                tracing::warn!(%err, "ドラッグアウトの一時置き場を消せませんでした（次に開くときに消します）");
            }
        }
    }

    /// 今ある回の数（試験）。
    #[cfg(test)]
    pub(crate) fn live(&self) -> usize {
        lock(&self.inner.reg).sessions.len()
    }
}

impl Drop for DragOuts {
    fn drop(&mut self) {
        self.close();
    }
}

/// ドラッグの見た目に使う小さなサムネイル（あれば）。
fn thumb_of(library: &Library, hash: &[u8; 32]) -> Option<PathBuf> {
    let hex = hex(hash);
    ThumbFormat::ALL
        .iter()
        .map(|f| library.layout().thumb_path_as(&hex, ThumbSize::Small, *f))
        .find(|p| p.is_file())
}

/// 裏でファイルを作る（写しはクローン、形式を変えるものはエンジンで描く）。やめたら残りは作らない。
fn prepare(inner: &Inner, session: &DragSession) {
    for &k in &session.order {
        if session.cancel.load(Ordering::SeqCst) {
            session.set(k, SlotState::Failed);
            continue;
        }
        let slot = &session.slots[k];
        match make(inner, session, k, slot) {
            Ok(()) => session.set(k, SlotState::Ready),
            Err(err) => {
                if !session.cancel.load(Ordering::SeqCst) {
                    tracing::warn!(%err, item = slot.item_id, "ドラッグアウトのファイルを作れませんでした");
                }
                let _ = std::fs::remove_file(&slot.path);
                session.set(k, SlotState::Failed);
            }
        }
    }
}

fn make(inner: &Inner, session: &DragSession, k: usize, slot: &Slot) -> Result<()> {
    let library = &inner.library;
    match &slot.plan {
        Plan::Copy => {
            let src = library.layout().content_path(&slot.row_hash, &slot.ext);
            if !src.is_file() {
                return Err(CoreError::NotFound(format!(
                    "ライブラリのファイルがありません: {}",
                    src.display()
                )));
            }
            reflink_copy::reflink_or_copy(&src, &slot.path)?;
            Ok(())
        }
        Plan::Convert { format } => {
            let row = item_row(library, slot.item_id)?;
            if row.hash != slot.row_hash {
                return Err(CoreError::Conflict(format!(
                    "アイテム {} がドラッグのあいだに変わりました",
                    slot.item_id
                )));
            }
            let unit = DragUnit {
                library,
                cancel: &session.cancel,
                tag: format!("dragout-{}-{k}", session.id),
            };
            let written =
                match render_converted(inner.engine.as_ref(), &unit, slot.item_id, &row, format)? {
                    Flow::Go(w) => w,
                    Flow::Stop(_) => {
                        return Err(CoreError::Invalid("ドラッグをやめました".into()));
                    }
                };
            let (hash, size, ext) = (written.out_hash(), written.out_size(), written.out_ext());
            reflink_copy::reflink_or_copy(written.part(), &slot.path)?;
            drop(written);
            // 形式を変えた出力の中身を記録する（落とした先のプロジェクトの照らし直しが一致させる）
            record_output(library, slot.item_id, &row, hash, &ext, size)
        }
    }
}

/// 片付けの係：終わった回の照らし直しを積み、時が来た回を片付ける。閉じるまで動く。
fn janitor(weak: &Weak<Inner>) {
    loop {
        let Some(inner) = weak.upgrade() else {
            return;
        };
        let now = Instant::now();
        let mut due_scan = false;
        let mut expired = Vec::new();
        let mut next: Option<Instant> = None;
        {
            let mut reg = lock(&inner.reg);
            if reg.closing {
                return;
            }
            reg.dirty = false;
            reg.sessions.retain(|s| {
                let mut st = lock(&s.state);
                if let Some(at) = st.scan_at {
                    if at <= now {
                        st.scan_at = None;
                        due_scan = true;
                    } else {
                        next = Some(next.map_or(at, |n| n.min(at)));
                    }
                }
                let expires = st.expires;
                drop(st);
                match expires {
                    Some(at) if at <= now => {
                        expired.push(s.clone());
                        false
                    }
                    Some(at) => {
                        next = Some(next.map_or(at, |n| n.min(at)));
                        true
                    }
                    None => true,
                }
            });
        }
        if due_scan
            && let Err(err) = crate::project::queue_if_registered(inner.library.db(), &inner.jobs)
        {
            tracing::warn!(%err, "ドラッグアウトのあとの照らし直しを積めませんでした");
        }
        for session in expired {
            session.retire();
        }
        let reg = lock(&inner.reg);
        if reg.closing {
            return;
        }
        if reg.dirty {
            continue;
        }
        let wait = next.map_or(Duration::from_secs(3600), |at| {
            at.saturating_duration_since(Instant::now())
        });
        // 回が終わる・閉じるときに起こされる
        let _ = inner.cond.wait_timeout(reg, wait);
    }
}

#[cfg(test)]
#[path = "dragout_tests.rs"]
mod tests;
