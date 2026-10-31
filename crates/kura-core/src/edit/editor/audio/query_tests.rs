//! 問い合わせ（A13・A14）：`edit_snap` は近いゼロ交差を返し、表示の外でも区間を描いて読む（表示を止めない）。

use kura_engine::Graph;
use serde_json::json;

use super::measure::{LoudnessState, measure, values_of};
use super::session_tests::*;
use super::snap::{SNAP_MAX_RADIUS, nearest_zero_crossing, snap};
use crate::edit::editor::{Step, Viewport};

/// 全体の描画で求めたゼロ交差（正）。
fn expected(a: &kura_engine::value::Audio, f: u64, r: u64) -> Option<u64> {
    let n = a.frames() as u64;
    let (lo, hi) = (f.saturating_sub(r + 1), (f + r + 1).min(n));
    let planes: Vec<Vec<f32>> = (0..a.channels())
        .map(|ch| {
            let mut v = vec![0f32; (hi - lo) as usize];
            a.read(ch, lo, &mut v).unwrap();
            v
        })
        .collect();
    let views: Vec<&[f32]> = planes.iter().map(Vec::as_slice).collect();
    nearest_zero_crossing(&views, lo, f, r)
}

/// 吸着が読む区間 `[f − r − 1, f + r + 1) ∩ [0, n)` を、公開した今の表示の値が覆うか（覆えば、そこから読む道を通る）。
fn published_covers(env: &Env, s: &str, rev: u64, f: u64, r: u64, n: u64) -> bool {
    let area = kura_engine::render::region::Area::span(f.saturating_sub(r + 1), (f + r + 1).min(n));
    env.editor(s).published_audio_covering(rev, &area).is_some()
}

/// A13：公開した表示の値（短い音声の Exact・長い音声の区間の継ぎはぎ）から読んでも、表示の外を描いて読んでも、全体の描画の
/// ゼロ交差と同じ。半径の内だけを返し、半径は 4,096 で切る。表示を止めない。
#[test]
fn snap_finds_the_nearest_zero_crossing() {
    let env = Env::new();
    // 短い音声（8 kHz ステレオ・3 チャンク余り。全体を表示）
    let n = 3 * C + 11;
    let bytes = noise_wav(8_000, 2, n, 51);
    let id = add_audio(&env.lib, &bytes, "wav");
    let opened = env.open(id);
    let s = opened.session.clone();
    let out = env.step(
        &s,
        vec![add(0, "audio.gain", json!({ "db": -2.0 }))],
        &Viewport::audio_idle(),
    );
    let info = shown_audio(&out.view);
    assert_eq!(info.valid, vec![(0, n)], "短い音声は全体を表示");
    let rev = info.rev;
    let (value, _) = env.editor(&s).published_audio(rev).unwrap();
    assert!(value.exact().is_some(), "全体を描いた値は Exact");
    let mut g = Graph::new_single();
    g.push_linear("audio.gain", 1, json!({ "db": -2.0 }), 1)
        .unwrap();
    let want = rendered(&bytes, "wav", &g);
    for f in [0, 1, 777, 3 * C + 5, n, n + 50] {
        let at = f.min(n);
        assert!(published_covers(&env, &s, rev, at, 32, n), "{f}");
        let got = snap(&env.env(), &s, f, 32).unwrap();
        let e = expected(&want, at, 32);
        assert_eq!(got.found, e.is_some(), "{f}");
        assert_eq!(got.frame, e.unwrap_or(at), "{f}");
        assert!(got.frame.abs_diff(at) <= 32, "半径の内 {f}");
    }
    let status = out.status.clone();
    assert_eq!(
        env.step(&s, Vec::new(), &Viewport::audio_idle()).status,
        status
    );

    // 長い音声：区間の表示の値（継ぎはぎ）が覆う所はそこから、覆わない所は区間を描いて読む
    let frames = 130 * C;
    let long_bytes = noise_wav(8_000, 1, frames, 52);
    let lid = add_audio(&env.lib, &long_bytes, "wav");
    let opened = env.open(lid);
    let s = opened.session.clone();
    let out = env.step(
        &s,
        vec![add(0, "audio.gain", json!({ "db": 1.0 }))],
        &spans(&[(10 * C, 11 * C)]),
    );
    let rev = shown_audio(&out.view).rev;
    let (value, _) = env.editor(&s).published_audio(rev).unwrap();
    assert!(value.exact().is_none(), "区間の表示の値は継ぎはぎ");
    let want = rendered(&long_bytes, "wav", &g_gain(1.0));
    let r = u64::from(SNAP_MAX_RADIUS);
    for (f, covered) in [
        (10 * C + 5_000, true),
        (11 * C - r - 2, true),
        (10 * C + 3, false),
        (100 * C + 9, false),
        (frames - 1, false),
    ] {
        assert_eq!(
            published_covers(&env, &s, rev, f, r, frames),
            covered,
            "{f}"
        );
        let got = snap(&env.env(), &s, f, 100_000).unwrap();
        let e = expected(&want, f, r);
        assert_eq!(got.frame, e.unwrap_or(f), "{f}");
        assert_eq!(got.found, e.is_some());
        assert!(got.frame.abs_diff(f) <= r, "半径の内 {f}");
    }
    // 吸着のあとも表示はそのまま使える（Superseded にしない）
    let out = env.step(&s, Vec::new(), &spans(&[(10 * C, 11 * C)]));
    assert_eq!(shown_audio(&out.view).valid, vec![(10 * C, 11 * C)]);
    // 知らないセッション
    assert!(snap(&env.env(), "abc", 0, 10).is_err());
}

