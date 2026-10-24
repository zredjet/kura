//! 取得したファイルをライブラリに入れる（docs/architecture.md §9 の「登録」「ユニット」）。
//!
//! 作業フォルダ（`staging/dl-<job>/`）からメディアのファイルとメタデータを集め、取り込みと同じ配置
//! （クローンでコピー → ハッシュ → `files/` へ rename）をしてから、1 つのトランザクションで
//! アイテム・タグ・フォルダ・ユニット・`remote_sources` を書く。

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use rayon::prelude::*;
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::Value;

use super::detect::Tool;
use super::meta::{FileMeta, TagPolicy, from_gallerydl, from_ytdlp};
use crate::Result;
use crate::db::Db;
use crate::import::place::{place, stage};
use crate::import::store::{
    NewItem, add_tag, add_to_folder, add_unit_member, create_unit, ensure_tag, find_owner,
    folder_exists, insert_item, restore_item, unit_exists,
};
use crate::import::{MediaKind, Rejected, classify_file, mime_for};
use crate::library::Layout;
use crate::media::inflight::{self, ProbeWay};
use crate::media::{MediaBackend, MediaInfo};
use crate::tags::TagName;
use crate::text::{natural_cmp, nfc};

/// 作業フォルダにできた 1 ファイル。
#[derive(Debug, Clone, PartialEq)]
pub struct Downloaded {
    pub path: PathBuf,
    /// 一緒に書かれたメタデータのファイル（あれば）。
    pub sidecar: Option<PathBuf>,
    pub name: String,
    pub ext: String,
    pub kind: MediaKind,
    pub meta: FileMeta,
}

/// メタデータの読み方。
#[derive(Debug, Clone)]
pub struct Collect<'a> {
    pub tool: Tool,
    pub site: &'a str,
    pub policy: TagPolicy,
    pub playlist_unit: bool,
    pub job_url: &'a str,
}

/// 作業フォルダからメディアのファイルを集める（対応していない形式・中身が拡張子と違うもの・書きかけ・メタデータは除く）。
/// 並びはまとまり（ユニット）の中の順、次にファイル名の自然順。
pub fn collect(dir: &Path, how: &Collect<'_>) -> Result<(Vec<Downloaded>, u64)> {
    let mut files = Vec::new();
    let mut unsupported = 0;
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok((files, 0)),
        Err(err) => return Err(err.into()),
    };
    for entry in entries {
        let path = entry?.path();
        if !path.is_file() {
            continue;
        }
        let Some(name) = path.file_name().map(|n| n.to_string_lossy().into_owned()) else {
            continue;
        };
        if name.starts_with('.') || name.ends_with(".json") || name.ends_with(".part") {
            continue;
        }
        let Some((stem, ext)) = name.rsplit_once('.') else {
            continue;
        };
        let ext = ext.to_ascii_lowercase();
        // yt-dlp がまとめる前の映像・音声（`<id>.f401.mp4`）と書きかけ（`.ytdl`・`.temp`）は使わない
        if ext == "ytdl" || ext == "temp" || is_format_fragment(stem) {
            continue;
        }
        // 取り込みと同じ判定（`.ase` は先頭のバイトで Aseprite かを見分ける）
        let kind = match classify_file(&path, &ext) {
            Ok(kind) => kind,
            Err(rejected) => {
                if !matches!(rejected, Rejected::Unsupported) {
                    tracing::warn!(path = %path.display(), reason = rejected.reason(), "取り込みません");
                }
                unsupported += 1;
                continue;
            }
        };
        let sidecar = match how.tool {
            Tool::YtDlp => dir.join(format!("{stem}.info.json")),
            Tool::GalleryDl => dir.join(format!("{name}.json")),
        };
        let sidecar = sidecar.is_file().then_some(sidecar);
        let json: Value = match &sidecar {
            Some(p) => std::fs::read(p)
                .ok()
                .and_then(|b| serde_json::from_slice(&b).ok())
                .unwrap_or(Value::Null),
            None => Value::Null,
        };
        let meta = match how.tool {
            Tool::YtDlp => from_ytdlp(&json, how.site, how.policy, how.playlist_unit, how.job_url),
            Tool::GalleryDl => from_gallerydl(&json, how.site, how.policy, how.job_url, stem),
        };
        let meta = if json.is_null() {
            FileMeta {
                title: stem.to_string(),
                ..meta
            }
        } else {
            meta
        };
        files.push(Downloaded {
            path,
            sidecar,
            name,
            ext,
            kind,
            meta,
        });
    }
    files.sort_by(|a, b| {
        let key = |d: &Downloaded| d.meta.group.as_ref().map(|g| (g.key.clone(), g.pos));
        match (key(a), key(b)) {
            (Some((ka, pa)), Some((kb, pb))) if ka == kb => pa.total_cmp(&pb),
            _ => natural_cmp(&a.name, &b.name),
        }
    });
    Ok((files, unsupported))
}

