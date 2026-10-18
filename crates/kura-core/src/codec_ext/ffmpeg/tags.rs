//! ffprobe のタグを kura のメタデータ（タグ・ループ点）にする（純粋な関数）。
//!
//! 規則は固定の段の読み取り（kura-engine の `codec/symph.rs` の `KNOWN_TAGS`・`comment_name`・`meta_from_tags`、WAV の
//! INFO、AIFF の NAME・AUTH）と同じにする。kura-engine はその表を公開していないので、ここに同じ表を持つ（公開されたら
//! 置き換える）。ffmpeg は読むときに名前を一般の名前に直すので、その分を戻してから照らす：
//!
//! - Vorbis コメント（Ogg の Opus・FLAC・Speex）：ffmpeg は ALBUMARTIST → `album_artist`、TRACKNUMBER → `track`、
//!   DISCNUMBER → `disc`、DESCRIPTION → `comment` に直す（ほかの名前は元の大文字小文字のまま）。前の 3 つは元の名前に戻し、
//!   知らない名前は `vorbis:<大文字の名前>` で運ぶ（Vorbis コメントとして書ける名前だけ）。同じ名前の複数の値は、ffmpeg が
//!   `;` でつないで 1 つにする（固定の段は最初の値だけ。ここは ffmpeg が渡したまま）。
//! - WAV の INFO：ffmpeg は ISFT を `encoder` にする（固定の段は `software`）。AIFF の AUTH は `author`（固定の段は
//!   `artist`）。
//! - ループ点：LOOPSTART・LOOPLENGTH（大文字小文字を区別しない。最初の値）が `0 < 長さ`・`始まり + 長さ ≤ フレームの数`
//!   のときだけ。位置は出力のレート（Opus は 48 kHz）。
//! - 1 項目の値は 4,096 バイトまで（超えるものは捨てる）、合計は `max_meta_bytes`（1 項目ごとに名前と値の長さに 32 を
//!   足して引く）。

use kura_engine::limits::Limits;
use kura_engine::value::{AudioMeta, LoopRange};

/// 1 項目の値の長さの上限（固定の段と同じ）。
const TEXT_MAX: usize = 4_096;
/// 1 項目ごとに予算から引く重さ（固定の段と同じ）。
const ITEM_COST: usize = 32;

/// 既知のタグ（kura の名前・Vorbis コメントの名前・同じ名前にする別名）。固定の段の `KNOWN_TAGS` と同じ表。
const KNOWN: [(&str, &str, &[&str]); 7] = [
    ("title", "TITLE", &[]),
    ("artist", "ARTIST", &[]),
    ("album", "ALBUM", &[]),
    ("comment", "COMMENT", &["DESCRIPTION"]),
    ("date", "DATE", &[]),
    ("genre", "GENRE", &[]),
    ("copyright", "COPYRIGHT", &[]),
];

/// ffmpeg が Vorbis コメントの名前を直したものを元に戻す（`ff_vorbiscomment_metadata_conv` の逆）。
const VORBIS_BACK: [(&str, &str); 3] = [
    ("album_artist", "ALBUMARTIST"),
    ("track", "TRACKNUMBER"),
    ("disc", "DISCNUMBER"),
];

/// どの入れ物のタグか（名前の扱いが違う）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TagStyle {
    /// Vorbis コメント（Ogg）。知らない名前も `vorbis:<KEY>` で運ぶ。
    Vorbis,
    /// WAV の INFO（ffmpeg の `encoder` は固定の段の `software`）。
    Wav,
    /// AIFF の NAME・AUTH・`(c) `・ANNO（ffmpeg の `author` は固定の段の `artist`）。
    Aiff,
    /// そのほか（m4a・ADTS・MP3）。既知の名前だけ。
    Plain,
}

/// 名前の扱い。
enum Name {
    LoopStart,
    LoopLength,
    Tag(String),
}

/// Vorbis コメントの名前として書ける形（空でなく、0x20〜0x7D の ASCII で `=` を含まない）。
fn vorbis_name_ok(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|b| (0x20..=0x7D).contains(&b) && b != b'=')
}

fn name_of(key: &str, style: TagStyle) -> Option<Name> {
    let key = match style {
        TagStyle::Vorbis => VORBIS_BACK
            .iter()
            .find(|(ff, _)| *ff == key)
            .map_or(key, |(_, orig)| orig),
        TagStyle::Wav if key == "encoder" => return Some(Name::Tag("software".into())),
        TagStyle::Aiff if key == "author" => return Some(Name::Tag("artist".into())),
        _ => key,
    };
    let upper = key.to_ascii_uppercase();
    match upper.as_str() {
        "LOOPSTART" => return Some(Name::LoopStart),
        "LOOPLENGTH" => return Some(Name::LoopLength),
        _ => {}
    }
    if let Some((name, _, _)) = KNOWN
        .iter()
        .find(|(_, vorbis, aliases)| upper == *vorbis || aliases.contains(&upper.as_str()))
    {
        return Some(Name::Tag((*name).to_string()));
    }
    (style == TagStyle::Vorbis && vorbis_name_ok(&upper))
        .then(|| Name::Tag(format!("vorbis:{upper}")))
}

