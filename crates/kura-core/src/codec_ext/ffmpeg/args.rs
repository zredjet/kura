//! ffmpeg・ffprobe の引数を組む純粋な関数。引数は識別子の約束に入る（変えたら識別子の版を上げる。`tests` の引数の
//! ゴールデンが見張る）。
//!
//! 守り（docs/editing.md §13 の安全）：
//! - プロトコルは入力・出力ごとに `-protocol_whitelist` で 1 つに絞る。読み取り（[`probe`]・[`decode`]）は入力が `file`
//!   （パスは `file:<絶対パス>`。`:` を含む名前をプロトコルと読ませない）、[`decode`] の出力が `pipe`（標準出力の WAV）。
//!   MP3 の書き出し（[`mp3_encode`]）は入力が `pipe`（標準入力の f32le）、出力が `file`（作業フォルダの一時ファイル）。
//!   LAME の版を調べる書き出し（[`lame_probe`]。検出のときだけ）は入力・出力とも `pipe`。
//! - 分離器とデコーダを `-f`・`-c:a` で強制し（推測させない）、同じ名前を `-format_whitelist`・`-codec_whitelist` に置く
//!   （HLS・concat などは名前が合わないので開けない）。
//! - 読み取りの出力は `pcm_f32le` の WAV を標準出力へ（ヘッダから実際のレート・チャンネルを読む）。メタデータ・章は写さず、
//!   `+bitexact` で版の文字列を書かない。
//! - 書き出しは入力と出力の `-ar`・`-ch_layout` を同じにし `-sample_fmt fltp` を明示する（明示しないと ffmpeg は 96 kHz を
//!   48 kHz に、3 チャンネルを 2 チャンネルに黙って変える）。既定の値も明示する（ffmpeg の既定が変わっても同じ約束で
//!   呼ぶため）。ID3 は書かない。出力はシークできるファイル（Xing/LAME のフレームを書かせ、gapless の長さを残す）。

use std::ffi::OsString;
use std::path::Path;

use super::probe::SHOW_ENTRIES;

fn os(args: &[&str]) -> Vec<OsString> {
    args.iter().map(OsString::from).collect()
}

/// `file:<絶対パス>`。
pub fn file_url(path: &Path) -> OsString {
    let abs = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    let mut s = OsString::from("file:");
    s.push(abs.as_os_str());
    s
}

/// ffprobe：最初の音声のストリームの形とタグ（JSON）。`decoders` は ffprobe が形を調べるときに開いてよいデコーダ。
pub fn probe(demuxer: &str, decoders: &[&str], path: &Path) -> Vec<OsString> {
    let mut a = os(&[
        "-hide_banner",
        "-v",
        "error",
        "-protocol_whitelist",
        "file",
        "-format_whitelist",
        demuxer,
    ]);
    if !decoders.is_empty() {
        a.push("-codec_whitelist".into());
        a.push(decoders.join(",").into());
    }
    a.extend(os(&[
        "-f",
        demuxer,
        "-select_streams",
        "a:0",
        "-show_entries",
        SHOW_ENTRIES,
        "-of",
        "json",
    ]));
    a.push(file_url(path));
    a
}

/// ffmpeg：最初の音声を `decoder` で読み、`pcm_f32le` の WAV を標準出力へ。
pub fn decode(demuxer: &str, decoder: &str, path: &Path) -> Vec<OsString> {
    let mut a = os(&[
        "-hide_banner",
        "-nostdin",
        "-nostats",
        "-loglevel",
        "level+warning",
        "-protocol_whitelist",
        "file",
        "-format_whitelist",
        demuxer,
        "-codec_whitelist",
        decoder,
        "-f",
        demuxer,
        "-c:a",
        decoder,
        "-i",
    ]);
    a.push(file_url(path));
    a.extend(os(&[
        "-map",
        "0:a:0",
        "-map_metadata",
        "-1",
        "-map_chapters",
        "-1",
        "-fflags",
        "+bitexact",
        "-c:a",
        "pcm_f32le",
        "-protocol_whitelist",
        "pipe",
        "-f",
        "wav",
        "pipe:1",
    ]));
    a
}

/// MP3 のビットレートの決め方（`libmp3lame`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mp3Rate {
    /// 固定（`-b:a <kbps>k`）。
    Cbr(u32),
    /// 品質で決める可変（LAME の `-V`。`-q:a <0〜9>`）。
    Vbr(u8),
}

/// チャンネルの並びの名前（MP3 は 1・2 チャンネルだけ）。
pub fn layout_name(channels: u16) -> Option<&'static str> {
    match channels {
        1 => Some("mono"),
        2 => Some("stereo"),
        _ => None,
    }
}

