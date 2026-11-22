//! symphonia の写し（`third_party/`）と、それを使う固定の段の読み取り（`codec/symph.rs`・`codec/lossy.rs`）の確かめ
//! （段 9 の検証の指摘。フィクスチャの作り方は `fixtures/README.md`）。
//!
//! - 壊れた Vorbis が出す NaN：値に入れる前に `0x7fc00000` にそろえるので、読んだ値は生のビットまで arm64 と x86_64 で
//!   同じ（f32 の WAV に書いたバイト列も）。凍結した値と照らすので、両方のターゲットで回す：
//!   `cargo test -p kura-engine --test suite symphonia_copies::` と、同じものに `--target x86_64-apple-darwin`（Rosetta）。
//! - 帯域の中の SBR（暗黙の HE-AAC）：写しのデコーダが fill 要素の SBR の拡張を知らせ（`AacDecoder::sbr_seen`）、固定の段は
//!   HE-AAC の ADTS・ASC に印のない m4a を断って後の段（AudioToolbox・ffmpeg）へ回す。AAC-LC は今までどおり固定の段で読む
//!   （値はデコーダのゴールデンと `tests/fixtures.rs` が見る）。M16 段 4 で固定の段の AAC は自作の `m4a-aac-kura-dec/1`・
//!   `aac-adts-kura-dec/1` になったので、同じ確かめを自作の読み手（固定の段）と、旧版として残した写し（`…-symphonia-dec/2`
//!   を名指し）の両方で行う。
//! - 写しが組まれていること自体は、`codec/lossy.rs` が写しにだけある印 `KURA_PATCHED` を参照して、組み立てで確かめる。

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::io::Cursor;
use std::path::PathBuf;
use std::sync::Arc;

use kura_engine::codec::{CodecSet, DecodeStamp, Decoder, ReadSeek, Tier};
use kura_engine::doc::DecodeOptions;
use kura_engine::value::{Audio, AudioMeta};
use kura_engine::{Dim, EngineError, Limits, Monitor, NoMonitor, Shape, Value};
use symphonia_core::codecs::CodecParameters;
use symphonia_core::codecs::audio::{AudioDecoder as _, AudioDecoderOptions};
use symphonia_core::formats::{FormatOptions, FormatReader, TrackType};
use symphonia_core::io::{MediaSourceStream, MediaSourceStreamOptions};

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures")
            .join(name),
    )
    .unwrap()
}

fn decode_in(
    set: &CodecSet,
    name: &str,
    format: &str,
) -> Result<(Value, DecodeStamp), EngineError> {
    set.decode(
        &mut Cursor::new(fixture(name)),
        format,
        &DecodeOptions::default(),
        &Limits::default(),
        &NoMonitor,
    )
}

/// 識別子 `id` のデコーダ（旧版も）で形を読む。
fn shape_by(set: &CodecSet, id: &str, name: &str, format: &str) -> Result<Shape, EngineError> {
    set.decoder_by_id(id).unwrap().shape(
        &mut Cursor::new(fixture(name)),
        format,
        &DecodeOptions::default(),
        &Limits::default(),
    )
}

/// 識別子 `id` のデコーダ（旧版も）で読む。
fn decode_by(set: &CodecSet, id: &str, name: &str, format: &str) -> Result<Value, EngineError> {
    set.decode_with(
        id,
        &mut Cursor::new(fixture(name)),
        format,
        &DecodeOptions::default(),
        &Limits::default(),
        &NoMonitor,
    )
    .map(|(v, _)| v)
}

/// 旧版の写しの識別子（入れ物ごと）。
fn legacy_aac(format: &str) -> &'static str {
    if format == "aac" {
        "aac-adts-symphonia-dec/2"
    } else {
        "m4a-aac-symphonia-dec/2"
    }
}

fn shape_in(
    set: &CodecSet,
    name: &str,
    format: &str,
) -> Result<(Shape, &'static str), EngineError> {
    set.shape(
        &mut Cursor::new(fixture(name)),
        format,
        &DecodeOptions::default(),
        &Limits::default(),
    )
}

