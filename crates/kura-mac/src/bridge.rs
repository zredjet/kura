//! Swift の Platform のコーデック（ImageIO・AudioToolbox）の C の口。Platform のコーデックの `unsafe` はここだけに置き、外へは
//! 安全な型（[`SwiftImage`]・[`SwiftAudio`]・[`SwiftAac`]）を出す。正本は `swift/Sources/KuraMacC/include/kura_mac_ffi.h`
//! （関数のプロトタイプ・構造体・番号。変えたら KURA_FFI_ABI・[`ABI`] を上げ、下の extern の宣言と `abi.rs` を同じに直す）。
//! 下の extern の宣言は、`abi.rs` のテストがヘッダのプロトタイプと文字列で照らす（Swift の定義とヘッダは
//! `Codec/FfiCheck.swift` がコンパイル時に照らす）。
//!
//! - 長さ・位置は `i64`（C の `int64_t`・Swift の `Int64`）。`usize` ↔ `UInt` は使わない（Swift の `Int(UInt)` は範囲の外で
//!   trap する）。
//! - 画素・PCM・ICC・出力は Rust が確保した領域のポインタと長さを渡し、Swift は呼び出しのあいだだけ書く。入力のバイト列も
//!   呼び出しのあいだだけ読む（画像は Swift が CFData に写す）。例外：音声の読み取りの入力のバイト列は、Swift がコピーせずに
//!   ハンドルの寿命のあいだ読む（`kura_aud_read` の中の AudioFile の読みのコールバック）。Rust は close まで落とさない。
//! - ハンドル（Swift の `Unmanaged.passRetained`）は `Drop` で 1 度だけ閉じる。音声の読み取りは、ハンドルを閉じてから入力の
//!   バイト列を落とす（構造体の項目は `Drop::drop` のあとに落ちる）。画像の展開のスレッドを見捨てたときは閉じない
//!   （`codec::image`。スレッドが戻ればスレッドの中で閉じる）。
//! - 失敗は状態の番号（kura-engine の `codec::platform::STATUS_*`）と、Rust が渡した 512 バイトの領域に Swift が書いた文言。
//! - 中止のコールバック（[`cancel_trampoline`]）は `&&dyn Monitor` を受け、panic を外に出さない（`extern "C"` を unwind で
//!   越えると abort）。panic したら中止とみなす。
#![deny(unsafe_op_in_unsafe_fn, clippy::undocumented_unsafe_blocks)]

use std::ffi::{CStr, c_char, c_void};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr::NonNull;
use std::sync::Arc;

use kura_engine::Monitor;
use kura_engine::codec::platform::BackendError;

use crate::abi::{ABI, AacSettingsC, AudioInfoC, ImageFrameC, ImageInfoC, rust_layout};

/// `kura_cancel_fn`。
type CancelFn = extern "C" fn(*mut c_void) -> bool;