/// ffmpeg：標準入力の f32le（インターリーブ）を MP3 にして `out` に書く。
pub fn mp3_encode(rate: u32, layout: &str, mode: Mp3Rate, out: &Path) -> Vec<OsString> {
    let rate = rate.to_string();
    let mut a = os(&[
        "-hide_banner",
        "-nostats",
        "-loglevel",
        "level+warning",
        "-protocol_whitelist",
        "pipe",
        "-f",
        "f32le",
        "-ar",
        &rate,
        "-ch_layout",
        layout,
        "-i",
        "pipe:0",
        "-map",
        "0:a:0",
        "-map_metadata",
        "-1",
        "-map_chapters",
        "-1",
        "-fflags",
        "+bitexact",
        "-flags:a",
        "+bitexact",
        "-c:a",
        "libmp3lame",
    ]);
    match mode {
        Mp3Rate::Cbr(kbps) => a.extend(os(&["-b:a", &format!("{kbps}k")])),
        Mp3Rate::Vbr(q) => a.extend(os(&["-q:a", &q.to_string()])),
    }
    a.extend(os(&[
        "-compression_level",
        "3",
        "-reservoir",
        "1",
        "-joint_stereo",
        "1",
        "-ar",
        &rate,
        "-ch_layout",
        layout,
        "-sample_fmt",
        "fltp",
        "-id3v2_version",
        "0",
        "-write_id3v1",
        "0",
        "-write_xing",
        "1",
        "-protocol_whitelist",
        "file",
        "-f",
        "mp3",
        "-y",
    ]));
    a.push(file_url(out));
    a
}

/// 版（`-version`）。
pub fn version() -> Vec<OsString> {
    os(&["-hide_banner", "-version"])
}

/// 一覧（`-encoders`・`-decoders`・`-demuxers`・`-muxers`）。
pub fn listing(what: &str) -> Vec<OsString> {
    os(&["-hide_banner", &format!("-{what}")])
}

/// エンコーダの説明（`-h encoder=<名前>`。対応するレートなど）。
pub fn encoder_help(name: &str) -> Vec<OsString> {
    os(&["-hide_banner", "-h", &format!("encoder={name}")])
}

/// LAME の版を調べるための書き出し（標準入力の 0.2 秒の無音を標準出力の MP3 へ。LAME は空いたビットに版の文字列を詰める）。
pub fn lame_probe() -> Vec<OsString> {
    os(&[
        "-hide_banner",
        "-nostats",
        "-loglevel",
        "level+warning",
        "-protocol_whitelist",
        "pipe",
        "-f",
        "f32le",
        "-ar",
        "44100",
        "-ch_layout",
        "mono",
        "-i",
        "pipe:0",
        "-map",
        "0:a:0",
        "-map_metadata",
        "-1",
        "-fflags",
        "+bitexact",
        "-flags:a",
        "+bitexact",
        "-c:a",
        "libmp3lame",
        "-b:a",
        "128k",
        "-ar",
        "44100",
        "-ch_layout",
        "mono",
        "-sample_fmt",
        "fltp",
        "-id3v2_version",
        "0",
        "-write_id3v1",
        "0",
        "-protocol_whitelist",
        "pipe",
        "-f",
        "mp3",
        "pipe:1",
    ])
}

