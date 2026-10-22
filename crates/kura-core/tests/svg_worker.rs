//! SVG の働き手（決定 23）を本物の子プロセスで確かめる（Linux でも回る）。働き手はフォントなしの `kura-svg-worker`
//! （このクレートの bin）と、止まる・落ちる・壊れた返事を書く偽の働き手（`/bin/sh`）。
//!
//! - 成功の値・形・読み取りの記録・エラーが、プロセスの中の固定の段と同じ（パスでも読み口でも）。
//! - 時間の上限で止める（木を作る段で止まる S1 の形・描く段で止まる S4 の形・何も返さない子）。
//! - 見張りの中止で止める。子が落ちた・壊れた返事・起動できないは `Decode`。大きすぎる入力は子を起こさずに断る。

// テストの補助関数は #[test] の外にあるので、テストと同じく unwrap などを許す（clippy.toml の allow-*-in-tests が届かない）
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use kura_core::codec_ext::svg_worker::{self, Deadlines, TIME_LIMIT, WorkerCommand};
use kura_engine::codec::{CodecSet, DecodeStamp, svg};
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
    svg_worker::install_with(&mut set, Ok(cmd), deadlines).unwrap();
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

/// 子が落ちた・何も書かずに終わった・壊れた返事・起動できない・実行ファイルが分からないは `Decode`（プロセスは落ちない）。
#[test]
fn crashed_or_broken_workers_are_decode_errors() {
    let dir = tempfile::tempdir().unwrap();
    let (opts, limits) = (DecodeOptions::default(), Limits::default());
    let path = write(dir.path(), "a.svg", CASES[0].1);
    let decode_error = |cmd: Result<WorkerCommand, String>, want: &str| {
        let mut set = CodecSet::pinned();
        svg_worker::install_with(&mut set, cmd, short(5_000, 5_000)).unwrap();
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
    decode_error(Ok(sh("cat >/dev/null; kill -SEGV $$")), "異常終了");
    decode_error(Ok(sh("cat >/dev/null; exit 3")), "異常終了");
    decode_error(Ok(sh("cat >/dev/null; printf garbage")), "壊れ");
    decode_error(Ok(sh("cat >/dev/null")), "壊れ");
    decode_error(
        Ok(WorkerCommand::new(
            "/nonexistent/kura-svg-worker",
            [svg_worker::WORKER_ARG],
        )),
        "起動できません",
    );
    decode_error(
        Err("実行ファイルが分かりません".into()),
        "実行ファイルが分かりません",
    );
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
