//! 音声のエディタの受け入れ（M13 段 4。docs/editing.md §12.12 の A1〜）：開く・表示の範囲・表示の情報・予算・閉じる。偽の
//! エンジンの部品・固定の段のコーデックで、Linux でも回る。

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use kura_engine::codec::CodecSet;
use kura_engine::doc::{InputId, OutputId};
use kura_engine::render::InputValue;
use kura_engine::testgen::InputGen;
use kura_engine::value::{AUDIO_CHUNK, LoopRange};
use kura_engine::{Digest, Engine, Graph, NoMonitor, Value};
use rusqlite::params;
use serde_json::json;

use super::super::*;
use crate::edit::services::fake::FakeEngineFactory;
use crate::edit::{ENGINE_WORK_DIR, EditHandler, EngineFactory, RuntimeConfig};
use crate::jobs::{EventSink, JobKind};
use crate::library::Library;

pub(super) const C: u64 = AUDIO_CHUNK as u64;

/// 予算を決められる偽のファクトリ（固定の段だけのコーデックの組）。
pub(super) struct Budgeted(pub u64);

impl EngineFactory for Budgeted {
    fn runtime_config(&self) -> RuntimeConfig {
        RuntimeConfig {
            cores: Some(2),
            budget: Some(self.0),
            ..RuntimeConfig::default()
        }
    }

    fn codecs(&self, _work_dir: &Path) -> CodecSet {
        CodecSet::pinned()
    }
}

/// 保存のジョブだけを動かすライブラリ（ジョブは動かさない）。
pub(super) struct Env {
    _dir: tempfile::TempDir,
    pub lib: Arc<Library>,
    pub jobs: JobManager,
    pub engine: Arc<EngineServices>,
}

impl Env {
    pub fn new() -> Self {
        Self::with(Arc::new(FakeEngineFactory::default()))
    }

    pub fn with(factory: Arc<dyn EngineFactory>) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let lib = Arc::new(Library::create(&dir.path().join("L")).unwrap());
        let engine = Arc::new(EngineServices::new(
            factory,
            lib.layout().staging_dir().join(ENGINE_WORK_DIR),
        ));
        let sink: EventSink = Arc::new(|_| {});
        let jobs = JobManager::new(
            lib.clone(),
            vec![(JobKind::Edit, Arc::new(EditHandler::new(engine.clone())))],
            sink,
        )
        .unwrap();
        Self {
            _dir: dir,
            lib,
            jobs,
            engine,
        }
    }

    pub fn env(&self) -> EditorEnv<'_> {
        EditorEnv {
            library: &self.lib,
            jobs: &self.jobs,
            engine: &self.engine,
        }
    }

    pub fn open(&self, item_id: i64) -> Opened {
        self.open_with(item_id, Viewport::audio_idle())
    }

    pub fn open_with(&self, item_id: i64, viewport: Viewport) -> Opened {
        open(
            &self.env(),
            &OpenRequest {
                item_id,
                viewport,
                from_draft: false,
            },
        )
        .unwrap()
    }

    pub fn step(&self, session: &str, steps: Vec<Step>, vp: &Viewport) -> StepOutcome {
        step(&self.env(), session, steps, vp).unwrap()
    }

    pub fn usage(&self) -> kura_engine::budget::Usage {
        self.engine.runtime().unwrap().budget().usage().unwrap()
    }

    pub fn editor(&self, session: &str) -> Arc<Editor> {
        self.engine.editor_for(session).unwrap()
    }
}

/// 音声の値をファイルのバイト列にする（`ext` の固定の段のエンコーダ）。
pub(super) fn encode(value: &Value, ext: &str) -> Vec<u8> {
    CodecSet::pinned()
        .encode_bytes(value, ext, &json!({}), &NoMonitor)
        .unwrap()
        .0
}

