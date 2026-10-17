#!/bin/sh
# 編集のゴールデンは追記だけ（docs/editing.md §8.5、ADR-136）。
# 基準のコミットからの差分で、crates/kura-engine/goldens/・goldens-codecs/・goldens-replay/・goldens-analysis/ の既存の行が
# 消えた・変わったもの、
# fixtures/・tests/vectors/ のファイルが消えた・変わったものがあれば失敗する。
# 使い方：scripts/check-goldens-append-only.sh [基準]（既定は origin/main。なければ main）
set -eu

base="${1:-}"
zero="0000000000000000000000000000000000000000"
if [ -z "$base" ] || [ "$base" = "$zero" ] || ! git cat-file -e "$base^{commit}" 2>/dev/null; then
  if git rev-parse --verify --quiet origin/main >/dev/null; then
    base="origin/main"
  else
    base="main"
  fi
fi

merge_base=$(git merge-base "$base" HEAD 2>/dev/null || echo "$base")
dirs="crates/kura-engine/goldens crates/kura-engine/goldens-codecs crates/kura-engine/goldens-replay crates/kura-engine/goldens-analysis"
# 改名（git mv）も、旧いパスの削除として見る（--no-renames）。ファイルごと消したものは名前で知らせる
# shellcheck disable=SC2086
deleted=$(git diff --no-renames --name-only --diff-filter=D "$merge_base" HEAD -- $dirs)
if [ -n "$deleted" ]; then
  echo "ゴールデンのファイルが消えた・改名されています（追記だけにしてください）:"
  echo "$deleted"
  exit 1
fi
# フィクスチャ（他のツールで作った入力）は、消すのも変えるのも認めない（README.md は除く）
changed=$(git diff --no-renames --name-only --diff-filter=DM "$merge_base" HEAD -- crates/kura-engine/fixtures ':!crates/kura-engine/fixtures/README.md')
if [ -n "$changed" ]; then
  echo "フィクスチャが消えた・変わっています（新しい名前で足してください）:"
  echo "$changed"
  exit 1
fi
# TS と共有するテストベクタ（描画の規則など）も、消すのも変えるのも認めない
changed=$(git diff --no-renames --name-only --diff-filter=DM "$merge_base" HEAD -- crates/kura-engine/tests/vectors)
if [ -n "$changed" ]; then
  echo "共有のテストベクタが消えた・変わっています（規則を変えるなら操作の版を上げて、新しいファイルを足してください）:"
  echo "$changed"
  exit 1
fi
# shellcheck disable=SC2086
removed=$(git diff --no-renames --text --unified=0 "$merge_base" HEAD -- $dirs | grep -E '^-[^-]' || true)
if [ -n "$removed" ]; then
  echo "ゴールデンの既存の行が消えた・変わっています（追記だけにしてください。結果を変えるなら操作の版を上げる）:"
  echo "$removed"
  exit 1
fi
echo "ゴールデン：追記だけ（基準 $merge_base）"
