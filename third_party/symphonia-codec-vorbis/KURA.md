# kura による変更（symphonia-codec-vorbis 0.6.1）

crates.io の `symphonia-codec-vorbis` 0.6.1 の写し（`Cargo.lock`・`.cargo-ok` は除く）に、次の 4 つの変更を入れたもの。
ルートの `Cargo.toml` と `fuzz/Cargo.toml` の `[patch.crates-io]` で差し替えて使う。ライセンスは元のまま MPL-2.0
（`LICENSE`）。変えたファイル・足したファイルも MPL-2.0 のままこのリポジトリで公開する。

1. residue 0・1 の分類の上書きの修正（段 7。ADR-149。上流の不具合）
2. 超越関数を OS の libm に頼らずに求める（段 9。kura の要件）
3. `lookup1_values` を整数で求める（段 9。上流の不具合でもある）
4. kura の印 `KURA_PATCHED`（段 9 の検証。写しが組まれていることを kura-engine の組み立てで確かめる）

## 変更 1：residue 0・1 の分類の上書き（段 7）

`src/residue.rs` の `Residue::read_residue_inner_type_0_1`：各チャンネルの分類を `decode_classes` に渡すスライスを、
そのチャンネルの範囲 `[part_first + i * parts_to_read .. (i + 1) * parts_to_read]` で切る（元は終わりのない範囲）。

元のままだと、1 チャンネルあたりの partition の数が分類の codebook の次元の倍数でないとき、チャンネル `i` の最後の
分類語が次のチャンネルの先頭の分類を上書きし、その partition を違う codebook で読んで値が壊れる。residue 0・1 を
2 チャンネル以上で使うストリーム（libvorbis・aoTuV・kura の自作のエンコーダが書く 3〜8 チャンネルの uncoupled の
Vorbis）のうち、この条件に当たるものだけで起きる。当たるかどうかはエンコーダが品質ごとに選ぶ設定（residue の範囲と
分類の codebook）で決まるので、同じエンコーダ・同じチャンネル数でも、品質によって起きたり起きなかったりする。
ステレオ（residue 2）は影響しない。

確かめたこと（どれも 3 チャンネル 44.1 kHz）：

- aoTuV b6.03 の quality 0.5：元のままの symphonia は元との SNR が 10/6/6 dB、lewton と ffmpeg は 22 dB で互いに
  1e-6 以内で一致する。
- Xiph の libvorbis 1.3.7（sox で書いたもの）：quality −0.1・0.1・0.3・0.4 で起き、0・0.2・0.5〜1.0 では起きない
  （起きないものは、元のままの symphonia と ffmpeg の差が 2.4e-7 以下）。quality 0.3 では、元のままの symphonia と
  ffmpeg の差の最大がチャンネルごとに 0.5・1.7・1.2 で、lewton と ffmpeg は 3.3e-7 以内で一致する。

この変更のあとの symphonia は、どちらの組でも lewton・ffmpeg と 1e-6 以内で一致する（kura の tests/ogg_vorbis.rs の
`channels_and_rates_round_trip` も 3〜8 チャンネルで確かめる）。

## 変更 2：超越関数を OS の libm に頼らずに求める（段 9）

| ファイル | 変えたところ |
|---|---|
| `src/kura_math.rs`（足した） | `cosf`・`expf`：f32 の引数を f64 にして libm クレートの `cos`・`exp` で計算し、f32 に丸める（symphonia-codec-aac の写しと同じ決め方） |
| `src/lib.rs` | `mod kura_math;` |
| `src/window.rs` | 窓の曲線の `sin`（f64）を `libm::sin` に。`frac.sin().powi(2)` は `powi` を残さず掛け算で書く（LLVM は `powi(x, 2)` を `x * x` に展開するので値は同じ。10 万点で確かめた） |
| `src/floor.rs` | floor 0 の係数と合成の `cos`（f32）、Bark の写像の `atan`（f64）、振幅の `exp`（f32） |
| `src/codebook.rs` | `float32_unpack` の `mantissa · 2.0f32.powi(e − 788)` を `libm::ldexpf(mantissa, e − 788)` に（仕様・libvorbis の `ldexp` と同じ。2^(e−788) を f32 で表せる範囲では元と同じ値。元は LLVM の展開（`__powisf2` の呼び出しか `ldexp` への書き換え）しだいで、表せない端の値が変わりうる） |
| `Cargo.toml` | 依存に `libm = "=0.2.16"`（`default-features = false`）を足す。`Cargo.toml.orig` は上流のまま |

