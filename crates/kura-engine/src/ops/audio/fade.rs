//! `audio.fade` v1：始めのフェードインと終わりのフェードアウト（長さはフレーム）。
//!
//! 長さ N の音声のフレーム i の倍率 g(i)：フェードイン（長さ a）は i < a のとき c(i / a)、フェードアウト（長さ b）は
//! i ≥ N − b のとき c((N − 1 − i) / b)、重なればその積、ほかは 1。最初と最後の標本は 0 になる。
//! 曲線 c(t) は `linear`（t）か `equalPower`（sin(πt/2)。`math::sin`）。t は f64 の割り算 1 回で求める。
//! 標本は `(f64 × g) as f32`。長さ 0 のフェードは何も変えない（E17）。長さが音声より長いものは断る。

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::audio::{AudioKernel, copy_from, render_all};
use crate::error::{EngineError, Issues};
use crate::mapping::{SpaceMap, TimeMap};
use crate::math;
use crate::op::{
    Applied, ApplyCx, Example, InferCx, Inferred, Lift, MetaAccess, Numeric, Op, OpParams,
    ParamKind, ParamSchema, PortSpec, Signature, Unit, expect_inputs, spec,
};
use crate::ops::util::{MAX_FRAME_PARAM, audio_of, audio_shape, bad_param};
use crate::render::region::{Area, Reach};
use crate::shape::{Dim, Shape, ValueKind};
use crate::testgen::InputGen;
use crate::value::{Audio, Value};

/// フェードの曲線。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FadeCurve {
    /// 直線（t）。
    Linear,
    /// 等パワー（sin(πt/2)）。
    EqualPower,
}

/// 引数。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FadeParams {
    /// フェードインの長さ（フレーム）。
    pub fade_in: u64,
    /// フェードアウトの長さ（フレーム）。
    pub fade_out: u64,
    pub curve: FadeCurve,
}

impl OpParams for FadeParams {
    fn schema() -> ParamSchema {
        let frames = || ParamKind::Int {
            min: 0,
            max: MAX_FRAME_PARAM,
            step: 1,
        };
        ParamSchema {
            params: vec![
                spec("fadeIn", frames(), Unit::Samples, json!(0)),
                spec("fadeOut", frames(), Unit::Samples, json!(0)),
                spec(
                    "curve",
                    ParamKind::Enum {
                        values: &["linear", "equalPower"],
                    },
                    Unit::None,
                    json!("linear"),
                ),
            ],
        }
    }
}

/// フェード。
pub struct Fade;

fn too_long() -> Issues {
    bad_param("fadeIn", "フェードが音声より長いです")
}

/// フェードの核。
struct FadeKernel {
    audio: Arc<Audio>,
    fade_in: u64,
    fade_out: u64,
    curve: FadeCurve,
}

impl FadeKernel {
    fn curve(&self, t: f64) -> f64 {
        match self.curve {
            FadeCurve::Linear => t,
            FadeCurve::EqualPower => math::sin(t * std::f64::consts::FRAC_PI_2),
        }
    }

    /// フレーム i の倍率。
    fn gain(&self, i: u64) -> f64 {
        let n = self.audio.frames() as u64;
        let mut g = 1.0;
        if i < self.fade_in {
            g *= self.curve(i as f64 / self.fade_in as f64);
        }
        if self.fade_out > 0 && i >= n - self.fade_out {
            g *= self.curve((n - 1 - i) as f64 / self.fade_out as f64);
        }
        g
    }
}

impl AudioKernel for FadeKernel {
    fn rate(&self) -> u32 {
        self.audio.rate()
    }

    fn channels(&self) -> usize {
        self.audio.channels()
    }

    fn frames(&self) -> u64 {
        self.audio.frames() as u64
    }

    fn render(&self, ch: usize, start: u64, out: &mut [f32]) {
        copy_from(&self.audio.planes()[ch], start, out);
        for (k, s) in out.iter_mut().enumerate() {
            let g = self.gain(start + k as u64);
            if g != 1.0 {
                *s = (f64::from(*s) * g) as f32;
            }
        }
    }
}

fn kernel(p: &FadeParams, inputs: &[&Value]) -> Result<FadeKernel, EngineError> {
    let audio = audio_of(inputs[0], Fade::ID)?;
    let n = audio.frames() as u64;
    if p.fade_in > n || p.fade_out > n {
        return Err(EngineError::Invalid(too_long()));
    }
    Ok(FadeKernel {
        audio: audio.clone(),
        fade_in: p.fade_in,
        fade_out: p.fade_out,
        curve: p.curve,
    })
}

impl Op for Fade {
    const ID: &'static str = "audio.fade";
    const VERSION: u16 = 1;
    type Params = FadeParams;

    fn signature() -> Signature {
        Signature {
            inputs: &[PortSpec {
                name: "audio",
                accepts: &[ValueKind::Audio],
            }],
            lift: Lift::None,
            numeric: Numeric::Float,
            meta: MetaAccess::Map,
        }
    }

