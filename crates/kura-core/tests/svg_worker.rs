//! SVG の働き手（決定 23）を本物の子プロセスで確かめる（Linux でも回る）。働き手はフォントなしの `kura-svg-worker`
//! （このクレートの bin）と、止まる・落ちる・壊れた返事を書く偽の働き手（`/bin/sh`）。
//!
//! - 成功の値・形・読み取りの記録・エラーが、プロセスの中の固定の段と同じ（パスでも読み口でも）。
//! - 時間の上限で止める（木を作る段で止まる S1 の形・描く段で止まる S4 の形・何も返さない子）。
//! - 見張りの中止で止める。子が落ちた・壊れた返事は `Decode`。大きすぎる入力は子を起こさずに断る。
//! - 確保の上限（ADR-164 (4)）：見積もりを通ったまま確保が膨らむ形は、子が上限で断って `Decode`（メモリ）。大きな値と層は、
//!   描く前に上限を広げて読める（値はプロセスの中と同じ）。
//! - 実行ファイルは起動のたびに求める。起動できない・求められない・別のビルド（ビルドの印・やり取りの版が違う）は、一時的な
//!   誤り（`is_unavailable`。M11 の 3 回目の確認 W2）。

// テストの補助関数は #[test] の外にあるので、テストと同じく unwrap などを許す（clippy.toml の allow-*-in-tests が届かない）
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use kura_core::codec_ext::svg_worker::{self, Deadlines, TIME_LIMIT, WorkerCommand};
use kura_engine::codec::{CodecSet, DecodeStamp, GenericFamilies, PinnedOptions, SvgFonts, svg};
use kura_engine::doc::DecodeOptions;
use kura_engine::{EngineError, Limits, Monitor, NoMonitor, Value};

fn worker() -> WorkerCommand {
    WorkerCommand::new(
        env!("CARGO_BIN_EXE_kura-svg-worker"),
        [svg_worker::WORKER_ARG],
    )
}

fn sh(script: &str) -> WorkerCommand {
    WorkerCommand::new("/bin/sh", ["-c", script])
}

fn set_with(cmd: WorkerCommand, deadlines: Deadlines) -> CodecSet {
    let mut set = CodecSet::pinned();
    svg_worker::install_with(&mut set, &PinnedOptions::default(), cmd, deadlines).unwrap();
    set
}

/// 値の一致を確かめるときの時間の上限（ビルドしたばかりの実行ファイルの最初の起動は、重い環境で 1 秒を超えることがあるので、
/// 時間で止まらないように長くする。時間の上限は `slow_svgs_are_stopped_at_the_deadline` が確かめる）。
fn generous() -> Deadlines {
    short(120_000, 120_000)
}

fn short(shape_ms: u64, decode_ms: u64) -> Deadlines {
    Deadlines {
        shape: Duration::from_millis(shape_ms),
        decode: Duration::from_millis(decode_ms),
    }
}

