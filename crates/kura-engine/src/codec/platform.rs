//! Platform の段（macOS の機能。docs/editing.md §9.1）の受け口。OS を知らない。
//!
//! - 層の分け方：kura-mac が backend の trait（[`ImageBackend`]・[`AudioBackend`]・[`AacBackend`]）を C の口（Swift の
//!   ImageIO・AudioToolbox）で実装し、この層の adapter（[`PlatformImageDecoder`]・[`PlatformAudioDecoder`]・
//!   [`PlatformAacEncoder`]）で `Decoder`・`Encoder` にして、`CodecSet::add_decoder`・`add_encoder` で足す。`unsafe` と
//!   Swift は kura-mac に閉じ、kura-engine は `forbid(unsafe_code)` のまま。
//! - 結果に効く変換（プレマルチプライドからストレートへの戻し・EXIF の向き・形と中身の照合・上限・中止・音声のタグの
//!   読み取り・m4a の時刻の書き換え・入力の標本の直し）は、すべてこの層の Rust（整数の式）で行う。OS に依存するのは
//!   backend が書いた画素・PCM・AAC のバイト列だけ。Linux では偽の backend（`testing::platform`）で同じ adapter を通す。
//! - 2 段の呼び出し：adapter が入力を `max_value_bytes` まで読み → backend が開いて形を返す → adapter が上限・予算を
//!   確かめて `try_reserve_exact` で確保 → backend が adapter の領域に書く（画像は帯、音声・AAC はチャンクごと。合間で
//!   adapter が中止を見て進捗を出す）。backend は渡された領域を呼び出しのあいだだけ書き、保持しない。backend の
//!   ハンドル（開いたもの）は `Drop` で 1 度だけ閉じる。
//! - 段は `Tier::Platform`、`reproducible()` は偽（読んだ元に初めて保存するとき（保存のジョブが描くのと同じ読み取りから作る。docs/editing.md §9.4）
//!   にアンカーを作る）。識別子は backend の実装ごとに 1 つ（`imageio-dec/1`・`audiotoolbox-dec/1`・`aac-audiotoolbox-enc/1`。kura-mac の呼び方と
//!   この層の変換の約束の版）で、OS・フレームワークの版は backend の `env()`（記録の `env`）に出す。旧版は残さない
//!   （作り直しはアンカーから読む）。
//! - 失敗は [`BackendError`]（C の口の状態の番号と 1 対 1）。adapter が `EngineError` に直す（`Unsupported` は次の段へ）。

pub mod aac;
pub mod audio;
pub mod image;

pub use aac::{
    AacBackend, AacHandle, AacMode, AacSettings, PlatformAacEncoder, aac_max_output, zero_mp4_times,
};
pub use audio::{AudioBackend, AudioHandle, AudioInfo, PlatformAudioDecoder, Priming};
pub use image::{
    DecodedFrame, ImageBackend, ImageHandle, ImageInfo, PlatformAlpha, PlatformImageDecoder,
    unpremultiply_rows, unpremultiply_u8, unpremultiply_u16,
};

use crate::codec::Stage;
use crate::error::EngineError;

/// 状態の番号：成功（C の口の戻り値。Swift の側の表と同じ値にする）。
pub const STATUS_OK: i32 = 0;
/// 状態の番号：扱えない形式・変種（次の段へ回す）。
pub const STATUS_UNSUPPORTED: i32 = 1;
/// 状態の番号：読み書きに失敗した（壊れたファイルなど）。
pub const STATUS_FAILED: i32 = 2;
/// 状態の番号：中止した（Swift の中のループが中止のコールバックを見た）。
pub const STATUS_CANCELED: i32 = 3;
/// 状態の番号：呼び方の誤り（範囲の外の帯・小さすぎる領域・閉じたハンドル。kura の内部の誤り）。
pub const STATUS_INVALID: i32 = 4;
/// 状態の番号：ヘッダと中身の大きさ・形式が合わない。
pub const STATUS_MISMATCH: i32 = 5;
/// 状態の番号：上限を超える（大きさ・出力の長さ）。
pub const STATUS_LIMIT: i32 = 6;

/// backend の失敗（C の口の状態の番号と 1 対 1。文言は Rust が渡した領域に Swift が書いたもの）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackendError {
    /// 扱えない形式・変種（`STATUS_UNSUPPORTED`）。次の段へ回る。
    Unsupported(String),
    /// 読み書きに失敗した（`STATUS_FAILED`）。
    Failed(String),
    /// 中止した（`STATUS_CANCELED`）。
    Canceled,
    /// 呼び方の誤り（`STATUS_INVALID`）。
    Invalid(String),
    /// ヘッダと中身が合わない（`STATUS_MISMATCH`）。
    Mismatch(String),
    /// 上限を超える（`STATUS_LIMIT`）。
    Limit(String),
}

