//! 保存・作り直しの段 1・2：元の形を読み、計画と予算を作り（呼び手）、元を読む（docs/editing.md §14 の表・§9.4）。
//!
//! - 読み方は 3 つ（[`Reading`]）：アンカーから（記録はアンカーの元の読み取りの記録）、元を記録の識別子のデコーダで読み直す、
//!   元を組のデコーダで段の順に読む（アンカーがまだない保存）。
//! - アンカーが使えない（形・中身を読めない、ファイルがない、壊れている、別の値）ときは、保存（上書き・別名）も版に戻す作り直しも、
//!   元を記録の識別子のデコーダで読み直して値のダイジェストを記録と照らす（[`read_recorded_input`]）。同じならその値で続け、
//!   知らせを版の記録の警告に残す（入力の記録は同じ値のアンカーを指したまま）。記録のデコーダがない・値が違えば Mismatch。
//! - OS のデコーダ（Platform の段。同じプロセスで動く）を通りうる読みは、見出し（形）を読む前に途中経過の `decoding` を真にして
//!   コミットし、読み終えるまで保つ（落ちずに終われば、失敗・止めるときも偽に戻す）。そこで落ちると、次に動いたときに 0 の
//!   確かめが 1 回だけ失敗にする（ADR-158 (3)(9) と同じ決まり。解析のジョブの処理中の印と同じく、どの段が読むかは読む前に
//!   分からないので、組にその形式の Platform の段のデコーダがあれば付ける）。

use kura_engine::codec::{CodecSet, Decoder};

use super::*;
use crate::edit::EngineRuntime;

/// 読むのに使うもの（1 つのジョブの間は同じ）。
pub(super) struct ReadCx<'a> {
    pub(super) ctx: &'a JobContext,
    pub(super) runtime: &'a EngineRuntime,
    pub(super) codecs: &'a CodecSet,
    pub(super) layout: &'a Layout,
    /// 元のファイル。
    pub(super) original: &'a Path,
    pub(super) limits: &'a Limits,
    pub(super) mon: &'a JobMonitor<'a>,
}

/// 元の読み方（1 回分）。
pub(super) enum Reading<'a> {
    /// アンカーのファイル `path` を読み、`rec`（アンカーの元の読み取りの記録）と照らす。
    Anchor {
        path: PathBuf,
        rec: &'a RecordedInput,
    },
    /// 元を記録の識別子のデコーダ `decoder` で読み直し、値を `rec` と照らす。
    Recorded {
        decoder: Arc<dyn Decoder>,
        rec: &'a RecordedInput,
    },
    /// 元を組のデコーダで段の順に読む（形式 `format`・読み方 `opts`。アンカーがまだない保存）。
    Any {
        format: &'a str,
        opts: &'a DecodeOptions,
    },
}

impl Reading<'_> {
    /// OS のデコーダ（Platform の段）を通りうるか。組のどのデコーダが読むかは読む前に分からないので、組にその形式の Platform の
    /// 段のデコーダがあれば真（固定の段が先に読む形式も含む。解析のジョブと同じ判定）。
    fn platform(&self, codecs: &CodecSet) -> bool {
        match self {
            Self::Anchor { .. } => false,
            Self::Recorded { decoder, .. } => decoder.tier() == Tier::Platform,
            Self::Any { format, .. } => codecs
                .decoders_for(format)
                .iter()
                .any(|d| d.tier() == Tier::Platform),
        }
    }

    /// アンカーから読むか。
    fn is_anchor(&self) -> bool {
        matches!(self, Self::Anchor { .. })
    }

    /// 読むファイル（アンカーか元 `original`）。
    pub(super) fn path<'p>(&'p self, original: &'p Path) -> &'p Path {
        match self {
            Self::Anchor { path, .. } => path,
            Self::Recorded { .. } | Self::Any { .. } => original,
        }
    }

    /// 予算の見積もりでの読み手。
    pub(super) fn source<'s>(&'s self, codecs: &'s CodecSet) -> Source<'s> {
        match self {
            Self::Anchor { .. } => Source::Decoder {
                decoder: &anchor::AnchorDecoder,
                format: anchor::FORMAT,
            },
            Self::Recorded { decoder, rec } => Source::Decoder {
                decoder: decoder.as_ref(),
                format: &rec.format,
            },
            Self::Any { format, .. } => Source::Decode { codecs, format },
        }
    }

    /// 形（見出し）を読む。
    fn shape(&self, cx: &ReadCx<'_>) -> std::result::Result<Shape, EngineError> {
        match self {
            Self::Anchor { path, .. } => File::open(path)
                .map_err(EngineError::from)
                .and_then(|f| anchor::shape(&mut BufReader::new(f), cx.limits)),
            Self::Recorded { decoder, rec } => decoder
                .shape_file_monitored(cx.original, &rec.format, &rec.options, cx.limits, cx.mon)
                .and_then(|s| cx.limits.check(&s).map_err(EngineError::Limit).map(|()| s)),
            Self::Any { format, opts } => cx
                .codecs
                .shape_path_monitored(cx.original, format, opts, cx.limits, cx.mon)
                .map(|(shape, _)| shape),
        }
    }
}

