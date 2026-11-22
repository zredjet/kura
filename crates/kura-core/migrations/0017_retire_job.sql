-- 0017（Plain）：退役する読み手の記録をアンカーへ移すジョブ（M16 段 5。docs/editing.md §9.4「退役する読み手の移し替え」、
-- docs/data-model.md §3・§4、ADR-222）。jobs_kind に retire を足す（名前付きの CHECK の入れ替え。表は作り直さない）。
-- 表・列・索引は足さない（移したアンカーは edit_anchors の行で、鍵の options_digest を別のドメインにする）。適用後は編集しない。

ALTER TABLE jobs DROP CONSTRAINT jobs_kind;
ALTER TABLE jobs ADD CONSTRAINT jobs_kind
  CHECK (kind IN ('import','thumbnail','proxy','download','capture','export','analyze','edit','gc','batch','project','retire'));
