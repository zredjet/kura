//! AAC-LC の読み取り（自作。`m4a-aac-kura-dec/1`・`aac-adts-kura-dec/1`。M16 段 4・ADR-216・ADR-220）。
//!
//! ISO/IEC 14496-3 の AAC-LC（object type 2・1,024 フレーム）を、仕様から自作した読み手で読む。表は `aac-tables`
//! （symphonia の符号表と帯の区切りを値を変えずに写したもの）。配置（channel_configuration）1〜7（モノラル〜7.1）を読み、
//! kura の並び（WAV のチャンネルの印の順：FL FR FC LFE BL BR FLC FRC）に置く。SBR・PS（HE-AAC）・CCE・PCE で決める配置・
//! AAC-LC でない object type・960 フレームは `Unsupported`（次の段へ）。壊れたものは `Decode`。
//!
//! | ファイル | 中身 |
//! |---|---|
//! | [`config`] | AudioSpecificConfig・ADTS の頭から読む形 |
//! | [`bits`] | MSB から読むビットの読み手 |
//! | [`huffman`] | ハフマンの引き表（2 段） |
//! | [`decoder`] | raw_data_block・ICS・逆量子化・PNS・M/S・intensity・TNS |
//! | [`filterbank`] | 窓・逆 MDCT・重ね合わせ |
//! | [`adts`] | ADTS の読み口 |
//! | [`read`] | デコーダの口（形・切り取り・HE-AAC の印・m4a と ADTS） |

pub(crate) mod adts;
pub(crate) mod bits;
pub(crate) mod config;
pub(crate) mod decoder;
pub(crate) mod filterbank;
pub(crate) mod huffman;
pub mod read;

use crate::error::EngineError;

/// MP4 の中の AAC-LC の読み取りの約束の版。
pub(crate) const M4A_ID: &str = "m4a-aac-kura-dec/1";
/// ADTS の AAC-LC の読み取りの約束の版。
pub(crate) const ADTS_ID: &str = "aac-adts-kura-dec/1";

/// 壊れたストリームの誤り（どちらの入れ物でも同じ文言。識別子は呼び手が付け直す）。
pub(crate) fn decode_err(message: impl Into<String>) -> EngineError {
    EngineError::Decode {
        codec: M4A_ID.to_string(),
        message: format!("AAC：{}", message.into()),
    }
}

/// 次の段へ回す形。
pub(crate) fn unsupported(message: &str) -> EngineError {
    EngineError::Unsupported(format!("AAC：{message}"))
}

#[cfg(test)]
mod tests {
    use symphonia_core::audio::{Audio as _, GenericAudioBufferRef};
    use symphonia_core::codecs::audio::AudioDecoder as _;

    use super::*;
    use crate::codec::symph::{self, Container};
    use crate::limits::Limits;
    use crate::monitor::NoMonitor;

    fn fixture(name: &str) -> Vec<u8> {
        std::fs::read(format!("{}/fixtures/{name}", env!("CARGO_MANIFEST_DIR"))).unwrap()
    }

