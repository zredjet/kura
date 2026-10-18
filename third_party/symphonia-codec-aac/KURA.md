# kura による変更（symphonia-codec-aac 0.6.1）

crates.io の `symphonia-codec-aac` 0.6.1 の写し（`Cargo.lock`・`.cargo-ok` は除く）に、下の 3 つの変更（表と係数の
sin・pow、帯域の中の SBR を知らせる口、kura の印）を入れたもの。ルートの `Cargo.toml`
と `fuzz/Cargo.toml` の `[patch.crates-io]` で差し替えて使う。ライセンスは元のまま MPL-2.0（`LICENSE`）。変えたファイル・
足したファイルも MPL-2.0 のままこのリポジトリで公開する。

## 変更 1：表と係数の sin・pow を OS の libm に頼らずに求める

| ファイル | 変えたところ |
|---|---|
| `src/kura_math.rs`（足した） | `sinf`・`powf`：f32 の引数を f64 にして libm クレートの `sin`・`pow` で計算し、f32 に丸める |
| `src/lib.rs` | `mod kura_math;` |
| `src/aac/window.rs` | サイン窓の `sin`（KBD 窓は f64 の四則と `sqrt` だけなので、そのまま） |
| `src/aac/ics/tns.rs` | TNS の反射係数の `sin` |
| `src/aac/ics/mod.rs` | `POW43_TABLE`（x^(4/3)）・`NORMAL_SCF_TABLE`（2^(x/4)）・`INTENSITY_SCF_TABLE`（0.5^(x/4)）の `powf` |
| `src/aac/codebooks.rs` | 符号表の値の逆量子化 `iquant` の `powf` |
| `src/aac/ics/pulse.rs` | pulse の `iquant`・`requant` の `powf`（`requant` が `bval` でなく `val` を使う上流の形はそのまま） |
| `Cargo.toml` | 依存に `libm = "=0.2.16"`（`default-features = false`）を足す。`Cargo.toml.orig` は上流のまま |

`sqrt`（IEEE 754 で正しく丸めると決まっている）はそのまま。

### 理由

kura は AAC を読んだ値を、編集の版の元として記録する（`docs/editing.md` §8・§9）。std の `f32::sin`・`f32::powf` は OS の
libm を呼び、値が OS の版・CPU で 1 ulp 揺れる。手元の macOS 26 では、arm64 と x86_64（Rosetta）で同じ m4a・ADTS を読んだ
値が実際に違った（フィクスチャ 3 本で、88,200〜92,160 個の標本のうち 972〜1,118 個、最大 1.2e-7）。原因は表で、正しく
丸めた値（f64 で計算して丸めたもの）と比べて、OS の arm64 は POW43 の表の 8,192 個のうち 59 個・サイン窓（1,024 点）の
9 個が 1 ulp ずれ、x86_64 はどちらも 0 個だった。

libm クレートの f32 の関数をそのまま使うと、環境には依らなくなるが、`powf`（FreeBSD の e_powf.c）は POW43 の表の
756 個で正しい丸めから 1 ulp ずれる。そこで f32 の引数を f64 にして libm クレート（純 Rust。kura-engine と同じ版。
`arch` の feature を外す）の f64 の関数で計算し、f32 に丸める（ほぼ正しく丸めた値になる）。

### 確かめたこと（macOS 26・Apple M4 Max）

- 新しい値は arm64 と x86_64（Rosetta）でビット単位で同じ（フィクスチャ 3 本。kura-engine の `tests/codec_golden.rs` の
  デコーダのゴールデン `m4a-aac-symphonia-dec@2`・`aac-adts-symphonia-dec@2` を両方で照らす）。
- 新しい値は、手を入れる前の x86_64 の値と同じで、手を入れる前の arm64 の値とは上の 972〜1,118 個だけ違う。
- ffmpeg・afconvert との照合（`tests/fixtures.rs` の `lossy_audio_matches_other_decoders`。信号/誤差 10^6 以上、長さが
  ちょうど同じ）は今までどおり通る。
- FMA・SIMD の経路がない：`mul_add`・`std::arch` を使わない。MDCT・FFT は symphonia-core の写し（`opt-simd` を使わない）。
- 上流のこのクレートのテスト（`tests/tests.rs`）が通る。

