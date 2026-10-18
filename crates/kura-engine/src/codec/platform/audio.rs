//! Platform の音声の受け口：backend（AudioToolbox の ExtAudioFile。メモリから）が出した PCM をチャンクごとに受け取り、
//! 値にする。
//!
//! 約束（adapter の側。識別子の版に入る）：
//! - backend は planar の f32 を kura の並び（WAV の並び。[`super::default_channel_mask`] を `ClientChannelLayout` に
//!   渡す）で書く。長さは backend が priming・remainder を切ったあとの長さ（`FileLengthFrames`）を形（`Exact`）にし、
//!   実際に読めた数が違えば壊れたファイル（`Decode`。1 フレーム余分に読んで、長すぎるものも見つける）。
//! - 上限は確保の前（形 → `Limits::check` → チャンネルごとに `try_reserve_exact`）。中止はチャンク（65,536 フレーム）の
//!   合間で見る。
//! - タグ・ループ点は OS に任せず、固定の段のパーサーで読む（m4a は MP4 の iTunes のタグ、Ogg・Opus は Vorbis コメント
//!   （OpusTags）と LOOPSTART・LOOPLENGTH。位置は出力のレート）。読めなければタグなし（PCM は backend が読めたので失敗に
//!   しない）。`AudioMeta.source` は backend の申告、なければ非可逆の扱い（16 ビットの整数・出力のレート）。

use std::sync::Arc;

use crate::codec::platform::BackendError;
use crate::codec::symph::{self, Container, read_all};
use crate::codec::{CodecEnv, Decoder, ReadSeek, Stage, Tier, contain, try_vec};
use crate::doc::DecodeOptions;
use crate::error::EngineError;
use crate::limits::Limits;
use crate::monitor::Monitor;
use crate::shape::{Dim, Shape};
use crate::value::{Audio, AudioMeta, AudioSource, Value};

/// 1 回の読み取りで求めるフレームの数。
pub const CHUNK_FRAMES: usize = 65_536;

/// backend が切ったエンコーダの遅延・詰め物（記録と調べ物のため。adapter は使わない）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Priming {
    /// 切っていない（ADTS など、入れ物に情報がない。固定の段と同じく切らない）。
    None,
    /// 切った（m4a の iTunSMPB・Ogg Opus の pre-skip など）。`start` は先頭、`end` は末尾で切ったフレームの数。
    Trimmed { start: u64, end: u64 },
}

/// 開いた音声の形。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioInfo {
    /// 出力のサンプルレート（[`super::rate_from_f64`] で整数にしたもの）。
    pub rate: u32,
    /// チャンネルの数（1 以上）。
    pub channels: u16,
    /// 1 チャンネルあたりのフレームの数（priming・remainder を切ったあと）。
    pub frames: u64,
    pub priming: Priming,
    /// 元の形式（可逆の PCM の変種なら申告する）。`None` なら `{16 ビット・整数・出力のレート}`（非可逆）。
    pub source: Option<AudioSource>,
}

/// 音声の backend（kura-mac の AudioToolbox・テストの偽物）。
pub trait AudioBackend: Send + Sync {
    /// 開いたもの。`Drop` で 1 度だけ閉じる。
    type Handle: AudioHandle;
    /// コーデックの識別子（例 `audiotoolbox-dec/1`）。
    fn id(&self) -> &'static str;
    /// この形式を扱うか（`m4a`・`aac`・`ogg`・`opus`、固定の段が断った `wav`・`aiff` の変種など）。
    fn can_decode(&self, format: &str) -> bool;
    /// 環境の記録（OS・AudioToolbox の版）。
    fn env(&self) -> Option<CodecEnv> {
        None
    }
    /// 入力のバイト列を開いて形を読む。中身のコーデックが扱わないもの（AudioToolbox の Ogg Vorbis など）は `Unsupported`。
    fn open(&self, bytes: Arc<Vec<u8>>, format: &str) -> Result<Self::Handle, BackendError>;
}

/// 開いた音声。
pub trait AudioHandle {
    /// 開いたときの形。
    fn info(&self) -> AudioInfo;
    /// 続きを `planes`（チャンネルの数ぶん、どれも同じ長さ）の先頭から書き、書いたフレームの数を返す（0 は終わり）。
    /// 求めた数（`planes[0].len()`）より多くは書かない。
    fn read(&mut self, planes: &mut [&mut [f32]]) -> Result<usize, BackendError>;
}

/// 音声の backend を `Decoder` にする adapter（`Tier::Platform`・再現を約束しない）。
pub struct PlatformAudioDecoder<B> {
    backend: B,
}

