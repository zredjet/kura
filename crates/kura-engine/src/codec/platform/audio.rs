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
//! - 流して読む（M13 段 2）：入力のバイト列は今どおりメモリに持ち（backend の口はメモリから）、塊ごとの `read` をそのまま
//!   `DecodeOut` に渡す（値は持たない）。backend の口（kura-mac）は変えない。

use std::sync::Arc;

use crate::codec::common::{self, Media, read_all};
use crate::codec::platform::BackendError;
use crate::codec::stream::{self, DecodeInfo, DecodeOut};
use crate::codec::{CodecEnv, Decoder, ReadSeek, Stage, Tier, try_vec};
use crate::codec::{mp4_read, ogg_read};
use crate::doc::DecodeOptions;
use crate::error::EngineError;
use crate::limits::Limits;
use crate::monitor::Monitor;
use crate::shape::{Dim, Shape};
use crate::value::{AudioMeta, AudioSource, Value};

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
        stream::collect(mon, |out| self.decode_into(src, format, limits, out, mon))
    }

    fn streams(&self, _format: &str) -> bool {
        true
    }

    /// 入力のバイト列（長さ `len`）と塊（65,536 フレーム）の作業列 × 2 と、backend の作業領域（8 MiB を見込む）。
    fn stream_work_bytes(&self, _format: &str, shape: &Shape, len: u64) -> u64 {
        let Shape::Audio { channels, .. } = shape else {
            return 0;
        };
        len.saturating_add(CHUNK_FRAMES as u64 * u64::from(*channels) * 4 * 2)
            .saturating_add(8 << 20)
    }

    /// 形を読むのにもファイル全体を読む。
    fn shape_work_bytes(&self, _format: &str, len: u64) -> u64 {
        len
    }

    fn decode_stream(
        &self,
        src: &mut (dyn ReadSeek + Send),
        format: &str,
        _opts: &DecodeOptions,
        limits: &Limits,
        _shape: &Shape,
        out: &mut DecodeOut<'_>,
        mon: &dyn Monitor,
    ) -> Result<(), EngineError> {
        self.decode_into(src, format, limits, out, mon)
    }
}

