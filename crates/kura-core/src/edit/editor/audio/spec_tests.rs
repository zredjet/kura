//! スペクトルのタイル（A28〜。M17 段 4。docs/editing.md §18.4）：表示の頼みの `spec` で、出力のポートの区間のタイルを作って
//! 公開し、配信がそれを返す。値は全体の描画に `kura-spec/1` を当てたものと同じ。

use kura_engine::Graph;
use kura_engine::audio::spec::{SpecChannel, SpecInput, SpecParams};
use serde_json::json;

use super::session_tests::*;
use super::spec::{SpecRequest, SpecSide, chan_token, serve_spec, tile_bytes};
use super::spec_average::{
    SpecAveragePhase, SpecAverageRequest, SpecAverageState, SpecAverageValues, spec_average,
};
use crate::edit::editor::{AudioViewport, Viewport};

/// 区間 `spans` を描き、出力のスペクトル（チャンネル `chan`・出力の区間 `spec`）を頼む要求。
fn vp(
    spans: &[(u64, u64)],
    spec: &[(u64, u64)],
    chan: SpecChannel,
    work: bool,
    interacting: bool,
) -> Viewport {
    Viewport::Audio(AudioViewport {
        spans: spans.to_vec(),
        work,
        interacting,
        spec: vec![SpecRequest {
            node: None,
            side: SpecSide::Output,
            channel: chan,
            spans: spec.to_vec(),
        }],
        ..AudioViewport::idle()
    })
}

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

/// 全体の描画にかけた `kura-spec/1` のチャンク `cs` のタイル。
fn want_tiles(bytes: &[u8], graph: &Graph, chan: SpecChannel, cs: &[u64]) -> Vec<Vec<u8>> {
    let audio = rendered(bytes, "wav", graph);
    let planes: Vec<Vec<f32>> = (0..audio.channels())
        .map(|ch| {
            let mut v = vec![0f32; audio.frames()];
            audio.read(ch, 0, &mut v).unwrap();
            v
        })
        .collect();
    let p = SpecParams::for_rate(audio.rate()).unwrap();
    let input = SpecInput {
        start: 0,
        len: audio.frames() as u64,
        planes: &planes,
    };
    cs.iter()
        .map(|&c| {
            let chunks = p
                .chunks(&input, chan, c, c + 1, &kura_engine::NoMonitor)
                .unwrap();
            tile_bytes(&p, chan, &chunks[0])
        })
        .collect()
}

fn served(env: &Env, s: &str, key: &str, chan: SpecChannel, c: u64) -> Option<Vec<u8>> {
    serve_spec(&env.engine, s, key, &chan_token(chan), c)
        .ok()
        .map(|b| b.as_ref().clone())
}