/// 雑音の WAV（16 ビット）。
pub(super) fn noise_wav(rate: u32, channels: u16, frames: u64, seed: u64) -> Vec<u8> {
    encode(
        &InputGen::AudioNoise {
            rate,
            channels,
            frames,
            seed,
        }
        .generate()
        .unwrap(),
        "wav",
    )
}

/// 正弦波の WAV（ループ点つき）。
pub(super) fn sine_wav(frames: u64, loop_range: Option<LoopRange>) -> Vec<u8> {
    encode(
        &InputGen::Sine {
            rate: 8_000,
            channels: 1,
            frames,
            freq: 440,
            loop_range,
        }
        .generate()
        .unwrap(),
        "wav",
    )
}

/// 音声のアイテムのファイルを置いて行を入れる。
pub(super) fn add_audio(lib: &Library, bytes: &[u8], ext: &'static str) -> i64 {
    let hash = *blake3::hash(bytes).as_bytes();
    let path = lib.layout().content_path(&hash, ext);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, bytes).unwrap();
    let size = bytes.len() as i64;
    lib.db()
        .write(move |tx| {
            tx.execute(
                "INSERT INTO items (hash, ext, kind, mime, size_bytes, title, original_name, original_path,
                                    source_site, imported_at, updated_at, thumb_state, proxy_state, analysis_state)
                 VALUES (?1, ?3, 'audio', 'audio/' || ?3, ?2, '音', '音.' || ?3, '/src/音.' || ?3, 'local', 1, 1,
                         'ready', 'not_needed', 'ready')",
                params![&hash[..], size, ext],
            )?;
            Ok(tx.last_insert_rowid())
        })
        .unwrap()
}

/// 元（`ext` のバイト列）に文書を当てた全体の値（キャッシュなしの正）。
pub(super) fn rendered(bytes: &[u8], ext: &str, graph: &Graph) -> Arc<kura_engine::value::Audio> {
    let codecs = CodecSet::pinned();
    let (value, _) = codecs
        .decode(
            &mut std::io::Cursor::new(bytes),
            ext,
            &Default::default(),
            &Default::default(),
            &NoMonitor,
        )
        .unwrap();
    let engine = Engine::new().unwrap();
    let plan = engine
        .compile(graph, &BTreeMap::from([(InputId(0), value.shape())]))
        .unwrap();
    let out = engine
        .render(
            &plan,
            &BTreeMap::from([(
                InputId(0),
                InputValue {
                    value,
                    source: Digest(*blake3::hash(bytes).as_bytes()),
                },
            )]),
            &NoMonitor,
        )
        .unwrap();
    match out.outputs.into_iter().next().unwrap().value {
        Value::Audio(a) => a,
        _ => panic!("音声ではありません"),
    }
}

pub(super) fn shown_audio(v: &ViewOutcome) -> &AudioViewInfo {
    match v {
        ViewOutcome::Shown(ViewInfo::Audio(info)) => info,
        other => panic!("音声を表示できませんでした: {other:?}"),
    }
}

/// 区間を描いて配ってほしい要求。
pub(super) fn spans(list: &[(u64, u64)]) -> Viewport {
    Viewport::Audio(AudioViewport {
        spans: list.to_vec(),
        ..AudioViewport::idle()
    })
}

/// 末尾に音声の操作を足す手。
pub(super) fn add(at: usize, op: &str, params: serde_json::Value) -> Step {
    Step::Commit {
        edit: Edit::AddNode {
            at,
            op: op.into(),
            v: 1,
            params,
            seed: None,
            extra_inputs: Vec::new(),
        },
        mode: CommitMode::Step,
    }
}

/// ノードの引数を変える手。
pub(super) fn set(node: u32, params: serde_json::Value) -> Step {
    Step::Commit {
        edit: Edit::SetParams {
            node: NodeId(node),
            params,
        },
        mode: CommitMode::Step,
    }
}

