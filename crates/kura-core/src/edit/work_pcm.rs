//! 作業用の PCM の置き場（M13 段 2。docs/editing.md §9.11）：直接シークして読めない形式（FLAC・Ogg・m4a・MP3・Platform・
//! External）の音声を、流して読んで作業用の f32 WAV（アンカーの形。`wav::F32WavWriter` → `anchor::seal_wav`）に書いて
//! 置き、保存・エディタ（段 3・4）・作り直し・GC の照合が源として使う（`anchor::open_source`）。
//!
//! - 置き場は `staging/pcm/`（`staging/engine` の隣）。名前は `<元のハッシュ 64 桁>-<鍵 16 桁>.wav`。鍵は BLAKE3
//!   （`kura-pcm-work-v1` ‖ 形式 ‖ 読み方の設定の正規の JSON ‖ デコーダの識別子か "any"）の先頭 8 バイト。書きかけは
//!   `.part` に書き、封をしてから改名する（見えている `.wav` はいつも封をし終えたもの）。再起動で消えるので `sync_all` しない。
//! - DB には記録しない：中身から決まる名前で、ライブラリを開くたびに `staging/` ごと消える（`clean_staging`）。版の記録に
//!   入るのはアンカーに昇格したものだけ（段 3）。
//! - ディスクの空き：要る量（ヘッダ 84 ＋ 形の上限の標本 ＋ メタデータの上限 16 MiB ＋ 余裕 1 GiB）より空きが少なければ、
//!   書く前に `CoreError::NoSpace`。空きは置き場の存在するいちばん近い祖先で測る（置き場は最初に書くまでない）。空きが
//!   分からなければ確かめない。書く途中の ENOSPC は `CoreError::Io`（`is_no_space`）。
//! - 寿命は参照の数で決める：[`WorkHandle`] を落とすと減り、0 になったら、残す印（[`WorkHandle::retain_on_drop`]。一時停止
//!   した保存）がなければすぐ消し、あれば残す期限（30 分）まで残す。残すものの合計は min(8 GiB, 空き / 4) までで、古い順に
//!   消す（[`WorkPcmStore::sweep`]。GC のジョブの終わりにも呼ぶ）。ライブラリを閉じるときは全部消す（[`WorkPcmStore::clear`]）。

use std::collections::BTreeMap;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use kura_engine::codec::anchor::{self, AnchorContext};
use kura_engine::codec::stream::DecodeSink;
use kura_engine::codec::wav::F32WavWriter;
use kura_engine::codec::{DecodeStamp, ReadSeek};
use kura_engine::digest::{Hasher, canonical};
use kura_engine::doc::DecodeOptions;
use kura_engine::{EngineError, Limits, Monitor, Shape};

use crate::library::upgrade::FreeSpaceFn;
use crate::{CoreError, Result};

/// 置き場の名前（ライブラリの `staging/` の下）。
pub const WORK_PCM_DIR: &str = "pcm";
/// 手放したものを残す期限（一時停止した保存の再開を待つ）。
pub const RETAIN_FOR: Duration = Duration::from_secs(30 * 60);
/// 残すものの合計の上限（空き / 4 とこれの小さいほう）。
pub const RETAIN_CAP: u64 = 8 << 30;
/// 空きの確かめで見込む余裕。
const SPARE: u64 = 1 << 30;
/// ヘッダ（RIFF・fmt の拡張形式・data）の大きさ。
const HEADER: u64 = 44 + 40;
/// アンカーのヘッダ（メタデータ）の上限の見込み。
const META: u64 = 16 << 20;

/// 作業用の PCM の鍵（元の中身・形式・読み方・デコーダ）。
#[derive(Debug, Clone, PartialEq)]
pub struct WorkKey {
    /// 元の中身のハッシュ。
    pub source: [u8; 32],
    /// 形式の ID。
    pub format: String,
    /// 読み方の設定。
    pub options: DecodeOptions,
    /// 読むデコーダの識別子（記録の識別子で読み直すとき。None は段の順）。
    pub decoder: Option<String>,
}

