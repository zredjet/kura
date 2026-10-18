//! Platform の AAC の書き出しの受け口：backend（AudioToolbox の ExtAudioFile。m4a をメモリへ）に PCM をチャンクごとに
//! 渡し、書いた m4a を受け取る。
//!
//! 約束（adapter の側。識別子の版に入る）：
//! - 書くのは AAC-LC の m4a だけ（HE-AAC は書かない）。レート・チャンネルは backend の能力（`supports`）の範囲で、
//!   チャンネルは 1〜8（並びは [`super::default_channel_mask`] を `ClientChannelLayout` に必ず渡す）。空の音声は書かない。
//! - 設定：`mode`（`vbrConstrained` 既定・`vbr`・`abr`・`cbr`）、`bitrate`（kbps。既定 256。その形で選べなければ、選べる
//!   値のうち 256 以下で最大のもの、なければ最小のもの。明示した値が選べなければ `Invalid`）、`quality`（0〜127。
//!   `vbr` のときだけ使う。既定 127）。
//! - 入力の NaN は 0、±∞ は ±1 にしてから渡す（Vorbis の書き出しと同じ。有限の値はそのまま）。4,096 フレームずつ渡し、
//!   合間で中止を見る。
//! - 出力は backend の申告の長さを [`aac_max_output`]（AAC の 1 フレーム 1 チャンネルの上限 6,144 ビットから決めた
//!   上限）で確かめてから確保して写し、moov の mvhd・tkhd・mdhd の作成・変更の時刻を 0 にする（[`zero_mp4_times`]。
//!   箱の大きさは変えない。docs §9.2 の「時刻は書かない」）。タグ・ループ点・マーカーは書かない。
//! - バイト列は同じ OS なら同じになる見込みだが、約束しない（`reproducible()` は偽）。

use std::io::Write;

use serde_json::json;

use crate::codec::formats::NONE;
use crate::codec::platform::{BackendError, default_channel_mask};
use crate::codec::{CodecEnv, Encoder, MetaCaps, Stage, Tier};
use crate::error::{EngineError, Issue, IssueCode, Issues};
use crate::monitor::Monitor;
use crate::op::{ParamKind, ParamSchema, Unit, spec};
use crate::shape::Shape;
use crate::value::Value;

/// 1 回に渡すフレームの数。
pub const WRITE_FRAMES: usize = 4_096;
/// 既定のビットレート（kbps）。
pub const DEFAULT_KBPS: u32 = 256;
/// `vbr` の既定の品質（AudioToolbox の `kAudioCodecPropertySoundQualityForVBR`。0〜127）。
pub const DEFAULT_QUALITY: u32 = 127;
/// チャンネルの数の上限（並びを決めているのは 8 まで）。
pub const MAX_CHANNELS: u16 = 8;
/// 設定 `mode` の値。
pub const MODES: &[&str] = &["vbrConstrained", "vbr", "abr", "cbr"];

/// ビットレートの決め方。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AacMode {
    /// 固定（`kAudioCodecBitRateControlMode_Constant`）。
    Cbr,
    /// 平均（`_LongTermAverage`）。
    Abr,
    /// 上限つきの可変（`_VariableConstrained`）。既定。
    VbrConstrained,
    /// 品質で決める可変（`_Variable`。`quality` を使う）。
    Vbr,
}

/// backend に渡す書き出しの設定。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AacSettings {
    pub rate: u32,
    pub channels: u16,
    /// チャンネルの並び（`WAVE_FORMAT_EXTENSIBLE` のマスク。[`super::default_channel_mask`]）。
    pub channel_mask: u32,
    pub mode: AacMode,
    /// ビットレート（kbps。`Vbr` では使わない）。
    pub bitrate_kbps: u32,
    /// `Vbr` の品質（0〜127）。
    pub quality: u32,
}