/// A1：WAV は遅延の源で開く（入力は中身を持たない）。元の形の要約・デコーダ。開くと試聴の置き場が空になる。FLAC は丸ごと読む。
#[test]
fn wav_opens_lazily_and_flac_whole() {
    let env = Env::new();
    let bytes = noise_wav(8_000, 2, 3 * C + 17, 1);
    let id = add_audio(&env.lib, &bytes, "wav");
    // 試聴の置き場に入れておく
    let hash = crate::library::hex(blake3::hash(&bytes).as_bytes());
    let src = crate::serve::pcm_source(env.lib.layout(), &hash).unwrap();
    crate::asset::pcm::serve(&env.engine, env.lib.layout().root(), &hash, &src, 0, 10).unwrap();
    assert!(env.engine.pcm_cache().cached().is_some());

    let opened = env.open(id);
    assert!(
        env.engine.pcm_cache().cached().is_none(),
        "試聴の置き場を空にする"
    );
    assert_eq!(opened.source.kind, "audio");
    let a = opened.source.audio.clone().unwrap();
    assert_eq!((a.rate, a.channels, a.frames), (8_000, 2, 3 * C + 17));
    assert_eq!((a.bits, a.float), (Some(16), false));
    assert!(a.lazy);
    assert!(a.decoder.starts_with("wav"), "{}", a.decoder);
    let editor = env.editor(&opened.session);
    let (input, decoder) = editor.published_input().unwrap();
    assert_eq!(decoder, a.decoder);
    let Value::Audio(input) = input else { panic!() };
    assert_eq!(input.resident_bytes(), 0, "遅延の入力は中身を持たない");
    let info = shown_audio(&opened.outcome.view);
    assert_eq!(
        (info.rate, info.channels, info.frames),
        (8_000, 2, 3 * C + 17)
    );
    assert_eq!(info.valid, vec![(0, 3 * C + 17)], "短い音声は全体");
    assert_eq!(info.changed, ChangedSpans::All);

    // FLAC は丸ごと読む（遅延でない）
    let value = InputGen::AudioNoise {
        rate: 8_000,
        channels: 1,
        frames: 2 * C,
        seed: 2,
    }
    .generate()
    .unwrap();
    let flac = encode(&value, "flac");
    let fid = add_audio(&env.lib, &flac, "flac");
    let opened = env.open(fid);
    let a = opened.source.audio.clone().unwrap();
    assert!(!a.lazy);
    assert!(a.decoder.starts_with("flac"), "{}", a.decoder);
    assert_eq!(shown_audio(&opened.outcome.view).valid, vec![(0, 2 * C)]);
}

/// アンカーを置く（保存のジョブの代わり）：アイテムの元 `bytes`（形式 `format`）を読んだ値を、見出しの記録 `header` で
/// アンカーの f32 WAV に書き、DB の行の記録を元を読んだ記録にする。返すのは DB の行の記録。
pub(super) fn place_anchor(
    lib: &Library,
    bytes: &[u8],
    format: &str,
    header: impl FnOnce(&kura_engine::codec::DecodeStamp) -> kura_engine::codec::DecodeStamp,
) -> kura_engine::codec::DecodeStamp {
    use kura_engine::codec::anchor;
    let (value, decoded) = CodecSet::pinned()
        .decode(
            &mut std::io::Cursor::new(bytes),
            format,
            &Default::default(),
            &Default::default(),
            &NoMonitor,
        )
        .unwrap();
    let source = *blake3::hash(bytes).as_bytes();
    let mut file = Vec::new();
    anchor::write(
        &value,
        &header(&decoded),
        &anchor::AnchorContext {
            source: Some(Digest(source)),
            format: Some(format.into()),
            options: Default::default(),
        },
        &Default::default(),
        &mut file,
        &NoMonitor,
    )
    .unwrap();
    let hash = *blake3::hash(&file).as_bytes();
    let path = lib.layout().content_path(&hash, "wav");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, &file).unwrap();
    let size = file.len() as u64;
    let row = decoded.clone();
    lib.db()
        .write(move |tx| {
            crate::edit::anchors::insert(
                tx,
                &crate::edit::anchors::NewAnchor {
                    source,
                    options: &Default::default(),
                    decoded: &row,
                    hash,
                    ext: "wav",
                    size,
                    now: 1,
                },
            )
        })
        .unwrap();
    decoded
}

