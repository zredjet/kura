//! 解析のジョブ（`JobKind::Analyze`。docs/architecture.md §5、decisions の決定 2・5・7・15、defaults 9・13・14）。
//!
//! 1 回の実行の流れ：
//! 1. 前の実行が OS のデコーダ（Platform の段）を通りうる形式を読んでいる途中で落ちていたら（処理中の印が残っていれば）、
//!    そのアイテムを failed（`crashed`）にして先へ進む（同じファイルで落ち続けない。critic 8）。
//! 2. エンジンの部品（実行の場とコーデックの組）を使う。最初に使うときにここ（ジョブのスレッド）で作られる。コーデックの組から
//!    デコーダの指紋を作って比べ、変わった形式に関わるものを pending に戻す。
//! 3. 起動のスレッドが計画の指紋の変化で作り直しの印を置いていれば（投影の版・ドット絵の規則が変わった）、古い事実の行を
//!    結果の表から作り直し（読まない。500 件ずつ）、終えたら印を消す。印がなければ事実の表を走査しない。
//! 4. 優先の指定（`request_analysis`）のあるものを先に、なければ新しい順に、`pending` を 16 件ずつ取り出して 1 件ずつ解く：
//!    - 同じ中身の結果（計画の要求の ID・版・正規の引数がすべてそろう）があり、それを読んだデコーダが今の組にあれば、読まずに
//!      使う（デコーダの識別子が変わって pending に戻したものは読み直す）。
//!    - JPEG は読まずに形式から書く（決定 5）。HEIC・HEIF は見出し（Platform の段の形。画素を展開しない）を読み、アルファを
//!      持つ形のものだけを先へ進めて読み、ほかと見出しを読めないものは形式から書く（決定 15）。見出しも OS のデコーダで読む
//!      ので、処理中の印を付けてから読む。
//!    - ヘッダだけ読んで形を出し、見積もりが min(予算/2, 1 GiB) を超えれば確保せずに failed（`too_large`）。収まれば一括の
//!      予算（Class::Bulk）をプールの外で確保し、一括のプールの中で読んで解く（決定 2）。画像の見積もりは値の 2 倍＋デコーダの
//!      作業領域。音声は値を持たずに流して解く（M13 段 2。`solve_stream`）：見積もりはデコーダの流れの作業領域＋窓 2 つ＋結果＋
//!      ダイジェストの作業領域で、長さによらない（`too_large` は `Limits`（48 kHz で 3 時間）を超えるときと、見積もりが上限を
//!      超えるとき（多チャンネルの窓など）だけ）。見積もりと形を読む前の確保は段ごと（実際に読む段）に数える：形は段の順に
//!      読み、段ごとに、その段で形を読む分（ファイル全体を読む段（MP3・AudioToolbox）は長さ、自作の FLAC・Ogg・m4a の段は
//!      読み手が持つタグ・箱・表）を確保してから読む。流して読む見積もりは形を読めた段のもので、その段が `Unsupported` を返せば、
//!      次の段で形を読み直して確保し直す（M13 段 2 の検証の直し）。
//!    - 波形の段は `peaks/ab/<hash>.peaks` に一時ファイルから改名して置く（DB に書く前）。
//!    - 16 件か 500 ms ごとに 1 つのトランザクションで書く（`store::write`。ハッシュと pending を条件にする）。
//! 5. 取り込み・サムネイルが来たら（`should_yield`）書きためた分を書いて譲る。一時停止・中止・アプリの終了では、解いている
//!    1 件を捨てて書きためた分を書いて返す（捨てた 1 件は pending のまま、処理中の印も消す）。強制終了（電源断・ほかの部品の
//!    異常終了を含む）でも、書くまでは pending のまま。ただし OS のデコーダ（Platform の段）を通りうる形式（コーデックの組に
//!    その形式の Platform のデコーダがあるもの。どの段が読むかは読む前に分からないので、固定の段が先に読む WAV・PSD なども）
//!    を読んでいた 1 件は処理中の印が残るので、次に動いたときに 1 の手順で failed（`crashed`）になる（印は落ちたのか止められた
//!    のかを見分けない。`retry_analysis` で戻せる。M11 の確かめ C1）。
//!
//! 失敗（`unsupported`・`too_large`・`decode`・`crashed`）は事実の行に記録し、指紋が変わるか利用者が頼む（`retry_analysis`）まで
//! 解き直さない。ただし SVG の働き手を使えない（起動できない・別のビルド。`svg_worker::is_unavailable`。M11 の 3 回目の確認
//! W2）のはアイテムのせいではないので、失敗にせず（pending のまま）、書きためた分を書いてジョブをその場で失敗にして止める
//! （同じアイテムを拾い続けない。次にジョブが積まれたとき（ライブラリを開く・取り込む・インスペクタで選ぶなど）にやり直す）。

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use kura_engine::analysis::audio::peaks::{Peaks, PeaksAnalysis};
use kura_engine::analysis::image::alpha::{Alpha, AlphaOutput};
use kura_engine::analysis::image::colors::{Colors, ColorsOutput};
use kura_engine::analysis::output::json_from_canonical;
use kura_engine::analysis::{
    Analysis, AnalysisOutput, AnalysisRegistry, Analyzed, Encoding, registry,
};
use kura_engine::audio::digest::PcmTreeDigest;
use kura_engine::budget::{Class, Lease, value_bytes};
use kura_engine::codec::stream::stream_window_bytes;
use kura_engine::codec::{CodecSet, DecodeStamp, PickedStage, Tier};
use kura_engine::doc::DecodeOptions;
use kura_engine::{AnalysisRequest, EngineError, Limits, Monitor};

