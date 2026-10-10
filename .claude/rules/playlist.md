---
paths:
  - "crates/kura-core/src/playlist/**"
  - "crates/kura-core/src/unit/**"
  - "src/features/playlists/**"
  - "src/features/player/**"
---

# プレイリスト・ユニット・再生のルール

- 仕様は `docs/playlists.md` に従う。特に「単位（file / unit）の切り替えは非破壊」「同じシードなら同じ並び」を崩さない。
- 解決（resolve）とキュー生成（build_queue）は DB に触れない純粋関数として `kura-core::playlist` に実装し、`docs/playlists.md` の受け入れテスト T1〜T15 をすべてテストとして持つ。
- 乱数は `rand_chacha::ChaCha8Rng::seed_from_u64` だけを使う。ユニット内シャッフルのシードは仕様どおりに派生させる。
- フロントの再生コントローラは、Rust が返したキューを順に再生するだけにする。並び替えのロジックをフロントに複製しない。
- 再生イベント（`playback_report`）は仕様の「再生済みの判定」に従って送る。
