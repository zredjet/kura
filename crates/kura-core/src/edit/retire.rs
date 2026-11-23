//! 退役する読み手の移し替え（`JobKind::Retire`。M16 段 5。docs/editing.md §9.4「退役する読み手の移し替え」、ADR-222）。
//!
//! 後の版で外す旧版の読み手（`Decoder::retiring`）で読んだ版の入力のうち、アンカーのないもの（[`targets`]）に、その読み手で
//! 1 度だけアンカーを作る。旧版の読み手を外したあとも、その版を作り直せる（版に戻す・一括の［元に戻す…］・GC の照合）ように
//! するため。M16 段 5 で symphonia の Vorbis・AAC の 3 つのために作り、段 10 でそれらを外した（`codec::RETIRED_DECODERS`）
//! ので、今の固定の組に退役する読み手はない（ジョブは積まれない）。後の版で読み手を外すときに、同じ仕組みを使う。
//!
//! - 1 件ずつ（版の行の ID・入力の番号の順）：記録の元がアイテムの元と同じか確かめ、同じ元・読み方・識別子の移したアンカーが
//!   すでにあれば（同じアイテムの戻した版の写しなど）記録が同じときだけ結び付け、なければ元を記録の読み手で読み直して値の
//!   ダイジェストを記録と照らし、アンカーに昇格して台帳とアンカーの行に載せ、同じアイテムの同じ記録の入力にまとめて結び付ける
//!   （`save::promote_recorded`・`commit_retired`）。移したアンカーの鍵は `anchors::retired_key`（ふつうの鍵と重ならない）で、
//!   保存・エディタ・一括が元 × 読み方で探しても見つからない（新しい読みは今の読み手で読む。利用者の決定）。
//! - 移せないもの（[`Left`]：記録が読めない・元がない・元のファイルがない・読み手がない・値が違う・別の記録のアンカー・
//!   移したアンカーのファイルがない）は触らずに数え、ログに残す。誤り（読みの入出力・上限など）は失敗として数えて次へ進む。
//!   ディスクの空きが足りない・DB が閉じた・DB の誤りはジョブを失敗にする。
//! - 一時停止・中止・譲る（GC と同じ相手と GC）は 1 件ごとに見て、カーソルと数を 1 件ごとのトランザクションで途中経過に書く
//!   （再開はカーソルの次から。読みの途中で止めたら、その 1 件を初めからやり直す）。
//! - 積むのは、ライブラリを開いたとき（[`queue_on_open`]）：退役する識別子の一覧が、移し終えた印（settings の [`DONE_KEY`]）と
//!   違うときだけ。印は、ジョブが終わったときに置く（移せないもの・1 件の失敗が残っても置く。やり直しても同じになりうるので、
//!   開くたびに読み直さない。残りは kura-devtools の `retired` で数え、`retired --again` が印を消して次に開いたときにまた積ませる
//!   [`forget`]）。ジョブが失敗した（空き・DB）・中止したら置かず、次に開いたときにまた積む。版の入力がひとつもなければ、積まずに
//!   印だけ置く。
//! - 対象の走査は、版の入力の全件（`json_extract` で記録の識別子を見る）で、印が置かれるまでの一度きり（docs/data-model.md §4）。

use std::collections::BTreeMap;
use std::sync::Arc;

use kura_engine::codec::recorded::RecordedInput;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

use super::save::{self, Flow, Promote};
use super::{EngineServices, anchors, versions};
use crate::db::Db;
use crate::jobs::{
    Control, JobContext, JobEnd, JobHandler, JobId, JobKind, JobManager, save_partial,
};
use crate::{CoreError, Result, settings};

/// 移し終えた退役の識別子の一覧（並べたもの）を置く settings のキー。
pub const DONE_KEY: &str = "edit.retired";

/// 1 回の問い合わせで読む対象の数。
const BATCH: usize = 16;

/// 進みの一言。
const MESSAGE: &str = "旧版の読み取りを残しています";

/// 移せなかった理由。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Left {
    /// 入力の記録を読めない。
    BadRecord,
    /// アイテムの元が記録の元と違う・ない。
    NoOriginal,
    /// 元のファイルがない。
    Missing,
    /// 記録の読み手がこの環境にない。
    NoDecoder,
    /// 元を読み直した値・記録が版の記録と違う。
    Mismatch,
    /// 同じ鍵に別の記録のアンカーがある。
    OtherAnchor,
    /// 移したアンカーのファイルがない（結び付けない）。
    BrokenAnchor,
}

