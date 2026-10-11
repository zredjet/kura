# データモデル

ライブラリは 1 つのフォルダ（`<名前>.kuralib`）で完結します。DB は SQLite 1 ファイル。
この文書のスキーマが正本です。変更は `.claude/skills/add-migration` の手順で行い、ここも更新します。

## 1. ライブラリのフォルダ構成

```text
Kura Library.kuralib/
  library.sqlite            # DB（-wal, -shm も同居）
  files/ab/cd/<blake3>.<ext>    # 実ファイル（内容アドレス。ハッシュ先頭 2 文字 / 次の 2 文字）
  thumbs/ab/<blake3>_s.jpg      # サムネイル 長辺 512px（グリッド用）
  thumbs/ab/<blake3>_l.jpg      # サムネイル 長辺 1600px（インスペクタ・プレビュー用）
  art/ab/<blake3>.jpg           # 音楽のジャケット画像
  proxies/ab/<blake3>.mp4       # 再生用プロキシ（WKWebView で再生できない形式だけ）
  staging/                      # 取り込み・ダウンロード・キャプチャの一時置き場
  archives/                     # yt-dlp / gallery-dl の download-archive
  logs/                         # ジョブのログ
```

- `files/` のファイル名はハッシュなので、元のファイル名は DB（`original_name`）にだけ残る。
- 既定の場所は初回起動時にユーザーが選ぶ（候補：`~/Pictures/Kura Library.kuralib`）。外付けディスクも可。
- iCloud Drive など同期フォルダの中は非推奨（SQLite の WAL と相性が悪い）。選ばれたら警告する。

## 2. 共通方針

- 時刻：UTC の Unix ミリ秒（INTEGER）。
- 真偽値：INTEGER 0/1。
- 並び順：`pos REAL`（間に挿入できるように）。
- 論理削除：アイテムは `trashed_at` を立てる。ゴミ箱を空にしたときに行とファイルを消す。
- 接続時の PRAGMA：`journal_mode=WAL`、`synchronous=NORMAL`、`foreign_keys=ON`、`temp_store=MEMORY`、`mmap_size=268435456`、`cache_size=-65536`。

## 3. スキーマ（`0001_init.sql` と、それ以降のマイグレーションを反映したもの）

| マイグレーション | 内容 |
|---|---|
| `0001_init` | 初期スキーマ |
| `0002_thumb_pending_index` | `idx_items_thumb_pending`（サムネイル未作成を新しい順に取り出す部分索引） |
| `0003_proxy_todo_index` | 画像の `proxy_state` を `not_needed` にし、`idx_items_proxy_unknown` / `idx_items_proxy_pending`（プロキシの判定・作成を待つものを新しい順に取り出す部分索引）を足す |
| `0004_playlist_generation` | `playlists.generated_at` / `sort_key` / `sort_desc`（スマートの並びを作った日時と、シャッフルしないときの並び順）と `playlist_state.item_id`（再開するアイテム）を足す |

```sql
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
  codec_info      TEXT,                             -- ffprobe 要約 JSON：{"codec": 主なコーデック}、プロキシの判定後は {"codec", "video", "audio"}
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
                                                    -- 判定前／元のまま再生できる（画像は常にこれ）／作成待ち／作成済み／読めない・作れない（architecture §8.1）
);
CREATE INDEX idx_items_imported ON items(imported_at DESC, id DESC) WHERE trashed_at IS NULL;
CREATE INDEX idx_items_kind     ON items(kind, imported_at DESC) WHERE trashed_at IS NULL;
CREATE INDEX idx_items_rating   ON items(rating) WHERE trashed_at IS NULL;
CREATE INDEX idx_items_played   ON items(last_played_at);
CREATE INDEX idx_items_trashed  ON items(trashed_at) WHERE trashed_at IS NOT NULL;
CREATE INDEX idx_items_size     ON items(size_bytes);                       -- 取り込みの重複の見込み（サイズで候補を絞る）
CREATE INDEX idx_items_thumb_pending ON items(imported_at DESC, id DESC) WHERE thumb_state = 'pending'; -- 0002：サムネイルのジョブが新しい順に取り出す
CREATE INDEX idx_items_proxy_unknown ON items(imported_at DESC, id DESC) WHERE proxy_state = 'unknown'; -- 0003：プロキシのジョブが判定するもの
CREATE INDEX idx_items_proxy_pending ON items(imported_at DESC, id DESC) WHERE proxy_state = 'pending'; -- 0003：プロキシのジョブが作るもの

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
  rule_json            TEXT,                        -- smart のみ。{"match":"all|any","clauses":["検索の文字列", …]}（ADR-78）
  limit_kind           TEXT    CHECK (limit_kind IN ('count','duration')),
  limit_value          INTEGER,
  unit_match           TEXT    NOT NULL DEFAULT 'any' CHECK (unit_match IN ('any','all','unit_only')),
  include_loose_items  INTEGER NOT NULL DEFAULT 0,
  pos                  REAL    NOT NULL,
  created_at           INTEGER NOT NULL,
  updated_at           INTEGER NOT NULL,
  generated_at         INTEGER,                     -- 0004：smart の並びを最後に作った日時（ADR-79）
  sort_key             TEXT    NOT NULL DEFAULT 'imported'   -- 0004：smart でシャッフルしないときの並び順
                       CHECK (sort_key IN ('imported','captured','name','rating','duration','size')),
  sort_desc            INTEGER NOT NULL DEFAULT 1   -- 0004：1 なら降順
);

CREATE TABLE playlist_entries (                    -- 手動のエントリと、smart が作った並び（ADR-79）
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
  updated_at    INTEGER NOT NULL,
  item_id       INTEGER                             -- 0004：その時のアイテム。スロットがずれていたらこのアイテムから（ADR-86）
);

CREATE TABLE play_events (                          -- 再生済みになったスロットごとに 1 行（playlists.md §6.4）
  id            INTEGER PRIMARY KEY,
  item_id       INTEGER NOT NULL REFERENCES items(id) ON DELETE CASCADE,
  unit_id       INTEGER REFERENCES units(id)     ON DELETE SET NULL,  -- ユニットとして再生したとき
  playlist_id   INTEGER REFERENCES playlists(id) ON DELETE SET NULL,  -- M6（選択から作ったキューでは NULL）
  played_at     INTEGER NOT NULL,
  played_ms     INTEGER NOT NULL,                   -- 実際に再生・表示した時間（シークで飛ばした分は除く）
  completed     INTEGER NOT NULL                    -- 最後まで再生・表示したか
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
-- キー：'playback.support' = プロキシの判定に使った再生可否の表の指紋（文字列。変われば判定をやり直す。M5）

-- ===== 全文検索（外部コンテンツ FTS5、trigram）=====
CREATE VIRTUAL TABLE items_fts USING fts5(
  title, original_name, note, source_url,
  content = 'items', content_rowid = 'id',
  tokenize = 'trigram'
);
-- items の INSERT / UPDATE / DELETE に合わせて items_fts を更新するトリガー（items_fts_ai / _ad / _au）を同じマイグレーションで作る
```