/// `<id>.f401` のような、まとめる前の形式ごとのファイル名か（`f` のあとに数字を含む英数字）。
fn is_format_fragment(stem: &str) -> bool {
    stem.rsplit_once('.').is_some_and(|(_, last)| {
        last.strip_prefix('f').is_some_and(|id| {
            id.chars().any(|c| c.is_ascii_digit())
                && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        })
    })
}

/// どこから来たか（`item_tags.source` と、まとめたときの `units.origin`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    Download,
    Capture,
}

impl Origin {
    fn tag_source(self) -> &'static str {
        match self {
            Self::Download => "download",
            Self::Capture => "capture",
        }
    }

    /// units.origin（キャプチャはユニットを作らないので、作るときはダウンロード扱い）。
    fn unit_origin(self) -> &'static str {
        "download"
    }
}

/// 入れたときの数。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Registered {
    pub added: u64,
    pub duplicates: u64,
    pub failed: u64,
    /// 入れた（または既存の）アイテム。
    pub item_ids: Vec<i64>,
    /// 作った・足したユニット。
    pub unit_ids: Vec<i64>,
    /// 付けたタグの数（重複は 1 回）。
    pub tags: u64,
    /// 配置できたファイル（作業フォルダから消してよいもの）。
    pub placed_paths: Vec<PathBuf>,
}

/// 配置の結果。
pub struct Placed {
    hash: [u8; 32],
    size: u64,
    info: Option<MediaInfo>,
    /// 前回このファイルのメディア情報を読んでいて落ちたので読まなかった（アイテムはサムネイルを作れなかったものにする）。
    crashed: bool,
}

/// 配置した結果、または失敗の理由。
pub type PlaceOutcome = std::result::Result<Placed, String>;

/// ファイルを配置し、メディア情報を処理中の印を付けて読む（`media::inflight::ProbeBatch`。OS のデコーダで落ちても、次の起動で
/// 同じファイル（作業フォルダのパスで覚える）で落ち続けない。M11 の検証 S2）。印は配置の前に 1 回書き、配置と読み取りは
/// 続けて並列に。前回まとめて読んでいて落ちたものは、あとで 1 件ずつ印を付けて読む。DB に書くのは印だけ。作業フォルダの
/// ファイルはクローンでコピーするので残る。印は [`write_entries`] が片付ける。
pub fn place_all(
    db: &Db,
    layout: &Layout,
    backend: &dyn MediaBackend,
    job_id: i64,
    files: &[Downloaded],
) -> Result<Vec<PlaceOutcome>> {
    let keys: Vec<String> = files.iter().map(|f| inflight::probe_key(&f.path)).collect();
    let probes = inflight::ProbeBatch::begin(db, job_id, keys.clone())?;
    let mut placed: Vec<PlaceOutcome> = files
        .par_iter()
        .zip(keys.par_iter())
        .enumerate()
        .map(|(i, (file, key))| {
            let staging = layout
                .staging_dir()
                .join(format!("import-dl-{job_id}-{i}.{}", file.ext));
            let staged = stage(&file.path, &staging).map_err(|e| e.to_string())?;
            let final_path = layout.content_path(&staged.hash, &file.ext);
            place(&staged, &final_path).map_err(|e| e.to_string())?;
            Ok(Placed {
                hash: staged.hash,
                size: staged.size,
                info: probes.probe_together(key, backend, &final_path, file.kind),
                crashed: probes.way(key) == ProbeWay::Skip,
            })
        })
        .collect();
    for ((file, key), outcome) in files.iter().zip(&keys).zip(&mut placed) {
        if let Ok(p) = outcome
            && probes.way(key) == ProbeWay::Alone
        {
            p.info = probes.probe_alone(
                db,
                key,
                backend,
                &layout.content_path(&p.hash, &file.ext),
                file.kind,
            )?;
        }
    }
    Ok(placed)
}