unsafe extern "C" {
    fn kura_ffi_abi() -> u32;
    fn kura_ffi_layout(out: *mut i64, cap: i64) -> i64;
    #[cfg(test)]
    fn kura_debug_stats(out: *mut i64, cap: i64) -> i64;
    #[cfg(test)]
    fn kura_ffi_call_cancel(cancel: Option<CancelFn>, ctx: *mut c_void, times: i64) -> i64;

    fn kura_img_open(
        bytes: *const u8,
        len: i64,
        utis: *const c_char,
        out: *mut ImageInfoC,
        status: *mut i32,
        err: *mut u8,
        err_cap: i64,
    ) -> *mut c_void;
    fn kura_img_begin(
        handle: *mut c_void,
        index: u32,
        out: *mut ImageFrameC,
        cancel: Option<CancelFn>,
        ctx: *mut c_void,
        err: *mut u8,
        err_cap: i64,
    ) -> i32;
    fn kura_img_read_rows(
        handle: *mut c_void,
        y0: i64,
        rows: i64,
        buf: *mut u8,
        len: i64,
        row_bytes: i64,
        err: *mut u8,
        err_cap: i64,
    ) -> i32;
    fn kura_img_copy_icc(
        handle: *mut c_void,
        buf: *mut u8,
        len: i64,
        err: *mut u8,
        err_cap: i64,
    ) -> i32;
    fn kura_img_close(handle: *mut c_void);

    fn kura_aud_open(
        bytes: *const u8,
        len: i64,
        hint: u32,
        allowed: *const u32,
        n_allowed: i64,
        masks: *const u32,
        n_masks: i64,
        out: *mut AudioInfoC,
        status: *mut i32,
        err: *mut u8,
        err_cap: i64,
    ) -> *mut c_void;
    fn kura_aud_read(
        handle: *mut c_void,
        planes: *const *mut f32,
        channels: i64,
        frames: i64,
        out_frames: *mut i64,
        err: *mut u8,
        err_cap: i64,
    ) -> i32;
    fn kura_aud_close(handle: *mut c_void);

    fn kura_aac_open(
        settings: *const AacSettingsC,
        max_out: i64,
        status: *mut i32,
        err: *mut u8,
        err_cap: i64,
    ) -> *mut c_void;
    fn kura_aac_write(
        handle: *mut c_void,
        planes: *const *const f32,
        channels: i64,
        frames: i64,
        err: *mut u8,
        err_cap: i64,
    ) -> i32;
    fn kura_aac_finish(
        handle: *mut c_void,
        out_len: *mut i64,
        cancel: Option<CancelFn>,
        ctx: *mut c_void,
        err: *mut u8,
        err_cap: i64,
    ) -> i32;
    fn kura_aac_copy(
        handle: *mut c_void,
        buf: *mut u8,
        len: i64,
        err: *mut u8,
        err_cap: i64,
    ) -> i32;
    fn kura_aac_close(handle: *mut c_void);

    fn kura_codec_caps(buf: *mut u8, cap: i64) -> i64;
}

/// エラーの文言を受け取る領域の大きさ。
const ERR_CAP: usize = 512;

/// エラーの文言を受け取る領域（Rust が確保。Swift は NUL 終端の UTF-8 を書く）。
struct ErrBuf([u8; ERR_CAP]);

impl ErrBuf {
    fn new() -> Self {
        Self([0; ERR_CAP])
    }

    fn ptr(&mut self) -> *mut u8 {
        self.0.as_mut_ptr()
    }

    fn cap(&self) -> i64 {
        ERR_CAP as i64
    }

    fn text(&self) -> String {
        let end = self.0.iter().position(|b| *b == 0).unwrap_or(ERR_CAP);
        String::from_utf8_lossy(&self.0[..end]).into_owned()
    }

    /// 状態の番号を結果にする。
    fn check(&self, status: i32) -> Result<(), BackendError> {
        match BackendError::from_status(status, self.text()) {
            None => Ok(()),
            Some(e) => Err(e),
        }
    }

    /// 開く関数の結果（NULL なら状態の番号と文言で失敗。NULL で成功の番号は Swift の約束違反）。
    fn opened(&self, handle: *mut c_void, status: i32) -> Result<NonNull<c_void>, BackendError> {
        match NonNull::new(handle) {
            Some(h) => Ok(h),
            None => {
                self.check(status)?;
                Err(BackendError::Invalid(format!(
                    "ハンドルが返りませんでした: {}",
                    self.text()
                )))
            }
        }
    }
}

/// 長さ・位置を C の `int64_t` にする。
fn len_i64(n: usize) -> Result<i64, BackendError> {
    i64::try_from(n).map_err(|_| BackendError::Limit(format!("長さが大きすぎます（{n}）")))
}

