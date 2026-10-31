//! ハンドルの便宜の手とノードの印（M13 段 4。docs/editing.md §12.12）。
//!
//! - **手**（[`handle_edit`]）：フロントは表示している出力のフレームだけを送る。対象のノードより下流の時間の写しを逆にたどって
//!   ノードの座標へ写し、ノードの引数にする（切り詰めの下流の写しは、呼び手が対象を外した計画の連鎖を渡す）。ノードがあれば `SetParams`（今の引数に重ねる。外していれば有効にして 1 手にする
//!   ため、外したノードは文書ごとの `Replace`。呼び手はまとめの鍵を `SetParams` と同じにする）、なければ末尾に `AddNode`
//!   （末尾なので下流の写しはなく、値はそのまま引数）。
//!   写せなければ `Invalid`。
//! - **印**（[`node_views`]）：連鎖のノードごとの入力のレート・長さと、引数の位置を下流の写しで出力のフレームに写したもの
//!   （trim は自分の写しを当てない：切り詰めの道具はこのノードを仮に外して入力を出している間に枠を出すため）。

use kura_engine::doc::NodeId;
use kura_engine::render::TimeLink;
use kura_engine::session::{CommitMode, Edit, ParamsView, Snapshot};
use kura_engine::{Engine, EngineError, Issue, IssueCode, Issues};
use serde_json::{Value as Json, json};

use super::timeline::{backward, backward_end, forward, forward_end, rate_frames};

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
    Fade {
        in_end: Option<u64>,
        out_start: Option<u64>,
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
    /// 入力 0 のレートと長さ。
    pub in_rate: u32,
    pub in_frames: u64,
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
fn handle_params(
    handle: &Handle,
    down: &[TimeLink],
    in_frames: u64,
    now: &Json,
) -> Result<Json, EngineError> {
    let back = |p: u64| backward(down, p).ok_or_else(|| invalid(UNMAPPABLE));
    let back_end = |p: u64| backward_end(down, p).ok_or_else(|| invalid(UNMAPPABLE));
    let in_end = forward_end(down, in_frames);
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

/// 連鎖のノードの表示（モジュールの説明）。
pub fn node_views(chain: &[TimeLink], head: &Snapshot) -> Vec<AudioNodeView> {
    chain
        .iter()
        .enumerate()
        .filter_map(|(i, l)| {
            let (in_rate, in_frames) = rate_frames(&l.input)?;
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
                "audio.fade" => {
                    let fin = num("fadeIn").filter(|n| *n > 0);
                    let fout = num("fadeOut").filter(|n| *n > 0);
                    Some(Marks::Fade {
                        in_end: fin.and_then(|n| forward_end(down, n.min(in_frames))),
                        out_start: fout.and_then(|n| forward(down, in_frames.saturating_sub(n))),
                    })
                }
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
                marks,
            })
        })
        .collect()
}
