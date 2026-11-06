//! 長い音声のラウドネス（A16。M13 段 4B）：`edit_measure` は計りを予約して `Measuring` を返し、裏の仕事（`edit_view { work }`
//! の区切り）で窓ごとに流して解く。区切りは波形の段を先に進め、残った時間で計りを進める。結果は全体の描画に当てた解析と
//! ビット単位で同じ。操作中は進めず、操作中でない確定の表示で出力の標本が変わったら計りかけを捨てる（仮の表示では残す）。

use kura_engine::analysis::NoParams;
use kura_engine::analysis::audio::loudness::LoudnessAnalysis;
use kura_engine::{Engine, Graph, NoMonitor, Value};
use serde_json::json;

use super::measure::{LoudnessState, LoudnessValues, MEASURE_WINDOW_CHUNKS, measure, values_of};
use super::session_tests::*;
use super::slice::{Limit, WorkProgress};
use crate::edit::editor::{AudioViewport, Step, Viewport};

/// 区間 `spans` を描く要求（裏の仕事は `work`、操作中は `interacting`）。
fn vp(spans: &[(u64, u64)], work: bool, interacting: bool) -> Viewport {
    Viewport::Audio(AudioViewport {
        spans: spans.to_vec(),
        work,
        interacting,
        ..AudioViewport::idle()
    })
}

/// 全体の描画に当てたラウドネス（正）。
fn whole_loudness(bytes: &[u8], graph: &Graph) -> LoudnessValues {
    let want = rendered(bytes, "wav", graph);
    let report = Engine::new()
        .unwrap()
        .analyze::<LoudnessAnalysis>(&Value::Audio(want), &NoParams {}, &NoMonitor)
        .unwrap();
    values_of(&report)
}

/// 値がビット単位で同じ。
fn same_bits(got: &LoudnessValues, want: &LoudnessValues) {
    let bits = |v: &LoudnessValues| {
        [v.loudness, v.loudness_range, v.true_peak, v.sample_peak].map(|x| x.map(f64::to_bits))
    };
    assert_eq!(bits(got), bits(want), "{got:?} と {want:?}");
    assert_eq!(got.loudness_reason, want.loudness_reason);
}

/// 裏の仕事の区切りを、計りが済むまで頼む。段が残っている区切りでは計りが進まない（波形の段が先）。
fn work_until_measured(env: &Env, s: &str, work: &Viewport) -> (LoudnessState, usize) {
    let mut last = 0.0;
    for calls in 1..=1_000 {
        let out = env.step(s, Vec::new(), work);
        let info = shown_audio(&out.view);
        match &info.loudness {
            LoudnessState::Measuring { progress, .. } => {
                assert!(*progress >= last, "進みが戻った");
                if info.peaks.pending {
                    assert_eq!(*progress, last, "波形の段が残る区切りでは計りを進めない");
                }
                assert!(info.work, "計っている間は裏の仕事がある");
                last = *progress;
            }
            LoudnessState::Ready { .. } | LoudnessState::Failed { .. } => {
                return (info.loudness.clone(), calls);
            }
            LoudnessState::Idle => panic!("計りかけが消えました"),
        }
    }
    panic!("計り終えません");
}

