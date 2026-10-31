//! 作業用のファイル（A15。M13 段 4B）：直接読めない形式（FLAC など）の長い音声は、作業用のファイル（封をした f32 WAV）に
//! 流して書いてから遅延の源で開き、閉じると消える。作る間の予算は対話の枠で待たずに確保し（足りなければ Busy）、閉じる・
//! ほかを開くと止まる。試聴の `/pcm` も同じ鍵の作業用のファイルを使い、エディタと使い回す。

use std::io::Cursor;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use kura_engine::budget::{Class, MemoryBudget};
use kura_engine::codec::{CodecEnv, CodecSet, DecodeOut, Decoder, Held, ReadSeek, Tier};
use kura_engine::doc::{DecodeOptions, InputId};
use kura_engine::render::cache::Fingerprint;
use kura_engine::testgen::InputGen;
use kura_engine::value::LoopRange;
use kura_engine::{Digest, EngineError, Graph, Limits, Monitor, NoMonitor, Shape, Value};
use serde_json::json;

use super::session_tests::*;
use super::source::{WorkCx, make_work, open_work, read_shape_staged, work_threshold};
use crate::CoreError;
use crate::edit::editor::{EditorError, OpenRequest, ProblemCode, Viewport, close, open};
use crate::edit::{EngineFactory, RuntimeConfig};

/// 予算 32 MiB：丸ごと読む上限と作業用のファイルの境は 8 MiB（8 kHz モノラルで約 2.1 M フレーム）。
const SMALL: u64 = 32 << 20;

/// 長い FLAC（8 kHz モノラルの正弦波・ループ点つき。`frames` フレーム）。
fn long_flac(frames: u64) -> Vec<u8> {
    let value = InputGen::Sine {
        rate: 8_000,
        channels: 1,
        frames,
        freq: 440,
        loop_range: Some(LoopRange {
            start: 1_000,
            end: frames - 7,
        }),
    }
    .generate()
    .unwrap();
    encode(&value, "flac")
}

/// 丸ごと読んだ値と記録（正）。
fn decode_whole(
    bytes: &[u8],
) -> (
    Arc<kura_engine::value::Audio>,
    kura_engine::codec::DecodeStamp,
) {
    let (value, stamp) = CodecSet::pinned()
        .decode(
            &mut Cursor::new(bytes),
            "flac",
            &Default::default(),
            &Default::default(),
            &NoMonitor,
        )
        .unwrap();
    match value {
        Value::Audio(a) => (a, stamp),
        _ => panic!("音声ではありません"),
    }
}

/// 置き場の中のファイル（名前の順）。
fn files(dir: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|d| d.flatten().map(|e| e.path()).collect())
        .unwrap_or_default();
    v.sort();
    v
}

/// 全チャンネルの全標本が同じ。
fn same_samples(got: &kura_engine::value::Audio, want: &kura_engine::value::Audio) {
    assert_eq!(
        (got.rate(), got.channels(), got.frames()),
        (want.rate(), want.channels(), want.frames())
    );
    for ch in 0..want.channels() {
        let mut a = vec![0f32; want.frames()];
        let mut b = vec![0f32; want.frames()];
        got.read(ch, 0, &mut a).unwrap();
        want.read(ch, 0, &mut b).unwrap();
        assert!(
            a.iter().zip(&b).all(|(x, y)| x.to_bits() == y.to_bits()),
            "チャンネル {ch} の標本が違います"
        );
    }
}

fn g_gain(db: f64) -> Graph {
    let mut g = Graph::new_single();
    g.push_linear("audio.gain", 1, json!({ "db": db }), 1)
        .unwrap();
    g
}

/// 形を読むたびに、その時点の対話の確保（`Usage::interactive`）を記す（予算の外で形を読まないことの確かめ）。
#[derive(Clone, Default)]
struct ShapeLog {
    budget: Arc<Mutex<Option<Arc<MemoryBudget>>>>,
    seen: Arc<Mutex<Vec<u64>>>,
}

impl ShapeLog {
    /// 記す予算（ライブラリのエンジンの部品を作ってから）。
    fn attach(&self, budget: &Arc<MemoryBudget>) {
        *self.budget.lock().unwrap() = Some(budget.clone());
    }

    fn note(&self) {
        let used = self
            .budget
            .lock()
            .unwrap()
            .as_ref()
            .map_or(0, |b| b.usage().unwrap().interactive);
        self.seen.lock().unwrap().push(used);
    }

    fn seen(&self) -> Vec<u64> {
        self.seen.lock().unwrap().clone()
    }

    fn clear(&self) {
        self.seen.lock().unwrap().clear();
    }
}

/// 試す段：`inner`（固定の段のデコーダ）の読み方をそのまま使い、識別子・読む形式・形を読む分・流しの断り・形を読んだときの
/// 記録だけを替える（段 2 の解析のジョブの `RefusingStream` と同じ考え方）。値を読んだ・流して読んだ数も数える。
pub(super) struct Stage {
    inner: Arc<dyn Decoder>,
    id: &'static str,
    /// この段が読む形式と、`inner` に渡す形式。
    format: &'static str,
    inner_format: &'static str,
    /// 形を読むのにファイル全体を読む段（MP3・Platform）のように、形を読む分をファイルの長さで申告する。
    whole_shape: bool,
    /// 値を読む・流して読むと、この誤りを返す（形は読める）。
    refuse: Option<fn() -> EngineError>,
    log: Option<ShapeLog>,
    /// 値を読んだ・流して読んだ数（[`Stage::counting`]）。
    decodes: Option<Arc<AtomicU64>>,
    /// 再現しない読み手として申告する（[`Stage::unreproducible`]。Platform・External と同じに、保存がアンカーに昇格する）。
    unreproducible: bool,
}

