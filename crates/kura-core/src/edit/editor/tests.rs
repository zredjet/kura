//! エディタのセッションのテスト（偽のエンジンの部品・固定の段のコーデックで、Linux でも回る）：開く・手・取り消し・表示・
//! スポイト・タイルの配信・閉じると予算が 0・下書き・保存の連携。

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use kura_engine::budget::Class;
use kura_engine::codec::CodecSet;
use kura_engine::doc::{FormatChoice, InputId};
use kura_engine::render::InputValue;
use kura_engine::render::region::PartialValue;
use kura_engine::testgen::InputGen;
use kura_engine::value::PixelFormat;
use kura_engine::{Digest, Engine, Graph, NoMonitor, Value};
use rusqlite::params;
use serde_json::json;

use super::tests_support::stroke;
use super::*;
use crate::edit::services::fake::FakeEngineFactory;
use crate::edit::{ENGINE_WORK_DIR, EditHandler, EngineFactory, RuntimeConfig};
use crate::jobs::{EventSink, JobKind};
use crate::library::{LibrarySession, Services};
use crate::media::fake::FakeBackend;

const WAIT: Duration = Duration::from_secs(60);

fn png_of(value: &Value) -> Vec<u8> {
    CodecSet::pinned()
        .encode_bytes(value, "png", &json!({}), &NoMonitor)
        .unwrap()
        .0
}

fn sprite_png(w: u32, h: u32, seed: u64) -> Vec<u8> {
    png_of(
        &InputGen::PixelSprite {
            w,
            h,
            fmt: PixelFormat::Rgba8,
            seed,
        }
        .generate()
        .unwrap(),
    )
}

fn noise_png(w: u32, h: u32, seed: u64) -> Vec<u8> {
    png_of(
        &InputGen::Noise {
            w,
            h,
            fmt: PixelFormat::Rgba8,
            seed,
        }
        .generate()
        .unwrap(),
    )
}

/// アイテムのファイル（PNG）を置いて行を入れる。
fn add_item(lib: &Library, bytes: &[u8], w: i64, h: i64) -> i64 {
    add_item_as(lib, bytes, "png", w, h)
}

/// アイテムのファイル（拡張子 `ext` の画像）を置いて行を入れる。
fn add_item_as(lib: &Library, bytes: &[u8], ext: &'static str, w: i64, h: i64) -> i64 {
    let hash = *blake3::hash(bytes).as_bytes();
    let path = lib.layout().content_path(&hash, ext);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, bytes).unwrap();
    let size = bytes.len() as i64;
    lib.db()
        .write(move |tx| {
            tx.execute(
                "INSERT INTO items (hash, ext, kind, mime, size_bytes, width, height, title, original_name, original_path,
                                    source_site, imported_at, updated_at, thumb_state, proxy_state, analysis_state)
                 VALUES (?1, ?5, 'image', 'image/' || ?5, ?2, ?3, ?4, '絵', '絵.' || ?5, '/src/絵.' || ?5, 'local', 1, 1,
                         'ready', 'not_needed', 'ready')",
                params![&hash[..], size, w, h, ext],
            )?;
            Ok(tx.last_insert_rowid())
        })
        .unwrap()
}

/// 予算を決められる偽のファクトリ（固定の段だけのコーデックの組）。
struct Budgeted(u64);

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

/// 保存のジョブだけを動かすライブラリ（ジョブは [`Env::start_jobs`] まで動かさない）。
struct Env {
    _dir: tempfile::TempDir,
    lib: Arc<Library>,
    jobs: JobManager,
    engine: Arc<EngineServices>,
}

impl Env {
    fn new() -> Self {
        Self::with(Arc::new(FakeEngineFactory::default()))
    }

    fn with(factory: Arc<dyn EngineFactory>) -> Self {
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

    fn env(&self) -> EditorEnv<'_> {
        EditorEnv {
            library: &self.lib,
            jobs: &self.jobs,
            engine: &self.engine,
        }
    }

    fn start_jobs(&self) {
        self.jobs.start().unwrap();
        assert!(self.jobs.wait_idle(WAIT));
    }

    fn open(&self, item_id: i64) -> Opened {
        self.open_with(item_id, Viewport::whole(), false)
    }

    fn open_with(&self, item_id: i64, viewport: Viewport, from_draft: bool) -> Opened {
        open(
            &self.env(),
            &OpenRequest {
                item_id,
                viewport,
                from_draft,
            },
        )
        .unwrap()
    }

    fn step(&self, session: &str, steps: Vec<Step>) -> StepOutcome {
        step(&self.env(), session, steps, &Viewport::whole()).unwrap()
    }

    fn usage(&self) -> kura_engine::budget::Usage {
        self.engine.runtime().unwrap().budget().usage().unwrap()
    }
}

fn flip(axis: &str) -> Step {
    Step::Commit {
        edit: Edit::AddNode {
            at: 0,
            op: "image.flip".into(),
            v: 1,
            params: json!({ "axis": axis }),
            seed: None,
            extra_inputs: Vec::new(),
        },
        mode: CommitMode::Step,
    }
}

fn shown(v: &ViewOutcome) -> &ViewInfo {
    match v {
        ViewOutcome::Shown(info) => info,
        other => panic!("表示できませんでした: {other:?}"),
    }
}

/// 元（PNG）に文書を当てた全体の値（キャッシュなしの正）。
fn rendered(png: &[u8], g: &Graph) -> Value {
    rendered_as(png, "png", g)
}

/// 元（形式 `format`）に文書を当てた全体の値（キャッシュなしの正）。
fn rendered_as(bytes: &[u8], format: &str, g: &Graph) -> Value {
    let engine = Engine::new().unwrap();
    let (value, _) = CodecSet::pinned()
        .decode(
            &mut std::io::Cursor::new(bytes),
            format,
            &Default::default(),
            engine.limits(),
            &NoMonitor,
        )
        .unwrap();
    let plan = engine
        .compile(g, &BTreeMap::from([(InputId(0), value.shape())]))
        .unwrap();
    let inputs = BTreeMap::from([(
        InputId(0),
        InputValue {
            value,
            source: Digest(*blake3::hash(bytes).as_bytes()),
        },
    )]);
    engine
        .render(&plan, &inputs, &NoMonitor)
        .unwrap()
        .outputs
        .remove(0)
        .value
}

/// 全体の値 `full` のフレーム `frame` のタイル `tx, ty` の RGBA8 と大きさ（配信の道 `tile_rgba` とは別に作る：`read_rect` で行を
/// 詰めて読み、`for_each_rgba8_key` で色にする。各画素はスポイトの値（`sample`。16 ビットの四捨五入・パレットの色）とも照らす）。
fn expected_tile(full: &PartialValue, frame: u32, tx: u32, ty: u32) -> (u32, u32, Vec<u8>) {
    let r = full.frame_rect(frame).unwrap();
    let (x, y) = (tx * TILE, ty * TILE);
    let (w, h) = (TILE.min(r.w - x), TILE.min(r.h - y));
    let fmt = full.format().unwrap();
    let mut raw = vec![0u8; (w * h) as usize * fmt.bytes_per_pixel()];
    full.read_rect(
        frame,
        kura_engine::geom::IRect::new(i64::from(x), i64::from(y), w, h),
        &mut raw,
    )
    .unwrap();
    let palette = full.palette().map(|p| p.colors.as_slice());
    let mut out = Vec::new();
    kura_engine::imaging::convert::for_each_rgba8_key(fmt, palette, &raw, |k| {
        out.extend_from_slice(&k.to_be_bytes());
        true
    })
    .unwrap();
    for j in 0..h {
        for i in 0..w {
            let (cx, cy) = (r.x + i64::from(x + i), r.y + i64::from(y + j));
            let s = full.sample(frame, cx, cy).unwrap().unwrap();
            let at = ((j * w + i) * 4) as usize;
            assert_eq!(
                &out[at..at + 4],
                &s.rgba,
                "{fmt:?} フレーム {frame} の {cx},{cy}"
            );
        }
    }
    (w, h, out)
}