`sqrt`（IEEE 754 で正しく丸めると決まっている）・`floor` はそのまま。MDCT は symphonia-core の写しで libm クレートにした。

理由：kura は Vorbis を読んだ値を、編集の版の元として記録する（`docs/editing.md` §8・§9）。std の超越関数は OS の libm を
呼ぶので、OS の版・CPU で値が変わりうる。libm クレート（純 Rust。kura-engine の超越関数と同じ版。`arch` の feature を外す）
にすると、どの環境でも同じ値を読むので、アンカー（§9.4）を作らずに済む（段 9 のユーザーの決定）。kura のデコーダの
識別子は `ogg-vorbis-symphonia-dec/2` → `/3` に上げた（`reproducible()` が真になる。公開前なので旧版は残さない）。

確かめたこと（macOS 26・Apple M4 Max。arm64 と x86_64（Rosetta））：

- 値は変わらなかった：フィクスチャ 4 本（libvorbis（aoTuV）・ffmpeg が書いた 2・3・5 チャンネル）と、kura の自作の
  エンコーダの出力 120 本（8・11.025・16・22.05・32・44.1・48・96 kHz × 1・2・3・6・8 チャンネル × 品質 −1・4・10）で、
  手を入れる前の値（arm64・x86_64 とも）とダイジェストが同じ。新しい値は arm64 と x86_64 でビット単位で同じ。
  floor 0 を使うファイルはフィクスチャにない（今のエンコーダは floor 1 だけを書く）。
- kura-engine の `tests/codec_golden.rs`（デコーダのゴールデン `ogg-vorbis-symphonia-dec@3` と、自作のエンコーダの
  35 例を `/3` で読み直した値）と `tests/fixtures.rs`（lewton との照合）が arm64 と x86_64 の両方で通る。
- FMA・SIMD の経路がない：`mul_add`・`std::arch` を使わない。

## 変更 3：`lookup1_values` を整数で求める（段 9）

`src/codebook.rs` の `lookup1_values`：value^dimensions ≤ entries となる最大の value（Vorbis I §9.2.3）を、
`(entries as f32).powf(1 / dimensions).floor()` と `assert!` でなく、整数だけの二分探索（`lookup1_values_exact`）で求める。
symphonia-format-ogg の写しの同じ名前の関数（Ogg の読み口が setup ヘッダを読み飛ばすときに使う）も同じ式にした。

理由：powf で見当を付けて切り捨てると、丸めで 1 大きくなる組がある（k^d の 1 つ手前など）。そのとき上流は `assert!` で
panic し、その組は OS・CPU で違う。entries が 2^24 未満・dimensions が 1〜64 の組で数えると、切り捨てが正しくない組は
手元の arm64 の OS の libm で 637 個、x86_64 で 604 個、libm クレートの `powf` で 638 個あり、arm64 と libm クレートでも
63 個違った。kura が読む範囲（kura-engine の `vorbis_setup_bytes` が、entries が 131,072 以下・dimensions が 1〜32 の
setup ヘッダだけを通す）では、どの powf でも正しい値になる（全部の組で確かめた）ので、kura の読む値は変わらない。
libvorbis（`_book_maptype1_quantvals`）も pow の見当を整数で確かめ直し、lewton・ffmpeg も整数で求める。

確かめたこと：上流のテスト `verify_lookup1_values` と、足したテスト `kura_lookup1_values_is_exact`（素朴な数え上げと、
dimensions 1〜4 の小さい entries 全部と、k^d の前後で一致。powf で 1 大きくなった 3 組を含む）が通る。

## 変更 4：kura の印 `KURA_PATCHED`（段 9 の検証）

| ファイル | 変えたところ |
|---|---|
| `src/lib.rs` | `use window::Windows;` のあとに `pub const KURA_PATCHED: u32 = 1;` を足す（上流にはない） |

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

