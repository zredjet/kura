//! ライブラリのロック（`<ライブラリ>/library.lock`）。同じライブラリを 2 つのアプリ（プロセス）で同時に開くのを断る
//! （ADR-156 (6)・M11 の検証のあとの決定 22。docs/data-model.md §1）。
//!
//! - 開くときは、DB を開く・当てる前のバックアップ・マイグレーション・staging の掃除・知らせより前に、`library.lock` を開いて
//!   （なければ作って）排他のロックを取る（`File::try_lock`。macOS・Linux とも `flock(LOCK_EX | LOCK_NB)`）。ほかが持って
//!   いれば `CoreError::LibraryInUse` で断り、ライブラリには何もしない。DB のファイル自体には掛けない（SQLite の fcntl の
//!   ロックと干渉しうる）。
//! - flock は開いたファイル（open file description）ごとなので、同じプロセスでも別々に開けばぶつかる。std はファイルを
//!   `O_CLOEXEC` で開くので、ジョブが起動する子プロセス（ffmpeg・SVG の働き手など）には継がれない。
//! - 閉じるときは DB を閉じてから明示的に放す（[`super::Library::close`]）。IPC が `Arc<Library>` を握っていて drop が
//!   遅れても、開き直し・ほかのアプリが自分のロックに当たらない。プロセスが落ちれば OS が放す。
//! - ロックファイルは消さない（消すと、消す前に開いた側と、あとで作り直した側が別々のファイルでロックを取れてしまう）。
//! - flock が使えないファイルシステム（ENOTSUP・ENOLCK など。一部のネットワークのボリューム）では、警告を残してロックなしで開く
//!   （利用者の決定）。そのときに 2 つのアプリで開くと、マイグレーションの読み直し（docs/data-model.md §5 の 3）だけが守る。

use std::fs::{File, OpenOptions, TryLockError};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

use crate::{CoreError, Result};

/// ロックファイルの名前（ライブラリのフォルダの直下）。
pub const LOCK_FILE: &str = "library.lock";

/// macOS の ENOTSUP（std は EOPNOTSUPP を `ErrorKind::Unsupported` にするが、macOS の ENOTSUP は別の値で入らない。Linux では
/// ENOTSUP = EOPNOTSUPP）。
const ENOTSUP_MACOS: i32 = 45;
/// ENOLCK（ロックを扱えないネットワークのボリューム。macOS 77・Linux 37）。flock はどちらの OS でも、ほかの値を返さない。
const ENOLCK: [i32; 2] = [77, 37];

/// 開いているライブラリのロック。持っている間、ほかのアプリ（と、同じプロセスで別に開いたもの）は同じライブラリを開けない。
/// [`super::Library::close`] か drop で放す。
#[derive(Debug)]
pub struct LibraryLock {
    root: PathBuf,
    /// ロックファイルの識別（デバイスと i ノード）。同じライブラリかを、場所の書き方（シンボリックリンク・大文字小文字）に
    /// よらずに比べる。
    id: (u64, u64),
    /// ロックを取ったファイル（放したら None。flock が使えないファイルシステムでは初めから None）。
    file: Mutex<Option<File>>,
}

impl LibraryLock {
    /// `root`（ライブラリのフォルダ。あること）の `library.lock` を開き（なければ作り）、排他のロックを取る。ほかが持って
    /// いれば `CoreError::LibraryInUse`。flock が使えないファイルシステムでは、警告を残してロックなしで返す。
    pub fn acquire(root: &Path) -> Result<Self> {
        let (path, file, id) = open_file(root)?;
        let held = match file.try_lock() {
            Ok(()) => Some(file),
            Err(TryLockError::WouldBlock) => return Err(CoreError::LibraryInUse),
            Err(TryLockError::Error(err)) if lock_unsupported(&err) => {
                tracing::warn!(
                    %err,
                    path = %path.display(),
                    "ロックを取れないファイルシステムなので、ロックなしでライブラリを開きます（ほかのアプリが同時に開いても断れません）"
                );
                None
            }
            Err(TryLockError::Error(err)) => return Err(err.into()),
        };
        Ok(Self {
            root: root.to_path_buf(),
            id,
            file: Mutex::new(held),
        })
    }

    /// ロックを取らずに作る（テスト用：flock が使えないファイルシステムで開いたときと同じ）。
    #[cfg(test)]
    pub(crate) fn unlocked(root: &Path) -> Result<Self> {
        let (_, _, id) = open_file(root)?;
        Ok(Self {
            root: root.to_path_buf(),
            id,
            file: Mutex::new(None),
        })
    }

    /// 落ちたときと同じく、ロックを外さずにファイルを閉じる（テスト用。flock は開いたファイルごとなので、ファイルを継いだ
    /// プロセスがなければ OS が外す）。
    #[cfg(test)]
    pub(crate) fn abandon(&self) {
        drop(
            self.file
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .take(),
        );
    }

    /// ライブラリのフォルダ（取ったときに渡した場所）。
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// ロックを持っているか（放したあと・flock が使えないファイルシステムでは false）。
    pub fn is_held(&self) -> bool {
        self.file
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .is_some()
    }

    /// `root` がこのロックのライブラリか（`root` の `library.lock` が、取ったときのファイルと同じか）。場所の書き方が
    /// 違っても（シンボリックリンク・大文字小文字・`..`）同じフォルダなら true。
    pub fn is_for(&self, root: &Path) -> bool {
        std::fs::metadata(root.join(LOCK_FILE))
            .is_ok_and(|meta| (meta.dev(), meta.ino()) == self.id)
    }

