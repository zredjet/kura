//! サムネイルの道の割り振り（M11 の決定 3）と、道の指紋（settings の `'thumbs.plan'`。plan defaults 20）。
//!
//! - 道は種類と形式で決める（純粋な関数 [`thumb_route`]）：エンジンは png・apng・gif・webp・aseprite・psd・psb・svg、
//!   アルファを保つ ImageIO は jpeg・heic・heif・avif・tiff・bmp（形式の表にない画像も）、フォントは CoreText、3D は kura-engine の
//!   glb の読み手と描画、動画・音楽は今までの道。
//! - 形式ごとの作り方の名前を指紋（[`plan`]）にして settings に置き、ライブラリを開くたびに前回と比べる（[`sync_plan`]）。
//!   作り方が変わった形式の `ready` は `stale`（今の絵を出したまま、`pending` のあとに作り直す）に、`failed` は `pending` に戻す。
//!   指紋がまだないライブラリ（M10 まで）は、M10 の作り方（[`legacy_plan`]：画像はすべて ImageIO の JPEG、フォント・3D は作らない）
//!   と比べる。

use std::collections::BTreeMap;

use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use crate::Result;
use crate::import::{MediaKind, format_id};
use crate::settings;
use kura_engine::codec::formats::{FORMATS, ItemKind};

/// settings のキー（サムネイルの道の指紋）。
pub const PLAN_KEY: &str = "thumbs.plan";

/// 指紋の形の版（形を変えたら上げる。値の比べ方は形式ごと）。
const PLAN_V: u32 = 1;

/// エンジンの道の作り方の名前（描き方・縮め方・入れ物の選び方を変えたら上げる。上げると、その形式を作り直す）。
pub const ENGINE_THUMBS: &str = "engine-thumbs/1";
/// アルファを保つ ImageIO の道。
const IMAGEIO_ALPHA: &str = "imageio-alpha/1";
/// M10 までの ImageIO の道（いつも JPEG）。JPEG は透明を持たないので、アルファを保つ道でも同じ絵になる。
const IMAGEIO_JPEG: &str = "imageio/1";
const VIDEO: &str = "video/1";
const AUDIO: &str = "audio/1";
const FONT: &str = "coretext/1";
/// M10 まで（作らない。フォント・3D は failed）。
const NONE: &str = "none";

/// エンジンで描く画像の形式（形式の表の ID）。
pub const ENGINE_FORMATS: [&str; 6] = ["png", "gif", "webp", "aseprite", "psd", "svg"];
/// アルファを保つ ImageIO で描く画像の形式。
pub const IMAGEIO_FORMATS: [&str; 6] = ["jpeg", "heic", "heif", "avif", "tiff", "bmp"];

/// サムネイルを作る道。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThumbRoute {
    /// kura-engine で読み、先頭のフレームを Box で縮め、透明があれば PNG・なければ JPEG（`engine_thumbs`）。
    Engine,
    /// OS（ImageIO）でアルファを保って（`MediaBackend::image_thumbnails`）。
    ImageIo,
    /// CoreText のフォントの見本（`MediaBackend::font_thumbnails`）。
    Font,
    /// kura-engine の glb の読み手とソフトウェアの描画（`engine_thumbs`）。
    Model,
    /// 動画のフレーム（AVFoundation・ffmpeg）。
    Video,
    /// 音楽のジャケット。
    Audio,
}

/// 種類と拡張子からサムネイルの道を決める。形式の表にない画像は ImageIO（M10 までと同じ）。
pub fn thumb_route(kind: MediaKind, ext: &str) -> ThumbRoute {
    match kind {
        MediaKind::Image => match format_id(&ext.to_ascii_lowercase()) {
            Some(id) if ENGINE_FORMATS.contains(&id) => ThumbRoute::Engine,
            _ => ThumbRoute::ImageIo,
        },
        MediaKind::Video => ThumbRoute::Video,
        MediaKind::Audio => ThumbRoute::Audio,
        MediaKind::Font => ThumbRoute::Font,
        MediaKind::Model => ThumbRoute::Model,
    }
}

/// 形式（表の ID）の作り方の名前。
fn way(id: &str, kind: ItemKind) -> &'static str {
    match MediaKind::from(kind) {
        MediaKind::Image if ENGINE_FORMATS.contains(&id) => ENGINE_THUMBS,
        MediaKind::Image if id == "jpeg" => IMAGEIO_JPEG,
        MediaKind::Image => IMAGEIO_ALPHA,
        MediaKind::Video => VIDEO,
        MediaKind::Audio => AUDIO,
        MediaKind::Font => FONT,
        MediaKind::Model => kura_engine::model::RENDERER,
    }
}

/// 指紋（settings の値）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Plan {
    pub v: u32,
    /// 形式（表の ID）→ 作り方の名前。
    pub formats: BTreeMap<String, String>,
}

/// 今の作り方の指紋（形式の表のすべての行）。
pub fn plan() -> Plan {
    Plan {
        v: PLAN_V,
        formats: FORMATS
            .iter()
            .map(|f| (f.id.to_string(), way(f.id, f.item_kind).to_string()))
            .collect(),
    }
}