/// 配信したタイル（RGBA8 と PNG）が、全体の値のフレーム `frame` の同じタイル（[`expected_tile`]。右端・下端は小さい）と同じ。
/// PNG は読み直して大きさと画素を照らす。
fn assert_tiles_match(env: &Env, session: &str, rev: u64, level: u8, frame: u32, expected: &Value) {
    let full = PartialValue::full(expected.clone());
    let rect = full.frame_rect(frame).unwrap();
    let (nx, ny) = (rect.w.div_ceil(TILE), rect.h.div_ceil(TILE));
    let get = |tx: u32, ty: u32, format: TileFormat| {
        tile::serve(
            &env.engine,
            &TileRequest {
                session,
                rev,
                level,
                frame,
                tx,
                ty,
                format,
            },
        )
    };
    for ty in 0..ny {
        for tx in 0..nx {
            let (w, h, want) = expected_tile(&full, frame, tx, ty);
            let got = get(tx, ty, TileFormat::Rgba).unwrap();
            assert_eq!(got, want, "フレーム {frame} のタイル {tx},{ty}（RGBA8）");
            let png = get(tx, ty, TileFormat::Png).unwrap();
            let (decoded, _) = CodecSet::pinned()
                .decode(
                    &mut std::io::Cursor::new(png),
                    "png",
                    &Default::default(),
                    Engine::new().unwrap().limits(),
                    &NoMonitor,
                )
                .unwrap();
            let decoded = PartialValue::full(decoded);
            assert_eq!(decoded.format(), Some(PixelFormat::Rgba8));
            let r = decoded.frame_rect(0).unwrap();
            assert_eq!(
                (r.w, r.h),
                (w, h),
                "フレーム {frame} のタイル {tx},{ty}（PNG）"
            );
            let mut px = vec![0u8; want.len()];
            decoded.read_rect(0, r, &mut px).unwrap();
            assert_eq!(px, want, "フレーム {frame} のタイル {tx},{ty}（PNG）");
        }
    }
    // 範囲の外のタイルはない
    assert_eq!(get(nx, 0, TileFormat::Rgba).unwrap_err().status(), 404);
    assert_eq!(get(0, ny, TileFormat::Rgba).unwrap_err().status(), 404);
}

/// 開くと対話の使用量と取り置きが 0 より大きく、閉じると 0（NFR-17・ADR-154）。閉じたセッションには答えない。
#[test]
fn open_reserves_the_budget_and_close_returns_it() {
    let env = Env::new();
    let id = add_item(&env.lib, &sprite_png(40, 30, 1), 40, 30);
    let opened = env.open(id);
    let u = env.usage();
    assert!(u.interactive > 0 && u.reserved > 0, "{u:?}");
    assert_eq!(u.reserved, kura_engine::budget::default_reserved(u.total));
    assert_eq!(opened.item_id, id);
    assert_eq!(opened.base_version, 0);
    assert_eq!(opened.source.kind, "image");
    assert_eq!((opened.source.width, opened.source.height), (40, 30));
    assert!(!opened.draft.exists);
    assert!(!opened.outcome.status.dirty);
    let info = shown(&opened.outcome.view);
    assert_eq!(
        (info.level, info.full, info.canvas),
        (0, (40, 30), (40, 30))
    );
    assert_eq!(info.changed, ChangedTiles::All);
    assert_eq!(opened.outcome.doc.base, None);
    assert!(opened.outcome.doc.nodes.is_empty());

    close(&env.engine, &opened.session);
    let u = env.usage();
    assert_eq!((u.interactive, u.reserved, u.standing), (0, 0, 0), "{u:?}");
    assert!(matches!(
        step(&env.env(), &opened.session, Vec::new(), &Viewport::whole()),
        Err(EditorError::NoSession)
    ));
    // 2 回閉じても、知らないセッションを閉じても何もしない
    close(&env.engine, &opened.session);
    close(&env.engine, "abc");
}

/// ライブラリを閉じると、開いていたエディタも閉じる（取り置きが 0 に戻る）。
#[test]
fn closing_the_library_closes_the_editor() {
    let dir = tempfile::tempdir().unwrap();
    let services = Services::new(Arc::new(FakeBackend))
        .with_engine(Arc::new(FakeEngineFactory::default()) as Arc<dyn EngineFactory>);
    let session =
        LibrarySession::create(&dir.path().join("L"), Arc::new(|_| {}), services).unwrap();
    session.wait_ready();
    let id = add_item(session.library(), &sprite_png(16, 16, 2), 16, 16);
    let env = EditorEnv::of(&session).unwrap();
    let opened = open(
        &env,
        &OpenRequest {
            item_id: id,
            viewport: Viewport::whole(),
            from_draft: false,
        },
    )
    .unwrap();
    let engine = session.engine().unwrap().clone();
    assert!(engine.editor_for(&opened.session).is_some());
    assert!(engine.runtime().unwrap().budget().usage().unwrap().reserved > 0);
    session.close();
    let u = engine.runtime().unwrap().budget().usage().unwrap();
    assert_eq!((u.interactive, u.reserved), (0, 0), "{u:?}");
    assert!(engine.current_editor().is_none());
}

