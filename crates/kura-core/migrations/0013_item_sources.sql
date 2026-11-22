-- 0013（Plain）：N → 1 の来歴（M14 段 6。docs/data-model.md §3・§4「一括加工（M14）」、docs/editing.md §4.5、ADR-197）。
-- 選んだ N 件から 1 枚のスプライトシート（新しいアイテム。元はできたファイル）を作ったときの、元のアイテム・読んだときの頭の版・
-- 読んだ中身・並べたフレームの数を、選んだ順に持つ（別名で保存の来歴 item_derivations は元 1 つなので別の表にする）。
-- 作ったアイテムが消えたら行も消え（CASCADE）、元のアイテムが消えたら NULL（SET NULL。版と中身は残す。item_derivations と同じ）。
-- 外部キーの子の列には索引を付ける（item_id は主キーの頭）。新しい表だけなので Plain。適用後は編集しない。

CREATE TABLE item_sources (
  item_id         INTEGER NOT NULL REFERENCES items(id) ON DELETE CASCADE,     -- 作ったアイテム
  ord             INTEGER NOT NULL CONSTRAINT item_sources_ord CHECK (ord >= 0),  -- 選んだ順（シートの並び）
  source_item_id  INTEGER REFERENCES items(id) ON DELETE SET NULL,
  source_version  INTEGER NOT NULL,                    -- 読んだときの頭の版
  source_hash     BLOB    NOT NULL,                    -- 読んだ中身（今）
  frames          INTEGER NOT NULL CONSTRAINT item_sources_frames CHECK (frames >= 1),  -- この元から並べたフレームの数
  created_at      INTEGER NOT NULL,
  PRIMARY KEY (item_id, ord)
) WITHOUT ROWID;
CREATE INDEX idx_item_sources_source ON item_sources(source_item_id);
