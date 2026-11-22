// Kura の Platform のコーデック（ImageIO・AudioToolbox）の C の口。Swift（@_cdecl）と Rust（crates/kura-mac/src/bridge.rs・
// abi.rs）の正本で、関数のプロトタイプ・構造体・番号をすべてここに書く。関数・構造体・番号を変えたら KURA_FFI_ABI を上げ、
// Swift の定義と Rust の宣言を同じに直す（食い違いは未定義動作）。照らし方：
// - Swift の @_cdecl の定義の型は、Codec/FfiCheck.swift がこのプロトタイプとコンパイル時に照らす。
// - Rust の extern の宣言（bridge.rs）・#[repr(C)] の構造体と番号（abi.rs）は、abi.rs のテストがこのファイルの文字列と照らす
//   （macOS 以外でも回る）。
// - 構造体の大きさと各項目の位置は、kura_mac_ffi.c の _Static_assert と、起動時の kura_ffi_abi・kura_ffi_layout で照らし、
//   合わなければ Rust は Platform のコーデックを登録しない。
// Platform のコーデックのほかの Swift の関数（サムネイル・メディア情報など。C 文字列と整数で受け渡す。ADR-37）はこの口に入れない。
//
// 約束（ADR-37 の改訂。docs/editing.md §9.1）：
// - 型は C の整数・小数・ポインタと長さ・不透明なハンドルだけ。長さ・位置は int64_t（Swift の Int64）で、両側で 0 以上を確かめる。
// - 画素・PCM・ICC・書き出しの出力は Rust が確保した領域に Swift が書く（長さを必ず渡す。Swift は呼び出しのあいだだけ書き、
//   保持しない）。例外は AAC の書き出しの途中の出力で、ハンドルの中の Swift の配列に溜め、close で消える。
// - 入力のバイト列も、Swift は呼び出しのあいだだけ読む（画像は kura_img_open の中で CFData に写す）。例外：音声の読み取りの
//   入力のバイト列（kura_aud_open の bytes）は、Swift がコピーせずにハンドルの寿命のあいだ読む（kura_aud_read の中の
//   AudioFile の読みのコールバックが読む）。Rust は kura_aud_close まで落とさない。
// - ハンドルは Unmanaged.passRetained の不透明なポインタ。Rust の Drop で *_close を 1 度だけ呼ぶ。1 つのハンドルを同時に
//   2 つのスレッドから使わない（別々のハンドルは同時に使ってよい）。
// - 失敗は状態の番号（KURA_*。kura-engine の codec::platform::STATUS_* と同じ値）と、Rust が渡した領域（err、err_cap バイト）に
//   Swift が書く NUL 終端の UTF-8 の文言（文字の境目で切る）。
// - 中止は Rust が帯・チャンクの合間で見る。Swift の中で止められる所には kura_cancel_fn（ctx は Rust の `&&dyn Monitor`）を渡す。
// - Swift の codec の経路は例外を投げる Objective-C の API を使わず、`!`・`try!`・`as!`・あふれる演算・範囲を確かめない
//   添字を使わない（Swift の trap と NSException はプロセスごと落ちる）。

#ifndef KURA_MAC_FFI_H
#define KURA_MAC_FFI_H

#include <stdbool.h>
#include <stdint.h>

/// ABI の版。
#define KURA_FFI_ABI 1u

/// 状態の番号。
#define KURA_OK 0
#define KURA_UNSUPPORTED 1
#define KURA_FAILED 2
#define KURA_CANCELED 3
#define KURA_INVALID 4
#define KURA_MISMATCH 5
#define KURA_LIMIT 6

/// 中止を尋ねる（真なら中止）。
typedef bool (*kura_cancel_fn)(void *ctx);

/// 画素のアルファの扱い（kura_image_info.alpha・kura_image_frame.alpha）。
#define KURA_ALPHA_NONE 0
#define KURA_ALPHA_STRAIGHT 1
#define KURA_ALPHA_PREMULTIPLIED 2

/// 画像の旗（kura_image_info.flags）。
#define KURA_IMG_HDR 1u        // HDR・浮動小数（Rust は Unsupported にする）
#define KURA_IMG_CONVERTED 2u  // CMYK・Lab を sRGB に変換した（ICC は sRGB のもの）
#define KURA_IMG_SEQUENCE 4u   // フレームの列（HEICS・AVIS）

/// 開いた画像の形（展開する前・向きを当てる前）。
typedef struct kura_image_info {
    uint32_t width;
    uint32_t height;
    uint32_t frames;        // 1 以上（フレームの列のときだけ 2 以上）
    uint32_t orientation;   // EXIF の向き（1〜8）
    int32_t pixel_format;   // kura の PixelFormat::code（1 Gray8 〜 8 Rgba16）。HDR なら 0
    int32_t alpha;          // KURA_ALPHA_*
    uint32_t loop_count;    // 0 は無限
    uint32_t flags;         // KURA_IMG_*
    int64_t icc_len;        // 0 はなし
    int64_t exif_len;       // 0 はなし（ImageIO は生の EXIF を返さない）
} kura_image_info;

/// 展開したフレームの実際の形。
typedef struct kura_image_frame {
    uint32_t width;
    uint32_t height;
    int32_t pixel_format;
    int32_t alpha;
    uint32_t duration_ms;
    uint32_t flags;
} kura_image_frame;