/// 1 回分の読みの結果。
pub(super) enum Attempt<P> {
    /// 読めた：値・元を読んだ記録（記録と照らす読みでは記録のもの）・呼び手の計画と予算。
    Read {
        value: Value,
        decoded: DecodeStamp,
        prep: P,
    },
    /// 使えない（理由）：アンカーが読めない・ない・壊れている・別の値か、元を記録のデコーダで読み直した値が記録と違う。
    Unusable(String),
}

/// 読みでつまずいたとき：止める指示による中止は止め方に、アンカーの誤り（中止を除く）は使えないことに、ほかは失敗に。
fn trip<P>(
    ctx: &JobContext,
    what: &str,
    e: EngineError,
    from_anchor: bool,
) -> Result<Flow<Attempt<P>>> {
    let canceled = matches!(e.root(), EngineError::Canceled);
    if canceled && let Some(end) = stop_end(ctx) {
        return Ok(Flow::Stop(end));
    }
    if from_anchor && !canceled {
        return Ok(Flow::Go(Attempt::Unusable(format!("{what}：{e}"))));
    }
    Err(from_engine(what, e))
}

/// OS のデコーダを通りうる読み（`platform`）なら、`f` の前に途中経過の `decoding` を真にしてコミットし、`f` が落ちずに終われば
/// （失敗・止めるときも）偽に戻す。
fn while_decoding<T>(ctx: &JobContext, platform: bool, f: impl FnOnce() -> Result<T>) -> Result<T> {
    if !platform {
        return f();
    }
    ctx.checkpoint(DECODING, 0.15, "元を読んでいます")?;
    let r = f();
    match ctx.checkpoint(DECODED, 0.3, "元を読みました") {
        Ok(()) => r,
        Err(err) => match r {
            Ok(_) => Err(err),
            Err(first) => {
                tracing::warn!(%err, "元を読む印を外せませんでした");
                Err(first)
            }
        },
    }
}

/// 1 回分：形を読み、`plan` で計画と予算を作り、値を読む（OS のデコーダを通りうるなら、形を読む前から読み終えるまで印を保つ）。
pub(super) fn read_once<P>(
    cx: &ReadCx<'_>,
    how: &Reading<'_>,
    plan: impl FnOnce(&Shape) -> Result<Flow<P>>,
) -> Result<Flow<Attempt<P>>> {
    while_decoding(cx.ctx, how.platform(cx.codecs), || {
        let from_anchor = how.is_anchor();
        let shape = match how.shape(cx) {
            Ok(shape) => shape,
            Err(e) => return trip(cx.ctx, "元の大きさを読めません", e, from_anchor),
        };
        let prep = match plan(&shape)? {
            Flow::Go(prep) => prep,
            Flow::Stop(end) => return Ok(Flow::Stop(end)),
        };
        cx.ctx.progress(0.15, "元を読んでいます");
        let (codecs, original, limits, mon) = (cx.codecs, cx.original, cx.limits, cx.mon);
        let matched = |r: Recorded, rec: &RecordedInput| match r {
            Recorded::Matched { input, .. } => Ok((input.value, rec.decoded.clone())),
            Recorded::Mismatched { mismatch, .. } => Err(mismatch.to_string()),
        };
        let read = cx.runtime.bulk(|_| match how {
            Reading::Anchor { path, rec } => recorded::read_recorded(
                codecs,
                rec,
                Some(recorded::Source::Path(path)),
                None,
                limits,
                mon,
            )
            .map(|r| matched(r, rec)),
            Reading::Recorded { rec, .. } => recorded::read_recorded(
                codecs,
                rec,
                None,
                Some(recorded::Source::Path(original)),
                limits,
                mon,
            )
            .map(|r| matched(r, rec)),
            Reading::Any { format, opts } => codecs
                .decode_path(original, format, opts, limits, mon)
                .map(Ok),
        });
        match read {
            Ok(Ok((value, decoded))) => Ok(Flow::Go(Attempt::Read {
                value,
                decoded,
                prep,
            })),
            Ok(Err(why)) => Ok(Flow::Go(Attempt::Unusable(why))),
            Err(e) => trip(cx.ctx, "元を読めません", e, from_anchor),
        }
    })
}

