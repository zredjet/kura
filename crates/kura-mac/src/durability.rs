//! 一括加工のファイルの耐久（M14 段 4。ADR-190 (2)・ADR-193）：1 件ずつは fsync（ドライブのキャッシュまでは流さない。
//! 1 回 約 0.5 ms）で置き、まとまりのコミットの前に 1 回だけ F_FULLFSYNC（ドライブのキャッシュを流す。1 回 約 7.5 ms）を
//! 取る。std の `File::sync_all`・`sync_data` は macOS ではどちらも F_FULLFSYNC になるので、fsync は libc で呼ぶ。macOS
//! 以外では 1 件ずつ `sync_all` で区切りは何もしない（kura-core の既定と同じ）。

use std::fs::File;

use kura_core::edit::Durability;

/// macOS の耐久の取り方。
pub struct MacDurability;

impl Durability for MacDurability {
    fn sync_file(&self, file: &File) -> std::io::Result<()> {
        #[cfg(target_os = "macos")]
        {
            use std::os::fd::AsRawFd;
            // SAFETY: 開いているファイルの記述子を渡すだけ（`file` が生きている間に呼ぶ）
            let rc = unsafe { libc::fsync(file.as_raw_fd()) };
            if rc == 0 {
                Ok(())
            } else {
                Err(std::io::Error::last_os_error())
            }
        }
        #[cfg(not(target_os = "macos"))]
        {
            file.sync_all()
        }
    }

    fn barrier(&self, file: &File) -> std::io::Result<()> {
        #[cfg(target_os = "macos")]
        {
            use std::os::fd::AsRawFd;
            // SAFETY: 開いているファイルの記述子に F_FULLFSYNC を出すだけ（同じボリュームのドライブのキャッシュを流す）
            let rc = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_FULLFSYNC) };
            if rc == -1 {
                Err(std::io::Error::last_os_error())
            } else {
                Ok(())
            }
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = file;
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 書いたファイルを同期でき、区切りも取れる（macOS では fsync と F_FULLFSYNC が通る）。
    #[test]
    fn sync_and_barrier_succeed_on_a_written_file() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.part");
        let mut file = File::create(&path).unwrap();
        file.write_all(b"kura").unwrap();
        MacDurability.sync_file(&file).unwrap();
        MacDurability.barrier(&file).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"kura");
    }
}