impl Left {
    /// 結果の JSON の名前。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::BadRecord => "badRecord",
            Self::NoOriginal => "noOriginal",
            Self::Missing => "missing",
            Self::NoDecoder => "noDecoder",
            Self::Mismatch => "mismatch",
            Self::OtherAnchor => "otherAnchor",
            Self::BrokenAnchor => "brokenAnchor",
        }
    }
}

/// ジョブの途中経過と結果（`result_json`）。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RetireState {
    /// 形の版（1）。
    pub v: u32,
    /// このジョブが見る退役の識別子（再開したときに組の一覧が変わっていれば初めから）。
    pub ids: Vec<String>,
    /// 見終えた最後の対象（版の行の ID・入力の番号）。この次から。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<(i64, i64)>,
    /// 作ったアンカーの数。
    pub anchored: u64,
    /// アンカーを結び付けた版の入力の数。
    pub linked: u64,
    /// 移せなかった数（理由の名前ごと）。
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub left: BTreeMap<String, u64>,
    /// 誤りで移せなかった数（ログに理由。印は置く。`retired --again` でやり直す）。
    pub failed: u64,
}

impl RetireState {
    fn json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| "{}".into())
    }

    fn left_total(&self) -> u64 {
        self.left.values().sum()
    }

    /// 終わりの言葉。
    pub fn message(&self) -> String {
        let mut s = format!(
            "旧版の読み取りを残しました（アンカー {} 個・版の入力 {} 件）",
            self.anchored, self.linked
        );
        let left = self.left_total();
        if left > 0 {
            s.push_str(&format!("。移せないもの {left} 件"));
        }
        if self.failed > 0 {
            s.push_str(&format!("。失敗 {} 件（理由はログに）", self.failed));
        }
        s
    }
}

/// 対象の版の入力 1 つ。
#[derive(Debug, Clone, PartialEq)]
pub struct Target {
    pub version_id: i64,
    pub input: i64,
    pub item_id: i64,
    /// `RecordedInput` の正規の JSON。
    pub recorded: String,
}

