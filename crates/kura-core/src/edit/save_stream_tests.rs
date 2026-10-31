//! 音声の保存を流して書く道の受け入れテスト（M13 段 3。docs/editing.md §14.1 の S18〜S20）。固定の段のコーデックと偽の
//! Platform のデコーダ・偽の AAC・偽の ffmpeg で、Linux でも回る。どのテストも終わりに `assert_consistent` で締める。

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};

use kura_engine::codec::platform::PlatformAacEncoder;
use kura_engine::codec::{CodecEnv, CodecSet, Decoder, EncodedStamp, ReadSeek, Tier};
use kura_engine::doc::{DecodeOptions, FormatChoice, InputId};
use kura_engine::render::InputValue;
use kura_engine::testgen::InputGen;
use kura_engine::testing::platform::{FakeAacBackend, FakePlatformDecoder};
use kura_engine::{Digest, Engine, EngineError, Graph, Limits, Monitor, NoMonitor, Shape, Value};
use rusqlite::params;
use serde_json::{Value as Json, json};

use super::save::{SaveAction, Stage};
use super::save_tests::{
    Env, Setup, WAIT, assert_consistent, assert_done, base_of, gc_version, hash_of, head,
    result_of, staged, states, versions_of,
};
use super::services::fake::FakeEngineFactory;
use super::versions::FileState;
use crate::codec_ext::ffmpeg::testing::FakeTools;
use crate::jobs::JobState;
use crate::library::Library;

/// 音声のアイテムのファイルを置いて行を入れる（サムネイル・解析は済んだものとして）。
fn add_audio(lib: &Library, bytes: &[u8], ext: &str, title: &str) -> (i64, [u8; 32]) {
    let hash = hash_of(bytes);
    let path = lib.layout().content_path(&hash, ext);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, bytes).unwrap();
    let (ext, title, size) = (ext.to_string(), title.to_string(), bytes.len() as i64);
    let id = lib
        .db()
        .write(move |tx| {
            tx.execute(
                "INSERT INTO items (hash, ext, kind, mime, size_bytes, title, original_name, original_path,
                                    source_site, imported_at, updated_at, thumb_state, proxy_state, analysis_state)
                 VALUES (?1, ?2, 'audio', ?3, ?4, ?5, ?5 || '.' || ?2, '/src/' || ?5, 'local', 1, 1, 'ready',
                         'not_needed', 'ready')",
                params![&hash[..], ext, crate::import::mime_for(&ext), size, title],
            )?;
            Ok(tx.last_insert_rowid())
        })
        .unwrap();
    (id, hash)
}

/// 8 kHz の雑音を `format` で書いたもの。
fn noise_file(codecs: &CodecSet, format: &str, channels: u16, frames: u64, seed: u64) -> Vec<u8> {
    let v = InputGen::AudioNoise {
        rate: 8_000,
        channels,
        frames,
        seed,
    }
    .generate()
    .unwrap();
    codecs
        .encode_bytes(&v, format, &json!({ "bits": 16 }), &NoMonitor)
        .unwrap()
        .0
}

/// 元に対する累積の音声の文書（`to` があれば出力の形式）。
fn audio_doc(db: f64, to: Option<&str>) -> Graph {
    let mut g = Graph::new_single();
    g.push_linear("audio.gain", 1, json!({ "db": db }), 1)
        .unwrap();
    g.push_linear(
        "audio.fade",
        1,
        json!({ "fadeIn": 1_000, "fadeOut": 3_000, "curve": "linear" }),
        2,
    )
    .unwrap();
    g.push_linear(
        "audio.normalize",
        1,
        json!({ "target": -18.0, "peakLimit": -1.0 }),
        3,
    )
    .unwrap();
    if let Some(format) = to {
        g.outputs[0].format = FormatChoice::To {
            format: format.into(),
            options: json!({}),
        };
    }
    g
}

/// 今までの道（値を描いて `encode_bytes`）で書いたもの：（バイト列, 書き出しの記録, 値のダイジェスト, 警告の文）。
fn whole_path(
    codecs: &CodecSet,
    value: Value,
    source: [u8; 32],
    g: &Graph,
    out_format: &str,
) -> (Vec<u8>, EncodedStamp, Digest, Vec<String>) {
    let engine = Engine::new().unwrap();
    let plan = engine
        .compile(g, &BTreeMap::from([(InputId(0), value.shape())]))
        .unwrap();
    let inputs = BTreeMap::from([(
        InputId(0),
        InputValue {
            value,
            source: Digest(source),
        },
    )]);
    let out = engine.render(&plan, &inputs, &NoMonitor).unwrap();
    let (bytes, stamp) = codecs
        .encode_bytes(&out.outputs[0].value, out_format, &json!({}), &NoMonitor)
        .unwrap();
    let warnings = out.warnings.iter().map(|w| w.message.clone()).collect();
    (bytes, stamp, out.outputs[0].digest, warnings)
}

/// 元 `bytes`（形式 `format`）を固定の段で読んだ値。
fn decode_pinned(bytes: &[u8], format: &str) -> Value {
    CodecSet::pinned()
        .decode(
            &mut std::io::Cursor::new(bytes),
            format,
            &Default::default(),
            Engine::new().unwrap().limits(),
            &NoMonitor,
        )
        .unwrap()
        .0
}

/// 今の版のファイルと記録。
fn current(env: &Env, item: i64) -> (Vec<u8>, super::versions::VersionRecord, Option<Digest>) {
    let h = head(&env.lib, item);
    let bytes = std::fs::read(env.lib.layout().content_path(&h.hash, &h.ext)).unwrap();
    let v = versions_of(&env.lib, item).pop().unwrap();
    (bytes, v.record().unwrap(), v.value_digest)
}

/// 保存の組（固定の段 ＋ 偽の AAC（m4a）＋ 偽の ffmpeg（mp3。標準入力の cksum を書く））。
fn save_codecs(tools: &FakeTools) -> CodecSet {
    let mut set = CodecSet::pinned();
    set.add_encoder(Arc::new(PlatformAacEncoder::new(FakeAacBackend::new())))
        .unwrap();
    tools.codecs().add_to(&mut set).unwrap();
    set
}