impl<B: AudioBackend> PlatformAudioDecoder<B> {
    /// backend を包む。
    pub fn new(backend: B) -> Self {
        Self { backend }
    }

    /// 包んだ backend。
    pub fn backend(&self) -> &B {
        &self.backend
    }

    fn open(
        &self,
        src: &mut dyn ReadSeek,
        format: &str,
        limits: &Limits,
    ) -> Result<Opened<B::Handle>, EngineError> {
        let id = self.backend.id();
        let bytes = Arc::new(read_all(src, limits)?);
        let h = self
            .backend
            .open(bytes.clone(), format)
            .map_err(|e| e.into_engine(id, Stage::Decode))?;
        let info = h.info();
        if info.rate == 0 || info.channels == 0 {
            return Err(EngineError::Decode {
                codec: id.to_string(),
                message: "サンプルレート・チャンネルの数が 0 です".into(),
            });
        }
        Ok(Opened {
            handle: h,
            info,
            bytes,
        })
    }
}

/// 開いた音声と、その形・入力のバイト列（タグを読むため）。
struct Opened<H> {
    handle: H,
    info: AudioInfo,
    bytes: Arc<Vec<u8>>,
}

fn shape_of(info: &AudioInfo) -> Shape {
    Shape::Audio {
        rate: info.rate,
        channels: info.channels,
        frames: Dim::Exact(info.frames),
    }
}

impl<B: AudioBackend> Decoder for PlatformAudioDecoder<B> {
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

    fn can_decode(&self, format: &str) -> bool {
        self.backend.can_decode(format)
    }

    fn shape(
        &self,
        src: &mut dyn ReadSeek,
        format: &str,
        _opts: &DecodeOptions,
        limits: &Limits,
    ) -> Result<Shape, EngineError> {
        Ok(shape_of(&self.open(src, format, limits)?.info))
    }

    fn decode(
        &self,
        src: &mut dyn ReadSeek,
        format: &str,
        _opts: &DecodeOptions,
        limits: &Limits,
        mon: &dyn Monitor,
    ) -> Result<Value, EngineError> {
        let id = self.backend.id();
        let Opened {
            handle: mut h,
            info,
            bytes,
        } = self.open(src, format, limits)?;
        // 確保の前に上限で判断する
        limits.check(&shape_of(&info)).map_err(EngineError::Limit)?;
        let frames = usize::try_from(info.frames)
            .map_err(|_| EngineError::Limit("音声が長すぎます".into()))?;
        let mut planes: Vec<Vec<f32>> = Vec::new();
        planes
            .try_reserve_exact(usize::from(info.channels))
            .map_err(|_| EngineError::Limit("チャンネルの表を確保できません".into()))?;
        for _ in 0..info.channels {
            planes.push(try_vec(frames, 0.0f32)?);
        }
        let short = |got: usize| EngineError::Decode {
            codec: id.to_string(),
            message: format!("ヘッダと中身の大きさが合いません（{frames} フレームのはずが {got}）"),
        };
        let mut off = 0usize;
        while off < frames {
            if mon.canceled() {
                return Err(EngineError::Canceled);
            }
            let n = CHUNK_FRAMES.min(frames - off);
            let mut views: Vec<&mut [f32]> =
                planes.iter_mut().map(|p| &mut p[off..off + n]).collect();
            let got = h
                .read(&mut views)
                .map_err(|e| e.into_engine(id, Stage::Decode))?;
            if got > n {
                return Err(EngineError::Internal(format!(
                    "{id}: 求めた数より多く書きました（{got} > {n}）"
                )));
            }
            if got == 0 {
                return Err(short(off));
            }
            off += got;
            mon.progress(off as u64, frames as u64);
        }
        // 形より長い中身も壊れたファイル（1 フレームだけ余分に読んで確かめる）
        let mut extra: Vec<Vec<f32>> = vec![vec![0.0]; usize::from(info.channels)];
        let mut views: Vec<&mut [f32]> = extra.iter_mut().map(|p| p.as_mut_slice()).collect();
        let more = h
            .read(&mut views)
            .map_err(|e| e.into_engine(id, Stage::Decode))?;
        if more > 0 {
            return Err(EngineError::Decode {
                codec: id.to_string(),
                message: format!("ヘッダと中身の大きさが合いません（{frames} フレームより長い）"),
            });
        }
        drop(h);
        let mut meta = container_meta(id, &bytes, format, info.frames, limits, mon)?;
        meta.source = Some(info.source.unwrap_or(AudioSource {
            bits: 16,
            float: false,
            rate: info.rate,
        }));
        Ok(Value::Audio(Arc::new(Audio::new(
            info.rate,
            planes.into_iter().map(Arc::new).collect(),
            meta,
        )?)))
    }
}

