//! Platform の段のコーデック（docs/editing.md §9.1・§9.2）：ImageIO の読み取り・AudioToolbox の読み取り・AudioToolbox の
//! AAC の書き出し。エンジンの backend の trait（`kura_engine::codec::platform`）を Swift の C の口で実装し、adapter で
//! `Decoder`・`Encoder` にして `CodecSet` に足す。
//!
//! - 段は `Tier::Platform`、値の再現を約束しない（読んだ元に初めて保存するとき（保存のジョブが描くのと同じ読み取りから作る。docs/editing.md §9.4）
//!   にアンカーを作る）。識別子は実装ごとに 1 つ（[`IMAGEIO_DEC`]・[`AUDIOTOOLBOX_DEC`]・[`AAC_AUDIOTOOLBOX_ENC`]）で、版は kura-mac の呼び方とエンジンの変換の約束。
//!   OS・ImageIO・AudioToolbox の版は記録の `env`（[`MacCaps::image_env`]・[`MacCaps::audio_env`]）に出す。旧版は残さない。
//! - 能力（[`codec_caps`]）は起動時に 1 度だけ Swift に問い合わせ、何を登録するかを決める（描画の中で OS に問い合わせない）。
//! - ABI の版と構造体の形が Swift と合わなければ、Platform のコーデックを登録しない（[`platform_status`] が理由を返す）。
//! - macOS 以外では能力がなく、どれも空（固定の段だけ）。

pub mod caps;
mod convert;
mod watchdog;

#[cfg(target_os = "macos")]
mod aac;
#[cfg(target_os = "macos")]
mod audio;
#[cfg(target_os = "macos")]
mod image;

use std::sync::Arc;

use kura_engine::codec::{CodecSet, Decoder, Encoder, PinnedOptions};

pub use caps::{AacChannels, AacRate, MacCaps};

/// ImageIO の読み取りの識別子。
pub const IMAGEIO_DEC: &str = "imageio-dec/1";
/// AudioToolbox の読み取りの識別子。
pub const AUDIOTOOLBOX_DEC: &str = "audiotoolbox-dec/1";
/// AudioToolbox の AAC の書き出しの識別子。
pub const AAC_AUDIOTOOLBOX_ENC: &str = "aac-audiotoolbox-enc/1";

/// macOS の能力と環境（プロセスで 1 度だけ調べる）。macOS 以外・ABI が合わないときは理由。
pub fn codec_caps() -> Result<Arc<MacCaps>, String> {
    #[cfg(target_os = "macos")]
    {
        use std::sync::OnceLock;
        static CAPS: OnceLock<Result<Arc<MacCaps>, String>> = OnceLock::new();
        CAPS.get_or_init(|| {
            crate::bridge::check_abi()?;
            let json = crate::bridge::codec_caps_json()?;
            let caps = MacCaps::parse(&json)?;
            if caps.abi != crate::abi::ABI {
                return Err(format!(
                    "能力の ABI の版 {} が Rust の {} と違います",
                    caps.abi,
                    crate::abi::ABI
                ));
            }
            Ok(Arc::new(caps))
        })
        .clone()
    }
    #[cfg(not(target_os = "macos"))]
    {
        Err("Platform のコーデックは macOS だけです".into())
    }
}

/// Platform のコーデックを使えるか（使えなければ理由）。
pub fn platform_status() -> Result<(), String> {
    codec_caps().map(|_| ())
}