/// 中止のコールバック。`ctx` は呼び出し側の `&&dyn Monitor`。
extern "C" fn cancel_trampoline(ctx: *mut c_void) -> bool {
    if ctx.is_null() {
        return false;
    }
    // SAFETY: ctx は `with_monitor` が渡した `&&dyn Monitor`（スタックの上）で、Swift の呼び出しのあいだ生きている。
    // Monitor は Sync なので、どのスレッドから読んでもよい
    let mon: &&dyn Monitor = unsafe { &*ctx.cast_const().cast::<&dyn Monitor>() };
    catch_unwind(AssertUnwindSafe(|| mon.canceled())).unwrap_or(true)
}

/// 中止のコールバックの文脈（`&&dyn Monitor`）を作って `f` に渡す。
fn with_monitor<T>(mon: &dyn Monitor, f: impl FnOnce(*mut c_void) -> T) -> T {
    let mon_ref: &dyn Monitor = mon;
    let ctx = std::ptr::from_ref(&mon_ref).cast_mut().cast::<c_void>();
    f(ctx)
}

/// ABI の版（Swift の見え方）。
pub(crate) fn abi() -> u32 {
    // SAFETY: 引数のない C 関数
    unsafe { kura_ffi_abi() }
}

/// 構造体の大きさと各項目の位置（Swift の見え方）。
pub(crate) fn swift_layout() -> Vec<i64> {
    let mut out = vec![0i64; 64];
    // SAFETY: out は 64 個書ける。Swift は cap を超えて書かず、全体の数を返す
    let n = unsafe { kura_ffi_layout(out.as_mut_ptr(), 64) };
    out.truncate(usize::try_from(n).unwrap_or(0).min(64));
    out
}

/// ABI の版と構造体の形が Swift と合うか（合わなければ理由）。
pub(crate) fn check_abi() -> Result<(), String> {
    let swift = abi();
    if swift != ABI {
        return Err(format!(
            "Swift の ABI の版 {swift} が Rust の {ABI} と違います"
        ));
    }
    let (s, r) = (swift_layout(), rust_layout());
    if s != r {
        return Err(format!(
            "構造体の形が Swift と Rust で違います（Swift {s:?}・Rust {r:?}）"
        ));
    }
    Ok(())
}

/// 調べ物の数：(生きているハンドル, 画像を展開した回数)。
#[cfg(test)]
pub(crate) fn debug_stats() -> (i64, i64) {
    let mut out = [0i64; 2];
    // SAFETY: out は 2 個書ける
    unsafe { kura_debug_stats(out.as_mut_ptr(), 2) };
    (out[0], out[1])
}

/// 中止のコールバックを Swift から `times` 回呼ばせ、真だった回数を返す（受け渡しの照合）。
#[cfg(test)]
pub(crate) fn call_cancel(mon: &dyn Monitor, times: i64) -> i64 {
    with_monitor(mon, |ctx| {
        // SAFETY: ctx は呼び出しのあいだ生きている `&&dyn Monitor`
        unsafe { kura_ffi_call_cancel(Some(cancel_trampoline), ctx, times) }
    })
}

/// 能力と環境の JSON（`kura_codec_caps`）。
pub(crate) fn codec_caps_json() -> Result<Vec<u8>, String> {
    // SAFETY: buf が NULL なら長さだけを返す
    let n = unsafe { kura_codec_caps(std::ptr::null_mut(), 0) };
    let len = usize::try_from(n)
        .ok()
        .filter(|n| *n > 0 && *n <= 16 << 20)
        .ok_or_else(|| format!("能力を調べられません（{n}）"))?;
    let mut buf = vec![0u8; len];
    // SAFETY: buf は len バイト書ける
    let got = unsafe { kura_codec_caps(buf.as_mut_ptr(), n) };
    if got != n {
        return Err(format!("能力の長さが変わりました（{n} → {got}）"));
    }
    Ok(buf)
}

/// 開いた画像（ImageIO）。展開（[`SwiftImage::begin`]）を専用のスレッドで行うため、スレッドをまたいで渡せる。
pub(crate) struct SwiftImage {
    handle: NonNull<c_void>,
    info: ImageInfoC,
}

