//! 書き出し（FR-SYS-02。ADR-120）：選んだアイテムを元のファイル名でフォルダへコピーし、
//! タグなどを `kura-export.json` に添える。
//!
//! - 読むのはライブラリの `files/` だけ。同じボリュームならクローンでコピーする。
//! - 書き出し先の既存のファイル・フォルダは上書きしない（同じ名前があれば「名前 2.jpg」）。ただし Kura が同じアイテムから
//!   書いた記録（`export_files`）どおりのまま（大きさ・更新時刻・中身が記録と同じ）のファイルは置き換える（M15 段 3）。
//! - 名前の規則・形式の変換（エンジンで描く）・JSON の有無・プロジェクトの中への書き出しは `options`（M15 段 3。ADR-205）。
//! - ユニットを選んだら、ユニット名のフォルダの中に並び順で書き出す。

pub mod dragout;
pub mod formats;
mod job;
pub mod names;

pub use dragout::{DragEntry, DragOutcome, DragOuts, DragSession};
pub use formats::{ExportOptions, ExportPreset, FormatTarget, Plan};
pub use job::{ExportHandler, ExportInput, ExportResult, ExportedFile, start_export};

#[cfg(test)]
mod job_tests;

use rusqlite::{Connection, OptionalExtension};

/// 書き出す名前の見本の 1 件（書き出しのシート。M15 段 3）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreviewName {
    pub item_id: i64,
    /// 書き出す名前（同じ名前があれば番号が付くことは見ない）。
    pub name: String,
    /// 形式を変えるならその形式。
    pub convert: Option<String>,
}

/// 選んだアイテム（ばらとして並べた順）の書き出す名前と形式の見本。ないアイテムは飛ばす。
pub fn preview_names(
    conn: &Connection,
    items: &[i64],
    options: &ExportOptions,
    writable: Option<formats::Writable<'_>>,
) -> crate::Result<Vec<PreviewName>> {
    options.check()?;
    let mut stmt = conn.prepare_cached(
        "SELECT ext, kind, original_name, title, width, height, json_extract(codec_info, '$.codec')
           FROM items WHERE id = ?1",
    )?;
    let mut out = Vec::new();
    for (k, &id) in items.iter().enumerate() {
        type Row = (
            String,
            String,
            String,
            String,
            Option<i64>,
            Option<i64>,
            Option<String>,
        );
        let row: Option<Row> = stmt
            .query_row([id], |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                ))
            })
            .optional()?;
        let Some((ext, kind, original_name, title, width, height, codec)) = row else {
            continue;
        };
        let Some(kind) = crate::import::MediaKind::parse(&kind) else {
            continue;
        };
        let parts = names::NameParts {
            item_id: id,
            title: &title,
            original_name: &original_name,
            index: k + 1,
            unit: None,
            width,
            height,
        };
        let (out_ext, convert) =
            match formats::plan(kind, &ext, codec.as_deref(), options.target(kind), writable) {
                Plan::Copy => (ext.clone(), None),
                Plan::Convert { format } => {
                    let e = crate::edit::save::output_ext(&format, &ext, None)
                        .unwrap_or_else(|| format.clone());
                    (e, Some(format))
                }
            };
        out.push(PreviewName {
            item_id: id,
            name: names::rule_name(&options.name, &parts, &out_ext),
            convert,
        });
    }
    Ok(out)
}

#[cfg(test)]
mod preview_tests {
    use super::*;
    use crate::db::test_conn;

    #[test]
    fn preview_shows_names_and_conversions() {
        let conn = test_conn();
        conn.execute_batch(
            "INSERT INTO items (id, hash, ext, kind, size_bytes, title, original_name, imported_at, updated_at)
               VALUES (1, x'01', 'psd', 'image', 1, 'Hero Idle', 'Hero Idle.psd', 0, 0),
                      (2, x'02', 'wav', 'audio', 1, 'Jump', 'jump.wav', 0, 0);",
        )
        .unwrap();
        let options = ExportOptions {
            name: names::NameRule {
                template: None,
                style: names::NameStyle::Snake,
                ascii: false,
            },
            image: FormatTarget::Playable,
            audio: FormatTarget::To {
                format: "ogg".into(),
            },
            ..ExportOptions::default()
        };
        let writable = |f: &str| matches!(f, "png" | "ogg" | "wav");
        let got = preview_names(&conn, &[1, 2, 99], &options, Some(&writable)).unwrap();
        assert_eq!(
            got,
            vec![
                PreviewName {
                    item_id: 1,
                    name: "hero_idle.png".into(),
                    convert: Some("png".into()),
                },
                PreviewName {
                    item_id: 2,
                    name: "jump.ogg".into(),
                    convert: Some("ogg".into()),
                },
            ]
        );
    }
}