impl Stage {
    /// `inner` と同じ識別子・形式で、形を読むたびに記す段（`CodecSet::replace_decoder` で差し替える）。
    fn watching(inner: Arc<dyn Decoder>, format: &'static str, log: &ShapeLog) -> Self {
        Self {
            id: inner.id(),
            inner,
            format,
            inner_format: format,
            whole_shape: true,
            refuse: None,
            log: Some(log.clone()),
            decodes: None,
            unreproducible: false,
        }
    }

    /// `inner` と同じに読む、識別子 `id` の段（遅延の源で開く判定から外したいときなど）。
    pub(super) fn renamed(inner: Arc<dyn Decoder>, id: &'static str, format: &'static str) -> Self {
        Self {
            inner,
            id,
            format,
            inner_format: format,
            whole_shape: false,
            refuse: None,
            log: None,
            decodes: None,
            unreproducible: false,
        }
    }

    /// 値を読む・流して読むたびに `counter` を 1 増やす。
    pub(super) fn counting(mut self, counter: &Arc<AtomicU64>) -> Self {
        self.decodes = Some(counter.clone());
        self
    }

    /// 再現しない読み手として申告する（読む値は `inner` と同じ）。
    pub(super) fn unreproducible(mut self) -> Self {
        self.unreproducible = true;
        self
    }

    /// 形は読めるが、値を読む・流して読むと `refuse` の誤りを返すようにする。
    fn refusing(mut self, refuse: fn() -> EngineError) -> Self {
        self.refuse = Some(refuse);
        self
    }

    fn note(&self) {
        if let Some(log) = &self.log {
            log.note();
        }
    }

    fn refused(&self) -> Result<(), EngineError> {
        if let Some(n) = &self.decodes {
            n.fetch_add(1, Ordering::SeqCst);
        }
        match self.refuse {
            Some(f) => Err(f()),
            None => Ok(()),
        }
    }
}

impl Decoder for Stage {
    fn id(&self) -> &'static str {
        self.id
    }
    fn tier(&self) -> Tier {
        self.inner.tier()
    }
    fn reproducible(&self) -> bool {
        !self.unreproducible && self.inner.reproducible()
    }
    fn env(&self) -> Option<CodecEnv> {
        self.inner.env()
    }
    fn can_decode(&self, format: &str) -> bool {
        format == self.format
    }
    fn work_bytes(&self, _format: &str, shape: &Shape) -> u64 {
        self.inner.work_bytes(self.inner_format, shape)
    }
    fn file_work_bytes(&self, _format: &str, shape: &Shape, len: u64) -> u64 {
        self.inner.file_work_bytes(self.inner_format, shape, len)
    }
    fn shape(
        &self,
        src: &mut dyn ReadSeek,
        _format: &str,
        opts: &DecodeOptions,
        limits: &Limits,
    ) -> Result<Shape, EngineError> {
        self.note();
        self.inner.shape(src, self.inner_format, opts, limits)
    }
    fn decode(
        &self,
        src: &mut dyn ReadSeek,
        _format: &str,
        opts: &DecodeOptions,
        limits: &Limits,
        mon: &dyn Monitor,
    ) -> Result<Value, EngineError> {
        self.refused()?;
        self.inner.decode(src, self.inner_format, opts, limits, mon)
    }
    fn shape_file(
        &self,
        path: &Path,
        _format: &str,
        opts: &DecodeOptions,
        limits: &Limits,
    ) -> Result<Shape, EngineError> {
        self.note();
        self.inner.shape_file(path, self.inner_format, opts, limits)
    }
    fn shape_monitored(
        &self,
        src: &mut dyn ReadSeek,
        _format: &str,
        opts: &DecodeOptions,
        limits: &Limits,
        mon: &dyn Monitor,
    ) -> Result<Shape, EngineError> {
        self.note();
        self.inner
            .shape_monitored(src, self.inner_format, opts, limits, mon)
    }
    fn shape_file_monitored(
        &self,
        path: &Path,
        _format: &str,
        opts: &DecodeOptions,
        limits: &Limits,
        mon: &dyn Monitor,
    ) -> Result<Shape, EngineError> {
        self.note();
        self.inner
            .shape_file_monitored(path, self.inner_format, opts, limits, mon)
    }
    fn decode_file(
        &self,
        path: &Path,
        _format: &str,
        opts: &DecodeOptions,
        limits: &Limits,
        mon: &dyn Monitor,
    ) -> Result<Value, EngineError> {
        self.refused()?;
        self.inner
            .decode_file(path, self.inner_format, opts, limits, mon)
    }
    fn streams(&self, _format: &str) -> bool {
        self.inner.streams(self.inner_format)
    }
    fn stream_work_bytes(&self, _format: &str, shape: &Shape, len: u64) -> u64 {
        self.inner.stream_work_bytes(self.inner_format, shape, len)
    }
    fn shape_work_bytes(&self, _format: &str, len: u64) -> u64 {
        if self.whole_shape {
            len
        } else {
            self.inner.shape_work_bytes(self.inner_format, len)
        }
    }
    fn held_bytes(
        &self,
        src: &mut dyn ReadSeek,
        _format: &str,
        limits: &Limits,
    ) -> Result<Held, EngineError> {
        self.inner.held_bytes(src, self.inner_format, limits)
    }
    fn held_bytes_file(
        &self,
        path: &Path,
        _format: &str,
        limits: &Limits,
    ) -> Result<Held, EngineError> {
        self.inner.held_bytes_file(path, self.inner_format, limits)
    }
    fn decode_stream(
        &self,
        src: &mut (dyn ReadSeek + Send),
        _format: &str,
        opts: &DecodeOptions,
        limits: &Limits,
        shape: &Shape,
        out: &mut DecodeOut<'_>,
        mon: &dyn Monitor,
    ) -> Result<(), EngineError> {
        self.refused()?;
        self.inner
            .decode_stream(src, self.inner_format, opts, limits, shape, out, mon)
    }
    fn decode_stream_file(
        &self,
        path: &Path,
        _format: &str,
        opts: &DecodeOptions,
        limits: &Limits,
        shape: &Shape,
        out: &mut DecodeOut<'_>,
        mon: &dyn Monitor,
    ) -> Result<(), EngineError> {
        self.refused()?;
        self.inner
            .decode_stream_file(path, self.inner_format, opts, limits, shape, out, mon)
    }
}

