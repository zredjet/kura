//! AudioToolbox の読み取りの backend（`audiotoolbox-dec/1`）。エンジンの `PlatformAudioDecoder` で包んで `Decoder` にする。
//! 約束は Swift の `AudioDecode.swift` とエンジンの `codec::platform::audio` に書いた。

use std::sync::Arc;

use kura_engine::codec::CodecEnv;
use kura_engine::codec::platform::{
    AudioBackend, AudioHandle, AudioInfo, BackendError, CHANNEL_MASKS,
};

use crate::bridge::SwiftAudio;
use crate::codec::AUDIOTOOLBOX_DEC;
use crate::codec::caps::{AUDIO_ROUTES, MacCaps, audio_route};
use crate::codec::convert;

/// AudioToolbox の読み取りの backend。
pub struct AudioToolboxBackend {
    env: CodecEnv,
    /// 読む形式（能力にあるものだけ）。
    formats: Vec<&'static str>,
}

impl AudioToolboxBackend {
    /// 能力から組む（入れ物・中身を読めない形式は扱わない）。
    pub fn new(caps: &MacCaps) -> Self {
        Self {
            env: caps.audio_env(),
            formats: AUDIO_ROUTES
                .iter()
                .map(|r| r.format)
                .filter(|f| caps.audio_readable(f))
                .collect(),
        }
    }
}

impl AudioBackend for AudioToolboxBackend {
    type Handle = AudioToolboxHandle;

    fn id(&self) -> &'static str {
        AUDIOTOOLBOX_DEC
    }

    fn can_decode(&self, format: &str) -> bool {
        self.formats.contains(&format)
    }

    fn env(&self) -> Option<CodecEnv> {
        Some(self.env.clone())
    }

    fn open(&self, bytes: Arc<Vec<u8>>, format: &str) -> Result<AudioToolboxHandle, BackendError> {
        let route = audio_route(format)
            .filter(|_| self.can_decode(format))
            .ok_or_else(|| {
                BackendError::Unsupported(format!("{format} は AudioToolbox で読みません"))
            })?;
        let audio = SwiftAudio::open(bytes, route.hint, route.allowed, &CHANNEL_MASKS)?;
        let info = convert::audio_info(&audio.info())?;
        Ok(AudioToolboxHandle { audio, info })
    }
}

/// 開いた音声。
pub struct AudioToolboxHandle {
    audio: SwiftAudio,
    info: AudioInfo,
}

impl AudioHandle for AudioToolboxHandle {
    fn info(&self) -> AudioInfo {
        self.info
    }

