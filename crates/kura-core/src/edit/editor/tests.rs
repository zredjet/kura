//! エディタのセッションのテスト（偽のエンジンの部品・固定の段のコーデックで、Linux でも回る）：開く・手・取り消し・表示・
//! スポイト・タイルの配信・閉じると予算が 0・下書き・保存の連携。

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use kura_engine::budget::Class;
use kura_engine::codec::CodecSet;
use kura_engine::doc::{FormatChoice, InputId};
use kura_engine::render::InputValue;
use kura_engine::render::region::PartialValue;
use kura_engine::testgen::InputGen;
use kura_engine::value::PixelFormat;
use kura_engine::{Digest, Dim, Engine, Graph, NoMonitor, Value};
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

fn shown(v: &ViewOutcome) -> &ImageViewInfo {
    match v {
        ViewOutcome::Shown(ViewInfo::Image(info)) => info,
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
    let vp = Viewport::from(ImageViewport {
        scale: 0.25,
        ..ImageViewport::whole()
    });
    let opened = env.open_with(id, vp.clone(), false);
    let info = shown(&opened.outcome.view);
    assert_eq!(
        (info.level, info.canvas, info.full),
        (2, (256, 192), (1024, 768))
    );
    let s = &opened.session;
    let moving = Viewport::from(ImageViewport {
        interacting: true,
        ..vp.image()
    });
    let out = step(&env.env(), s, Vec::new(), &moving).unwrap();
    assert_eq!(shown(&out.view).level, 3);
    assert_eq!(
        shown(&out.view).changed,
        ChangedTiles::All,
        "段を替えると全体"
    );
    // 等倍で一部だけを見る
    let part = Viewport::from(ImageViewport {
        rect: Some(kura_engine::geom::IRect::new(300, 300, 100, 100)),
        ..ImageViewport::whole()
    });
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
            tx.execute("UPDATE items SET kind = 'video' WHERE id = ?1", [b])?;
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

/// 音声のアイテム（雑音の WAV）のファイルを置いて行を入れる。
fn add_wav(lib: &Library, rate: u32, frames: u64) -> i64 {
    let value = InputGen::AudioNoise {
        rate,
        channels: 1,
        frames,
        seed: 7,
    }
    .generate()
    .unwrap();
    let bytes = CodecSet::pinned()
        .encode_bytes(&value, "wav", &json!({}), &NoMonitor)
        .unwrap()
        .0;
    let hash = *blake3::hash(&bytes).as_bytes();
    let path = lib.layout().content_path(&hash, "wav");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, &bytes).unwrap();
    let size = bytes.len() as i64;
    lib.db()
        .write(move |tx| {
            tx.execute(
                "INSERT INTO items (hash, ext, kind, mime, size_bytes, title, original_name, original_path,
                                    source_site, imported_at, updated_at, thumb_state, proxy_state, analysis_state)
                 VALUES (?1, 'wav', 'audio', 'audio/wav', ?2, '音', '音.wav', '/src/音.wav', 'local', 1, 1,
                         'ready', 'not_needed', 'ready')",
                params![&hash[..], size],
            )?;
            Ok(tx.last_insert_rowid())
        })
        .unwrap()
}

