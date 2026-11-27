//! 長い音声の流し解き（A19〜A25。M17 段 3。docs/editing.md §18.3）：重い手（流し解きの推定が 25 ms を超えるか、出力が
//! 32 MiB を超える）は錠の中で解かず、裏の仕事の区切りで進め、その間は最後に公開した rev の計画で描き続ける。解き終えたら
//! 新しい rev を公開する。値はどちらも全体の描画と同じ。

use kura_engine::Graph;
use kura_engine::doc::NodeId;
use kura_engine::render::region::Area;
use serde_json::json;

use super::ChangedSpans;
use super::handles::{Handle, HandleMode};
use super::session_tests::*;
use crate::edit::editor::{AudioViewport, CommitMode, Edit, Step, Viewport};

/// 区間 `spans` を描く要求（裏の仕事は `work`、操作中は `interacting`）。
fn vp(spans: &[(u64, u64)], work: bool, interacting: bool) -> Viewport {
    Viewport::Audio(AudioViewport {
        spans: spans.to_vec(),
        work,
        interacting,
        ..AudioViewport::idle()
    })
}

/// 次の裏の仕事の区切りを、時間ではなくチャンクの数 `n` で区切る。
fn script(env: &Env, s: &str, n: u64) {
    env.editor(s)
        .lock_state()
        .as_mut()
        .unwrap()
        .audio
        .as_mut()
        .unwrap()
        .script = Some(n);
}

/// ベル 1 つの EQ の引数（8 kHz の音声）。
fn eq(gain: f64) -> serde_json::Value {
    json!({ "bands": [{ "type": "bell", "freq": 1000.0, "gainDb": gain, "q": 1.2 }], "phase": "minimum", "gainDb": 0.0 })
}

/// 段の多い EQ（96 dB/oct のカット 6 本 ＝ 48 段。短い音声でも流し解きの推定が境を超える）。
fn heavy_eq(gain: f64) -> serde_json::Value {
    let mut bands: Vec<serde_json::Value> = (0..6)
        .map(|i| json!({ "type": "lowCut", "freq": 30.0 + f64::from(i), "slope": 96 }))
        .collect();
    bands.truncate(5);
    bands.push(json!({ "type": "bell", "freq": 900.0, "gainDb": gain, "q": 1.0 }));
    json!({ "bands": bands, "phase": "minimum", "gainDb": 0.0 })
}

/// 公開した rev `rev` の表示の値の区間 [a, b) が、全体の描画 `want` と同じ。
fn same_as(env: &Env, s: &str, rev: u64, a: u64, b: u64, want: &kura_engine::value::Audio) {
    let (value, _) = env
        .editor(s)
        .published_audio_covering(rev, &Area::span(a, b))
        .expect("公開した表示が区間を覆っていない");
    for c in 0..want.channels() {
        let mut x = vec![0.0f32; (b - a) as usize];
        let mut y = vec![0.0f32; (b - a) as usize];
        value.audio_read(c, a, &mut x).unwrap();
        want.read(c, a, &mut y).unwrap();
        assert!(
            x.iter().zip(&y).all(|(p, q)| p.to_bits() == q.to_bits()),
            "rev {rev} のチャンネル {c} の [{a}, {b}) が全体の描画と違う"
        );
    }
}

/// 頭の文書の計画で、`n` 段の区切りで仕事を終えるまで `work: true` を頼む（区切りの数を返す）。
fn work_until_solved(env: &Env, s: &str, spans: &[(u64, u64)], n: u64) -> (usize, AudioOut) {
    let mut last = 0.0;
    for calls in 1..=500 {
        script(env, s, n);
        let out = env.step(s, Vec::new(), &vp(spans, true, false));
        let info = shown_audio(&out.view).clone();
        match info.solving {
            Some(p) => {
                assert!(p >= last, "進みが戻った：{p} < {last}");
                assert!(info.work, "解いている間は裏の仕事がある");
                assert!(info.rev < out.status.rev, "古い rev の表示");
                last = p;
            }
            None => {
                return (
                    calls,
                    AudioOut {
                        head: out.status.rev,
                        info,
                    },
                );
            }
        }
    }
    panic!("解き終わらない");
}

/// 応えの頭の rev と表示。
struct AudioOut {
    head: u64,
    info: crate::edit::editor::AudioViewInfo,
}