kura のデコーダの識別子は、この変更で `m4a-aac-symphonia-dec/1` → `/2`、`aac-adts-symphonia-dec/1` → `/2` に上げた
（`reproducible()` が真になる。公開前なので旧版は残さない）。

## 変更 2：帯域の中の SBR の拡張を見つけたことを知らせる口（段 9 の検証）

| ファイル | 変えたところ |
|---|---|
| `src/aac/mod.rs` | `AacDecoder` に `kura_sbr_seen: bool` を足し、`decode_ga` の fill 要素（`ID_FIL`）で拡張の種類が `EXT_SBR_DATA`（0xd）・`EXT_SBR_DATA_CRC`（0xe）のとき真にする（上流が `asc.sbr_present` を立てる同じ所）。公開の `AacDecoder::sbr_seen()` で返す。`reset` でも戻さない |

デコードの経路（ビットの読み方・出す標本）は変えない。知らせるだけ。

### 理由

このデコーダは SBR（HE-AAC の帯域の複製）を持たない。HE-AAC を渡すと、断らずに LC の芯だけを半分のレートで読む。
HE-AAC の印が AudioSpecificConfig にある（明示の印。オブジェクトの種類 5・29、同期の拡張の sbrPresentFlag）なら、
kura-engine はデコーダを作る前に ASC を見て断れる（`codec/symph.rs` の `is_he_aac`）。しかし、ADTS の HE-AAC はいつも
暗黙の印（ADTS の見出しは芯の LC・22,050 Hz としか言わない）で、ASC に印のない m4a（ADTS から写したものなど）も同じ。
これはビットストリームの中（各フレームの fill 要素の SBR の拡張）を見ないと見分けられず、kura の固定の段は
`aac-adts-symphonia-dec/2` で 22,050 Hz の値を成功として返し、SBR を読める後の段（AudioToolbox・ffmpeg）に届かなかった
（段 9 の検証の T5）。上流もその場所で SBR を見つけている（`asc.sbr_present = true`）が、外から聞く口がない。そこで
口を足し、kura-engine（`codec/lossy.rs`）が、形を決めるときに先頭の 16 パケットをデコードしてこの口に聞き、読むときも
全部のパケットのあとに聞いて、SBR があれば `Unsupported` を返して後の段へ回す。

### 確かめたこと（macOS 26・Apple M4 Max）

- 段 9 の検証で作った AAC で数えた：afconvert の HE-AAC・HE-AAC v2（`-d aach`・`aacp`。ADTS・m4a、11 本）は、どれも
  最初のパケットから知らせる（ffmpeg の `aac_at` の HE-AAC の ADTS、ADTS から写した印のない m4a も）。afconvert・
  ffmpeg の AAC-LC（ADTS・m4a、8〜96 kHz、1〜2 チャンネル、CBR・VBR、TNS・PNS・intensity。68 本）は最後まで知らせない。
- kura-engine の `tests/symphonia_copies.rs`：HE-AAC の ADTS（`fixtures/aac-he.aac`）・ASC に印のない m4a
  （`aac-he-implicit.m4a`）・LC のあとに HE-AAC をつないだ ADTS（`aac-lc-then-he.aac`。24 パケット目から）で知らせ、
  AAC-LC のフィクスチャでは知らせない。固定の段はこれらの HE-AAC を断り、後の段が読む。
- AAC-LC の値は変わらない：デコーダのゴールデン（`m4a-aac-symphonia-dec@2`・`aac-adts-symphonia-dec@2`）と
  `tests/fixtures.rs` の照合が今までどおり通る（デコーダの識別子は上げない）。
- 写しでこの口が真にならないようにすると（`kura_sbr_seen = true` を消す）、上のテストが落ちる。
- 上流のこのクレートのテスト（`tests/tests.rs`）が通る。

## 変更 3：kura の印 `KURA_PATCHED`（段 9 の検証）

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

不具合の修正ではなく kura の要件（環境に依らない値。HE-AAC を後の段へ回すための口と、写しの印）なので、上流には
報告しない。上流の版を上げるときは、この写しを
作り直して同じ差し替えを当て、kura のデコーダのゴールデンで値が変わらないことを確かめる（変わるならデコーダの識別子を
上げる）。