/// A16：長い音声の計りは予約だけで返り、裏の仕事の区切りで進む。結果は全体の描画に当てた解析とビット単位で同じで、結果の
/// LRU に入る（表示の情報・`edit_measure` の両方に載る）。
#[test]
fn long_measure_runs_in_the_background_and_equals_the_whole_analysis() {
    let env = Env::new();
    // 8 kHz モノラルで 32 MiB を超える長さ（窓 9 つ）
    let frames = 130 * C;
    let bytes = noise_wav(8_000, 1, frames, 81);
    let id = add_audio(&env.lib, &bytes, "wav");
    let s = env.open_with(id, vp(&[], false, false)).session.clone();
    let idle = vp(&[(0, C)], false, false);
    let out = env.step(&s, vec![add(0, "audio.gain", json!({ "db": -6.0 }))], &idle);
    let sid = shown_audio(&out.view).samples_id.clone();
    let got = measure(&env.env(), &s).unwrap();
    assert_eq!(
        got,
        LoudnessState::Measuring {
            samples_id: sid.clone(),
            progress: 0.0
        }
    );
    // 予約しただけ：裏の仕事を頼まなければ進まない。表示の情報にも載り、`work` が真
    let out = env.step(&s, Vec::new(), &idle);
    let info = shown_audio(&out.view);
    assert_eq!(info.loudness, got);
    assert!(info.work);
    assert_eq!(
        measure(&env.env(), &s).unwrap(),
        got,
        "同じ出力の予約は 1 つ"
    );

    let (state, calls) = work_until_measured(&env, &s, &vp(&[(0, C)], true, false));
    assert!(calls > 1, "裏の仕事の区切りで小分けに進む（{calls} 回）");
    let LoudnessState::Ready { samples_id, values } = &state else {
        panic!("{state:?}")
    };
    assert_eq!(*samples_id, sid);
    let mut g = Graph::new_single();
    g.push_linear("audio.gain", 1, json!({ "db": -6.0 }), 1)
        .unwrap();
    same_bits(values, &whole_loudness(&bytes, &g));
    assert!(values.loudness.is_some() && values.true_peak.is_some());
    // 覚えた結果を返す・裏の仕事は残らない
    assert_eq!(measure(&env.env(), &s).unwrap(), state);
    let out = env.step(&s, Vec::new(), &idle);
    assert_eq!(shown_audio(&out.view).loudness, state);
    let editor = env.editor(&s);
    assert!(
        editor
            .lock_state()
            .as_ref()
            .unwrap()
            .audio
            .as_ref()
            .unwrap()
            .measure
            .run_position()
            .is_none()
    );
}

/// A16：操作中は計りを進めない。出力の標本が変わったら計りかけを捨て、取り消しで同じ標本に戻っても計りかけは戻らない
/// （計り直しは `edit_measure` から）。
#[test]
fn long_measure_waits_while_interacting_and_drops_stale_runs() {
    let env = Env::new();
    let frames = 130 * C;
    let id = add_audio(&env.lib, &noise_wav(8_000, 1, frames, 82), "wav");
    let s = env.open_with(id, vp(&[], false, false)).session.clone();
    let idle = vp(&[(0, C)], false, false);
    let out = env.step(&s, vec![add(0, "audio.gain", json!({ "db": -6.0 }))], &idle);
    let a = shown_audio(&out.view).samples_id.clone();
    let measuring = LoudnessState::Measuring {
        samples_id: a.clone(),
        progress: 0.0,
    };
    assert_eq!(measure(&env.env(), &s).unwrap(), measuring);
    let editor = env.editor(&s);
    let position = || {
        editor
            .lock_state()
            .as_ref()
            .unwrap()
            .audio
            .as_ref()
            .unwrap()
            .measure
            .run_position()
    };
    let chunks = frames.div_ceil(C) as usize;
    assert_eq!(position(), Some((a.clone(), 0, chunks)));
    // 操作中（裏の仕事を頼んでいても）は進めない
    let dragging = vp(&[(0, C)], true, true);
    for _ in 0..3 {
        let out = env.step(&s, Vec::new(), &dragging);
        assert_eq!(shown_audio(&out.view).loudness, measuring);
    }
    assert_eq!(position(), Some((a.clone(), 0, chunks)));
    // 標本が変わった：計りかけを捨てる
    let out = env.step(&s, vec![set(1, json!({ "db": -7.0 }))], &idle);
    let info = shown_audio(&out.view);
    assert_ne!(info.samples_id, a);
    assert_eq!(info.loudness, LoudnessState::Idle);
    assert_eq!(position(), None);
    // 取り消しで同じ標本に戻っても、計りかけはない
    let out = env.step(&s, vec![Step::Undo], &idle);
    let info = shown_audio(&out.view);
    assert_eq!(info.samples_id, a);
    assert_eq!(info.loudness, LoudnessState::Idle);
    assert_eq!(measure(&env.env(), &s).unwrap(), measuring, "計り直す");
    assert_eq!(position(), Some((a, 0, chunks)));
}

