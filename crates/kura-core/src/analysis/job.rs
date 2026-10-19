//! 解析のジョブ（`JobKind::Analyze`。docs/architecture.md §5、decisions の決定 2・5・7、defaults 9・13・14）。
//!
//! 1 回の実行の流れ：
//! 1. 前の実行が OS のデコーダ（Platform の段）で読んでいる途中で落ちていたら（処理中の印が残っていれば）、そのアイテムを
//!    failed（`crashed`）にして先へ進む（同じファイルで落ち続けない。critic 8）。
//! 2. エンジンの部品（実行の場とコーデックの組）を使う。最初に使うときにここ（ジョブのスレッド）で作られる。コーデックの組から
//!    デコーダの指紋を作って比べ、変わった形式に関わるものを pending に戻す。
//! 3. 投影の版・ドット絵の規則が古い事実の行を、結果の表から作り直す（読まない。500 件ずつ）。
//! 4. 優先の指定（`request_analysis`）のあるものを先に、なければ新しい順に、`pending` を 16 件ずつ取り出して 1 件ずつ解く：
//!    - 同じ中身の結果（計画の要求の ID・版・正規の引数がすべてそろう）があれば読まずに使う。
//!    - ヘッダだけ読んで形を出し、見積もり（値の 2 倍＋デコーダの作業領域）が min(予算/2, 1 GiB) を超えれば確保せずに
//!      failed（`too_large`）。収まれば一括の予算（Class::Bulk）をプールの外で確保し、一括のプールの中で読んで解く（決定 2）。
//!    - 波形の段は `peaks/ab/<hash>.peaks` に一時ファイルから改名して置く（DB に書く前）。
//!    - 16 件か 500 ms ごとに 1 つのトランザクションで書く（`store::write`。ハッシュと pending を条件にする）。
//! 5. 取り込み・サムネイルが来たら（`should_yield`）書きためた分を書いて譲る。一時停止・中止・アプリの終了では、解いている
//!    1 件を捨てて書きためた分を書いて返す（捨てた 1 件は pending のまま。強制終了でも書くまでは pending のまま）。
//!
//! 失敗（`unsupported`・`too_large`・`decode`・`crashed`）は事実の行に記録し、指紋が変わるか利用者が頼む（`retry_analysis`）まで
//! 解き直さない。

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use kura_engine::analysis::image::alpha::{Alpha, AlphaOutput};
use kura_engine::analysis::image::colors::{Colors, ColorsOutput};
use kura_engine::analysis::output::json_from_canonical;
use kura_engine::analysis::{Analysis, AnalysisRegistry, Analyzed, Encoding, registry};
use kura_engine::budget::{Class, value_bytes};
use kura_engine::codec::{CodecSet, DecodeStamp, Tier};
use kura_engine::doc::DecodeOptions;
use kura_engine::{AnalysisRequest, EngineError, Limits, Monitor};

use super::facts::{MediaFacts, ReadRecord, from_format, project};
use super::plan::{
    DecoderFingerprint, Plan, READ_ID, READ_V, RequestKey, plan_for, request_key, stage1, stage2,
};
use super::store::{self, Inflight, ItemWrite, NewResult, Pending, ReadStamp, Write};
use crate::edit::{EngineRuntime, EngineServices};
use crate::import::format_id;
use crate::jobs::{Control, JobContext, JobEnd, JobHandler};
use crate::library::{Layout, hex, parse_hex32};
use crate::time::now_ms;
use crate::{CoreError, Result};

/// 1 回に取り出して、まとめて書く件数。
pub const BATCH: usize = 16;
/// 書きためたものを書く間隔。
pub const FLUSH_INTERVAL: Duration = Duration::from_millis(500);
/// 投影を作り直す 1 回の件数。
pub const REPROJECT_BATCH: usize = 500;
/// 1 件の確保の上限（予算の半分とこれの小さい方。決定 2）。
pub const ITEM_CAP: u64 = 1 << 30;
/// 解析の作業領域の見積もり：値の何倍か（実測：5 分の FLAC で値 106 MB に対しピーク 213 MB、2048² の PSD で 16 MB に対し 31 MB）。
pub const WORK_FACTOR: u64 = 2;
/// 優先の指定を覚える数（直近のもの）。
const PRIORITY_MAX: usize = 64;

/// ジョブとライブラリで共有するもの：先に解くアイテム（インスペクタで選んだもの）。
#[derive(Debug, Default)]
pub struct AnalyzeShared {
    priority: Mutex<Vec<i64>>,
}