/// S18：音声の上書きを流して書く（WAV の元は源、FLAC の元は作業用の PCM から。出力は WAV・AIFF・FLAC・ogg・偽の AAC の
/// m4a・偽の ffmpeg の mp3）と、今の道（`render` → `encode_bytes`）とバイト列・書き出しの記録・値のダイジェスト・警告が同じ。
/// 作業用の PCM は保存のあと残らない。
#[test]
fn s18_streamed_audio_save_equals_the_whole_path() {
    let tools = FakeTools::new("exit 1", "exit 1", "cksum > \"$out\"");
    let set = save_codecs(&tools);
    let env = Env::with(
        FakeEngineFactory::with_codecs(Arc::new(Mutex::new(save_codecs(&tools)))),
        None,
    );
    let pinned = CodecSet::pinned();
    let mut checked = 0;
    // 偽の AAC は f32 をそのまま書くので、m4a の上限（AAC の大きさの見積もり）に収まる短い元で
    let cases: [(&str, u16, u64, &[Option<&str>]); 3] = [
        (
            "wav",
            2,
            3 * 65_536 + 77,
            &[None, Some("aiff"), Some("flac"), Some("ogg"), Some("mp3")],
        ),
        ("wav", 2, 9_000, &[Some("m4a")]),
        ("flac", 1, 3 * 65_536 + 77, &[Some("wav"), Some("ogg")]),
    ];
    for (format_in, channels, frames, outs) in cases {
        let original = noise_file(&pinned, format_in, channels, frames, 18 + frames);
        let (item, hash) = add_audio(&env.lib, &original, format_in, format_in);
        let value = decode_pinned(&original, format_in);
        for (k, to) in outs.iter().enumerate() {
            let g = audio_doc(-1.0 - k as f64, *to);
            let out_format = to.unwrap_or(format_in);
            let (want, stamp, digest, warnings) =
                whole_path(&set, value.clone(), hash, &g, out_format);
            let record = env.overwrite(item, g);
            assert_done(&record);
            let (got, rec, value_digest) = current(&env, item);
            assert!(
                got == want,
                "{format_in} → {out_format}：バイト列が違う（{} と {}）",
                got.len(),
                want.len()
            );
            assert_eq!(
                rec.encoded.as_ref(),
                Some(&stamp),
                "{format_in} → {out_format}"
            );
            assert_eq!(value_digest, Some(digest), "{format_in} → {out_format}");
            assert_eq!(rec.warnings, warnings, "{format_in} → {out_format}");
            assert_eq!(result_of(&record)["anchor"], Json::Null);
            assert_eq!(
                env.engine.work_pcm().usage(),
                (0, 0),
                "作業用の PCM が残っている"
            );
            checked += 1;
        }
    }
    assert_eq!(checked, 8);
    assert_consistent(&env.lib);
}

/// 偽の Platform のデコーダ（opus。8 kHz の雑音を読むたびに少しずつ変えて返す）。
fn opus_decoder(channels: u16, frames: u64) -> Arc<FakePlatformDecoder> {
    Arc::new(
        FakePlatformDecoder::new(
            "fake-opus-dec/1",
            &["opus"],
            InputGen::AudioNoise {
                rate: 8_000,
                channels,
                frames,
                seed: 19,
            },
        )
        .drifting(),
    )
}

