//! AudioToolbox の AAC の書き出しの backend（`aac-audiotoolbox-enc/1`）。エンジンの `PlatformAacEncoder` で包んで `Encoder`
//! にする。約束は Swift の `AacEncode.swift` とエンジンの `codec::platform::aac` に書いた。

use std::sync::Arc;

use kura_engine::Monitor;
use kura_engine::codec::CodecEnv;
use kura_engine::codec::platform::{AacBackend, AacHandle, AacSettings, BackendError};

use crate::bridge::SwiftAac;
use crate::codec::AAC_AUDIOTOOLBOX_ENC;
use crate::codec::caps::MacCaps;
use crate::codec::convert;

/// 書くチャンネルの数の上限。7・8 チャンネルは、kura の並び（WAV のマスク 0x70F・0x63F）と AAC の配置（6.1 の Cs・7.1 の
/// Lc・Rc）のラベルが合わず、AudioToolbox がチャンネルを写し替える（読み直しても並びを確かめられない）ので書かない。
pub const AAC_MAX_CHANNELS: u16 = 6;

/// AudioToolbox の AAC の書き出しの backend。
pub struct AacToolboxBackend {
    caps: Arc<MacCaps>,
    env: CodecEnv,
}

impl AacToolboxBackend {
    /// 能力から組む（レート・チャンネル・ビットレートの表は能力のもの。呼ぶたびに OS に問い合わせない）。
    pub fn new(caps: Arc<MacCaps>) -> Self {
        let env = caps.audio_env();
        Self { caps, env }
    }
}

impl AacBackend for AacToolboxBackend {
    type Handle = AacToolboxHandle;

    fn id(&self) -> &'static str {
        AAC_AUDIOTOOLBOX_ENC
    }

    fn env(&self) -> Option<CodecEnv> {
        Some(self.env.clone())
    }

    fn supports(&self, rate: u32, channels: u16) -> bool {
        channels <= AAC_MAX_CHANNELS && self.caps.aac_supports(rate, channels)
    }

    fn bitrates(&self, rate: u32, channels: u16) -> Vec<u32> {
        if channels <= AAC_MAX_CHANNELS {
            self.caps.aac_bitrates_kbps(rate, channels)
        } else {
            Vec::new()
        }
    }

    fn open(&self, settings: &AacSettings, max_out: u64) -> Result<AacToolboxHandle, BackendError> {
        let aac = SwiftAac::open(&convert::aac_settings(settings), max_out)?;
        Ok(AacToolboxHandle { aac })
    }
}

/// 書きかけの AAC。
pub struct AacToolboxHandle {
    aac: SwiftAac,
}

impl AacHandle for AacToolboxHandle {
    fn write(&mut self, planes: &[&[f32]]) -> Result<(), BackendError> {
        self.aac.write(planes)
    }

    fn finish(&mut self, mon: &dyn Monitor) -> Result<u64, BackendError> {
        self.aac.finish(mon)
    }