use super::facts::{MediaFacts, ReadRecord, from_format, project};
use super::plan::{
    DecoderFingerprint, Plan, READ_ID, READ_V, RequestKey, from_format_unless_read, peaks_params,
    plan_for, read_after_header, request_key, stage1, stage2, stream_requests,
};
use super::store::{self, Inflight, ItemWrite, NewResult, Pending, ReadStamp, Write, error_code};
use crate::codec_ext::svg_worker;
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
    /// 読んで解いた 1 件ごとの時間（ミリ秒。ヘッダの読み取りから結果を作るまで）。
    read_ms: Vec<f64>,
    /// 見出しで決める形式（HEIC・HEIF）の見出しの読み取りの 1 件ごとの時間（ミリ秒。決定 15）。
    header_ms: Vec<f64>,
}

/// 時間の列の要約（件数・p50・p95・最大。ミリ秒）。
fn timing(ms: &[f64]) -> serde_json::Value {
    let mut ms = ms.to_vec();
    ms.sort_by(f64::total_cmp);
    serde_json::json!({
        "count": ms.len(),
        "p50": percentile(&ms, 50),
        "p95": percentile(&ms, 95),
        "max": ms.last().copied().unwrap_or(0.0),
    })
}

/// 並べた列の p（0〜100）パーセンタイル（最も近い順位）。
fn percentile(sorted: &[f64], p: usize) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let rank = (sorted.len() * p).div_ceil(100).clamp(1, sorted.len());
    sorted[rank - 1]
}

