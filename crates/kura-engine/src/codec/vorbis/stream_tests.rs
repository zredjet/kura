//! ストリーム全体の照合（`KURA_VORBIS_ORACLE` があるときだけ。CI では回らない）：kura の書き出し（[`encoder::encode_packets`]）
//! のパケットの列が、照合用の C エンコーダ（`oracle::encode`）と、識別・setup・すべての音声パケットでバイト単位で一致し、
//! granulepos と EOS も一致する（コメントは vendor とタグが違うので比べない）。
//!
//! - 入力は `oracle::signal`（Vorbis の並び）。kura には WAV の並びの planes（`ORDER` の逆）で渡し、encoder が Vorbis の並びに
//!   戻す。照合用の C には Vorbis の並びのまま渡す。
//! - 一致しないときは、照合用のダンプ（レベル 2）と kura の段 1・段 2 の値（窓・MDCT・FFT・logfft・logmdct・ampmax・M5 の
//!   lowcomp）を比べて、最初に違うブロック・記録を示す（段 3 の中は psy.rs・floor1.rs・res0.rs の照合が関数ごとに見る）。

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;

use super::ORDER;
use super::encoder::{self, Encoded};
use super::oracle::{self, Signal};
use crate::monitor::NoMonitor;
use crate::value::{Audio, AudioMeta};

const RATES: [u32; 9] = [
    8_000, 11_025, 16_000, 22_050, 32_000, 44_100, 48_000, 96_000, 192_000,
];
const CHANNELS: [usize; 5] = [1, 2, 3, 6, 8];
const QS: [i32; 5] = [-1, 0, 3, 5, 10];
const SIGNALS: [Signal; 5] = [
    Signal::Music,
    Signal::Noise,
    Signal::Sine,
    Signal::Silence,
    Signal::Impulses,
];

/// Vorbis の並びの planes を、kura の値（WAV の並び）にする。
pub(crate) fn kura_audio(rate: u32, vorbis_planes: &[Vec<f32>]) -> Audio {
    let ch = vorbis_planes.len();
    let mut planes = vec![Vec::new(); ch];
    for (i, p) in vorbis_planes.iter().enumerate() {
        planes[ORDER[ch - 1][i]] = p.clone();
    }
    Audio::new(
        rate,
        planes.into_iter().map(Arc::new).collect(),
        AudioMeta::default(),
    )
    .unwrap()
}

/// 照合の数（範囲に届いたかを確かめる）。
#[derive(Debug, Default)]
pub(crate) struct Stats {
    pub cases: usize,
    pub packets: usize,
    pub bytes: usize,
    /// W = 0・1 のブロックの数。
    pub windows: [usize; 2],
    pub templates: std::collections::BTreeSet<String>,
    pub qs: std::collections::BTreeSet<i32>,
    pub channels: std::collections::BTreeSet<usize>,
    pub rates: std::collections::BTreeSet<u32>,
}

/// 1 件の照合（照合用の C がなければ None）。一致しなければ panic（最初に違う所を示す）。
pub(crate) fn check_stream(
    kind: Signal,
    rate: u32,
    ch: usize,
    q: i32,
    frames: usize,
    seed: u64,
    stats: &mut Stats,
) -> Option<()> {
    let input = oracle::signal(kind, rate, ch, frames, seed);
    check_input(&format!("{kind:?}"), &input, rate, q, stats)
}

/// 1 つの入力（Vorbis の並び）の照合。
pub(crate) fn check_input(
    label: &str,
    input: &[Vec<f32>],
    rate: u32,
    q: i32,
    stats: &mut Stats,
) -> Option<()> {
    let ch = input.len();
    let frames = input.first().map_or(0, Vec::len);
    let (want, _) = oracle::encode(input, rate, q, None)?;
    let audio = kura_audio(rate, input);
    let got = encoder::encode_packets(&audio, i64::from(q), &NoMonitor).unwrap();
    let name = format!("{label}・{rate} Hz・{ch} ch・q{q}・{frames} フレーム");
    if let Some(msg) = first_difference(&got, &want) {
        let detail = locate(input, rate, q, &got, &want);
        panic!("{name}：{msg}\n{detail}");
    }
    stats.cases += 1;
    stats.packets += got.packets.len();
    stats.bytes += got.packets.iter().map(|p| p.packet.len()).sum::<usize>();
    let modebits = vorbis_tables::ov_ilog(got.blockflags.len() as u32 - 1) as u32;
    for p in &got.packets {
        let w = encoder::packet_window(&p.packet, modebits, &got.blockflags).unwrap();
        stats.windows[usize::from(w.long)] += 1;
    }
    let setup = oracle::setup(rate, ch, q, false).unwrap();
    stats
        .templates
        .insert(setup["template"]["name"].as_str().unwrap().to_string());
    stats.qs.insert(q);
    stats.channels.insert(ch);
    stats.rates.insert(rate);
    Some(())
}