/// LAME の版を調べる入力（0.2 秒・44.1 kHz・モノラルの無音の f32le）。
pub const LAME_PROBE_BYTES: usize = 8_820 * 4;

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};
    use std::path::PathBuf;

    use super::*;
    use crate::codec_ext::ffmpeg::encode;
    use crate::codec_ext::ffmpeg::env::{Detected, parse_version};
    use crate::codec_ext::ffmpeg::{KINDS, env};

    /// 引数のゴールデンの置き場所（識別子ごとに 1 ファイル。追記だけ：識別子の版を上げたら新しいファイルを足し、古い
    /// ファイルは消さない）。
    fn golden_dir() -> PathBuf {
        PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/goldens-ffmpeg-args"))
    }

    /// `opus-ffmpeg-dec/1` → `opus-ffmpeg-dec.v1.txt`。
    fn file_name(id: &str) -> String {
        match id.rsplit_once('/') {
            Some((name, v)) => format!("{name}.v{v}.txt"),
            None => format!("{id}.txt"),
        }
    }

    fn block(out: &mut String, title: &str, args: &[OsString]) {
        out.push_str("# ");
        out.push_str(title);
        out.push('\n');
        for a in args {
            out.push_str(&a.to_string_lossy());
            out.push('\n');
        }
        out.push('\n');
    }

    /// どのデコーダもある ffmpeg（ゴールデンが手元の ffmpeg の構成によらないように）。
    fn all_decoders() -> Detected {
        let decoders: BTreeSet<String> = KINDS
            .iter()
            .flat_map(|k| {
                k.codecs
                    .iter()
                    .flat_map(|(_, c)| c.iter().map(|s| s.to_string()))
            })
            .collect();
        Detected {
            info: parse_version(env::tests::VERSION_902).unwrap(),
            ffprobe_version: "9.0.2".into(),
            decoders,
            encoders: BTreeSet::new(),
            demuxers: BTreeSet::new(),
            muxers: BTreeSet::new(),
            lame_rates: Vec::new(),
            lame_version: None,
            stamps: Vec::new(),
        }
    }

    /// 識別子ごとの代表の引数（読み取り：ffprobe と、中身ごとのデコーダの候補すべて。書き出し：レートの 3 つの版 × 1・2
    /// チャンネル × CBR の既定・VBR の既定）。
    fn cases() -> BTreeMap<&'static str, String> {
        let d = all_decoders();
        let mut out = BTreeMap::new();
        for k in &KINDS {
            let path = PathBuf::from(format!("/library/files/ab/abcdef0123.{}", k.format));
            let mut text = String::new();
            block(
                &mut text,
                "probe",
                &probe(k.demuxer, &k.probe_decoders(&d), &path),
            );
            for (codec, cands) in k.codecs {
                for dec in *cands {
                    block(
                        &mut text,
                        &format!("decode {codec} {dec}"),
                        &decode(k.demuxer, dec, &path),
                    );
                }
            }
            out.insert(k.id, text);
        }
        let mut text = String::new();
        let tmp = PathBuf::from("/library/staging/codec/ffenc-1-0.mp3");
        for rate in [44_100, 22_050, 8_000] {
            for ch in [1u16, 2] {
                for opts in [serde_json::json!({}), serde_json::json!({ "mode": "vbr" })] {
                    let mode = encode::settings(rate, &opts).unwrap();
                    let layout = layout_name(ch).unwrap();
                    block(
                        &mut text,
                        &format!("encode {rate} {layout} {opts}"),
                        &mp3_encode(rate, layout, mode, &tmp),
                    );
                }
            }
        }
        out.insert(encode::ID, text);
        out
    }

    /// 引数は識別子の約束：変えたら識別子の版を上げる（ゴールデンが合わなければ落ちる）。新しい識別子のゴールデンは
    /// `KURA_BLESS_FFMPEG_ARGS=1` で作る（あるファイルは書き換えない）。
    #[test]
    fn arguments_are_frozen_per_codec_id() {
        let bless = std::env::var_os("KURA_BLESS_FFMPEG_ARGS").is_some_and(|v| v == "1");
        let dir = golden_dir();
        for (id, text) in cases() {
            let file = dir.join(file_name(id));
            match std::fs::read_to_string(&file) {
                Ok(old) => assert!(
                    old == text,
                    "{id} の引数が変わりました。識別子の版を上げて、新しいゴールデンを足してください（古いファイルは消さない）\n--- 記録\n{old}\n--- 今\n{text}"
                ),
                Err(_) if bless => {
                    std::fs::create_dir_all(&dir).unwrap();
                    std::fs::write(&file, text).unwrap();
                }
                Err(e) => panic!(
                    "{id} の引数のゴールデン {} がありません（{e}）。KURA_BLESS_FFMPEG_ARGS=1 で作ってください",
                    file.display()
                ),
            }
        }
    }

    /// 守りの引数が必ずある（ゴールデンとは別に、意味で確かめる）。
    #[test]
    fn guards_are_always_present() {
        let has = |a: &[OsString], pair: [&str; 2]| {
            a.windows(2).any(|w| w[0] == pair[0] && w[1] == pair[1])
        };
        let p = Path::new("/a/b:c.opus");
        let d = decode("ogg", "libopus", p);
        for pair in [
            ["-protocol_whitelist", "file"],
            ["-format_whitelist", "ogg"],
            ["-codec_whitelist", "libopus"],
            ["-f", "ogg"],
            ["-c:a", "libopus"],
            ["-i", "file:/a/b:c.opus"],
            ["-map_metadata", "-1"],
            ["-fflags", "+bitexact"],
            ["-protocol_whitelist", "pipe"],
        ] {
            assert!(has(&d, pair), "{pair:?}");
        }
        assert!(d.iter().any(|a| a == "-nostdin"));
        let pr = probe("ogg", &["libopus", "opus"], p);
        assert!(has(&pr, ["-codec_whitelist", "libopus,opus"]) && has(&pr, ["-f", "ogg"]));
        assert_eq!(
            pr.last().map(|a| a.to_string_lossy().into_owned()),
            Some("file:/a/b:c.opus".into())
        );
        let e = mp3_encode(48_000, "stereo", Mp3Rate::Cbr(320), Path::new("/w/o.mp3"));
        let ars: Vec<usize> = e
            .iter()
            .enumerate()
            .filter(|(_, a)| *a == "-ar")
            .map(|(i, _)| i)
            .collect();
        assert_eq!(ars.len(), 2);
        assert!(ars.iter().all(|&i| e[i + 1] == "48000"));
        for pair in [
            ["-ch_layout", "stereo"],
            ["-sample_fmt", "fltp"],
            ["-b:a", "320k"],
            ["-id3v2_version", "0"],
            ["-write_id3v1", "0"],
            ["-write_xing", "1"],
            ["-flags:a", "+bitexact"],
            ["-protocol_whitelist", "file"],
        ] {
            assert!(has(&e, pair), "{pair:?}");
        }
        assert_eq!(
            e.last().map(|a| a.to_string_lossy().into_owned()),
            Some("file:/w/o.mp3".into())
        );
        assert_eq!(layout_name(3), None);
        // 相対パスは絶対パスにする
        let rel = file_url(Path::new("x.opus"));
        assert!(Path::new(rel.to_string_lossy().trim_start_matches("file:")).is_absolute());
    }
}
