//! ハンドルの便宜の手とノードの印（M13 段 4。docs/editing.md §12.12）。
//!
//! - **手**（[`handle_edit`]）：フロントは表示している出力のフレームだけを送る。対象のノードより下流の時間の写しを逆にたどって
//!   ノードの座標へ写し、ノードの引数にする（切り詰めの下流の写しは、呼び手が対象を外した計画の連鎖を渡す）。ノードがあれば `SetParams`（今の引数に重ねる。外していれば有効にして 1 手にする
//!   ため、外したノードは文書ごとの `Replace`。呼び手はまとめの鍵を `SetParams` と同じにする）、なければ末尾に `AddNode`
//!   （末尾なので下流の写しはなく、値はそのまま引数）。
//!   写せなければ `Invalid`。
//! - **印**（[`node_views`]）：連鎖のノードごとの入力のレート・長さと、引数の位置を下流の写しで出力のフレームに写したもの
//!   （trim は自分の写しを当てない：切り詰めの道具はこのノードを仮に外して入力を出している間に枠を出すため）。フェードは
//!   三角の位置（`in_end`・`out_start`）に加えて、曲線を引くための見かけの始まり・終わり（[`fade_marks`]）。

use kura_engine::doc::NodeId;
use kura_engine::render::TimeLink;
use kura_engine::session::{CommitMode, Edit, ParamsView, Snapshot};
use kura_engine::{Engine, EngineError, Issue, IssueCode, Issues};
use serde_json::{Value as Json, json};

use super::timeline::{backward, backward_end, forward, forward_end, forward_line, rate_frames};

/// ハンドル（どれも表示している出力のフレーム）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Handle {
    /// 切り詰めの枠 `[start, end)`。
    Trim { start: u64, end: u64 },
    /// フェードインの終わり。
    FadeIn { end: u64 },
    /// フェードアウトの始まり。
    FadeOut { start: u64 },
    /// ループの区間（None は消す）。
    Loop { range: Option<(u64, u64)> },
}

impl Handle {
    /// ハンドルの操作。
    pub fn op(&self) -> &'static str {
        match self {
            Self::Trim { .. } => "audio.trim",
            Self::FadeIn { .. } | Self::FadeOut { .. } => "audio.fade",
            Self::Loop { .. } => "audio.set_loop",
        }
    }
}

/// ハンドルの手の当て方。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HandleMode {
    /// 仮の値（ドラッグの途中）。
    Preview,
    /// 確定する（`Merge` で同じノードの続く手を 1 手に）。
    Commit(CommitMode),
}

/// ノードの印（出力のフレーム）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Marks {
    Trim {
        start: u64,
        end: u64,
    },
    /// フェード。`in_end`・`out_start` は三角（ハンドル）の位置。`in_start`・`out_end` は曲線を引く見かけの始まり・終わり
    /// （符号つき。下流で頭・終わりを切られていれば出力の範囲の外）。どれも長さ 0・写せなければ None。
    Fade {
        in_start: Option<i64>,
        in_end: Option<u64>,
        out_start: Option<u64>,
        out_end: Option<i64>,
    },
    Loop {
        start: u64,
        end: u64,
    },
}

/// 連鎖のノード 1 つの表示（引数の欄で時間と標本を変換するのに使う）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioNodeView {
    pub id: NodeId,
    pub op: String,
    pub enabled: bool,
    /// 入力 0 のレートと長さ・チャンネルの数。
    pub in_rate: u32,
    pub in_frames: u64,
    pub in_channels: u16,
    /// 引数の位置を出力のフレームに写したもの（写せない・印のない操作は None）。
    pub marks: Option<Marks>,
}

fn invalid(message: impl Into<String>) -> EngineError {
    EngineError::Invalid(Issues::one(Issue::error(IssueCode::BadParams, message)))
}

/// ノードの今の引数（オブジェクト）。
fn params_of(head: &Snapshot, node: NodeId) -> Option<Json> {
    match head.node(node)?.params {
        ParamsView::Json(v) => Some(v.clone()),
        ParamsView::Log { shell, .. } => Some(shell.clone()),
    }
}

/// 引数の欄を足す・上書きする。
fn merged(base: Json, patch: Json) -> Json {
    match (base, patch) {
        (Json::Object(mut a), Json::Object(b)) => {
            for (k, v) in b {
                a.insert(k, v);
            }
            Json::Object(a)
        }
        (_, patch) => patch,
    }
}