/// S19：Platform で読む音声（偽のデコーダ）の最初の保存は、作業用のファイルを写してアンカーに昇格する（アンカーは f32 の
/// WAV。照らしたもの）。OS のデコーダを外しても、アンカーから作り直せる（S15 の音声版）。
#[test]
fn s19_platform_audio_first_save_promotes_the_work_file() {
    let decoder = opus_decoder(2, 2 * 65_536 + 11);
    let codecs = {
        let mut set = CodecSet::pinned();
        set.add_decoder(decoder.clone()).unwrap();
        Arc::new(Mutex::new(set))
    };
    let env = Env::with(FakeEngineFactory::with_codecs(codecs.clone()), None);
    let (item, hash) = add_audio(&env.lib, b"fake opus bytes", "opus", "声");
    let first = decoder.value_for(0).unwrap();
    let g = audio_doc(-2.0, Some("wav"));
    let (want, stamp, _, _) = whole_path(&CodecSet::pinned(), first.clone(), hash, &g, "wav");
    let record = env.overwrite(item, g);
    assert_done(&record);
    assert_eq!(result_of(&record)["anchor"], "created");
    assert_eq!(decoder.reads(), 1);
    let (got, rec, _) = current(&env, item);
    assert!(got == want, "最初の保存のバイト列が違う");
    assert_eq!(rec.encoded.as_ref(), Some(&stamp));
    let (anchor_hash, ext): (Vec<u8>, String) = env
        .lib
        .db()
        .read(|c| {
            Ok(c.query_row(
                "SELECT b.hash, b.ext FROM edit_anchors a JOIN blobs b ON b.id = a.blob_id",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?)
        })
        .unwrap();
    assert_eq!(ext, "wav");
    let anchor_hash: [u8; 32] = anchor_hash.try_into().unwrap();
    // アンカーは値を anchor::write で書いたものと同じバイト列（封をした作業用のファイル）
    let anchor_bytes = std::fs::read(env.lib.layout().content_path(&anchor_hash, "wav")).unwrap();
    let mut written = Vec::new();
    let decoded = env
        .lib
        .db()
        .read(|c| {
            Ok(c.query_row("SELECT decoded FROM edit_anchors", [], |r| {
                r.get::<_, String>(0)
            })?)
        })
        .unwrap();
    let decoded: kura_engine::codec::DecodeStamp = serde_json::from_str(&decoded).unwrap();
    kura_engine::codec::anchor::write(
        &first,
        &decoded,
        &kura_engine::codec::anchor::AnchorContext {
            source: Some(Digest(hash)),
            format: Some("opus".into()),
            options: Default::default(),
        },
        Engine::new().unwrap().limits(),
        &mut written,
        &NoMonitor,
    )
    .unwrap();
    assert!(anchor_bytes == written, "アンカーが anchor::write と違う");
    assert_eq!(env.engine.work_pcm().usage(), (0, 0));
    assert_consistent(&env.lib);

    // OS のデコーダを外して 2 回目：アンカーから読む（読み直さない）
    *codecs.lock().unwrap() = CodecSet::pinned();
    let g = audio_doc(-4.0, Some("flac"));
    let (want, _, _, _) = whole_path(&CodecSet::pinned(), first, hash, &g, "flac");
    let record = env.overwrite(item, g);
    assert_done(&record);
    assert_eq!(result_of(&record)["anchor"], "used");
    assert_eq!(decoder.reads(), 1);
    let (got, _, _) = current(&env, item);
    assert!(got == want, "アンカーから作り直したバイト列が違う");
    assert_consistent(&env.lib);
}

/// S20 で止める所。
#[derive(Debug, Clone, Copy)]
enum StopAt {
    /// 書く回の途中：その回の最初の窓を書き手に渡したあと（`second` なら 2 回目の回。ogg の 1 回目は後ろ向きの回）。
    Writing { second: bool },
    /// 作業用の PCM を作る途中（偽の Platform のデコーダの中）。
    Decoding,
    /// R2 の読み直しの途中（最初の区切りを読んだあと）。
    Rehash,
    /// 書き終えた所（R2 のあと、止める指示を見る前）。
    Written,
}

/// S20：窓が 2 つ以上ある音声（8 kHz で 1.1M・2.3M フレーム）の保存を、書いている途中（書く回の窓の合間。ogg は後ろ向きの回と
/// 前向きの回）・作業用の PCM を作る途中・R2 の読み直しの途中・書き終えた所で一時停止 → 再開・中止する。止めたジョブは
/// Paused／Canceled（Failed ではない）で、止めた所より先へ進まず、staging に .part は残らない。一時停止は作業用の PCM を残し
/// （作る途中なら書きかけは残さない）、再開すると初めから書いて作業用の PCM を作り直さない（デコーダが読んだ数が 1 のまま）。
/// 中止は何も残らない。
#[test]
fn s20_long_audio_pause_restarts_and_reuses_the_work_file() {
    let cases = [
        (StopAt::Writing { second: false }, "wav", 2_300_000),
        (StopAt::Writing { second: false }, "ogg", 1_100_000),
        (StopAt::Writing { second: true }, "ogg", 1_100_000),
        (StopAt::Decoding, "wav", 1_100_000),
        (StopAt::Rehash, "wav", 1_100_000),
        (StopAt::Written, "wav", 2_300_000),
    ];
    for (at, out, frames) in cases {
        for pause in [true, false] {
            s20_case(at, out, frames, pause);
        }
    }
}

fn s20_case(at: StopAt, out: &str, frames: u64, pause: bool) {
    let what = format!(
        "{at:?} → {out}・{frames} フレーム（{}）",
        if pause { "一時停止" } else { "中止" }
    );
    let decoder = opus_decoder(1, frames);
    let (gated, in_decoder) = gated_decoder(decoder.clone());
    let mut set = CodecSet::pinned();
    set.add_decoder(gated).unwrap();
    let (env, env_gate) = gated_env(Arc::new(Mutex::new(set)), move |stage| match (at, stage) {
        (StopAt::Writing { second }, Stage::Window { fed, frames }) => {
            if second {
                fed > frames && fed < 2 * frames
            } else {
                fed < frames
            }
        }
        (StopAt::Rehash, Stage::Rehash) | (StopAt::Written, Stage::Written) => true,
        _ => false,
    });
    let (item, _) = add_audio(&env.lib, b"long fake opus", "opus", "長い声");
    let gate = match at {
        StopAt::Decoding => &in_decoder,
        _ => &env_gate,
    };
    gate.arm();
    let id = env.start(
        item,
        base_of(&env.lib, item),
        SaveAction::Overwrite {
            graph: audio_doc(-3.0, Some(out)),
        },
    );
    gate.wait(&env, id);
    if pause {
        env.jobs.pause(id).unwrap();
    } else {
        env.jobs.cancel(id).unwrap();
    }
    gate.open();
    assert!(env.jobs.wait_idle(4 * WAIT), "{what}");
    let record = env.jobs.get(id).unwrap();
    let state = if pause {
        JobState::Paused
    } else {
        JobState::Canceled
    };
    assert_eq!(record.state, state, "{what}：{:?}", record.message);
    // 止めた所より先へ進んでいない（書く途中で止めたら R2 も書き終えた所も通らない）
    let seen = env_gate.seen();
    let beyond = |s: &Stage| match at {
        StopAt::Writing { .. } => matches!(s, Stage::Rehash | Stage::Written),
        StopAt::Decoding => matches!(s, Stage::Window { .. } | Stage::Rehash | Stage::Written),
        StopAt::Rehash => matches!(s, Stage::Written),
        StopAt::Written => false,
    };
    assert!(!seen.iter().any(beyond), "{what}：止めずに進んだ {seen:?}");
    let parts: Vec<_> = staged(&env.lib)
        .into_iter()
        .filter(|p| p.extension().is_some_and(|x| x == "part"))
        .collect();
    assert!(parts.is_empty(), "{what}：{parts:?}");
    // 作る途中で止めたら、デコーダは読み終えていない（書きかけの作業用の PCM は残さない）
    let decoding = matches!(at, StopAt::Decoding);
    assert_eq!(decoder.reads(), u64::from(!decoding), "{what}");
    if pause {
        assert_eq!(
            env.engine.work_pcm().usage().0,
            usize::from(!decoding),
            "{what}：一時停止で残す作業用の PCM"
        );
        env.jobs.resume(id).unwrap();
        assert!(env.jobs.wait_idle(4 * WAIT), "{what}");
        let record = env.jobs.get(id).unwrap();
        assert_done(&record);
        assert_eq!(decoder.reads(), 1, "{what}：作業用の PCM を作り直した");
        assert_eq!(result_of(&record)["anchor"], "created", "{what}");
    }
    assert_eq!(env.engine.work_pcm().usage(), (0, 0), "{what}");
    assert_consistent(&env.lib);
}

/// 読む途中で門に止まる偽の Platform のデコーダ（`inner` に読ませる前に、構えていればテストに知らせて「進め」を待ち、止める
/// 指示が出ていれば中止を返す）。作業用の PCM を作る途中の一時停止・中止を写す。
struct GatedDecoder {
    inner: Arc<FakePlatformDecoder>,
    reached: Mutex<mpsc::Sender<()>>,
    go: Mutex<mpsc::Receiver<()>>,
    armed: Arc<AtomicBool>,
}

/// `inner` を包んだ [`GatedDecoder`] と、その門（通った段は覚えない）。
fn gated_decoder(inner: Arc<FakePlatformDecoder>) -> (Arc<GatedDecoder>, Gate) {
    let (reached_tx, reached_rx) = mpsc::channel::<()>();
    let (go_tx, go_rx) = mpsc::channel::<()>();
    let armed = Arc::new(AtomicBool::new(false));
    (
        Arc::new(GatedDecoder {
            inner,
            reached: Mutex::new(reached_tx),
            go: Mutex::new(go_rx),
            armed: armed.clone(),
        }),
        Gate {
            reached: reached_rx,
            go: go_tx,
            armed,
            seen: Arc::new(Mutex::new(Vec::new())),
        },
    )
}

impl Decoder for GatedDecoder {
    fn id(&self) -> &'static str {
        self.inner.id()
    }

    fn tier(&self) -> Tier {
        self.inner.tier()
    }

    fn reproducible(&self) -> bool {
        false
    }

    fn env(&self) -> Option<CodecEnv> {
        self.inner.env()
    }

    fn can_decode(&self, format: &str) -> bool {
        self.inner.can_decode(format)
    }

    fn shape(
        &self,
        src: &mut dyn ReadSeek,
        format: &str,
        opts: &DecodeOptions,
        limits: &Limits,
    ) -> Result<Shape, EngineError> {
        self.inner.shape(src, format, opts, limits)
    }

    fn decode(
        &self,
        src: &mut dyn ReadSeek,
        format: &str,
        opts: &DecodeOptions,
        limits: &Limits,
        mon: &dyn Monitor,
    ) -> Result<Value, EngineError> {
        if self.armed.swap(false, Ordering::SeqCst) {
            self.reached.lock().unwrap().send(()).unwrap();
            let _ = self.go.lock().unwrap().recv();
            if mon.canceled() {
                return Err(EngineError::Canceled);
            }
        }
        self.inner.decode(src, format, opts, limits, mon)
    }
}

