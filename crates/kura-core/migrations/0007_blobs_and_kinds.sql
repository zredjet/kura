-- 0007（Rebuild）：実ファイルの台帳 blobs と、items の作り直し（docs/data-model.md §3・§5）。
-- - kind に font・model を足す。CHECK に名前を付ける（items_kind・items_rating・items_thumb_state・items_proxy_state・
--   items_analysis_state、表の制約 items_proxy_av）。以後の値の追加は ALTER TABLE … DROP/ADD CONSTRAINT で行う。
-- - hash は台帳を参照する。台帳への登録はトリガ items_blob_bi・items_blob_bu が行う。
-- - 末尾に analysis_state（解析の進み具合。画像・音声は pending、ほかは not_needed）と thumb_rev（サムネイルを作り直した回数。
--   URL の ?r= に使う）を足す。thumb_state に 'stale'（今の絵を出したまま作り直す）を足す。
-- runner がトランザクションの外で foreign_keys を切ってから 1 つのトランザクションで当て、PRAGMA foreign_key_check に
-- 新しい違反がないことを確かめてからコミットする（切らずに items を DROP すると ON DELETE CASCADE で子の行が消える）。
-- 古い表を先に改名しない（子の REFERENCES が改名後の名前を指したまま残る）。新しい表を作って移し、古い表を消してから
-- 新しい表の名前を変える。適用後は編集しない。

-- ===== 実ファイルの台帳（files/ab/cd/<hash>.<ext> 1 つに 1 行）=====
CREATE TABLE blobs (
  id          INTEGER PRIMARY KEY,
  hash        BLOB    NOT NULL UNIQUE,                 -- BLAKE3 32 バイト
  ext         TEXT    NOT NULL,                        -- files/ のファイルの拡張子（小文字）
  size_bytes  INTEGER NOT NULL CONSTRAINT blobs_size CHECK (size_bytes >= 0),
  created_at  INTEGER NOT NULL                         -- 台帳に載せた時刻（0007 では取り込んだ時刻）
);
CREATE INDEX idx_blobs_size ON blobs(size_bytes);       -- 容量（覆う索引）・重複の見込み（サイズで候補を絞る）

-- 今の items の 1 行が files/ の 1 ファイル（items.hash は UNIQUE。ゴミ箱のものもファイルは残っている）
INSERT INTO blobs (hash, ext, size_bytes, created_at)
  SELECT hash, ext, size_bytes, imported_at FROM items ORDER BY id;

-- ===== items（列の並びは 0001 と同じ。id の値を保つ：FTS の rowid・playlist_state.item_id・jobs の JSON が値で持つ）=====
CREATE TABLE items_new (
  id              INTEGER PRIMARY KEY,
  hash            BLOB    NOT NULL UNIQUE REFERENCES blobs(hash),
  ext             TEXT    NOT NULL,
  kind            TEXT    NOT NULL CONSTRAINT items_kind CHECK (kind IN ('image','video','audio','font','model')),
  mime            TEXT,
  size_bytes      INTEGER NOT NULL,
  width           INTEGER,
  height          INTEGER,
  duration_ms     INTEGER,
  codec_info      TEXT,
  title           TEXT    NOT NULL,
  original_name   TEXT    NOT NULL,
  original_path   TEXT,
  source_url      TEXT,
  source_site     TEXT,
  rating          INTEGER NOT NULL DEFAULT 0 CONSTRAINT items_rating CHECK (rating BETWEEN 0 AND 5),
  note            TEXT,
  captured_at     INTEGER,
  imported_at     INTEGER NOT NULL,
  updated_at      INTEGER NOT NULL,
  trashed_at      INTEGER,
  play_count      INTEGER NOT NULL DEFAULT 0,
  last_played_at  INTEGER,
  thumb_state     TEXT    NOT NULL DEFAULT 'pending'
                  CONSTRAINT items_thumb_state CHECK (thumb_state IN ('pending','ready','stale','failed')),
  proxy_state     TEXT    NOT NULL DEFAULT 'unknown'
                  CONSTRAINT items_proxy_state CHECK (proxy_state IN ('unknown','not_needed','pending','ready','failed')),
  analysis_state  TEXT    NOT NULL DEFAULT 'pending'
                  CONSTRAINT items_analysis_state CHECK (analysis_state IN ('pending','ready','failed','not_needed')),
  thumb_rev       INTEGER NOT NULL DEFAULT 0,
  -- プロキシを作るのは video・audio だけ（ADR-140）。ほかの種類は作成待ち・作成済み・失敗にならない
  -- （既定の unknown は許す：判定のジョブは +kind IN ('video','audio') で選ぶので触れない）
  CONSTRAINT items_proxy_av CHECK (kind IN ('video','audio') OR proxy_state IN ('unknown','not_needed'))
);
INSERT INTO items_new (id, hash, ext, kind, mime, size_bytes, width, height, duration_ms, codec_info, title,
                       original_name, original_path, source_url, source_site, rating, note, captured_at,
                       imported_at, updated_at, trashed_at, play_count, last_played_at, thumb_state, proxy_state,
                       analysis_state, thumb_rev)
  SELECT id, hash, ext, kind, mime, size_bytes, width, height, duration_ms, codec_info, title,
         original_name, original_path, source_url, source_site, rating, note, captured_at,
         imported_at, updated_at, trashed_at, play_count, last_played_at, thumb_state,
         -- items_proxy_av に反する行（画像で pending・ready・failed）は意味がないので not_needed にする（当てられずに開けなくなるのを防ぐ）
         CASE WHEN kind IN ('video','audio') OR proxy_state IN ('unknown','not_needed') THEN proxy_state ELSE 'not_needed' END,
         CASE WHEN kind IN ('image','audio') THEN 'pending' ELSE 'not_needed' END,
         0
  FROM items;
