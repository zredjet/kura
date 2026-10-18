//! 能力の表（docs/editing.md §9.3）。組み立てた `CodecSet` が、今の環境で形式ごとに読める・書ける・保存できるもの・
//! 再現できるか・選べる設定・どの段のどのコーデックかを返す。
//!
//! - 環境は組み立てた `CodecSet` そのもの（kura-core・kura-mac が、固定の段に Platform・External のコーデックを
//!   `add_decoder`・`add_encoder` で足して組む。macOS は `kura_codec_caps`、ffmpeg は `-encoders` などを起動時に調べて
//!   何を足すかを決める）。この表は登録されたコーデックの `can_decode`・`writes_format`・`reproducible`・`caps`・
//!   `options`・`env` から組むので、描画の中で OS やツールに問い合わせない。
//! - 形式の表（`FORMATS`）の順に、表のすべての形式を返す（読めない・書けない形式は `read`・`write` が空）。表にない
//!   内部の形式（アンカーなど）は出さない。旧版のデコーダは出さない（ふつうの読み取りでは選ばないため）。
//! - 値に合うかの細かい判断（チャンネルの数・レートの範囲）は、値の形が分かってから `CodecSet::encoder(format, shape)`
//!   で行う。既定でない書き手を選んだときは `CodecSet::encode_bytes_with(WriteCap.codec, …)` で書く（その書き手が値の形を
//!   書けなければ `Unsupported`）。

use serde::Serialize;

use crate::codec::formats::{FORMATS, FormatDesc, ItemKind, MetaCaps, format_by_id};
use crate::codec::{CodecEnv, CodecSet, Tier};
use crate::op::ParamSchema;
use crate::shape::{Dim, Fullness, Shape, ValueKind};
use crate::value::PixelFormat;

/// 読み手 1 つ（段の順に並べる。先頭がふつうの読み取りで最初に試すもの）。
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadCap {
    pub codec: &'static str,
    pub tier: Tier,
    /// どの環境でも同じ値を読むか（偽なら、読んだ元に初めて編集するときにアンカーを作る。§9.4）。
    pub reproducible: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub env: Option<CodecEnv>,
}

/// 書き手 1 つ（段の順に並べる。先頭が既定：`CodecSet::encoder` と同じ選び方）。
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WriteCap {
    /// エンコーダの識別子。既定でない書き手で書くときは、そのまま `CodecSet::encode_bytes_with` に渡す。
    pub codec: &'static str,
    pub tier: Tier,
    /// 同じ値・同じ設定なら同じバイト列を書くか（R2）。
    pub reproducible: bool,
    /// 書ける値の種類（形式の表の `decodes_to` のうち、アニメを書けなければ `Frames` を除いたもの）。
    pub kinds: Vec<ValueKind>,
    /// 既定の設定（`{}`）で実際に保存できるもの（`Encoder::caps`）。
    pub meta: MetaCaps,
    /// 選べる設定（`Encoder::options`）。
    pub options: ParamSchema,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub env: Option<CodecEnv>,
}

/// 形式 1 つの能力。
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FormatCaps {
    pub format: &'static str,
    pub item_kind: ItemKind,
    pub exts: &'static [&'static str],
    pub mime: &'static str,
    /// 読んだときの値の型（形式の表）。
    pub decodes_to: &'static [ValueKind],
    /// 形式そのものが保存できるもの（形式の表）。実際に保存できるものは書き手ごとの `meta`。
    pub format_meta: MetaCaps,
    /// 読み手（段の順）。空なら今の環境では読めない。
    pub read: Vec<ReadCap>,
    /// 書き手（段の順）。空なら今の環境では書けない。
    pub write: Vec<WriteCap>,
}

impl FormatCaps {
    /// 今の環境で読めるか。
    pub fn readable(&self) -> bool {
        !self.read.is_empty()
    }

    /// 今の環境で書けるか。
    pub fn writable(&self) -> bool {
        !self.write.is_empty()
    }

    /// 最初に試す読み手が値を再現できるか（読めなければ `None`）。後ろの段（固定の段が断った変種を読む Platform など）は
    /// 再現を約束しないことがある。実際にアンカーが要るかは、ファイルごとの読み取りの記録（`DecodeStamp.reproducible`）で決まる。
    pub fn read_reproducible(&self) -> Option<bool> {
        self.read.first().map(|r| r.reproducible)
    }

    /// 既定の書き手（段の順で最初のもの）。
    pub fn default_writer(&self) -> Option<&WriteCap> {
        self.write.first()
    }
}

impl CodecSet {
    /// 能力の表（形式の表の順。§9.3）。
    pub fn capabilities(&self) -> Vec<FormatCaps> {
        FORMATS.iter().map(|f| self.caps_for(f)).collect()
    }

    /// 1 つの形式の能力（形式の表にない形式は `None`）。
    pub fn format_caps(&self, format: &str) -> Option<FormatCaps> {
        format_by_id(format).map(|f| self.caps_for(f))
    }