const UNMAPPABLE: &str = "後ろの操作で時間が写せないので、ハンドルでは直せません";

/// ハンドルの位置（出力のフレーム）を、下流の写し `down` を逆にたどってノードの座標の引数にする。`in_frames` はノードの
/// 入力の長さ、`now` はノードの今の引数（頭の文書。末尾に足すときは null）。
///
/// - 動かさなかった側（今の引数を下流で写した位置、つまり印と同じ値）は今の引数のまま（往復の丸めでずらさない）。
/// - 区間の終わり（trim の end・ループの end・フェードインの終わり）は `back_end` で戻す。入力の終わりを下流で写した位置
///   （`forward_end(in_frames)`。出力の終わり）以上なら入力の終わりにする（trim は end: null）。下げるレートの往復は、
///   入力の長さを超えたり手前に戻ったりするため。
/// - フェードを消す手（フェードインの終わりが出力の頭 0、フェードアウトの始めが出力の終わり以上）は写さずに 0 にする。
///   下流に trim があると、出力の頭・終わりちょうどは写した先の区間の外になり戻せないため。
fn handle_params(
    handle: &Handle,
    down: &[TimeLink],
    in_frames: u64,
    now: &Json,
) -> Result<Json, EngineError> {
    let back = |p: u64| backward(down, p).ok_or_else(|| invalid(UNMAPPABLE));
    let back_end = |p: u64| backward_end(down, p).ok_or_else(|| invalid(UNMAPPABLE));
    let in_end = forward_end(down, in_frames);
    // 出力の長さ（下流がなければ入力の長さ）
    let out_frames = match down.last() {
        Some(l) => rate_frames(&l.output).map(|(_, n)| n),
        None => Some(in_frames),
    };
    let at_out_end = |p: u64| out_frames.or(in_end).is_some_and(|n| p >= n);
    let start_of = |p: u64, cur: Option<u64>| match cur {
        Some(c) if forward(down, c) == Some(p) => Ok(c),
        _ => back(p),
    };
    let end_of = |p: u64, cur: Option<u64>| match cur {
        Some(c) if forward_end(down, c) == Some(p) => Ok(c),
        _ if in_end.is_some_and(|n| p >= n) => Ok(in_frames),
        _ => back_end(p),
    };
    let num = |v: &Json, k: &str| v.get(k).and_then(Json::as_u64);
    Ok(match *handle {
        Handle::Trim { start, end } => {
            if start >= end {
                return Err(invalid("切り詰めの始めは終わりより前にします"));
            }
            // 今の終わり（null は入力の終わり）
            let cur_end = now
                .is_object()
                .then(|| num(now, "end").unwrap_or(in_frames));
            let (s, e) = (start_of(start, num(now, "start"))?, end_of(end, cur_end)?);
            if e > in_frames || s >= e {
                return Err(invalid("切り詰めの枠が入力の外です"));
            }
            json!({ "start": s, "end": (e != in_frames).then_some(e) })
        }
        // フェードを消す（出力の頭・終わりまで寄せた）
        Handle::FadeIn { end: 0 } => json!({ "fadeIn": 0 }),
        Handle::FadeOut { start } if at_out_end(start) => json!({ "fadeOut": 0 }),
        // フェードインの長さは区間の終わり（含まない位置）なので、印（forward_end）と対になる back_end で戻す
        Handle::FadeIn { end } => json!({ "fadeIn": end_of(end, None)?.min(in_frames) }),
        Handle::FadeOut { start } => {
            let s = back(start)?;
            json!({ "fadeOut": in_frames.saturating_sub(s) })
        }
        Handle::Loop { range: None } => json!({ "loop": null }),
        Handle::Loop {
            range: Some((start, end)),
        } => {
            if start >= end {
                return Err(invalid("ループの始めは終わりより前にします"));
            }
            let cur = now.get("loop").filter(|l| l.is_object());
            let s = start_of(start, cur.and_then(|l| num(l, "start")))?;
            let e = end_of(end, cur.and_then(|l| num(l, "end")))?;
            json!({ "loop": { "start": s, "end": e } })
        }
    })
}

/// 既定の引数（末尾に足すとき）。
fn fresh_params(handle: &Handle, frames: u64) -> Result<Json, EngineError> {
    let base = match handle {
        Handle::FadeIn { .. } | Handle::FadeOut { .. } => {
            json!({ "fadeIn": 0, "fadeOut": 0, "curve": "linear" })
        }
        _ => json!({}),
    };
    Ok(merged(
        base,
        handle_params(handle, &[], frames, &Json::Null)?,
    ))
}

