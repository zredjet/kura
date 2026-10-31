//! 時間の写しとハンドル（A11・A12）：`sourceMap`・`nodes[].marks` が出力の標本と合い、`Step::Handle` が下流の写しを逆に
//! たどってノードの引数にする（なければ末尾に足す・外したノードは有効に戻す・写せなければ断って状態を変えない）。

use kura_engine::Graph;
use kura_engine::doc::NodeId;
use serde_json::json;

use super::handles::{Handle, HandleMode, Marks};
use super::session_tests::*;
use super::timeline::SourcePiece;
use crate::edit::editor::{CommitMode, Edit, Step, Viewport};

fn handle(h: Handle, node: Option<u32>, mode: HandleMode) -> Step {
    Step::Handle {
        handle: h,
        node: node.map(NodeId),
        mode,
    }
}

const STEP: HandleMode = HandleMode::Commit(CommitMode::Step);

/// 頭の文書のノードの引数。
fn params(env: &Env, s: &str, node: u32) -> serde_json::Value {
    let editor = env.editor(s);
    let guard = editor.lock_state();
    let st = guard.as_ref().unwrap();
    let g = st.session.head().to_graph();
    g.nodes
        .iter()
        .find(|n| n.id == NodeId(node))
        .map(|n| n.params.clone())
        .unwrap()
}

fn enabled(env: &Env, s: &str, node: u32) -> bool {
    let editor = env.editor(s);
    let guard = editor.lock_state();
    let g = guard.as_ref().unwrap().session.head().to_graph();
    g.nodes
        .iter()
        .find(|n| n.id == NodeId(node))
        .unwrap()
        .enabled
}

/// A11：trim → resample → fade → set_loop の文書で、元への写しと各ノードの印が出力のフレームで合う。
#[test]
fn source_map_and_marks_follow_the_chain() {
    let env = Env::new();
    let frames = 3 * C;
    let bytes = noise_wav(8_000, 1, frames, 41);
    let id = add_audio(&env.lib, &bytes, "wav");
    let opened = env.open(id);
    let s = opened.session.clone();
    let vp = Viewport::audio_idle();
    let out = env.step(
        &s,
        vec![
            add(0, "audio.trim", json!({ "start": 1_000, "end": 50_000 })),
            add(1, "audio.resample", json!({ "rate": 16_000 })),
            add(
                2,
                "audio.fade",
                json!({ "fadeIn": 600, "fadeOut": 400, "curve": "linear" }),
            ),
            add(
                3,
                "audio.set_loop",
                json!({ "loop": { "start": 100, "end": 2_000 } }),
            ),
        ],
        &vp,
    );
    assert!(out.rejected.is_none(), "{:?}", out.rejected);
    let info = shown_audio(&out.view).clone();
    assert_eq!((info.rate, info.frames), (16_000, 98_000));
    assert_eq!(
        info.source_map,
        Some(vec![SourcePiece {
            out_start: 0,
            out_end: 98_000,
            in_start: 1_000,
            in_rate: 8_000,
            out_rate: 16_000
        }])
    );
    let ids: Vec<u32> = info.nodes.iter().map(|n| n.id.0).collect();
    assert_eq!(ids, vec![1, 2, 3, 4]);
    let n = &info.nodes;
    assert_eq!((n[0].in_rate, n[0].in_frames), (8_000, frames));
    assert_eq!((n[1].in_rate, n[1].in_frames), (8_000, 49_000));
    assert_eq!((n[2].in_rate, n[2].in_frames), (16_000, 98_000));
    // trim の枠は自分の写しを当てず、下流の resample で写す（入力を出しているときの位置）
    assert_eq!(
        n[0].marks,
        Some(Marks::Trim {
            start: 2_000,
            end: 100_000
        })
    );
    assert_eq!(n[1].marks, None);
    assert_eq!(
        n[2].marks,
        Some(Marks::Fade {
            in_end: Some(600),
            out_start: Some(98_000 - 400)
        })
    );
    assert_eq!(
        n[3].marks,
        Some(Marks::Loop {
            start: 100,
            end: 2_000
        })
    );
    assert_eq!(info.loop_range, Some((100, 2_000)));

    // trim だけなら、出力の標本は元への写しの位置の元の標本と同じ
    let opened = env.open(id);
    let s = opened.session.clone();
    let out = env.step(
        &s,
        vec![add(0, "audio.trim", json!({ "start": 1_000, "end": null }))],
        &vp,
    );
    let info = shown_audio(&out.view).clone();
    let map = info.source_map.clone().unwrap();
    assert_eq!(map.len(), 1);
    let original = rendered(&bytes, "wav", &Graph::new_single());
    let (value, _) = env.editor(&s).published_audio(info.rev).unwrap();
    for f in [0u64, 17, 5_000, info.frames - 1] {
        let mut got = [0f32; 1];
        value.audio_read(0, f, &mut got).unwrap();
        let mut want = [0f32; 1];
        original.read(0, map[0].in_start + f, &mut want).unwrap();
        assert_eq!(got, want, "{f}");
    }
}