impl AnalyzeShared {
    /// 先に解くアイテムを入れ替える（直近の 64 件まで）。
    pub fn prioritize(&self, ids: Vec<i64>) {
        if let Ok(mut slot) = self.priority.lock() {
            let skip = ids.len().saturating_sub(PRIORITY_MAX);
            *slot = ids.into_iter().skip(skip).collect();
        }
    }

    fn priority(&self) -> Vec<i64> {
        self.priority.lock().map(|p| p.clone()).unwrap_or_default()
    }

    fn forget(&self, done: &[i64]) {
        if let Ok(mut slot) = self.priority.lock() {
            slot.retain(|id| !done.contains(id));
        }
    }
}

/// 解析のジョブの処理。
pub struct AnalyzeHandler {
    engine: Arc<EngineServices>,
    shared: Arc<AnalyzeShared>,
}

impl AnalyzeHandler {
    /// このライブラリのエンジンの部品で解析する。
    pub fn new(engine: Arc<EngineServices>, shared: Arc<AnalyzeShared>) -> Self {
        Self { engine, shared }
    }
}

/// ジョブの指示（一時停止・中止・終了）をエンジンの中止に写す。
struct JobMonitor<'a>(&'a JobContext);

impl Monitor for JobMonitor<'_> {
    fn canceled(&self) -> bool {
        self.0.control() != Control::Continue
    }
}

/// 数えたもの（ジョブの結果）。
#[derive(Debug, Default)]
struct Counts {
    done: u64,
    failed: u64,
    reused: u64,
    reprojected: u64,
    skipped: u64,
}

impl Counts {
    fn summary(&self) -> String {
        serde_json::json!({
            "done": self.done,
            "failed": self.failed,
            "reused": self.reused,
            "reprojected": self.reprojected,
            "skipped": self.skipped,
        })
        .to_string()
    }
}

/// 1 件の結果。
enum Step {
    /// 書くもの。`platform` なら処理中の印を付けて読んだ（書くときに印を消す）。
    Write {
        write: Write,
        platform: bool,
        reused: bool,
    },
    /// 中止・一時停止で捨てた（pending のまま）。
    Aborted,
}

/// 1 回の実行の道具。
struct Run<'a> {
    ctx: &'a JobContext,
    layout: Layout,
    runtime: Arc<EngineRuntime>,
    codecs: Arc<CodecSet>,
    registry: AnalysisRegistry,
    limits: Limits,
    buf: Vec<ItemWrite>,
    last_flush: Instant,
    counts: Counts,
}

impl JobHandler for AnalyzeHandler {
    fn run(&self, ctx: &JobContext) -> Result<JobEnd> {
        let mut counts = Counts::default();
        recover_crash(ctx, &mut counts)?;
        let runtime = self.engine.runtime()?.clone();
        let codecs = self.engine.codecs();
        let decoders = DecoderFingerprint::of(&codecs);
        let reset = ctx
            .library()
            .db()
            .write(move |tx| store::sync_decoders(tx, &decoders))?;
        if reset > 0 {
            ctx.log(&format!("デコーダが変わったので {reset} 件を解き直します"));
        }
        let registry =
            registry().map_err(|e| CoreError::Invalid(format!("解析の登録表を作れません: {e}")))?;
        let limits = runtime.bulk(|e| e.limits().clone());
        let mut run = Run {
            ctx,
            layout: ctx.library().layout().clone(),
            runtime,
            codecs,
            registry,
            limits,
            buf: Vec::new(),
            last_flush: Instant::now(),
            counts,
        };
        if let Some(end) = run.reproject()? {
            return Ok(end);
        }
        if let Some(end) = run.analyze_pending(&self.shared)? {
            return Ok(end);
        }
        let c = &run.counts;
        let message = format!(
            "{} 件を解析（読まずに使ったもの {}・解析できなかったもの {}）",
            c.done, c.reused, c.failed
        );
        ctx.log(&message);
        Ok(JobEnd::Done {
            result_json: c.summary(),
            message: Some(message),
        })
    }
}

