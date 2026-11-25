//! 試聴だけの重ね（M17 段 6。docs/editing.md §18.7）：一時的に外して聞く音（試聴の計画）。文書・rev・取り消しの列を変えず、
//! 外した音はそのノードを外した文書の全体の描画と同じ。外したノードを動かしても `auditionId` は変わらない。長い音声で重い
//! 流し解きが要るときは、表示より先に解き、それまで外していない音（`auditionId` は `samplesId`）。

use kura_engine::Graph;
use kura_engine::doc::NodeId;
use kura_engine::render::region::Area;
use serde_json::json;

use super::audition::{AuditionPhase, AuditionRequest};
use super::serve::{EDIT_DECODER_PREFIX, serve_audition_pcm};
use super::serve_tests::parse_wav;
use super::session_tests::*;
use crate::asset::pcm::PcmError;
use crate::edit::editor::{AudioViewInfo, AudioViewport, CommitMode, Edit, Step, Viewport};

/// 区間 `spans` を描き、ノード `node` を外した音も同じ区間で頼む要求。
fn bypass(node: u32, spans: &[(u64, u64)], work: bool) -> Viewport {
    Viewport::Audio(AudioViewport {
        spans: spans.to_vec(),
        work,
        audition: Some(AuditionRequest {
            bypass: NodeId(node),
            spans: spans.to_vec(),
        }),
        ..AudioViewport::idle()
    })
}

/// ベル 1 つの EQ の引数（8 kHz の音声）。
fn eq(gain: f64) -> serde_json::Value {
    json!({ "bands": [{ "type": "bell", "freq": 1000.0, "gainDb": gain, "q": 1.2 }], "phase": "minimum", "gainDb": 0.0 })
}

/// gain だけの文書。
fn only_gain(db: f64) -> Graph {
    let mut g = Graph::new_single();
    g.push_linear("audio.gain", 1, json!({ "db": db }), 1)
        .unwrap();
    g
}

/// 公開した試聴の値（`id`）の区間 [a, b) が、全体の描画 `want` と標本ごとに同じ。
fn same_audition(env: &Env, s: &str, id: &str, a: u64, b: u64, want: &kura_engine::value::Audio) {
    let value = env
        .editor(s)
        .published_audition_covering(id, &Area::span(a, b))
        .expect("公開した試聴の値が区間を覆っていない");
    for c in 0..want.channels() {
        let mut x = vec![0.0f32; (b - a) as usize];
        let mut y = vec![0.0f32; (b - a) as usize];
        value.audio_read(c, a, &mut x).unwrap();
        want.read(c, a, &mut y).unwrap();
        assert!(
            x.iter().zip(&y).all(|(p, q)| p.to_bits() == q.to_bits()),
            "試聴 {id} のチャンネル {c} の [{a}, {b}) が全体の描画と違う"
        );
    }
}

fn phase(info: &AudioViewInfo) -> AuditionPhase {
    info.audition.as_ref().expect("試聴の情報がない").phase
}

