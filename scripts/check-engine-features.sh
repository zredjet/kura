#!/bin/sh
# kura-engine の結果に効く依存で、ターゲットや実装で値・バイト列を変えうる feature が有効になっていないかを確かめる
# （docs/editing.md §8.2）。エンジンを動かすバイナリ（kura-devtools・アプリ）でも、ワークスペースの他のクレートと
# feature が合わさって解決されるので、パッケージごとに見る。
# - zune-jpeg の neon・x86・portable_simd（既定の feature に入っている）、jpeg-encoder の simd（SIMD の経路）
# - flate2 の zlib 系（miniz_oxide より優先されて、PNG の書くバイト列が変わる）、png の zlib-rs
# - symphonia-core の opt-simd（rustfft。AAC・Vorbis の値が変わる）、symphonia-metadata の id3 系（スタックのあふれ）、
#   nanomp3 の SIMD（wide）。md-5 は FLAC の MD5 を書くだけで、値はどの実装でも同じなので見張らない
set -eu

check() {
  pkg="$1"
  crate="$2"
  pattern="$3"
  tree=$(cargo tree -p "$pkg" -e features,normal,dev --target all -i "$crate" 2>/dev/null || true)
  printf '%s\n' "$tree" | grep -E "$crate feature \"($pattern)\"" | sed "s/^/[$pkg] /" || true
}

bad=""
for pkg in kura-engine kura-devtools kura-app; do
  if ! cargo tree -p "$pkg" --depth 0 >/dev/null 2>&1; then
    continue
  fi
  bad="$bad$(check "$pkg" zune-jpeg 'default|neon|x86|portable_simd')"
  bad="$bad$(check "$pkg" jpeg-encoder 'simd')"
  bad="$bad$(check "$pkg" flate2 'zlib|zlib-default|zlib-ng|zlib-ng-compat|zlib-rs|cloudflare_zlib|any_zlib|any_c_zlib')"
  bad="$bad$(check "$pkg" png 'zlib-rs')"
  bad="$bad$(check "$pkg" symphonia-core 'opt-simd|opt-simd-sse|opt-simd-avx|opt-simd-neon')"
  bad="$bad$(check "$pkg" symphonia-metadata 'id3v1|id3v2|ape|riff-id3')"
  bad="$bad$(check "$pkg" nanomp3 'default|wide')"
done
if [ -n "$bad" ]; then
  echo "kura-engine の結果を変えうる feature が有効になっています:"
  echo "$bad"
  exit 1
fi
echo "kura-engine の依存：SIMD・別の圧縮の実装の feature なし（kura-engine・kura-devtools・kura-app）"