/// A28：長い音声の表示の頼みに出力のスペクトルを添えると、頼んだ区間のチャンクのタイルを作って公開する（手・表示の頼み
/// では 2 チャンクまで、残りは `pending` で裏の仕事の区切りが作る）。値は全体の描画にかけたものと同じ。置き場にない
/// チャンクは 404。
#[test]
fn output_tiles_match_the_whole_render() {
    let env = Env::new();
    let frames = 150 * C + 77;
    let bytes = noise_wav(8_000, 2, frames, 51);
    let id = add_audio(&env.lib, &bytes, "wav");
    let view = [(10 * C, 11 * C)];
    let s = env
        .open_with(id, vp(&view, &[], SpecChannel::Sum, false, false))
        .session
        .clone();
    let gain = json!({ "db": -3.0 });
    env.step(
        &s,
        vec![add(0, "audio.gain", gain.clone())],
        &vp(&view, &[], SpecChannel::Sum, false, false),
    );
    let mut g = Graph::new_single();
    g.push_linear("audio.gain", 1, gain, 1).unwrap();
    // 4 チャンクを頼む：この頼みでは 2 チャンクまで
    let want_spans = [(10 * C + 5, 14 * C - 1)];
    let out = env.step(
        &s,
        Vec::new(),
        &vp(&view, &want_spans, SpecChannel::Sum, false, false),
    );
    let info = shown_audio(&out.view);
    assert_eq!(info.spec.len(), 1);
    let port = &info.spec[0];
    assert_eq!(port.ready, vec![(10, 12)]);
    assert!(port.pending);
    assert!(info.work, "残りは裏の仕事で");
    assert_eq!(
        port.bands as usize,
        SpecParams::for_rate(8_000).unwrap().bands().len()
    );
    // 裏の仕事で残りを作る
    script(&env, &s, 10);
    let out = env.step(
        &s,
        Vec::new(),
        &vp(&view, &want_spans, SpecChannel::Sum, true, false),
    );
    let port = &shown_audio(&out.view).spec[0];
    assert_eq!(port.ready, vec![(10, 14)]);
    assert!(!port.pending);
    let want = want_tiles(&bytes, &g, SpecChannel::Sum, &[10, 11, 12, 13]);
    for (c, want) in (10..14).zip(want) {
        assert_eq!(
            served(&env, &s, &port.key, SpecChannel::Sum, c).expect("配る"),
            want,
            "チャンク {c}"
        );
    }
    assert!(
        served(&env, &s, &port.key, SpecChannel::Sum, 40).is_none(),
        "作っていないチャンクは 404"
    );
    assert!(
        served(&env, &s, &port.key, SpecChannel::Index(0), 10).is_none(),
        "ほかのチャンネルは 404"
    );
    // 最後のチャンク（フレームが少ない）
    let tail = [(frames - 10, frames)];
    let out = env.step(
        &s,
        Vec::new(),
        &vp(&view, &tail, SpecChannel::Index(1), false, false),
    );
    let port = &shown_audio(&out.view).spec[0];
    assert_eq!(port.ready, vec![(150, 151)]);
    assert_eq!(
        served(&env, &s, &port.key, SpecChannel::Index(1), 150).unwrap(),
        want_tiles(&bytes, &g, SpecChannel::Index(1), &[150])[0]
    );
}

/// A29：操作中（ドラッグの途中）は、前の頼みから鍵が変わったポート（手ごとに変わる後）を作らない。鍵が変わらなければ作る。
/// 選べないチャンネル（モノラルの M）は応えに出さない。
#[test]
fn interacting_does_not_build_changed_ports() {
    let env = Env::new();
    let frames = 140 * C;
    let id = add_audio(&env.lib, &noise_wav(8_000, 1, frames, 52), "wav");
    let view = [(0, C)];
    let s = env
        .open_with(id, vp(&view, &[], SpecChannel::Sum, false, false))
        .session
        .clone();
    // 操作中の手で鍵が変わる：作らない
    let out = env.step(
        &s,
        vec![add(0, "audio.gain", json!({ "db": -1.0 }))],
        &vp(&view, &[(0, C)], SpecChannel::Sum, false, true),
    );
    let port = &shown_audio(&out.view).spec[0];
    assert!(port.ready.is_empty());
    assert!(port.pending);
    // 操作中でも鍵が前の頼みと同じなら作る
    let out = env.step(
        &s,
        Vec::new(),
        &vp(&view, &[(0, C)], SpecChannel::Sum, false, true),
    );
    let port = &shown_audio(&out.view).spec[0];
    assert_eq!(port.ready, vec![(0, 1)]);
    assert!(!port.pending);
    let out = env.step(
        &s,
        Vec::new(),
        &vp(&view, &[(0, C)], SpecChannel::Mid, false, false),
    );
    assert!(shown_audio(&out.view).spec.is_empty());
}

