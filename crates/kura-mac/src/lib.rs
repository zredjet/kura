//! Kura の macOS 専用機能。Swift ヘルパー（`swift/`）を swift-rs 経由で呼ぶ。
//!
//! macOS 以外では同じ関数が既定値を返すだけの空の実装になり、ワークスペース全体がビルドできる。

#[cfg(target_os = "macos")]
mod ffi {
    use swift_rs::{Bool, swift};

    swift!(pub(crate) fn kura_reduce_transparency() -> Bool);
}

/// 「透明度を下げる」がオンか。macOS 以外では常に false。
pub fn reduce_transparency() -> bool {
    #[cfg(target_os = "macos")]
    {
        // SAFETY: 引数を取らず Bool を返すだけの Swift 関数（Appearance.swift）。
        unsafe { ffi::kura_reduce_transparency() }
    }
    #[cfg(not(target_os = "macos"))]
    {
        false
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn reduce_transparency_is_callable() {
        // 値は実行環境の設定次第。Swift ヘルパーがリンクされて呼べることだけを確かめる。
        let _ = super::reduce_transparency();
    }
}