/// A12：ノードがなければ末尾に足し、あれば SetParams。下流に resample があっても、写した引数でその位置に効く。
#[test]
fn handles_map_back_through_the_chain() {
    let env = Env::new();
    let bytes = noise_wav(8_000, 1, 3 * C, 42);
    let id = add_audio(&env.lib, &bytes, "wav");
    let opened = env.open(id);
    let s = opened.session.clone();
    let vp = Viewport::audio_idle();
    // ノードがない：末尾に足す（値はそのまま引数）
    let out = env.step(
        &s,
        vec![handle(Handle::FadeIn { end: 4_000 }, None, STEP)],
        &vp,
    );
    assert!(out.rejected.is_none(), "{:?}", out.rejected);
    assert_eq!(out.created, vec![NodeId(1)]);
    assert_eq!(
        params(&env, &s, 1),
        json!({ "fadeIn": 4_000, "fadeOut": 0, "curve": "linear" })
    );
    // 下流に resample（8 → 16 kHz）：出力の 2,000 は fade の 1,000
    let out = env.step(
        &s,
        vec![add(1, "audio.resample", json!({ "rate": 16_000 }))],
        &vp,
    );
    assert!(out.rejected.is_none());
    let out = env.step(
        &s,
        vec![handle(Handle::FadeIn { end: 2_000 }, Some(1), STEP)],
        &vp,
    );
    assert!(out.rejected.is_none(), "{:?}", out.rejected);
    assert_eq!(params(&env, &s, 1)["fadeIn"], json!(1_000));
    let info = shown_audio(&out.view);
    assert_eq!(
        info.nodes[0].marks,
        Some(Marks::Fade {
            in_end: Some(2_000),
            out_start: None
        })
    );
    // フェードアウトの始まり：fadeOut = 入力の長さ − 戻した位置
    let out = env.step(
        &s,
        vec![handle(
            Handle::FadeOut { start: 6 * C - 100 },
            Some(1),
            STEP,
        )],
        &vp,
    );
    assert!(out.rejected.is_none(), "{:?}", out.rejected);
    assert_eq!(params(&env, &s, 1)["fadeOut"], json!(50));
    // ループ（末尾に足す）と消す
    let out = env.step(
        &s,
        vec![handle(
            Handle::Loop {
                range: Some((10, 900)),
            },
            None,
            STEP,
        )],
        &vp,
    );
    assert!(out.rejected.is_none());
    assert_eq!(shown_audio(&out.view).loop_range, Some((10, 900)));
    let out = env.step(
        &s,
        vec![handle(Handle::Loop { range: None }, Some(3), STEP)],
        &vp,
    );
    assert!(out.rejected.is_none());
    assert_eq!(shown_audio(&out.view).loop_range, None);
}