上流（[pdeljanov/Symphonia](https://github.com/pdeljanov/Symphonia)）への報告はユーザーが行う。報告したら issue の URL を
下の「issue」に書く。変更 1（と変更 3）が直った版が出たら、その修正は外し、Cargo.toml の版を上げる（読む値が変わるなら
デコーダの版も上げる）。変更 2・4 は kura の要件なので、上流の版を上げるときも写しを作り直して当て直す（kura のデコーダの
ゴールデンで値が変わらないことを確かめる）。

- issue（変更 1）：（未報告）
- issue（変更 3）：（未報告）

### 報告の案文（変更 1）

GitHub の issue にそのまま貼る英語の本文（件名と本文）。

件名：`Vorbis: residue type 0/1 with 2+ channels overwrites the next channel's partition classes`

````markdown
**Crate:** symphonia-codec-vorbis 0.6.1

## Summary

In `Residue::read_residue_inner_type_0_1` (src/residue.rs), the class assignments for each channel are decoded
with

```rust
decode_classes(
    code,
    parts_per_classword,
    self.setup.residue_classifications as u32,
    &mut self.part_classes[part_first + i * parts_to_read..],
);
```

The output slice is not limited to channel `i`'s region (`i * parts_to_read .. (i + 1) * parts_to_read`).
`decode_classes` uses `out.len()` to decide how many trailing classes of the last (partial) classword to drop.
When `parts_to_read` is not a multiple of the classbook's dimensions, the last classword of channel `i` therefore
writes past the end of its region and overwrites the first class entries of channel `i + 1` (which were decoded
earlier in the same pass). The affected partitions of channel `i + 1` are then decoded with the wrong VQ books,
so the decoded audio is wrong.

This only affects residue types 0 and 1 with two or more channels in the submap (type 2 uses a single vector and
is not affected). libvorbis uses residue 1 for "uncoupled" streams, i.e. mono and any channel count other than 2
(and other than its 5.1 setups), so 3-, 4-, 5-, 7- and 8-channel libvorbis/aoTuV files whose residue partition
count is not a multiple of the classbook dimension decode incorrectly (for example aoTuV b6.03 at quality 0.5, or
Xiph libvorbis 1.3.7 at quality 0.3). Whether a file is affected depends on the residue setup the encoder picks for
the chosen quality, so the same encoder and channel count can be affected at one quality and not at another.

## Reproduction

Encode a 3-channel, 44.1 kHz signal with aoTuV b6.03 at quality 0.5, or with Xiph libvorbis 1.3.7 at quality 0.3
(with Xiph 1.3.7 we saw the bug at quality -0.1, 0.1, 0.3 and 0.4, but not at 0.0, 0.2 or 0.5-1.0), and decode it
with symphonia, lewton and ffmpeg. lewton and ffmpeg agree with each other (within 1e-6), while symphonia does not.
For the aoTuV file, lewton and ffmpeg are at about 22 dB SNR per channel against the source, while symphonia's
channels 0/1/2 come out at about 10 / 6 / 6 dB SNR. For the Xiph file, symphonia's channels differ from ffmpeg's
output by up to 0.5 / 1.7 / 1.2 (in a -1.0..1.0 range).

## Fix

Limit the slice to the channel's region:

```diff
-                            &mut self.part_classes[part_first + i * parts_to_read..],
+                            &mut self.part_classes
+                                [part_first + i * parts_to_read..(i + 1) * parts_to_read],
```

With this change, symphonia's output matches lewton and ffmpeg within 1e-6 on all channels for both files above.
````

### 報告の案文（変更 3）

件名：`Vorbis/Ogg: lookup1_values() can panic because powf() rounds up to the next integer`

````markdown
**Crates:** symphonia-codec-vorbis 0.6.1, symphonia-format-ogg 0.6.1

## Summary

`lookup1_values()` (symphonia-codec-vorbis `src/codebook.rs`, and a copy in symphonia-format-ogg
`src/mappings/vorbis.rs`) computes "the greatest integer value for which [value] to the power of [dimensions] is less
than or equal to [entries]" (Vorbis I §9.2.3) as

```rust
let value = (entries as f32).powf(1.0f32 / f32::from(dimensions)).floor() as u32;
assert!(value.pow(u32::from(dimensions)) <= entries);
```

When the exact root is just below an integer, `powf` rounds up to that integer, `floor` keeps it, and the `assert!`
panics for a valid codebook. For example `entries = 1_953_124 = 125^3 - 1, dimensions = 3` (the correct value is 124,
`powf` returns 125.0), or `entries = 16_687_224, dimensions = 2` (correct 4084). Counting all `entries < 2^24` and
`dimensions` 1..=64, we found about 600 such pairs, and the exact set depends on the platform's libm (macOS arm64 and
x86_64 differ by a few dozen pairs), so whether a stream decodes depends on the machine. The format-ogg copy also
asserts `(value + 1).pow(dimensions) > entries`, which wraps in release builds for large `dimensions`.

## Fix

Compute the value with integers only (as lewton and ffmpeg do; libvorbis verifies its `pow` guess with integers), e.g.
a binary search over `0..=entries` using a saturating `u64` product that stops once it exceeds `entries`.
````
