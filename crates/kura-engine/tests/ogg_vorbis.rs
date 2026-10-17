//! 段 7 の受け入れテスト（docs/editing.md §17 の E28）：自作の Ogg Vorbis の出力が、独立したデコーダ 2 つ
//! （symphonia・lewton）で読め、長さ・位置・ループのコメントが保たれる。ページの割り方の規則（ADR-149）も確かめる。
//!
//! - lewton は低い層の口（ヘッダとパケットの読み取り）だけを使い、Ogg のページはここで読む（lewton の
//!   `OggStreamReader` は末尾の切り取りを誤ることがあるため。ADR-149）。最後のページのグラニュールで長さを切る。

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::io::Cursor;
use std::sync::Arc;

use kura_engine::codec::CodecSet;
use kura_engine::doc::DecodeOptions;
use kura_engine::testgen::InputGen;
use kura_engine::value::{Audio, AudioMeta, LoopRange};
use kura_engine::{EngineError, Limits, NoMonitor, Value};
use serde_json::json;

fn audio(v: &Value) -> &Arc<Audio> {
    match v {
        Value::Audio(a) => a,
        _ => panic!("音声ではありません"),
    }
}

fn encode(v: &Value, opts: serde_json::Value) -> Vec<u8> {
    CodecSet::pinned()
        .encode_bytes(v, "ogg", &opts, &NoMonitor)
        .unwrap_or_else(|e| panic!("ogg に書けません: {e}"))
        .0
}

/// symphonia（kura の Ogg Vorbis のデコーダ）で読む。
fn decode_symphonia(bytes: &[u8]) -> Arc<Audio> {
    let (v, _) = CodecSet::pinned()
        .decode(
            &mut Cursor::new(bytes.to_vec()),
            "ogg",
            &DecodeOptions::default(),
            &Limits::default(),
            &NoMonitor,
        )
        .unwrap_or_else(|e| panic!("symphonia で読めません: {e}"));
    audio(&v).clone()
}

/// Ogg の 1 ページ。
#[derive(Debug)]
struct Page {
    flags: u8,
    granule: i64,
    serial: u32,
    seq: u32,
    /// セグメントの長さの並び。
    lacing: Vec<u8>,
    body: Vec<u8>,
}

/// CRC32（多項式 0x04C11DB7、反射なし、初期値 0）。
fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0u32;
    for &b in data {
        crc ^= u32::from(b) << 24;
        for _ in 0..8 {
            crc = if crc & 0x8000_0000 != 0 {
                (crc << 1) ^ 0x04C1_1DB7
            } else {
                crc << 1
            };
        }
    }
    crc
}

/// ページに分ける（形と CRC を確かめる）。
fn pages(bytes: &[u8]) -> Vec<Page> {
    let mut out = Vec::new();
    let mut at = 0usize;
    while at < bytes.len() {
        let h = &bytes[at..];
        assert_eq!(&h[0..4], b"OggS", "ページの印（{at}）");
        assert_eq!(h[4], 0, "版");
        let flags = h[5];
        assert_eq!(flags & !0x07, 0, "未定義の旗");
        let granule = i64::from_le_bytes(h[6..14].try_into().unwrap());
        let serial = u32::from_le_bytes(h[14..18].try_into().unwrap());
        let seq = u32::from_le_bytes(h[18..22].try_into().unwrap());
        let crc = u32::from_le_bytes(h[22..26].try_into().unwrap());
        let n = usize::from(h[26]);
        let lacing = h[27..27 + n].to_vec();
        let body_len: usize = lacing.iter().map(|l| usize::from(*l)).sum();
        let total = 27 + n + body_len;
        let mut page = h[..total].to_vec();
        page[22..26].fill(0);
        assert_eq!(crc32(&page), crc, "CRC（ページ {seq}）");
        out.push(Page {
            flags,
            granule,
            serial,
            seq,
            body: h[27 + n..total].to_vec(),
            lacing,
        });
        at += total;
    }
    out
}