/// A12：preview → commitPreview で 1 手。外したノードは確定で有効に戻る（1 手）。写せない・操作の違う手は断り、状態を変えない。
#[test]
fn handle_drags_are_one_step_and_refusals_keep_the_state() {
    let env = Env::new();
    let bytes = noise_wav(8_000, 1, 3 * C, 43);
    let id = add_audio(&env.lib, &bytes, "wav");
    let opened = env.open(id);
    let s = opened.session.clone();
    let vp = Viewport::audio_idle();
    env.step(
        &s,
        vec![
            add(
                0,
                "audio.fade",
                json!({ "fadeIn": 10, "fadeOut": 0, "curve": "linear" }),
            ),
            add(1, "audio.trim", json!({ "start": 1_000, "end": 2_000 })),
        ],
        &vp,
    );
    let before = env.step(&s, Vec::new(), &vp).status.head;
    // ドラッグ：preview を続けて、離したら commitPreview（1 手）
    for end in [100, 200, 300] {
        let out = env.step(
            &s,
            vec![handle(Handle::FadeIn { end }, Some(1), HandleMode::Preview)],
            &vp,
        );
        assert!(out.status.provisional);
    }
    let out = env.step(
        &s,
        vec![Step::CommitPreview {
            mode: CommitMode::Step,
        }],
        &vp,
    );
    assert!(!out.status.provisional);
    // fade の下流は trim [1000, 2000)：出力の 300 は fade の 1,300
    assert_eq!(params(&env, &s, 1)["fadeIn"], json!(1_300));
    let out = env.step(&s, vec![Step::Undo], &vp);
    assert_eq!(out.status.head, before, "1 手で戻る");
    assert_eq!(params(&env, &s, 1)["fadeIn"], json!(10));

    // 外したノード：確定で有効に戻り、引数も替わる（1 手）
    env.step(
        &s,
        vec![Step::Commit {
            edit: Edit::SetEnabled {
                node: NodeId(1),
                enabled: false,
            },
            mode: CommitMode::Step,
        }],
        &vp,
    );
    assert!(!enabled(&env, &s, 1));
    let out = env.step(
        &s,
        vec![handle(Handle::FadeIn { end: 50 }, Some(1), STEP)],
        &vp,
    );
    assert!(out.rejected.is_none(), "{:?}", out.rejected);
    assert!(enabled(&env, &s, 1));
    assert_eq!(params(&env, &s, 1)["fadeIn"], json!(1_050));
    env.step(&s, vec![Step::Undo], &vp);
    assert!(!enabled(&env, &s, 1));
    assert_eq!(params(&env, &s, 1)["fadeIn"], json!(10));

    // 写せない（trim の外）・操作が違う：断って状態を変えない
    let head = env.step(&s, Vec::new(), &vp).status;
    for (step, why) in [
        (
            handle(Handle::FadeIn { end: 5_000 }, Some(1), STEP),
            "写せない",
        ),
        (
            handle(
                Handle::Loop {
                    range: Some((0, 5)),
                },
                Some(1),
                STEP,
            ),
            "ではありません",
        ),
        (
            handle(Handle::Trim { start: 9, end: 3 }, Some(2), STEP),
            "前にします",
        ),
        (
            handle(Handle::FadeIn { end: 5 }, Some(9), STEP),
            "ありません",
        ),
    ] {
        let out = env.step(&s, vec![step], &vp);
        let r = out.rejected.expect("断るはず");
        assert!(r.problem.message.contains(why), "{}", r.problem.message);
        assert_eq!(out.status, head, "状態を変えない");
    }
}