    /// 放す（何度呼んでもよい）。flock は開いたファイルごとなので、ファイルを継いだプロセスがあっても、ここで外す。
    pub(crate) fn release(&self) {
        let file = self
            .file
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some(file) = file
            && let Err(err) = file.unlock()
        {
            // 閉じれば（ファイルを継いだプロセスがなければ）外れる
            tracing::warn!(%err, root = %self.root.display(), "ライブラリのロックを外せませんでした");
        }
    }
}

impl Drop for LibraryLock {
    fn drop(&mut self) {
        self.release();
    }
}

/// `root` の `library.lock` を開き（なければ作り）、場所・ファイル・識別（デバイスと i ノード）を返す。
fn open_file(root: &Path) -> Result<(PathBuf, File, (u64, u64))> {
    let path = root.join(LOCK_FILE);
    let file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)?;
    let meta = file.metadata()?;
    Ok((path, file, (meta.dev(), meta.ino())))
}

/// flock が使えないことを表す失敗か（ENOTSUP・EOPNOTSUPP・ENOLCK、ロックのない OS の std の Unsupported）。
fn lock_unsupported(err: &std::io::Error) -> bool {
    err.kind() == std::io::ErrorKind::Unsupported
        || err
            .raw_os_error()
            .is_some_and(|code| code == ENOTSUP_MACOS || ENOLCK.contains(&code))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ロックを取ったあとに起動した子プロセスは、ロックのファイルを継がない（std は O_CLOEXEC で開く）。落ちたときと同じく
    /// 外さずに閉じても、子が生きている間に取り直せる（継いでいれば、子が終わるまで自分のライブラリを断られる）。
    /// 並べて走るほかのテストが子を起こす一瞬（fork から exec まで）はその子もファイルを握るので、取り直しは 1 秒まで待つ
    /// （継いだ子なら 5 秒のあいだ握り続けるので、待っても取れない）。
    #[test]
    fn children_do_not_inherit_the_lock_file() {
        let dir = tempfile::tempdir().unwrap();
        let lock = LibraryLock::acquire(dir.path()).unwrap();
        let mut child = std::process::Command::new("sleep")
            .arg("5")
            .spawn()
            .unwrap();
        lock.abandon();
        let mut again = LibraryLock::acquire(dir.path());
        for _ in 0..50 {
            if !matches!(again, Err(CoreError::LibraryInUse)) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
            again = LibraryLock::acquire(dir.path());
        }
        child.kill().unwrap();
        child.wait().unwrap();
        assert!(again.unwrap().is_held());
    }

    #[test]
    fn a_second_lock_on_the_same_library_is_refused_until_released() {
        let dir = tempfile::tempdir().unwrap();
        let first = LibraryLock::acquire(dir.path()).unwrap();
        assert!(first.is_held());
        assert!(dir.path().join(LOCK_FILE).is_file());
        // 同じプロセスでも、別々に開いたファイルどうしはぶつかる
        assert!(matches!(
            LibraryLock::acquire(dir.path()),
            Err(CoreError::LibraryInUse)
        ));
        first.release();
        assert!(!first.is_held());
        first.release();
        let second = LibraryLock::acquire(dir.path()).unwrap();
        assert!(second.is_held());
        drop(second);
        // drop でも放す。ロックファイルは消さない
        drop(LibraryLock::acquire(dir.path()).unwrap());
        assert!(dir.path().join(LOCK_FILE).is_file());
    }

    #[test]
    fn library_in_use_says_another_app_has_it_open() {
        let err = CoreError::LibraryInUse;
        assert_eq!(err.code(), "LibraryInUse");
        let text = err.to_string();
        assert!(text.contains("ほかのアプリで開かれています"), "{text}");
        assert!(text.contains("終了してから開き直してください"), "{text}");
    }

    #[test]
    fn is_for_compares_the_folder_not_the_spelling() {
        let dir = tempfile::tempdir().unwrap();
        let (a, b) = (dir.path().join("A.kuralib"), dir.path().join("B.kuralib"));
        std::fs::create_dir(&a).unwrap();
        std::fs::create_dir(&b).unwrap();
        let link = dir.path().join("link.kuralib");
        std::os::unix::fs::symlink(&a, &link).unwrap();
        let lock = LibraryLock::acquire(&a).unwrap();
        assert!(lock.is_for(&a));
        assert!(lock.is_for(&link));
        assert!(lock.is_for(&dir.path().join("B.kuralib/../A.kuralib")));
        // ロックファイルのないフォルダ・ほかのライブラリは別物
        assert!(!lock.is_for(&b));
        drop(LibraryLock::acquire(&b).unwrap());
        assert!(!lock.is_for(&b));
        // シンボリックリンクから取っても、同じライブラリなのでぶつかる
        assert!(matches!(
            LibraryLock::acquire(&link),
            Err(CoreError::LibraryInUse)
        ));
    }

    #[test]
    fn unsupported_file_systems_are_told_apart_from_other_failures() {
        use std::io::{Error, ErrorKind};
        assert!(lock_unsupported(&Error::from(ErrorKind::Unsupported)));
        assert!(lock_unsupported(&Error::from_raw_os_error(ENOTSUP_MACOS)));
        for code in ENOLCK {
            assert!(lock_unsupported(&Error::from_raw_os_error(code)));
        }
        assert!(!lock_unsupported(&Error::from(ErrorKind::PermissionDenied)));
        assert!(!lock_unsupported(&Error::from(ErrorKind::WouldBlock)));
    }
}