/// 手を当てると表示の経路で描き、配信するタイルはキャッシュなしの描画と同じ。取り消し・やり直しは戻った先の番号を返し、
/// 文書の差分は変わったノードだけ。
#[test]
fn steps_render_like_the_bulk_path_and_undo_returns_to_earlier_revs() {
    let env = Env::new();
    let png = sprite_png(300, 280, 3);
    let id = add_item(&env.lib, &png, 300, 280);
    let opened = env.open(id);
    let s = &opened.session;
    let rev0 = opened.outcome.status.rev;

    let out = env.step(s, vec![flip("horizontal")]);
    assert_eq!(out.rejected, None);
    assert_eq!(out.created, vec![NodeId(1)]);
    assert!(out.status.dirty && out.status.can_undo && !out.status.can_redo);
    assert_eq!(out.doc.base, Some(rev0));
    assert_eq!(out.doc.order, vec![NodeId(1)]);
    assert_eq!(out.doc.nodes[0].op, "image.flip");
    let info = shown(&out.view);
    let rev1 = info.rev;
    assert_eq!(rev1, out.status.rev);
    assert!(
        matches!(&info.changed, ChangedTiles::Tiles(t) if !t.is_empty()),
        "{:?}",
        info.changed
    );
    let mut g = Graph::new_single();
    g.push_linear("image.flip", 1, json!({ "axis": "horizontal" }), 0)
        .unwrap();
    assert_tiles_match(&env, s, rev1, 0, 0, &rendered(&png, &g));

    // 2 手をまとめて当てて、描くのは 1 回
    let out = env.step(
        s,
        vec![
            Step::Commit {
                edit: Edit::SetParam {
                    node: NodeId(1),
                    key: "axis".into(),
                    value: json!("vertical"),
                },
                mode: CommitMode::Step,
            },
            flip("horizontal"),
        ],
    );
    assert_eq!(out.created, vec![NodeId(2)]);
    assert_eq!(out.doc.order, vec![NodeId(2), NodeId(1)]);
    assert_eq!(out.doc.nodes.len(), 2, "足したノードと引数を変えたノード");
    let rev3 = shown(&out.view).rev;
    let mut g = Graph::new_single();
    g.push_linear("image.flip", 1, json!({ "axis": "horizontal" }), 0)
        .unwrap();
    g.push_linear("image.flip", 1, json!({ "axis": "vertical" }), 0)
        .unwrap();
    assert_tiles_match(&env, s, rev3, 0, 0, &rendered(&png, &g));

    // 取り消し：戻った先の手の番号（1 手目のあと）。やり直しは同じ番号
    let back = env.step(s, vec![Step::Undo, Step::Undo]);
    assert_eq!(back.status.rev, rev1);
    assert!(back.status.can_redo);
    assert_eq!(back.doc.order, vec![NodeId(1)]);
    let again = env.step(s, vec![Step::Redo]);
    assert_ne!(
        again.status.rev, rev3,
        "2 手目のあとの番号（1 手目と 2 手目は別の手）"
    );
    let again = env.step(s, vec![Step::Redo]);
    assert_eq!(again.status.rev, rev3);
    // 表示だけ：変わったタイルはない
    let same = view(&env.env(), s, &Viewport::whole()).unwrap();
    assert!(same.doc.is_empty());
    assert_eq!(shown(&same.view).changed, ChangedTiles::Tiles(Vec::new()));
}

/// 計画を作れない手は断り、状態を変えない（それより前の手は残る）。仮の値は確定・取り消しができる。
#[test]
fn invalid_steps_are_rejected_and_previews_commit_or_cancel() {
    let env = Env::new();
    let id = add_item(&env.lib, &sprite_png(32, 32, 4), 32, 32);
    let opened = env.open(id);
    let s = &opened.session;
    let out = env.step(
        s,
        vec![
            flip("vertical"),
            Step::Commit {
                edit: Edit::SetParam {
                    node: NodeId(1),
                    key: "axis".into(),
                    value: json!("diagonal"),
                },
                mode: CommitMode::Step,
            },
            flip("horizontal"),
        ],
    );
    let rejected = out.rejected.clone().unwrap();
    assert_eq!(rejected.index, 1);
    assert_eq!(rejected.problem.code, ProblemCode::Invalid);
    assert!(!rejected.problem.issues.is_empty(), "{rejected:?}");
    assert_eq!(
        out.doc.order,
        vec![NodeId(1)],
        "断った手のあとの手は当てない"
    );
    let before = out.status.rev;
    shown(&out.view);

    // 仮の値：履歴に入らない。捨てると頭に戻り、確定すると 1 手になる
    let set = |axis: &str| Edit::SetParam {
        node: NodeId(1),
        key: "axis".into(),
        value: json!(axis),
    };
    let p = env.step(
        s,
        vec![Step::Preview {
            edit: set("horizontal"),
        }],
    );
    assert!(p.status.provisional);
    assert_eq!(p.status.head, before);
    assert_ne!(p.status.rev, before);
    let c = env.step(s, vec![Step::CancelPreview]);
    assert_eq!((c.status.rev, c.status.provisional), (before, false));
    let p = env.step(
        s,
        vec![Step::Preview {
            edit: set("horizontal"),
        }],
    );
    let c = env.step(
        s,
        vec![Step::CommitPreview {
            mode: CommitMode::Step,
        }],
    );
    assert_eq!(c.status.rev, p.status.rev, "確定は仮の値の番号を引き継ぐ");
    assert_eq!(c.status.head, c.status.rev);
    // 断る仮の値は前の状態のまま
    let r = env.step(
        s,
        vec![Step::Preview {
            edit: set("diagonal"),
        }],
    );
    assert!(r.rejected.is_some());
    assert!(!r.status.provisional);
}

/// 描画：最初の 1 本は末尾に `paint.actions` を足す 1 手、次からはそのノードへ追記。文書の差分は数だけ。スポイトは等倍の値。
#[test]
fn paint_steps_append_to_the_last_paint_node_and_sample_reads_them() {
    let env = Env::new();
    let png = noise_png(64, 48, 5);
    let id = add_item(&env.lib, &png, 64, 48);
    let opened = env.open(id);
    let s = &opened.session;
    let red = [255, 0, 0, 255];
    let out = env.step(
        s,
        vec![Step::Paint {
            actions: vec![stroke(&[(2, 3), (10, 3)], red)],
        }],
    );
    assert_eq!(out.rejected, None);
    assert_eq!(out.created, vec![NodeId(1)]);
    assert_eq!(out.doc.nodes[0].op, "paint.actions");
    assert_eq!(out.doc.nodes[0].log.as_ref().map(|l| l.count), Some(1));
    let out = env.step(
        s,
        vec![Step::Paint {
            actions: vec![stroke(&[(5, 20)], [0, 0, 255, 255])],
        }],
    );
    assert!(out.created.is_empty(), "追記");
    assert_eq!(out.doc.nodes.len(), 1);
    assert_eq!(out.doc.nodes[0].log.as_ref().map(|l| l.count), Some(2));
    assert_eq!(out.doc.nodes[0].params, json!({ "actions": [] }));
    let undo_point = out.status.rev;
    // 末尾がほかの操作なら、新しい paint.actions を末尾に足す
    let out = env.step(
        s,
        vec![
            Step::Commit {
                edit: Edit::AddNode {
                    at: 1,
                    op: "image.flip".into(),
                    v: 1,
                    params: json!({ "axis": "vertical" }),
                    seed: None,
                    extra_inputs: Vec::new(),
                },
                mode: CommitMode::Step,
            },
            Step::Paint {
                actions: vec![stroke(&[(0, 0)], red)],
            },
        ],
    );
    assert_eq!(out.created, vec![NodeId(2), NodeId(3)]);
    assert_eq!(out.doc.order, vec![NodeId(1), NodeId(2), NodeId(3)]);
    let back = env.step(s, vec![Step::Undo, Step::Undo]);
    assert_eq!(back.status.rev, undo_point);

    assert_eq!(
        sample(&env.env(), s, 0, 4, 3).unwrap(),
        Some(Sampled {
            rgba: red,
            index: None
        })
    );
    assert_eq!(
        sample(&env.env(), s, 0, 5, 20).unwrap().map(|p| p.rgba),
        Some([0, 0, 255, 255])
    );
    assert_eq!(sample(&env.env(), s, 0, 64, 0).unwrap(), None, "画像の外");
    assert_eq!(sample(&env.env(), s, 0, -1, 0).unwrap(), None);
}

