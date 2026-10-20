//! 大きな確保を OS へすぐ返す確保器（アプリと kura-devtools の `#[global_allocator]`。M11）。
//!
//! macOS の libmalloc は、解放した大きな領域（large）を使い回しのために抱えたままにする（large cache）。解析・エンジンの
//! サムネイルのように数 MB〜数百 MB を確保しては捨てる処理のあとも、そのぶんがアプリのフットプリントに残り続け、
//! 10 万件の表示で 500 MB 以内（NFR-07）に戻らなかった（ジョブのあと 588 MB のまま。抱えた空の領域が 445 MB）。
//! `malloc_zone_pressure_relief` では返らず、切る設定（環境変数）は文書にない。
//!
//! そこで [`LARGE`] 以上の確保だけを `mmap` で直接取り、解放したら `munmap` で返す。それより小さいもの（エンジンの
//! 256² のタイル、音声のブロックなど）は今までどおり `System`（libmalloc）。どちらの道かは大きさと揃えだけで決まる
//! （解放・取り直しのときも同じ `Layout` が渡るので、目印を持たずに済む）。`mmap` の領域は 0 で埋まっているので、
//! 0 で埋めた確保は書かずに済む。macOS 以外は `System` をそのまま使う。

use std::alloc::{GlobalAlloc, Layout, System};

/// これ以上の確保を `mmap` で取る（1 MiB）。
pub const LARGE: usize = 1 << 20;

/// 大きな確保を OS へすぐ返す確保器。
#[derive(Debug, Default, Clone, Copy)]
pub struct ReturningAllocator;

#[cfg(target_os = "macos")]
mod imp {
    use super::{LARGE, Layout};
    use std::ptr;

    /// ページの大きさ（arm64 の macOS は 16 KiB）。`mmap` の領域はこれに揃う。
    fn page() -> usize {
        // SAFETY: 引数を書き換えない問い合わせ
        let size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
        usize::try_from(size).unwrap_or(16_384).max(4_096)
    }

    /// `mmap` の道を使う確保か（大きさと揃えだけで決める）。
    pub fn is_large(layout: &Layout) -> bool {
        layout.size() >= LARGE && layout.align() <= page()
    }

    /// ページに切り上げた大きさ。あふれたら None。
    fn rounded(size: usize) -> Option<usize> {
        let p = page();
        size.checked_add(p - 1).map(|s| s & !(p - 1))
    }

    /// 0 で埋まった領域を取る。失敗したらヌル。
    pub fn map(size: usize) -> *mut u8 {
        let Some(len) = rounded(size) else {
            return ptr::null_mut();
        };
        // SAFETY: 名前のない私用の領域を新しく作るだけで、既存の領域に触れない
        let p = unsafe {
            libc::mmap(
                ptr::null_mut(),
                len,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANON,
                -1,
                0,
            )
        };
        if p == libc::MAP_FAILED {
            ptr::null_mut()
        } else {
            p.cast()
        }
    }

    /// [`map`] で取った領域を返す。
    ///
    /// # Safety
    /// `p` は同じ `size` で [`map`] が返したもので、まだ返していないこと。
    pub unsafe fn unmap(p: *mut u8, size: usize) {
        if let Some(len) = rounded(size) {
            // SAFETY: 呼び手の約束どおり、map が作った長さ len の領域
            unsafe {
                libc::munmap(p.cast(), len);
            }
        }
    }
}

// SAFETY: 大きな確保は `mmap` が返すページ揃えの新しい領域（揃えはページ以下のときだけこの道）、ほかは `System` に任せる。
// 解放・取り直しは確保と同じ `Layout` で道を選ぶので、`System` の領域を `munmap` したり、その逆をしたりしない。
unsafe impl GlobalAlloc for ReturningAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        #[cfg(target_os = "macos")]
        if imp::is_large(&layout) {
            return imp::map(layout.size());
        }
        // SAFETY: GlobalAlloc の約束（大きさ 0 でない Layout）を呼び手が守る
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        #[cfg(target_os = "macos")]
        if imp::is_large(&layout) {
            // 新しい領域は 0 で埋まっている
            return imp::map(layout.size());
        }
        // SAFETY: 同上
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        #[cfg(target_os = "macos")]
        if imp::is_large(&layout) {
            // SAFETY: 同じ Layout の alloc が map で取った領域
            unsafe { imp::unmap(ptr, layout.size()) };
            return;
        }
        // SAFETY: 同じ Layout の alloc が System で取った領域
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        #[cfg(target_os = "macos")]
        {
            let Ok(new_layout) = Layout::from_size_align(new_size, layout.align()) else {
                return std::ptr::null_mut();
            };
            if imp::is_large(&layout) || imp::is_large(&new_layout) {
                // 道が変わりうるので、新しく取って写し、古いものを返す
                // SAFETY: new_layout は大きさ 0 でない（new_size は呼び手の約束で 0 でない）
                let fresh = unsafe { self.alloc(new_layout) };
                if !fresh.is_null() {
                    // SAFETY: 両方とも少なくとも min(古い大きさ, 新しい大きさ) の長さがあり、重ならない
                    unsafe {
                        std::ptr::copy_nonoverlapping(ptr, fresh, layout.size().min(new_size));
                        self.dealloc(ptr, layout);
                    }
                }
                return fresh;
            }
        }
        // SAFETY: 古い・新しいどちらも System の道
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[cfg(test)]
#[cfg(target_os = "macos")]
mod tests {
    use super::*;