/// 退役の識別子 `ids`（JSON の配列）で読んだ、アンカーのない版の入力を、カーソル `after` の次から `limit` 件（版の行の ID・
/// 入力の番号の順。版の入力の主キーの範囲で引き、記録の識別子は行ごとに見る。JSON として壊れた記録は対象にしない：
/// `json_extract` が問い合わせごと失敗するので `json_valid` で守る）。
pub fn targets(
    conn: &Connection,
    ids: &str,
    after: Option<(i64, i64)>,
    limit: usize,
) -> Result<Vec<Target>> {
    let (v, i) = after.unwrap_or((i64::MIN, i64::MIN));
    let mut stmt = conn.prepare_cached(
        "SELECT i.version_id, i.input, v.item_id, i.recorded
           FROM item_version_inputs i JOIN item_versions v ON v.id = i.version_id
          WHERE (i.version_id, i.input) > (?2, ?3)
            AND +i.anchor_id IS NULL
            AND CASE WHEN json_valid(i.recorded) THEN json_extract(i.recorded, '$.decoded.codec') END
                IN (SELECT value FROM json_each(?1))
          ORDER BY i.version_id, i.input
          LIMIT ?4",
    )?;
    let rows = stmt.query_map(params![ids, v, i, limit as i64], |r| {
        Ok(Target {
            version_id: r.get(0)?,
            input: r.get(1)?,
            item_id: r.get(2)?,
            recorded: r.get(3)?,
        })
    })?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

/// 対象の数（カーソル `after` の次から。進み具合と kura-devtools の数え上げ）。
pub fn count(conn: &Connection, ids: &str, after: Option<(i64, i64)>) -> Result<u64> {
    let (v, i) = after.unwrap_or((i64::MIN, i64::MIN));
    let n: i64 = conn
        .prepare_cached(
            "SELECT count(*) FROM item_version_inputs i
              WHERE (i.version_id, i.input) > (?2, ?3)
                AND +i.anchor_id IS NULL
                AND CASE WHEN json_valid(i.recorded) THEN json_extract(i.recorded, '$.decoded.codec') END
                IN (SELECT value FROM json_each(?1))",
        )?
        .query_row(params![ids, v, i], |r| r.get(0))?;
    Ok(n.max(0) as u64)
}

/// 退役の識別子の一覧の JSON（配列）。
fn ids_json(ids: &[String]) -> Result<String> {
    Ok(serde_json::to_string(ids)?)
}

/// ライブラリを開いたとき：退役する識別子 `ids` があり、移し終えた印と違えばジョブを積む（版の入力がひとつもなければ、積まずに
/// 印だけ置く）。読みの接続の外で積む。
pub fn queue_on_open(db: &Db, jobs: &JobManager, ids: &[&str]) -> Result<Option<JobId>> {
    if ids.is_empty() {
        return Ok(None);
    }
    let ids: Vec<String> = ids.iter().map(|s| s.to_string()).collect();
    let done: Option<Vec<String>> = db.read(|c| settings::get(c, DONE_KEY))?;
    if done.as_ref() == Some(&ids) {
        return Ok(None);
    }
    let any: bool = db.read(|c| {
        Ok(
            c.prepare_cached("SELECT EXISTS (SELECT 1 FROM item_version_inputs)")?
                .query_row([], |r| r.get(0))?,
        )
    })?;
    if !any {
        db.write(move |tx| settings::set(tx, DONE_KEY, &ids))?;
        return Ok(None);
    }
    jobs.ensure_queued(JobKind::Retire, "{}")
}

/// 移し終えた印を消す（kura-devtools の `retired --again`。次に開いたときにまた積む。移せなかったもの・失敗したものを、元や
/// 記録を直したあとにやり直す）。
pub fn forget(db: &Db) -> Result<()> {
    db.write(|tx| settings::remove(tx, DONE_KEY))
}

/// 移し替えのジョブの係。
pub struct RetireHandler {
    engine: Arc<EngineServices>,
    #[cfg(test)]
    hook: Option<RetireHook>,
    /// 試験の強制終了（1 度だけ）。
    #[cfg(test)]
    fault: std::sync::Mutex<Option<save::Fault>>,
}

impl RetireHandler {
    /// このライブラリのエンジンの部品で移す。
    pub fn new(engine: Arc<EngineServices>) -> Self {
        Self {
            engine,
            #[cfg(test)]
            hook: None,
            #[cfg(test)]
            fault: std::sync::Mutex::new(None),
        }
    }

    /// 試験の強制終了を付ける（次の 1 件で 1 度だけ）。
    #[cfg(test)]
    pub(crate) fn with_fault(self, fault: save::Fault) -> Self {
        *self
            .fault
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(fault);
        self
    }

    /// 1 件の場（試験では強制終了を 1 度だけ渡す）。
    fn unit<'a>(&self, ctx: &'a JobContext) -> Unit<'a> {
        Unit {
            ctx,
            #[cfg(test)]
            fault: self
                .fault
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take(),
        }
    }

    /// 試験の割り込みを付ける。
    #[cfg(test)]
    pub(crate) fn with_hook(mut self, hook: RetireHook) -> Self {
        self.hook = Some(hook);
        self
    }

    #[cfg(test)]
    fn at(&self, stage: RetireStage) {
        if let Some(hook) = &self.hook {
            hook(stage);
        }
    }

    #[cfg(not(test))]
    fn at(&self, _stage: RetireStage) {}
}

/// 試験の割り込みの段（版の行の ID）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RetireStage {
    /// 対象の 1 件の前（止める指示を見る前）。
    Target(i64),
    /// 読んで照らしたアンカーを載せる前（台帳の門を取る前）。
    Commit(i64),
}

/// 試験の割り込み。
#[cfg(test)]
pub(crate) type RetireHook = Arc<dyn Fn(RetireStage) + Send + Sync>;

/// 1 件の場（保存の部品に渡す。ジョブの文脈と同じで、試験では強制終了の段を持つ）。
struct Unit<'a> {
    ctx: &'a JobContext,
    #[cfg(test)]
    fault: Option<save::Fault>,
}