    fn infer(p: &FadeParams, inputs: &[Shape], _cx: &InferCx<'_>) -> Result<Inferred, Issues> {
        expect_inputs(inputs, &Self::signature())?;
        let Some((_, channels, frames)) = audio_shape(&inputs[0]) else {
            return Err(Issues::none());
        };
        // 長さが分からないときは上限で確かめる（実際は実行で確かめる）
        let n = match frames {
            Dim::Exact(n) | Dim::AtMost(n) => n,
        };
        if p.fade_in > n || p.fade_out > n {
            return Err(too_long());
        }
        Ok(Inferred {
            shape: inputs[0],
            space: SpaceMap::Identity,
            time: TimeMap::Identity,
            cost: frames.max().saturating_mul(u64::from(channels)),
        })
    }

    fn numeric_for(p: &FadeParams) -> Numeric {
        if p.fade_in == 0 && p.fade_out == 0 {
            Numeric::Exact
        } else {
            Numeric::Float
        }
    }

    fn apply(
        p: &FadeParams,
        inputs: &[&Value],
        cx: &mut ApplyCx<'_>,
    ) -> Result<Applied, EngineError> {
        cx.check()?;
        let k = kernel(p, inputs)?;
        if p.fade_in == 0 && p.fade_out == 0 {
            // 何も変えない（同じ実体を返す。E17）
            return Ok(Applied::value(Value::Audio(k.audio)));
        }
        let meta = k.audio.meta.clone();
        Ok(Applied::value(render_all(&k, meta, cx.mon)?))
    }

    fn work_bytes(_p: &Self::Params, _inputs: &[Shape], out: &Shape) -> crate::op::Work {
        crate::audio::render_all_work(out)
    }

    fn audio_kernel(
        p: &FadeParams,
        inputs: &[&Value],
        _cx: &mut ApplyCx<'_>,
    ) -> Option<Result<Box<dyn AudioKernel>, EngineError>> {
        Some(kernel(p, inputs).map(|k| Box::new(k) as Box<dyn AudioKernel>))
    }

    fn examples() -> Vec<Example<FadeParams>> {
        let noise = |seed| InputGen::AudioNoise {
            rate: 44_100,
            channels: 2,
            frames: 9_000,
            seed,
        };
        vec![
            Example {
                name: "linear-in-and-out",
                params: FadeParams {
                    fade_in: 2_000,
                    fade_out: 3_000,
                    curve: FadeCurve::Linear,
                },
                inputs: vec![noise(41)],
            },
            Example {
                name: "equal-power-overlapping",
                params: FadeParams {
                    fade_in: 6_000,
                    fade_out: 6_000,
                    curve: FadeCurve::EqualPower,
                },
                inputs: vec![noise(42)],
            },
            Example {
                name: "zero-is-identity",
                params: FadeParams {
                    fade_in: 0,
                    fade_out: 0,
                    curve: FadeCurve::EqualPower,
                },
                inputs: vec![noise(43)],
            },
        ]
    }

    // 段 10：範囲（同じ位置）と、引数を変えたとき・外すときの範囲（フェードの区間）
    fn reach(_p: &FadeParams, _inputs: &[Shape]) -> Reach {
        Reach::SameTime
    }

    fn param_change(old: &FadeParams, new: &FadeParams, inputs: &[Shape]) -> Area {
        // 倍率はフェードの区間の外で 1 なので、新旧の区間の和だけが変わりうる（曲線を変えたら両方の区間）
        let Some(n) = inputs
            .first()
            .and_then(audio_shape)
            .and_then(|(_, _, f)| f.exact())
        else {
            return Area::All;
        };
        let curve = old.curve != new.curve;
        let mut area = Area::Empty;
        if curve || old.fade_in != new.fade_in {
            area.union_with(&Area::span(0, old.fade_in.max(new.fade_in).min(n)));
        }
        if curve || old.fade_out != new.fade_out {
            let len = old.fade_out.max(new.fade_out).min(n);
            area.union_with(&Area::span(n - len, n));
        }
        area
    }

    fn effect(p: &FadeParams, inputs: &[Shape]) -> Area {
        let Some(n) = inputs
            .first()
            .and_then(audio_shape)
            .and_then(|(_, _, f)| f.exact())
        else {
            return Area::All;
        };
        Area::span(0, p.fade_in.min(n)).union(&Area::span(n - p.fade_out.min(n), n))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::render_value;

    #[test]
    fn ends_are_silent_and_middle_is_kept() {
        let input = Fade::examples()[0].inputs[0].generate().unwrap();
        let out = render_value(
            Fade::ID,
            1,
            json!({ "fadeIn": 2_000, "fadeOut": 3_000, "curve": "linear" }),
            input.clone(),
        )
        .unwrap();
        let (Value::Audio(a), Value::Audio(src)) = (&out, &input) else {
            panic!("音声ではありません")
        };
        for ch in 0..2 {
            assert_eq!(a.planes()[ch][0], 0.0);
            assert_eq!(a.planes()[ch][8_999], 0.0);
            assert_eq!(a.planes()[ch][2_000..6_000], src.planes()[ch][2_000..6_000]);
            // フェードインの真ん中は半分
            let half = (f64::from(src.planes()[ch][1_000]) * 0.5) as f32;
            assert_eq!(a.planes()[ch][1_000], half);
        }
        assert!(
            render_value(
                Fade::ID,
                1,
                json!({ "fadeIn": 9_001, "fadeOut": 0, "curve": "linear" }),
                input
            )
            .is_err()
        );
    }
}
