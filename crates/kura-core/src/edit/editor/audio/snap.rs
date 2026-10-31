//! ゼロ交差への吸着（M13 段 4。`edit_snap`）。
//!
//! - 読む区間は `[f − r − 1, f + r + 1) ∩ [0, n)`（r は [`SNAP_MAX_RADIUS`] で切る）。公開した今の表示の値が覆っていれば
//!   そこから読み、覆っていなければ錠の中で区間だけを描いて読む（`render_area`）。
//! - `Latest` の世代を払い出さない（スポイトと同じ。描いている途中の表示を止めない）。止まるのは閉じたときだけ。
//! - ゼロ交差（[`nearest_zero_crossing`]）：チャンネルの和 m について、g は「m[g] == 0、または m[g−1] と m[g] の符号が違う」
//!   位置で、|g − f| ≤ r のもの（読む区間の左端 f − r − 1 は前の標本との比べのためだけに読む）。|g − f| が最も小さいもの
//!   （同じなら小さい g）。なければ `found: false` で f を返す。

use kura_engine::Shape;
use kura_engine::render::area::AreaRequest;
use kura_engine::render::estimate::{EstimateSpec, Source};
use kura_engine::render::region::{Area, PartialValue};

use super::super::{EditorEnv, EditorError, ProblemCode, editor_of, settle};

/// ゼロ交差を探す半径の上限（フレーム）。
pub const SNAP_MAX_RADIUS: u32 = 4_096;

/// 吸着の結果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Snapped {
    /// 寄せた位置（見つからなければ頼んだ位置）。
    pub frame: u64,
    pub found: bool,
}

/// `planes`（フレーム `offset` から。チャンネルの順・同じ長さ）の中で、`f` に最も近い半径 `radius` 以内のゼロ交差
/// （モジュールの説明）。
pub fn nearest_zero_crossing(planes: &[&[f32]], offset: u64, f: u64, radius: u64) -> Option<u64> {
    let n = planes.first().map_or(0, |p| p.len());
    let mix = |i: usize| -> f32 { planes.iter().map(|p| p[i]).sum() };
    let mut best: Option<u64> = None;
    let mut prev: Option<f32> = None;
    for i in 0..n {
        let m = mix(i);
        let g = offset + i as u64;
        let crossing =
            m == 0.0 || prev.is_some_and(|p| (p < 0.0 && m > 0.0) || (p > 0.0 && m < 0.0));
        if crossing && g.abs_diff(f) <= radius && best.is_none_or(|b| g.abs_diff(f) < b.abs_diff(f))
        {
            best = Some(g);
        }
        prev = Some(m);
    }
    best
}

/// 区間の全チャンネルを読む。
fn read(
    value: &PartialValue,
    channels: usize,
    a: u64,
    b: u64,
) -> Result<Vec<Vec<f32>>, EditorError> {
    (0..channels)
        .map(|ch| {
            let mut v = vec![0f32; (b - a) as usize];
            value
                .audio_read(ch, a, &mut v)
                .map(|()| v)
                .map_err(|e| EditorError::from_engine("標本を読めません", e))
        })
        .collect()
}