impl save::UnitCx for Unit<'_> {
    fn library(&self) -> &crate::library::Library {
        self.ctx.library()
    }

    fn control(&self) -> Control {
        self.ctx.control()
    }

    fn progress(&self, progress: f64, message: &str) {
        self.ctx.progress(progress, message);
    }

    fn log(&self, line: &str) {
        self.ctx.log(line);
    }

    fn mark(&self, result_json: &str, progress: f64, message: &str) -> Result<()> {
        self.ctx.mark(result_json, progress, message)
    }

    fn job(&self) -> i64 {
        self.ctx.id()
    }

    #[cfg(test)]
    fn fault(&self) -> Option<save::Fault> {
        self.fault
    }
}

/// 止める指示（一時停止・中止・譲る）を見る。
fn interrupted(ctx: &JobContext, st: &RetireState) -> Option<JobEnd> {
    match ctx.control() {
        Control::Pause => Some(JobEnd::Paused),
        Control::Cancel => Some(JobEnd::Canceled {
            result_json: Some(st.json()),
        }),
        Control::Continue if ctx.should_yield() => Some(JobEnd::Yield),
        Control::Continue => None,
    }
}

/// 読みの中で止めたときの終わり方（中止なら途中経過を結果に）。
fn stopped(end: JobEnd, st: &RetireState) -> JobEnd {
    match end {
        JobEnd::Canceled { .. } => JobEnd::Canceled {
            result_json: Some(st.json()),
        },
        other => other,
    }
}

/// ジョブを失敗にする誤り（続けても同じ：空きがない・DB が使えない）。
fn fatal(e: &CoreError) -> bool {
    matches!(
        e,
        CoreError::NoSpace { .. } | CoreError::DbClosed | CoreError::Db(_)
    ) || e.is_no_space()
}

/// 1 件の行き先。
enum Outcome {
    /// 作ったアンカーを結び付けた（数は途中経過に書いた）。
    Anchored,
    /// あったアンカーに結び付ける。
    Link(Box<anchors::Anchor>),
    /// 移せない。
    Left(Left, String),
}

impl JobHandler for RetireHandler {
    fn run(&self, ctx: &JobContext) -> Result<JobEnd> {
        let library = ctx.library();
        let db = library.db();
        let job = ctx.id();
        let codecs = self.engine.codecs();
        let ids: Vec<String> = codecs
            .retiring_ids()
            .iter()
            .map(|s| s.to_string())
            .collect();
        let mut st: RetireState = ctx
            .partial()
            .and_then(|p| serde_json::from_str::<RetireState>(p).ok())
            .filter(|s| s.v == 1 && s.ids == ids)
            .unwrap_or_default();
        st.v = 1;
        st.ids = ids.clone();
        let list = ids_json(&ids)?;
        if !ids.is_empty() {
            let total = {
                let (list, cursor) = (list.clone(), st.cursor);
                db.read(|c| count(c, &list, cursor))?.max(1)
            };
            let mut seen = 0u64;
            loop {
                let batch = {
                    let (list, cursor) = (list.clone(), st.cursor);
                    db.read(|c| targets(c, &list, cursor, BATCH))?
                };
                if batch.is_empty() {
                    break;
                }
                for t in batch {
                    self.at(RetireStage::Target(t.version_id));
                    if let Some(end) = interrupted(ctx, &st) {
                        return Ok(end);
                    }
                    let frac = (seen as f64 / total as f64).min(1.0) * 0.99;
                    match self.one(ctx, &t, &mut st, frac) {
                        Ok(Flow::Go(())) => {}
                        Ok(Flow::Stop(end)) => return Ok(stopped(end, &st)),
                        Err(e) if fatal(&e) => return Err(e),
                        Err(e) => {
                            ctx.log(&format!(
                                "版の入力（版の行 {}・入力 {}）を移せませんでした：{e}",
                                t.version_id, t.input
                            ));
                            st.failed += 1;
                            st.cursor = Some((t.version_id, t.input));
                            let json = st.json();
                            db.write(move |tx| save_partial(tx, job, &json, frac, MESSAGE))?;
                        }
                    }
                    seen += 1;
                    ctx.progress((seen as f64 / total as f64).min(1.0) * 0.99, MESSAGE);
                }
            }
        }
        if let Some(end) = interrupted(ctx, &st) {
            return Ok(end);
        }
        // 印を置く（移せないもの・1 件の失敗が残っても。開くたびに読み直さない）
        let json = st.json();
        db.write(move |tx| {
            settings::set(tx, DONE_KEY, &ids)?;
            save_partial(tx, job, &json, 1.0, "旧版の読み取りを残しました")
        })?;
        let message = st.message();
        ctx.log(&message);
        Ok(JobEnd::Done {
            result_json: st.json(),
            message: Some(message),
        })
    }
}