    fn read(&mut self, planes: &mut [&mut [f32]]) -> Result<usize, BackendError> {
        if planes.len() != usize::from(self.info.channels) {
            return Err(BackendError::Invalid(format!(
                "チャンネルの数が合いません（{} / {}）",
                planes.len(),
                self.info.channels
            )));
        }
        self.audio.read(planes)
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use kura_engine::codec::platform::PlatformAudioDecoder;
    use kura_engine::codec::{CodecSet, Decoder, Tier};
    use kura_engine::doc::DecodeOptions;
    use kura_engine::monitor::CancelAfter;
    use kura_engine::value::{Audio, AudioSource};
    use kura_engine::{EngineError, Limits, Monitor, NoMonitor, Value};
    use rand_chacha::rand_core::Rng;

    use super::*;
    use crate::codec::codec_caps;
    use crate::codec::testutil::*;

    fn decoder() -> PlatformAudioDecoder<AudioToolboxBackend> {
        PlatformAudioDecoder::new(AudioToolboxBackend::new(&codec_caps().unwrap()))
    }

    fn read(
        dec: &dyn Decoder,
        bytes: &[u8],
        format: &str,
        limits: &Limits,
        mon: &dyn Monitor,
    ) -> Result<Value, EngineError> {
        dec.decode(
            &mut Cursor::new(bytes.to_vec()),
            format,
            &DecodeOptions::default(),
            limits,
            mon,
        )
    }

    fn audio(v: &Value) -> &Audio {
        match v {
            Value::Audio(a) => a,
            other => panic!("音声ではありません: {:?}", other.shape()),
        }
    }

    /// 固定の段が読めない HE-AAC（m4a）と Ogg Opus は Platform の段で読み、priming・pre-skip を切った長さになる。ADTS は切らない。
    #[test]
    fn he_aac_and_opus_read_on_the_platform_tier() {
        let _g = shared();
        let set = mac_set();
        let (v, stamp) = decode(&set, &engine_fixture("aac-he.m4a"), "m4a").unwrap();
        assert_eq!(
            (stamp.codec.as_str(), stamp.tier, stamp.reproducible),
            (AUDIOTOOLBOX_DEC, Tier::Platform, false)
        );
        let env = stamp.env.unwrap();
        assert_eq!(env.tool, "macOS");
        assert!(env.components.contains_key("audiotoolbox"), "{env:?}");
        let a = audio(&v);
        assert_eq!((a.rate(), a.channels(), a.frames()), (44_100, 2, 44_100));
        // 非可逆なので元の形式は 16 ビットの整数の扱い
        assert_eq!(
            a.meta.source,
            Some(AudioSource {
                bits: 16,
                float: false,
                rate: 44_100
            })
        );
        // ADTS（固定の段も読む AAC-LC）は、入れ物に情報がないので切らない（46,080。固定の段と同じ規則）
        let adts = read(
            &decoder(),
            &engine_fixture("aac-lc.aac"),
            "aac",
            &Limits::default(),
            &NoMonitor,
        )
        .unwrap();
        assert_eq!(audio(&adts).frames(), 46_080);
        let opus = engine_fixture("ogg-opus.ogg");
        for format in ["ogg", "opus"] {
            let (v, stamp) = decode(&set, &opus, format).unwrap();
            assert_eq!(
                (stamp.codec.as_str(), stamp.tier),
                (AUDIOTOOLBOX_DEC, Tier::Platform)
            );
            let a = audio(&v);
            assert_eq!(
                (a.rate(), a.channels(), a.frames()),
                (48_000, 2, 48_000),
                "{format}"
            );
        }
    }

    /// Ogg Vorbis は AudioToolbox では読まない（先頭を 128 フレーム余分に切るため）。固定の段が読む。
    #[test]
    fn vorbis_is_left_to_the_pinned_tier() {
        let _g = shared();
        let vorbis = engine_fixture("ogg-vorbis-ffmpeg.ogg");
        let (_, stamp) = decode(&mac_set(), &vorbis, "ogg").unwrap();
        assert_eq!(stamp.tier, Tier::Pinned);
        match read(&decoder(), &vorbis, "ogg", &Limits::default(), &NoMonitor) {
            Err(EngineError::Unsupported(m)) => assert!(m.contains("vorb"), "{m}"),
            other => panic!("{other:?}"),
        }
    }

    /// WAV の PCM（16・24 ビット）は、AudioToolbox で読んでも固定の段とビット一致し、元の形式を申告する。
    #[test]
    fn wav_pcm_matches_the_pinned_reader() {
        let _g = shared();
        let dec = decoder();
        let pinned = CodecSet::pinned();
        for (name, bits, frames) in [
            ("audio-src16.wav", 16, 44_100),
            ("audio-src24.wav", 24, 24_000),
        ] {
            let bytes = engine_fixture(name);
            let (want, _) = decode(&pinned, &bytes, "wav").unwrap();
            let got = read(&dec, &bytes, "wav", &Limits::default(), &NoMonitor).unwrap();
            let (g, w) = (audio(&got), audio(&want));
            assert_eq!(g.frames(), frames, "{name}");
            assert_eq!(g.planes(), w.planes(), "{name}");
            assert_eq!(
                g.meta.source.map(|s| (s.bits, s.float)),
                Some((bits, false))
            );
        }
    }

    /// 上限は確保の前（長さ・チャンネル）。入力の長さも `max_value_bytes` まで。
    #[test]
    fn limits_are_checked_before_reading() {
        let _g = shared();
        let dec = decoder();
        let m4a = engine_fixture("aac-he.m4a");
        for limits in [
            Limits {
                max_audio_frames: 1_000,
                ..Limits::default()
            },
            Limits {
                max_channels: 1,
                ..Limits::default()
            },
            Limits {
                max_value_bytes: 100,
                ..Limits::default()
            },
        ] {
            assert!(
                matches!(
                    read(&dec, &m4a, "m4a", &limits, &NoMonitor),
                    Err(EngineError::Limit(_))
                ),
                "{limits:?}"
            );
        }
    }

    /// 中止：チャンク（65,536 フレーム）の合間の、1〜3 回目の確認のどこでも止まる。
    #[test]
    fn cancels_between_chunks() {
        let _g = shared();
        let dec = decoder();
        // 200,000 フレームの 16 ビットの WAV（チャンクが 4 つ）
        let mut wav = Vec::new();
        let frames: u32 = 200_000;
        let data = frames * 4;
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&(36 + data).to_le_bytes());
        wav.extend_from_slice(b"WAVEfmt ");
        wav.extend_from_slice(&16u32.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes());
        wav.extend_from_slice(&2u16.to_le_bytes());
        wav.extend_from_slice(&48_000u32.to_le_bytes());
        wav.extend_from_slice(&(48_000u32 * 4).to_le_bytes());
        wav.extend_from_slice(&4u16.to_le_bytes());
        wav.extend_from_slice(&16u16.to_le_bytes());
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&data.to_le_bytes());
        for i in 0..frames * 2 {
            wav.extend_from_slice(&((i % 2000) as i16 - 1000).to_le_bytes());
        }
        let v = read(&dec, &wav, "wav", &Limits::default(), &NoMonitor).unwrap();
        assert_eq!(audio(&v).frames(), 200_000);
        for after in 1..=3 {
            assert!(
                matches!(
                    read(
                        &dec,
                        &wav,
                        "wav",
                        &Limits::default(),
                        &CancelAfter::new(after)
                    ),
                    Err(EngineError::Canceled)
                ),
                "{after}"
            );
        }
    }

    /// 壊れた入力（切った・変異させた m4a・Ogg Opus、ゴミ）でプロセスが落ちず、ハンドルが漏れない。
    #[test]
    fn broken_audio_fails_without_crashing() {
        let _g = exclusive();
        let (live, _) = stats();
        let dec = decoder();
        let read_any = |bytes: &[u8], format: &str| {
            let _ = read(&dec, bytes, format, &Limits::default(), &NoMonitor);
        };
        let m4a = engine_fixture("aac-he.m4a");
        let opus = engine_fixture("ogg-opus.ogg");
        for pct in [1, 10, 50, 90, 99] {
            read_any(&m4a[..m4a.len() * pct / 100], "m4a");
            read_any(&opus[..opus.len() * pct / 100], "ogg");
        }
        assert!(read(&dec, b"not audio", "m4a", &Limits::default(), &NoMonitor).is_err());
        assert!(read(&dec, &[], "wav", &Limits::default(), &NoMonitor).is_err());
        let mut rng = kura_engine::determinism::rng(0xa0d1);
        for (seed, format) in [(&m4a, "m4a"), (&opus, "ogg")] {
            for _ in 0..100 {
                let mut b = seed.clone();
                for _ in 0..1 + rng.next_u32() % 8 {
                    let i = rng.next_u32() as usize % b.len();
                    b[i] = rng.next_u32() as u8;
                }
                read_any(&b, format);
            }
        }
        assert_eq!(stats().0, live);
    }
}