fn g_gain(db: f64) -> Graph {
    let mut g = Graph::new_single();
    g.push_linear("audio.gain", 1, json!({ "db": db }), 1)
        .unwrap();
    g
}

/// A14：短い音声は `edit_measure` の中で計って Ready（`audio.loudness` を全体の描画に当てたものと同じ）。出力の指紋ごとに覚え、
/// 表示の情報にも載る。長い音声は段 4B まで Failed。
#[test]
fn measure_matches_the_analysis_of_the_render() {
    use kura_engine::analysis::NoParams;
    use kura_engine::analysis::audio::loudness::LoudnessAnalysis;
    let env = Env::new();
    let bytes = noise_wav(8_000, 2, 5 * C, 53);
    let id = add_audio(&env.lib, &bytes, "wav");
    let opened = env.open(id);
    let s = opened.session.clone();
    let vp = Viewport::audio_idle();
    assert_eq!(
        shown_audio(&opened.outcome.view).loudness,
        LoudnessState::Idle
    );
    let out = env.step(&s, vec![add(0, "audio.gain", json!({ "db": -6.0 }))], &vp);
    let sid = shown_audio(&out.view).samples_id.clone();
    let got = measure(&env.env(), &s).unwrap();
    let want = rendered(&bytes, "wav", &g_gain(-6.0));
    let report = kura_engine::Engine::new()
        .unwrap()
        .analyze::<LoudnessAnalysis>(
            &kura_engine::Value::Audio(want),
            &NoParams {},
            &kura_engine::NoMonitor,
        )
        .unwrap();
    assert_eq!(
        got,
        LoudnessState::Ready {
            samples_id: sid.clone(),
            values: values_of(&report)
        }
    );
    let LoudnessState::Ready { values, .. } = &got else {
        unreachable!()
    };
    assert!(values.loudness.is_some() && values.true_peak.is_some());
    // 表示の情報にも載る・取り消しで戻った標本は別の指紋・やり直しで覚えたものを返す
    let out = env.step(&s, Vec::new(), &vp);
    assert_eq!(shown_audio(&out.view).loudness, got);
    let out = env.step(&s, vec![Step::Undo], &vp);
    assert_eq!(shown_audio(&out.view).loudness, LoudnessState::Idle);
    let original = measure(&env.env(), &s).unwrap();
    assert!(matches!(&original, LoudnessState::Ready { samples_id, .. } if *samples_id != sid));
    let out = env.step(&s, vec![Step::Redo], &vp);
    assert_eq!(shown_audio(&out.view).loudness, got);
    assert_eq!(measure(&env.env(), &s).unwrap(), got);

    // 長い音声は段 4B まで計らない
    let long = add_audio(&env.lib, &noise_wav(8_000, 1, 130 * C, 54), "wav");
    let opened = env.open(long);
    let state = measure(&env.env(), &opened.session).unwrap();
    assert!(
        matches!(&state, LoudnessState::Failed { message, .. } if message.contains("まだ計れません")),
        "{state:?}"
    );
}

/// A14：「短い」は全体を描く見積もり（上流を含む）で決める。長い元 → gain → trim は出力が短くても、全体を描くと上流に長い値が
/// できるので長い音声と同じ扱い（段 4B まで Failed・結果を覚えない）。表示はそのまま描ける。上流が長い値を作らない
/// （長い元 → trim）なら計って Ready。
#[test]
fn measure_judges_the_whole_render_not_the_output() {
    use kura_engine::analysis::NoParams;
    use kura_engine::analysis::audio::loudness::LoudnessAnalysis;
    let env = Env::new();
    // 8 kHz モノラルで 32 MiB を超える長さ
    let bytes = noise_wav(8_000, 1, 130 * C + 5, 55);
    let id = add_audio(&env.lib, &bytes, "wav");
    let vp = Viewport::audio_idle();
    let s = env.open(id).session.clone();
    let out = env.step(
        &s,
        vec![
            add(0, "audio.gain", json!({ "db": -3.0 })),
            add(1, "audio.trim", json!({ "start": 0, "end": C })),
        ],
        &vp,
    );
    let info = shown_audio(&out.view);
    assert_eq!(info.frames, C);
    assert_eq!(info.valid, vec![(0, C)], "表示は短い音声として全体を描く");
    let state = measure(&env.env(), &s).unwrap();
    assert!(
        matches!(&state, LoudnessState::Failed { message, .. } if message.contains("まだ計れません")),
        "{state:?}"
    );
    let out = env.step(&s, Vec::new(), &vp);
    assert_eq!(
        shown_audio(&out.view).loudness,
        LoudnessState::Idle,
        "結果を覚えない"
    );

    // 長い元 → trim：上流に長い値を作らないので計る
    let s = env.open(id).session.clone();
    env.step(
        &s,
        vec![add(0, "audio.trim", json!({ "start": 7, "end": C + 7 }))],
        &vp,
    );
    let got = measure(&env.env(), &s).unwrap();
    let mut g = Graph::new_single();
    g.push_linear("audio.trim", 1, json!({ "start": 7, "end": C + 7 }), 1)
        .unwrap();
    let want = rendered(&bytes, "wav", &g);
    let report = kura_engine::Engine::new()
        .unwrap()
        .analyze::<LoudnessAnalysis>(
            &kura_engine::Value::Audio(want),
            &NoParams {},
            &kura_engine::NoMonitor,
        )
        .unwrap();
    assert!(
        matches!(&got, LoudnessState::Ready { values, .. } if *values == values_of(&report)),
        "{got:?}"
    );
}