/// 短い音声：外した音は、そのノードを外した文書の全体の描画と同じで、文書・rev・取り消しの列は変わらない。PCM は
/// `/edit/{s}/audition/{id}/pcm` で取れ、デコーダは `kura-edit/1:{auditionId}`。外したノードの引数を変えても `auditionId` は
/// 変わらず、ほかのノードを変えると変わる。頼みに試聴がなくなれば `auditionId` は `samplesId` に戻り、試聴の値は片付く。
#[test]
fn bypass_plays_the_doc_without_the_node_and_leaves_the_doc_alone() {
    let env = Env::new();
    let frames = 6 * C + 17;
    let bytes = noise_wav(8_000, 2, frames, 61);
    let id = add_audio(&env.lib, &bytes, "wav");
    let s = env.open(id).session.clone();
    let out = env.step(
        &s,
        vec![
            add(0, "audio.gain", json!({ "db": -3.0 })),
            add(1, "audio.eq", eq(6.0)),
        ],
        &spans(&[]),
    );
    assert_eq!(out.created, vec![NodeId(1), NodeId(2)]);
    let before = out.status.clone();
    let shown = shown_audio(&out.view).clone();
    assert_eq!(shown.audition_id, shown.samples_id, "重ねがなければ同じ");
    assert!(shown.audition.is_none());
    // EQ を外して聞く
    let out = env.step(&s, Vec::new(), &bypass(2, &[], false));
    assert_eq!(out.status, before, "文書・rev・取り消しの列は変わらない");
    assert!(out.doc.nodes.is_empty(), "文書の差分はない");
    let info = shown_audio(&out.view).clone();
    assert_eq!(info.samples_id, shown.samples_id, "表示は文書の値のまま");
    assert_ne!(info.audition_id, info.samples_id);
    assert_eq!(phase(&info), AuditionPhase::Ready);
    assert_eq!(info.audition.as_ref().unwrap().valid, vec![(0, frames)]);
    assert_eq!(info.audition.as_ref().unwrap().node, NodeId(2));
    let want = rendered(&bytes, "wav", &only_gain(-3.0));
    same_audition(&env, &s, &info.audition_id, 0, frames, &want);
    // 配信（区間の PCM とデコーダ）・範囲の外は 416・知らない id は 404
    let w =
        parse_wav(&serve_audition_pcm(&env.engine, &s, &info.audition_id, C, 2 * C + 5).unwrap());
    assert_eq!(
        (w.channels, w.rate, w.start, w.total),
        (2, 8_000, C, frames)
    );
    assert_eq!(
        w.decoder,
        format!("{EDIT_DECODER_PREFIX}{}", info.audition_id)
    );
    let mut left = vec![0.0f32; (C + 5) as usize];
    want.read(0, C, &mut left).unwrap();
    for (a, b) in w.planes[0].iter().zip(&left) {
        assert!(
            (a - b).abs() <= 1.0 / 32_768.0 + 1e-6,
            "PCM が外した音と違う"
        );
    }
    assert!(matches!(
        serve_audition_pcm(&env.engine, &s, &info.audition_id, 0, frames + 1),
        Err(PcmError::OutOfRange { .. })
    ));
    assert!(matches!(
        serve_audition_pcm(&env.engine, &s, "0000000000000000", 0, 1),
        Err(PcmError::NotFound)
    ));
    // 外した EQ の引数を変えても外した音は同じ（表示は変わる）
    let out = env.step(&s, vec![set(2, eq(-9.0))], &bypass(2, &[], false));
    let moved = shown_audio(&out.view).clone();
    assert_ne!(moved.samples_id, info.samples_id);
    assert_eq!(moved.audition_id, info.audition_id);
    // ほかのノード（gain）を変えると外した音も変わる
    let out = env.step(
        &s,
        vec![set(1, json!({ "db": -6.0 }))],
        &bypass(2, &[], false),
    );
    let regained = shown_audio(&out.view).clone();
    assert_ne!(regained.audition_id, info.audition_id);
    same_audition(
        &env,
        &s,
        &regained.audition_id,
        0,
        frames,
        &rendered(&bytes, "wav", &only_gain(-6.0)),
    );
    // 試聴をやめる：id は表示と同じ・試聴の値は片付く
    let out = env.step(&s, Vec::new(), &spans(&[]));
    let ended = shown_audio(&out.view).clone();
    assert_eq!(ended.audition_id, ended.samples_id);
    assert!(ended.audition.is_none());
    assert!(matches!(
        serve_audition_pcm(&env.engine, &s, &regained.audition_id, 0, 1),
        Err(PcmError::NotFound)
    ));
}

/// もう外してあるノードは表示と同じ音（Ready・id は `samplesId`）。文書にないノード・外すと出力の形が変わるノード（mono）は
/// 外せない（Unavailable・id は `samplesId`）。
#[test]
fn bypass_falls_back_to_the_shown_sound_when_it_cannot_change_it() {
    let env = Env::new();
    let frames = 4 * C;
    let bytes = noise_wav(8_000, 2, frames, 62);
    let id = add_audio(&env.lib, &bytes, "wav");
    let s = env.open(id).session.clone();
    env.step(
        &s,
        vec![add(0, "audio.eq", eq(3.0)), add(1, "audio.mono", json!({}))],
        &spans(&[]),
    );
    // mono を外すと 2 チャンネルになる：外せない
    let out = env.step(&s, Vec::new(), &bypass(2, &[], false));
    let info = shown_audio(&out.view).clone();
    assert_eq!(phase(&info), AuditionPhase::Unavailable);
    assert_eq!(info.audition_id, info.samples_id);
    // 文書にないノード
    let out = env.step(&s, Vec::new(), &bypass(9, &[], false));
    let info = shown_audio(&out.view).clone();
    assert_eq!(phase(&info), AuditionPhase::Unavailable);
    assert_eq!(info.audition_id, info.samples_id);
    // もう外してある EQ：表示と同じ音
    let off = Step::Commit {
        edit: Edit::SetEnabled {
            node: NodeId(1),
            enabled: false,
        },
        mode: CommitMode::Step,
    };
    let out = env.step(&s, vec![off], &bypass(1, &[], false));
    let info = shown_audio(&out.view).clone();
    assert_eq!(phase(&info), AuditionPhase::Ready);
    assert_eq!(info.audition_id, info.samples_id);
    assert_eq!(info.audition.as_ref().unwrap().valid, info.valid);
}

/// 外して聞いても、出力のラウドネスの結果（`samplesId` で覚える）は「計っています…」に戻らない。
#[test]
fn bypass_keeps_the_loudness_of_the_shown_sound() {
    let env = Env::new();
    let frames = 6 * C;
    let bytes = noise_wav(8_000, 1, frames, 63);
    let id = add_audio(&env.lib, &bytes, "wav");
    let s = env.open(id).session.clone();
    env.step(&s, vec![add(0, "audio.eq", eq(4.0))], &spans(&[]));
    let measured = super::measure::measure(&env.env(), &s).unwrap();
    assert!(matches!(
        measured,
        super::measure::LoudnessState::Ready { .. }
    ));
    let out = env.step(&s, Vec::new(), &bypass(1, &[], false));
    let info = shown_audio(&out.view);
    assert_ne!(info.audition_id, info.samples_id);
    assert_eq!(info.loudness, measured);
}