/// 文書（gain の上に EQ）。
fn graph(gain_db: f64, eq_params: serde_json::Value) -> Graph {
    let mut g = Graph::new_single();
    g.push_linear("audio.gain", 1, json!({ "db": gain_db }), 1)
        .unwrap();
    g.push_linear("audio.eq", 1, eq_params, 2).unwrap();
    g
}

/// A19：長い音声で状態の表を解き直す手は、錠の中で解かずに古い rev の表示を返し（値は前の計画の描画）、裏の仕事の区切り
/// （チャンクの数）で進めて解き終えると、新しい rev の表示（全体の描画と同じ）になる。先読みの区間も古い計画で描く。
#[test]
fn heavy_steps_draw_the_held_plan_until_solved() {
    let env = Env::new();
    let frames = 150 * C + 9;
    let bytes = noise_wav(8_000, 1, frames, 31);
    let id = add_audio(&env.lib, &bytes, "wav");
    let view = [(70 * C, 72 * C)];
    let s = env.open_with(id, vp(&view, false, false)).session.clone();
    // gain だけ（流し解きのない計画）：手の中で描く
    let out = env.step(
        &s,
        vec![add(0, "audio.gain", json!({ "db": -2.0 }))],
        &vp(&view, false, false),
    );
    let gained = shown_audio(&out.view).rev;
    assert_eq!(gained, out.status.rev);
    let out = env.step(
        &s,
        vec![add(1, "audio.eq", eq(4.0))],
        &vp(&view, false, false),
    );
    // 前の rev（gain だけ）の表示
    let info = shown_audio(&out.view);
    assert_eq!(info.solving, Some(0.0));
    assert!(info.work);
    assert_eq!(info.rev, gained);
    assert!(out.status.rev > gained);
    assert_eq!(info.valid, view.to_vec());
    let mut only_gain = Graph::new_single();
    only_gain
        .push_linear("audio.gain", 1, json!({ "db": -2.0 }), 1)
        .unwrap();
    let old = rendered(&bytes, "wav", &only_gain);
    same_as(&env, &s, info.rev, 70 * C, 72 * C, &old);
    // 先読みの区間（別の区間）も古い計画で描く
    let ahead = [(100 * C, 101 * C)];
    let out = env.step(&s, Vec::new(), &vp(&ahead, false, false));
    let info = shown_audio(&out.view);
    assert_eq!(info.valid, ahead.to_vec());
    same_as(&env, &s, info.rev, 100 * C, 101 * C, &old);
    // 区切りで進めて解き終える（151 チャンクを 40 ずつ）
    let (calls, done) = work_until_solved(&env, &s, &view, 40);
    assert!((4..=6).contains(&calls), "区切り {calls}");
    assert_eq!(done.info.rev, done.head, "解き終えたら頭の rev");
    assert!(
        !matches!(&done.info.changed, ChangedSpans::Spans(v) if v.is_empty()),
        "前の表示から変わった区間"
    );
    let want = rendered(&bytes, "wav", &graph(-2.0, eq(4.0)));
    same_as(&env, &s, done.info.rev, 70 * C, 72 * C, &want);
}

/// A20：仕事は要る結果の鍵の列で見分ける：下流の手（EQ の後ろの gain）では進みが残り、EQ の引数を変えると最初からになる
/// （最新の手だけ）。操作中（ドラッグ中）でも `work: true` なら進む。
#[test]
fn the_job_follows_the_needed_tables() {
    let env = Env::new();
    let frames = 150 * C;
    let bytes = noise_wav(8_000, 1, frames, 32);
    let id = add_audio(&env.lib, &bytes, "wav");
    let view = [(10 * C, 11 * C)];
    let s = env.open_with(id, vp(&view, false, false)).session.clone();
    env.step(
        &s,
        vec![add(0, "audio.eq", eq(3.0))],
        &vp(&view, false, false),
    );
    script(&env, &s, 30);
    let out = env.step(&s, Vec::new(), &vp(&view, true, true));
    let p1 = shown_audio(&out.view).solving.unwrap();
    assert!(p1 > 0.15 && p1 < 0.25, "操作中でも進む：{p1}");
    // 下流の gain：進みが残る
    let out = env.step(
        &s,
        vec![add(1, "audio.gain", json!({ "db": -1.0 }))],
        &vp(&view, false, false),
    );
    assert_eq!(shown_audio(&out.view).solving, Some(p1));
    // EQ の引数：最初から
    let out = env.step(&s, vec![set(1, eq(5.0))], &vp(&view, false, false));
    assert_eq!(shown_audio(&out.view).solving, Some(0.0));
    let (_, done) = work_until_solved(&env, &s, &view, 50);
    let mut g = Graph::new_single();
    g.push_linear("audio.eq", 1, eq(5.0), 1).unwrap();
    g.push_linear("audio.gain", 1, json!({ "db": -1.0 }), 2)
        .unwrap();
    same_as(
        &env,
        &s,
        done.info.rev,
        10 * C,
        11 * C,
        &rendered(&bytes, "wav", &g),
    );
}

