# データモデル

ライブラリは 1 つのフォルダ（`<名前>.kuralib`）で完結します。DB は SQLite 1 ファイル。
この文書のスキーマが正本です。変更は `.claude/skills/add-migration` の手順で行い、ここも更新します。

## 1. ライブラリのフォルダ構成

```text
Kura Library.kuralib/
  library.sqlite            # DB（-wal, -shm も同居）
  files/ab/cd/<blake3>.<ext>    # 実ファイル（内容アドレス。ハッシュ先頭 2 文字 / 次の 2 文字）。1 つに台帳 blobs の 1 行
  thumbs/ab/<blake3>_s.{jpg,png}  # サムネイル 長辺 512px（グリッド用）。透明のないものは JPEG、透明のあるものは PNG（M11）
  thumbs/ab/<blake3>_l.{jpg,png}  # サムネイル 長辺 1600px（インスペクタ・プレビュー用）
  art/ab/<blake3>.jpg           # 音楽のジャケット画像
  proxies/ab/<blake3>.mp4       # 再生用プロキシ（WKWebView で再生できない形式だけ）
  peaks/ab/<blake3>.peaks       # 音声の波形の段（解析のジョブが書く。M11）
  anim/ab/<blake3>-1.png        # アニメの帯（見たときにその場で作る。末尾は形式の版。M11）
  staging/                      # 取り込み・ダウンロード・キャプチャの一時置き場
  archives/                     # yt-dlp / gallery-dl の download-archive
  logs/                         # ジョブのログ
  backups/library-YYYYMMDD-HHMMSS.sqlite  # DB のバックアップ（UTC。新しい 7 個。FR-SYS-03・ADR-121。マイグレーションの前のものも同じ名前。§5）
```

- `files/` のファイル名はハッシュなので、元のファイル名は DB（`original_name`）にだけ残る。
- `thumbs/`・`art/`・`proxies/`・`peaks/`・`anim/` は中身から作った派生のファイル（`Layout::derived_paths`）。台帳の行を解放したとき
  （ゴミ箱を空にしたとき）に、実ファイルと一緒に消す。派生のファイルは整合性チェックの対象の外。
- 既定の場所は初回起動時にユーザーが選ぶ（候補：`~/Pictures/Kura Library.kuralib`）。外付けディスクも可。
- iCloud Drive など同期フォルダの中は非推奨（SQLite の WAL と相性が悪い）。選ばれたら警告する。
- マイグレーションを当てる前にも、いつもの名前でバックアップを作る（7 個の保持に乗る。§5）。
- **バックアップから戻す**（アプリには入れない。ADR-121）：Kura を終了し、`library.sqlite`・`library.sqlite-wal`・`library.sqlite-shm` を別の場所へ移してから、`backups/` の戻したいファイルを `library.sqlite` という名前でライブラリのフォルダへコピーして開く。バックアップより後に取り込んだファイルは `files/` に残り、開いたときの整合性チェックで「DB にないファイル」として数えられる（消えはしない）。

## 2. 共通方針

- 時刻：UTC の Unix ミリ秒（INTEGER）。
- 真偽値：INTEGER 0/1。
- 並び順：`pos REAL`（間に挿入できるように）。
- 論理削除：アイテムは `trashed_at` を立てる。ゴミ箱を空にしたときに行とファイルを消す。
- 接続時の PRAGMA：`journal_mode=WAL`、`synchronous=NORMAL`、`foreign_keys=ON`、`temp_store=MEMORY`、`mmap_size=268435456`、`cache_size=-65536`。
- 閉じるときは読み取りの接続を先に閉じ、書き込みの接続を最後にする（最後に閉じた接続が WAL を本体へ移して消す）。
- **CHECK の名前**（0007〜）：列の CHECK は `<表>_<列>`（`items_kind`）、表の制約は `<表>_<意味>`（`items_proxy_av`）。
  違反したときのエラーの文に名前が出る。値を足すときは作り直さずに
  `ALTER TABLE <表> DROP CONSTRAINT <名前>; ALTER TABLE <表> ADD CONSTRAINT <名前> CHECK (…);` で入れ替える
  （同梱の SQLite 3.53 以上。テスト `named_checks_can_be_widened_without_a_rebuild` が版を見張る）。