/// タイルは表示した今か直前の rev・段にだけ答える（ほかは 404）。PNG は RGBA8 と同じ画素。
#[test]
fn tiles_answer_only_the_published_views() {
    let env = Env::new();
    let png = sprite_png(300, 260, 6);
    let id = add_item(&env.lib, &png, 300, 260);
    let opened = env.open(id);
    let s = opened.session.clone();
    let rev0 = shown(&opened.outcome.view).rev;
    let rev1 = shown(&env.step(&s, vec![flip("horizontal")]).view).rev;
    let req = |rev: u64, level: u8, tx: u32, ty: u32, format: TileFormat| {
        tile::serve(
            &env.engine,
            &TileRequest {
                session: &s,
                rev,
                level,
                frame: 0,
                tx,
                ty,
                format,
            },
        )
    };
    // 今と直前の表示
    assert_eq!(
        req(rev1, 0, 1, 1, TileFormat::Rgba).unwrap().len(),
        44 * 4 * 4
    );
    assert_eq!(
        req(rev0, 0, 0, 0, TileFormat::Rgba).unwrap().len(),
        256 * 256 * 4
    );
    let rev2 = shown(&env.step(&s, vec![flip("vertical")]).view).rev;
    // 2 つ前の表示・違う段・範囲の外・違うセッションは 404
    for (what, r) in [
        ("2 つ前", req(rev0, 0, 0, 0, TileFormat::Rgba)),
        ("段", req(rev2, 1, 0, 0, TileFormat::Rgba)),
        ("範囲の外", req(rev2, 0, 2, 0, TileFormat::Rgba)),
        ("フレーム", {
            tile::serve(
                &env.engine,
                &TileRequest {
                    session: &s,
                    rev: rev2,
                    level: 0,
                    frame: 1,
                    tx: 0,
                    ty: 0,
                    format: TileFormat::Rgba,
                },
            )
        }),
    ] {
        assert_eq!(r.unwrap_err().status(), 404, "{what}");
    }
    let other = tile::serve(
        &env.engine,
        &TileRequest {
            session: "abc",
            rev: rev2,
            level: 0,
            frame: 0,
            tx: 0,
            ty: 0,
            format: TileFormat::Rgba,
        },
    );
    assert_eq!(other.unwrap_err().status(), 404);
    // PNG は同じ画素
    let raw = req(rev2, 0, 1, 0, TileFormat::Rgba).unwrap();
    let png_tile = req(rev2, 0, 1, 0, TileFormat::Png).unwrap();
    let (decoded, _) = CodecSet::pinned()
        .decode(
            &mut std::io::Cursor::new(png_tile),
            "png",
            &Default::default(),
            Engine::new().unwrap().limits(),
            &NoMonitor,
        )
        .unwrap();
    let (_, _, again) = tile::tile_rgba(&PartialValue::full(decoded), 0, 0, 0).unwrap();
    assert_eq!(again, raw);
    close(&env.engine, &s);
    assert_eq!(
        req(rev2, 0, 0, 0, TileFormat::Rgba).unwrap_err().status(),
        404
    );
}

/// 縮小して見ると段の計画で描き（段のタイル）、操作中は 1 段粗い。範囲を指すと範囲のタイルが有効になる。
#[test]
fn zoomed_out_views_use_coarser_levels() {
    let env = Env::new();
    let png = noise_png(1024, 768, 7);
    let id = add_item(&env.lib, &png, 1024, 768);
    let vp = Viewport {
        scale: 0.25,
        ..Viewport::whole()
    };
    let opened = env.open_with(id, vp.clone(), false);
    let info = shown(&opened.outcome.view);
    assert_eq!(
        (info.level, info.canvas, info.full),
        (2, (256, 192), (1024, 768))
    );
    let s = &opened.session;
    let moving = Viewport {
        interacting: true,
        ..vp.clone()
    };
    let out = step(&env.env(), s, Vec::new(), &moving).unwrap();
    assert_eq!(shown(&out.view).level, 3);
    assert_eq!(
        shown(&out.view).changed,
        ChangedTiles::All,
        "段を替えると全体"
    );
    // 等倍で一部だけを見る
    let part = Viewport {
        rect: Some(kura_engine::geom::IRect::new(300, 300, 100, 100)),
        ..Viewport::whole()
    };
    let out = step(&env.env(), s, vec![flip("horizontal")], &part).unwrap();
    let info = shown(&out.view);
    assert_eq!(info.level, 0);
    let tile = tile::serve(
        &env.engine,
        &TileRequest {
            session: s,
            rev: info.rev,
            level: 0,
            frame: 0,
            tx: 1,
            ty: 1,
            format: TileFormat::Rgba,
        },
    );
    assert!(tile.is_ok());
    // 見ていないタイルは描いていない
    let far = tile::serve(
        &env.engine,
        &TileRequest {
            session: s,
            rev: info.rev,
            level: 0,
            frame: 0,
            tx: 3,
            ty: 2,
            format: TileFormat::Rgba,
        },
    );
    assert_eq!(far.unwrap_err().status(), 404);
}

/// 予算：元を読む見積もりが予算の全体を超えれば TooLarge、空いていなければ Busy（どちらもエディタを残さない）。
#[test]
fn opening_respects_the_budget() {
    let env = Env::with(Arc::new(Budgeted(1 << 20)));
    let id = add_item(&env.lib, &noise_png(1024, 1024, 8), 1024, 1024);
    let r = open(
        &env.env(),
        &OpenRequest {
            item_id: id,
            viewport: Viewport::whole(),
            from_draft: false,
        },
    );
    assert!(matches!(r, Err(EditorError::TooLarge(_))), "{r:?}");
    assert!(env.engine.current_editor().is_none());
    let u = env.usage();
    assert_eq!((u.interactive, u.reserved), (0, 0));

    let env = Env::with(Arc::new(Budgeted(64 << 20)));
    let id = add_item(&env.lib, &noise_png(256, 256, 9), 256, 256);
    let budget = env.engine.runtime().unwrap().budget().clone();
    let hog = budget
        .try_acquire(budget.total(), Class::Bulk)
        .unwrap()
        .unwrap();
    let r = open(
        &env.env(),
        &OpenRequest {
            item_id: id,
            viewport: Viewport::whole(),
            from_draft: false,
        },
    );
    assert!(matches!(r, Err(EditorError::Busy(_))), "{r:?}");
    assert!(env.engine.current_editor().is_none());
    drop(hog);
    let opened = env.open(id);
    shown(&opened.outcome.view);
}

/// 画像でないもの・ゴミ箱のもの・ないものは開かない。前のエディタは別のアイテムを開くと閉じる。
#[test]
fn open_refuses_other_kinds_and_replaces_the_previous_editor() {
    let env = Env::new();
    let a = add_item(&env.lib, &sprite_png(8, 8, 10), 8, 8);
    let b = add_item(&env.lib, &sprite_png(8, 8, 11), 8, 8);
    let first = env.open(a);
    let second = env.open(b);
    assert_ne!(first.session, second.session);
    assert!(env.engine.editor_for(&first.session).is_none());
    assert!(env.engine.editor_for(&second.session).is_some());
    assert!(is_session_id(&second.session), "{}", second.session);
    let r = open(
        &env.env(),
        &OpenRequest {
            item_id: 999,
            viewport: Viewport::whole(),
            from_draft: false,
        },
    );
    assert!(matches!(r, Err(EditorError::NotFound(_))), "{r:?}");
    env.lib
        .db()
        .write(move |tx| {
            tx.execute("UPDATE items SET trashed_at = 1 WHERE id = ?1", [a])?;
            tx.execute("UPDATE items SET kind = 'audio' WHERE id = ?1", [b])?;
            Ok(())
        })
        .unwrap();
    for id in [a, b] {
        let r = open(
            &env.env(),
            &OpenRequest {
                item_id: id,
                viewport: Viewport::whole(),
                from_draft: false,
            },
        );
        assert!(
            matches!(
                r,
                Err(EditorError::Conflict(_) | EditorError::Invalid { .. })
            ),
            "{r:?}"
        );
    }
}