/// A21：解いている間は、ゼロ交差・計り・波形の段が錠の中で表を解かない（ゼロ交差は表示の rev の計画で読み、計りは予約だけ）。
/// 取り消しで前の計画に戻ると、表がキャッシュに残っていれば今の計画ですぐ描く。
#[test]
fn queries_do_not_solve_while_solving() {
    let env = Env::new();
    let frames = 150 * C;
    let bytes = noise_wav(8_000, 1, frames, 33);
    let id = add_audio(&env.lib, &bytes, "wav");
    let view = [(5 * C, 6 * C)];
    let s = env.open_with(id, vp(&view, false, false)).session.clone();
    env.step(
        &s,
        vec![add(0, "audio.eq", eq(2.0))],
        &vp(&view, false, false),
    );
    let (_, first) = work_until_solved(&env, &s, &view, 200);
    let solved_rev = first.info.rev;
    env.step(&s, vec![set(1, eq(6.0))], &vp(&view, false, false));
    let solved_before = solves_in_cache(&env, &s);
    let snapped = super::snap::snap(&env.env(), &s, 5 * C + 1_000, 512).unwrap();
    assert!(snapped.frame >= 5 * C);
    assert!(matches!(
        super::measure::measure(&env.env(), &s).unwrap(),
        super::measure::LoudnessState::Measuring { .. }
    ));
    script(&env, &s, 1);
    let out = env.step(&s, Vec::new(), &vp(&view, true, false));
    assert!(shown_audio(&out.view).solving.is_some());
    assert_eq!(
        solves_in_cache(&env, &s),
        solved_before,
        "表を錠の中で解いた"
    );
    // 取り消し：前の計画の表はキャッシュに戻してあるので、今の計画ですぐ描く
    let out = env.step(&s, vec![Step::Undo], &vp(&view, false, false));
    let info = shown_audio(&out.view);
    assert_eq!(info.solving, None);
    assert_eq!(info.rev, solved_rev);
}

/// キャッシュが解いた解析の数（`analysis_solved`。流し解き・値に当てた解析）。
fn solves_in_cache(env: &Env, s: &str) -> u64 {
    env.editor(s)
        .lock_state()
        .as_ref()
        .unwrap()
        .cache
        .stats()
        .analysis_solved
}

/// A22：境の内（短い音声で推定が 25 ms 以下）は今までどおり手の中で解いて新しい rev を返す。短い音声でも推定が境を
/// 超えれば（48 段）裏の仕事になり、計りはその場で計らずに予約する。
#[test]
fn the_boundary_decides_by_the_estimate() {
    let env = Env::new();
    let frames = 4 * C;
    let bytes = noise_wav(8_000, 1, frames, 34);
    let id = add_audio(&env.lib, &bytes, "wav");
    let all = Viewport::audio_idle();
    let s = env.open_with(id, all.clone()).session.clone();
    // 1 段：4 チャンク × 13.6 ns ≈ 3.6 ms
    let out = env.step(&s, vec![add(0, "audio.eq", eq(2.0))], &all);
    let info = shown_audio(&out.view);
    assert_eq!(info.solving, None);
    assert_eq!(info.rev, out.status.rev);
    // 48 段：4 チャンク × 228 ns ≈ 60 ms
    let out = env.step(&s, vec![set(1, heavy_eq(2.0))], &all);
    let info = shown_audio(&out.view);
    assert!(info.solving.is_some());
    assert!(info.rev < out.status.rev);
    assert!(matches!(
        super::measure::measure(&env.env(), &s).unwrap(),
        super::measure::LoudnessState::Measuring { .. }
    ));
    let work = Viewport::Audio(AudioViewport {
        work: true,
        ..AudioViewport::idle()
    });
    let mut done = None;
    for _ in 0..50 {
        script(&env, &s, 2);
        let out = env.step(&s, Vec::new(), &work);
        if shown_audio(&out.view).solving.is_none() {
            done = Some(shown_audio(&out.view).rev);
            break;
        }
    }
    let rev = done.expect("解き終わらない");
    let mut g = Graph::new_single();
    g.push_linear("audio.eq", 1, heavy_eq(2.0), 1).unwrap();
    same_as(&env, &s, rev, 0, frames, &rendered(&bytes, "wav", &g));
}