/// ふつうの形・読めない形・断る形（固定の段のファズの種と、確認のケースから）。
const CASES: &[(&str, &[u8])] = &[
    (
        "rect",
        br##"<svg xmlns="http://www.w3.org/2000/svg" width="10.5" height="4"><rect x="0" y="0" width="5" height="4" fill="#ff0000"/></svg>"##,
    ),
    (
        "use-marker",
        br##"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" width="20" height="20"><defs><g id="a"><rect width="3" height="3" fill="#f00"/></g><marker id="m"><circle r="1"/></marker></defs><use xlink:href="#a" x="4"/><path d="M0 0 L5 5 L9 2" stroke="#000" marker-mid="url(#m)"/></svg>"##,
    ),
    (
        "gradient-clip-pattern",
        br##"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="12" viewBox="0 0 8 6"><defs><linearGradient id="g" x1="0" x2="1"><stop offset="0" stop-color="#f00"/><stop offset="1" stop-color="#00f" stop-opacity="0.3"/></linearGradient><clipPath id="c"><rect width="6" height="4"/></clipPath><pattern id="p" width="2" height="2" patternUnits="userSpaceOnUse"><rect width="1" height="1"/></pattern></defs><g clip-path="url(#c)" transform="rotate(10 4 3)"><rect width="8" height="6" fill="url(#g)"/><ellipse cx="4" cy="3" rx="3" ry="1.5" fill="url(#p)" stroke="#0a0" stroke-width="0.5" stroke-dasharray="1 0.5"/></g></svg>"##,
    ),
    (
        "css-filter-mask",
        br##"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10"><style>.a{fill:#123456;opacity:.5}</style><filter id="f"><feGaussianBlur stdDeviation="1"/></filter><mask id="k"><rect width="10" height="5" fill="#fff"/></mask><path class="a" d="M1 1h8v8H1zM3 3l4 4m0-4q-2 3-4 0c1-1 2 2 3 0a2 2 0 1 1 1 1z" mask="url(#k)" filter="url(#f)"/></svg>"##,
    ),
    (
        "radial-opacity",
        br##"<svg xmlns="http://www.w3.org/2000/svg" width="300" height="200" viewBox="0 0 30 20"><defs><radialGradient id="r"><stop offset="0" stop-color="#fff"/><stop offset=".6" stop-color="#f80" stop-opacity=".5"/><stop offset="1" stop-color="#004"/></radialGradient></defs><g opacity="0.7"><circle cx="15" cy="10" r="9" fill="url(#r)"/><path d="M2 18 Q15 -5 28 18" fill="none" stroke="#0af" stroke-width="1.5" stroke-linecap="round"/></g></svg>"##,
    ),
    // 文字（フォントなし）・画像は Unsupported
    (
        "text",
        br#"<svg xmlns="http://www.w3.org/2000/svg" width="40" height="20"><text x="0" y="15">Kura</text></svg>"#,
    ),
    (
        "image",
        br#"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" width="4" height="4"><image width="4" height="4" xlink:href="/etc/hosts"/></svg>"#,
    ),
    // 見積もりで断る（Limit）・読めない（Decode）
    (
        "too-big",
        br#"<svg xmlns="http://www.w3.org/2000/svg" width="40000" height="4"/>"#,
    ),
    ("not-xml", b"<svg"),
    ("not-utf8", b"<svg \xff>"),
    ("svgz", b"\x1f\x8b\x08\x00"),
];

/// 木を作る段で止まる形（確認の S1 の 2 区間。release で 2.6 秒、debug はもっと遅い）。
const SLOW_SHAPE: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="100"><g transform="translate(1e9)"><path d="m 30.2,100.5 c 20.1,-30.4 60.2,-30.4 80.3,0 s 60.2,30.4 80.3,0" fill="none" stroke="#000" stroke-width="1e7" transform="rotate(-12.5)"/></g></svg>"##;

/// 形は速いが描く段で止まる形（確認の S4 の 1 本。release で 8.9 秒）。
const SLOW_DECODE: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" width="1000" height="1000"><path d="M0 5 C300 0 700 10 1000 5" stroke="black" stroke-width="3" stroke-linecap="round" stroke-dasharray="0.0005 0.000501" fill="none"/></svg>"##;

