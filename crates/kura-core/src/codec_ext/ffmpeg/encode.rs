//! ffmpeg の MP3 の書き出し（`mp3-ffmpeg-enc/1`。`libmp3lame`）。
//!
//! 約束（識別子の版に入る）：
//! - 書けるのは 1・2 チャンネル、`libmp3lame` が書けるレート（MPEG-1：32・44.1・48 kHz、MPEG-2：16・22.05・24 kHz、
//!   MPEG-2.5：8・11.025・12 kHz）、1 フレーム以上。ほかは `can_encode` が偽（SRC・モノラル化を先に）。
//! - 設定：`mode`（`cbr` 既定・`vbr`）、`bitrate`（kbps。`cbr` で使う。既定 192。そのレートの MPEG の版で選べなければ、
//!   選べる値のうち 192 以下で最大のもの。明示した値が選べなければ `Invalid`）、`quality`（LAME の `-V`。0〜9。`vbr` で
//!   使う。既定 2）。
//! - 入力の標本は NaN → 0、±∞ → ±1、有限の値は ±1 に切り詰めてから渡す（LAME は NaN・f32::MAX で abort する）。
//!   4,096 フレームずつインターリーブした f32le を標準入力に書く。
//! - 出力は作業フォルダの一時ファイル（シークできる出力でないと Xing/LAME のフレームが書かれず、gapless の長さが失われる）。
//!   書いた大きさを 320 kbps の上限から決めた大きさで確かめてから `out` に写す。ID3・タグ・ループ点・マーカーは書かない。
//! - バイト列は同じ ffmpeg・同じ LAME・同じ設定なら同じ（`+bitexact`）だが、約束しない（`reproducible()` は偽）。

use std::io::{Read, Write};
use std::sync::Arc;

use serde_json::json;

use kura_engine::codec::formats::NONE;
use kura_engine::codec::{CodecEnv, Encoder, MetaCaps, Stage, Tier};
use kura_engine::error::{EngineError, Issue, IssueCode, Issues};
use kura_engine::monitor::Monitor;
use kura_engine::op::{ParamKind, ParamSchema, Unit, spec};
use kura_engine::shape::Shape;
use kura_engine::value::Value;

use super::args::{Mp3Rate, layout_name};
use super::env::Detected;
use super::{STALL, Shared, args, deadline_for, failure, io_error, run_error};
use crate::tools::{Budget, run_binary};

/// コーデックの識別子。
pub const ID: &str = "mp3-ffmpeg-enc/1";
/// 既定のビットレート（kbps）。
pub const DEFAULT_KBPS: u32 = 192;
/// `vbr` の既定の品質（LAME の `-V 2`。`--preset standard`）。
pub const DEFAULT_QUALITY: u8 = 2;
/// 設定 `mode` の値。
pub const MODES: &[&str] = &["cbr", "vbr"];
/// 1 回に書くフレームの数。
pub const WRITE_FRAMES: usize = 4_096;
/// 写すときの区切り（合間で中止を見る）。
const COPY_CHUNK: usize = 1 << 20;

/// MPEG-1 のビットレート（kbps）。
const MPEG1: [u32; 14] = [
    32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320,
];
/// MPEG-2 のビットレート（kbps）。
const MPEG2: [u32; 14] = [8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 144, 160];
/// MPEG-2.5 のビットレート（kbps）。
const MPEG25: [u32; 8] = [8, 16, 24, 32, 40, 48, 56, 64];

/// このレートで選べるビットレート（MP3 にないレートは空）。
pub fn bitrates(rate: u32) -> &'static [u32] {
    match rate {
        32_000 | 44_100 | 48_000 => &MPEG1,
        16_000 | 22_050 | 24_000 => &MPEG2,
        8_000 | 11_025 | 12_000 => &MPEG25,
        _ => &[],
    }
}

/// 渡す前の標本の直し方：NaN → 0、±∞ → ±1、有限の値は ±1 に切り詰める。
pub fn sanitize(x: f32) -> f32 {
    if x.is_nan() { 0.0 } else { x.clamp(-1.0, 1.0) }
}

