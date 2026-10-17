//! 試験の音：記述から決定的に作る合成の音（乱数は ChaCha8。`kura_engine::determinism::rng_stream`）。
//!
//! 作った値は 16 ビットに丸めたもの（`round(x · 32768) / 32768`）を正とする。同じ値を 16 ビットの WAV に書くので、
//! 外のエンコーダ（libvorbis など）で同じ WAV から作った参照と、同じ元で比べられる。

use libm::{cos, exp, log, pow, sin, sqrt};
use rand_chacha::ChaCha8Rng;
use rand_chacha::rand_core::Rng;

const PI: f64 = std::f64::consts::PI;

/// 音の種類。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// 正弦 1 kHz（−6 dBFS）。
    Sine1k,
    /// 持続する和音（C・E・G・C の 4 音、倍音 6 本。右は 0.1% 高く）。
    Chord,
    /// グロッケンのような打鍵（高い倍音、0.25 秒ごと、左右に振る）。
    Bells,
    /// 指数の掃引（左は 20 Hz → 20 kHz、右は逆向き）。
    Sweep,
    /// ピンクに近い雑音（Paul Kellet のフィルタ。左右は独立）。
    Pink,
    /// 拍のような衝撃（0.25 秒ごとに 4 ms の雑音の破裂）。
    Pulses,
    /// 音楽に近いもの（キック・ベース・和音・ハイハットの 120 BPM のループ）。
    Music,
    /// 短い効果音：クリック（2 ms の破裂と 3 kHz の響き）。
    SfxClick,
    /// 短い効果音：ブリップ（矩形に近い波、880 → 1,320 Hz）。
    SfxBlip,
    /// 短い効果音：コイン（B5 → E6 の 2 音）。
    SfxCoin,
    /// 短い効果音：打撃（120 → 50 Hz の低い音と雑音）。
    SfxHit,
    /// 短い効果音：ザップ（4 kHz → 200 Hz の下降と振幅の揺れ）。
    SfxZap,
    /// 短い効果音：風切り（雑音の帯域が動き、左右に流れる）。
    SfxWhoosh,
}

impl Kind {
    /// 記述（表の頭書きに書く）。
    pub fn describe(self) -> &'static str {
        match self {
            Kind::Sine1k => "正弦 1 kHz、−6 dBFS",
            Kind::Chord => {
                "持続する和音（C4・E4・G4・C5、倍音 6 本、右は 0.1% 高い）、ピーク −4.4 dBFS"
            }
            Kind::Bells => {
                "グロッケンのような打鍵（0.25 秒ごと、倍音 1・3・5・7・9、不協和 0.0004）、ピーク −6 dBFS"
            }
            Kind::Sweep => "指数の掃引（左 20 Hz→20 kHz、右は逆）、−6 dBFS",
            Kind::Pink => {
                "ピンクに近い雑音（白色のガウス雑音に Kellet のフィルタ、左右独立）、ピーク −6 dBFS"
            }
            Kind::Pulses => {
                "0.25 秒ごとに 4 ms の白色雑音の破裂（3.5 ms で消える）、ピーク −3 dBFS"
            }
            Kind::Music => {
                "キック・ベース・和音（倍音 10 本）・ハイハット（雑音の微分）の 120 BPM のループ、ピーク −3 dBFS"
            }
            Kind::SfxClick => "クリック：2 ms の雑音の破裂と 3 kHz の響き（τ 8 ms）、0 標本目から",
            Kind::SfxBlip => {
                "ブリップ：奇数倍音の矩形に近い波 880 Hz → 1,320 Hz（60 ms で切り替え）、線形に消える"
            }
            Kind::SfxCoin => "コイン：B5（80 ms）→ E6（減衰）、倍音 3 本",
            Kind::SfxHit => {
                "打撃：120 → 50 Hz に下がる低い音（τ 60 ms）と雑音（τ 20 ms）、0 標本目から"
            }
            Kind::SfxZap => "ザップ：4 kHz → 200 Hz の指数の下降、30 Hz の振幅の揺れ、減衰",
            Kind::SfxWhoosh => {
                "風切り：雑音の 1 次の帯域通過（中心 300 Hz → 6 kHz）、Hann の包絡、左右に流れる"
            }
        }
    }

    /// 乱数の種に混ぜる名前（レート・チャンネルによらない）。
    fn key(self) -> &'static str {
        match self {
            Kind::Sine1k => "sine1k",
            Kind::Chord => "chord",
            Kind::Bells => "bells",
            Kind::Sweep => "sweep",
            Kind::Pink => "pink",
            Kind::Pulses => "pulses",
            Kind::Music => "music",
            Kind::SfxClick => "sfx-click",
            Kind::SfxBlip => "sfx-blip",
            Kind::SfxCoin => "sfx-coin",
            Kind::SfxHit => "sfx-hit",
            Kind::SfxZap => "sfx-zap",
            Kind::SfxWhoosh => "sfx-whoosh",
        }
    }
}

