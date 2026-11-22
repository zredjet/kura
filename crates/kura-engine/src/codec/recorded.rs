//! 版の入力の記録と、作り直しでその記録に合う入力を読む口（E29。docs/editing.md §8.6・§9.4・§17）。
//!
//! - 記録（[`RecordedInput`]）：入力ごと（Primary・Fixed・Selection）に、元のファイルのハッシュ・形式・読み方の設定・元を
//!   読んだ記録（`DecodeStamp`。Platform・External なら `env` を含む）・使ったアンカーのハッシュ。M12 の版の入力の表の 1 行。
//! - 選び方（[`read_recorded`]）：アンカーがあればアンカーを読み、その元の読み取りの記録の値のダイジェストが版の記録と
//!   同じかを照らす。なければ、記録の識別子のデコーダ（旧版を含む）で元を読み直し、値のダイジェストを照らす。
//! - 「変わったら検出」は失敗ではなく、記録との食い違い（[`Recorded::Mismatched`] の [`Mismatch`]：値が変わった・
//!   記録のデコーダがない・アンカーが使えない）として返す。呼び手（kura-core）は、版のファイルを残す・アンカーから
//!   作り直す・利用者に知らせる、を決める。読み書きの失敗（入出力・上限・中止・元が読めない）は `Err`。
//! - アンカーから読んでも、描くときの元のハッシュ（`InputValue.source`。乱数の種）は元のファイルのハッシュのまま
//!   （アンカーのファイルのハッシュにすると、ディザなどの出力が変わる）。[`Recorded::Matched`] の `input` はそうしてある。
//! - アンカーが壊れていて元もあるとき、元で読み直すかは呼び手が決める（`anchor` を渡さずにもう一度呼ぶ）。

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::codec::anchor::{self, AnchorContext};
use crate::codec::{CodecSet, DecodeStamp, ReadSeek};
use crate::digest::Digest;
use crate::doc::DecodeOptions;
use crate::error::{EngineError, Mismatch};
use crate::limits::Limits;
use crate::monitor::Monitor;
use crate::render::InputValue;
use crate::value::Value;

/// 版の入力 1 つの記録（保存のために読み戻せる。知らない項目は拒む）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RecordedInput {
    /// 元のファイルの中身のハッシュ（描くときの `InputValue.source`）。
    pub source: Digest,
    /// 元の形式の ID。
    pub format: String,
    /// 読み方の設定（文書の `InputSlot.decode`）。
    #[serde(default)]
    pub options: DecodeOptions,
    /// 元を読んだ記録。
    pub decoded: DecodeStamp,
    /// 使ったアンカーのファイルのハッシュ（kura-core の台帳の鍵。再現できる読み取りなら持たない）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anchor: Option<Digest>,
}

impl RecordedInput {
    /// 元を読んだ記録から作る（アンカーはまだない）。
    pub fn new(
        source: Digest,
        format: impl Into<String>,
        options: DecodeOptions,
        decoded: DecodeStamp,
    ) -> Self {
        Self {
            source,
            format: format.into(),
            options,
            decoded,
            anchor: None,
        }
    }

    /// この入力にアンカーが要るか（再現を約束しないデコーダで読んだ）。
    pub fn needs_anchor(&self) -> bool {
        anchor::needs_anchor(&self.decoded)
    }

    /// この入力のアンカーを書くときの文脈。
    pub fn anchor_context(&self) -> AnchorContext {
        AnchorContext {
            source: Some(self.source),
            format: Some(self.format.clone()),
            options: self.options.clone(),
        }
    }
}

/// 読む元（読み口か、ファイルのパス）。外部のツールのデコーダはパスで読む（`Decoder::decode_file`）。
pub enum Source<'a> {
    Reader(&'a mut dyn ReadSeek),
    Path(&'a Path),
}

/// 値をどこから読んだか。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum InputOrigin {
    /// アンカーから。
    Anchor,
    /// 元のファイルを、記録の識別子のデコーダで読み直した。
    Original,
}

/// 作り直しの入力を読んだ結果。
#[derive(Debug, Clone)]
pub enum Recorded {
    /// 記録と同じ値（作り直しに使える）。`input.source` は元のファイルのハッシュ。
    Matched {
        input: InputValue,
        origin: InputOrigin,
    },
    /// 記録と食い違った（E29）。失敗ではない。
    Mismatched {
        mismatch: Mismatch,
        /// 元を記録の識別子のデコーダで読めたときの、今の値とその記録（`SourceChanged` のときだけ）。
        current: Option<(Value, DecodeStamp)>,
    },
}

