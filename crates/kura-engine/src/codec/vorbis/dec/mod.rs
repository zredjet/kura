//! Ogg Vorbis の読み取り（自作。`ogg-vorbis-kura-dec/1`。M16 段 2・ADR-217）。
//!
//! libvorbis 1.3.7（aoTuV b6.03 に同梱。書き出しの照合用の C と同じ源）の読み取りを、書き出しと同じ規則（`super` の
//! 「移植の規則」12 か条）で移したもの。正しさの基準は、照合用の C（`crates/vorbis-tables/oracle/` の `oracle_dec`。libogg と
//! libvorbis で `decoder_example` と同じ流れに読む）と、出した PCM がビット単位で一致すること。
//!
//! # 流れ（C の `decoder_example` と同じ）
//!
//! ```text
//! codec::ogg_read::OggReader        Ogg のページ → パケット（libogg と同じパケット・グラニュール・旗。壊れたページは誤り）
//! info::Headers::headerin × 3       識別・コメント・setup（info.c の _vorbis_unpack_*）
//! synth::Dsp::new                   vorbis_synthesis_init（codebook の復号の表、floor・residue の look、MDCT）
//! 音声のパケットごとに：
//!   synth::Dsp::synthesis           vorbis_synthesis → mapping0_inverse（floor の inverse1 → residue → coupling → floor の
//!                                   inverse2 → mdct_backward）
//!   synth::Dsp::blockin             vorbis_synthesis_blockin（窓の重ね合わせ・グラニュールによる始まりと終わりの切り取り）
//!   synth::Dsp::pcmout / read       出せる標本を取り出す
//! ```
//!
//! # ファイル
//!
//! | ファイル | C |
//! |---|---|
//! | [`bitwise`] | ogg_vendor/src/bitwise.c の読み・aoTuV の codebook.c の `vorbis_oggpack_look`・`_adv` |
//! | [`codebook`] | codebook.c の unpack・decode、sharedbook.c の decode の init |
//! | [`info`] | info.c・floor0.c・floor1.c・res0.c・mapping0.c の unpack |
//! | [`floor`] | floor0.c・lsp.c・floor1.c の look・inverse |
//! | [`res0`] | res0.c の look・inverse |
//! | [`synth`] | block.c・synthesis.c・mapping0.c の inverse |
//! | `../mdct.rs` の `mdct_backward` | mdct.c |
//!
//! # kura の側で決めること（照合の対象外）
//!
//! - 壊れたファイル（ADR-217 (2)）：音声のパケットが読めない（音声の印でない・mode が正しくない）ものは誤り（C の
//!   `decoder_example` はそのパケットを飛ばす）。長さ 0 のパケットは C と同じく飛ばす（パケットの番号は進む）。
//!   パケットの中の終わり（floor・residue の途中で切れる）は C と同じく残りを 0 にする（仕様の「終わりの扱い」）。
//! - チャンネルの並び：Vorbis のチャンネル `i` を kura の `ORDER[n − 1][i]` 番目に置く（書き出しの逆。前の symphonia と同じ）。
//!   9 チャンネル以上は `Unsupported`（前の symphonia と同じく次の段へ）。
//! - NaN は `0x7fc00000` にそろえる（`symph::canonical_nan`。壊れた入力で出る NaN のビットを環境によらずそろえる）。
//! - タグは段 1 の FLAC と同じ規則（`flac_read::comment_tag`）。元の形式は 16 ビットの整数とみなす（前と同じ）。
//! - 形（長さ）は、値を作らずにパケットの頭だけを読んで `blockin` の位置の計算を回して数える（[`synth`] の冒頭）。
//!
//! ライセンス：libvorbis・libogg・aoTuV の C を移したもの（Copyright (c) 2002-2020 Xiph.org Foundation、Copyright (c)
//! 2003-2021 Aoyumi）。BSD-3-Clause で、条件と免責は `../LICENSE`（ADR-149・ADR-217）。

#![allow(clippy::too_many_arguments)]

pub(crate) mod bitwise;
pub(crate) mod codebook;
pub(crate) mod floor;
pub(crate) mod info;
pub(crate) mod res0;
pub(crate) mod synth;

use std::io::BufReader;
use std::path::Path;

