-- 0016（Plain）：Kura が書き出したファイルの記録（M15 段 3 の検証の直し。docs/data-model.md §3・§4「書き出したファイル（M15）」、
-- ADR-205 (6)）。書き出し先に同じ名前のファイルがあるとき、置き換えてよいのは、この記録どおりのまま（同じアイテムとして書き、
-- そのあと大きさ・更新時刻・中身が変わっていない）ファイルだけにする（利用者の決定。取り込み元のファイル・利用者が直したファイル・
-- ほかのアイテムとして書いたファイルを壊さない）。ユニットのフォルダも、中に同じユニットのアイテムの記録があれば使い直す。
-- アイテムを消すと記録も消える（CASCADE。そのファイルはもう置き換えない）。新しい表だけなので Plain。適用後は編集しない。

CREATE TABLE export_files (
  path        TEXT    NOT NULL PRIMARY KEY,              -- 書いたファイルの絶対パス（書き出し先を正規化したフォルダ＋書いた名前）
  item_id     INTEGER NOT NULL REFERENCES items(id) ON DELETE CASCADE,
  hash        BLOB    NOT NULL,                          -- 書いた中身
  size_bytes  INTEGER NOT NULL CONSTRAINT export_files_size CHECK (size_bytes >= 0),
  mtime_ns    INTEGER NOT NULL,                          -- 書いた直後の更新時刻（Unix ナノ秒）
  job_id      INTEGER NOT NULL,                          -- 書いた書き出しのジョブ（同じ回に書いたものは置き換えない）
  written_at  INTEGER NOT NULL
) WITHOUT ROWID;
CREATE INDEX idx_export_files_item ON export_files(item_id);
