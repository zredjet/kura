-- 0011（Plain）：保存と版（M12 の段 2。docs/data-model.md §3・§4、docs/editing.md §4.4・§9.4・§14、ADR-169）。
-- 文書を内容アドレスの部品で持つ edit_objects・edit_edges、版 item_versions（版 0＝元は最初の保存のときに足す。既存の
-- アイテムには行を足さないので、10 万件でも表を作るだけ）、版の入力 item_version_inputs、アンカー edit_anchors、
-- 来歴 item_derivations、下書き edit_drafts、台帳の予告 ledger_pending を足し、jobs_kind に edit・gc を足す。
-- 台帳 blobs を参照する列（item_versions.file_blob_id・edit_anchors.blob_id）には ON DELETE を付けない（参照されている
-- 行は消せない。library::ledger::REFERRERS に足す）。items を参照する列は ON DELETE CASCADE（来歴の元だけ SET NULL）。
-- 外部キーの子の列には部分でない索引を付ける。新しい表だけなので Plain。適用後は編集しない。

CREATE TABLE edit_objects (
  id          INTEGER PRIMARY KEY,
  digest      BLOB    NOT NULL UNIQUE,               -- 部品のダイジェスト（ドメイン付き BLAKE3。本文から求め直して照らす）
  kind        TEXT    NOT NULL CONSTRAINT edit_objects_kind CHECK (kind IN ('doc','node','chunk')),
  body        BLOB    NOT NULL,                      -- 正規の JSON（UTF-8）。子は 16 進のダイジェストで書く
  created_at  INTEGER NOT NULL
);
CREATE TABLE edit_edges (
  parent_id   INTEGER NOT NULL REFERENCES edit_objects(id) ON DELETE CASCADE,
  ord         INTEGER NOT NULL CONSTRAINT edit_edges_ord CHECK (ord >= 0),
  child_id    INTEGER NOT NULL REFERENCES edit_objects(id),
  PRIMARY KEY (parent_id, ord)
) WITHOUT ROWID;
CREATE INDEX idx_edit_edges_child ON edit_edges(child_id);

CREATE TABLE edit_anchors (
  id              INTEGER PRIMARY KEY,
  source_hash     BLOB    NOT NULL,                  -- 元のファイルの中身
  options_digest  BLOB    NOT NULL,                  -- 読み方の設定（DecodeOptions の正規の JSON）のダイジェスト
  options         TEXT    NOT NULL,
  decoded         TEXT    NOT NULL,                  -- 元を読んだ記録（DecodeStamp の正規の JSON）
  value_digest    BLOB    NOT NULL,
  blob_id         INTEGER NOT NULL REFERENCES blobs(id),
  created_at      INTEGER NOT NULL,
  UNIQUE (source_hash, options_digest)
);
CREATE INDEX idx_edit_anchors_blob ON edit_anchors(blob_id);

