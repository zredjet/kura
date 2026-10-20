//! 解析のジョブ（kura-core）を Platform の段（ImageIO）で動かすテスト（決定 15：HEIC・HEIF の透過）。
//!
//! アプリと同じく、固定の段に ImageIO・AudioToolbox を足した組で解析のジョブを 1 本動かし、事実（素材の欄の値）と検索の
//! 一致を確かめる。ライブラリは一時フォルダに作る。

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use kura_core::analysis::{AnalyzeHandler, AnalyzeShared};
use kura_core::asset::{self, Alpha, AnalysisState};
use kura_core::edit::{CodecSet, ENGINE_WORK_DIR, EngineFactory, EngineServices, RuntimeConfig};
use kura_core::items::{ItemQuery, query_ids};
use kura_core::jobs::{EventSink, JobFilter, JobHandler, JobKind, JobManager, JobState};
use kura_core::library::Library;
use kura_core::query::{QueryContext, parse};
use kura_engine::codec::PinnedOptions;

use super::IMAGEIO_DEC;
use super::testutil::{engine_fixture, exclusive, fixture, stats};

/// 固定の段 → ImageIO・AudioToolbox の組（ffmpeg は足さない）と小さな予算。
struct MacFactory;

impl EngineFactory for MacFactory {
    fn runtime_config(&self) -> RuntimeConfig {
        RuntimeConfig {
            cores: Some(2),
            budget: Some(256 << 20),
            ..RuntimeConfig::default()
        }
    }

    fn codecs(&self, _work_dir: &Path) -> CodecSet {
        super::codec_set(&PinnedOptions::default())
    }
}

/// 中身 `bytes`・拡張子 `ext` のアイテムを ID `id` で入れる（解析を待つ画像）。
fn add(library: &Library, id: i64, ext: &str, bytes: &[u8]) {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join(format!("src.{ext}"));
    std::fs::write(&src, bytes).unwrap();
    let hash = kura_core::import::hash_file(&src).unwrap();
    let dest = library.layout().content_path(&hash, ext);
    std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
    std::fs::copy(&src, &dest).unwrap();
    let (ext, size) = (ext.to_string(), bytes.len() as i64);
    library
        .db()
        .write(move |tx| {
            tx.execute(
                "INSERT INTO items (id, hash, ext, kind, size_bytes, title, original_name, imported_at,
                                    updated_at, thumb_state, proxy_state, analysis_state)
                 VALUES (?1, ?2, ?3, 'image', ?4, 't', 't', ?1, ?1, 'ready', 'not_needed', 'pending')",
                (id, hash.as_slice(), ext, size),
            )?;
            Ok(())
        })
        .unwrap();
}

/// 検索の一致（ID の順）。
fn search(library: &Library, text: &str) -> Vec<u32> {
    let expr = parse(text).unwrap().expr;
    let mut ids = library
        .db()
        .read(move |c| {
            let query = ItemQuery {
                filter: Some(expr),
                ..ItemQuery::default()
            };
            let ctx = QueryContext {
                now_ms: 0,
                utc_offset_minutes: 0,
            };
            Ok(query_ids(c, &query, &ctx)?.ids)
        })
        .unwrap();
    ids.sort_unstable();
    ids
}

/// 完了条件（決定 15）：アルファのある HEIC（`heic-alpha-48x32.heic`）は見出しで透過ありと分かり、ImageIO で読んで解析する
/// （透過あり（二値か半透明）・色数を計る・デコーダは imageio-dec/1。検索の「透過:あり」に入る）。不透明な HEIC・HEIC の
/// シーケンス・JPEG は今のまま読まずに形式から書く（透過なし・色数は計らない・デコーダなし。ImageIO の展開は 1 回だけ）。
#[test]
fn heic_with_alpha_is_analyzed_through_imageio() {
    let _g = exclusive();
    let dir = tempfile::tempdir().unwrap();
    let library = Arc::new(Library::create(&dir.path().join("L.kuralib")).unwrap());
    add(&library, 1, "heic", &fixture("heic-alpha-48x32.heic"));
    add(&library, 2, "heic", &fixture("heic-gradient-300x200.heic"));
    add(&library, 3, "heic", &fixture("heics-3x24x16.heic"));
    add(&library, 4, "jpg", &engine_fixture("jpeg-422.jpg"));
    let engine = Arc::new(EngineServices::new(
        Arc::new(MacFactory),
        library.layout().staging_dir().join(ENGINE_WORK_DIR),
    ));
    let sink: EventSink = Arc::new(|_| {});
    let jobs = JobManager::new(
        library.clone(),
        vec![(
            JobKind::Analyze,
            Arc::new(AnalyzeHandler::new(
                engine.clone(),
                Arc::new(AnalyzeShared::default()),
            )) as Arc<dyn JobHandler>,
        )],
        sink,
    )
    .unwrap();
    // 組と実行の場は先に作る（展開の回数を照らす前に）
    engine.runtime().unwrap();
    let _codecs = engine.codecs();
    let (live, decodes) = stats();
    jobs.start().unwrap();
    jobs.ensure_queued(JobKind::Analyze, "{}").unwrap();
    assert!(jobs.wait_idle(Duration::from_secs(60)));
    let record = jobs
        .list(&JobFilter {
            kinds: vec![JobKind::Analyze],
            states: vec![],
            limit: Some(1),
        })
        .unwrap()
        .remove(0);
    assert_eq!(record.state, JobState::Done, "{:?}", record.message);
    assert_eq!(
        stats(),
        (live, decodes + 1),
        "ImageIO で展開したのはアルファのある HEIC だけ"
    );

    let info = |id: i64| library.db().read(move |c| asset::get(c, id)).unwrap();
    let alpha = info(1);
    assert_eq!(alpha.analysis_state, AnalysisState::Ready);
    let facts = alpha.facts.unwrap();
    assert!(facts.ok);
    assert_eq!(facts.decoder.as_deref(), Some(IMAGEIO_DEC));
    assert!(
        matches!(facts.image.alpha, Some(Alpha::Binary | Alpha::Partial)),
        "{:?}",
        facts.image.alpha
    );
    assert!(facts.image.colors.is_some());
    assert_eq!(facts.image.frame_count, Some(1));
    for id in 2..=4 {
        let photo = info(id);
        assert_eq!(photo.analysis_state, AnalysisState::Ready, "{id}");
        let facts = photo.facts.unwrap();
        assert_eq!(facts.image.alpha, Some(Alpha::None), "{id}");
        assert_eq!(facts.image.colors, None, "{id}");
        assert_eq!(facts.image.pixel_art, Some(false), "{id}");
        assert_eq!(facts.decoder, None, "{id}");
    }
    assert_eq!(search(&library, "透過:あり"), [1]);
    assert_eq!(search(&library, "透過:なし"), [2, 3, 4]);
    let summary: serde_json::Value =
        serde_json::from_str(record.result_json.as_deref().unwrap()).unwrap();
    assert_eq!(summary["headerMs"]["count"].as_u64(), Some(3));
    assert_eq!(summary["readMs"]["count"].as_u64(), Some(1));
    jobs.shutdown();
}