## 4. クエリ方針

### タグの AND / OR / NOT

```sql
-- AND（タグ 3 つ）
SELECT item_id FROM item_tags WHERE tag_id IN (?1, ?2, ?3)
GROUP BY item_id HAVING COUNT(*) = 3;

-- NOT
... AND i.id NOT IN (SELECT item_id FROM item_tags WHERE tag_id IN (?))
```

- 親タグ：検索語のタグ ID を「そのタグ＋子孫タグ」の集合に展開してから使う。タグの親子関係はメモリ上にキャッシュし、変更時に作り直す。
- 別名：入力を `tag_aliases` で正式タグに置き換えてから使う。

### 仮想フォルダ（子孫を含む）

再帰 CTE で子孫フォルダ ID を求め、`item_folders` と結合する。フォルダ木全体は小さいのでメモリにキャッシュしてよい。

### 一覧の取得（ID 先取り）

1. `items_query_ids(query, sort)` で条件に合う ID と縦横比だけを並び順で返す（`tauri::ipc::Response` で生バイト。形式は `docs/architecture.md` §3）。縦横比は `width / height`、音楽と不明は 1.0。
2. フロントは見えている範囲の ID だけ `items_get(ids)` で詳細を取る（最大 200 件ずつ）。

### サイドバーの一覧（M3）

- すべて・最近の取り込み・未分類・タグなしは `idx_items_imported` の順に読み、未分類・タグなしは `NOT EXISTS (… item_folders / item_tags …)` で絞る（それぞれ `idx_item_folders_item`・`idx_item_tags_item` を引く）。
- フォルダは再帰 CTE で子孫フォルダを求め、`id IN (SELECT item_id FROM item_folders WHERE folder_id IN …)` で絞る。
- 「ユニットをまとめて表示」は、ゴミ箱以外の set ユニットの所属（`unit_items`）を一度読んで、Rust で最初に現れた位置にまとめる。
- フォルダの件数は `item_folders` を `item_id` 順に一度読み、祖先へ数え上げる（同じアイテムは 1 回）。

### 全文検索

- 3 文字以上：`items_fts MATCH ?`（語は `"…"` で囲んだ句として渡す）。
- 1〜2 文字：`title` / `original_name` / `note` / `source_url` の `LIKE '%…%' ESCAPE '\'`（インデックスは効かないが、10 万件で 21ms）。

### 検索の条件（M4）

- 検索言語（docs/search-query.md）の AST を `items i` に対する WHERE に変換し、一覧の種類の条件に AND で重ねる。値はすべてバインド変数。
- タグの AND は `i.id IN (SELECT item_id FROM item_tags WHERE tag_id IN (…) INTERSECT …)`（主キー `(tag_id, item_id)` の範囲を交わらせる）。
- スマートフォルダは `smart_folders.query` に検索の文字列をそのまま保存し、開くたびに解析・変換する（相対の日付が今の時刻で解決されるように）。

### ユニットの集計

set ユニットの件数・合計時間・種類は表示のたびに集計してよい（数百件程度）。遅くなったら `units` に集計列を追加し、トリガーで更新する。

### 遅くなったら

10 万件超でタグ検索が 50ms を超えるようになったら、起動時に「タグ ID → アイテム ID の Roaring Bitmap」をメモリに作り、集合演算で絞り込む方式を追加する（DB はそのまま正本）。