impl WorkKey {
    /// ファイルの名前（`<元のハッシュ 64 桁>-<鍵 16 桁>.wav`）。
    pub fn file_name(&self) -> Result<String> {
        let opts = canonical(&self.options)
            .map_err(|e| CoreError::Invalid(format!("読み方の設定を正規の形にできません: {e}")))?;
        let d = Hasher::new("kura-pcm-work-v1")
            .bytes_with_len(self.format.as_bytes())
            .bytes_with_len(&opts)
            .bytes_with_len(self.decoder.as_deref().unwrap_or("any").as_bytes())
            .finish();
        let key: String = d.0[..8].iter().map(|b| format!("{b:02x}")).collect();
        let source: String = self.source.iter().map(|b| format!("{b:02x}")).collect();
        Ok(format!("{source}-{key}.wav"))
    }
}

/// 置いた作業用の PCM（封をした f32 WAV。アンカーの形）。
#[derive(Debug, Clone, PartialEq)]
pub struct WorkPcm {
    /// ファイルの場所。
    pub path: PathBuf,
    /// 元を読んだ記録（封のヘッダに入っている。値のダイジェストは中の標本のもの）。
    pub decoded: DecodeStamp,
    /// 値の形（長さは実際の長さ）。
    pub shape: Shape,
    /// ファイルの大きさ（バイト）。
    pub bytes: u64,
}

/// 置き場の 1 項目。
struct Entry {
    pcm: Arc<WorkPcm>,
    refs: usize,
    /// 手放したあと残す期限（参照が 0 で、残す印があったもの）。
    retained_until: Option<Instant>,
    /// 手放した順（古い順に消すため）。
    released_at: u64,
}

/// 置き場の中身（ハンドルと共有する）。
struct Inner {
    dir: PathBuf,
    free_space: Mutex<Option<Arc<FreeSpaceFn>>>,
    entries: Mutex<BTreeMap<String, Entry>>,
    retain_for: Mutex<Duration>,
    /// 書きかけの名前の通し番号・手放した順の番号。
    seq: AtomicU64,
}

impl Inner {
    fn entries(&self) -> std::sync::MutexGuard<'_, BTreeMap<String, Entry>> {
        self.entries.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// 置き場のボリュームの空き。置き場はまだないことがある（開くたびの staging の掃除で消え、最初の `ensure` で作る）ので、
    /// 存在するいちばん近い祖先（同じボリュームの staging かライブラリ）で測る（ないパスでは空きを測れず、確かめずに
    /// 通ってしまうため。M13 段 2 の検証の直し）。
    fn free(&self) -> Option<u64> {
        let f = self
            .free_space
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()?;
        let at = self.dir.ancestors().find(|p| p.exists())?;
        f(at)
    }

    /// 参照を 1 つ減らす（0 になったら、残す印がなければ消す）。
    fn release(&self, name: &str, retain: bool) {
        let remove = {
            let mut entries = self.entries();
            let Some(e) = entries.get_mut(name) else {
                return;
            };
            e.refs = e.refs.saturating_sub(1);
            if e.refs > 0 {
                return;
            }
            if retain {
                let d = *self
                    .retain_for
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner);
                e.retained_until = Some(Instant::now() + d);
                e.released_at = self.seq.fetch_add(1, Ordering::Relaxed);
                None
            } else {
                entries.remove(name)
            }
        };
        if let Some(e) = remove {
            remove_file(&e.pcm.path);
        }
    }
}

fn remove_file(path: &Path) {
    match std::fs::remove_file(path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => tracing::debug!(%e, path = %path.display(), "作業用の PCM を消せませんでした"),
    }
}

/// 作業用の PCM の置き場（モジュールの説明）。
pub struct WorkPcmStore {
    inner: Arc<Inner>,
}

/// 作業用の PCM の参照（落とすと参照が減る）。
pub struct WorkHandle {
    pcm: Arc<WorkPcm>,
    name: String,
    inner: Arc<Inner>,
    retain: bool,
}

impl WorkHandle {
    /// 置いた作業用の PCM。
    pub fn pcm(&self) -> &WorkPcm {
        &self.pcm
    }

    /// 源として開く閉包（`anchor::open_source` に渡す）。
    pub fn opener(&self) -> kura_engine::audio::source::Opener {
        let path = self.pcm.path.clone();
        Arc::new(move || {
            let f = File::open(&path)?;
            Ok(Box::new(std::io::BufReader::new(f)) as Box<dyn ReadSeek + Send>)
        })
    }

    /// 手放したあとも残す期限まで残す（一時停止した保存。再開で作り直さない）。
    pub fn retain_on_drop(&mut self) {
        self.retain = true;
    }
}