/// M10 までの作り方（指紋のないライブラリと比べる）：画像はすべて ImageIO の JPEG、動画・音楽は今と同じ、フォント・3D は
/// 作らない。
pub fn legacy_plan() -> Plan {
    Plan {
        v: PLAN_V,
        formats: FORMATS
            .iter()
            .map(|f| {
                let way = match MediaKind::from(f.item_kind) {
                    MediaKind::Image => IMAGEIO_JPEG,
                    MediaKind::Video => VIDEO,
                    MediaKind::Audio => AUDIO,
                    MediaKind::Font | MediaKind::Model => NONE,
                };
                (f.id.to_string(), way.to_string())
            })
            .collect(),
    }
}

/// 前回と作り方が変わった形式の拡張子（小文字。並べたもの）。前回の指紋にない形式は「作らない」と比べる。
pub fn changed_exts(before: &Plan, now: &Plan) -> Vec<&'static str> {
    let mut exts: Vec<&'static str> = FORMATS
        .iter()
        .filter(|f| {
            let old = before.formats.get(f.id).map_or(NONE, String::as_str);
            let new = now.formats.get(f.id).map_or(NONE, String::as_str);
            old != new
        })
        .flat_map(|f| f.exts.iter().copied())
        .collect();
    exts.sort();
    exts
}

/// [`sync_plan`] の結果。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PlanSync {
    /// `ready` から `stale` にした数。
    pub stale: usize,
    /// `failed` から `pending` に戻した数。
    pub retried: usize,
}