impl BackendError {
    /// 状態の番号（C の口の戻り値）。
    pub fn status(&self) -> i32 {
        match self {
            Self::Unsupported(_) => STATUS_UNSUPPORTED,
            Self::Failed(_) => STATUS_FAILED,
            Self::Canceled => STATUS_CANCELED,
            Self::Invalid(_) => STATUS_INVALID,
            Self::Mismatch(_) => STATUS_MISMATCH,
            Self::Limit(_) => STATUS_LIMIT,
        }
    }

    /// C の口の戻り値から作る（`STATUS_OK` は `None`）。知らない番号は失敗（`Failed`）として扱う。
    pub fn from_status(status: i32, message: impl Into<String>) -> Option<Self> {
        let m = message.into();
        Some(match status {
            STATUS_OK => return None,
            STATUS_UNSUPPORTED => Self::Unsupported(m),
            STATUS_FAILED => Self::Failed(m),
            STATUS_CANCELED => Self::Canceled,
            STATUS_INVALID => Self::Invalid(m),
            STATUS_MISMATCH => Self::Mismatch(m),
            STATUS_LIMIT => Self::Limit(m),
            other => Self::Failed(format!("知らない状態の番号です（{other}）: {m}")),
        })
    }

    /// エンジンのエラーにする。`codec` はコーデックの識別子、`stage` は読み取りか書き出しか。
    pub fn into_engine(self, codec: &str, stage: Stage) -> EngineError {
        let io = |message: String| match stage {
            Stage::Decode => EngineError::Decode {
                codec: codec.to_string(),
                message,
            },
            Stage::Encode => EngineError::Encode {
                codec: codec.to_string(),
                message,
            },
        };
        match self {
            Self::Unsupported(m) => EngineError::Unsupported(format!("{codec}: {m}")),
            Self::Failed(m) => io(m),
            Self::Canceled => EngineError::Canceled,
            Self::Invalid(m) => EngineError::Internal(format!("{codec}: {m}")),
            Self::Mismatch(m) => io(format!("ヘッダと中身が合いません: {m}")),
            Self::Limit(m) => EngineError::Limit(m),
        }
    }
}

/// サンプルレート（AudioToolbox の `Float64`）を整数にする。有限・正・整数・`u32` の範囲でなければ `Unsupported`。
pub fn rate_from_f64(rate: f64) -> Result<u32, BackendError> {
    if rate.is_finite() && rate >= 1.0 && rate <= f64::from(u32::MAX) && rate.fract() == 0.0 {
        // 範囲と整数であることを確かめたので、変換で値は変わらない
        Ok(rate as u32)
    } else {
        Err(BackendError::Unsupported(format!(
            "サンプルレートが整数ではありません（{rate}）"
        )))
    }
}

/// チャンネルの数ごとの既定の並び（`WAVE_FORMAT_EXTENSIBLE` のチャンネルのマスク。kura の planes の並びそのもの）。
/// 1：C、2：L R、3：L R C、4：L R BL BR、5：L R C BL BR、6：L R C LFE BL BR、7：L R C LFE BC SL SR、
/// 8：L R C LFE BL BR SL SR（Vorbis の書き出しの `ORDER` と同じ並び）。AudioToolbox の読み書きでは、これを
/// `kAudioChannelLayoutTag_UseChannelBitmap` の `ClientChannelLayout` に必ず渡す（渡さないとファイルの並びのまま返る）。
pub const CHANNEL_MASKS: [u32; 8] = [0x4, 0x3, 0x7, 0x33, 0x37, 0x3F, 0x70F, 0x63F];