/// 前の実行が OS のデコーダで読んでいる途中で落ちていたら、そのアイテムを failed（`crashed`）にする（まだ待っていて、同じ
/// 中身のときだけ）。
fn recover_crash(ctx: &JobContext, counts: &mut Counts) -> Result<()> {
    let recovered = ctx.library().db().write(|tx| {
        let Some(mark) = store::take_inflight(tx)? else {
            return Ok(None);
        };
        let Some(hash) = parse_hex32(&mark.hash) else {
            return Ok(None);
        };
        let w = store::write(
            tx,
            &[ItemWrite {
                id: mark.id,
                hash,
                write: Write::Failed {
                    code: "crashed",
                    stamp: None,
                },
            }],
            now_ms(),
        )?;
        Ok(Some((mark.id, w.written)))
    })?;
    if let Some((id, written)) = recovered {
        if written > 0 {
            counts.failed += 1;
        }
        tracing::warn!(
            id,
            "前回、OS のデコーダで読んでいる途中で終わったアイテムを、解析できなかったものにしました"
        );
        ctx.log(&format!(
            "アイテム {id}: 前回 OS のデコーダで読んでいる途中で終わったので、解析できなかったものにしました"
        ));
    }
    Ok(())
}

/// 解析の失敗の分け方（事実の行の error_code）。中止は None。
fn failure_code(err: &EngineError) -> Option<&'static str> {
    match err {
        EngineError::Canceled => None,
        EngineError::Unsupported(_) | EngineError::CodecMissing { .. } => Some("unsupported"),
        EngineError::Limit(_) => Some("too_large"),
        EngineError::AtNode { source, .. } => failure_code(source),
        EngineError::Invalid(_)
        | EngineError::Decode { .. }
        | EngineError::Encode { .. }
        | EngineError::Io(_)
        | EngineError::Internal(_)
        | EngineError::Mismatch(_) => Some("decode"),
    }
}

fn failed(code: &'static str) -> Step {
    Step::Write {
        write: Write::Failed { code, stamp: None },
        platform: false,
        reused: false,
    }
}

fn engine_err(err: EngineError) -> CoreError {
    CoreError::Invalid(format!("解析できませんでした: {err}"))
}

/// 結果の正規の形の行を作る。
fn new_result(a: &Analyzed, in_file: bool) -> Result<NewResult> {
    let digest = a.digest().map_err(engine_err)?;
    Ok(NewResult {
        analysis: a.id.to_string(),
        version: a.version,
        params: super::plan::canonical_string(&a.params)?,
        encoding: match a.encoding {
            Encoding::Json => "json",
            Encoding::Binary => "binary",
        },
        output: (!in_file).then(|| a.output.clone()),
        output_len: a.output.len() as u64,
        digest: digest.0,
    })
}

/// 読み取りの記録の結果の行。
fn read_result(read: &ReadRecord) -> Result<(NewResult, Vec<u8>)> {
    let output = read.to_canonical()?;
    let a = Analyzed {
        id: READ_ID,
        version: READ_V,
        params: serde_json::json!({}),
        encoding: Encoding::Json,
        output: output.clone(),
    };
    Ok((new_result(&a, false)?, output))
}

fn stamp_of(s: &DecodeStamp) -> ReadStamp {
    ReadStamp {
        decoder: Some(s.codec.clone()),
        reproducible: Some(s.reproducible),
        value_digest: Some(s.value_digest.0),
        env: s.env.as_ref().and_then(|e| serde_json::to_string(e).ok()),
    }
}

/// 波形の段をファイルに置く（staging の一時ファイルに書いてから改名する。DB に書く前に呼ぶ）。
fn write_peaks(layout: &Layout, hash_hex: &str, bytes: &[u8]) -> Result<()> {
    let dest = layout.peaks_path(hash_hex);
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let staging = layout.staging_dir();
    std::fs::create_dir_all(&staging)?;
    let tmp = staging.join(format!("peaks-{hash_hex}.part"));
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, &dest)?;
    Ok(())
}