/// 切り詰めの道具の流れ：仮に外して入力を出し、印の位置で枠を出して、`[cancelPreview, handle{trim, step}]` で確定する。
#[test]
fn the_trim_tool_flow() {
    let env = Env::new();
    let bytes = noise_wav(8_000, 1, 3 * C, 44);
    let id = add_audio(&env.lib, &bytes, "wav");
    let opened = env.open(id);
    let s = opened.session.clone();
    let vp = Viewport::audio_idle();
    env.step(
        &s,
        vec![add(
            0,
            "audio.trim",
            json!({ "start": 1_000, "end": 2_000 }),
        )],
        &vp,
    );
    let out = env.step(
        &s,
        vec![Step::Preview {
            edit: Edit::SetEnabled {
                node: NodeId(1),
                enabled: false,
            },
        }],
        &vp,
    );
    let info = shown_audio(&out.view);
    assert_eq!(info.frames, 3 * C, "外している間は入力を出す");
    assert_eq!(
        info.nodes[0].marks,
        Some(Marks::Trim {
            start: 1_000,
            end: 2_000
        })
    );
    let out = env.step(
        &s,
        vec![
            Step::CancelPreview,
            handle(
                Handle::Trim {
                    start: 500,
                    end: 3 * C,
                },
                Some(1),
                STEP,
            ),
        ],
        &vp,
    );
    assert!(out.rejected.is_none(), "{:?}", out.rejected);
    assert_eq!(params(&env, &s, 1), json!({ "start": 500, "end": null }));
    assert_eq!(shown_audio(&out.view).frames, 3 * C - 500);
}

/// `rate`・`frames` の元に `steps` を当てて開いた、セッションと、`tool` が Some ならそのノードを仮に外した表示の印。
fn open_with_nodes(
    env: &Env,
    rate: u32,
    frames: u64,
    steps: Vec<Step>,
    tool: Option<u32>,
) -> (String, Vec<Option<Marks>>) {
    // 開くたびに別のアイテム（同じ中身は同じハッシュになるので種を変える）
    static SEED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(100);
    let seed = SEED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let bytes = noise_wav(rate, 1, frames, seed);
    let id = add_audio(&env.lib, &bytes, "wav");
    let s = env.open(id).session.clone();
    let vp = Viewport::audio_idle();
    let out = env.step(&s, steps, &vp);
    assert!(out.rejected.is_none(), "{:?}", out.rejected);
    let out = match tool {
        Some(node) => env.step(&s, vec![preview_disabled(node)], &vp),
        None => out,
    };
    let marks = shown_audio(&out.view)
        .nodes
        .iter()
        .map(|n| n.marks)
        .collect();
    (s, marks)
}