    fn caps_for(&self, f: &'static FormatDesc) -> FormatCaps {
        let read = self
            .decoders_for(f.id)
            .into_iter()
            .map(|d| ReadCap {
                codec: d.id(),
                tier: d.tier(),
                reproducible: d.reproducible(),
                env: d.env(),
            })
            .collect();
        let write = self
            .encoders
            .iter()
            .filter(|e| e.writes_format(f.id))
            .map(|e| {
                let meta = e.caps(f.id, &serde_json::json!({}));
                WriteCap {
                    codec: e.id(),
                    tier: e.tier(),
                    reproducible: e.reproducible(),
                    kinds: f
                        .decodes_to
                        .iter()
                        .copied()
                        .filter(|k| *k != ValueKind::Frames || meta.animation)
                        .collect(),
                    meta,
                    options: e.options(f.id),
                    env: e.env(),
                }
            })
            .collect();
        FormatCaps {
            format: f.id,
            item_kind: f.item_kind,
            exts: f.exts,
            mime: f.mime,
            decodes_to: f.decodes_to,
            format_meta: f.caps,
            read,
            write,
        }
    }
}

/// 代表の音声のサンプルレート（`probe_shapes`）。
const PROBE_RATES: [u32; 10] = [
    8_000, 11_025, 16_000, 22_050, 32_000, 44_100, 48_000, 88_200, 96_000, 192_000,
];
/// 代表の音声のチャンネルの数（`probe_shapes`）。
const PROBE_CHANNELS: [u16; 3] = [1, 2, 6];
/// 代表の音声の長さ（`probe_shapes`）。空の音声を断るエンコーダもあるので 0 にしない。
const PROBE_FRAMES: u64 = 4_096;