/// 記録に合う入力を読んだ結果（[`read_recorded_input`]）。
#[allow(clippy::large_enum_variant)] // 1 回の読みで 1 度だけ返して、すぐ取り出す（値を箱に入れ直さない）
pub(super) enum RecordedRead<P> {
    /// 読めた：値・元を読んだ記録・アンカーの使い方・呼び手の計画と予算。
    Read {
        value: Value,
        decoded: DecodeStamp,
        anchor: AnchorUse,
        prep: P,
    },
    /// 読めない：アンカーが使えず、記録の識別子のデコーダがこの環境にない（`platform` が偽なら、OS のデコーダでしか読めない
    /// ときも）。文は呼び手の言葉（`cant`）から始まる。
    Unavailable(String),
    /// 読み直した値が記録と違う（アンカーも使えない）。文は呼び手の言葉から始まる。
    Mismatch(String),
}

impl<P> RecordedRead<P> {
    /// 保存・版に戻すの扱い：読めない・違うはどちらも Mismatch（docs/editing.md §14「作り直しの照合の決まり」）。
    pub(super) fn or_mismatch(self) -> Result<(Value, DecodeStamp, AnchorUse, P)> {
        match self {
            Self::Read {
                value,
                decoded,
                anchor,
                prep,
            } => Ok((value, decoded, anchor, prep)),
            Self::Unavailable(why) | Self::Mismatch(why) => Err(CoreError::Mismatch(why)),
        }
    }
}

/// 記録 `rec` に合う入力を読む（保存でアンカーがあるとき・版に戻す作り直し・GC の照合）：アンカー `anchor` があればアンカーから、
/// 使えなければ元を記録の識別子のデコーダで読み直す（知らせを `warnings` に足す）。記録のデコーダがない（`platform` が偽で、
/// それが OS のデコーダのときも）なら Unavailable、読み直した値が記録と違えば Mismatch（`cant` は何ができないかの言葉）。
/// GC は同じプロセスで OS のデコーダを呼ばない（ライブラリを開くたびに動くので、落ちる読みを繰り返さない）ので `platform` を
/// 偽にする。
pub(super) fn read_recorded_input<P>(
    cx: &ReadCx<'_>,
    rec: &RecordedInput,
    anchor: Option<Anchor>,
    cant: &str,
    platform: bool,
    warnings: &mut Vec<String>,
    mut plan: impl FnMut(&Reading<'_>, &Shape) -> Result<Flow<P>>,
) -> Result<Flow<RecordedRead<P>>> {
    let mut unusable = None;
    if let Some(a) = anchor {
        let how = Reading::Anchor {
            path: cx.layout.content_path(&a.hash, &a.ext),
            rec,
        };
        match read_once(cx, &how, |shape| plan(&how, shape))? {
            Flow::Stop(end) => return Ok(Flow::Stop(end)),
            Flow::Go(Attempt::Read {
                value,
                decoded,
                prep,
            }) => {
                return Ok(Flow::Go(RecordedRead::Read {
                    value,
                    decoded,
                    anchor: AnchorUse::Existing(a),
                    prep,
                }));
            }
            Flow::Go(Attempt::Unusable(why)) => {
                warnings.push(format!(
                    "アンカーが使えないので、元を読み直しました（{why}）"
                ));
                unusable = Some((a, why));
            }
        }
    }
    let also = |unusable: &Option<(Anchor, String)>| match unusable {
        Some((_, why)) => format!("（アンカーも使えません：{why}）"),
        None => String::new(),
    };
    let Some(decoder) = cx
        .codecs
        .decoder_by_id(&rec.decoded.codec)
        .filter(|d| d.can_decode(&rec.format))
        .cloned()
    else {
        return Ok(Flow::Go(RecordedRead::Unavailable(format!(
            "{cant}：記録のデコーダ {} がこの環境にありません{}",
            rec.decoded.codec,
            also(&unusable)
        ))));
    };
    if !platform && decoder.tier() == Tier::Platform {
        return Ok(Flow::Go(RecordedRead::Unavailable(format!(
            "{cant}：記録のデコーダ {} は OS のデコーダなので、ここでは読みません{}",
            rec.decoded.codec,
            also(&unusable)
        ))));
    }
    let how = Reading::Recorded { decoder, rec };
    match read_once(cx, &how, |shape| plan(&how, shape))? {
        Flow::Stop(end) => Ok(Flow::Stop(end)),
        Flow::Go(Attempt::Read {
            value,
            decoded,
            prep,
        }) => {
            let used = match unusable {
                Some((a, _)) => AnchorUse::Fallback(a),
                None => AnchorUse::None,
            };
            Ok(Flow::Go(RecordedRead::Read {
                value,
                decoded,
                anchor: used,
                prep,
            }))
        }
        Flow::Go(Attempt::Unusable(why)) => Ok(Flow::Go(RecordedRead::Mismatch(format!(
            "{cant}：{why}{}",
            also(&unusable)
        )))),
    }
}
