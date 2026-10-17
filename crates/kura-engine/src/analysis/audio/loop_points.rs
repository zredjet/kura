//! `audio.loop` v1：ファイルに書かれたループ点とマーカー（`AudioMeta`）。ID は `audio.loop`（`loop` は Rust の予約語なので
//! ファイル名は `loop_points.rs`）。
//!
//! - ループ点は書き出しと同じ判定（`codec::symph::writable_loop`：start < end ≤ 長さ）を通ったものだけ。表示する
//!   ループ点と、保存して書かれるループ点が一致する。範囲の外・長さ 0・逆向きのループ点は null にして `droppedLoop`。
//!   （デコーダと写し方がすでに落とすので、ふつうは起きない。`AudioMeta` を直接組む呼び手のため。）
//! - マーカーは pos < 長さのもの（WAV・AIFF の読み取り・写し方と同じ）。位置の順（同じ位置は元の順。安定な並べ替え）。
//!   範囲の外の数を `droppedMarkers` に、範囲の中の数を `markerCount` に書く。結果の大きさを形から見積もれるように、
//!   結果に載せるのは先頭の `MAX_MARKERS`（1,024）個まで、名前は `MAX_NAME_BYTES`（256）バイトまで（文字の境で切る）。
//!   `markers` の数が `markerCount` より少なければ、載せきれなかったものがある。
//! - 読まないもの：MP3・m4a・AAC のループ点、Vorbis コメントの `LOOPEND` などの別名（読むならデコーダの側の変更）。
//! - 整数だけ（`Numeric::Exact`）。

use serde::{Deserialize, Serialize};

use crate::analysis::audio::FrameSpan;
use crate::analysis::{Analysis, AnalysisExample, AnalyzeCx, Estimate, NoParams};
use crate::codec::symph::writable_loop;
use crate::error::EngineError;
use crate::op::Numeric;
use crate::shape::{Shape, ValueKind};
use crate::testgen::{InputGen, MetaGen};
use crate::value::{Audio, LoopRange, Value};

/// 結果に載せるマーカーの数の上限。
pub const MAX_MARKERS: usize = 1_024;
/// 結果に載せるマーカーの名前のバイト数の上限（UTF-8。文字の境で切る）。
pub const MAX_NAME_BYTES: usize = 256;

/// マーカー 1 つ。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MarkerPoint {
    /// 位置（フレーム）。
    pub pos: u64,
    /// 名前（`MAX_NAME_BYTES` まで）。
    pub name: String,
}

/// 結果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LoopReport {
    /// 音声の長さ（フレーム）。
    pub frames: u64,
    /// サンプルレート（Hz。秒に直すため）。
    pub rate: u32,
    /// ループの区間 [start, end)（書き出しと同じ判定を通ったもの）。なければ null。
    #[serde(rename = "loop")]
    pub range: Option<FrameSpan>,
    /// メタデータにループ点があったが、範囲の外・長さ 0・逆向きで落とした。
    pub dropped_loop: bool,
    /// 範囲の中のマーカー（位置の順。先頭の `MAX_MARKERS` 個まで）。
    pub markers: Vec<MarkerPoint>,
    /// 範囲の中のマーカーの数。
    pub marker_count: u64,
    /// 範囲の外（pos ≥ 長さ）で落としたマーカーの数。
    pub dropped_markers: u64,
}
crate::json_output!(LoopReport);

fn clip_name(name: &str) -> String {
    if name.len() <= MAX_NAME_BYTES {
        return name.to_string();
    }
    let mut end = MAX_NAME_BYTES;
    while end > 0 && !name.is_char_boundary(end) {
        end -= 1;
    }
    name.get(..end).unwrap_or_default().to_string()
}