/// kura のパケットの列と照合用のものの最初の違い（同じなら None）。
fn first_difference(got: &Encoded, want: &[oracle::Packet]) -> Option<String> {
    let bit = |a: &[u8], b: &[u8]| {
        a.iter()
            .zip(b)
            .position(|(x, y)| x != y)
            .map(|k| k * 8 + (a[k] ^ b[k]).trailing_zeros() as usize)
    };
    for (i, name) in [(0usize, "識別ヘッダ"), (2, "setup ヘッダ")] {
        let (g, w) = (&got.headers[i].packet, &want[i].body);
        if g != w {
            return Some(format!(
                "{name}が違う（{} と {} バイト、最初に違うビット {:?}）",
                g.len(),
                w.len(),
                bit(g, w)
            ));
        }
    }
    let audio = &want[3..];
    for (k, (g, w)) in got.packets.iter().zip(audio).enumerate() {
        if g.packet != w.body {
            return Some(format!(
                "音声パケット {k} が違う（{} と {} バイト、最初に違うビット {:?}）",
                g.packet.len(),
                w.body.len(),
                bit(&g.packet, &w.body)
            ));
        }
        if g.granulepos != w.granule {
            return Some(format!(
                "音声パケット {k} の granulepos が違う（{} と {}）",
                g.granulepos, w.granule
            ));
        }
        if g.e_o_s != (w.flags & 2 != 0) {
            return Some(format!("音声パケット {k} の EOS が違う"));
        }
    }
    if got.packets.len() != audio.len() {
        return Some(format!(
            "音声パケットの数が違う（{} と {}）",
            got.packets.len(),
            audio.len()
        ));
    }
    None
}

