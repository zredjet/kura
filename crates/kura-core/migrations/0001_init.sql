-- 0001_init: 初期スキーマ（docs/data-model.md §3 が正本）。適用後は編集しない。

-- ===== アイテム =====
CREATE TABLE items (
  id              INTEGER PRIMARY KEY,
  hash            BLOB    NOT NULL UNIQUE,          -- BLAKE3 32 バイト
  ext             TEXT    NOT NULL,                 -- 小文字。例 'jpg'
  kind            TEXT    NOT NULL CHECK (kind IN ('image','video','audio')),
  mime            TEXT,
  size_bytes      INTEGER NOT NULL,
  width           INTEGER,
  height          INTEGER,
  duration_ms     INTEGER,                          -- 音楽・動画
  codec_info      TEXT,                             -- ffprobe 要約 JSON
  title           TEXT    NOT NULL,                 -- 表示名（初期値は元ファイル名から拡張子を除いたもの）
  original_name   TEXT    NOT NULL,
  original_path   TEXT,                             -- 取り込み元（参考情報。アクセスはしない）
  source_url      TEXT,
  source_site     TEXT,                             -- 'local','youtube','pixiv','x','capture' など
  rating          INTEGER NOT NULL DEFAULT 0 CHECK (rating BETWEEN 0 AND 5),
  note            TEXT,
  captured_at     INTEGER,                          -- 撮影・投稿日時
  imported_at     INTEGER NOT NULL,
  updated_at      INTEGER NOT NULL,
  trashed_at      INTEGER,
  play_count      INTEGER NOT NULL DEFAULT 0,
  last_played_at  INTEGER,
  thumb_state     TEXT    NOT NULL DEFAULT 'pending' CHECK (thumb_state IN ('pending','ready','failed')),
  proxy_state     TEXT    NOT NULL DEFAULT 'unknown' CHECK (proxy_state IN ('unknown','not_needed','pending','ready','failed'))
);
CREATE INDEX idx_items_imported ON items(imported_at DESC, id DESC) WHERE trashed_at IS NULL;
CREATE INDEX idx_items_kind     ON items(kind, imported_at DESC) WHERE trashed_at IS NULL;
CREATE INDEX idx_items_rating   ON items(rating) WHERE trashed_at IS NULL;
CREATE INDEX idx_items_played   ON items(last_played_at);
CREATE INDEX idx_items_trashed  ON items(trashed_at) WHERE trashed_at IS NOT NULL;
CREATE INDEX idx_items_size     ON items(size_bytes);                       -- 取り込みの重複の見込み（サイズで候補を絞る）

-- ===== 仮想フォルダ =====
CREATE TABLE folders (
  id          INTEGER PRIMARY KEY,
  parent_id   INTEGER REFERENCES folders(id) ON DELETE CASCADE,
  name        TEXT    NOT NULL,
  color       TEXT,                                 -- トークン名（'blue','violet','amber','teal'）
  pos         REAL    NOT NULL,
  created_at  INTEGER NOT NULL
);
-- ルート（parent_id IS NULL）でも名前の重複を防ぐため式インデックスにする
CREATE UNIQUE INDEX idx_folders_name ON folders(ifnull(parent_id, 0), name);

CREATE TABLE item_folders (
  folder_id   INTEGER NOT NULL REFERENCES folders(id) ON DELETE CASCADE,
  item_id     INTEGER NOT NULL REFERENCES items(id)   ON DELETE CASCADE,
  pos         REAL,
  PRIMARY KEY (folder_id, item_id)
) WITHOUT ROWID;
CREATE INDEX idx_item_folders_item ON item_folders(item_id);

-- ===== タグ =====
CREATE TABLE tags (
  id          INTEGER PRIMARY KEY,
  namespace   TEXT    NOT NULL DEFAULT '',          -- '作者','場所','投稿者' など。空は名前空間なし
  name        TEXT    NOT NULL,
  color       TEXT,
  created_at  INTEGER NOT NULL,
  UNIQUE (namespace, name)
);
CREATE TABLE tag_aliases (                          -- 別名 → 正式タグ
  alias       TEXT    PRIMARY KEY,
  tag_id      INTEGER NOT NULL REFERENCES tags(id) ON DELETE CASCADE
);
CREATE TABLE tag_parents (                          -- 子タグを付けると親タグも付いているとみなす
  child_id    INTEGER NOT NULL REFERENCES tags(id) ON DELETE CASCADE,
  parent_id   INTEGER NOT NULL REFERENCES tags(id) ON DELETE CASCADE,
  PRIMARY KEY (child_id, parent_id)
) WITHOUT ROWID;
CREATE TABLE item_tags (
  tag_id      INTEGER NOT NULL REFERENCES tags(id)  ON DELETE CASCADE,
  item_id     INTEGER NOT NULL REFERENCES items(id) ON DELETE CASCADE,
  source      TEXT    NOT NULL DEFAULT 'user' CHECK (source IN ('user','import','download','capture')),
  PRIMARY KEY (tag_id, item_id)
) WITHOUT ROWID;
CREATE INDEX idx_item_tags_item ON item_tags(item_id);

