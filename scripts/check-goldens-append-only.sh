#!/bin/sh
# 編集のゴールデンは追記だけ（docs/editing.md §8.5、ADR-136）。
# 基準のコミットからの差分で、crates/kura-engine/goldens/・goldens-codecs/ の既存の行が消えた・変わったものがあれば失敗する。
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
removed=$(git diff --unified=0 "$merge_base" HEAD -- crates/kura-engine/goldens crates/kura-engine/goldens-codecs | grep -E '^-[^-]' || true)
if [ -n "$removed" ]; then
  echo "ゴールデンの既存の行が消えた・変わっています（追記だけにしてください。結果を変えるなら操作の版を上げる）:"
  echo "$removed"
  exit 1
fi
echo "ゴールデン：追記だけ（基準 $merge_base）"
