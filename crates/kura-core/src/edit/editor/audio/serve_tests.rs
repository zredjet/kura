//! `/edit` の音声の PCM（A7）：公開した rev の有効な区間は全体の描画と標本が同じで、kura チャンクのデコーダは
//! `kura-edit/1:{samplesId}`。古い rev・無効な区間は 404、範囲の外は 416。元の PCM は元の値と同じ。

use kura_engine::Graph;
use serde_json::json;

use super::serve::{EDIT_DECODER_PREFIX, serve_input_pcm, serve_pcm};
use super::session_tests::*;
use crate::asset::pcm::PcmError;
use crate::edit::editor::Viewport;

/// 応答の WAV（テスト用）。
pub(super) struct Wav {
    pub channels: usize,
    pub rate: u32,
    pub start: u64,
    pub total: u64,
    pub looped: Option<(u64, u64)>,
    pub decoder: String,
    /// チャンネルごとの標本（i16 は /32768）。
    pub planes: Vec<Vec<f32>>,
}

pub(super) fn parse_wav(b: &[u8]) -> Wav {
    let u16_at = |i: usize| u16::from_le_bytes([b[i], b[i + 1]]);
    let u32_at = |i: usize| u32::from_le_bytes(b[i..i + 4].try_into().unwrap());
    let u64_at = |i: usize| u64::from_le_bytes(b[i..i + 8].try_into().unwrap());
    assert_eq!(&b[0..4], b"RIFF");
    assert_eq!(&b[36..40], b"kura");
    let (tag, channels, rate) = (u16_at(20), usize::from(u16_at(22)), u32_at(24));
    let k = 44;
    let len = usize::from(u16_at(k + 2));
    let flags = u32_at(k + 4);
    let kura_len = u32_at(40) as usize;
    let data_at = 44 + kura_len + (kura_len & 1);
    assert_eq!(&b[data_at..data_at + 4], b"data");
    let data = &b[data_at + 8..];
    let mut planes = vec![Vec::new(); channels];
    match tag {
        1 => {
            for (i, s) in data.as_chunks::<2>().0.iter().enumerate() {
                planes[i % channels].push(f32::from(i16::from_le_bytes([s[0], s[1]])) / 32_768.0);
            }
        }
        3 => {
            for (i, s) in data.as_chunks::<4>().0.iter().enumerate() {
                planes[i % channels].push(f32::from_le_bytes([s[0], s[1], s[2], s[3]]));
            }
        }
        other => panic!("知らない形式 {other}"),
    }
    Wav {
        channels,
        rate,
        start: u64_at(k + 8),
        total: u64_at(k + 16),
        looped: (flags & 1 == 1).then(|| (u64_at(k + 24), u64_at(k + 32))),
        decoder: String::from_utf8(b[k + 40..k + 40 + len].to_vec()).unwrap(),
        planes,
    }
}

/// 全体の描画の区間。
fn expected(a: &kura_engine::value::Audio, start: u64, end: u64) -> Vec<Vec<f32>> {
    (0..a.channels())
        .map(|ch| {
            let mut v = vec![0f32; (end - start) as usize];
            a.read(ch, start, &mut v).unwrap();
            v
        })
        .collect()
}