/// 割り込みの門：構えたあと、`at` が真になる段に初めて来たら、テストに知らせて「進め」を待つ（一度だけ。構え直せる）。
/// 門を開けたあとに通った段を覚える。
struct Gate {
    reached: mpsc::Receiver<()>,
    go: mpsc::Sender<()>,
    armed: Arc<AtomicBool>,
    seen: Arc<Mutex<Vec<Stage>>>,
}

impl Gate {
    fn arm(&self) {
        self.armed.store(true, Ordering::SeqCst);
    }

    /// ジョブ `id` が門に来るのを待つ（来る前にジョブが終わったら、その結果で落とす）。
    fn wait(&self, env: &Env, id: i64) {
        let until = std::time::Instant::now() + 4 * WAIT;
        loop {
            match self
                .reached
                .recv_timeout(std::time::Duration::from_millis(50))
            {
                Ok(()) => return,
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    let record = env.jobs.get(id).unwrap();
                    assert!(
                        !record.state.is_terminal() && record.state != JobState::Paused,
                        "門に来る前にジョブが終わった：{:?} {:?} {:?}",
                        record.state,
                        record.error_code,
                        record.message
                    );
                    assert!(std::time::Instant::now() < until, "門に来ない");
                }
                Err(e) => panic!("{e}"),
            }
        }
    }

    /// 門を開ける（止めた段から進める）。ここから通った段を覚え直す。
    fn open(&self) {
        self.seen.lock().unwrap().clear();
        self.go.send(()).unwrap();
    }

    /// 門を開けたあとに通った段。
    fn seen(&self) -> Vec<Stage> {
        self.seen.lock().unwrap().clone()
    }
}

/// 割り込みの門つきの Env（コーデックの組は `codecs`）。
fn gated_env(
    codecs: Arc<Mutex<CodecSet>>,
    at: impl Fn(Stage) -> bool + Send + Sync + 'static,
) -> (Env, Gate) {
    let (reached_tx, reached_rx) = mpsc::channel::<()>();
    let (go_tx, go_rx) = mpsc::channel::<()>();
    let armed = Arc::new(AtomicBool::new(false));
    let seen = Arc::new(Mutex::new(Vec::new()));
    let env = Env::setup(
        Setup {
            factory: FakeEngineFactory::with_codecs(codecs).with_budget(256 << 20),
            ..Setup::default()
        },
        {
            let (armed, seen) = (armed.clone(), seen.clone());
            move |_| {
                let (reached, go) = (Mutex::new(reached_tx), Mutex::new(go_rx));
                Some(Arc::new(move |stage| {
                    seen.lock().unwrap().push(stage);
                    if at(stage) && armed.swap(false, Ordering::SeqCst) {
                        reached.lock().unwrap().send(()).unwrap();
                        let _ = go.lock().unwrap().recv();
                    }
                }))
            }
        },
    );
    (
        env,
        Gate {
            reached: reached_rx,
            go: go_tx,
            armed,
            seen,
        },
    )
}

/// 偽の Platform のデコーダ（opus）だけを足した組。
fn opus_codecs(decoder: &Arc<FakePlatformDecoder>) -> Arc<Mutex<CodecSet>> {
    let mut set = CodecSet::pinned();
    set.add_decoder(decoder.clone()).unwrap();
    Arc::new(Mutex::new(set))
}

/// 一時停止したまま中止した保存は、残していた作業用の PCM を消す（GC のジョブ・閉じるのを待たない。M13 段 3 の検証の
/// 直し #5）。
#[test]
fn paused_then_canceled_save_removes_the_work_file() {
    let decoder = opus_decoder(1, 2 * 65_536 + 11);
    let (env, gate) = gated_env(opus_codecs(&decoder), |s| s == Stage::Written);
    let (item, _) = add_audio(&env.lib, b"paused fake opus", "opus", "止めた声");
    gate.arm();
    let id = env.start(
        item,
        base_of(&env.lib, item),
        SaveAction::Overwrite {
            graph: audio_doc(-3.0, Some("wav")),
        },
    );
    gate.wait(&env, id);
    env.jobs.pause(id).unwrap();
    gate.open();
    assert!(env.jobs.wait_idle(WAIT));
    assert_eq!(env.jobs.get(id).unwrap().state, JobState::Paused);
    assert_eq!(env.engine.work_pcm().usage().0, 1, "一時停止は残す");
    env.jobs.cancel(id).unwrap();
    assert!(env.jobs.wait_idle(WAIT));
    assert_eq!(env.jobs.get(id).unwrap().state, JobState::Canceled);
    assert_eq!(
        env.engine.work_pcm().usage(),
        (0, 0),
        "中止したら作業用の PCM を消す"
    );
    assert_consistent(&env.lib);
}