/// 予算 `budget` で、`make` が作るコーデックの組を使う偽のファクトリ。
pub(super) struct Staged {
    pub budget: u64,
    pub make: Box<dyn Fn() -> CodecSet + Send + Sync>,
}

impl EngineFactory for Staged {
    fn runtime_config(&self) -> RuntimeConfig {
        RuntimeConfig {
            cores: Some(2),
            budget: Some(self.budget),
            ..RuntimeConfig::default()
        }
    }

    fn codecs(&self, _work_dir: &Path) -> CodecSet {
        (self.make)()
    }
}

/// 固定の段の組の、`format` の最初の段のデコーダ。
pub(super) fn pinned_decoder(format: &str) -> Arc<dyn Decoder> {
    CodecSet::pinned().decoders_for(format)[0].clone()
}

/// 固定の段の FLAC を、形を読むたびに記す段（形を読む分はファイルの長さ）に替えたライブラリ（予算 `budget`）。
fn watched_env(budget: u64, log: &ShapeLog) -> Env {
    let log = log.clone();
    Env::with(Arc::new(Staged {
        budget,
        make: Box::new(move || {
            let mut set = CodecSet::pinned();
            for format in ["flac", "mp3"] {
                set.replace_decoder(Arc::new(Stage::watching(
                    pinned_decoder(format),
                    format,
                    &log,
                )))
                .unwrap();
            }
            set
        }),
    }))
}

/// FLAC を `stages`（段の順）だけで読むライブラリ（予算 `budget`）。
fn flac_env(budget: u64, stages: fn() -> Vec<Arc<dyn Decoder>>) -> Env {
    Env::with(Arc::new(Staged {
        budget,
        make: Box::new(move || {
            let mut set = CodecSet::default();
            for d in stages() {
                set.add_decoder(d).unwrap();
            }
            set
        }),
    }))
}

/// 開くのを断った誤り。
fn open_err(env: &Env, item_id: i64) -> EditorError {
    open(
        &env.env(),
        &OpenRequest {
            item_id,
            viewport: Viewport::audio_idle(),
            from_draft: false,
        },
    )
    .unwrap_err()
}

/// WAV を、固定の段の WAV と同じに読むが識別子の違う段（`plain-wav-dec/1`）だけで読むライブラリ（予算 `budget`）。遅延の源で
/// 開かない（直接読めない形式と同じ道）ので、長い値を速く作って作業用のファイルの道を試せる。
fn plain_wav_env(budget: u64) -> Env {
    Env::with(Arc::new(Staged {
        budget,
        make: Box::new(|| {
            let mut set = CodecSet::default();
            set.add_decoder(Arc::new(Stage::renamed(
                pinned_decoder("wav"),
                "plain-wav-dec/1",
                "wav",
            )))
            .unwrap();
            set
        }),
    }))
}

/// 16 ビット・モノラルの WAV（`frames` フレーム。標本は (i mod 2000) − 1000）を手で書く（長い値を速く作る）。
pub(super) fn plain_wav_bytes(rate: u32, frames: u64) -> Vec<u8> {
    let data = frames * 2;
    let mut b = Vec::with_capacity(44 + data as usize);
    b.extend_from_slice(b"RIFF");
    b.extend_from_slice(&u32::try_from(36 + data).unwrap().to_le_bytes());
    b.extend_from_slice(b"WAVEfmt ");
    b.extend_from_slice(&16u32.to_le_bytes());
    b.extend_from_slice(&1u16.to_le_bytes());
    b.extend_from_slice(&1u16.to_le_bytes());
    b.extend_from_slice(&rate.to_le_bytes());
    b.extend_from_slice(&(rate * 2).to_le_bytes());
    b.extend_from_slice(&2u16.to_le_bytes());
    b.extend_from_slice(&16u16.to_le_bytes());
    b.extend_from_slice(b"data");
    b.extend_from_slice(&u32::try_from(data).unwrap().to_le_bytes());
    for i in 0..frames {
        b.extend_from_slice(&(((i % 2000) as i16) - 1000).to_le_bytes());
    }
    b
}

/// 置き場の空きを `bytes` にする（空きを測る関数）。
fn set_free(env: &Env, bytes: u64) {
    env.engine.set_free_space(Some(
        Arc::new(move |_: &Path| Some(bytes)) as Arc<crate::library::upgrade::FreeSpaceFn>
    ));
}