/// 木を作る段で、見積もりを通ったまま確保が膨らむ形（確認の S2 の小さい版）：stop の多いグラデーション（単位は
/// objectBoundingBox）を塗った図形を use で並べると、usvg は図形ごとに stop を写して木に持つ（stop 1 個 12 バイト）。
fn gradient_copies(stops: usize, uses: usize) -> String {
    let mut s = String::from(
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="100"><defs><linearGradient id="g">"#,
    );
    for i in 0..stops {
        s.push_str(&format!(
            r##"<stop offset="{:.6}" stop-color="#{:06x}"/>"##,
            (i + 1) as f64 / (stops + 1) as f64,
            i * 2_654_435 % 0x100_0000
        ));
    }
    s.push_str(r##"</linearGradient><rect id="r" width="9" height="9" fill="url(#g)"/></defs>"##);
    s.push_str(&r##"<use href="#r"/>"##.repeat(uses));
    s.push_str("</svg>");
    s
}

fn write(dir: &Path, name: &str, body: &[u8]) -> PathBuf {
    let p = dir.join(name);
    std::fs::write(&p, body).unwrap();
    p
}

type Read = Result<(Value, DecodeStamp), EngineError>;

fn same(a: &Read, b: &Read, what: &str) {
    match (a, b) {
        (Ok((va, sa)), Ok((vb, sb))) => {
            assert_eq!(va, vb, "{what}");
            assert_eq!(sa, sb, "{what}");
        }
        (Err(ea), Err(eb)) => same_error(ea, eb, what),
        _ => panic!("{what}: {a:?} と {b:?}"),
    }
}

fn same_error(a: &EngineError, b: &EngineError, what: &str) {
    assert_eq!(
        std::mem::discriminant(a),
        std::mem::discriminant(b),
        "{what}: {a} と {b}"
    );
    assert_eq!(a.to_string(), b.to_string(), "{what}");
}

/// 値・形・読み取りの記録・エラーが、プロセスの中の固定の段と同じ（パスで読む口・読み口・記録の識別子で読む口）。
#[test]
fn values_stamps_and_errors_match_the_in_process_decoder() {
    let dir = tempfile::tempdir().unwrap();
    let local = CodecSet::pinned();
    let remote = set_with(worker(), generous());
    let (opts, limits) = (DecodeOptions::default(), Limits::default());
    let mut ok = 0;
    for (name, body) in CASES {
        let path = write(dir.path(), &format!("{name}.svg"), body);
        same(
            &local.decode_path(&path, "svg", &opts, &limits, &NoMonitor),
            &remote.decode_path(&path, "svg", &opts, &limits, &NoMonitor),
            name,
        );
        let reader = |set: &CodecSet| {
            set.decode(
                &mut Cursor::new(body.to_vec()),
                "svg",
                &opts,
                &limits,
                &NoMonitor,
            )
        };
        same(&reader(&local), &reader(&remote), name);
        let by_id = |set: &CodecSet| {
            set.decode_with_path(svg::DEC_ID, &path, "svg", &opts, &limits, &NoMonitor)
        };
        same(&by_id(&local), &by_id(&remote), name);
        match (
            local.shape_path(&path, "svg", &opts, &limits),
            remote.shape_path(&path, "svg", &opts, &limits),
        ) {
            (Ok(a), Ok(b)) => {
                assert_eq!(a, b, "{name}");
                ok += 1;
            }
            (Err(a), Err(b)) => same_error(&a, &b, name),
            (a, b) => panic!("{name}: {a:?} と {b:?}"),
        }
    }
    assert_eq!(ok, 5, "読めるケースの数");
    // ないファイルは入出力のエラー（子を起こす前に）
    let missing = dir.path().join("missing.svg");
    match (
        local.decode_path(&missing, "svg", &opts, &limits, &NoMonitor),
        remote.decode_path(&missing, "svg", &opts, &limits, &NoMonitor),
    ) {
        (Err(EngineError::Io(a)), Err(EngineError::Io(b))) => assert_eq!(a.kind(), b.kind()),
        other => panic!("{other:?}"),
    }
    // 識別子・段・再現の約束は差し替えても同じ（能力の表も）
    assert_eq!(local.capabilities(), remote.capabilities());
    let d = remote.decoder_by_id(svg::DEC_ID).unwrap();
    assert!(!d.reproducible());
    // 予算の見積もりは前と同じ
    let shape = local
        .shape_path(&dir.path().join("rect.svg"), "svg", &opts, &limits)
        .unwrap()
        .0;
    assert_eq!(
        local.decode_work_bytes("svg", &shape),
        remote.decode_work_bytes("svg", &shape)
    );
}

/// 時間の上限：木を作る段で止まる形も、描く段で止まる形も、何も返さない子も、上限で止めて `Limit`（時間）にする。
#[test]
fn slow_svgs_are_stopped_at_the_deadline() {
    let dir = tempfile::tempdir().unwrap();
    let (opts, limits) = (DecodeOptions::default(), Limits::default());
    let slow_shape = write(dir.path(), "s1.svg", SLOW_SHAPE.as_bytes());
    let slow_decode = write(dir.path(), "s4.svg", SLOW_DECODE.as_bytes());
    // ビルドしたばかりの実行ファイルの最初の起動（重い環境で 1 秒を超えることがある）を、時間を測る前に済ませる
    let warm = set_with(worker(), generous());
    assert!(
        warm.shape(&mut Cursor::new(CASES[0].1.to_vec()), "svg", &opts, &limits)
            .is_ok()
    );
    let set = set_with(worker(), short(300, 600));
    let timed = |r: Result<(), EngineError>, what: &str, started: Instant| {
        match r {
            Err(EngineError::Limit(m)) => assert_eq!(m, TIME_LIMIT, "{what}"),
            other => panic!("{what}: {other:?}"),
        }
        assert!(
            started.elapsed() < Duration::from_millis(2_500),
            "{what}: {:?}",
            started.elapsed()
        );
    };
    let t = Instant::now();
    timed(
        set.shape_path(&slow_shape, "svg", &opts, &limits).map(drop),
        "S1 の形",
        t,
    );
    let t = Instant::now();
    timed(
        set.decode_path(&slow_decode, "svg", &opts, &limits, &NoMonitor)
            .map(drop),
        "S4 の値",
        t,
    );
    // 形は答える（描く段だけが遅い）
    assert!(warm.shape_path(&slow_decode, "svg", &opts, &limits).is_ok());
    let silent = set_with(sh("sleep 36.4159"), short(300, 300));
    let t = Instant::now();
    timed(
        silent
            .decode_path(&slow_decode, "svg", &opts, &limits, &NoMonitor)
            .map(drop),
        "黙る子",
        t,
    );
    assert!(svg_worker::is_time_limit(&EngineError::Limit(
        TIME_LIMIT.into()
    )));
    assert!(!svg_worker::is_time_limit(&EngineError::Limit(
        "SVG が大きすぎます".into()
    )));
}

/// 確保の上限（ADR-164 (4)）：木を作る段で見積もりを通ったまま確保が膨らむ形（stop 2 万 × use 2,000 の写しで約 490 MB。
/// 入力 0.9 MB・展開した要素 2.2 万なので、木の分の上限は 140 MB ほど）は、子が上限で断って落ち、親は `Decode`
/// （[`svg_worker::MEMORY_LIMIT`]）にする（形も値も、パスでも読み口でも）。プロセスの中では見積もりを通る（断れない）。
#[test]
fn memory_hungry_svgs_are_refused_by_the_cap() {
    let dir = tempfile::tempdir().unwrap();
    let (opts, limits) = (DecodeOptions::default(), Limits::default());
    let body = gradient_copies(20_000, 2_000);
    let path = write(dir.path(), "s2.svg", body.as_bytes());
    assert!(
        CodecSet::pinned()
            .shape_path(&path, "svg", &opts, &limits)
            .is_ok()
    );
    let set = set_with(worker(), generous());
    let started = Instant::now();
    for (what, r) in [
        ("形", set.shape_path(&path, "svg", &opts, &limits).map(drop)),
        (
            "値",
            set.decode_path(&path, "svg", &opts, &limits, &NoMonitor)
                .map(drop),
        ),
        (
            "読み口の値",
            set.decode(
                &mut Cursor::new(body.as_bytes().to_vec()),
                "svg",
                &opts,
                &limits,
                &NoMonitor,
            )
            .map(drop),
        ),
    ] {
        match r {
            Err(e @ EngineError::Decode { .. }) => {
                assert!(svg_worker::is_memory_limit(&e), "{what}: {e}");
                assert!(!svg_worker::is_time_limit(&e) && !svg_worker::is_unavailable(&e));
            }
            other => panic!("{what}: {other:?}"),
        }
    }
    // 上限で断るので、時間の上限を待たない
    assert!(
        started.elapsed() < Duration::from_secs(60),
        "{:?}",
        started.elapsed()
    );
}

/// 確保の上限は、値を読むとき描く前に、キャンバスの 3 枚（描く画像・乗算を戻した行・値）と一時の画像の見積もり（層）の分
/// だけ広げる：木の分の上限（64 MiB ほど）を超える大きな値（2,500² で 1 枚 25 MB）と層も読め、値はプロセスの中と同じ。
/// use の展開の多い形（図形 100 個のアイコン × 1,600：入力 0.06 MB・要素 16 万で、木は 120 MB ほど）も、展開した要素の数の分
/// だけ広げて読める。
#[test]
fn large_values_and_layers_fit_under_the_cap() {
    let dir = tempfile::tempdir().unwrap();
    let (opts, limits) = (DecodeOptions::default(), Limits::default());
    let body = r##"<svg xmlns="http://www.w3.org/2000/svg" width="2500" height="2500"><defs><linearGradient id="g" x2="1" y2="1"><stop offset="0" stop-color="#048"/><stop offset="1" stop-color="#fc8"/></linearGradient></defs><rect width="2500" height="2500" fill="url(#g)"/><g opacity="0.5"><rect x="10" y="10" width="2480" height="2480" fill="#0a4"/><circle cx="1250" cy="1250" r="900" fill="#f30"/></g></svg>"##;
    let path = write(dir.path(), "big.svg", body.as_bytes());
    let local = CodecSet::pinned();
    let remote = set_with(worker(), generous());
    let icons = {
        let icon: String = (0..100)
            .map(|i| {
                format!(
                    r#"<rect x="{}" y="{}" width="2" height="2"/>"#,
                    i % 10 * 3,
                    i / 10 * 3
                )
            })
            .collect();
        let uses: String = (0..1_600)
            .map(|i| {
                format!(
                    r##"<use href="#s" x="{}" y="{}"/>"##,
                    i % 40 * 32,
                    i / 40 * 32
                )
            })
            .collect();
        format!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="1280" height="1280"><defs><g id="s">{icon}</g></defs>{uses}</svg>"#
        )
    };
    let icons = write(dir.path(), "icons.svg", icons.as_bytes());
    for (what, path) in [("2,500² と層", &path), ("use の展開", &icons)] {
        let want = local.decode_path(path, "svg", &opts, &limits, &NoMonitor);
        let got = remote.decode_path(path, "svg", &opts, &limits, &NoMonitor);
        // 失敗のときに値（数 MB〜25 MB）を書き出さない
        let (Ok((a, sa)), Ok((b, sb))) = (&want, &got) else {
            panic!(
                "{what}: {:?} と {:?}",
                want.as_ref().err(),
                got.as_ref().err()
            );
        };
        assert!(a == b, "{what}: 値がプロセスの中と違います");
        assert_eq!(sa, sb, "{what}");
    }
}

/// 中止の旗を、時刻が来たら立てる見張り。
struct CancelAt {
    started: Instant,
    after: Duration,
    seen: AtomicBool,
}

impl Monitor for CancelAt {
    fn canceled(&self) -> bool {
        let c = self.started.elapsed() > self.after;
        if c {
            self.seen.store(true, Ordering::SeqCst);
        }
        c
    }
}

/// 見張りの中止で子を止める（形を読む口も、値を読む口も）。
#[test]
fn cancel_stops_the_child() {
    let dir = tempfile::tempdir().unwrap();
    let (opts, limits) = (DecodeOptions::default(), Limits::default());
    let path = write(dir.path(), "a.svg", CASES[0].1);
    let set = set_with(sh("sleep 37.4159"), short(30_000, 30_000));
    let mon = CancelAt {
        started: Instant::now(),
        after: Duration::from_millis(200),
        seen: AtomicBool::new(false),
    };
    assert!(matches!(
        set.decode_path(&path, "svg", &opts, &limits, &mon),
        Err(EngineError::Canceled)
    ));
    assert!(mon.started.elapsed() < Duration::from_secs(2));
    let mon = CancelAt {
        started: Instant::now(),
        after: Duration::from_millis(200),
        seen: AtomicBool::new(false),
    };
    assert!(matches!(
        set.shape_path_monitored(&path, "svg", &opts, &limits, &mon),
        Err(EngineError::Canceled)
    ));
    assert!(mon.seen.load(Ordering::SeqCst));
    assert!(mon.started.elapsed() < Duration::from_secs(2));
}

/// 子が落ちた・何も書かずに終わった・壊れた返事は `Decode`（プロセスは落ちない）。
#[test]
fn crashed_or_broken_workers_are_decode_errors() {
    let dir = tempfile::tempdir().unwrap();
    let (opts, limits) = (DecodeOptions::default(), Limits::default());
    let path = write(dir.path(), "a.svg", CASES[0].1);
    let decode_error = |cmd: WorkerCommand, want: &str| {
        let set = set_with(cmd, short(5_000, 5_000));
        for r in [
            set.decode_path(&path, "svg", &opts, &limits, &NoMonitor)
                .map(drop),
            set.shape_path(&path, "svg", &opts, &limits).map(drop),
        ] {
            match r {
                Err(EngineError::Decode { codec, message }) => {
                    assert_eq!(codec, svg::DEC_ID);
                    assert!(message.contains(want), "{want}: {message}");
                }
                other => panic!("{want}: {other:?}"),
            }
        }
    };
    decode_error(sh("cat >/dev/null; kill -SEGV $$"), "異常終了");
    // 確保を断られて落ちた（Rust の確保の失敗の言葉で abort）は、確保の上限の `Decode`
    decode_error(
        sh("cat >/dev/null; echo 'memory allocation of 1048576 bytes failed' >&2; kill -ABRT $$"),
        svg_worker::MEMORY_LIMIT,
    );
    decode_error(sh("cat >/dev/null; exit 3"), "異常終了");
    decode_error(sh("cat >/dev/null; printf garbage"), "壊れ");
    decode_error(sh("cat >/dev/null"), "壊れ");
}

/// 起動できない・実行ファイルが分からない・別のビルド（ビルドの印・やり取りの版・返事の印が違う）は、アイテムのせいでは
/// ない一時的な誤り（`is_unavailable`。`Decode` にしない。M11 の 3 回目の確認 W2）。
#[test]
fn missing_or_foreign_workers_are_unavailable() {
    let dir = tempfile::tempdir().unwrap();
    let (opts, limits) = (DecodeOptions::default(), Limits::default());
    let path = write(dir.path(), "a.svg", CASES[0].1);
    let unavailable = |set: &CodecSet, want: &str| {
        for r in [
            set.decode_path(&path, "svg", &opts, &limits, &NoMonitor)
                .map(drop),
            set.shape_path(&path, "svg", &opts, &limits).map(drop),
            set.decode(
                &mut Cursor::new(CASES[0].1.to_vec()),
                "svg",
                &opts,
                &limits,
                &NoMonitor,
            )
            .map(drop),
        ] {
            match r {
                Err(e) if svg_worker::is_unavailable(&e) => {
                    assert!(e.to_string().contains(want), "{want}: {e}");
                    assert_eq!(e.code(), "ToolMissing");
                }
                other => panic!("{want}: {other:?}"),
            }
        }
    };
    let generous = short(5_000, 5_000);
    unavailable(
        &set_with(
            WorkerCommand::new("/nonexistent/kura-svg-worker", [svg_worker::WORKER_ARG]),
            generous,
        ),
        "起動できません",
    );
    unavailable(
        &set_with(
            WorkerCommand::resolved_by(
                || Err(std::io::Error::other("消えました")),
                [svg_worker::WORKER_ARG],
            ),
            generous,
        ),
        "実行ファイルが分かりません: 消えました",
    );
    // 前の版のやり取りで答える（置き換えた前のビルド）・働き手でないもの
    unavailable(
        &set_with(
            sh(r"cat >/dev/null; printf 'KURASVG<\001\000\000\000\000\000\000\000'"),
            generous,
        ),
        "版が違います",
    );
    unavailable(
        &set_with(
            sh("cat >/dev/null; printf 'not a kura svg worker'"),
            generous,
        ),
        "働き手の返事ではありません",
    );
    // 本物の働き手でも、親とビルドの印（ここではフォントの有無）が違えば、子が断り、親も返事の印で断る
    let mut set = CodecSet::pinned();
    let fonts = PinnedOptions {
        svg_fonts: Some(SvgFonts::new(Vec::new(), GenericFamilies::default())),
    };
    svg_worker::install_with(&mut set, &fonts, worker(), generous).unwrap();
    unavailable(&set, "別のビルドです（fonts");
    // 印は設定とビルドで決まる（同じ設定なら同じ、フォントの有無で違う）
    assert_eq!(
        svg_worker::build_stamp(&PinnedOptions::default()),
        svg_worker::build_stamp(&PinnedOptions::default())
    );
    assert_ne!(
        svg_worker::build_stamp(&PinnedOptions::default()),
        svg_worker::build_stamp(&fonts)
    );
    assert!(svg_worker::build_stamp(&fonts).contains(svg::DEC_ID));
}

/// 実行ファイルは起動のたびに求める（動いているアプリを移す・改名すると、次の起動から新しい場所を使う。M11 の 3 回目の確認
/// W2）。求める口は、子を起動するたびに 1 回ずつ呼ばれる（`decode_path` は形と値で 2 回、`shape_path` は 1 回）。
#[test]
fn the_worker_executable_is_resolved_for_every_spawn() {
    let dir = tempfile::tempdir().unwrap();
    let (opts, limits) = (DecodeOptions::default(), Limits::default());
    let path = write(dir.path(), "a.svg", CASES[0].1);
    let a = dir.path().join("a");
    std::fs::create_dir(&a).unwrap();
    std::fs::copy(env!("CARGO_BIN_EXE_kura-svg-worker"), a.join("w")).unwrap();
    let current = Arc::new(Mutex::new(a.join("w")));
    let calls = Arc::new(AtomicUsize::new(0));
    let cmd = {
        let (current, calls) = (current.clone(), calls.clone());
        WorkerCommand::resolved_by(
            move || {
                calls.fetch_add(1, Ordering::SeqCst);
                Ok(current.lock().unwrap().clone())
            },
            [svg_worker::WORKER_ARG],
        )
    };
    let set = set_with(cmd, generous());
    let want = CodecSet::pinned()
        .decode_path(&path, "svg", &opts, &limits, &NoMonitor)
        .unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 0, "組むときには求めない");
    assert_eq!(
        set.decode_path(&path, "svg", &opts, &limits, &NoMonitor)
            .unwrap(),
        want
    );
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert!(set.shape_path(&path, "svg", &opts, &limits).is_ok());
    assert_eq!(calls.load(Ordering::SeqCst), 3);
    // 動いているあいだにフォルダを改名した：求める口が新しい場所を返せば、そのまま読める
    let b = dir.path().join("b");
    std::fs::rename(&a, &b).unwrap();
    *current.lock().unwrap() = b.join("w");
    assert_eq!(
        set.decode_path(&path, "svg", &opts, &limits, &NoMonitor)
            .unwrap(),
        want
    );
    assert_eq!(calls.load(Ordering::SeqCst), 5);
    // 求める口が古い場所のままなら、起動できない一時的な誤り（失敗にしない）
    *current.lock().unwrap() = a.join("w");
    let err = set
        .decode_path(&path, "svg", &opts, &limits, &NoMonitor)
        .unwrap_err();
    assert!(svg_worker::is_unavailable(&err), "{err}");
    assert_eq!(calls.load(Ordering::SeqCst), 6);
}