/// 合成の音の記述。
#[derive(Debug, Clone, PartialEq)]
pub struct Spec {
    pub name: String,
    pub kind: Kind,
    pub rate: u32,
    pub channels: u16,
    pub seconds: f64,
}

fn spec(kind: Kind, rate: u32, channels: u16, seconds: f64) -> Spec {
    let r = if rate == 44_100 { "44k" } else { "48k" };
    let c = if channels == 1 { "mono" } else { "st" };
    Spec {
        name: format!("{}-{r}-{c}", kind.key()),
        kind,
        rate,
        channels,
        seconds,
    }
}

/// 既定の試験の音の組（主な 7 つは 48 kHz ステレオ 4 秒、音楽は 48・44.1 kHz のステレオ・モノ、短い効果音 6 つ）。
pub fn default_set() -> Vec<Spec> {
    let mut v = vec![
        spec(Kind::Sine1k, 48_000, 2, 4.0),
        spec(Kind::Chord, 48_000, 2, 4.0),
        spec(Kind::Bells, 48_000, 2, 4.0),
        spec(Kind::Sweep, 48_000, 2, 4.0),
        spec(Kind::Pink, 48_000, 2, 4.0),
        spec(Kind::Pulses, 48_000, 2, 4.0),
        spec(Kind::Music, 48_000, 2, 4.0),
        spec(Kind::Music, 48_000, 1, 4.0),
        spec(Kind::Music, 44_100, 2, 4.0),
        spec(Kind::Music, 44_100, 1, 4.0),
        spec(Kind::Sine1k, 44_100, 1, 4.0),
        spec(Kind::Sweep, 44_100, 2, 4.0),
    ];
    v.extend([
        spec(Kind::SfxClick, 48_000, 1, 0.05),
        spec(Kind::SfxBlip, 44_100, 1, 0.12),
        spec(Kind::SfxCoin, 44_100, 2, 0.35),
        spec(Kind::SfxHit, 48_000, 2, 0.3),
        spec(Kind::SfxZap, 48_000, 1, 0.5),
        spec(Kind::SfxWhoosh, 48_000, 2, 0.8),
    ]);
    v
}

/// 乱数の種（全体の種と種類の名前から。レート・チャンネルによらない）。
fn seed_of(seed: u64, kind: Kind) -> u64 {
    use kura_engine::determinism::{fnv1a64, splitmix64};
    splitmix64(seed ^ fnv1a64(kind.key().as_bytes()))
}

/// −1〜1 の一様な乱数。
fn uniform(rng: &mut ChaCha8Rng) -> f64 {
    rng.next_u32() as f64 / 2_147_483_648.0 - 1.0
}

/// 標準正規分布の乱数（Box–Muller。2 語を使い、1 つだけ返す）。
fn gauss(rng: &mut ChaCha8Rng) -> f64 {
    let u1 = (f64::from(rng.next_u32()) + 1.0) / 4_294_967_296.0;
    let u2 = f64::from(rng.next_u32()) / 4_294_967_296.0;
    sqrt(-2.0 * log(u1)) * cos(2.0 * PI * u2)
}

/// 減衰する倍音の音（gen.py の `note` と同じ形：2 ms の立ち上がり、倍音 k は exp(−t·decay·(1 + 0.3k)) で減る）。
fn note(f0: f64, len: usize, fs: f64, partials: &[f64], decay: f64, inharm: f64) -> Vec<f64> {
    // （倍音の周波数, 振幅, 減衰の速さ）。20 kHz・ナイキストを超える倍音から先は使わない、振幅 0 は飛ばす
    let mut parts = Vec::new();
    for (k, &a) in partials.iter().enumerate() {
        let k = (k + 1) as f64;
        let fk = f0 * k * sqrt(1.0 + inharm * k * k);
        if fk > 20_000.0 || fk >= fs / 2.0 {
            break;
        }
        if a != 0.0 {
            parts.push((fk, a, decay * (1.0 + 0.3 * k)));
        }
    }
    (0..len)
        .map(|i| {
            let t = i as f64 / fs;
            let mut y = 0.0;
            for &(fk, a, d) in &parts {
                y += a * sin(2.0 * PI * fk * t) * exp(-t * d);
            }
            y * (t / 0.002).min(1.0)
        })
        .collect()
}

