# データモデル

ライブラリは 1 つのフォルダ（`<名前>.kuralib`）で完結します。DB は SQLite 1 ファイル。
この文書のスキーマが正本です。変更は `.claude/skills/add-migration` の手順で行い、ここも更新します。

## 1. ライブラリのフォルダ構成

```text
Kura Library.kuralib/
  library.sqlite            # DB（-wal, -shm も同居）
  library.lock              # ライブラリのロック（中身は空。開いている間 flock を持つ。2 つ目のアプリは開けない。M12。下の注）
  files/ab/cd/<blake3>.<ext>    # 実ファイル（内容アドレス。ハッシュ先頭 2 文字 / 次の 2 文字）。1 つに台帳 blobs の 1 行。アイテムの今の
                                #   ほか、M12 から編集したアイテムの元・猶予中の版（置き換えから 7 日。段 3 の GC が消す）・アンカー
                                #   （再現を約束しないデコーダで読んだ元の読み取り結果。PNG・APNG・f32 の WAV）も置く
  thumbs/ab/<blake3>_s.{jpg,png}  # サムネイル 長辺 512px（グリッド用）。透明のないものは JPEG、透明のあるものは PNG（M11）
  thumbs/ab/<blake3>_l.{jpg,png}  # サムネイル 長辺 1600px（インスペクタ・プレビュー用）
  art/ab/<blake3>.jpg           # 音楽のジャケット画像
  proxies/ab/<blake3>.mp4       # 再生用プロキシ（WKWebView で再生できない形式だけ）
  peaks/ab/<blake3>.peaks       # 音声の波形の段（kura-peaks 版 1。解析のジョブが staging/peaks-<blake3>.part に書いてから改名する。M11）
  anim/ab/<blake3>-1.png        # アニメの帯（見たときにその場で作る。末尾は帯の並べ方の版 ANIM_V。tEXt `kura-anim` に帯の情報と作った元の鍵。
                                #   staging/anim-<pid>-<連番>.png.part に書いてから改名する。M11）
  staging/                      # 取り込み・ダウンロード・キャプチャの一時置き場。M11 から、波形の段・アニメの帯の書きかけと、
                                #   エンジンの ffmpeg の段の一時ファイル（staging/engine/）も置く。M12 から、保存のジョブの書きかけ
                                #   （staging/edit-<job>-out.<ext>.part・staging/edit-<job>-anchor.<png|wav>.part。sync_all してから
                                #   files/ へ同じボリュームの中で改名する）も置く。開くときに消す
  archives/                     # yt-dlp / gallery-dl の download-archive
  logs/                         # ジョブのログ
  backups/library-YYYYMMDD-HHMMSS.sqlite  # DB のバックアップ（UTC。新しい 7 個。FR-SYS-03・ADR-121。マイグレーションの前のものも同じ名前。§5）
```

- `files/` のファイル名はハッシュなので、元のファイル名は DB（`original_name`）にだけ残る。
- `thumbs/`・`art/`・`proxies/`・`peaks/`・`anim/` は中身から作った派生のファイル（`Layout::derived_paths`：サムネイルは jpg・png の
  両方の名前、ジャケット、プロキシ、波形の段、今の版のアニメの帯）。台帳の行を解放したとき（ゴミ箱を空にしたとき）に、実ファイルと
  一緒に消す。派生のファイルは整合性チェックの対象の外。
- サムネイルは 1 つの中身に 1 つの形式だけを置く（作り直して形式が変わったら、ほかの形式の古いファイルを消す）。書くたびに
  `items.thumb_rev` を 1 上げ、URL の `?r=` にする（作り直すと同じ名前のファイルを書き換えるため。docs/architecture.md §4）。
- `thumbs/`（`art/` を含む）・`peaks/`・`anim/` の大きさは、設定の容量にライブラリのファイルとは別の行で出す（ファイルを数える。
  ゴミ箱のアイテムの分も含む。`media::usage`）。
- 既定の場所は初回起動時にユーザーが選ぶ（候補：`~/Pictures/Kura Library.kuralib`）。外付けディスクも可。
- iCloud Drive など同期フォルダの中は非推奨（SQLite の WAL と相性が悪い）。選ばれたら警告する。
- マイグレーションを当てる前にも、いつもの名前でバックアップを作る（7 個の保持に乗る。§5）。
- **ライブラリのロック**（`library::LibraryLock`。M12、ADR-156 (6)・M11 の決定 22）：開くときは、何より先に（足りないフォルダを
  作り直す・DB を開く・当てる前のバックアップ・マイグレーション・staging の掃除・更新の知らせより前に）`library.lock` を開き
  （なければ作り）、排他のロックを取る（std の `File::try_lock`。macOS・Linux とも `flock(LOCK_EX | LOCK_NB)`）。ほかのアプリ
  （Kura を 2 つ動かした・開発版と本番版・`kura-devtools`）が持っていれば `CoreError::LibraryInUse`（アプリの code も
  `LibraryInUse`）で断り、ライブラリには何もしない。新しいライブラリは根のフォルダを作ってから取る。DB のファイル自体には
  掛けない（SQLite の fcntl のロックと干渉しうる）。flock は開いたファイルごとなので、同じプロセスでも別々に開けばぶつかる。
  閉じるとき（`LibrarySession::close` の最後の `Library::close`）は、DB を閉じてから明示的に放す（IPC が `Arc` で握った
  ライブラリが残っても、開き直しが自分のロックに当たらない）。プロセスが落ちれば OS が放す。std はファイルを `O_CLOEXEC` で
  開くので、ジョブが起動する子プロセス（ffmpeg・SVG の働き手など）には継がれない。ロックファイルは消さない（消すと、消す前に
  開いた側と作り直した側が別々のファイルでロックを取れてしまう）。flock が使えないファイルシステム（ENOTSUP・ENOLCK など）
  では、警告をログに残してロックなしで開く（そのときは §5 の 3 の読み直しだけが守る）。
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
  `WHERE id = ? AND hash = ?` のようにハッシュを条件にする（読んだあとに中身が変わっていたら書かない）。M11 で解析
  （`analysis::store::write`）とサムネイル（`media::job` の結果の書き込み）に入れた。M12 でプロキシ（`playback::proxy` の判定と
  作った結果の書き込み）にも入れた。

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
| `0010_items_blob_id` | `items.blob_id`（台帳の行の ID の写し。CHECK・外部キーなしの `ADD COLUMN`）を足して hash から埋め、`idx_items_blob` と、入れる・hash を変えるときに台帳から引いて入れる AFTER のトリガ `items_blob_id_ai`・`items_blob_id_au` を作る（M12。検索の事実の条件を整数の鍵で引く。M11 の決定 17。10 万件で約 0.2 秒） |
| `0011_edit_versions` | 保存と版（M12 の段 2。docs/editing.md §4.4・§9.4・§14）：文書の部品 `edit_objects`・`edit_edges`、版 `item_versions`、版の入力 `item_version_inputs`、アンカー `edit_anchors`、来歴 `item_derivations`、下書き `edit_drafts`、台帳の予告 `ledger_pending` を足し、`jobs_kind` に `edit`・`gc` を足す（名前付きの CHECK の入れ替え）。新しい表だけなので Plain。版 0（元）は最初の保存のときに足すので、既存のアイテムには行を足さない（10 万件でも表を作るだけ。合成 10 万件 `--history` で 1.18〜1.21 ms。`bench migrate --runs 3`、2026-10-24）。台帳を参照する列（`item_versions.file_blob_id`・`edit_anchors.blob_id`）は ON DELETE なし（`ledger::REFERRERS` に足した）、items を参照する列は CASCADE（来歴の元だけ SET NULL） |

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
  codec_info      TEXT,                             -- 要約 JSON（下の「codec_info の形」）
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
  blob_id         INTEGER,                          -- 台帳の行の ID（blobs.id）の写し（0010。M12）。下のトリガが hash から引いて入れる。
                                                    -- CHECK・外部キーなし（正は hash の外部キー）。事実・解析の結果を整数の鍵で引くのに使う
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
CREATE INDEX idx_items_blob ON items(blob_id); -- 0010：事実の集合（台帳の行の ID）からアイテムを引く
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

-- blob_id の写し（0010）。BEFORE のトリガは NEW を書き換えられないので AFTER で入れる（台帳への登録は上の BEFORE が先に済ませる）。
-- 書き手は blob_id を書かない。食い違いは設定の「いま確かめる」（`library_check`）で直す（items の全件の走査なので起動ごとには走らせない）
CREATE TRIGGER items_blob_id_ai AFTER INSERT ON items BEGIN
  UPDATE items SET blob_id = (SELECT b.id FROM blobs b WHERE b.hash = new.hash) WHERE id = new.id;
END;
-- items_blob_id_au：AFTER UPDATE OF hash。同じ形