/// チャンネルの数の既定の並び（1〜8。ほかは `None`：並びを推測しない）。
pub fn default_channel_mask(channels: u16) -> Option<u32> {
    let i = usize::from(channels).checked_sub(1)?;
    CHANNEL_MASKS.get(i).copied()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 状態の番号は凍結（Swift の側の表と同じ）。往復し、知らない番号は失敗になる。
    #[test]
    fn status_codes_are_frozen_and_round_trip() {
        let all = [
            BackendError::Unsupported("u".into()),
            BackendError::Failed("f".into()),
            BackendError::Canceled,
            BackendError::Invalid("i".into()),
            BackendError::Mismatch("m".into()),
            BackendError::Limit("l".into()),
        ];
        assert_eq!(
            all.iter().map(BackendError::status).collect::<Vec<_>>(),
            [1, 2, 3, 4, 5, 6]
        );
        for e in &all {
            let msg = match e {
                BackendError::Unsupported(m)
                | BackendError::Failed(m)
                | BackendError::Invalid(m)
                | BackendError::Mismatch(m)
                | BackendError::Limit(m) => m.clone(),
                BackendError::Canceled => String::new(),
            };
            assert_eq!(BackendError::from_status(e.status(), msg).as_ref(), Some(e));
        }
        assert_eq!(BackendError::from_status(STATUS_OK, "x"), None);
        assert!(matches!(
            BackendError::from_status(77, "x"),
            Some(BackendError::Failed(m)) if m.contains("77")
        ));
    }

    /// エンジンのエラーへの写し：扱えない変種は次の段へ回る `Unsupported`、呼び方の誤りは内部の誤り。
    #[test]
    fn maps_to_engine_errors() {
        let id = "x-dec/1";
        assert!(matches!(
            BackendError::Unsupported("a".into()).into_engine(id, Stage::Decode),
            EngineError::Unsupported(m) if m.starts_with(id)
        ));
        assert!(matches!(
            BackendError::Failed("a".into()).into_engine(id, Stage::Decode),
            EngineError::Decode { codec, .. } if codec == id
        ));
        assert!(matches!(
            BackendError::Failed("a".into()).into_engine(id, Stage::Encode),
            EngineError::Encode { codec, .. } if codec == id
        ));
        assert!(matches!(
            BackendError::Mismatch("a".into()).into_engine(id, Stage::Decode),
            EngineError::Decode { .. }
        ));
        assert!(matches!(
            BackendError::Canceled.into_engine(id, Stage::Decode),
            EngineError::Canceled
        ));
        assert!(matches!(
            BackendError::Invalid("a".into()).into_engine(id, Stage::Decode),
            EngineError::Internal(_)
        ));
        assert!(matches!(
            BackendError::Limit("a".into()).into_engine(id, Stage::Encode),
            EngineError::Limit(_)
        ));
    }

    #[test]
    fn rates_must_be_whole_numbers() {
        assert_eq!(rate_from_f64(44_100.0), Ok(44_100));
        assert_eq!(rate_from_f64(1.0), Ok(1));
        assert_eq!(rate_from_f64(f64::from(u32::MAX)), Ok(u32::MAX));
        for bad in [
            0.0,
            -48_000.0,
            0.5,
            44_100.5,
            f64::NAN,
            f64::INFINITY,
            f64::from(u32::MAX) + 1.0,
        ] {
            assert!(rate_from_f64(bad).is_err(), "{bad}");
        }
    }

    /// 既定の並びは、Vorbis の書き出しの並び（`ORDER`）と同じ：Vorbis のチャンネルの順の WAV のビットを、kura の planes の
    /// 並びに置くと、ビットの小さい順になる。
    #[test]
    fn channel_masks_match_the_vorbis_order() {
        // Vorbis I §4.3.9 の並び（WAVE_FORMAT_EXTENSIBLE のビット）
        const FL: u32 = 0x1;
        const FR: u32 = 0x2;
        const FC: u32 = 0x4;
        const LFE: u32 = 0x8;
        const BL: u32 = 0x10;
        const BR: u32 = 0x20;
        const BC: u32 = 0x100;
        const SL: u32 = 0x200;
        const SR: u32 = 0x400;
        let vorbis: [&[u32]; 8] = [
            &[FC],
            &[FL, FR],
            &[FL, FC, FR],
            &[FL, FR, BL, BR],
            &[FL, FC, FR, BL, BR],
            &[FL, FC, FR, BL, BR, LFE],
            &[FL, FC, FR, SL, SR, BC, LFE],
            &[FL, FC, FR, SL, SR, BL, BR, LFE],
        ];
        for (n, order) in crate::codec::vorbis::ORDER.iter().enumerate() {
            let mut planes = vec![0u32; n + 1];
            for (i, &plane) in order.iter().enumerate() {
                planes[plane] = vorbis[n][i];
            }
            // planes はビットの小さい順
            assert!(planes.windows(2).all(|w| w[0] < w[1]), "{n}");
            let mask = planes.iter().fold(0, |m, b| m | b);
            assert_eq!(default_channel_mask(n as u16 + 1), Some(mask), "{n}");
            assert_eq!(mask.count_ones() as usize, n + 1);
        }
        assert_eq!(default_channel_mask(0), None);
        assert_eq!(default_channel_mask(9), None);
    }
}