/// 下書き：頭が変わると置き、閉じても残る（S17）。下書きから開くと同じ文書で dirty、保存した頭に戻ると捨てる。
#[test]
fn drafts_follow_the_head_and_reopen_as_unsaved() {
    let env = Env::new();
    let id = add_item(&env.lib, &sprite_png(20, 20, 12), 20, 20);
    let opened = env.open(id);
    let s = &opened.session;
    env.step(s, vec![flip("horizontal")]);
    env.engine.current_editor().unwrap().autosave.flush();
    let draft = env.lib.db().read(|c| drafts::get(c, id)).unwrap().unwrap();
    assert_eq!(draft.base.version, 0);
    assert_eq!(draft.after_job, None);
    // 保存した頭（開いた文書）に戻ると捨てる
    env.step(s, vec![Step::Undo]);
    env.engine.current_editor().unwrap().autosave.flush();
    assert!(env.lib.db().read(|c| drafts::get(c, id)).unwrap().is_none());
    env.step(s, vec![Step::Redo, flip("vertical")]);
    close(&env.engine, s);
    let draft = env.lib.db().read(|c| drafts::get(c, id)).unwrap().unwrap();

    // 開き直す：下書きがあることを知らせる（頭の文書で開く）
    let plain = env.open(id);
    assert!(plain.draft.exists && plain.draft.fresh && !plain.draft.opened);
    assert!(plain.outcome.doc.nodes.is_empty());
    // 下書きから開く：同じ文書で、保存していない
    let restored = env.open_with(id, Viewport::whole(), true);
    assert!(restored.draft.opened);
    assert_eq!(restored.outcome.doc.order.len(), 2);
    assert!(restored.outcome.status.dirty);
    let editor = env.engine.current_editor().unwrap();
    let head_digest = {
        let st = editor.lock_state();
        let st = st.as_ref().unwrap();
        kura_engine::doc_parts::split(&st.session.head().to_graph())
            .unwrap()
            .root
    };
    assert_eq!(head_digest, draft.doc);
    // 捨てる
    assert!(discard_draft(&env.env(), id).unwrap());
    assert!(!discard_draft(&env.env(), id).unwrap());
}

/// 保存：積んだあとも編集を続けられ、終わるまで次の保存は Busy。終わると土台が新しい頭になり、積んだときの手に保存の印が
/// 付く（その手に戻ると dirty でない）。保存のあとに続けた編集の下書きは、新しい頭を土台にする。
#[test]
fn saving_continues_editing_and_marks_the_saved_rev() {
    let env = Env::new();
    let png = sprite_png(24, 20, 13);
    let id = add_item(&env.lib, &png, 24, 20);
    let opened = env.open(id);
    let s = &opened.session;
    let saved_rev = env.step(s, vec![flip("horizontal")]).status.rev;
    let started = save(&env.env(), s, SaveMode::Overwrite, None).unwrap();
    assert_eq!(started.status.saving, Some(started.job));
    assert!(started.doc.is_empty());
    // 保存の間も編集を続ける
    let more = env.step(s, vec![flip("vertical")]);
    assert_eq!(more.status.saving, Some(started.job));
    let busy = save(&env.env(), s, SaveMode::Overwrite, None);
    assert!(matches!(busy, Err(EditorError::Busy(_))), "{busy:?}");

    env.start_jobs();
    let after = view(&env.env(), s, &Viewport::whole()).unwrap();
    assert_eq!(after.status.saving, None);
    assert!(after.status.dirty, "頭は保存した手より後");
    assert!(
        after.notices.iter().any(|n| n == "保存しました"),
        "{:?}",
        after.notices
    );
    let head = env
        .lib
        .db()
        .read(|c| versions::head(c, id))
        .unwrap()
        .unwrap();
    assert_eq!(head.version, 1);
    {
        let editor = env.engine.current_editor().unwrap();
        let st = editor.lock_state();
        let st = st.as_ref().unwrap();
        assert_eq!(
            st.base,
            Base {
                hash: head.hash,
                version: 1
            }
        );
    }
    let back = env.step(s, vec![Step::Undo]);
    assert_eq!(back.status.rev, saved_rev);
    assert!(!back.status.dirty, "保存した手に戻った");
    // 続けた編集の下書きは新しい頭を土台にする
    env.step(s, vec![Step::Redo]);
    env.engine.current_editor().unwrap().autosave.flush();
    let draft = env.lib.db().read(|c| drafts::get(c, id)).unwrap().unwrap();
    assert_eq!((draft.base.version, draft.after_job), (1, None));
    assert!(env.lib.db().read(|c| drafts::is_fresh(c, &draft)).unwrap());

    // 出力の形式を変えて保存：形式を変える手を確定してから積む
    let started = save(
        &env.env(),
        s,
        SaveMode::Overwrite,
        Some(FormatChoice::To {
            format: "webp".into(),
            options: json!({}),
        }),
    )
    .unwrap();
    assert!(started.doc.outputs.is_some());
    assert!(started.status.can_undo);
    assert!(env.jobs.wait_idle(WAIT));
    let after = view(&env.env(), s, &Viewport::whole()).unwrap();
    assert!(!after.status.dirty);
    let head = env
        .lib
        .db()
        .read(|c| versions::head(c, id))
        .unwrap()
        .unwrap();
    assert_eq!((head.version, head.ext.as_str()), (2, "webp"));
    // 版の一覧
    let list = env.lib.db().read(|c| version_list(c, id)).unwrap();
    assert_eq!(
        list.iter().map(|v| (v.version, v.kind)).collect::<Vec<_>>(),
        vec![(0, "original"), (1, "edit"), (2, "edit")]
    );
    assert_eq!(list[1].file_state, "waiting");
}

/// 別名で保存は保存の印だけ付け（土台は元のアイテムのまま）、版に戻したら開き直す（手・保存は Conflict）。
#[test]
fn save_as_marks_saved_and_revert_requires_reopening() {
    let env = Env::new();
    env.start_jobs();
    let id = add_item(&env.lib, &sprite_png(24, 20, 14), 24, 20);
    let opened = env.open(id);
    let s = &opened.session;
    env.step(s, vec![flip("horizontal")]);
    save(
        &env.env(),
        s,
        SaveMode::SaveAs {
            title: "写し".into(),
            stem: "copy".into(),
        },
        None,
    )
    .unwrap();
    assert!(env.jobs.wait_idle(WAIT));
    let after = view(&env.env(), s, &Viewport::whole()).unwrap();
    assert!(!after.status.dirty);
    {
        let editor = env.engine.current_editor().unwrap();
        let st = editor.lock_state();
        assert_eq!(
            st.as_ref().unwrap().base.version,
            0,
            "元のアイテムは変わらない"
        );
    }
    // 上書きしてから元に戻す（別名で作ったアイテムの今と同じ中身の上書きは Conflict なので、もう 1 手当ててから）
    env.step(s, vec![flip("vertical")]);
    save(&env.env(), s, SaveMode::Overwrite, None).unwrap();
    assert!(env.jobs.wait_idle(WAIT));
    let after = view(&env.env(), s, &Viewport::whole()).unwrap();
    assert!(!after.status.dirty, "{:?}", after.notices);
    save(&env.env(), s, SaveMode::Revert { to: 0 }, None).unwrap();
    assert!(env.jobs.wait_idle(WAIT));
    let after = view(&env.env(), s, &Viewport::whole()).unwrap();
    assert!(after.status.needs_reopen);
    let r = step(&env.env(), s, vec![flip("vertical")], &Viewport::whole());
    assert!(matches!(r, Err(EditorError::Conflict(_))), "{r:?}");
    let r = save(&env.env(), s, SaveMode::Overwrite, None);
    assert!(matches!(r, Err(EditorError::Conflict(_))), "{r:?}");
}