impl RetireHandler {
    /// 1 件：行き先を決め（[`Self::decide`]）、結び付ける・数える。カーソルと数は 1 件ごとのトランザクションで途中経過に書く
    /// （作ったアンカーは載せるトランザクションで）。結び付けるアンカーは書くトランザクションの中で確かめ直し、決めたあとに
    /// 片付けられていれば（ゴミ箱を空にした）、もう一度決める（今度は作る）。
    fn one(
        &self,
        ctx: &JobContext,
        t: &Target,
        st: &mut RetireState,
        frac: f64,
    ) -> Result<Flow<()>> {
        let db = ctx.library().db();
        let layout = ctx.library().layout();
        let job = ctx.id();
        let key = (t.version_id, t.input);
        let mut again = true;
        let (why, detail) = loop {
            let outcome = match self.decide(ctx, t, st, frac)? {
                Flow::Go(o) => o,
                Flow::Stop(end) => return Ok(Flow::Stop(end)),
            };
            let a = match outcome {
                Outcome::Anchored => return Ok(Flow::Go(())),
                Outcome::Left(why, detail) => break (why, detail),
                Outcome::Link(a) => a,
            };
            if !layout.content_path(&a.hash, &a.ext).is_file() {
                break (
                    Left::BrokenAnchor,
                    format!("移したアンカー {} のファイルがありません", a.id),
                );
            }
            let rec: RecordedInput = serde_json::from_str(&t.recorded)?;
            let mut next = st.clone();
            next.cursor = Some(key);
            let item_id = t.item_id;
            let linked = db.write(move |tx| {
                // 決めたあとに片付けられた・別の行に替わったアンカーには結び付けない
                let same = anchors::get(tx, a.id)?
                    .is_some_and(|now| now.hash == a.hash && now.decoded == a.decoded);
                if !same {
                    return Ok(None);
                }
                let n = save::link_inputs(tx, item_id, a.id, &a.hash, &rec)?;
                next.linked += n as u64;
                save_partial(tx, job, &next.json(), frac, MESSAGE)?;
                Ok(Some(n))
            })?;
            match linked {
                Some(n) => {
                    st.linked += n as u64;
                    st.cursor = Some(key);
                    return Ok(Flow::Go(()));
                }
                None if again => again = false,
                None => {
                    break (
                        Left::OtherAnchor,
                        "移したアンカーが結び付ける前に替わりました".to_string(),
                    );
                }
            }
        };
        ctx.log(&format!(
            "版の入力（版の行 {}・入力 {}）は移せません（{}）：{detail}",
            t.version_id,
            t.input,
            why.as_str()
        ));
        *st.left.entry(why.as_str().to_string()).or_insert(0) += 1;
        st.cursor = Some(key);
        let json = st.json();
        db.write(move |tx| save_partial(tx, job, &json, frac, MESSAGE))?;
        Ok(Flow::Go(()))
    }