/// ページからパケットを組み立てる：（パケット, それが終わったページの番号）。
fn packets(pages: &[Page]) -> Vec<(Vec<u8>, usize)> {
    let mut out = Vec::new();
    let mut cur: Vec<u8> = Vec::new();
    for (i, p) in pages.iter().enumerate() {
        let mut off = 0usize;
        if i > 0 && p.flags & 0x01 == 0 {
            assert!(
                cur.is_empty(),
                "続きの印がないのに前のパケットが終わっていません"
            );
        }
        for &l in &p.lacing {
            cur.extend_from_slice(&p.body[off..off + usize::from(l)]);
            off += usize::from(l);
            if l < 255 {
                out.push((std::mem::take(&mut cur), i));
            }
        }
    }
    assert!(cur.is_empty(), "最後のパケットが終わっていません");
    out
}

/// lewton で読む（最後のページのグラニュールで長さを切る）。
fn decode_lewton(bytes: &[u8]) -> (Vec<Vec<f32>>, lewton::header::CommentHeader) {
    let pages = pages(bytes);
    let pk = packets(&pages);
    let ident = lewton::header::read_header_ident(&pk[0].0).unwrap();
    let comment = lewton::header::read_header_comment(&pk[1].0).unwrap();
    let setup = lewton::header::read_header_setup(
        &pk[2].0,
        ident.audio_channels,
        (ident.blocksize_0, ident.blocksize_1),
    )
    .unwrap();
    let mut pwr = lewton::audio::PreviousWindowRight::new();
    let mut planes: Vec<Vec<f32>> = vec![Vec::new(); usize::from(ident.audio_channels)];
    for (p, _) in &pk[3..] {
        let out: Vec<Vec<f32>> =
            lewton::audio::read_audio_packet_generic(&ident, &setup, p, &mut pwr).unwrap();
        for (dst, src) in planes.iter_mut().zip(out) {
            dst.extend_from_slice(&src);
        }
    }
    let last = pages.last().unwrap().granule;
    assert!(last >= 0, "最後のグラニュール");
    let n = usize::try_from(last).unwrap();
    for p in &mut planes {
        assert!(
            p.len() >= n,
            "lewton の出力が最後のグラニュールより短い：{} < {n}",
            p.len()
        );
        p.truncate(n);
    }
    (planes, comment)
}

/// 元と同じ位置にあるか：位置 0 の誤差が、±1 ずらした誤差より小さい。信号と誤差の比（dB）を返す。
fn aligned_snr(src: &[f32], got: &[f32]) -> f64 {
    let err = |lag: i64| -> f64 {
        let mut e = 0.0;
        for (i, s) in src.iter().enumerate() {
            let j = i as i64 + lag;
            if j < 0 || j as usize >= got.len() {
                continue;
            }
            let d = f64::from(*s) - f64::from(got[j as usize]);
            e += d * d;
        }
        e
    };
    let (e0, em, ep) = (err(0), err(-1), err(1));
    assert!(e0 <= em && e0 <= ep, "位置がずれている：{e0} {em} {ep}");
    let sig: f64 = src.iter().map(|s| f64::from(*s) * f64::from(*s)).sum();
    10.0 * kura_engine::math::log10(sig / kura_engine::math::max(e0, 1e-30))
}

fn sine(rate: u32, channels: u16, frames: u64) -> Value {
    InputGen::Sine {
        rate,
        channels,
        frames,
        freq: 440,
        loop_range: None,
    }
    .generate()
    .unwrap()
}

/// E28：長さ・位置が、symphonia と lewton の両方で元とちょうど同じ。2 つのデコーダの値も一致する。
#[test]
fn e28_lengths_and_positions_match_in_two_decoders() {
    for &rate in &[8_000u32, 22_050, 44_100, 48_000, 96_000] {
        for &channels in &[1u16, 2] {
            for &frames in &[0u64, 1, 37, 1_000, 4_410, 44_101] {
                let v = sine(rate, channels, frames);
                let bytes = encode(&v, json!({}));
                let sym = decode_symphonia(&bytes);
                let (lew, _) = decode_lewton(&bytes);
                let name = format!("{rate} Hz・{channels} ch・{frames} フレーム");
                assert_eq!(sym.frames() as u64, frames, "{name}：symphonia の長さ");
                assert_eq!(lew[0].len() as u64, frames, "{name}：lewton の長さ");
                assert_eq!(
                    (sym.rate(), sym.channels()),
                    (rate, usize::from(channels)),
                    "{name}"
                );
                for (ch, (a, b)) in sym.planes().iter().zip(&lew).enumerate() {
                    let d = a
                        .iter()
                        .zip(b)
                        .map(|(x, y)| (x - y).abs())
                        .fold(0f32, kura_engine::math::max32);
                    assert!(
                        d <= 1e-4,
                        "{name}：チャンネル {ch} で 2 つのデコーダの差 {d}"
                    );
                }
                // 長いものは、位置がずれず、正弦波がそれなりの精度で戻る
                if frames >= 4_410 {
                    for (ch, p) in audio(&v).planes().iter().enumerate() {
                        let snr = aligned_snr(p, &sym.planes()[ch]);
                        assert!(snr >= 20.0, "{name}：チャンネル {ch} の SNR {snr:.1} dB");
                    }
                }
            }
        }
    }
}