/// 作業用の PCM をエディタと共有していても、一時停止した保存の残す印は捨てない：エディタを閉じても残り、再開で読み直さない
/// （M13 段 3 の検証の直し #9）。
#[test]
fn paused_save_keeps_the_work_file_shared_with_the_editor() {
    use super::anchors::options_digest;
    use super::services::{LentAnchor, LentInput};
    use super::work_pcm::WorkKey;
    use kura_engine::codec::anchor::{self, AnchorContext};
    use kura_engine::doc::DecodeOptions;
    let decoder = opus_decoder(1, 2 * 65_536 + 11);
    let (env, gate) = gated_env(opus_codecs(&decoder), |s| s == Stage::Written);
    let (item, hash) = add_audio(&env.lib, b"shared fake opus", "opus", "共有の声");
    // エディタと同じ鍵で作業用の PCM を作って貸す
    let set = env.engine.codecs();
    let path = env.lib.layout().content_path(&hash, "opus");
    let opts = DecodeOptions::default();
    let limits = Engine::new().unwrap().limits().clone();
    let shape = set.shape_path(&path, "opus", &opts, &limits).unwrap().0;
    let work = env
        .engine
        .work_pcm()
        .ensure(
            &WorkKey {
                source: hash,
                format: "opus".into(),
                options: opts.clone(),
                decoder: None,
            },
            &shape,
            AnchorContext {
                source: Some(Digest(hash)),
                format: Some("opus".into()),
                options: opts.clone(),
            },
            |sink| set.decode_stream_path(&path, "opus", &opts, &limits, sink, &NoMonitor),
            &limits,
            &NoMonitor,
        )
        .unwrap();
    let opened = anchor::open_source(&work.opener(), &limits, 64 << 20)
        .unwrap()
        .unwrap();
    env.engine.lend_input(
        "editor-9",
        LentInput {
            item_id: item,
            source: hash,
            options_digest: Digest(options_digest(&opts).unwrap()),
            format: "opus".into(),
            value: opened.value,
            decoded: work.pcm().decoded.clone(),
            anchor: LentAnchor::None,
            work: Some(work),
            verified: true,
        },
    );
    assert_eq!(decoder.reads(), 1);
    gate.arm();
    let id = env.start(
        item,
        base_of(&env.lib, item),
        SaveAction::Overwrite {
            graph: audio_doc(-3.0, Some("wav")),
        },
    );
    gate.wait(&env, id);
    env.jobs.pause(id).unwrap();
    gate.open();
    assert!(env.jobs.wait_idle(WAIT));
    assert_eq!(env.jobs.get(id).unwrap().state, JobState::Paused);
    // エディタを閉じる（貸したものを引き上げる）
    env.engine.withdraw_input("editor-9");
    assert_eq!(
        env.engine.work_pcm().usage().0,
        1,
        "エディタを閉じたら消えた"
    );
    env.jobs.resume(id).unwrap();
    assert!(env.jobs.wait_idle(WAIT));
    assert_done(&env.jobs.get(id).unwrap());
    assert_eq!(decoder.reads(), 1, "再開で元を読み直した");
    assert_eq!(env.engine.work_pcm().usage(), (0, 0));
    assert_consistent(&env.lib);
}

/// 読んだ数を数える包み（固定の段のデコーダを `CodecSet::replace_decoder` で差し替える。識別子・段・約束・読み方は同じ）。
struct CountingDecoder {
    inner: Arc<dyn Decoder>,
    reads: std::sync::atomic::AtomicU64,
}

impl CountingDecoder {
    fn reads(&self) -> u64 {
        self.reads.load(Ordering::SeqCst)
    }
}

impl Decoder for CountingDecoder {
    fn id(&self) -> &'static str {
        self.inner.id()
    }

    fn tier(&self) -> Tier {
        self.inner.tier()
    }

    fn reproducible(&self) -> bool {
        self.inner.reproducible()
    }

    fn env(&self) -> Option<CodecEnv> {
        self.inner.env()
    }

    fn can_decode(&self, format: &str) -> bool {
        self.inner.can_decode(format)
    }

    fn work_bytes(&self, format: &str, shape: &Shape) -> u64 {
        self.inner.work_bytes(format, shape)
    }

    fn file_work_bytes(&self, format: &str, shape: &Shape, len: u64) -> u64 {
        self.inner.file_work_bytes(format, shape, len)
    }

    fn shape(
        &self,
        src: &mut dyn ReadSeek,
        format: &str,
        opts: &DecodeOptions,
        limits: &Limits,
    ) -> Result<Shape, EngineError> {
        self.inner.shape(src, format, opts, limits)
    }

    fn decode(
        &self,
        src: &mut dyn ReadSeek,
        format: &str,
        opts: &DecodeOptions,
        limits: &Limits,
        mon: &dyn Monitor,
    ) -> Result<Value, EngineError> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        self.inner.decode(src, format, opts, limits, mon)
    }

    fn shape_file(
        &self,
        path: &std::path::Path,
        format: &str,
        opts: &DecodeOptions,
        limits: &Limits,
    ) -> Result<Shape, EngineError> {
        self.inner.shape_file(path, format, opts, limits)
    }

    fn shape_file_monitored(
        &self,
        path: &std::path::Path,
        format: &str,
        opts: &DecodeOptions,
        limits: &Limits,
        mon: &dyn Monitor,
    ) -> Result<Shape, EngineError> {
        self.inner
            .shape_file_monitored(path, format, opts, limits, mon)
    }

    fn streams(&self, format: &str) -> bool {
        self.inner.streams(format)
    }

    fn stream_work_bytes(&self, format: &str, shape: &Shape, len: u64) -> u64 {
        self.inner.stream_work_bytes(format, shape, len)
    }

    fn shape_work_bytes(&self, format: &str, len: u64) -> u64 {
        self.inner.shape_work_bytes(format, len)
    }

    fn held_bytes_file(
        &self,
        path: &std::path::Path,
        format: &str,
        limits: &Limits,
    ) -> Result<kura_engine::codec::Held, EngineError> {
        self.inner.held_bytes_file(path, format, limits)
    }

    fn decode_stream_file(
        &self,
        path: &std::path::Path,
        format: &str,
        opts: &DecodeOptions,
        limits: &Limits,
        shape: &Shape,
        out: &mut kura_engine::codec::DecodeOut<'_>,
        mon: &dyn Monitor,
    ) -> Result<(), EngineError> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        self.inner
            .decode_stream_file(path, format, opts, limits, shape, out, mon)
    }
}

/// 版に戻すの作り直し（アンカーのない FLAC の元。作業用の PCM から読む）を書く途中で一時停止すると、作業用の PCM を残し、
/// 再開で元を読み直さない（M13 段 3 の検証の直し #8。A3）。一時停止のまま中止すれば消す。
#[test]
fn paused_revert_keeps_the_work_file() {
    let pinned = CodecSet::pinned();
    let mut set = CodecSet::pinned();
    let flac = set.decoders_for("flac")[0].clone();
    let counting = Arc::new(CountingDecoder {
        inner: flac,
        reads: Default::default(),
    });
    set.replace_decoder(counting.clone()).unwrap();
    let (env, gate) = gated_env(
        Arc::new(Mutex::new(set)),
        |s| matches!(s, Stage::Window { fed, frames } if fed < frames),
    );
    let original = noise_file(&pinned, "flac", 1, 1_100_000, 8);
    let (item, _) = add_audio(&env.lib, &original, "flac", "戻す声");
    assert_done(&env.overwrite(item, audio_doc(-2.0, Some("wav"))));
    assert_done(&env.overwrite(item, audio_doc(-4.0, Some("wav"))));
    // 版 1 のファイルを GC で消す（照合も作業用の PCM から読む）
    gc_version(&env, item, 1);
    assert_eq!(env.engine.work_pcm().usage(), (0, 0));
    // 中止の回（版を足さない）のあとに、再開の回
    for pause_then_cancel in [true, false] {
        let before = counting.reads();
        gate.arm();
        let id = env.start(item, base_of(&env.lib, item), SaveAction::Revert { to: 1 });
        gate.wait(&env, id);
        env.jobs.pause(id).unwrap();
        gate.open();
        assert!(env.jobs.wait_idle(WAIT));
        assert_eq!(env.jobs.get(id).unwrap().state, JobState::Paused);
        assert_eq!(counting.reads(), before + 1);
        assert_eq!(
            env.engine.work_pcm().usage().0,
            1,
            "版に戻すの一時停止で作業用の PCM を消した"
        );
        if pause_then_cancel {
            env.jobs.cancel(id).unwrap();
            assert!(env.jobs.wait_idle(WAIT));
            assert_eq!(env.jobs.get(id).unwrap().state, JobState::Canceled);
            assert_eq!(env.engine.work_pcm().usage(), (0, 0));
        } else {
            env.jobs.resume(id).unwrap();
            assert!(env.jobs.wait_idle(WAIT));
            assert_done(&env.jobs.get(id).unwrap());
            assert_eq!(counting.reads(), before + 1, "再開で元を読み直した");
            assert_eq!(env.engine.work_pcm().usage(), (0, 0));
        }
        assert_consistent(&env.lib);
    }
}