impl Drop for WorkHandle {
    fn drop(&mut self) {
        self.inner.release(&self.name, self.retain);
    }
}

/// 書きかけの `.part`（落とすと消す）。
struct Part(PathBuf);

impl Drop for Part {
    fn drop(&mut self) {
        remove_file(&self.0);
    }
}

impl WorkPcmStore {
    /// 置き場 `dir`（`staging/pcm`）と空きを測る関数で作る（まだ何も作らない）。
    pub fn new(dir: PathBuf, free_space: Option<Arc<FreeSpaceFn>>) -> Self {
        Self {
            inner: Arc::new(Inner {
                dir,
                free_space: Mutex::new(free_space),
                entries: Mutex::new(BTreeMap::new()),
                retain_for: Mutex::new(RETAIN_FOR),
                seq: AtomicU64::new(0),
            }),
        }
    }

    /// 置き場の場所。
    pub fn dir(&self) -> &Path {
        &self.inner.dir
    }

    /// 空きを測る関数を入れ替える（ライブラリを開くとき。kura-mac の `volume_capacity`）。
    pub fn set_free_space(&self, f: Option<Arc<FreeSpaceFn>>) {
        *self
            .inner
            .free_space
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = f;
    }

    /// 手放したものを残す期限を変える（テスト）。
    pub fn set_retention(&self, d: Duration) {
        *self
            .inner
            .retain_for
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = d;
    }

    /// 置いたものの数と大きさの和（テスト・調べ物）。
    pub fn usage(&self) -> (usize, u64) {
        let entries = self.inner.entries();
        (
            entries.len(),
            entries.values().map(|e| e.pcm.bytes).sum::<u64>(),
        )
    }

    fn handle(&self, name: &str, e: &mut Entry) -> WorkHandle {
        e.refs += 1;
        e.retained_until = None;
        WorkHandle {
            pcm: e.pcm.clone(),
            name: name.to_string(),
            inner: self.inner.clone(),
            retain: false,
        }
    }

    /// 封をし終えた作業用の PCM があれば、その参照。
    pub fn find(&self, key: &WorkKey) -> Option<WorkHandle> {
        let name = key.file_name().ok()?;
        let mut entries = self.inner.entries();
        let e = entries.get_mut(&name)?;
        Some(self.handle(&name, e))
    }

    /// ディスクの空きを確かめる（形の上限 `bound` の音声を書く前）。
    fn check_space(&self, bound: &Shape) -> Result<()> {
        let Shape::Audio {
            channels, frames, ..
        } = bound
        else {
            return Err(CoreError::Invalid("音声の形ではありません".into()));
        };
        let needed = HEADER
            .saturating_add(
                frames
                    .max()
                    .saturating_mul(u64::from(*channels))
                    .saturating_mul(4),
            )
            .saturating_add(META)
            .saturating_add(SPARE);
        if let Some(available) = self.inner.free()
            && available < needed
        {
            return Err(CoreError::NoSpace { needed, available });
        }
        Ok(())
    }