/// E28：ループ点（LOOPSTART・LOOPLENGTH）とタグが保たれる。RPG ツクール MV/MZ の読み方に合わせて、
/// LOOPLENGTH の直後に ENCODER（長さの下位バイトが数字にならない番兵）を置く。vendor は "kura"。
#[test]
fn e28_loop_comments_are_kept() {
    let mut a = (**audio(&sine(44_100, 2, 88_200))).clone();
    a.meta.loop_range = Some(LoopRange {
        start: 4_410,
        end: 4_410 + 22_050,
    });
    a.meta.tags.insert("title".into(), "効果音".into());
    a.meta.tags.insert("vorbis:ENCODER".into(), "other".into());
    let v = Value::Audio(Arc::new(a));
    let bytes = encode(&v, json!({ "quality": 3 }));
    let back = decode_symphonia(&bytes);
    assert_eq!(
        back.meta.loop_range,
        Some(LoopRange {
            start: 4_410,
            end: 26_460
        })
    );
    assert_eq!(
        back.meta.tags.get("title").map(String::as_str),
        Some("効果音")
    );
    let (_, comment) = decode_lewton(&bytes);
    assert_eq!(comment.vendor, "kura");
    let list: Vec<(String, String)> = comment.comment_list.clone();
    assert_eq!(list[0], ("LOOPSTART".into(), "4410".into()));
    assert_eq!(list[1], ("LOOPLENGTH".into(), "22050".into()));
    assert_eq!(list[2], ("ENCODER".into(), "ogg-vorbis-kura-enc/1".into()));
    // 元の ENCODER は運ばない
    assert_eq!(list.iter().filter(|(k, _)| k == "ENCODER").count(), 1);
    // 番兵：ENCODER の項目の長さ（4 バイトの小さいエンディアン）の下位バイトが '0'〜'9' でない
    let entry = format!("ENCODER={}", "ogg-vorbis-kura-enc/1");
    assert!(!((entry.len() & 0xFF) as u8).is_ascii_digit());
}