/// AAC の書き出しの backend（kura-mac の AudioToolbox・テストの偽物）。
pub trait AacBackend: Send + Sync {
    /// 書きかけのもの。`Drop` で 1 度だけ閉じる（書きかけの出力も消える）。
    type Handle: AacHandle;
    /// コーデックの識別子（例 `aac-audiotoolbox-enc/1`）。
    fn id(&self) -> &'static str;
    /// 環境の記録（OS・AudioToolbox の版）。
    fn env(&self) -> Option<CodecEnv> {
        None
    }
    /// AAC-LC でこのレート・チャンネルの数を書けるか（組み立てたときに調べた能力から。呼ぶたびに OS に問い合わせない）。
    fn supports(&self, rate: u32, channels: u16) -> bool;
    /// このレート・チャンネルの数で選べるビットレート（kbps）。
    fn bitrates(&self, rate: u32, channels: u16) -> Vec<u32>;
    /// 書き始める。出力が `max_out` バイトを超えそうなら backend も止めてよい（`Limit`）。
    fn open(&self, settings: &AacSettings, max_out: u64) -> Result<Self::Handle, BackendError>;
}

/// 書きかけの AAC。
pub trait AacHandle {
    /// 続きの PCM（planar の f32。チャンネルの数ぶん、どれも同じ長さ）を渡す。backend は呼び出しのあいだだけ読む。
    fn write(&mut self, planes: &[&[f32]]) -> Result<(), BackendError>;
    /// 書き終える（エンコーダの残りを出し、m4a を閉じる）。出力の長さを返す。
    fn finish(&mut self, mon: &dyn Monitor) -> Result<u64, BackendError>;
    /// 出力を `out`（`finish` が返した長さちょうど）に写す。
    fn copy(&mut self, out: &mut [u8]) -> Result<(), BackendError>;
}

/// AAC の backend を `Encoder` にする adapter（`Tier::Platform`・バイト列の再現を約束しない）。形式は `m4a` だけ。
pub struct PlatformAacEncoder<B> {
    backend: B,
}

impl<B: AacBackend> PlatformAacEncoder<B> {
    /// backend を包む。
    pub fn new(backend: B) -> Self {
        Self { backend }
    }

    /// 包んだ backend。
    pub fn backend(&self) -> &B {
        &self.backend
    }

    fn invalid(key: &'static str, message: String) -> EngineError {
        EngineError::Invalid(Issues::one(
            Issue::error(IssueCode::OutOfRange, message).param(key),
        ))
    }

    /// 設定を読む（範囲・型は `CodecSet::encode_bytes` が `options` で確かめたもの。直接呼ばれたときも断る）。
    fn settings(
        &self,
        rate: u32,
        channels: u16,
        opts: &serde_json::Value,
    ) -> Result<AacSettings, EngineError> {
        let mode = match opts.get("mode").and_then(serde_json::Value::as_str) {
            None => AacMode::VbrConstrained,
            Some("vbrConstrained") => AacMode::VbrConstrained,
            Some("vbr") => AacMode::Vbr,
            Some("abr") => AacMode::Abr,
            Some("cbr") => AacMode::Cbr,
            Some(other) => {
                return Err(Self::invalid(
                    "mode",
                    format!("知らない mode です（{other}）"),
                ));
            }
        };
        let quality = match opts.get("quality") {
            None => DEFAULT_QUALITY,
            Some(q) => q
                .as_u64()
                .and_then(|q| u32::try_from(q).ok())
                .filter(|q| *q <= 127)
                .ok_or_else(|| Self::invalid("quality", "quality は 0〜127 です".into()))?,
        };
        let choices = self.backend.bitrates(rate, channels);
        let bitrate_kbps = match opts.get("bitrate") {
            Some(b) => {
                let b = b.as_u64().and_then(|b| u32::try_from(b).ok());
                match b {
                    Some(b) if mode == AacMode::Vbr || choices.contains(&b) => b,
                    _ => {
                        return Err(Self::invalid(
                            "bitrate",
                            format!(
                                "{rate} Hz・{channels} チャンネルで選べるビットレートは {choices:?} kbps です"
                            ),
                        ));
                    }
                }
            }
            None => default_bitrate(&choices).unwrap_or(DEFAULT_KBPS),
        };
        let channel_mask = default_channel_mask(channels).ok_or_else(|| {
            EngineError::Unsupported(format!("AAC に {channels} チャンネルは書けません"))
        })?;
        Ok(AacSettings {
            rate,
            channels,
            channel_mask,
            mode,
            bitrate_kbps,
            quality,
        })
    }
}