/// A30：部分の作り直し：出力の後ろだけを変える手（尾のフェードの長さ）では、変わった区間 ±(4N − hop) に掛からないチャンクの
/// タイルを前の鍵から写す（作り直さずに同じ値。操作中でも出す）。全体を変える手（gain）では写さない。
#[test]
fn unchanged_chunks_are_copied_to_the_new_key() {
    let env = Env::new();
    let frames = 140 * C;
    let bytes = noise_wav(8_000, 1, frames, 53);
    let id = add_audio(&env.lib, &bytes, "wav");
    let view = [(0, C)];
    let s = env
        .open_with(id, vp(&view, &[], SpecChannel::Sum, false, false))
        .session
        .clone();
    let fade = |out: u64| json!({ "fadeIn": 0, "fadeOut": out, "curve": "linear" });
    env.step(
        &s,
        vec![
            add(0, "audio.gain", json!({ "db": -1.0 })),
            add(1, "audio.fade", fade(C)),
        ],
        &vp(&view, &[], SpecChannel::Sum, false, false),
    );
    // 20〜21 と、尾の手前の 137（フェードを 2C にすると変わる 138C の手前の 4N − hop に掛かる）
    let spans = [(20 * C, 22 * C), (137 * C, 138 * C)];
    let mut before = None;
    for _ in 0..4 {
        let out = env.step(
            &s,
            Vec::new(),
            &vp(&view, &spans, SpecChannel::Sum, false, false),
        );
        let p = shown_audio(&out.view).spec[0].clone();
        if !p.pending {
            before = Some(p);
            break;
        }
    }
    let before = before.expect("作り終える");
    assert_eq!(before.ready, vec![(20, 22), (137, 138)]);
    // 尾のフェードを 2 チャンクに：鍵は変わるが、20〜21 のチャンクは写す
    let out = env.step(
        &s,
        vec![set(2, fade(2 * C))],
        &vp(&view, &spans, SpecChannel::Sum, false, true),
    );
    let after = shown_audio(&out.view).spec[0].clone();
    assert_ne!(after.key, before.key);
    assert_eq!(
        after.ready,
        vec![(20, 22)],
        "操作中でも写したものは出す。変わった区間 ±(4N − hop) に掛かる 137 は写さない"
    );
    assert!(after.pending);
    // 写した値は、新しい文書を全体で描いたものと同じ（変わった区間 ±(4N − hop) の外なので）
    let mut g = Graph::new_single();
    g.push_linear("audio.gain", 1, json!({ "db": -1.0 }), 1)
        .unwrap();
    g.push_linear("audio.fade", 1, fade(2 * C), 1).unwrap();
    let want = want_tiles(&bytes, &g, SpecChannel::Sum, &[20, 21]);
    for c in 20..22 {
        let tile = served(&env, &s, &after.key, SpecChannel::Sum, c);
        assert_eq!(tile, served(&env, &s, &before.key, SpecChannel::Sum, c));
        assert_eq!(tile.unwrap(), want[(c - 20) as usize]);
    }
    // 鍵が変わらない次の頼みでは 137 を作り直し、新しい文書の値になる
    let out = env.step(
        &s,
        Vec::new(),
        &vp(&view, &spans, SpecChannel::Sum, false, false),
    );
    assert_eq!(
        shown_audio(&out.view).spec[0].ready,
        vec![(20, 22), (137, 138)]
    );
    let want = want_tiles(&bytes, &g, SpecChannel::Sum, &[137]);
    assert_eq!(
        served(&env, &s, &after.key, SpecChannel::Sum, 137).unwrap(),
        want[0]
    );
    // 全体を変える手：写さない（操作中なので作りもしない）
    let out = env.step(
        &s,
        vec![set(1, json!({ "db": -6.0 }))],
        &vp(&view, &spans, SpecChannel::Sum, false, true),
    );
    let changed = shown_audio(&out.view).spec[0].clone();
    assert!(changed.ready.is_empty());
    assert!(changed.pending);
}

/// タイルを作る速さの見積もりを決める（平均をその場で求めるかを決める）。
fn pace(env: &Env, s: &str, per_chunk: std::time::Duration) {
    let editor = env.editor(s);
    let mut guard = editor.lock_state();
    let a = guard.as_mut().unwrap().audio.as_mut().unwrap();
    a.spec.pace = super::slice::Pace::default();
    a.spec.pace.note(1, per_chunk);
}

fn avg_req(channel: SpecChannel, start: u64, end: u64) -> SpecAverageRequest {
    SpecAverageRequest {
        node: None,
        side: SpecSide::Output,
        channel,
        start,
        end,
    }
}

fn avg(env: &Env, s: &str, req: &SpecAverageRequest) -> SpecAverageState {
    spec_average(&env.env(), s, req).unwrap()
}