/// メタデータを読む。
pub fn read(a: &Audio) -> LoopReport {
    let frames = a.frames() as u64;
    let range = writable_loop(a.meta.loop_range, frames)
        .map(|LoopRange { start, end }| FrameSpan { start, end });
    let mut inside: Vec<&crate::value::Marker> =
        a.meta.markers.iter().filter(|m| m.pos < frames).collect();
    inside.sort_by_key(|m| m.pos);
    let marker_count = inside.len() as u64;
    LoopReport {
        frames,
        rate: a.rate(),
        range,
        dropped_loop: a.meta.loop_range.is_some() && range.is_none(),
        markers: inside
            .iter()
            .take(MAX_MARKERS)
            .map(|m| MarkerPoint {
                pos: m.pos,
                name: clip_name(&m.name),
            })
            .collect(),
        marker_count,
        dropped_markers: a.meta.markers.len() as u64 - marker_count,
    }
}

/// ループ点とマーカー。
pub struct LoopAnalysis;

/// マーカー 1 つの正規の JSON の長さの上限（`{"name":"…","pos":…},`。名前は逃がしで 6 倍まで、位置は 20 桁まで）。
const MARKER_BYTES: u64 = 9 + 6 * MAX_NAME_BYTES as u64 + 8 + 20 + 2;

impl Analysis for LoopAnalysis {
    const ID: &'static str = "audio.loop";
    const VERSION: u16 = 1;
    const ACCEPTS: &'static [ValueKind] = &[ValueKind::Audio];
    const NUMERIC: Numeric = Numeric::Exact;
    type Params = NoParams;
    type Output = LoopReport;

    fn run(_p: &NoParams, v: &Value, cx: &AnalyzeCx<'_>) -> Result<LoopReport, EngineError> {
        cx.check()?;
        let Value::Audio(a) = v else {
            return Err(EngineError::Internal(format!(
                "{} の入力が音声ではありません",
                Self::ID
            )));
        };
        Ok(read(a))
    }

    fn examples() -> Vec<AnalysisExample<NoParams>> {
        let sine = |loop_range| InputGen::Sine {
            rate: 22_050,
            channels: 1,
            frames: 1_000,
            freq: 440,
            loop_range,
        };
        let ex = |name, input| AnalysisExample {
            name,
            params: NoParams {},
            input,
        };
        vec![
            ex(
                "loop",
                sine(Some(LoopRange {
                    start: 10,
                    end: 1_000,
                })),
            ),
            ex("none", sine(None)),
            // マーカー：位置の順に並べ、範囲の外を落とす
            ex(
                "markers",
                InputGen::WithMeta {
                    base: Box::new(sine(Some(LoopRange { start: 0, end: 1 }))),
                    meta: MetaGen {
                        markers: vec![
                            (500, "後".into()),
                            (3, "b".into()),
                            (3, "a".into()),
                            (1_000, "外".into()),
                            (999, "x".repeat(300)),
                        ],
                        ..MetaGen::default()
                    },
                },
            ),
            // 範囲の外のループ点は落とす
            ex(
                "out-of-range-loop",
                sine(Some(LoopRange {
                    start: 900,
                    end: 1_001,
                })),
            ),
            ex(
                "reversed-loop",
                sine(Some(LoopRange {
                    start: 600,
                    end: 600,
                })),
            ),
            // 逆向き（start > end。どちらも範囲の中）は落とす（"reversed-loop" は長さ 0 の例）
            ex(
                "start-after-end-loop",
                sine(Some(LoopRange {
                    start: 700,
                    end: 300,
                })),
            ),
            // 始まりも終わりも長さの外は落とす
            ex(
                "loop-past-the-end",
                sine(Some(LoopRange {
                    start: 1_200,
                    end: 1_500,
                })),
            ),
            // 認める境：最後の 1 フレームだけのループ（start = 長さ − 1、end = 長さ）
            ex(
                "last-frame-loop",
                sine(Some(LoopRange {
                    start: 999,
                    end: 1_000,
                })),
            ),
        ]
    }