/// 新しい要求に取って代わられた描画は Superseded（当てた手は残る）。
#[test]
fn superseded_renders_are_reported() {
    let env = Env::new();
    let id = add_item(&env.lib, &sprite_png(64, 64, 15), 64, 64);
    let opened = env.open(id);
    let editor = env.engine.editor_for(&opened.session).unwrap();
    let stale = editor.latest.issue(editor.cancel.clone());
    let newer = editor.latest.issue(editor.cancel.clone());
    editor.latest.supersede(newer.generation());
    let mut guard = editor.lock_state();
    let st = guard.as_mut().unwrap();
    let applied = editor
        .runtime
        .preview(|e| apply_steps(e, st, vec![flip("horizontal")]));
    assert_eq!(applied.1, None);
    // 前回の表示があると継ぎはぎで描くので、確実に描くところを通すよう前回の表示を捨てる
    st.view = None;
    let r = editor.render(st, &Viewport::whole(), &stale).unwrap();
    assert_eq!(r, ViewOutcome::Superseded);
    assert_eq!(st.session.head().node_count(), 1, "当てた手は残る");
    let r = editor.render(st, &Viewport::whole(), &newer).unwrap();
    shown(&r);
}

/// 保存が予算を待って駐車していれば、エディタのキャッシュを空にする（入力は数え直し、取り置きは外さない）。固定した値で
/// キャッシュの確保が取り置きを超えていた分が戻り、駐車が解けて一括が通る（エディタは開いたまま）。
#[test]
fn a_parked_save_empties_the_editor_cache() {
    // 予算 16 MiB・取り置き 8 MiB。入力 4.5 MiB と、編集中のノード（反転）の出力 4.5 MiB を固定すると、持ち続ける確保は
    // 9 MiB（取り置きを超える）
    let env = Env::with(Arc::new(Budgeted(16 << 20)));
    let (w, h) = (1024u32, 1152u32);
    let input_bytes = u64::from(w) * u64::from(h) * 4;
    let id = add_item(&env.lib, &noise_png(w, h, 16), i64::from(w), i64::from(h));
    let opened = env.open(id);
    let s = opened.session.clone();
    let focused = Viewport {
        focus: Some(NodeId(1)),
        ..Viewport::whole()
    };
    let out = step(&env.env(), &s, vec![flip("horizontal")], &focused).unwrap();
    shown(&out.view);
    let editor = env.engine.editor_for(&s).unwrap();
    let budget = env.engine.runtime().unwrap().budget().clone();
    let u = budget.usage().unwrap();
    assert_eq!(
        (u.reserved, u.standing),
        (8 << 20, 2 * input_bytes),
        "{u:?}"
    );
    // 「予算 − 持ち続ける確保」（7 MiB）を超える一括の要求（7.5 MiB）は駐車する
    let need = 7_864_320;
    let got = Arc::new(Mutex::new(None));
    let waiter = {
        let (budget, got) = (budget.clone(), got.clone());
        std::thread::spawn(move || {
            let lease = budget.acquire(need, Class::Bulk, &NoMonitor).unwrap();
            *got.lock().unwrap() = Some(lease.bytes());
            lease
        })
    };
    let deadline = std::time::Instant::now() + WAIT;
    while budget.usage().unwrap().parked == 0 {
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
    }
    let mut notices = Vec::new();
    {
        let mut guard = editor.lock_state();
        let st = guard.as_mut().unwrap();
        editor.relieve(st, &mut notices);
        assert_eq!(st.cache.bytes(), input_bytes, "入力だけ数える");
        // 入力だけになっていれば、もう空にしない
        editor.relieve(st, &mut notices);
    }
    assert_eq!(notices.len(), 1);
    // 駐車が解けて一括が通る（エディタは開いたまま、取り置きは外さない）
    while got.lock().unwrap().is_none() {
        assert!(std::time::Instant::now() < deadline, "駐車が解けません");
        std::thread::sleep(Duration::from_millis(5));
    }
    let lease = waiter.join().unwrap();
    assert_eq!(lease.bytes(), need);
    let u = budget.usage().unwrap();
    assert_eq!(
        (u.reserved, u.standing, u.parked),
        (8 << 20, 8 << 20, 0),
        "{u:?}"
    );
    assert!(env.engine.editor_for(&s).is_some());
    drop(lease);
    close(&env.engine, &s);
}

/// 描画の手を作る規則（末尾が有効な paint.actions なら追記、外したノードや別の操作なら足す）。
#[test]
fn paint_edit_follows_the_tail() {
    let engine = Engine::new().unwrap();
    let mut g = Graph::new_single();
    let snap = |g: &Graph| {
        EditSession::open(&engine, g, Digest::default(), HistoryLimits::default())
            .unwrap()
            .head()
            .clone()
    };
    let a = vec![stroke(&[(0, 0)], [1, 2, 3, 255])];
    assert!(matches!(
        paint_edit(&engine, &snap(&g), a.clone()).unwrap(),
        Edit::AddNode { at: 0, ref op, .. } if op == "paint.actions"
    ));
    g.push_linear("paint.actions", 1, json!({ "actions": [] }), 0)
        .unwrap();
    assert!(matches!(
        paint_edit(&engine, &snap(&g), a.clone()).unwrap(),
        Edit::AppendItems {
            node: NodeId(1),
            ..
        }
    ));
    g.nodes[0].enabled = false;
    assert!(matches!(
        paint_edit(&engine, &snap(&g), a.clone()).unwrap(),
        Edit::AddNode { at: 1, .. }
    ));
}

/// 画素の形式（パレット・16 ビット・グレーとアルファ）によらず、配るタイルは全体の値と同じ RGBA8（16 ビットは四捨五入、パレット
/// は色に）。右端・下端の小さいタイルも、PNG も同じ画素。
#[test]
fn tiles_convert_every_pixel_format() {
    let env = Env::new();
    for (fmt, w, h, seed) in [
        (PixelFormat::Indexed8, 300, 280, 21),
        (PixelFormat::Rgba16, 260, 300, 22),
        (PixelFormat::GrayA8, 270, 257, 23),
    ] {
        let png = png_of(&InputGen::Noise { w, h, fmt, seed }.generate().unwrap());
        let id = add_item(&env.lib, &png, i64::from(w), i64::from(h));
        let opened = env.open(id);
        let info = shown(&opened.outcome.view);
        let expected = rendered(&png, &Graph::new_single());
        assert_eq!(
            PartialValue::full(expected.clone()).format(),
            Some(fmt),
            "元の形式のまま表示する"
        );
        assert_tiles_match(&env, &opened.session, info.rev, 0, 0, &expected);
        close(&env.engine, &opened.session);
    }
}

