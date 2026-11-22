//! MP4 の中の ALAC の読み取り（自作。`m4a-alac-symphonia-dec/1`。M16 段 3・ADR-219）。
//!
//! 入れ物は `codec::mp4_read`、ビット列は `codec::alac_read`。前の symphonia（`IsoMp4Reader` と `AlacDecoder`）と、正しい
//! ファイルでは同じ形・同じ値（標本・タグ・元の形式）を出すので、識別子はそのまま（ADR-216 (2)）。違いは壊れたファイルと
//! symphonia が読み違える形だけ（各モジュールの説明・docs/editing.md §9.6）。
//!
//! - 形：パケットの長さ（stts の delta）の和を `Dim::Exact` にする。デコードした長さがそれと違えば壊れたファイル（形より
//!   長く出てきたものも `Decode`）。数える量も `max_audio_frames`・`max_value_bytes` で確かめる。
//! - 標本：32 ビットに左詰めした整数を `pcm::int_to_f32(v, 32)`（WAV・AIFF・FLAC と同じ値）。
//! - 元の形式：cookie のビットの深さを入れ物の幅にしたもの（`pcm::container_bits`）。タグは MP4 の 7 つのキー
//!   （`symph::meta_from_tags`）。

use std::io::BufReader;
use std::path::Path;

use crate::codec::alac_read::{AlacDecoder, DEC_ID, MAX_FRAME_LENGTH};
use crate::codec::held::{self, Held};
use crate::codec::mp4_read::{self, Mp4Track};
use crate::codec::stream::{self, DecodeInfo, DecodeOut};
use crate::codec::symph::{self, Media};
use crate::codec::{Decoder, ReadSeek, Tier, pcm};
use crate::doc::DecodeOptions;
use crate::error::EngineError;
use crate::limits::Limits;
use crate::monitor::{Monitor, NoMonitor};
use crate::shape::{Dim, Shape};
use crate::value::{AudioSource, Value};

/// MP4 の中の ALAC の読み取り。
pub struct AlacCodec;

/// パケットの大きさの上限の、非圧縮の標本のほかの分（要素の頭・DSE・FIL・後ろのごみ）。正しいエンコーダの出力は数十バイト。
/// [`Decoder::stream_work_bytes`] の読み口の 4 MiB の内。
const PACKET_EXTRA: u64 = 1 << 20;

fn dec_err(message: impl Into<String>) -> EngineError {
    EngineError::Decode {
        codec: DEC_ID.to_string(),
        message: message.into(),
    }
}

/// 読んだ形。
struct Probe {
    rate: u32,
    channels: u16,
    frames: u64,
}

/// 開いて、レート・チャンネルを上限で確かめる（symphonia の読み口と同じ順）。
fn open<'a>(
    media: &'a mut Media<'_>,
    limits: &Limits,
    mon: &'a dyn Monitor,
) -> Result<Mp4Track<'a>, EngineError> {
    let r = mp4_read::open(media, mon, DEC_ID, mp4_read::Want::Alac)?;
    if r.rate == 0 {
        return Err(dec_err("サンプルレートがありません"));
    }
    if r.channels as u64 > limits.max_channels {
        return Err(EngineError::Limit("チャンネルが多すぎます".into()));
    }
    if r.rate > limits.max_sample_rate {
        return Err(EngineError::Limit("サンプルレートが大きすぎます".into()));
    }
    Ok(r)
}

/// パケットの大きさの上限（どの正しい ALAC のパケットも、非圧縮の標本と頭に収まる。超えるものは作業領域の見積もりの外
/// なので `Limit`。symphonia は読んだ。docs/editing.md §9.6）。
fn packet_max(r: &Mp4Track<'_>) -> u64 {
    let fl = crate::codec::alac_read::Cookie::parse(&r.cookie)
        .map_or(u64::from(MAX_FRAME_LENGTH), |c| {
            u64::from(c.frame_length.min(MAX_FRAME_LENGTH))
        });
    fl * r.channels as u64 * 4 + PACKET_EXTRA
}