/// A15：丸ごと読む上限を超える FLAC は作業用のファイルから遅延で開く。値・記録・指紋は丸ごと読んだものと同じで、閉じると
/// 作業用のファイルが消え、予算も 0 に戻る。
#[test]
fn long_flac_opens_from_a_work_file_and_closing_removes_it() {
    let env = Env::with(Arc::new(Budgeted(SMALL)));
    let frames = 40 * C;
    assert!(frames * 4 > work_threshold(SMALL));
    let flac = long_flac(frames);
    let id = add_audio(&env.lib, &flac, "flac");
    let dir = env.engine.work_pcm().dir().to_path_buf();
    let opened = env.open(id);
    let a = opened.source.audio.clone().unwrap();
    assert!(a.lazy, "作業用のファイルから遅延で開く");
    assert!(a.decoder.starts_with("flac"), "{}", a.decoder);
    assert_eq!((a.rate, a.channels, a.frames), (8_000, 1, frames));
    assert_eq!(a.loop_range, Some((1_000, frames - 7)));
    let made = files(&dir);
    assert_eq!(made.len(), 1, "{made:?}");
    assert!(made[0].extension().is_some_and(|x| x == "wav"));
    assert_eq!(env.engine.work_pcm().usage().0, 1);

    let (whole, stamp) = decode_whole(&flac);
    let s = opened.session.clone();
    let editor = env.editor(&s);
    {
        let guard = editor.lock_state();
        let st = guard.as_ref().unwrap();
        let info = &st.audio.as_ref().unwrap().input;
        assert_eq!(
            info.decoded.as_ref(),
            Some(&stamp),
            "記録は流れのダイジェストから"
        );
        assert_eq!(info.work.as_ref().unwrap().path(), made[0]);
        assert_eq!(
            st.inputs[&InputId(0)].fingerprint,
            Fingerprint::of_decoded(&stamp)
        );
    }
    let (input, decoder) = editor.published_input().unwrap();
    assert_eq!(decoder, stamp.codec);
    let Value::Audio(input) = input else { panic!() };
    same_samples(&input, &whole);
    assert_eq!(input.meta, whole.meta, "メタデータも丸ごと読んだものと同じ");

    // 丸ごと読むライブラリで開いたものと同じ指紋（samplesId）
    let whole_env = Env::new();
    let wid = add_audio(&whole_env.lib, &flac, "flac");
    let w = whole_env.open(wid);
    assert!(!w.source.audio.unwrap().lazy, "予算が大きければ丸ごと読む");
    assert_eq!(
        shown_audio(&w.outcome.view).samples_id,
        shown_audio(&opened.outcome.view).samples_id
    );
    assert_eq!(whole_env.engine.work_pcm().usage().0, 0);

    // 手の表示は全体の描画と同じ
    let out = env.step(
        &s,
        vec![add(0, "audio.gain", json!({ "db": -3.0 }))],
        &Viewport::audio_idle(),
    );
    let info = shown_audio(&out.view);
    assert_eq!(info.valid, vec![(0, frames)]);
    let (value, _) = editor.published_audio(info.rev).unwrap();
    let want = rendered(&flac, "flac", &g_gain(-3.0));
    let mut got = vec![0f32; want.frames()];
    value.audio_read(0, 0, &mut got).unwrap();
    let mut expect = vec![0f32; want.frames()];
    want.read(0, 0, &mut expect).unwrap();
    assert_eq!(got, expect);

    // 閉じると作業用のファイルが消える
    close(&env.engine, &s);
    assert!(files(&dir).is_empty(), "{:?}", files(&dir));
    assert_eq!(env.engine.work_pcm().usage(), (0, 0));
    let u = env.usage();
    assert_eq!((u.reserved, u.interactive, u.standing), (0, 0, 0));
}

/// A15：作る間の予算が空いていなければ、待たずに Busy で断る（作業用のファイル・取り置きを残さない）。空けば開ける。
#[test]
fn work_file_is_refused_when_the_budget_is_busy() {
    let env = Env::with(Arc::new(Budgeted(SMALL)));
    let flac = long_flac(40 * C);
    let id = add_audio(&env.lib, &flac, "flac");
    let dir = env.engine.work_pcm().dir().to_path_buf();
    let budget = env.engine.runtime().unwrap().budget().clone();
    // 形を読む分（数 MiB）は空いているが、流して書く分（窓・デコーダ・ダイジェスト・書き手）は空いていない
    let held = budget
        .try_acquire(SMALL - (6 << 20), kura_engine::budget::Class::Interactive)
        .unwrap()
        .unwrap();
    let err = open(
        &env.env(),
        &OpenRequest {
            item_id: id,
            viewport: Viewport::audio_idle(),
            from_draft: false,
        },
    )
    .unwrap_err();
    assert!(
        matches!(&err, EditorError::Busy(m) if m.contains("作業用のファイルにする")),
        "{err}"
    );
    assert!(files(&dir).is_empty(), "{:?}", files(&dir));
    let u = env.usage();
    assert_eq!(u.reserved, 0, "断ったら取り置きを残さない");
    assert_eq!(u.interactive, held.bytes(), "作る分を返した");
    assert!(env.engine.current_editor().is_none());
    drop(held);
    let opened = env.open(id);
    assert!(opened.source.audio.unwrap().lazy);
    assert_eq!(files(&dir).len(), 1);
}

/// 置き場に書きかけ（.part）が現れたら中止を頼む見張り（読みの途中で止める）。
struct CancelWhenWriting(PathBuf);

impl Monitor for CancelWhenWriting {
    fn canceled(&self) -> bool {
        files(&self.0)
            .iter()
            .any(|p| p.extension().is_some_and(|x| x == "part"))
    }
}