/// 全体の描画から求めた区間の平均（中心が区間に入るフレームの P を、チャンクの中はフレームの順に、チャンクの順に足す）。
fn want_average(
    bytes: &[u8],
    graph: &Graph,
    chan: SpecChannel,
    start: u64,
    end: u64,
) -> SpecAverageValues {
    let audio = rendered(bytes, "wav", graph);
    let planes: Vec<Vec<f32>> = (0..audio.channels())
        .map(|ch| {
            let mut v = vec![0f32; audio.frames()];
            audio.read(ch, 0, &mut v).unwrap();
            v
        })
        .collect();
    let p = SpecParams::for_rate(audio.rate()).unwrap();
    let len = audio.frames() as u64;
    let (hop, total, per) = (p.hop() as u64, p.frames(len), p.frames_per_chunk());
    let (j0, j1) = (start.div_ceil(hop).min(total), end.div_ceil(hop).min(total));
    if start >= end || j0 >= j1 {
        return SpecAverageValues {
            frames: 0,
            mean: Vec::new(),
            max: Vec::new(),
        };
    }
    let input = SpecInput {
        start: 0,
        len,
        planes: &planes,
    };
    let rows = p
        .frame_bands(&input, chan, j0, j1, &kura_engine::NoMonitor)
        .unwrap();
    let bands = p.bands().len();
    // チャンクの中はフレームの順、まとまり（16 チャンク）の中はチャンクの順、全体はまとまりの順
    let group = super::spec_average::SUMMARY_GROUP;
    let (mut sum, mut max) = (vec![0.0f64; bands], vec![0.0f64; bands]);
    let mut j = j0;
    while j < j1 {
        let ge = ((j / per / group + 1) * group * per).min(j1);
        let mut gpart = vec![0.0f64; bands];
        while j < ge {
            let e = ((j / per + 1) * per).min(ge);
            let mut part = vec![0.0f64; bands];
            for row in &rows[(j - j0) as usize..(e - j0) as usize] {
                for k in 0..bands {
                    part[k] += row[k];
                    max[k] = kura_engine::math::max(max[k], row[k]);
                }
            }
            for k in 0..bands {
                gpart[k] += part[k];
            }
            j = e;
        }
        for k in 0..bands {
            sum[k] += gpart[k];
        }
    }
    let db = |x: f64| -> f32 {
        if x <= 0.0 {
            return -120.0;
        }
        let d = 10.0 * kura_engine::math::log10(x);
        if d < -120.0 { -120.0 } else { d as f32 }
    };
    let n = (j1 - j0) as f64;
    SpecAverageValues {
        frames: j1 - j0,
        mean: sum.iter().map(|x| db(*x / n)).collect(),
        max: max.iter().map(|x| db(*x)).collect(),
    }
}