/// A23：表示が古い間、ハンドルを写す時間の写しの列が表示した計画と違う手（EQ の上流の trim の道具で対象を外した直後）は
/// 断り、解き終えれば通る。写しが同じ手（EQ の後ろのフェード）は通る。
#[test]
fn handles_wait_while_the_time_map_differs() {
    let env = Env::new();
    let frames = 150 * C;
    let bytes = noise_wav(8_000, 1, frames, 35);
    let id = add_audio(&env.lib, &bytes, "wav");
    let view = [(0, C)];
    let s = env.open_with(id, vp(&view, false, false)).session.clone();
    env.step(
        &s,
        vec![
            add(0, "audio.trim", json!({ "start": 1000, "end": 140 * C })),
            add(1, "audio.eq", eq(2.0)),
            add(
                2,
                "audio.fade",
                json!({ "fadeIn": 0, "fadeOut": 0, "curve": "linear" }),
            ),
        ],
        &vp(&view, false, false),
    );
    work_until_solved(&env, &s, &view, 200);
    // 写しが同じ手：通る（表は変わらないので解き直しもない）
    let out = env.step(
        &s,
        vec![Step::Handle {
            handle: Handle::FadeIn { end: 500 },
            node: Some(NodeId(3)),
            mode: HandleMode::Commit(CommitMode::Step),
        }],
        &vp(&view, false, false),
    );
    assert!(out.rejected.is_none(), "{:?}", out.rejected);
    // trim の道具を開く（対象を仮に外す）：EQ の入力が変わるので解き直し（表示は古い）
    let out = env.step(
        &s,
        vec![Step::Preview {
            edit: Edit::SetEnabled {
                node: NodeId(1),
                enabled: false,
            },
        }],
        &vp(&view, false, false),
    );
    assert!(shown_audio(&out.view).solving.is_some());
    let trim = || Step::Handle {
        handle: Handle::Trim {
            start: 2_000,
            end: 130 * C,
        },
        node: Some(NodeId(1)),
        mode: HandleMode::Preview,
    };
    let out = env.step(&s, vec![trim()], &vp(&view, false, false));
    let r = out.rejected.expect("断るはず");
    assert!(
        r.problem.message.contains("変更を反映しています"),
        "{}",
        r.problem.message
    );
    work_until_solved(&env, &s, &view, 200);
    let out = env.step(&s, vec![trim()], &vp(&view, false, false));
    assert!(out.rejected.is_none(), "{:?}", out.rejected);
}

/// A24：正規化の手前を動かす手も同じ道に乗る（申告した解析なら何でも）。
#[test]
fn normalize_rides_the_same_path() {
    let env = Env::new();
    let frames = 140 * C;
    let bytes = noise_wav(8_000, 1, frames, 36);
    let id = add_audio(&env.lib, &bytes, "wav");
    let view = [(3 * C, 4 * C)];
    let s = env.open_with(id, vp(&view, false, false)).session.clone();
    env.step(
        &s,
        vec![
            add(0, "audio.gain", json!({ "db": -6.0 })),
            add(
                1,
                "audio.normalize",
                json!({ "target": -20.0, "peakLimit": null }),
            ),
        ],
        &vp(&view, false, false),
    );
    let (_, done) = work_until_solved(&env, &s, &view, 200);
    let out = env.step(
        &s,
        vec![set(1, json!({ "db": -3.0 }))],
        &vp(&view, false, false),
    );
    let info = shown_audio(&out.view);
    assert!(info.solving.is_some());
    assert_eq!(info.rev, done.info.rev);
    let (_, done) = work_until_solved(&env, &s, &view, 200);
    let mut g = Graph::new_single();
    g.push_linear("audio.gain", 1, json!({ "db": -3.0 }), 1)
        .unwrap();
    g.push_linear(
        "audio.normalize",
        1,
        json!({ "target": -20.0, "peakLimit": null }),
        2,
    )
    .unwrap();
    same_as(
        &env,
        &s,
        done.info.rev,
        3 * C,
        4 * C,
        &rendered(&bytes, "wav", &g),
    );
}