-- ===== ユニット（管理単位）=====
CREATE TABLE units (
  id                 INTEGER PRIMARY KEY,
  kind               TEXT    NOT NULL CHECK (kind IN ('set','folder')),
  name               TEXT    NOT NULL,
  cover_item_id      INTEGER REFERENCES items(id) ON DELETE SET NULL,
  order_mode         TEXT    NOT NULL CHECK (order_mode IN ('manual','name','imported','captured','duration')),
  inner_shuffle      INTEGER NOT NULL DEFAULT 0,
  image_duration_ms  INTEGER,
  rating             INTEGER NOT NULL DEFAULT 0 CHECK (rating BETWEEN 0 AND 5),
  note               TEXT,
  origin             TEXT    NOT NULL DEFAULT 'user' CHECK (origin IN ('user','import','download')),
  source_url         TEXT,
  created_at         INTEGER NOT NULL,
  updated_at         INTEGER NOT NULL,
  play_count         INTEGER NOT NULL DEFAULT 0,
  last_played_at     INTEGER
);
CREATE INDEX idx_units_name ON units(name);

CREATE TABLE unit_items (                           -- kind = 'set' のメンバー
  unit_id     INTEGER NOT NULL REFERENCES units(id) ON DELETE CASCADE,
  item_id     INTEGER NOT NULL REFERENCES items(id) ON DELETE CASCADE,
  pos         REAL    NOT NULL,
  PRIMARY KEY (unit_id, item_id)
) WITHOUT ROWID;
CREATE INDEX idx_unit_items_item ON unit_items(item_id);

CREATE TABLE unit_folders (                         -- kind = 'folder' の参照フォルダ
  unit_id     INTEGER NOT NULL REFERENCES units(id)   ON DELETE CASCADE,
  folder_id   INTEGER NOT NULL REFERENCES folders(id) ON DELETE CASCADE,
  recursive   INTEGER NOT NULL DEFAULT 1,
  pos         REAL    NOT NULL,
  PRIMARY KEY (unit_id, folder_id)
) WITHOUT ROWID;

CREATE TABLE unit_tags (
  tag_id      INTEGER NOT NULL REFERENCES tags(id)  ON DELETE CASCADE,
  unit_id     INTEGER NOT NULL REFERENCES units(id) ON DELETE CASCADE,
  PRIMARY KEY (tag_id, unit_id)
) WITHOUT ROWID;

-- ===== スマートフォルダ（保存した検索）=====
CREATE TABLE smart_folders (
  id          INTEGER PRIMARY KEY,
  name        TEXT    NOT NULL,
  query       TEXT    NOT NULL,                     -- 検索クエリ文字列（docs/search-query.md）
  sort        TEXT    NOT NULL DEFAULT 'imported_desc',
  pos         REAL    NOT NULL,
  created_at  INTEGER NOT NULL
);

-- ===== プレイリスト =====
CREATE TABLE playlists (
  id                   INTEGER PRIMARY KEY,
  name                 TEXT    NOT NULL,
  kind                 TEXT    NOT NULL CHECK (kind IN ('manual','smart')),
  granularity          TEXT    NOT NULL DEFAULT 'file' CHECK (granularity IN ('file','unit')),
  shuffle              INTEGER NOT NULL DEFAULT 0,
  shuffle_seed         INTEGER,
  seed_locked          INTEGER NOT NULL DEFAULT 0,
  regenerate_on_play   INTEGER NOT NULL DEFAULT 1,
  weight_by_rating     INTEGER NOT NULL DEFAULT 1,
  max_same_kind_run    INTEGER NOT NULL DEFAULT 2,
  dedup_items          INTEGER NOT NULL DEFAULT 1,
  image_duration_ms    INTEGER NOT NULL DEFAULT 8000,
  bgm_through_images   INTEGER NOT NULL DEFAULT 1,
  repeat_mode          TEXT    NOT NULL DEFAULT 'off' CHECK (repeat_mode IN ('off','all','one')),
  rule_json            TEXT,                        -- smart のみ。検索 AST の JSON
  limit_kind           TEXT    CHECK (limit_kind IN ('count','duration')),
  limit_value          INTEGER,
  unit_match           TEXT    NOT NULL DEFAULT 'any' CHECK (unit_match IN ('any','all','unit_only')),
  include_loose_items  INTEGER NOT NULL DEFAULT 0,
  pos                  REAL    NOT NULL,
  created_at           INTEGER NOT NULL,
  updated_at           INTEGER NOT NULL
);

