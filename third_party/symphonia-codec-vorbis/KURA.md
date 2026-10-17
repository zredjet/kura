# kura による変更（symphonia-codec-vorbis 0.6.1）

crates.io の `symphonia-codec-vorbis` 0.6.1 の写しに、次の 1 か所だけ手を入れたもの。ルートの `Cargo.toml` の
`[patch.crates-io]` で差し替えて使う。ライセンスは元のまま MPL-2.0（`LICENSE`）。変えたファイルも MPL-2.0 のまま
このリポジトリで公開する。

## 変更

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

## 上流

上流（[pdeljanov/Symphonia](https://github.com/pdeljanov/Symphonia)）への報告はユーザーが行う。報告したら issue の URL を
下の「issue」に書く。直った版が出たら、この写しと `[patch.crates-io]`（ワークスペースと `fuzz/`）を外し、
Cargo.toml の版を上げる（読む値が変わるならデコーダの版も上げる）。

- issue：（未報告）

### 報告の案文

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