/// 音声の操作を足す手。
fn audio_op(at: usize, op: &str, params: serde_json::Value) -> Step {
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

/// 音声の保存は、上書き・版に戻すの前（今）と後の長さをフレームとレートで返す（「長さが変わりました」の比べ。ms では 1 ms 未満の
/// 切り詰めもレート変換も同じになる）。前は確定した頭ではなく今（土台の版）で、保存を終えると新しい今になる。
#[test]
fn audio_saves_report_the_lengths_before_and_after() {
    let length = |frames, rate| AudioLength { frames, rate };
    let env = Env::new();
    let id = add_wav(&env.lib, 48_000, 96_000);
    let opened = env.open_with(id, Viewport::audio_idle(), false);
    let s = &opened.session;
    let vp = Viewport::audio_idle();

    // 1 ms 未満の切り詰め（どちらも 2000 ms）
    step(
        &env.env(),
        s,
        vec![audio_op(
            0,
            "audio.trim",
            json!({ "start": 0, "end": 95_990 }),
        )],
        &vp,
    )
    .unwrap();
    let trimmed = save(&env.env(), s, SaveMode::Overwrite, None).unwrap();
    assert_eq!(
        trimmed.lengths,
        Some(AudioLengths {
            before: length(96_000, 48_000),
            after: length(95_990, 48_000),
        })
    );
    env.start_jobs();
    let after = view(&env.env(), s, &vp).unwrap();
    assert!(!after.status.dirty, "{:?}", after.notices);

    // レート変換（今は切り詰めたあとの長さ。ms はほとんど変わらない）
    step(
        &env.env(),
        s,
        vec![audio_op(1, "audio.resample", json!({ "rate": 44_100 }))],
        &vp,
    )
    .unwrap();
    let resampled = save(&env.env(), s, SaveMode::Overwrite, None).unwrap();
    assert_eq!(
        resampled.lengths,
        Some(AudioLengths {
            before: length(95_990, 48_000),
            after: length(88_191, 44_100),
        })
    );
    assert!(env.jobs.wait_idle(WAIT));
    let after = view(&env.env(), s, &vp).unwrap();
    assert!(!after.status.dirty, "{:?}", after.notices);

    // 別名は前後を比べない（新しいアイテム）
    let copy = save(
        &env.env(),
        s,
        SaveMode::SaveAs {
            title: "写し".into(),
            stem: "写し".into(),
        },
        None,
    )
    .unwrap();
    assert_eq!(copy.lengths, None);
    assert!(env.jobs.wait_idle(WAIT));
    view(&env.env(), s, &vp).unwrap();

    // 版に戻す：後は戻す先の版の長さ（途中の版も、元も）
    let reverted = save(&env.env(), s, SaveMode::Revert { to: 1 }, None).unwrap();
    assert_eq!(
        reverted.lengths,
        Some(AudioLengths {
            before: length(88_191, 44_100),
            after: length(95_990, 48_000),
        })
    );
    assert!(env.jobs.wait_idle(WAIT));
    let opened = env.open_with(id, Viewport::audio_idle(), false);
    let to_original = save(
        &env.env(),
        &opened.session,
        SaveMode::Revert { to: 0 },
        None,
    )
    .unwrap();
    assert_eq!(
        to_original.lengths,
        Some(AudioLengths {
            before: length(95_990, 48_000),
            after: length(96_000, 48_000),
        })
    );
}

/// 画像の保存は長さを返さない（大きさはアイテムの概要で比べる）。
#[test]
fn image_saves_do_not_report_lengths() {
    let env = Env::new();
    let id = add_item(&env.lib, &sprite_png(24, 20, 13), 24, 20);
    let opened = env.open(id);
    env.step(&opened.session, vec![flip("horizontal")]);
    let started = save(&env.env(), &opened.session, SaveMode::Overwrite, None).unwrap();
    assert_eq!(started.lengths, None);
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
    let focused = Viewport::from(ImageViewport {
        focus: Some(NodeId(1)),
        ..ImageViewport::whole()
    });
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
    let at = |frame: u32| {
        Viewport::from(ImageViewport {
            frame,
            ..ImageViewport::whole()
        })
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

/// 利用者が保存を中止した（一時停止の間に中止した）：失敗の言葉を出さず、中止したと知らせ、保存していない変更のまま次の保存を
/// 積める（ADR-189）。
#[test]
fn canceling_a_save_tells_it_was_canceled_and_allows_the_next_save() {
    let env = Env::new();
    let png = sprite_png(24, 20, 13);
    let id = add_item(&env.lib, &png, 24, 20);
    let opened = env.open(id);
    let s = &opened.session;
    env.step(s, vec![flip("horizontal")]);
    let started = save(&env.env(), s, SaveMode::Overwrite, None).unwrap();
    env.jobs.pause(started.job).unwrap();
    assert_eq!(env.jobs.get(started.job).unwrap().state, JobState::Paused);
    let paused = view(&env.env(), s, &Viewport::whole()).unwrap();
    assert_eq!(
        paused.status.saving,
        Some(started.job),
        "一時停止の間も保存は待っている"
    );
    assert!(paused.notices.is_empty(), "{:?}", paused.notices);
    let busy = save(&env.env(), s, SaveMode::Overwrite, None);
    assert!(matches!(busy, Err(EditorError::Busy(_))), "{busy:?}");

    env.jobs.cancel(started.job).unwrap();
    let after = view(&env.env(), s, &Viewport::whole()).unwrap();
    assert_eq!(after.status.saving, None);
    assert!(after.status.dirty);
    assert_eq!(after.notices, vec!["保存を中止しました".to_string()]);
    assert!(save(&env.env(), s, SaveMode::Overwrite, None).is_ok());
}

/// 開いているエディタの外で積んだ同じアイテムの保存が待っている間は、エディタの次の保存を Busy で断る（DB のジョブで見る。土台が
/// ずれて Conflict になるため）。形式を変える手も確定しない。保存の口（`start_save`）も同じで、断るときは文書の部品も置かない。
/// ほかのアイテムは積める。
#[test]
fn a_pending_save_blocks_the_next_save_of_an_open_editor() {
    let env = Env::new();
    let png = sprite_png(24, 20, 13);
    let id = add_item(&env.lib, &png, 24, 20);
    let other = add_item(&env.lib, &sprite_png(8, 8, 18), 8, 8);
    let opened = env.open(id);
    let s = &opened.session;
    assert_eq!(opened.base_version, 0);
    let edited = env.step(s, vec![flip("vertical")]);
    let hash = *blake3::hash(&png).as_bytes();
    let doc = |axes: &[&str]| {
        let mut graph = Graph::new_single();
        for axis in axes {
            graph
                .push_linear("image.flip", 1, json!({ "axis": axis }), 0)
                .unwrap();
        }
        graph
    };
    let req = |item_id: i64, base_hash: [u8; 32], graph: Graph| SaveRequest {
        item_id,
        base: Base {
            hash: base_hash,
            version: 0,
        },
        action: SaveAction::Overwrite { graph },
    };
    let first = start_save(
        env.lib.db(),
        &env.jobs,
        &req(id, hash, doc(&["horizontal"])),
    )
    .unwrap();
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
    let refused = doc(&["vertical", "vertical"]);
    let r = start_save(env.lib.db(), &env.jobs, &req(id, hash, refused.clone()));
    assert!(matches!(r, Err(CoreError::Busy(_))), "{r:?}");
    let root = kura_engine::doc_parts::split(&refused).unwrap().root;
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
    let other_job = start_save(
        env.lib.db(),
        &env.jobs,
        &req(other, other_hash, doc(&["vertical"])),
    )
    .unwrap();
    env.start_jobs();
    for job in [first, other_job] {
        assert_eq!(env.jobs.get(job).unwrap().state, JobState::Done);
    }
    close(&env.engine, s);
}

/// 開こうとして断られた誤り（`Opened` は比べられないので、開けたら止める）。
fn refused_open(env: &EditorEnv<'_>, item_id: i64, from_draft: bool) -> EditorError {
    match open(
        env,
        &OpenRequest {
            item_id,
            viewport: Viewport::whole(),
            from_draft,
        },
    ) {
        Ok(_) => panic!("前の保存が終わっていないのに開けた"),
        Err(err) => err,
    }
}

/// `job` が終わるまで、開いても下書きから開いても `SavePending` で断られ、セッションを作らない。
fn assert_held_by(env: &EditorEnv<'_>, item_id: i64, job: JobId) {
    for from_draft in [false, true] {
        let err = refused_open(env, item_id, from_draft);
        assert!(
            matches!(err, EditorError::SavePending { job: j } if j == job),
            "{err:?}"
        );
        assert!(
            env.engine.current_editor().is_none(),
            "セッションを作らない"
        );
    }
}

/// 上書きを積み、手を足して閉じる（下書きは保存のあとの手まで。土台を待つジョブ付き）。
fn saved_then_edited_and_closed(env: &Env, item_id: i64) -> JobId {
    let opened = env.open(item_id);
    let s = &opened.session;
    env.step(s, vec![flip("horizontal")]);
    let job = save(&env.env(), s, SaveMode::Overwrite, None).unwrap().job;
    env.step(s, vec![flip("vertical")]);
    close(&env.engine, s);
    job
}

/// 保存のジョブが正しく終わったことを確かめ（Conflict で失敗していない）、アイテムの頭の版を返す。
fn assert_saved(env: &Env, item_id: i64, job: JobId) -> i64 {
    let record = env.jobs.get(job).unwrap();
    assert_eq!(record.state, JobState::Done, "{:?}", record.message);
    env.lib
        .db()
        .read(|c| versions::head(c, item_id))
        .unwrap()
        .unwrap()
        .version
}

/// 同じアイテムの保存が終わっていなければ開かない（待ち・一時停止。開いても下書きから開いても。ADR-189）：セッションがないので
/// 手は Rust に届かず、下書きは置き換わらない・消えない。ほかのアイテムは開ける。
#[test]
fn a_pending_save_refuses_opening_and_keeps_the_draft() {
    let env = Env::new();
    let id = add_item(&env.lib, &sprite_png(24, 20, 21), 24, 20);
    let other = add_item(&env.lib, &sprite_png(8, 8, 22), 8, 8);
    let opened = env.open(id);
    let s = opened.session.clone();
    env.step(&s, vec![flip("horizontal")]);
    let job = save(&env.env(), &s, SaveMode::Overwrite, None).unwrap().job;
    env.step(&s, vec![flip("vertical")]);
    close(&env.engine, &s);
    let draft = || env.lib.db().read(|c| drafts::get(c, id)).unwrap();
    let kept = draft().unwrap();
    assert_eq!(kept.after_job, Some(job));

    assert_held_by(&env.env(), id, job);
    env.jobs.pause(job).unwrap();
    assert_eq!(env.jobs.get(job).unwrap().state, JobState::Paused);
    assert_held_by(&env.env(), id, job);

    // 前のセッションの口にも手は届かない（前の問題：手で下書きが置き換わり、取り消すと消えた）
    let late = step(&env.env(), &s, vec![flip("horizontal")], &Viewport::whole());
    assert!(matches!(late, Err(EditorError::NoSession)), "{late:?}");
    assert_eq!(draft(), Some(kept), "下書きは置き換わらない・消えない");

    // ほかのアイテムは開いて編集できる。そのあとも同じアイテムは断る
    let o = env.open(other);
    env.step(&o.session, vec![flip("horizontal")]);
    close(&env.engine, &o.session);
    assert_held_by(&env.env(), id, job);
}

/// 再開して終わったら開ける：下書きは新しい頭の上で開け、続きの保存は Conflict にならず版が増える（結果まで確かめる。下書きから
/// でも、下書きを使わずに頭から編集しても。上書きでも別名で保存でも）。
#[test]
fn resuming_the_pending_save_lets_the_next_save_succeed() {
    let env = Env::new();
    let id = add_item(&env.lib, &sprite_png(24, 20, 23), 24, 20);
    let job = saved_then_edited_and_closed(&env, id);
    env.jobs.pause(job).unwrap();
    assert_held_by(&env.env(), id, job);
    env.jobs.resume(job).unwrap();
    env.start_jobs();
    assert_eq!(assert_saved(&env, id, job), 1);

    // 下書きは新しい頭に付け替わって開ける
    let plain = env.open(id);
    assert_eq!(plain.base_version, 1);
    assert!(plain.draft.exists && plain.draft.fresh && !plain.draft.opened);
    let restored = env.open_with(id, Viewport::whole(), true);
    assert!(restored.draft.opened);
    assert_eq!(restored.base_version, 1);
    assert_eq!(restored.outcome.doc.order.len(), 2, "保存のあとの手まで");
    assert!(restored.outcome.status.dirty);
    let s = &restored.session;
    let second = save(&env.env(), s, SaveMode::Overwrite, None).unwrap();
    assert!(env.jobs.wait_idle(WAIT));
    assert_eq!(assert_saved(&env, id, second.job), 2, "Conflict にならない");
    let after = view(&env.env(), s, &Viewport::whole()).unwrap();
    assert_eq!(after.status.saving, None);
    assert!(!after.status.dirty);
    close(&env.engine, s);

    // 下書きを使わずに開いても、頭の上で編集して保存できる（別名で保存も）
    let fresh = env.open(id);
    assert!(!fresh.draft.exists, "保存した文書と同じ下書きは消える");
    assert_eq!(fresh.base_version, 2);
    let s = &fresh.session;
    env.step(s, vec![flip("horizontal")]);
    let third = save(&env.env(), s, SaveMode::Overwrite, None).unwrap();
    assert!(env.jobs.wait_idle(WAIT));
    assert_eq!(assert_saved(&env, id, third.job), 3);
    view(&env.env(), s, &Viewport::whole()).unwrap();
    env.step(s, vec![flip("vertical")]);
    let copy = save(
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
    assert_eq!(
        assert_saved(&env, id, copy.job),
        3,
        "別名はこのアイテムの版を増やさない"
    );
    close(&env.engine, s);
}

/// 保存のあとに手がなければ、終わると下書きは消えていて（同じ文書）、シートなしで開ける。
#[test]
fn a_draft_of_the_same_doc_is_gone_after_the_pending_save() {
    let env = Env::new();
    let id = add_item(&env.lib, &sprite_png(24, 20, 24), 24, 20);
    let opened = env.open(id);
    let s = &opened.session;
    env.step(s, vec![flip("horizontal")]);
    let job = save(&env.env(), s, SaveMode::Overwrite, None).unwrap().job;
    close(&env.engine, s);
    assert!(env.lib.db().read(|c| drafts::get(c, id)).unwrap().is_some());
    assert_held_by(&env.env(), id, job);
    env.start_jobs();
    assert_eq!(assert_saved(&env, id, job), 1);
    let again = env.open(id);
    assert!(!again.draft.exists);
    assert!(!again.outcome.status.dirty);
    assert_eq!(again.outcome.doc.order.len(), 1, "保存した版の文書");
    close(&env.engine, &again.session);
}

/// 中止したら開ける：頭は変わらず、下書きは同じ土台のまま開けて、保存のあとの手まで保存できる（前の問題：中止すると編集が失われた）。
#[test]
fn canceling_the_pending_save_opens_from_the_draft() {
    let env = Env::new();
    let id = add_item(&env.lib, &sprite_png(24, 20, 25), 24, 20);
    let job = saved_then_edited_and_closed(&env, id);
    let kept = env.lib.db().read(|c| drafts::get(c, id)).unwrap().unwrap();
    env.jobs.pause(job).unwrap();
    assert_held_by(&env.env(), id, job);
    env.jobs.cancel(job).unwrap();
    assert_eq!(env.jobs.get(job).unwrap().state, JobState::Canceled);

    let plain = env.open(id);
    assert_eq!(plain.base_version, 0);
    assert!(plain.draft.exists && plain.draft.fresh);
    let restored = env.open_with(id, Viewport::whole(), true);
    assert_eq!(restored.outcome.doc.order.len(), 2);
    assert!(restored.outcome.status.dirty);
    let editor = env.engine.current_editor().unwrap();
    let digest = {
        let st = editor.lock_state();
        let st = st.as_ref().unwrap();
        kura_engine::doc_parts::split(&st.session.head().to_graph())
            .unwrap()
            .root
    };
    assert_eq!(digest, kept.doc, "下書きの文書");
    let s = &restored.session;
    let second = save(&env.env(), s, SaveMode::Overwrite, None).unwrap();
    env.start_jobs();
    assert_eq!(assert_saved(&env, id, second.job), 1);
    close(&env.engine, s);
}

/// アプリを開き直す（ジョブの管理を止め、同じライブラリに新しいジョブの管理・エンジンを作る。動かし始めるのは呼び手）。
fn restart(env: &Env) -> (Arc<EngineServices>, JobManager) {
    env.jobs.shutdown();
    let engine = Arc::new(EngineServices::new(
        Arc::new(FakeEngineFactory::default()),
        env.lib.layout().staging_dir().join(ENGINE_WORK_DIR),
    ));
    let sink: EventSink = Arc::new(|_| {});
    let jobs = JobManager::new(
        env.lib.clone(),
        vec![(JobKind::Edit, Arc::new(EditHandler::new(engine.clone())))],
        sink,
    )
    .unwrap();
    (engine, jobs)
}

/// アプリを開き直したあと（新しいジョブの管理・エンジン。DB の一時停止のジョブは残る）も開かない。再開して終われば開ける。
#[test]
fn a_paused_save_still_refuses_opening_after_a_restart() {
    let env = Env::new();
    let id = add_item(&env.lib, &sprite_png(24, 20, 26), 24, 20);
    let job = saved_then_edited_and_closed(&env, id);
    env.jobs.pause(job).unwrap();

    let (engine, jobs) = restart(&env);
    let restarted = EditorEnv {
        library: &env.lib,
        jobs: &jobs,
        engine: &engine,
    };
    assert_held_by(&restarted, id, job);
    jobs.resume(job).unwrap();
    jobs.start().unwrap();
    assert!(jobs.wait_idle(WAIT));
    assert_eq!(jobs.get(job).unwrap().state, JobState::Done);
    let opened = open(
        &restarted,
        &OpenRequest {
            item_id: id,
            viewport: Viewport::whole(),
            from_draft: true,
        },
    )
    .unwrap();
    assert_eq!(opened.base_version, 1);
    let second = save(&restarted, &opened.session, SaveMode::Overwrite, None).unwrap();
    assert!(jobs.wait_idle(WAIT));
    assert_eq!(jobs.get(second.job).unwrap().state, JobState::Done);
    close(&engine, &opened.session);
}

/// 迷う形も `save::pending_save` の見方に揃える：別名で保存（ジョブの入力のアイテムは元のアイテム）・版に戻すが待っている間も、
/// 元のアイテムは開かない（次の保存を Busy で断るのと同じ条件）。別名で保存が終われば元のアイテムの土台は変わらず、版に戻すが
/// 終われば頭が進む。
#[test]
fn save_as_and_revert_hold_the_item_they_came_from() {
    let env = Env::new();
    let id = add_item(&env.lib, &sprite_png(24, 20, 27), 24, 20);
    let opened = env.open(id);
    let s = &opened.session;
    env.step(s, vec![flip("horizontal")]);
    let copy = save(
        &env.env(),
        s,
        SaveMode::SaveAs {
            title: "写し".into(),
            stem: "copy".into(),
        },
        None,
    )
    .unwrap()
    .job;
    close(&env.engine, s);
    assert_held_by(&env.env(), id, copy);
    env.start_jobs();
    assert_eq!(
        assert_saved(&env, id, copy),
        0,
        "別名はこのアイテムの版を増やさない"
    );

    let opened = env.open(id);
    assert_eq!(opened.base_version, 0);
    let s = &opened.session;
    env.step(s, vec![flip("vertical")]);
    let first = save(&env.env(), s, SaveMode::Overwrite, None).unwrap().job;
    assert!(env.jobs.wait_idle(WAIT));
    assert_eq!(assert_saved(&env, id, first), 1);
    close(&env.engine, s);

    // 版に戻す（動かし始める前のジョブの管理で積む）
    let (engine, jobs) = restart(&env);
    let held = EditorEnv {
        library: &env.lib,
        jobs: &jobs,
        engine: &engine,
    };
    let opened = open(
        &held,
        &OpenRequest {
            item_id: id,
            viewport: Viewport::whole(),
            from_draft: false,
        },
    )
    .unwrap();
    let revert = save(&held, &opened.session, SaveMode::Revert { to: 0 }, None)
        .unwrap()
        .job;
    close(&engine, &opened.session);
    assert_held_by(&held, id, revert);
    jobs.start().unwrap();
    assert!(jobs.wait_idle(WAIT));
    assert_eq!(jobs.get(revert).unwrap().state, JobState::Done);
    let again = open(
        &held,
        &OpenRequest {
            item_id: id,
            viewport: Viewport::whole(),
            from_draft: false,
        },
    )
    .unwrap();
    assert_eq!(again.base_version, 2);
    close(&engine, &again.session);
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

/// 書ける形式が読む確定した頭の出力の形は、全体を compile した形と同じ：手のあと（今の計画）・取り消しのあと・プレビューの途中
/// （版ごとの計画）・版ごとの計画から頭が追い出されたあと（今の計画からの差分）。
#[test]
fn formats_read_the_head_shape_without_a_whole_compile() {
    let env = Env::new();
    let id = add_item(&env.lib, &sprite_png(16, 12, 31), 16, 12);
    let opened = env.open(id);
    let s = &opened.session;
    let editor = env.engine.current_editor().unwrap();
    let check = |what: &str, want: (u64, u64)| {
        let guard = editor.lock_state();
        let st = guard.as_ref().unwrap();
        let shape = formats::head_shape(&editor, st).unwrap();
        let graph = st.session.head().to_graph();
        let whole = editor
            .runtime
            .preview(|e| e.compile(&graph, &st.shapes))
            .unwrap();
        assert_eq!(shape, whole.output_shapes()[0].1, "{what}");
        let Shape::Image { w, h, .. } = shape else {
            panic!("{what}：{shape:?}");
        };
        assert_eq!((w, h), (Dim::Exact(want.0), Dim::Exact(want.1)), "{what}");
    };
    let crop = |at: usize, w: u32, h: u32| Edit::AddNode {
        at,
        op: "image.crop".into(),
        v: 1,
        params: json!({ "rect": { "x": 0, "y": 0, "w": w, "h": h } }),
        seed: None,
        extra_inputs: Vec::new(),
    };
    check("開いた直後", (16, 12));
    env.step(
        s,
        vec![Step::Commit {
            edit: crop(0, 8, 6),
            mode: CommitMode::Step,
        }],
    );
    check("手のあと", (8, 6));
    env.step(s, vec![Step::Undo]);
    check("取り消しのあと", (16, 12));
    env.step(s, vec![Step::Redo]);
    check("やり直しのあと", (8, 6));

    // プレビューの途中は確定した頭の形（仮の値の形ではない）
    let preview = |edit: Edit| Step::Preview { edit };
    let out = env.step(s, vec![preview(crop(1, 4, 4))]);
    assert_eq!(out.rejected, None);
    check("プレビューの途中", (8, 6));
    // 版ごとの計画から頭が追い出されても（仮の値を 8 つより多く替えた）、今の計画からの差分で作る
    for i in 0..10 {
        let out = env.step(s, vec![preview(crop(1, 1 + i % 8, 1 + i % 6))]);
        assert_eq!(out.rejected, None);
    }
    {
        let guard = editor.lock_state();
        let st = guard.as_ref().unwrap();
        let rev = st.session.head_rev();
        assert_ne!(st.plan_rev, rev);
        assert!(
            st.plans.iter().all(|(r, _)| *r != rev),
            "頭の計画はもうない"
        );
    }
    check("頭の計画を覚えていないプレビューの途中", (8, 6));
    let f = formats::formats(&env.env(), s).unwrap();
    assert_eq!(f.current, "png");
    env.step(s, vec![Step::CancelPreview]);
    check("プレビューをやめたあと", (8, 6));
    close(&env.engine, s);
}

/// 手のあとの書ける形式の読み（`edit_formats`）が手を待たせないことの測り：筆 5 万本の文書で、筆を 1 本描くたびに読む時間（全体を
/// compile する道と比べる）と、読み続けている間の手の時間（読まないときと比べる）。release で
/// `cargo test --release -p kura-core --lib formats_with_many_strokes -- --ignored --nocapture`。
#[test]
#[ignore]
fn formats_with_many_strokes() {
    use kura_engine::testing::bench::{pen, walk_stroke};
    const SIDE: u32 = 1024;
    let summary = |what: &str, mut ms: Vec<f64>| {
        ms.sort_by(f64::total_cmp);
        let at = |q: f64| ms[((ms.len() - 1) as f64 * q).round() as usize];
        println!(
            "{what}：p50 {:.3} ms・p95 {:.3} ms・最大 {:.3} ms（{} 回）",
            at(0.5),
            at(0.95),
            at(1.0),
            ms.len()
        );
    };
    let env = Env::with(Arc::new(Machine));
    let png = noise_png(SIDE, SIDE, 31);
    let id = add_item(&env.lib, &png, i64::from(SIDE), i64::from(SIDE));
    let action = |seed: u64, points: usize| {
        serde_json::to_value(pen(
            walk_stroke(seed, points, SIDE, SIDE, 12),
            4,
            false,
            [(seed % 251) as u8, 40, 200, 255],
        ))
        .unwrap()
    };
    let prior: Vec<Json> = (0..50_000u64).map(|i| action(1_000 + i, 50)).collect();
    let mut graph = Graph::new_single();
    graph
        .push_linear("paint.actions", 1, json!({ "actions": prior }), 0)
        .unwrap();
    let base = Base {
        hash: *blake3::hash(&png).as_bytes(),
        version: 0,
    };
    drafts::save(env.lib.db(), id, &graph, base, None).unwrap();
    let vp = Viewport::from(ImageViewport {
        scale: 0.25,
        ..ImageViewport::whole()
    });
    let opened = env.open_with(id, vp.clone(), true);
    let s = opened.session.clone();
    let paint = |i: u64| {
        let started = std::time::Instant::now();
        let steps = vec![Step::Paint {
            actions: vec![action(100_000 + i, 40)],
        }];
        step(&env.env(), &s, steps, &vp).unwrap();
        started.elapsed().as_secs_f64() * 1000.0
    };
    // 1 本描くたびに読む
    let mut reads = Vec::new();
    for i in 0..30 {
        paint(i);
        let started = std::time::Instant::now();
        formats::formats(&env.env(), &s).unwrap();
        reads.push(started.elapsed().as_secs_f64() * 1000.0);
    }
    summary("筆 5 万本：手のあとの読み", reads);
    // 比べ：全体を compile する道（今の頭の to_graph と compile。錠を持つ間）
    let editor = env.engine.current_editor().unwrap();
    let whole: Vec<f64> = (0..5)
        .map(|_| {
            let guard = editor.lock_state();
            let st = guard.as_ref().unwrap();
            let started = std::time::Instant::now();
            let graph = st.session.head().to_graph();
            editor
                .runtime
                .preview(|e| e.compile(&graph, &st.shapes))
                .unwrap();
            started.elapsed().as_secs_f64() * 1000.0
        })
        .collect();
    summary("比べ：全体を compile する道", whole);
    // 手と競るか：読まない間の手と、別のスレッドで読み続けている間の手
    let quiet: Vec<f64> = (100..130).map(paint).collect();
    summary("手（読まない間）", quiet);
    let done = AtomicBool::new(false);
    let contended = std::thread::scope(|scope| {
        scope.spawn(|| {
            while !done.load(Ordering::Relaxed) {
                formats::formats(&env.env(), &s).unwrap();
            }
        });
        let ms: Vec<f64> = (200..230).map(paint).collect();
        done.store(true, Ordering::Relaxed);
        ms
    });
    summary("手（別のスレッドで読み続けている間）", contended);
    close(&env.engine, &s);
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
            Viewport::from(ImageViewport {
                scale: 0.25,
                ..ImageViewport::whole()
            }),
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

/// 格子を書く手（`meta.set_grid`。位置 `at`）。
fn set_grid(at: usize) -> Step {
    Step::Commit {
        edit: Edit::AddNode {
            at,
            op: "meta.set_grid".into(),
            v: 1,
            params: json!({ "grid": { "cellW": 10, "cellH": 10, "gapX": 0, "gapY": 0, "offsetX": 0, "offsetY": 0 } }),
            seed: None,
            extra_inputs: Vec::new(),
        },
        mode: CommitMode::Step,
    }
}

/// レシピの見本のモード（M14 段 7）：見本の元（版 0 の空の文書）にレシピを継ぎ足して開き、結び付けはエディタの文書の ID に
/// 付け替える。見本の下書きは開かず・置かず・消さず、アイテムへの保存（上書き・別名・版に戻す）は断る。頭の文書からレシピへ
/// 戻す（見本で編集の保存）と同じレシピになる。ないレシピは NotFound。
#[test]
fn recipe_sample_opens_the_original_and_never_touches_the_item() {
    use kura_engine::recipe::{Binding, SettingKey, Source as BindFrom};

    let env = Env::new();
    let id = add_item(&env.lib, &sprite_png(20, 20, 12), 20, 20);
    let runtime = env.engine.runtime().unwrap().clone();
    // レシピを作る：裏返す → 格子（素材の格子に結ぶ）
    let opened = env.open(id);
    let s = opened.session.clone();
    env.step(&s, vec![flip("horizontal"), set_grid(1)]);
    let graph = head_graph(&env.env(), &s).unwrap();
    let keep: Vec<NodeId> = graph.nodes.iter().map(|n| n.id).collect();
    let grid_node = graph
        .nodes
        .iter()
        .find(|n| n.op == "meta.set_grid")
        .unwrap()
        .id;
    let recipe = crate::edit::recipes::create_from_editor(
        env.lib.db(),
        &runtime,
        "裏返して格子",
        "説明",
        crate::edit::recipes::EditorPick {
            graph: &graph,
            keep: &keep,
            with_format: false,
            bindings: vec![Binding {
                node: grid_node,
                param: "grid".into(),
                part: None,
                from: BindFrom::Setting {
                    key: SettingKey::Grid,
                },
            }],
        },
    )
    .unwrap();
    let before = crate::edit::recipes::get(env.lib.db(), &runtime, recipe).unwrap();
    // 見本の下書き（今の手）を置いて閉じる
    env.engine.current_editor().unwrap().autosave.flush();
    close(&env.engine, &s);
    let draft = env.lib.db().read(|c| drafts::get(c, id)).unwrap().unwrap();

    let sample = open_recipe(
        &env.env(),
        &OpenRequest {
            item_id: id,
            viewport: Viewport::whole(),
            from_draft: true,
        },
        recipe,
    )
    .unwrap();
    let info = sample.recipe.clone().unwrap();
    assert_eq!(info.id, recipe);
    assert_eq!(info.name, "裏返して格子");
    assert_eq!(info.description, "説明");
    assert!(!info.with_format);
    // 下書きから開かず、下書きがあることも言わない（見本のアイテムの下書きはこのモードと関係しない）
    assert!(!sample.draft.exists && !sample.draft.opened);
    assert_eq!(sample.base_version, 0);
    let ops: Vec<&str> = sample
        .outcome
        .doc
        .nodes
        .iter()
        .map(|n| n.op.as_str())
        .collect();
    assert_eq!(ops, ["image.flip", "meta.set_grid"]);
    // 結び付けはエディタの文書の格子のノードを指す
    let sample_graph = head_graph(&env.env(), &sample.session).unwrap();
    let sample_grid = sample_graph
        .nodes
        .iter()
        .find(|n| n.op == "meta.set_grid")
        .unwrap()
        .id;
    assert_eq!(info.bindings.len(), 1);
    assert_eq!(info.bindings[0].node, sample_grid);

    // 手を足しても下書きは置かない。アイテムには保存できない
    env.step(&sample.session, vec![flip("vertical")]);
    for mode in [
        SaveMode::Overwrite,
        SaveMode::SaveAs {
            title: "x".into(),
            stem: "x".into(),
        },
        SaveMode::Revert { to: 0 },
    ] {
        let err = save(&env.env(), &sample.session, mode, None).unwrap_err();
        assert!(matches!(err, EditorError::Invalid { .. }), "{err:?}");
    }
    env.step(&sample.session, vec![Step::Undo]);

    // 頭の文書からレシピへ戻すと同じレシピ（ID は付け直す）
    let back = head_graph(&env.env(), &sample.session).unwrap();
    let keep: Vec<NodeId> = back.nodes.iter().map(|n| n.id).collect();
    crate::edit::recipes::replace_from_editor(
        env.lib.db(),
        &runtime,
        recipe,
        crate::edit::recipes::EditorPick {
            graph: &back,
            keep: &keep,
            with_format: info.with_format,
            bindings: info.bindings.clone(),
        },
    )
    .unwrap();
    let after = crate::edit::recipes::get(env.lib.db(), &runtime, recipe).unwrap();
    assert_eq!(after.recipe, before.recipe);
    close(&env.engine, &sample.session);
    // 見本の下書きはそのまま、版も足していない
    let kept = env.lib.db().read(|c| drafts::get(c, id)).unwrap().unwrap();
    assert_eq!(kept.updated_at, draft.updated_at);
    assert_eq!(kept.doc_id, draft.doc_id);
    let head = env
        .lib
        .db()
        .read(|c| versions::head(c, id))
        .unwrap()
        .unwrap();
    assert_eq!(head.version, 0);

    // ないレシピ
    let missing = open_recipe(
        &env.env(),
        &OpenRequest {
            item_id: id,
            viewport: Viewport::whole(),
            from_draft: false,
        },
        recipe + 100,
    )
    .unwrap_err();
    assert!(matches!(missing, EditorError::NotFound(_)), "{missing:?}");
}

/// レシピの見本のモード（M14 段 7 の検証の直し）：見本に保存した版があっても、文書は元にレシピを継ぎ足したもの（見本の
/// 既存の編集は映らない）。N → 1 のレシピ（今の Kura で当てられないもの）は Invalid、前の保存が終わっていない見本は
/// SavePending。
#[test]
fn recipe_sample_ignores_saved_edits_and_refuses_merge_and_pending() {
    let env = Env::new();
    let id = add_item(&env.lib, &sprite_png(20, 20, 31), 20, 20);
    let runtime = env.engine.runtime().unwrap().clone();
    // レシピ：裏返す 1 つ
    let opened = env.open(id);
    let s = opened.session.clone();
    env.step(&s, vec![flip("horizontal")]);
    let graph = head_graph(&env.env(), &s).unwrap();
    let keep: Vec<NodeId> = graph.nodes.iter().map(|n| n.id).collect();
    let recipe = crate::edit::recipes::create_from_editor(
        env.lib.db(),
        &runtime,
        "裏返す",
        "",
        crate::edit::recipes::EditorPick {
            graph: &graph,
            keep: &keep,
            with_format: false,
            bindings: Vec::new(),
        },
    )
    .unwrap();
    // 見本に版を保存する（縦にも裏返して上書き）
    env.step(&s, vec![flip("vertical")]);
    save(&env.env(), &s, SaveMode::Overwrite, None).unwrap();
    env.start_jobs();
    close(&env.engine, &s);
    let head = env
        .lib
        .db()
        .read(|c| versions::head(c, id))
        .unwrap()
        .unwrap();
    assert_eq!(head.version, 1);

    let req = |item_id| OpenRequest {
        item_id,
        viewport: Viewport::whole(),
        from_draft: false,
    };
    let sample = open_recipe(&env.env(), &req(id), recipe).unwrap();
    let ops: Vec<(&str, Json)> = sample
        .outcome
        .doc
        .nodes
        .iter()
        .map(|n| (n.op.as_str(), n.params.clone()))
        .collect();
    assert_eq!(ops, [("image.flip", json!({ "axis": "horizontal" }))]);
    close(&env.engine, &sample.session);

    // N → 1 の印のレシピは開かない
    env.lib
        .db()
        .write(move |tx| {
            tx.execute(
                "UPDATE edit_recipes SET kind = 'merge' WHERE id = ?1",
                [recipe],
            )?;
            Ok(())
        })
        .unwrap();
    let merge = open_recipe(&env.env(), &req(id), recipe).unwrap_err();
    assert!(matches!(merge, EditorError::Invalid { .. }), "{merge:?}");
    env.lib
        .db()
        .write(move |tx| {
            tx.execute(
                "UPDATE edit_recipes SET kind = 'each' WHERE id = ?1",
                [recipe],
            )?;
            Ok(())
        })
        .unwrap();

    // 前の保存が終わっていない見本は開かない（ふだんの開き方と同じ）
    let other = add_item(&env.lib, &sprite_png(12, 12, 32), 12, 12);
    let o = env.open(other);
    env.step(&o.session, vec![flip("horizontal")]);
    let job = save(&env.env(), &o.session, SaveMode::Overwrite, None)
        .unwrap()
        .job;
    close(&env.engine, &o.session);
    env.jobs.pause(job).unwrap();
    let pending = open_recipe(&env.env(), &req(other), recipe).unwrap_err();
    assert!(
        matches!(pending, EditorError::SavePending { job: j } if j == job),
        "{pending:?}"
    );
}