CREATE TABLE playlist_entries (
  id                 INTEGER PRIMARY KEY,
  playlist_id        INTEGER NOT NULL REFERENCES playlists(id) ON DELETE CASCADE,
  pos                REAL    NOT NULL,
  entry_type         TEXT    NOT NULL CHECK (entry_type IN ('item','unit','folder')),
  item_id            INTEGER REFERENCES items(id)   ON DELETE CASCADE,
  unit_id            INTEGER REFERENCES units(id)   ON DELETE CASCADE,
  folder_id          INTEGER REFERENCES folders(id) ON DELETE CASCADE,
  recursive          INTEGER NOT NULL DEFAULT 1,     -- folder のときだけ意味を持つ
  image_duration_ms  INTEGER,
  start_ms           INTEGER,
  end_ms             INTEGER,
  CHECK (
    (entry_type = 'item'   AND item_id   IS NOT NULL AND unit_id IS NULL AND folder_id IS NULL) OR
    (entry_type = 'unit'   AND unit_id   IS NOT NULL AND item_id IS NULL AND folder_id IS NULL) OR
    (entry_type = 'folder' AND folder_id IS NOT NULL AND item_id IS NULL AND unit_id   IS NULL)
  )
);
CREATE INDEX idx_playlist_entries_pos ON playlist_entries(playlist_id, pos);

CREATE TABLE playlist_state (                       -- 再開用
  playlist_id   INTEGER PRIMARY KEY REFERENCES playlists(id) ON DELETE CASCADE,
  seed          INTEGER NOT NULL,
  slot_index    INTEGER NOT NULL,
  position_ms   INTEGER NOT NULL DEFAULT 0,
  updated_at    INTEGER NOT NULL
);

CREATE TABLE play_events (
  id            INTEGER PRIMARY KEY,
  item_id       INTEGER NOT NULL REFERENCES items(id) ON DELETE CASCADE,
  unit_id       INTEGER REFERENCES units(id)     ON DELETE SET NULL,
  playlist_id   INTEGER REFERENCES playlists(id) ON DELETE SET NULL,
  played_at     INTEGER NOT NULL,
  played_ms     INTEGER NOT NULL,
  completed     INTEGER NOT NULL
);
CREATE INDEX idx_play_events_item ON play_events(item_id, played_at DESC);

-- ===== ジョブ =====
CREATE TABLE jobs (
  id            INTEGER PRIMARY KEY,
  kind          TEXT    NOT NULL CHECK (kind IN ('import','thumbnail','proxy','download','capture')),
  state         TEXT    NOT NULL CHECK (state IN ('queued','running','paused','done','failed','canceled')),
  priority      INTEGER NOT NULL DEFAULT 0,
  input_json    TEXT    NOT NULL,
  progress      REAL,                               -- 0.0〜1.0
  message       TEXT,                               -- 画面に出す一行（例：'38 / 112 枚'）
  error_code    TEXT,
  log_path      TEXT,
  result_json   TEXT,
  created_at    INTEGER NOT NULL,
  started_at    INTEGER,
  finished_at   INTEGER
);
CREATE INDEX idx_jobs_queue ON jobs(state, priority DESC, created_at);

-- ===== ダウンロード元とアイテムの対応 =====
CREATE TABLE remote_sources (
  site          TEXT    NOT NULL,                   -- 'youtube','pixiv' など
  remote_id     TEXT    NOT NULL,                   -- 動画 ID・作品 ID
  item_id       INTEGER NOT NULL REFERENCES items(id) ON DELETE CASCADE,
  unit_id       INTEGER REFERENCES units(id) ON DELETE SET NULL,
  page_index    INTEGER,                            -- 複数ページ作品のページ番号
  fetched_at    INTEGER NOT NULL,
  PRIMARY KEY (site, remote_id, item_id)
) WITHOUT ROWID;
CREATE INDEX idx_remote_sources_item ON remote_sources(item_id);

-- ===== キャプチャのルール =====
CREATE TABLE capture_rules (
  id                INTEGER PRIMARY KEY,
  name              TEXT    NOT NULL,
  url_pattern       TEXT    NOT NULL,               -- '*' ワイルドカード
  mode              TEXT    NOT NULL CHECK (mode IN ('element','full_page','viewport')),
  target_selector   TEXT,                           -- mode = element のとき
  hide_selectors    TEXT,                           -- JSON 配列
  viewport_width    INTEGER NOT NULL DEFAULT 1200,
  scroll_times      INTEGER NOT NULL DEFAULT 0,
  wait_ms           INTEGER NOT NULL DEFAULT 1500,
  dest_folder_id    INTEGER REFERENCES folders(id) ON DELETE SET NULL,
  enabled           INTEGER NOT NULL DEFAULT 1,
  last_run_at       INTEGER,
  last_status       TEXT,
  pos               REAL    NOT NULL
);

-- ===== 設定 =====
CREATE TABLE settings (
  key    TEXT PRIMARY KEY,
  value  TEXT NOT NULL                              -- JSON
);

-- ===== 全文検索（外部コンテンツ FTS5、trigram）=====
CREATE VIRTUAL TABLE items_fts USING fts5(
  title, original_name, note, source_url,
  content = 'items', content_rowid = 'id',
  tokenize = 'trigram'
);
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
