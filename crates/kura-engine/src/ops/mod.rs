//! 操作の一覧（docs/editing.md §7.3）。
//!
//! 操作を足すときは、`<分類>/<名前>.rs` を作って `Op` を実装し、下の `register_ops!` に 1 行足す。
//! 結果を変えた旧版は `legacy` に入れる（新しく作れないが、保存済みの文書の作り直しに使う。§8.4）。
//! 操作をなくすときは、今の版を `current` から `retired` に移す（カタログに出さないが、作り直しに使う）。

pub mod audio;
pub mod image;

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
        audio::gain::Gain,
    ],
    legacy: [],
    // なくした操作の最後の版（新しく作れないが、作り直しに使う。§8.4）
    retired: [],
}