use crate::codec::flac_read::comment_tag;
use crate::codec::held::{self, Held};
use crate::codec::ogg_read::OggReader;
use crate::codec::stream::{self, DecodeInfo, DecodeOut};
use crate::codec::symph::{self, Media, read_all};
use crate::codec::vorbis::ORDER;
use crate::codec::{Decoder, ReadSeek, Tier};
use crate::doc::DecodeOptions;
use crate::error::EngineError;
use crate::limits::Limits;
use crate::monitor::{Monitor, NoMonitor};
use crate::shape::{Dim, Shape};
use crate::value::AudioSource;

use info::{Headers, IdHeader};
use synth::Dsp;

/// 読み取りの約束の版。
pub(crate) const DEC_ID: &str = "ogg-vorbis-kura-dec/1";
/// 読み口と作業列のほかに、codebook の表として見込む量（これを超える分は `held_bytes` が setup から `codebook::book_bytes` の
/// 和で見積もる。前の symphonia と同じ量）。
const VORBIS_TABLES: u64 = 32 << 20;

/// codebook の表の予算（前の symphonia と同じ `min(max_value_bytes, 256 MiB)`。`codec::held::ogg` の上限とも同じ）。
pub(crate) fn table_budget(limits: &Limits) -> u64 {
    limits.max_value_bytes.min(256 << 20)
}

/// 壊れたヘッダ・パケットの誤り。
pub(crate) fn bad_header(why: &str) -> EngineError {
    EngineError::Decode {
        codec: DEC_ID.to_string(),
        message: format!("Vorbis：{why}"),
    }
}

/// 開いた Ogg Vorbis（ヘッダを読み、合成の初期化を終えたところ）。
pub(crate) struct VorbisReader<'a> {
    ogg: OggReader<'a>,
    pub id: IdHeader,
    pub tags: Vec<(String, String)>,
    dsp: Dsp,
    /// 次のパケットの番号（libogg の `packetno`。ヘッダが 0〜2）。
    packetno: i64,
    limits: Limits,
}

/// 開いて 3 つのヘッダを読む。チャンネル・レートを上限で確かめ、codebook の表の大きさ（`codebook::book_bytes` の和）を
/// [`table_budget`] で断る（確保の前。`Unsupported`）。`with_pcm` が偽なら数えるだけ（復号の表を作らない。`synth` の冒頭）。
pub(crate) fn open<'a>(
    media: &'a mut Media<'_>,
    limits: &Limits,
    mon: &'a dyn Monitor,
    with_pcm: bool,
) -> Result<VorbisReader<'a>, EngineError> {
    let mut ogg = OggReader::open(media, mon, DEC_ID)?;
    let mut h = Headers::new(table_budget(limits));
    let mut packetno = 0i64;
    while h.setup.is_none() {
        let Some(p) = ogg.next_packet()? else {
            return Err(bad_header("ヘッダが足りません"));
        };
        packetno += 1;
        h.headerin(&p.data, p.bos)?;
    }
    let (Some(id), Some(comments), Some(setup)) = (h.id, h.comments, h.setup) else {
        return Err(bad_header("ヘッダが足りません"));
    };
    if id.channels > 8 {
        return Err(EngineError::Unsupported(format!(
            "{DEC_ID}: 9 チャンネル以上の Vorbis は次の段で読みます"
        )));
    }
    if id.channels as u64 > limits.max_channels {
        return Err(EngineError::Limit("チャンネルが多すぎます".into()));
    }
    if id.rate > i64::from(limits.max_sample_rate) {
        return Err(EngineError::Limit("サンプルレートが大きすぎます".into()));
    }
    let tags = comments.iter().filter_map(|c| comment_tag(c)).collect();
    let dsp = Dsp::new(&id, &setup, with_pcm)?;
    Ok(VorbisReader {
        ogg,
        id,
        tags,
        dsp,
        packetno,
        limits: limits.clone(),
    })
}

