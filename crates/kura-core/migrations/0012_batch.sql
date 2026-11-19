-- 0012（Plain）：一括加工の表と口（M14 段 3。docs/data-model.md §3・§4「一括加工（M14）」、docs/editing.md §4.3、ADR-190・192）。
-- レシピ edit_recipes（文書は edit_objects の部品、結び付けは recipe::bindings_json の正規の JSON）と、一括の対象と結果
-- batch_items（1 件ずつのコミットと同じトランザクションで行を書いて「ちょうど 1 回」を守る。一括で足した版は
-- item_versions.job_id を空にし、この行の version_id で引く。ADR-192 (1)）を足し、jobs_kind に batch を足す。
-- batch_items.job_id は外部キーにしない（item_versions.job_id・ledger_pending.job_id と同じ。jobs を参照される表にしない）。
-- 対象のアイテムが消えたら行も消える（ON DELETE CASCADE。ADR-192 (4)）。版・別名で作ったアイテムが消えたら NULL。
-- 外部キーの子の列には部分でない索引を付ける。新しい表だけなので Plain。適用後は編集しない。

CREATE TABLE edit_recipes (
  id           INTEGER PRIMARY KEY,
  name         TEXT    NOT NULL,                     -- 1〜256 バイト・制御文字なし（エンジンの recipe::Recipe が確かめる）。重なってよい
  description  TEXT    NOT NULL DEFAULT '',          -- 4,096 バイトまで（改行・タブだけ可）
  kind         TEXT    NOT NULL CONSTRAINT edit_recipes_kind CHECK (kind IN ('each','merge')),
  doc_id       INTEGER NOT NULL REFERENCES edit_objects(id),
  bindings     TEXT    NOT NULL DEFAULT '[]',        -- 結び付け（recipe::bindings_json の正規の JSON）
  pos          REAL    NOT NULL,                     -- サイドバーの並び（プレイリストと同じ）
  created_at   INTEGER NOT NULL,
  updated_at   INTEGER NOT NULL
);
CREATE INDEX idx_edit_recipes_doc ON edit_recipes(doc_id);

CREATE TABLE batch_items (
  job_id        INTEGER NOT NULL,
  ord           INTEGER NOT NULL CONSTRAINT batch_items_ord CHECK (ord >= 0),
  item_id       INTEGER NOT NULL REFERENCES items(id) ON DELETE CASCADE,
  state         TEXT    NOT NULL DEFAULT 'pending' CONSTRAINT batch_items_state
                CHECK (state IN ('pending','saved','unchanged','existing','skipped','failed','canceled')),
  decoding      INTEGER NOT NULL DEFAULT 0 CONSTRAINT batch_items_decoding CHECK (decoding IN (0, 1)),
  base_hash     BLOB,                                -- 処理したときの頭（継ぎ足した土台）
  base_version  INTEGER,
  version_id    INTEGER REFERENCES item_versions(id) ON DELETE SET NULL,
  out_item_id   INTEGER REFERENCES items(id) ON DELETE SET NULL,  -- 別名で作ったアイテム
  code          TEXT,                                -- 飛ばした・失敗した理由（AppError の code）
  message       TEXT,
  finished_at   INTEGER,
  PRIMARY KEY (job_id, ord),
  UNIQUE (job_id, item_id),
  CONSTRAINT batch_items_finished CHECK ((state = 'pending') = (finished_at IS NULL))
) WITHOUT ROWID;
CREATE INDEX idx_batch_items_item     ON batch_items(item_id);
CREATE INDEX idx_batch_items_version  ON batch_items(version_id);
CREATE INDEX idx_batch_items_out_item ON batch_items(out_item_id);

ALTER TABLE jobs DROP CONSTRAINT jobs_kind;
ALTER TABLE jobs ADD CONSTRAINT jobs_kind
  CHECK (kind IN ('import','thumbnail','proxy','download','capture','export','analyze','edit','gc','batch'));