/// 段 4B の検証の直し 1：計りの途中の仮の表示（ドラッグ中の Preview。標本が違う）では計りかけを捨てない。CancelPreview で
/// 同じ出力に戻れば、計りかけが残っていて、裏の仕事の区切りで計り終える（結果は全体の解析とビット単位で同じ）。捨てるのは
/// 操作中でない確定の表示で標本が替わったときだけ。
#[test]
fn long_measure_survives_a_canceled_preview() {
    use kura_engine::doc::NodeId;
    use kura_engine::session::Edit;

    let env = Env::new();
    let frames = 130 * C;
    let bytes = noise_wav(8_000, 1, frames, 83);
    let id = add_audio(&env.lib, &bytes, "wav");
    let s = env.open_with(id, vp(&[], false, false)).session.clone();
    let idle = vp(&[(0, C)], false, false);
    let out = env.step(&s, vec![add(0, "audio.gain", json!({ "db": -6.0 }))], &idle);
    let a = shown_audio(&out.view).samples_id.clone();
    assert!(matches!(
        measure(&env.env(), &s).unwrap(),
        LoudnessState::Measuring { .. }
    ));
    let editor = env.editor(&s);
    let position = || {
        editor
            .lock_state()
            .as_ref()
            .unwrap()
            .audio
            .as_ref()
            .unwrap()
            .measure
            .run_position()
    };
    let preview = |db: f64| Step::Preview {
        edit: Edit::SetParams {
            node: NodeId(1),
            params: json!({ "db": db }),
        },
    };
    // ドラッグ中の仮の表示（別の標本）：計りかけは残る（進めない）
    let dragging = vp(&[(0, C)], true, true);
    for db in [-7.0, -8.0] {
        let out = env.step(&s, vec![preview(db)], &dragging);
        let info = shown_audio(&out.view);
        assert_ne!(info.samples_id, a);
        assert_eq!(info.loudness, LoudnessState::Idle);
        assert_eq!(
            position().map(|p| p.0),
            Some(a.clone()),
            "仮の表示では捨てない"
        );
    }
    // 手を離した仮の表示（操作中でない）でも、仮の値の間は捨てない
    let out = env.step(&s, vec![preview(-9.0)], &idle);
    assert_ne!(shown_audio(&out.view).samples_id, a);
    assert_eq!(position().map(|p| p.0), Some(a.clone()));
    // 取り消して同じ出力に戻る：計りかけがそのまま見え、裏の仕事で計り終える
    let out = env.step(&s, vec![Step::CancelPreview], &idle);
    let info = shown_audio(&out.view);
    assert_eq!(info.samples_id, a);
    assert!(
        matches!(&info.loudness, LoudnessState::Measuring { samples_id, .. } if *samples_id == a),
        "{:?}",
        info.loudness
    );
    assert!(info.work, "計りかけがあるので裏の仕事がある");
    let (state, _) = work_until_measured(&env, &s, &vp(&[(0, C)], true, false));
    let LoudnessState::Ready { samples_id, values } = &state else {
        panic!("{state:?}")
    };
    assert_eq!(*samples_id, a);
    let mut g = Graph::new_single();
    g.push_linear("audio.gain", 1, json!({ "db": -6.0 }), 1)
        .unwrap();
    same_bits(values, &whole_loudness(&bytes, &g));
    // 確定の表示（操作中でない）で標本が替われば捨てる
    assert_eq!(
        measure(&env.env(), &s).unwrap(),
        state,
        "計り終えたものは覚えている"
    );
    let out = env.step(&s, vec![set(1, json!({ "db": -5.0 }))], &idle);
    let b = shown_audio(&out.view).samples_id.clone();
    assert!(matches!(
        measure(&env.env(), &s).unwrap(),
        LoudnessState::Measuring { .. }
    ));
    assert_eq!(position().map(|p| p.0), Some(b.clone()));
    let out = env.step(&s, vec![Step::Undo], &idle);
    assert_eq!(shown_audio(&out.view).samples_id, a);
    assert_eq!(position(), None, "確定の表示で標本が替わったので捨てた");
}