impl VorbisReader<'_> {
    /// 音声のパケットを終わりまで回す。`with_pcm` なら、出せる標本ごとに `emit(dsp, n)` を呼ぶ（Vorbis のチャンネルの並び）。
    /// 出した標本の数を返す。`max` を超えたら `limit` を返す。切り取る前の長さの和（デコードする量）も上限
    /// （`max_audio_frames`・`max_value_bytes`）で確かめる（グラニュールで長さを 0 近くまで詰めると、形は小さいまま大量の
    /// パケットを読ませられるため。前の symphonia の読み取りの確かめと同じ数え方）。
    fn run(
        &mut self,
        with_pcm: bool,
        max: u64,
        limit: &dyn Fn(u64) -> EngineError,
        emit: &mut dyn FnMut(&Dsp, usize) -> Result<(), EngineError>,
    ) -> Result<u64, EngineError> {
        let mut total = 0u64;
        let mut decoded = 0u64;
        let shape = |n: u64| Shape::Audio {
            rate: self.id.rate as u32,
            channels: self.id.channels as u16,
            frames: Dim::Exact(n),
        };
        let limits = self.limits.clone();
        while let Some(p) = self.ogg.next_packet()? {
            let packetno = self.packetno;
            self.packetno += 1;
            // 長さ 0 のパケットは C と同じく飛ばす
            if p.data.is_empty() {
                continue;
            }
            let block = self
                .dsp
                .synthesis(&p.data, p.granulepos, packetno, p.eos, with_pcm)?;
            let nominal = self.dsp.blockin(&block)?;
            decoded = decoded.saturating_add(nominal.max(0) as u64);
            limits
                .check(&shape(decoded))
                .map_err(|m| EngineError::Limit(format!("デコードする量が大きすぎます（{m}）")))?;
            let n = self.dsp.pcmout();
            if n > 0 {
                total = total.saturating_add(n as u64);
                if total > max {
                    return Err(limit(total));
                }
                if with_pcm {
                    emit(&self.dsp, n as usize)?;
                }
                self.dsp.read(n)?;
            }
        }
        Ok(total)
    }

    /// 長さ（値を作らずに数える。数える量を上限で確かめる）。
    pub fn count_frames(&mut self, limits: &Limits) -> Result<u64, EngineError> {
        let (rate, channels) = (self.id.rate as u32, self.id.channels as u16);
        let max = limits.max_audio_frames;
        let limit = |n: u64| {
            EngineError::Limit(format!(
                "デコードする量が大きすぎます（{}）",
                limits
                    .check(&Shape::Audio {
                        rate,
                        channels,
                        frames: Dim::Exact(n),
                    })
                    .err()
                    .unwrap_or_default()
            ))
        };
        let n = self.run(false, max, &limit, &mut |_, _| Ok(()))?;
        limits
            .check(&Shape::Audio {
                rate,
                channels,
                frames: Dim::Exact(n),
            })
            .map_err(|m| EngineError::Limit(format!("デコードする量が大きすぎます（{m}）")))?;
        Ok(n)
    }

    /// すべての標本を `out` に渡す（kura のチャンネルの並び。長さは `frames` まで）。
    pub fn decode_all(&mut self, frames: u64, out: &mut DecodeOut<'_>) -> Result<u64, EngineError> {
        let ch = self.id.channels as usize;
        let order: &[usize] = ORDER.get(ch.wrapping_sub(1)).copied().unwrap_or(&[]);
        let mut planes: Vec<Vec<f32>> = (0..ch).map(|_| Vec::new()).collect();
        let limit = |_| bad_header("長さがヘッダより長い");
        self.run(true, frames, &limit, &mut |dsp, n| {
            for (i, &dst) in order.iter().enumerate() {
                let plane = &mut planes[dst];
                plane.clear();
                plane
                    .try_reserve(n)
                    .map_err(|_| EngineError::Limit("音声が長すぎます".into()))?;
                plane.extend(dsp.pcm_slice(i, n).iter().map(|s| symph::canonical_nan(*s)));
            }
            let views: Vec<&[f32]> = planes.iter().map(Vec::as_slice).collect();
            out.push_frames(&views)
        })
    }
}

/// Ogg Vorbis の読み取り（`ogg-vorbis-kura-dec/1`）。
pub struct VorbisDecoder;

/// 読んだ形。
struct Probe {
    rate: u32,
    channels: u16,
    frames: u64,
}