/// 書いた MP3 の大きさの上限（バイト）：320 kbps（1 秒 40,000 バイト）の 1.1 倍に、ヘッダなどの 64 KiB を足す。
pub fn max_output_bytes(frames: u64, rate: u32) -> u64 {
    if rate == 0 {
        return 0;
    }
    let body = u128::from(frames) * 40_000 * 11 / (10 * u128::from(rate));
    u64::try_from(body)
        .unwrap_or(u64::MAX)
        .saturating_add(64 << 10)
}

/// 既定のビットレート：192 が選べればそれ、なければ 192 以下で最大のもの、それもなければ最小のもの。
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

fn invalid(key: &'static str, message: String) -> EngineError {
    EngineError::Invalid(Issues::one(
        Issue::error(IssueCode::OutOfRange, message).param(key),
    ))
}

/// 設定を読む（範囲・型は `CodecSet::encode_bytes` が `options` で確かめたもの。直接呼ばれたときも断る）。
pub fn settings(rate: u32, opts: &serde_json::Value) -> Result<Mp3Rate, EngineError> {
    let vbr = match opts.get("mode").and_then(serde_json::Value::as_str) {
        None | Some("cbr") => false,
        Some("vbr") => true,
        Some(other) => return Err(invalid("mode", format!("知らない mode です（{other}）"))),
    };
    if vbr {
        let q = match opts.get("quality") {
            None => DEFAULT_QUALITY,
            Some(q) => q
                .as_u64()
                .and_then(|q| u8::try_from(q).ok())
                .filter(|q| *q <= 9)
                .ok_or_else(|| invalid("quality", "quality は 0〜9 です".into()))?,
        };
        return Ok(Mp3Rate::Vbr(q));
    }
    let choices = bitrates(rate);
    let kbps = match opts.get("bitrate") {
        Some(b) => b
            .as_u64()
            .and_then(|b| u32::try_from(b).ok())
            .filter(|b| choices.contains(b))
            .ok_or_else(|| {
                invalid(
                    "bitrate",
                    format!("{rate} Hz で選べるビットレートは {choices:?} kbps です"),
                )
            })?,
        None => default_bitrate(choices)
            .ok_or_else(|| EngineError::Unsupported(format!("{rate} Hz は mp3 に書けません")))?,
    };
    Ok(Mp3Rate::Cbr(kbps))
}

/// ffmpeg の MP3 の書き出し。
pub struct Mp3FfmpegEncoder {
    shared: Arc<Shared>,
}

impl Mp3FfmpegEncoder {
    pub(crate) fn new(shared: Arc<Shared>) -> Self {
        Self { shared }
    }

    /// この ffmpeg で書けるか（`libmp3lame`・mp3 の多重化器・f32le の分離器がある）。
    pub fn available(d: &Detected) -> bool {
        d.encoders.contains("libmp3lame")
            && d.muxers.contains("mp3")
            && d.demuxers.contains("f32le")
            && !d.lame_rates.is_empty()
    }

    fn shape_ok(d: &Detected, rate: u32, channels: u16, frames: u64) -> bool {
        layout_name(channels).is_some()
            && frames >= 1
            && d.lame_rates.contains(&rate)
            && !bitrates(rate).is_empty()
    }
}

