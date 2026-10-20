//! glb の JSON を読むときの確保（M11 の検証 S3）。数える確保器（このテストのバイナリだけの `#[global_allocator]`）で、
//! `read_info`・`load_scene` の確保の山を測る。
//!
//! - 配列の要素・文字列・名前の並びを serde が組み立てると、JSON の 5〜20 倍を確保していた（32 MiB の JSON で 650 MiB）。
//!   形の上限（値の数・オブジェクトの数）を超えるものは serde に渡す前に断り、読めたものの山は情報の見積もり
//!   （`ModelInfo::json_heap`。サムネイルの予算の見積もり `estimate_load_bytes` に入る）以下で、64 MiB に収まること。

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

use kura_engine::NoMonitor;
use kura_engine::model::{ModelLimits, load_scene, read_info};

struct Counting;
static CUR: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

// SAFETY: System にそのまま渡し、成功した確保の大きさを数えるだけ。
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        // SAFETY: 呼び手の約束をそのまま System に渡す。
        let p = unsafe { System.alloc(l) };
        if !p.is_null() {
            let c = CUR.fetch_add(l.size(), Ordering::Relaxed) + l.size();
            PEAK.fetch_max(c, Ordering::Relaxed);
        }
        p
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        // SAFETY: 呼び手の約束をそのまま System に渡す。
        unsafe { System.dealloc(p, l) };
        CUR.fetch_sub(l.size(), Ordering::Relaxed);
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, n: usize) -> *mut u8 {
        // SAFETY: 呼び手の約束をそのまま System に渡す。
        let q = unsafe { System.realloc(p, l, n) };
        if !q.is_null() {
            if n > l.size() {
                let c = CUR.fetch_add(n - l.size(), Ordering::Relaxed) + (n - l.size());
                PEAK.fetch_max(c, Ordering::Relaxed);
            } else {
                CUR.fetch_sub(l.size() - n, Ordering::Relaxed);
            }
        }
        q
    }
}

#[global_allocator]
static A: Counting = Counting;

fn glb(json: &str) -> Vec<u8> {
    let mut j = json.as_bytes().to_vec();
    while !j.len().is_multiple_of(4) {
        j.push(b' ');
    }
    let total = 12 + 8 + j.len();
    let mut b = Vec::with_capacity(total);
    b.extend_from_slice(b"glTF");
    b.extend_from_slice(&2u32.to_le_bytes());
    b.extend_from_slice(&(total as u32).to_le_bytes());
    b.extend_from_slice(&(j.len() as u32).to_le_bytes());
    b.extend_from_slice(b"JSON");
    b.extend_from_slice(&j);
    b
}

/// `head` のあとに `item(i)` をカンマでつなぎ、`tail` で閉じる（全体が `target` バイトまで、`max` 個まで）。
fn fill(
    head: &str,
    item: &dyn Fn(usize) -> String,
    tail: &str,
    target: usize,
    max: usize,
) -> String {
    let mut s = String::from(head);
    let mut i = 0;
    loop {
        let it = item(i);
        if s.len() + it.len() + 1 + tail.len() > target || i >= max {
            break;
        }
        if i > 0 {
            s.push(',');
        }
        s.push_str(&it);
        i += 1;
    }
    s.push_str(tail);
    s
}