/// ハンドルの手を `Edit` にする（モジュールの説明）。`chain` は時間の写しの連鎖（切り詰めは今の文書で対象を外した計画の
/// もの、ほかは今の計画のもの）、`output` は今の出力の形、`head` は確定した頭の文書。
pub fn handle_edit(
    e: &Engine,
    chain: &[TimeLink],
    output: &kura_engine::Shape,
    head: &Snapshot,
    handle: &Handle,
    node: Option<NodeId>,
) -> Result<Edit, EngineError> {
    let op = handle.op();
    let Some(id) = node else {
        let frames = rate_frames(output)
            .map(|(_, n)| n)
            .ok_or_else(|| invalid("出力の長さが分かりません"))?;
        let v = e
            .registry()
            .current_version(op)
            .ok_or_else(|| EngineError::Internal(format!("{op} がありません")))?;
        return Ok(Edit::AddNode {
            at: head.node_count(),
            op: op.into(),
            v,
            params: fresh_params(handle, frames)?,
            seed: None,
            extra_inputs: Vec::new(),
        });
    };
    let view = head
        .node(id)
        .ok_or_else(|| invalid(format!("ノード {} はありません", id.0)))?;
    if view.op != op {
        return Err(invalid(format!("このノードは {op} ではありません")));
    }
    let enabled = view.enabled;
    let at = chain
        .iter()
        .position(|l| l.node == id)
        .ok_or_else(|| invalid("このノードは出力につながっていません"))?;
    let in_frames = rate_frames(&chain[at].input)
        .map(|(_, n)| n)
        .ok_or_else(|| invalid("ノードの入力の長さが分かりません"))?;
    let now = params_of(head, id).ok_or_else(|| invalid("引数を読めません"))?;
    let patch = handle_params(handle, &chain[at + 1..], in_frames, &now)?;
    let params = merged(now, patch);
    if enabled {
        return Ok(Edit::SetParams { node: id, params });
    }
    // 外したノード：引数と有効を 1 手で替える
    let mut graph = head.to_graph();
    let n = graph
        .nodes
        .iter_mut()
        .find(|n| n.id == id)
        .ok_or_else(|| invalid(format!("ノード {} はありません", id.0)))?;
    n.params = params;
    n.enabled = true;
    Ok(Edit::Replace { graph })
}

/// フェードの印。`down` はノードより下流の写し、`in_frames` はノードの入力の長さ（フェードは時間を変えないので出力の長さも同じ）。
///
/// - 三角：`in_end` は `fadeIn`（入力の長さで切る）を `forward_end`、`out_start` は `in_frames − fadeOut` を `forward` で写す
///   （手の写し方と対）。
/// - 見かけの始まり `in_start`・終わり `out_end`：フェードインの区間 `[0, fadeIn)`（フェードアウトは `[in_frames − fadeOut,
///   in_frames)`）の出力に写る部分の写しを 1 本の直線で延ばし、入力の 0（入力の終わり）を写した位置（[`forward_line`]）。
///   三角が写せないとき・区間が継ぎ目にかかるときは None（フロントは曲線を描かない）。
pub fn fade_marks(down: &[TimeLink], in_frames: u64, fade_in: u64, fade_out: u64) -> Marks {
    let fin = fade_in.min(in_frames);
    let out_from = in_frames.saturating_sub(fade_out);
    let in_end = (fade_in > 0).then(|| forward_end(down, fin)).flatten();
    let out_start = (fade_out > 0).then(|| forward(down, out_from)).flatten();
    Marks::Fade {
        in_start: in_end.and_then(|_| forward_line(down, (0, fin), 0)),
        in_end,
        out_start,
        out_end: out_start.and_then(|_| forward_line(down, (out_from, in_frames), in_frames)),
    }
}