/// 段 4B の検証の直し 6（#8）：計りの窓の途中で中止されたら（新しい要求・閉じる）、その窓は扇に渡さず次の窓の番号を進めない
/// （次の区切りで同じ窓を流し直す）。Failed にせず計りかけを残し、そのあとの普通の区切りで計り終えた結果は、全体の描画に
/// 当てた解析とビット単位で同じ（窓を飛ばしたり、扇に半端に渡したりしていない）。
#[test]
fn long_measure_redoes_a_window_canceled_midway() {
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use kura_engine::monitor::CancelAfter;

    let env = Env::new();
    let frames = 130 * C;
    let bytes = noise_wav(8_000, 1, frames, 84);
    let id = add_audio(&env.lib, &bytes, "wav");
    let s = env.open_with(id, vp(&[], false, false)).session.clone();
    let idle = vp(&[(0, C)], false, false);
    let out = env.step(&s, vec![add(0, "audio.gain", json!({ "db": -6.0 }))], &idle);
    let sid = shown_audio(&out.view).samples_id.clone();
    assert!(matches!(
        measure(&env.env(), &s).unwrap(),
        LoudnessState::Measuring { .. }
    ));
    let editor = env.editor(&s);
    // 親の見張りが n 回目の問いから中止を返す世代で、計りを直に進める（1 回目は窓の頭の確かめ。2 回目からは窓を流している
    // 途中のどこか）
    for n in 2..=5 {
        let mut guard = editor.lock_state();
        let st = guard.as_mut().unwrap();
        let (_, before, chunks) = st.audio.as_ref().unwrap().measure.run_position().unwrap();
        let ticket = editor.latest.issue(Arc::new(CancelAfter::new(n)));
        let mut limit = Limit::Deadline(Instant::now() + Duration::from_secs(60));
        let mut progress = WorkProgress::default();
        editor.advance_measure(st, &sid, &mut limit, &mut progress, &ticket);
        assert!(!progress.stalled(), "中止は進まなかったことにしない");
        let a = st.audio.as_ref().unwrap();
        let (run, next, _) = a.measure.run_position().expect("中止で計りかけを捨てない");
        assert_eq!(run, sid);
        if n == 2 {
            assert_eq!(next, before, "途中で中止した窓は進めない");
        }
        assert!(next >= before && next < chunks, "{n}: {before} → {next}");
        assert!(
            matches!(a.measure.state(&sid), LoudnessState::Measuring { .. }),
            "{n}: {:?}",
            a.measure.state(&sid)
        );
    }
    let (state, _) = work_until_measured(&env, &s, &vp(&[(0, C)], true, false));
    let LoudnessState::Ready { samples_id, values } = &state else {
        panic!("{state:?}")
    };
    assert_eq!(*samples_id, sid);
    let mut g = Graph::new_single();
    g.push_linear("audio.gain", 1, json!({ "db": -6.0 }), 1)
        .unwrap();
    same_bits(values, &whole_loudness(&bytes, &g));
}

/// 源の読みの数（遅延の入力の源）。
fn source_stats(env: &Env, s: &str) -> kura_engine::audio::source::SourceStats {
    env.editor(s)
        .audio_counts()
        .and_then(|c| c.source)
        .expect("遅延の入力")
}