/// 配置したファイルを DB に入れる（アイテム・フォルダ・タグ・ユニット・`remote_sources`）。
/// 同じ中身がすでにあれば（アイテムの今・元・過去の版。M12）、そのアイテムに所属とタグを足し、空いている取得元と投稿日だけ
/// 埋める。同じ中身で拡張子だけ違うコピー（台帳の拡張子と違う `files/` のファイル）と、消えた過去の版と同じ中身のコピー
/// （台帳に行がない）は、どの行も使わないのでここで消す。台帳にだけある中身（アンカーなど）は台帳の拡張子で入れる。
/// [`place_all`] がジョブ `job_id` に付けた probe の印を、同じトランザクションで片付ける。
pub fn write_entries(
    tx: &Connection,
    layout: &Layout,
    entries: Vec<(Downloaded, PlaceOutcome)>,
    dest_folder_id: Option<i64>,
    origin: Origin,
    job_id: i64,
) -> Result<Registered> {
    let now = crate::time::now_ms();
    let mut out = Registered::default();
    let folder = match dest_folder_id {
        Some(id) if folder_exists(tx, id)? => Some(id),
        _ => None,
    };
    let mut tags: HashMap<TagName, i64> = HashMap::new();
    let mut units: HashMap<String, i64> = HashMap::new();
    let done: Vec<String> = entries
        .iter()
        .filter(|(_, o)| o.is_ok())
        .map(|(f, _)| inflight::probe_key(&f.path))
        .collect();
    for (file, outcome) in entries {
        let Ok(placed) = outcome else {
            out.failed += 1;
            continue;
        };
        let meta = &file.meta;
        let title = nfc(&meta.title);
        // 今・元・過去の版のどれかと同じ中身なら、そのアイテムの重複（docs/editing.md §14）
        let item_id = match find_owner(tx, &placed.hash)? {
            Some(existing) => {
                out.duplicates += 1;
                remove_unused_copy(tx, layout, &placed.hash, &file.ext, true)?;
                if existing.trashed {
                    restore_item(tx, existing.id, now)?;
                }
                // 取り込んだものを取得し直したときは、空いている取得元と投稿日だけ埋める
                let mut stmt = tx.prepare_cached(
                    "UPDATE items SET source_url = ?2, source_site = ?3,
                                      captured_at = coalesce(captured_at, ?4), updated_at = ?5
                     WHERE id = ?1 AND source_url IS NULL",
                )?;
                stmt.execute(params![
                    existing.id,
                    meta.source_url,
                    meta.site,
                    meta.captured_at,
                    now
                ])?;
                existing.id
            }
            None => {
                // 台帳にだけある中身（アンカーなど）は、台帳の拡張子で入れる（items のトリガが食い違いを断るため）
                let kept = remove_unused_copy(tx, layout, &placed.hash, &file.ext, false)?;
                let ext = kept.as_deref().unwrap_or(&file.ext);
                let id = insert_item(
                    tx,
                    &NewItem {
                        hash: &placed.hash,
                        ext,
                        kind: file.kind,
                        mime: mime_for(ext),
                        size: placed.size,
                        title: &title,
                        original_name: &file.name,
                        original_path: "",
                        info: placed.info.as_ref(),
                        source_site: &meta.site,
                        source_url: meta.source_url.as_deref(),
                        captured_at: meta.captured_at,
                        now,
                    },
                )?;
                out.added += 1;
                id
            }
        };
        out.item_ids.push(item_id);
        if placed.crashed {
            inflight::fail_crashed_thumb(tx, item_id)?;
        }
        if let Some(folder) = folder {
            add_to_folder(tx, folder, item_id)?;
        }
        for tag in &meta.tags {
            let tag_id = match tags.get(tag) {
                Some(id) => *id,
                None => {
                    let id = ensure_tag(tx, tag, now)?;
                    tags.insert(tag.clone(), id);
                    id
                }
            };
            add_tag(tx, tag_id, item_id, origin.tag_source())?;
        }
        let mut unit_id = None;
        if let Some(group) = &meta.group {
            let id = match units.get(&group.key) {
                Some(id) => *id,
                None => {
                    // 前に同じ投稿・再生リストを取ったときのユニットがあれば、そこへ足す
                    let existing = match &meta.remote_id {
                        Some(remote) => unit_of_remote(tx, &meta.site, remote)?,
                        None => None,
                    };
                    let id = match existing {
                        Some(id) if unit_exists(tx, id)? => id,
                        _ => create_unit(
                            tx,
                            &nfc(&group.name),
                            origin.unit_origin(),
                            group.source_url.as_deref(),
                            now,
                        )?,
                    };
                    units.insert(group.key.clone(), id);
                    out.unit_ids.push(id);
                    id
                }
            };
            add_unit_member(tx, id, item_id, group.pos)?;
            unit_id = Some(id);
        }
        if let Some(remote) = &meta.remote_id {
            let mut stmt = tx.prepare_cached(
                "INSERT OR REPLACE INTO remote_sources (site, remote_id, item_id, unit_id, page_index, fetched_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            )?;
            stmt.execute(params![
                meta.site,
                remote,
                item_id,
                unit_id,
                meta.page_index,
                now
            ])?;
        }
        out.placed_paths.push(file.path);
        if let Some(sidecar) = file.sidecar {
            out.placed_paths.push(sidecar);
        }
    }
    out.tags = tags.len() as u64;
    inflight::settle_probes(tx, job_id, &done)?;
    Ok(out)
}

/// 同じ取得元（サイト・ID）のアイテムが入っているユニット。
/// 置いたコピー（`files/<hash>.<ext>`）は、台帳にその中身が同じ拡張子で載っているときだけ使われる。台帳の拡張子と違う
/// コピーと、持ち主がいる（`owned`）のに台帳に行がない（消えた過去の版と同じ中身の）コピーは、どの行も使わないので消す
/// （CLAUDE.md の「files/ を DB を通さずに消さない」に沿って、書き込みのトランザクションの中で台帳を見てから消す）。
/// 台帳の拡張子を返す（載っていなければ None）。
fn remove_unused_copy(
    conn: &Connection,
    layout: &Layout,
    hash: &[u8; 32],
    ext: &str,
    owned: bool,
) -> Result<Option<String>> {
    let kept = crate::library::ledger::ext_of(conn, hash)?;
    let unused = match &kept {
        Some(kept) => kept != ext,
        None => owned,
    };
    if unused {
        let path = layout.content_path(hash, ext);
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => {
                tracing::warn!(%err, path = %path.display(), "どの行も使わないコピーを消せませんでした");
            }
        }
    }
    Ok(kept)
}