/// 連鎖のノードの表示（モジュールの説明）。
pub fn node_views(chain: &[TimeLink], head: &Snapshot) -> Vec<AudioNodeView> {
    chain
        .iter()
        .enumerate()
        .filter_map(|(i, l)| {
            let (in_rate, in_frames) = rate_frames(&l.input)?;
            let in_channels = match l.input {
                kura_engine::Shape::Audio { channels, .. } => channels,
                _ => return None,
            };
            let down = &chain[i + 1..];
            let p = params_of(head, l.node).unwrap_or(Json::Null);
            let num = |k: &str| p.get(k).and_then(Json::as_u64);
            let marks = match l.op.as_str() {
                "audio.trim" => num("start")
                    .zip(Some(num("end").unwrap_or(in_frames)))
                    .and_then(|(s, e)| {
                        Some(Marks::Trim {
                            start: forward(down, s)?,
                            end: forward_end(down, e)?,
                        })
                    }),
                "audio.fade" => Some(fade_marks(
                    down,
                    in_frames,
                    num("fadeIn").unwrap_or(0),
                    num("fadeOut").unwrap_or(0),
                )),
                "audio.set_loop" => p.get("loop").and_then(|r| {
                    let (s, e) = (r.get("start")?.as_u64()?, r.get("end")?.as_u64()?);
                    Some(Marks::Loop {
                        start: forward(down, s)?,
                        end: forward_end(down, e)?,
                    })
                }),
                _ => None,
            };
            Some(AudioNodeView {
                id: l.node,
                op: l.op.clone(),
                enabled: l.enabled,
                in_rate,
                in_frames,
                in_channels,
                marks,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use kura_engine::mapping::{Seg, TimeMap};
    use kura_engine::{Dim, Shape};

    fn audio(frames: u64) -> Shape {
        Shape::Audio {
            rate: 8_000,
            channels: 1,
            frames: Dim::Exact(frames),
        }
    }

    fn segs(list: &[(u64, u64, u64)], frames: u64, out: u64) -> TimeLink {
        TimeLink {
            node: NodeId(9),
            op: "x".into(),
            enabled: true,
            time: TimeMap::Segments {
                segs: list
                    .iter()
                    .map(|&(src_start, src_end, dst_start)| Seg {
                        src_start,
                        src_end,
                        dst_start,
                    })
                    .collect(),
            },
            input: audio(frames),
            output: audio(out),
        }
    }

    /// 下流がなければ、見かけの始まりは 0・終わりは入力の長さ（三角と同じ所から曲線を引く今までの描き方と同じ）。長さ 0 は None。
    #[test]
    fn fade_marks_without_downstream() {
        assert_eq!(
            fade_marks(&[], 10_000, 3_000, 2_000),
            Marks::Fade {
                in_start: Some(0),
                in_end: Some(3_000),
                out_start: Some(8_000),
                out_end: Some(10_000),
            }
        );
        assert_eq!(
            fade_marks(&[], 10_000, 0, 0),
            Marks::Fade {
                in_start: None,
                in_end: None,
                out_start: None,
                out_end: None,
            }
        );
    }

    /// フェードの区間の出力に写る部分が継ぎ目にかかる（真ん中を抜いて継ぐ）・写しが分からないと、見かけの始まり・終わりは
    /// None（三角はそれぞれ写せれば出す）。
    #[test]
    fn fade_marks_refuse_seams_and_unknown_maps() {
        // [0, 1,000) と [2,000, 10,000) を継ぐ：フェードイン [0, 3,000) は継ぎ目にかかる。フェードアウト [8,000, 10,000) は 1 本
        let cut = segs(&[(0, 1_000, 0), (2_000, 10_000, 1_000)], 10_000, 9_000);
        assert_eq!(
            fade_marks(std::slice::from_ref(&cut), 10_000, 3_000, 2_000),
            Marks::Fade {
                in_start: None,
                in_end: Some(2_000),
                out_start: Some(7_000),
                out_end: Some(9_000),
            }
        );
        // フェードアウトの区間が継ぎ目にかかる
        let cut = segs(&[(0, 8_500, 0), (9_000, 10_000, 8_500)], 10_000, 9_500);
        assert_eq!(
            fade_marks(&[cut], 10_000, 0, 2_000),
            Marks::Fade {
                in_start: None,
                in_end: None,
                out_start: Some(8_000),
                out_end: None,
            }
        );
        // 写しが分からない：三角も見かけの位置も None
        for time in [TimeMap::Unknown, TimeMap::Reset] {
            let l = TimeLink {
                node: NodeId(9),
                op: "x".into(),
                enabled: true,
                time,
                input: audio(10_000),
                output: audio(10_000),
            };
            assert_eq!(
                fade_marks(&[l], 10_000, 3_000, 2_000),
                Marks::Fade {
                    in_start: None,
                    in_end: None,
                    out_start: None,
                    out_end: None,
                }
            );
        }
    }
}