/// A15：作っている途中の中止は Canceled で、書きかけを残さない（作業用のファイルの口で決まった形に止める）。エディタでは
/// 閉じると止まる。
#[test]
fn work_file_creation_stops_on_cancel() {
    let env = Env::with(Arc::new(Budgeted(SMALL)));
    let flac = long_flac(40 * C);
    let id = add_audio(&env.lib, &flac, "flac");
    let dir = env.engine.work_pcm().dir().to_path_buf();
    let layout = env.lib.layout();
    let hash = *blake3::hash(&flac).as_bytes();
    let path = layout.content_path(&hash, "flac");
    let runtime = env.engine.runtime().unwrap().clone();
    let codecs = env.engine.codecs();
    let (opts, limits) = (Default::default(), Default::default());
    let mon = CancelWhenWriting(dir.clone());
    let cx = WorkCx {
        runtime: &runtime,
        codecs: &codecs,
        store: env.engine.work_pcm(),
        path: &path,
        source: Digest(hash),
        format: "flac",
        opts: &opts,
        limits: &limits,
        mon: &mon,
    };
    let (shape, stage) = read_shape_staged(&cx, None).unwrap().unwrap();
    let err = make_work(&cx, shape, stage).err().unwrap();
    assert!(matches!(err, EditorError::Canceled), "{err}");
    assert!(files(&dir).is_empty(), "{:?}", files(&dir));
    assert_eq!(env.engine.work_pcm().usage().0, 0);
    assert_eq!(runtime.budget().usage().unwrap().interactive, 0);

    // エディタ：開いている途中で閉じると止まる
    let engine = env.engine.clone();
    let jobs_env = &env;
    std::thread::scope(|scope| {
        let opening = scope.spawn(move || {
            open(
                &jobs_env.env(),
                &OpenRequest {
                    item_id: id,
                    viewport: Viewport::audio_idle(),
                    from_draft: false,
                },
            )
        });
        let started = std::time::Instant::now();
        while !files(&dir)
            .iter()
            .any(|p| p.extension().is_some_and(|x| x == "part"))
        {
            assert!(
                started.elapsed() < std::time::Duration::from_secs(60),
                "書き始めません"
            );
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        engine.close_editor();
        let r = opening.join().unwrap();
        assert!(matches!(r, Err(EditorError::Canceled)), "{:?}", r.err());
    });
    assert!(files(&dir).is_empty(), "{:?}", files(&dir));
    assert_eq!(env.engine.work_pcm().usage().0, 0);
    let u = env.usage();
    assert_eq!((u.reserved, u.interactive, u.standing), (0, 0, 0));
}

/// 試聴の `/pcm`：長い FLAC は作業用のファイルから読み、応答は丸ごと読んだものとバイト列まで同じ（M11 の値・kura チャンク）。
/// エディタとは同じ鍵のファイルを使い回し（どちらが先でも作り直さない）、どちらも手放すと消える。
#[test]
fn pcm_reads_long_flac_from_the_work_file_shared_with_the_editor() {
    let env = Env::with(Arc::new(Budgeted(SMALL)));
    let frames = 40 * C;
    let flac = long_flac(frames);
    let id = add_audio(&env.lib, &flac, "flac");
    let dir = env.engine.work_pcm().dir().to_path_buf();
    let hash = crate::library::hex(blake3::hash(&flac).as_bytes());
    let root = env.lib.layout().root().to_path_buf();
    let src = crate::serve::pcm_source(env.lib.layout(), &hash).unwrap();
    let serve = |a: u64, b: u64| crate::asset::pcm::serve(&env.engine, &root, &hash, &src, a, b);
    let first = serve(1_000, 9_000).unwrap();
    let made = files(&dir);
    assert_eq!(made.len(), 1, "{made:?}");
    let ino = std::fs::metadata(&made[0]).unwrap().ino();
    assert_eq!(
        env.engine
            .pcm_cache()
            .work()
            .map(|w| w.path().to_path_buf()),
        Some(made[0].clone())
    );
    // 予算の大きいライブラリ（丸ごと読む）の同じ要求と、バイト列まで同じ
    let big = Env::with(Arc::new(Budgeted(256 << 20)));
    add_audio(&big.lib, &flac, "flac");
    let big_src = crate::serve::pcm_source(big.lib.layout(), &hash).unwrap();
    for (a, b) in [(1_000, 9_000), (0, 10), (frames - 100, frames)] {
        let whole =
            crate::asset::pcm::serve(&big.engine, big.lib.layout().root(), &hash, &big_src, a, b)
                .unwrap();
        let got = if a == 1_000 {
            first.clone()
        } else {
            serve(a, b).unwrap()
        };
        assert_eq!(got, whole, "{a}-{b}");
    }
    assert_eq!(big.engine.work_pcm().usage().0, 0, "丸ごと読んだ");
    assert!(big.engine.pcm_cache().work().is_none());

    // エディタは試聴が作ったファイルを使い回す（試聴の置き場を空にしても消さない）
    let opened = env.open(id);
    assert!(opened.source.audio.clone().unwrap().lazy);
    assert!(env.engine.pcm_cache().cached().is_none());
    assert_eq!(files(&dir), made);
    assert_eq!(
        std::fs::metadata(&made[0]).unwrap().ino(),
        ino,
        "作り直さない"
    );
    // 試聴はエディタのファイルを使い回す
    let again = serve(1_000, 9_000).unwrap();
    assert_eq!(again, first);
    assert_eq!(files(&dir), made);
    assert_eq!(std::fs::metadata(&made[0]).unwrap().ino(), ino);
    assert_eq!(env.engine.work_pcm().usage().0, 1);
    // 閉じても試聴が持っている間は残り、試聴を手放すと消える
    close(&env.engine, &opened.session);
    assert!(made[0].is_file());
    env.engine.pcm_cache().clear();
    assert!(files(&dir).is_empty(), "{:?}", files(&dir));
    // 試聴の予算も返した
    assert_eq!(env.usage().interactive, 0);
}

/// 段 4B の検証の直し 2：直接読めない形式（形を読むのにファイル全体・埋め込みの画像を読む段がある）は、遅延で開けるかを
/// 確かめるために予算の外で形を読まない。エディタを開くときの形の読みはどれも、その段の見積もり（ここではファイルの長さ）を
/// 対話の枠で確保している間に起き、空いていなければ形を読む前に Busy で断る。
#[test]
fn opening_reads_the_shape_only_inside_the_budget() {
    let log = ShapeLog::default();
    let env = watched_env(SMALL, &log);
    let flac = long_flac(40 * C);
    let len = flac.len() as u64;
    let id = add_audio(&env.lib, &flac, "flac");
    let budget = env.engine.runtime().unwrap().budget().clone();
    log.attach(&budget);
    // 形を読む分（ファイルの長さ）が空いていない：読む前に断る
    let held = budget
        .try_acquire(SMALL - len / 2, Class::Interactive)
        .unwrap()
        .unwrap();
    let err = open(
        &env.env(),
        &OpenRequest {
            item_id: id,
            viewport: Viewport::audio_idle(),
            from_draft: false,
        },
    )
    .unwrap_err();
    assert!(
        matches!(&err, EditorError::Busy(m) if m.contains("元の形を読む")),
        "{err}"
    );
    assert!(
        log.seen().is_empty(),
        "断る前に形を読んだ：{:?}",
        log.seen()
    );
    assert!(env.engine.current_editor().is_none());
    drop(held);
    // 空いていれば開ける。形の読みはどれも確保の中
    let opened = env.open(id);
    assert!(opened.source.audio.unwrap().lazy);
    let seen = log.seen();
    assert!(!seen.is_empty());
    assert!(
        seen.iter().all(|&u| u >= len),
        "予算の外で形を読んだ：{seen:?}（ファイルは {len} バイト）"
    );
}

/// 段 4B の検証の直し 2：試聴の `/pcm` も、直接読めない形式は予算の外で形を読まない（境の判定・丸ごと読みに使う形は段ごとに
/// 確保して読む）。空いていなければ、形を読む前に 503。短いもの（丸ごと読む）も同じ。
#[test]
fn pcm_reads_the_shape_only_inside_the_budget() {
    let log = ShapeLog::default();
    let env = watched_env(SMALL, &log);
    let root = env.lib.layout().root().to_path_buf();
    let budget = env.engine.runtime().unwrap().budget().clone();
    log.attach(&budget);
    // 長い FLAC（作業用のファイル）・短い FLAC（丸ごと読み）・MP3（固定の段は形を読むのにファイル全体を読む。丸ごと読み）
    let mp3 = std::fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../kura-engine/fixtures/mp3-free.mp3"),
    )
    .unwrap();
    for (bytes, ext) in [
        (long_flac(40 * C), "flac"),
        (long_flac(2 * C), "flac"),
        (mp3, "mp3"),
    ] {
        let len = bytes.len() as u64;
        add_audio(&env.lib, &bytes, ext);
        let hash = crate::library::hex(blake3::hash(&bytes).as_bytes());
        let src = crate::serve::pcm_source(env.lib.layout(), &hash).unwrap();
        let serve = || crate::asset::pcm::serve(&env.engine, &root, &hash, &src, 0, 10);
        env.engine.pcm_cache().clear();
        log.clear();
        let held = budget
            .try_acquire(SMALL - len / 2, Class::Interactive)
            .unwrap()
            .unwrap();
        let err = serve().unwrap_err();
        assert_eq!(err.status(), 503, "{err}");
        assert!(
            log.seen().is_empty(),
            "断る前に形を読んだ：{:?}",
            log.seen()
        );
        drop(held);
        serve().unwrap();
        let seen = log.seen();
        assert!(!seen.is_empty());
        assert!(
            seen.iter().all(|&u| u >= len),
            "予算の外で形を読んだ（{ext}）：{seen:?}（ファイルは {len} バイト）"
        );
    }
}