/// A31：区間の平均（求めるチャンクが少ない）はその場で返し、値は全体の描画から求めたものと同じ。すっぽり入るチャンクは要約を
/// 覚え（タイルも置く）、選び直すと端のチャンクだけを求めて同じ式の値。空の区間はフレーム 0。表示の情報に最後の頼みの状態。
#[test]
fn averages_match_the_whole_render() {
    let env = Env::new();
    let frames = 140 * C;
    let bytes = noise_wav(8_000, 2, frames, 61);
    let id = add_audio(&env.lib, &bytes, "wav");
    let view = [(0, C)];
    let s = env.open_with(id, spans(&view)).session.clone();
    let gain = json!({ "db": -2.0 });
    env.step(&s, vec![add(0, "audio.gain", gain.clone())], &spans(&view));
    let mut g = Graph::new_single();
    g.push_linear("audio.gain", 1, gain, 1).unwrap();
    pace(&env, &s, std::time::Duration::ZERO);
    let (a, b) = (10 * C + 5_000, 12 * C + 300);
    let r = avg(&env, &s, &avg_req(SpecChannel::Side, a, b));
    let SpecAverageState::Ready(v) = r else {
        panic!("その場で求める: {r:?}");
    };
    assert_eq!(v, want_average(&bytes, &g, SpecChannel::Side, a, b));
    assert_eq!(
        v.mean.len(),
        SpecParams::for_rate(8_000).unwrap().bands().len()
    );
    let out = env.step(&s, Vec::new(), &spans(&view));
    let st = &shown_audio(&out.view).spec_average;
    assert_eq!(st.len(), 1);
    assert_eq!(st[0].phase, SpecAveragePhase::Ready);
    assert_eq!(st[0].request, avg_req(SpecChannel::Side, a, b));
    // 平均の道はタイルを置かない（見えている所のタイルを追い出さない。要約だけを覚える）
    assert!(served(&env, &s, &st[0].key, SpecChannel::Side, 11).is_none());
    // 選び直す：端だけ（すっぽり入る 11 は要約から）
    pace(&env, &s, std::time::Duration::ZERO);
    let (a2, b2) = (10 * C + 6_000, 12 * C + 1);
    let r = avg(&env, &s, &avg_req(SpecChannel::Side, a2, b2));
    let SpecAverageState::Ready(v2) = r else {
        panic!("その場で求める: {r:?}");
    };
    assert_eq!(v2, want_average(&bytes, &g, SpecChannel::Side, a2, b2));
    // 空の区間
    let SpecAverageState::Ready(e) = avg(&env, &s, &avg_req(SpecChannel::Sum, 7, 7)) else {
        panic!("空");
    };
    assert_eq!(e.frames, 0);
    // 選べないチャンネル・連鎖にないノード・文書の出力の入力側は断る
    assert!(spec_average(&env.env(), &s, &avg_req(SpecChannel::Index(2), 0, 9)).is_err());
    let mut node = avg_req(SpecChannel::Sum, 0, 9);
    node.node = Some(kura_engine::doc::NodeId(9));
    assert!(spec_average(&env.env(), &s, &node).is_err());
    let mut input = avg_req(SpecChannel::Sum, 0, 9);
    input.side = SpecSide::Input;
    assert!(spec_average(&env.env(), &s, &input).is_err());
}

/// A32：求めるチャンクが多い区間は予約して `measuring` を返し、裏の仕事の区切りで進めて（表示の情報の `work`）、終えると
/// 表示の情報の状態が `ready` になり、頼み直すと値が返る（全体の描画から求めたものと同じ）。出力が変わると予約を捨てる。
#[test]
fn long_averages_run_in_the_background() {
    let env = Env::new();
    let frames = 140 * C + 999;
    let bytes = noise_wav(8_000, 1, frames, 62);
    let id = add_audio(&env.lib, &bytes, "wav");
    let view = [(0, C)];
    let work = |on: bool| {
        Viewport::Audio(AudioViewport {
            spans: view.to_vec(),
            work: on,
            ..AudioViewport::idle()
        })
    };
    let s = env.open_with(id, work(false)).session.clone();
    pace(&env, &s, std::time::Duration::from_secs(3600));
    let (a, b) = (C / 2, frames);
    let req = avg_req(SpecChannel::Index(0), a, b);
    assert_eq!(
        avg(&env, &s, &req),
        SpecAverageState::Measuring { progress: 0.0 }
    );
    let out = env.step(&s, Vec::new(), &work(false));
    let info = shown_audio(&out.view);
    assert!(info.work);
    assert_eq!(
        info.spec_average[0].phase,
        SpecAveragePhase::Measuring { progress: 0.0 }
    );
    let mut rounds = 0;
    loop {
        script(&env, &s, 40);
        let out = env.step(&s, Vec::new(), &work(true));
        let info = shown_audio(&out.view);
        match &info.spec_average[0].phase {
            SpecAveragePhase::Ready => break,
            SpecAveragePhase::Measuring { progress } => assert!(*progress > 0.0 && *progress < 1.0),
            other => panic!("{other:?}"),
        }
        rounds += 1;
        assert!(rounds < 10, "進まない");
    }
    assert!(rounds >= 2, "区切りごとに進む（{rounds}）");
    let SpecAverageState::Ready(v) = avg(&env, &s, &req) else {
        panic!("覚えた値");
    };
    assert_eq!(
        v,
        want_average(&bytes, &Graph::new_single(), SpecChannel::Index(0), a, b)
    );
    // 要約があれば端だけなのでその場で（同じ区間の覚えた値と同じ式）
    pace(&env, &s, std::time::Duration::from_secs(3600));
    let inner = avg_req(SpecChannel::Index(0), 3 * C, 90 * C + 7);
    let SpecAverageState::Ready(v) = avg(&env, &s, &inner) else {
        panic!("要約から");
    };
    assert_eq!(
        v,
        want_average(
            &bytes,
            &Graph::new_single(),
            SpecChannel::Index(0),
            3 * C,
            90 * C + 7
        )
    );
    // 予約したあとで出力が変わる：裏の仕事の区切りで捨てる
    let req2 = avg_req(SpecChannel::Sum, 3 * C, 90 * C);
    pace(&env, &s, std::time::Duration::from_secs(3600));
    assert!(matches!(
        avg(&env, &s, &req2),
        SpecAverageState::Measuring { .. }
    ));
    env.step(
        &s,
        vec![add(0, "audio.gain", json!({ "db": -2.0 }))],
        &work(false),
    );
    let out = env.step(&s, Vec::new(), &work(true));
    let info = shown_audio(&out.view);
    assert_eq!(info.spec_average[0].phase, SpecAveragePhase::Idle);
}