/// `buf` の `at` 秒の位置に、ゲイン（チャンネルごと）を掛けて足す。
fn place(buf: &mut [Vec<f64>], x: &[f64], at: f64, fs: f64, gains: &[f64]) {
    let start = (at * fs) as usize;
    for (c, ch) in buf.iter_mut().enumerate() {
        let g = gains.get(c).copied().unwrap_or(gains[0]);
        for (i, v) in x.iter().enumerate() {
            if let Some(d) = ch.get_mut(start + i) {
                *d += g * v;
            }
        }
    }
}

/// ピークを `peak` にそろえる。
fn normalize(buf: &mut [Vec<f64>], peak: f64) {
    let m = buf.iter().flatten().fold(0.0f64, |m, v| m.max(v.abs()));
    if m > 0.0 {
        for v in buf.iter_mut().flatten() {
            *v *= peak / m;
        }
    }
}

/// 0.5 ms の線形の消え（終わりの不連続を避ける）。
fn fade_out(buf: &mut [Vec<f64>], fs: f64) {
    let n = (0.0005 * fs) as usize;
    for ch in buf.iter_mut() {
        let len = ch.len();
        for i in 0..n.min(len) {
            ch[len - 1 - i] *= i as f64 / n as f64;
        }
    }
}

/// 作る（チャンネルごとの f64。まだ 16 ビットに丸めていない）。
fn synth(s: &Spec, seed: u64) -> Vec<Vec<f64>> {
    let fs = f64::from(s.rate);
    let len = (s.seconds * fs).round() as usize;
    let nch = usize::from(s.channels);
    let seed = seed_of(seed, s.kind);
    let mut buf = vec![vec![0.0; len]; nch];
    let t = |i: usize| i as f64 / fs;
    // チャンネル c の振り分け（モノは左右の平均）
    let pan = |l: f64, r: f64| -> Vec<f64> {
        if nch == 1 {
            vec![(l + r) / 2.0]
        } else {
            vec![l, r]
        }
    };
    match s.kind {
        Kind::Sine1k => {
            for ch in buf.iter_mut() {
                for (i, v) in ch.iter_mut().enumerate() {
                    *v = 0.5 * sin(2.0 * PI * 1000.0 * t(i));
                }
            }
        }
        Kind::Chord => {
            let notes = [261.63, 329.63, 392.0, 523.25];
            let partials = [1.0, 0.5, 0.33, 0.25, 0.2, 0.16];
            for (c, ch) in buf.iter_mut().enumerate() {
                let detune = 1.0 + 0.001 * c as f64;
                for (i, v) in ch.iter_mut().enumerate() {
                    let mut y = 0.0;
                    for f in notes {
                        for (k, a) in partials.iter().enumerate() {
                            y += a * sin(2.0 * PI * f * detune * (k + 1) as f64 * t(i));
                        }
                    }
                    let env = (t(i) / 0.005).min(1.0) * ((s.seconds - t(i)) / 0.005).min(1.0);
                    *v = y * env;
                }
            }
            normalize(&mut buf, 0.6);
        }
        Kind::Bells => {
            let scale = [
                523.25, 587.33, 659.26, 783.99, 880.0, 1046.5, 1174.7, 1318.5,
            ];
            let count = (s.seconds / 0.25) as usize;
            for i in 0..count {
                let f = scale[(i * 5) % scale.len()];
                let x = note(
                    f,
                    (1.5 * fs) as usize,
                    fs,
                    &[1.0, 0.0, 0.6, 0.0, 0.3, 0.0, 0.15, 0.0, 0.1],
                    2.5,
                    0.0004,
                );
                let p = 0.3 * sin(i as f64);
                place(&mut buf, &x, i as f64 * 0.25, fs, &pan(0.6 + p, 0.6 - p));
            }
            fade_out(&mut buf, fs);
            normalize(&mut buf, 0.5);
        }
        Kind::Sweep => {
            let (f0, f1) = (20.0, 20_000.0);
            let k = log(f1 / f0);
            for (c, ch) in buf.iter_mut().enumerate() {
                for (i, v) in ch.iter_mut().enumerate() {
                    let x = t(i) / s.seconds;
                    // 位相 = 2π f0 T / ln(f1/f0) · (e^{x ln(f1/f0)} − 1)。右は向きを逆に（f1 から f0 へ）
                    let ph = if c == 0 {
                        2.0 * PI * f0 * s.seconds / k * (exp(x * k) - 1.0)
                    } else {
                        2.0 * PI * f1 * s.seconds / -k * (exp(-x * k) - 1.0)
                    };
                    *v = 0.5 * sin(ph);
                }
            }
            fade_out(&mut buf, fs);
        }
        Kind::Pink => {
            for (c, ch) in buf.iter_mut().enumerate() {
                let mut rng = kura_engine::determinism::rng_stream(seed, c as u64);
                let mut b = [0.0f64; 7];
                for v in ch.iter_mut() {
                    let w = gauss(&mut rng);
                    b[0] = 0.99886 * b[0] + w * 0.0555179;
                    b[1] = 0.99332 * b[1] + w * 0.0750759;
                    b[2] = 0.96900 * b[2] + w * 0.1538520;
                    b[3] = 0.86650 * b[3] + w * 0.3104856;
                    b[4] = 0.55000 * b[4] + w * 0.5329522;
                    b[5] = -0.7616 * b[5] - w * 0.0168980;
                    *v = b[0] + b[1] + b[2] + b[3] + b[4] + b[5] + b[6] + w * 0.5362;
                    b[6] = w * 0.115926;
                }
            }
            normalize(&mut buf, 0.5);
        }
        Kind::Pulses => {
            let period = (0.25 * fs) as usize;
            let burst = (0.004 * fs) as usize;
            let fade = (0.0035 * fs) as usize;
            for (c, ch) in buf.iter_mut().enumerate() {
                let mut rng = kura_engine::determinism::rng_stream(seed, c as u64);
                for (i, v) in ch.iter_mut().enumerate() {
                    let j = i % period;
                    if j < burst {
                        let g = if j < burst - fade {
                            1.0
                        } else {
                            (burst - j) as f64 / fade as f64
                        };
                        *v = uniform(&mut rng) * g;
                    }
                }
            }
            normalize(&mut buf, 0.7);
        }
        Kind::Music => {
            let beat = 0.5;
            let chords = [
                [261.63, 329.63, 392.0],
                [220.0, 261.63, 329.63],
                [174.61, 220.0, 261.63],
                [196.0, 246.94, 293.66],
            ];
            let mut rng = kura_engine::determinism::rng_stream(seed, 0);
            let bars = (s.seconds / (4.0 * beat)).ceil() as usize;
            for bar in 0..bars {
                let chord = chords[bar % chords.len()];
                let at = bar as f64 * 4.0 * beat;
                for f in chord {
                    let x = note(
                        f,
                        (4.0 * beat * fs) as usize,
                        fs,
                        &[1.0, 0.5, 0.33, 0.25, 0.2, 0.16, 0.12, 0.1, 0.08, 0.06],
                        0.6,
                        0.0,
                    );
                    place(&mut buf, &x, at, fs, &pan(0.35, 0.25));
                    let x = note(
                        f * 2.003,
                        (4.0 * beat * fs) as usize,
                        fs,
                        &[1.0, 0.3, 0.1],
                        0.8,
                        0.0,
                    );
                    place(&mut buf, &x, at + 0.01, fs, &pan(0.15, 0.25));
                }
                for k in 0..4 {
                    let n = (0.4 * fs) as usize;
                    let kick: Vec<f64> = (0..n)
                        .map(|i| {
                            let t = i as f64 / fs;
                            sin(2.0 * PI * (50.0 * t + 60.0 * (1.0 - exp(-t * 30.0)) / 30.0))
                                * exp(-t * 8.0)
                        })
                        .collect();
                    let kt = at + k as f64 * beat;
                    place(&mut buf, &kick, kt, fs, &pan(0.8, 0.8));
                    let bass = note(
                        chord[0] / 4.0,
                        (beat * fs) as usize,
                        fs,
                        &[1.0, 0.6, 0.3, 0.15],
                        1.5,
                        0.0,
                    );
                    place(&mut buf, &bass, kt, fs, &pan(0.5, 0.5));
                }
                for k in 0..8 {
                    let n = (0.06 * fs) as usize;
                    let mut prev = 0.0;
                    let hh: Vec<f64> = (0..n)
                        .map(|i| {
                            let x = gauss(&mut rng) * exp(-(i as f64 / fs) * 60.0);
                            let d = x - prev;
                            prev = x;
                            d
                        })
                        .collect();
                    place(
                        &mut buf,
                        &hh,
                        at + k as f64 * beat / 2.0 + beat / 4.0,
                        fs,
                        &pan(0.15, 0.25),
                    );
                }
            }
            fade_out(&mut buf, fs);
            normalize(&mut buf, 0.7);
        }
        Kind::SfxClick => {
            let mut rng = kura_engine::determinism::rng_stream(seed, 0);
            let burst = (0.002 * fs) as usize;
            let x: Vec<f64> = (0..len)
                .map(|i| {
                    let n = if i < burst {
                        uniform(&mut rng) * (1.0 - i as f64 / burst as f64)
                    } else {
                        0.0
                    };
                    n + 0.6 * sin(2.0 * PI * 3000.0 * t(i)) * exp(-t(i) / 0.008)
                })
                .collect();
            place(&mut buf, &x, 0.0, fs, &[1.0]);
            normalize(&mut buf, 0.8);
        }
        Kind::SfxBlip => {
            let x: Vec<f64> = (0..len)
                .map(|i| {
                    let f = if t(i) < 0.06 { 880.0 } else { 1320.0 };
                    let mut y = 0.0;
                    let mut k = 1.0;
                    while f * k < 15_000.0 {
                        y += sin(2.0 * PI * f * k * t(i)) / k;
                        k += 2.0;
                    }
                    y * (1.0 - i as f64 / len as f64)
                })
                .collect();
            place(&mut buf, &x, 0.0, fs, &[1.0]);
            normalize(&mut buf, 0.6);
        }
        Kind::SfxCoin => {
            let first = (0.08 * fs) as usize;
            let a = note(987.77, first, fs, &[1.0, 0.4, 0.2], 3.0, 0.0);
            let b = note(
                1318.51,
                len.saturating_sub(first),
                fs,
                &[1.0, 0.4, 0.2],
                6.0,
                0.0,
            );
            place(&mut buf, &a, 0.0, fs, &pan(0.9, 0.6));
            place(&mut buf, &b, first as f64 / fs, fs, &pan(0.6, 0.9));
            fade_out(&mut buf, fs);
            normalize(&mut buf, 0.6);
        }
        Kind::SfxHit => {
            for (c, ch) in buf.iter_mut().enumerate() {
                let mut rng = kura_engine::determinism::rng_stream(seed, c as u64);
                for (i, v) in ch.iter_mut().enumerate() {
                    let tt = t(i);
                    let thump = sin(2.0 * PI * (50.0 * tt + 70.0 * (1.0 - exp(-tt * 25.0)) / 25.0))
                        * exp(-tt / 0.06);
                    *v = thump + 0.7 * uniform(&mut rng) * exp(-tt / 0.02);
                }
            }
            fade_out(&mut buf, fs);
            normalize(&mut buf, 0.9);
        }
        Kind::SfxZap => {
            let (f0, f1) = (4000.0, 200.0);
            let k = log(f1 / f0);
            let x: Vec<f64> = (0..len)
                .map(|i| {
                    let x = t(i) / s.seconds;
                    let ph = 2.0 * PI * f0 * s.seconds / k * (exp(x * k) - 1.0);
                    sin(ph) * (0.6 + 0.4 * sin(2.0 * PI * 30.0 * t(i))) * exp(-t(i) * 4.0)
                })
                .collect();
            place(&mut buf, &x, 0.0, fs, &[1.0]);
            fade_out(&mut buf, fs);
            normalize(&mut buf, 0.7);
        }
        Kind::SfxWhoosh => {
            let mut rng = kura_engine::determinism::rng_stream(seed, 0);
            let (mut lp, mut hp_in, mut hp) = (0.0, 0.0, 0.0);
            let x: Vec<f64> = (0..len)
                .map(|i| {
                    let p = i as f64 / len as f64;
                    let fc = 300.0 * pow(20.0, p);
                    // 1 次の低域通過（fc の 2 倍）のあと 1 次の高域通過（fc の半分）
                    let a = exp(-2.0 * PI * 2.0 * fc / fs);
                    let b = exp(-2.0 * PI * 0.5 * fc / fs);
                    lp = (1.0 - a) * gauss(&mut rng) + a * lp;
                    hp = b * (hp + lp - hp_in);
                    hp_in = lp;
                    hp * 0.5 * (1.0 - cos(2.0 * PI * p))
                })
                .collect();
            for (c, ch) in buf.iter_mut().enumerate() {
                for (i, v) in ch.iter_mut().enumerate() {
                    let p = i as f64 / len as f64;
                    let g = if nch == 1 {
                        1.0
                    } else if c == 0 {
                        1.0 - p
                    } else {
                        p
                    };
                    *v = x[i] * (0.3 + 0.7 * g);
                }
            }
            normalize(&mut buf, 0.7);
        }
    }
    buf
}