/// Platform のデコーダ（ImageIO・AudioToolbox）。使えなければ空。
pub fn platform_decoders() -> Vec<Arc<dyn Decoder>> {
    #[cfg(target_os = "macos")]
    {
        use kura_engine::codec::platform::{PlatformAudioDecoder, PlatformImageDecoder};
        match codec_caps() {
            Ok(caps) => vec![
                Arc::new(PlatformImageDecoder::new(image::ImageIoBackend::new(&caps)))
                    as Arc<dyn Decoder>,
                Arc::new(PlatformAudioDecoder::new(audio::AudioToolboxBackend::new(
                    &caps,
                ))),
            ],
            Err(e) => {
                tracing::warn!("Platform のデコーダを登録しません: {e}");
                Vec::new()
            }
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        Vec::new()
    }
}

/// Platform のエンコーダ（AudioToolbox の AAC）。使えなければ空。
pub fn platform_encoders() -> Vec<Arc<dyn Encoder>> {
    #[cfg(target_os = "macos")]
    {
        match codec_caps() {
            Ok(caps) => encoders_for(caps),
            Err(e) => {
                tracing::warn!("Platform のエンコーダを登録しません: {e}");
                Vec::new()
            }
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        Vec::new()
    }
}

/// 能力から Platform のエンコーダを組む。AAC の書き手は、AAC-LC のエンコーダとレートの表（書ける組）があるときだけ
/// （Swift の問い合わせが失敗して表が空のときに、能力の表が m4a を書けると偽らない）。
#[cfg(target_os = "macos")]
fn encoders_for(caps: Arc<MacCaps>) -> Vec<Arc<dyn Encoder>> {
    use kura_engine::codec::platform::PlatformAacEncoder;
    if !caps.aac_writable(aac::AAC_MAX_CHANNELS) {
        tracing::warn!(
            "AudioToolbox に AAC-LC のエンコーダか書けるレートの表がないので、AAC の書き手を登録しません"
        );
        return Vec::new();
    }
    vec![Arc::new(PlatformAacEncoder::new(aac::AacToolboxBackend::new(caps))) as Arc<dyn Encoder>]
}

/// 固定の段（`opts` の設定つき）に Platform のコーデックを足した組。External（ffmpeg）は kura-core が足す。
pub fn codec_set(opts: &PinnedOptions) -> CodecSet {
    let mut set = CodecSet::pinned_with(opts);
    for d in platform_decoders() {
        let id = d.id();
        if let Err(e) = set.add_decoder(d) {
            tracing::warn!("デコーダ {id} を足せません: {e}");
        }
    }
    for e in platform_encoders() {
        let id = e.id();
        if let Err(err) = set.add_encoder(e) {
            tracing::warn!("エンコーダ {id} を足せません: {err}");
        }
    }
    set
}

/// アプリで使う全部の組：固定の段 → Platform（macOS）→ External（ffmpeg。`work_dir` は一時ファイルの置き場。見つからなければ
/// 足さない）。ffmpeg の段を足せないときは警告を残し、足せた段だけの組を返す。
pub fn full_codec_set(opts: &PinnedOptions, work_dir: &std::path::Path) -> CodecSet {
    let mut set = codec_set(opts);
    if let Err(e) = kura_core::codec_ext::ffmpeg::add_ffmpeg_codecs(&mut set, work_dir) {
        tracing::warn!("ffmpeg の段を足せません: {e}");
    }
    set
}

/// 解析のジョブを ImageIO で動かすテスト（決定 15）。
#[cfg(test)]
#[cfg(target_os = "macos")]
mod analysis_tests;

/// テストの共通の部品（macOS）。
#[cfg(test)]
#[cfg(target_os = "macos")]
pub(crate) mod testutil {
    use std::io::Cursor;
    use std::path::PathBuf;
    use std::process::Command;
    use std::sync::{Arc, RwLock, RwLockReadGuard, RwLockWriteGuard};

    use kura_engine::codec::{CodecSet, DecodeStamp, PinnedOptions};
    use kura_engine::doc::DecodeOptions;
    use kura_engine::testgen::InputGen;
    use kura_engine::value::{Image, ImageMeta, PixelFormat, Raster};
    use kura_engine::{EngineError, Limits, Monitor, NoMonitor, Value};

    static FFI: RwLock<()> = RwLock::new(());

    /// Swift の大域の数（生きているハンドル・展開の回数）を照らさないテストが取る。
    pub fn shared() -> RwLockReadGuard<'static, ()> {
        FFI.read().unwrap_or_else(|e| e.into_inner())
    }

    /// Swift の大域の数を照らすテストが取る（ほかの FFI のテストと同時に走らない）。
    pub fn exclusive() -> RwLockWriteGuard<'static, ()> {
        FFI.write().unwrap_or_else(|e| e.into_inner())
    }

    /// (生きているハンドル, 画像を展開した回数)。
    pub fn stats() -> (i64, i64) {
        crate::bridge::debug_stats()
    }

    /// 固定の段に Platform を足した組。
    pub fn mac_set() -> CodecSet {
        super::codec_set(&PinnedOptions::default())
    }

    /// 読む（既定の上限・中止しない）。
    pub fn decode(
        set: &CodecSet,
        bytes: &[u8],
        format: &str,
    ) -> Result<(Value, DecodeStamp), EngineError> {
        decode_with(set, bytes, format, &Limits::default(), &NoMonitor)
    }

    /// 読む（上限・中止つき）。
    pub fn decode_with(
        set: &CodecSet,
        bytes: &[u8],
        format: &str,
        limits: &Limits,
        mon: &dyn Monitor,
    ) -> Result<(Value, DecodeStamp), EngineError> {
        set.decode(
            &mut Cursor::new(bytes.to_vec()),
            format,
            &DecodeOptions::default(),
            limits,
            mon,
        )
    }

    /// kura-mac のフィクスチャ。
    pub fn fixture(name: &str) -> Vec<u8> {
        std::fs::read(
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("fixtures")
                .join(name),
        )
        .unwrap()
    }

    /// kura-engine のフィクスチャ。
    pub fn engine_fixture(name: &str) -> Vec<u8> {
        std::fs::read(
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../kura-engine/fixtures")
                .join(name),
        )
        .unwrap()
    }

    /// `sips` で形式を変える（`from`・`to` は拡張子。`to_format` は sips の形式の名前）。
    pub fn sips(bytes: &[u8], from: &str, to_format: &str, to: &str) -> Vec<u8> {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join(format!("src.{from}"));
        let dst = dir.path().join(format!("dst.{to}"));
        std::fs::write(&src, bytes).unwrap();
        let out = Command::new("sips")
            .args(["-s", "format", to_format])
            .arg(&src)
            .arg("--out")
            .arg(&dst)
            .output()
            .unwrap();
        assert!(out.status.success(), "sips {to_format}: {out:?}");
        std::fs::read(&dst).unwrap()
    }

    /// ChaCha8 の乱数の画像。
    pub fn noise(w: u32, h: u32, fmt: PixelFormat, seed: u64) -> Value {
        (InputGen::Noise { w, h, fmt, seed }).generate().unwrap()
    }

    /// 画素とメタデータから画像の値を作る。
    pub fn image(px: Raster, meta: ImageMeta) -> Value {
        Value::Image(Arc::new(Image { px, meta }))
    }

    /// 固定の段の PNG のエンコーダで書く。
    pub fn png_bytes(v: &Value) -> Vec<u8> {
        CodecSet::pinned()
            .encode_bytes(v, "png", &serde_json::json!({}), &NoMonitor)
            .unwrap()
            .0
    }

    /// 画像の値の画素（パレットは RGB(A) に広げる。`alpha` が真なら RGBA）。
    pub fn rows(v: &Value, alpha: bool) -> (PixelFormat, Vec<u8>) {
        let Value::Image(img) = v else {
            panic!("画像ではありません: {:?}", v.shape())
        };
        let px = &img.px;
        let rows = px.to_rows().unwrap();
        if px.format() != PixelFormat::Indexed8 {
            return (px.format(), rows);
        }
        let colors = &px.palette().unwrap().colors;
        let n = if alpha { 4 } else { 3 };
        let out = rows
            .iter()
            .flat_map(|&i| colors[usize::from(i)][..n].to_vec())
            .collect();
        (
            if alpha {
                PixelFormat::Rgba8
            } else {
                PixelFormat::Rgb8
            },
            out,
        )
    }
}