fn unit_of_remote(conn: &Connection, site: &str, remote_id: &str) -> Result<Option<i64>> {
    let mut stmt = conn.prepare_cached(
        "SELECT unit_id FROM remote_sources
         WHERE site = ?1 AND remote_id = ?2 AND unit_id IS NOT NULL
         ORDER BY fetched_at DESC LIMIT 1",
    )?;
    Ok(stmt
        .query_row(params![site, remote_id], |row| row.get(0))
        .optional()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_conn;
    use crate::download::meta::Group;
    use crate::media::NoBackend;

    const ALL: TagPolicy = TagPolicy {
        uploader: true,
        site_tags: true,
    };

    fn how(tool: Tool) -> Collect<'static> {
        Collect {
            tool,
            site: "pixiv",
            policy: ALL,
            playlist_unit: true,
            job_url: "https://www.pixiv.net/artworks/1",
        }
    }

    #[test]
    fn collects_media_with_sidecars_in_page_order() {
        let dir = tempfile::tempdir().unwrap();
        let w = |name: &str, body: &str| std::fs::write(dir.path().join(name), body).unwrap();
        for (num, name) in [(1, "1_p1.jpg"), (0, "1_p0.jpg")] {
            w(name, "img");
            w(
                &format!("{name}.json"),
                &format!(
                    r#"{{"category":"pixiv","id":1,"num":{num},"count":2,"title":"作品","user":{{"name":"KR"}},"tags":["猫"]}}"#
                ),
            );
        }
        w("1_p2.jpg.part", "x");
        w("notes.txt", "x");
        w("ugoira.zip", "x");
        let (files, unsupported) = collect(dir.path(), &how(Tool::GalleryDl)).unwrap();
        assert_eq!(unsupported, 2);
        assert_eq!(
            files.iter().map(|f| f.name.as_str()).collect::<Vec<_>>(),
            ["1_p0.jpg", "1_p1.jpg"]
        );
        assert_eq!(files[0].meta.title, "作品（1/2）");
        assert!(files[0].sidecar.is_some());
    }

    /// 種類の判定は取り込みと同じ：フォント・3D は取り、Aseprite でない `.ase` は対象外にする。
    #[test]
    fn collects_fonts_and_models_and_skips_swatch_ase() {
        let dir = tempfile::tempdir().unwrap();
        let w = |name: &str, body: &[u8]| std::fs::write(dir.path().join(name), body).unwrap();
        w("a.woff2", b"wOF2");
        w("b.glb", b"glTF");
        w("c.ase", &[0x80, 0, 0, 0, 0xE0, 0xA5, 1, 0]);
        w("d.ase", b"ASEF\x00\x01\x00\x00");
        let (files, unsupported) = collect(dir.path(), &how(Tool::GalleryDl)).unwrap();
        assert_eq!(unsupported, 1);
        assert_eq!(
            files
                .iter()
                .map(|f| (f.name.as_str(), f.kind))
                .collect::<Vec<_>>(),
            [
                ("a.woff2", MediaKind::Font),
                ("b.glb", MediaKind::Model),
                ("c.ase", MediaKind::Image)
            ]
        );
    }

    #[test]
    fn skips_ytdlp_fragments_and_reads_info_json() {
        let dir = tempfile::tempdir().unwrap();
        let w = |name: &str, body: &str| std::fs::write(dir.path().join(name), body).unwrap();
        w("abc.mp4", "v");
        w(
            "abc.info.json",
            r#"{"id":"abc","title":"動画","uploader":"ch"}"#,
        );
        w("abc.f401.mp4", "v");
        w("abc.f140.m4a", "a");
        w("abc.fhls-720p.mp4", "v");
        w("video.final.mp4", "v");
        w("abc.mp4.ytdl", "x");
        w("other.webm", "v");
        let mut h = how(Tool::YtDlp);
        h.site = "youtube";
        let (files, _) = collect(dir.path(), &h).unwrap();
        assert_eq!(
            files.iter().map(|f| f.name.as_str()).collect::<Vec<_>>(),
            ["abc.mp4", "other.webm", "video.final.mp4"]
        );
        assert_eq!(files[0].meta.title, "動画");
        // メタデータがなければファイル名
        assert_eq!(files[1].meta.title, "other");
        assert_eq!(files[1].sidecar, None);
    }

    fn meta(remote: &str, page: i64, group: bool) -> FileMeta {
        FileMeta {
            title: format!("作品（{page}/2）"),
            source_url: Some(format!("https://www.pixiv.net/artworks/{remote}")),
            site: "pixiv".into(),
            remote_id: Some(remote.into()),
            page_index: Some(page),
            captured_at: Some(1_000),
            tags: vec![
                TagName {
                    namespace: "作者".into(),
                    name: "KR".into(),
                },
                TagName::plain("猫"),
            ],
            group: group.then(|| Group {
                key: format!("pixiv:{remote}"),
                name: "作品".into(),
                source_url: Some(format!("https://www.pixiv.net/artworks/{remote}")),
                pos: page as f64,
            }),
        }
    }

    fn entry(hash: u8, meta: FileMeta) -> (Downloaded, PlaceOutcome) {
        (
            Downloaded {
                path: format!("/w/{hash}.jpg").into(),
                sidecar: Some(format!("/w/{hash}.jpg.json").into()),
                name: format!("{hash}.jpg"),
                ext: "jpg".into(),
                kind: MediaKind::Image,
                meta,
            },
            Ok(Placed {
                hash: [hash; 32],
                size: 3,
                info: None,
                crashed: false,
            }),
        )
    }

    #[test]
    fn writes_items_tags_units_and_remote_sources() {
        let conn = test_conn();
        conn.execute(
            "INSERT INTO folders (id, name, pos, created_at) VALUES (9, '取得', 1, 0)",
            [],
        )
        .unwrap();
        let out = write_entries(
            &conn,
            &Layout::new("/kura-test-no-library"),
            vec![
                entry(1, meta("100", 1, true)),
                entry(2, meta("100", 2, true)),
            ],
            Some(9),
            Origin::Download,
            1,
        )
        .unwrap();
        assert_eq!((out.added, out.duplicates, out.tags), (2, 0, 2));
        assert_eq!(out.unit_ids.len(), 1);
        assert_eq!(out.placed_paths.len(), 4);
        let unit = out.unit_ids[0];
        let (name, origin, url): (String, String, Option<String>) = conn
            .query_row(
                "SELECT name, origin, source_url FROM units WHERE id = ?1",
                [unit],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(
            (name.as_str(), origin.as_str(), url.as_deref()),
            (
                "作品",
                "download",
                Some("https://www.pixiv.net/artworks/100")
            )
        );
        assert_eq!(crate::unit::members(&conn, unit).unwrap(), out.item_ids);
        let (site, src, captured, title): (String, String, i64, String) = conn
            .query_row(
                "SELECT source_site, source_url, captured_at, title FROM items WHERE id = ?1",
                [out.item_ids[0]],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap();
        assert_eq!(
            (site.as_str(), src.as_str(), captured, title.as_str()),
            (
                "pixiv",
                "https://www.pixiv.net/artworks/100",
                1_000,
                "作品（1/2）"
            )
        );
        let tag_sources: Vec<String> = conn
            .prepare("SELECT DISTINCT source FROM item_tags")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        assert_eq!(tag_sources, vec!["download".to_string()]);
        let in_folder: i64 = conn
            .query_row(
                "SELECT count(*) FROM item_folders WHERE folder_id = 9",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(in_folder, 2);
        let remote: Vec<(i64, Option<i64>, Option<i64>)> = conn
            .prepare("SELECT item_id, unit_id, page_index FROM remote_sources ORDER BY page_index")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        assert_eq!(
            remote,
            vec![
                (out.item_ids[0], Some(unit), Some(1)),
                (out.item_ids[1], Some(unit), Some(2))
            ]
        );

        // 同じ作品を取り直すと、既存のアイテムとユニットを使う
        let again = write_entries(
            &conn,
            &Layout::new("/kura-test-no-library"),
            vec![
                entry(1, meta("100", 1, true)),
                entry(3, meta("100", 3, true)),
            ],
            None,
            Origin::Download,
            1,
        )
        .unwrap();
        assert_eq!((again.added, again.duplicates), (1, 1));
        assert_eq!(again.unit_ids, vec![unit]);
        assert_eq!(crate::unit::members(&conn, unit).unwrap().len(), 3);
    }

    #[test]
    fn duplicates_of_local_files_get_the_source() {
        let conn = test_conn();
        let id = insert_item(
            &conn,
            &NewItem {
                hash: &[5; 32],
                ext: "jpg",
                kind: MediaKind::Image,
                mime: None,
                size: 3,
                title: "手元",
                original_name: "a.jpg",
                original_path: "/a.jpg",
                info: None,
                source_site: "local",
                source_url: None,
                captured_at: None,
                now: 1,
            },
        )
        .unwrap();
        conn.execute("UPDATE items SET trashed_at = 2 WHERE id = ?1", [id])
            .unwrap();
        let out = write_entries(
            &conn,
            &Layout::new("/kura-test-no-library"),
            vec![entry(5, meta("7", 1, false))],
            None,
            Origin::Download,
            1,
        )
        .unwrap();
        assert_eq!(
            (out.added, out.duplicates, out.item_ids.clone()),
            (0, 1, vec![id])
        );
        let (site, title, trashed): (String, String, Option<i64>) = conn
            .query_row(
                "SELECT source_site, title, trashed_at FROM items WHERE id = ?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        // 題名は変えない
        assert_eq!(
            (site.as_str(), title.as_str(), trashed),
            ("pixiv", "手元", None)
        );
    }

    #[test]
    fn places_files_and_counts_failures() {
        let lib_dir = tempfile::tempdir().unwrap();
        let lib = crate::library::Library::create(&lib_dir.path().join("L")).unwrap();
        let layout = lib.layout().clone();
        let work = tempfile::tempdir().unwrap();
        let good = work.path().join("a.jpg");
        std::fs::write(&good, "image").unwrap();
        let mut ok = entry(1, meta("1", 1, false)).0;
        ok.path = good.clone();
        let mut missing = entry(2, meta("2", 1, false)).0;
        missing.path = work.path().join("missing.jpg");
        let placed = place_all(lib.db(), &layout, &NoBackend, 7, &[ok, missing]).unwrap();
        assert!(placed[0].is_ok() && placed[1].is_err());
        let hash = placed[0].as_ref().unwrap().hash;
        assert!(layout.content_path(&hash, "jpg").is_file());
        // 元のファイル（作業フォルダ）はクローンでコピーしただけなので残る
        assert!(good.is_file());
    }

    /// 検証 S2：ダウンロード・キャプチャでも、メディア情報は処理中の印を付けて読む。前回 1 件だけで読んでいて落ちたファイルは
    /// 読まずに入れ、サムネイルを作れなかったものにする。入れたら印と覚えを片付ける。
    #[test]
    fn place_all_marks_probes_and_skips_files_that_crashed() {
        let lib_dir = tempfile::tempdir().unwrap();
        let lib = crate::library::Library::create(&lib_dir.path().join("L")).unwrap();
        let layout = lib.layout().clone();
        let work = tempfile::tempdir().unwrap();
        let (a_path, b_path) = (work.path().join("a.jpg"), work.path().join("b.jpg"));
        std::fs::write(&a_path, "crashed-imageio").unwrap();
        std::fs::write(&b_path, "fine").unwrap();
        let crashed = serde_json::json!([inflight::probe_key(&a_path)]);
        lib.db()
            .write(move |tx| crate::settings::set(tx, "probe.crashed", &crashed))
            .unwrap();
        let mut fa = entry(1, meta("1", 1, false)).0;
        fa.path = a_path;
        let mut fb = entry(2, meta("2", 1, false)).0;
        fb.path = b_path;
        let files = vec![fa, fb];
        let placed = place_all(
            lib.db(),
            &layout,
            &crate::media::fake::FakeBackend,
            7,
            &files,
        )
        .unwrap();
        let (pa, pb) = (placed[0].as_ref().unwrap(), placed[1].as_ref().unwrap());
        assert!(pa.crashed && pa.info.is_none(), "落ちたファイルは読まない");
        assert!(!pb.crashed && pb.info.is_some());
        let mark: serde_json::Value = lib
            .db()
            .read(|c| crate::settings::get(c, "probe.inflight"))
            .unwrap()
            .unwrap();
        assert_eq!(mark[0]["job"], serde_json::json!(7));
        assert_eq!(mark[0]["solo"], serde_json::json!(false));
        let written = layout.clone();
        let entries: Vec<_> = files.into_iter().zip(placed).collect();
        let out = lib
            .db()
            .write(move |tx| write_entries(tx, &written, entries, None, Origin::Download, 7))
            .unwrap();
        assert_eq!(out.added, 2);
        let states: Vec<String> = lib
            .db()
            .read(|c| {
                let mut stmt = c.prepare("SELECT thumb_state FROM items ORDER BY id")?;
                let rows = stmt.query_map([], |r| r.get(0))?;
                Ok(rows.collect::<std::result::Result<_, _>>()?)
            })
            .unwrap();
        assert_eq!(states, vec!["failed".to_string(), "pending".to_string()]);
        for key in ["probe.inflight", "probe.crashed"] {
            let left: Option<serde_json::Value> =
                lib.db().read(|c| crate::settings::get(c, key)).unwrap();
            assert_eq!(left, None, "{key}");
        }
    }

    /// S14（docs/editing.md §14.1）：取得したファイルが、あるアイテムの元・消えた過去の版と同じ中身なら、そのアイテムの重複に
    /// なり、孤立したファイルが残らない（消えた版と同じ中身のコピーは台帳に行がないので、書き込みのトランザクションの中で消す）。
    /// 版の行は、保存のジョブ（段 2 の B）の代わりにテストの補助で入れる。
    #[test]
    fn download_duplicate_of_original_or_released_version_is_owned() {
        use crate::edit::testing::insert_version_with;
        let dir = tempfile::tempdir().unwrap();
        let lib = crate::library::Library::create(&dir.path().join("L")).unwrap();
        let layout = lib.layout().clone();
        let put = |hash: [u8; 32], ext: &str| {
            let path = layout.content_path(&hash, ext);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, "abc").unwrap();
        };
        // アイテム（今 [5]）：元 [6]・版 1 [7]（GC で消えた）・版 2 が今
        let id = lib
            .db()
            .write(|tx| {
                let id = insert_item(
                    tx,
                    &NewItem {
                        hash: &[5; 32],
                        ext: "jpg",
                        kind: MediaKind::Image,
                        mime: None,
                        size: 3,
                        title: "手元",
                        original_name: "a.jpg",
                        original_path: "/a.jpg",
                        info: None,
                        source_site: "local",
                        source_url: None,
                        captured_at: None,
                        now: 1,
                    },
                )?;
                insert_version_with(tx, id, 0, [6; 32], "jpg", 3, "hold", None);
                insert_version_with(tx, id, 1, [7; 32], "jpg", 3, "released", None);
                insert_version_with(tx, id, 2, [5; 32], "jpg", 3, "hold", None);
                Ok(id)
            })
            .unwrap();
        put([5; 32], "jpg");
        put([6; 32], "jpg");
        // 取得したファイル（どちらも jpg）を place_all が置いたところ（元と同じ中身は、もうあるので置き直さない）
        put([7; 32], "jpg");
        let written = layout.clone();
        let out = lib
            .db()
            .write(move |tx| {
                write_entries(
                    tx,
                    &written,
                    vec![entry(6, meta("7", 1, false)), entry(7, meta("8", 1, false))],
                    None,
                    Origin::Download,
                    1,
                )
            })
            .unwrap();
        assert_eq!(
            (out.added, out.duplicates, out.item_ids.clone()),
            (0, 2, vec![id, id])
        );
        assert!(layout.content_path(&[6; 32], "jpg").exists());
        assert!(
            !layout.content_path(&[7; 32], "jpg").exists(),
            "消えた版と同じ中身のコピーが残った"
        );
        let items: i64 = lib
            .db()
            .read(|c| Ok(c.query_row("SELECT count(*) FROM items", [], |r| r.get(0))?))
            .unwrap();
        assert_eq!(items, 1);
        assert_eq!(
            crate::library::check_files(lib.db(), lib.layout()).unwrap(),
            (0, 0)
        );
    }

    #[test]
    fn download_duplicate_with_other_ext_leaves_no_orphan() {
        // 同じ中身を別の拡張子で取ると、place_all が files/ に置いたコピーをどの行も使わない。
        // 台帳の拡張子と違うファイルだけを、書き込みのトランザクションの中で消す（同じ拡張子なら消さない）
        let dir = tempfile::tempdir().unwrap();
        let lib = crate::library::Library::create(&dir.path().join("L")).unwrap();
        let layout = lib.layout().clone();
        let put = |hash: [u8; 32], ext: &str| {
            let path = layout.content_path(&hash, ext);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, "abc").unwrap();
        };
        for (hash, ext) in [([5u8; 32], "jpeg"), ([6u8; 32], "jpg")] {
            lib.db()
                .write(move |tx| {
                    insert_item(
                        tx,
                        &NewItem {
                            hash: &hash,
                            ext,
                            kind: MediaKind::Image,
                            mime: None,
                            size: 3,
                            title: "手元",
                            original_name: "a",
                            original_path: "/a",
                            info: None,
                            source_site: "local",
                            source_url: None,
                            captured_at: None,
                            now: 1,
                        },
                    )?;
                    Ok(())
                })
                .unwrap();
            put(hash, ext);
        }
        // 取得したファイル（どちらも jpg）を place_all が置いたところ
        put([5u8; 32], "jpg");
        let written = layout.clone();
        let out = lib
            .db()
            .write(move |tx| {
                write_entries(
                    tx,
                    &written,
                    vec![entry(5, meta("7", 1, false)), entry(6, meta("8", 1, false))],
                    None,
                    Origin::Download,
                    1,
                )
            })
            .unwrap();
        assert_eq!((out.added, out.duplicates), (0, 2));
        assert!(
            !layout.content_path(&[5u8; 32], "jpg").exists(),
            "拡張子だけ違うコピーが残った"
        );
        assert!(layout.content_path(&[5u8; 32], "jpeg").exists());
        assert!(
            layout.content_path(&[6u8; 32], "jpg").exists(),
            "同じ拡張子の重複は消さない"
        );
        assert_eq!(
            crate::library::check_files(lib.db(), lib.layout()).unwrap(),
            (0, 0)
        );
    }
}