/// 入力の大きさの上限（16 MiB）は親が先に断る（子を起こさない：起こすと失敗で終わる偽の働き手でも `Limit`）。
#[test]
fn oversized_input_is_refused_before_starting_a_child() {
    let dir = tempfile::tempdir().unwrap();
    let (opts, limits) = (DecodeOptions::default(), Limits::default());
    let mut body = b"<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"4\" height=\"4\">".to_vec();
    body.resize(svg::MAX_SOURCE as usize + 1, b' ');
    let path = write(dir.path(), "big.svg", &body);
    let set = set_with(sh("exit 9"), Deadlines::default());
    let local = CodecSet::pinned();
    for (a, b) in [
        (
            local.decode_path(&path, "svg", &opts, &limits, &NoMonitor),
            set.decode_path(&path, "svg", &opts, &limits, &NoMonitor),
        ),
        (
            local.decode(
                &mut Cursor::new(body.clone()),
                "svg",
                &opts,
                &limits,
                &NoMonitor,
            ),
            set.decode(
                &mut Cursor::new(body.clone()),
                "svg",
                &opts,
                &limits,
                &NoMonitor,
            ),
        ),
    ] {
        same(&a, &b, "大きすぎる");
        assert!(matches!(b, Err(EngineError::Limit(_))));
    }
}

