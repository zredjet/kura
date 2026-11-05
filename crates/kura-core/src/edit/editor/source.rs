//! エディタを開くときに元を読む（docs/editing.md §12.10・§9.4。M12 の決定：開くときの読みも予算で確保する）。
//!
//! - 読み方は保存のジョブ（`edit::save` の段 1・2）と同じ順：アンカーがあればアンカーから（記録はアンカーの元の読み取りの
//!   記録）、使えなければ（形・中身を読めない・別の値）元を記録の識別子のデコーダで読み直して値を記録と照らし（違えば
//!   Mismatch）、アンカーがなければ元を組のデコーダで段の順に読む。アンカーは作らない（初めて保存するときに保存のジョブが
//!   作る。決定 7）。
//! - 予算：形を読んでから、読む段の見積もり（`Engine::estimate` の読む段。デコーダの作業領域・ファイルの長さを含む）を対話
//!   （`Class::Interactive`）で待たずに確保する。予算の全体を超えれば TooLarge、今すぐ確保できなければ Busy。確保は読み
//!   終えたら手放し、そのあとはエディタのキャッシュの確保（入力を `register_external` で数える）が覆う。
//! - 形の読みも予算の外で行わない（M13 段 7）：元の形は、その段で形を読む分（ファイル全体を読む段の長さ・埋め込みの画像など）
//!   を対話の枠で待たずに確保してから読む（組の段の順は `audio::source::shape_staged`、記録のデコーダは同じ見積もりを 1 段で）。
//!   呼び手が確保の中で読んだ形（[`SourceCx::shape`]。アンカーのない短い音声の境の判定で読んだもの）があれば、それを使って
//!   読み直さない。アンカーの見出し（自前の f32 WAV の見出しだけ）は確保しない。
//! - 音声のアイテムは、試す読み方の形で丸ごと読む上限を確かめる（[`SourceCx::check_whole`]。超えれば TooLarge。元の形を
//!   先に読まない）。
//! - 中止：エディタを閉じる・別のアイテムを開くと止まる（`mon`）。

use std::collections::BTreeMap;
use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use kura_engine::budget::{Class, Lease};
use kura_engine::codec::anchor;
use kura_engine::codec::recorded::{self, Recorded, RecordedInput};
use kura_engine::codec::{CodecSet, DecodeStamp, Decoder};
use kura_engine::doc::{DecodeOptions, InputId};
use kura_engine::render::estimate::{EstimateSpec, Source};
use kura_engine::{Digest, EngineError, Graph, Limits, Monitor, Shape, Value};

use super::EditorError;
use super::audio::source::{ShapeCx, admit, check_whole_read, shape_staged};
use crate::edit::EngineRuntime;
use crate::edit::anchors::Anchor;

/// 読んだ元。
pub(super) struct ReadSource {
    pub value: Value,
    /// 読んだ記録（アンカーから読んだときは、アンカーの元の読み取りの記録。値のダイジェストが入力の指紋になる）。
    pub decoded: DecodeStamp,
    /// 知らせ（アンカーが使えずに元を読み直した、など）。
    pub warnings: Vec<String>,
}

/// 読む元の場所と読み方の材料。
pub(super) struct SourceCx<'a> {
    pub runtime: &'a EngineRuntime,
    pub codecs: &'a CodecSet,
    /// 元のファイル（`files/<hash>.<ext>`）とその中身・形式・読み方。
    pub original: &'a Path,
    pub source: Digest,
    pub format: &'a str,
    pub opts: &'a DecodeOptions,
    /// アンカー（あれば）とそのファイル。
    pub anchor: Option<(Anchor, PathBuf)>,
    pub limits: &'a Limits,
    pub mon: &'a dyn Monitor,
    /// 呼び手が確保の中で読んだ元の形（組の段の順で読んだもの。アンカーのない読み（組の段の順）で使い、読み直さない）。
    pub shape: Option<Shape>,
    /// 音声のアイテム：形を読んだら、丸ごと読む上限（`audio::source::check_whole_read`）を確かめる。
    pub check_whole: bool,
}

/// 1 回分の読み方。
enum How<'a> {
    Anchor {
        path: &'a Path,
        rec: &'a RecordedInput,
    },
    Recorded {
        decoder: Arc<dyn Decoder>,
        rec: &'a RecordedInput,
    },
    Any,
}