/// 開いた音声の形。
typedef struct kura_audio_info {
    double rate;            // 出力のサンプルレート
    uint32_t channels;
    uint32_t format_id;     // 中身の FourCC（'aac ' など）
    int64_t frames;         // priming・remainder を切ったあとの長さ（FileLengthFrames）
    int64_t priming;        // 切った先頭のフレーム（-1 は情報なし）
    int64_t remainder;      // 切った末尾のフレーム（-1 は情報なし）
    uint32_t bits;          // 元の PCM のビット数（lpcm のときだけ。ほかは 0）
    uint32_t flags;         // KURA_AUD_*
} kura_audio_info;

/// 音声の旗（kura_audio_info.flags）。
#define KURA_AUD_FLOAT 1u      // 元の PCM が浮動小数
#define KURA_AUD_PCM 2u        // 元が可逆の PCM（lpcm）

/// AAC の書き出しの設定。
typedef struct kura_aac_settings {
    double rate;
    uint32_t channels;
    uint32_t channel_mask;  // WAVE_FORMAT_EXTENSIBLE のマスク（ClientChannelLayout に渡す）
    uint32_t mode;          // kAudioCodecBitRateControlMode（0 固定・1 平均・2 上限つき可変・3 可変）
    uint32_t bitrate;       // ビット毎秒（mode = 3 では使わない）
    uint32_t quality;       // mode = 3 の品質（0〜127）
    uint32_t reserved;      // 0
} kura_aac_settings;

// ---- 関数（定義は Swift の @_cdecl。err は err_cap バイトの文言の領域、戻り値の int32_t は KURA_* の状態の番号）----

/// ABI の版（KURA_FFI_ABI）。
uint32_t kura_ffi_abi(void);
/// 構造体の大きさと各項目の位置を、Swift の見え方で out（cap 個まで）に書き、全体の数を返す。
int64_t kura_ffi_layout(int64_t *out, int64_t cap);
/// テスト用：[生きているハンドル, 画像を展開した回数] を out（cap 個まで）に書き、全体の数を返す。
int64_t kura_debug_stats(int64_t *out, int64_t cap);
/// テスト用：中止のコールバックを times 回呼び、真を返した回数を返す。
int64_t kura_ffi_call_cancel(kura_cancel_fn cancel, void *ctx, int64_t times);

/// 画像を開く（展開しない）。utis は扱う UTI をカンマで区切った NUL 終端の文字列。成功なら out に形を書いてハンドルを、
/// 失敗なら NULL を返す（status に番号）。
void *kura_img_open(const uint8_t *bytes, int64_t len, const char *utis, kura_image_info *out, int32_t *status,
                    uint8_t *err, int64_t err_cap);
/// フレーム index を全体で 1 度だけ展開し、実際の形を out に書く（途中では止められない。前後で cancel を見る）。
int32_t kura_img_begin(void *handle, uint32_t index, kura_image_frame *out, kura_cancel_fn cancel, void *ctx,
                       uint8_t *err, int64_t err_cap);
/// 展開したフレームの行 y0..y0 + rows を kura の並びで buf（len = rows × row_bytes バイトちょうど）に書く。
int32_t kura_img_read_rows(void *handle, int64_t y0, int64_t rows, uint8_t *buf, int64_t len, int64_t row_bytes,
                           uint8_t *err, int64_t err_cap);
/// ICC を buf（len = icc_len バイトちょうど）に写す。
int32_t kura_img_copy_icc(void *handle, uint8_t *buf, int64_t len, uint8_t *err, int64_t err_cap);
/// 画像のハンドルを閉じる（1 度だけ）。
void kura_img_close(void *handle);

/// 音声を開く。bytes は閉じるまで読む（上の約束の例外）。hint は AudioFile の種類の手掛かり、allowed は読む中身の FourCC、
/// masks はチャンネルの数ごとの既定の並び。
void *kura_aud_open(const uint8_t *bytes, int64_t len, uint32_t hint, const uint32_t *allowed, int64_t n_allowed,
                    const uint32_t *masks, int64_t n_masks, kura_audio_info *out, int32_t *status, uint8_t *err,
                    int64_t err_cap);
/// 続きを planes（channels 個。それぞれ frames 個の float）に書き、書いたフレームの数を out_frames に書く（0 は終わり）。
int32_t kura_aud_read(void *handle, float *const *planes, int64_t channels, int64_t frames, int64_t *out_frames,
                      uint8_t *err, int64_t err_cap);
/// 音声のハンドルを閉じる（1 度だけ。このあと Rust は入力のバイト列を落としてよい）。
void kura_aud_close(void *handle);

/// AAC を書き始める（max_out は出力の長さの上限）。
void *kura_aac_open(const kura_aac_settings *settings, int64_t max_out, int32_t *status, uint8_t *err,
                    int64_t err_cap);
/// 続きの PCM（channels 個。それぞれ frames 個の float）を渡す。
int32_t kura_aac_write(void *handle, const float *const *planes, int64_t channels, int64_t frames, uint8_t *err,
                       int64_t err_cap);
/// 書き終えて、出力の長さを out_len に書く。
int32_t kura_aac_finish(void *handle, int64_t *out_len, kura_cancel_fn cancel, void *ctx, uint8_t *err,
                        int64_t err_cap);
/// 出力を buf（len = finish が返した長さちょうど）に写す。
int32_t kura_aac_copy(void *handle, uint8_t *buf, int64_t len, uint8_t *err, int64_t err_cap);
/// AAC のハンドルを閉じる（1 度だけ。書きかけの出力も消える）。
void kura_aac_close(void *handle);

/// 能力と環境の JSON を buf（cap バイト）に書き、全体の長さを返す（buf が NULL なら長さだけ）。
int64_t kura_codec_caps(uint8_t *buf, int64_t cap);

#endif