    /// symphonia の読み口でパケットを取り出し、自作のデコーダと symphonia の AAC のデコーダの標本を照らす（信号 / 誤差）。
    fn snr_against_symphonia(name: &str, c: Container, lcg: bool) -> f64 {
        let data = fixture(name);
        let limits = Limits::default();
        let mut o = symph::open("t", &data, c, &NoMonitor, &limits).unwrap();
        let cfg = match o.params.extra_data.as_deref() {
            Some(asc) => config::parse_asc(asc).unwrap(),
            None => {
                // ADTS：最初の頭から
                let b = &data;
                let profile = u32::from(b[2] >> 6);
                let index = u32::from((b[2] >> 2) & 0xf);
                let layout = (u32::from(b[2] & 1) << 2) | u32::from(b[3] >> 6);
                config::from_adts(profile, index, layout).unwrap()
            }
        };
        let mut ours = decoder::AacDecoder::new(cfg).unwrap();
        if lcg {
            // symphonia と同じ種（ffmpeg と同じ）
            ours.noise = decoder::Noise::Lcg(0x1f2e_3d4c);
        }
        let mut theirs =
            symphonia_codec_aac::AacDecoder::try_new(&o.params, &symph::decoder_options()).unwrap();
        let (mut sig, mut err) = (0f64, 0f64);
        while let Some(pkt) = o.reader.next_packet().unwrap() {
            let n = ours.decode(&pkt.data).unwrap();
            let buf = theirs.decode(&pkt).unwrap();
            let GenericAudioBufferRef::F32(b) = &buf else {
                panic!()
            };
            assert_eq!(n, b.frames(), "{name}");
            for ch in 0..cfg.channels {
                let t = b.plane(ch).unwrap();
                for (a, x) in ours.planes[ch][..n].iter().zip(t) {
                    sig += f64::from(*x) * f64::from(*x);
                    err += f64::from(a - x) * f64::from(a - x);
                }
            }
        }
        10.0 * crate::math::log10(sig / crate::math::max(err, 1e-300))
    }

    /// symphonia の読み口でパケットを取り出し、自作のデコーダ（雑音の源は `noise`。None なら既定の ChaCha8）で読んだ面。
    fn raw(name: &str, c: Container, noise: Option<decoder::Noise>) -> Vec<Vec<f32>> {
        let data = fixture(name);
        let mut o = symph::open("t", &data, c, &NoMonitor, &Limits::default()).unwrap();
        let cfg = match o.params.extra_data.as_deref() {
            Some(asc) => config::parse_asc(asc).unwrap(),
            None => {
                let b = &data;
                let profile = u32::from(b[2] >> 6);
                let index = u32::from((b[2] >> 2) & 0xf);
                let layout = (u32::from(b[2] & 1) << 2) | u32::from(b[3] >> 6);
                config::from_adts(profile, index, layout).unwrap()
            }
        };
        let mut d = decoder::AacDecoder::new(cfg).unwrap();
        if let Some(n) = noise {
            d.noise = n;
        }
        let mut planes = vec![Vec::new(); cfg.channels];
        while let Some(pkt) = o.reader.next_packet().unwrap() {
            let n = d.decode(&pkt.data).unwrap();
            for (p, src) in planes.iter_mut().zip(&d.planes) {
                p.extend_from_slice(&src[..n]);
            }
        }
        planes
    }

    /// 自作のデコーダ（雑音の源 `noise`）の標本を、ffmpeg の読み取りの参照（16 ビットの FLAC）と照らす（信号 / 誤差）。
    /// 頭の `skip` フレーム（priming）を飛ばし、参照の長さだけ比べる。参照が 16 ビットの端に張り付いた標本は比べない。
    fn snr_against_ffmpeg(
        name: &str,
        c: Container,
        noise: decoder::Noise,
        reference: &str,
        skip: usize,
    ) -> f64 {
        let planes = raw(name, c, Some(noise));
        let (v, _) = crate::codec::CodecSet::pinned()
            .decode(
                &mut std::io::Cursor::new(fixture(reference)),
                "flac",
                &crate::doc::DecodeOptions::default(),
                &Limits::default(),
                &NoMonitor,
            )
            .unwrap();
        let crate::value::Value::Audio(a) = v else {
            panic!("{reference}")
        };
        let theirs = a.to_planes().unwrap();
        assert_eq!(theirs.len(), planes.len(), "{name}");
        let (mut sig, mut err) = (0f64, 0f64);
        for (p, t) in planes.iter().zip(theirs.iter()) {
            assert!(p.len() >= skip + t.len(), "{name}");
            for (a, x) in p[skip..].iter().zip(t.iter()) {
                if *x <= -1.0 || *x >= 32_767.0 / 32_768.0 {
                    continue;
                }
                sig += f64::from(*x) * f64::from(*x);
                err += f64::from(a - x) * f64::from(a - x);
            }
        }
        10.0 * crate::math::log10(sig / crate::math::max(err, 1e-300))
    }