/// ノードのポートの頼み（区間は出力のフレーム）。
fn port_req(node: u32, side: SpecSide, chan: SpecChannel, spans: &[(u64, u64)]) -> SpecRequest {
    SpecRequest {
        node: Some(kura_engine::doc::NodeId(node)),
        side,
        channel: chan,
        spans: spans.to_vec(),
    }
}

fn vp_ports(
    view: &[(u64, u64)],
    reqs: Vec<SpecRequest>,
    work: bool,
    interacting: bool,
) -> Viewport {
    Viewport::Audio(AudioViewport {
        spans: view.to_vec(),
        work,
        interacting,
        spec: reqs,
        ..AudioViewport::idle()
    })
}

/// A33：ノードの入力・出力（前と後）のタイル：区間は出力のフレームで頼み、下流の時間の写し（trim）でポートのフレームに写す
/// （応えに写しの区分）。値はそのノードまでの文書を全体で描いたものと同じ。操作中は前（入力）だけを作り、後（出力）は作らない。
/// 連鎖にないノードは応えに出さない。
#[test]
fn node_ports_follow_the_time_map() {
    let env = Env::new();
    let frames = 140 * C;
    let bytes = noise_wav(8_000, 1, frames, 71);
    let id = add_audio(&env.lib, &bytes, "wav");
    let view = [(0, C)];
    let s = env.open_with(id, spans(&view)).session.clone();
    let shift = 3 * C + 100;
    let gain = json!({ "db": -2.0 });
    env.step(
        &s,
        vec![
            add(0, "audio.gain", gain.clone()),
            add(1, "audio.trim", json!({ "start": shift, "end": null })),
            add(2, "audio.gain", json!({ "db": 1.0 })),
        ],
        &spans(&view),
    );
    let want_out = [(10 * C, 12 * C)];
    let reqs = vec![
        port_req(1, SpecSide::Input, SpecChannel::Index(0), &want_out),
        port_req(1, SpecSide::Output, SpecChannel::Index(0), &want_out),
    ];
    let mut info = None;
    for _ in 0..4 {
        script(&env, &s, 20);
        let out = env.step(&s, Vec::new(), &vp_ports(&view, reqs.clone(), true, false));
        let i = shown_audio(&out.view).clone();
        if i.spec.iter().all(|p| !p.pending) {
            info = Some(i);
            break;
        }
    }
    let info = info.expect("作り終える");
    assert_eq!(info.spec.len(), 2);
    let (before, after) = (&info.spec[0], &info.spec[1]);
    assert_eq!(before.frames, frames);
    assert_eq!(before.map[0].in_start, shift);
    // 出力 10C..12C はポートの 13C+100..15C+100：チャンク 13〜15
    assert_eq!(before.ready, vec![(13, 16)]);
    assert_eq!(after.ready, vec![(13, 16)]);
    assert_ne!(before.key, after.key);
    let original = want_tiles(
        &bytes,
        &Graph::new_single(),
        SpecChannel::Index(0),
        &[13, 14, 15],
    );
    let mut g = Graph::new_single();
    g.push_linear("audio.gain", 1, gain, 1).unwrap();
    let gained = want_tiles(&bytes, &g, SpecChannel::Index(0), &[13, 14, 15]);
    for (i, c) in (13..16).enumerate() {
        assert_eq!(
            served(&env, &s, &before.key, SpecChannel::Index(0), c).unwrap(),
            original[i]
        );
        assert_eq!(
            served(&env, &s, &after.key, SpecChannel::Index(0), c).unwrap(),
            gained[i]
        );
    }
    // 操作中に gain を動かす手：鍵の変わらない前（入力）は作り、鍵の変わった後（出力）は作らない
    let far = [(50 * C, 50 * C + 10)];
    let reqs = vec![
        port_req(1, SpecSide::Input, SpecChannel::Index(0), &far),
        port_req(1, SpecSide::Output, SpecChannel::Index(0), &far),
    ];
    let out = env.step(
        &s,
        vec![set(1, json!({ "db": -5.0 }))],
        &vp_ports(&view, reqs, false, true),
    );
    let info = shown_audio(&out.view);
    assert_eq!(info.spec[0].ready, vec![(53, 54)]);
    assert!(info.spec[1].ready.is_empty() && info.spec[1].pending);
    // 連鎖にないノード
    let out = env.step(
        &s,
        Vec::new(),
        &vp_ports(
            &view,
            vec![port_req(9, SpecSide::Input, SpecChannel::Index(0), &far)],
            false,
            false,
        ),
    );
    assert!(shown_audio(&out.view).spec.is_empty());
}

