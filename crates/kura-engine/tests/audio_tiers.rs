//! 自作の音声の読み手の段の振る舞い（M16 段 10 で、symphonia の写しの確かめ `tests/symphonia_copies.rs` から、自作の読み手に
//! かかわる部分を移した。フィクスチャの作り方は `fixtures/README.md`）。
//!
//! - 壊れた Vorbis が出しうる NaN：値に入れる前に `0x7fc00000` にそろえるので、読んだ値は生のビットまで arm64 と x86_64 で
//!   同じ（f32 の WAV に書いたバイト列も）。凍結した値と照らすので、両方のターゲットで回す：
//!   `cargo test -p kura-engine --test suite audio_tiers::` と、同じものに `--target x86_64-apple-darwin`（Rosetta）。
//! - 帯域の中の SBR（暗黙の HE-AAC）：固定の段の AAC の読み手（`m4a-aac-kura-dec/1`・`aac-adts-kura-dec/1`）は、HE-AAC の
//!   ADTS・ASC に印のない m4a を断って後の段（AudioToolbox・ffmpeg）へ回す。AAC-LC は固定の段で読む（値はデコーダの
//!   ゴールデンと `tests/fixtures.rs` が見る）。

// テストの補助関数は #[test] の外にあるので、テストと同じく unwrap などを許す（clippy.toml の allow-*-in-tests が届かない）
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::io::Cursor;
use std::path::PathBuf;
use std::sync::Arc;

use kura_engine::codec::{CodecSet, DecodeStamp, Decoder, ReadSeek, Tier};
use kura_engine::doc::DecodeOptions;
use kura_engine::value::{Audio, AudioMeta};
use kura_engine::{Dim, EngineError, Limits, Monitor, NoMonitor, Shape, Value};

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

/// 自作の読み手（`ogg-vorbis-kura-dec/1`。M16 段 2）も、同じ壊れた Vorbis の NaN を `0x7fc00000` にそろえ、どのターゲットでも
/// 凍結した値とビット単位で同じ（自作の読み手は libvorbis と同じく、この 2 本では NaN を出さない。出したときは
/// `common::canonical_nan` でそろえる）。
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

/// AAC-LC（SBR の拡張のないもの）は、後の段があっても今までどおり固定の段（M16 段 4 から自作の読み手）で読む。
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
    }
}