/// 下流でレートを下げる resample があっても、印の終わり（出力の終わり）は入力の終わりに戻り（trim は end: null）、動かさな
/// かった側は今の引数のまま（丸めでずらさない）。2:1・48 kHz → 44.1 kHz、奇数の長さ。
#[test]
fn handle_ends_survive_downsampling() {
    let env = Env::new();
    let vp = Viewport::audio_idle();
    let trim_then = |start: u64, end: Option<u64>, rate: u32| {
        vec![
            add(0, "audio.trim", json!({ "start": start, "end": end })),
            add(1, "audio.resample", json!({ "rate": rate })),
        ]
    };
    let commit = |s: &str, h: Handle| {
        let out = env.step(s, vec![Step::CancelPreview, handle(h, Some(1), STEP)], &vp);
        assert!(out.rejected.is_none(), "{h:?}: {:?}", out.rejected);
    };
    // 2:1・3C+3：始めだけ動かす（印の終わりは出力の終わり 98,306。戻すと 196,612 で入力を超えていた）
    let n = 3 * C + 3;
    let (s, marks) = open_with_nodes(&env, 8_000, n, trim_then(1_000, None, 4_000), Some(1));
    assert_eq!(
        marks[0],
        Some(Marks::Trim {
            start: 500,
            end: 98_306
        })
    );
    commit(
        &s,
        Handle::Trim {
            start: 400,
            end: 98_306,
        },
    );
    assert_eq!(params(&env, &s, 1), json!({ "start": 800, "end": null }));
    // 2:1・3C+1：印の終わりを戻すと 196,608（黙って 1 フレーム縮んでいた）
    let n = 3 * C + 1;
    let (s, marks) = open_with_nodes(&env, 8_000, n, trim_then(1_000, None, 4_000), Some(1));
    assert_eq!(
        marks[0],
        Some(Marks::Trim {
            start: 500,
            end: 98_304
        })
    );
    commit(
        &s,
        Handle::Trim {
            start: 400,
            end: 98_304,
        },
    );
    assert_eq!(params(&env, &s, 1), json!({ "start": 800, "end": null }));
    // 2:1・奇数の始め：終わりを出力の終わりへ動かす（始めの 1,001 は印 500 から戻すと 1,000 になっていた）
    let n = 3 * C + 3;
    let (s, marks) = open_with_nodes(
        &env,
        8_000,
        n,
        trim_then(1_001, Some(150_000), 4_000),
        Some(1),
    );
    assert_eq!(
        marks[0],
        Some(Marks::Trim {
            start: 500,
            end: 75_000
        })
    );
    commit(
        &s,
        Handle::Trim {
            start: 500,
            end: 98_306,
        },
    );
    assert_eq!(params(&env, &s, 1), json!({ "start": 1_001, "end": null }));
    // 終わりだけを途中へ動かす
    env.step(&s, vec![preview_disabled(1)], &vp);
    commit(
        &s,
        Handle::Trim {
            start: 500,
            end: 90_000,
        },
    );
    assert_eq!(
        params(&env, &s, 1),
        json!({ "start": 1_001, "end": 180_000 })
    );
    // 48 kHz → 44.1 kHz・3C+1：印の終わり 180,635 を戻すと 196,610（入力を超えていた）。始め 1,003 は印 922 から戻すと 1,004
    let n = 3 * C + 1;
    let (s, marks) = open_with_nodes(&env, 48_000, n, trim_then(1_003, None, 44_100), Some(1));
    assert_eq!(
        marks[0],
        Some(Marks::Trim {
            start: 922,
            end: 180_635
        })
    );
    commit(
        &s,
        Handle::Trim {
            start: 900,
            end: 180_635,
        },
    );
    assert_eq!(params(&env, &s, 1), json!({ "start": 980, "end": null }));
    env.step(&s, vec![Step::Undo, preview_disabled(1)], &vp);
    commit(
        &s,
        Handle::Trim {
            start: 922,
            end: 170_000,
        },
    );
    assert_eq!(
        params(&env, &s, 1),
        json!({ "start": 1_003, "end": 185_034 })
    );
    // ループの終わりを出力の終わりに置く（2:1・3C+3）
    let n = 3 * C + 3;
    let (s, marks) = open_with_nodes(
        &env,
        8_000,
        n,
        vec![
            add(
                0,
                "audio.set_loop",
                json!({ "loop": { "start": 0, "end": n } }),
            ),
            add(1, "audio.resample", json!({ "rate": 4_000 })),
        ],
        None,
    );
    assert_eq!(
        marks[0],
        Some(Marks::Loop {
            start: 0,
            end: 98_306
        })
    );
    let out = env.step(
        &s,
        vec![handle(
            Handle::Loop {
                range: Some((100, 98_306)),
            },
            Some(1),
            STEP,
        )],
        &vp,
    );
    assert!(out.rejected.is_none(), "{:?}", out.rejected);
    assert_eq!(
        params(&env, &s, 1),
        json!({ "loop": { "start": 200, "end": n } })
    );
}

/// 文書のノードを仮に外す手。
fn preview_disabled(node: u32) -> Step {
    Step::Preview {
        edit: Edit::SetEnabled {
            node: NodeId(node),
            enabled: false,
        },
    }
}