/// フレームの列（GIF）：開いたまま（操作なし。入力をそのまま配る）と、反転したあとの、見ているフレームのタイルは全体の値の
/// そのフレームと同じ（右端・下端も）。手のあと、見ていないフレームは描かないので 404。表示だけの要求でフレームを替えると、
/// そのフレームのタイルを配る。
#[test]
fn tiles_follow_the_viewed_frame() {
    let env = Env::new();
    let (w, h) = (300u32, 270u32);
    let frames = InputGen::NoiseFrames {
        w,
        h,
        fmt: PixelFormat::Indexed8,
        count: 3,
        seed: 24,
    }
    .generate()
    .unwrap();
    let gif = CodecSet::pinned()
        .encode_bytes(&frames, "gif", &json!({}), &NoMonitor)
        .unwrap()
        .0;
    let id = add_item_as(&env.lib, &gif, "gif", i64::from(w), i64::from(h));
    let expected = rendered_as(&gif, "gif", &Graph::new_single());
    assert_eq!(PartialValue::full(expected.clone()).frame_count(), 3);
    let at = |frame: u32| Viewport {
        frame,
        ..Viewport::whole()
    };
    let opened = env.open_with(id, at(1), false);
    assert_eq!(opened.source.frames, Some(3));
    let s = &opened.session;
    let info = shown(&opened.outcome.view);
    assert_tiles_match(&env, s, info.rev, 0, 1, &expected);
    // 反転する（フレームごと）。描くのは見ているフレームだけ
    let out = step(&env.env(), s, vec![flip("horizontal")], &at(1)).unwrap();
    let info = shown(&out.view);
    let mut g = Graph::new_single();
    g.push_linear("image.flip", 1, json!({ "axis": "horizontal" }), 0)
        .unwrap();
    let expected = rendered_as(&gif, "gif", &g);
    assert_tiles_match(&env, s, info.rev, 0, 1, &expected);
    let frame0 = tile::serve(
        &env.engine,
        &TileRequest {
            session: s,
            rev: info.rev,
            level: 0,
            frame: 0,
            tx: 0,
            ty: 0,
            format: TileFormat::Rgba,
        },
    );
    assert_eq!(
        frame0.unwrap_err().status(),
        404,
        "見ていないフレームは描かない"
    );
    let out = step(&env.env(), s, Vec::new(), &at(2)).unwrap();
    let info = shown(&out.view);
    assert_tiles_match(&env, s, info.rev, 0, 2, &expected);
    close(&env.engine, s);
}

/// 仮の値がある間に、計画を作れない仮の値・確定の手を断っても、前の仮の値・番号・表示はそのまま（断った手は状態を変えない）。
/// そのまま表示・スポイト・仮の値の確定ができる。
#[test]
fn rejected_steps_keep_the_previous_provisional() {
    let env = Env::new();
    let png = sprite_png(32, 32, 4);
    let id = add_item(&env.lib, &png, 32, 32);
    let opened = env.open(id);
    let s = &opened.session;
    let crop = Step::Commit {
        edit: Edit::AddNode {
            at: 0,
            op: "image.crop".into(),
            v: 1,
            params: json!({ "rect": { "x": 0, "y": 0, "w": 16, "h": 16 } }),
            seed: None,
            extra_inputs: Vec::new(),
        },
        mode: CommitMode::Step,
    };
    let out = env.step(s, vec![crop]);
    assert_eq!(out.rejected, None);
    let set = |w: u32| Edit::SetParam {
        node: NodeId(1),
        key: "rect".into(),
        value: json!({ "x": 0, "y": 0, "w": w, "h": w }),
    };
    let p = env.step(s, vec![Step::Preview { edit: set(20) }]);
    assert!(p.status.provisional);
    assert_eq!(shown(&p.view).canvas, (20, 20));
    // 画像の外へ切り抜く手は計画を作れない（仮の値・確定のどちらで送っても）
    for step in [
        Step::Preview { edit: set(40) },
        Step::Commit {
            edit: set(50),
            mode: CommitMode::Step,
        },
    ] {
        let r = env.step(s, vec![step.clone()]);
        let rejected = r.rejected.clone().unwrap();
        assert_eq!(
            (rejected.index, rejected.problem.code),
            (0, ProblemCode::Invalid),
            "{step:?}"
        );
        assert_eq!(r.status, p.status, "{step:?}：仮の値・番号はそのまま");
        let info = shown(&r.view);
        assert_eq!(
            (info.rev, info.canvas),
            (p.status.rev, (20, 20)),
            "{step:?}"
        );
        let v = view(&env.env(), s, &Viewport::whole()).unwrap();
        assert_eq!(shown(&v.view).rev, p.status.rev, "{step:?}");
        assert!(sample(&env.env(), s, 0, 19, 19).unwrap().is_some());
    }
    // 仮の値は確定できる
    let c = env.step(
        s,
        vec![Step::CommitPreview {
            mode: CommitMode::Step,
        }],
    );
    assert_eq!((c.status.rev, c.status.provisional), (p.status.rev, false));
    let mut g = Graph::new_single();
    g.push_linear(
        "image.crop",
        1,
        json!({ "rect": { "x": 0, "y": 0, "w": 20, "h": 20 } }),
        0,
    )
    .unwrap();
    assert_tiles_match(&env, s, c.status.rev, 0, 0, &rendered(&png, &g));
}

/// 同じアイテムの保存が待っている間は、エディタを閉じて開き直しても次の保存を Busy で断る（DB のジョブで見る。土台がずれて
/// Conflict になるため）。形式を変える手も確定しない。保存の口（`start_save`）も同じで、断るときは文書の部品も置かない。ほかの
/// アイテムは積める。待っている保存が終われば積める。
#[test]
fn a_pending_save_blocks_the_next_save_even_after_reopening() {
    let env = Env::new();
    let png = sprite_png(24, 20, 13);
    let id = add_item(&env.lib, &png, 24, 20);
    let other = add_item(&env.lib, &sprite_png(8, 8, 18), 8, 8);
    let opened = env.open(id);
    env.step(&opened.session, vec![flip("horizontal")]);
    let first = save(&env.env(), &opened.session, SaveMode::Overwrite, None).unwrap();
    close(&env.engine, &opened.session);
    // 開き直す（保存はまだ動いていない）
    let again = env.open(id);
    let s = &again.session;
    assert_eq!(again.base_version, 0);
    let edited = env.step(s, vec![flip("vertical")]);
    for format in [
        None,
        Some(FormatChoice::To {
            format: "webp".into(),
            options: json!({}),
        }),
    ] {
        let busy = save(&env.env(), s, SaveMode::Overwrite, format);
        assert!(matches!(busy, Err(EditorError::Busy(_))), "{busy:?}");
    }
    let still = view(&env.env(), s, &Viewport::whole()).unwrap();
    assert_eq!(still.status, edited.status, "形式を変える手も確定しない");
    // 保存の口も同じ（部品も置かない）
    let hash = *blake3::hash(&png).as_bytes();
    let mut graph = Graph::new_single();
    graph
        .push_linear("image.flip", 1, json!({ "axis": "vertical" }), 0)
        .unwrap();
    graph
        .push_linear("image.flip", 1, json!({ "axis": "vertical" }), 0)
        .unwrap();
    let req = |item_id: i64, base_hash: [u8; 32]| SaveRequest {
        item_id,
        base: Base {
            hash: base_hash,
            version: 0,
        },
        action: SaveAction::Overwrite {
            graph: graph.clone(),
        },
    };
    let r = start_save(env.lib.db(), &env.jobs, &req(id, hash));
    assert!(matches!(r, Err(CoreError::Busy(_))), "{r:?}");
    let root = kura_engine::doc_parts::split(&graph).unwrap().root;
    assert!(
        env.lib
            .db()
            .read(|c| store::doc_id_by_digest(c, &root))
            .unwrap()
            .is_none(),
        "断った保存の文書の部品は置かない"
    );
    // ほかのアイテムは積める
    let other_hash: [u8; 32] = env
        .lib
        .db()
        .read(|c| {
            Ok(
                c.query_row("SELECT hash FROM items WHERE id = ?1", [other], |r| {
                    r.get::<_, Vec<u8>>(0)
                })?,
            )
        })
        .unwrap()
        .try_into()
        .unwrap();
    let other_job = start_save(env.lib.db(), &env.jobs, &req(other, other_hash)).unwrap();
    // 待っている保存が終われば積める
    env.start_jobs();
    for job in [first.job, other_job] {
        assert_eq!(env.jobs.get(job).unwrap().state, JobState::Done);
    }
    assert!(save(&env.env(), s, SaveMode::Overwrite, None).is_ok());
    assert!(env.jobs.wait_idle(WAIT));
    close(&env.engine, s);
}