/// S23：流して書いた音声の版は、GC の照合（流して作り直して生の BLAKE3 を比べる）で一致して released になり、版に戻すと
/// 流して作り直して同じバイト列になる。記録の値のダイジェストと違えば、照合は kept（ファイルを残す）。
#[test]
fn s23_streamed_rebuild_verifies_audio_versions() {
    let env = Env::new();
    let pinned = CodecSet::pinned();
    let original = noise_file(&pinned, "wav", 2, 3 * 65_536 + 77, 23);
    let (a, _) = add_audio(&env.lib, &original, "wav", "版の声");
    assert_done(&env.overwrite(a, audio_doc(-2.0, Some("flac"))));
    let v1 = versions_of(&env.lib, a)[1].clone();
    let path1 = env.lib.layout().content_path(&v1.output_hash, &v1.ext);
    let bytes1 = std::fs::read(&path1).unwrap();
    assert_done(&env.overwrite(a, audio_doc(-5.0, Some("ogg"))));
    // GC の照合：流して作り直したバイト列が版 1 と同じなので消す
    gc_version(&env, a, 1);
    assert!(!path1.exists());
    // 版に戻す：流して作り直す
    let record = env.revert(a, 1);
    assert_done(&record);
    let result = result_of(&record);
    assert_eq!(
        result["revert"],
        json!({ "to": 1, "reused": false, "rebuilt": true })
    );
    assert_eq!(result["warnings"], json!([]));
    assert!(std::fs::read(&path1).unwrap() == bytes1, "同じバイト列");
    let vs = versions_of(&env.lib, a);
    assert_eq!(
        (vs[3].output_hash, vs[3].value_digest),
        (v1.output_hash, v1.value_digest)
    );
    assert_eq!(env.engine.work_pcm().usage(), (0, 0));
    assert_consistent(&env.lib);
    // 版 2（ogg。消し待ち）の値のダイジェストの記録を変えると、照合は合わずに kept
    let v2 = vs[2].clone();
    env.lib
        .db()
        .write(move |tx| {
            tx.execute(
                "UPDATE item_versions SET value_digest = ?1 WHERE id = ?2",
                params![&[7u8; 32][..], v2.id],
            )?;
            Ok(())
        })
        .unwrap();
    let result = env.gc();
    assert_eq!(result["kept"], json!(1), "{result}");
    assert_eq!(states(&env.lib, a)[2], (2, FileState::Kept));
    assert!(
        env.lib
            .layout()
            .content_path(&vs[2].output_hash, &vs[2].ext)
            .exists()
    );
    assert_consistent(&env.lib);
}

/// S21：エディタの入力（作業用の PCM から開いた遅延の値）を貸して保存すると、元を読み直さない（デコーダが読んだ数が増えない）。
/// 結果は貸さないときと同じ（バイト列・アンカーの昇格）。照らしていない貸しものの値が記録と違えば、使わずにふつうに読む。
#[test]
fn s21_lent_input_is_not_read_again() {
    use super::anchors::options_digest;
    use super::services::{LentAnchor, LentInput};
    use super::work_pcm::WorkKey;
    use kura_engine::codec::anchor::{self, AnchorContext};
    use kura_engine::doc::DecodeOptions;
    let decoder = opus_decoder(2, 2 * 65_536 + 11);
    let codecs = {
        let mut set = CodecSet::pinned();
        set.add_decoder(decoder.clone()).unwrap();
        Arc::new(Mutex::new(set))
    };
    let env = Env::with(FakeEngineFactory::with_codecs(codecs), None);
    let (item, hash) = add_audio(&env.lib, b"lent fake opus", "opus", "貸す声");
    // エディタと同じに、作業用の PCM を作って遅延の値で開く
    let set = env.engine.codecs();
    let path = env.lib.layout().content_path(&hash, "opus");
    let opts = DecodeOptions::default();
    let limits = Engine::new().unwrap().limits().clone();
    let shape = set.shape_path(&path, "opus", &opts, &limits).unwrap().0;
    let lend = |verified: bool, value_digest: Option<Digest>| {
        let work = env
            .engine
            .work_pcm()
            .ensure(
                &WorkKey {
                    source: hash,
                    format: "opus".into(),
                    options: opts.clone(),
                    decoder: None,
                },
                &shape,
                AnchorContext {
                    source: Some(Digest(hash)),
                    format: Some("opus".into()),
                    options: opts.clone(),
                },
                |sink| set.decode_stream_path(&path, "opus", &opts, &limits, sink, &NoMonitor),
                &limits,
                &NoMonitor,
            )
            .unwrap();
        let opened = anchor::open_source(&work.opener(), &limits, 64 << 20)
            .unwrap()
            .unwrap();
        let mut decoded = work.pcm().decoded.clone();
        if let Some(d) = value_digest {
            decoded.value_digest = d;
        }
        env.engine.lend_input(
            "editor-1",
            LentInput {
                item_id: item,
                source: hash,
                options_digest: Digest(options_digest(&opts).unwrap()),
                format: "opus".into(),
                value: opened.value,
                decoded,
                anchor: LentAnchor::None,
                work: Some(work),
                verified,
            },
        );
    };
    lend(true, None);
    assert_eq!(decoder.reads(), 1);
    let first = decoder.value_for(0).unwrap();
    let g = audio_doc(-2.0, Some("wav"));
    let (want, _, _, _) = whole_path(&CodecSet::pinned(), first, hash, &g, "wav");
    let record = env.overwrite(item, g);
    assert_done(&record);
    assert_eq!(decoder.reads(), 1, "貸した入力を使い、元を読み直さない");
    assert_eq!(result_of(&record)["anchor"], "created");
    let (got, _, _) = current(&env, item);
    assert!(got == want, "貸した入力の保存のバイト列が違う");
    // 貸している間は作業用の PCM が残り、引き上げると消える
    assert_eq!(env.engine.work_pcm().usage().0, 1);
    env.engine.withdraw_input("editor-1");
    assert_eq!(env.engine.work_pcm().usage(), (0, 0));
    assert_consistent(&env.lib);
}