/// 段 4B の検証の直し 3：試聴の `/pcm` で作業用のファイルを作れない（ディスクの空きが足りない）とき、丸ごと読む見積もりが
/// 丸ごと読む上限を超えれば 507（「ディスクの空き容量が足りません…」）。作業用のファイル・覚えたもの・予算を残さない。
#[test]
fn pcm_without_disk_space_is_507_when_it_cannot_read_whole() {
    let env = Env::with(Arc::new(Budgeted(SMALL)));
    set_free(&env, 1 << 20);
    let flac = long_flac(40 * C);
    add_audio(&env.lib, &flac, "flac");
    let dir = env.engine.work_pcm().dir().to_path_buf();
    let hash = crate::library::hex(blake3::hash(&flac).as_bytes());
    let root = env.lib.layout().root().to_path_buf();
    let src = crate::serve::pcm_source(env.lib.layout(), &hash).unwrap();
    let err = crate::asset::pcm::serve(&env.engine, &root, &hash, &src, 0, 10).unwrap_err();
    assert_eq!(err.status(), 507, "{err}");
    assert!(
        matches!(&err, crate::asset::pcm::PcmError::NoSpace(m) if m.starts_with("ディスクの空き容量が足りません")),
        "{err}"
    );
    assert!(files(&dir).is_empty(), "{:?}", files(&dir));
    assert!(env.engine.pcm_cache().cached().is_none());
    assert_eq!(env.engine.pcm_cache().decodes(), 0);
    assert_eq!(env.usage().interactive, 0);
}

/// 段 4B の検証の直し 3：試聴の `/pcm` で作業用のファイルを作れない（ディスクの空きが足りない）とき、丸ごと読む見積もりが
/// 丸ごと読む上限に収まり、対話の枠で今すぐ確保できれば、M11 の丸ごと読みに落として鳴らす（応答は空きのあるライブラリの
/// 作業用のファイルからのものとバイト列まで同じ）。確保できなければ 507。
#[test]
fn pcm_without_disk_space_falls_back_to_the_whole_read() {
    // 予算 1 GiB：作業用のファイルの境は 64 MiB、丸ごと読む上限は 256 MiB。値は 64 MiB を少し超える
    let budget = 1u64 << 30;
    let frames = (64u64 << 20) / 4 + 4 * C;
    let bytes = plain_wav_bytes(8_000, frames);
    let hash = crate::library::hex(blake3::hash(&bytes).as_bytes());
    let ranges = [(0, 10), (frames - 1_000, frames), (5 * C + 7, 9 * C)];
    // 空きのあるライブラリ（作業用のファイルから。丸ごと読みと同じ値であることは A15）
    let want: Vec<Vec<u8>> = {
        let roomy = plain_wav_env(budget);
        add_audio(&roomy.lib, &bytes, "wav");
        let src = crate::serve::pcm_source(roomy.lib.layout(), &hash).unwrap();
        let root = roomy.lib.layout().root().to_path_buf();
        let got = ranges
            .iter()
            .map(|&(a, b)| {
                crate::asset::pcm::serve(&roomy.engine, &root, &hash, &src, a, b).unwrap()
            })
            .collect();
        assert_eq!(
            roomy.engine.work_pcm().usage().0,
            1,
            "作業用のファイルから読んだ"
        );
        got
    };
    let env = plain_wav_env(budget);
    set_free(&env, 1 << 20);
    add_audio(&env.lib, &bytes, "wav");
    let dir = env.engine.work_pcm().dir().to_path_buf();
    let src = crate::serve::pcm_source(env.lib.layout(), &hash).unwrap();
    let root = env.lib.layout().root().to_path_buf();
    let serve = |a: u64, b: u64| crate::asset::pcm::serve(&env.engine, &root, &hash, &src, a, b);
    // 丸ごと読む分が空いていない：507（作業用のファイル・覚えたものを残さない）
    let rt = env.engine.runtime().unwrap().clone();
    let held = rt
        .budget()
        .try_acquire(budget - (100 << 20), Class::Interactive)
        .unwrap()
        .unwrap();
    let err = serve(0, 10).unwrap_err();
    assert_eq!(err.status(), 507, "{err}");
    assert!(files(&dir).is_empty(), "{:?}", files(&dir));
    assert!(env.engine.pcm_cache().cached().is_none());
    drop(held);
    // 空いていれば丸ごと読む（作業用のファイルを作らない）
    for (&(a, b), want) in ranges.iter().zip(&want) {
        assert_eq!(&serve(a, b).unwrap(), want, "{a}-{b}");
    }
    assert_eq!(env.engine.pcm_cache().decodes(), 1, "1 回だけ読んだ");
    assert!(env.engine.pcm_cache().work().is_none(), "丸ごと読んだ");
    assert!(files(&dir).is_empty(), "{:?}", files(&dir));
    assert_eq!(
        env.usage().interactive,
        frames * 4,
        "覚えている値の分だけを持ち続ける"
    );
}