/// A34：ノードの入力の区間の平均：出力の区間をポートに写して求め、値はそのノードまでの文書の全体の描画から求めたものと同じ。
#[test]
fn node_port_averages_follow_the_time_map() {
    let env = Env::new();
    let frames = 140 * C;
    let bytes = noise_wav(8_000, 2, frames, 72);
    let id = add_audio(&env.lib, &bytes, "wav");
    let view = [(0, C)];
    let s = env.open_with(id, spans(&view)).session.clone();
    let shift = 2 * C + 7;
    env.step(
        &s,
        vec![
            add(0, "audio.trim", json!({ "start": shift, "end": null })),
            add(1, "audio.gain", json!({ "db": -4.0 })),
        ],
        &spans(&view),
    );
    pace(&env, &s, std::time::Duration::ZERO);
    let mut req = avg_req(SpecChannel::Mid, 5 * C + 3, 6 * C + 900);
    req.node = Some(kura_engine::doc::NodeId(2));
    req.side = SpecSide::Input;
    let r = avg(&env, &s, &req);
    let SpecAverageState::Ready(v) = r else {
        panic!("その場で求める: {r:?}");
    };
    // gain の入力は trim の出力（元を shift だけずらしたもの）：出力の区間はそのまま
    let mut g = Graph::new_single();
    g.push_linear("audio.trim", 1, json!({ "start": shift, "end": null }), 1)
        .unwrap();
    assert_eq!(
        v,
        want_average(&bytes, &g, SpecChannel::Mid, 5 * C + 3, 6 * C + 900)
    );
    // trim の入力（元）：元のフレームに写す
    pace(&env, &s, std::time::Duration::ZERO);
    req.node = Some(kura_engine::doc::NodeId(1));
    let r = avg(&env, &s, &req);
    let SpecAverageState::Ready(v) = r else {
        panic!("その場で求める: {r:?}");
    };
    assert_eq!(
        v,
        want_average(
            &bytes,
            &Graph::new_single(),
            SpecChannel::Mid,
            5 * C + 3 + shift,
            6 * C + 900 + shift
        )
    );
}