-- codec_info の形（JSON。`media::MediaInfo::codec_json`。どれもなければ NULL。読み手は知らない欄を無視する）：
--   画像・動画・音楽：{"codec": 主なコーデック}（例 'h264'・'aac'・'jpeg'）。プロキシの判定後は {"codec", "video", "audio"}
--   フォント（M11）：{"codec": 形式（'ttf'・'otf'・'ttc'・'woff'・'woff2'）, "font": {"family", "style", "fullName",
--     "postscriptName", "glyphs"（字の数）, "faces"（ファイルの中の書体の数。コレクションは 2 以上）, "variable"}}
--     （取り込みの probe が kura-mac の CoreText で読む。読めなかったものはサムネイルのジョブが読み直す）
--   3D（M11）：{"codec": "glb", "model": {"vertices", "triangles", "meshes", "materials", "textures", "animations", "skins",
--     "nodes", "size"（範囲の max − min。[x, y, z]。メートル。数えられなければ null）, "generator", "extensionsRequired",
--     "unsupported"（描けない理由：必須の Draco・meshopt・知らない拡張の名前と 'skin'。空なら描ける）}}
--     （サムネイルのジョブが kura-engine の glb の読み手で読む。docs/editing.md §9.10）
--   フォント・3D の幅・高さ・長さは空。サムネイルのジョブは codec_info が空のときだけ書く（`coalesce(codec_info, ?)`）

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
  kind          TEXT    NOT NULL                    -- 0005：export、0008：analyze（M11 の解析）、0011：edit（M12 の保存）・gc（M12 の段 3）
                CONSTRAINT jobs_kind CHECK (kind IN ('import','thumbnail','proxy','download','capture','export','analyze','edit','gc')),
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
--       'analysis.plan'     = 解析の計画の指紋（計画・投影・ドット絵の規則・読み取りの記録の版と、種類ごとの解析 ID の版と既定の
--                             引数の正規の形、形式ごとの扱い（読む以外のもの：formats = {"jpeg":"fromFormat","heic":"byHeader",
--                             "heif":"byHeader"}。決定 15）。起動のスレッドで比べ、解析の版・計画が変わった種類と、扱いが変わった
--                             形式だけ pending に戻す。formats のない前の指紋は JPEG・HEIC・HEIF をすべて fromFormat と読む。M11）
--       'analysis.reproject'= 投影の作り直しの印（true。'analysis.plan' の投影の版・ドット絵の規則が変わったときに起動のスレッドが
--                             置き、解析のジョブが投影の古い事実の行を結果の表から作り直し終えたら消す。M11 の検証 D3）
--       'analysis.decoders' = 解析で読む形式（見出しで決める HEIC・HEIF を含む）ごとの、段の順のデコーダの識別子（解析のジョブが
--                             コーデックの組を作ったときに比べ、今の組にないデコーダで読んだものと、並びが変わった形式で失敗して
--                             いたもの・読まずに形式から書いたもの（decoder が NULL）を pending に戻す。M11）
--       'analysis.inflight' = 解析のジョブが OS のデコーダ（Platform の段）を通りうる形式を読んでいるアイテム（{"id","hash"}。
--                             1 件ずつ解くので 1 件だけ。コーデックの組にその形式の Platform のデコーダがあれば、どの段が読むかに
--                             よらず読む前（HEIC・HEIF の見出しを読む前も）にコミットし、書いたら・落ちずに失敗したら消す。アプリの
--                             組では HEIC・HEIF・AVIF・TIFF・BMP と、固定の段が先に読む PSD・WAV・AIFF・Ogg・Opus・m4a・AAC。
--                             PNG・GIF・WebP・aseprite・SVG・MP3・FLAC には付けない。
--                             解析のジョブが次に動いたときに残っていれば、そのアイテムを failed（crashed）にする。M11）
--       'thumbs.plan'       = サムネイルの道の指紋（{"v":1,"formats":{形式の表の ID: 作り方の名前}}。作り方の名前は
--                             'engine-thumbs/1'・'imageio-alpha/1'・'imageio/1'（jpeg）・'video/1'・'audio/1'・'coretext/1'・
--                             'model-kura-render/1'。開くたびに比べ、変わった形式の ready を stale に、failed を pending に戻す。
--                             指紋のないライブラリ（M10 まで）は「画像はすべて ImageIO の JPEG、フォント・3D は作らない」と比べる。M11）
--       'thumbs.inflight'   = サムネイルを作っている途中のアイテム（{"solo": 1 件だけか, "ids": [ID…]}。まとめて作る 64 件か、
--                             疑わしいものの 1 件。作る前にコミットし、結果を書くトランザクションで消す。並べて作るので、まとめての
--                             印ではどれで落ちたかは分からない。M11）
--       'thumbs.suspects'   = 前回まとめて作っている途中で終わったときのアイテム（[ID…]）。次に開いたときに 'thumbs.inflight' から
--                             移し、サムネイルのジョブが先に 1 件ずつ作る。1 件だけの印が残っていれば、そのアイテムで落ちたとみなし、
--                             pending は failed に、stale は ready（前の絵のまま）に戻す（同じファイルで落ちるのは多くても 2 回。M11）
--       'probe.inflight'    = 取り込み・ダウンロード・キャプチャがメディア情報（OS のデコーダ）を読んでいる途中のファイル
--                             （[{"job": ジョブ ID, "solo": 1 件だけか, "paths": [取り込み元のパス…]}]。ジョブごと。バッチの前に
--                             コミットし、結果を書くトランザクションで消す。M11 の検証 S2）
--       'probe.suspects'    = 前回まとめて読んでいる途中で終わったときのファイル（[取り込み元のパス…]。4,096 件まで）。次に開いた
--                             ときに 'probe.inflight' から移し、次の取り込みなどで 1 件ずつ・1 件だけの印で読む。入れたら外す
--       'probe.crashed'     = 前回 1 件だけで読んでいる途中で終わったファイル（[取り込み元のパス…]。4,096 件まで）。もう読まずに
--                             入れ、そのアイテムのサムネイルを failed にする（pending のときだけ）。入れたら外す
--       'gc.parts'          = 文書の部品が孤立したかもしれない印（true。下書きを置き換えた・捨てた、上書き・別名の保存のジョブが
--                             版を足さずに終わった（待ち・一時停止のまま中止されたときも）、版 0 に戻した保存が空の文書の下書きを
--                             消したときに置く。あれば消し待ちの版がなくても GC を積み、GC が部品を片付けたトランザクションで、
--                             終わっていない保存のジョブも 7 日以内に失敗・中止した保存のジョブもなければ消す。M12 の段 3。
--                             §4「保存と版（M12）」）