/// 確保を膨らませる形（検証の glbmem と同じもの）と、それらしい大きな場面。
fn patterns(target: usize) -> Vec<(&'static str, String)> {
    let asset = r#"{"asset":{"version":"2.0"}"#;
    let all = usize::MAX;
    vec![
        (
            "extensionsUsed の 1 字",
            fill(
                &format!(r#"{asset},"extensionsUsed":["#),
                &|_| r#""a""#.into(),
                "]}",
                target,
                all,
            ),
        ),
        (
            "extensionsUsed の空",
            fill(
                &format!(r#"{asset},"extensionsUsed":["#),
                &|_| r#""""#.into(),
                "]}",
                target,
                all,
            ),
        ),
        (
            "extensionsRequired",
            fill(
                &format!(r#"{asset},"extensionsRequired":["#),
                &|_| r#""a""#.into(),
                "]}",
                target,
                all,
            ),
        ),
        (
            "scenes の nodes",
            fill(
                &format!(r#"{asset},"scenes":[{{"nodes":["#),
                &|_| "0".into(),
                "]}]}",
                target,
                all,
            ),
        ),
        (
            "children",
            fill(
                &format!(r#"{asset},"nodes":[{{"children":["#),
                &|_| "0".into(),
                "]}]}",
                target,
                all,
            ),
        ),
        (
            "children（値の上限の手前）",
            fill(
                &format!(r#"{asset},"nodes":[{{"children":["#),
                &|_| "0".into(),
                "]}]}",
                target,
                (1 << 21) - 64,
            ),
        ),
        (
            "min",
            fill(
                &format!(r#"{asset},"accessors":[{{"type":"SCALAR","min":["#),
                &|_| "0".into(),
                "]}]}",
                target,
                all,
            ),
        ),
        (
            "attributes",
            fill(
                &format!(r#"{asset},"meshes":[{{"primitives":[{{"attributes":{{"#),
                &|i| format!(r#""{i:x}":0"#),
                "}}]}]}",
                target,
                all,
            ),
        ),
        (
            "材質の extensions",
            fill(
                &format!(r#"{asset},"materials":[{{"extensions":{{"#),
                &|i| format!(r#""{i:x}":0"#),
                "}}]}",
                target,
                all,
            ),
        ),
        (
            "プリミティブの extensions",
            fill(
                &format!(
                    r#"{asset},"meshes":[{{"primitives":[{{"attributes":{{}},"extensions":{{"#
                ),
                &|i| format!(r#""{i:x}":0"#),
                "}}]}]}",
                target,
                all,
            ),
        ),
        (
            "空の材質",
            fill(
                &format!(r#"{asset},"materials":["#),
                &|_| "{}".into(),
                "]}",
                target,
                131_000,
            ),
        ),
        (
            "buffer の uri",
            format!(
                r#"{asset},"buffers":[{{"uri":"data:{}"}}]}}"#,
                "A".repeat(target - 100)
            ),
        ),
        (
            "generator",
            format!(
                r#"{{"asset":{{"version":"2.0","generator":"{}"}}}}"#,
                "A".repeat(target - 100)
            ),
        ),
        (
            "accessor の type",
            format!(
                r#"{asset},"accessors":[{{"type":"{}"}}]}}"#,
                "A".repeat(target - 100)
            ),
        ),
        ("ノード 6 万の場面", {
            let n = 60_000;
            let nodes = (0..n)
                .map(|i| format!(r#"{{"name":"node{i}","mesh":0,"matrix":[1,0,0,0,0,1,0,0,0,0,1,0,{i},0,0,1]}}"#))
                .collect::<Vec<_>>()
                .join(",");
            let children = (1..n).map(|i| i.to_string()).collect::<Vec<_>>().join(",");
            format!(
                r#"{asset},"scene":0,"scenes":[{{"nodes":[0]}}],"nodes":[{{"children":[{children}]}},{nodes}],"meshes":[{{"primitives":[{{"attributes":{{"POSITION":0}}}}]}}],"accessors":[{{"bufferView":0,"componentType":5126,"count":3,"type":"VEC3","min":[0,0,0],"max":[1,1,1]}}],"bufferViews":[{{"buffer":0,"byteLength":36}}],"buffers":[{{"byteLength":36}}]}}"#
            )
        }),
    ]
}

/// `f` を呼ぶあいだの確保の山（呼ぶ前からの増え方）。
fn peak_of<T>(f: impl FnOnce() -> T) -> (T, usize) {
    let base = CUR.load(Ordering::Relaxed);
    PEAK.store(base, Ordering::Relaxed);
    let r = f();
    (r, PEAK.load(Ordering::Relaxed).saturating_sub(base))
}

/// 検証 S3：どの形でも、JSON を読む確保の山は 64 MiB 以下。
#[test]
fn json_allocations_are_bounded() {
    let limits = ModelLimits::default();
    for (name, json) in patterns(6 << 20) {
        let bytes = glb(&json);
        drop(json);
        let (info, peak) = peak_of(|| read_info(&bytes, &limits));
        assert!(
            peak <= 64 << 20,
            "{name}: read_info の確保の山 {peak} バイト（JSON {} バイト）",
            bytes.len()
        );
        if let Ok(info) = &info {
            assert!(
                peak as u64 <= info.json_heap,
                "{name}: 確保の山 {peak} バイトが見積もり {} を超える",
                info.json_heap
            );
        }
        let (_, scene_peak) = peak_of(|| load_scene(&bytes, &limits, &NoMonitor));
        assert!(
            scene_peak <= 64 << 20,
            "{name}: load_scene の確保の山 {scene_peak} バイト"
        );
        eprintln!(
            "{name}: JSON {} バイト・山 {peak}・{:?}",
            bytes.len(),
            info.as_ref().map(|_| ()).map_err(|e| e.to_string())
        );
    }
}