CREATE TABLE item_versions (
  id            INTEGER PRIMARY KEY,
  item_id       INTEGER NOT NULL REFERENCES items(id) ON DELETE CASCADE,
  version       INTEGER NOT NULL CONSTRAINT item_versions_version CHECK (version >= 0),
  kind          TEXT    NOT NULL CONSTRAINT item_versions_kind CHECK (kind IN ('original','edit','revert')),
  doc_id        INTEGER REFERENCES edit_objects(id),  -- 版 0 と、版 0 に戻した版は NULL（空の文書）
  base_version  INTEGER,
  revert_to     INTEGER,
  output_hash   BLOB    NOT NULL,
  ext           TEXT    NOT NULL,
  size_bytes    INTEGER NOT NULL CONSTRAINT item_versions_size CHECK (size_bytes >= 0),
  format        TEXT,
  value_digest  BLOB,
  width         INTEGER,
  height        INTEGER,
  duration_ms   INTEGER,
  record        TEXT    NOT NULL,                    -- VersionRecord（JSON。§2.6）
  file_blob_id  INTEGER REFERENCES blobs(id),
  file_state    TEXT    NOT NULL CONSTRAINT item_versions_file_state
                CHECK (file_state IN ('hold','waiting','kept','released')),
  replaced_at   INTEGER,
  job_id        INTEGER,
  created_at    INTEGER NOT NULL,
  UNIQUE (item_id, version),
  CONSTRAINT item_versions_original CHECK ((version = 0) = (kind = 'original')),
  CONSTRAINT item_versions_lineage CHECK (
       (kind = 'original' AND doc_id IS NULL AND base_version IS NULL AND revert_to IS NULL)
    OR (kind = 'edit'     AND doc_id IS NOT NULL AND base_version IS NOT NULL AND revert_to IS NULL)
    OR (kind = 'revert'   AND base_version IS NOT NULL AND revert_to IS NOT NULL AND revert_to < version)),
  CONSTRAINT item_versions_file     CHECK ((file_state = 'released') = (file_blob_id IS NULL)),
  CONSTRAINT item_versions_replaced CHECK ((file_state = 'hold') = (replaced_at IS NULL))
);
CREATE INDEX idx_item_versions_output    ON item_versions(output_hash);
CREATE INDEX idx_item_versions_file_blob ON item_versions(file_blob_id);
CREATE INDEX idx_item_versions_doc       ON item_versions(doc_id);
CREATE INDEX idx_item_versions_waiting   ON item_versions(replaced_at) WHERE file_state = 'waiting';
CREATE UNIQUE INDEX idx_item_versions_job ON item_versions(job_id) WHERE job_id IS NOT NULL;

CREATE TABLE item_version_inputs (
  version_id    INTEGER NOT NULL REFERENCES item_versions(id) ON DELETE CASCADE,
  input         INTEGER NOT NULL CONSTRAINT item_version_inputs_input CHECK (input >= 0),
  role          TEXT    NOT NULL CONSTRAINT item_version_inputs_role CHECK (role IN ('primary','fixed','selection')),
  source_hash   BLOB    NOT NULL,
  value_digest  BLOB    NOT NULL,
  recorded      TEXT    NOT NULL,                    -- RecordedInput の正規の JSON
  anchor_id     INTEGER REFERENCES edit_anchors(id),
  PRIMARY KEY (version_id, input)
) WITHOUT ROWID;
CREATE INDEX idx_item_version_inputs_anchor ON item_version_inputs(anchor_id);

CREATE TABLE item_derivations (
  item_id         INTEGER PRIMARY KEY REFERENCES items(id) ON DELETE CASCADE,
  source_item_id  INTEGER REFERENCES items(id) ON DELETE SET NULL,
  source_version  INTEGER NOT NULL,
  source_hash     BLOB    NOT NULL,
  created_at      INTEGER NOT NULL
);
CREATE INDEX idx_item_derivations_source ON item_derivations(source_item_id);

CREATE TABLE edit_drafts (
  item_id       INTEGER PRIMARY KEY REFERENCES items(id) ON DELETE CASCADE,
  doc_id        INTEGER NOT NULL REFERENCES edit_objects(id),
  base_hash     BLOB    NOT NULL,
  base_version  INTEGER NOT NULL,
  after_job     INTEGER,
  updated_at    INTEGER NOT NULL
);
CREATE INDEX idx_edit_drafts_doc ON edit_drafts(doc_id);

CREATE TABLE ledger_pending (
  id          INTEGER PRIMARY KEY,
  hash        BLOB    NOT NULL,
  ext         TEXT    NOT NULL,
  kind        TEXT    NOT NULL CONSTRAINT ledger_pending_kind CHECK (kind IN ('place','delete')),
  job_id      INTEGER,
  created_at  INTEGER NOT NULL
);

ALTER TABLE jobs DROP CONSTRAINT jobs_kind;
ALTER TABLE jobs ADD CONSTRAINT jobs_kind
  CHECK (kind IN ('import','thumbnail','proxy','download','capture','export','analyze','edit','gc'));