impl Run<'_> {
    /// 書きためたものを書く（`clear_mark` なら処理中の印も消す）。
    fn flush(&mut self, clear_mark: bool) -> Result<()> {
        if self.buf.is_empty() && !clear_mark {
            self.last_flush = Instant::now();
            return Ok(());
        }
        let items = std::mem::take(&mut self.buf);
        let written = self.ctx.library().db().write(move |tx| {
            let w = store::write(tx, &items, now_ms())?;
            if clear_mark {
                store::clear_inflight(tx)?;
            }
            Ok(w)
        })?;
        self.counts.skipped += written.skipped;
        self.last_flush = Instant::now();
        Ok(())
    }

    /// 書きためたものを書いて、処理中の印を付ける（コミットしてから読む）。
    fn flush_and_mark(&mut self, mark: Inflight) -> Result<()> {
        let items = std::mem::take(&mut self.buf);
        let written = self.ctx.library().db().write(move |tx| {
            let w = store::write(tx, &items, now_ms())?;
            store::set_inflight(tx, &mark)?;
            Ok(w)
        })?;
        self.counts.skipped += written.skipped;
        self.last_flush = Instant::now();
        Ok(())
    }

    /// 止める指示・譲る相手を見て、止めるなら書きためた分を書いて終わり方を返す。
    fn stop(&mut self) -> Result<Option<JobEnd>> {
        let end = match self.ctx.control() {
            Control::Pause => Some(JobEnd::Paused),
            Control::Cancel => Some(JobEnd::Canceled {
                result_json: Some(self.counts.summary()),
            }),
            Control::Continue if self.ctx.should_yield() => Some(JobEnd::Yield),
            Control::Continue => None,
        };
        if end.is_some() {
            self.flush(false)?;
        }
        Ok(end)
    }

    /// 投影の版・ドット絵の規則が古い事実の行を、結果の表から作り直す（読まない）。
    fn reproject(&mut self) -> Result<Option<JobEnd>> {
        let mut after = 0;
        loop {
            if let Some(end) = self.stop()? {
                return Ok(Some(end));
            }
            let rows = self
                .ctx
                .library()
                .db()
                .read(|c| store::stale_facts(c, after, REPROJECT_BATCH))?;
            let Some(last) = rows.last() else {
                return Ok(None);
            };
            after = last.blob_id;
            let projected: Vec<(i64, MediaFacts)> = rows
                .into_iter()
                .map(|r| {
                    let format = format_id(&r.ext).unwrap_or_default();
                    let facts = if store::is_from_format(format) {
                        from_format(format)
                    } else {
                        let read = r
                            .results
                            .get(READ_ID)
                            .and_then(|b| ReadRecord::from_canonical(b));
                        project(format, read.as_ref(), &r.results)
                    };
                    (r.blob_id, facts)
                })
                .collect();
            self.counts.reprojected += projected.len() as u64;
            self.ctx.library().db().write(move |tx| {
                let now = now_ms();
                for (blob_id, facts) in &projected {
                    store::write_projection(tx, *blob_id, facts, now)?;
                }
                Ok(())
            })?;
        }
    }

    /// 解析を待つものを解く。止めるなら終わり方を返す。
    fn analyze_pending(&mut self, shared: &AnalyzeShared) -> Result<Option<JobEnd>> {
        loop {
            if let Some(end) = self.stop()? {
                return Ok(Some(end));
            }
            let priority = shared.priority();
            let db = self.ctx.library().db();
            let found = if priority.is_empty() {
                Vec::new()
            } else {
                db.read(|c| store::pending_by_ids(c, &priority))?
            };
            // もう待っていないものと、この回に解くものは優先の指定から外す（残りは次の回に先に解く）
            let mut forget: Vec<i64> = priority
                .iter()
                .copied()
                .filter(|id| !found.iter().any(|p| p.id == *id))
                .collect();
            let mut batch = found;
            batch.truncate(BATCH);
            forget.extend(batch.iter().map(|p| p.id));
            shared.forget(&forget);
            if batch.is_empty() {
                batch = db.read(|c| store::pending_batch(c, BATCH))?;
            }
            if batch.is_empty() {
                return Ok(None);
            }
            for item in &batch {
                if let Some(end) = self.stop()? {
                    return Ok(Some(end));
                }
                match self.analyze(item)? {
                    Step::Aborted => {
                        // 中止・一時停止で捨てた（pending のまま）。処理中の印は消す（落ちたのではない）
                        self.flush(true)?;
                        return Ok(Some(self.stop()?.unwrap_or(JobEnd::Paused)));
                    }
                    Step::Write {
                        write,
                        platform,
                        reused,
                    } => {
                        match &write {
                            Write::Failed { code, .. } => {
                                self.counts.failed += 1;
                                self.ctx.log(&format!(
                                    "アイテム {}: 解析できません（{code}）",
                                    item.id
                                ));
                            }
                            Write::Ready { .. } | Write::NotNeeded => {}
                        }
                        self.counts.done += 1;
                        if reused {
                            self.counts.reused += 1;
                        }
                        self.buf.push(ItemWrite {
                            id: item.id,
                            hash: item.hash,
                            write,
                        });
                        if platform {
                            // OS のデコーダで読んだものは、すぐに書いて印を消す（次の 1 件で落ちても取り違えない）
                            self.flush(true)?;
                        } else if self.buf.len() >= BATCH
                            || self.last_flush.elapsed() >= FLUSH_INTERVAL
                        {
                            self.flush(false)?;
                        }
                    }
                }
            }
            // 次の取り出しで同じものを拾わないように書く
            self.flush(false)?;
            let remaining = db.read(store::count_pending)?;
            let total = self.counts.done + remaining;
            self.ctx.progress(
                self.counts.done as f64 / total.max(1) as f64,
                &format!("{} / {total} 件", self.counts.done),
            );
        }
    }

    /// 1 件を解く（DB には書かない。書くものを返す）。
    fn analyze(&mut self, item: &Pending) -> Result<Step> {
        let Some(format) = format_id(&item.ext) else {
            return Ok(failed("unsupported"));
        };
        match plan_for(item.kind, format) {
            Plan::NotNeeded => Ok(Step::Write {
                write: Write::NotNeeded,
                platform: false,
                reused: false,
            }),
            Plan::FromFormat => Ok(Step::Write {
                write: Write::Ready {
                    results: Vec::new(),
                    keep: Some(Vec::new()),
                    facts: Box::new(from_format(format)),
                    stamp: Some(ReadStamp::default()),
                },
                platform: false,
                reused: false,
            }),
            Plan::Read => self.read_and_analyze(item, format),
        }
    }

    /// 結果の表に計画の要求がすべてそろっていれば、読まずに事実を作る。
    fn reuse(&self, item: &Pending, format: &str) -> Result<Option<MediaFacts>> {
        let stored = self
            .ctx
            .library()
            .db()
            .read(|c| store::stored_results(c, &item.hash))?;
        let by_id: BTreeMap<&str, &store::StoredResult> =
            stored.iter().map(|r| (r.analysis.as_str(), r)).collect();
        let Some(read) = by_id
            .get(READ_ID)
            .filter(|r| r.version == READ_V)
            .and_then(|r| r.output.as_deref())
            .and_then(ReadRecord::from_canonical)
        else {
            return Ok(None);
        };
        let json: BTreeMap<String, Vec<u8>> = stored
            .iter()
            .filter(|r| r.encoding == "json")
            .filter_map(|r| Some((r.analysis.clone(), r.output.clone()?)))
            .collect();
        let alpha: Option<AlphaOutput> = json
            .get(Alpha::ID)
            .and_then(|b| json_from_canonical(b).ok());
        let colors: Option<ColorsOutput> = json
            .get(Colors::ID)
            .and_then(|b| json_from_canonical(b).ok());
        let mut wanted = stage1(&read);
        wanted.extend(stage2(&read, alpha.as_ref(), colors.as_ref()));
        let hash_hex = hex(&item.hash);
        for req in &wanted {
            let key: RequestKey = request_key(&self.registry, req)?;
            let Some(row) = by_id.get(key.id.as_str()) else {
                return Ok(None);
            };
            if row.version != key.v || row.params != key.params {
                return Ok(None);
            }
            // ファイルに置く結果（波形の段）は、ファイルがなければ作り直す
            if row.output.is_none() && !self.layout.peaks_path(&hash_hex).is_file() {
                return Ok(None);
            }
        }
        Ok(Some(project(format, Some(&read), &json)))
    }

    fn read_and_analyze(&mut self, item: &Pending, format: &str) -> Result<Step> {
        if let Some(facts) = self.reuse(item, format)? {
            return Ok(Step::Write {
                write: Write::Ready {
                    results: Vec::new(),
                    keep: None,
                    facts: Box::new(facts),
                    stamp: None,
                },
                platform: false,
                reused: true,
            });
        }
        let path = self.layout.content_path(&item.hash, &item.ext);
        let hash_hex = hex(&item.hash);
        // OS のデコーダ（同じプロセスで動く）を通りうるなら、読む前に処理中の印を付けてコミットする
        let platform = self
            .codecs
            .decoders_for(format)
            .iter()
            .any(|d| d.tier() == Tier::Platform);
        if platform {
            self.flush_and_mark(Inflight {
                id: item.id,
                hash: hash_hex.clone(),
            })?;
        }
        let step = self.decode_and_solve(item, format, &path, &hash_hex)?;
        Ok(match step {
            Step::Write { write, reused, .. } => Step::Write {
                write,
                platform,
                reused,
            },
            Step::Aborted => Step::Aborted,
        })
    }

    fn decode_and_solve(
        &mut self,
        item: &Pending,
        format: &str,
        path: &Path,
        hash_hex: &str,
    ) -> Result<Step> {
        let opts = DecodeOptions::default();
        let (shape, _) = match self.codecs.shape_path(path, format, &opts, &self.limits) {
            Ok(s) => s,
            Err(err) => {
                return Ok(match failure_code(&err) {
                    Some(code) => {
                        self.ctx.log(&format!("アイテム {}: {err}", item.id));
                        failed(code)
                    }
                    None => Step::Aborted,
                });
            }
        };
        let len = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
        let need = value_bytes(&shape)
            .saturating_mul(WORK_FACTOR)
            .saturating_add(self.codecs.decode_file_work_bytes(format, &shape, len));
        let cap = (self.runtime.budget().total() / 2).min(ITEM_CAP);
        if need > cap {
            self.ctx.log(&format!(
                "アイテム {}: 大きすぎるので解析しません（見積もり {need} バイト・上限 {cap} バイト）",
                item.id
            ));
            return Ok(failed("too_large"));
        }
        let mon = JobMonitor(self.ctx);
        let lease = match self.runtime.budget().acquire(need, Class::Bulk, &mon) {
            Ok(lease) => lease,
            Err(err) => {
                return Ok(match failure_code(&err) {
                    Some(code) => failed(code),
                    None => Step::Aborted,
                });
            }
        };
        let codecs = self.codecs.clone();
        let solved = self.runtime.bulk(|engine| {
            let (value, stamp) = codecs.decode_path(path, format, &opts, engine.limits(), &mon)?;
            let read = ReadRecord::from_value(&value, format);
            let first = stage1(&read);
            let mut results = engine.analyze_many(&value, &first, &mon);
            let typed = |id: &str| {
                first
                    .iter()
                    .zip(&results)
                    .find(|(r, _)| r.id == id)
                    .and_then(|(_, a)| a.as_ref().ok())
                    .map(|a| a.output.clone())
            };
            let alpha: Option<AlphaOutput> =
                typed(Alpha::ID).and_then(|b| json_from_canonical(&b).ok());
            let colors: Option<ColorsOutput> =
                typed(Colors::ID).and_then(|b| json_from_canonical(&b).ok());
            let second = stage2(&read, alpha.as_ref(), colors.as_ref());
            results.extend(engine.analyze_many(&value, &second, &mon));
            let requests: Vec<AnalysisRequest> = first.into_iter().chain(second).collect();
            Ok::<_, EngineError>((read, stamp, requests, results))
        });
        drop(lease);
        let (read, stamp, requests, results) = match solved {
            Ok(s) => s,
            Err(err) => {
                return Ok(match failure_code(&err) {
                    Some(code) => {
                        self.ctx.log(&format!("アイテム {}: {err}", item.id));
                        failed(code)
                    }
                    None => Step::Aborted,
                });
            }
        };
        let mut rows = Vec::new();
        let mut keep = vec![READ_ID.to_string()];
        let mut json = BTreeMap::new();
        for (req, r) in requests.iter().zip(results) {
            match r {
                Ok(a) => {
                    let in_file = a.encoding == Encoding::Binary;
                    if in_file {
                        write_peaks(&self.layout, hash_hex, &a.output)?;
                    } else {
                        json.insert(a.id.to_string(), a.output.clone());
                    }
                    rows.push(new_result(&a, in_file)?);
                    keep.push(a.id.to_string());
                }
                Err(EngineError::Canceled) => return Ok(Step::Aborted),
                Err(err) => {
                    self.ctx.log(&format!(
                        "アイテム {}: {} を解けません: {err}",
                        item.id, req.id
                    ));
                }
            }
        }
        let (read_row, read_bytes) = read_result(&read)?;
        json.insert(READ_ID.to_string(), read_bytes);
        rows.push(read_row);
        let facts = project(format, Some(&read), &json);
        Ok(Step::Write {
            write: Write::Ready {
                results: rows,
                keep: Some(keep),
                facts: Box::new(facts),
                stamp: Some(stamp_of(&stamp)),
            },
            platform: false,
            reused: false,
        })
    }
}