// SAFETY: ハンドル（Swift の ImageDecodeHandle：CFData・CGImageSource・CGImage・vImageConverter）はスレッドに縛られず、
// どのスレッドから使っても閉じてもよい。約束は「1 つのハンドルを同時に 2 つのスレッドから使わない」だけで、SwiftImage を
// 動かす（所有を渡す）か `&mut self` で使う限り同時には使われない（`&self` で読むのは Rust の側の info だけ）
unsafe impl Send for SwiftImage {}

impl Drop for SwiftImage {
    fn drop(&mut self) {
        // SAFETY: open が返したハンドルを 1 度だけ閉じる
        unsafe { kura_img_close(self.handle.as_ptr()) };
    }
}

impl SwiftImage {
    /// 開く（展開しない）。`utis` は扱う UTI（カンマ区切り）。入力は Swift が写すので、呼び出しのあとは要らない。
    pub(crate) fn open(bytes: &[u8], utis: &CStr) -> Result<Self, BackendError> {
        let len = len_i64(bytes.len())?;
        let mut info = ImageInfoC::default();
        let mut status = 0i32;
        let mut err = ErrBuf::new();
        // SAFETY: bytes は len バイト読める（Swift は CFData に写す）。utis は NUL 終端。info・status は書ける。
        // err は err.cap() バイト書ける
        let handle = unsafe {
            kura_img_open(
                bytes.as_ptr(),
                len,
                utis.as_ptr(),
                &mut info,
                &mut status,
                err.ptr(),
                err.cap(),
            )
        };
        let handle = err.opened(handle, status)?;
        Ok(Self { handle, info })
    }

    /// 開いたときの形。
    pub(crate) fn info(&self) -> ImageInfoC {
        self.info
    }

    /// フレーム `index` を 1 度だけ展開して、実際の形を返す。
    pub(crate) fn begin(
        &mut self,
        index: u32,
        mon: &dyn Monitor,
    ) -> Result<ImageFrameC, BackendError> {
        let mut frame = ImageFrameC::default();
        let mut err = ErrBuf::new();
        let status = with_monitor(mon, |ctx| {
            // SAFETY: ハンドルは開いている。frame は書ける。ctx は呼び出しのあいだ生きている `&&dyn Monitor`
            unsafe {
                kura_img_begin(
                    self.handle.as_ptr(),
                    index,
                    &mut frame,
                    Some(cancel_trampoline),
                    ctx,
                    err.ptr(),
                    err.cap(),
                )
            }
        });
        err.check(status)?;
        Ok(frame)
    }

    /// 行 `y0..y0 + rows` を `out`（`rows × row_bytes` バイトちょうど）に書かせる。
    pub(crate) fn read_rows(
        &mut self,
        y0: u32,
        rows: u32,
        row_bytes: usize,
        out: &mut [u8],
    ) -> Result<(), BackendError> {
        let len = len_i64(out.len())?;
        let stride = len_i64(row_bytes)?;
        let mut err = ErrBuf::new();
        // SAFETY: ハンドルは開いている。out は len バイト書ける（Swift は len = rows × row_bytes を確かめ、その範囲だけ書く）
        let status = unsafe {
            kura_img_read_rows(
                self.handle.as_ptr(),
                i64::from(y0),
                i64::from(rows),
                out.as_mut_ptr(),
                len,
                stride,
                err.ptr(),
                err.cap(),
            )
        };
        err.check(status)
    }

    /// ICC を `out`（icc_len バイトちょうど）に写させる。
    pub(crate) fn copy_icc(&mut self, out: &mut [u8]) -> Result<(), BackendError> {
        let len = len_i64(out.len())?;
        let mut err = ErrBuf::new();
        // SAFETY: ハンドルは開いている。out は len バイト書ける（Swift は len が ICC の長さと同じときだけ写す）
        let status = unsafe {
            kura_img_copy_icc(
                self.handle.as_ptr(),
                out.as_mut_ptr(),
                len,
                err.ptr(),
                err.cap(),
            )
        };
        err.check(status)
    }
}