/// A1：アンカーのある音声は、見出しの記録が DB と同じなら遅延で開き（入力は中身を持たない）、違えば丸ごと読みに落ちる。
/// どちらの道でも、元の読み取りのデコーダは記録のもの（アンカーの読み手の名前ではない）。
#[test]
fn anchored_audio_opens_lazily_with_the_recorded_decoder() {
    let env = Env::new();
    for (seed, same) in [(61u64, true), (62, false)] {
        let bytes = noise_wav(8_000, 2, 2 * C + 9, seed);
        let id = add_audio(&env.lib, &bytes, "wav");
        let recorded = place_anchor(&env.lib, &bytes, "wav", |d| {
            let mut h = d.clone();
            if !same {
                // 値は同じで、見出しの記録だけが DB と違う
                h.codec = "other-kura-dec/9".into();
            }
            h
        });
        let opened = env.open(id);
        let a = opened.source.audio.clone().unwrap();
        assert_eq!(a.lazy, same, "見出しが DB と同じときだけ遅延");
        assert_eq!(a.decoder, recorded.codec, "記録のデコーダ（{same}）");
        let editor = env.editor(&opened.session);
        let (input, decoder) = editor.published_input().unwrap();
        assert_eq!(decoder, recorded.codec);
        let Value::Audio(input) = input else { panic!() };
        if same {
            assert_eq!(input.resident_bytes(), 0, "遅延の入力は中身を持たない");
        }
        let info = shown_audio(&opened.outcome.view);
        assert_eq!((info.channels, info.frames), (2, 2 * C + 9));
    }
}

/// A1・A15：丸ごと読む形式は、値が min(64 MiB, 丸ごと読む上限 min(取り置き/2, 512 MiB)) を超えれば、断らずに作業用の
/// ファイルから遅延で開く（段 4A までは TooLarge だった）。WAV は遅延なので同じ長さでもそのまま開ける。
#[test]
fn long_whole_reads_open_from_a_work_file() {
    // 予算 16 MiB：取り置き 8 MiB、丸ごと読む上限と作業用のファイルの境は 4 MiB
    let env = Env::with(Arc::new(Budgeted(16 << 20)));
    let value = InputGen::AudioNoise {
        rate: 8_000,
        channels: 1,
        frames: 20 * C,
        seed: 3,
    }
    .generate()
    .unwrap();
    let flac = add_audio(&env.lib, &encode(&value, "flac"), "flac");
    let opened = env.open(flac);
    let a = opened.source.audio.unwrap();
    assert!(a.lazy, "作業用のファイルから開く");
    assert_eq!(env.engine.work_pcm().usage().0, 1);
    let wav = add_audio(&env.lib, &encode(&value, "wav"), "wav");
    let opened = env.open(wav);
    assert!(opened.source.audio.unwrap().lazy);
    assert_eq!(
        env.engine.work_pcm().usage().0,
        0,
        "前のエディタを閉じたら作業用のファイルは消える"
    );
}