impl Counts {
    fn summary(&self) -> String {
        serde_json::json!({
            "done": self.done,
            "failed": self.failed,
            "reused": self.reused,
            "reprojected": self.reprojected,
            "skipped": self.skipped,
            "readMs": timing(&self.read_ms),
            "headerMs": timing(&self.header_ms),
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
    /// SVG の働き手を使えない（一時的な誤り。pending のまま。ジョブを止める。理由）。
    Unavailable(String),
}

/// 段の順に形を読んだ結果（`Run::read_shape`）。
enum Header {
    /// 読めた形と、読んだ段。
    Read(kura_engine::Shape, PickedStage),
    /// 形を読めなかった。
    Failed(EngineError),
    /// 段の予算の確保で断った（失敗の符号。`too_large`・確保の失敗。None は中止）。
    Refused(Option<&'static str>),
}

impl Header {
    /// 確保で断ったときの手順と、読もうとしたか（読み取りの時間に数えるか）。
    fn refused(code: Option<&'static str>) -> (Step, bool) {
        match code {
            Some(code) => (failed(code), true),
            None => (Step::Aborted, false),
        }
    }
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

/// 前の実行が OS のデコーダを通りうる形式を読んでいる途中で落ちていたら、そのアイテムを failed（`crashed`）にする（まだ
/// 待っていて、同じ中身のときだけ）。
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
                    code: error_code::CRASHED,
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
            "前回、OS のデコーダを通りうる形式を読んでいる途中で終わったアイテムを、解析できなかったものにしました"
        );
        ctx.log(&format!(
            "アイテム {id}: 前回 OS のデコーダを通りうる形式を読んでいる途中で終わったので、解析できなかったものにしました"
        ));
    }
    Ok(())
}

/// 解析の失敗の分け方（事実の行の error_code）。中止は None。
fn failure_code(err: &EngineError) -> Option<&'static str> {
    match err {
        EngineError::Canceled => None,
        EngineError::Unsupported(_) | EngineError::CodecMissing { .. } => {
            Some(error_code::UNSUPPORTED)
        }
        EngineError::Limit(_) => Some(error_code::TOO_LARGE),
        EngineError::AtNode { source, .. } => failure_code(source),
        EngineError::Invalid(_)
        | EngineError::Decode { .. }
        | EngineError::Encode { .. }
        | EngineError::Io(_)
        | EngineError::Internal(_)
        | EngineError::Mismatch(_) => Some(error_code::DECODE),
    }
}

fn failed(code: &'static str) -> Step {
    Step::Write {
        write: Write::Failed { code, stamp: None },
        platform: false,
        reused: false,
    }
}

/// 読まずに形式から書く（JPEG と、見出しでアルファがないと分かった・見出しを読めなかった HEIC・HEIF）。読み取りの記録の列は
/// 空にし、結果の行は残さない。
fn from_format_step(format: &str) -> Step {
    Step::Write {
        write: Write::Ready {
            results: Vec::new(),
            keep: Some(Vec::new()),
            facts: Box::new(from_format(format)),
            stamp: Some(ReadStamp::default()),
        },
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

/// 1 件の見積もり：値の [`WORK_FACTOR`] 倍 ＋ デコーダの作業領域（音声のデコーダは作業領域を値と同じに見積もるので、音声は
/// 値の 3 倍。値は フレーム数 × チャンネル数 × 4 バイト）。
fn estimate(codecs: &CodecSet, format: &str, shape: &kura_engine::Shape, len: u64) -> u64 {
    value_bytes(shape)
        .saturating_mul(WORK_FACTOR)
        .saturating_add(codecs.decode_file_work_bytes(format, shape, len))
}

/// 音声を流して解く 1 件の見積もり（M13 段 2）：デコーダの流れの作業領域 `decoder_work`（実際に読む段の
/// `CodecSet::stage_stream_work_bytes`。読み手が持つ埋め込みの画像などを含む）＋ 窓の作業列 × 2（デコードの窓と、解析の
/// 中の半端な残り）＋ 要求ごとの結果の見積もり ＋ ダイジェストの作業領域（一括のプールの仕事の数ぶん）。長さが上限しか
/// 分からない形は上限で数える。
pub fn estimate_stream(
    decoder_work: u64,
    registry: &AnalysisRegistry,
    shape: &kura_engine::Shape,
    reqs: &[AnalysisRequest],
    tasks: u64,
) -> u64 {
    let kura_engine::Shape::Audio {
        channels, frames, ..
    } = *shape
    else {
        return u64::MAX;
    };
    let results = reqs
        .iter()
        .map(|r| {
            registry
                .resolve(&r.id, r.v)
                .ok()
                .and_then(|a| {
                    let params = a.check_params(&r.params).ok()?;
                    a.estimate(&params, shape).ok()
                })
                .map_or(0, |e| e.bytes)
        })
        .fold(0u64, u64::saturating_add);
    decoder_work
        .saturating_add(stream_window_bytes(usize::from(channels), frames.max()).saturating_mul(2))
        .saturating_add(results)
        .saturating_add(PcmTreeDigest::work_bytes(usize::from(channels), tasks))
}

/// 1 件の確保の上限：min(予算の全体/2, [`ITEM_CAP`])（決定 2）。予算は min(物理メモリ/4, 4 GiB) なので、物理メモリが 8 GB
/// 以上なら 1 GiB。
fn item_cap(total: u64) -> u64 {
    (total / 2).min(ITEM_CAP)
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

    /// 投影の版・ドット絵の規則が古い事実の行を、結果の表から作り直す（読まない）。作り直しの印があるときだけ走査し
    /// （一度きりの全件の走査）、終えたら印を消す。途中で止めたら印は残り、次の実行で初めから走査し直す。
    fn reproject(&mut self) -> Result<Option<JobEnd>> {
        if !self.ctx.library().db().read(store::reproject_due)? {
            return Ok(None);
        }
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
                self.ctx
                    .library()
                    .db()
                    .write(|tx| store::clear_reproject(tx))?;
                return Ok(None);
            };
            after = last.blob_id;
            let projected: Vec<(i64, MediaFacts)> = rows
                .into_iter()
                .map(|r| {
                    let format = format_id(&r.ext).unwrap_or_default();
                    let read = r
                        .results
                        .get(READ_ID)
                        .and_then(|b| ReadRecord::from_canonical(b));
                    // 読まなかったもの（JPEG・見出しで決めた HEIC・HEIF）は形式から、読んだものは結果の表から
                    let facts = match read {
                        None if from_format_unless_read(format) => from_format(format),
                        read => project(format, read.as_ref(), &r.results),
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
                    Step::Unavailable(why) => {
                        // SVG の働き手を使えない：失敗にせず（pending のまま）、書きためた分を書いて処理中の印を消し、ジョブを
                        // 止める（同じアイテムを拾い続けない）
                        self.flush(true)?;
                        let message = svg_worker::job_stopped("解析", why);
                        self.ctx.log(&format!("アイテム {}: {message}", item.id));
                        tracing::warn!(id = item.id, "{message}");
                        return Err(CoreError::Tool(message));
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
                            // 印を付けて読んだものは、すぐに書いて印を消す（次の 1 件で落ちても取り違えない）
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
        let (Some(format), Some(kind)) = (format_id(&item.ext), item.kind) else {
            return Ok(failed(error_code::UNSUPPORTED));
        };
        match plan_for(kind, format) {
            Plan::NotNeeded => Ok(Step::Write {
                write: Write::NotNeeded,
                platform: false,
                reused: false,
            }),
            Plan::FromFormat => Ok(from_format_step(format)),
            Plan::ByHeader => self.read_and_analyze(item, format, true),
            Plan::Read => self.read_and_analyze(item, format, false),
        }
    }

    /// 結果の表に計画の要求がすべてそろい、それを読んだデコーダが今の組でこの形式を読むもののどれかなら、読まずに事実を作る。
    /// デコーダが今の組にない（デコーダの指紋で pending に戻したもの）・分からないものは読み直す（docs/data-model.md
    /// 「解析の読み書き」の「デコーダの識別子が変わったときだけ解き直す」）。
    fn reuse(&self, item: &Pending, format: &str) -> Result<Option<MediaFacts>> {
        let (decoder, stored) = self.ctx.library().db().read(|c| {
            Ok((
                store::read_decoder(c, &item.hash)?,
                store::stored_results(c, &item.hash)?,
            ))
        })?;
        let current = decoder.is_some_and(|id| {
            self.codecs
                .decoders_for(format)
                .iter()
                .any(|d| d.id() == id)
        });
        if !current {
            return Ok(None);
        }
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

    /// 読んで解く。見出しで決める形式（`by_header`。HEIC・HEIF）は、見出しの形でアルファを持つと分かったものだけを読む。
    fn read_and_analyze(&mut self, item: &Pending, format: &str, by_header: bool) -> Result<Step> {
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
        // OS のデコーダ（同じプロセスで動く）を通りうるなら、読む前に処理中の印を付けてコミットする（見出しだけを読むときも。
        // 固定の段が読めば OS のデコーダは呼ばれないが、どの段が読むかは読む前に分からない。M11 の確かめ C1）
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
        let started = Instant::now();
        let (step, read) = match self.decode_and_solve(item, format, &path, &hash_hex, by_header) {
            Ok(solved) => solved,
            Err(err) => {
                // 落ちたのではない（波形の段を書けないなど）ので、処理中の印は消してからジョブを失敗にする
                if platform
                    && let Err(clear) = self
                        .ctx
                        .library()
                        .db()
                        .write(|tx| store::clear_inflight(tx))
                {
                    tracing::warn!(%clear, "解析の処理中の印を消せませんでした");
                }
                return Err(err);
            }
        };
        if read {
            self.counts
                .read_ms
                .push(started.elapsed().as_secs_f64() * 1000.0);
        }
        Ok(match step {
            Step::Write { write, reused, .. } => Step::Write {
                write,
                platform,
                reused,
            },
            Step::Aborted => Step::Aborted,
            Step::Unavailable(why) => Step::Unavailable(why),
        })
    }

    /// 段 `stage` で形を読む分の予算を確保する（M13 段 2 の検証の直し：段ごとに、その段で形を読む分だけ。0 なら確保しない）。
    /// 上限を超えれば `too_large`、確保できなければその失敗の符号（中止は None）を返す。
    fn shape_lease(
        &self,
        item_id: i64,
        stage: &PickedStage,
    ) -> std::result::Result<Option<Lease>, Option<&'static str>> {
        let need = stage.shape_bytes;
        if need == 0 {
            return Ok(None);
        }
        let cap = item_cap(self.runtime.budget().total());
        if need > cap {
            self.ctx.log(&format!(
                "アイテム {item_id}: 形を読むのに大きすぎるので解析しません（{} が {need} バイト・上限 {cap} バイト）",
                stage.id
            ));
            return Err(Some(error_code::TOO_LARGE));
        }
        self.runtime
            .budget()
            .acquire(need, Class::Bulk, &JobMonitor(self.ctx))
            .map(Some)
            .map_err(|err| failure_code(&err))
    }

    /// 段の順に形を読む（`after` の段より後ろから。`CodecSet::shape_path_staged`）。段ごとに、その段で形を読む分の予算を
    /// 確保し直してから読む（前の段の分は返す）。形を読む子（別のプロセスで読む SVG。決定 23）も、ジョブの中止で止める。
    fn read_shape(&self, item_id: i64, format: &str, path: &Path, after: Option<&str>) -> Header {
        let opts = DecodeOptions::default();
        let mon = JobMonitor(self.ctx);
        let mut lease: Option<Lease> = None;
        let mut refused = None;
        let read = self.codecs.shape_path_staged(
            path,
            format,
            &opts,
            &self.limits,
            &mon,
            after,
            &mut |stage| {
                lease = None;
                match self.shape_lease(item_id, stage) {
                    Ok(l) => {
                        lease = l;
                        Ok(())
                    }
                    Err(code) => {
                        refused = Some(code);
                        Err(EngineError::Canceled)
                    }
                }
            },
        );
        drop(lease);
        if let Some(code) = refused {
            return Header::Refused(code);
        }
        match read {
            Ok((shape, stage)) => Header::Read(shape, stage),
            Err(err) => Header::Failed(err),
        }
    }

    /// ヘッダ（形）を読んで、読むなら読んで解く。見出しで決める形式（`by_header`）は、形がアルファを持たなければ・形を
    /// 読めなければ（中止を除く）読まずに形式から書く。返す真偽は中身を読もうとしたか（読み取りの時間に数えるか）。
    fn decode_and_solve(
        &mut self,
        item: &Pending,
        format: &str,
        path: &Path,
        hash_hex: &str,
        by_header: bool,
    ) -> Result<(Step, bool)> {
        let started = Instant::now();
        // 形を読むのにファイル全体を読む段（MP3・AudioToolbox）・読み手が埋め込みの画像などを持つ段は、その段で形を読む
        // 前にその分を確保する（M13 段 2 とその検証の直し）
        let header = self.read_shape(item.id, format, path, None);
        if by_header && !matches!(header, Header::Refused(..)) {
            self.counts
                .header_ms
                .push(started.elapsed().as_secs_f64() * 1000.0);
        }
        let (shape, stage) = match header {
            Header::Read(shape, stage) => (shape, stage),
            Header::Refused(code) => return Ok(Header::refused(code)),
            // SVG の働き手を使えない：アイテムのせいではない（失敗にも、形式から書くことにもしない）
            Header::Failed(err) if svg_worker::is_unavailable(&err) => {
                return Ok((Step::Unavailable(err.to_string()), false));
            }
            Header::Failed(err) => {
                return Ok(match failure_code(&err) {
                    Some(_) if by_header => {
                        self.ctx.log(&format!(
                            "アイテム {}: 見出しを読めないので形式から書きます（{err}）",
                            item.id
                        ));
                        (from_format_step(format), false)
                    }
                    Some(code) => {
                        self.ctx.log(&format!("アイテム {}: {err}", item.id));
                        (failed(code), true)
                    }
                    None => (Step::Aborted, false),
                });
            }
        };
        if by_header && !read_after_header(&shape) {
            return Ok((from_format_step(format), false));
        }
        if matches!(shape, kura_engine::Shape::Audio { .. }) {
            return Ok((
                self.solve_stream(item, format, path, hash_hex, &shape, stage)?,
                true,
            ));
        }
        Ok((self.solve(item, format, path, hash_hex, &shape)?, true))
    }

    /// 音声を流して解く（M13 段 2）：形から要求を決めて見積もり、一括の予算で確保して、一括のプールの中で流れのデコードを
    /// 解析の扇（`Engine::analyze_stream`）に流す。値は持たない。結果の行・波形の段のファイル・事実は `solve` と同じ形。
    ///
    /// 見積もりは、形を読んだ段 `stage`（実際に読む段）の流れの作業領域で数える（M13 段 2 の検証の直し：段の最大で数えると、
    /// アプリの組では WAV・AIFF を後ろの段の AudioToolbox がファイルの長さで見積もり、Limits の中の長い WAV が too_large に
    /// なっていた）。その段が流して読む途中で `Unsupported` を返したら、次の段で形を読み直し、その段の見積もりで確保し直して
    /// 解き直す（`CodecSet::decode_stream_path` が次の段へ回るのと同じ段の順。要求は最初に読んだ形で決めたまま。扇は
    /// `begin` で読み始めた形に作り直す）。
    fn solve_stream(
        &mut self,
        item: &Pending,
        format: &str,
        path: &Path,
        hash_hex: &str,
        shape: &kura_engine::Shape,
        stage: PickedStage,
    ) -> Result<Step> {
        let opts = DecodeOptions::default();
        let reqs = stream_requests(shape);
        let len = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
        let tasks = self.runtime.bulk_threads() as u64;
        let cap = item_cap(self.runtime.budget().total());
        let (mut shape, mut stage) = (*shape, stage);
        loop {
            let work = self
                .codecs
                .stage_stream_work_bytes(&stage, format, &shape, len);
            // 流して読む段も形を読み直すので、形を読む分も覆う
            let need =
                estimate_stream(work, &self.registry, &shape, &reqs, tasks).max(stage.shape_bytes);
            if need > cap {
                self.ctx.log(&format!(
                    "アイテム {}: 大きすぎるので解析しません（{} の見積もり {need} バイト・上限 {cap} バイト）",
                    item.id, stage.id
                ));
                return Ok(failed(error_code::TOO_LARGE));
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
            let registry = &self.registry;
            let id = stage.id;
            let streamed = shape;
            let solved = self.runtime.bulk(|engine| {
                let mut fan = engine.analyze_stream(&reqs, &streamed);
                let stamp = codecs.decode_stream_with_path(
                    id,
                    path,
                    format,
                    &opts,
                    engine.limits(),
                    &mut fan,
                    &mon,
                )?;
                let end = fan
                    .end()
                    .cloned()
                    .ok_or_else(|| EngineError::Internal("流れの終わりがありません".into()))?;
                let kura_engine::Shape::Audio { rate, channels, .. } = *fan.input() else {
                    return Err(EngineError::Internal("流れの形が音声ではありません".into()));
                };
                let read =
                    ReadRecord::from_audio(rate, channels, end.frames, end.meta.source, format);
                let mut results = fan.finish(&mon);
                let requests = stage1(&read);
                // 長さが上限しか分からなかった形：波形の段を終わりの長さの引数に合わせる
                for (req, r) in reqs.iter().zip(results.iter_mut()) {
                    if req.id != PeaksAnalysis::ID {
                        continue;
                    }
                    if let Ok(a) = r {
                        let want = peaks_params(end.frames);
                        let refit = coarsen_peaks(registry, a, &want);
                        *r = refit;
                    }
                }
                Ok::<_, EngineError>((read, stamp, requests, results))
            });
            drop(lease);
            let err = match solved {
                Ok((read, stamp, requests, results)) => {
                    return self
                        .finish_solved(item, format, hash_hex, read, stamp, &requests, results);
                }
                Err(err) => err,
            };
            if let EngineError::Unsupported(_) = err {
                // この段では読めない：次の段（あれば）で形を読み直す
                match self.read_shape(item.id, format, path, Some(id)) {
                    Header::Read(next_shape, next_stage) => {
                        shape = next_shape;
                        stage = next_stage;
                        continue;
                    }
                    Header::Refused(code) => return Ok(Header::refused(code).0),
                    // 次の段がない・次の段も形を読めない（Unsupported）：この段の誤りで失敗にする
                    Header::Failed(EngineError::Unsupported(_)) => {}
                    Header::Failed(next) => {
                        return Ok(match failure_code(&next) {
                            Some(code) => {
                                self.ctx.log(&format!("アイテム {}: {next}", item.id));
                                failed(code)
                            }
                            None => Step::Aborted,
                        });
                    }
                }
            }
            return Ok(match failure_code(&err) {
                Some(code) => {
                    self.ctx.log(&format!("アイテム {}: {err}", item.id));
                    failed(code)
                }
                None => Step::Aborted,
            });
        }
    }

    /// 形 `shape` を見積もって予算を確保し、読んで解く。
    fn solve(
        &mut self,
        item: &Pending,
        format: &str,
        path: &Path,
        hash_hex: &str,
        shape: &kura_engine::Shape,
    ) -> Result<Step> {
        let opts = DecodeOptions::default();
        let len = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
        let need = estimate(&self.codecs, format, shape, len);
        let cap = item_cap(self.runtime.budget().total());
        if need > cap {
            self.ctx.log(&format!(
                "アイテム {}: 大きすぎるので解析しません（見積もり {need} バイト・上限 {cap} バイト）",
                item.id
            ));
            return Ok(failed(error_code::TOO_LARGE));
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
            Err(err) if svg_worker::is_unavailable(&err) => {
                return Ok(Step::Unavailable(err.to_string()));
            }
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
        self.finish_solved(item, format, hash_hex, read, stamp, &requests, results)
    }

    /// 解いた結果から、結果の行・波形の段のファイル・事実を作る（`solve`・`solve_stream` が通る）。
    #[allow(clippy::too_many_arguments)]
    fn finish_solved(
        &mut self,
        item: &Pending,
        format: &str,
        hash_hex: &str,
        read: ReadRecord,
        stamp: DecodeStamp,
        requests: &[AnalysisRequest],
        results: Vec<std::result::Result<Analyzed, EngineError>>,
    ) -> Result<Step> {
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

/// 最も細かい段から作った波形の段の結果を、引数 `want`（正規の形にする前の `peaks_params`）の段に合わせる
/// （`Peaks::coarsen`。細かい段を落とすだけで、標本から `compute(want)` したものと同じ）。引数が同じならそのまま。
fn coarsen_peaks(
    registry: &AnalysisRegistry,
    a: &Analyzed,
    want: &serde_json::Value,
) -> std::result::Result<Analyzed, EngineError> {
    let d = registry
        .resolve(a.id, Some(a.version))
        .map_err(|i| EngineError::Internal(i.message))?;
    let params = d.check_params(want).map_err(EngineError::Invalid)?;
    if params == a.params {
        return Ok(a.clone());
    }
    let finest = params
        .get("finest")
        .and_then(serde_json::Value::as_u64)
        .and_then(|f| u32::try_from(f).ok())
        .ok_or_else(|| EngineError::Internal("波形の段の引数が読めません".into()))?;
    let peaks = Peaks::from_canonical(&a.output)?.coarsen(finest)?;
    Ok(Analyzed {
        id: a.id,
        version: a.version,
        params,
        encoding: a.encoding,
        output: peaks.to_canonical()?,
    })
}

#[cfg(test)]
mod tests {
    use kura_engine::{Dim, EngineError, Shape};

    use super::super::plan::stream_requests;
    use super::store::error_code;
    use std::sync::Arc;

    use kura_engine::codec::platform::PlatformAudioDecoder;
    use kura_engine::codec::{Held, PickedStage};
    use kura_engine::testing::platform::{FakeAudio, FakeAudioBackend};

    use super::{CodecSet, ITEM_CAP, estimate_stream, failure_code, item_cap, percentile};

    /// 解析の長さの範囲（M13 段 2。docs/requirements.md の「解析の長さの範囲」）：音声は流して解くので、1 件の見積もりは長さに
    /// よらない。60 分・3 時間の 48 kHz ステレオ、96 kHz ステレオ・32 チャンネル（`Limits` の中の長さ）、上限しか分からない
    /// MP3 の形でも 1 件の上限（1 GiB。物理メモリが 8 GB 以上）より十分小さい。解析しないのは `Limits` を超えるもの
    /// （48 kHz で 3 時間 ＋ 1 秒など）だけ。
    #[test]
    fn long_audio_fits_up_to_the_engine_limits() {
        let codecs = CodecSet::pinned();
        let registry = kura_engine::analysis::registry().unwrap();
        let limits = kura_engine::Limits::default();
        let audio = |rate: u32, channels: u16, secs: u64, exact: bool| Shape::Audio {
            rate,
            channels,
            frames: if exact {
                Dim::Exact(u64::from(rate) * secs)
            } else {
                Dim::AtMost(u64::from(rate) * secs)
            },
        };
        assert_eq!(item_cap(4 << 30), ITEM_CAP);
        assert_eq!(item_cap(2 << 30), ITEM_CAP, "物理メモリ 8 GB");
        assert_eq!(item_cap(1 << 30), 512 << 20);
        // アプリの組と同じく、後ろの段に WAV・AIFF・FLAC・Ogg・m4a も読む Platform の音声デコーダ（ファイルの長さで見積もる）
        // を置いた組（M13 段 2 の検証の直し）
        let mut app = CodecSet::pinned();
        let mut backend = FakeAudioBackend::new(FakeAudio::new(48_000, vec![vec![0.0; 4]; 2]));
        backend.formats = vec!["wav", "aiff", "flac", "ogg", "m4a", "aac"];
        app.add_decoder(Arc::new(PlatformAudioDecoder::new(backend)))
            .unwrap();
        // 見積もりは実際に読む段（前の段）で数える
        let need_in = |set: &CodecSet, format: &str, shape: &Shape, len: u64| {
            let stage = PickedStage {
                id: set.decoders_for(format)[0].id(),
                held: Held::default(),
                shape_bytes: 0,
            };
            estimate_stream(
                set.stage_stream_work_bytes(&stage, format, shape, len),
                &registry,
                shape,
                &stream_requests(shape),
                16,
            )
        };
        let need = |format: &str, shape: &Shape, len: u64| need_in(&codecs, format, shape, len);
        // WAV・AIFF はファイルの長さ（値の半分〜同じ）を渡す
        let pcm = |shape: &Shape, bytes: u64| match shape {
            Shape::Audio {
                channels, frames, ..
            } => frames.max() * u64::from(*channels) * bytes,
            _ => 0,
        };
        for (format, shape, len) in [
            ("wav", audio(48_000, 2, 3_600, true), Some(2)),
            ("flac", audio(48_000, 2, 3_600, true), None),
            ("wav", audio(48_000, 2, 3 * 3_600, true), Some(2)),
            ("wav", audio(48_000, 2, 3_600, true), Some(4)),
            ("aiff", audio(96_000, 2, 3 * 3_600 / 2, true), Some(3)),
            ("wav", audio(48_000, 32, 600, true), Some(2)),
            ("ogg", audio(48_000, 2, 3 * 3_600, true), None),
            // 3 時間の 320 kbps の MP3（432 MB）：ファイルを持つ分が入る
            ("mp3", audio(48_000, 2, 3 * 3_600, false), Some(0)),
        ] {
            assert!(limits.check(&shape).is_ok(), "{format} {shape:?}");
            let len = match len {
                Some(0) => 432_000_000,
                Some(b) => pcm(&shape, b),
                None => 0,
            };
            let n = need(format, &shape, len);
            assert!(n <= ITEM_CAP / 2, "{format} {shape:?}: {n}");
            // アプリの組でも同じ（後ろの段のファイルの長さが入らない）
            assert_eq!(need_in(&app, format, &shape, len), n, "{format} {shape:?}");
        }
        // ほぼ長さによらない（ファイルを持たないデコーダ。伸びるのは結果（波形の段）の分だけ）
        let (one, two) = (
            need("flac", &audio(48_000, 2, 3_600, true), 0),
            need("flac", &audio(48_000, 2, 2 * 3_600, true), 0),
        );
        assert!(two >= one && two - one < 1 << 20, "{one} {two}");
        // 上限の形で最も細かい段から作った波形の段を、終わりの長さの引数に合わせると、その引数で解いたものと同じ
        let engine = kura_engine::Engine::new().unwrap();
        let v = kura_engine::testgen::InputGen::AudioNoise {
            rate: 8_000,
            channels: 1,
            frames: 1_200_000,
            seed: 9,
        }
        .generate()
        .unwrap();
        let req = |params| kura_engine::AnalysisRequest {
            id: "audio.peaks".into(),
            v: None,
            params,
        };
        let fine = engine
            .analyze_dyn(
                &req(serde_json::json!({ "finest": 256, "coarsest": 65_536 })),
                &v,
                &kura_engine::NoMonitor,
            )
            .unwrap();
        let want_params = super::super::plan::peaks_params(1_200_000);
        assert_ne!(want_params["finest"], 256);
        let want = engine
            .analyze_dyn(&req(want_params.clone()), &v, &kura_engine::NoMonitor)
            .unwrap();
        let got = super::coarsen_peaks(&registry, &fine, &want_params).unwrap();
        assert_eq!((got.params, got.output), (want.params, want.output));
        // Limits を超えるものは断る（too_large）
        assert!(
            limits
                .check(&audio(48_000, 2, 3 * 3_600 + 1, true))
                .is_err()
        );
        assert!(
            failure_code(&kura_engine::EngineError::Limit("x".into()))
                == Some(error_code::TOO_LARGE)
        );
    }

    /// 失敗の分け方は、どれも bindings に出す理由の一覧（`error_code::ALL`）にある（フロントが文言を引く）。
    #[test]
    fn failure_codes_are_in_the_published_list() {
        let errors = [
            EngineError::Unsupported("x".into()),
            EngineError::CodecMissing { tool: "x".into() },
            EngineError::Limit("x".into()),
            EngineError::Decode {
                codec: "x".into(),
                message: "x".into(),
            },
            EngineError::Internal("x".into()),
        ];
        for err in &errors {
            let code = failure_code(err);
            assert!(code.is_some_and(|c| error_code::ALL.contains(&c)), "{err}");
        }
        assert_eq!(failure_code(&EngineError::Canceled), None);
        assert!(error_code::ALL.contains(&error_code::CRASHED));
    }

    #[test]
    fn percentiles_use_the_nearest_rank() {
        let v: Vec<f64> = (1..=100).map(f64::from).collect();
        assert_eq!((percentile(&v, 50), percentile(&v, 95)), (50.0, 95.0));
        assert_eq!(percentile(&[3.0], 95), 3.0);
        assert_eq!(percentile(&[], 50), 0.0);
        assert_eq!(percentile(&[1.0, 2.0], 50), 1.0);
    }
}
