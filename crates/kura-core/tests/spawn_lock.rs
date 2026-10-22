//! 子の起動の鍵（`tools::spawn`・`tools::status`。M11 の 3 回目の確認 W1）を本物の子プロセスで確かめる。
//!
//! macOS の std はパイプを `pipe()` で作ってから別の呼び出しで FD_CLOEXEC を付けるので、鍵がないと、その間に別のスレッドが
//! 起動した子がパイプの端を受け継ぐ。パイプ付きの子を並べて起動しながら、別のスレッドで起動した子に自分の開いている
//! ファイル記述子（`/dev/fd`）を数えさせ、ほかの起動のパイプの端を受け継いでいないことを見る（lsof を使わない）。
//! ほかのテストの起動（鍵を通らない）と混ざらないように、このファイルだけの実行ファイルにする。

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::io::Read;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use kura_core::tools;

/// 子が開いているファイル記述子の数（`/dev/fd` の項目。`ls` が読むのに開くものを含む）。
fn open_fds_of_a_child() -> usize {
    let mut child = tools::spawn(
        Command::new("/bin/ls")
            .arg("/dev/fd")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null()),
    )
    .unwrap();
    let mut out = String::new();
    child
        .stdout
        .take()
        .unwrap()
        .read_to_string(&mut out)
        .unwrap();
    assert!(child.wait().unwrap().success());
    out.lines().filter(|l| !l.trim().is_empty()).count()
}

/// パイプ付きの子を並べて起動していても、鍵を通して起動した子は、ほかの起動のパイプの端を受け継がない。
#[test]
fn children_do_not_inherit_pipes_of_other_spawns() {
    let baseline = open_fds_of_a_child();
    let stop = AtomicBool::new(false);
    let spawned = AtomicUsize::new(0);
    let leaks = AtomicUsize::new(0);
    let observed = AtomicUsize::new(0);
    let started = Instant::now();
    std::thread::scope(|s| {
        // パイプを作り続ける（標準入力・標準出力・標準エラーの 3 本）
        for _ in 0..6 {
            s.spawn(|| {
                while !stop.load(Ordering::SeqCst) {
                    let mut child = tools::spawn(
                        Command::new("/usr/bin/true")
                            .stdin(Stdio::piped())
                            .stdout(Stdio::piped())
                            .stderr(Stdio::piped()),
                    )
                    .unwrap();
                    drop(child.stdin.take());
                    child.wait().unwrap();
                    spawned.fetch_add(1, Ordering::Relaxed);
                }
            });
        }
        // 数える
        for _ in 0..4 {
            s.spawn(|| {
                while !stop.load(Ordering::SeqCst) {
                    let n = open_fds_of_a_child();
                    observed.fetch_add(1, Ordering::Relaxed);
                    if n != baseline {
                        leaks.fetch_add(1, Ordering::Relaxed);
                    }
                }
            });
        }
        std::thread::sleep(Duration::from_secs(3));
        stop.store(true, Ordering::SeqCst);
    });
    let (spawned, observed, leaks) = (
        spawned.load(Ordering::SeqCst),
        observed.load(Ordering::SeqCst),
        leaks.load(Ordering::SeqCst),
    );
    assert!(spawned > 100 && observed > 50, "{spawned} {observed}");
    assert_eq!(
        leaks,
        0,
        "{observed} 回のうち {leaks} 回、ほかの起動のパイプの端を受け継いだ（基準 {baseline}・{:?}）",
        started.elapsed()
    );
}