impl Encoder for Mp3FfmpegEncoder {
    fn id(&self) -> &'static str {
        ID
    }

    fn tier(&self) -> Tier {
        Tier::External
    }

    fn env(&self) -> Option<CodecEnv> {
        let d = self.shared.detected();
        let mut env = d.env().with("codec", "libmp3lame");
        if let Some(v) = &d.lame_version {
            env = env.with("lame", v.clone());
        }
        Some(env)
    }

    fn can_encode(&self, format: &str, shape: &Shape) -> bool {
        let d = self.shared.detected();
        format == "mp3"
            && matches!(shape, Shape::Audio { rate, channels, frames }
                if Self::shape_ok(&d, *rate, *channels, frames.max()))
    }

    fn options(&self, _format: &str) -> ParamSchema {
        ParamSchema {
            params: vec![
                spec(
                    "mode",
                    ParamKind::Enum { values: MODES },
                    Unit::None,
                    json!("cbr"),
                ),
                spec(
                    "bitrate",
                    ParamKind::Int {
                        min: 8,
                        max: 320,
                        step: 1,
                    },
                    Unit::None,
                    json!(DEFAULT_KBPS),
                ),
                spec(
                    "quality",
                    ParamKind::Int {
                        min: 0,
                        max: 9,
                        step: 1,
                    },
                    Unit::None,
                    json!(DEFAULT_QUALITY),
                ),
            ],
        }
    }

    /// タグ・ループ点・マーカーは書かない（ID3 を書かない）。
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
        let Value::Audio(audio) = value else {
            return Err(EngineError::Unsupported(
                "mp3 に書けるのは音声だけです".into(),
            ));
        };
        let (rate, frames) = (audio.rate(), audio.frames());
        let channels = u16::try_from(audio.channels()).unwrap_or(u16::MAX);
        if !self.can_encode(format, &value.shape()) {
            return Err(EngineError::Unsupported(format!(
                "{rate} Hz・{channels} チャンネル・{frames} フレームの音声は mp3 に書けません"
            )));
        }
        let layout = layout_name(channels).ok_or_else(|| {
            EngineError::Unsupported(format!("mp3 に {channels} チャンネルは書けません"))
        })?;
        let mode = settings(rate, opts)?;
        self.shared.refresh()?;
        let _permit = self.shared.acquire(mon)?;
        let dir = self.shared.work_dir()?;
        // 名前を取ってから ffmpeg に書かせる（`-y` で、作った空のファイルを上書きする）
        let (tmp, _) = self.shared.create_temp("ffenc", "mp3")?;
        let a = args::mp3_encode(rate, layout, mode, tmp.path());
        let secs = frames as f64 / f64::from(rate);
        let budget = Budget {
            max_stdout: 1 << 20,
            stall: STALL,
            deadline: deadline_for(secs),
        };
        let planes = audio.planes();
        let total = frames as u64;
        let mut feed = |w: &mut dyn Write| -> std::io::Result<()> {
            let mut buf: Vec<u8> = Vec::with_capacity(WRITE_FRAMES * planes.len() * 4);
            let mut off = 0usize;
            while off < frames {
                let n = WRITE_FRAMES.min(frames - off);
                buf.clear();
                for i in off..off + n {
                    for p in planes {
                        buf.extend_from_slice(&sanitize(p[i]).to_le_bytes());
                    }
                }
                w.write_all(&buf)?;
                off += n;
                mon.progress(off as u64, total);
            }
            Ok(())
        };
        let outcome = run_binary(
            self.shared.ffmpeg(),
            &a,
            dir,
            &budget,
            &|| mon.canceled(),
            Some(&mut feed),
            &mut |_: &[u8]| Ok(()),
        )
        .map_err(|e| run_error(ID, Stage::Encode, e, &budget))?;
        if mon.canceled() {
            return Err(EngineError::Canceled);
        }
        if let Some(e) = failure(ID, Stage::Encode, &outcome) {
            return Err(e);
        }
        if let Some(e) = outcome.feed_error {
            return Err(io_error(
                ID,
                Stage::Encode,
                format!("ffmpeg に標本を渡せませんでした: {e}"),
            ));
        }
        let len = std::fs::metadata(tmp.path())
            .map_err(|e| {
                io_error(
                    ID,
                    Stage::Encode,
                    format!("ffmpeg が出力を書きませんでした: {e}"),
                )
            })?
            .len();
        // 名前を取るときに空のファイルを作っているので、書かなかったときは 0 バイトのまま残る
        if len == 0 {
            return Err(io_error(
                ID,
                Stage::Encode,
                "ffmpeg が出力を書きませんでした（0 バイト）",
            ));
        }
        let max = max_output_bytes(total, rate);
        if len > max {
            return Err(io_error(
                ID,
                Stage::Encode,
                format!("ffmpeg の出力の大きさが想定と違います（{len} バイト、上限 {max}）"),
            ));
        }
        let mut f = std::fs::File::open(tmp.path())?;
        let mut buf = vec![0u8; COPY_CHUNK];
        let mut copied = 0u64;
        loop {
            if mon.canceled() {
                return Err(EngineError::Canceled);
            }
            let n = match f.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => n,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e.into()),
            };
            copied += n as u64;
            if copied > len {
                return Err(io_error(
                    ID,
                    Stage::Encode,
                    "出力の大きさが途中で変わりました",
                ));
            }
            out.write_all(&buf[..n])?;
        }
        if copied != len {
            return Err(io_error(
                ID,
                Stage::Encode,
                "出力の大きさが途中で変わりました",
            ));
        }
        Ok(copied)
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::{Duration, Instant};

    use kura_engine::codec::CodecSet;
    use kura_engine::doc::DecodeOptions;
    use kura_engine::limits::Limits;
    use kura_engine::monitor::NoMonitor;
    use kura_engine::shape::Dim;
    use kura_engine::value::{Audio, AudioMeta};

    use super::*;
    use crate::codec_ext::ffmpeg::FfmpegCodecs;
    use crate::codec_ext::ffmpeg::testing::{FakeTools, real_tools};

    fn audio_value(rate: u32, planes: Vec<Vec<f32>>) -> Value {
        Value::Audio(Arc::new(
            Audio::new(
                rate,
                planes.into_iter().map(Arc::new).collect(),
                AudioMeta::default(),
            )
            .unwrap(),
        ))
    }

    /// 偽物：書き出しは標準入力をそのまま出力のパスに写し、引数を args.log に書く。
    fn fake(encode_body: &str) -> FakeTools {
        FakeTools::new("exit 1", "exit 1", encode_body)
    }

    fn copying() -> FakeTools {
        fake("printf '%s\\n' \"$@\" > \"$(dirname \"$0\")/args.log\"; cat > \"$out\"")
    }

    fn set_with(codecs: &FfmpegCodecs) -> CodecSet {
        let mut set = CodecSet::pinned();
        codecs.add_to(&mut set).unwrap();
        set
    }

    fn args_of(t: &FakeTools) -> Vec<String> {
        std::fs::read_to_string(t.path("args.log"))
            .unwrap()
            .lines()
            .map(str::to_string)
            .collect()
    }

    fn has(args: &[String], pair: [&str; 2]) -> bool {
        args.windows(2).any(|w| w[0] == pair[0] && w[1] == pair[1])
    }

    /// 渡す標本は直してからインターリーブする。出力は一時ファイルから写し、一時ファイルは残らない。記録は External・
    /// 再現なし・LAME の版つき。
    #[test]
    fn samples_are_sanitized_interleaved_and_copied() {
        let t = copying();
        let codecs = t.codecs();
        let set = set_with(&codecs);
        let left = vec![0.5, f32::NAN, f32::INFINITY, 2.0, -0.25];
        let right = vec![-0.5, f32::NEG_INFINITY, -3.0, f32::MAX, 1.0];
        let v = audio_value(44_100, vec![left, right]);
        let (bytes, stamp) = set.encode_bytes(&v, "mp3", &json!({}), &NoMonitor).unwrap();
        let got: Vec<f32> = bytes
            .chunks_exact(4)
            .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
            .collect();
        assert_eq!(got, [0.5, -0.5, 0.0, -1.0, 1.0, -1.0, 1.0, 1.0, -0.25, 1.0]);
        assert_eq!(stamp.bytes, 40);
        assert_eq!(
            (stamp.codec.as_str(), stamp.tier, stamp.reproducible),
            (ID, Tier::External, false)
        );
        let env = stamp.env.unwrap();
        assert_eq!(
            env.components.get("codec").map(String::as_str),
            Some("libmp3lame")
        );
        assert_eq!(env.components.get("lame").map(String::as_str), Some("4.0"));
        let a = args_of(&t);
        for pair in [
            ["-ar", "44100"],
            ["-ch_layout", "stereo"],
            ["-sample_fmt", "fltp"],
            ["-b:a", "192k"],
            ["-id3v2_version", "0"],
            ["-protocol_whitelist", "pipe"],
            ["-protocol_whitelist", "file"],
        ] {
            assert!(has(&a, pair), "{pair:?} {a:?}");
        }
        assert!(
            a.last()
                .is_some_and(|l| l.starts_with("file:") && l.ends_with(".mp3"))
        );
        let left: Vec<_> = std::fs::read_dir(&t.work).unwrap().collect();
        assert!(left.is_empty(), "{left:?}");
        // 能力の表：mp3 の書き手として External が出る（タグなどは保存しない）
        let caps = set.format_caps("mp3").unwrap();
        let w = caps.default_writer().unwrap();
        assert_eq!(
            (w.codec, w.tier, w.reproducible),
            (ID, Tier::External, false)
        );
        assert!(!w.meta.tags && !w.meta.loop_points);
    }

    /// 設定：既定は CBR 192（そのレートで選べなければ 192 以下で最大）、VBR は LAME の -V、選べないビットレートは `Invalid`。
    #[test]
    fn settings_follow_the_mpeg_version() {
        let t = copying();
        let set = set_with(&t.codecs());
        let mono = |rate| audio_value(rate, vec![vec![0.1; 10]]);
        for (rate, opts, want) in [
            (44_100, json!({}), ["-b:a", "192k"]),
            (22_050, json!({}), ["-b:a", "160k"]),
            (8_000, json!({}), ["-b:a", "64k"]),
            (48_000, json!({ "bitrate": 320 }), ["-b:a", "320k"]),
            (
                16_000,
                json!({ "mode": "cbr", "bitrate": 8 }),
                ["-b:a", "8k"],
            ),
            (44_100, json!({ "mode": "vbr" }), ["-q:a", "2"]),
            (
                44_100,
                json!({ "mode": "vbr", "quality": 0, "bitrate": 8 }),
                ["-q:a", "0"],
            ),
            (
                32_000,
                json!({ "mode": "vbr", "quality": 9 }),
                ["-q:a", "9"],
            ),
        ] {
            set.encode_bytes(&mono(rate), "mp3", &opts, &NoMonitor)
                .unwrap();
            assert!(has(&args_of(&t), want), "{rate} {opts} {want:?}");
            assert!(has(&args_of(&t), ["-ch_layout", "mono"]));
        }
        for (rate, bad) in [
            (44_100, json!({ "bitrate": 8 })),
            (22_050, json!({ "bitrate": 192 })),
            (8_000, json!({ "bitrate": 80 })),
            (44_100, json!({ "bitrate": 321 })),
            (44_100, json!({ "mode": "abr" })),
            (44_100, json!({ "mode": "vbr", "quality": 10 })),
            (44_100, json!({ "id3": true })),
        ] {
            assert!(
                matches!(
                    set.encode_bytes(&mono(rate), "mp3", &bad, &NoMonitor),
                    Err(EngineError::Invalid(_))
                ),
                "{rate} {bad}"
            );
        }
    }

    /// 書けない形（レート・チャンネル・空）は `can_encode` が偽で、組みは「書けません」。
    #[test]
    fn unsupported_shapes_are_refused() {
        let t = copying();
        let codecs = t.codecs();
        let enc = codecs.encoders().pop().unwrap();
        let shape = |rate, channels, frames| Shape::Audio {
            rate,
            channels,
            frames: Dim::Exact(frames),
        };
        assert!(enc.can_encode("mp3", &shape(44_100, 2, 1)));
        assert!(enc.can_encode("mp3", &shape(8_000, 1, 10)));
        for (rate, ch, frames) in [
            (96_000, 1, 10),
            (44_100, 3, 10),
            (44_100, 2, 0),
            (44_000, 2, 10),
        ] {
            assert!(
                !enc.can_encode("mp3", &shape(rate, ch, frames)),
                "{rate} {ch} {frames}"
            );
        }
        assert!(!enc.can_encode("wav", &shape(44_100, 2, 10)));
        assert!(enc.writes_format("mp3") && !enc.writes_format("ogg"));
        let set = set_with(&codecs);
        let v = audio_value(96_000, vec![vec![0.0; 10]]);
        assert!(matches!(
            set.encode_bytes(&v, "mp3", &json!({}), &NoMonitor),
            Err(EngineError::Unsupported(_))
        ));
        // 直接呼んでも断る
        let mut out = Vec::new();
        assert!(matches!(
            enc.encode(&v, "mp3", &json!({}), &mut out, &NoMonitor),
            Err(EngineError::Unsupported(_))
        ));
        assert!(out.is_empty());
    }

    /// ffmpeg の失敗・出力がない・大きすぎる出力は書き出しの失敗。中止が効く。一時ファイルは残らない。
    #[test]
    fn failures_and_cancel() {
        let v = audio_value(44_100, vec![vec![0.1; 100], vec![0.2; 100]]);
        for (body, what) in [
            (
                "cat > /dev/null; echo '[libmp3lame @ 0x1] [error] boom' >&2; exit 1",
                "boom",
            ),
            ("cat > /dev/null; exit 0", "書きませんでした"),
            (
                "cat > /dev/null; head -c 200000 /dev/zero > \"$out\"",
                "大きさ",
            ),
            (
                "cat > \"$out\"; echo '[mp3 @ 0x1] [error] late' >&2",
                "late",
            ),
        ] {
            let t = fake(body);
            let set = set_with(&t.codecs());
            match set.encode_bytes(&v, "mp3", &json!({}), &NoMonitor) {
                Err(EngineError::Encode { codec, message }) => {
                    assert_eq!(codec, ID);
                    assert!(message.contains(what), "{message}");
                }
                other => panic!("{body}: {other:?}"),
            }
            let left: Vec<_> = std::fs::read_dir(&t.work).unwrap().collect();
            assert!(left.is_empty(), "{left:?}");
        }
        // 中止：入力を読まずに眠る相手に、進捗のあとで中止を頼む
        let t = fake("sleep 30");
        let codecs = t.codecs();
        let enc = codecs.encoders().pop().unwrap();
        struct CancelSoon(AtomicBool);
        impl Monitor for CancelSoon {
            fn canceled(&self) -> bool {
                self.0.load(Ordering::SeqCst)
            }
            fn progress(&self, _: u64, _: u64) {
                self.0.store(true, Ordering::SeqCst);
            }
        }
        let big = audio_value(44_100, vec![vec![0.1; 44_100 * 60]; 2]);
        let started = Instant::now();
        let mut out = Vec::new();
        let r = enc.encode(
            &big,
            "mp3",
            &json!({}),
            &mut out,
            &CancelSoon(AtomicBool::new(false)),
        );
        assert!(matches!(r, Err(EngineError::Canceled)), "{r:?}");
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "{:?}",
            started.elapsed()
        );
        assert!(out.is_empty());
    }

    #[test]
    fn helpers() {
        assert_eq!(bitrates(44_100).len(), 14);
        assert_eq!(bitrates(24_000).last(), Some(&160));
        assert_eq!(bitrates(11_025).last(), Some(&64));
        assert!(bitrates(96_000).is_empty());
        assert_eq!(sanitize(f32::NAN), 0.0);
        assert_eq!(sanitize(-0.0).to_bits(), (-0.0f32).to_bits());
        assert_eq!(sanitize(f32::MIN), -1.0);
        assert_eq!(sanitize(0.75), 0.75);
        // 1 秒・320 kbps は 40,000 バイト。その 1.1 倍と 64 KiB
        assert_eq!(max_output_bytes(44_100, 44_100), 44_000 + 65_536);
        assert_eq!(max_output_bytes(0, 44_100), 65_536);
        assert_eq!(max_output_bytes(1, 0), 0);
        assert!(max_output_bytes(u64::MAX, 8_000) == u64::MAX);
    }

    // ---- 実物の ffmpeg（あるときだけ） ----

    fn real() -> Option<(FfmpegCodecs, tempfile::TempDir)> {
        let (ffmpeg, ffprobe) = real_tools()?;
        let dir = tempfile::tempdir().unwrap();
        let codecs = FfmpegCodecs::detect_at(&ffmpeg, &ffprobe, &dir.path().join("work")).unwrap();
        Some((codecs, dir))
    }

    fn noise(ch: usize, frames: usize, seed: u32) -> Vec<Vec<f32>> {
        let mut x = seed.wrapping_mul(2_654_435_761).max(1);
        (0..ch)
            .map(|_| {
                (0..frames)
                    .map(|i| {
                        x ^= x << 13;
                        x ^= x >> 17;
                        x ^= x << 5;
                        let n = (x % 2_000) as f32 / 4_000.0 - 0.25;
                        n + 0.4 * (i as f32 * 0.031).sin()
                    })
                    .collect()
            })
            .collect()
    }

    fn read_back(bytes: &[u8]) -> (u32, usize, usize, Vec<Arc<Vec<f32>>>) {
        let (v, stamp) = CodecSet::pinned()
            .decode(
                &mut Cursor::new(bytes.to_vec()),
                "mp3",
                &DecodeOptions::default(),
                &Limits::default(),
                &NoMonitor,
            )
            .unwrap();
        assert_eq!(stamp.codec, "mp3-nanomp3-dec/1");
        match v {
            Value::Audio(a) => (a.rate(), a.channels(), a.frames(), a.planes().to_vec()),
            _ => panic!("音声ではありません"),
        }
    }

    /// 実物の ffmpeg：すべてのレート × 1・2 チャンネル × CBR・VBR で、kura の MP3 のデコーダ（nanomp3）で読み直すと長さが
    /// ちょうど元と同じ（LAME のタグの gapless）。同じ入力は同じバイト列。ID3 なし。
    #[test]
    fn real_ffmpeg_mp3_round_trips_with_exact_length() {
        let Some((codecs, _dir)) = real() else {
            return;
        };
        let set = set_with(&codecs);
        let rates = codecs.detected().lame_rates.clone();
        assert_eq!(rates.len(), 9, "{rates:?}");
        for &rate in &rates {
            for ch in [1usize, 2] {
                for opts in [json!({}), json!({ "mode": "vbr" })] {
                    let frames = 10_000;
                    let v = audio_value(rate, noise(ch, frames, rate + ch as u32));
                    let (bytes, stamp) = set.encode_bytes(&v, "mp3", &opts, &NoMonitor).unwrap();
                    assert_eq!(stamp.codec, ID);
                    assert_ne!(&bytes[..3], b"ID3", "ID3 を書かない");
                    let (r, c, f, _) = read_back(&bytes);
                    assert_eq!((r, c, f), (rate, ch, frames), "{rate} {ch} {opts}");
                }
            }
        }
        let v = audio_value(44_100, noise(2, 30_000, 7));
        let (a, _) = set.encode_bytes(&v, "mp3", &json!({}), &NoMonitor).unwrap();
        let (b, _) = set.encode_bytes(&v, "mp3", &json!({}), &NoMonitor).unwrap();
        assert_eq!(a, b);
    }

    /// 実物の ffmpeg：読み直した値は元に近い（gapless で位置がそろう）。NaN・∞・f32::MAX を含んでも LAME が止まらない。
    /// 中止はすぐ効く。
    #[test]
    fn real_ffmpeg_mp3_quality_bad_samples_and_cancel() {
        let Some((codecs, _dir)) = real() else {
            return;
        };
        let set = set_with(&codecs);
        let frames = 44_100;
        let sine: Vec<f32> = (0..frames)
            .map(|i| 0.5 * (std::f32::consts::TAU * 1_000.0 * i as f32 / 44_100.0).sin())
            .collect();
        let v = audio_value(44_100, vec![sine.clone()]);
        let (bytes, _) = set.encode_bytes(&v, "mp3", &json!({}), &NoMonitor).unwrap();
        let (_, _, f, planes) = read_back(&bytes);
        assert_eq!(f, frames);
        let (mut sig, mut err) = (0f64, 0f64);
        for (a, b) in sine.iter().zip(planes[0].iter()) {
            sig += f64::from(*a) * f64::from(*a);
            err += f64::from(a - b) * f64::from(a - b);
        }
        let snr = 10.0 * (sig / err.max(1e-30)).log10();
        assert!(snr > 20.0, "SNR {snr:.1} dB");
        let mut bad = noise(2, 5_000, 3);
        bad[0][10] = f32::NAN;
        bad[0][20] = f32::INFINITY;
        bad[1][30] = f32::NEG_INFINITY;
        bad[1][40] = f32::MAX;
        bad[0][50] = 1e30;
        let (bytes, _) = set
            .encode_bytes(&audio_value(48_000, bad), "mp3", &json!({}), &NoMonitor)
            .unwrap();
        assert_eq!(read_back(&bytes).2, 5_000);
        // 中止
        struct CancelAt(AtomicBool, std::sync::Mutex<Option<Instant>>);
        impl Monitor for CancelAt {
            fn canceled(&self) -> bool {
                self.0.load(Ordering::SeqCst)
            }
            fn progress(&self, done: u64, _: u64) {
                if done > 441_000
                    && !self.0.swap(true, Ordering::SeqCst)
                    && let Ok(mut t) = self.1.lock()
                {
                    *t = Some(Instant::now());
                }
            }
        }
        let long = audio_value(44_100, noise(2, 44_100 * 120, 5));
        let enc = codecs.encoders().pop().unwrap();
        let mon = CancelAt(AtomicBool::new(false), std::sync::Mutex::new(None));
        let mut out = Vec::new();
        let r = enc.encode(&long, "mp3", &json!({}), &mut out, &mon);
        assert!(matches!(r, Err(EngineError::Canceled)), "{r:?}");
        let at = mon.1.lock().unwrap().unwrap();
        assert!(
            at.elapsed() < Duration::from_millis(500),
            "{:?}",
            at.elapsed()
        );
    }
}
