//! ファイルの配置。staging にコピー（同じボリュームなら APFS クローン）→ コピー側のハッシュ → `files/` へ rename。
//!
//! 元のファイルは読むだけ。コピー側でハッシュを取るので、取り込み中に元のファイルが変わっても
//! ファイル名（ハッシュ）と中身が必ず一致する。

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use crate::Result;

/// 重複の見込みに使う、先頭と末尾の大きさ。
pub const PARTIAL_CHUNK: u64 = 64 * 1024;

/// これ以上の大きさのファイルは、まとめて読んで並列にハッシュを取る。
const PARALLEL_THRESHOLD: u64 = 1024 * 1024;

/// 大きいファイルを読む単位。mmap（ページフォールトで少しずつ読む）より、まとめて read する方が
/// ディスクの速度に近づく（M9 の実測で 1GB のファイルが 1.2 → 5.2 GB/s。ADR-113）。
const READ_CHUNK: u64 = 8 * 1024 * 1024;

/// staging に置いたコピー。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Staged {
    pub path: PathBuf,
    /// 内容の BLAKE3。
    pub hash: [u8; 32],
    pub size: u64,
}

/// 配置の結果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Placed {
    /// 新しく置いた。
    New,
    /// 同じ中身がすでにあった（staging のコピーは消した）。
    AlreadyPresent,
}

/// `src` を `staging_path` にコピーし、コピーした側のハッシュとサイズを返す。
pub fn stage(src: &Path, staging_path: &Path) -> Result<Staged> {
    if let Some(parent) = staging_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    remove_if_exists(staging_path)?;
    let staged = (|| {
        reflink_copy::reflink_or_copy(src, staging_path)?;
        let size = std::fs::metadata(staging_path)?.len();
        let hash = hash_file(staging_path)?;
        Ok(Staged {
            path: staging_path.to_path_buf(),
            hash,
            size,
        })
    })();
    if staged.is_err() {
        // 書きかけは消す（残っても次に開いたときの掃除で消える）
        let _ = remove_if_exists(staging_path);
    }
    staged
}

/// staging のコピーを `final_path` に置く。同じものがすでにあればコピーを消す。
pub fn place(staged: &Staged, final_path: &Path) -> Result<Placed> {
    if final_path.exists() {
        remove_if_exists(&staged.path)?;
        return Ok(Placed::AlreadyPresent);
    }
    if let Some(parent) = final_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // 同じボリューム内の rename なので原子的に置かれる
    std::fs::rename(&staged.path, final_path)?;
    Ok(Placed::New)
}

/// ファイル全体の BLAKE3。
pub fn hash_file(path: &Path) -> Result<[u8; 32]> {
    let mut hasher = blake3::Hasher::new();
    let mut file = File::open(path)?;
    let size = file.metadata()?.len();
    // 小さいファイルも 1 回で読む（8KB ずつ読むと、キャッシュにないファイルで倍ほど遅い）
    let mut buf = vec![0u8; usize::try_from(size.clamp(1, READ_CHUNK)).unwrap_or(1)];
    loop {
        let filled = read_full(&mut file, &mut buf)?;
        if filled == 0 {
            break;
        }
        if size < PARALLEL_THRESHOLD {
            // 小さいファイルはスレッド分割の手間の方が大きい
            hasher.update(&buf[..filled]);
        } else {
            hasher.update_rayon(&buf[..filled]);
        }
    }
    Ok(*hasher.finalize().as_bytes())
}