/// 長さ（パケットの長さの和。読まずに数える。数える量を上限で確かめる）。
fn count(r: &mut Mp4Track<'_>, limits: &Limits, mon: &dyn Monitor) -> Result<u64, EngineError> {
    let max = packet_max(r);
    let (rate, channels) = (r.rate, u16::try_from(r.channels).unwrap_or(u16::MAX));
    let shape = |n: u64| Shape::Audio {
        rate,
        channels,
        frames: Dim::Exact(n),
    };
    let mut total = 0u64;
    while let Some(s) = r.next_sample()? {
        if mon.canceled() {
            return Err(EngineError::Canceled);
        }
        if u64::from(s.size) > max {
            return Err(EngineError::Limit(format!(
                "ALAC のパケットが大きすぎます（{} バイト）",
                s.size
            )));
        }
        total = total.saturating_add(u64::from(s.dur));
        limits
            .check(&shape(total))
            .map_err(|m| EngineError::Limit(format!("デコードする量が大きすぎます（{m}）")))?;
    }
    Ok(total)
}

fn probe(media: &mut Media<'_>, limits: &Limits, mon: &dyn Monitor) -> Result<Probe, EngineError> {
    let mut r = open(media, limits, mon)?;
    let frames = count(&mut r, limits, mon)?;
    Ok(Probe {
        rate: r.rate,
        channels: u16::try_from(r.channels).map_err(|_| dec_err("チャンネルが多すぎます"))?,
        frames,
    })
}

fn shape_of(p: &Probe) -> Shape {
    Shape::Audio {
        rate: p.rate,
        channels: p.channels,
        frames: Dim::Exact(p.frames),
    }
}

fn decode_media(
    media: &mut Media<'_>,
    limits: &Limits,
    out: &mut DecodeOut<'_>,
    mon: &dyn Monitor,
) -> Result<(), EngineError> {
    let p = probe(media, limits, mon)?;
    limits.check(&shape_of(&p)).map_err(EngineError::Limit)?;
    let mut r = open(media, limits, mon)?;
    out.begin(DecodeInfo {
        rate: p.rate,
        channels: usize::from(p.channels),
        frames: Dim::Exact(p.frames),
        expect: p.frames,
    })?;
    let mut dec = AlacDecoder::new(&r.cookie)?;
    let max = packet_max(&r);
    let ch = r.channels;
    let mut planes: Vec<Vec<f32>> = (0..ch).map(|_| Vec::new()).collect();
    let mut total = 0u64;
    while let Some(s) = r.next_sample()? {
        if mon.canceled() {
            return Err(EngineError::Canceled);
        }
        if u64::from(s.size) > max {
            return Err(EngineError::Limit(format!(
                "ALAC のパケットが大きすぎます（{} バイト）",
                s.size
            )));
        }
        let n = dec.decode(r.read_sample(&s)?)?;
        total = total.saturating_add(n as u64);
        if total > p.frames {
            return Err(dec_err("長さがヘッダより長い"));
        }
        for (plane, src) in planes.iter_mut().zip(&dec.planes) {
            plane.clear();
            plane
                .try_reserve(n)
                .map_err(|_| EngineError::Limit("音声が長すぎます".into()))?;
            plane.extend(src[..n].iter().map(|v| pcm::int_to_f32(*v, 32)));
        }
        let views: Vec<&[f32]> = planes.iter().map(Vec::as_slice).collect();
        out.push_frames(&views)?;
    }
    if out.frames() != p.frames {
        return Err(dec_err("長さがヘッダと合いません"));
    }
    let mut meta = symph::meta_from_tags(&r.tags, p.frames, false, limits);
    meta.source = Some(AudioSource {
        bits: pcm::container_bits(u16::from(symph::alac_bits(&r.cookie).unwrap_or(16))),
        float: false,
        rate: p.rate,
    });
    out.end(meta)
}