/// 壊れた Vorbis（`ogg-vorbis-3ch.ogg`・`ogg-vorbis-ffmpeg.ogg` の数バイトを変えて CRC を直したもの）は、デコードの途中で
/// inf − inf などが起きて NaN を出す。NaN のビット列は CPU の既定の NaN（x86 の SSE は `0xffc00000`、ARM は
/// `0x7fc00000`）と演算の順で決まり、直す前は arm64 と x86_64 で生のビットも、f32 の WAV に書いたバイト列も違った（値の
/// ダイジェストは NaN をそろえるので同じ）。読み取りが NaN を `0x7fc00000` にそろえるので、どのターゲットでも凍結した値と
/// ビット単位で同じになる。値のダイジェストは直す前と同じ（デコーダの識別子は変えない）。
///
/// M16 段 2 で今の読み手は自作の `ogg-vorbis-kura-dec/1` になったので、ここでは旧版として残した symphonia の写し
/// （`ogg-vorbis-symphonia-dec/3`）を名指しして読む。自作の読み手の同じ確かめは `kura_vorbis_nans_have_the_same_bits_on_every_target`。
#[test]
fn broken_vorbis_nans_have_the_same_bits_on_every_target() {
    check_nan_bits(
        "ogg-vorbis-symphonia-dec/3",
        &[
            (
                "ogg-vorbis-3ch-nan.ogg",
                34_628,
                "5f884da681a1a5571086cbb040516601820674e3c1b381bdf00d5343293f0e41",
                "b0a276b3aa0a8feb868891f23a867942d180a9effdd2b65077a6fbc3d72c1403",
                "231430ff203985726ddca36ac9a064a6933d8aa04f978247dd08d173d662ed82",
            ),
            (
                "ogg-vorbis-ffmpeg-nan.ogg",
                22_784,
                "58cc551eace11bdcbb251c08a7d5921e40315c908be054d812501a1c53ff6c9a",
                "87d3867cbe6f2ee43fb52f036b1e3d0cdefbe47a7d5186bab10eb23947782b12",
                "ffb7b96cb713d66e7287e7f29c9155b351f03f29e2e6b4544186e85e5fbe4dfe",
            ),
        ],
    );
}

/// 自作の読み手（`ogg-vorbis-kura-dec/1`。M16 段 2）も、同じ壊れた Vorbis の NaN を `0x7fc00000` にそろえ、どのターゲットでも
/// 凍結した値とビット単位で同じ（自作の読み手は libvorbis と同じく、この 2 本では NaN を出さない。出したときは
/// `symph::canonical_nan` でそろえる）。
#[test]
fn kura_vorbis_nans_have_the_same_bits_on_every_target() {
    check_nan_bits(
        "ogg-vorbis-kura-dec/1",
        &[
            (
                "ogg-vorbis-3ch-nan.ogg",
                0,
                "04083100a00848f1e5b94de33043e4769ef9aa099f7ffc1eb62dadafd74ab3e9",
                "6c92e621b82282336889d6715f32e311d032908653c194d2bf3d38dae9395d1c",
                "3090205fff6b55223793bb305bb446083c9e97df0001888e769b71fe723f8377",
            ),
            (
                "ogg-vorbis-ffmpeg-nan.ogg",
                0,
                "553c93e83c749d76fa11a5707d30966ff69a775e38040c5280210c3dccd1f9dc",
                "8bcf2b392ddef45efd212c14e69f739fe1d6ef50572922fc4bd1d2d48f57627a",
                "145a46673ad4d8dc912928cdbfb2908770e4298bbf60fd3f092440f9417e378a",
            ),
        ],
    );
}

/// 識別子 `decoder` で読み、NaN の数・値のダイジェスト・生のビット・f32 の WAV のダイジェストを凍結した値と照らす。
fn check_nan_bits(decoder: &str, cases: &[(&str, usize, &str, &str, &str)]) {
    for &(file, nans, value, raw, wav) in cases {
        let set = CodecSet::pinned();
        let (v, stamp) = set
            .decode_with(
                decoder,
                &mut Cursor::new(fixture(file)),
                "ogg",
                &DecodeOptions::default(),
                &Limits::default(),
                &NoMonitor,
            )
            .unwrap();
        assert_eq!(stamp.codec, decoder, "{file}");
        assert!(stamp.reproducible, "{file}");
        let Value::Audio(a) = &v else {
            panic!("{file}")
        };
        let mut h = blake3::Hasher::new();
        let mut count = 0usize;
        for p in a.to_planes().unwrap() {
            for s in p.iter() {
                h.update(&s.to_bits().to_le_bytes());
                if s.is_nan() {
                    assert_eq!(s.to_bits(), 0x7fc0_0000, "{file}：NaN のビット列");
                    count += 1;
                }
            }
        }
        let (_, enc) = set
            .encode_bytes(&v, "wav", &serde_json::json!({ "float": true }), &NoMonitor)
            .unwrap();
        let got = (
            count,
            stamp.value_digest.to_hex(),
            h.finalize().to_hex().to_string(),
            enc.file_digest.to_hex(),
        );
        assert_eq!(
            got,
            (nans, value.into(), raw.into(), wav.into()),
            "{file}：NaN の数・値のダイジェスト・生のビット・f32 の WAV"
        );
    }
}