/// 一致しないときの手がかり：照合用のダンプ（レベル 2）と、kura の直列の段 1 の値を比べて、最初に違うブロック・記録。
fn locate(input: &[Vec<f32>], rate: u32, q: i32, got: &Encoded, want: &[oracle::Packet]) -> String {
    use super::block;
    use super::mapping0::{AotuvState, mapping0_ampmax, mapping0_analyze, mapping0_lowcomp};
    let Some((_, Some(dump))) = oracle::encode(input, rate, q, Some(2)) else {
        return "ダンプがない".into();
    };
    let ch = input.len();
    let frames = input.first().map_or(0, Vec::len);
    let vi =
        super::setup::vorbis_encode_init_vbr(ch as i64, i64::from(rate), q as f32 / 10.0).unwrap();
    let mut vd = block::vorbis_analysis_init(vi).unwrap();
    let mut vb = block::vorbis_block_init(&vd);
    let mut st = AotuvState::new(ch);
    if frames > 0 {
        let at = block::vorbis_analysis_buffer(&mut vd, frames as i32).unwrap();
        for (dst, src) in vd.pcm.iter_mut().zip(input) {
            for (d, s) in dst[at..at + frames].iter_mut().zip(src) {
                *d = encoder::sanitize(*s);
            }
        }
        block::vorbis_analysis_wrote(&mut vd, frames as i32, &NoMonitor).unwrap();
    }
    block::vorbis_analysis_wrote(&mut vd, 0, &NoMonitor).unwrap();
    let same = |a: &[f32], b: &[u32]| -> Option<usize> {
        if a.len() != b.len() {
            return Some(usize::MAX);
        }
        a.iter().zip(b).position(|(x, &y)| {
            let y = f32::from_bits(y);
            !(x.to_bits() == y.to_bits() || (x.is_nan() && y.is_nan()))
        })
    };
    let mut b = 0usize;
    while block::vorbis_analysis_blockout(&mut vd, &mut vb).unwrap() {
        let info = dump.block_info(b);
        let mine = [
            b as i64,
            vb.w,
            vb.lw,
            vb.nw,
            i64::from(vb.blocktype),
            i64::from(vb.blocktype) | (vb.w << 1),
            vb.granulepos,
            vb.sequence,
            i64::from(vb.pcmend),
            i64::from(vb.eofflag),
        ];
        if info != mine {
            return format!("ブロック {b}：BLCK {mine:?} と {info:?}");
        }
        let lw_block_mode = st.window.lw_block_mode;
        let a = mapping0_analyze(
            &vd,
            vb.lw,
            vb.w,
            vb.nw,
            vb.blocktype,
            lw_block_mode,
            vb.pcm.clone(),
        )
        .unwrap();
        let n = vb.pcmend as usize;
        for c in 0..ch {
            let rec = |t: &str| dump.get(b, t, c as u16).unwrap().f32_bits();
            for (t, v) in [
                ("LFFT", &a.spec[c][..n / 2]),
                ("LMDC", &a.spec[c][n / 2..]),
                ("AMPL", std::slice::from_ref(&a.local_ampmax[c])),
            ] {
                if let Some(at) = same(v, &rec(t)) {
                    return format!("ブロック {b} ch {c}：{t} の {at} 番目");
                }
            }
        }
        // 段 2（ampmax）：blockout が減衰させた値から
        let g = mapping0_ampmax(vb.ampmax, &a.local_ampmax);
        let ampg = dump.get(b, "AMPG", 0xFFFF).unwrap().f32_bits();
        if same(&[g], &ampg).is_some() {
            return format!("ブロック {b}：AMPG {g} と {}", f32::from_bits(ampg[0]));
        }
        // 段 2（M5 の lowcomp）
        let mut lowcomp = st.lowcomp.clone();
        mapping0_lowcomp(&vd, vb.blocktype, vb.w, lw_block_mode, &a, &mut lowcomp).unwrap();
        for c in 0..ch {
            let lcmp = dump.get(b, "LCMP", c as u16).unwrap().f32_bits();
            if same(&lowcomp[c..=c], &lcmp).is_some() {
                return format!(
                    "ブロック {b} ch {c}：LCMP {} と {}",
                    lowcomp[c],
                    f32::from_bits(lcmp[0])
                );
            }
        }
        let pkt = block::vorbis_analysis(&mut vb, &vd, &mut st).unwrap();
        if pkt.packet != want[3 + b].body {
            let same_as_got = got.packets.get(b).is_some_and(|p| p.packet == pkt.packet);
            return format!(
                "ブロック {b}：段 1・2 は一致、パケットが違う（並列の書き出しと直列の値は{}）。段 3 の中は psy・floor1・res0 の照合で",
                if same_as_got { "同じ" } else { "違う" }
            );
        }
        b += 1;
    }
    format!("直列ではどのブロックも一致（{b} ブロック）。並列の書き出しか Ogg の前の組み立てが違う")
}

/// 1 つのレートの照合：q ごとにチャンネルの数と信号を回す（すべての q・チャンネルの数・テンプレートに届く）。
fn check_rate(ri: usize) {
    let rate = RATES[ri];
    let mut stats = Stats::default();
    // 長さ：0.6 秒（96 kHz 以上は 0.3 秒）
    let frames = if rate >= 96_000 {
        rate as usize * 3 / 10
    } else {
        rate as usize * 6 / 10
    };
    for (qi, &q) in QS.iter().enumerate() {
        for (k, &ch) in CHANNELS.iter().enumerate() {
            // 信号：Music と Impulses は毎回どちらか、ほかの 3 種は q と ch で回す
            let kind = if (qi + k) % 2 == 0 {
                [Signal::Music, Signal::Impulses][(qi + k) / 2 % 2]
            } else {
                SIGNALS[1 + (qi + k + ri) % 3]
            };
            let seed = (ri * 100 + qi * 10 + k) as u64;
            if check_stream(kind, rate, ch, q, frames, seed, &mut stats).is_none() {
                return;
            }
        }
    }
    eprintln!(
        "照合 rate {rate}：{} 件・{} パケット（短 {}・長 {}）・{} バイトがビット一致。テンプレート {:?}",
        stats.cases,
        stats.packets,
        stats.windows[0],
        stats.windows[1],
        stats.bytes,
        stats.templates
    );
    assert_eq!(stats.cases, QS.len() * CHANNELS.len());
    assert_eq!(stats.templates.len(), 2, "stereo と uncoupled");
}