/// E28：ページの割り方の規則（ADR-149）。lewton・ffmpeg が長さを保ち、symphonia・stb_vorbis が読めるように。
#[test]
fn e28_pages_follow_the_rules() {
    for frames in [0u64, 1, 2_000, 200_000] {
        let bytes = encode(&sine(48_000, 2, frames), json!({}));
        let ps = pages(&bytes);
        let pk = packets(&ps);
        // A1：1 ページ目は識別ヘッダ（30 バイト）だけ、BOS、グラニュール 0
        assert_eq!(ps[0].flags, 0x02, "{frames}：BOS");
        assert_eq!(ps[0].lacing, vec![30]);
        assert_eq!(ps[0].granule, 0);
        // A2・A3：コメントは 2 ページ目の頭から、setup でページを閉じ（次のページは続きでない）、グラニュール 0。
        // 音声は setup のあとのページから
        assert_eq!(pk[0].1, 0);
        assert_eq!(ps[1].flags & 0x01, 0);
        assert_eq!(ps[pk[2].1].granule, 0);
        assert_eq!(ps[pk[2].1 + 1].flags & 0x01, 0, "setup でページを閉じる");
        assert!(
            pk.iter().skip(3).all(|(_, page)| *page > pk[2].1),
            "音声は setup のあとのページ"
        );
        // A4：最初の音声パケットだけで 1 ページ、グラニュール 0
        let first_audio_page = pk[3].1;
        assert_eq!(
            pk.iter()
                .filter(|(_, page)| *page == first_audio_page)
                .count(),
            1
        );
        assert_eq!(ps[first_audio_page].granule, 0);
        // A6：EOS は最後のページだけ、空の EOS ページはない
        for (i, p) in ps.iter().enumerate() {
            assert_eq!(
                p.flags & 0x04 != 0,
                i + 1 == ps.len(),
                "{frames}：EOS（ページ {i}）"
            );
            assert_eq!(p.seq as usize, i, "ページ番号");
            assert_eq!(p.serial, ps[0].serial, "シリアル番号");
        }
        assert!(!ps.last().unwrap().lacing.is_empty());
        // A7：最後のページのグラニュールは長さちょうど、グラニュールは減らない
        assert_eq!(ps.last().unwrap().granule, i64::try_from(frames).unwrap());
        let gs: Vec<i64> = ps.iter().map(|p| p.granule).filter(|g| *g >= 0).collect();
        assert!(gs.windows(2).all(|w| w[0] <= w[1]));
        // 1 ページの lacing（セグメント）は 255 個まで（RFC 3533）
        assert!(ps.iter().all(|p| p.lacing.len() <= 255));
        // 識別ヘッダとコメントヘッダの最後のバイトは framing bit の 0x01（Vorbis I §4.2.2・§5.2.1）
        assert_eq!(*pk[0].0.last().unwrap(), 1);
        assert_eq!(*pk[1].0.last().unwrap(), 1);
        // 音声パケットは空でない（1 バイト以上）
        assert!(pk[3..].iter().all(|(p, _)| !p.is_empty()));
    }
}

/// 品質の段：q −1〜10 のどれでも書いて読め、ビットレートは q とともに増える（音楽に近い信号で）。
#[test]
fn quality_steps_encode_and_grow() {
    let v = InputGen::AudioNoise {
        rate: 44_100,
        channels: 2,
        frames: 88_200,
        seed: 3,
    }
    .generate()
    .unwrap();
    let mut sizes = Vec::new();
    for q in -1..=10 {
        let bytes = encode(&v, json!({ "quality": q }));
        assert_eq!(decode_symphonia(&bytes).frames(), 88_200, "q{q}");
        sizes.push(bytes.len());
    }
    assert!(sizes.windows(2).all(|w| w[0] <= w[1]), "{sizes:?}");
    // 範囲の外・知らない設定は断る
    for bad in [
        json!({ "quality": 11 }),
        json!({ "quality": -2 }),
        json!({ "bitrate": 128 }),
    ] {
        assert!(
            CodecSet::pinned()
                .encode_bytes(&v, "ogg", &bad, &NoMonitor)
                .is_err(),
            "{bad}"
        );
    }
}

/// 決定性：同じ入力・設定なら同じバイト列。スレッド数（1・3・16）によらない。
#[test]
fn encoding_is_deterministic() {
    let v = InputGen::AudioNoise {
        rate: 48_000,
        channels: 2,
        frames: 150_000,
        seed: 11,
    }
    .generate()
    .unwrap();
    let mut outs = Vec::new();
    for threads in [1, 3, 16] {
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .build()
            .unwrap();
        outs.push(pool.install(|| encode(&v, json!({ "quality": 4 }))));
    }
    assert!(outs.windows(2).all(|w| w[0] == w[1]));
}