/// 既定のビットレート：256 kbps が選べればそれ、なければ 256 以下で最大のもの、それもなければ最小のもの。
fn default_bitrate(choices: &[u32]) -> Option<u32> {
    if choices.contains(&DEFAULT_KBPS) {
        return Some(DEFAULT_KBPS);
    }
    choices
        .iter()
        .copied()
        .filter(|b| *b <= DEFAULT_KBPS)
        .max()
        .or_else(|| choices.iter().copied().min())
}

/// 書いた m4a の長さの上限（バイト）：AAC の 1 フレーム（1,024 標本）・1 チャンネルは 6,144 ビット（768 バイト）まで
/// （ISO/IEC 14496-3 のバッファの上限）。priming（2,112）と remainder のぶん 4 フレーム、表の 1 フレームあたり 16 バイト、
/// 入れ物に 1 MiB を足す。
pub fn aac_max_output(frames: u64, channels: u16) -> u64 {
    let packets = frames.div_ceil(1_024).saturating_add(4);
    packets
        .saturating_mul(
            768u64
                .saturating_mul(u64::from(channels))
                .saturating_add(16),
        )
        .saturating_add(1 << 20)
}

impl<B: AacBackend> Encoder for PlatformAacEncoder<B> {
    fn id(&self) -> &'static str {
        self.backend.id()
    }

    fn tier(&self) -> Tier {
        Tier::Platform
    }

    fn reproducible(&self) -> bool {
        false
    }

    fn env(&self) -> Option<CodecEnv> {
        self.backend.env()
    }

    fn can_encode(&self, format: &str, shape: &Shape) -> bool {
        format == "m4a"
            && matches!(shape, Shape::Audio { rate, channels, frames }
                if (1..=MAX_CHANNELS).contains(channels)
                    && frames.max() >= 1
                    && self.backend.supports(*rate, *channels))
    }

    // `writes_format` は既定の実装（代表の形 × `can_encode`）に任せる：backend の能力（`supports`）が空なら、能力の表に
    // 「m4a を書ける」と出さない（AudioToolbox の問い合わせに失敗してレートの表が空のときなど）。

    fn options(&self, _format: &str) -> ParamSchema {
        ParamSchema {
            params: vec![
                spec(
                    "mode",
                    ParamKind::Enum { values: MODES },
                    Unit::None,
                    json!("vbrConstrained"),
                ),
                spec(
                    "bitrate",
                    ParamKind::Int {
                        min: 8,
                        max: 960,
                        step: 1,
                    },
                    Unit::None,
                    json!(DEFAULT_KBPS),
                ),
                spec(
                    "quality",
                    ParamKind::Int {
                        min: 0,
                        max: 127,
                        step: 1,
                    },
                    Unit::None,
                    json!(DEFAULT_QUALITY),
                ),
            ],
        }
    }

    /// タグ・ループ点・マーカーは書かない（m4a のタグを書くのは lofty を選び直す段）。
    fn caps(&self, _format: &str, _opts: &serde_json::Value) -> MetaCaps {
        NONE
    }

    fn encode(
        &self,
        value: &Value,
        format: &str,
        opts: &serde_json::Value,
        out: &mut dyn Write,
        mon: &dyn Monitor,
    ) -> Result<u64, EngineError> {
        let id = self.backend.id();
        let Value::Audio(audio) = value else {
            return Err(EngineError::Unsupported(
                "m4a には音声だけを書けます".into(),
            ));
        };
        if !self.can_encode(format, &value.shape()) {
            return Err(EngineError::Unsupported(format!(
                "{id}: {} Hz・{} チャンネルの AAC は書けません",
                audio.rate(),
                audio.channels()
            )));
        }
        let channels = u16::try_from(audio.channels())
            .map_err(|_| EngineError::Unsupported("チャンネルが多すぎます".into()))?;
        let settings = self.settings(audio.rate(), channels, opts)?;
        let frames = audio.frames();
        let max_out = aac_max_output(frames as u64, channels);
        let mut h = self
            .backend
            .open(&settings, max_out)
            .map_err(|e| e.into_engine(id, Stage::Encode))?;
        let mut scratch: Vec<Vec<f32>> =
            vec![Vec::with_capacity(WRITE_FRAMES.min(frames)); audio.channels()];
        let mut off = 0usize;
        while off < frames {
            if mon.canceled() {
                return Err(EngineError::Canceled);
            }
            let n = WRITE_FRAMES.min(frames - off);
            for (dst, src) in scratch.iter_mut().zip(audio.planes()) {
                dst.clear();
                dst.extend(src[off..off + n].iter().map(|&s| sanitize(s)));
            }
            let views: Vec<&[f32]> = scratch.iter().map(Vec::as_slice).collect();
            h.write(&views)
                .map_err(|e| e.into_engine(id, Stage::Encode))?;
            off += n;
            mon.progress(off as u64, frames as u64);
        }
        if mon.canceled() {
            return Err(EngineError::Canceled);
        }
        let len = h
            .finish(mon)
            .map_err(|e| e.into_engine(id, Stage::Encode))?;
        if len > max_out {
            return Err(EngineError::Encode {
                codec: id.to_string(),
                message: format!("書いた m4a が大きすぎます（{len} > {max_out} バイト）"),
            });
        }
        let n =
            usize::try_from(len).map_err(|_| EngineError::Limit("出力が大きすぎます".into()))?;
        let mut buf = crate::codec::try_vec(n, 0u8)?;
        h.copy(&mut buf)
            .map_err(|e| e.into_engine(id, Stage::Encode))?;
        drop(h);
        zero_mp4_times(&mut buf).map_err(|m| EngineError::Encode {
            codec: id.to_string(),
            message: format!("書いた m4a の形が読めません: {m}"),
        })?;
        out.write_all(&buf)?;
        Ok(len)
    }
}

