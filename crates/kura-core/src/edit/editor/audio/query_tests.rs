//! 問い合わせ（A13・A14）：`edit_snap` は近いゼロ交差を返し、表示の外でも区間を描いて読む（表示を止めない）。

use kura_engine::Graph;
use serde_json::json;

use super::session_tests::*;
use super::snap::{SNAP_MAX_RADIUS, nearest_zero_crossing, snap};
use crate::edit::editor::Viewport;

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
    nearest_zero_crossing(&views, lo, f)
}

/// A13：公開した表示の値からも、表示の外を描いても、全体の描画のゼロ交差と同じ。半径は 4,096 で切る。表示を止めない。
#[test]
fn snap_finds_the_nearest_zero_crossing() {
    let env = Env::new();
    let frames = 130 * C;
    let bytes = noise_wav(8_000, 2, frames / 2, 51);
    // 短い音声（全体を表示）
    let id = add_audio(&env.lib, &bytes, "wav");
    let opened = env.open(id);
    let s = opened.session.clone();
    let out = env.step(
        &s,
        vec![add(0, "audio.gain", json!({ "db": -2.0 }))],
        &Viewport::audio_idle(),
    );
    let mut g = Graph::new_single();
    g.push_linear("audio.gain", 1, json!({ "db": -2.0 }), 1)
        .unwrap();
    let want = rendered(&bytes, "wav", &g);
    for f in [0, 1, 777, 3 * C + 5, frames / 2] {
        let got = snap(&env.env(), &s, f, 32).unwrap();
        let e = expected(&want, f.min(frames / 2), 32);
        assert_eq!(got.found, e.is_some(), "{f}");
        assert_eq!(got.frame, e.unwrap_or(f.min(frames / 2)), "{f}");
    }
    let status = out.status.clone();
    assert_eq!(
        env.step(&s, Vec::new(), &Viewport::audio_idle()).status,
        status
    );

    // 長い音声：表示の外を描いて読む
    let long_bytes = noise_wav(8_000, 1, frames, 52);
    let lid = add_audio(&env.lib, &long_bytes, "wav");
    let opened = env.open(lid);
    let s = opened.session.clone();
    env.step(
        &s,
        vec![add(0, "audio.gain", json!({ "db": 1.0 }))],
        &spans(&[(10 * C, 11 * C)]),
    );
    let want = rendered(&long_bytes, "wav", &g_gain(1.0));
    for f in [10 * C + 3, 100 * C + 9, frames - 1] {
        let got = snap(&env.env(), &s, f, 100_000).unwrap();
        let e = expected(&want, f, u64::from(SNAP_MAX_RADIUS));
        assert_eq!(got.frame, e.unwrap_or(f), "{f}");
        assert_eq!(got.found, e.is_some());
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
