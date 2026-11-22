//! テストの補助：版・アンカーの行を入れる（重複の判定・ゴミ箱を空にする処理・下書き・不変条件の試験に使う）。版の行の不変条件
//! （版 0 は original、頭は hold、hold・waiting・kept は出力の台帳の行を指す、描いた版は入力 0 を持つ）に沿って入れる。

use kura_engine::Graph;
use kura_engine::doc_parts::split;
use rusqlite::{Connection, params};

use crate::library::ledger;

/// 画像のアイテム（png、1 バイト）を入れる。
pub(crate) fn insert_item(conn: &Connection, id: i64, hash: [u8; 32]) -> i64 {
    conn.execute(
        "INSERT INTO items (id, hash, ext, kind, size_bytes, title, original_name, imported_at, updated_at)
         VALUES (?1, ?2, 'png', 'image', 1, 't', 't.png', 1, 1)",
        params![id, &hash[..]],
    )
    .unwrap();
    id
}

/// アンカーの行（元 `source`、ファイル `file`（wav・1 バイト）を台帳に載せる）を入れる。読み方の設定のダイジェストは
/// ファイルの hash にする（同じ元に別の読み方のアンカーを複数入れられるように）。
pub(crate) fn insert_anchor(conn: &Connection, source: [u8; 32], file: [u8; 32]) -> i64 {
    let blob = ledger::register(conn, &file, "wav", 1, 1).unwrap();
    conn.query_row(
        "INSERT INTO edit_anchors (source_hash, options_digest, options, decoded, value_digest, blob_id, created_at)
         VALUES (?1, ?3, '{}', '{}', x'00', ?2, 1) RETURNING id",
        params![&source[..], blob, &file[..]],
        |r| r.get(0),
    )
    .unwrap()
}

/// 版の行（出力は png・1 バイト）を入れる。[`insert_version_with`] の短い形。
pub(crate) fn insert_version(
    conn: &Connection,
    item_id: i64,
    version: i64,
    output: [u8; 32],
    file_state: &str,
    anchor: Option<i64>,
) -> i64 {
    insert_version_with(conn, item_id, version, output, "png", 1, file_state, anchor)
}

/// 版の行を入れる。版 0 は original、ほかは edit（空の文書の部品を置いて指し、入力 0 の行を持つ）。released でなければ出力の
/// 台帳の行を指し（なければ `ledger::register` で載せる）、hold でなければ置き換えの時刻を持つ。`anchor` があれば入力 0 に
/// アンカーを付ける。
#[allow(clippy::too_many_arguments)]
pub(crate) fn insert_version_with(
    conn: &Connection,
    item_id: i64,
    version: i64,
    output: [u8; 32],
    ext: &str,
    size: u64,
    file_state: &str,
    anchor: Option<i64>,
) -> i64 {
    let (kind, doc, base) = if version == 0 {
        ("original", None, None)
    } else {
        let doc =
            crate::edit::store::put_doc(conn, &split(&Graph::new_single()).unwrap(), 1).unwrap();
        ("edit", Some(doc), Some(version - 1))
    };
    let file =
        (file_state != "released").then(|| ledger::register(conn, &output, ext, size, 1).unwrap());
    let replaced = (file_state != "hold").then_some(5);
    let id: i64 = conn
        .query_row(
            r#"INSERT INTO item_versions (item_id, version, kind, doc_id, base_version, output_hash, ext, size_bytes,
                                        record, file_blob_id, file_state, replaced_at, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, '{"v":1,"engine":"test"}', ?9, ?10, ?11, 1) RETURNING id"#,
            params![
                item_id,
                version,
                kind,
                doc,
                base,
                &output[..],
                ext,
                i64::try_from(size).unwrap(),
                file,
                file_state,
                replaced
            ],
            |r| r.get(0),
        )
        .unwrap();
    if version != 0 {
        // 描いた版は入力 0（元）を持つ（不変条件）。元はアンカーの元か、なければ版 0 の出力
        conn.execute(
            "INSERT INTO item_version_inputs (version_id, input, role, source_hash, value_digest, recorded, anchor_id)
             VALUES (?1, 0, 'primary',
                     coalesce((SELECT source_hash FROM edit_anchors WHERE id = ?2),
                              (SELECT output_hash FROM item_versions WHERE item_id = ?3 AND version = 0),
                              zeroblob(32)),
                     zeroblob(32), '{}', ?2)",
            params![id, anchor, item_id],
        )
        .unwrap();
    }
    id
}