/// 今の rev の出力で、`frame` に最も近いゼロ交差（半径 `radius`）。
pub fn snap(
    env: &EditorEnv<'_>,
    session: &str,
    frame: u64,
    radius: u32,
) -> Result<Snapped, EditorError> {
    let editor = editor_of(env, session)?;
    let mut guard = editor.lock_state();
    let st = guard.as_mut().ok_or(EditorError::NoSession)?;
    if st.plan_rev != st.session.rev() {
        return Err(EditorError::Invalid {
            message: "今の文書の計画を作れません".into(),
            issues: Vec::new(),
        });
    }
    let Some(Shape::Audio {
        channels, frames, ..
    }) = st
        .plan
        .output_shapes()
        .into_iter()
        .find(|(id, _)| *id == st.output)
        .map(|(_, s)| s)
    else {
        return Err(EditorError::Invalid {
            message: "音声ではありません".into(),
            issues: Vec::new(),
        });
    };
    let n = frames.max();
    let r = u64::from(radius.min(SNAP_MAX_RADIUS));
    let f = frame.min(n);
    let (a, b) = (f.saturating_sub(r + 1), f.saturating_add(r + 1).min(n));
    if a >= b {
        return Ok(Snapped {
            frame: f,
            found: false,
        });
    }
    let channels = usize::from(channels);
    let span = Area::span(a, b);
    let rev = st.session.rev().0;
    let planes = match editor.published_audio_covering(rev, &span) {
        Some((value, _)) => read(&value, channels, a, b)?,
        None => {
            let plan = st.plan.clone();
            let spec = EstimateSpec::new(0)
                .all_inputs(&plan, Source::Held)
                .area(st.output, span.clone());
            if let Err(e) = editor.runtime.admit_preview(
                &plan,
                spec.clone(),
                &st.inputs,
                &mut st.cache,
                &mut st.lease,
            ) {
                let problem = editor.admit_problem(&plan, spec, st, e);
                settle(st);
                return Err(match problem.code {
                    ProblemCode::TooLarge => EditorError::TooLarge(problem.message),
                    _ => EditorError::Busy(problem.message),
                });
            }
            let req = AreaRequest {
                output: st.output,
                area: span,
            };
            let (inputs, cache, mon) = (&st.inputs, &mut st.cache, editor.cancel.as_ref());
            let r = editor
                .runtime
                .preview(|e| e.render_area(&plan, inputs, cache, &req, None, mon));
            settle(st);
            let drawn = r.map_err(|e| EditorError::from_engine("区間を描けません", e))?;
            read(&drawn.value, channels, a, b)?
        }
    };
    let views: Vec<&[f32]> = planes.iter().map(Vec::as_slice).collect();
    Ok(match nearest_zero_crossing(&views, a, f, r) {
        Some(g) => Snapped {
            frame: g,
            found: true,
        },
        None => Snapped {
            frame: f,
            found: false,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nearest_crossings() {
        // 和：[0.5, 0.2, -0.1, -0.3, 0.0, 0.4]
        let l = [0.25f32, 0.1, -0.05, -0.2, 0.0, 0.2];
        let r = [0.25f32, 0.1, -0.05, -0.1, 0.0, 0.2];
        let p: Vec<&[f32]> = vec![&l, &r];
        // 交差：2（+→−）・4（0）・5 は 4 が 0 なので符号の比べには入らない（0 は正でも負でもない）
        assert_eq!(nearest_zero_crossing(&p, 100, 100, 9), Some(102));
        assert_eq!(
            nearest_zero_crossing(&p, 100, 103, 9),
            Some(102),
            "同じ距離なら小さい g"
        );
        assert_eq!(nearest_zero_crossing(&p, 100, 105, 9), Some(104));
        let flat = [0.5f32; 8];
        assert_eq!(nearest_zero_crossing(&[&flat], 0, 3, 9), None);
        assert_eq!(nearest_zero_crossing(&[], 0, 3, 9), None);
    }

    /// 半径の外は返さない：読む区間の左端（f − r − 1）は前の標本との比べのためだけに読む。半径 0・左端の 0 も。
    #[test]
    fn crossings_stay_within_the_radius() {
        // 半径 2・f = 3：読む区間は [0, 6)。g = 0 は |g − f| = 3 なので返さない
        let edge = [0.0f32, 0.5, 0.5, 0.5, 0.5, 0.5];
        assert_eq!(nearest_zero_crossing(&[&edge], 0, 3, 2), None);
        // 符号の比べは左端の標本と g = f − r の間でもよい（[−, +] の + の位置 1 は半径 2 の内）
        let flip = [-0.5f32, 0.5, 0.5, 0.5, 0.5, 0.5];
        assert_eq!(nearest_zero_crossing(&[&flip], 0, 3, 2), Some(1));
        // 半径 0：f の標本が 0 のときだけ
        let pair = [0.0f32, 0.5];
        assert_eq!(nearest_zero_crossing(&[&pair], 2, 3, 0), None);
        let zero = [0.5f32, 0.0];
        assert_eq!(nearest_zero_crossing(&[&zero], 2, 3, 0), Some(3));
        // 左端（f ≤ r なら読む区間は 0 から）：g = 0 の 0 は半径の内
        let start = [0.0f32, 0.5, 0.5];
        assert_eq!(nearest_zero_crossing(&[&start], 0, 1, 2), Some(0));
        assert_eq!(nearest_zero_crossing(&[&start], 0, 2, 1), None);
    }
}