/// 段 4B の検証の直し 5：エディタを開く間に握る試聴の作業用のファイルは、同じ元のものだけ。別の元 X を試聴していたら、Y を
/// 開くときに X のファイルを手放してから Y の作業用のファイルを作る（Y の空きを確かめる時点で X のファイルはもうない）。
#[test]
fn opening_another_item_releases_the_auditioned_work_file_first() {
    let env = Env::with(Arc::new(Budgeted(SMALL)));
    let x = long_flac(40 * C);
    let y = long_flac(41 * C);
    add_audio(&env.lib, &x, "flac");
    let yid = add_audio(&env.lib, &y, "flac");
    let dir = env.engine.work_pcm().dir().to_path_buf();
    let hash = crate::library::hex(blake3::hash(&x).as_bytes());
    let src = crate::serve::pcm_source(env.lib.layout(), &hash).unwrap();
    crate::asset::pcm::serve(&env.engine, env.lib.layout().root(), &hash, &src, 0, 10).unwrap();
    let made = files(&dir);
    assert_eq!(made.len(), 1, "{made:?}");
    let x_path = made[0].clone();
    // 空きを測るたびに、X のファイルが残っているかを記す（空きは測れないことにして確かめを通す）
    let seen: Arc<Mutex<Vec<bool>>> = Arc::default();
    let (s, xp) = (seen.clone(), x_path.clone());
    env.engine.set_free_space(Some(Arc::new(move |_: &Path| {
        s.lock().unwrap().push(xp.exists());
        None
    }) as Arc<crate::library::upgrade::FreeSpaceFn>));
    let opened = env.open(yid);
    assert!(opened.source.audio.unwrap().lazy);
    assert_eq!(
        *seen.lock().unwrap(),
        vec![false],
        "Y の空きを確かめるときに X の作業用のファイルが残っていた"
    );
    assert!(!x_path.exists());
    assert_eq!(files(&dir).len(), 1, "Y の分だけ");
    assert!(env.engine.pcm_cache().cached().is_none());
}

/// 段 4B の検証の直し 4：置き場そのものの誤り（作る前の空きの確かめ）は `Core(NoSpace)` で、作業用のファイル・取り置きを
/// 残さず、エディタも残らない。元を読む側の Io（デコーダが返したもの）は、書き手の誤りと違って「元を読めません」（Failed）の
/// まま（書き手の Io の写しは `source::tests::write_errors_are_kept_apart_from_read_errors`）。
#[test]
fn work_file_errors_keep_their_kind() {
    let flac = long_flac(40 * C);
    let env = Env::with(Arc::new(Budgeted(SMALL)));
    set_free(&env, 1 << 20);
    let id = add_audio(&env.lib, &flac, "flac");
    let dir = env.engine.work_pcm().dir().to_path_buf();
    let err = open_err(&env, id);
    assert!(
        matches!(&err, EditorError::Core(CoreError::NoSpace { .. })),
        "{err}"
    );
    assert!(
        err.to_string()
            .starts_with("ディスクの空き容量が足りません")
    );
    assert!(files(&dir).is_empty(), "{:?}", files(&dir));
    let u = env.usage();
    assert_eq!((u.reserved, u.interactive, u.standing), (0, 0, 0));
    assert!(env.engine.current_editor().is_none());

    let env = flac_env(SMALL, || {
        vec![Arc::new(
            Stage::renamed(pinned_decoder("flac"), "eio-flac-dec/1", "flac")
                .refusing(|| EngineError::Io(std::io::Error::from_raw_os_error(5))),
        )]
    });
    let id = add_audio(&env.lib, &flac, "flac");
    let dir = env.engine.work_pcm().dir().to_path_buf();
    let err = open_err(&env, id);
    assert!(
        matches!(&err, EditorError::Failed(m) if m.starts_with("元を読めません")),
        "{err}"
    );
    assert!(files(&dir).is_empty(), "{:?}", files(&dir));
    let u = env.usage();
    assert_eq!((u.reserved, u.interactive, u.standing), (0, 0, 0));
    assert!(env.engine.current_editor().is_none());
}

fn refuse_a() -> EngineError {
    EngineError::Unsupported("段 A では読まない".into())
}

fn refuse_b() -> EngineError {
    EngineError::Unsupported("段 B では読まない".into())
}

/// 形は固定の FLAC と同じに読み、流して読むと `refuse` で断る段。
fn refusing_flac(id: &'static str, refuse: fn() -> EngineError) -> Arc<dyn Decoder> {
    Arc::new(Stage::renamed(pinned_decoder("flac"), id, "flac").refusing(refuse))
}