/// 段 7（F15 の申し送り 2）：裏の仕事（波形の段の作り直し・ラウドネスの計り）は出力の全体を流すが、源の LRU の主の段を
/// 変えない（読み通し）。見えている区間を描いたあと裏の仕事を済ませても、次の全体に効く手（gain の引数）は区間を源から
/// 読み直さない。裏の仕事の読みは LRU に入れなかった数（`streamed`）に出る。
#[test]
fn background_work_keeps_the_visible_chunks_in_the_source_lru() {
    let env = Env::new();
    // 8 kHz モノラル 131 チャンク（32.75 MiB）。源の LRU は 8 MiB（主の段 24 チャンク・流しの段 8 チャンク）
    let frames = 130 * C + 5;
    let bytes = noise_wav(8_000, 1, frames, 85);
    let id = add_audio(&env.lib, &bytes, "wav");
    let span = [(60 * C, 68 * C)];
    let idle = vp(&span, false, false);
    let s = env.open_with(id, idle.clone()).session.clone();
    let out = env.step(&s, vec![add(0, "audio.gain", json!({ "db": -6.0 }))], &idle);
    assert!(shown_audio(&out.view).peaks.pending, "波形の段は裏の仕事");
    assert!(matches!(
        measure(&env.env(), &s).unwrap(),
        LoudnessState::Measuring { .. }
    ));
    let before = source_stats(&env, &s);
    let work = Viewport::Audio(AudioViewport {
        spans: span.to_vec(),
        visible: Some(span[0]),
        work: true,
        ..AudioViewport::idle()
    });
    let (state, _) = work_until_measured(&env, &s, &work);
    assert!(matches!(state, LoudnessState::Ready { .. }), "{state:?}");
    let after_work = source_stats(&env, &s);
    assert!(
        after_work.streamed >= 131 - 8,
        "裏の仕事は区間の外を読み通す: {before:?} → {after_work:?}"
    );
    // 次の全体に効く手：区間は LRU の主の段に残っているので、源から読まない
    let out = env.step(&s, vec![set(1, json!({ "db": -7.0 }))], &idle);
    assert!(!shown_audio(&out.view).valid.is_empty());
    let after_step = source_stats(&env, &s);
    assert_eq!(
        after_step.reads, after_work.reads,
        "裏の仕事のあとの手は区間を読み直さない: {after_work:?} → {after_step:?}"
    );
}

/// 段 7 の検証の直し（裏の仕事の空回り）：計りの窓の確保が断られたら、1 チャンクも進まないことを知らせ、計りかけは残す。空いたら
/// 続きから進む。
#[test]
fn long_measure_reports_a_refused_budget() {
    use std::sync::Arc;

    use kura_engine::budget::Class;
    use kura_engine::monitor::CancelFlag;

    let env = Env::new();
    let frames = 130 * C;
    let bytes = noise_wav(8_000, 1, frames, 86);
    let id = add_audio(&env.lib, &bytes, "wav");
    let budget = env.engine.runtime().unwrap().budget().clone();
    // 源の LRU（8 MiB）と少しだけを残して埋める
    let held = budget
        .try_acquire(budget.total() - (8 << 20) - (1 << 20), Class::Interactive)
        .unwrap()
        .unwrap();
    let idle = vp(&[], false, false);
    let s = env.open_with(id, idle.clone()).session.clone();
    let out = env.step(&s, vec![add(0, "audio.gain", json!({ "db": -6.0 }))], &idle);
    let sid = shown_audio(&out.view).samples_id.clone();
    assert!(matches!(
        measure(&env.env(), &s).unwrap(),
        LoudnessState::Measuring { .. }
    ));
    let editor = env.editor(&s);
    let advance = || {
        let mut guard = editor.lock_state();
        let st = guard.as_mut().unwrap();
        let ticket = editor.latest.issue(Arc::new(CancelFlag::default()));
        let mut limit = Limit::Chunks(MEASURE_WINDOW_CHUNKS);
        let mut progress = WorkProgress::default();
        editor.advance_measure(st, &sid, &mut limit, &mut progress, &ticket);
        let at = st.audio.as_ref().unwrap().measure.run_position().unwrap().1;
        (progress, at)
    };
    let (progress, at) = advance();
    assert!(progress.stalled(), "{progress:?}");
    assert_eq!(at, 0, "計りかけは残す");
    drop(held);
    let (progress, at) = advance();
    assert_eq!(progress.moved, MEASURE_WINDOW_CHUNKS);
    assert!(!progress.stalled());
    assert_eq!(at, MEASURE_WINDOW_CHUNKS as usize);
}