/// 開いた音声（AudioToolbox）。
pub(crate) struct SwiftAudio {
    handle: NonNull<c_void>,
    /// Swift がコピーせずにハンドルの寿命のあいだ読む入力（C の口の約束の例外。ハンドルを閉じてから落とす）。
    _bytes: Arc<Vec<u8>>,
    info: AudioInfoC,
}

impl Drop for SwiftAudio {
    fn drop(&mut self) {
        // SAFETY: open が返したハンドルを 1 度だけ閉じる。このあと `_bytes` が落ちる
        unsafe { kura_aud_close(self.handle.as_ptr()) };
    }
}

impl SwiftAudio {
    /// 開く。`hint` は AudioFile の種類の手掛かり（ADTS だけ。ほかは 0）、`allowed` は読む中身の FourCC、`masks` は
    /// チャンネルの数（1〜）ごとの既定の並び（3 チャンネル以上はファイルの配置がこれと同じときだけ読む）。
    pub(crate) fn open(
        bytes: Arc<Vec<u8>>,
        hint: u32,
        allowed: &[u32],
        masks: &[u32],
    ) -> Result<Self, BackendError> {
        let len = len_i64(bytes.len())?;
        let n_allowed = len_i64(allowed.len())?;
        let n_masks = len_i64(masks.len())?;
        let mut info = AudioInfoC::default();
        let mut status = 0i32;
        let mut err = ErrBuf::new();
        // SAFETY: bytes は len バイト読め、返したハンドルより長く生きる（Self が持ち、閉じてから落とす）。allowed は
        // n_allowed 個、masks は n_masks 個読める。info・status は書ける。err は err.cap() バイト書ける
        let handle = unsafe {
            kura_aud_open(
                bytes.as_ptr(),
                len,
                hint,
                allowed.as_ptr(),
                n_allowed,
                masks.as_ptr(),
                n_masks,
                &mut info,
                &mut status,
                err.ptr(),
                err.cap(),
            )
        };
        let handle = err.opened(handle, status)?;
        Ok(Self {
            handle,
            _bytes: bytes,
            info,
        })
    }

    /// 開いたときの形。
    pub(crate) fn info(&self) -> AudioInfoC {
        self.info
    }

    /// 続きを `planes`（チャンネルの数ぶん、同じ長さ）の先頭から書かせ、書いたフレームの数を返す（0 は終わり）。
    pub(crate) fn read(&mut self, planes: &mut [&mut [f32]]) -> Result<usize, BackendError> {
        let frames = planes.first().map_or(0, |p| p.len());
        if planes.iter().any(|p| p.len() != frames) {
            return Err(BackendError::Invalid(
                "チャンネルの長さがそろっていません".into(),
            ));
        }
        if frames == 0 {
            return Ok(0);
        }
        let channels = len_i64(planes.len())?;
        let n = len_i64(frames)?;
        let ptrs: Vec<*mut f32> = planes.iter_mut().map(|p| p.as_mut_ptr()).collect();
        let mut got = 0i64;
        let mut err = ErrBuf::new();
        // SAFETY: ハンドルは開いている。ptrs は channels 個で、それぞれ frames 個の f32 を書ける（Swift は n 個まで書く）
        let status = unsafe {
            kura_aud_read(
                self.handle.as_ptr(),
                ptrs.as_ptr(),
                channels,
                n,
                &mut got,
                err.ptr(),
                err.cap(),
            )
        };
        err.check(status)?;
        usize::try_from(got)
            .ok()
            .filter(|g| *g <= frames)
            .ok_or_else(|| BackendError::Invalid(format!("読んだ数が範囲の外です（{got}）")))
    }
}

/// 書きかけの AAC（AudioToolbox）。
pub(crate) struct SwiftAac {
    handle: NonNull<c_void>,
}