/// 段 4B の検証の直し 6（#11）：アンカーがあって遅延で使えない（見出しの記録が DB と違う）長い音声は、作業用のファイルに
/// せず、記録のデコーダで丸ごと読み直す道に落ちる。丸ごと読む上限を超えれば TooLarge（「まだエディタで開けません」。既知の
/// 限界）で断り、作業用のファイル・取り置きを残さず、エディタも残らない。
#[test]
fn anchored_long_audio_that_cannot_be_used_lazily_is_too_large() {
    // 予算 16 MiB：丸ごと読む上限と作業用のファイルの境は 4 MiB
    let env = Env::with(Arc::new(Budgeted(16 << 20)));
    let value = InputGen::AudioNoise {
        rate: 8_000,
        channels: 1,
        frames: 20 * C,
        seed: 9,
    }
    .generate()
    .unwrap();
    let flac = encode(&value, "flac");
    let id = add_audio(&env.lib, &flac, "flac");
    place_anchor(&env.lib, &flac, "flac", |d| {
        let mut h = d.clone();
        h.codec = "other-kura-dec/9".into();
        h
    });
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
        matches!(&err, EditorError::TooLarge(m) if m.contains("まだエディタで開けません")),
        "{err}"
    );
    assert_eq!(
        env.engine.work_pcm().usage().0,
        0,
        "作業用のファイルにしない"
    );
    let u = env.usage();
    assert_eq!((u.reserved, u.interactive, u.standing), (0, 0, 0));
    assert!(env.engine.current_editor().is_none());
}

/// 画像・音声のほかは断る。
#[test]
fn other_kinds_are_refused() {
    let env = Env::new();
    let id = add_audio(&env.lib, &sine_wav(100, None), "wav");
    env.lib
        .db()
        .write(move |tx| {
            tx.execute("UPDATE items SET kind = 'video' WHERE id = ?1", [id])?;
            Ok(())
        })
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
        matches!(&err, EditorError::Invalid { message, .. } if message.contains("画像と音声だけ")),
        "{err}"
    );
}

/// A2：閉じると取り置き・対話・持ち続ける分が 0、公開した表示・入力も外れる。
#[test]
fn closing_releases_everything() {
    let env = Env::new();
    let id = add_audio(&env.lib, &noise_wav(8_000, 2, 2 * C, 4), "wav");
    let opened = env.open(id);
    let s = opened.session.clone();
    let out = env.step(
        &s,
        vec![add(0, "audio.gain", json!({ "db": -3.0 }))],
        &Viewport::audio_idle(),
    );
    let rev = shown_audio(&out.view).rev;
    let editor = env.editor(&s);
    assert!(editor.published_audio(rev).is_some());
    let u = env.usage();
    assert!(u.reserved > 0 && u.standing > 0);
    close(&env.engine, &s);
    let u = env.usage();
    assert_eq!((u.reserved, u.interactive, u.standing), (0, 0, 0));
    assert!(editor.published_audio(rev).is_none());
    assert!(editor.published_input().is_none());
}

/// A3：短い音声は全体（表示の値は由来 Exact）。長い音声は頼んだ区間だけをチャンクに揃えて、48 チャンクまで。
#[test]
fn short_audio_is_whole_and_long_audio_is_spans() {
    let env = Env::new();
    let id = add_audio(&env.lib, &noise_wav(8_000, 1, 3 * C, 5), "wav");
    let opened = env.open(id);
    let s = opened.session.clone();
    let out = env.step(
        &s,
        vec![add(0, "audio.gain", json!({ "db": -6.0 }))],
        &spans(&[(0, 10)]),
    );
    let info = shown_audio(&out.view);
    assert_eq!(info.valid, vec![(0, 3 * C)]);
    let (value, id_) = env.editor(&s).published_audio(info.rev).unwrap();
    assert!(value.exact().is_some(), "全体を描いた値は Exact");
    assert_eq!(id_, info.samples_id);

    // 8 kHz モノラルで 32 MiB を超える長さ（130 チャンク）
    let frames = 130 * C;
    let long = add_audio(&env.lib, &noise_wav(8_000, 1, frames, 6), "wav");
    let opened = env.open(long);
    let info = shown_audio(&opened.outcome.view);
    assert_eq!(
        info.valid,
        vec![(0, frames)],
        "手を当てていなければ出力は遅延の入力そのもの"
    );
    let s = opened.session.clone();
    let out = env.step(
        &s,
        vec![add(0, "audio.gain", json!({ "db": -6.0 }))],
        &spans(&[(C + 5, 2 * C + 1)]),
    );
    assert_eq!(shown_audio(&out.view).valid, vec![(C, 3 * C)]);
    let out = env.step(&s, Vec::new(), &Viewport::audio_idle());
    assert!(
        shown_audio(&out.view).valid.is_empty(),
        "区間を頼まなければ描かない"
    );
    let out = env.step(&s, Vec::new(), &spans(&[(0, 60 * C), (100 * C, 101 * C)]));
    assert_eq!(
        shown_audio(&out.view).valid,
        vec![(0, 48 * C)],
        "48 チャンクで切る"
    );
    // 出力の PCM は全体の描画と同じ
    let (value, _) = env
        .editor(&s)
        .published_audio(shown_audio(&out.view).rev)
        .unwrap();
    let mut got = vec![0f32; 1000];
    value.audio_read(0, 47 * C, &mut got).unwrap();
    let mut g = Graph::new_single();
    g.push_linear("audio.gain", 1, json!({ "db": -6.0 }), 1)
        .unwrap();
    let want = rendered(&noise_wav(8_000, 1, frames, 6), "wav", &g);
    let mut expect = vec![0f32; 1000];
    want.read(0, 47 * C, &mut expect).unwrap();
    assert_eq!(got, expect);
}