- CHECK 付きの `ADD COLUMN` は全行を確かめる（10 万件で約 150ms）。表を作り直すついでに足すか、CHECK なしにする。
- **ほかの表から外部キーで参照される表**（`items`・`blobs`・`folders`・`tags`・`units`・`playlists` など）を作り直すときは
  Rebuild で当てる（§5）。`INSERT OR REPLACE INTO items`（`REPLACE INTO items`）は使わない（REPLACE は行を消してから入れるので、
  子の行が CASCADE で消える）。
- **中身ごととアイテムごと**：中身（ファイルのバイト列）から決まるもの（台帳・事実・解析の結果）は `blobs.id` を鍵にし、
  利用者がアイテムに決めたもの（素材の設定）は `items.id` を鍵にする。中身に依る書き込み（解析・サムネイル・プロキシの結果）は
  `WHERE id = ? AND hash = ?` のようにハッシュを条件にする（読んだあとに中身が変わっていたら書かない）。

## 3. スキーマ（`0001_init.sql` と、それ以降のマイグレーションを反映したもの）

| マイグレーション | 内容 |
|---|---|
| `0001_init` | 初期スキーマ |
| `0002_thumb_pending_index` | `idx_items_thumb_pending`（サムネイル未作成を新しい順に取り出す部分索引） |
| `0003_proxy_todo_index` | 画像の `proxy_state` を `not_needed` にし、`idx_items_proxy_unknown` / `idx_items_proxy_pending`（プロキシの判定・作成を待つものを新しい順に取り出す部分索引）を足す |
| `0004_playlist_generation` | `playlists.generated_at` / `sort_key` / `sort_desc`（スマートの並びを作った日時と、シャッフルしないときの並び順）と `playlist_state.item_id`（再開するアイテム）を足す |
| `0005_export_job` | `jobs.kind` に `export`（書き出し。FR-SYS-02）を足す。CHECK を変えるため `jobs` を作り直して中身を移す |
| `0006_unit_order_reverse` | `units.order_reverse`（並びを逆にする。ADR-130）を足す。今あるユニットは 0 |
| `0007_blobs_and_kinds`（Rebuild） | 実ファイルの台帳 `blobs` を items から作り、`items` を作り直す：`kind` に `font`・`model`、名前付きの CHECK、表の制約 `items_proxy_av`、`thumb_state` に `stale`、末尾に `analysis_state`（画像・音声は `pending`、ほかは `not_needed`）と `thumb_rev`。`items.hash` → `blobs.hash` の外部キーと台帳のトリガ `items_blob_bi`・`items_blob_bu`。索引 9 つと FTS のトリガを作り直し、`idx_items_thumb_stale`・`idx_items_analysis_pending` を足す。意味のない `proxy_state`（画像で pending など）は `not_needed` に正す |
| `0008_jobs_named_checks` | `jobs` を作り直し、`jobs_kind`（`analyze` を足す）・`jobs_state` に名前を付ける |
| `0009_analysis` | 中身ごとの事実 `media_facts`・解析の結果 `analysis_results`（どちらも `blob_id` が鍵で、台帳の行を消すと CASCADE）と、アイテムごとの素材の設定 `asset_settings`（`item_id` が鍵で CASCADE） |