/// Vorbis の並び（Vorbis I §4.3.9）の i 番のチャンネルが、kura（WAV の WAVEFORMATEXTENSIBLE のマスクの順）の何番か。
/// エンコーダの並び（symphonia の `map_vorbis_channel` の写し）とは別に、仕様から書いた表。
///
/// | ch | Vorbis の並び | WAV の並び |
/// |---|---|---|
/// | 3 | L C R | FL FR FC |
/// | 4 | FL FR RL RR | FL FR BL BR |
/// | 5 | FL C FR RL RR | FL FR FC BL BR |
/// | 6 | FL C FR RL RR LFE | FL FR FC LFE BL BR |
/// | 7 | FL C FR SL SR RC LFE | FL FR FC LFE BC SL SR |
/// | 8 | FL C FR SL SR RL RR LFE | FL FR FC LFE BL BR SL SR |
fn vorbis_to_wav(channels: usize) -> &'static [usize] {
    match channels {
        1 => &[0],
        2 => &[0, 1],
        3 => &[0, 2, 1],
        4 => &[0, 1, 2, 3],
        5 => &[0, 2, 1, 3, 4],
        6 => &[0, 2, 1, 4, 5, 3],
        7 => &[0, 2, 1, 5, 6, 4, 3],
        8 => &[0, 2, 1, 6, 7, 4, 5, 3],
        _ => panic!("{channels} チャンネルの並びはありません"),
    }
}

/// 1〜8 チャンネルとレート 8〜192 kHz：チャンネルの並びが保たれる（チャンネルごとに違う周波数で確かめる）。
/// symphonia（WAV の並びで出す）で元と照らし、lewton（Vorbis の並びのまま出す）の値を [`vorbis_to_wav`] で WAV の並びに
/// 直して symphonia と照らす（エンコーダと symphonia が同じ並びの誤りをしても、仕様の表とは合わない）。
#[test]
fn channels_and_rates_round_trip() {
    for &(rate, channels) in &[
        (8_000u32, 1u16),
        (11_025, 2),
        (16_000, 3),
        (32_000, 4),
        (44_100, 5),
        (48_000, 6),
        (88_200, 7),
        (192_000, 8),
    ] {
        let v = sine(rate, channels, u64::from(rate));
        let bytes = encode(&v, json!({}));
        let back = decode_symphonia(&bytes);
        assert_eq!(back.channels(), usize::from(channels), "{rate}・{channels}");
        assert_eq!(back.frames() as u64, u64::from(rate));
        for (ch, p) in audio(&v).planes().iter().enumerate() {
            let snr = aligned_snr(p, &back.planes()[ch]);
            assert!(
                snr >= 15.0,
                "{rate} Hz・{channels} ch のチャンネル {ch}：{snr:.1} dB"
            );
        }
        let (lew, _) = decode_lewton(&bytes);
        assert_eq!(lew.len(), usize::from(channels), "{rate}・{channels}");
        let map = vorbis_to_wav(usize::from(channels));
        for (vi, plane) in lew.iter().enumerate() {
            let wi = map[vi];
            assert_eq!(plane.len(), back.frames(), "{rate}・{channels}");
            let d = plane
                .iter()
                .zip(back.planes()[wi].iter())
                .map(|(x, y)| (x - y).abs())
                .fold(0f32, kura_engine::math::max32);
            assert!(
                d <= 1e-4,
                "{rate} Hz・{channels} ch：lewton の {vi} 番（Vorbis の並び）と symphonia の {wi} 番（WAV の並び）の差 {d}"
            );
            // 元の同じチャンネルとも位置が合う
            let snr = aligned_snr(&audio(&v).planes()[wi], plane);
            assert!(
                snr >= 15.0,
                "{rate} Hz・{channels} ch：lewton の {vi} 番と元の {wi} 番：{snr:.1} dB"
            );
        }
    }
    // 範囲の外は書かない（次の段へ）
    for (rate, channels) in [(7_999u32, 1u16), (192_001, 1), (44_100, 9)] {
        let v = Value::Audio(Arc::new(
            Audio::new(
                rate,
                (0..channels).map(|_| Arc::new(vec![0f32; 100])).collect(),
                AudioMeta::default(),
            )
            .unwrap(),
        ));
        let r = CodecSet::pinned().encode_bytes(&v, "ogg", &json!({}), &NoMonitor);
        assert!(r.is_err(), "{rate}・{channels}");
    }
}

/// Ogg のページのシリアル番号と CRC を 0 にしたもの（シリアル番号は入力の値のダイジェストから決まるので、標本の違う
/// 入力どうしでは違う。それ以外のバイト列を比べる）。
fn without_serial(ogg: &[u8]) -> Vec<u8> {
    let mut b = ogg.to_vec();
    let mut at = 0usize;
    while at < b.len() {
        assert_eq!(&b[at..at + 4], b"OggS", "ページの印（{at}）");
        let n = usize::from(b[at + 26]);
        let body: usize = b[at + 27..at + 27 + n]
            .iter()
            .map(|l| usize::from(*l))
            .sum();
        b[at + 14..at + 18].fill(0);
        b[at + 22..at + 26].fill(0);
        at += 27 + n + body;
    }
    b
}