    fn copy(&mut self, out: &mut [u8]) -> Result<(), BackendError> {
        self.aac.copy(out)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use kura_engine::codec::platform::{AacMode, zero_mp4_times};
    use kura_engine::codec::{CodecSet, Tier};
    use kura_engine::monitor::CancelAfter;
    use kura_engine::value::{Audio, AudioMeta};
    use kura_engine::{EngineError, NoMonitor, Value};
    use serde_json::json;

    use super::*;
    use crate::abi::AacSettingsC;
    use crate::codec::testutil::*;
    use crate::codec::{AUDIOTOOLBOX_DEC, codec_caps};

    /// チャンネルごとに周波数の違う正弦波（テストの値。std の sin でよい）。
    fn tone(rate: u32, channels: usize, frames: usize) -> Value {
        let planes = (0..channels)
            .map(|c| {
                let f = [
                    440.0, 660.0, 990.0, 55.0, 1_320.0, 1_760.0, 2_200.0, 2_640.0,
                ][c];
                Arc::new(
                    (0..frames)
                        .map(|i| {
                            (0.4 * (2.0 * std::f64::consts::PI * f * i as f64 / f64::from(rate))
                                .sin()) as f32
                        })
                        .collect::<Vec<f32>>(),
                )
            })
            .collect();
        Value::Audio(Arc::new(
            Audio::new(rate, planes, AudioMeta::default()).unwrap(),
        ))
    }

    fn audio(v: &Value) -> &Audio {
        match v {
            Value::Audio(a) => a,
            other => panic!("音声ではありません: {:?}", other.shape()),
        }
    }

    /// `a` と `b` の相互相関（`b` を `lag` だけ遅らせる）。
    fn correlate(a: &[f32], b: &[f32], lag: isize) -> f64 {
        let mut s = 0.0;
        for (i, x) in a.iter().enumerate() {
            let j = i as isize + lag;
            if j >= 0 && (j as usize) < b.len() {
                s += f64::from(*x) * f64::from(b[j as usize]);
            }
        }
        s
    }

    /// 正規化した相関（-1〜1）。
    fn ncc(a: &[f32], b: &[f32]) -> f64 {
        correlate(a, b, 0) / (correlate(a, a, 0) * correlate(b, b, 0)).sqrt()
    }

    /// 信号と誤差の比（dB）。
    fn snr(src: &[f32], got: &[f32]) -> f64 {
        let (mut s, mut e) = (0.0, 0.0);
        for (x, y) in src.iter().zip(got) {
            s += f64::from(*x).powi(2);
            e += (f64::from(*x) - f64::from(*y)).powi(2);
        }
        10.0 * (s / e).log10()
    }

    /// AAC を書き、段 6 の読み取り（固定の段の m4a）で読み直すと、長さがちょうど同じで、ずれが 0（相互相関の山が 0）、
    /// 元に近い。記録は Platform・再現を約束しない・環境つき。m4a の時刻は 0 で、iTunSMPB（priming）がある。
    #[test]
    fn aac_round_trips_through_the_pinned_reader() {
        let _g = shared();
        let set = mac_set();
        let pinned = CodecSet::pinned();
        for (rate, channels, frames) in
            [(44_100, 2, 44_117), (48_000, 1, 96_005), (32_000, 2, 1_000)]
        {
            let src = tone(rate, channels, frames);
            let (m4a, stamp) = set
                .encode_bytes(&src, "m4a", &json!({}), &NoMonitor)
                .unwrap();
            assert_eq!(
                (stamp.codec.as_str(), stamp.tier, stamp.reproducible),
                (AAC_AUDIOTOOLBOX_ENC, Tier::Platform, false)
            );
            assert!(stamp.env.unwrap().components.contains_key("audiotoolbox"));
            let mut zeroed = m4a.clone();
            zero_mp4_times(&mut zeroed).unwrap();
            assert_eq!(zeroed, m4a, "時刻は 0 にしてある");
            assert!(m4a.windows(8).any(|w| w == b"iTunSMPB"));
            let (back, dstamp) = decode(&pinned, &m4a, "m4a").unwrap();
            assert_eq!(dstamp.tier, Tier::Pinned);
            let (a, b) = (audio(&src), audio(&back));
            assert_eq!(
                (b.rate(), b.channels(), b.frames()),
                (rate, channels, frames),
                "{rate} {channels} {frames}"
            );
            let (x, y) = (
                &a.to_planes().unwrap()[0][..],
                &b.to_planes().unwrap()[0][..],
            );
            let best = (-64..=64)
                .max_by(|&p, &q| correlate(x, y, p).total_cmp(&correlate(x, y, q)))
                .unwrap();
            assert_eq!(best, 0, "{rate} {channels}");
            if frames > 10_000 {
                let db = snr(x, y);
                assert!(db > 25.0, "{rate} {channels}: {db} dB");
            }
        }
    }

    /// M13 段 3：流して書く口（`CodecSet::encode_stream`。値の生産者）でも、値を書いたものと同じ形の m4a（backend には同じ
    /// 4,096 フレームずつ渡る）。AudioToolbox は呼び出しの道によって最後のパケットのバイト列が変わることがある（同じ道なら
    /// 同じ。R2 は約束しない）ので、固定の段の読み取りで読み直した長さと音が同じことを確かめる。値のダイジェストは流した
    /// 標本のもの。書いたバイト数は比べない（macOS 26.7.2 では最後のパケットの長さが 1 バイト違った。26.7 では同じ。
    /// ADR-213）。記録のバイト数が、それぞれ実際に書いた長さと合うことだけを確かめる。
    #[test]
    fn streamed_aac_equals_encode_here() {
        let _g = shared();
        let set = mac_set();
        let pinned = CodecSet::pinned();
        for (rate, channels, frames) in [(48_000, 2, 30_000), (44_100, 1, 70_001)] {
            let src = tone(rate, channels, frames);
            let (want, stamp) = set
                .encode_bytes(&src, "m4a", &json!({}), &NoMonitor)
                .unwrap();
            let a = audio(&src);
            let mut producer = kura_engine::audio::stream::ValueProducer::new(a);
            let mut out = std::io::Cursor::new(Vec::new());
            let se = set
                .encode_stream(&mut producer, "m4a", &json!({}), &mut out, &NoMonitor)
                .unwrap();
            let got = out.into_inner();
            assert_eq!(
                (se.codec.as_str(), se.tier),
                (stamp.codec.as_str(), stamp.tier)
            );
            assert_eq!(
                (se.bytes, stamp.bytes),
                (got.len() as u64, want.len() as u64),
                "{rate} {channels} {frames}"
            );
            assert_eq!(se.value_digest, src.digest().unwrap());
            let (x, _) = decode(&pinned, &got, "m4a").unwrap();
            let (y, _) = decode(&pinned, &want, "m4a").unwrap();
            let (x, y) = (audio(&x), audio(&y));
            assert_eq!(x.frames(), y.frames());
            for c in 0..channels {
                let (p, q) = (&x.to_planes().unwrap()[c], &y.to_planes().unwrap()[c]);
                assert!(ncc(p, q) > 0.999, "{rate} {channels} {frames} ch {c}");
            }
        }
    }

    /// 同じ値・同じ設定なら、同じプロセスでは同じバイト列（Apple の時刻は 0 にした）。約束はしない（R2 は偽）。
    #[test]
    fn same_input_writes_the_same_bytes_here() {
        let _g = shared();
        let set = mac_set();
        let src = tone(48_000, 2, 30_000);
        let (a, _) = set
            .encode_bytes(&src, "m4a", &json!({}), &NoMonitor)
            .unwrap();
        let (b, _) = set
            .encode_bytes(&src, "m4a", &json!({}), &NoMonitor)
            .unwrap();
        assert_eq!(a, b);
    }

    /// 3〜6 チャンネル：ClientChannelLayout で kura の並び（5.1 は L R C LFE BL BR）を渡して書き、段の順に任せても読め、AudioToolbox で読み直すと
    /// 同じ並びに戻る（各チャンネルが元の同じチャンネルと最もよく相関する）。7・8 チャンネルは書かない。M16 段 4 から、
    /// channel_configuration で書かれたものは固定の段の自作の AAC（`m4a-aac-kura-dec/1`）が読み、それも同じ並びに戻る。
    #[test]
    fn surround_keeps_the_channel_order() {
        let _g = shared();
        let set = mac_set();
        for channels in 3..=6 {
            let src = tone(48_000, channels, 48_000);
            let (m4a, _) = set
                .encode_bytes(&src, "m4a", &json!({}), &NoMonitor)
                .unwrap();
            // 段の順に任せても、形と読みを同じ段が答える：channel_configuration（3・5・6 チャンネル）は固定の段の自作の
            // AAC、AAC の配置にない kura の 4 チャンネル（L R BL BR。PCE で書かれる）は macOS の段
            let want = if channels == 4 {
                AUDIOTOOLBOX_DEC
            } else {
                "m4a-aac-kura-dec/1"
            };
            let (_, shaped_by) = set
                .shape(
                    &mut std::io::Cursor::new(m4a.clone()),
                    "m4a",
                    &kura_engine::doc::DecodeOptions::default(),
                    &kura_engine::Limits::default(),
                )
                .unwrap();
            assert_eq!(shaped_by, want, "{channels} ch の形");
            let (routed, routed_stamp) = decode(&set, &m4a, "m4a").unwrap();
            assert_eq!(
                (audio(&routed).channels(), audio(&routed).frames()),
                (channels, 48_000),
                "{channels} ch を段の順で読む"
            );
            assert_eq!(routed_stamp.codec, want, "{channels} ch の読み");
            let (a, r) = (audio(&src), audio(&routed));
            for c in 0..channels {
                let same = ncc(&a.to_planes().unwrap()[c], &r.to_planes().unwrap()[c]);
                assert!(same > 0.9, "{channels} ch・段の順・チャンネル {c}: {same}");
            }
            let (back, stamp) = set
                .decode_with(
                    AUDIOTOOLBOX_DEC,
                    &mut std::io::Cursor::new(m4a),
                    "m4a",
                    &kura_engine::doc::DecodeOptions::default(),
                    &kura_engine::Limits::default(),
                    &NoMonitor,
                )
                .unwrap();
            assert_eq!(stamp.tier, Tier::Platform);
            let (a, b) = (audio(&src), audio(&back));
            assert_eq!((b.channels(), b.frames()), (channels, 48_000));
            for c in 0..channels {
                let same = ncc(&a.to_planes().unwrap()[c], &b.to_planes().unwrap()[c]);
                assert!(same > 0.9, "{channels} ch・チャンネル {c}: {same}");
                for o in (0..channels).filter(|o| *o != c) {
                    assert!(
                        ncc(&a.to_planes().unwrap()[o], &b.to_planes().unwrap()[c]).abs() < same,
                        "{c} {o}"
                    );
                }
            }
        }
        for channels in [7, 8] {
            assert!(
                set.encoder("m4a", &tone(48_000, channels, 10).shape())
                    .is_none()
            );
        }
    }

    /// 設定：cbr・abr・vbr（品質）も書け、選べないビットレートは断る。書けないレート（96 kHz）には m4a のエンコーダがない。
    #[test]
    fn options_and_unsupported_shapes() {
        let _g = shared();
        let set = mac_set();
        let src = tone(44_100, 2, 10_000);
        for opts in [
            json!({"mode": "cbr", "bitrate": 128}),
            json!({"mode": "abr", "bitrate": 192}),
            json!({"mode": "vbr", "quality": 64}),
            json!({"bitrate": 320}),
        ] {
            let (m4a, _) = set.encode_bytes(&src, "m4a", &opts, &NoMonitor).unwrap();
            let (back, _) = decode(&CodecSet::pinned(), &m4a, "m4a").unwrap();
            assert_eq!(audio(&back).frames(), 10_000, "{opts}");
        }
        assert!(matches!(
            set.encode_bytes(&src, "m4a", &json!({"bitrate": 100}), &NoMonitor),
            Err(EngineError::Invalid(_))
        ));
        let hi = tone(96_000, 2, 1_000);
        assert!(set.encoder("m4a", &hi.shape()).is_none());
        assert!(matches!(
            set.encode_bytes(&hi, "m4a", &json!({}), &NoMonitor),
            Err(EngineError::Unsupported(_))
        ));
    }

    /// 中止：4,096 フレームの合間（と書き終える前）の、1〜3 回目の確認のどこでも止まり、ハンドルは漏れない。
    #[test]
    fn cancels_between_chunks() {
        let _g = exclusive();
        let (live, _) = stats();
        let set = mac_set();
        let src = tone(48_000, 2, 20_000);
        for after in 1..=3 {
            assert!(
                matches!(
                    set.encode_bytes(&src, "m4a", &json!({}), &CancelAfter::new(after)),
                    Err(EngineError::Canceled)
                ),
                "{after}"
            );
        }
        // 1 チャンクだけの音声は、書き終える前（Swift の中の確認）で止まる
        let short = tone(48_000, 2, 100);
        assert!(matches!(
            set.encode_bytes(&short, "m4a", &json!({}), &CancelAfter::new(3)),
            Err(EngineError::Canceled)
        ));
        assert_eq!(stats().0, live);
    }

    /// 出力の上限は Swift の側でも効く（超える書き込みを断って Limit）。設定の誤りは呼び方の誤り。
    /// M13 段 3 の検証の直し：Swift は出力の配列を open で上限まで確保する（倍々に伸ばさない）。確保する大きさ（上限）は書く
    /// バイト列を変えない：同じ値を、上限 `aac_max_output` と、その 8 倍で書いたものが同じ。書き手を直に呼ぶので、m4a の
    /// 作成・変更の時刻（秒）が入っている。2 回の書き出しが秒をまたぐと違う（2026-11 に CI の macOS で 1 秒ずれて落ちた）ので、
    /// 書き出しの道と同じく `zero_mp4_times` で 0 にしてから比べる。
    #[test]
    fn reserved_output_capacity_does_not_change_the_bytes() {
        let _g = shared();
        let caps = codec_caps().unwrap();
        let backend = AacToolboxBackend::new(caps);
        for (rate, channels, frames) in [(48_000u32, 2u16, 30_000usize), (44_100, 1, 70_001)] {
            let settings = AacSettings {
                rate,
                channels,
                channel_mask: if channels == 1 { 0x4 } else { 0x3 },
                mode: AacMode::VbrConstrained,
                bitrate_kbps: 128,
                quality: 127,
            };
            let src = tone(rate, usize::from(channels), frames);
            let planes = audio(&src).to_planes().unwrap();
            let write = |max_out: u64| {
                let mut h = backend.open(&settings, max_out).unwrap();
                let mut start = 0;
                while start < frames {
                    let end = (start + 4_096).min(frames);
                    let ps: Vec<&[f32]> = planes.iter().map(|p| &p[start..end]).collect();
                    h.write(&ps).unwrap();
                    start = end;
                }
                let len = h.finish(&NoMonitor).unwrap();
                let mut out = vec![0u8; usize::try_from(len).unwrap()];
                h.copy(&mut out).unwrap();
                out
            };
            let max = kura_engine::codec::platform::aac_max_output(frames as u64, channels);
            let mut a = write(max);
            let mut b = write(max * 8);
            assert!(!a.is_empty());
            zero_mp4_times(&mut a).unwrap();
            zero_mp4_times(&mut b).unwrap();
            assert_eq!(a, b, "{rate} {channels} {frames}");
        }
    }

    #[test]
    fn swift_enforces_the_output_limit_and_settings() {
        let _g = shared();
        let caps = codec_caps().unwrap();
        let backend = AacToolboxBackend::new(caps);
        let settings = AacSettings {
            rate: 48_000,
            channels: 2,
            channel_mask: 0x3,
            mode: AacMode::Cbr,
            bitrate_kbps: 320,
            quality: 127,
        };
        let mut h = backend.open(&settings, 2_000).unwrap();
        let plane = vec![0.25f32; 4_096];
        let mut result = Ok(());
        for _ in 0..20 {
            result = h.write(&[&plane, &plane]);
            if result.is_err() {
                break;
            }
        }
        let result = result.and_then(|()| h.finish(&NoMonitor).map(|_| ()));
        assert!(matches!(result, Err(BackendError::Limit(_))), "{result:?}");
        // 並びとチャンネルの数が合わない・知らない mode は Swift が断る
        for bad in [
            AacSettingsC {
                channel_mask: 0x7,
                ..convert::aac_settings(&settings)
            },
            AacSettingsC {
                mode: 9,
                ..convert::aac_settings(&settings)
            },
        ] {
            assert!(matches!(
                SwiftAac::open(&bad, 1 << 20),
                Err(BackendError::Invalid(_))
            ));
        }
    }
}