#[cfg(test)]
#[cfg(target_os = "macos")]
mod tests {
    use kura_engine::codec::Tier;

    use super::*;

    /// 組んだ CodecSet の能力の表：HEIC・HEIF・AVIF・TIFF・BMP は ImageIO だけが、CMYK の JPEG・PSD は固定の段のあとに
    /// ImageIO が読む。HE-AAC の m4a・ADTS・Ogg Opus・WAV・AIFF の変種は固定の段のあとに AudioToolbox が読み、m4a は
    /// AudioToolbox の AAC が書く。どれも Platform・再現を約束しない・環境つき。能力は 1 度だけ調べる。
    #[test]
    fn codec_set_registers_the_platform_codecs() {
        let _g = testutil::shared();
        platform_status().unwrap();
        let set = testutil::mac_set();
        let read = |f: &str| set.format_caps(f).unwrap().read;
        for f in ["heic", "heif", "avif", "tiff", "bmp"] {
            let r = read(f);
            assert_eq!(r.len(), 1, "{f}");
            assert_eq!(
                (r[0].codec, r[0].tier, r[0].reproducible),
                (IMAGEIO_DEC, Tier::Platform, false),
                "{f}"
            );
            assert!(r[0].env.is_some());
        }
        for f in ["jpeg", "psd"] {
            let r = read(f);
            assert_eq!(r[0].tier, Tier::Pinned, "{f}");
            assert_eq!(r.last().map(|c| c.codec), Some(IMAGEIO_DEC), "{f}");
        }
        for f in ["m4a", "aac", "ogg", "wav", "aiff"] {
            let r = read(f);
            assert_eq!(r[0].tier, Tier::Pinned, "{f}");
            assert_eq!(r.last().map(|c| c.codec), Some(AUDIOTOOLBOX_DEC), "{f}");
        }
        // .opus には固定の段の読み手がない
        assert_eq!(read("opus")[0].codec, AUDIOTOOLBOX_DEC);
        let w = set.format_caps("m4a").unwrap().write;
        assert_eq!(
            (w[0].codec, w[0].tier, w[0].reproducible),
            (AAC_AUDIOTOOLBOX_ENC, Tier::Platform, false)
        );
        assert!(Arc::ptr_eq(&codec_caps().unwrap(), &codec_caps().unwrap()));
        assert_eq!(platform_decoders().len(), 2);
        assert_eq!(platform_encoders().len(), 1);
    }

    /// 能力に AAC-LC のエンコーダか書けるレートの表がなければ、AAC の書き手を登録しない（Swift の問い合わせが失敗すると
    /// 表は空になる。登録すると能力の表が m4a を書けると偽る）。
    #[test]
    fn aac_writer_needs_the_aac_caps() {
        let _g = testutil::shared();
        let caps = codec_caps().unwrap();
        assert_eq!(encoders_for(caps.clone()).len(), 1);
        let strips: [fn(&mut MacCaps); 2] = [|c| c.aac_lc.clear(), |c| c.audio_encode.clear()];
        for strip in strips {
            let mut c = (*caps).clone();
            strip(&mut c);
            assert!(encoders_for(Arc::new(c)).is_empty());
        }
    }
}
