//! 操作の一覧（docs/editing.md §7.3）。
//!
//! 操作を足すときは、`<分類>/<名前>.rs` を作って `Op` を実装し、下の `register_ops!` に 1 行足す。
//! 結果を変えた旧版は `legacy` に入れる（新しく作れないが、保存済みの文書の作り直しに使う。§8.4）。
//! 操作をなくすときは、今の版を `current` から `retired` に移す（カタログに出さないが、作り直しに使う）。

pub mod audio;
pub mod image;
pub mod meta;
pub mod paint;
pub mod palette;
pub mod sprite;
pub mod util;

use crate::op::Registry;

/// 今の版の操作と旧版から、登録表を作る関数 `registry()` を定義する。
macro_rules! register_ops {
    (current: [$($op:ty),* $(,)?], legacy: [$($old:ty),* $(,)?], retired: [$($gone:ty),* $(,)?] $(,)?) => {
        /// すべての操作の登録表。同じ操作・版が 2 つあればエラー（テストで確かめる）。
        pub fn registry() -> Result<Registry, String> {
            let mut r = Registry::empty();
            $( r.add_current::<$op>()?; )*
            $( r.add_legacy::<$old>()?; )*
            $( r.add_retired::<$gone>()?; )*
            Ok(r)
        }
    };
}

register_ops! {
    current: [
        image::crop::Crop,
        image::flip::Flip,
        image::canvas::Canvas,
        image::pad::Pad,
        image::fit_canvas::FitCanvas,
        image::trim_alpha::TrimAlpha,
        image::rotate90::Rotate90,
        image::resize::Resize,
        image::convert::Convert,
        palette::replace::Replace,
        sprite::split_grid::SplitGrid,
        sprite::pack_grid::PackGrid,
        meta::set_grid::SetGrid,
        meta::set_pivot::SetPivot,
        meta::anchor_pivot::AnchorPivot,
        paint::actions::Actions,
        audio::gain::Gain,
        audio::trim::Trim,
        audio::fade::Fade,
        audio::mono::Mono,
        audio::set_loop::SetLoop,
        audio::normalize::Normalize,
        audio::resample::Resample,
        audio::eq::Eq,
    ],
    legacy: [],
    // なくした操作の最後の版（新しく作れないが、作り直しに使う。§8.4）
    retired: [],
}

#[cfg(test)]
mod tests {
    use crate::limits::Limits;
    use crate::op::{InferCx, Lift, Work};

    /// 作業領域の見積もり（`Op::work_bytes`）は登録表から引け、値の数倍を持つ操作（Indexed8 への convert・平滑な resize・
    /// `render_all` で描く音声・塗りつぶしの集合と待ち行列とストロークの集合を持つ paint.actions）だけが 0 でない。
    #[test]
    fn work_bytes_are_declared_where_values_are_multiplied() {
        let reg = super::registry().unwrap();
        let limits = Limits::default();
        let cx = InferCx { limits: &limits };
        let mut nonzero = Vec::new();
        for op in reg.current() {
            for ex in op.examples().unwrap() {
                let mut shapes: Vec<_> = ex
                    .inputs
                    .iter()
                    .map(|g| g.generate().unwrap().shape())
                    .collect();
                if op.signature().lift == Lift::PerFrame
                    && let Some(frame) = shapes[0].frame_image()
                {
                    shapes[0] = frame;
                }
                let p = op.parse(&ex.params).unwrap();
                let Ok(inferred) = op.infer(p.as_ref(), &shapes, &cx) else {
                    continue;
                };
                let w = op.work_bytes(p.as_ref(), &shapes, &inferred.shape).unwrap();
                if w != Work::ZERO && nonzero.last() != Some(&op.id()) {
                    nonzero.push(op.id());
                }
            }
        }
        assert_eq!(
            nonzero,
            [
                "audio.eq",
                "audio.fade",
                "audio.gain",
                "audio.mono",
                "audio.normalize",
                "audio.resample",
                "audio.trim",
                "image.convert",
                "image.resize",
                "paint.actions",
            ]
        );
    }

    /// 本物の登録表：同じ ID の旧版の版は今の版より小さく（E52）、どの実装の版も `Registry::status` で今の版・旧版・
    /// 退役した版のどれかに見分けられ、使う実装はその版そのもの。
    #[test]
    fn legacy_versions_are_below_the_current_one() {
        use crate::op::OpStatus;
        let reg = super::registry().unwrap();
        let all = reg.all();
        assert!(!all.is_empty());
        for op in all {
            let (id, v) = (op.id(), op.version());
            match reg.current_version(id) {
                Some(current) => {
                    assert!(v <= current, "{id} v{v} は今の版 v{current} より上です");
                    let want = if v == current {
                        OpStatus::Current
                    } else {
                        OpStatus::Legacy { uses: v, current }
                    };
                    assert_eq!(reg.status(id, v), Ok(want), "{id} v{v}");
                }
                None => assert_eq!(reg.status(id, v), Ok(OpStatus::Retired { uses: v })),
            }
            let newest = reg.versions(id).last().copied().unwrap();
            assert!(
                matches!(reg.status(id, newest + 1), Err(i) if i.code == crate::IssueCode::UnknownVersion),
                "{id}"
            );
        }
    }

    #[test]
    fn work_totals_saturate() {
        let w = Work {
            shared: 10,
            per_task: 3,
        };
        assert_eq!(w.total(0), 10);
        assert_eq!(w.total(4), 22);
        assert_eq!(w.total(u64::MAX), u64::MAX);
        assert_eq!(
            Work::shared(5),
            Work {
                shared: 5,
                per_task: 0
            }
        );
        assert_eq!(Work::default(), Work::ZERO);
    }
}