/// A35：短い音声（全体を描く）は、公開した表示の値から読んで作る（流さない）。値は全体の描画にかけたものと同じ。
#[test]
fn short_audio_reads_the_shown_value() {
    let env = Env::new();
    let frames = 70_000;
    let bytes = noise_wav(8_000, 2, frames, 81);
    let id = add_audio(&env.lib, &bytes, "wav");
    let s = env.open_with(id, spans(&[])).session.clone();
    let gain = json!({ "db": -3.0 });
    env.step(&s, vec![add(0, "audio.gain", gain.clone())], &spans(&[]));
    let out = env.step(
        &s,
        Vec::new(),
        &vp(&[], &[(0, frames)], SpecChannel::Sum, false, false),
    );
    let port = &shown_audio(&out.view).spec[0];
    assert_eq!(port.ready, vec![(0, 2)], "{port:?}");
    let mut g = Graph::new_single();
    g.push_linear("audio.gain", 1, gain, 1).unwrap();
    let want = want_tiles(&bytes, &g, SpecChannel::Sum, &[0, 1]);
    for c in 0..2 {
        assert_eq!(
            served(&env, &s, &port.key, SpecChannel::Sum, c).unwrap(),
            want[c as usize]
        );
    }
}

/// A36：1 つのポートの 1 回の頼みは、置き場に収まる数（`max_chunks`）まで頼みの順で切る（作ったタイルが同じ頼みのほかの
/// タイルに追い出されない）。`pending` は置き場と照らして決める。
#[test]
fn requests_are_capped_to_the_store() {
    let env = Env::new();
    let frames = 140 * C;
    let id = add_audio(&env.lib, &noise_wav(8_000, 1, frames, 91), "wav");
    let view = [(0, C)];
    let s = env
        .open_with(id, vp(&view, &[], SpecChannel::Sum, false, false))
        .session
        .clone();
    let cap = super::spec::max_chunks(&SpecParams::for_rate(8_000).unwrap());
    assert!((10..60).contains(&cap), "{cap}");
    let want = [(5 * C, (5 + cap + 20) * C)];
    let mut port = None;
    for _ in 0..40 {
        script(&env, &s, 50);
        let out = env.step(
            &s,
            Vec::new(),
            &vp(&view, &want, SpecChannel::Sum, true, false),
        );
        let p = shown_audio(&out.view).spec[0].clone();
        if !p.pending {
            port = Some(p);
            break;
        }
    }
    let port = port.expect("作り終える");
    assert_eq!(port.max_chunks, cap);
    assert_eq!(port.ready, vec![(5, 5 + cap)]);
    // 置き場から追い出されていない
    for c in 5..5 + cap {
        assert!(
            served(&env, &s, &port.key, SpecChannel::Sum, c).is_some(),
            "{c}"
        );
    }
}

/// A37：下流に resample があるノードのポート：出力のフレームをレートの比でポートのフレームに写し（区分の in_rate・out_rate）、
/// 値はそのノードまでの文書を全体で描いたものと同じ。
#[test]
fn node_ports_follow_a_resample() {
    let env = Env::new();
    let frames = 70 * C;
    let bytes = noise_wav(8_000, 1, frames, 92);
    let id = add_audio(&env.lib, &bytes, "wav");
    let view = [(0, C)];
    let s = env.open_with(id, spans(&view)).session.clone();
    let gain = json!({ "db": -2.0 });
    env.step(
        &s,
        vec![
            add(0, "audio.gain", gain.clone()),
            add(1, "audio.resample", json!({ "rate": 16_000 })),
        ],
        &spans(&view),
    );
    // 出力（16 kHz）の 20C..22C は gain の出力（8 kHz）の 10C..11C
    let reqs = vec![port_req(
        1,
        SpecSide::Output,
        SpecChannel::Index(0),
        &[(20 * C, 22 * C)],
    )];
    let out = env.step(&s, Vec::new(), &vp_ports(&view, reqs, false, false));
    let port = shown_audio(&out.view).spec[0].clone();
    assert_eq!((port.rate, port.frames), (8_000, frames));
    assert_eq!((port.map[0].in_rate, port.map[0].out_rate), (8_000, 16_000));
    assert_eq!(port.ready, vec![(10, 11)]);
    let mut g = Graph::new_single();
    g.push_linear("audio.gain", 1, gain, 1).unwrap();
    let want = want_tiles(&bytes, &g, SpecChannel::Index(0), &[10]);
    assert_eq!(
        served(&env, &s, &port.key, SpecChannel::Index(0), 10).unwrap(),
        want[0]
    );
}