/// A25：閉じると、仕事・握った表・取り置きがすべて戻る。
#[test]
fn closing_drops_the_job_and_the_held_tables() {
    let env = Env::new();
    let frames = 80 * C;
    let id = add_audio(&env.lib, &noise_wav(8_000, 2, frames, 37), "wav");
    let view = [(0, C)];
    let s = env.open_with(id, vp(&view, false, false)).session.clone();
    env.step(
        &s,
        vec![add(0, "audio.eq", eq(1.0))],
        &vp(&view, false, false),
    );
    work_until_solved(&env, &s, &view, 200);
    env.step(&s, vec![set(1, eq(2.0))], &vp(&view, false, false));
    script(&env, &s, 3);
    env.step(&s, Vec::new(), &vp(&view, true, false));
    crate::edit::editor::close(&env.engine, &s);
    let u = env.usage();
    assert_eq!(u.standing, 0);
    assert_eq!(u.reserved, 0);
}

/// 見直しの直し：中止・予算の断りのほかの誤り（源が途中から読めない）が続いたら、仕事を捨てて錠の中で解く道で描き、誤りを
/// 表示の失敗として知らせる（「変更を反映しています」のままにしない）。同じ鍵の列の間は戻らない。
#[test]
fn persistent_errors_fall_back_and_report() {
    let env = Env::new();
    let frames = 150 * C;
    let bytes = noise_wav(8_000, 1, frames, 38);
    let id = add_audio(&env.lib, &bytes, "wav");
    let view = [(0, C)];
    let s = env.open_with(id, vp(&view, false, false)).session.clone();
    env.step(
        &s,
        vec![add(0, "audio.gain", json!({ "db": -1.0 }))],
        &vp(&view, false, false),
    );
    // 開いたあとで元のファイルを半分に切る（遅延の源は 60 チャンクから先を読めない）
    let hash = *blake3::hash(&bytes).as_bytes();
    let path = env.lib.layout().content_path(&hash, "wav");
    let f = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
    f.set_len(44 + 60 * C * 2).unwrap();
    drop(f);
    let out = env.step(
        &s,
        vec![add(1, "audio.eq", eq(3.0))],
        &vp(&view, false, false),
    );
    assert!(shown_audio(&out.view).solving.is_some());
    let mut failed = false;
    for _ in 0..50 {
        script(&env, &s, 16);
        let out = env.step(&s, Vec::new(), &vp(&view, true, false));
        match &out.view {
            crate::edit::editor::ViewOutcome::Shown(_) => {
                assert!(shown_audio(&out.view).solving.is_some());
            }
            crate::edit::editor::ViewOutcome::Failed(_) => {
                failed = true;
                break;
            }
            other => panic!("{other:?}"),
        }
    }
    assert!(failed, "誤りを知らせない");
    // 同じ鍵の列の間は裏の仕事に戻らない（錠の中で解いて、また失敗を知らせる）
    let out = env.step(&s, Vec::new(), &vp(&view, false, false));
    assert!(matches!(
        out.view,
        crate::edit::editor::ViewOutcome::Failed(_)
    ));
}

/// 見直しの直し：仕事が解いた上流の結果がキャッシュから消えても（保存の駐車の `clear` など）、解き終えた区切りで今の計画を
/// 描くときに錠の中で解き直さない（解けた結果を入れ直す）。
#[test]
fn solved_tables_survive_a_cleared_cache() {
    let env = Env::new();
    let frames = 150 * C;
    let bytes = noise_wav(8_000, 1, frames, 39);
    let id = add_audio(&env.lib, &bytes, "wav");
    let view = [(2 * C, 3 * C)];
    let s = env.open_with(id, vp(&view, false, false)).session.clone();
    env.step(
        &s,
        vec![add(0, "audio.gain", json!({ "db": -1.0 }))],
        &vp(&view, false, false),
    );
    // 正規化（上流の仕事）と EQ（下流の仕事）を足す
    env.step(
        &s,
        vec![
            add(
                1,
                "audio.normalize",
                json!({ "target": -20.0, "peakLimit": null }),
            ),
            add(2, "audio.eq", eq(2.0)),
        ],
        &vp(&view, false, false),
    );
    // 上流の仕事を解き終えるまで進める（151 チャンク。下流の仕事に入る）
    script(&env, &s, 160);
    let out = env.step(&s, Vec::new(), &vp(&view, true, false));
    assert!(shown_audio(&out.view).solving.unwrap() > 0.5);
    // キャッシュを空にする（駐車と同じ：入力と握る結果は入れ直す）
    {
        let editor = env.editor(&s);
        let mut guard = editor.lock_state();
        let st = guard.as_mut().unwrap();
        st.cache.clear();
        for input in st.inputs.values() {
            st.cache.register_external(&input.value);
        }
        editor.rehold(st);
    }
    let before = solves_in_cache(&env, &s);
    let (_, done) = work_until_solved(&env, &s, &view, 400);
    assert_eq!(
        solves_in_cache(&env, &s) - before,
        1,
        "下流の表のほかに解き直した"
    );
    let mut g = Graph::new_single();
    g.push_linear("audio.gain", 1, json!({ "db": -1.0 }), 1)
        .unwrap();
    g.push_linear(
        "audio.normalize",
        1,
        json!({ "target": -20.0, "peakLimit": null }),
        2,
    )
    .unwrap();
    g.push_linear("audio.eq", 1, eq(2.0), 3).unwrap();
    same_as(
        &env,
        &s,
        done.info.rev,
        2 * C,
        3 * C,
        &rendered(&bytes, "wav", &g),
    );
}