fn probe(media: &mut Media<'_>, limits: &Limits, mon: &dyn Monitor) -> Result<Probe, EngineError> {
    let mut r = open(media, limits, mon, false)?;
    let frames = r.count_frames(limits)?;
    Ok(Probe {
        rate: r.id.rate as u32,
        channels: r.id.channels as u16,
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
    let mut r = open(media, limits, mon, true)?;
    out.begin(DecodeInfo {
        rate: p.rate,
        channels: usize::from(p.channels),
        frames: Dim::Exact(p.frames),
        expect: p.frames,
    })?;
    r.decode_all(p.frames, out)?;
    if out.frames() != p.frames {
        return Err(bad_header("長さが数えた長さと合いません"));
    }
    let mut meta = symph::meta_from_tags(&r.tags, p.frames, true, limits);
    meta.source = Some(AudioSource {
        bits: 16,
        float: false,
        rate: p.rate,
    });
    out.end(meta)
}

impl Decoder for VorbisDecoder {
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
        format == "ogg"
    }

    fn shape(
        &self,
        src: &mut dyn ReadSeek,
        _format: &str,
        _opts: &DecodeOptions,
        limits: &Limits,
    ) -> Result<Shape, EngineError> {
        let data = read_all(src, limits)?;
        Ok(shape_of(&probe(
            &mut Media::Bytes(&data),
            limits,
            &NoMonitor,
        )?))
    }

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
        Ok(shape_of(&probe(&mut Media::Reader(&mut f), limits, mon)?))
    }

    fn decode(
        &self,
        src: &mut dyn ReadSeek,
        _format: &str,
        _opts: &DecodeOptions,
        limits: &Limits,
        mon: &dyn Monitor,
    ) -> Result<crate::value::Value, EngineError> {
        let data = read_all(src, limits)?;
        stream::collect(mon, |out| {
            decode_media(&mut Media::Bytes(&data), limits, out, mon)
        })
    }

    fn streams(&self, _format: &str) -> bool {
        true
    }

    /// 読み口（64 KiB と 1 ページ）・ブロックの作業列（2 × 8,192 × チャンネル × 4）・出す標本の写し（8,192 × チャンネル × 4）
    /// を、前の symphonia の見積もり（4 MiB ＋ 8,192 × チャンネル × 8）のまま多めに見込み、codebook の表を [`VORBIS_TABLES`]
    /// まで足す（それを超える分は [`Decoder::held_bytes`]）。
    fn stream_work_bytes(&self, _format: &str, shape: &Shape, _len: u64) -> u64 {
        let Shape::Audio { channels, .. } = shape else {
            return 0;
        };
        let ch = u64::from(*channels);
        (4 << 20) + 8_192 * ch * 8 + 8_192 * ch * 12 + VORBIS_TABLES
    }

    /// 頭のパケット（コメント・画像）と、codebook の表のうち [`VORBIS_TABLES`] を超える分（`codec::held`）。
    fn held_bytes(
        &self,
        src: &mut dyn ReadSeek,
        _format: &str,
        limits: &Limits,
    ) -> Result<Held, EngineError> {
        let mut h = held::ogg(src, limits, held::VorbisTables::Kura)?;
        h.decode = h.decode.saturating_sub(VORBIS_TABLES);
        Ok(h)
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

    use super::*;
    use crate::codec::lossy::SymphoniaDecoder;
    use crate::value::Value;

    fn planes(v: &Value) -> Vec<Vec<f32>> {
        let Value::Audio(a) = v else {
            panic!("音声でない")
        };
        (0..a.channels())
            .map(|c| {
                let mut p = vec![0f32; a.frames()];
                a.read(c, 0, &mut p).unwrap();
                p
            })
            .collect()
    }

    fn decode_with(d: &dyn Decoder, data: &[u8]) -> Result<Value, EngineError> {
        d.decode(
            &mut Cursor::new(data.to_vec()),
            "ogg",
            &DecodeOptions::default(),
            &Limits::default(),
            &NoMonitor,
        )
    }

    /// 自作の読み手で、Vorbis のチャンネルの並びのまま交互に並べた標本のビット（照合用の C の出力と同じ形）。
    fn ours_interleaved(data: &[u8]) -> Result<(u32, u32, Vec<u32>), EngineError> {
        let limits = Limits::default();
        let mut media = Media::Bytes(data);
        let mut r = open(&mut media, &limits, &NoMonitor, true)?;
        let (ch, rate) = (r.id.channels as usize, r.id.rate as u32);
        let mut out = Vec::new();
        let limit = |_| bad_header("上限");
        r.run(true, u64::MAX, &limit, &mut |dsp, n| {
            for i in 0..n {
                for c in 0..ch {
                    out.push(dsp.pcm_slice(c, n)[i].to_bits());
                }
            }
            Ok(())
        })?;
        Ok((ch as u32, rate, out))
    }

    /// 照合の入力：Vorbis のフィクスチャ（細工のものを除く）と、自作の書き出しの出力（レート × チャンネル × 品質 × 音）。
    fn oracle_inputs() -> Vec<(String, Vec<u8>)> {
        oracle_inputs_every(1)
    }

    /// [`oracle_inputs`] の、書き出しの出力を `step` 個に 1 つだけ作るもの。
    fn oracle_inputs_every(step: u64) -> Vec<(String, Vec<u8>)> {
        use crate::codec::vorbis::oracle::{Signal, signal};
        use std::sync::Arc;
        let mut out = Vec::new();
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/fixtures");
        let mut names: Vec<_> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
        names.sort();
        for p in names {
            let name = p.file_name().unwrap().to_string_lossy().to_string();
            if name.starts_with("ogg-vorbis")
                && name.ends_with(".ogg")
                && !name.contains("granule-trim")
            {
                out.push((name, std::fs::read(&p).unwrap()));
            }
        }
        let signals = [
            Signal::Music,
            Signal::Noise,
            Signal::Impulses,
            Signal::Bursts,
        ];
        let mut k = 0u64;
        for rate in [
            8_000u32, 11_025, 16_000, 22_050, 32_000, 44_100, 48_000, 96_000,
        ] {
            for channels in [1usize, 2, 3, 6, 8] {
                for q in [-1i32, 0, 5, 10] {
                    k += 1;
                    if !(k - 1).is_multiple_of(step) {
                        continue;
                    }
                    let sig = signals[(k % 4) as usize];
                    let frames = (rate as usize / 3) + (k as usize * 37) % 700;
                    let planes = signal(sig, rate, channels, frames, k)
                        .into_iter()
                        .map(Arc::new)
                        .collect();
                    let v = crate::value::Value::Audio(Arc::new(
                        crate::value::Audio::new(rate, planes, crate::value::AudioMeta::default())
                            .unwrap(),
                    ));
                    let bytes = crate::codec::CodecSet::pinned()
                        .encode_bytes(&v, "ogg", &serde_json::json!({ "quality": q }), &NoMonitor)
                        .unwrap()
                        .0;
                    out.push((
                        format!("kura-enc {rate} Hz {channels} ch q{q} {sig:?}"),
                        bytes,
                    ));
                }
            }
        }
        out
    }

    /// 値を持たずに数える表の量（`codec::held::ogg` の見積もり）が、読み取りが予算で確かめる量と同じ（フィクスチャと
    /// 書き出しの出力の 1/8）。
    #[test]
    fn table_scan_matches_the_unpack() {
        for (name, data) in oracle_inputs_every(8) {
            let mut media = Media::Bytes(&data);
            let mut ogg = OggReader::open(&mut media, &NoMonitor, DEC_ID).unwrap();
            let mut h = Headers::new(u64::MAX);
            let mut last = Vec::new();
            while h.setup.is_none() {
                let p = ogg.next_packet().unwrap().unwrap();
                h.headerin(&p.data, p.bos).unwrap();
                last = p.data;
            }
            let want = h.setup.unwrap().table_bytes;
            assert!(want.setup > 0 && want.decode > 0, "{name}");
            assert_eq!(codebook::setup_table_bytes(&last), Some(want), "{name}");
        }
    }

    /// 音声のパケットを 1 つずつのページに入れ直し、最初の数パケットのどれか 1 つにだけグラニュールを付ける（ほかのページは
    /// −1）。その値を極端（i64 の端・−2）にしても panic しない（位置の足し算は C の -fwrapv と同じく折り返す。最初の
    /// パケットに i64::MIN + 1 を付けると始まりの切り取りが折り返し、i64::MAX − 100 を付けると次のパケットで位置が
    /// 折り返す）。読めるか、壊れたファイル（`Decode`）か、上限（`Limit`）のどれか。
    #[test]
    fn extreme_granules_do_not_panic() {
        use crate::codec::ogg::tests::page;
        let data = std::fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/fixtures/ogg-vorbis-ffmpeg.ogg"
        ))
        .unwrap();
        let mut media = Media::Bytes(&data);
        let mut ogg = OggReader::open(&mut media, &NoMonitor, DEC_ID).unwrap();
        let mut packets = Vec::new();
        while let Some(p) = ogg.next_packet().unwrap() {
            packets.push(p.data);
        }
        assert!(packets.len() > 8);
        let file = |target: usize, g: i64| {
            let mut f = Vec::new();
            for (i, p) in packets.iter().enumerate() {
                let (flags, granule) = match i {
                    0 => (0x02, 0),
                    1 | 2 => (0, 0),
                    _ if i == packets.len() - 1 => (0x04, u64::MAX),
                    _ if i == 3 + target => (0, g as u64),
                    _ => (0, u64::MAX),
                };
                f.extend_from_slice(&page(1, i as u32, flags, granule, &[p]));
            }
            f
        };
        let mut wrapped = 0;
        for target in 0..3 {
            for g in [
                i64::MIN,
                i64::MIN + 1,
                i64::MIN + 3_000,
                -2,
                i64::MAX - 100,
                i64::MAX,
            ] {
                match decode_with(&VorbisDecoder, &file(target, g)) {
                    Ok(_) | Err(EngineError::Limit(_)) => {}
                    Err(EngineError::Decode { .. }) => wrapped += 1,
                    Err(e) => panic!("{target} {g}: {e:?}"),
                }
            }
        }
        // 始まりの切り取りが折り返したものは、次のパケットで壊れたファイルになる
        assert!(wrapped > 0);
    }

    /// 予算（`max_value_bytes`）より表が大きければ、開くときに `Unsupported`。
    #[test]
    fn small_table_budget_is_unsupported() {
        let data = std::fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/fixtures/ogg-vorbis-ffmpeg.ogg"
        ))
        .unwrap();
        let limits = Limits {
            max_value_bytes: 64 << 10,
            ..Limits::default()
        };
        let mut media = Media::Bytes(&data);
        let err = open(&mut media, &limits, &NoMonitor, true).err().unwrap();
        assert!(matches!(err, EngineError::Unsupported(_)), "{err:?}");
    }

    /// 照合用の C（libogg と libvorbis の `decoder_example` と同じ流れ。`oracle_dec`）とビット単位で同じ PCM。
    /// `KURA_VORBIS_ORACLE` のときだけ回る（`crates/vorbis-tables/oracle/build.py` で組む）。
    #[test]
    fn bit_exact_against_the_c_decoder() {
        let Some(root) = crate::codec::vorbis::oracle::root() else {
            return;
        };
        let bin = root.join("bin/oracle_dec");
        assert!(
            bin.exists(),
            "{} がありません（build.py で組み直す）",
            bin.display()
        );
        let (inp, outp) = (
            crate::codec::vorbis::oracle::temp("dec-in.ogg"),
            crate::codec::vorbis::oracle::temp("dec-out.pcm"),
        );
        let mut n = 0;
        for (name, data) in oracle_inputs() {
            std::fs::write(&inp, &data).unwrap();
            let st = std::process::Command::new(&bin)
                .arg(&inp)
                .arg(&outp)
                .status()
                .unwrap();
            assert!(st.success(), "{name}：oracle_dec が失敗");
            let c = std::fs::read(&outp).unwrap();
            assert_eq!(&c[..4], b"KDEC", "{name}");
            let word = |i: usize| u32::from_le_bytes([c[i], c[i + 1], c[i + 2], c[i + 3]]);
            let (cch, crate_) = (word(4), word(8));
            let cbits: Vec<u32> = (12..c.len()).step_by(4).map(word).collect();
            let (ch, rate, ours) =
                ours_interleaved(&data).unwrap_or_else(|e| panic!("{name}：{e}"));
            assert_eq!((ch, rate), (cch, crate_), "{name}");
            assert_eq!(
                ours.len(),
                cbits.len(),
                "{name}：長さ（{} / {} 標本）",
                ours.len(),
                cbits.len()
            );
            if let Some(i) = ours.iter().zip(&cbits).position(|(a, b)| a != b) {
                panic!(
                    "{name}：標本 {i}（フレーム {}・チャンネル {}）が違う：{} / {}",
                    i / ch as usize,
                    i % ch as usize,
                    f32::from_bits(ours[i]),
                    f32::from_bits(cbits[i])
                );
            }
            n += 1;
        }
        let _ = std::fs::remove_file(&inp);
        let _ = std::fs::remove_file(&outp);
        assert!(n > 150, "{n}");
    }

    /// 前の symphonia の読み取り（/3）とほぼ同じ値（演算の順が違うので少しずれる）・同じ長さ・同じタグ。
    #[test]
    fn close_to_symphonia_on_fixtures() {
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/fixtures");
        let mut n = 0;
        let mut names: Vec<_> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
        names.sort();
        for p in names {
            if p.extension().is_none_or(|x| x != "ogg") {
                continue;
            }
            let data = std::fs::read(&p).unwrap();
            // 上限の試験のために作った細工のファイル：floor 0 が値のない codebook を使うので、libvorbis（floor0.c:85）と同じく断る
            if p.ends_with("ogg-vorbis-granule-trim.ogg") {
                assert!(matches!(
                    decode_with(&VorbisDecoder, &data),
                    Err(EngineError::Decode { .. })
                ));
                continue;
            }
            let old = decode_with(&SymphoniaDecoder::ogg_vorbis(), &data);
            let new = decode_with(&VorbisDecoder, &data);
            match (old, new) {
                (Ok(a), Ok(b)) => {
                    let (pa, pb) = (planes(&a), planes(&b));
                    assert_eq!(pa.len(), pb.len(), "{}", p.display());
                    assert_eq!(pa[0].len(), pb[0].len(), "{}：長さ", p.display());
                    let mut max = 0f32;
                    for (x, y) in pa.iter().zip(&pb) {
                        for (u, v) in x.iter().zip(y) {
                            max = crate::math::max32(max, (u - v).abs());
                        }
                    }
                    assert!(max < 1e-4, "{}：差の最大 {max}", p.display());
                    let (Value::Audio(a), Value::Audio(b)) = (&a, &b) else {
                        unreachable!()
                    };
                    assert_eq!(a.meta.tags, b.meta.tags, "{}", p.display());
                    assert_eq!(a.meta.loop_range, b.meta.loop_range, "{}", p.display());
                    n += 1;
                }
                (Err(EngineError::Unsupported(_)), Err(EngineError::Unsupported(_))) => {}
                (a, b) => panic!("{}：{:?} / {:?}", p.display(), a.err(), b.err()),
            }
        }
        assert!(n >= 3, "{n}");
    }
    /// 速さの目安（release で `--ignored`）：5 分のステレオ（自作の書き出しの品質 5。6.5 MB）を前の symphonia の読み手と自作の
    /// 読み手で読む（値のダイジェストは含めない）。M16 段 2 の実測（M4 Max）：symphonia 0.176 秒、自作 0.270 秒（うち長さを
    /// 数える道 0.004 秒）。同じ旗（-O2・FMA なし・-fwrapv）で組んだ C の libvorbis（出力を書かない `oracle_dec`）は 0.32 秒。
    #[test]
    #[ignore]
    // 計るだけの試験（値は時刻によらない）
    #[allow(clippy::disallowed_methods)]
    fn speed_against_symphonia() {
        use crate::codec::vorbis::oracle::{Signal, signal};
        use std::sync::Arc;
        let input = signal(Signal::Music, 44_100, 2, 44_100 * 300, 7)
            .into_iter()
            .map(Arc::new)
            .collect();
        let v = Value::Audio(Arc::new(
            crate::value::Audio::new(44_100, input, crate::value::AudioMeta::default()).unwrap(),
        ));
        let data = crate::codec::CodecSet::pinned()
            .encode_bytes(&v, "ogg", &serde_json::json!({ "quality": 5 }), &NoMonitor)
            .unwrap()
            .0;
        let mut best = [f64::MAX; 2];
        for _ in 0..3 {
            let t = std::time::Instant::now();
            let a = decode_with(&SymphoniaDecoder::ogg_vorbis(), &data).unwrap();
            best[0] = crate::math::min(best[0], t.elapsed().as_secs_f64());
            let t = std::time::Instant::now();
            let b = decode_with(&VorbisDecoder, &data).unwrap();
            best[1] = crate::math::min(best[1], t.elapsed().as_secs_f64());
            assert_eq!(planes(&a)[0].len(), planes(&b)[0].len());
        }
        // 内訳：長さを数える道だけ
        let mut count = f64::MAX;
        for _ in 0..3 {
            let t = std::time::Instant::now();
            let mut media = Media::Bytes(&data);
            probe(&mut media, &Limits::default(), &NoMonitor).unwrap();
            count = crate::math::min(count, t.elapsed().as_secs_f64());
        }
        println!(
            "{} バイト：symphonia {:.3} 秒、自作 {:.3} 秒（うち長さを数える道 {:.3} 秒）",
            data.len(),
            best[0],
            best[1],
            count
        );
    }
}