/// 開くとき：前回の指紋と今の作り方を比べ、変わった形式の `ready` を `stale` に、`failed` を `pending` に戻して、今の指紋を
/// 記録する。同じなら何もしない。
///
/// 指紋が変わったときだけの一度きりの全件の走査（`SCAN items`。`ready`・`failed` には索引がない。
/// テスト `plan_updates_scan_items_only_when_the_plan_changes` に例外として書く）。
pub fn sync_plan(tx: &Connection) -> Result<PlanSync> {
    let now = plan();
    let before: Plan = settings::get(tx, PLAN_KEY)?.unwrap_or_else(legacy_plan);
    if before == now {
        return Ok(PlanSync::default());
    }
    let exts = changed_exts(&before, &now);
    let mut out = PlanSync::default();
    if !exts.is_empty() {
        let json = serde_json::to_string(&exts)?;
        let mut stale = tx.prepare_cached(
            "UPDATE items SET thumb_state = 'stale'
             WHERE thumb_state = 'ready' AND lower(ext) IN (SELECT value FROM json_each(?1))",
        )?;
        out.stale = stale.execute([&json])?;
        let mut retry = tx.prepare_cached(
            "UPDATE items SET thumb_state = 'pending'
             WHERE thumb_state = 'failed' AND lower(ext) IN (SELECT value FROM json_each(?1))",
        )?;
        out.retried = retry.execute([&json])?;
    }
    settings::set(tx, PLAN_KEY, &now)?;
    tracing::info!(
        stale = out.stale,
        retried = out.retried,
        ?exts,
        "サムネイルの作り方が変わった形式を作り直します"
    );
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{query_plan, test_conn};
    use rusqlite::params;

    /// 決定 3：形式ごとの道。形式の表の画像の行は、どれもエンジンか ImageIO のどちらかの一覧に書いてある（行を足したら
    /// どちらかに決めないと落ちる）。
    #[test]
    fn routes_formats_to_engine_imageio_font_model() {
        use ThumbRoute::*;
        for (ext, route) in [
            ("png", Engine),
            ("apng", Engine),
            ("gif", Engine),
            ("webp", Engine),
            ("aseprite", Engine),
            ("ase", Engine),
            ("psd", Engine),
            ("psb", Engine),
            ("svg", Engine),
            ("PNG", Engine),
            ("jpg", ImageIo),
            ("jpeg", ImageIo),
            ("heic", ImageIo),
            ("heif", ImageIo),
            ("avif", ImageIo),
            ("tif", ImageIo),
            ("tiff", ImageIo),
            ("bmp", ImageIo),
            ("raw", ImageIo),
        ] {
            assert_eq!(thumb_route(MediaKind::Image, ext), route, "{ext}");
        }
        for ext in ["ttf", "otf", "ttc", "woff", "woff2"] {
            assert_eq!(thumb_route(MediaKind::Font, ext), Font, "{ext}");
        }
        assert_eq!(thumb_route(MediaKind::Model, "glb"), Model);
        assert_eq!(thumb_route(MediaKind::Video, "mov"), Video);
        assert_eq!(thumb_route(MediaKind::Audio, "flac"), Audio);
        for f in FORMATS
            .iter()
            .filter(|f| MediaKind::from(f.item_kind) == MediaKind::Image)
        {
            assert!(
                ENGINE_FORMATS.contains(&f.id) != IMAGEIO_FORMATS.contains(&f.id),
                "画像の形式 {} の道を決めてください",
                f.id
            );
        }
    }

    /// 指紋は形式の表のすべての行を持ち、正規の JSON で安定している。M10 からは png・gif・webp・aseprite・psd・svg・heic など
    /// とフォント・3D が変わり、jpeg・動画・音楽は変わらない。
    #[test]
    fn the_plan_covers_every_format_and_changes_only_what_moved() {
        let now = plan();
        assert_eq!(now.formats.len(), FORMATS.len());
        assert_eq!(now, plan());
        let json = serde_json::to_string(&now).unwrap();
        assert_eq!(serde_json::from_str::<Plan>(&json).unwrap(), now);
        let exts = changed_exts(&legacy_plan(), &now);
        for ext in [
            "png", "apng", "gif", "webp", "aseprite", "ase", "psd", "psb", "svg", "heic", "heif",
            "avif", "tif", "tiff", "bmp", "ttf", "otf", "ttc", "woff", "woff2", "glb",
        ] {
            assert!(exts.contains(&ext), "{ext}");
        }
        for ext in ["jpg", "jpeg", "mov", "mp4", "flac", "mp3", "ogg", "wav"] {
            assert!(!exts.contains(&ext), "{ext}");
        }
        assert!(changed_exts(&now, &now).is_empty());
    }

    fn add(conn: &Connection, id: i64, kind: &str, ext: &str, state: &str) {
        conn.execute(
            "INSERT INTO items (id, hash, ext, kind, size_bytes, title, original_name, imported_at, updated_at, thumb_state)
             VALUES (?1, ?2, ?3, ?4, 1, 't', 't', ?1, ?1, ?5)",
            params![id, vec![id as u8; 32], ext, kind, state],
        )
        .unwrap();
    }

    fn state(conn: &Connection, id: i64) -> String {
        conn.query_row("SELECT thumb_state FROM items WHERE id = ?1", [id], |r| {
            r.get(0)
        })
        .unwrap()
    }

    /// plan tests 12：作り方が変わった形式の ready は stale に、failed は pending に戻る。jpeg・動画は変わらない。同じ指紋なら
    /// 何もしない。指紋の一部だけ変われば、その形式だけ。
    #[test]
    fn thumb_plan_change_marks_affected_items_stale() {
        let conn = test_conn();
        let rows = [
            (1, "image", "png", "ready"),
            (2, "image", "GIF", "ready"),
            (3, "image", "heic", "ready"),
            (4, "image", "jpg", "ready"),
            (5, "video", "mov", "ready"),
            (6, "font", "ttf", "failed"),
            (7, "model", "glb", "failed"),
            (8, "image", "webp", "failed"),
            (9, "image", "jpg", "failed"),
            (10, "image", "png", "pending"),
            (11, "audio", "flac", "failed"),
        ];
        for (id, kind, ext, s) in rows {
            add(&conn, id, kind, ext, s);
        }
        let sync = sync_plan(&conn).unwrap();
        assert_eq!(
            sync,
            PlanSync {
                stale: 3,
                retried: 3
            }
        );
        let states: Vec<String> = (1..=11).map(|id| state(&conn, id)).collect();
        assert_eq!(
            states,
            [
                "stale", "stale", "stale", "ready", "ready", "pending", "pending", "pending",
                "failed", "pending", "failed"
            ]
        );
        let stored: Plan = settings::get(&conn, PLAN_KEY).unwrap().unwrap();
        assert_eq!(stored, plan());

        // 同じ指紋なら何もしない
        conn.execute("UPDATE items SET thumb_state = 'ready' WHERE id = 1", [])
            .unwrap();
        assert_eq!(sync_plan(&conn).unwrap(), PlanSync::default());
        assert_eq!(state(&conn, 1), "ready");

        // 一部（gif）の作り方だけ変われば、その形式だけ
        let mut older = plan();
        older.formats.insert("gif".into(), "engine-thumbs/0".into());
        settings::set(&conn, PLAN_KEY, &older).unwrap();
        conn.execute(
            "UPDATE items SET thumb_state = 'ready' WHERE id IN (2, 3)",
            [],
        )
        .unwrap();
        assert_eq!(
            sync_plan(&conn).unwrap(),
            PlanSync {
                stale: 1,
                retried: 0
            }
        );
        assert_eq!(
            (state(&conn, 1), state(&conn, 2), state(&conn, 3)),
            ("ready".into(), "stale".into(), "ready".into())
        );
    }

    /// 例外（.claude/rules/database.md）：指紋が変わったときだけの一度きりの全件の走査。ready・failed には索引がない
    /// （索引は pending・stale だけ）ので SCAN items になる。指紋が同じなら走らない（上のテスト）。
    #[test]
    fn plan_updates_scan_items_only_when_the_plan_changes() {
        let conn = test_conn();
        for sql in [
            "UPDATE items SET thumb_state = 'stale'
             WHERE thumb_state = 'ready' AND lower(ext) IN (SELECT value FROM json_each('[\"png\"]'))",
            "UPDATE items SET thumb_state = 'pending'
             WHERE thumb_state = 'failed' AND lower(ext) IN (SELECT value FROM json_each('[\"png\"]'))",
        ] {
            let plan = query_plan(&conn, sql);
            assert!(plan.contains("SCAN items"), "{plan}");
        }
    }
}