/// 作って 16 ビットに丸める（`round(x · 32768)` を −32,768〜32,767 に抑えたもの）。チャンネルごとの整数。
pub fn generate(s: &Spec, seed: u64) -> Vec<Vec<i16>> {
    synth(s, seed)
        .into_iter()
        .map(|ch| {
            ch.into_iter()
                .map(|v| (v * 32768.0).round().clamp(-32768.0, 32767.0) as i16)
                .collect()
        })
        .collect()
}

/// 16 ビットの PCM の WAV（RIFF・`fmt `・`data` だけ）。
pub fn wav16(rate: u32, planes: &[Vec<i16>]) -> Vec<u8> {
    let ch = planes.len() as u16;
    let frames = planes.first().map_or(0, Vec::len);
    let data_len = (frames * planes.len() * 2) as u32;
    let mut out = Vec::with_capacity(44 + data_len as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&ch.to_le_bytes());
    out.extend_from_slice(&rate.to_le_bytes());
    out.extend_from_slice(&(rate * u32::from(ch) * 2).to_le_bytes());
    out.extend_from_slice(&(ch * 2).to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for i in 0..frames {
        for p in planes {
            out.extend_from_slice(&p[i].to_le_bytes());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 同じ記述と種なら同じ値。種を変えると雑音の入る音だけ変わる。長さ・チャンネル・ピークが記述どおり。
    #[test]
    fn deterministic_and_as_described() {
        // デバッグのビルドで速く回すため、長い音は 0.5 秒に縮めて確かめる（作り方は長さによらない）
        for s in default_set() {
            let s = Spec {
                seconds: s.seconds.min(0.5),
                ..s
            };
            let a = generate(&s, 1);
            assert_eq!(a.len(), usize::from(s.channels), "{}", s.name);
            let len = (s.seconds * f64::from(s.rate)).round() as usize;
            assert!(a.iter().all(|c| c.len() == len), "{}", s.name);
            let peak = a
                .iter()
                .flatten()
                .map(|v| i32::from(*v).abs())
                .max()
                .unwrap_or(0);
            assert!(peak > 8_000 && peak < 32_767, "{}：ピーク {peak}", s.name);
            assert_eq!(a, generate(&s, 1), "{}", s.name);
            let noisy = matches!(
                s.kind,
                Kind::Pink
                    | Kind::Pulses
                    | Kind::Music
                    | Kind::SfxClick
                    | Kind::SfxHit
                    | Kind::SfxWhoosh
            );
            assert_eq!(a != generate(&s, 2), noisy, "{}", s.name);
            // 短い効果音は 0 標本目から鳴る（最初の 1 ms に音がある。風切りは Hann の包絡で立ち上がる）
            if s.seconds < 1.0 && s.kind != Kind::SfxWhoosh {
                let ms = (s.rate / 1000) as usize;
                assert!(a[0][..ms].iter().any(|v| v.abs() > 100), "{}", s.name);
            }
        }
        // 名前は重ならない
        let names: std::collections::BTreeSet<String> =
            default_set().into_iter().map(|s| s.name).collect();
        assert_eq!(names.len(), default_set().len());
    }

    /// 正弦 1 kHz は −6 dBFS（振幅 0.5）。
    #[test]
    fn sine_is_half_scale() {
        let s = spec(Kind::Sine1k, 48_000, 1, 0.01);
        let a = generate(&s, 0);
        assert_eq!(a[0][12], 16_384); // sin(2π·1000·12/48000) = sin(π/2) = 1
        assert_eq!(a[0][0], 0);
    }

    /// WAV の頭と中身。
    #[test]
    fn wav_layout() {
        let w = wav16(48_000, &[vec![1, -2], vec![3, 4]]);
        assert_eq!(w.len(), 44 + 8);
        assert_eq!(&w[0..4], b"RIFF");
        assert_eq!(&w[36..40], b"data");
        assert_eq!(&w[44..], &[1, 0, 3, 0, 0xfe, 0xff, 4, 0]);
    }
}