/// 切り詰めの道具：後ろに `end: null` の trim があっても、前の trim を広げられる（下流の写しは対象を外した表示のもの）。
/// `[cancelPreview, handle{trim, step}]` と `handle{trim, step}` だけの、どちらでも。
#[test]
fn the_trim_tool_widens_an_earlier_trim() {
    let env = Env::new();
    let bytes = noise_wav(8_000, 1, 3 * C, 45);
    let id = add_audio(&env.lib, &bytes, "wav");
    let vp = Viewport::audio_idle();
    for cancel_first in [true, false] {
        let opened = env.open(id);
        let s = opened.session.clone();
        env.step(
            &s,
            vec![
                add(0, "audio.trim", json!({ "start": 1_000, "end": 2_000 })),
                add(1, "audio.trim", json!({ "start": 100, "end": null })),
            ],
            &vp,
        );
        let out = env.step(&s, vec![preview_disabled(1)], &vp);
        let info = shown_audio(&out.view);
        assert_eq!(info.frames, 3 * C - 100, "外している間は後ろの trim だけ");
        assert_eq!(
            info.nodes[0].marks,
            Some(Marks::Trim {
                start: 900,
                end: 1_900
            })
        );
        // 枠を広げて確定する（表示の 900〜5,000 は前の trim の 1,000〜5,100）
        let commit = handle(
            Handle::Trim {
                start: 900,
                end: 5_000,
            },
            Some(1),
            STEP,
        );
        let steps = if cancel_first {
            vec![Step::CancelPreview, commit]
        } else {
            vec![commit]
        };
        let out = env.step(&s, steps, &vp);
        assert!(out.rejected.is_none(), "{cancel_first}: {:?}", out.rejected);
        assert!(!out.status.provisional);
        assert!(enabled(&env, &s, 1));
        assert_eq!(params(&env, &s, 1), json!({ "start": 1_000, "end": 5_100 }));
        assert_eq!(shown_audio(&out.view).frames, 4_100 - 100);
        // 1 手で戻る
        env.step(&s, vec![Step::Undo], &vp);
        assert_eq!(params(&env, &s, 1), json!({ "start": 1_000, "end": 2_000 }));
    }
}

/// 数の欄などから続ける merge のハンドルの手：有効なノードでも外したノードでも 1 手にまとまり、⌘Z 1 回で前に戻る
/// （外したノードは外したまま・元の引数に）。
#[test]
fn merged_handle_steps_undo_at_once() {
    let env = Env::new();
    let vp = Viewport::audio_idle();
    let merge = HandleMode::Commit(CommitMode::Merge);
    for disabled in [false, true] {
        let (s, _) = open_with_nodes(
            &env,
            8_000,
            3 * C,
            vec![add(
                0,
                "audio.fade",
                json!({ "fadeIn": 10, "fadeOut": 0, "curve": "linear" }),
            )],
            None,
        );
        if disabled {
            env.step(
                &s,
                vec![Step::Commit {
                    edit: Edit::SetEnabled {
                        node: NodeId(1),
                        enabled: false,
                    },
                    mode: CommitMode::Step,
                }],
                &vp,
            );
        }
        let before = env.step(&s, Vec::new(), &vp).status.head;
        for end in [100, 200, 300] {
            let out = env.step(
                &s,
                vec![handle(Handle::FadeIn { end }, Some(1), merge)],
                &vp,
            );
            assert!(out.rejected.is_none(), "{:?}", out.rejected);
        }
        assert!(enabled(&env, &s, 1));
        assert_eq!(params(&env, &s, 1)["fadeIn"], json!(300));
        let out = env.step(&s, vec![Step::Undo], &vp);
        assert_eq!(out.status.head, before, "外した {disabled}：1 手で戻る");
        assert_eq!(enabled(&env, &s, 1), !disabled);
        assert_eq!(params(&env, &s, 1)["fadeIn"], json!(10));
    }
}

/// フェードインの終わりは区間の終わり（含まない位置）として戻す：下流の trim の出力の終わりへも動かせる。
#[test]
fn fade_in_can_reach_the_end_of_a_downstream_trim() {
    let env = Env::new();
    let (s, marks) = open_with_nodes(
        &env,
        8_000,
        3 * C,
        vec![
            add(
                0,
                "audio.fade",
                json!({ "fadeIn": 1_500, "fadeOut": 0, "curve": "linear" }),
            ),
            add(1, "audio.trim", json!({ "start": 1_000, "end": 2_000 })),
        ],
        None,
    );
    assert_eq!(
        marks[0],
        Some(Marks::Fade {
            in_end: Some(500),
            out_start: None
        })
    );
    // 出力の終わり（1,000）＝ trim の終わり 2,000 まで掛ける
    let out = env.step(
        &s,
        vec![handle(Handle::FadeIn { end: 1_000 }, Some(1), STEP)],
        &Viewport::audio_idle(),
    );
    assert!(out.rejected.is_none(), "{:?}", out.rejected);
    assert_eq!(params(&env, &s, 1)["fadeIn"], json!(2_000));
    assert_eq!(
        shown_audio(&out.view).nodes[0].marks,
        Some(Marks::Fade {
            in_end: Some(1_000),
            out_start: None
        })
    );
}