impl Drop for SwiftAac {
    fn drop(&mut self) {
        // SAFETY: open が返したハンドルを 1 度だけ閉じる（書きかけの出力も消える）
        unsafe { kura_aac_close(self.handle.as_ptr()) };
    }
}

impl SwiftAac {
    /// 書き始める。
    pub(crate) fn open(settings: &AacSettingsC, max_out: u64) -> Result<Self, BackendError> {
        let max_out = i64::try_from(max_out).unwrap_or(i64::MAX);
        let mut status = 0i32;
        let mut err = ErrBuf::new();
        // SAFETY: settings は読める。status は書ける。err は err.cap() バイト書ける
        let handle = unsafe { kura_aac_open(settings, max_out, &mut status, err.ptr(), err.cap()) };
        let handle = err.opened(handle, status)?;
        Ok(Self { handle })
    }

    /// 続きの PCM を渡す（Swift は呼び出しのあいだだけ読む）。
    pub(crate) fn write(&mut self, planes: &[&[f32]]) -> Result<(), BackendError> {
        let frames = planes.first().map_or(0, |p| p.len());
        if planes.iter().any(|p| p.len() != frames) {
            return Err(BackendError::Invalid(
                "チャンネルの長さがそろっていません".into(),
            ));
        }
        if frames == 0 {
            return Ok(());
        }
        let channels = len_i64(planes.len())?;
        let n = len_i64(frames)?;
        let ptrs: Vec<*const f32> = planes.iter().map(|p| p.as_ptr()).collect();
        let mut err = ErrBuf::new();
        // SAFETY: ハンドルは開いている。ptrs は channels 個で、それぞれ frames 個の f32 を読める（Swift は読むだけ）
        let status = unsafe {
            kura_aac_write(
                self.handle.as_ptr(),
                ptrs.as_ptr(),
                channels,
                n,
                err.ptr(),
                err.cap(),
            )
        };
        err.check(status)
    }

    /// 書き終えて、出力の長さを返す。
    pub(crate) fn finish(&mut self, mon: &dyn Monitor) -> Result<u64, BackendError> {
        let mut len = 0i64;
        let mut err = ErrBuf::new();
        let status = with_monitor(mon, |ctx| {
            // SAFETY: ハンドルは開いている。len は書ける。ctx は呼び出しのあいだ生きている `&&dyn Monitor`
            unsafe {
                kura_aac_finish(
                    self.handle.as_ptr(),
                    &mut len,
                    Some(cancel_trampoline),
                    ctx,
                    err.ptr(),
                    err.cap(),
                )
            }
        });
        err.check(status)?;
        u64::try_from(len)
            .map_err(|_| BackendError::Invalid(format!("出力の長さが負です（{len}）")))
    }

    /// 出力を `out`（finish が返した長さちょうど）に写させる。
    pub(crate) fn copy(&mut self, out: &mut [u8]) -> Result<(), BackendError> {
        let len = len_i64(out.len())?;
        let mut err = ErrBuf::new();
        // SAFETY: ハンドルは開いている。out は len バイト書ける（Swift は len が出力の長さと同じときだけ写す）
        let status = unsafe {
            kura_aac_copy(
                self.handle.as_ptr(),
                out.as_mut_ptr(),
                len,
                err.ptr(),
                err.cap(),
            )
        };
        err.check(status)
    }
}

#[cfg(test)]
mod tests {
    use kura_engine::codec::platform::CHANNEL_MASKS;
    use kura_engine::monitor::CancelAfter;
    use kura_engine::value::PixelFormat;
    use kura_engine::{Monitor, NoMonitor};

    use super::*;
    use crate::abi::{ALPHA_STRAIGHT, AUD_PCM};
    use crate::codec::caps::{AAC, LPCM, MacCaps};
    use crate::codec::testutil::{engine_fixture, exclusive, noise, png_bytes, shared, stats};