/// NaN・∞ を含む入力でも落ちずに書け、読める。NaN（符号・ペイロードを問わない）は 0、±∞ は ±1 として符号化する：
/// 0・±1 に置き換えた入力の書き出しと、シリアル番号と CRC のほかはバイト一致する（ターゲットで NaN のビットの形が
/// 違っても同じ出力にする約束。docs/editing.md の E28）。1 を超える有限の値は切らない（1.5 は 1 と違う出力）。
#[test]
fn non_finite_samples_are_tamed() {
    let nans = [
        f32::NAN,
        -f32::NAN,
        f32::from_bits(0x7FC0_1234),
        f32::from_bits(0xFFC0_0001),
        // シグナリング NaN（仮数の最上位ビットが 0）
        f32::from_bits(0x7F80_0001),
    ];
    // (チャンネル数, 品質)
    for (channels, q) in [(1usize, -1i64), (1, 5), (1, 10), (6, 3)] {
        let frames = 20_000usize;
        let base: Vec<Vec<f32>> = (0..channels)
            .map(|c| {
                (0..frames)
                    .map(|i| {
                        0.3 * kura_engine::math::sin(i as f64 * (0.05 + 0.01 * c as f64)) as f32
                    })
                    .collect()
            })
            .collect();
        let (mut raw, mut fixed) = (base.clone(), base.clone());
        for (c, (r, f)) in raw.iter_mut().zip(fixed.iter_mut()).enumerate() {
            for (pos, x, y) in [
                (100 + c, nans[c % nans.len()], 0.0f32),
                (200 + c, f32::INFINITY, 1.0),
                (300 + c, f32::NEG_INFINITY, -1.0),
                (9_000 + 7 * c, nans[(c + 1) % nans.len()], 0.0),
                // 連続した非有限の標本
                (15_000, f32::INFINITY, 1.0),
                (15_001, nans[(c + 2) % nans.len()], 0.0),
                (15_002, f32::NEG_INFINITY, -1.0),
            ] {
                r[pos] = x;
                f[pos] = y;
            }
        }
        let make = |planes: &[Vec<f32>]| {
            Value::Audio(Arc::new(
                Audio::new(
                    44_100,
                    planes.iter().cloned().map(Arc::new).collect(),
                    AudioMeta::default(),
                )
                .unwrap(),
            ))
        };
        let name = format!("{channels} ch・q{q}");
        let raw_bytes = encode(&make(&raw), json!({ "quality": q }));
        let fixed_bytes = encode(&make(&fixed), json!({ "quality": q }));
        assert_eq!(
            without_serial(&raw_bytes),
            without_serial(&fixed_bytes),
            "{name}：NaN・±∞ を 0・±1 に置き換えた入力と違う"
        );
        // 1 を超える値は切らない（置き換えは非有限の値だけ）
        let mut over = fixed.clone();
        over[0][200] = 1.5;
        assert_ne!(
            without_serial(&encode(&make(&over), json!({ "quality": q }))),
            without_serial(&fixed_bytes),
            "{name}：1.5 を 1 と同じに書いた"
        );
        let back = decode_symphonia(&raw_bytes);
        assert_eq!(back.frames(), frames, "{name}");
        assert!(
            back.planes()
                .iter()
                .all(|p| p.iter().all(|s| s.is_finite())),
            "{name}"
        );
    }
}

/// 書けない設定のエラーの種類。
#[test]
fn unsupported_shapes_are_unsupported() {
    let v = Value::Audio(Arc::new(
        Audio::new(
            44_100,
            (0..9).map(|_| Arc::new(vec![0f32; 10])).collect(),
            AudioMeta::default(),
        )
        .unwrap(),
    ));
    let r = CodecSet::pinned().encode_bytes(&v, "ogg", &json!({}), &NoMonitor);
    assert!(matches!(r, Err(EngineError::Unsupported(_))), "{r:?}");
}