/// テスト：次の裏の仕事の区切りを、時間ではなくチャンクの数 `n` で区切る。
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

/// 段 7（F15 の申し送り 3）：計りの区切りは窓（`MEASURE_WINDOW_CHUNKS`）の途中でもチャンクごとに止める。区切りに渡した
/// チャンクの数だけ進み（窓の途中で止まり）、続きは次の区切りで流す。ランダムな数で区切って計り終えた結果は、全体の描画に
/// 当てた解析とビット単位で同じ。
#[test]
fn long_measure_stops_mid_window_and_resumes_bit_exact() {
    use rand_chacha::rand_core::Rng;

    let env = Env::new();
    let frames = 130 * C + 7;
    let bytes = noise_wav(8_000, 1, frames, 86);
    let id = add_audio(&env.lib, &bytes, "wav");
    let s = env.open_with(id, vp(&[], false, false)).session.clone();
    let idle = vp(&[(0, C)], false, false);
    let out = env.step(&s, vec![add(0, "audio.gain", json!({ "db": -6.0 }))], &idle);
    let sid = shown_audio(&out.view).samples_id.clone();
    // 波形の段を先に済ませる（区切りは波形の段が先に使う）
    let work = vp(&[(0, C)], true, false);
    for _ in 0..400 {
        if !shown_audio(&env.step(&s, Vec::new(), &work).view)
            .peaks
            .pending
        {
            break;
        }
    }
    assert!(matches!(
        measure(&env.env(), &s).unwrap(),
        LoudnessState::Measuring { .. }
    ));
    let editor = env.editor(&s);
    let position = || {
        editor
            .lock_state()
            .as_ref()
            .unwrap()
            .audio
            .as_ref()
            .unwrap()
            .measure
            .run_position()
    };
    let total = frames.div_ceil(C) as usize;
    assert_eq!(position(), Some((sid.clone(), 0, total)));
    // 窓の途中で止まる
    const { assert!(MEASURE_WINDOW_CHUNKS > 3) };
    script(&env, &s, 3);
    let out = env.step(&s, Vec::new(), &work);
    assert!(matches!(
        shown_audio(&out.view).loudness,
        LoudnessState::Measuring { .. }
    ));
    assert_eq!(
        position(),
        Some((sid.clone(), 3, total)),
        "窓の途中で止まる"
    );
    // 渡した数だけ進み、続きから流す
    let mut r = kura_engine::determinism::rng(23);
    let mut at = 3usize;
    let state = loop {
        let n = 1 + u64::from(r.next_u32() % 9);
        script(&env, &s, n);
        let out = env.step(&s, Vec::new(), &work);
        let info = shown_audio(&out.view);
        match position() {
            Some((_, pos, _)) => {
                assert_eq!(pos, at + n as usize, "渡した数だけ進む");
                assert!(pos < total);
                at = pos;
            }
            None => {
                assert!(at + n as usize >= total);
                break info.loudness.clone();
            }
        }
    };
    let LoudnessState::Ready { samples_id, values } = &state else {
        panic!("{state:?}")
    };
    assert_eq!(*samples_id, sid);
    let mut g = Graph::new_single();
    g.push_linear("audio.gain", 1, json!({ "db": -6.0 }), 1)
        .unwrap();
    same_bits(values, &whole_loudness(&bytes, &g));
}