/// 元を読む（モジュールの説明）。
pub(super) fn read(cx: &SourceCx<'_>) -> Result<ReadSource, EditorError> {
    let mut warnings = Vec::new();
    let Some((a, path)) = &cx.anchor else {
        return match attempt(cx, &How::Any)? {
            Ok((value, decoded)) => Ok(ReadSource {
                value,
                decoded,
                warnings,
            }),
            Err(why) => Err(EditorError::Mismatch(format!("元を読めません：{why}"))),
        };
    };
    let rec = RecordedInput {
        source: cx.source,
        format: cx.format.to_string(),
        options: cx.opts.clone(),
        decoded: a.decoded.clone(),
        anchor: Some(Digest(a.hash)),
    };
    let why = match attempt(cx, &How::Anchor { path, rec: &rec })? {
        Ok((value, decoded)) => {
            return Ok(ReadSource {
                value,
                decoded,
                warnings,
            });
        }
        Err(why) => why,
    };
    warnings.push(format!(
        "アンカーが使えないので、元を読み直しました（{why}）"
    ));
    let Some(decoder) = cx
        .codecs
        .decoder_by_id(&rec.decoded.codec)
        .filter(|d| d.can_decode(&rec.format))
        .cloned()
    else {
        return Err(EditorError::Mismatch(format!(
            "元を読めません：記録のデコーダ {} がこの環境にありません（アンカーも使えません：{why}）",
            rec.decoded.codec
        )));
    };
    match attempt(cx, &How::Recorded { decoder, rec: &rec })? {
        Ok((value, decoded)) => Ok(ReadSource {
            value,
            decoded,
            warnings,
        }),
        Err(again) => Err(EditorError::Mismatch(format!(
            "元を読めません：{again}（アンカーも使えません：{why}）"
        ))),
    }
}

/// 1 回分：形を読み、予算を確保して値を読む。使えない（アンカーが読めない・壊れている・記録と違う値）なら `Ok(Err(理由))`。
/// 中止は `Canceled`、予算は TooLarge・Busy、ほかの読めない誤りは元を組で読むときだけ失敗にする。
fn attempt(
    cx: &SourceCx<'_>,
    how: &How<'_>,
) -> Result<std::result::Result<(Value, DecodeStamp), String>, EditorError> {
    let from_record = !matches!(how, How::Any);
    let shape = match (how, cx.shape) {
        // 呼び手が確保の中で読んだ形（同じ組の段の順で読んだもの）：読み直さない
        (How::Any, Some(shape)) => shape,
        _ => match read_shape(cx, how)? {
            Ok(shape) => shape,
            Err(e) => return unusable(e, from_record, "元の大きさを読めません"),
        },
    };
    if cx.check_whole {
        check_whole_read(&shape, cx.runtime.budget().total())?;
    }
    let _lease = admit_read(cx, how, &shape)?;
    let read = cx.runtime.preview(|_| match how {
        How::Anchor { path, rec } => recorded::read_recorded(
            cx.codecs,
            rec,
            Some(recorded::Source::Path(path)),
            None,
            cx.limits,
            cx.mon,
        )
        .map(|r| matched(r, rec)),
        How::Recorded { rec, .. } => recorded::read_recorded(
            cx.codecs,
            rec,
            None,
            Some(recorded::Source::Path(cx.original)),
            cx.limits,
            cx.mon,
        )
        .map(|r| matched(r, rec)),
        How::Any => cx
            .codecs
            .decode_path(cx.original, cx.format, cx.opts, cx.limits, cx.mon)
            .map(Ok),
    });
    match read {
        Ok(r) => Ok(r),
        Err(e) => unusable(e, from_record, "元を読めません"),
    }
}

/// 記録と照らした結果：同じ値なら値と記録、違えば理由。
fn matched(r: Recorded, rec: &RecordedInput) -> std::result::Result<(Value, DecodeStamp), String> {
    match r {
        Recorded::Matched { input, .. } => Ok((input.value, rec.decoded.clone())),
        Recorded::Mismatched { mismatch, .. } => Err(mismatch.to_string()),
    }
}

/// 読みでつまずいたとき：中止は中止に、記録と照らす読み（アンカー・記録のデコーダ）は使えないことに、ほかは失敗に。
fn unusable<T>(
    e: EngineError,
    from_record: bool,
    what: &str,
) -> Result<std::result::Result<T, String>, EditorError> {
    if matches!(e.root(), EngineError::Canceled) {
        return Err(EditorError::Canceled);
    }
    if from_record && !matches!(e.root(), EngineError::Limit(_)) {
        return Ok(Err(format!("{what}：{e}")));
    }
    Err(EditorError::from_engine(what, e))
}