/// 渡す標本を直す：NaN は 0、±∞ は ±1、有限の値はそのまま。
fn sanitize(s: f32) -> f32 {
    if s.is_nan() {
        0.0
    } else if s.is_infinite() {
        s.signum()
    } else {
        s
    }
}

/// ISO BMFF の箱 1 つ（中身の始まり・終わり・種類）。
pub(crate) struct Mp4Box {
    pub(crate) body: usize,
    pub(crate) end: usize,
    pub(crate) kind: [u8; 4],
}

/// `buf[start..end]` に並ぶ箱を読む（大きさ 0 は終わりまで、1 は 64 ビットの大きさ）。外にはみ出す・8 バイトより小さい
/// 箱は壊れた形。各箱は 8 バイト以上なので、ループは必ず進む。
pub(crate) fn boxes(buf: &[u8], start: usize, end: usize) -> Result<Vec<Mp4Box>, String> {
    let mut out = Vec::new();
    let mut pos = start;
    while pos < end {
        let head = buf
            .get(pos..pos.checked_add(8).ok_or("位置があふれます")?)
            .filter(|_| end - pos >= 8)
            .ok_or_else(|| format!("{pos} の箱の頭が切れています"))?;
        let size32 = u32::from_be_bytes([head[0], head[1], head[2], head[3]]);
        let kind = [head[4], head[5], head[6], head[7]];
        let (size, header) = match size32 {
            0 => (end - pos, 8usize),
            1 => {
                let large = buf
                    .get(pos + 8..pos + 16)
                    .filter(|_| end - pos >= 16)
                    .ok_or_else(|| format!("{pos} の箱の 64 ビットの大きさが切れています"))?;
                let mut b = [0u8; 8];
                b.copy_from_slice(large);
                let size = usize::try_from(u64::from_be_bytes(b))
                    .map_err(|_| format!("{pos} の箱が大きすぎます"))?;
                (size, 16usize)
            }
            n => (n as usize, 8usize),
        };
        if size < header || size > end - pos {
            return Err(format!("{pos} の箱の大きさ {size} が範囲に合いません"));
        }
        out.push(Mp4Box {
            body: pos + header,
            end: pos + size,
            kind,
        });
        pos += size;
    }
    Ok(out)
}