impl Recorded {
    /// 食い違い（あれば）。
    pub fn mismatch(&self) -> Option<&Mismatch> {
        match self {
            Self::Matched { .. } => None,
            Self::Mismatched { mismatch, .. } => Some(mismatch),
        }
    }

    /// 記録と同じ値なら入力と読んだ元、食い違いなら `EngineError::Mismatch`（`?` でつなぐとき）。
    pub fn into_matched(self) -> Result<(InputValue, InputOrigin), EngineError> {
        match self {
            Self::Matched { input, origin } => Ok((input, origin)),
            Self::Mismatched { mismatch, .. } => Err(EngineError::Mismatch(mismatch)),
        }
    }
}

/// 作り直しで、版の記録に合う入力の値を読む（E29）。`anchor` があればアンカーだけを読み（元は読まない）、なければ `original`
/// を記録の識別子のデコーダで読み直す。どちらもなければ `Internal`。
pub fn read_recorded(
    codecs: &CodecSet,
    rec: &RecordedInput,
    anchor: Option<Source<'_>>,
    original: Option<Source<'_>>,
    limits: &Limits,
    mon: &dyn Monitor,
) -> Result<Recorded, EngineError> {
    match (anchor, original) {
        (Some(a), _) => from_anchor(rec, a, limits, mon),
        (None, Some(o)) => from_original(codecs, rec, o, limits, mon),
        (None, None) => Err(EngineError::Internal(
            "作り直しの入力の読む元（アンカーか元のファイル）がありません".into(),
        )),
    }
}

fn mismatched(mismatch: Mismatch) -> Recorded {
    Recorded::Mismatched {
        mismatch,
        current: None,
    }
}

fn anchor_broken(reason: &str) -> Recorded {
    mismatched(Mismatch::AnchorBroken {
        reason: reason.to_string(),
    })
}

/// アンカーから読む。アンカーの元の読み取りの記録の値が版の記録と同じか、元のハッシュが同じか（ヘッダにあれば）を照らす。
fn from_anchor(
    rec: &RecordedInput,
    src: Source<'_>,
    limits: &Limits,
    mon: &dyn Monitor,
) -> Result<Recorded, EngineError> {
    let read = match src {
        Source::Reader(r) => anchor::read(r, limits, mon),
        Source::Path(p) => anchor::read_path(p, limits, mon),
    };
    let r = match read {
        Ok(r) => r,
        Err(EngineError::Mismatch(m)) => return Ok(mismatched(m)),
        Err(e) => return Err(e),
    };
    if r.header.decoded.value_digest != rec.decoded.value_digest {
        return Ok(anchor_broken("版の記録と違う値のアンカーです"));
    }
    if r.header.source.is_some_and(|s| s != rec.source) {
        return Ok(anchor_broken("別の元のファイルのアンカーです"));
    }
    Ok(Recorded::Matched {
        input: InputValue {
            value: r.value,
            source: rec.source,
        },
        origin: InputOrigin::Anchor,
    })
}

