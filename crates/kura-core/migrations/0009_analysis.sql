-- 0009（Plain）：解析（M11。docs/data-model.md §3）。items.analysis_state と部分索引は 0007 の作り直しで足した
-- （CHECK 付きの ADD COLUMN は全行を確かめて 10 万件で約 150ms かかるため）。
-- 中身ごとの事実 media_facts と解析の結果 analysis_results（台帳の行ごと。台帳の行を消すと CASCADE で消える）、
-- アイテムごとの素材の設定 asset_settings（アイテムを消すと CASCADE で消える）。適用後は編集しない。

-- ===== 中身ごとの事実（検索と表示の列。台帳の 1 行に 1 行。解析の結果から純粋な関数で作る）=====
CREATE TABLE media_facts (
  blob_id          INTEGER PRIMARY KEY REFERENCES blobs(id) ON DELETE CASCADE,
  state            TEXT    NOT NULL CONSTRAINT media_facts_state CHECK (state IN ('ok','failed')),
  error_code       TEXT,                   -- failed：'unsupported'・'too_large'・'decode'
  decoder          TEXT,                   -- 読んだデコーダの識別子（DecodeStamp.codec）。形式だけで決めたときは NULL
  reproducible     INTEGER,                -- 0/1（DecodeStamp.reproducible）
  value_digest     BLOB,                   -- 読んだ値のダイジェスト（32 バイト）
  env              TEXT,                   -- 再現を約束しないデコーダの環境（OS の版など）
  facts_v          INTEGER NOT NULL,       -- 投影の版（上がれば analysis_results から作り直す。読み直さない）
  -- 画像
  alpha            INTEGER,                -- 0 なし・1 二値・2 半透明あり。NULL は計っていない
  colors           INTEGER,                -- 1〜257（257 は 256 を超える）
  visible_colors   INTEGER,
  pixel_kx         INTEGER,
  pixel_ky         INTEGER,
  pixel_lossless   INTEGER,
  pixel_art        INTEGER,                -- 自動の判定（0/1。pixel_rule の版の規則で）
  pixel_rule       INTEGER,
  frame_count      INTEGER,
  -- 音声
  loudness         REAL,                   -- 積分ラウドネス（LUFS）。計れなければ NULL で loudness_reason
  loudness_reason  TEXT,
  loudness_range   REAL,                   -- LRA（LU）
  true_peak        REAL,                   -- dBTP
  sample_peak      REAL,
  has_loop         INTEGER,                -- 0/1
  loop_start       INTEGER,                -- 標本
  loop_end         INTEGER,
  sample_rate      INTEGER,
  channels         INTEGER,
  bits             INTEGER,                -- 非可逆の形式は NULL
  clip_runs        INTEGER,
  -- フォント・3D（M11 では items.codec_info に入れ、ここは使わない。後の検索のための場所）
  font_family      TEXT,
  glyph_count      INTEGER,
  vertex_count     INTEGER,
  triangle_count   INTEGER,
  analyzed_at      INTEGER NOT NULL
);
-- 部分索引の WHERE は、検索の条件から導ける形にする（比較は IS NOT NULL を導くが、colors <= 16 は colors <= 256 を導かない）
CREATE INDEX idx_media_facts_alpha     ON media_facts(alpha) WHERE alpha IS NOT NULL;
CREATE INDEX idx_media_facts_colors    ON media_facts(colors) WHERE colors IS NOT NULL;
CREATE INDEX idx_media_facts_loudness  ON media_facts(loudness) WHERE loudness IS NOT NULL;
CREATE INDEX idx_media_facts_pixel_art ON media_facts(pixel_art) WHERE pixel_art = 1;
CREATE INDEX idx_media_facts_loop      ON media_facts(has_loop) WHERE has_loop = 1;

-- ===== 解析の結果（正本。中身 × 解析 ID ごとに今の版・引数の 1 行。波形の段は peaks/ab/<hash>.peaks に置き output は NULL）=====
CREATE TABLE analysis_results (
  id          INTEGER PRIMARY KEY,
  blob_id     INTEGER NOT NULL REFERENCES blobs(id) ON DELETE CASCADE,
  analysis    TEXT    NOT NULL,            -- 'image.alpha' など
  version     INTEGER NOT NULL,
  params      TEXT    NOT NULL,            -- 正規の引数（正規の JSON）
  encoding    TEXT    NOT NULL CONSTRAINT analysis_results_encoding CHECK (encoding IN ('json','binary')),
  output      BLOB,                        -- 結果の正規の形（バイト列を変えない）。ファイルに置くものは NULL
  output_len  INTEGER NOT NULL,
  digest      BLOB    NOT NULL,            -- kura-analysis-v1
  created_at  INTEGER NOT NULL,
  UNIQUE (blob_id, analysis)
);

-- ===== 素材の設定（利用者の決定。アイテムごと、変えたアイテムにだけ行がある）=====
CREATE TABLE asset_settings (
  item_id      INTEGER PRIMARY KEY REFERENCES items(id) ON DELETE CASCADE,
  pixel_art    INTEGER CONSTRAINT asset_settings_pixel_art CHECK (pixel_art IN (0, 1)),   -- NULL は自動
  pixel_scale  INTEGER CONSTRAINT asset_settings_pixel_scale CHECK (pixel_scale BETWEEN 1 AND 64),
  grid_json    TEXT,                       -- {"v":1,"cellW","cellH","offsetX","offsetY","gapX","gapY","count"}
  frame_ms     INTEGER CONSTRAINT asset_settings_frame_ms CHECK (frame_ms > 0),
  pivot_x      REAL,
  pivot_y      REAL,
  loop_start   INTEGER,                    -- 試聴のループ（標本）。ファイルは変えない（M11 では使わない。M13）
  loop_end     INTEGER,
  anim_tag     TEXT,                       -- aseprite の再生するタグ
  updated_at   INTEGER NOT NULL,
  CONSTRAINT asset_settings_loop CHECK (
    -- NULL を含む比較は CHECK を通ってしまうので、片方だけ NULL を明示して断る
    (loop_start IS NULL AND loop_end IS NULL)
    OR (loop_start IS NOT NULL AND loop_end IS NOT NULL AND loop_start >= 0 AND loop_start < loop_end))
);
CREATE INDEX idx_asset_settings_pixel_art ON asset_settings(pixel_art) WHERE pixel_art IS NOT NULL;
CREATE INDEX idx_asset_settings_loop      ON asset_settings(item_id) WHERE loop_start IS NOT NULL;