#[test]
fn edit_pcm_matches_the_render() {
    let env = Env::new();
    let frames = 3 * C + 17;
    let bytes = noise_wav(8_000, 2, frames, 21);
    let id = add_audio(&env.lib, &bytes, "wav");
    let opened = env.open(id);
    let s = opened.session.clone();
    let vp = Viewport::audio_idle();
    let out = env.step(
        &s,
        vec![
            add(
                0,
                "audio.fade",
                json!({ "fadeIn": C + 3, "fadeOut": 500, "curve": "linear" }),
            ),
            add(
                1,
                "audio.set_loop",
                json!({ "loop": { "start": 7, "end": 2 * C } }),
            ),
        ],
        &vp,
    );
    let info = shown_audio(&out.view).clone();
    let mut g = Graph::new_single();
    g.push_linear(
        "audio.fade",
        1,
        json!({ "fadeIn": C + 3, "fadeOut": 500, "curve": "linear" }),
        1,
    )
    .unwrap();
    g.push_linear(
        "audio.set_loop",
        1,
        json!({ "loop": { "start": 7, "end": 2 * C } }),
        2,
    )
    .unwrap();
    let want = rendered(&bytes, "wav", &g);
    for (a, b) in [(0, C), (C, 2 * C + 17), (3 * C, frames), (5, 6)] {
        let w = parse_wav(&serve_pcm(&env.engine, &s, info.rev, a, b).unwrap());
        assert_eq!(
            (w.channels, w.rate, w.start, w.total),
            (2, 8_000, a, frames)
        );
        assert_eq!(w.looped, Some((7, 2 * C)));
        assert_eq!(
            w.decoder,
            format!("{EDIT_DECODER_PREFIX}{}", info.samples_id)
        );
        assert_eq!(w.planes, expected(&want, a, b), "[{a}, {b})");
    }
    // 範囲の外は 416、知らないセッション・まだない rev は 404
    assert!(matches!(
        serve_pcm(&env.engine, &s, info.rev, 0, frames + 1),
        Err(PcmError::OutOfRange { .. })
    ));
    assert!(matches!(
        serve_pcm(&env.engine, "abc", info.rev, 0, 1),
        Err(PcmError::NotFound)
    ));
    assert!(matches!(
        serve_pcm(&env.engine, &s, info.rev + 1, 0, 1),
        Err(PcmError::NotFound)
    ));
    // 直前の rev は残り、その前は 404
    let out = env.step(
        &s,
        vec![set(
            1,
            json!({ "fadeIn": 9, "fadeOut": 500, "curve": "linear" }),
        )],
        &vp,
    );
    let next = shown_audio(&out.view).rev;
    assert!(
        serve_pcm(&env.engine, &s, info.rev, 0, 1).is_ok(),
        "直前の rev"
    );
    assert!(serve_pcm(&env.engine, &s, next, 0, 1).is_ok());
    env.step(
        &s,
        vec![set(
            1,
            json!({ "fadeIn": 11, "fadeOut": 500, "curve": "linear" }),
        )],
        &vp,
    );
    assert!(matches!(
        serve_pcm(&env.engine, &s, info.rev, 0, 1),
        Err(PcmError::NotFound)
    ));

    // 元の PCM は元の値（16 ビットのまま）・元のデコーダ
    let w = parse_wav(&serve_input_pcm(&env.engine, &s, C - 3, C + 9).unwrap());
    let original = rendered(&bytes, "wav", &Graph::new_single());
    assert_eq!(w.planes, expected(&original, C - 3, C + 9));
    assert_eq!(w.total, frames);
    assert_eq!(w.decoder, opened.source.audio.unwrap().decoder);
    assert!(matches!(
        serve_input_pcm(&env.engine, &s, 0, frames + 1),
        Err(PcmError::OutOfRange { .. })
    ));
}

/// 長い音声：表示の値の有効なチャンクに覆われない区間は 404（頼んでから取る）。
#[test]
fn edit_pcm_outside_the_drawn_spans_is_not_found() {
    let env = Env::new();
    let frames = 130 * C;
    let bytes = noise_wav(8_000, 1, frames, 22);
    let id = add_audio(&env.lib, &bytes, "wav");
    let opened = env.open(id);
    let s = opened.session.clone();
    let out = env.step(
        &s,
        vec![add(0, "audio.gain", json!({ "db": -1.0 }))],
        &spans(&[(10 * C, 11 * C)]),
    );
    let rev = shown_audio(&out.view).rev;
    assert!(serve_pcm(&env.engine, &s, rev, 10 * C, 11 * C).is_ok());
    assert!(matches!(
        serve_pcm(&env.engine, &s, rev, 10 * C, 11 * C + 1),
        Err(PcmError::NotFound)
    ));
    assert!(matches!(
        serve_pcm(&env.engine, &s, rev, 0, 1),
        Err(PcmError::NotFound)
    ));
    // 元は遅延の源から読める（どこでも）
    let w = parse_wav(&serve_input_pcm(&env.engine, &s, 100 * C, 100 * C + 4).unwrap());
    assert_eq!(w.planes[0].len(), 4);
}

/// A7：同じ rev のまま区間を替えて描き直しても、直前の表示（同じ rev）が覆う区間の PCM は取れる（今 → 直前の順に覆うものを
/// 探す）。どちらにも覆われない区間は 404。
#[test]
fn edit_pcm_of_the_same_rev_reads_the_previous_spans_too() {
    let env = Env::new();
    let frames = 130 * C;
    let bytes = noise_wav(8_000, 1, frames, 23);
    let id = add_audio(&env.lib, &bytes, "wav");
    let s = env.open(id).session.clone();
    let out = env.step(
        &s,
        vec![add(0, "audio.gain", json!({ "db": -1.0 }))],
        &spans(&[(10 * C, 11 * C)]),
    );
    let rev = shown_audio(&out.view).rev;
    let out = env.step(&s, Vec::new(), &spans(&[(50 * C, 51 * C)]));
    assert_eq!(shown_audio(&out.view).rev, rev, "手なしは同じ rev");
    let mut g = Graph::new_single();
    g.push_linear("audio.gain", 1, json!({ "db": -1.0 }), 1)
        .unwrap();
    let want = rendered(&bytes, "wav", &g);
    for (a, b) in [(10 * C, 11 * C), (50 * C, 51 * C), (10 * C + 5, 10 * C + 9)] {
        let w = parse_wav(&serve_pcm(&env.engine, &s, rev, a, b).unwrap());
        assert_eq!(w.planes, expected(&want, a, b), "[{a}, {b})");
    }
    assert!(matches!(
        serve_pcm(&env.engine, &s, rev, 11 * C - 1, 50 * C + 1),
        Err(PcmError::NotFound)
    ));
    assert!(matches!(
        serve_pcm(&env.engine, &s, rev, 0, 1),
        Err(PcmError::NotFound)
    ));
}