/// 読み口を開き、AAC のデコーダで全部のパケットを読んで、SBR の拡張を最初に知らせたパケットの番号を返す（コーデックの
/// パラメータのサンプルレートと ASC も返す）。一度知らせたら最後まで知らせ続けること、知らせても値は返ることも確かめる。
fn first_sbr_packet(name: &str) -> (u32, Option<Vec<u8>>, Option<usize>) {
    let mss = MediaSourceStream::new(
        Box::new(Cursor::new(fixture(name))),
        MediaSourceStreamOptions::default(),
    );
    let mut reader: Box<dyn FormatReader> = if name.ends_with(".aac") {
        Box::new(symphonia_codec_aac::AdtsReader::try_new(mss, FormatOptions::default()).unwrap())
    } else {
        Box::new(
            symphonia_format_isomp4::IsoMp4Reader::try_new(mss, FormatOptions::default()).unwrap(),
        )
    };
    let track = reader.default_track(TrackType::Audio).cloned().unwrap();
    let params = track
        .codec_params
        .as_ref()
        .and_then(CodecParameters::audio)
        .cloned()
        .unwrap();
    let mut dec =
        symphonia_codec_aac::AacDecoder::try_new(&params, &AudioDecoderOptions::default()).unwrap();
    let mut first = None;
    let mut i = 0usize;
    while let Some(pkt) = reader.next_packet().unwrap() {
        if pkt.track_id != track.id {
            continue;
        }
        let was = dec.sbr_seen();
        let frames = dec.decode(&pkt).unwrap().frames();
        assert_eq!(frames, 1_024, "{name}：パケット {i}");
        assert!(!was || dec.sbr_seen(), "{name}：知らせは消えない");
        if dec.sbr_seen() && first.is_none() {
            first = Some(i);
        }
        i += 1;
    }
    (
        params.sample_rate.unwrap(),
        params.extra_data.map(|b| b.to_vec()),
        first,
    )
}

/// 写しのデコーダは、fill 要素の SBR の拡張（`EXT_SBR_DATA`・`EXT_SBR_DATA_CRC`）を見つけたことを知らせる。HE-AAC
/// （afconvert の aach）は最初のパケットから、LC のあとに HE-AAC をつないだものは HE-AAC の最初のパケット（24）から。
/// AAC-LC（afconvert・ffmpeg）は最後まで知らせない。ADTS の HE-AAC と、ADTS から写した m4a は、コーデックのパラメータ
/// では LC にしか見えない（ADTS の見出しは芯の 22,050 Hz。m4a は入れ物が 44,100 Hz と書くが、ASC は 22,050 Hz の LC で、
/// SBR の印がない）。
#[test]
fn aac_decoder_reports_in_band_sbr() {
    // ASC：オブジェクトの種類 2（LC）・22,050 Hz・2 チャンネル。同期の拡張（明示の印）がない
    let implicit = Some(vec![0x13, 0x90]);
    for (file, rate, asc, first) in [
        ("aac-he.aac", 22_050, None, Some(0)),
        ("aac-he-implicit.m4a", 44_100, implicit, Some(0)),
        ("aac-lc-then-he.aac", 22_050, None, Some(24)),
        ("aac-lc.aac", 44_100, None, None),
        (
            "aac-lc-ffmpeg.m4a",
            44_100,
            Some(vec![0x12, 0x10, 0x56, 0xE5, 0x00]),
            None,
        ),
        ("aac-lc-afconvert.m4a", 44_100, Some(vec![0x12, 0x10]), None),
    ] {
        assert_eq!(first_sbr_packet(file), (rate, asc, first), "{file}");
    }
}

/// 後の段の代わり（Platform。決まった値を返す）。固定の段が断ったものがここへ回ることを確かめる。
struct LaterTier;

impl Decoder for LaterTier {
    fn id(&self) -> &'static str {
        "test-later-dec/1"
    }
    fn tier(&self) -> Tier {
        Tier::Platform
    }
    fn can_decode(&self, format: &str) -> bool {
        matches!(format, "aac" | "m4a")
    }
    fn shape(
        &self,
        _src: &mut dyn ReadSeek,
        _format: &str,
        _opts: &DecodeOptions,
        _limits: &Limits,
    ) -> Result<Shape, EngineError> {
        Ok(Shape::Audio {
            rate: 44_100,
            channels: 2,
            frames: Dim::Exact(4),
        })
    }
    fn decode(
        &self,
        _src: &mut dyn ReadSeek,
        _format: &str,
        _opts: &DecodeOptions,
        _limits: &Limits,
        _mon: &dyn Monitor,
    ) -> Result<Value, EngineError> {
        let plane = Arc::new(vec![0.0f32; 4]);
        let a = Audio::new(44_100, vec![plane.clone(), plane], AudioMeta::default())?;
        Ok(Value::Audio(Arc::new(a)))
    }
}

fn with_later_tier() -> CodecSet {
    let mut set = CodecSet::pinned();
    set.add_decoder(Arc::new(LaterTier)).unwrap();
    set
}