/// A4：手の `changed`。fade の長さは区間、ループ点だけなら空で `metaChanged`、構造の手（trim）は長さが変わるので全体。
#[test]
fn changed_spans_follow_the_edit() {
    let env = Env::new();
    let bytes = sine_wav(4 * C, None);
    let id = add_audio(&env.lib, &bytes, "wav");
    let opened = env.open(id);
    let s = opened.session.clone();
    let vp = Viewport::audio_idle();
    let out = env.step(
        &s,
        vec![add(
            0,
            "audio.fade",
            json!({ "fadeIn": 1_000, "fadeOut": 0, "curve": "linear" }),
        )],
        &vp,
    );
    shown_audio(&out.view);
    // フェードの長さを延ばす：前の長さとの差の区間
    let out = env.step(
        &s,
        vec![set(
            1,
            json!({ "fadeIn": C + 10, "fadeOut": 0, "curve": "linear" }),
        )],
        &vp,
    );
    let info = shown_audio(&out.view);
    let ChangedSpans::Spans(sp) = &info.changed else {
        panic!("{:?}", info.changed)
    };
    assert!(
        !sp.is_empty() && sp.iter().all(|(_, b)| *b <= 2 * C),
        "{sp:?}"
    );
    assert!(!info.meta_changed);
    // ループ点だけ：標本は変わらない（足した手は保守的に全体でもよい。引数だけを変えた手は空）
    let out = env.step(
        &s,
        vec![add(
            1,
            "audio.set_loop",
            json!({ "loop": { "start": 5, "end": 900 } }),
        )],
        &vp,
    );
    assert!(shown_audio(&out.view).meta_changed);
    let out = env.step(
        &s,
        vec![set(2, json!({ "loop": { "start": 10, "end": 2_000 } }))],
        &vp,
    );
    let info = shown_audio(&out.view);
    assert_eq!(info.changed, ChangedSpans::Spans(Vec::new()));
    assert!(info.meta_changed);
    assert_eq!(info.loop_range, Some((10, 2_000)));
    // 表示の値は全体の描画と同じ
    let g = env
        .editor(&s)
        .lock_state()
        .as_ref()
        .unwrap()
        .session
        .head()
        .to_graph();
    let want = rendered(&bytes, "wav", &g);
    let (value, _) = env.editor(&s).published_audio(info.rev).unwrap();
    let mut got = vec![0f32; want.frames()];
    value.audio_read(0, 0, &mut got).unwrap();
    let mut expect = vec![0f32; want.frames()];
    want.read(0, 0, &mut expect).unwrap();
    assert_eq!(got, expect);
    // trim の終わり：長さが変わる
    let out = env.step(
        &s,
        vec![add(2, "audio.trim", json!({ "start": 0, "end": 3 * C }))],
        &vp,
    );
    let info = shown_audio(&out.view);
    assert_eq!(info.frames, 3 * C);
    assert!(matches!(
        info.changed,
        ChangedSpans::All | ChangedSpans::Spans(_)
    ));
}