/// タグの列（ファイルの順）をメタデータにする。`frames` は音声の長さ（ループ点の確かめ）。
pub fn meta_from_tags(
    raw: &[(String, String)],
    frames: u64,
    style: TagStyle,
    limits: &Limits,
) -> AudioMeta {
    let mut budget = limits.max_meta_bytes;
    let mut meta = AudioMeta::default();
    let (mut loop_start, mut loop_len) = (None, None);
    for (key, value) in raw {
        let name = match name_of(key, style) {
            Some(Name::LoopStart) => {
                loop_start = loop_start.or_else(|| value.trim().parse::<u64>().ok());
                continue;
            }
            Some(Name::LoopLength) => {
                loop_len = loop_len.or_else(|| value.trim().parse::<u64>().ok());
                continue;
            }
            Some(Name::Tag(name)) => name,
            None => continue,
        };
        if meta.tags.contains_key(&name) || value.len() > TEXT_MAX {
            continue;
        }
        let cost = (name.len() + value.len() + ITEM_COST) as u64;
        if cost > budget {
            budget = 0;
            continue;
        }
        budget -= cost;
        meta.tags.insert(name, value.clone());
    }
    if let (Some(s), Some(n)) = (loop_start, loop_len)
        && n > 0
        && let Some(e) = s.checked_add(n)
        && e <= frames
    {
        meta.loop_range = Some(LoopRange { start: s, end: e });
    }
    meta
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    /// ffmpeg 9.0.2 が Ogg Opus のコメントを読んだ形（ALBUMARTIST・TRACKNUMBER・DISCNUMBER・description・LOOPSTART・
    /// LoopLength・MyKey・title を書いたファイルの ffprobe の出力）から、固定の段と同じ名前になる。
    #[test]
    fn vorbis_names_match_the_pinned_reader() {
        let m = meta_from_tags(
            &raw(&[
                ("encoder", "Lavc libopus"),
                ("album_artist", "AA"),
                ("track", "3"),
                ("disc", "1"),
                ("comment", "DD"),
                ("LOOPSTART", "100"),
                ("LoopLength", "200"),
                ("MyKey", "v"),
                ("title", "T"),
                ("TITLE", "second"),
                ("bad=name", "x"),
            ]),
            300,
            TagStyle::Vorbis,
            &Limits::default(),
        );
        let tags: Vec<(&str, &str)> = m
            .tags
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        assert_eq!(
            tags,
            [
                ("comment", "DD"),
                ("title", "T"),
                ("vorbis:ALBUMARTIST", "AA"),
                ("vorbis:DISCNUMBER", "1"),
                ("vorbis:ENCODER", "Lavc libopus"),
                ("vorbis:MYKEY", "v"),
                ("vorbis:TRACKNUMBER", "3"),
            ]
        );
        assert_eq!(
            m.loop_range,
            Some(LoopRange {
                start: 100,
                end: 300
            })
        );
        // 範囲の外・長さ 0 のループ点は運ばない
        for (s, n, frames) in [("100", "201", 300), ("0", "0", 300), ("x", "5", 300)] {
            let m = meta_from_tags(
                &raw(&[("LOOPSTART", s), ("LOOPLENGTH", n)]),
                frames,
                TagStyle::Vorbis,
                &Limits::default(),
            );
            assert_eq!(m.loop_range, None, "{s} {n}");
        }
    }

    #[test]
    fn other_containers_keep_known_names_only() {
        let wav = meta_from_tags(
            &raw(&[
                ("title", "t"),
                ("encoder", "Lavf"),
                ("artist", "a"),
                ("track", "1"),
            ]),
            0,
            TagStyle::Wav,
            &Limits::default(),
        );
        let keys: Vec<&String> = wav.tags.keys().collect();
        assert_eq!(keys, ["artist", "software", "title"]);
        let aiff = meta_from_tags(
            &raw(&[("author", "x")]),
            0,
            TagStyle::Aiff,
            &Limits::default(),
        );
        assert_eq!(aiff.tags.get("artist").map(String::as_str), Some("x"));
        let m4a = meta_from_tags(
            &raw(&[
                ("major_brand", "M4A "),
                ("title", "t"),
                ("copyright", "c"),
                ("LOOPSTART", "1"),
                ("LOOPLENGTH", "1"),
            ]),
            10,
            TagStyle::Plain,
            &Limits::default(),
        );
        let keys: Vec<&String> = m4a.tags.keys().collect();
        assert_eq!(keys, ["copyright", "title"]);
        assert_eq!(m4a.loop_range, Some(LoopRange { start: 1, end: 2 }));
    }

    #[test]
    fn sizes_are_budgeted() {
        let long = "x".repeat(TEXT_MAX + 1);
        let m = meta_from_tags(
            &raw(&[("title", &long), ("artist", "a")]),
            0,
            TagStyle::Plain,
            &Limits::default(),
        );
        assert_eq!(m.tags.len(), 1);
        let limits = Limits {
            max_meta_bytes: 40,
            ..Limits::default()
        };
        // title（5）＋ 3 ＋ 32 = 40 で使い切り、次は入らない
        let m = meta_from_tags(
            &raw(&[("title", "abc"), ("artist", "a")]),
            0,
            TagStyle::Plain,
            &limits,
        );
        let keys: Vec<&String> = m.tags.keys().collect();
        assert_eq!(keys, ["title"]);
    }
}