/// 入れ物のタグ・ループ点を固定の段のパーサーで読む（m4a・ogg・opus。ほかの形式はタグなし）。読めなければタグなし
/// （中止だけは返す）。
fn container_meta(
    id: &str,
    bytes: &[u8],
    format: &str,
    frames: u64,
    limits: &Limits,
    mon: &dyn Monitor,
) -> Result<AudioMeta, EngineError> {
    let (container, vorbis) = match format {
        "m4a" => (Container::Mp4, false),
        "ogg" | "opus" => (Container::Ogg, true),
        _ => return Ok(AudioMeta::default()),
    };
    let raw = contain(id, Stage::Decode, || {
        symph::open(id, bytes, container, mon, limits).map(|mut o| o.raw_tags())
    });
    match raw {
        Ok(raw) => Ok(symph::meta_from_tags(&raw, frames, vorbis, limits)),
        Err(EngineError::Canceled) => Err(EngineError::Canceled),
        Err(_) => Ok(AudioMeta::default()),
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;
    use std::sync::atomic::Ordering;

    use super::*;
    use crate::codec::CodecSet;
    use crate::monitor::{CancelAfter, NoMonitor};
    use crate::testing::platform::{AudioFault, FakeAudio, FakeAudioBackend};

    fn planes(channels: usize, frames: usize) -> Vec<Vec<f32>> {
        (0..channels)
            .map(|c| {
                (0..frames)
                    .map(|i| ((i * 7 + c * 13) % 1000) as f32 / 1000.0 - 0.5)
                    .collect()
            })
            .collect()
    }

    fn decode_bytes(
        b: FakeAudioBackend,
        bytes: Vec<u8>,
        format: &str,
        limits: &Limits,
        mon: &dyn Monitor,
    ) -> Result<Value, EngineError> {
        PlatformAudioDecoder::new(b).decode(
            &mut Cursor::new(bytes),
            format,
            &DecodeOptions::default(),
            limits,
            mon,
        )
    }

    fn decode(
        b: FakeAudioBackend,
        limits: &Limits,
        mon: &dyn Monitor,
    ) -> Result<Value, EngineError> {
        decode_bytes(b, vec![0u8; 4], "m4a", limits, mon)
    }

    /// PCM はそのまま値になる（backend が 1 回に書く数が求めた数より少なくても続けて読む）。記録は Platform・環境つき。
    #[test]
    fn pcm_becomes_the_value() {
        for (channels, frames, chunk) in
            [(1, 10, 0), (2, 200_000, 0), (6, 70_000, 1_000), (3, 1, 0)]
        {
            let data = planes(channels, frames);
            let mut fake = FakeAudio::new(48_000, data.clone());
            fake.chunk = chunk;
            let b = FakeAudioBackend::new(fake);
            let counters = b.counters();
            let mut set = CodecSet::pinned();
            set.add_decoder(Arc::new(PlatformAudioDecoder::new(b)))
                .unwrap();
            let (v, stamp) = set
                .decode(
                    &mut Cursor::new(vec![0u8; 4]),
                    "m4a",
                    &DecodeOptions::default(),
                    &Limits::default(),
                    &NoMonitor,
                )
                .unwrap();
            let Value::Audio(a) = &v else { panic!("{v:?}") };
            assert_eq!(a.rate(), 48_000);
            let got: Vec<Vec<f32>> = a.planes().iter().map(|p| p.to_vec()).collect();
            assert_eq!(got, data);
            assert_eq!(
                a.meta.source,
                Some(AudioSource {
                    bits: 16,
                    float: false,
                    rate: 48_000
                })
            );
            assert_eq!(
                (stamp.codec.as_str(), stamp.tier, stamp.reproducible),
                ("fake-audiotoolbox-dec/1", Tier::Platform, false)
            );
            assert_eq!(stamp.env, Some(FakeAudioBackend::default_env()));
            assert_eq!(
                counters.opens.load(Ordering::Relaxed),
                counters.closes.load(Ordering::Relaxed)
            );
        }
        // 申告した元の形式はそのまま
        let mut fake = FakeAudio::new(8_000, planes(1, 4));
        fake.source = Some(AudioSource {
            bits: 24,
            float: false,
            rate: 8_000,
        });
        let Value::Audio(a) =
            decode(FakeAudioBackend::new(fake), &Limits::default(), &NoMonitor).unwrap()
        else {
            panic!()
        };
        assert_eq!(a.meta.source.map(|s| s.bits), Some(24));
    }

    /// 形と中身の照合：短い・長い中身は壊れたファイル。backend が求めた数より多く書いたら内部の誤り。
    #[test]
    fn length_must_match_the_header() {
        let data = planes(2, 1_000);
        let mut short = FakeAudio::new(44_100, data.clone());
        short.declared_frames = Some(1_001);
        assert!(matches!(
            decode(FakeAudioBackend::new(short), &Limits::default(), &NoMonitor),
            Err(EngineError::Decode { .. })
        ));
        let mut long = FakeAudio::new(44_100, data.clone());
        long.declared_frames = Some(999);
        assert!(matches!(
            decode(FakeAudioBackend::new(long), &Limits::default(), &NoMonitor),
            Err(EngineError::Decode { .. })
        ));
        let mut b = FakeAudioBackend::new(FakeAudio::new(44_100, data.clone()));
        b.faults.push(AudioFault::Overrun);
        assert!(matches!(
            decode(b, &Limits::default(), &NoMonitor),
            Err(EngineError::Internal(_))
        ));
        let mut b = FakeAudioBackend::new(FakeAudio::new(44_100, planes(2, 200_000)));
        b.faults.push(AudioFault::Read(
            1,
            BackendError::Failed("壊れたパケット".into()),
        ));
        assert!(matches!(
            decode(b, &Limits::default(), &NoMonitor),
            Err(EngineError::Decode { .. })
        ));
        let mut b = FakeAudioBackend::new(FakeAudio::new(44_100, data));
        b.faults.push(AudioFault::Open(BackendError::Unsupported(
            "Ogg Vorbis".into(),
        )));
        assert!(matches!(
            decode(b, &Limits::default(), &NoMonitor),
            Err(EngineError::Unsupported(_))
        ));
        let mut zero = FakeAudio::new(44_100, planes(1, 4));
        zero.rate = 0;
        assert!(matches!(
            decode(FakeAudioBackend::new(zero), &Limits::default(), &NoMonitor),
            Err(EngineError::Decode { .. })
        ));
    }

    /// 上限を超える形は、確保も読み取りもせずに `Limit`（申告の長さが巨大でも確保しない）。
    #[test]
    fn limits_are_checked_before_allocating() {
        let mut huge = FakeAudio::new(48_000, planes(2, 10));
        huge.declared_frames = Some(u64::MAX / 2);
        let b = FakeAudioBackend::new(huge);
        let counters = b.counters();
        assert!(matches!(
            decode(b, &Limits::default(), &NoMonitor),
            Err(EngineError::Limit(_))
        ));
        assert_eq!(counters.reads.load(Ordering::Relaxed), 0);
        let many = FakeAudio::new(48_000, planes(40, 4));
        assert!(matches!(
            decode(FakeAudioBackend::new(many), &Limits::default(), &NoMonitor),
            Err(EngineError::Limit(_))
        ));
    }

    /// 中止：チャンクの合間の 1〜4 回目の確認のどこでも止まる。backend の中の中止も Canceled。
    #[test]
    fn cancels_between_chunks() {
        for after in 1..=4 {
            let b = FakeAudioBackend::new(FakeAudio::new(48_000, planes(2, 300_000)));
            assert!(
                matches!(
                    decode(b, &Limits::default(), &CancelAfter::new(after)),
                    Err(EngineError::Canceled)
                ),
                "{after}"
            );
        }
        let mut b = FakeAudioBackend::new(FakeAudio::new(48_000, planes(2, 300_000)));
        b.faults.push(AudioFault::Read(2, BackendError::Canceled));
        assert!(matches!(
            decode(b, &Limits::default(), &NoMonitor),
            Err(EngineError::Canceled)
        ));
    }

    /// タグ・ループ点は固定の段のパーサーで入れ物から読む（Ogg Opus の OpusTags）。読めない入れ物はタグなしで失敗にしない。
    #[test]
    fn tags_come_from_the_container() {
        let opus = std::fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/fixtures/ogg-opus.ogg"
        ))
        .unwrap();
        let b = FakeAudioBackend::new(FakeAudio::new(48_000, planes(2, 48_000)));
        let Value::Audio(a) =
            decode_bytes(b, opus, "opus", &Limits::default(), &NoMonitor).unwrap()
        else {
            panic!()
        };
        assert!(
            a.meta.tags.keys().any(|k| k.starts_with("vorbis:")),
            "{:?}",
            a.meta.tags
        );
        let b = FakeAudioBackend::new(FakeAudio::new(48_000, planes(2, 10)));
        let Value::Audio(a) =
            decode_bytes(b, vec![1, 2, 3], "m4a", &Limits::default(), &NoMonitor).unwrap()
        else {
            panic!()
        };
        assert!(a.meta.tags.is_empty() && a.meta.loop_range.is_none());
    }
}