/// 形（見出し）を読む。元の形は予算の外で読まない（モジュールの説明）：組の段の順なら段ごとに、記録のデコーダならその段の
/// 形を読む分（`Decoder::shape_work_bytes` ＋ 頭で数えた保持の量。`CodecSet::shape_path_staged` と同じ見積もり）を対話の枠で
/// 待たずに確保してから読む。アンカーの見出しは確保しない。予算で断ったら外の Err（Busy・TooLarge）、読めなければ内の Err。
fn read_shape(
    cx: &SourceCx<'_>,
    how: &How<'_>,
) -> Result<std::result::Result<Shape, EngineError>, EditorError> {
    let read = match how {
        How::Anchor { path, .. } => cx.runtime.preview(|_| {
            File::open(path)
                .map_err(EngineError::from)
                .and_then(|f| anchor::shape(&mut BufReader::new(f), cx.limits))
        }),
        How::Recorded { decoder, rec } => {
            let len = std::fs::metadata(cx.original).map_or(0, |m| m.len());
            let mut lease: Option<Lease> = None;
            let mut refused: Option<EditorError> = None;
            let read = cx.runtime.preview(|_| {
                let held = decoder.held_bytes_file(cx.original, &rec.format, cx.limits)?;
                let need = decoder
                    .shape_work_bytes(&rec.format, len)
                    .saturating_add(held.open);
                match admit(cx.runtime.budget(), need, "元の形を読む") {
                    Ok(l) => lease = l,
                    Err(e) => {
                        refused = Some(e);
                        return Err(EngineError::Canceled);
                    }
                }
                decoder.shape_file_monitored(
                    cx.original,
                    &rec.format,
                    &rec.options,
                    cx.limits,
                    cx.mon,
                )
            });
            drop(lease);
            if let Some(e) = refused {
                return Err(e);
            }
            read
        }
        How::Any => {
            let staged = shape_staged(
                &ShapeCx {
                    runtime: cx.runtime,
                    codecs: cx.codecs,
                    path: cx.original,
                    format: cx.format,
                    opts: cx.opts,
                    limits: cx.limits,
                    mon: cx.mon,
                },
                None,
            )?;
            // 段の順の読みは上限の確かめを済ませている
            return Ok(staged.map(|(shape, _)| shape));
        }
    };
    Ok(read.and_then(|shape| {
        cx.limits.check(&shape).map_err(EngineError::Limit)?;
        Ok(shape)
    }))
}

/// 読む段の見積もりを、対話で待たずに確保する（予算の全体を超えれば TooLarge、空いていなければ Busy）。
fn admit_read(cx: &SourceCx<'_>, how: &How<'_>, shape: &Shape) -> Result<Lease, EditorError> {
    let path = match how {
        How::Anchor { path, .. } => *path,
        How::Recorded { .. } | How::Any => cx.original,
    };
    let len = std::fs::metadata(path).map(|m| m.len()).ok();
    let need = cx
        .runtime
        .preview(|e| {
            // 文書の入力をそのまま出す計画で、読む段だけを数える（出力は入力と同じ実体で、数え直さない）
            let plan = e
                .compile(
                    &Graph::new_single(),
                    &BTreeMap::from([(InputId(0), *shape)]),
                )
                .map_err(EngineError::Invalid)?;
            let src = match how {
                How::Anchor { .. } => Source::Decoder {
                    decoder: &anchor::AnchorDecoder,
                    format: anchor::FORMAT,
                },
                How::Recorded { decoder, rec } => Source::Decoder {
                    decoder: decoder.as_ref(),
                    format: &rec.format,
                },
                How::Any => Source::Decode {
                    codecs: cx.codecs,
                    format: cx.format,
                },
            };
            let mut spec =
                EstimateSpec::new(cx.runtime.preview_threads() as u64).input(InputId(0), src);
            if let Some(len) = len {
                spec = spec.input_len(InputId(0), len);
            }
            e.estimate(&plan, &spec, None).map(|est| est.peak)
        })
        .map_err(|e| EditorError::from_engine("元を読むメモリを見積もれません", e))?;
    let budget = cx.runtime.budget();
    if need > budget.total() {
        return Err(EditorError::TooLarge(format!(
            "元を読むのにメモリの予算を超えます（見積もり {need} バイト。予算は {} バイト）",
            budget.total()
        )));
    }
    match budget.try_acquire(need, Class::Interactive) {
        Ok(Some(lease)) => Ok(lease),
        Ok(None) => Err(EditorError::Busy(format!(
            "元を読むメモリ（{need} バイト）が空いていません。少し待ってから開き直してください"
        ))),
        Err(e) => Err(EditorError::Busy(e.to_string())),
    }
}