#[test]
fn oracle_stream_8000() {
    check_rate(0);
}
#[test]
fn oracle_stream_11025() {
    check_rate(1);
}
#[test]
fn oracle_stream_16000() {
    check_rate(2);
}
#[test]
fn oracle_stream_22050() {
    check_rate(3);
}
#[test]
fn oracle_stream_32000() {
    check_rate(4);
}
#[test]
fn oracle_stream_44100() {
    check_rate(5);
}
#[test]
fn oracle_stream_48000() {
    check_rate(6);
}
#[test]
fn oracle_stream_96000() {
    check_rate(7);
}
#[test]
fn oracle_stream_192000() {
    check_rate(8);
}

/// 端の長さ（0・1・32・33・64・65・1,000・blocksizes[1] とその前後）と数秒：外挿の条件・最後のブロック・グラニュール。
#[test]
fn oracle_stream_edge_lengths() {
    let mut stats = Stats::default();
    for (i, (rate, ch, q)) in [
        (44_100u32, 2usize, 5),
        (8_000, 1, -1),
        (48_000, 6, 0),
        (192_000, 8, 10),
        (22_050, 3, 3),
        (16_000, 2, 10),
        (32_000, 1, 0),
    ]
    .into_iter()
    .enumerate()
    {
        let Some(setup) = oracle::setup(rate, ch, q, false) else {
            return;
        };
        let bs1 = setup["blocksizes"][1].as_u64().unwrap() as usize;
        for (j, frames) in [0, 1, 32, 33, 64, 65, 1000, bs1 - 1, bs1, bs1 + 1]
            .into_iter()
            .enumerate()
        {
            let kind = SIGNALS[(i + j) % SIGNALS.len()];
            check_stream(kind, rate, ch, q, frames, (i * 20 + j) as u64, &mut stats).unwrap();
        }
        // 直流（0.999）
        let dc: Vec<Vec<f32>> = (0..ch).map(|_| vec![0.999_f32; 3000]).collect();
        check_input("直流", &dc, rate, q, &mut stats).unwrap();
    }
    eprintln!(
        "照合 端の長さ：{} 件・{} パケットがビット一致",
        stats.cases, stats.packets
    );
}

/// 数秒の入力（長い窓と短い窓が入れ替わり、ampmax の再帰が長く続く）。
#[test]
fn oracle_stream_seconds() {
    let mut stats = Stats::default();
    for (i, (kind, rate, ch, q, secs)) in [
        (Signal::Music, 44_100u32, 2usize, 5, 4usize),
        (Signal::Music, 48_000, 1, 10, 3),
        (Signal::Impulses, 22_050, 2, 3, 3),
        (Signal::Music, 32_000, 6, -1, 2),
        (Signal::Noise, 96_000, 2, 0, 2),
        (Signal::Music, 16_000, 3, 5, 4),
    ]
    .into_iter()
    .enumerate()
    {
        if check_stream(
            kind,
            rate,
            ch,
            q,
            rate as usize * secs + 17,
            900 + i as u64,
            &mut stats,
        )
        .is_none()
        {
            return;
        }
    }
    eprintln!(
        "照合 数秒：{} 件・{} パケット（短 {}・長 {}）がビット一致",
        stats.cases, stats.packets, stats.windows[0], stats.windows[1]
    );
}