    const A: ReturningAllocator = ReturningAllocator;

    fn layout(size: usize, align: usize) -> Layout {
        Layout::from_size_align(size, align).unwrap()
    }

    /// プロセスの物理フットプリント（proc_pid_rusage）。
    fn footprint() -> u64 {
        let mut info = std::mem::MaybeUninit::<libc::rusage_info_v2>::zeroed();
        // SAFETY: 自分のプロセスの番号と、書き込み先の領域を渡す。成功したときだけ読む
        let rc = unsafe {
            libc::proc_pid_rusage(
                std::process::id() as i32,
                libc::RUSAGE_INFO_V2,
                info.as_mut_ptr().cast(),
            )
        };
        assert_eq!(rc, 0);
        // SAFETY: 成功したので書き込まれている
        unsafe { info.assume_init() }.ri_phys_footprint
    }

    /// 大小どちらの道も、書いた値を読め、0 で埋めた確保は 0 で、揃えを守る。
    #[test]
    fn both_ways_hold_their_values_and_alignment() {
        for (size, align) in [
            (16, 8),
            (LARGE - 1, 16),
            (LARGE, 16),
            (LARGE + 1, 64),
            (3 * LARGE + 7, 4_096),
            (LARGE, 1 << 20), // ページより大きな揃えは System の道
        ] {
            let l = layout(size, align);
            // SAFETY: 大きさ 0 でない Layout で取り、同じ Layout で返す
            unsafe {
                let z = A.alloc_zeroed(l);
                assert!(!z.is_null());
                assert_eq!(z as usize % align, 0, "{size} {align}");
                assert!(std::slice::from_raw_parts(z, size).iter().all(|&b| b == 0));
                A.dealloc(z, l);

                let p = A.alloc(l);
                assert!(!p.is_null());
                assert_eq!(p as usize % align, 0);
                for i in (0..size).step_by(4_093) {
                    *p.add(i) = (i % 251) as u8;
                }
                *p.add(size - 1) = 0xAB;
                for i in (0..size - 1).step_by(4_093) {
                    assert_eq!(*p.add(i), (i % 251) as u8);
                }
                assert_eq!(*p.add(size - 1), 0xAB);
                A.dealloc(p, l);
            }
        }
    }

    /// 取り直しは、道をまたいでも（小 → 大 → 小）中身を保つ。
    #[test]
    fn realloc_keeps_the_contents_across_the_threshold() {
        let sizes = [1_000, LARGE - 8, LARGE, 5 * LARGE, 2 * LARGE + 3, 4_000, 10];
        let mut l = layout(sizes[0], 8);
        // SAFETY: 取った Layout を追いかけて、取り直し・解放に渡す
        unsafe {
            let mut p = A.alloc(l);
            for i in 0..sizes[0] {
                *p.add(i) = (i % 199) as u8;
            }
            let mut valid = sizes[0];
            for &next in &sizes[1..] {
                p = A.realloc(p, l, next);
                assert!(!p.is_null());
                valid = valid.min(next);
                for i in (0..valid).step_by(97) {
                    assert_eq!(*p.add(i), (i % 199) as u8, "{next} の {i}");
                }
                for i in valid..next {
                    *p.add(i) = (i % 199) as u8;
                }
                valid = next;
                l = layout(next, 8);
            }
            A.dealloc(p, l);
        }
    }

    /// Vec を伸ばしても、大きな領域で正しく伸びる（全体の確保器として使ったときの形）。
    #[test]
    fn growing_through_the_threshold_works_like_a_vec() {
        let mut l = layout(8, 4);
        // SAFETY: 同上
        unsafe {
            let mut p = A.alloc(l);
            let mut cap = 2_usize;
            let mut len = 0_usize;
            while len < 3_000_000 {
                if len == cap {
                    let next = cap * 2;
                    p = A.realloc(p, l, next * 4);
                    assert!(!p.is_null());
                    l = layout(next * 4, 4);
                    cap = next;
                }
                p.cast::<u32>().add(len).write(len as u32);
                len += 1;
            }
            for i in (0..len).step_by(997) {
                assert_eq!(p.cast::<u32>().add(i).read(), i as u32);
            }
            A.dealloc(p, l);
        }
    }

    /// 大きな確保を返すと、フットプリントがすぐ戻る（libmalloc の large cache に残らない）。
    #[test]
    fn large_blocks_are_returned_to_the_os() {
        let l = layout(64 << 20, 16);
        let before = footprint();
        // SAFETY: 取った領域に書いてから同じ Layout で返す
        unsafe {
            let blocks: Vec<*mut u8> = (0..4)
                .map(|_| {
                    let p = A.alloc(l);
                    assert!(!p.is_null());
                    std::ptr::write_bytes(p, 1, l.size());
                    p
                })
                .collect();
            let during = footprint();
            assert!(during >= before + (200 << 20), "{before} → {during}");
            for p in blocks {
                A.dealloc(p, l);
            }
        }
        let after = footprint();
        // ほかのテストが同じプロセスで動くので、返した 256 MiB に余裕を見て比べる
        assert!(after < before + (64 << 20), "{before} → {after}");
    }
}