/// 後ろに trim（と resample）がある鎖でも、フェードインの終わりを出力の頭（0）に、フェードアウトの始めを出力の終わりに
/// 置く手は、写さずにフェードを 0 にする（三角を端まで・欄に 0・I/O）。ほかの値の写し方は変わらない。
#[test]
fn fades_can_be_cleared_through_a_downstream_trim() {
    let env = Env::new();
    let vp = Viewport::audio_idle();
    let n = 3 * C;
    let chains = [
        vec![add(1, "audio.trim", json!({ "start": 1_000, "end": null }))],
        vec![
            add(1, "audio.trim", json!({ "start": 1_000, "end": null })),
            add(2, "audio.resample", json!({ "rate": 4_000 })),
        ],
        vec![add(
            1,
            "audio.trim",
            json!({ "start": 1_000, "end": 150_000 }),
        )],
    ];
    for (i, tail) in chains.into_iter().enumerate() {
        let mut steps = vec![add(
            0,
            "audio.fade",
            json!({ "fadeIn": 1_500, "fadeOut": 2_000, "curve": "linear" }),
        )];
        steps.extend(tail);
        let (s, _) = open_with_nodes(&env, 8_000, n, steps, None);
        let frames = shown_audio(&env.step(&s, Vec::new(), &vp).view).frames;
        // フェードインを 0 に
        let out = env.step(
            &s,
            vec![handle(Handle::FadeIn { end: 0 }, Some(1), STEP)],
            &vp,
        );
        assert!(out.rejected.is_none(), "{i}: {:?}", out.rejected);
        assert_eq!(params(&env, &s, 1)["fadeIn"], json!(0), "{i}");
        assert_eq!(params(&env, &s, 1)["fadeOut"], json!(2_000), "{i}");
        // フェードアウトを 0 に（始めを出力の終わりへ）
        let out = env.step(
            &s,
            vec![handle(Handle::FadeOut { start: frames }, Some(1), STEP)],
            &vp,
        );
        assert!(out.rejected.is_none(), "{i}: {:?}", out.rejected);
        assert_eq!(params(&env, &s, 1)["fadeOut"], json!(0), "{i}");
        assert_eq!(
            shown_audio(&out.view).nodes[0].marks,
            Some(Marks::Fade {
                in_end: None,
                out_start: None
            }),
            "{i}"
        );
    }
    // ほかの値はこれまでどおり写す（trim の始め 1,000 の分だけ戻す）
    let (s, _) = open_with_nodes(
        &env,
        8_000,
        n,
        vec![
            add(
                0,
                "audio.fade",
                json!({ "fadeIn": 0, "fadeOut": 0, "curve": "linear" }),
            ),
            add(1, "audio.trim", json!({ "start": 1_000, "end": null })),
        ],
        None,
    );
    let out = env.step(
        &s,
        vec![
            handle(Handle::FadeIn { end: 500 }, Some(1), STEP),
            handle(
                Handle::FadeOut {
                    start: n - 1_000 - 300,
                },
                Some(1),
                STEP,
            ),
        ],
        &vp,
    );
    assert!(out.rejected.is_none(), "{:?}", out.rejected);
    assert_eq!(params(&env, &s, 1)["fadeIn"], json!(1_500));
    assert_eq!(params(&env, &s, 1)["fadeOut"], json!(300));
}