```sql
-- ===== 実ファイルの台帳（0007。files/ab/cd/<hash>.<ext> 1 つに 1 行）=====
CREATE TABLE blobs (
  id          INTEGER PRIMARY KEY,                  -- 事実・結果（と M12 の版）が整数で参照する
  hash        BLOB    NOT NULL UNIQUE,              -- BLAKE3 32 バイト
  ext         TEXT    NOT NULL,                     -- files/ のファイルの拡張子（小文字）
  size_bytes  INTEGER NOT NULL CONSTRAINT blobs_size CHECK (size_bytes >= 0),
  created_at  INTEGER NOT NULL                      -- 台帳に載せた時刻（0007 では取り込んだ時刻）
);
CREATE INDEX idx_blobs_size ON blobs(size_bytes);   -- 容量（覆う索引）・重複の見込み（サイズで候補を絞る）

-- ===== アイテム（0007 で作り直した形）=====
CREATE TABLE items (
  id              INTEGER PRIMARY KEY,
  hash            BLOB    NOT NULL UNIQUE REFERENCES blobs(hash),  -- BLAKE3 32 バイト。台帳の行の写し
  ext             TEXT    NOT NULL,                 -- 小文字。例 'jpg'（台帳の行の写し）
  kind            TEXT    NOT NULL CONSTRAINT items_kind CHECK (kind IN ('image','video','audio','font','model')),
  mime            TEXT,
  size_bytes      INTEGER NOT NULL,                 -- 台帳の行の写し
  width           INTEGER,
  height          INTEGER,
  duration_ms     INTEGER,                          -- 音楽・動画
  codec_info      TEXT,                             -- ffprobe 要約 JSON：{"codec": 主なコーデック}、プロキシの判定後は {"codec", "video", "audio"}
  title           TEXT    NOT NULL,                 -- 表示名（初期値は元ファイル名から拡張子を除いたもの）
  original_name   TEXT    NOT NULL,
  original_path   TEXT,                             -- 取り込み元（参考情報。アクセスはしない）
  source_url      TEXT,
  source_site     TEXT,                             -- 'local','youtube','pixiv','x','capture' など
  rating          INTEGER NOT NULL DEFAULT 0 CONSTRAINT items_rating CHECK (rating BETWEEN 0 AND 5),
  note            TEXT,
  captured_at     INTEGER,                          -- 撮影・投稿日時
  imported_at     INTEGER NOT NULL,
  updated_at      INTEGER NOT NULL,
  trashed_at      INTEGER,
  play_count      INTEGER NOT NULL DEFAULT 0,
  last_played_at  INTEGER,
  thumb_state     TEXT    NOT NULL DEFAULT 'pending'
                  CONSTRAINT items_thumb_state CHECK (thumb_state IN ('pending','ready','stale','failed')),
                                                    -- stale：今の絵を出したまま作り直す（作り方が変わった。M11）
  proxy_state     TEXT    NOT NULL DEFAULT 'unknown'
                  CONSTRAINT items_proxy_state CHECK (proxy_state IN ('unknown','not_needed','pending','ready','failed')),
                                                    -- 判定前／元のまま再生できる（動画・音声以外は常にこれ）／作成待ち／作成済み／読めない・作れない（architecture §8.1）
  analysis_state  TEXT    NOT NULL DEFAULT 'pending'
                  CONSTRAINT items_analysis_state CHECK (analysis_state IN ('pending','ready','failed','not_needed')),
                                                    -- 解析の進み具合（M11）。解析するのは画像・音声だけ。動画・フォント・3D は not_needed
  thumb_rev       INTEGER NOT NULL DEFAULT 0,       -- サムネイルを作り直した回数（URL の ?r=。0 なら付けない。M11）
  -- プロキシを作るのは video・audio だけ（ADR-140）。ほかの種類は作成待ち・作成済み・失敗にならない（既定の unknown は許す）
  CONSTRAINT items_proxy_av CHECK (kind IN ('video','audio') OR proxy_state IN ('unknown','not_needed'))
);
CREATE INDEX idx_items_imported ON items(imported_at DESC, id DESC) WHERE trashed_at IS NULL;
CREATE INDEX idx_items_kind     ON items(kind, imported_at DESC) WHERE trashed_at IS NULL;
CREATE INDEX idx_items_rating   ON items(rating) WHERE trashed_at IS NULL;
CREATE INDEX idx_items_played   ON items(last_played_at);
CREATE INDEX idx_items_trashed  ON items(trashed_at) WHERE trashed_at IS NOT NULL;
CREATE INDEX idx_items_size     ON items(size_bytes);
CREATE INDEX idx_items_thumb_pending ON items(imported_at DESC, id DESC) WHERE thumb_state = 'pending'; -- 0002：サムネイルのジョブが新しい順に取り出す
CREATE INDEX idx_items_proxy_unknown ON items(imported_at DESC, id DESC) WHERE proxy_state = 'unknown'; -- 0003：プロキシのジョブが判定するもの
CREATE INDEX idx_items_proxy_pending ON items(imported_at DESC, id DESC) WHERE proxy_state = 'pending'; -- 0003：プロキシのジョブが作るもの
CREATE INDEX idx_items_thumb_stale   ON items(imported_at DESC, id DESC) WHERE thumb_state = 'stale';    -- 0007：pending がなくなったら作り直すもの
CREATE INDEX idx_items_analysis_pending ON items(imported_at DESC, id DESC) WHERE analysis_state = 'pending'; -- 0007：解析のジョブが新しい順に取り出す
-- プロキシのジョブの条件は `+kind IN ('video','audio')` と書く（`+` で kind の索引を使わせず、上の部分索引を新しい順に読む。
-- 素の `kind IN (…)` は TEMP B-TREE に退行する）

-- 台帳への自動の登録（0007）。items の (hash, ext, size_bytes) は台帳の行の写し。入れる・変えるときに台帳になければ載せ、
-- あって拡張子かサイズが食い違えば 'items_blob_mismatch' で断る（外部キーは文の終わりに確かめるので、BEFORE で先に載せる）。
-- 台帳の行を消すのは library::ledger::release だけ（どの表からも参照されない行だけ。参照されている行は外部キーで消せない）。
CREATE TRIGGER items_blob_bi BEFORE INSERT ON items BEGIN
  SELECT RAISE(ABORT, 'items_blob_mismatch: 台帳の拡張子・サイズと食い違います')
   WHERE EXISTS (SELECT 1 FROM blobs WHERE hash = new.hash AND (ext <> new.ext OR size_bytes <> new.size_bytes));
  INSERT INTO blobs (hash, ext, size_bytes, created_at)
    SELECT new.hash, new.ext, new.size_bytes, new.imported_at WHERE NOT EXISTS (SELECT 1 FROM blobs WHERE hash = new.hash);
END;
-- items_blob_bu：BEFORE UPDATE OF hash, ext, size_bytes。同じ形で、作った時刻は new.updated_at
-- （M12 の保存はハッシュ・拡張子・サイズを 1 つの UPDATE で変える）

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
  order_reverse      INTEGER NOT NULL DEFAULT 0 CHECK (order_reverse IN (0, 1)),  -- 0006：1 なら並び全体を逆にする（ADR-130）
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
CREATE TABLE jobs (                                -- 0008 で作り直した形
  id            INTEGER PRIMARY KEY,
  kind          TEXT    NOT NULL                    -- 0005：export、0008：analyze（M11 の解析）
                CONSTRAINT jobs_kind CHECK (kind IN ('import','thumbnail','proxy','download','capture','export','analyze')),
  state         TEXT    NOT NULL
                CONSTRAINT jobs_state CHECK (state IN ('queued','running','paused','done','failed','canceled')),
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

-- ===== ダウンロード元とアイテムの対応（M7。取得したファイルごとに 1 行。同じ投稿を取り直すと unit_id のユニットへ足す。ADR-96）=====
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
  last_status       TEXT,                           -- 'ok' / 'failed'（M8）
  pos               REAL    NOT NULL
);

-- ===== 設定 =====
CREATE TABLE settings (
  key    TEXT PRIMARY KEY,
  value  TEXT NOT NULL                              -- JSON
);
-- キー：'playback.support' = プロキシの判定に使った再生可否の表の指紋（文字列。変われば判定をやり直す。M5）
--       'download.defaults' = ダウンロードの前回の選択（画質・音声のみ・保存先・自動タグ・再生リストをユニットに。M7、ADR-98）
--       'analysis.plan'     = 解析の計画の指紋（版・解析 ID ごとの版と引数・形式ごとのデコーダ。変わった分だけ pending に戻す。M11）
--       'thumbs.plan'       = サムネイルの道の指紋（形式ごとの作り方。変わった形式の ready を stale に、failed を pending に戻す。M11）

-- ===== 中身ごとの事実（0009。検索と表示の列。台帳の 1 行に 1 行。解析の結果から純粋な関数で作る）=====
CREATE TABLE media_facts (
  blob_id          INTEGER PRIMARY KEY REFERENCES blobs(id) ON DELETE CASCADE,
  state            TEXT    NOT NULL CONSTRAINT media_facts_state CHECK (state IN ('ok','failed')),
  error_code       TEXT,                   -- failed：'unsupported'・'too_large'・'decode'
  decoder          TEXT,                   -- 読んだデコーダの識別子。形式だけで決めたとき（JPEG・HEIC）は NULL
  reproducible     INTEGER,                -- 0/1
  value_digest     BLOB,                   -- 読んだ値のダイジェスト（32 バイト）
  env              TEXT,                   -- 再現を約束しないデコーダの環境（OS の版など）
  facts_v          INTEGER NOT NULL,       -- 投影の版（上がれば analysis_results から作り直す。読み直さない）
  alpha            INTEGER,                -- 画像：0 なし・1 二値・2 半透明あり。NULL は計っていない
  colors           INTEGER,                -- 1〜257（257 は 256 を超える）
  visible_colors   INTEGER,
  pixel_kx         INTEGER, pixel_ky INTEGER, pixel_lossless INTEGER,
  pixel_art        INTEGER,                -- 自動の判定（0/1。pixel_rule の版の規則で）
  pixel_rule       INTEGER,
  frame_count      INTEGER,
  loudness         REAL,                   -- 音声：積分ラウドネス（LUFS）。計れなければ NULL で loudness_reason
  loudness_reason  TEXT,
  loudness_range   REAL, true_peak REAL, sample_peak REAL,
  has_loop         INTEGER,                -- 0/1
  loop_start       INTEGER, loop_end INTEGER,  -- 標本
  sample_rate      INTEGER, channels INTEGER,
  bits             INTEGER,                -- 非可逆の形式は NULL
  clip_runs        INTEGER,
  font_family      TEXT, glyph_count INTEGER, vertex_count INTEGER, triangle_count INTEGER,
                                           -- フォント・3D（M11 では items.codec_info に入れ、ここは使わない）
  analyzed_at      INTEGER NOT NULL
);
-- 部分索引の WHERE は検索の条件から導ける形にする（比較は IS NOT NULL を導くが、colors <= 16 は colors <= 256 を導かない）
CREATE INDEX idx_media_facts_alpha     ON media_facts(alpha) WHERE alpha IS NOT NULL;
CREATE INDEX idx_media_facts_colors    ON media_facts(colors) WHERE colors IS NOT NULL;
CREATE INDEX idx_media_facts_loudness  ON media_facts(loudness) WHERE loudness IS NOT NULL;
CREATE INDEX idx_media_facts_pixel_art ON media_facts(pixel_art) WHERE pixel_art = 1;
CREATE INDEX idx_media_facts_loop      ON media_facts(has_loop) WHERE has_loop = 1;

-- ===== 解析の結果（0009。正本。中身 × 解析 ID ごとに今の版・正規の引数の 1 行）=====
CREATE TABLE analysis_results (
  id          INTEGER PRIMARY KEY,
  blob_id     INTEGER NOT NULL REFERENCES blobs(id) ON DELETE CASCADE,
  analysis    TEXT    NOT NULL,            -- 'image.alpha' など
  version     INTEGER NOT NULL,
  params      TEXT    NOT NULL,            -- 正規の引数（正規の JSON）
  encoding    TEXT    NOT NULL CONSTRAINT analysis_results_encoding CHECK (encoding IN ('json','binary')),
  output      BLOB,                        -- 結果の正規の形。ファイルに置くもの（波形の段は peaks/）は NULL
  output_len  INTEGER NOT NULL,
  digest      BLOB    NOT NULL,            -- kura-analysis-v1
  created_at  INTEGER NOT NULL,
  UNIQUE (blob_id, analysis)
);

-- ===== 素材の設定（0009。利用者の決定。アイテムごと、変えたアイテムにだけ行がある）=====
CREATE TABLE asset_settings (
  item_id      INTEGER PRIMARY KEY REFERENCES items(id) ON DELETE CASCADE,
  pixel_art    INTEGER CONSTRAINT asset_settings_pixel_art CHECK (pixel_art IN (0, 1)),   -- NULL は自動
  pixel_scale  INTEGER CONSTRAINT asset_settings_pixel_scale CHECK (pixel_scale BETWEEN 1 AND 64),
  grid_json    TEXT,                       -- {"v":1,"cellW","cellH","offsetX","offsetY","gapX","gapY","count"}
  frame_ms     INTEGER CONSTRAINT asset_settings_frame_ms CHECK (frame_ms > 0),
  pivot_x      REAL, pivot_y REAL,
  loop_start   INTEGER, loop_end INTEGER,  -- 試聴のループ（標本）。ファイルは変えない（M11 では使わない。M13）
  anim_tag     TEXT,                       -- aseprite の再生するタグ
  updated_at   INTEGER NOT NULL,
  CONSTRAINT asset_settings_loop CHECK (   -- NULL を含む比較は CHECK を通ってしまうので、片方だけ NULL を明示して断る
    (loop_start IS NULL AND loop_end IS NULL)
    OR (loop_start IS NOT NULL AND loop_end IS NOT NULL AND loop_start >= 0 AND loop_start < loop_end))
);
CREATE INDEX idx_asset_settings_pixel_art ON asset_settings(pixel_art) WHERE pixel_art IS NOT NULL;
CREATE INDEX idx_asset_settings_loop      ON asset_settings(item_id) WHERE loop_start IS NOT NULL;

-- ===== 全文検索（外部コンテンツ FTS5、trigram）=====
CREATE VIRTUAL TABLE items_fts USING fts5(
  title, original_name, note, source_url,
  content = 'items', content_rowid = 'id',
  tokenize = 'trigram'
);
-- items の INSERT / UPDATE / DELETE に合わせて items_fts を更新するトリガー（items_fts_ai / _ad / _au）を同じマイグレーションで作る
-- （0007 で items を作り直したときに作り直した。items_fts は rowid = items.id を保ったのでそのまま）
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

### 台帳で数える（M11）

- 容量：`SELECT coalesce(sum(size_bytes), 0) FROM blobs`（覆う索引 `idx_blobs_size`）。ゴミ箱のものも含む。
- 取り込みの重複の見込み：`SELECT hash, ext FROM blobs WHERE size_bytes = ?`（`idx_blobs_size`）。重複の判定（`find_by_hash`）は
  M11 では items のまま（台帳と items が 1 対 1 なので同じ結果。M12 で台帳を引く形に広げる）。
- 整合性チェック：`files/` にあるはずのファイルは台帳の `(hash, ext)` から作る。どの表からも参照されない台帳の行も数える
  （`unreferencedBlobs`）。
- ゴミ箱を空にする：items の行を消したのと同じトランザクションで、どの表からも参照されなくなった台帳の行を `ledger::release` で
  消す（事実・結果は CASCADE）。コミットのあとで、解放した中身の実ファイルと派生のファイルを消す（ADR-46 の順）。
  取り込み（files/ に置く → DB に書く）とは `Library` の 1 つの鍵で直列にする（置いたばかりの同じ中身を消さないように）。
- 「どの表からも参照されない」は、`blobs` を参照する CASCADE でない外部キーをすべて見る（`ledger::REFERRERS`。表を足すと
  テスト `release_sees_every_table_that_references_the_ledger` が落ちる。M12 の版・アンカーの行を消さないため）。
- ダウンロード・キャプチャで、同じ中身を別の拡張子で取ったときに置いたコピーは、書き込みのトランザクションの中で台帳の拡張子と
  比べ、違う拡張子のファイルだけを消す。

### 事実で引く（M11）

- 事実の条件は `i.hash IN (SELECT b.hash FROM media_facts f JOIN blobs b ON b.id = f.blob_id WHERE …)` の形にする
  （部分索引から集合を作る。EXISTS の相関した問い合わせにしない）。
- 実効値は「設定 ?? 自動」：ドット絵は `asset_settings.pixel_art` があればそれ、なければ `media_facts.pixel_art`。
- 2 のべき乗は列を持たず、`width & (width - 1) = 0` のビット演算で求める。
- まだ解析していないもの・計れない値は、肯定・`!=`・大小の比較のどれにも入れない（除外だけが含む。docs/search-query.md §4）。

### ユニットの集計

set ユニットの件数・合計時間・種類は表示のたびに集計してよい（数百件程度）。遅くなったら `units` に集計列を追加し、トリガーで更新する。

### 遅くなったら

10 万件超でタグ検索が 50ms を超えるようになったら、起動時に「タグ ID → アイテム ID の Roaring Bitmap」をメモリに作り、集合演算で絞り込む方式を追加する（DB はそのまま正本）。

## 5. マイグレーションの当て方（M11。`crates/kura-core/src/db/migrate.rs`）

`Library::open` が `Db::open_with` で当てる。版は `PRAGMA user_version`（当てたマイグレーションの数）。

1. **DB の方が新しい**（版がこの Kura の知るより大きい）なら、当てずに開くのを失敗にする。
2. **当てる前のバックアップ**：未適用のマイグレーションがある既存の DB（版 1 以上）では、書き込みの接続から backup API で
   `backups/library-YYYYMMDD-HHMMSS.sqlite`（いつもの名前・DELETE モード）へ一度に写し、7 個の保持に乗せる。
   失敗したら（空き容量など）当てずに開くのを失敗にする。新しいライブラリ（版 0）では作らない。
3. **当てる**：1 つずつ、それぞれ 1 つのトランザクションで当てる。
   - `Plain`：そのまま当てる。
   - `Rebuild`（ほかの表から参照される表の作り直し。0007）：トランザクションの中からは断る。トランザクションの外で `foreign_keys` を
     切り（読み直して確かめる）、`BEGIN IMMEDIATE` → SQL → `PRAGMA foreign_key_check` → 違反がなければ版を上げてコミット。
     違反があるときだけいったん戻し、当てる前からあった違反と比べ（同じ行の数も比べる）、新しい違反があれば当てない。
     前からの違反だけなら当て直して警告を残す。成功しても失敗しても `foreign_keys` を戻す。事前の確かめはしない（10 万件で約 160ms を省く）。
   - 作り直しの SQL は「新しい表を作る → 移す → 古い表を DROP → 新しい表を RENAME」。古い表を先に改名しない。
4. **WAL を空にする**：1 つでも当てたら `PRAGMA wal_checkpoint(TRUNCATE)`（しないと次に開くときに WAL を読み直す）。
5. 当てられなかったら、版は前のまま（失敗したマイグレーションのトランザクションは戻る。それより前に当てたものは残る）。
   理由は `CoreError::Upgrade`（新しい版・空き容量・バックアップ・当てる失敗）の言葉で返す。開く前に当てるかどうかは
   `library::needs_upgrade` で分かる（「ライブラリを更新しています」を出すのに使う）。
6. アプリの中では `quick_check`・FTS の照合・VACUUM をしない。照合はテストと `kura-devtools bench migrate` で行う。

**10 万件の時間**（2026-10-20、M4 Max、同梱の SQLite 3.53、ページキャッシュは温まった状態、ほかの作業でロードアベレージ 9〜12。
`bench migrate --runs 3`。開くところから数えた時間）：

| 元の DB | バックアップ | 0007 | 0008 | 0009 | TRUNCATE | 開くまでの合計 | 当てたあと | 2 回目に開く |
|---|---|---|---|---|---|---|---|---|
| seed 10 万件（スキーマ 6、51.8MB ＋ WAL 10MB） | 47〜49ms | 711〜738ms | 1.1〜1.2ms | 0.2〜0.3ms | 0.8〜2.6ms | 764〜792ms | DB 85.9MB・WAL 0 | 1.9〜2.2ms |
| 合成 10 万件 `--history`（再生の記録 20 万・エントリ 1 万・取得元 1 万、60.1MB） | 52〜62ms | 751〜779ms | 1.0〜1.1ms | 0.3ms | 0.6〜0.7ms | 807〜839ms | DB 95.9MB・WAL 0 | 1.8〜2.0ms |

どちらも、すべての表の行数が当てる前と同じ、`foreign_key_check` が空、FTS の照合（rank=1）と `quick_check` が ok、台帳と items が
1 対 1（10 万行）だった。更新後の初回の起動にだけ載る一度きりの作業で、NFR-01 の外として別に記録する（2 回目以降は 1.5 秒を保つ）。