/// NaN が途中に出る照合用の入力（README §8.3。`bark_noise_hybridmp` の 0/0 が量子化まで届く）。
#[test]
fn oracle_stream_nan_inputs() {
    let Some(root) = oracle::root() else {
        return;
    };
    let mut stats = Stats::default();
    for (name, rate, ch, q) in [
        ("synth_32000_1.f32", 32_000u32, 1usize, 0),
        ("synth_32000_2.f32", 32_000, 2, 0),
        ("synth_192000_1.f32", 192_000, 1, -1),
    ] {
        // 入力は crates/vorbis-tables/oracle/build.py が test/in/ に作る（なければ何もせずに通らないよう止まる）
        let path = root.join("test/in").join(name);
        let bytes = std::fs::read(&path)
            .unwrap_or_else(|e| panic!("{}：{e}（build.py で作る）", path.display()));
        let samples: Vec<f32> = bytes
            .as_chunks::<4>()
            .0
            .iter()
            .map(|b| f32::from_le_bytes(*b))
            .collect();
        let frames = samples.len() / ch;
        let input: Vec<Vec<f32>> = samples.chunks_exact(frames).map(<[f32]>::to_vec).collect();
        check_input(name, &input, rate, q, &mut stats).unwrap();
    }
    eprintln!("照合 NaN の入力：{} 件がビット一致", stats.cases);
}

/// 極端な振幅の雑音（Vorbis の並び）：`amp` × 一様な [−1, 1)。`sign_only` なら ±`amp` だけ（±f32::MAX の矩形のような雑音）。
fn loud_noise(amp: f32, sign_only: bool, ch: usize, frames: usize, seed: u64) -> Vec<Vec<f32>> {
    use rand_chacha::rand_core::Rng;
    (0..ch)
        .map(|c| {
            let mut r = crate::determinism::rng(seed.wrapping_mul(31).wrapping_add(c as u64));
            (0..frames)
                .map(|_| {
                    let u = r.next_u32();
                    if sign_only {
                        if u & 1 == 0 { amp } else { -amp }
                    } else {
                        ((f64::from(u) / 4_294_967_296.0 * 2.0 - 1.0) * f64::from(amp)) as f32
                    }
                })
                .collect()
        })
        .collect()
}

/// 極端な振幅（標本の絶対値 1e9・1e10・1e12・1e30・f32::MAX の雑音と ±f32::MAX、1 チャンネルだけ 1e20 倍の 2・3・6 チャンネル）。
/// residue の量子化が INT_MIN・INT_MAX に張り付き、C の `int` があふれる（res0.c の `_01class`・`_2class`・
/// `local_book_besterror`、psy.c の M6 の `out*out`・coupling）。照合用の C は `-fwrapv` で 2 の補数の折り返しと定めて組む
/// （oracle/README.md §2・§7.8。`-fwrapv` のない `-O2` はここで `-O0` と違うパケットを出し、この照合の最初の件で落ちる）。
/// 移植の wrapping の演算と一致する。浮動小数 → 整数の範囲の外の変換は、C も移植も飽和（aarch64 の C と Rust の `as`）。
#[test]
fn oracle_stream_extreme_amplitudes() {
    let mut stats = Stats::default();
    let cases: [(f32, bool, u32, usize, i32); 20] = [
        (1e10, false, 8_000, 1, -1),
        (1e10, false, 44_100, 2, 5),
        (1e10, false, 22_050, 1, 0),
        (1e12, false, 8_000, 1, -1),
        (1e12, false, 44_100, 1, 5),
        (1e12, false, 48_000, 2, 10),
        (1e12, false, 16_000, 2, 3),
        (1e30, false, 44_100, 2, 5),
        (1e30, false, 32_000, 1, 10),
        (1e30, false, 11_025, 2, -1),
        (1e30, false, 96_000, 1, 0),
        (f32::MAX, false, 44_100, 1, 5),
        (f32::MAX, false, 48_000, 2, 0),
        (f32::MAX, false, 8_000, 2, 10),
        (f32::MAX, true, 44_100, 2, 5),
        (f32::MAX, true, 22_050, 1, 3),
        (f32::MAX, true, 192_000, 2, -1),
        (f32::MAX, true, 16_000, 1, 10),
        (-f32::MAX, true, 32_000, 2, 0),
        (1e9, false, 44_100, 2, 3),
    ];
    for (i, &(amp, sign_only, rate, ch, q)) in cases.iter().enumerate() {
        let frames = rate as usize / 2 + 321;
        let input = loud_noise(amp, sign_only, ch, frames, 11_000 + i as u64);
        let label = format!("雑音 {amp:e}{}", if sign_only { "（±）" } else { "" });
        if check_input(&label, &input, rate, q, &mut stats).is_none() {
            return;
        }
    }
    // 1 チャンネルだけ 1e20 倍（ほかはふつうの大きさ）の 3・6 チャンネル（uncoupled）と、片方だけ 1e20 倍のステレオ（coupling）
    for (i, (rate, ch, q, loud)) in [
        (44_100u32, 3usize, 5, 1usize),
        (48_000, 6, 0, 4),
        (44_100, 2, 3, 0),
        (22_050, 2, 10, 1),
    ]
    .into_iter()
    .enumerate()
    {
        let mut input = oracle::signal(
            Signal::Music,
            rate,
            ch,
            rate as usize / 2,
            12_000 + i as u64,
        );
        for s in &mut input[loud] {
            *s *= 1e20;
        }
        if check_input("1 チャンネルだけ 1e20 倍", &input, rate, q, &mut stats).is_none() {
            return;
        }
    }
    assert_eq!(stats.cases, cases.len() + 4);
    assert_eq!(stats.channels.len(), 4, "{:?}", stats.channels);
    eprintln!(
        "照合 極端な振幅：{} 件・{} パケット・{} バイトがビット一致",
        stats.cases, stats.packets, stats.bytes
    );
}