    /// ABI の版と、構造体の大きさ・各項目の位置が Swift と Rust で同じ（ヘッダの _Static_assert の大きさとも）。
    #[test]
    fn abi_and_layout_match_swift() {
        let _g = shared();
        assert_eq!(abi(), ABI);
        let (s, r) = (swift_layout(), rust_layout());
        assert_eq!(s, r);
        assert_eq!(r.len(), 35);
        assert_eq!((r[0], r[11], r[18], r[27]), (48, 24, 48, 32));
        check_abi().unwrap();
    }

    /// 中止のコールバック：Swift から呼ぶと Monitor の答えが返る。Monitor の panic は中止として受け止める（abort しない）。
    #[test]
    fn cancel_callback_round_trips() {
        let _g = shared();
        assert_eq!(call_cancel(&NoMonitor, 10), 0);
        assert_eq!(call_cancel(&CancelAfter::new(4), 10), 7);
        struct Panics;
        impl Monitor for Panics {
            fn canceled(&self) -> bool {
                panic!("中止の確認で panic")
            }
        }
        assert_eq!(call_cancel(&Panics, 3), 3);
    }

    /// 能力の JSON は読め、必要なもの（HEIC・AVIF の読み、AAC-LC の 48 kHz・ステレオ・256 kbps の書き、OS の版）がある。
    #[test]
    fn caps_have_what_kura_needs() {
        let _g = shared();
        let caps = MacCaps::parse(&codec_caps_json().unwrap()).unwrap();
        assert_eq!(caps.abi, ABI);
        assert!(!caps.os.is_empty() && !caps.build.is_empty(), "{caps:?}");
        assert!(caps.frameworks.contains_key("imageio"));
        assert!(caps.frameworks.contains_key("audiotoolbox"));
        for uti in ["public.heic", "public.avif", "public.tiff"] {
            assert!(caps.image_read.iter().any(|u| u == uti), "{uti}");
        }
        assert!(caps.audio_encode.contains(&AAC));
        assert!(caps.aac_bitrates_kbps(48_000, 2).contains(&256));
        assert!(caps.aac_rates().contains(&44_100));
    }