    /// PNS の帯まで ffmpeg と同じ雑音（試験だけの [`decoder::Noise::Ffmpeg`]）にすれば、ffmpeg が書いた PNS のある
    /// ファイルも ffmpeg の読み取りと 16 ビットの丸めの差まで一致する（約 86 dB。ffmpeg 9.0 の f32 の読み取りとは 135.8 dB。
    /// PNS の帯の雑音の大きさ・帯の並び・窓の扱いの確かめ）。ChaCha8 の雑音では、PNS の帯の分だけ離れる。
    #[test]
    fn pns_matches_ffmpeg_with_its_noise() {
        for (name, c, reference, skip) in [
            (
                "aac-lc-ffmpeg.m4a",
                Container::Mp4,
                "aac-lc-ffmpeg.ffmpeg.flac",
                1_024,
            ),
            ("aac-lc.aac", Container::Adts, "aac-lc.ffmpeg.flac", 0),
        ] {
            let same = snr_against_ffmpeg(
                name,
                c,
                decoder::Noise::Ffmpeg(0x1f2e_3d4c),
                reference,
                skip,
            );
            let chacha = snr_against_ffmpeg(
                name,
                c,
                decoder::Noise::ChaCha(crate::determinism::rng(0)),
                reference,
                skip,
            );
            eprintln!("{name}: ffmpeg の雑音で {same:.1} dB（ChaCha8 では {chacha:.1} dB）");
            assert!(same > 80.0, "{name}: {same:.1} dB");
            assert!(chacha < 70.0, "{name}: PNS の帯がない？ {chacha:.1} dB");
        }
    }

    /// 試験用の f64 の複素 FFT（基数 2。`re.len()` は 2 のべき）。
    fn fft64(re: &mut [f64], im: &mut [f64]) {
        let n = re.len();
        let bits = n.trailing_zeros();
        for i in 0..n {
            let j = i.reverse_bits() >> (usize::BITS - bits);
            if j > i {
                re.swap(i, j);
                im.swap(i, j);
            }
        }
        let mut len = 2;
        while len <= n {
            let half = len / 2;
            for start in (0..n).step_by(len) {
                for k in 0..half {
                    let a = -2.0 * std::f64::consts::PI * k as f64 / len as f64;
                    let (wr, wi) = (crate::math::cos(a), crate::math::sin(a));
                    let (x, y) = (start + k, start + k + half);
                    let tr = re[y] * wr - im[y] * wi;
                    let ti = re[y] * wi + im[y] * wr;
                    re[y] = re[x] - tr;
                    im[y] = im[x] - ti;
                    re[x] += tr;
                    im[x] += ti;
                }
            }
            len *= 2;
        }
    }

    /// 面の全体のパワースペクトルを、1 kHz ごとの帯に足したもの（`rate` Hz）。
    fn band_energies(p: &[f32], rate: f64) -> Vec<f64> {
        let n = p.len().next_power_of_two();
        let mut re: Vec<f64> = p.iter().map(|v| f64::from(*v)).collect();
        re.resize(n, 0.0);
        let mut im = vec![0.0; n];
        fft64(&mut re, &mut im);
        let mut bands = vec![0.0; (rate / 2_000.0) as usize + 1];
        for k in 0..n / 2 {
            let f = k as f64 * rate / n as f64;
            bands[(f / 1_000.0) as usize] += re[k] * re[k] + im[k] * im[k];
        }
        bands
    }