/// A5：長い音声の 1 手は区間の分だけを描き、持ち続ける分（`extra`）は公開した区間の表示の値だけ。入力は遅延のまま。
#[test]
fn long_audio_steps_hold_only_the_spans() {
    let env = Env::new();
    let frames = 150 * C;
    let id = add_audio(&env.lib, &noise_wav(8_000, 1, frames, 7), "wav");
    let opened = env.open(id);
    let s = opened.session.clone();
    let vp = spans(&[(70 * C, 72 * C)]);
    let out = env.step(&s, vec![add(0, "audio.gain", json!({ "db": -3.0 }))], &vp);
    let info = shown_audio(&out.view);
    assert_eq!(info.valid, vec![(70 * C, 72 * C)]);
    let out = env.step(&s, vec![set(1, json!({ "db": -4.0 }))], &vp);
    let info = shown_audio(&out.view);
    assert_eq!(info.valid, vec![(70 * C, 72 * C)]);
    let editor = env.editor(&s);
    let guard = editor.lock_state();
    let st = guard.as_ref().unwrap();
    // 今と直前の 2 つの表示の値 × 2 チャンク ＋ 波形の段（公開した 2 つと組んでいる 1 つ。上の段を含めて最も細かい段の 2 倍まで）
    let peaks = frames.div_ceil(256) * 4 * 2;
    assert!(
        st.lease.extra() <= 2 * 2 * C * 4 + 3 * peaks,
        "{}",
        st.lease.extra()
    );
    assert!(st.lease.extra() > 0);
    let Value::Audio(input) = &st.inputs[&InputId(0)].value else {
        panic!()
    };
    assert_eq!(input.resident_bytes(), 0, "入力は遅延のまま");
    drop(guard);
    let u = env.usage();
    assert_eq!(
        u.standing,
        editor.lock_state().as_ref().unwrap().lease.bytes()
    );
}

/// A6：`samplesId` は同じ標本で同じ（取り消しで戻る）。量子化を変えると変わる。
#[test]
fn samples_id_follows_the_samples() {
    let env = Env::new();
    let id = add_audio(&env.lib, &noise_wav(8_000, 1, 2 * C, 8), "wav");
    let opened = env.open(id);
    let s = opened.session.clone();
    let vp = Viewport::audio_idle();
    let first = shown_audio(&opened.outcome.view).samples_id.clone();
    assert_eq!(first.len(), 16);
    let out = env.step(&s, vec![add(0, "audio.gain", json!({ "db": -3.0 }))], &vp);
    let gained = shown_audio(&out.view).samples_id.clone();
    assert_ne!(gained, first);
    let out = env.step(&s, vec![Step::Undo], &vp);
    assert_eq!(shown_audio(&out.view).samples_id, first);
    let out = env.step(&s, vec![Step::Redo], &vp);
    assert_eq!(shown_audio(&out.view).samples_id, gained);
    let quantize = Step::Commit {
        edit: Edit::SetOutput {
            output: OutputId(0),
            format: kura_engine::doc::FormatChoice::Keep,
            quantize: Some(kura_engine::doc::Quantize {
                v: kura_engine::doc::QUANTIZE_VERSION,
                bits: 16,
                dither: kura_engine::doc::Dither::None,
                seed: 0,
            }),
            expand: kura_engine::doc::Expand::One,
        },
        mode: CommitMode::Step,
    };
    let out = env.step(&s, vec![quantize], &vp);
    assert!(out.rejected.is_none(), "{:?}", out.rejected);
    assert_ne!(shown_audio(&out.view).samples_id, gained);
}