/// 形式の表の値の種類ごとの代表の形（`Encoder::writes_format` の既定の実装が、どれかを `can_encode` が受け付けるかを見る）。
/// 画像は 1×1 の全画素の形式、フレームの列は 2 フレーム、音声は代表のレート × チャンネル。形式の表にない形式は空。
pub fn probe_shapes(format: &str) -> Vec<Shape> {
    let Some(f) = format_by_id(format) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for kind in f.decodes_to {
        match kind {
            ValueKind::Image => out.extend(PixelFormat::ALL.iter().map(|fmt| Shape::Image {
                w: Dim::Exact(1),
                h: Dim::Exact(1),
                fmt: *fmt,
            })),
            ValueKind::Frames => out.extend(PixelFormat::ALL.iter().map(|fmt| Shape::Frames {
                count: Dim::Exact(2),
                w: Dim::Exact(1),
                h: Dim::Exact(1),
                fmt: *fmt,
                full: Fullness::Full,
            })),
            ValueKind::Audio => {
                for rate in PROBE_RATES {
                    for channels in PROBE_CHANNELS {
                        out.push(Shape::Audio {
                            rate,
                            channels,
                            frames: Dim::Exact(PROBE_FRAMES),
                        });
                    }
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::codec::formats::AlphaCaps;
    use crate::testgen::InputGen;
    use crate::testing::platform::{FakePlatformDecoder, FakePlatformEncoder};

    fn caps(set: &CodecSet, format: &str) -> FormatCaps {
        set.format_caps(format).expect("形式の表にある")
    }

    /// 形式の表の順にすべての形式が出る。表にない形式は出ない。
    #[test]
    fn every_format_in_table_order() {
        let set = CodecSet::pinned();
        let all = set.capabilities();
        assert_eq!(
            all.iter().map(|c| c.format).collect::<Vec<_>>(),
            FORMATS.iter().map(|f| f.id).collect::<Vec<_>>()
        );
        assert!(set.format_caps("anchor").is_none());
        assert!(set.format_caps("txt").is_none());
        for c in &all {
            let f = format_by_id(c.format).expect("表の行");
            assert_eq!(
                (c.item_kind, c.exts, c.mime, c.decodes_to, c.format_meta),
                (f.item_kind, f.exts, f.mime, f.decodes_to, f.caps)
            );
        }
    }

    /// 固定の段だけの組：書けるのは決めた 8 つの形式、読み手・書き手の記録はコーデックの答えと食い違わない。
    #[test]
    fn pinned_table_matches_registered_codecs() {
        let set = CodecSet::pinned();
        let all = set.capabilities();
        let writable: Vec<&str> = all
            .iter()
            .filter(|c| c.writable())
            .map(|c| c.format)
            .collect();
        assert_eq!(
            writable,
            ["png", "jpeg", "gif", "webp", "wav", "aiff", "flac", "ogg"]
        );
        for c in &all {
            for r in &c.read {
                let d = set.decoder_by_id(r.codec).expect("登録されている");
                assert_eq!(
                    (r.tier, r.reproducible, &r.env),
                    (d.tier(), d.reproducible(), &None)
                );
                assert!(d.can_decode(c.format));
            }
            for w in &c.write {
                let e = set.encoder_by_id(w.codec).expect("登録されている");
                assert_eq!(w.tier, Tier::Pinned);
                assert!(w.reproducible && w.env.is_none());
                assert_eq!(w.meta, e.caps(c.format, &serde_json::json!({})));
                assert_eq!(w.options, e.options(c.format));
                assert_eq!(w.kinds.contains(&ValueKind::Frames), w.meta.animation);
                assert!(w.kinds.iter().all(|k| c.decodes_to.contains(k)));
            }
        }
        // 段 9 より前から決まっている行
        let png = caps(&set, "png");
        assert_eq!(png.read_reproducible(), Some(true));
        let w = png.default_writer().expect("png を書ける");
        assert_eq!(w.codec, "png-rs-enc/1");
        assert_eq!(w.kinds, [ValueKind::Image, ValueKind::Frames]);
        assert!(w.meta.lossless && w.meta.icc && w.meta.alpha == AlphaCaps::Full);
        let jpeg = caps(&set, "jpeg");
        assert_eq!(jpeg.write[0].kinds, [ValueKind::Image]);
        assert!(!jpeg.write[0].meta.lossless);
        let wav = caps(&set, "wav");
        assert_eq!(wav.read[0].codec, "wav-kura-dec/1");
        assert!(wav.write[0].meta.loop_points);
        // 今の固定の段では読めない・書けない
        for format in ["heic", "heif", "avif", "tiff", "bmp", "opus"] {
            assert!(!caps(&set, format).readable(), "{format}");
        }
        for format in ["mp3", "m4a", "aac", "opus", "heic", "mp4", "ttf"] {
            assert!(!caps(&set, format).writable(), "{format}");
        }
        assert!(caps(&set, "mp3").readable());
    }

    /// Platform のコーデックを足すと、固定の段の後ろに、再現を約束しない読み手・書き手として出る（環境の記録つき）。
    #[test]
    fn platform_codecs_follow_the_pinned_tier() {
        let env = CodecEnv::new("macOS", "26.7 (25G229)").with("imageio", "2784.6.4");
        let mut set = CodecSet::pinned();
        set.add_decoder(Arc::new(
            FakePlatformDecoder::new(
                "fake-imageio-dec/1",
                &["heic", "avif", "jpeg"],
                InputGen::Noise {
                    w: 2,
                    h: 2,
                    fmt: PixelFormat::Rgb8,
                    seed: 1,
                },
            )
            .with_env(env.clone()),
        ))
        .expect("足せる");
        set.add_encoder(Arc::new(
            FakePlatformEncoder::new("fake-aac-enc/1", &["m4a"]).with_env(env.clone()),
        ))
        .expect("足せる");
        let heic = caps(&set, "heic");
        assert_eq!(
            heic.read,
            [ReadCap {
                codec: "fake-imageio-dec/1",
                tier: Tier::Platform,
                reproducible: false,
                env: Some(env.clone()),
            }]
        );
        assert_eq!(heic.read_reproducible(), Some(false));
        // 固定の段が先、Platform は後ろ（CMYK の JPEG など、固定の段が断った変種だけが来る）
        let jpeg = caps(&set, "jpeg");
        assert_eq!(
            jpeg.read.iter().map(|r| r.tier).collect::<Vec<_>>(),
            [Tier::Pinned, Tier::Platform]
        );
        assert_eq!(jpeg.read_reproducible(), Some(true));
        let m4a = caps(&set, "m4a");
        let w = m4a.default_writer().expect("m4a を書ける");
        assert_eq!(
            (w.codec, w.tier, w.reproducible, w.kinds.as_slice()),
            (
                "fake-aac-enc/1",
                Tier::Platform,
                false,
                &[ValueKind::Audio][..]
            )
        );
        assert_eq!(w.env, Some(env));
        assert!(!caps(&set, "mp3").writable());
        // JSON に環境が出る（固定の段の行には env の項目がない）
        let json = serde_json::to_value(set.capabilities()).expect("JSON にできる");
        let rows = json.as_array().expect("配列");
        let png = rows.iter().find(|r| r["format"] == "png").expect("png");
        assert!(png["read"][0].get("env").is_none());
        let heic = rows.iter().find(|r| r["format"] == "heic").expect("heic");
        assert_eq!(heic["read"][0]["env"]["components"]["imageio"], "2784.6.4");
        assert_eq!(heic["read"][0]["tier"], "platform");
    }

    /// 代表の形：形式の表の値の種類ごとに出る。表にない形式は空。
    #[test]
    fn probe_shapes_cover_value_kinds() {
        assert_eq!(probe_shapes("jpeg").len(), PixelFormat::ALL.len());
        assert_eq!(probe_shapes("png").len(), PixelFormat::ALL.len() * 2);
        assert_eq!(
            probe_shapes("wav").len(),
            PROBE_RATES.len() * PROBE_CHANNELS.len()
        );
        assert!(probe_shapes("mp4").is_empty());
        assert!(probe_shapes("nope").is_empty());
        assert!(
            probe_shapes("ogg")
                .iter()
                .all(|s| matches!(s, Shape::Audio { frames, .. } if frames.max() > 0))
        );
    }
}