/// 段 4B の検証の直し 6（#3・#10）：形を読めた段が流して読む途中で `Unsupported` を返したら、次の段で形を読み直し、その段の
/// 見積もりで確保し直して作業用のファイルを作る。記録・値は丸ごと読み（`CodecSet::decode`。段の順で同じ後ろの段が読む）と
/// 同じで、作り終えたら対話の確保は 0 に戻る。エディタも後ろの段の記録・指紋で開く。
#[test]
fn a_stage_refusing_the_stream_falls_through_to_the_next_stage() {
    let env = flac_env(SMALL, || {
        vec![
            refusing_flac("refusing-flac-dec/1", refuse_a),
            pinned_decoder("flac"),
        ]
    });
    let flac = long_flac(40 * C);
    let id = add_audio(&env.lib, &flac, "flac");
    let dir = env.engine.work_pcm().dir().to_path_buf();
    let hash = *blake3::hash(&flac).as_bytes();
    let path = env.lib.layout().content_path(&hash, "flac");
    let runtime = env.engine.runtime().unwrap().clone();
    let codecs = env.engine.codecs();
    let (opts, limits) = (DecodeOptions::default(), Limits::default());
    let cx = WorkCx {
        runtime: &runtime,
        codecs: &codecs,
        store: env.engine.work_pcm(),
        path: &path,
        source: Digest(hash),
        format: "flac",
        opts: &opts,
        limits: &limits,
        mon: &NoMonitor,
    };
    let (shape, stage) = read_shape_staged(&cx, None).unwrap().unwrap();
    assert_eq!(stage.id, "refusing-flac-dec/1", "形は前の段で読める");
    let handle = make_work(&cx, shape, stage).unwrap();
    assert_eq!(
        runtime.budget().usage().unwrap().interactive,
        0,
        "作る間の確保を返した"
    );
    let (whole, stamp) = codecs
        .decode(&mut Cursor::new(&flac), "flac", &opts, &limits, &NoMonitor)
        .unwrap();
    assert_eq!(stamp.codec, "flac-symphonia-dec/1");
    assert_eq!(
        handle.pcm().decoded,
        stamp,
        "記録は後ろの段のもの（丸ごと読みと同じ）"
    );
    let made = files(&dir);
    assert_eq!(made.len(), 1, "{made:?}");
    assert!(made[0].extension().is_some_and(|x| x == "wav"));
    let opened = open_work(
        handle,
        &limits,
        runtime.preview_threads(),
        runtime.budget().total(),
    )
    .unwrap();
    let (Value::Audio(got), Value::Audio(whole)) = (&opened.value, &whole) else {
        panic!("音声ではありません")
    };
    same_samples(got, whole);
    assert_eq!(got.meta, whole.meta);
    drop(opened);
    assert!(files(&dir).is_empty(), "{:?}", files(&dir));

    let opened = env.open(id);
    let a = opened.source.audio.clone().unwrap();
    assert!(a.lazy);
    assert_eq!(a.decoder, stamp.codec);
    let editor = env.editor(&opened.session);
    let guard = editor.lock_state();
    assert_eq!(
        guard.as_ref().unwrap().inputs[&InputId(0)].fingerprint,
        Fingerprint::of_decoded(&stamp)
    );
}

/// 段 4B の検証の直し 6（#3・#10）：どの段も流しを断る組では「元を読めません」（Failed）で断り、書きかけ・作業用のファイル・
/// 取り置き・確保を残さず、エディタも残らない。断る段が 1 つならその段の誤り。2 つ以上なら最後に断った段の誤り（丸ごと
/// 読みの `decode_any`・段 2 の解析のジョブと同じ）。
#[test]
fn every_stage_refusing_the_stream_fails_without_leftovers() {
    let flac = long_flac(40 * C);
    for two in [false, true] {
        let stages: fn() -> Vec<Arc<dyn Decoder>> = if two {
            || {
                vec![
                    refusing_flac("refusing-flac-dec/1", refuse_a),
                    refusing_flac("refusing2-flac-dec/1", refuse_b),
                ]
            }
        } else {
            || vec![refusing_flac("refusing-flac-dec/1", refuse_a)]
        };
        let env = flac_env(SMALL, stages);
        let id = add_audio(&env.lib, &flac, "flac");
        let dir = env.engine.work_pcm().dir().to_path_buf();
        let err = open_err(&env, id);
        let want = if two { "段 B" } else { "段 A" };
        assert!(
            matches!(&err, EditorError::Failed(m) if m.starts_with("元を読めません") && m.contains(want)),
            "{two}: {err}"
        );
        assert!(files(&dir).is_empty(), "{:?}", files(&dir));
        assert_eq!(env.engine.work_pcm().usage().0, 0);
        let u = env.usage();
        assert_eq!((u.reserved, u.interactive, u.standing), (0, 0, 0));
        assert!(env.engine.current_editor().is_none());
    }
}

/// 段 4B の検証の直し 6（#10）：流して読む段の道具がない（`CodecMissing`）ときは、次の段へ回さずに ToolMissing で断る（丸ごと
/// 読みと同じ）。書きかけ・取り置き・確保を残さない。
#[test]
fn a_missing_tool_is_reported_as_tool_missing() {
    let env = flac_env(SMALL, || {
        vec![
            Arc::new(
                Stage::renamed(pinned_decoder("flac"), "fake-ffmpeg-dec/1", "flac").refusing(
                    || EngineError::CodecMissing {
                        tool: "ffmpeg".into(),
                    },
                ),
            ),
            pinned_decoder("flac"),
        ]
    });
    let flac = long_flac(40 * C);
    let id = add_audio(&env.lib, &flac, "flac");
    let dir = env.engine.work_pcm().dir().to_path_buf();
    let err = open_err(&env, id);
    assert!(
        matches!(&err, EditorError::ToolMissing(t) if t == "ffmpeg"),
        "{err}"
    );
    assert_eq!(err.problem().code, ProblemCode::ToolMissing);
    assert!(files(&dir).is_empty(), "{:?}", files(&dir));
    let u = env.usage();
    assert_eq!((u.reserved, u.interactive, u.standing), (0, 0, 0));
    assert!(env.engine.current_editor().is_none());
}