    /// ChaCha8 の雑音（本番）の PNS の帯は、ffmpeg と同じ雑音で読んだ値と、1 kHz ごとの帯のエネルギーが近い（ADR-220 (2)：
    /// 雑音の並びは違っても、帯ごとの大きさは同じ）。ffmpeg と同じ雑音の値は、上の試験で ffmpeg の読み取りと照らしてある。
    /// 全体のエネルギーの 1e-9 に満たない帯は比べない（右の 10〜16 kHz の雑音の帯は −53〜−68 dB で、差は 0.35 dB まで。
    /// 切り方を変えると 1.4 dB ほどまで揺れるので、許す差は 3 dB）。
    #[test]
    fn chacha_noise_keeps_the_band_energies() {
        for (name, c) in [
            ("aac-lc-ffmpeg.m4a", Container::Mp4),
            ("aac-lc.aac", Container::Adts),
        ] {
            let ours = raw(name, c, None);
            let theirs = raw(name, c, Some(decoder::Noise::Ffmpeg(0x1f2e_3d4c)));
            assert_ne!(ours, theirs, "{name}：PNS の帯がない");
            let mut worst = 0f64;
            for (a, b) in ours.iter().zip(&theirs) {
                assert_eq!(a.len(), b.len(), "{name}");
                let (ea, eb) = (band_energies(a, 44_100.0), band_energies(b, 44_100.0));
                let total: f64 = eb.iter().sum();
                for (k, (x, y)) in ea.iter().zip(&eb).enumerate() {
                    if *y < total * 1e-9 {
                        continue;
                    }
                    let db = 10.0 * crate::math::log10(x / y);
                    worst = crate::math::max(worst, db.abs());
                    assert!(db.abs() < 3.0, "{name}：{k} kHz の帯 {db:+.2} dB");
                }
            }
            eprintln!("{name}：帯のエネルギーの差は最大 {worst:.2} dB");
        }
    }

    /// 壊したファイル（乱数で数バイトを変える・切る。m4a と ADTS のフィクスチャ）を自作の読み手で読んでも panic せず、必ず
    /// 終わり、誤りは壊れたファイル・扱わない形・上限のどれか（`Internal` にしない）。形を答えたら値はその形に収まり、
    /// ダイジェストを取れる。旧版（symphonia）と両方で読めたら長さとレートとチャンネルの数が同じ。
    #[test]
    fn broken_files_are_refused_or_read() {
        use crate::codec::Decoder as _;
        use crate::value::Value;
        use rand_chacha::rand_core::Rng as _;
        use std::io::Cursor;
        let mut rng = crate::determinism::rng(61);
        let seeds: Vec<(&str, Vec<u8>)> = [
            ("m4a", "aac-lc-afconvert.m4a"),
            ("m4a", "aac-lc-ffmpeg.m4a"),
            ("m4a", "aac-lc-mono.m4a"),
            ("m4a", "aac-lc-8ch-afconvert.m4a"),
            ("m4a", "aac-he.m4a"),
            ("aac", "aac-lc.aac"),
            ("aac", "aac-lc-6ch.aac"),
            ("aac", "aac-lc-then-he.aac"),
        ]
        .into_iter()
        .map(|(f, n)| (f, fixture(n)))
        .collect();
        let limits = Limits::default();
        let opts = crate::doc::DecodeOptions::default();
        let (mut read, mut refused, mut same_as_old) = (0, 0, 0);
        for round in 0..1_200 {
            let (format, seed) = &seeds[(rng.next_u32() as usize) % seeds.len()];
            let mut b = seed.clone();
            for _ in 0..1 + rng.next_u32() % 6 {
                // 箱の頭・ADTS の頭の多い前のほうを多めに
                let span = if rng.next_u32().is_multiple_of(2) {
                    b.len().min(4_096)
                } else {
                    b.len()
                };
                let i = (rng.next_u32() as usize) % span;
                b[i] = rng.next_u32() as u8;
            }
            if rng.next_u32().is_multiple_of(5) {
                let cut = (rng.next_u32() as usize) % b.len();
                b.truncate(cut);
            }
            let (ours, old) = if *format == "m4a" {
                (
                    read::AacCodec::m4a(),
                    crate::codec::lossy::SymphoniaDecoder::m4a_aac(),
                )
            } else {
                (
                    read::AacCodec::adts(),
                    crate::codec::lossy::SymphoniaDecoder::adts_aac(),
                )
            };
            let ok_err = |e: &EngineError| {
                matches!(
                    e,
                    EngineError::Decode { .. }
                        | EngineError::Unsupported(_)
                        | EngineError::Limit(_)
                )
            };
            let held = ours.held_bytes(&mut Cursor::new(&b), format, &limits);
            assert!(held.is_ok(), "回 {round}：{held:?}");
            let got = ours
                .shape(&mut Cursor::new(&b), format, &opts, &limits)
                .and_then(|s| {
                    let v =
                        ours.decode(&mut Cursor::new(&b), format, &opts, &limits, &NoMonitor)?;
                    Ok((s, v))
                });
            match got {
                Ok((s, v)) => {
                    assert!(s.admits(&v.shape()), "回 {round}：{s:?} と {:?}", v.shape());
                    v.digest().unwrap();
                    read += 1;
                    let Value::Audio(a) = &v else {
                        panic!("回 {round}")
                    };
                    if let Ok(Value::Audio(o)) =
                        old.decode(&mut Cursor::new(&b), format, &opts, &limits, &NoMonitor)
                    {
                        assert_eq!(
                            (a.rate(), a.channels(), a.frames()),
                            (o.rate(), o.channels(), o.frames()),
                            "回 {round}：旧版と形が違う"
                        );
                        same_as_old += 1;
                    }
                }
                Err(e) => {
                    assert!(ok_err(&e), "回 {round}：{e:?}");
                    refused += 1;
                }
            }
        }
        eprintln!("読めた {read}（旧版とも同じ形 {same_as_old}）・断った {refused}");
        assert!(read > 100 && refused > 100, "{read} {refused}");
    }