/// 長い音声（出力が 32 MiB を超える）で、表示も試聴も流し解きが要るとき：裏の仕事の区切りは試聴に先に渡し、試聴が解き終えるまで `auditionId` は
/// `samplesId`（外していない音）。試聴が先に解き終え、そのあと表示も解き終える。外した音は全体の描画と同じ。
#[test]
fn long_audio_solves_the_audition_first() {
    let env = Env::new();
    let frames = 150 * C;
    let bytes = noise_wav(8_000, 1, frames, 64);
    let id = add_audio(&env.lib, &bytes, "wav");
    let view = [(70 * C, 72 * C)];
    let plain = Viewport::Audio(AudioViewport {
        spans: view.to_vec(),
        work: false,
        ..AudioViewport::idle()
    });
    let s = env.open_with(id, plain.clone()).session.clone();
    env.step(
        &s,
        vec![add(0, "audio.gain", json!({ "db": -2.0 }))],
        &plain,
    );
    // EQ を足して解き終える
    env.step(&s, vec![add(1, "audio.eq", eq(4.0))], &plain);
    for _ in 0..200 {
        script(&env, &s, 60);
        let out = env.step(
            &s,
            Vec::new(),
            &Viewport::Audio(AudioViewport {
                work: true,
                ..match plain.clone() {
                    Viewport::Audio(a) => a,
                    Viewport::Image(_) => unreachable!(),
                }
            }),
        );
        if shown_audio(&out.view).solving.is_none() {
            break;
        }
    }
    // 上流の gain を変え（表示は EQ の表を解き直す）、同時に gain を外して聞く（試聴は gain なしの入力で EQ の表を解く）
    let out = env.step(
        &s,
        vec![set(1, json!({ "db": -5.0 }))],
        &bypass(1, &view, false),
    );
    let info = shown_audio(&out.view).clone();
    assert!(info.solving.is_some(), "表示も解いている");
    assert!(matches!(phase(&info), AuditionPhase::Solving(_)));
    assert_eq!(
        info.audition_id, info.samples_id,
        "解き終えるまで外していない音"
    );
    let mut audition_done = None;
    let mut display_done = None;
    for call in 1..=400 {
        script(&env, &s, 40);
        let out = env.step(&s, Vec::new(), &bypass(1, &view, true));
        let info = shown_audio(&out.view).clone();
        if audition_done.is_none() && phase(&info) == AuditionPhase::Ready {
            audition_done = Some((call, info.clone()));
            assert!(info.solving.is_some(), "試聴が先に解き終える");
        }
        if info.solving.is_none() {
            display_done = Some(call);
            break;
        }
    }
    let (call, ready) = audition_done.expect("試聴が解き終わらない");
    let shown_done = display_done.expect("表示が解き終わらない");
    assert!(
        call < shown_done,
        "試聴 {call} 回目・表示 {shown_done} 回目"
    );
    assert_ne!(ready.audition_id, ready.samples_id);
    let mut g = Graph::new_single();
    g.push_linear("audio.eq", 1, eq(4.0), 1).unwrap();
    same_audition(
        &env,
        &s,
        &ready.audition_id,
        70 * C,
        72 * C,
        &rendered(&bytes, "wav", &g),
    );
}

/// 試聴の値は確保の `extra` に数える（外して聞くと増え、やめると戻る。表示の今と直前がそろったあとで比べる）。
#[test]
fn audition_values_count_in_the_lease() {
    let env = Env::new();
    let frames = 6 * C;
    let bytes = noise_wav(8_000, 2, frames, 65);
    let id = add_audio(&env.lib, &bytes, "wav");
    let s = env.open(id).session.clone();
    env.step(
        &s,
        vec![
            add(0, "audio.gain", json!({ "db": -2.0 })),
            add(1, "audio.eq", eq(4.0)),
        ],
        &spans(&[]),
    );
    env.step(&s, Vec::new(), &spans(&[]));
    let editor = env.editor(&s);
    let plain = editor.audio_counts().unwrap().extra;
    // EQ を外すと gain の値（描いた値）を試聴に持つ
    env.step(&s, Vec::new(), &bypass(2, &[], false));
    let with = editor.audio_counts().unwrap().extra;
    assert!(
        with >= plain + frames * 2 * 4,
        "試聴の値を数える：{plain} → {with}"
    );
    env.step(&s, Vec::new(), &spans(&[]));
    let after = editor.audio_counts().unwrap().extra;
    assert!(after < with, "やめると戻る：{plain} → {with} → {after}");
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