/// 閉じている途中（前の要求がエディタの錠を持っている間）に別のアイテムを開いても、閉じ終わりが新しいエディタの取り置きを
/// 消さない（取り置きを外すのは持ち主のエディタだけ。§12.10・ADR-154）。新しいエディタを閉じると 0 に戻る。
#[test]
fn closing_while_another_item_opens_keeps_the_new_reservation() {
    let env = Env::new();
    let a = add_item(&env.lib, &sprite_png(16, 16, 1), 16, 16);
    let b = add_item(&env.lib, &sprite_png(16, 16, 2), 16, 16);
    let first = env.open(a);
    let editor_a = env.engine.editor_for(&first.session).unwrap();
    let guard = editor_a.lock_state();
    let closing = {
        let (engine, session) = (env.engine.clone(), first.session.clone());
        std::thread::spawn(move || close(&engine, &session))
    };
    // 置き場から外れた（閉じ終わりは錠を待っている）
    while env.engine.current_editor().is_some() {
        std::thread::sleep(Duration::from_millis(1));
    }
    let second = env.open(b);
    let reserved = env.usage().reserved;
    assert!(reserved > 0);
    drop(guard);
    closing.join().unwrap();
    let u = env.usage();
    assert_eq!(u.reserved, reserved, "{u:?}");
    assert!(env.engine.editor_for(&second.session).is_some());
    close(&env.engine, &second.session);
    let u = env.usage();
    assert_eq!((u.interactive, u.reserved), (0, 0), "{u:?}");
}

/// スポイトは描いている途中の表示を止めない（止めると、止めた要求の代わりの表示がどこからも返らない）：手の要求が錠を待って
/// いる間にスポイトを頼んでも、手の表示は描かれて公開される。
#[test]
fn sampling_does_not_supersede_a_render() {
    let env = Env::new();
    let id = add_item(&env.lib, &sprite_png(16, 16, 3), 16, 16);
    let opened = env.open(id);
    let s = opened.session.clone();
    let editor = env.engine.editor_for(&s).unwrap();
    let guard = editor.lock_state();
    let issued = editor.latest.latest();
    std::thread::scope(|scope| {
        let stepping = scope.spawn(|| {
            step(
                &env.env(),
                &s,
                vec![Step::Paint {
                    actions: vec![stroke(&[(1, 0), (1, 1)], [255, 0, 0, 255])],
                }],
                &Viewport::whole(),
            )
            .unwrap()
        });
        while editor.latest.latest() < issued + 1 {
            std::thread::yield_now();
        }
        let sampling = scope.spawn(|| sample(&env.env(), &s, 0, 1, 1).unwrap());
        std::thread::sleep(Duration::from_millis(20));
        assert_eq!(
            editor.latest.latest(),
            issued + 1,
            "スポイトは世代を払い出さない"
        );
        drop(guard);
        let stepped = stepping.join().unwrap();
        let picked = sampling.join().unwrap();
        let info = shown(&stepped.view);
        assert_eq!(info.rev, stepped.status.rev);
        assert!(picked.is_some());
        let p = editor.lock_published();
        assert_eq!(p.now.as_ref().map(|n| n.rev), Some(stepped.status.rev));
    });
    close(&env.engine, &s);
}

/// 実行の場の大きさをこの機械に任せたファクトリ（予算 2 GiB）。
struct Machine;

impl EngineFactory for Machine {
    fn runtime_config(&self) -> RuntimeConfig {
        RuntimeConfig {
            budget: Some(2 << 30),
            ..RuntimeConfig::default()
        }
    }

    fn codecs(&self, _work_dir: &Path) -> CodecSet {
        CodecSet::pinned()
    }
}

/// 1 手の往復の目安（Rust の中だけ：手を当てる → 計画 → 確保 → 表示の経路 → 文書の差分。IPC・タイルの配信・WebKit は
/// 含めない。NFR-11 の全体は段 6 で測る）。2048² の写真に 8px の筆（40 点）を 1 本ずつ足す：等倍の全体の表示と、縮小
/// （scale 0.25 → 段 2）の表示。release で
/// `cargo test --release -p kura-core --lib editor_step_latency -- --ignored --nocapture`。
#[test]
#[ignore]
fn editor_step_latency() {
    let env = Env::with(Arc::new(Machine));
    let id = add_item(&env.lib, &noise_png(2048, 2048, 17), 2048, 2048);
    for (what, vp) in [
        ("等倍の全体", Viewport::whole()),
        (
            "縮小（段 2）",
            Viewport {
                scale: 0.25,
                ..Viewport::whole()
            },
        ),
    ] {
        let opened = env.open_with(id, vp.clone(), false);
        let s = opened.session.clone();
        let mut ms = Vec::new();
        for i in 0..60i64 {
            let y = 20 + i * 30;
            let points: Vec<(i64, i64)> = (0..40).map(|k| (100 + k * 40, y + (k % 3))).collect();
            let action = serde_json::to_value(kura_engine::paint::PaintAction::Stroke {
                tool: kura_engine::paint::Tool::Pen,
                color: Some(kura_engine::paint::PaintColor::Rgba([255, 0, 0, 255])),
                blend: kura_engine::paint::Blend::Replace,
                brush: kura_engine::paint::stroke::Brush {
                    shape: kura_engine::paint::stroke::BrushShape::Square,
                    size: 8,
                },
                pixel_perfect: false,
                frame: 0,
                points: kura_engine::paint::PointList(
                    points
                        .iter()
                        .map(|&(x, y)| kura_engine::geom::IPoint { x, y })
                        .collect(),
                ),
            })
            .unwrap();
            let started = std::time::Instant::now();
            let out = step(
                &env.env(),
                &s,
                vec![Step::Paint {
                    actions: vec![action],
                }],
                &vp,
            )
            .unwrap();
            ms.push(started.elapsed().as_secs_f64() * 1000.0);
            shown(&out.view);
        }
        ms.sort_by(f64::total_cmp);
        let at = |q: f64| ms[((ms.len() - 1) as f64 * q).round() as usize];
        println!(
            "{what}：1 手 p50 {:.2} ms・p95 {:.2} ms・最大 {:.2} ms（{} 手。プレビューのスレッド {}）",
            at(0.5),
            at(0.95),
            at(1.0),
            ms.len(),
            env.engine.runtime().unwrap().preview_threads()
        );
        close(&env.engine, &s);
    }
}