    /// 鍵 `key` の作業用の PCM を返す。なければ、`decode`（受け手に流して読む。`CodecSet::decode_stream_path` など）で
    /// `.part` に書き、元の読み取りの記録で封をして改名し、置く。形の上限 `bound` で空きを先に確かめる。
    pub fn ensure(
        &self,
        key: &WorkKey,
        bound: &Shape,
        cx: AnchorContext,
        decode: impl FnOnce(&mut dyn DecodeSink) -> std::result::Result<DecodeStamp, EngineError>,
        limits: &Limits,
        mon: &dyn Monitor,
    ) -> Result<WorkHandle> {
        if let Some(h) = self.find(key) {
            return Ok(h);
        }
        self.check_space(bound)?;
        let name = key.file_name()?;
        std::fs::create_dir_all(&self.inner.dir)?;
        let seq = self.inner.seq.fetch_add(1, Ordering::Relaxed);
        let part = Part(self.inner.dir.join(format!("{name}.{seq}.part")));
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&part.0)?;
        let mut writer = F32WavWriter::new(file);
        let decoded = decode(&mut writer).map_err(engine_err)?;
        if mon.canceled() {
            return Err(CoreError::Invalid("中止しました".into()));
        }
        let end = writer
            .stream_end()
            .cloned()
            .ok_or_else(|| CoreError::Invalid("作業用の PCM を書き終えていません".into()))?;
        let (rate, channels) = writer.format();
        let data_end = writer.data_end();
        let mut file = writer.into_inner();
        let stamp = anchor::seal_wav(&mut file, data_end, &decoded, &cx, &end.meta, limits)
            .map_err(engine_err)?;
        drop(file);
        let path = self.inner.dir.join(&name);
        let pcm = Arc::new(WorkPcm {
            path: path.clone(),
            decoded,
            shape: Shape::Audio {
                rate,
                channels: u16::try_from(channels)
                    .map_err(|_| CoreError::Invalid("チャンネルが多すぎます".into()))?,
                frames: kura_engine::Dim::Exact(end.frames),
            },
            bytes: stamp.bytes,
        });
        let mut entries = self.inner.entries();
        // 同じ鍵をほかが先に置いていれば、そちらを使う（こちらの .part は落とすと消える）
        if let Some(e) = entries.get_mut(&name) {
            return Ok(self.handle(&name, e));
        }
        std::fs::rename(&part.0, &path)?;
        std::mem::forget(part);
        let mut e = Entry {
            pcm,
            refs: 0,
            retained_until: None,
            released_at: 0,
        };
        let h = self.handle(&name, &mut e);
        entries.insert(name, e);
        Ok(h)
    }

    /// 期限を過ぎた・残す上限（min(8 GiB, 空き / 4)）を超えた手放したものを、古い順に消す。
    pub fn sweep(&self) {
        let now = Instant::now();
        let cap = self
            .inner
            .free()
            .map_or(RETAIN_CAP, |f| (f / 4).min(RETAIN_CAP));
        let removed: Vec<Entry> = {
            let mut entries = self.inner.entries();
            let mut out = Vec::new();
            let expired: Vec<String> = entries
                .iter()
                .filter(|(_, e)| e.refs == 0 && e.retained_until.is_some_and(|t| t <= now))
                .map(|(k, _)| k.clone())
                .collect();
            for k in expired {
                out.extend(entries.remove(&k));
            }
            let mut retained: Vec<(u64, String, u64)> = entries
                .iter()
                .filter(|(_, e)| e.refs == 0)
                .map(|(k, e)| (e.released_at, k.clone(), e.pcm.bytes))
                .collect();
            retained.sort();
            let mut total: u64 = retained.iter().map(|r| r.2).sum();
            for (_, k, bytes) in retained {
                if total <= cap {
                    break;
                }
                out.extend(entries.remove(&k));
                total = total.saturating_sub(bytes);
            }
            out
        };
        for e in removed {
            remove_file(&e.pcm.path);
        }
    }

    /// 全部消す（ライブラリを閉じるとき。ジョブを止めたあと）。空になった置き場も消す。
    pub fn clear(&self) {
        let all: Vec<Entry> = std::mem::take(&mut *self.inner.entries())
            .into_values()
            .collect();
        for e in all {
            remove_file(&e.pcm.path);
        }
        if let Ok(entries) = std::fs::read_dir(&self.inner.dir) {
            for entry in entries.flatten() {
                let p = entry.path();
                if p.extension().is_some_and(|x| x == "part" || x == "wav") {
                    remove_file(&p);
                }
            }
        }
        match std::fs::remove_dir(&self.inner.dir) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => tracing::debug!(%e, "作業用の PCM の置き場を残しました"),
        }
    }
}