impl<B: AudioBackend> PlatformAudioDecoder<B> {
    /// 塊ごとに読んで `out` に渡す（`decode`・`decode_stream` が通る）。
    fn decode_into(
        &self,
        src: &mut dyn ReadSeek,
        format: &str,
        limits: &Limits,
        out: &mut DecodeOut<'_>,
        mon: &dyn Monitor,
    ) -> Result<(), EngineError> {
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
        out.begin(DecodeInfo {
            rate: info.rate,
            channels: usize::from(info.channels),
            frames: Dim::Exact(info.frames),
            expect: info.frames,
        })?;
        // 塊ごとに作業列へ読んでから組み手に足す（持つ量は値と塊の分だけ）
        let mut planes: Vec<Vec<f32>> = Vec::new();
        planes
            .try_reserve_exact(usize::from(info.channels))
            .map_err(|_| EngineError::Limit("チャンネルの表を確保できません".into()))?;
        for _ in 0..info.channels {
            planes.push(try_vec(CHUNK_FRAMES.min(frames), 0.0f32)?);
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
            let mut views: Vec<&mut [f32]> = planes.iter_mut().map(|p| &mut p[..n]).collect();
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
            let read: Vec<&[f32]> = planes.iter().map(|p| &p[..got]).collect();
            out.push_frames(&read)?;
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
        drop(planes);
        out.end(meta)
    }
}

/// 入れ物のタグ・ループ点を固定の段のパーサーで読む（m4a は自作の MP4 の読み口、ogg・opus は自作の Ogg の読み口。ほかの形式は
/// タグなし。M16 段 10 で symphonia の読み口から替えた。docs/editing.md §9.2）。読めなければタグなし（中止だけは返す）。
fn container_meta(
    id: &'static str,
    bytes: &[u8],
    format: &str,
    frames: u64,
    limits: &Limits,
    mon: &dyn Monitor,
) -> Result<AudioMeta, EngineError> {
    let mut media = Media::Bytes(bytes);
    // タグにするコメントの数の上限：1 つのタグは値に入るとき 32 バイト以上を予算から引く（`common::meta_from_tags`）ので、
    // それを超える数は値にならない（ループ点だけはそのあとでも読めたが、そこまで並べたファイルは読まない）
    let max = usize::try_from(limits.max_meta_bytes / 32).unwrap_or(usize::MAX);
    let (raw, vorbis) = match format {
        "m4a" => (mp4_read::tags(&mut media, mon, id), false),
        "ogg" | "opus" => (ogg_read::tags(&mut media, mon, id, max), true),
        _ => return Ok(AudioMeta::default()),
    };
    match raw {
        Ok(raw) => Ok(common::meta_from_tags(&raw, frames, vorbis, limits)),
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
    use crate::value::LoopRange;

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
            let got: Vec<Vec<f32>> = a.to_planes().unwrap().iter().map(|p| p.to_vec()).collect();
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

    /// Vorbis コメント 1 つ（長さつき）の並び。
    fn comment_list(items: &[&str]) -> Vec<u8> {
        let mut b = 4u32.to_le_bytes().to_vec();
        b.extend_from_slice(b"test");
        b.extend_from_slice(&(items.len() as u32).to_le_bytes());
        for c in items {
            b.extend_from_slice(&(c.len() as u32).to_le_bytes());
            b.extend_from_slice(c.as_bytes());
        }
        b
    }

    fn meta_of(bytes: &[u8], format: &str) -> AudioMeta {
        container_meta(
            "test-dec/1",
            bytes,
            format,
            1_000,
            &Limits::default(),
            &NoMonitor,
        )
        .unwrap()
    }

    /// M16 段 10：入れ物のタグは自作の読み口で読む。Ogg は最初の BOS の組の Vorbis・Opus・FLAC の最初の論理ストリームの
    /// コメント（Vorbis・Opus は 2 つ目のパケット、FLAC は頭のパケットの VORBIS_COMMENT。前の symphonia は最後に読んだ
    /// コメント（多重化なら後の論理ストリームの、FLAC なら PICTURE のあとは空）を使っていた）。頭だけを読むので、あとの
    /// ページが壊れていてもタグを読む。m4a は中身を問わず音声のトラックがあればタグを読む。
    #[test]
    fn container_tags_are_read_by_the_own_readers() {
        use crate::codec::ogg::tests::page;
        // Ogg の FLAC：STREAMINFO・PICTURE・VORBIS_COMMENT（最後の箱）
        let mut head = b"\x7fFLAC\x01\x00\x00\x02fLaC".to_vec();
        head.extend_from_slice(&[0, 0, 0, 34]);
        head.extend_from_slice(&[0; 34]);
        let picture = [6u8, 0, 0, 4, 1, 2, 3, 4];
        let mut comment = vec![0x84];
        let list = comment_list(&["TITLE=ふぁいる", "LOOPSTART=10", "LOOPLENGTH=20"]);
        comment.extend_from_slice(&(list.len() as u32).to_be_bytes()[1..]);
        comment.extend_from_slice(&list);
        let mut flac = page(5, 0, 0x02, 0, &[&head]);
        flac.extend_from_slice(&page(5, 1, 0, 0, &[&picture, &comment]));
        flac.extend_from_slice(&page(5, 2, 0x04, 1_000, &[&[0xff, 0xf8]]));
        let m = meta_of(&flac, "ogg");
        assert_eq!(m.tags.get("title").map(String::as_str), Some("ふぁいる"));
        assert_eq!(m.loop_range, Some(LoopRange { start: 10, end: 30 }));
        // 多重化：Opus と Vorbis が同じ BOS の組にあれば、最初の Opus のコメント
        let opus_head = b"OpusHead\x01\x02\x00\x00\x80\xbb\x00\x00\x00\x00\x00".to_vec();
        let mut opus_tags = b"OpusTags".to_vec();
        opus_tags.extend_from_slice(&comment_list(&["TITLE=first"]));
        let mut vorbis_id = b"\x01vorbis".to_vec();
        vorbis_id.extend_from_slice(&[0; 23]);
        let mut vorbis_comment = b"\x03vorbis".to_vec();
        vorbis_comment.extend_from_slice(&comment_list(&["TITLE=second"]));
        vorbis_comment.push(1);
        let mut muxed = page(1, 0, 0x02, 0, &[&opus_head]);
        muxed.extend_from_slice(&page(2, 0, 0x02, 0, &[&vorbis_id]));
        muxed.extend_from_slice(&page(1, 1, 0, 0, &[&opus_tags]));
        muxed.extend_from_slice(&page(2, 1, 0, 0, &[&vorbis_comment]));
        assert_eq!(
            meta_of(&muxed, "opus")
                .tags
                .get("title")
                .map(String::as_str),
            Some("first")
        );
        // 頭のあとが壊れていても（ページの頭でないものが続く）タグは読む
        let mut tail = muxed.clone();
        tail.extend_from_slice(b"garbage that is not a page");
        assert_eq!(
            meta_of(&tail, "opus").tags.get("title").map(String::as_str),
            Some("first")
        );
        // コメントが途中で切れていればタグなし（誤りにしない）
        let mut cut = b"OpusTags".to_vec();
        cut.extend_from_slice(&comment_list(&["TITLE=first"])[..12]);
        let mut broken = page(1, 0, 0x02, 0, &[&opus_head]);
        broken.extend_from_slice(&page(1, 1, 0, 0, &[&cut]));
        assert!(meta_of(&broken, "opus").tags.is_empty());
        // タグにするコメントは、メタデータの予算（1 つ 32 バイト以上）に入る数まで
        let mut many = b"OpusTags".to_vec();
        many.extend_from_slice(&comment_list(&[
            "A=1", "B=2", "C=3", "D=4", "E=5", "F=6", "G=7", "H=8",
        ]));
        let mut capped = page(1, 0, 0x02, 0, &[&opus_head]);
        capped.extend_from_slice(&page(1, 1, 0, 0, &[&many]));
        let tight = Limits {
            max_meta_bytes: 3 * 32,
            ..Limits::default()
        };
        let m = container_meta("test-dec/1", &capped, "opus", 1_000, &tight, &NoMonitor).unwrap();
        assert!(m.tags.len() <= 3, "{:?}", m.tags);
        assert_eq!(meta_of(&capped, "opus").tags.len(), 8);
        // Vorbis でも Opus でも FLAC でもない Ogg・空はタグなし
        assert!(
            meta_of(&page(3, 0, 0x02, 0, &[b"\x80theora"]), "ogg")
                .tags
                .is_empty()
        );
        assert!(meta_of(&[], "ogg").tags.is_empty());
        // m4a：ALAC のトラックのタグ（AAC など、ほかの中身でも同じ道）
        let alac = std::fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/fixtures/alac-tags.m4a"
        ))
        .unwrap();
        assert_eq!(meta_of(&alac, "m4a").tags.len(), 7);
        // タグを読む形式でなければ読まない
        assert!(meta_of(&alac, "wav").tags.is_empty());
    }
}