/// `buf` が埋まるか終わりまで読む（読めた大きさ）。
fn read_full(file: &mut File, buf: &mut [u8]) -> Result<usize> {
    let mut filled = 0;
    while filled < buf.len() {
        match file.read(&mut buf[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => {}
            Err(err) => return Err(err.into()),
        }
    }
    Ok(filled)
}

/// 重複の見込み用のハッシュ（先頭と末尾 64KB。小さいファイルは全体）。
pub fn partial_hash(path: &Path) -> Result<[u8; 32]> {
    let mut file = File::open(path)?;
    let size = file.metadata()?.len();
    let mut hasher = blake3::Hasher::new();
    if size <= PARTIAL_CHUNK * 2 {
        std::io::copy(&mut file, &mut hasher)?;
    } else {
        let mut buf = vec![0u8; usize::try_from(PARTIAL_CHUNK).unwrap_or(65_536)];
        file.read_exact(&mut buf)?;
        hasher.update(&buf);
        file.seek(SeekFrom::End(
            -i64::try_from(PARTIAL_CHUNK).unwrap_or(65_536),
        ))?;
        file.read_exact(&mut buf)?;
        hasher.update(&buf);
    }
    Ok(*hasher.finalize().as_bytes())
}

fn remove_if_exists(path: &Path) -> Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot(path: &Path) -> (Vec<u8>, std::time::SystemTime) {
        (
            std::fs::read(path).unwrap(),
            std::fs::metadata(path).unwrap().modified().unwrap(),
        )
    }

    #[test]
    fn stage_copies_and_hashes_the_copy_without_touching_source() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src.jpg");
        std::fs::write(&src, b"hello kura").unwrap();
        let before = snapshot(&src);

        let staged = stage(&src, &dir.path().join("staging/import-1-0.jpg")).unwrap();
        assert_eq!(staged.hash, *blake3::hash(b"hello kura").as_bytes());
        assert_eq!(staged.size, 10);
        assert_eq!(std::fs::read(&staged.path).unwrap(), b"hello kura");
        assert_eq!(snapshot(&src), before);
    }

    #[test]
    fn place_moves_new_files_and_drops_copies_of_existing_ones() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src.jpg");
        std::fs::write(&src, b"content").unwrap();
        let final_path = dir.path().join("files/ab/cd/x.jpg");

        let first = stage(&src, &dir.path().join("staging/a")).unwrap();
        assert_eq!(place(&first, &final_path).unwrap(), Placed::New);
        assert_eq!(std::fs::read(&final_path).unwrap(), b"content");
        assert!(!first.path.exists());

        let second = stage(&src, &dir.path().join("staging/b")).unwrap();
        let placed_before = snapshot(&final_path);
        assert_eq!(place(&second, &final_path).unwrap(), Placed::AlreadyPresent);
        assert!(!second.path.exists());
        assert_eq!(snapshot(&final_path), placed_before);
    }

    #[test]
    fn stage_overwrites_leftover_staging_file() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src.jpg");
        std::fs::write(&src, b"new").unwrap();
        let staging = dir.path().join("staging/a");
        std::fs::create_dir_all(staging.parent().unwrap()).unwrap();
        std::fs::write(&staging, b"old leftover").unwrap();
        let staged = stage(&src, &staging).unwrap();
        assert_eq!(std::fs::read(&staged.path).unwrap(), b"new");
    }

    #[test]
    fn hash_file_matches_blake3_across_read_chunks() {
        let dir = tempfile::tempdir().unwrap();
        // 空、1MB 未満（1 回で読む）、ちょうど 1 単位、単位の境目をまたぐ大きさ
        for size in [
            0,
            10_000usize,
            READ_CHUNK as usize,
            READ_CHUNK as usize * 2 + 12_345,
        ] {
            let data: Vec<u8> = (0..size).map(|i| (i * 31 % 251) as u8).collect();
            let path = dir.path().join(format!("{size}.bin"));
            std::fs::write(&path, &data).unwrap();
            assert_eq!(
                hash_file(&path).unwrap(),
                *blake3::hash(&data).as_bytes(),
                "{size}"
            );
        }
    }

    #[test]
    fn partial_hash_sees_only_head_and_tail() {
        let dir = tempfile::tempdir().unwrap();
        let size = (PARTIAL_CHUNK * 3) as usize;
        let mut a = vec![1u8; size];
        let mut b = a.clone();
        b[size / 2] = 9; // 真ん中だけ違う
        let mut c = a.clone();
        c[size - 1] = 9; // 末尾が違う
        a[0] = 1;
        let (pa, pb, pc) = (
            dir.path().join("a"),
            dir.path().join("b"),
            dir.path().join("c"),
        );
        std::fs::write(&pa, &a).unwrap();
        std::fs::write(&pb, &b).unwrap();
        std::fs::write(&pc, &c).unwrap();
        assert_eq!(partial_hash(&pa).unwrap(), partial_hash(&pb).unwrap());
        assert_ne!(partial_hash(&pa).unwrap(), partial_hash(&pc).unwrap());

        let small = dir.path().join("small");
        std::fs::write(&small, b"tiny").unwrap();
        assert_eq!(
            partial_hash(&small).unwrap(),
            *blake3::hash(b"tiny").as_bytes()
        );
        assert_eq!(
            hash_file(&small).unwrap(),
            *blake3::hash(b"tiny").as_bytes()
        );
    }
}