/// 同時の呼び出しは、それぞれの子で読む（同じ値）。
#[test]
fn concurrent_calls_each_use_their_own_child() {
    let dir = tempfile::tempdir().unwrap();
    let (opts, limits) = (DecodeOptions::default(), Limits::default());
    let path = write(dir.path(), "r.svg", CASES[4].1);
    let set = set_with(worker(), generous());
    let want = CodecSet::pinned()
        .decode_path(&path, "svg", &opts, &limits, &NoMonitor)
        .unwrap();
    std::thread::scope(|s| {
        let handles: Vec<_> = (0..8)
            .map(|_| s.spawn(|| set.decode_path(&path, "svg", &opts, &limits, &NoMonitor)))
            .collect();
        for h in handles {
            let got = h.join().unwrap().unwrap();
            assert_eq!(got, want);
        }
    });
}

/// 親が先にいなくなっても（アプリが落ちた・終わった）、子は頼みの時間の上限のあと少しで自分で終わる（止まる SVG を読み
/// 続けて、CPU とメモリを使い続ける子を残さない）。
#[test]
fn an_orphaned_worker_exits_on_its_own() {
    use std::io::Write as _;
    use std::process::{Command, Stdio};
    let dir = tempfile::tempdir().unwrap();
    let path = write(dir.path(), "s1.svg", SLOW_SHAPE.as_bytes());
    let mut request = Vec::new();
    svg_worker::wire::write_request(
        &mut request,
        &svg_worker::build_stamp(&PinnedOptions::default()),
        svg_worker::wire::Op::Shape,
        svg_worker::wire::Input::Path(&path),
        "svg",
        &DecodeOptions::default(),
        &Limits::default(),
        Duration::from_millis(300),
    )
    .unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_kura-svg-worker"))
        .arg(svg_worker::WORKER_ARG)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let started = Instant::now();
    child.stdin.take().unwrap().write_all(&request).unwrap();
    // 親は待つだけで止めない（上限 0.3 秒 ＋ 子の猶予 2 秒のあとに、子が自分で終わる）
    let status = loop {
        if let Some(s) = child.try_wait().unwrap() {
            break Some(s);
        }
        if started.elapsed() > Duration::from_secs(15) {
            break None;
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let took = started.elapsed();
    if status.is_none() {
        let _ = child.kill();
        let _ = child.wait();
    }
    let status = status.expect("子が自分で終わらない");
    assert!(!status.success(), "{status:?}");
    assert!(
        took >= svg_worker::SELF_GRACE && took < Duration::from_secs(10),
        "{took:?}"
    );
}
