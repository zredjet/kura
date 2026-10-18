# kura による変更（symphonia-core 0.6.1）

crates.io の `symphonia-core` 0.6.1 の写し（`Cargo.lock`・`.cargo-ok` は除く）に手を入れたもの。ルートの `Cargo.toml` と
`fuzz/Cargo.toml` の `[patch.crates-io]` で差し替えて使う。ライセンスは元のまま MPL-2.0（`LICENSE`）。変えたファイルも
MPL-2.0 のままこのリポジトリで公開する。

## 変更：MDCT・FFT の表の cos・sin を libm クレートにする

| ファイル | 変えたところ |
|---|---|
| `src/dsp/mdct.rs` | `Imdct::new_scaled` の twiddle の表の `theta.cos()`・`theta.sin()`（f64）を `libm::cos`・`libm::sin` に |
| `src/dsp/fft/no_simd.rs` | `fft_twiddle_table!` の表（64〜65,536 点）の `angle.cos()`・`angle.sin()`（f64）を `libm::cos`・`libm::sin` に |
| `src/dsp/mdct.rs`・`src/dsp/fft/mod.rs` のテスト | 照合用の素朴な IMDCT・DFT の cos・sin も同じ関数に（値に効かない。`src/` に std の超越関数を残さないため） |
| `Cargo.toml` | 依存に `libm = "=0.2.16"`（`default-features = false`）を足す。`Cargo.toml.orig` は上流のまま |

`sqrt`（IEEE 754 で正しく丸めると決まっている）はそのまま。

### 理由

kura は symphonia の Vorbis・AAC のデコーダが読んだ値を、編集の版の元として記録する（`docs/editing.md` §8・§9）。
std の `f64::cos`・`f64::sin` は OS の libm を呼ぶので、OS の版や CPU で結果が変わりうる。そのままでは「同じファイルを
どの環境で読んでも同じ値」を約束できず、編集するたびにアンカー（読んだ値の写し。§9.4）を作る必要があった。
libm クレート（純 Rust。kura-engine の超越関数と同じ版。`arch` の feature を外して CPU 固有の実装にも頼らない）にすると、
表はどの環境でも同じになる（段 9 のユーザーの決定。symphonia-codec-aac・symphonia-codec-vorbis の写しと組み）。

FLAC・ALAC・MP4・Ogg の読み口もこのクレートを使うが、変えたのは `dsp` の表だけで、ほかの経路の値は変わらない。

### 確かめたこと（macOS 26・Apple M4 Max。arm64 と x86_64（Rosetta））

- 表の値は変わらない：MDCT の表（n = 64〜8,192 の 15,168 個）は、f64 の段では OS の libm と libm クレートで
  arm64 が 521 個、x86_64 が 1,781 個違うが、f32 に丸めた表では 0 個。
- FMA・SIMD の経路がない：`opt-simd`（rustfft）の feature を使わない（`scripts/check-engine-features.sh` が見張る。
  `no_simd.rs` だけを組む）。`mul_add` も `std::arch` もない。Rust は掛け算と足し算を黙って FMA にまとめない。
- kura-engine のテスト（`tests/codec_golden.rs` のデコーダのゴールデン、`tests/fixtures.rs` の lewton・ffmpeg・afconvert
  との照合）が arm64 と x86_64 の両方で通る。
- 上流のこのクレートのテスト（`verify_imdct`・FFT の照合）が通る。

## 変更：kura の印 `KURA_PATCHED`（段 9 の検証）

| ファイル | 変えたところ |
|---|---|
| `src/lib.rs` | 末尾に `pub const KURA_PATCHED: u32 = 1;` を足す（上流にはない） |

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

不具合の修正ではなく kura の要件（環境に依らない値）なので、上流には報告しない。上流の版を上げるときは、この写しを
作り直して同じ差し替えを当て、kura の Vorbis・AAC のデコーダのゴールデンで値が変わらないことを確かめる（変わるなら
デコーダの識別子を上げる）。