impl Decoder for AlacCodec {
    fn id(&self) -> &'static str {
        DEC_ID
    }

    fn tier(&self) -> Tier {
        Tier::Pinned
    }

    fn reproducible(&self) -> bool {
        true
    }

    fn can_decode(&self, format: &str) -> bool {
        format == "m4a"
    }

    fn shape(
        &self,
        src: &mut dyn ReadSeek,
        _format: &str,
        _opts: &DecodeOptions,
        limits: &Limits,
    ) -> Result<Shape, EngineError> {
        let data = symph::read_all(src, limits)?;
        let p = probe(&mut Media::Bytes(&data), limits, &NoMonitor)?;
        Ok(shape_of(&p))
    }

    /// ファイルを開いて箱の頭と表を読む（全体を読み込まない）。
    fn shape_file(
        &self,
        path: &Path,
        format: &str,
        opts: &DecodeOptions,
        limits: &Limits,
    ) -> Result<Shape, EngineError> {
        self.shape_file_monitored(path, format, opts, limits, &NoMonitor)
    }

    fn shape_file_monitored(
        &self,
        path: &Path,
        _format: &str,
        _opts: &DecodeOptions,
        limits: &Limits,
        mon: &dyn Monitor,
    ) -> Result<Shape, EngineError> {
        let mut f = BufReader::new(std::fs::File::open(path)?);
        let p = probe(&mut Media::Reader(&mut f), limits, mon)?;
        Ok(shape_of(&p))
    }

    fn decode(
        &self,
        src: &mut dyn ReadSeek,
        _format: &str,
        _opts: &DecodeOptions,
        limits: &Limits,
        mon: &dyn Monitor,
    ) -> Result<Value, EngineError> {
        let data = symph::read_all(src, limits)?;
        stream::collect(mon, |out| {
            decode_media(&mut Media::Bytes(&data), limits, out, mon)
        })
    }

    fn streams(&self, _format: &str) -> bool {
        true
    }

    /// 前の symphonia の見積もりのまま（読み口の 4 MiB と、65,536 フレームの i32 の面・作業列・パケット（12 バイト ×
    /// チャンネル）と tail（4 バイト））。今の自作の読み手は、i32 の面・f32 の写し・パケット（非圧縮の大きさまで）と tail
    /// （2 チャンネル × 2 バイト）をフレームごとに持つので、この内。読み手が持つ表とタグは [`Decoder::held_bytes`]。
    fn stream_work_bytes(&self, _format: &str, shape: &Shape, _len: u64) -> u64 {
        let Shape::Audio { channels, .. } = shape else {
            return 0;
        };
        let ch = u64::from(*channels);
        let frames = u64::from(MAX_FRAME_LENGTH);
        (4 << 20) + frames * ch * 12 + frames * 4
    }

    /// 読み手が開くときに読んで持ち続けるもの（選んだトラックのサンプルの表と、7 つのキーのタグ。`codec::held::mp4`）。
    fn held_bytes(
        &self,
        src: &mut dyn ReadSeek,
        _format: &str,
        _limits: &Limits,
    ) -> Result<Held, EngineError> {
        held::mp4(src, held::Mp4Reader::Kura)
    }

    fn held_bytes_file(
        &self,
        path: &Path,
        format: &str,
        limits: &Limits,
    ) -> Result<Held, EngineError> {
        let mut f = BufReader::new(std::fs::File::open(path)?);
        self.held_bytes(&mut f, format, limits)
    }

    fn decode_stream(
        &self,
        src: &mut (dyn ReadSeek + Send),
        _format: &str,
        _opts: &DecodeOptions,
        limits: &Limits,
        _shape: &Shape,
        out: &mut DecodeOut<'_>,
        mon: &dyn Monitor,
    ) -> Result<(), EngineError> {
        decode_media(&mut Media::Reader(src), limits, out, mon)
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use rand_chacha::ChaCha8Rng;
    use rand_chacha::rand_core::Rng;
    use symphonia_core::codecs::audio::well_known::CODEC_ID_ALAC;

    use super::*;
    use crate::codec::alac_read::tests::{BitWriter, cookie as make_cookie};
    use crate::codec::mp4_read::m4a_gen::{Extra, M4a, Tag};
    use crate::codec::symph::Container;

    /// 前の読み手（symphonia の `IsoMp4Reader` と `AlacDecoder`。M16 段 3 の前の `lossy` の道そのまま）。panic は None。
    fn old(data: &[u8]) -> Option<Result<Value, EngineError>> {
        let data = data.to_vec();
        std::panic::catch_unwind(move || old_inner(&data)).ok()
    }

    fn old_inner(data: &[u8]) -> Result<Value, EngineError> {
        let limits = Limits::default();
        let mon = NoMonitor;
        stream::collect(&mon, |out| {
            let mut o = symph::open(DEC_ID, data, Container::Mp4, &mon, &limits)?;
            if o.params.codec != CODEC_ID_ALAC {
                return Err(EngineError::Unsupported("ALAC でない".into()));
            }
            let raw = o.raw_tags();
            let counted = o.count_frames(DEC_ID, &mon, &limits)?;
            let (rate, ch) = (o.rate, o.channels);
            drop(o);
            limits
                .check(&Shape::Audio {
                    rate,
                    channels: ch as u16,
                    frames: Dim::Exact(counted),
                })
                .map_err(EngineError::Limit)?;
            let mut o = symph::open(DEC_ID, data, Container::Mp4, &mon, &limits)?;
            out.begin(DecodeInfo {
                rate,
                channels: ch,
                frames: Dim::Exact(counted),
                expect: counted,
            })?;
            let mut dec =
                symphonia_codec_alac::AlacDecoder::try_new(&o.params, &symph::decoder_options())
                    .map_err(|e| symph::sym_err(DEC_ID, e, &mon))?;
            o.decode_all(DEC_ID, &mut dec, counted, out, &mon)?;
            if out.frames() != counted {
                return Err(dec_err("長さがヘッダと合いません"));
            }
            let mut meta = symph::meta_from_tags(&raw, counted, false, &limits);
            meta.source = Some(AudioSource {
                bits: pcm::container_bits(u16::from(
                    o.params
                        .extra_data
                        .as_deref()
                        .and_then(symph::alac_bits)
                        .unwrap_or(16),
                )),
                float: false,
                rate,
            });
            out.end(meta)
        })
    }

    fn ours(data: &[u8]) -> Result<Value, EngineError> {
        AlacCodec.decode(
            &mut Cursor::new(data.to_vec()),
            "m4a",
            &DecodeOptions::default(),
            &Limits::default(),
            &NoMonitor,
        )
    }

    /// 流して読む道（読み口から）。
    fn ours_stream(data: &[u8]) -> Result<Value, EngineError> {
        let limits = Limits::default();
        let mut c = Cursor::new(data.to_vec());
        stream::collect(&NoMonitor, |out| {
            decode_media(&mut Media::Reader(&mut c), &limits, out, &NoMonitor)
        })
    }

    fn fixture(name: &str) -> Vec<u8> {
        std::fs::read(format!("{}/fixtures/{name}", env!("CARGO_MANIFEST_DIR"))).unwrap()
    }

    const FIXTURES: [&str; 9] = [
        "alac-16.m4a",
        "alac-24.m4a",
        "alac-mono.m4a",
        "alac-20.m4a",
        "alac-32.m4a",
        "alac-6ch.m4a",
        "alac-8ch.m4a",
        "alac-frag.m4a",
        "alac-tags.m4a",
    ];

    /// afconvert・ffmpeg の ALAC（16・20・24・32 ビット、1・2・6・8 チャンネル、断片、タグ）を、前の symphonia の読み手と
    /// 同じ値（標本・タグ・元の形式のダイジェスト）で読む。流して読む道も同じ。
    #[test]
    fn fixtures_match_symphonia() {
        for name in FIXTURES {
            let data = fixture(name);
            let a = ours(&data).unwrap_or_else(|e| panic!("{name}: {e}"));
            let b = old(&data)
                .unwrap()
                .unwrap_or_else(|e| panic!("{name}: {e}"));
            assert_eq!(a.digest().unwrap(), b.digest().unwrap(), "{name}");
            let s = ours_stream(&data).unwrap();
            assert_eq!(a.digest().unwrap(), s.digest().unwrap(), "{name}：流す道");
            if name == "alac-tags.m4a" {
                let Value::Audio(au) = &a else { panic!() };
                assert_eq!(
                    au.meta.tags.get("title").map(String::as_str),
                    Some("Sweep 掃引")
                );
                assert_eq!(au.meta.tags.len(), 7, "{:?}", au.meta.tags);
            }
        }
    }

    /// 非圧縮のパケット（Apple の要素の並び。最後のパケットは短い）。
    fn packets(rng: &mut ChaCha8Rng, ch: u8, bd: u8, fl: u32, count: usize) -> Vec<(Vec<u8>, u32)> {
        let layout: &[u64] = match ch {
            1 => &[0],
            2 => &[1],
            3 => &[0, 1],
            4 => &[0, 1, 0],
            5 => &[0, 1, 1],
            6 => &[0, 1, 1, 3],
            7 => &[0, 1, 1, 3, 0],
            _ => &[0, 1, 1, 1, 3],
        };
        let mut out = Vec::new();
        for i in 0..count {
            let n = if i + 1 == count {
                1 + rng.next_u32() % fl
            } else {
                fl
            };
            let mut w = BitWriter::default();
            for &tag in layout {
                w.put(tag, 3);
                w.put(0, 4);
                w.put(0, 12);
                w.put(u64::from(n != fl), 1);
                w.put(0, 2);
                w.put(1, 1);
                if n != fl {
                    w.put(u64::from(n), 32);
                }
                for _ in 0..n * if tag == 1 { 2 } else { 1 } {
                    w.put(u64::from(rng.next_u32()), usize::from(bd));
                }
            }
            w.put(7, 3);
            out.push((w.b, n));
        }
        out
    }

    fn text(rng: &mut ChaCha8Rng) -> Vec<u8> {
        let words = ["kura", "Sweep 掃引", "Ünïcode", "", "a\0b", "x"];
        words[(rng.next_u32() % words.len() as u32) as usize]
            .as_bytes()
            .to_vec()
    }

    fn utf16(s: &str, bom: u32) -> Vec<u8> {
        let mut v = match bom {
            0 => vec![0xfe, 0xff],
            1 => vec![0xff, 0xfe],
            _ => Vec::new(),
        };
        for u in s.encode_utf16() {
            v.extend_from_slice(&if bom == 1 {
                u.to_le_bytes()
            } else {
                u.to_be_bytes()
            });
        }
        v
    }

    fn random_tags(rng: &mut ChaCha8Rng) -> Vec<Tag> {
        let keys: [[u8; 4]; 10] = [
            [0xa9, b'n', b'a', b'm'],
            [0xa9, b'A', b'R', b'T'],
            [0xa9, b'a', b'l', b'b'],
            [0xa9, b'c', b'm', b't'],
            [0xa9, b'd', b'a', b'y'],
            [0xa9, b'g', b'e', b'n'],
            *b"cprt",
            [0xa9, b'l', b'y', b'r'],
            *b"gnre",
            *b"trkn",
        ];
        let mut tags = Vec::new();
        for _ in 0..rng.next_u32() % 8 {
            let key = keys[(rng.next_u32() % keys.len() as u32) as usize];
            let mut data = Vec::new();
            for _ in 0..1 + rng.next_u32() % 2 {
                let t = text(rng);
                data.push(match rng.next_u32() % 6 {
                    0 => (
                        2,
                        utf16(std::str::from_utf8(&t).unwrap(), rng.next_u32() % 3),
                    ),
                    1 => (5, utf16(std::str::from_utf8(&t).unwrap(), 2)),
                    2 => (21, vec![0, 7]),
                    3 => (1, vec![b'y'; 4_096 + rng.next_u32() as usize % 3]),
                    4 => (4, t),
                    _ => (1, t),
                });
            }
            tags.push(Tag { key, data });
        }
        tags
    }

    /// 乱数の形の m4a。
    fn random_m4a(rng: &mut ChaCha8Rng) -> M4a {
        let ch = (1 + rng.next_u32() % 8) as u8;
        let bd = [16u8, 20, 24, 32][(rng.next_u32() % 4) as usize];
        let fl = [16u32, 33, 100][(rng.next_u32() % 3) as usize];
        let count = 1 + (rng.next_u32() % 12) as usize;
        let mut c = make_cookie(fl, bd, 40, 10, 14, ch);
        if rng.next_u32().is_multiple_of(4) {
            // 48 バイト（chan の配置）
            let tags = [
                0x0064_0001u32,
                0x0065_0002,
                0x0071_0003,
                0x0074_0004,
                0x0078_0005,
                0x007c_0006,
                0x008e_0007,
                0x007f_0008,
            ];
            c.extend_from_slice(&24u32.to_be_bytes());
            c.extend_from_slice(b"chan");
            c.extend_from_slice(&0u32.to_be_bytes());
            c.extend_from_slice(&tags[usize::from(ch) - 1].to_be_bytes());
            c.extend_from_slice(&[0; 8]);
        }
        let mut m = M4a::new(c, packets(rng, ch, bd, fl, count));
        m.moov_first = !rng.next_u32().is_multiple_of(3);
        m.co64 = rng.next_u32().is_multiple_of(3);
        m.chunking = (0..1 + rng.next_u32() % 3)
            .map(|_| 1 + rng.next_u32() % 5)
            .collect();
        m.gap = if rng.next_u32().is_multiple_of(3) {
            rng.next_u32() % 40
        } else {
            0
        };
        m.entry_version = if rng.next_u32().is_multiple_of(5) {
            1
        } else {
            0
        };
        m.tags = random_tags(rng);
        if rng.next_u32().is_multiple_of(3) {
            m.top_tags = Some(random_tags(rng));
        }
        if rng.next_u32().is_multiple_of(4) {
            m.extra_before = vec![Extra::Text];
        }
        if rng.next_u32().is_multiple_of(5) {
            m.fragment = Some(1 + rng.next_u32() % 4);
        }
        m
    }

    /// 乱数で作った形の m4a（moov の前後・co64・chunk の分け方と詰め物・見本の版 1・48 バイトの cookie・タグの型と BOM・
    /// 最上段の meta・前のチャプターのトラック・断片）を、前の symphonia の読み手と同じ値で読む。stz2 は前の読み手が
    /// 読めなかったもの（自作は読む）、前の AAC のトラックは両方とも `Unsupported`。
    #[test]
    fn generated_files_match_symphonia() {
        let mut rng = crate::determinism::rng(43);
        for round in 0..300 {
            let mut m = random_m4a(&mut rng);
            let data = m.build();
            let a = ours(&data).unwrap_or_else(|e| panic!("回 {round}: {e}\n{m:?}"));
            let b = old(&data)
                .unwrap()
                .unwrap_or_else(|e| panic!("回 {round}（前）: {e}"));
            assert_eq!(
                a.digest().unwrap(),
                b.digest().unwrap(),
                "回 {round}\n{m:?}"
            );
            assert_eq!(
                a.digest().unwrap(),
                ours_stream(&data).unwrap().digest().unwrap(),
                "回 {round}：流す道"
            );
            if m.fragment.is_none() {
                m.stz2 = true;
                if m.packets.iter().all(|(p, _)| p.len() < 65_536) {
                    let z = m.build();
                    assert_eq!(
                        ours(&z).unwrap().digest().unwrap(),
                        a.digest().unwrap(),
                        "回 {round}：stz2"
                    );
                    assert!(
                        old(&z).unwrap().is_err(),
                        "回 {round}：前の読み手は stz2 を読めなかった"
                    );
                }
                m.stz2 = false;
                m.extra_before.insert(0, Extra::Aac);
                let aac = m.build();
                assert!(
                    matches!(ours(&aac), Err(EngineError::Unsupported(_))),
                    "回 {round}：AAC が前"
                );
                assert!(matches!(old(&aac), Some(Err(EngineError::Unsupported(_)))));
            }
        }
    }

    /// 自作の読み手が前の読み手より厳しく断る形の誤り（ADR-219 (3)。モジュール `mp4_read` の説明）。
    const STRICT: [&str; 9] = [
        "stts と stsz",
        "大きさ 0 のサンプル",
        "chunk の位置が前",
        "trun の位置が前",
        "moof のあとに mdat",
        "パケットが大きすぎます",
        "traf が 2 つ",
        "trex の並び",
        "ファイルの外",
    ];

    /// 壊したファイル（乱数で数バイトを変える・切る。フィクスチャと作ったもの）で、前の読み手が読めたものは同じ値で読むか、
    /// 厳しく断る形（[`STRICT`]）の誤りにする。panic しない。
    #[test]
    fn broken_files_follow_symphonia() {
        let mut rng = crate::determinism::rng(47);
        let mut seeds: Vec<Vec<u8>> = FIXTURES.iter().map(|n| fixture(n)).collect();
        for _ in 0..12 {
            seeds.push(random_m4a(&mut rng).build());
        }
        let (mut same, mut strict, mut both_err, mut better, mut panicked) = (0, 0, 0, 0, 0);
        for round in 0..1_500 {
            let mut b = seeds[(rng.next_u32() as usize) % seeds.len()].clone();
            for _ in 0..1 + rng.next_u32() % 4 {
                // 箱の頭と表の多い前のほうを多めに
                let span = if rng.next_u32().is_multiple_of(2) {
                    b.len().min(4_096)
                } else {
                    b.len()
                };
                let i = (rng.next_u32() as usize) % span;
                b[i] = rng.next_u32() as u8;
            }
            if rng.next_u32().is_multiple_of(6) {
                let cut = (rng.next_u32() as usize) % b.len();
                b.truncate(cut);
            }
            let a = ours(&b);
            let Some(o) = old(&b) else {
                panicked += 1;
                continue;
            };
            match (&a, &o) {
                (Ok(x), Ok(y)) => {
                    assert_eq!(x.digest().unwrap(), y.digest().unwrap(), "回 {round}");
                    same += 1;
                }
                (Err(e), Ok(_)) => {
                    let msg = e.to_string();
                    assert!(
                        STRICT.iter().any(|s| msg.contains(s)),
                        "回 {round}：前は読めた：{msg}"
                    );
                    strict += 1;
                }
                // 自作だけが読む：ほかのトラック・使わないタグの項目・ftyp・mvhd の版の誤り（モジュール `mp4_read` の説明）
                (Ok(_), Err(_)) => better += 1,
                (Err(_), Err(_)) => both_err += 1,
            }
        }
        eprintln!(
            "同じ {same}・厳しい {strict}・両方とも誤り {both_err}・自作だけ読む {better}・前が panic {panicked}"
        );
        assert!(same > 100 && both_err > 100, "{same} {both_err}");
    }

    /// 見直しで見つけた、symphonia と食い違っていた形（M16 段 3）：stsz と stz2 の両方、UTF-16 の長さの境目、前の trak の
    /// esds の記述子の並び・v2 の PCM の見本、断片のサンプル 0 の trun・mdat のない空の moof・終わりの切れた頭。どれも前の
    /// 読み手と同じ値か、同じく `Unsupported`。
    #[test]
    fn review_shapes_match_symphonia() {
        let mut rng = crate::determinism::rng(53);
        for round in 0..40 {
            let mut m = random_m4a(&mut rng);
            m.fragment = if round % 2 == 0 {
                Some(1 + round % 3)
            } else {
                None
            };
            m.extra_stz2 = m.fragment.is_none();
            m.frag_extras = m.fragment.is_some();
            m.extra_before = vec![Extra::PcmV2];
            // UTF-16（BOM あり）の長さ 2 · 4,096 + 3：値は 4,096 バイトでちょうど残る
            let mut v = vec![0xfe, 0xff];
            for _ in 0..4_096 {
                v.extend_from_slice(&[0, b'a']);
            }
            v.push(0);
            m.tags.insert(
                0,
                Tag {
                    key: [0xa9, b'n', b'a', b'm'],
                    data: vec![(2, v)],
                },
            );
            let data = m.build();
            let a = ours(&data).unwrap_or_else(|e| panic!("回 {round}: {e}"));
            let b = old(&data)
                .unwrap()
                .unwrap_or_else(|e| panic!("回 {round}（前）: {e}"));
            assert_eq!(a.digest().unwrap(), b.digest().unwrap(), "回 {round}");
            let Value::Audio(au) = &a else { panic!() };
            assert_eq!(
                au.meta.tags.get("title").map(String::len),
                Some(4_096),
                "回 {round}"
            );
            // DecoderConfigDescriptor の前にほかの記述子がある AAC が前：両方とも AAC のトラックを選ぶ（ALAC は `Unsupported`）
            m.extra_before = vec![Extra::AacDesc];
            let aac = m.build();
            assert!(
                matches!(ours(&aac), Err(EngineError::Unsupported(_))),
                "回 {round}"
            );
            assert!(
                matches!(old(&aac), Some(Err(EngineError::Unsupported(_)))),
                "回 {round}"
            );
        }
    }

    /// 断片の trun が小さな箱で多くのサンプルを宣言しても（既定の大きさ 0 か 1・サンプル 2³² − 1）、数えるのに回らずに断る。
    #[test]
    // 時間だけを見る（値は時刻によらない）
    #[allow(clippy::disallowed_methods)]
    fn huge_trun_counts_are_refused_quickly() {
        let mut rng = crate::determinism::rng(59);
        let mut m = random_m4a(&mut rng);
        m.fragment = Some(2);
        let base = m.build();
        let tfhd_id = (m.extra_before.len() as u32 + 1).to_be_bytes();
        for (flags, size) in [(0x02_0000u32, None), (0x02_0010, Some(1u32))] {
            let mut tfhd = tfhd_id.to_vec();
            if let Some(s) = size {
                tfhd.extend_from_slice(&s.to_be_bytes());
            }
            let tfhd = crate::codec::mp4_read::m4a_gen::full(b"tfhd", 0, flags, &tfhd);
            let trun =
                crate::codec::mp4_read::m4a_gen::full(b"trun", 0, 0, &u32::MAX.to_be_bytes());
            let mfhd = crate::codec::mp4_read::m4a_gen::full(b"mfhd", 0, 0, &7u32.to_be_bytes());
            let bx = crate::codec::mp4_read::m4a_gen::bx;
            let mut data = base.clone();
            for _ in 0..200 {
                data.extend_from_slice(&bx(
                    b"moof",
                    &[
                        mfhd.clone(),
                        bx(b"traf", &[tfhd.clone(), trun.clone()].concat()),
                    ]
                    .concat(),
                ));
            }
            data.extend_from_slice(&bx(b"mdat", &[0u8; 16]));
            let t = std::time::Instant::now();
            assert!(
                matches!(ours(&data), Err(EngineError::Decode { .. })),
                "{flags:x}"
            );
            let took = t.elapsed();
            assert!(took.as_secs() < 5, "{took:?}");
        }
    }

    /// 速さの目安（release で `--ignored`）：`alac-16.m4a`（afconvert の 1 秒のステレオ 16 ビット）のパケットを 300 回並べた
    /// 5 分の m4a を、前の symphonia の読み手と自作の読み手で読む（値のダイジェストは含めない）。M16 段 3 の実測（M4 Max）：
    /// symphonia 0.35 秒、自作 0.30 秒。
    #[test]
    #[ignore]
    // 計るだけの試験（値は時刻によらない）
    #[allow(clippy::disallowed_methods)]
    fn speed_against_symphonia() {
        let src = fixture("alac-16.m4a");
        let mut media = Media::Bytes(&src);
        let mut r = mp4_read::open(&mut media, &NoMonitor, DEC_ID, mp4_read::Want::Alac).unwrap();
        let cookie = r.cookie.clone();
        let mut one = Vec::new();
        while let Some(s) = r.next_sample().unwrap() {
            one.push((r.read_sample(&s).unwrap().to_vec(), s.dur));
        }
        // 最後の短いパケットは最後に 1 度だけ
        let last = one.pop().unwrap();
        let mut packets = Vec::new();
        for _ in 0..300 {
            packets.extend(one.iter().cloned());
        }
        packets.push(last);
        let mut m = M4a::new(cookie, packets);
        m.chunking = vec![20];
        let data = m.build();
        let mut best = [f64::MAX; 2];
        for _ in 0..3 {
            let t = std::time::Instant::now();
            let a = old_inner(&data).unwrap();
            best[0] = crate::math::min(best[0], t.elapsed().as_secs_f64());
            let t = std::time::Instant::now();
            let b = ours(&data).unwrap();
            best[1] = crate::math::min(best[1], t.elapsed().as_secs_f64());
            let Value::Audio(x) = a else { panic!() };
            let Value::Audio(y) = b else { panic!() };
            assert_eq!(x.frames(), y.frames());
        }
        println!(
            "{} バイト：symphonia {:.3} 秒、自作 {:.3} 秒",
            data.len(),
            best[0],
            best[1]
        );
    }
}