DROP TABLE items;                       -- 索引 9 つと FTS のトリガ 3 つも一緒に消える（foreign_keys=OFF なので子は消えない）
ALTER TABLE items_new RENAME TO items;  -- 子の REFERENCES items(id) は名前で解決されるので、新しい表を指す

CREATE INDEX idx_items_imported ON items(imported_at DESC, id DESC) WHERE trashed_at IS NULL;
CREATE INDEX idx_items_kind     ON items(kind, imported_at DESC) WHERE trashed_at IS NULL;
CREATE INDEX idx_items_rating   ON items(rating) WHERE trashed_at IS NULL;
CREATE INDEX idx_items_played   ON items(last_played_at);
CREATE INDEX idx_items_trashed  ON items(trashed_at) WHERE trashed_at IS NOT NULL;
CREATE INDEX idx_items_size     ON items(size_bytes);
CREATE INDEX idx_items_thumb_pending ON items(imported_at DESC, id DESC) WHERE thumb_state = 'pending';
CREATE INDEX idx_items_proxy_unknown ON items(imported_at DESC, id DESC) WHERE proxy_state = 'unknown';
CREATE INDEX idx_items_proxy_pending ON items(imported_at DESC, id DESC) WHERE proxy_state = 'pending';
-- 新しい索引：サムネイルの作り直し（pending がなくなったら新しい順に取る）と、解析のジョブの取り出し
CREATE INDEX idx_items_thumb_stale ON items(imported_at DESC, id DESC) WHERE thumb_state = 'stale';
CREATE INDEX idx_items_analysis_pending ON items(imported_at DESC, id DESC) WHERE analysis_state = 'pending';

-- items_fts（外部コンテンツ）は消さない。rowid = items.id を保ったので索引はそのまま使える
CREATE TRIGGER items_fts_ai AFTER INSERT ON items BEGIN
  INSERT INTO items_fts(rowid, title, original_name, note, source_url)
  VALUES (new.id, new.title, new.original_name, new.note, new.source_url);
END;
CREATE TRIGGER items_fts_ad AFTER DELETE ON items BEGIN
  INSERT INTO items_fts(items_fts, rowid, title, original_name, note, source_url)
  VALUES ('delete', old.id, old.title, old.original_name, old.note, old.source_url);
END;
CREATE TRIGGER items_fts_au AFTER UPDATE OF title, original_name, note, source_url ON items BEGIN
  INSERT INTO items_fts(items_fts, rowid, title, original_name, note, source_url)
  VALUES ('delete', old.id, old.title, old.original_name, old.note, old.source_url);
  INSERT INTO items_fts(rowid, title, original_name, note, source_url)
  VALUES (new.id, new.title, new.original_name, new.note, new.source_url);
END;

-- ===== 台帳への自動の登録 =====
-- items の (hash, ext, size_bytes) は台帳の行の写し。入れる・変えるときに台帳になければ載せ、あって食い違えば断る
-- （外部キー items.hash → blobs.hash は文の終わりに確かめる。BEFORE のトリガで先に載せる）。
-- OR IGNORE は使わない（外側の文の衝突の方針がトリガの中に効くのを避ける）。
-- 台帳の行を消すのは呼び手（ゴミ箱を空にする・M12 の GC）。参照されている行は外部キーで消せない。
CREATE TRIGGER items_blob_bi BEFORE INSERT ON items BEGIN
  SELECT RAISE(ABORT, 'items_blob_mismatch: 台帳の拡張子・サイズと食い違います')
   WHERE EXISTS (SELECT 1 FROM blobs WHERE hash = new.hash AND (ext <> new.ext OR size_bytes <> new.size_bytes));
  INSERT INTO blobs (hash, ext, size_bytes, created_at)
    SELECT new.hash, new.ext, new.size_bytes, new.imported_at
     WHERE NOT EXISTS (SELECT 1 FROM blobs WHERE hash = new.hash);
END;
CREATE TRIGGER items_blob_bu BEFORE UPDATE OF hash, ext, size_bytes ON items BEGIN
  SELECT RAISE(ABORT, 'items_blob_mismatch: 台帳の拡張子・サイズと食い違います')
   WHERE EXISTS (SELECT 1 FROM blobs WHERE hash = new.hash AND (ext <> new.ext OR size_bytes <> new.size_bytes));
  INSERT INTO blobs (hash, ext, size_bytes, created_at)
    SELECT new.hash, new.ext, new.size_bytes, new.updated_at
     WHERE NOT EXISTS (SELECT 1 FROM blobs WHERE hash = new.hash);
END;