    /// すべての関数を 1 度ずつ、呼び方の誤り（範囲の外・長さの違い・順番の誤り）でも呼び、Swift が断る（落ちない）。
    /// ハンドルはどの道でも閉じる。
    #[test]
    fn every_function_rejects_misuse() {
        let _g = exclusive();
        let (live, _) = stats();
        // 画像
        let png = png_bytes(&noise(10, 300, PixelFormat::Rgba8, 1));
        let utis = c"public.png";
        assert!(matches!(
            SwiftImage::open(b"garbage", utis),
            Err(BackendError::Failed(_))
        ));
        assert!(matches!(
            SwiftImage::open(&[], utis),
            Err(BackendError::Invalid(_))
        ));
        assert!(matches!(
            SwiftImage::open(&png, c"public.heic,public.avif"),
            Err(BackendError::Unsupported(_))
        ));
        let mut img = SwiftImage::open(&png, utis).unwrap();
        let info = img.info();
        assert_eq!(
            (
                info.width,
                info.height,
                info.frames,
                info.pixel_format,
                info.alpha
            ),
            (10, 300, 1, 4, ALPHA_STRAIGHT)
        );
        assert!(info.icc_len > 0);
        let mut buf = vec![0u8; 40 * 10];
        assert!(matches!(
            img.read_rows(0, 10, 40, &mut buf),
            Err(BackendError::Invalid(_))
        ));
        assert!(matches!(
            img.begin(1, &NoMonitor),
            Err(BackendError::Invalid(_))
        ));
        assert!(matches!(
            img.begin(0, &CancelAfter::new(1)),
            Err(BackendError::Canceled)
        ));
        let frame = img.begin(0, &NoMonitor).unwrap();
        assert_eq!(
            (frame.width, frame.height, frame.pixel_format, frame.alpha),
            (10, 300, 4, ALPHA_STRAIGHT)
        );
        assert!(matches!(
            img.read_rows(295, 10, 40, &mut buf),
            Err(BackendError::Invalid(_))
        ));
        assert!(matches!(
            img.read_rows(0, 10, 39, &mut buf[..390]),
            Err(BackendError::Invalid(_))
        ));
        assert!(matches!(
            img.read_rows(0, 10, 40, &mut buf[..399]),
            Err(BackendError::Invalid(_))
        ));
        assert!(matches!(
            img.read_rows(0, 0, 40, &mut []),
            Err(BackendError::Invalid(_))
        ));
        img.read_rows(290, 10, 40, &mut buf).unwrap();
        let n = usize::try_from(info.icc_len).unwrap();
        let mut icc = vec![0u8; n + 1];
        assert!(matches!(
            img.copy_icc(&mut icc),
            Err(BackendError::Invalid(_))
        ));
        img.copy_icc(&mut icc[..n]).unwrap();
        drop(img);

        // 音声
        let wav = engine_fixture("audio-src16.wav");
        assert!(
            SwiftAudio::open(Arc::new(b"garbage".to_vec()), 0, &[LPCM], &CHANNEL_MASKS).is_err()
        );
        assert!(matches!(
            SwiftAudio::open(Arc::new(Vec::new()), 0, &[LPCM], &CHANNEL_MASKS),
            Err(BackendError::Invalid(_))
        ));
        assert!(matches!(
            SwiftAudio::open(Arc::new(wav.clone()), 0, &[AAC], &CHANNEL_MASKS),
            Err(BackendError::Unsupported(_))
        ));
        let mut a = SwiftAudio::open(Arc::new(wav), 0, &[LPCM], &CHANNEL_MASKS).unwrap();
        let ai = a.info();
        assert_eq!(
            (
                ai.rate,
                ai.channels,
                ai.frames,
                ai.format_id,
                ai.bits,
                ai.flags
            ),
            (44_100.0, 2, 44_100, LPCM, 16, AUD_PCM)
        );
        let mut one = vec![0f32; 100];
        assert!(matches!(
            a.read(&mut [&mut one]),
            Err(BackendError::Invalid(_))
        ));
        let (mut l, mut r, mut short) = (vec![0f32; 1_000], vec![0f32; 1_000], vec![0f32; 10]);
        assert_eq!(a.read(&mut [&mut l, &mut r]).unwrap(), 1_000);
        assert!(matches!(
            a.read(&mut [&mut l, &mut short]),
            Err(BackendError::Invalid(_))
        ));
        drop(a);

        // AAC
        let s = AacSettingsC {
            rate: 48_000.0,
            channels: 2,
            channel_mask: 0x3,
            mode: 2,
            bitrate: 256_000,
            quality: 127,
            reserved: 0,
        };
        assert!(matches!(
            SwiftAac::open(&AacSettingsC { reserved: 1, ..s }, 1 << 20),
            Err(BackendError::Invalid(_))
        ));
        assert!(matches!(
            SwiftAac::open(&s, 0),
            Err(BackendError::Invalid(_))
        ));
        let mut h = SwiftAac::open(&s, 1 << 20).unwrap();
        let mut out = vec![0u8; 10];
        assert!(matches!(h.copy(&mut out), Err(BackendError::Invalid(_))));
        let p = vec![0.1f32; 2_048];
        assert!(matches!(h.write(&[&p]), Err(BackendError::Invalid(_))));
        h.write(&[&p, &p]).unwrap();
        assert!(matches!(
            h.finish(&CancelAfter::new(1)),
            Err(BackendError::Canceled)
        ));
        let len = h.finish(&NoMonitor).unwrap();
        assert!(matches!(h.write(&[&p, &p]), Err(BackendError::Invalid(_))));
        assert!(matches!(
            h.finish(&NoMonitor),
            Err(BackendError::Invalid(_))
        ));
        let mut out = vec![0u8; usize::try_from(len).unwrap()];
        h.copy(&mut out).unwrap();
        assert_eq!(&out[4..8], b"ftyp");
        drop(h);
        assert_eq!(stats().0, live);
    }
}