/// レート × チャンネル × q のすべての組（9 × 5 × 5 = 225 通り）を、短い入力（0.25 秒。信号は組ごとに回す）で。
#[test]
fn oracle_stream_matrix() {
    let mut stats = Stats::default();
    for (ri, &rate) in RATES.iter().enumerate() {
        for (qi, &q) in QS.iter().enumerate() {
            for (k, &ch) in CHANNELS.iter().enumerate() {
                let kind = SIGNALS[(ri + qi + k) % SIGNALS.len()];
                let seed = 5_000 + (ri * 25 + qi * 5 + k) as u64;
                if check_stream(kind, rate, ch, q, rate as usize / 4, seed, &mut stats).is_none() {
                    return;
                }
            }
        }
    }
    assert_eq!(stats.cases, 225);
    // SETUP_LIST の 18 個のうち、XX（8 kHz 未満）は範囲の外なので 16 個（stereo・uncoupled × 8 種のレート）
    assert_eq!(stats.templates.len(), 16, "{:?}", stats.templates);
    eprintln!(
        "照合 全組：{} 件・{} パケット（短 {}・長 {}）・{} バイトがビット一致（テンプレート {} 個）",
        stats.cases,
        stats.packets,
        stats.windows[0],
        stats.windows[1],
        stats.bytes,
        stats.templates.len()
    );
}

/// 4・5・7 チャンネル（uncoupled）と、テンプレートの境のレート（88.2 kHz など）。
#[test]
fn oracle_stream_other_channels_and_rates() {
    let mut stats = Stats::default();
    for (i, (rate, ch, q)) in [
        (44_100u32, 4usize, 5),
        (44_100, 5, 3),
        (88_200, 7, 0),
        (12_000, 4, 10),
        (24_000, 5, -1),
        (64_000, 7, 5),
        (176_400, 2, 3),
        (50_000, 2, 0),
    ]
    .into_iter()
    .enumerate()
    {
        let kind = SIGNALS[i % SIGNALS.len()];
        if check_stream(
            kind,
            rate,
            ch,
            q,
            rate as usize / 2,
            7_000 + i as u64,
            &mut stats,
        )
        .is_none()
        {
            return;
        }
    }
    eprintln!(
        "照合 4・5・7 チャンネルとほかのレート：{} 件・{} パケットがビット一致（テンプレート {:?}）",
        stats.cases, stats.packets, stats.templates
    );
}