/// 貸した入力のうち、照らしていない（`verified` が偽）もので値が記録と違うものは使わず、アンカーから読む。
#[test]
fn unverified_lent_input_that_does_not_match_is_not_used() {
    use super::anchors::options_digest;
    use super::services::{LentAnchor, LentInput};
    use kura_engine::doc::DecodeOptions;
    let env = Env::new();
    let pinned = CodecSet::pinned();
    let original = noise_file(&pinned, "wav", 1, 70_001, 21);
    let (item, hash) = add_audio(&env.lib, &original, "wav", "照らす声");
    let value = decode_pinned(&original, "wav");
    // 別の値を貸す（記録のダイジェストは元のもの）
    let other = decode_pinned(&noise_file(&pinned, "wav", 1, 70_001, 22), "wav");
    let (_, stamp) = pinned
        .decode(
            &mut std::io::Cursor::new(&original),
            "wav",
            &DecodeOptions::default(),
            Engine::new().unwrap().limits(),
            &NoMonitor,
        )
        .unwrap();
    env.engine.lend_input(
        "editor-2",
        LentInput {
            item_id: item,
            source: hash,
            options_digest: Digest(options_digest(&DecodeOptions::default()).unwrap()),
            format: "wav".into(),
            value: other,
            decoded: stamp,
            anchor: LentAnchor::None,
            work: None,
            verified: false,
        },
    );
    let g = audio_doc(-3.0, None);
    let (want, _, _, _) = whole_path(&pinned, value, hash, &g, "wav");
    assert_done(&env.overwrite(item, g));
    let (got, _, _) = current(&env, item);
    assert!(got == want, "記録と違う貸しものを使った");
    env.engine.withdraw_input("editor-2");
    assert_consistent(&env.lib);
}

/// S22：mp3 の元を「元の形式のまま」で保存しようとしても、ffmpeg がない組では書けない（`save_format` が
/// `Unavailable{fallback: "ogg"}`。エディタの保存はこれで `FormatUnavailable` を返し、積まない）。書ける組なら通る。積んだ
/// あとに書き手が消えた（ffmpeg がない組でジョブが動いた）ら、ジョブは `Unsupported`（「保存する形式を選び直してください」）。
/// 書ける形式の一覧は、その種類の形式で書き手のあるものだけ。
#[test]
fn s22_mp3_without_ffmpeg_needs_another_format() {
    use super::save::{FormatProblem, WritableFormat, save_format, writable_formats};
    use crate::import::MediaKind;
    use kura_engine::Shape;
    let shape = Shape::Audio {
        rate: 44_100,
        channels: 2,
        frames: kura_engine::Dim::Exact(1_000),
    };
    let pinned = CodecSet::pinned();
    assert_eq!(
        save_format(&pinned, &FormatChoice::Keep, "mp3", &shape),
        Err(FormatProblem::Unavailable {
            format: "mp3".into(),
            fallback: Some("ogg".into()),
        })
    );
    assert_eq!(
        save_format(
            &pinned,
            &FormatChoice::To {
                format: "flac".into(),
                options: json!({ "bits": 24 }),
            },
            "mp3",
            &shape,
        ),
        Ok(("flac".into(), json!({ "bits": 24 })))
    );
    // Ogg Vorbis に書けない形（9 チャンネル）の代わりは WAV
    let nine = Shape::Audio {
        rate: 44_100,
        channels: 9,
        frames: kura_engine::Dim::Exact(1_000),
    };
    assert_eq!(
        save_format(&pinned, &FormatChoice::Keep, "mp3", &nine),
        Err(FormatProblem::Unavailable {
            format: "mp3".into(),
            fallback: Some("wav".into()),
        })
    );
    let tools = FakeTools::new("exit 1", "exit 1", "cksum > \"$out\"");
    let with_mp3 = save_codecs(&tools);
    assert!(save_format(&with_mp3, &FormatChoice::Keep, "mp3", &shape).is_ok());
    let formats: Vec<String> = writable_formats(&pinned, MediaKind::Audio, &shape)
        .into_iter()
        .map(|w: WritableFormat| w.format)
        .collect();
    assert_eq!(formats, ["wav", "aiff", "flac", "ogg"]);
    let with: Vec<String> = writable_formats(&with_mp3, MediaKind::Audio, &shape)
        .into_iter()
        .map(|w| w.format)
        .collect();
    assert!(with.contains(&"mp3".to_string()) && with.contains(&"m4a".to_string()));

    // 積んだあとに書き手がない：ジョブは Unsupported（何も変えない）
    let env = Env::new();
    let mp3 = std::fs::read(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../kura-engine/fixtures/mp3-lame.mp3"),
    )
    .unwrap();
    let (item, _) = add_audio(&env.lib, &mp3, "mp3", "mp3 の声");
    let record = env.overwrite(item, audio_doc(-1.0, None));
    assert_eq!(record.state, JobState::Failed, "{:?}", record.message);
    assert_eq!(record.error_code.as_deref(), Some("Unsupported"));
    assert!(
        record
            .message
            .as_deref()
            .is_some_and(|m| m.contains("選び直して")),
        "{:?}",
        record.message
    );
    assert_eq!(versions_of(&env.lib, item).len(), 0);
    // 形式を選び直せば（ogg）保存できる
    assert_done(&env.overwrite(item, audio_doc(-1.0, Some("ogg"))));
    assert_eq!(head(&env.lib, item).ext, "ogg");
    assert_consistent(&env.lib);
}

/// 保存の流れの進捗の文言（GC の照合では送らない）。
const STREAM_MESSAGES: [&str; 7] = [
    "元の大きさを確かめています",
    "元を照らしています",
    "元の読み取りを照らしています",
    "作業用のファイルに読んでいます",
    "書いています",
    "書いたものを確かめています",
    "戻す版の元を読んでいます",
];

/// GC の照合で音声を流して作り直すときは、保存の進捗（文言・割合）を送らない（GC は自分の進捗を送る。M13 段 3 の検証の
/// 直し #6）。
#[test]
fn gc_rebuild_does_not_send_save_progress() {
    let env = Env::new();
    env.jobs.set_progress_interval(0);
    let pinned = CodecSet::pinned();
    let original = noise_file(&pinned, "wav", 2, 3 * 65_536 + 77, 6);
    let (item, _) = add_audio(&env.lib, &original, "wav", "照合の声");
    assert_done(&env.overwrite(item, audio_doc(-2.0, Some("flac"))));
    assert_done(&env.overwrite(item, audio_doc(-5.0, Some("wav"))));
    env.events.lock().unwrap().clear();
    gc_version(&env, item, 1);
    let events = env.events.lock().unwrap();
    let gc: Vec<_> = events
        .iter()
        .filter(|e| e.kind == crate::jobs::JobKind::Gc && e.progress.is_some())
        .collect();
    assert!(!gc.is_empty());
    for e in gc {
        let m = e.message.as_deref().unwrap_or("");
        assert!(
            !STREAM_MESSAGES.contains(&m),
            "GC が保存の進捗を送った：{e:?}"
        );
    }
    drop(events);
    assert_consistent(&env.lib);
}