/// 元を記録の識別子のデコーダ（旧版を含む）で読み直し、値のダイジェストを照らす。
fn from_original(
    codecs: &CodecSet,
    rec: &RecordedInput,
    src: Source<'_>,
    limits: &Limits,
    mon: &dyn Monitor,
) -> Result<Recorded, EngineError> {
    let id = rec.decoded.codec.as_str();
    if !codecs
        .decoder_by_id(id)
        .is_some_and(|d| d.can_decode(&rec.format))
    {
        return Ok(mismatched(Mismatch::DecoderGone {
            codec: id.to_string(),
        }));
    }
    let (value, stamp) = match src {
        Source::Reader(r) => codecs.decode_with(id, r, &rec.format, &rec.options, limits, mon)?,
        Source::Path(p) => {
            codecs.decode_with_path(id, p, &rec.format, &rec.options, limits, mon)?
        }
    };
    if stamp.value_digest != rec.decoded.value_digest {
        return Ok(Recorded::Mismatched {
            mismatch: Mismatch::SourceChanged {
                codec: id.to_string(),
                expected: rec.decoded.value_digest,
                got: stamp.value_digest,
            },
            current: Some((value, stamp)),
        });
    }
    Ok(Recorded::Matched {
        input: InputValue {
            value,
            source: rec.source,
        },
        origin: InputOrigin::Original,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::{CodecEnv, Tier};
    use crate::monitor::NoMonitor;
    use crate::testgen::InputGen;
    use crate::testing::FakePlatformDecoder;
    use crate::value::PixelFormat;
    use std::io::Cursor;
    use std::sync::Arc;

    fn noise() -> InputGen {
        InputGen::Noise {
            w: 5,
            h: 4,
            fmt: PixelFormat::Rgba8,
            seed: 11,
        }
    }

    /// 記録の JSON は読み戻せ、知らない項目は拒む。アンカーがなければ書かない。
    #[test]
    fn records_round_trip_as_json() {
        let stamp = DecodeStamp {
            codec: "heic-fake-dec/1".into(),
            tier: Tier::Platform,
            reproducible: false,
            value_digest: Digest([2; 32]),
            env: Some(CodecEnv::new("macOS", "26.7 (25G229)")),
        };
        let mut rec = RecordedInput::new(Digest([1; 32]), "heic", DecodeOptions::default(), stamp);
        assert!(rec.needs_anchor());
        let json = serde_json::to_value(&rec).unwrap();
        assert!(json.get("anchor").is_none());
        assert_eq!(
            serde_json::from_value::<RecordedInput>(json.clone()).unwrap(),
            rec
        );
        rec.anchor = Some(Digest([3; 32]));
        let back: RecordedInput =
            serde_json::from_value(serde_json::to_value(&rec).unwrap()).unwrap();
        assert_eq!(back, rec);
        let mut extra = json;
        extra["extra"] = serde_json::json!(1);
        assert!(serde_json::from_value::<RecordedInput>(extra).is_err());
        let cx = rec.anchor_context();
        assert_eq!(cx.source, Some(Digest([1; 32])));
        assert_eq!(cx.format.as_deref(), Some("heic"));
    }

    /// 読む元がなければ内部の誤り。食い違いは `into_matched` で `EngineError::Mismatch` になり、種類ごとに code が違う。
    #[test]
    fn no_source_is_internal_and_mismatch_codes_differ() {
        let codecs = CodecSet::pinned();
        let v = noise().generate().unwrap();
        let rec = RecordedInput::new(
            Digest::default(),
            "heic",
            DecodeOptions::default(),
            DecodeStamp {
                codec: "heic-fake-dec/1".into(),
                tier: Tier::Platform,
                reproducible: false,
                value_digest: v.digest().unwrap(),
                env: None,
            },
        );
        assert!(matches!(
            read_recorded(&codecs, &rec, None, None, &Limits::default(), &NoMonitor),
            Err(EngineError::Internal(_))
        ));
        let r = read_recorded(
            &codecs,
            &rec,
            None,
            Some(Source::Reader(&mut Cursor::new(vec![0u8; 3]))),
            &Limits::default(),
            &NoMonitor,
        )
        .unwrap();
        assert!(matches!(r.mismatch(), Some(Mismatch::DecoderGone { .. })));
        let e = r.into_matched().unwrap_err();
        assert_eq!(e.code(), "DecoderGone");
        let codes = [
            Mismatch::SourceChanged {
                codec: String::new(),
                expected: Digest::default(),
                got: Digest::default(),
            },
            Mismatch::DecoderGone {
                codec: String::new(),
            },
            Mismatch::AnchorBroken {
                reason: String::new(),
            },
        ]
        .map(|m| EngineError::Mismatch(m).code());
        assert_eq!(codes, ["SourceChanged", "DecoderGone", "AnchorBroken"]);
    }

    /// 元から読むとき、記録と同じなら元のハッシュを付けて返し、違えば今の値を添えて食い違いを返す。
    #[test]
    fn original_is_checked_against_the_record() {
        let fake =
            Arc::new(FakePlatformDecoder::new("heic-fake-dec/1", &["heic"], noise()).drifting());
        let mut codecs = CodecSet::pinned();
        codecs.add_decoder(fake.clone()).unwrap();
        let (_, stamp) = codecs
            .decode(
                &mut Cursor::new(b"x".to_vec()),
                "heic",
                &DecodeOptions::default(),
                &Limits::default(),
                &NoMonitor,
            )
            .unwrap();
        let rec = RecordedInput::new(Digest([5; 32]), "heic", DecodeOptions::default(), stamp);
        let r = read_recorded(
            &codecs,
            &rec,
            None,
            Some(Source::Reader(&mut Cursor::new(b"x".to_vec()))),
            &Limits::default(),
            &NoMonitor,
        )
        .unwrap();
        match r {
            Recorded::Mismatched {
                mismatch: Mismatch::SourceChanged { expected, got, .. },
                current: Some((v, s)),
            } => {
                assert_eq!(expected, rec.decoded.value_digest);
                assert_eq!(got, s.value_digest);
                assert_eq!(v.digest().unwrap(), got);
                assert_ne!(got, expected);
            }
            other => panic!("{other:?}"),
        }
    }
}