/// 直線位相のベル 1 つ（8 kHz で 20 Hz・Q 10 は帯域 2 Hz・65,537 タップ。核の重さはモノラルで 20.8 ns / フレーム、設計は 13 ms）。
fn linear_eq(gain: f64) -> serde_json::Value {
    json!({ "bands": [{ "type": "bell", "freq": 20.0, "gainDb": gain, "q": 10.0 }], "phase": "linear", "gainDb": 0.0 })
}

/// A26（M17 段 7）：重い核（直線位相）の手は、表示の範囲を描く推定が 25 ms を超えると、操作中の手の頼みだけ握る計画で描く
/// （古い rev・`solving`）。裏の仕事の頼み（操作中でも）と手が止まった頼みは今の計画で描き、操作中でない手・推定の小さい
/// 範囲はその場で描く。
#[test]
fn heavy_kernels_hold_only_while_interacting() {
    let env = Env::new();
    let frames = 150 * C + 9;
    let bytes = noise_wav(8_000, 1, frames, 36);
    let id = add_audio(&env.lib, &bytes, "wav");
    // 20 チャンク × 20.8 ns ≈ 27 ms（境を超える）
    let view = [(10 * C, 30 * C)];
    let s = env.open_with(id, vp(&view, false, false)).session.clone();
    // 操作中でない手：その場で描く
    let out = env.step(
        &s,
        vec![add(0, "audio.eq", linear_eq(4.0))],
        &vp(&view, false, false),
    );
    let info = shown_audio(&out.view);
    assert_eq!((info.rev, info.solving), (out.status.rev, None));
    let first = info.rev;
    // 操作中の手：握る計画（前の rev）
    let out = env.step(&s, vec![set(1, linear_eq(5.0))], &vp(&view, false, true));
    let info = shown_audio(&out.view);
    assert_eq!(info.rev, first);
    assert!(out.status.rev > first);
    assert_eq!(info.solving, Some(0.0));
    assert!(info.work);
    assert_eq!(info.valid, view.to_vec());
    // 操作中の裏の仕事の頼み（止まって 150 ms）：今の計画
    let out = env.step(&s, Vec::new(), &vp(&view, true, true));
    let info = shown_audio(&out.view);
    assert_eq!((info.rev, info.solving), (out.status.rev, None));
    let second = info.rev;
    // もう 1 手（操作中）は握る計画、手が止まった頼みは今の計画
    let out = env.step(&s, vec![set(1, linear_eq(6.0))], &vp(&view, false, true));
    assert_eq!(shown_audio(&out.view).rev, second);
    let out = env.step(&s, Vec::new(), &vp(&view, false, false));
    let info = shown_audio(&out.view);
    assert_eq!((info.rev, info.solving), (out.status.rev, None));
    // 推定の小さい範囲（2 チャンク ≈ 2.7 ms ＋ 設計 13 ms）は、操作中でもその場で描く
    let small = [(10 * C, 12 * C)];
    let out = env.step(&s, vec![set(1, linear_eq(7.0))], &vp(&small, false, true));
    let info = shown_audio(&out.view);
    assert_eq!((info.rev, info.solving), (out.status.rev, None));
}
