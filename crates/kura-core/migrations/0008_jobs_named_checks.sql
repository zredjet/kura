-- 0008（Plain）：jobs.kind に analyze（解析。M11）を足し、CHECK に名前を付ける（jobs_kind・jobs_state）。
-- 以後の値の追加は作り直さずに ALTER TABLE jobs DROP CONSTRAINT jobs_kind; ADD CONSTRAINT jobs_kind CHECK (…) で行う
-- （SQLite 3.53 以上）。jobs を参照する外部キーはないので 0005 と同じ作り直し（Plain）でよい。適用後は編集しない。
CREATE TABLE jobs_new (
  id            INTEGER PRIMARY KEY,
  kind          TEXT    NOT NULL
                CONSTRAINT jobs_kind CHECK (kind IN ('import','thumbnail','proxy','download','capture','export','analyze')),
  state         TEXT    NOT NULL
                CONSTRAINT jobs_state CHECK (state IN ('queued','running','paused','done','failed','canceled')),
  priority      INTEGER NOT NULL DEFAULT 0,
  input_json    TEXT    NOT NULL,
  progress      REAL,
  message       TEXT,
  error_code    TEXT,
  log_path      TEXT,
  result_json   TEXT,
  created_at    INTEGER NOT NULL,
  started_at    INTEGER,
  finished_at   INTEGER
);
INSERT INTO jobs_new (id, kind, state, priority, input_json, progress, message, error_code, log_path,
                      result_json, created_at, started_at, finished_at)
  SELECT id, kind, state, priority, input_json, progress, message, error_code, log_path,
         result_json, created_at, started_at, finished_at
  FROM jobs;
DROP TABLE jobs;
ALTER TABLE jobs_new RENAME TO jobs;
CREATE INDEX idx_jobs_queue ON jobs(state, priority DESC, created_at);