/// フルボックス（版・旗のあとに作成・変更の時刻）の時刻を 0 にする。版 0 は 32 ビット × 2、版 1 は 64 ビット × 2。
fn zero_full_box_times(buf: &mut [u8], b: &Mp4Box) -> Result<(), String> {
    let version = *buf.get(b.body).ok_or("フルボックスの版がありません")?;
    let width = match version {
        0 => 8,
        1 => 16,
        v => return Err(format!("知らない版です（{v}）")),
    };
    let start = b.body + 4;
    let end = start + width;
    if end > b.end {
        return Err("フルボックスの時刻が切れています".into());
    }
    buf[start..end].fill(0);
    Ok(())
}

/// m4a（ISO BMFF）の moov の mvhd、moov/trak の tkhd、moov/trak/mdia の mdhd の作成・変更の時刻を 0 にする（箱の大きさは
/// 変えない。docs §9.2 の「時刻は書かない」）。書き換えた箱の数を返す。moov か mvhd がない・箱の形が壊れていれば `Err`。
/// 入れ子は 3 段だけ見る（再帰しない）。
pub fn zero_mp4_times(buf: &mut [u8]) -> Result<usize, String> {
    let mut n = 0usize;
    let mut mvhd = false;
    for moov in boxes(buf, 0, buf.len())?
        .into_iter()
        .filter(|b| &b.kind == b"moov")
    {
        for b in boxes(buf, moov.body, moov.end)? {
            match &b.kind {
                b"mvhd" => {
                    zero_full_box_times(buf, &b)?;
                    mvhd = true;
                    n += 1;
                }
                b"trak" => {
                    for t in boxes(buf, b.body, b.end)? {
                        match &t.kind {
                            b"tkhd" => {
                                zero_full_box_times(buf, &t)?;
                                n += 1;
                            }
                            b"mdia" => {
                                for m in boxes(buf, t.body, t.end)?
                                    .into_iter()
                                    .filter(|m| &m.kind == b"mdhd")
                                {
                                    zero_full_box_times(buf, &m)?;
                                    n += 1;
                                }
                            }
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
        }
    }
    if !mvhd {
        return Err("moov・mvhd がありません".into());
    }
    Ok(n)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use rand_chacha::rand_core::Rng;

    use super::*;
    use crate::codec::CodecSet;
    use crate::monitor::{CancelAfter, NoMonitor};
    use crate::testing::platform::{AacFault, FakeAacBackend, fake_m4a, fake_m4a_payload};
    use crate::value::{Audio, AudioMeta};

    fn audio(rate: u32, planes: Vec<Vec<f32>>) -> Value {
        Value::Audio(Arc::new(
            Audio::new(
                rate,
                planes.into_iter().map(Arc::new).collect(),
                AudioMeta::default(),
            )
            .unwrap(),
        ))
    }

    fn set_with(b: FakeAacBackend) -> CodecSet {
        let mut set = CodecSet::pinned();
        set.add_encoder(Arc::new(PlatformAacEncoder::new(b)))
            .unwrap();
        set
    }

    /// 書いた m4a：PCM は直してから 4,096 フレームずつ渡り、時刻は 0 になり、記録は Platform・R2 なし・環境つき。
    #[test]
    fn writes_m4a_with_zeroed_times() {
        let b = FakeAacBackend::new();
        let last = b.last_settings();
        let set = set_with(b);
        let frames: usize = 10_000;
        let left: Vec<f32> = (0..frames).map(|i| (i % 100) as f32 / 100.0).collect();
        let mut right = left.clone();
        right[0] = f32::NAN;
        right[1] = f32::INFINITY;
        right[2] = f32::NEG_INFINITY;
        right[3] = 3.5;
        let v = audio(48_000, vec![left.clone(), right.clone()]);
        let (bytes, stamp) = set
            .encode_bytes(&v, "m4a", &serde_json::json!({}), &NoMonitor)
            .unwrap();
        assert_eq!(
            (stamp.codec.as_str(), stamp.tier, stamp.reproducible),
            ("fake-aac-enc/1", Tier::Platform, false)
        );
        assert_eq!(stamp.env, Some(FakeAacBackend::default_env()));
        let s = last.lock().unwrap().unwrap();
        assert_eq!(
            s,
            AacSettings {
                rate: 48_000,
                channels: 2,
                channel_mask: 0x3,
                mode: AacMode::VbrConstrained,
                bitrate_kbps: 256,
                quality: 127,
            }
        );
        // 時刻は 0（偽の backend は 0 でない時刻を書く）
        let mut again = bytes.clone();
        assert_eq!(zero_mp4_times(&mut again), Ok(3));
        assert_eq!(again, bytes);
        let (got, chunks) = fake_m4a_payload(&bytes).unwrap();
        assert_eq!(chunks, frames.div_ceil(WRITE_FRAMES));
        right[0] = 0.0;
        right[1] = 1.0;
        right[2] = -1.0;
        assert_eq!(got, [left, right]);
        // JSON の記録に R2 なしと環境が出る
        let json = serde_json::to_value(&stamp).unwrap();
        assert_eq!(json["reproducible"], false);
        assert_eq!(json["env"]["tool"], "fake");
    }

    /// 設定：既定・明示した値・選べないビットレート・知らない値。
    #[test]
    fn options_are_checked_against_the_backend() {
        let b = FakeAacBackend::new();
        let last = b.last_settings();
        let set = set_with(b);
        let v = audio(48_000, vec![vec![0.1; 100]]);
        for (opts, want) in [
            (
                serde_json::json!({ "bitrate": 128 }),
                (AacMode::VbrConstrained, 128, 127),
            ),
            (
                serde_json::json!({ "mode": "cbr", "bitrate": 320 }),
                (AacMode::Cbr, 320, 127),
            ),
            (
                serde_json::json!({ "mode": "vbr", "quality": 90 }),
                (AacMode::Vbr, 256, 90),
            ),
            (
                serde_json::json!({ "mode": "abr" }),
                (AacMode::Abr, 256, 127),
            ),
        ] {
            set.encode_bytes(&v, "m4a", &opts, &NoMonitor).unwrap();
            let s = last.lock().unwrap().unwrap();
            assert_eq!((s.mode, s.bitrate_kbps, s.quality), want, "{opts}");
        }
        for bad in [
            serde_json::json!({ "bitrate": 100 }),
            serde_json::json!({ "bitrate": 1000 }),
            serde_json::json!({ "mode": "he" }),
            serde_json::json!({ "quality": 128 }),
            serde_json::json!({ "profile": "lc" }),
        ] {
            assert!(
                matches!(
                    set.encode_bytes(&v, "m4a", &bad, &NoMonitor),
                    Err(EngineError::Invalid(_))
                ),
                "{bad}"
            );
        }
        // 256 を選べない形では、256 以下で最大のもの
        let mut b = FakeAacBackend::new();
        b.bitrates = vec![32, 48, 64, 96];
        let last = b.last_settings();
        let set = set_with(b);
        set.encode_bytes(&v, "m4a", &serde_json::json!({}), &NoMonitor)
            .unwrap();
        assert_eq!(last.lock().unwrap().unwrap().bitrate_kbps, 96);
        assert_eq!(default_bitrate(&[320, 384]), Some(320));
        assert_eq!(default_bitrate(&[]), None);
    }

    /// 書ける形：m4a・1〜8 チャンネル・backend のレート・空でない。範囲の外は次の段へ（`encoder` が選ばない）。
    #[test]
    fn can_encode_follows_the_backend() {
        let set = set_with(FakeAacBackend::new());
        let shape = |rate, channels, frames| Shape::Audio {
            rate,
            channels,
            frames: crate::shape::Dim::Exact(frames),
        };
        let pick = |s: Shape| set.encoder("m4a", &s).map(|e| e.id());
        assert_eq!(pick(shape(48_000, 2, 10)), Some("fake-aac-enc/1"));
        assert_eq!(pick(shape(8_000, 8, 10)), Some("fake-aac-enc/1"));
        assert_eq!(pick(shape(96_000, 2, 10)), None);
        assert_eq!(pick(shape(48_000, 9, 10)), None);
        assert_eq!(pick(shape(48_000, 2, 0)), None);
        assert!(set.encoder("mp3", &shape(48_000, 2, 10)).is_none());
        let caps = set.format_caps("m4a").unwrap();
        assert_eq!(caps.write.len(), 1);
        assert_eq!(caps.write[0].meta, NONE);
        assert_eq!(caps.write[0].options.params.len(), 3);
    }

    /// 能力の表は backend の能力に従う（`writes_format` は既定の実装＝代表の形 × `can_encode`）：どのレート・チャンネルも
    /// 書けない backend（AudioToolbox の問い合わせに失敗してレートの表が空など）なら「m4a を書ける」と出さない。代表の形の
    /// どれか 1 つでも書ければ出る。
    #[test]
    fn caps_follow_the_backend() {
        let writable = |b: FakeAacBackend| {
            let set = set_with(b);
            let e = set.encoder_by_id("fake-aac-enc/1").unwrap().clone();
            let caps = set.format_caps("m4a").unwrap();
            assert_eq!(caps.writable(), e.writes_format("m4a"));
            assert!(!e.writes_format("mp3") && !e.writes_format("wav"));
            caps.writable()
        };
        assert!(writable(FakeAacBackend::new()));
        let mut b = FakeAacBackend::new();
        b.rates.clear();
        assert!(!writable(b));
        let mut b = FakeAacBackend::new();
        b.max_channels = 0;
        assert!(!writable(b));
        let mut b = FakeAacBackend::new();
        b.rates = vec![44_100];
        b.max_channels = 1;
        assert!(writable(b));
        // 書けない backend では、書こうとしても扱えない（能力の表と食い違わない）
        let mut b = FakeAacBackend::new();
        b.rates.clear();
        let set = set_with(b);
        assert!(matches!(
            set.encode_bytes(
                &audio(48_000, vec![vec![0.1; 100]]),
                "m4a",
                &serde_json::json!({}),
                &NoMonitor
            ),
            Err(EngineError::Unsupported(_))
        ));
    }

    /// 中止は 4,096 フレームの合間（1〜4 回目）。backend の失敗・大きすぎる出力・壊れた m4a はエラー。
    #[test]
    fn cancel_and_failures() {
        let v = audio(48_000, vec![vec![0.25; 20_000]]);
        for after in 1..=4 {
            let set = set_with(FakeAacBackend::new());
            assert!(
                matches!(
                    set.encode_bytes(&v, "m4a", &serde_json::json!({}), &CancelAfter::new(after)),
                    Err(EngineError::Canceled)
                ),
                "{after}"
            );
        }
        for (fault, check) in [
            (
                AacFault::Write(1, BackendError::Failed("x".into())),
                (|e: &EngineError| matches!(e, EngineError::Encode { .. }))
                    as fn(&EngineError) -> bool,
            ),
            (AacFault::Finish(BackendError::Limit("x".into())), |e| {
                matches!(e, EngineError::Limit(_))
            }),
            (AacFault::Open(BackendError::Unsupported("x".into())), |e| {
                matches!(e, EngineError::Unsupported(_))
            }),
            (AacFault::Oversize, |e| {
                matches!(e, EngineError::Encode { .. })
            }),
            (AacFault::Broken, |e| {
                matches!(e, EngineError::Encode { .. })
            }),
        ] {
            let mut b = FakeAacBackend::new();
            b.fault = Some(fault.clone());
            let set = set_with(b);
            let err = set
                .encode_bytes(&v, "m4a", &serde_json::json!({}), &NoMonitor)
                .unwrap_err();
            assert!(check(&err), "{fault:?}: {err:?}");
        }
    }

    /// 上限：AAC の 1 フレーム 1 チャンネル 768 バイトから決めた値。
    #[test]
    fn max_output_bound() {
        assert_eq!(aac_max_output(0, 2), 4 * (768 * 2 + 16) + (1 << 20));
        assert_eq!(aac_max_output(1_024, 1), 5 * (768 + 16) + (1 << 20));
        assert_eq!(aac_max_output(u64::MAX, u16::MAX), u64::MAX);
    }

    /// 時刻の書き換え：版 0・1、64 ビットの大きさ、終わりまでの大きさ 0、複数の trak。ほかのバイトは変えない。
    #[test]
    fn zero_times_in_handmade_boxes() {
        let m4a = fake_m4a(b"payload", 0xAABB_CCDD);
        let mut z = m4a.clone();
        assert_eq!(zero_mp4_times(&mut z), Ok(3));
        let diff = m4a.iter().zip(&z).filter(|(a, b)| a != b).count();
        assert!(diff > 0 && diff <= 8 + 8 + 16);
        assert!(z.windows(4).all(|w| w != [0xAA, 0xBB, 0xCC, 0xDD]));
        // moov のない・壊れた入力は Err
        assert!(zero_mp4_times(&mut b"\0\0\0\x08free".to_vec()).is_err());
        assert!(zero_mp4_times(&mut Vec::new()).is_err());
        assert!(zero_mp4_times(&mut b"\0\0\0\x04moov".to_vec()).is_err());
        let mut v2 = m4a.clone();
        // mvhd の版を 2 にする
        let at = v2.windows(4).position(|w| w == b"mvhd").unwrap() + 4;
        v2[at] = 2;
        assert!(zero_mp4_times(&mut v2).is_err());
    }

    /// 壊れた入力（ChaCha8 で切る・書き換える）でも落ちず、Ok なら箱の大きさの欄は変えていない。
    #[test]
    fn broken_boxes_never_panic() {
        let base = fake_m4a(&[7u8; 300], 0x1234_5678);
        let mut rng = crate::determinism::rng(0x6d34_u64);
        for cut in 0..base.len() {
            let mut b = base[..cut].to_vec();
            let _ = zero_mp4_times(&mut b);
        }
        for _ in 0..5_000 {
            let mut b = base.clone();
            for _ in 0..1 + rng.next_u32() % 4 {
                let i = rng.next_u32() as usize % b.len();
                b[i] = rng.next_u32() as u8;
            }
            let before = b.clone();
            if zero_mp4_times(&mut b).is_ok() {
                assert_eq!(b.len(), before.len());
            }
        }
    }
}
