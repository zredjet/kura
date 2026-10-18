# kura による変更（symphonia-format-ogg 0.6.1）

crates.io の `symphonia-format-ogg` 0.6.1 の写し（`Cargo.lock`・`.cargo-ok` は除く）に、下の 2 つの変更（`lookup1_values`
の整数化、kura の印）を入れたもの。ルートの
`Cargo.toml` と `fuzz/Cargo.toml` の `[patch.crates-io]` で差し替えて使う。ライセンスは元のまま MPL-2.0（`LICENSE`）。
変えたファイルも MPL-2.0 のままこのリポジトリで公開する。

## 変更 1：`lookup1_values` を整数で求める

`src/mappings/vorbis.rs` の `lookup1_values`（Ogg の読み口が Vorbis の setup ヘッダの codebook を読み飛ばすときに、
VQ の値の数を求める）：value^dimensions ≤ entries となる最大の value（Vorbis I §9.2.3）を、
`(entries as f32).powf(1 / dimensions).floor()` と 2 つの `assert!` でなく、整数だけの二分探索で求める（dimensions が
0 なら 0。symphonia-codec-vorbis の写しの `lookup1_values` と同じ式）。同じファイルに、素朴な数え上げと比べるテスト
`kura_tests::lookup1_values_is_exact` を足した。

### 理由

kura の Ogg Vorbis の読み取り（`ogg-vorbis-symphonia-dec/3`）を、どの環境でも同じ結果にするため（段 9。
symphonia-core・symphonia-codec-aac・symphonia-codec-vorbis の写しと組み）。この関数は Ogg の読み口がパケットの長さ
（グラニュールの切り取り）を決めるための setup ヘッダの読み取りで使うので、デコーダと同じく、読む値と成否に効く。

`powf` は OS の libm を呼ぶ。powf で見当を付けて切り捨てると、丸めで 1 大きくなる組（k^d の 1 つ手前など）があり、
そのとき `assert!` で panic する。その組は OS・CPU で違う（entries が 2^24 未満・dimensions が 1〜64 の組で、正しくない
切り捨ては手元の macOS 26 の arm64 で 637 個、x86_64 で 604 個。詳しくは symphonia-codec-vorbis の写しの KURA.md の
変更 3）。また 2 つめの `assert!((value + 1).pow(dimensions) > entries)` は、dimensions が大きいと release のビルドで
あふれて折り返し、正しい codebook でも panic していた。整数で求めれば、どの環境でも同じで、panic もしない。

kura が値を読む範囲（kura-engine の `vorbis_setup_bytes` が、entries が 131,072 以下・dimensions が 1〜32 の setup ヘッダ
だけを通す）では、どの powf でも正しい値になる（全部の組で確かめた）ので、kura の読む値は変わらない。範囲の外の
setup ヘッダは、この読み口を通ったあと kura-engine が断る（元は、組によってはここで panic し、kura-engine がそれを
受け止めてエラーにしていた。どちらもエラーで、値は返らない）。

### 確かめたこと

- 足したテストと、kura-engine の Ogg Vorbis のテスト（`tests/fixtures.rs`・`tests/codec_golden.rs`・`tests/ogg_vorbis.rs`・
  `tests/fuzz_smoke.rs`）が arm64 と x86_64（Rosetta）で通る。

## 変更：kura の印 `KURA_PATCHED`（段 9 の検証）

| ファイル | 変えたところ |
|---|---|
| `src/lib.rs` | `pub use demuxer::OggReader;` のあとに `pub const KURA_PATCHED: u32 = 1;` を足す（上流にはない） |

### 理由

kura-engine の `src/codec/lossy.rs` がこの印を参照する（`const _: [u32; 4] = [symphonia_core::KURA_PATCHED, …]`）。
写しの差し替え（ルートと `fuzz/` の `[patch.crates-io]`）が外れて crates.io の上流のクレートが組まれると、印がないので
組み立てが失敗する。差し替えが外れても cargo は警告しか出さず、上流の表は手元の機械では f32 に丸めると写しと同じ値に
なるので、それまではデコーダのゴールデンもほかのテストも気づけなかった（段 9 の検証で、ルートの `[patch]` から
`symphonia-core`・`symphonia-format-ogg` の行を外しても kura-engine の全テストが通ることを確かめた）。

### 確かめたこと

- リポジトリの写しで、ルートの `Cargo.toml` の `[patch.crates-io]` から 4 つの写しの行を 1 つずつ外すと、どの場合も
  `cargo check -p kura-engine` が `cannot find value KURA_PATCHED in crate …` で失敗する。`lossy.rs` の参照を外した写しでは、
  `symphonia-core`・`symphonia-codec-vorbis`・`symphonia-format-ogg` の行を外しても組み立てが通る（印がなければ気づけない）。
- `fuzz/` でも同じ：`cd fuzz && cargo check` が通り、`fuzz/Cargo.toml` の `[patch]` から 1 行外すと同じエラーで失敗する。
- 値・読み方は変わらない（定数を足しただけ）。

## 上流

上流（[pdeljanov/Symphonia](https://github.com/pdeljanov/Symphonia)）への報告はユーザーが行う（symphonia-codec-vorbis と
同じ不具合なので、symphonia-codec-vorbis の写しの KURA.md の「報告の案文（変更 3）」で両方を報告する）。直った版が
出たら、この写しと `[patch.crates-io]`（ワークスペースと `fuzz/`）を外し、Cargo.toml の版を上げる。

- issue：（未報告）
