//! フォントなしの SVG の働き手（決定 23。OS によらない）。kura-core のテスト（`tests/svg_worker.rs`）と Linux の CI で、
//! アプリの代わりに起動する。アプリ（src-tauri）と kura-devtools は自分の実行ファイルを働き手にする（システムのフォントつき）。
//! 確保器は、アプリの `kura_mac::ReturningAllocator` と同じく確保の量を目印（`AllocMeter`）で数える `System`（働き手のときだけ
//! 上限を付ける。ADR-164 (4)）。

use std::alloc::{GlobalAlloc, Layout, System};
use std::process::ExitCode;

use kura_core::codec_ext::svg_worker::{self, AllocMeter};
use kura_engine::codec::PinnedOptions;

/// 確保の量を数える `System`。
struct MeteredSystem {
    meter: AllocMeter,
}

#[global_allocator]
static ALLOC: MeteredSystem = MeteredSystem {
    meter: AllocMeter::new(),
};

// SAFETY: 確保・解放はすべて `System` に同じ引数で任せ、その前後で量を数えるだけ。上限で断るときは、`System` を呼ばずに
// ヌルを返す（GlobalAlloc の約束どおりの失敗。取り直しなら古い領域はそのまま）。
unsafe impl GlobalAlloc for MeteredSystem {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if !self.meter.charge(layout.size()) {
            return std::ptr::null_mut();
        }
        // SAFETY: 呼び手の約束（大きさ 0 でない Layout）をそのまま渡す
        let p = unsafe { System.alloc(layout) };
        if p.is_null() {
            self.meter.refund(layout.size());
        }
        p
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        if !self.meter.charge(layout.size()) {
            return std::ptr::null_mut();
        }
        // SAFETY: 同上
        let p = unsafe { System.alloc_zeroed(layout) };
        if p.is_null() {
            self.meter.refund(layout.size());
        }
        p
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: 同じ Layout の alloc が System で取った領域
        unsafe { System.dealloc(ptr, layout) };
        self.meter.refund(layout.size());
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // 新しい大きさを数えてから取り直し、できたら古い大きさを返す（写すあいだは両方を持ちうる）
        if !self.meter.charge(new_size) {
            return std::ptr::null_mut();
        }
        // SAFETY: 呼び手の約束（ptr は layout で取った領域、new_size は 0 でない）をそのまま渡す
        let p = unsafe { System.realloc(ptr, layout, new_size) };
        if p.is_null() {
            self.meter.refund(new_size);
        } else {
            self.meter.refund(layout.size());
        }
        p
    }
}

fn main() -> ExitCode {
    if !svg_worker::is_worker_invocation() {
        return ExitCode::from(64);
    }
    svg_worker::serve(&PinnedOptions::default(), &ALLOC.meter)
}