/// 帯域の中だけに SBR の印がある HE-AAC（ADTS・ASC に印のない m4a）と、ASC に明示の印がある m4a を、固定の段は形の段階で
/// 断り（`Unsupported`）、後の段が読む。直す前は、ADTS と印のない m4a を固定の段が SBR を捨てて 22,050 Hz の LC の芯
/// として読み、後の段（AudioToolbox・ffmpeg）に届かなかった（段 9 の検証の T5）。
#[test]
fn in_band_he_aac_falls_through_to_later_tiers() {
    for (file, format) in [
        ("aac-he.aac", "aac"),
        ("aac-he-implicit.m4a", "m4a"),
        ("aac-he.m4a", "m4a"),
    ] {
        let pinned = CodecSet::pinned();
        assert!(
            matches!(
                shape_in(&pinned, file, format),
                Err(EngineError::Unsupported(_))
            ),
            "{file}"
        );
        // 旧版の写し（名指し）も断る
        let legacy = legacy_aac(format);
        assert!(
            matches!(
                shape_by(&pinned, legacy, file, format),
                Err(EngineError::Unsupported(_))
            ),
            "{file}：{legacy}"
        );
        assert!(
            matches!(
                decode_by(&pinned, legacy, file, format),
                Err(EngineError::Unsupported(_))
            ),
            "{file}：{legacy}"
        );
        assert!(
            matches!(
                decode_in(&pinned, file, format),
                Err(EngineError::Unsupported(_))
            ),
            "{file}"
        );
        let set = with_later_tier();
        assert_eq!(
            shape_in(&set, file, format).unwrap().1,
            "test-later-dec/1",
            "{file}"
        );
        let (_, stamp) = decode_in(&set, file, format).unwrap();
        assert_eq!(
            (stamp.codec.as_str(), stamp.tier),
            ("test-later-dec/1", Tier::Platform),
            "{file}"
        );
    }
}

/// 先頭の 16 パケット（形を決めるときに見る数）より後で初めて SBR が出てくるもの（LC の 24 パケットのあとに HE-AAC を
/// つないだ ADTS）：形は固定の段が LC として答えるが、読むときに全部のパケットを見て断り、半分のレートの値を返さずに
/// 後の段へ回す。
#[test]
fn late_sbr_is_refused_when_reading() {
    let file = "aac-lc-then-he.aac";
    for set in [CodecSet::pinned(), with_later_tier()] {
        let (shape, id) = shape_in(&set, file, "aac").unwrap();
        assert_eq!(id, "aac-adts-kura-dec/1");
        // 旧版の写しも同じ形を答え、読むときに断る
        assert_eq!(
            shape_by(&set, "aac-adts-symphonia-dec/2", file, "aac").unwrap(),
            shape
        );
        assert!(matches!(
            decode_by(&set, "aac-adts-symphonia-dec/2", file, "aac"),
            Err(EngineError::Unsupported(_))
        ));
        assert!(
            matches!(
                shape,
                Shape::Audio {
                    rate: 22_050,
                    channels: 2,
                    ..
                }
            ),
            "{shape:?}"
        );
    }
    assert!(matches!(
        decode_in(&CodecSet::pinned(), file, "aac"),
        Err(EngineError::Unsupported(_))
    ));
    let (_, stamp) = decode_in(&with_later_tier(), file, "aac").unwrap();
    assert_eq!(stamp.codec, "test-later-dec/1");
}

/// AAC-LC（SBR の拡張のないもの）は、後の段があっても今までどおり固定の段（M16 段 4 から自作の読み手）で読む。旧版の写しも
/// 名指しすれば同じ長さで読む。
#[test]
fn aac_lc_stays_on_the_pinned_tier() {
    let set = with_later_tier();
    for (file, format, id, rate, frames) in [
        ("aac-lc.aac", "aac", "aac-adts-kura-dec/1", 44_100, 46_080),
        (
            "aac-lc-ffmpeg.m4a",
            "m4a",
            "m4a-aac-kura-dec/1",
            44_100,
            44_100,
        ),
        (
            "aac-lc-afconvert.m4a",
            "m4a",
            "m4a-aac-kura-dec/1",
            44_100,
            44_100,
        ),
    ] {
        let (v, stamp) = decode_in(&set, file, format).unwrap();
        let Value::Audio(a) = &v else {
            panic!("{file}")
        };
        assert_eq!(
            (stamp.codec.as_str(), stamp.tier, a.rate(), a.frames()),
            (id, Tier::Pinned, rate, frames),
            "{file}"
        );
        let Value::Audio(old) = decode_by(&set, legacy_aac(format), file, format).unwrap() else {
            panic!("{file}")
        };
        assert_eq!((old.rate(), old.frames()), (rate, frames), "{file}：旧版");
    }
}
