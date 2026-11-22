-- 0015（Plain）：プロジェクトと使った記録（M15 段 2。docs/data-model.md §3・§4「プロジェクトと使った記録（M15）」、ADR-202・204）。
-- 登録したプロジェクト（Godot・Unity・ほかのフォルダ）と、照らし直しのジョブ（kind = 'project'）が見たファイル（取り込める形式だけ。
-- 大きさと更新時刻でハッシュを使い直す）、それと中身が一致したアイテム（今・元・過去の版・持ち出した出力）を足す。使った記録は
-- 「今プロジェクトにあるもの」で、照らし直しで見えなくなったファイルの行は消す。プロジェクトのフォルダが見つからなければ
-- missing = 1 にして行は残す（利用者の決定 4）。持ち出した出力（段 3 の書き出し・段 5 のドラッグアウトで形式を変えて作った
-- ファイルの中身）は item_outputs に置き、照らし直しが引く。jobs_kind に project を足す（名前付きの CHECK の入れ替え）。
-- 新しい表と CHECK の入れ替えだけなので Plain。適用後は編集しない。

CREATE TABLE projects (
  id          INTEGER PRIMARY KEY,
  name        TEXT    NOT NULL,                        -- 表示名（既定はフォルダの名前）
  root        TEXT    NOT NULL,                        -- 正規化した絶対パス（一意。ほかのプロジェクトと重ならない）
  engine      TEXT    NOT NULL CONSTRAINT projects_engine CHECK (engine IN ('godot','unity','folder')),
  settings    TEXT    NOT NULL DEFAULT '{}',           -- 持ち出しの既定（命名・形式。段 3 で形を決める JSON）
  pos         REAL    NOT NULL,                        -- 並び
  missing     INTEGER NOT NULL DEFAULT 0 CONSTRAINT projects_missing CHECK (missing IN (0, 1)),  -- 最後の照らしでフォルダがなかった
  scanned_at  INTEGER,                                 -- 最後に照らし終えた時刻
  created_at  INTEGER NOT NULL,
  updated_at  INTEGER NOT NULL
);
CREATE UNIQUE INDEX idx_projects_root ON projects(root);

CREATE TABLE project_files (
  project_id  INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
  path        TEXT    NOT NULL,                        -- ルートからの相対パス（/ 区切り）
  size_bytes  INTEGER NOT NULL CONSTRAINT project_files_size CHECK (size_bytes >= 0),
  mtime_ns    INTEGER NOT NULL,                        -- 更新時刻（Unix ナノ秒。大きさと合わせてハッシュを使い直す）
  hash        BLOB    NOT NULL,                        -- BLAKE3 32 バイト
  item_id     INTEGER REFERENCES items(id) ON DELETE SET NULL,  -- 一致したアイテム（なければ NULL。照らし直しで付け直す）
  matched     TEXT    CONSTRAINT project_files_matched CHECK (matched IN ('current','original','version','output')),
                                                       -- 何と一致したか（今・元・過去の版・持ち出した出力）。item_id と同じときに書く
  seen_at     INTEGER NOT NULL,                        -- 最後に見た照らしの時刻
  PRIMARY KEY (project_id, path)
) WITHOUT ROWID;
CREATE INDEX idx_project_files_item ON project_files(item_id);

CREATE TABLE item_outputs (
  hash         BLOB    NOT NULL,                       -- 持ち出したファイルの中身
  item_id      INTEGER NOT NULL REFERENCES items(id) ON DELETE CASCADE,
  source_hash  BLOB    NOT NULL,                       -- 描いたときのアイテムの今
  ext          TEXT    NOT NULL,
  size_bytes   INTEGER NOT NULL CONSTRAINT item_outputs_size CHECK (size_bytes >= 0),
  created_at   INTEGER NOT NULL,
  PRIMARY KEY (hash, item_id)
) WITHOUT ROWID;
CREATE INDEX idx_item_outputs_item ON item_outputs(item_id);

ALTER TABLE jobs DROP CONSTRAINT jobs_kind;
ALTER TABLE jobs ADD CONSTRAINT jobs_kind
  CHECK (kind IN ('import','thumbnail','proxy','download','capture','export','analyze','edit','gc','batch','project'));