/// aoTuV の M2・M3・M5・M7・M8・M9 がよく働く入力（衝撃と、すぐ止まる音の繰り返し。`Signal::Bursts`）を、26 kHz 以上の
/// レート × q × 1・2・6 チャンネルで（M2 の後エコーの補正は q0 で、M3 の衝撃の続きは impulse のブロックで届く）。
#[test]
fn oracle_stream_aotuv_bursts() {
    let mut stats = Stats::default();
    for (ri, &rate) in [32_000u32, 44_100, 48_000, 96_000, 192_000]
        .iter()
        .enumerate()
    {
        for (qi, &q) in [-1, 0, 3, 5, 10].iter().enumerate() {
            for (k, &ch) in [1usize, 2, 6].iter().enumerate() {
                let frames = if rate >= 96_000 {
                    rate as usize / 2
                } else {
                    rate as usize
                };
                let seed = 8_000 + (ri * 100 + qi * 10 + k) as u64;
                if check_stream(Signal::Bursts, rate, ch, q, frames, seed, &mut stats).is_none() {
                    return;
                }
            }
        }
    }
    assert_eq!(stats.cases, 75);
    eprintln!(
        "照合 aoTuV の衝撃の入力：{} 件・{} パケット（短 {}・長 {}）・{} バイトがビット一致",
        stats.cases, stats.packets, stats.windows[0], stats.windows[1], stats.bytes
    );
}

/// 並列の書き出しの区間の大きさ（1・5・既定）とスレッド数（1・3・16）を変えても、照合用の C とビット一致する
/// （Ogg のバイト列もスレッド数・区間で変わらない）。
#[test]
fn oracle_stream_segments_and_threads() {
    let mut cases = 0;
    for (kind, rate, ch, q, frames) in [
        (Signal::Music, 44_100u32, 2usize, 5, 88_200usize),
        (Signal::Impulses, 48_000, 6, 0, 48_000),
        (Signal::Music, 22_050, 3, 10, 44_100),
        // aoTuV の M2・M3（impulse の続きの tempmdct が区間をまたぐ）
        (Signal::Bursts, 44_100, 2, 0, 88_200),
        (Signal::Bursts, 32_000, 1, -1, 64_000),
    ] {
        let input = oracle::signal(kind, rate, ch, frames, 77);
        let Some((want, _)) = oracle::encode(&input, rate, q, None) else {
            return;
        };
        let audio = kura_audio(rate, &input);
        let mut oggs = Vec::new();
        for threads in [1usize, 3, 16] {
            let pool = rayon::ThreadPoolBuilder::new()
                .num_threads(threads)
                .build()
                .unwrap();
            for segment in [Some(1), Some(5), None] {
                let got = pool
                    .install(|| {
                        encoder::encode_packets_with(&audio, i64::from(q), &NoMonitor, segment)
                    })
                    .unwrap();
                if let Some(msg) = first_difference(&got, &want) {
                    panic!(
                        "{kind:?}・{rate} Hz・{ch} ch・q{q}：{threads} スレッド・区間 {segment:?}：{msg}"
                    );
                }
                cases += 1;
            }
            oggs.push(pool.install(|| encoder::encode(&audio, i64::from(q), &NoMonitor).unwrap()));
        }
        assert!(oggs.windows(2).all(|w| w[0] == w[1]));
    }
    eprintln!("照合 区間とスレッド数：{cases} 通りがビット一致");
}

/// 速さ（release で手で回す）：3 分の 44.1 kHz ステレオ（`oracle::signal` の Music）を、既定のスレッド数と 1 スレッドで書く。
/// `cargo test --release -p kura-engine --lib vorbis_bench -- --ignored --nocapture`
#[test]
#[ignore]
fn vorbis_bench_three_minutes_stereo() {
    #![allow(clippy::disallowed_methods)]
    let rate = 44_100u32;
    let input = oracle::signal(Signal::Music, rate, 2, rate as usize * 180, 1);
    let audio = kura_audio(rate, &input);
    for q in [5i64, 0, 10] {
        let mut sizes = Vec::new();
        for threads in [0usize, 1] {
            let pool = rayon::ThreadPoolBuilder::new()
                .num_threads(threads)
                .build()
                .unwrap();
            let n = pool.current_num_threads();
            let t = std::time::Instant::now();
            let bytes = pool.install(|| encoder::encode(&audio, q, &NoMonitor).unwrap());
            let secs = t.elapsed().as_secs_f64();
            eprintln!(
                "q{q}・{n} スレッド：{secs:.3} 秒（{} バイト、{:.1} kbps）",
                bytes.len(),
                bytes.len() as f64 * 8.0 / 180.0 / 1000.0
            );
            sizes.push(bytes);
        }
        assert!(sizes.windows(2).all(|w| w[0] == w[1]));
    }
}