-- ===== 中身ごとの事実（0009。検索と表示の列。台帳の 1 行に 1 行。解析の結果から純粋な関数で作る）=====
CREATE TABLE media_facts (
  blob_id          INTEGER PRIMARY KEY REFERENCES blobs(id) ON DELETE CASCADE,
  state            TEXT    NOT NULL CONSTRAINT media_facts_state CHECK (state IN ('ok','failed')),
  error_code       TEXT,                   -- failed：'unsupported'・'too_large'・'decode'・'crashed'（OS のデコーダを通りうる形式を読んでいる途中で終わった。固定の段が読んでいても）。一覧は analysis::store::error_code::ALL（bindings の ANALYSIS_ERROR_CODES）
  decoder          TEXT,                   -- 読んだデコーダの識別子。形式だけで決めたとき（JPEG と、アルファのない HEIC・HEIF）は NULL
  reproducible     INTEGER,                -- 0/1
  value_digest     BLOB,                   -- 読んだ値のダイジェスト（32 バイト）
  env              TEXT,                   -- 再現を約束しないデコーダの環境（OS の版など）
  facts_v          INTEGER NOT NULL,       -- 投影の版（上がれば analysis_results から作り直す。読み直さない）
  alpha            INTEGER,                -- 画像：0 なし・1 二値（透明か不透明だけ）・2 半透明あり。NULL は計っていない
  colors           INTEGER,                -- 違う色の数 1〜257（257 は「257 色以上」。アルファ 0 の色も RGB が違えば別）
  visible_colors   INTEGER,                -- アルファ 0 の色を 1 つにまとめた数（1〜257。ドット絵の判定に使う）
  pixel_kx         INTEGER, pixel_ky INTEGER, pixel_lossless INTEGER,  -- 軸ごとの倍率 k と、可逆か（image.pixel_scale）
  pixel_art        INTEGER,                -- 自動の判定（0/1。pixel_rule の版の規則で。材料が足りなければ NULL）
  pixel_rule       INTEGER,                -- ドット絵の判定の版（今は 1。上がれば読み直さずに作り直す）
  frame_count      INTEGER,                -- コマの数（1 枚の画像は 1。アニメの画像は 2 以上）
  -- JPEG は読まずに形式から書く（決定 5）：alpha 0・colors と visible_colors は NULL（計らない）・pixel_art 0・
  -- frame_count 1（静止画として扱う）・decoder NULL。HEIC・HEIF は見出しでアルファを持つものだけを読んで解析し、ほか
  -- （と見出しを読めないもの）は JPEG と同じく形式から書く（決定 15。HEIC の連続した画像も先頭だけ）
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
  analysis    TEXT    NOT NULL,            -- 'image.alpha' など。'kura.read' は kura-core の読み取りの記録（値の種類・大きさ・長さ）
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
-- 付けるのは画像だけ（`asset::set_settings` がほかの種類の ID を飛ばす）。すべての欄が自動（NULL）に戻れば行を消す。
CREATE TABLE asset_settings (
  item_id      INTEGER PRIMARY KEY REFERENCES items(id) ON DELETE CASCADE,
  pixel_art    INTEGER CONSTRAINT asset_settings_pixel_art CHECK (pixel_art IN (0, 1)),   -- NULL は自動
  pixel_scale  INTEGER CONSTRAINT asset_settings_pixel_scale CHECK (pixel_scale BETWEEN 1 AND 64),
  grid_json    TEXT,                       -- スプライトシートの格子（版 1）：{"v":1,"cellW","cellH","offsetX","offsetY","gapX","gapY",
                                           -- "count"}（画素の整数。count は使うコマの数で、null はすべて。セルの切り方は
                                           -- sprite.split_grid と同じ）。知らない版・知らない欄・壊れたものは格子なしとして読む
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

-- ===== 文書の部品（0011。M12 の段 2。docs/editing.md §4.4 の doc・node・chunk。同じ中身の部品は 1 行）=====
CREATE TABLE edit_objects (
  id          INTEGER PRIMARY KEY,
  digest      BLOB    NOT NULL UNIQUE,               -- 部品のダイジェスト（ドメイン付き BLAKE3。本文から求め直して照らす）
  kind        TEXT    NOT NULL CONSTRAINT edit_objects_kind CHECK (kind IN ('doc','node','chunk')),
  body        BLOB    NOT NULL,                      -- 正規の JSON（UTF-8）。子は 16 進のダイジェストで書く
  created_at  INTEGER NOT NULL
);
CREATE TABLE edit_edges (                          -- 親の部品 → 子の部品（本文に書いた順。同じ子が 2 度出てもよい）
  parent_id   INTEGER NOT NULL REFERENCES edit_objects(id) ON DELETE CASCADE,
  ord         INTEGER NOT NULL CONSTRAINT edit_edges_ord CHECK (ord >= 0),
  child_id    INTEGER NOT NULL REFERENCES edit_objects(id),   -- 参照されている部品は消せない（片付けは段 3 の GC。親から順に消す）
  PRIMARY KEY (parent_id, ord)
) WITHOUT ROWID;
CREATE INDEX idx_edit_edges_child ON edit_edges(child_id);

-- ===== アンカー（0011。元のファイル × 読み方の設定ごとに 1 行。docs/editing.md §9.4）=====
CREATE TABLE edit_anchors (
  id              INTEGER PRIMARY KEY,
  source_hash     BLOB    NOT NULL,                  -- 元のファイルの中身
  options_digest  BLOB    NOT NULL,                  -- 読み方の設定（DecodeOptions の正規の JSON）のダイジェスト
  options         TEXT    NOT NULL,
  decoded         TEXT    NOT NULL,                  -- 元を読んだ記録（DecodeStamp の正規の JSON）
  value_digest    BLOB    NOT NULL,
  blob_id         INTEGER NOT NULL REFERENCES blobs(id),       -- アンカーのファイル（ON DELETE なし。REFERRERS）
  created_at      INTEGER NOT NULL,
  UNIQUE (source_hash, options_digest)
);
CREATE INDEX idx_edit_anchors_blob ON edit_anchors(blob_id);

-- ===== 版（0011。アイテムごとに 0..n。版 0＝元は最初の保存のときに足す。行がなければ「版 0 は暗に今、頭は 0」）=====
CREATE TABLE item_versions (
  id            INTEGER PRIMARY KEY,
  item_id       INTEGER NOT NULL REFERENCES items(id) ON DELETE CASCADE,
  version       INTEGER NOT NULL CONSTRAINT item_versions_version CHECK (version >= 0),
  kind          TEXT    NOT NULL CONSTRAINT item_versions_kind CHECK (kind IN ('original','edit','revert')),
  doc_id        INTEGER REFERENCES edit_objects(id),  -- 文書の根の部品。版 0 と、版 0 に戻した版は NULL（空の文書）
  base_version  INTEGER,                              -- 保存したときの土台の版
  revert_to     INTEGER,                              -- 戻した先の版（revert だけ）
  output_hash   BLOB    NOT NULL,                     -- 出力の中身（items.hash と同じ生の BLAKE3）
  ext           TEXT    NOT NULL,
  size_bytes    INTEGER NOT NULL CONSTRAINT item_versions_size CHECK (size_bytes >= 0),
  format        TEXT,
  value_digest  BLOB,                                 -- 描いた値のダイジェスト（作り直しの照合）
  width         INTEGER,
  height        INTEGER,
  duration_ms   INTEGER,
  record        TEXT    NOT NULL,                     -- VersionRecord（JSON。エンジンの版・エンコーダ・読み取りの記録・メディア情報・警告）
  file_blob_id  INTEGER REFERENCES blobs(id),         -- この版が持つファイル（released なら NULL。ON DELETE なし。REFERRERS）
  file_state    TEXT    NOT NULL CONSTRAINT item_versions_file_state
                CHECK (file_state IN ('hold','waiting','kept','released')),
                -- hold：今か元（消さない）／waiting：頭でなくなった（7 日と、置き換えより前のバックアップの条件で段 3 の GC が消す）／
                -- kept：GC の照合が合わず残す（もう GC の候補にならない）／released：もうファイルを持たない
  replaced_at   INTEGER,                              -- 頭でなくなった時刻（hold のときだけ NULL）
  job_id        INTEGER,                              -- 頭になった行を作った保存のジョブ（再開で版を 2 つ足さない）
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
CREATE INDEX idx_item_versions_output    ON item_versions(output_hash);       -- 重複の判定（find_owner）
CREATE INDEX idx_item_versions_file_blob ON item_versions(file_blob_id);      -- 台帳の行が参照されているか（REFERRERS）
CREATE INDEX idx_item_versions_doc       ON item_versions(doc_id);
CREATE INDEX idx_item_versions_waiting   ON item_versions(replaced_at) WHERE file_state = 'waiting';   -- 段 3 の GC の候補
CREATE UNIQUE INDEX idx_item_versions_job ON item_versions(job_id) WHERE job_id IS NOT NULL;
-- 不変条件（`edit::versions::check_invariants`。§4「保存と版（M12）」の「不変条件」に全部）：版の行は 0 個か 2 個以上で 0..n に欠けなく並ぶ。
-- 版 0 と頭は hold（ほかの版は hold でない）。頭の output_hash・ext は items.hash・ext に等しい。hold・waiting・kept の file_blob_id が指す
-- 台帳の行の hash は output_hash と同じ。土台は 1 つ前の版。edit は入力 0 を持ち、revert は戻した先と文書・値・入力の数が同じ

-- ===== 版の入力（0011。版ごとの入力の記録）=====
CREATE TABLE item_version_inputs (
  version_id    INTEGER NOT NULL REFERENCES item_versions(id) ON DELETE CASCADE,
  input         INTEGER NOT NULL CONSTRAINT item_version_inputs_input CHECK (input >= 0),
  role          TEXT    NOT NULL CONSTRAINT item_version_inputs_role CHECK (role IN ('primary','fixed','selection')),
  source_hash   BLOB    NOT NULL,
  value_digest  BLOB    NOT NULL,
  recorded      TEXT    NOT NULL,                    -- RecordedInput の正規の JSON
  anchor_id     INTEGER REFERENCES edit_anchors(id),  -- 読んだアンカー（あれば）
  PRIMARY KEY (version_id, input)
) WITHOUT ROWID;
CREATE INDEX idx_item_version_inputs_anchor ON item_version_inputs(anchor_id);

-- ===== 来歴（0011。別名で保存したアイテムが、どのアイテムのどの版から作られたか）=====
CREATE TABLE item_derivations (
  item_id         INTEGER PRIMARY KEY REFERENCES items(id) ON DELETE CASCADE,
  source_item_id  INTEGER REFERENCES items(id) ON DELETE SET NULL,
  source_version  INTEGER NOT NULL,
  source_hash     BLOB    NOT NULL,
  created_at      INTEGER NOT NULL
);
CREATE INDEX idx_item_derivations_source ON item_derivations(source_item_id);

-- ===== 下書き（0011。アイテムごとに保存していない文書 1 つ。docs/editing.md §12.12）=====
CREATE TABLE edit_drafts (
  item_id       INTEGER PRIMARY KEY REFERENCES items(id) ON DELETE CASCADE,
  doc_id        INTEGER NOT NULL REFERENCES edit_objects(id),
  base_hash     BLOB    NOT NULL,                    -- 編集を始めたときの今
  base_version  INTEGER NOT NULL,
  after_job     INTEGER,                             -- 土台を待っている保存のジョブ（コミットすると土台がその版に付け替わる）
  updated_at    INTEGER NOT NULL
);
CREATE INDEX idx_edit_drafts_doc ON edit_drafts(doc_id);

-- ===== 台帳の予告（0011。台帳にまだない中身を files/ に置く・台帳から外した中身を消す前に書く。§4「保存と版（M12）」）=====
CREATE TABLE ledger_pending (                      -- 索引なし（実行中の保存・GC の分だけの小さな表。§4 の例外）
  id          INTEGER PRIMARY KEY,
  hash        BLOB    NOT NULL,
  ext         TEXT    NOT NULL,
  kind        TEXT    NOT NULL CONSTRAINT ledger_pending_kind CHECK (kind IN ('place','delete')),
  job_id      INTEGER,
  created_at  INTEGER NOT NULL
);

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
   - M11：タイルの素材の表示（市松模様・ドット絵・アニメとループの印）のため、`items i LEFT JOIN media_facts f ON f.blob_id = i.blob_id
     LEFT JOIN asset_settings s ON s.item_id = i.id WHERE i.id IN (…)` で引く（主キーだけを引き、200 件でも全件を走査しない。
     `summaries_join_facts_and_settings_without_scanning`。M11 は台帳を `blobs.hash` の UNIQUE で通っていたが、M12 の 0010 から
     `items.blob_id` で直に引く）。
     ドット絵の実効値は「設定 ?? 自動 ?? 偽」（素材の欄と同じ関数 `asset::effective_pixel_art`。表示のため、どちらもないものは偽）。
     検索は「設定 ?? 自動」で比べ、どちらもないものは `ドット絵:なし`・`ドット絵!=あり` に入れず、除外にだけ入れる（下の「事実で引く」、
     docs/search-query.md §4。`ドット絵:あり` の集合は表示の実効値が真のものと同じ）。アニメの実効値は「格子の設定 ?? 解析のコマが
     2 つ以上」（`asset::effective_animated`。タイルの印）で、検索の `アニメ:あり` の集合と同じ（下の「事実で引く」）。ユニットの表紙も
     同じ形で 1 件引く。

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
- 取り込みの重複の見込み：`SELECT hash, ext FROM blobs WHERE size_bytes = ?`（`idx_blobs_size`）。重複の判定は M12 から
  `import::store::find_owner`（アイテムの今 → 元 → 過去の版。下の「保存と版（M12）」。M11 は items の `find_by_hash` だけ）。
- 整合性チェック：`files/` にあるはずのファイルは台帳の `(hash, ext)` から作る。どの表からも参照されない台帳の行も数える
  （`unreferencedBlobs`）。起動時と設定の「いま確かめる」は同じ `library::check_integrity` で、いま確かめるとき（`Full`）だけ
  `items.blob_id` が hash の台帳の行と食い違うアイテムを直し（`repair_blob_ids`。M12）、GC が残した版・7 日を過ぎた消し待ちの版を
  数える（M12 の段 3。下の「保存と版（M12）」の「整合性の数」）。
- ゴミ箱を空にする：items の行を消したのと同じトランザクションで、どの表からも参照されなくなった台帳の行を `ledger::release` で
  消す（事実・結果は CASCADE）。編集したアイテム（M12）は、版の行が持つファイルとアンカーも解放し、文書の部品も片付ける。消しうる
  中身は先に 'delete' で予告してコミットし、コミットのあとで解放した中身の実ファイルと派生のファイルを消してから予告を消す（M12 の
  段 3。手順は下の「保存と版（M12）」の「ゴミ箱を空にする処理」。落ちても開くときの予告の照合が片付ける）。
  取り込み（files/ に置く → DB に書く）とは `Library` の 1 つの鍵で直列にする（置いたばかりの同じ中身を消さないように。
  ゴミ箱を空にする処理は書きの側 `Library::ingest_exclusive`、取り込みは読みの側 `ingest_guard`）。
- 「どの表からも参照されない」は、`blobs` を参照する CASCADE でない外部キーをすべて見る（`ledger::REFERRERS`：`items.hash`・
  `item_versions.file_blob_id`・`edit_anchors.blob_id`（M12）。3 つの NOT EXISTS はそれぞれ `sqlite_autoindex_items_1`・
  `idx_item_versions_file_blob`・`idx_edit_anchors_blob` で引く。表を足すとテスト `release_sees_every_table_that_references_the_ledger`
  が落ちる）。
- 取り込み・ダウンロード・キャプチャで置いたコピー（`files/<hash>.<取り込んだ拡張子>`）は、書き込みのトランザクションの中で台帳と
  比べ、台帳にその中身が同じ拡張子で載っているときだけ残す。違う拡張子のコピーと、持ち主（今・元・過去の版）がいるのに台帳に
  行がない（消えた過去の版と同じ中身の）コピーは消す（取り込みはコミットのあと、ダウンロード・キャプチャはトランザクションの
  中で）。持ち主がいなくても台帳に行がある中身（アンカーのファイルなど）は、台帳の拡張子で新しいアイテムを入れる（items の
  トリガ `items_blob_mismatch` が拡張子の食い違いを断るため）。

### 保存と版（M12。`crates/kura-core/src/edit/`・`library/ledger.rs`・`import/store.rs`）

段 2 の A（重複の判定・台帳の門・予告と照合・文書の部品・ゴミ箱を空にする処理）、B（版の読み書き・アンカー・保存のジョブの上書きと別名）、
C（版に戻す・下書き・不変条件）と、段 3（GC・ゴミ箱を空にする処理の作り直し。`edit::gc`）で入れたもの。

- **重複の判定**（`import::store::find_owner`。取り込み・ダウンロード・キャプチャ）：今 → 元 → 過去の版の順に、2 段の問い合わせで
  探す（どちらも索引。テスト `find_owner_looks_at_current_then_original_then_past_versions` の EXPLAIN）。

  ```sql
  SELECT id, ext, trashed_at IS NOT NULL FROM items WHERE hash = ?1;                     -- sqlite_autoindex_items_1
  SELECT v.item_id, i.ext, i.trashed_at IS NOT NULL, v.version
    FROM item_versions v JOIN items i ON i.id = v.item_id
   WHERE v.output_hash = ?1                                                              -- idx_item_versions_output
   ORDER BY v.version <> 0, v.item_id, v.version DESC LIMIT 1;
  ```

  持ち主が見つかれば（どの役割でも）そのアイテムの重複（所属・タグを足し、ゴミ箱から戻す）。置いたコピーの扱いは上の「台帳で数える」。
- **台帳の門**：台帳にまだない中身を `files/` に置いて台帳に載せる処理（保存の「予告 → 配置 → コミット → 後始末」）と、台帳から外して
  ファイルを消す処理（ゴミ箱を空にする・GC）は、取り込みの鍵の書きの側（`Library::ingest_exclusive`）を持つ。取り込み・
  ダウンロード・キャプチャは「置いてから DB に書く」まで読みの側を持つので、取り込みが「もう置いてある」と判断した同じ中身を、
  保存が台帳に載せる前に消すことがない。描く・エンコードする間は持たない。
- **予告と照合**（`ledger::announce`・`clear_announced`・`clear_pending`・`settle_pending`）：台帳にない中身を置く前に `ledger_pending` に
  `(hash, ext, 'place', job)` を書いてコミットし、'place' の予告は台帳に載せるトランザクションで `DELETE FROM ledger_pending WHERE job_id = ?`
  で消す。台帳から外す処理（段 3 の GC・ゴミ箱を空にする処理）は、外す前に消しうる中身を `'delete'` で予告してコミットし（`announce` は
  行の ID を返す。ゴミ箱を空にする処理は `job_id` が NULL）、外したトランザクションのコミットのあとでファイルを消し終えてから、
  その予告を行の ID で消す（`DELETE FROM ledger_pending WHERE id = ?`。主キー。消せなかったファイルの予告は残し、同じジョブのほかの
  予告を消さない）。外さなかった中身（ほかが参照していた）の予告は、照合で行だけ消える。ライブラリを開くとき、staging の掃除の直後・
  整合性チェックとジョブの前に同期で照らす（`LibrarySession::start`。片付けた数はログ）：

  ```sql
  SELECT p.id, p.hash, p.ext,
         EXISTS (SELECT 1 FROM blobs b WHERE b.hash = p.hash AND b.ext = p.ext),   -- 置き終えて載せた（消すのをやめた）
         EXISTS (SELECT 1 FROM blobs b WHERE b.hash = p.hash)                      -- ほかの拡張子で載っている
    FROM ledger_pending p ORDER BY p.id;
  ```

  同じ拡張子で台帳にあれば予告の行だけを消す。なければ `files/<hash>.<ext>` を消し、どの拡張子でも台帳にない中身なら派生の
  ファイル（`Layout::derived_paths`）も消してから予告の行を消す（ほかの拡張子で載っている中身の派生は、そのアイテムのもの）。
  消せなかったファイルの予告は残し、次に開くときにやり直す。電源断で DB の最後のコミットを失っても、残りうるのは「DB にない
  ファイル」だけ（置く前に `sync_all`）。
- **索引なしの例外**：`ledger_pending` は実行中の保存・GC・ゴミ箱を空にする処理の分だけの小さな表なので索引を持たず、開くときの照合と
  `DELETE FROM ledger_pending WHERE job_id = ?` は走査する（`announcements_are_cleared_by_job` の EXPLAIN。台帳の側は hash の一意の
  索引で引く。'delete' の予告を ID で消すのは主キー：`delete_announcements_are_cleared_by_id`）。
- **台帳に載せる**（`ledger::register`）：items のトリガを通らないファイル（版の出力・アンカー）は
  `INSERT INTO blobs … ON CONFLICT(hash) DO NOTHING` のあと行を読み、拡張子・サイズが食い違えば items のトリガと同じ言葉
  （`items_blob_mismatch`）の Conflict。
- **文書の部品**（`edit::store`。部品の形は docs/editing.md §4.4）：置く（`put_doc`）ときは子から順に
  `INSERT INTO edit_objects … ON CONFLICT(digest) DO NOTHING` → `SELECT id FROM edit_objects WHERE digest = ?`、新しく入れた親の分だけ
  `edit_edges` を足す（すでにある部品の辺はもうある）。筆 200 本の文書に 10 本足して置くと、新しい行は最後の切れ目・描画のノード・
  根の 3 行と、その辺だけ（`appending_ten_strokes_writes_only_the_last_chunk_the_node_and_the_root`）。読む（`load_doc_bytes`）ときは
  根から 2 段の子を 1 つの問い合わせで集め、`doc_parts::join` で本文からダイジェストを求め直して照らしてつなぐ（`load_doc` は
  さらに `Engine::load`）。根のダイジェストから行を引くのは `SELECT id FROM edit_objects WHERE digest = ? AND kind = 'doc'`。

  ```sql
  WITH RECURSIVE t(id, depth) AS (
    SELECT ?1, 0
    UNION
    SELECT e.child_id, t.depth + 1 FROM edit_edges e JOIN t ON e.parent_id = t.id WHERE t.depth < 2)   -- 辺は主キーで引く
  SELECT o.id, o.digest, o.kind, o.body FROM edit_objects o WHERE o.id IN (SELECT id FROM t);         -- 部品は行の ID で引く
  ```

- **版を読む**（`edit::versions`）：頭は `SELECT i.hash, i.ext, i.kind, i.trashed_at IS NOT NULL, (SELECT coalesce(max(v.version), 0)
  FROM item_versions v WHERE v.item_id = i.id) FROM items i WHERE i.id = ?`（版の行がなければ版 0。max は `UNIQUE(item_id, version)` の
  索引）。元は版 0 の行、なければアイテムの今。再開の確かめは `WHERE job_id = ?`（部分の一意の索引 `idx_item_versions_job`）。版の一覧は
  `WHERE item_id = ? ORDER BY version`、入力は主キー `(version_id, input)`（テスト `queries_use_their_indexes` の EXPLAIN）。
  不変条件の確かめ（`check_invariants`）は版の表を走査する（テストと整合性の確かめだけ。起動の道では呼ばない）。
- **アンカー**（`edit::anchors`）：引くのは `SELECT … FROM edit_anchors a JOIN blobs b ON b.id = a.blob_id WHERE a.source_hash = ?1 AND
  a.options_digest = ?2`（`UNIQUE(source_hash, options_digest)` の索引）。足すのは保存のトランザクション 2 で、`ledger::register` のあと
  `INSERT … ON CONFLICT(source_hash, options_digest) DO NOTHING` → 行を読み、別のファイルを指す行が先にあれば Conflict（参照されない台帳の
  行を残さない）。`options` と `decoded` は正規の JSON、`value_digest` は元を読んだ記録の値のダイジェスト。
- **保存のトランザクション 1（予告）**（`edit::save`。台帳の門を持って）：土台（`versions::head`）を確かめ直し、読んだアンカー（前からあった
  もの）の行がまだ同じ台帳の行を指して残っていることを `anchors::get` で確かめ直し（描いている間にゴミ箱を空にして片付けられていれば、何も
  置かずに Conflict。トランザクション 2 の外部キーの誤りにしない）、出力の中身 H の持ち主を
  `find_owner` で引いて決める（上書き：ほかのアイテムの今なら Conflict、自分の今なら版だけ足す、元・過去の版なら通す。別名：持ち主が
  いれば existing）。拡張子は台帳に H の行があればその拡張子（`ledger::ext_of`。items のトリガが食い違いで断らないように）、形式が今と
  同じなら今の拡張子、ほかは形式の代表。台帳にない H と今回作ったアンカーを `ledger::announce(…, 'place', job)`。
- **置き換えの UPDATE**（上書きのトランザクション 2）：土台を確かめ直し、`SELECT id FROM items WHERE hash = ?H AND id <> ?1`（ゴミ箱を含む
  ほかのアイテムの今。hash の一意の索引）があれば Conflict。最初の保存なら版 0（出力＝今、`file_blob_id`＝今の台帳の行、hold。
  メディア情報と元を読んだ記録を写す）を足してから、

  ```sql
  UPDATE items SET hash = ?2, ext = ?3, size_bytes = ?4, mime = ?5, width = ?6, height = ?7, duration_ms = ?8,
         codec_info = NULL, thumb_state = 'pending',
         proxy_state = CASE WHEN kind IN ('video','audio') THEN 'unknown' ELSE 'not_needed' END,
         analysis_state = CASE WHEN kind IN ('image','audio') THEN 'pending' ELSE 'not_needed' END,
         updated_at = ?9
   WHERE id = ?1 AND hash = ?10 AND trashed_at IS NULL;   -- 1 行でなければ Conflict（台帳への登録は items_blob_bu が行う）
  ```

  H が今と同じなら `UPDATE items SET updated_at = ? WHERE id = ?` だけ。title・original_name・取り込み日時・再生の記録・素材の設定・
  thumb_rev・kind は変えない（形式が変わる上書きでも。段 2 の決定 10・11）。codec_info は空にし、サムネイル・プロキシのジョブが読み直す。
- **前の頭**：頭の版 n が 1 以上なら、新しい頭を足す前に

  ```sql
  UPDATE item_versions SET file_state = ?3,                                   -- 出力が H か版 0 と同じなら 'released'、ほかは 'waiting'
         file_blob_id = CASE WHEN ?3 = 'released' THEN NULL ELSE file_blob_id END, replaced_at = ?4
   WHERE item_id = ?1 AND version = ?2;
  ```

  （released の行のファイルは新しい頭か版 0 が持つので、台帳の行は参照されたまま）。最初の保存（n = 0）では前の今は元そのもので、
  版 0 が hold で持つ。新しい頭は `kind = 'edit'`・`doc_id`＝根の行・`base_version = n`・`file_blob_id`＝H の台帳の行・hold・
  `job_id`（部分の一意の索引で、再開で版を 2 つ足さない）。版の入力（Primary）は `source_hash`＝元、`value_digest`＝元を読んだ値、
  `recorded`＝`RecordedInput` の正規の JSON、`anchor_id`。最後に `DELETE FROM ledger_pending WHERE job_id = ?` と、途中経過
  `{"v":1,"committed":{…結果…}}`（`jobs.result_json`）を同じトランザクションで書く。
- **別名の写し方**（別名のトランザクション 2）：元のアイテム A の土台を確かめ直し、H の持ち主がいないことを確かめ直してから、新しい
  アイテム B を `import::store::insert_item` で入れ（original_name は「語幹．拡張子」、original_path は NULL、幅・高さ・長さは出力から）、

  ```sql
  UPDATE items SET rating = (SELECT rating FROM items WHERE id = ?A), note = (SELECT note FROM items WHERE id = ?A),
         source_site = (SELECT source_site FROM items WHERE id = ?A), source_url = (SELECT source_url FROM items WHERE id = ?A),
         captured_at = (SELECT captured_at FROM items WHERE id = ?A)
   WHERE id = ?B;
  INSERT INTO item_tags (tag_id, item_id, source) SELECT tag_id, ?B, source FROM item_tags WHERE item_id = ?A;        -- idx_item_tags_item
  INSERT INTO item_folders (folder_id, item_id, pos) SELECT folder_id, ?B, pos FROM item_folders WHERE item_id = ?A; -- idx_item_folders_item
  INSERT INTO item_derivations (item_id, source_item_id, source_version, source_hash, created_at) VALUES (?B, ?A, ?土台の版, ?A の今, ?);
  ```

  B の版 0 は A の元（出力・拡張子・大きさ・メディア情報は A の元のもの、`file_blob_id`＝A の元の台帳の行。A を消しても B の版 0 が台帳の
  行を参照し続ける）、版 1 は描いた版（`base_version = 0`）。ユニット・プレイリスト・再生の記録・素材の設定（`asset_settings`）は写さない
  （決定 4、段 2 の決定 9）。A には版の行を足さない。
- **保存をやめるとき**（トランザクション 2 が Conflict など）：台帳の門を持ったまま、`SELECT p.hash, p.ext FROM ledger_pending p WHERE
  p.job_id = ? AND NOT EXISTS (SELECT 1 FROM blobs b WHERE b.hash = p.hash AND b.ext = p.ext)` のファイルを消し、予告を消す（予告の表は
  走査の例外）。
- **ゴミ箱を空にする処理**（`Library::empty_trash`・`items::plan_empty_trash`・`items::delete_trashed`。段 3 で GC と同じ部品の上に作り
  直した。利用者の決定 6：猶予中のファイル・元・アンカーも照合せずにすぐ消す）：台帳の門を持って、
  1. トランザクション 1：消すアイテム（ゴミ箱の部分索引 `idx_items_trashed` の順に `SELECT id FROM items WHERE trashed_at IS NOT NULL
     ORDER BY trashed_at, id`。`ORDER BY id` にすると items を走査する）と、消しうる中身（アイテムの今・版が持つファイル・版の入力が
     使うアンカーのファイル）を集め、`'delete'` で予告してコミット（このあとゴミ箱に入れられたものは、次に空にするまで残す）。
  2. トランザクション 2：決めたアイテムを 1 件ずつ（主キー・`UNIQUE(item_id, version)`・入力の主キー・下書きの主キーで）消す前に、版が持つ
     ファイルの hash・入力が使うアンカーの ID・版と下書きの文書の根を集めてから行を消す（版・入力・下書きは CASCADE）。どの入力からも
     参照されなくなったアンカーの行を消し（`edit::anchors::sweep_unreferenced`。`idx_item_version_inputs_anchor`）、items ∪ 版 ∪
     アンカーの hash を `ledger::release` に渡す。消したアイテムの版・下書きの文書の部品のうち、ほかから辿れないものを消す
     （`gc::sweep_parts` の根の範囲。下の「文書の部品の片付け」）。予告していない中身を解放したら（ないはずだが）同じトランザクションで
     予告する。
  3. コミットのあとで、解放した中身の実ファイルと派生のファイルを消し（`ledger::remove_files`）、4. 予告を行の ID で消す（消せない
     ファイルがあれば予告を残し、次に開くときの照合が消す）。

  ほかのアイテムの版が持つ同じ中身（別名で保存したアイテムの版 0 が持つ元のアイテムの元）・ほかの版の入力が使うアンカーは REFERRERS で
  解放されない。途中で落ちても、2 のあとなら開くときの照合が台帳にないファイルを消す（`s16_emptying_the_trash_of_edited_items`）。

  ```sql
  -- 1 の消しうる中身（ゴミ箱の部分索引からたどる。CROSS JOIN で結ぶ順を決める。統計のない小さな表で版の表を走査しないよう）
  SELECT b.hash, b.ext FROM items i CROSS JOIN blobs b ON b.hash = i.hash WHERE i.trashed_at IS NOT NULL
  UNION
  SELECT b.hash, b.ext FROM items i CROSS JOIN item_versions v ON v.item_id = i.id
    JOIN blobs b ON b.id = v.file_blob_id WHERE i.trashed_at IS NOT NULL
  UNION
  SELECT b.hash, b.ext FROM items i CROSS JOIN item_versions v ON v.item_id = i.id
    CROSS JOIN item_version_inputs n ON n.version_id = v.id JOIN edit_anchors a ON a.id = n.anchor_id
    JOIN blobs b ON b.id = a.blob_id WHERE i.trashed_at IS NOT NULL;
  -- 2 のアイテムごと
  SELECT b.hash FROM item_versions v JOIN blobs b ON b.id = v.file_blob_id WHERE v.item_id = ?1;
  SELECT n.anchor_id FROM item_versions v CROSS JOIN item_version_inputs n ON n.version_id = v.id
   WHERE v.item_id = ?1 AND n.anchor_id IS NOT NULL;
  SELECT doc_id FROM item_versions WHERE item_id = ?1 AND doc_id IS NOT NULL UNION SELECT doc_id FROM edit_drafts WHERE item_id = ?1;
  DELETE FROM edit_anchors WHERE id = ?1
     AND NOT EXISTS (SELECT 1 FROM item_version_inputs i WHERE i.anchor_id = edit_anchors.id) RETURNING blob_id;
  ```

- **GC の候補**（`edit::gc`。意味は docs/editing.md §14「GC のジョブ」）：消してよい置き換えの時刻の上限 `cutoff` は
  `min(今 − 7 日, 残っているいちばん古いバックアップの作成時刻 − 1)`（バックアップがなければ `今 − 7 日`。`gc::cutoff`。バックアップは
  `backups/` の名前の UTC。秒の単位）。積むかは部分索引の 1 行で決める（`gc::due`。版の表を走査しない）：

  ```sql
  SELECT EXISTS (SELECT 1 FROM item_versions WHERE file_state = 'waiting' AND replaced_at <= ?cutoff);   -- idx_item_versions_waiting
  -- ほかに settings の 'gc.parts'（文書の部品の印）があれば積む
  ```

  候補は、カーソル（置き換えの時刻と版の行の ID）の次から 16 個ずつ、置き換えの古い順に読む（共有していても候補にする。共有は照合が
  一致したあとに門の中で見る。下の「GC の手順」）：

  ```sql
  SELECT v.id, v.item_id, v.version, v.replaced_at, v.file_blob_id, b.hash, b.ext
    FROM item_versions v JOIN blobs b ON b.id = v.file_blob_id
   WHERE v.file_state = 'waiting' AND v.replaced_at <= ?1 AND (v.replaced_at, v.id) > (?2, ?3)
   ORDER BY v.replaced_at, v.id LIMIT ?4;
  -- 共有（ほかの hold の版・アイテムの今・アンカーが同じ台帳の行を参照する。門の中で。3 つの EXISTS はそれぞれ
  -- sqlite_autoindex_items_1・idx_item_versions_file_blob・idx_edit_anchors_blob。candidates_follow_the_cursor_and_sharing_uses_indexes の EXPLAIN）
  SELECT EXISTS (SELECT 1 FROM items i WHERE i.hash = ?hash)
      OR EXISTS (SELECT 1 FROM item_versions w WHERE w.file_blob_id = ?blob AND w.file_state = 'hold')
      OR EXISTS (SELECT 1 FROM edit_anchors a WHERE a.blob_id = ?blob);
  ```

- **GC の手順**（候補ごと。途中経過 `{"v":1,"phase","cursor","released","shared","kept","deferred","anchors","parts","files"}` は候補ごとの
  トランザクションで書く）：
  - どの候補も（共有しているものも）、門の外で照合する（`save::verify_version`）。共有を照合せずに `released` にすると、共有相手が
    消えたときに照合していない版のファイルが消えるため（ADR-170 (2)）。合わなければ
    `UPDATE item_versions SET file_state = 'kept' WHERE id = ? AND file_state = 'waiting' AND file_blob_id = ?`（台帳の行を参照し続ける）、
    照らせなければ何も変えない（カーソルだけ進める）。一致すれば台帳の門を取り、トランザクション 1 で
    `SELECT file_state, file_blob_id FROM item_versions WHERE id = ?` で確かめ直し、共有していれば（前からか、作り直している間に使われた）
    下の UPDATE で `released` にするだけ（ファイルは消さない）、していなければ `ledger::announce(hash, ext, 'delete', job)` をコミット、
    トランザクション 2 で

    ```sql
    UPDATE item_versions SET file_state = 'released', file_blob_id = NULL
     WHERE id = ?1 AND file_state = 'waiting' AND file_blob_id = ?2;   -- 1 行なら ledger::release(hash)
    ```

    をコミットしてから、解放した中身の実ファイルと派生のファイルを消し、予告を ID で消す。
  - アンカー：`SELECT a.id, b.hash, b.ext FROM edit_anchors a JOIN blobs b ON b.id = a.blob_id WHERE NOT EXISTS (SELECT 1 FROM item_version_inputs n
    WHERE n.anchor_id = a.id)`（アンカーの表は元 × 読み方の設定ごとの小さな表なので走査する。入力は `idx_item_version_inputs_anchor`）を
    同じく予告 → `sweep_unreferenced` と `ledger::release` → ファイル → 予告の順で片付ける。
- **文書の部品の片付け**（`gc::sweep_parts`。1 つのトランザクション）：根は版の `doc_id`・下書きの `doc_id` と、保存のジョブの入力の
  `action.doc`（根のダイジェスト）のうち、終わっていないものと 7 日以内に失敗・中止したもの（`save::gc_roots`。利用者がやり直せるように）：

  ```sql
  SELECT input_json FROM jobs WHERE state IN ('queued', 'running', 'paused') AND kind = 'edit'
  UNION ALL
  SELECT input_json FROM jobs WHERE state IN ('failed', 'canceled') AND kind = 'edit' AND finished_at >= ?1;   -- どちらも idx_jobs_queue
  ```

  範囲（GC はすべて：親の辺のない部品＝文書の根と孤立したノード・切れ目。`edit_objects` を走査する（部品の印か消し待ちの候補が
  あって GC が動くときだけ）。ゴミ箱を空にする処理は消したアイテムの版・下書きの文書の根）の部品のうち、根でなく、どこからも
  参照されないものを消し、その子へ進む（`edit_edges.child_id` は ON DELETE なしなので親から順に。親を消すと親の辺は CASCADE で消え、
  子は残った辺がなくなったときに消える。部品の深さは 2 まで）：

  ```sql
  SELECT EXISTS (SELECT 1 FROM edit_edges WHERE child_id = ?1)          -- idx_edit_edges_child
      OR EXISTS (SELECT 1 FROM item_versions WHERE doc_id = ?1)        -- idx_item_versions_doc
      OR EXISTS (SELECT 1 FROM edit_drafts WHERE doc_id = ?1);         -- idx_edit_drafts_doc
  SELECT child_id FROM edit_edges WHERE parent_id = ?1;                 -- 主キー
  DELETE FROM edit_objects WHERE id = ?1;
  ```

  部品が孤立しうるとき（下書きを置き換える・捨てる、上書き・別名の保存のジョブが版を足さずに終わる（待ち・一時停止のまま中止されたときは、
  ハンドラを通らないので `JobHandler::canceled_while_idle` が中止と同じトランザクションで置く）、版 0 に戻した保存が空の文書の下書きを消す）は
  settings の `'gc.parts'` を置き、GC が片付けたトランザクションで

  ```sql
  DELETE FROM settings WHERE key = 'gc.parts'
     AND NOT EXISTS (SELECT 1 FROM jobs WHERE state IN ('queued','running','paused') AND kind = 'edit')
     AND NOT EXISTS (SELECT 1 FROM jobs WHERE state IN ('failed','canceled') AND kind = 'edit' AND finished_at >= ?since);  -- どちらも idx_jobs_queue
  ```

  （終わっていない保存と、7 日以内（`?since` は根と同じ今 − 7 日）に失敗・中止した保存は根として残すので、その部品があとで孤立しうるので
  印を残す。7 日を過ぎて根から外れると、残した印で GC が積まれて片付け、印も消える。`the_parts_mark_outlives_failed_saves_until_their_roots_expire`）。
  保存を積む口（`start_save`）は、同じアイテムの終わっていない保存のジョブがあれば何も置かずに `Busy` で断る（`save::pending_save`：
  `SELECT id, input_json FROM jobs WHERE state IN ('queued','running','paused') AND kind = 'edit'`（`idx_jobs_queue`）の入力のアイテムを比べる。
  部品を置くのと同じトランザクション）。保存のジョブは、入力が指す部品を置くのと同じトランザクションで
  積む（`JobManager::enqueue_with`。間に片付けが入っても、積んだジョブの根として残る）。
- **整合性の数**（M12 の段 3。問題には数えない）：設定の「いま確かめる」で、GC の照合が合わずに残した版
  `SELECT count(*) FROM item_versions WHERE file_state = 'kept'`（版の表を走査する。いま確かめるときだけ）と、7 日とバックアップの条件を
  満たしたのに消し待ちのままの版 `SELECT count(*) FROM item_versions WHERE file_state = 'waiting' AND replaced_at <= ?cutoff`（部分索引）を
  `IntegrityReport` の `keptVersions`・`overdueVersions` に入れる（起動時は GC のジョブの前なので数えず null）。
- **版に戻す**（`edit::save` の `revert`。意味は docs/editing.md §14「版に戻す」）：戻す先 k は 0 以上・頭の版より前。版 k の行は
  `UNIQUE(item_id, version)`、入力は主キーで引く。
  - 使い直す（版 k の中身が台帳にあってファイルがある）：台帳の門を持って `ledger::ext_of`（hash の一意の索引）とファイルを確かめ、
    そのまま下のトランザクション 2 だけを行う（予告・配置なし）。作り直す：保存と同じ予告 → 配置 → トランザクション 2。
  - トランザクション 2：土台（`versions::head`）と、`SELECT id FROM items WHERE hash = ?H AND id <> ?1`（別のアイテムの今なら Conflict）を
    確かめ直し、`ledger::register`（作り直したものはここで台帳に載る。使い直すならもうある）のあと、

    ```sql
    UPDATE items SET hash = ?2, ext = ?3, size_bytes = ?4, mime = ?5, width = ?6, height = ?7, duration_ms = ?8,
           codec_info = ?9, thumb_state = 'pending',                      -- mime・codec_info は版 k の記録（中身が変わったら拡張子から・空）
           proxy_state = CASE WHEN kind IN ('video','audio') THEN 'unknown' ELSE 'not_needed' END,
           analysis_state = CASE WHEN kind IN ('image','audio') THEN 'pending' ELSE 'not_needed' END,
           updated_at = ?10
     WHERE id = ?1 AND hash = ?11 AND trashed_at IS NULL;   -- 1 行でなければ Conflict。H が今と同じなら updated_at だけ（unchanged）
    ```

    前の頭は上書きと同じ（出力が H か版 0 と同じなら released、ほかは waiting）。版 k の行は変えない。新しい頭は
    `kind = 'revert'`・`revert_to = k`・`base_version = n`・`doc_id`＝版 k の `doc_id`（版 0 と、版 0 に戻した版は NULL）・
    `value_digest`＝版 k の値・`file_blob_id`＝H の台帳の行・hold・`job_id`。記録（`record`）は版 k の書き出しの記録（作り直したら
    新しい記録）・版 0 なら元を読んだ記録・メディア情報・警告と `revert: {to, reused, rebuilt}`。入力は版 k の行を写す
    （`INSERT INTO item_version_inputs … ` を版 k の行ごとに。アンカーの参照も写すので、アンカーは参照されたまま）。下書きの片付け・予告の
    消去・途中経過は上書きと同じ。
- **下書き**（`edit::drafts`。意味は docs/editing.md §12.12）：アイテムごとに 1 行（主キー `item_id`）。置く（`drafts::save`）ときは文書を
  `put_doc` で部品にしてから `INSERT … ON CONFLICT(item_id) DO UPDATE`。`after_job` のジョブがもうコミットしていれば（`versions::by_job`）、
  そのとき付け替える。開く（`drafts::open`）ときは `SELECT … FROM edit_drafts d JOIN edit_objects o ON o.id = d.doc_id WHERE d.item_id = ?`
  と、fresh の判定に `versions::head` と `SELECT state IN ('queued','running','paused') FROM jobs WHERE id = ?after_job`（どれも主キー）。
  保存のトランザクション 2 の片付け（`settle_after_save`。保存した文書の根の行 `?saved`。版 0 に戻したときは空の文書の根の行）：

  ```sql
  DELETE FROM edit_drafts WHERE item_id = ?1 AND doc_id = ?saved AND (after_job IS NULL OR after_job = ?job);
  UPDATE edit_drafts SET base_hash = ?new_hash, base_version = ?new_version, after_job = NULL
   WHERE item_id = ?1 AND after_job = ?job;
  ```

  別名で保存では元のアイテム A の下書きに同じ規則を当てる（土台は A の頭のまま）。下書きの部品の行は消さない（置き換える・捨てると
  `'gc.parts'` の印を置き、GC が片付ける。上の「文書の部品の片付け」）。
- **不変条件**（`edit::versions::check_invariants`。表を走査するので、テスト・crash-loop・整合性の確かめだけで呼び、起動の道では呼ばない）：
  1. 版の行は 0 個か 2 個以上で 0..n に欠けなく並ぶ。版 0 と頭は hold、ほかの版は hold でない。頭の output_hash・ext は items.hash・ext。
  2. released でない行の `file_blob_id` が指す台帳の行の hash は output_hash と同じ。
  3. 版 0 でない版の `base_version` は `version - 1`。文書を持つ版・下書きの文書は根（`kind = 'doc'`）の部品で、部品をつなぎ直せる
     （`load_doc_bytes`。本文からダイジェストを求め直す）。
  4. edit の版は入力 0（primary）の行を持つ。revert の版は戻した先の版がそのアイテムにあり、文書・値のダイジェスト・入力の数が同じ。
  5. 入力の `anchor_id` はアンカーの行を指し、アンカーは台帳の行を持ち、元（`source_hash`）が入力の元と同じ。どの入力からも参照されない
     アンカーはない（ゴミ箱を空にする処理が片付ける。残っていれば GC も）。
  6. 終わっていない（待ち・実行中・一時停止の）保存のジョブの入力の文書は、根の部品があってつなぎ直せる（GC の部品の片付けが消さない。
     段 3）。

### 解析の読み書き（M11。`crates/kura-core/src/analysis/`）

- **取り出し**：`SELECT id, hash, ext, kind FROM items WHERE analysis_state = 'pending' ORDER BY imported_at DESC, id DESC LIMIT 16`
  （`idx_items_analysis_pending` だけを読む）。インスペクタで選んだもの（`request_analysis`）は ID で引いて先に解く。
- **計画**：画像は透過・色数・倍率（段 1）と、1 枚の画像で透過があるか 256 色以下ならスプライトの格子の推定（段 2）。音声は
  ラウドネス・ループ・無音・波形の段（最も細かい段はビンが 4,096 以下になる段、粗い方は 65,536 フレーム）。JPEG は
  読まずに形式から事実を書く（透過なし・色数は計らない・ドット絵でない・1 コマ。`decoder` は NULL）。HEIC・HEIF は見出し
  （Platform の段の形。ImageIO が開くだけで展開しない `CGImage` のアルファの扱い）を読み、アルファを持つものだけを読んで
  ほかの画像と同じく解析し、ほかと見出しを読めないものは JPEG と同じく形式から書く（決定 15。見出しを読む前にも処理中の印を
  付ける）。動画・フォント・3D は `not_needed`。
- **結果の鍵**：台帳の行 × 解析 ID（`UNIQUE(blob_id, analysis)`）。版と正規の引数は列で、計画の要求と照らす。同じ中身の結果
  （計画の要求の ID・版・正規の引数と、読み取りの記録 `kura.read`）がそろい、それを読んだデコーダ（事実の `decoder`）が今の組で
  その形式を読むもののどれかなら、読まずに使う。再現を約束しないデコーダの結果も「その環境で読んだ事実」として持ち、OS の更新
  では解き直さず、デコーダの識別子が変わったときだけ解き直す（`analysis.decoders`。戻したものは今の組にないデコーダで読んだ
  ものなので、結果を使い回さずに読み直す。テスト `a_decoder_change_rereads_instead_of_reusing_old_results`）。
- **書き込み**：16 件か 500 ms ごとに 1 つのトランザクションで、アイテムごとに
  `UPDATE items SET analysis_state = ? WHERE id = ? AND hash = ? AND analysis_state = 'pending'` が 1 行変えたときだけ、
  `blob_id` をそのトランザクションの中で `blobs.hash` から引いて、結果（`ON CONFLICT (blob_id, analysis) DO UPDATE`）と
  事実（`ON CONFLICT (blob_id) DO UPDATE`）を書き、今の計画にない解析の行を消す。読まずに作ったときは、事実の読み取りの
  記録の列（decoder・reproducible・value_digest・env）を変えない。
- **波形の段**：結果の行は `output` が NULL（`output_len`・`digest` だけ）で、中身は `peaks/ab/<hash>.peaks`（kura-peaks 版 1。
  `/peaks` でそのまま配る）。DB に書く前に置く（書き込みが失敗してもファイルが残るだけ。次に解くときに置き換える）。
- **投影**：`facts_v`（投影の版）か `pixel_rule`（ドット絵の判定の版）が古い行は、読み直さずに結果の表から作り直す
  （`media_facts` を `blob_id` の順に 500 件ずつ。`state = 'ok'` の行だけ。読み取りの記録 `kura.read` のない JPEG・HEIC・HEIF は
  形式から）。古い行を探すのは、計画の指紋の投影の版・ドット絵の
  規則が変わって起動のスレッドが作り直しの印（settings の `analysis.reproject`）を置いたときだけで、作り直し終えたら消す
  （途中で止めたら印は残り、次の実行で初めから探す）。ふだんの起動・ジョブは事実の表を走査しない。
- **失敗**：事実の行に `state = 'failed'` と `error_code` を書き、items は `failed`。計画・デコーダの指紋が変わるか、利用者が頼む
  （`retry_analysis`：`failed` を `pending` に戻す）まで解き直さない。見積もり（値の 2 倍＋デコーダの作業領域）が
  min(予算/2, 1 GiB) を超えるものは、予算を確保せずに `too_large`。中止・一時停止・強制終了（電源断・ほかの部品の異常終了を
  含む）では、書くまでは `pending` のまま。ただし OS のデコーダ（Platform の段）を通りうる形式（コーデックの組にその形式の Platform の
  デコーダがあるもの。アプリの組では HEIC・HEIF の見出しと AVIF・TIFF・BMP、固定の段が先に読む PSD・WAV・AIFF・Ogg・Opus・m4a・AAC）を
  読んでいる 1 件は、固定の段が読んでいても、読む前にコミットした処理中の印（settings の `analysis.inflight`）が残るので、強制終了でも
  次にジョブが動いたときに `failed`（`crashed`）になる（印は落ちたのか止められたのかを見分けず、どの段が読むかも読む前には分からない。
  `retry_analysis` で戻せる。M11 の確かめ C1）。
- **一度きりの全件の走査**（例外）：計画の指紋が変わった種類を戻す `UPDATE items … WHERE kind = ? AND analysis_state IN ('ready','failed')`
  と扱いが変わった形式を戻す `UPDATE items … WHERE lower(ext) IN (…) AND analysis_state IN ('ready','failed')`（決定 15 の更新で、
  形式から書いていた HEIC・HEIF を一度だけ見直す）、デコーダの指紋で戻す `UPDATE … WHERE … hash IN (SELECT b.hash FROM media_facts f JOIN blobs b …)` は全件を走査する
  （指紋が変わったときだけ。10 万件で約 70 ms）。投影の古い行を探す `stale_facts`（`media_facts` を主キーの範囲で読み、残りの
  条件で絞る）も、古い行がなければ表の終わりまで読む（作り直しの印があるときだけ。8 万行で約 3〜7 ms）。テスト
  `sync_plan_reset_is_a_one_time_full_scan`。

### サムネイルの作り直し（M11。`crates/kura-core/src/media/`）

- **取り出し**：`thumb_state = 'pending'` を新しい順に 64 件（`idx_items_thumb_pending`）。pending がなくなったら `stale` を新しい順に
  （`idx_items_thumb_stale`）。疑わしいもの（settings の `thumbs.suspects`）があれば、それを先に 1 件ずつ。
- **書き込み**：64 件を 1 つのトランザクションで、`UPDATE items SET … thumb_state = ?, thumb_rev = thumb_rev + ? … WHERE id = ? AND hash = ?`。
  作れたら ready で `thumb_rev` を 1 上げる。作り直し（stale）を作れなかったときは、前の絵のまま ready に戻す（rev は変えない）。
  読んだメディア情報は空の列だけ埋める。
- **作り方の指紋**（`media::route::sync_plan`。ライブラリを開くとき、ジョブを動かす前）：settings の `thumbs.plan` と今の作り方を
  形式ごとに比べ、変わった形式の拡張子の `ready` を `stale`、`failed` を `pending` にする。
- **一度きりの全件の走査**（例外）：この 2 つの `UPDATE items … WHERE thumb_state = 'ready'（'failed'） AND lower(ext) IN (…)` は
  `ready`・`failed` に索引がないので全件を走査する（指紋が変わったときだけ。M10 の seed 10 万件で 46,860 件を stale にして 71 ms。
  テスト `plan_updates_scan_items_only_when_the_plan_changes`）。

### 事実で引く（M11）

- 事実の条件は `i.blob_id IN (SELECT f.blob_id FROM media_facts f WHERE …)` の形にする（部分索引から台帳の行の ID の集合を作り、
  `idx_items_blob` で items を引く。EXISTS の相関した問い合わせにしない。M11 は `i.hash IN (SELECT b.hash … JOIN blobs b …)` で、
  M12 の 0010 で整数の鍵にした）。
- 検索の実効値は「設定 ?? 自動」：ドット絵は `asset_settings.pixel_art` があればそれ、なければ `media_facts.pixel_art`。
  どちらもない（未解析か、解析しても材料が足りず NULL）ものは決まっていないので、`:あり`・`:なし`・`!=` のどれにも入れない
  （表示の実効値は「?? 偽」で偽にする。上の「一覧の取得」。テスト `pixel_art_search_follows_the_effective_value_of_the_asset_panel`）。
- アニメも「設定 ?? 事実」（M11 のユーザーの決定 16）：`asset_settings.grid_json` の格子があればアニメ、なければ
  `media_facts.frame_count` が 2 以上。`アニメ:あり` は `i.id IN (格子のあるアイテム) OR i.blob_id IN (… frame_count > 1)`、
  `アニメ:なし` は `i.blob_id IN (… frame_count <= 1) AND i.id NOT IN (格子のあるアイテム)`（AND の中ではコマの側を事実の
  部分問い合わせにまとめる）。格子のあるアイテムは `asset::grid_items_sql`（`grid_json` が版 1 の JSON。壊れたもの・知らない版は
  素材の欄と同じく格子なし。版 1 の中の欄の欠け・知らない欄までは SQL で見ない：書き手 `asset::set_settings` は版 1 のそろった形
  だけを書く）で、`asset_settings` を索引なしで読む（変えたアイテムにだけ行があるので小さい。items は主キーで引き、
  走査しない。テスト `anim_search_follows_the_effective_value_of_the_asset_panel`・`anim_conditions_find_items_without_scanning_them`）。
  ドット絵・アニメと同じ AND の種類の条件は `+i.kind` にする（種類の索引で画像を全部たどらない。10 万件で、種類の索引を使うと
  `種類:画像 アニメ:あり` 27 ms・`種類:画像 ドット絵:あり` 22 ms、使わなければ 7.7 ms・3.2 ms。一致が 6 万件を超える
  `種類:画像 ドット絵:なし` は 84 ms から 96 ms に延びたが、下の広い条件として M12 で直した。ADR-166）。
- 2 のべき乗は列を持たず、`width & (width - 1) = 0` のビット演算で求める。
- まだ解析していないもの・計れない値は、肯定・`!=`・大小の比較のどれにも入れない（除外だけが含む。docs/search-query.md §4）。
  色数の打ち切りの 257 は、結果が決まる比較（`>`・`!=` は 256 以下、`>=` は 257 以下の値）のときだけ入れ、ほかは
  `f.colors < 257` を足して除く。
- 一致が 6〜9 万件の広い条件（`透過!=あり`・`ドット絵:なし`・`アニメ:なし`・`-透過:あり` など）は、M11 の形（数万件の 32 バイトの
  ハッシュで items を引く）では 10 万件で 43〜114 ms かかった。M12 で items に台帳の行の ID（`blob_id`。0010）を持たせ、事実の集合を
  整数の鍵で引く形にした（M11 のユーザーの決定 17。ADR-166）。同じ機械・同じ seed で、広い条件の一覧は 44〜103 ms から 27〜37 ms、
  狭い条件も 0.7〜12 ms から 0.3〜9.8 ms になった（`bench query --fake-facts` の 26 の条件）。
- ⌘K の件数（`query::check_db`）は、種類ごとの件数と「未解析 N 件」を読み取りの接続 2 つで同時に数える（どちらも広い条件で
  30 ms 前後かかり、順に数えると和で 50 ms を超える。結果は 1 つの接続で順に数える `query::check` と同じ。`check_db_matches_check`）。
  広い条件で 25〜40 ms（M11 の形で 53〜127 ms）。

### ユニットの集計

set ユニットの件数・合計時間・種類は表示のたびに集計してよい（数百件程度）。遅くなったら `units` に集計列を追加し、トリガーで更新する。

### 遅くなったら

10 万件超でタグ検索が 50ms を超えるようになったら、起動時に「タグ ID → アイテム ID の Roaring Bitmap」をメモリに作り、集合演算で絞り込む方式を追加する（DB はそのまま正本）。

## 5. マイグレーションの当て方（M11。`crates/kura-core/src/db/migrate.rs`）

`Library::open` が `Db::open_with` で当てる。版は `PRAGMA user_version`（当てたマイグレーションの数）。その前にライブラリのロック
（`library.lock`。§1）を取り、ほかのアプリが開いていれば以下のどれもせずに `LibraryInUse` で断る（M12）。

1. **DB の方が新しい**（版がこの Kura の知るより大きい）なら、当てずに開くのを失敗にする。
2. **空き容量の確かめ**：未適用のマイグレーションがある既存の DB（版 1 以上）では、まず要る空きを DB の大きさ（頁の数 × 頁の大きさ）
   から見積もり（`library::upgrade::space_needed`：バックアップ 1 ＋ 作り直す表を DB の中と WAL に 1 つずつで、DB の 3 倍 ＋ 64MiB）、
   ボリュームの空き（アプリが渡す。macOS は kura-mac の `volume_capacity` の「使用可能」）が足りなければ、バックアップも取らずに断る
   （`UpgradeError::NoSpace`。ライブラリは変えない）。10 万件の seed（DB 51.8MB）で測った増え方の最大は 119.6MB（DB の 2.31 倍：
   バックアップ 51.8MB、DB の伸び 34.1MB、WAL の伸び 33.7MB）で、見積もり（約 222MB）に収まる。
   **当てる前のバックアップ**：続けて、書き込みの接続から backup API で
   `backups/library-YYYYMMDD-HHMMSS.sqlite`（いつもの名前・DELETE モード）へ一度に写し、7 個の保持に乗せる。
   失敗したら（空き容量など）当てずに開くのを失敗にする。新しいライブラリ（版 0）では作らない。書きかけの一時ファイルは
   `.<名前>.<プロセス ID>-<通し番号>.part` で、同じ秒に同じライブラリを開くほかのプロセスと取り合わない。同じ秒の名前の
   バックアップがあって版が違えば（同時に開いた別のプロセスが、先に当てる前の中身を残してから当て始めた）、それを置き換えない。
3. **当てる**：1 つずつ、それぞれ 1 つのトランザクション（`BEGIN IMMEDIATE`）で当てる。トランザクションの中で版を読み直し、
   もう当たっていれば飛ばし、1 つ前の版でなければ断る（別のプロセスが同じライブラリを同時に開くと、版を読んでから当てるまでの
   間に先に当てていることがある。当て直して版を下げない。テスト `a_migration_another_connection_applied_is_skipped_without_lowering_the_version`）。
   - `Plain`：そのまま当てる。
   - `Rebuild`（ほかの表から参照される表の作り直し。0007）：トランザクションの中からは断る。トランザクションの外で `foreign_keys` を
     切り（読み直して確かめる）、`BEGIN IMMEDIATE` → SQL → `PRAGMA foreign_key_check` → 違反がなければ版を上げてコミット。
     違反があるときだけいったん戻し、当てる前からあった違反と比べ（同じ行の数も比べる）、新しい違反があれば当てない。
     前からの違反だけなら当て直して警告を残す。成功しても失敗しても `foreign_keys` を戻す。事前の確かめはしない（10 万件で約 160ms を省く）。
   - 作り直しの SQL は「新しい表を作る → 移す → 古い表を DROP → 新しい表を RENAME」。古い表を先に改名しない。
4. **WAL を空にする**：1 つでも当てたら `PRAGMA wal_checkpoint(TRUNCATE)`（しないと次に開くときに WAL を読み直す）。
5. 当てられなかったら、失敗したマイグレーションのトランザクションは戻り、それより前に当てたものは残る（版はそこまで上がる。
   0007・0008 が通って 0009 で失敗すれば版 8）。理由は `CoreError::Upgrade`（新しい版・空き容量・バックアップ・当てる失敗）の
   言葉で返す。当てる失敗の言葉は、1 つも当てていなければ「前の版のまま」、途中の版まで当てていれば「途中の版（スキーマ N）まで
   当てたので前の版の Kura ではもう開けない・原因を取り除いて開き直すと続きを当てる・当てる前のバックアップが backups にある」と
   言う（`UpgradeError::Apply` の `from`・`reached`）。新しいライブラリ（版 0 から当てる。`library_create`）は「新しいライブラリを
   作る途中で止まりました。開き直すと続きを当てます」と言う（前の版の Kura も当てる前のバックアップもないので、前の版・途中の版の
   言葉は使わない。作りかけの DB は上がった版のまま残り、同じ場所を開く・作ると続きを当てる。M11 の確かめ C3）。SQL の本文は言葉に入れず（SQLite の言葉と位置だけ。`CoreError::short`）、
   ログに全文を残す（テスト `a_late_failure_says_the_library_is_now_at_a_middle_version`）。アプリはこれを `AppError` の code
   `LibraryTooNew`・`NoSpace`・`BackupFailed`・`UpgradeFailed` にし、`library_open` は失敗として、`library_restore` は
   `error` に入れて返す（docs/architecture.md §3）。開く前に当てるかどうかは `library::needs_upgrade` で分かる。
6. **知らせ**：当て始めたら（空きの確かめの前）`UpgradeEvent::Started`、終わったら `Finished`、当てられなかったら `Failed` を、
   アプリが渡した受け手（`library::UpgradeHooks`）に知らせる。アプリはイベント `library://upgrade` にしてフロントへ送り、フロントは
   300ms を超えたら「ライブラリを更新しています」を出す。新しいライブラリ・当てるものがないとき・新しい版の DB では知らせない。
   開いたことのログ（「起動から前回のライブラリを開くまで」「起動から最初のタイルの詳細を返すまで」）に `upgraded`（この起動で
   新しい版にした）と `upgrade_ms` を付け、更新後の初回を NFR-01 の外として 2 回目以降と分けて記録できるようにする。
7. アプリの中では `quick_check`・FTS の照合・VACUUM をしない。照合はテストと `kura-devtools bench migrate` で行う。
8. アプリはライブラリを開く・作る・戻すを 1 つの鍵（`AppState::opening`）で順に行う。起動の restore と開く操作が重なっても、
   新しい版にするのとその前のバックアップは 1 回だけ（2 回目は当て終えた版を開く。src-tauri の
   `opening_the_same_library_in_parallel_upgrades_once`）。別のプロセス（Kura を 2 つ動かした・開発版と本番版）が同じライブラリを
   開くのは、M12 からライブラリのロック（§1）が 2 つ目を `LibraryInUse` で断る（`a_library_held_by_another_app_is_refused_before_anything_happens`）。
   flock が使えないファイルシステムでロックなしに同時に開いたときは、3 の読み直しとバックアップの一時ファイルの名前で、どちらも
   開け、版は下がらない（`two_processes_opening_the_same_library_at_once_both_open_and_keep_the_version`。スレッドごとの接続と
   ロックなしの開き方で代わりに確かめる）。遅れた側は、先の側の当てている途中の版から当てる前のバックアップを作ることがある
   （秒が同じなら作らない）。

**10 万件の時間**（2026-10-20、M4 Max、同梱の SQLite 3.53、ページキャッシュは温まった状態、ほかの作業でロードアベレージ 9〜12。
`bench migrate --runs 3`。開くところから数えた時間）：

| 元の DB | バックアップ | 0007 | 0008 | 0009 | TRUNCATE | 開くまでの合計 | 当てたあと | 2 回目に開く |
|---|---|---|---|---|---|---|---|---|
| seed 10 万件（スキーマ 6、51.8MB ＋ WAL 10MB） | 47〜49ms | 711〜738ms | 1.1〜1.2ms | 0.2〜0.3ms | 0.8〜2.6ms | 764〜792ms | DB 85.9MB・WAL 0 | 1.9〜2.2ms |
| 合成 10 万件 `--history`（再生の記録 20 万・エントリ 1 万・取得元 1 万、60.1MB） | 52〜62ms | 751〜779ms | 1.0〜1.1ms | 0.3ms | 0.6〜0.7ms | 807〜839ms | DB 95.9MB・WAL 0 | 1.8〜2.0ms |

どちらも、すべての表の行数が当てる前と同じ、`foreign_key_check` が空、FTS の照合（rank=1）と `quick_check` が ok、台帳と items が
1 対 1（10 万行）だった。更新後の初回の起動にだけ載る一度きりの作業で、NFR-01 の外として別に記録する（2 回目以降は 1.5 秒を保つ）。

確保器を入れたあとのコミット（fbd7580。ロードアベレージ 3.4〜7）で、seed 10 万件を測り直した（`bench migrate --runs 3`）：開く処理の全体
713〜715ms（バックアップ 42〜43ms、0007 667〜671ms、0008 1ms、0009 0.2ms、TRUNCATE 0.7ms）。21 表の行数 504,767 が当てる前と同じ、
`foreign_key_check` が空、FTS の照合と `quick_check` が ok、台帳と items が 1 対 1。アプリで更新後の初回を開くと、起動から最初の
タイルの詳細を返すまで 1.39〜1.43 秒（うち更新 0.86〜0.90 秒：当てる前の確かめとバックアップ約 0.15 秒・0007〜0009・TRUNCATE。
ログの `upgrade_ms` で、`MigrationReport::total_ms`）、更新直後の 2 回目は 0.39 秒だった（docs/requirements.md の NFR-01）。
サムネイルの作り方の指紋で 46,860 件を stale にする 71ms（§4「サムネイルの作り直し」）は、DB を開き終えたあとに起動のスレッド
（`media::prepare_on_open`）で最初のタイルの詳細と並んで動くので、更新の時間には入らない。