    /// 1 件の行き先：あるアンカーに結び付ける・移せない・読み直してアンカーを作って載せる（載せたら数を `st` に足す）。
    fn decide(
        &self,
        ctx: &JobContext,
        t: &Target,
        st: &mut RetireState,
        frac: f64,
    ) -> Result<Flow<Outcome>> {
        let library = ctx.library();
        let db = library.db();
        let left = |why: Left, detail: String| Ok(Flow::Go(Outcome::Left(why, detail)));
        let rec: RecordedInput = match serde_json::from_str(&t.recorded) {
            Ok(rec) => rec,
            Err(e) => return left(Left::BadRecord, e.to_string()),
        };
        let item_id = t.item_id;
        let Some(original) = db.read(|c| versions::original(c, item_id))? else {
            return left(
                Left::NoOriginal,
                format!("アイテム {item_id} の元がありません"),
            );
        };
        if original.hash != rec.source.0 {
            return left(
                Left::NoOriginal,
                format!("アイテム {item_id} の元が記録の元と違います"),
            );
        }
        // 同じ元・読み方・識別子の移したアンカーがあれば、同じ記録のときだけ結び付ける
        let key = anchors::retired_key(&rec.options, &rec.decoded.codec)?;
        let source = rec.source.0;
        if let Some(a) = db.read(|c| anchors::find(c, &source, &key))? {
            if a.decoded == rec.decoded {
                return Ok(Flow::Go(Outcome::Link(Box::new(a))));
            }
            return left(
                Left::OtherAnchor,
                format!("同じ元に別の記録のアンカー（{}）があります", a.id),
            );
        }
        if self
            .engine
            .codecs()
            .decoder_by_id(&rec.decoded.codec)
            .is_none()
        {
            return left(
                Left::NoDecoder,
                format!("記録の読み手 {} がありません", rec.decoded.codec),
            );
        }
        let path = library.layout().content_path(&original.hash, &original.ext);
        if !path.is_file() {
            return left(Left::Missing, path.display().to_string());
        }
        let unit = self.unit(ctx);
        let made = match save::promote_recorded(&self.engine, &unit, &original, &path, &rec)? {
            Flow::Go(Promote::Made(made)) => made,
            Flow::Go(Promote::Mismatch(why)) => return left(Left::Mismatch, why),
            Flow::Go(Promote::Unavailable(why)) => return left(Left::NoDecoder, why),
            Flow::Stop(end) => return Ok(Flow::Stop(end)),
        };
        let size = made.size();
        let mut next = st.clone();
        next.cursor = Some((t.version_id, t.input));
        next.anchored += 1;
        let job = ctx.id();
        let after: save::After = Box::new(move |tx, linked| {
            next.linked += linked as u64;
            save_partial(tx, job, &next.json(), frac, MESSAGE)
        });
        self.at(RetireStage::Commit(t.version_id));
        match save::commit_retired(&unit, made, item_id, &rec, after)? {
            Some(placed) => {
                st.anchored += 1;
                st.linked += placed.linked as u64;
                st.cursor = Some((t.version_id, t.input));
                ctx.log(&format!(
                    "アイテム {item_id} の元の旧版の読み取りを残しました（アンカー {}・{} バイト・版の入力 {} 件）",
                    placed.anchor_id, size, placed.linked
                ));
                Ok(Flow::Go(Outcome::Anchored))
            }
            // 読んでいる間に版がなくなった（ゴミ箱を空にした）・記録が変わった：置いたものは片付けた
            None => left(
                Left::NoOriginal,
                format!("アイテム {item_id} に結び付ける版の入力がなくなりました"),
            ),
        }
    }
}

/// kura-devtools の数え上げ：退役の識別子 `ids` で読んだアンカーのない版の入力の数（識別子 × 版のファイルの状態ごと）と、
/// 移し終えた印・移したアンカーの数・最後の移し替えのジョブ。段 10 で旧版の読み手を外す前に、利用者のライブラリで 0 になった
/// ことを確かめる（利用者の決定）。
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Remaining {
    /// 移し終えた印（退役の識別子の一覧。なければ None）。
    pub done: Option<Vec<String>>,
    /// （記録の識別子, 版のファイルの状態）ごとの数。
    pub by_codec: BTreeMap<String, BTreeMap<String, u64>>,
    /// 合計。
    pub total: u64,
    /// 残っているもののうち先頭の 20 件（アイテム・版）。
    pub examples: Vec<(i64, i64)>,
    /// 移したアンカーの数（読み取りの記録の識別子が `ids` の行）。
    pub retired_anchors: u64,
    /// 移したアンカーのファイルの大きさの合計（バイト。f32 の WAV なので、ステレオ 44.1 kHz で 1 分あたり約 21 MB）。
    pub retired_bytes: u64,
    /// 最後の移し替えのジョブ（状態・結果の JSON）。
    pub last_job: Option<(String, Option<serde_json::Value>)>,
}

