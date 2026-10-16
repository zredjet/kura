---
paths:
  - "crates/kura-engine/**"
---

# 操作エンジン（kura-engine）のルール

- 仕様は `docs/editing.md` に従う。特に「同じ文書・同じ元なら同じ結果」（§8）と「元に対する累積の文書」（§4.2）を崩さない。
- kura-engine は OS・DB・Tauri を知らない。`#[cfg(target_os = …)]` を書かず、macOS の機能は kura-mac が trait を実装して注入する。Linux でも `cargo test -p kura-engine` が通ること。
- 決定性の規則（`crates/kura-engine/clippy.toml` で強制）：
  - 超越関数は `crate::math` だけ（OS の libm を呼ばない）。
  - `HashMap`・`HashSet` を使わない。出力に効く順序は `BTreeMap` か並べた `Vec` で決める。
  - 並列は `determinism::par_blocks` だけ（rayon のほかの口・`std::thread` は禁止）。浮動小数の総和は `sum_blocks`（凍結した `SUM_BLOCK` ずつの部分和を順に足す）。並列の分け方（`TILE`・`BAND_ROWS`・`AUDIO_BLOCK`）で結果が変わらないように書く。
  - 浮動小数の max・min は `math::max`・`min`（±0 と NaN の扱いを決めたもの）。`sort_unstable*`・`binary_search*` は使わない。
  - 乱数は ChaCha8 だけ。種は `determinism::seed`、生成器は `ApplyCx::rng_at`（絶対位置：画像は行・音声はチャンネルを流れにして位置へシーク）。タイル・ブロック・フレームの番号を流れに使わない。
  - 描画の中で時刻・環境変数を読まない。`unsafe` は使わない（`forbid`）。
  - 座標の計算はあふれを確かめる（`checked_*`・`saturating_*`。あふれたら範囲の外として落とす）。
  - 結果に効く依存は Cargo.toml で `=` で版を固定する。SIMD の経路で値が変わりうる依存は SIMD の feature を外す（`scripts/check-engine-features.sh` が確かめる）。
- 操作を足すときは `ops/<分類>/<名前>.rs` を 1 つ作り（`infer`・`apply`・`examples`・テスト）、`ops/mod.rs` の `register_ops!` に 1 行足す。メタデータは自分で写さず、`SpaceMap`・`TimeMap` を申告する。メタデータを書く操作（`MetaAccess::Write`。設定する操作と、格子を書く・消す分割・結合だけ）が座標を写すときは、`mapping::map_image_meta_for(cx.rules, …)` で文書の規則の写し方を使う。引数の型は入れ子のどの層でも知らない項目を拒む（`deny_unknown_fields`。メタデータの型を引数に流用しない）。描くときのエラーは、実行器がノードを付ける。
- 結果を変える修正は操作の版を上げ、旧版を `legacy` に残す。引数の形だけの変更は `migrate` で移し、旧版のゴールデンの例が同じ値になることをテストで確かめる。
- ゴールデン（`goldens/<操作>/v<版>.jsonl`）は追記だけ。既存の行を書き換えない。例（名前・引数・入力）は変えずに、新しい名前の例を足す。
- 文書の型（doc.rs）に項目を足したら `SCHEMA` を上げ、`SCHEMA_STEPS` に移行を足す（文書の型は知らない項目を受け付けない）。
- 全体の規則（メタデータの写し方・丸めなど）を変えるときは `RULES` を上げ、古い規則の実装を残す（`render` が文書の `rules` で選ぶ）。
- 外から来たファイルを読むコードは、確保の前にヘッダから大きさを見積もって `Limits` と予算で判断し、`try_reserve_exact` で確保する。展開は上限付きの領域にだけ行う。第三者のデコーダの境界では `catch_unwind` する。
- スタックのあふれは `catch_unwind` で受け止められない（プロセスごと落ちる）。入れ子の深さだけ再帰する第三者のパーサー（roxmltree・usvg など）に渡す前に、生のバイト列で深さを数えて断る。自前の木のたどりも再帰させない（明示の積み上げで）。深さの上限は、2 MiB のスタックのデバッグのビルドで余裕があるように決め、ビルドで変えない。
- 新しいパーサーには、壊れた入力のテスト（ChaCha8 で作る）とファズの対象（`tests/fuzz_smoke.rs` の `targets()` と `fuzz/fuzz_targets/`）を足す。外から来たバイト列を `&str` の位置で切らない（多バイト文字の途中で panic する。走査はバイト列で）。自前の走査のループは、どの枝でも必ず 1 バイト以上進める（ファズで、読み飛ばしの枝が進まない無限ループを見つけた）。第三者のパーサー・描画器に渡す前の見積もりは、その実装が実際に確保する大きさ・回る数に合わせ、キャンバスに比べた相対ではなく絶対の予算で断る。自前のエンコーダで作れない形は、他のツールで作ったフィクスチャ（`fixtures/`。作り方は README）で他の実装と照らす。フィクスチャは消さない・変えない。
- Rust と TS で同じ規則を実装するもの（描画の筆・点の列の詰め方など）は、共有のテストベクタ（`tests/vectors/`）で照らす。ベクタは消さない・変えない（規則を変えるなら操作の版を上げて新しいファイルを足す）。
- 引数によって整数だけで済む操作は `numeric_for` で `Exact` を返す（その例のゴールデンはすべてのターゲットで照らす）。
- コーデックは読み書きで別の識別子に約束の版を持つ（`png-rs-dec/1`・`png-rs-enc/1`）。書くバイト列が変わる変更はエンコーダの版を上げる。読む値は変えない（変えるならデコーダの版を上げ、旧版を `add_legacy_decoder` で残す）。`goldens-codecs/` は追記だけ。
- 受け入れテスト E1〜E30（docs/editing.md §17）は、その段で実装するものを先に失敗する形で書く。
