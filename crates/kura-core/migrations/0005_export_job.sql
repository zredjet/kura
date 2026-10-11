-- 書き出しのジョブ（kind = 'export'。FR-SYS-02）を足す。
-- SQLite は CHECK を変えられないので、jobs を作り直して中身を移す（列と索引は 0001 と同じ）。
CREATE TABLE jobs_new (
  id            INTEGER PRIMARY KEY,
  kind          TEXT    NOT NULL CHECK (kind IN ('import','thumbnail','proxy','download','capture','export')),
  state         TEXT    NOT NULL CHECK (state IN ('queued','running','paused','done','failed','canceled')),
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