    fn estimate(_p: &NoParams, _input: &Shape) -> Estimate {
        Estimate {
            bytes: 256 + MAX_MARKERS as u64 * MARKER_BYTES,
            // メタデータを読むだけ（標本を見ない）
            cost: 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::AnalysisOutput;
    use crate::value::{AudioMeta, Marker};
    use std::sync::Arc;

    fn audio(frames: usize, meta: AudioMeta) -> Audio {
        Audio::new(8_000, vec![Arc::new(vec![0.0; frames])], meta).unwrap()
    }

    #[test]
    fn loops_use_the_export_rule_and_markers_are_sorted() {
        let l = |start, end| Some(LoopRange { start, end });
        for (range, want, dropped) in [
            (l(0, 100), Some(FrameSpan { start: 0, end: 100 }), false),
            (
                l(99, 100),
                Some(FrameSpan {
                    start: 99,
                    end: 100,
                }),
                false,
            ),
            (l(0, 101), None, true),
            (l(50, 50), None, true),
            (l(60, 50), None, true),
            (None, None, false),
        ] {
            let r = read(&audio(
                100,
                AudioMeta {
                    loop_range: range,
                    ..AudioMeta::default()
                },
            ));
            assert_eq!((r.range, r.dropped_loop), (want, dropped), "{range:?}");
            assert_eq!(writable_loop(range, 100).is_some(), want.is_some());
        }
        let markers = vec![
            Marker {
                pos: 7,
                name: "c".into(),
            },
            Marker {
                pos: 2,
                name: "a".into(),
            },
            Marker {
                pos: 100,
                name: "out".into(),
            },
            Marker {
                pos: 2,
                name: "b".into(),
            },
        ];
        let r = read(&audio(
            100,
            AudioMeta {
                markers,
                ..AudioMeta::default()
            },
        ));
        let names: Vec<&str> = r.markers.iter().map(|m| m.name.as_str()).collect();
        assert_eq!(names, ["a", "b", "c"]);
        assert_eq!((r.marker_count, r.dropped_markers), (3, 1));
    }

    /// 例の値を直接確かめる（ゴールデンに凍結する前に）：逆向き・長さの外は落とし、最後の 1 フレームは認める。
    #[test]
    fn loop_examples_follow_the_export_rule() {
        for (name, want) in [
            ("start-after-end-loop", None),
            ("loop-past-the-end", None),
            (
                "last-frame-loop",
                Some(FrameSpan {
                    start: 999,
                    end: 1_000,
                }),
            ),
        ] {
            let ex = LoopAnalysis::examples()
                .into_iter()
                .find(|e| e.name == name)
                .unwrap();
            let Value::Audio(a) = ex.input.generate().unwrap() else {
                panic!()
            };
            let r = read(&a);
            assert_eq!((r.range, r.dropped_loop), (want, want.is_none()), "{name}");
        }
    }

    /// 結果の大きさは見積もりに収まる（マーカーは 1,024 個まで・名前は 256 バイトまで、文字の境で切る）。
    #[test]
    fn the_output_is_bounded() {
        let markers: Vec<Marker> = (0..3_000u64)
            .map(|i| Marker {
                pos: i % 10,
                name: format!("\u{1}{}", "あ".repeat(200)),
            })
            .collect();
        let r = read(&audio(
            10,
            AudioMeta {
                markers,
                ..AudioMeta::default()
            },
        ));
        assert_eq!(r.markers.len(), MAX_MARKERS);
        assert_eq!(r.marker_count, 3_000);
        for m in &r.markers {
            assert!(m.name.len() <= MAX_NAME_BYTES);
            // 1 バイト + 「あ」（3 バイト）85 個 = 256
            assert_eq!(m.name.len(), 256);
        }
        // 同じ位置のものは元の順
        assert!(r.markers.windows(2).all(|w| w[0].pos <= w[1].pos));
        let bytes = r.to_canonical().unwrap();
        let est = LoopAnalysis::estimate(
            &NoParams {},
            &Shape::Audio {
                rate: 8_000,
                channels: 1,
                frames: crate::shape::Dim::Exact(10),
            },
        );
        assert!(bytes.len() as u64 <= est.bytes);
        assert_eq!(clip_name("あい"), "あい");
        assert_eq!(clip_name(&"x".repeat(300)).len(), 256);
    }
}
