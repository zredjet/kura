-- 0014（Plain）：ライセンスとクレジット（M15 段 1。docs/data-model.md §3・§4「ライセンスとクレジット（M15）」、ADR-202・203）。
-- ライセンスの表（既定の行と利用者が足した行。表記義務・商用・改変・継承の印）と、アイテムごとのクレジット（ライセンス・作者・
-- 作品・出典・表記の文）を足す。既定の行は builtin に鍵を持ち、名前と印を変えられず消せない（kura-core の credits が断る）。
-- 自作の行を消すと、使っていたアイテムのライセンスは NULL（未設定）に戻る（SET NULL。利用者の決定 4）。アイテムを消すと
-- クレジットの行も消える（CASCADE）。新しい表だけなので Plain。適用後は編集しない（既定の行を足す・直すのは新しいマイグレーション）。

CREATE TABLE licenses (
  id           INTEGER PRIMARY KEY,
  builtin      TEXT,                                   -- 既定の行の鍵（'cc-by-4.0' など）。利用者が足した行は NULL
  name         TEXT    NOT NULL,                       -- 表示名（'CC BY 4.0'）。大文字小文字を問わず一意
  spdx         TEXT,                                   -- SPDX の識別子（'CC-BY-4.0'）。ないものは NULL
  url          TEXT,                                   -- ライセンスの文の URL
  attribution  TEXT    NOT NULL CONSTRAINT licenses_attribution CHECK (attribution IN ('required','optional','none')),
                                                       -- 表記義務：必要（一覧に必ず載る）・任意（推奨。載せるか選べる）・不要
  commercial   TEXT    NOT NULL CONSTRAINT licenses_commercial CHECK (commercial IN ('yes','no','unknown')),  -- 商用
  modify       TEXT    NOT NULL CONSTRAINT licenses_modify CHECK (modify IN ('yes','no','unknown')),          -- 改変
  share_alike  INTEGER NOT NULL DEFAULT 0 CONSTRAINT licenses_share_alike CHECK (share_alike IN (0, 1)),    -- 継承（同じ条件で配る）
  note         TEXT,                                   -- 規約の要点など
  pos          INTEGER NOT NULL,                       -- 並び（既定の行が先）
  created_at   INTEGER NOT NULL,
  updated_at   INTEGER NOT NULL
);
CREATE UNIQUE INDEX idx_licenses_builtin ON licenses(builtin) WHERE builtin IS NOT NULL;
CREATE UNIQUE INDEX idx_licenses_name ON licenses(name COLLATE NOCASE);

INSERT INTO licenses (builtin, name, spdx, url, attribution, commercial, modify, share_alike, pos, created_at, updated_at) VALUES
  ('cc0-1.0',         'CC0 1.0',         'CC0-1.0',         'https://creativecommons.org/publicdomain/zero/1.0/',      'none',     'yes', 'yes', 0, 0,  0, 0),
  ('cc-by-4.0',       'CC BY 4.0',       'CC-BY-4.0',       'https://creativecommons.org/licenses/by/4.0/',            'required', 'yes', 'yes', 0, 1,  0, 0),
  ('cc-by-3.0',       'CC BY 3.0',       'CC-BY-3.0',       'https://creativecommons.org/licenses/by/3.0/',            'required', 'yes', 'yes', 0, 2,  0, 0),
  ('cc-by-sa-4.0',    'CC BY-SA 4.0',    'CC-BY-SA-4.0',    'https://creativecommons.org/licenses/by-sa/4.0/',         'required', 'yes', 'yes', 1, 3,  0, 0),
  ('cc-by-sa-3.0',    'CC BY-SA 3.0',    'CC-BY-SA-3.0',    'https://creativecommons.org/licenses/by-sa/3.0/',         'required', 'yes', 'yes', 1, 4,  0, 0),
  ('cc-by-nc-4.0',    'CC BY-NC 4.0',    'CC-BY-NC-4.0',    'https://creativecommons.org/licenses/by-nc/4.0/',         'required', 'no',  'yes', 0, 5,  0, 0),
  ('cc-by-nc-sa-4.0', 'CC BY-NC-SA 4.0', 'CC-BY-NC-SA-4.0', 'https://creativecommons.org/licenses/by-nc-sa/4.0/',      'required', 'no',  'yes', 1, 6,  0, 0),
  ('oga-by-3.0',      'OGA-BY 3.0',      NULL,              'https://static.opengameart.org/OGA-BY-3.0.txt',           'required', 'yes', 'yes', 0, 7,  0, 0),
  ('ofl-1.1',         'SIL OFL 1.1',     'OFL-1.1',         'https://openfontlicense.org/open-font-license-official-text/', 'required', 'yes', 'yes', 1, 8, 0, 0),
  ('mit',             'MIT',             'MIT',             'https://opensource.org/license/mit',                      'required', 'yes', 'yes', 0, 9,  0, 0),
  ('apache-2.0',      'Apache 2.0',      'Apache-2.0',      'https://www.apache.org/licenses/LICENSE-2.0',             'required', 'yes', 'yes', 0, 10, 0, 0),
  ('public-domain',   'パブリックドメイン', NULL,           NULL,                                                      'none',     'yes', 'yes', 0, 11, 0, 0),
  ('own',             '自作',            NULL,              NULL,                                                      'none',     'yes', 'yes', 0, 12, 0, 0);

CREATE TABLE item_credits (
  item_id     INTEGER PRIMARY KEY REFERENCES items(id) ON DELETE CASCADE,
  license_id  INTEGER REFERENCES licenses(id) ON DELETE SET NULL,  -- NULL は未設定
  author      TEXT,                                    -- 作者
  work        TEXT,                                    -- 作品・素材集の名前
  url         TEXT,                                    -- 出典（NULL なら items.source_url を使う）
  credit      TEXT,                                    -- 表記の文（NULL なら作者・作品・出典・ライセンスから組む）
  updated_at  INTEGER NOT NULL
);
CREATE INDEX idx_item_credits_license ON item_credits(license_id);