    /// 速さの目安（release で `--ignored`）：`aac-lc.aac`（ffmpeg の 1.04 秒のステレオ 128 kbps の ADTS。PNS あり）を 290 回
    /// つないだ 5 分の ADTS を、前の symphonia の読み手と自作の読み手で読む（値のダイジェストは含めない）。M16 段 4 の実測
    /// （M4 Max）：symphonia 0.118 秒、自作 0.100 秒。
    #[test]
    #[ignore]
    // 計るだけの試験（値は時刻によらない）
    #[allow(clippy::disallowed_methods)]
    fn speed_against_symphonia() {
        use crate::codec::Decoder as _;
        use std::io::Cursor;
        let one = fixture("aac-lc.aac");
        let data = one.repeat(290);
        let (limits, opts) = (Limits::default(), crate::doc::DecodeOptions::default());
        let ours = read::AacCodec::adts();
        let old = crate::codec::lossy::SymphoniaDecoder::adts_aac();
        let mut best = [f64::MAX; 2];
        for _ in 0..3 {
            let t = std::time::Instant::now();
            let a = old
                .decode(&mut Cursor::new(&data), "aac", &opts, &limits, &NoMonitor)
                .unwrap();
            best[0] = crate::math::min(best[0], t.elapsed().as_secs_f64());
            let t = std::time::Instant::now();
            let b = ours
                .decode(&mut Cursor::new(&data), "aac", &opts, &limits, &NoMonitor)
                .unwrap();
            best[1] = crate::math::min(best[1], t.elapsed().as_secs_f64());
            assert_eq!(a.shape(), b.shape());
        }
        println!(
            "{} バイト：symphonia {:.3} 秒、自作 {:.3} 秒",
            data.len(),
            best[0],
            best[1]
        );
    }

    #[test]
    fn close_to_symphonia_on_fixtures() {
        for (name, c) in [
            ("aac-lc-afconvert.m4a", Container::Mp4),
            ("aac-lc-ffmpeg.m4a", Container::Mp4),
            ("aac-lc.aac", Container::Adts),
        ] {
            let snr = snr_against_symphonia(name, c, true);
            let chacha = snr_against_symphonia(name, c, false);
            eprintln!("{name}: {snr:.1} dB（ChaCha8 の雑音では {chacha:.1} dB）");
            assert!(snr > 100.0, "{name}: {snr:.1} dB");
        }
    }
}
