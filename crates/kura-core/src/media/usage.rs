//! 中身から作ったキャッシュの容量（設定の「容量」の行。M11 の決定 4 で ADR-122 を広げる）。
//!
//! 透明のある画像のサムネイルを PNG にしたので（決定 4）、サムネイルが今までの JPEG の 10〜15 倍になりうる。利用者が
//! 置き場の大きさを知れるように、サムネイル（`thumbs/` と音楽のジャケット `art/`）・波形の段（`peaks/`）・アニメの帯（`anim/`）を
//! 別々に数える。ファイルを数えるので、ライブラリの容量（台帳の合計）とは別のコマンドで、設定の画面を開いたときにだけ呼ぶ。

use std::path::Path;

use crate::Result;
use crate::library::Layout;

/// 1 つのキャッシュの大きさ。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DirUsage {
    /// ファイルの大きさの合計（バイト）。
    pub bytes: u64,
    /// ファイルの数。
    pub files: u64,
}

impl DirUsage {
    fn add(&mut self, other: DirUsage) {
        self.bytes = self.bytes.saturating_add(other.bytes);
        self.files = self.files.saturating_add(other.files);
    }
}

/// キャッシュの大きさ（ゴミ箱のアイテムの分も含む。ゴミ箱を空にすると消える）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CacheUsage {
    /// サムネイル（`thumbs/` の s・l の JPEG・PNG）と音楽のジャケット（`art/`）。
    pub thumbs: DirUsage,
    /// 音声の波形の段（`peaks/`）。
    pub peaks: DirUsage,
    /// アニメの帯（`anim/`）。
    pub anim: DirUsage,
}

/// ライブラリのキャッシュの大きさを数える（ファイルを走査するので `spawn_blocking` で呼ぶ）。
pub fn cache_usage(layout: &Layout) -> Result<CacheUsage> {
    let root = layout.root();
    let mut thumbs = dir_usage(&root.join("thumbs"))?;
    thumbs.add(dir_usage(&root.join("art"))?);
    Ok(CacheUsage {
        thumbs,
        peaks: dir_usage(&root.join("peaks"))?,
        anim: dir_usage(&root.join("anim"))?,
    })
}

/// フォルダの下のファイルの大きさの合計（再帰しない積み上げでたどる。シンボリックリンクはたどらない。フォルダがなければ 0）。
fn dir_usage(dir: &Path) -> Result<DirUsage> {
    let mut out = DirUsage::default();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
            Err(err) => return Err(err.into()),
        };
        for entry in entries {
            let entry = entry?;
            let kind = entry.file_type()?;
            if kind.is_dir() {
                stack.push(entry.path());
            } else if kind.is_file() {
                // 数えている間に消えたもの（作り直し・ゴミ箱を空にした）は数えない
                if let Ok(meta) = entry.metadata() {
                    out.bytes = out.bytes.saturating_add(meta.len());
                    out.files += 1;
                }
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::{ThumbFormat, ThumbSize};

    /// サムネイル（jpg・png とジャケット）・波形・アニメを別々に数える。ないフォルダは 0。ほかのフォルダ（files・proxies）は
    /// 数えない。
    #[test]
    fn counts_thumbs_peaks_and_anim_separately() {
        let dir = tempfile::tempdir().unwrap();
        let layout = Layout::new(dir.path());
        assert_eq!(cache_usage(&layout).unwrap(), CacheUsage::default());
        let write = |path: std::path::PathBuf, len: usize| {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, vec![0u8; len]).unwrap();
        };
        let a = "ab".repeat(32);
        let b = "cd".repeat(32);
        write(
            layout.thumb_path_as(&a, ThumbSize::Small, ThumbFormat::Png),
            100,
        );
        write(
            layout.thumb_path_as(&a, ThumbSize::Large, ThumbFormat::Png),
            1000,
        );
        write(layout.thumb_path(&b, ThumbSize::Small), 10);
        write(layout.art_path(&b), 7);
        write(layout.peaks_path(&a), 33);
        write(layout.anim_path(&a), 400);
        write(layout.anim_path(&b), 5);
        write(layout.proxy_path(&a), 9999);
        write(layout.content_path(&[1u8; 32], "png"), 9999);
        assert_eq!(
            cache_usage(&layout).unwrap(),
            CacheUsage {
                thumbs: DirUsage {
                    bytes: 1117,
                    files: 4
                },
                peaks: DirUsage {
                    bytes: 33,
                    files: 1
                },
                anim: DirUsage {
                    bytes: 405,
                    files: 2
                },
            }
        );
    }
}