/// 残っている対象を数える（全件の走査。手で回す確かめだけに使う）。
pub fn remaining(conn: &Connection, ids: &[&str]) -> Result<Remaining> {
    let list = serde_json::to_string(ids)?;
    let mut out = Remaining {
        done: settings::get(conn, DONE_KEY)?,
        ..Remaining::default()
    };
    let mut stmt = conn.prepare_cached(
        "SELECT CASE WHEN json_valid(i.recorded) THEN json_extract(i.recorded, '$.decoded.codec') END,
                v.file_state, count(*)
           FROM item_version_inputs i JOIN item_versions v ON v.id = i.version_id
          WHERE +i.anchor_id IS NULL
            AND CASE WHEN json_valid(i.recorded) THEN json_extract(i.recorded, '$.decoded.codec') END
                IN (SELECT value FROM json_each(?1))
          GROUP BY 1, 2",
    )?;
    let rows = stmt.query_map([&list], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, i64>(2)?,
        ))
    })?;
    for row in rows {
        let (codec, state, n) = row?;
        let n = n.max(0) as u64;
        *out.by_codec
            .entry(codec)
            .or_default()
            .entry(state)
            .or_insert(0) += n;
        out.total += n;
    }
    let mut stmt = conn.prepare_cached(
        "SELECT v.item_id, v.version
           FROM item_version_inputs i JOIN item_versions v ON v.id = i.version_id
          WHERE +i.anchor_id IS NULL
            AND CASE WHEN json_valid(i.recorded) THEN json_extract(i.recorded, '$.decoded.codec') END
                IN (SELECT value FROM json_each(?1))
          ORDER BY i.version_id, i.input
          LIMIT 20",
    )?;
    let rows = stmt.query_map([&list], |r| Ok((r.get(0)?, r.get(1)?)))?;
    out.examples = rows.collect::<std::result::Result<_, _>>()?;
    out.last_job = conn
        .prepare_cached(
            "SELECT state, result_json FROM jobs WHERE kind = 'retire' ORDER BY id DESC LIMIT 1",
        )?
        .query_row([], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?))
        })
        .optional()?
        .map(|(state, json)| (state, json.and_then(|j| serde_json::from_str(&j).ok())));
    let (anchors, bytes): (i64, i64) = conn
        .prepare_cached(
            "SELECT count(*), coalesce(sum(b.size_bytes), 0)
               FROM edit_anchors a JOIN blobs b ON b.id = a.blob_id
              WHERE CASE WHEN json_valid(a.decoded) THEN json_extract(a.decoded, '$.codec') END
                    IN (SELECT value FROM json_each(?1))",
        )?
        .query_row([&list], |r| Ok((r.get(0)?, r.get(1)?)))?;
    out.retired_anchors = anchors.max(0) as u64;
    out.retired_bytes = bytes.max(0) as u64;
    Ok(out)
}

/// 数える識別子（kura-devtools の `retired`）：固定の組の退役する読み手（[`kura_engine::codec::CodecSet::retiring_ids`]）と、
/// 外した読み手（[`kura_engine::codec::RETIRED_DECODERS`]。アンカーのない版の入力は作り直せない：`DecoderGone`）。
pub fn watched_ids() -> Vec<&'static str> {
    let mut ids = kura_engine::codec::CodecSet::pinned().retiring_ids();
    for id in kura_engine::codec::RETIRED_DECODERS {
        if !ids.contains(id) {
            ids.push(id);
        }
    }
    ids
}

/// ライブラリ `path`（`*.kuralib`）の DB を読み取り専用で開いて、識別子 `ids`（ふつうは [`watched_ids`]）で読んだアンカーのない
/// 版の入力を数える（kura-devtools。アプリを閉じてから回す。ジョブもマイグレーションも動かさない）。版の表のない古い DB は、
/// 先にアプリで開くよう断る。
pub fn remaining_in(path: &std::path::Path, ids: &[&str]) -> Result<Remaining> {
    let db = path.join(crate::library::DB_FILE);
    if !db.is_file() {
        return Err(CoreError::NotFound(format!("{} の DB", path.display())));
    }
    let conn = Connection::open_with_flags(
        &db,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    if crate::db::applied(&conn)? < crate::db::MIGRATIONS.len() {
        return Err(CoreError::Invalid(
            "ライブラリの DB が古い版です。先に今のアプリで開いてください".into(),
        ));
    }
    remaining(&conn, ids)
}
