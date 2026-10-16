//! `palette.replace` v1：パレットの色を置き換える（Indexed8 だけ。画素の番号は変えない）。
//!
//! 番号はパレットの色の数より小さいこと（推論ではパレットが分からないので、実行で確かめる）。同じ番号を 2 回は書けない。
//! フレームの列は全フレームが 1 つのパレットを共有しているので、全フレームで同じように置き換える。

use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::error::{EngineError, Issues};
use crate::mapping::{SpaceMap, TimeMap};
use crate::op::{
    Applied, ApplyCx, Example, InferCx, Inferred, Lift, MetaAccess, Numeric, Op, OpParams,
    ParamKind, ParamSchema, PortSpec, Signature, Unit, expect_inputs, spec,
};
use crate::ops::util::{bad_input, bad_param, map_rasters};
use crate::shape::{Shape, ValueKind};
use crate::testgen::InputGen;
use crate::value::{Palette, PixelFormat, Raster, Value};

/// 置き換える色 1 つ。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PaletteEntry {
    pub index: u8,
    pub color: [u8; 4],
}

/// 引数。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReplaceParams {
    pub entries: Vec<PaletteEntry>,
}

impl OpParams for ReplaceParams {
    fn schema() -> ParamSchema {
        ParamSchema {
            params: vec![spec(
                "entries",
                ParamKind::PaletteEntries,
                Unit::None,
                json!([]),
            )],
        }
    }
}

/// パレットの置き換え。
pub struct Replace;

/// 置き換えたパレット。
pub fn replaced(p: &Palette, entries: &[PaletteEntry]) -> Result<Palette, EngineError> {
    let mut out = p.clone();
    for e in entries {
        let slot = out.colors.get_mut(usize::from(e.index)).ok_or_else(|| {
            EngineError::Invalid(bad_param(
                "entries",
                format!(
                    "番号 {} はパレットにありません（{} 色）",
                    e.index,
                    p.colors.len()
                ),
            ))
        })?;
        *slot = e.color;
    }
    Ok(out)
}

impl Op for Replace {
    const ID: &'static str = "palette.replace";
    const VERSION: u16 = 1;
    type Params = ReplaceParams;

    fn signature() -> Signature {
        Signature {
            inputs: &[PortSpec {
                name: "image",
                accepts: &[ValueKind::Image, ValueKind::Frames],
            }],
            lift: Lift::None,
            numeric: Numeric::Exact,
            meta: MetaAccess::Map,
        }
    }

    fn infer(p: &ReplaceParams, inputs: &[Shape], _cx: &InferCx<'_>) -> Result<Inferred, Issues> {
        expect_inputs(inputs, &Self::signature())?;
        let (fmt, cost) = match inputs[0] {
            Shape::Image { fmt, .. } => (fmt, 1),
            Shape::Frames { fmt, count, .. } => (fmt, count.max()),
            Shape::Audio { .. } => return Err(Issues::none()),
        };
        if fmt != PixelFormat::Indexed8 {
            return Err(bad_input(
                "パレットの置き換えはパレットの画像（Indexed8）だけに使えます",
            ));
        }
        let mut seen = [false; 256];
        for e in &p.entries {
            if std::mem::replace(&mut seen[usize::from(e.index)], true) {
                return Err(bad_param(
                    "entries",
                    format!("番号 {} が 2 回あります", e.index),
                ));
            }
        }
        Ok(Inferred {
            shape: inputs[0],
            space: SpaceMap::Identity,
            time: TimeMap::Identity,
            cost,
        })
    }

    fn apply(
        p: &ReplaceParams,
        inputs: &[&Value],
        cx: &mut ApplyCx<'_>,
    ) -> Result<Applied, EngineError> {
        cx.check()?;
        let mon = cx.mon;
        let value = map_rasters(inputs[0], mon, |_, px| {
            let pal = px
                .palette()
                .ok_or_else(|| EngineError::Internal("パレットがありません".into()))?;
            let new = replaced(pal, &p.entries)?;
            let rows = px.to_rows()?;
            Raster::from_rows(
                px.width(),
                px.height(),
                PixelFormat::Indexed8,
                Some(new),
                &rows,
            )
        })?;
        Ok(Applied::value(value))
    }

    fn examples() -> Vec<Example<ReplaceParams>> {
        vec![
            Example {
                name: "two-colors",
                params: ReplaceParams {
                    entries: vec![
                        PaletteEntry {
                            index: 0,
                            color: [255, 0, 255, 255],
                        },
                        PaletteEntry {
                            index: 15,
                            color: [1, 2, 3, 0],
                        },
                    ],
                },
                inputs: vec![InputGen::Noise {
                    w: 300,
                    h: 20,
                    fmt: PixelFormat::Indexed8,
                    seed: 61,
                }],
            },
            Example {
                name: "frames-share-the-palette",
                params: ReplaceParams {
                    entries: vec![PaletteEntry {
                        index: 3,
                        color: [9, 9, 9, 255],
                    }],
                },
                inputs: vec![InputGen::NoiseFrames {
                    w: 5,
                    h: 4,
                    fmt: PixelFormat::Indexed8,
                    count: 2,
                    seed: 62,
                }],
            },
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::render_single;

    #[test]
    fn checks_formats_indexes_and_duplicates() {
        let run = |fmt, entries: Vec<PaletteEntry>| {
            render_single(
                Replace::ID,
                1,
                serde_json::to_value(ReplaceParams { entries }).unwrap(),
                &[InputGen::Noise {
                    w: 3,
                    h: 3,
                    fmt,
                    seed: 1,
                }],
            )
        };
        let e = |index| PaletteEntry {
            index,
            color: [0; 4],
        };
        assert!(run(PixelFormat::Rgba8, vec![e(0)]).is_err());
        assert!(run(PixelFormat::Indexed8, vec![e(1), e(1)]).is_err());
        // 16 色のパレットに 16 番はない
        assert!(run(PixelFormat::Indexed8, vec![e(16)]).is_err());
        let Value::Image(img) = run(PixelFormat::Indexed8, vec![e(15)]).unwrap() else {
            panic!()
        };
        assert_eq!(img.px.palette().unwrap().colors[15], [0; 4]);
    }
}