fn engine_err(e: EngineError) -> CoreError {
    match e {
        EngineError::Io(io) => CoreError::Io(io),
        EngineError::Limit(m) => CoreError::TooLarge(m),
        EngineError::Unsupported(m) => CoreError::Unsupported(m),
        EngineError::Canceled => CoreError::Invalid("中止しました".into()),
        other => CoreError::Invalid(format!("作業用の PCM を作れませんでした: {other}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kura_engine::NoMonitor;
    use kura_engine::codec::CodecSet;

    fn fixture(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../kura-engine/fixtures")
            .join(name)
    }

    fn key(source: u8) -> WorkKey {
        WorkKey {
            source: [source; 32],
            format: "flac".into(),
            options: DecodeOptions::default(),
            decoder: None,
        }
    }

    fn shape(set: &CodecSet, path: &Path) -> Shape {
        set.shape_path(path, "flac", &DecodeOptions::default(), &Limits::default())
            .unwrap()
            .0
    }

    /// FLAC を流して読んで置く（読んだ回数を数える）。
    fn ensure(store: &WorkPcmStore, k: &WorkKey, reads: &AtomicU64) -> Result<WorkHandle> {
        let set = CodecSet::pinned();
        let path = fixture("flac-24.flac");
        let bound = shape(&set, &path);
        store.ensure(
            k,
            &bound,
            AnchorContext::default(),
            |sink| {
                reads.fetch_add(1, Ordering::SeqCst);
                set.decode_stream_path(
                    &path,
                    "flac",
                    &DecodeOptions::default(),
                    &Limits::default(),
                    sink,
                    &NoMonitor,
                )
            },
            &Limits::default(),
            &NoMonitor,
        )
    }

    fn files(dir: &Path) -> Vec<String> {
        let mut v: Vec<String> = std::fs::read_dir(dir)
            .map(|d| {
                d.flatten()
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default();
        v.sort();
        v
    }

    /// 封をし終えたものは使い回す（2 度目は読まない）。置いたものはアンカーとして開け、値は元をデコードしたものと同じ。
    #[test]
    fn ensure_reuses_finished_files() {
        let d = tempfile::tempdir().unwrap();
        let store = WorkPcmStore::new(d.path().join("pcm"), None);
        let reads = AtomicU64::new(0);
        let a = ensure(&store, &key(1), &reads).unwrap();
        let b = ensure(&store, &key(1), &reads).unwrap();
        assert_eq!(reads.load(Ordering::SeqCst), 1);
        assert_eq!(a.pcm().path, b.pcm().path);
        assert!(store.find(&key(1)).is_some());
        assert!(store.find(&key(2)).is_none());
        let name = a
            .pcm()
            .path
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        assert!(
            name.starts_with(&"01".repeat(32)) && name.ends_with(".wav"),
            "{name}"
        );
        let set = CodecSet::pinned();
        let (v, stamp) = set
            .decode_path(
                &fixture("flac-24.flac"),
                "flac",
                &DecodeOptions::default(),
                &Limits::default(),
                &NoMonitor,
            )
            .unwrap();
        assert_eq!(a.pcm().decoded, stamp);
        assert!(v.shape() == a.pcm().shape);
        let opened = set
            .open_audio_source(
                &a.opener(),
                anchor::FORMAT,
                &DecodeOptions::default(),
                &Limits::default(),
                1 << 20,
            )
            .unwrap()
            .unwrap();
        assert_eq!(opened.value.digest().unwrap(), stamp.value_digest);
        anchor::verify_stream(&a.opener(), &stamp, &Limits::default(), &NoMonitor).unwrap();
        // 別の読み方・デコーダは別のファイル
        let mut other = key(1);
        other.decoder = Some("flac-symphonia-dec/1".into());
        assert_ne!(other.file_name().unwrap(), key(1).file_name().unwrap());
    }

    /// 書きかけ（.part）は見える名前にならない：読みが失敗したら .part を残さず、置き場にも入れない。
    #[test]
    fn part_is_never_visible() {
        let d = tempfile::tempdir().unwrap();
        let dir = d.path().join("pcm");
        let store = WorkPcmStore::new(dir.clone(), None);
        let set = CodecSet::pinned();
        let path = fixture("flac-24.flac");
        let bound = shape(&set, &path);
        let seen = Mutex::new(Vec::new());
        let r = store.ensure(
            &key(3),
            &bound,
            AnchorContext::default(),
            |sink| {
                // 途中まで読んだところで、置き場には .part だけがある
                let got = set.decode_stream_path(
                    &path,
                    "flac",
                    &DecodeOptions::default(),
                    &Limits::default(),
                    sink,
                    &NoMonitor,
                );
                seen.lock().unwrap().extend(files(&dir));
                got?;
                Err(EngineError::Decode {
                    codec: "x".into(),
                    message: "壊れた".into(),
                })
            },
            &Limits::default(),
            &NoMonitor,
        );
        assert!(r.is_err());
        let seen = seen.into_inner().unwrap();
        assert!(seen.len() == 1 && seen[0].ends_with(".part"), "{seen:?}");
        assert!(files(&dir).is_empty(), "{:?}", files(&dir));
        assert!(store.find(&key(3)).is_none());
    }

    /// 空きが足りなければ、書く前に NoSpace（ファイルを作らない）。
    /// 空きを測る偽の関数（本物の `kura_mac::volume_capacity` と同じく、ないパスでは測れない）。
    fn free_space(bytes: u64) -> Arc<FreeSpaceFn> {
        Arc::new(move |p: &Path| p.exists().then_some(bytes))
    }

    /// 置き場がまだなくても（開いたあとの最初の作成）、存在する祖先で空きを測って、書く前に断る。
    #[test]
    fn no_space_is_refused_before_writing() {
        let d = tempfile::tempdir().unwrap();
        let dir = d.path().join("staging").join("pcm");
        let store = WorkPcmStore::new(dir.clone(), Some(free_space(100 << 20)));
        let reads = AtomicU64::new(0);
        match ensure(&store, &key(4), &reads) {
            Err(CoreError::NoSpace { needed, available }) => {
                assert_eq!(available, 100 << 20);
                assert!(needed > SPARE);
            }
            Err(other) => panic!("{other}"),
            Ok(_) => panic!("書けてしまった"),
        }
        assert_eq!(reads.load(Ordering::SeqCst), 0);
        assert!(!dir.exists() && !d.path().join("staging").exists());
        // 空きが分からなければ確かめない
        store.set_free_space(Some(Arc::new(|_: &Path| None) as Arc<FreeSpaceFn>));
        assert!(ensure(&store, &key(4), &reads).is_ok());
    }

    /// 参照が 0 になったら消す。残す印があれば期限まで残し、期限を過ぎたら sweep で消す。取り直せば印は外れる。
    #[test]
    fn released_files_are_removed_unless_retained() {
        let d = tempfile::tempdir().unwrap();
        let dir = d.path().join("pcm");
        let store = WorkPcmStore::new(dir.clone(), None);
        let reads = AtomicU64::new(0);
        let a = ensure(&store, &key(5), &reads).unwrap();
        let b = store.find(&key(5)).unwrap();
        let path = a.pcm().path.clone();
        drop(a);
        assert!(path.is_file(), "まだ参照がある");
        drop(b);
        assert!(!path.exists(), "参照が 0 になったら消す");
        assert_eq!(store.usage().0, 0);
        // 残す印
        let mut c = ensure(&store, &key(5), &reads).unwrap();
        c.retain_on_drop();
        drop(c);
        assert!(path.is_file());
        store.sweep();
        assert!(path.is_file(), "期限の前は残す");
        let again = store.find(&key(5)).unwrap();
        assert_eq!(reads.load(Ordering::SeqCst), 2, "残したものを使い回す");
        drop(again);
        assert!(!path.exists(), "取り直したら印は外れる");
        // 期限を過ぎたもの
        store.set_retention(Duration::ZERO);
        let mut e = ensure(&store, &key(5), &reads).unwrap();
        e.retain_on_drop();
        drop(e);
        std::thread::sleep(Duration::from_millis(5));
        store.sweep();
        assert!(!path.exists());
        // 残す上限（空き / 4）を超えたものは古い順に消す
        store.set_retention(Duration::from_secs(3_600));
        let mut f = ensure(&store, &key(6), &reads).unwrap();
        let mut g = ensure(&store, &key(7), &reads).unwrap();
        let (pf, pg) = (f.pcm().path.clone(), g.pcm().path.clone());
        let one = f.pcm().bytes;
        f.retain_on_drop();
        g.retain_on_drop();
        drop(f);
        drop(g);
        store.set_free_space(Some(free_space(one * 4 + 4)));
        store.sweep();
        assert!(!pf.exists() && pg.is_file(), "古いほうから消す");
    }

    /// 閉じるときは全部消し、空の置き場も消す（残っていた .part・.wav も）。
    #[test]
    fn clear_on_close_and_staging_cleanup() {
        let d = tempfile::tempdir().unwrap();
        let dir = d.path().join("pcm");
        let store = WorkPcmStore::new(dir.clone(), None);
        let reads = AtomicU64::new(0);
        let a = ensure(&store, &key(8), &reads).unwrap();
        std::fs::write(dir.join("left.0.part"), b"x").unwrap();
        store.clear();
        assert!(!dir.exists());
        assert_eq!(store.usage().0, 0);
        // 持っていたハンドルを落としても何も起きない
        drop(a);
        assert!(!dir.exists());
    }
}