/// ジョブ `id` の進捗の列（動いている間の (割合, 文言)。間引かない Env で）。
fn progress_of(env: &Env, id: i64) -> Vec<(f64, String)> {
    env.events
        .lock()
        .unwrap()
        .iter()
        .filter(|e| e.id == id && e.state == JobState::Running)
        .filter_map(|e| Some((e.progress?, e.message.clone().unwrap_or_default())))
        .collect()
}

/// 進捗の列が戻らない（単調に増える）。
fn assert_monotonic(seq: &[(f64, String)], what: &str) {
    assert!(seq.len() > 3, "{what}：{seq:?}");
    for w in seq.windows(2) {
        assert!(
            w[1].0 >= w[0].0,
            "{what}：進捗が戻った {:?} → {:?}\n{seq:?}",
            w[0],
            w[1]
        );
    }
}

/// 流して書く保存・版に戻すの進捗は単調に増える（M13 段 3 の検証の直し #7）：Platform で読む音声（作業用の PCM・OS の
/// デコーダの印・昇格・2 回流す ogg・R2・コミット）と WAV の元（源の照らしの回）の上書き、アンカー・源から作り直す版に戻す。
#[test]
fn streamed_progress_never_goes_back() {
    let decoder = opus_decoder(1, 2 * 65_536 + 11);
    let env = Env::with(FakeEngineFactory::with_codecs(opus_codecs(&decoder)), None);
    env.jobs.set_progress_interval(0);
    let pinned = CodecSet::pinned();
    let wav = noise_file(&pinned, "wav", 2, 3 * 65_536 + 77, 7);
    let items = [
        add_audio(&env.lib, b"progress fake opus", "opus", "進みの声").0,
        add_audio(&env.lib, &wav, "wav", "進みの WAV").0,
    ];
    for item in items {
        let record = env.overwrite(item, audio_doc(-2.0, Some("ogg")));
        assert_done(&record);
        let seq = progress_of(&env, record.id);
        assert_monotonic(&seq, &format!("上書き {item}"));
        assert!(
            seq.iter().any(|(_, m)| m == "書いたものを確かめています"),
            "{seq:?}"
        );
        assert_done(&env.overwrite(item, audio_doc(-4.0, Some("wav"))));
        gc_version(&env, item, 1);
        let record = env.revert(item, 1);
        assert_done(&record);
        assert_eq!(result_of(&record)["revert"]["rebuilt"], json!(true));
        assert_monotonic(&progress_of(&env, record.id), &format!("版に戻す {item}"));
    }
    assert_consistent(&env.lib);
}

/// mp3 のフィクスチャ（固定の段の nanomp3 で読める）を元にした音声のアイテム。
fn add_mp3(env: &Env) -> i64 {
    let mp3 = std::fs::read(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../kura-engine/fixtures/mp3-lame.mp3"),
    )
    .unwrap();
    add_audio(&env.lib, &mp3, "mp3", "mp3 の声").0
}

/// エディタで開く（音声の表示の範囲は既定）。
fn open_editor(env: &Env, item: i64) -> String {
    use super::editor::{EditorEnv, OpenRequest, Viewport, open};
    open(
        &EditorEnv {
            library: &env.lib,
            jobs: &env.jobs,
            engine: &env.engine,
        },
        &OpenRequest {
            item_id: item,
            viewport: Viewport::audio_idle(),
            from_draft: false,
        },
    )
    .unwrap()
    .session
}

/// エディタの保存（`editor::save`）。
fn editor_save(
    env: &Env,
    session: &str,
    mode: super::editor::SaveMode,
    format: Option<FormatChoice>,
) -> Result<super::editor::SaveStarted, super::editor::EditorError> {
    use super::editor::{EditorEnv, save};
    save(
        &EditorEnv {
            library: &env.lib,
            jobs: &env.jobs,
            engine: &env.engine,
        },
        session,
        mode,
        format,
    )
}

/// 保存のジョブの数。
fn edit_jobs(env: &Env) -> i64 {
    env.count("SELECT count(*) FROM jobs WHERE kind = 'edit'")
}

/// エディタの保存の FormatUnavailable は、開いたときの組ではなく今のコーデックの組（保存のジョブと同じ）で確かめる：開いた
/// あとに ffmpeg の書き手のある組へ入れ替えて作り直せば、開いたままのエディタで元の形式のまま（mp3）保存でき、書き手を
/// 外せばまた断る（M13 段 3 の検証の直し #12）。
#[test]
fn editor_save_checks_the_current_codecs() {
    use super::editor::{EditorError, SaveMode};
    let codecs = Arc::new(Mutex::new(CodecSet::pinned()));
    let env = Env::with(FakeEngineFactory::with_codecs(codecs.clone()), None);
    let item = add_mp3(&env);
    let session = open_editor(&env, item);
    let r = editor_save(&env, &session, SaveMode::Overwrite, None);
    assert!(
        matches!(&r, Err(EditorError::FormatUnavailable { format, .. }) if format == "mp3"),
        "{:?}",
        r.err()
    );
    // ffmpeg の場所を指定した（書き手のある組を作り直した）
    let tools = FakeTools::new("exit 1", "exit 1", "cksum > \"$out\"");
    *codecs.lock().unwrap() = save_codecs(&tools);
    env.engine.reload_codecs();
    let started = editor_save(&env, &session, SaveMode::Overwrite, None).unwrap();
    assert!(env.jobs.wait_idle(WAIT));
    assert_done(&env.jobs.get(started.job).unwrap());
    assert_eq!(head(&env.lib, item).ext, "mp3");
    // 書き手を外した（ffmpeg の指定を外した）：また積む前に断る
    *codecs.lock().unwrap() = CodecSet::pinned();
    env.engine.reload_codecs();
    let jobs = edit_jobs(&env);
    let r = editor_save(&env, &session, SaveMode::Overwrite, None);
    assert!(
        matches!(&r, Err(EditorError::FormatUnavailable { .. })),
        "{:?}",
        r.err()
    );
    assert_eq!(edit_jobs(&env), jobs, "断ったのに保存を積んだ");
    assert_consistent(&env.lib);
}
