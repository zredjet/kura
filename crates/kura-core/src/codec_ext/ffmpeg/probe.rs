//! ffprobe の JSON（`-of json`）の読み取り（純粋な関数）。出力の大きさは呼び手が上限で抑えたもの。
//!
//! タグはファイルの順のまま（ffprobe は辞書の順＝読んだ順に書く）残す。`serde_json` の `Map` は名前の順に並べ替える
//! ので、タグの表だけは順を保つ自前の読み方にする（同じ名前にまとめるタグの「先のものを採る」規則のため）。

use serde::Deserialize;
use serde::de::{Deserializer, MapAccess, Visitor};

/// 読み取りに使う ffprobe の項目（`-show_entries` の値。引数のゴールデンに入る）。
pub const SHOW_ENTRIES: &str = "stream=index,codec_name,sample_rate,channels,bits_per_raw_sample,duration_ts,time_base:stream_tags:format=format_name,duration:format_tags";

/// 最初の音声のストリームの調べた結果。
#[derive(Debug, Clone, PartialEq)]
pub struct Probe {
    /// 中身のコーデック（ffmpeg の codec id の名前。例 `opus`・`mp2`・`adpcm_ms`）。
    pub codec_name: String,
    pub sample_rate: Option<u32>,
    pub channels: Option<u16>,
    /// 可逆の中身（FLAC・ALAC）の標本のビット数。
    pub bits_per_raw_sample: Option<u16>,
    /// 長さ（秒。ストリームの `duration_ts × time_base`、なければ入れ物の `duration`）。時間の上限と確保の目安だけに使う。
    pub duration_secs: Option<f64>,
    /// タグ（ストリームのものを先に、入れ物のものを後に。ファイルの順）。
    pub tags: Vec<(String, String)>,
}

/// 順を保つタグの表（値が文字列でないものは捨てる）。
#[derive(Debug, Default)]
struct OrderedTags(Vec<(String, String)>);

impl<'de> Deserialize<'de> for OrderedTags {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = OrderedTags;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("タグの表")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut m: A) -> Result<OrderedTags, A::Error> {
                let mut out = Vec::new();
                while let Some(k) = m.next_key::<String>()? {
                    if let serde_json::Value::String(v) = m.next_value::<serde_json::Value>()? {
                        out.push((k, v));
                    }
                }
                Ok(OrderedTags(out))
            }
        }
        d.deserialize_map(V)
    }
}

#[derive(Deserialize)]
struct ProbeJson {
    #[serde(default)]
    streams: Vec<StreamJson>,
    #[serde(default)]
    format: Option<FormatJson>,
}

#[derive(Deserialize)]
struct StreamJson {
    codec_name: Option<String>,
    sample_rate: Option<NumOrText>,
    channels: Option<NumOrText>,
    bits_per_raw_sample: Option<NumOrText>,
    duration_ts: Option<NumOrText>,
    time_base: Option<String>,
    #[serde(default)]
    tags: OrderedTags,
}

#[derive(Deserialize)]
struct FormatJson {
    duration: Option<NumOrText>,
    #[serde(default)]
    tags: OrderedTags,
}

/// ffprobe は数を文字列で書くもの（`sample_rate`・`duration`）と数で書くもの（`channels`）がある。
#[derive(Deserialize)]
#[serde(untagged)]
enum NumOrText {
    Int(i64),
    Float(f64),
    Text(String),
}

impl NumOrText {
    fn as_i64(&self) -> Option<i64> {
        match self {
            Self::Int(n) => Some(*n),
            Self::Float(_) => None,
            Self::Text(t) => t.trim().parse().ok(),
        }
    }

    fn as_f64(&self) -> Option<f64> {
        match self {
            Self::Int(n) => Some(*n as f64),
            Self::Float(f) => Some(*f),
            Self::Text(t) => t.trim().parse().ok(),
        }
    }
}

/// `1/48000` の形の時間の単位。
fn time_base(t: &str) -> Option<f64> {
    let (n, d) = t.split_once('/')?;
    let (n, d): (f64, f64) = (n.trim().parse().ok()?, d.trim().parse().ok()?);
    (n > 0.0 && d > 0.0).then_some(n / d)
}

/// ffprobe の JSON を読む。音声のストリームがなければ `Ok(None)`。JSON として読めなければ `Err`。
pub fn parse_probe(json: &[u8]) -> Result<Option<Probe>, String> {
    let p: ProbeJson =
        serde_json::from_slice(json).map_err(|e| format!("ffprobe の出力を読めません: {e}"))?;
    let Some(s) = p.streams.into_iter().next() else {
        return Ok(None);
    };
    let Some(codec_name) = s.codec_name.filter(|c| !c.is_empty()) else {
        return Ok(None);
    };
    let positive = |v: Option<&NumOrText>| v.and_then(NumOrText::as_i64).filter(|n| *n > 0);
    let stream_secs = match (
        positive(s.duration_ts.as_ref()),
        s.time_base.as_deref().and_then(time_base),
    ) {
        (Some(ts), Some(tb)) => Some(ts as f64 * tb),
        _ => None,
    };
    let format_secs = p
        .format
        .as_ref()
        .and_then(|f| f.duration.as_ref())
        .and_then(NumOrText::as_f64);
    let duration_secs = stream_secs
        .or(format_secs)
        .filter(|d| d.is_finite() && *d >= 0.0);
    let mut tags = s.tags.0;
    if let Some(f) = p.format {
        tags.extend(f.tags.0);
    }
    Ok(Some(Probe {
        codec_name,
        sample_rate: positive(s.sample_rate.as_ref()).and_then(|n| u32::try_from(n).ok()),
        channels: positive(s.channels.as_ref()).and_then(|n| u16::try_from(n).ok()),
        bits_per_raw_sample: positive(s.bits_per_raw_sample.as_ref())
            .and_then(|n| u16::try_from(n).ok()),
        duration_secs,
        tags,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ffprobe 9.0.2 の出力（kura-engine のフィクスチャ ogg-opus.ogg。タグは試しに足したもの）。
    const OPUS: &str = r#"{
    "programs": [

    ],
    "stream_groups": [

    ],
    "streams": [
        {
            "index": 0,
            "codec_name": "opus",
            "sample_rate": "48000",
            "channels": 2,
            "time_base": "1/48000",
            "duration_ts": 48312,
            "tags": {
                "encoder": "Lavc63.1.102 libopus",
                "title": "T",
                "album_artist": "AA",
                "LOOPSTART": "100"
            }
        }
    ],
    "format": {
        "format_name": "ogg",
        "duration": "1.006500",
        "tags": {
            "title": "format title"
        }
    }
}"#;

    #[test]
    fn reads_the_first_audio_stream_and_keeps_tag_order() {
        let p = parse_probe(OPUS.as_bytes()).unwrap().unwrap();
        assert_eq!(p.codec_name, "opus");
        assert_eq!((p.sample_rate, p.channels), (Some(48_000), Some(2)));
        assert_eq!(p.bits_per_raw_sample, None);
        assert!((p.duration_secs.unwrap() - 1.0065).abs() < 1e-9);
        let keys: Vec<&str> = p.tags.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(
            keys,
            ["encoder", "title", "album_artist", "LOOPSTART", "title"]
        );
        assert_eq!(p.tags[4].1, "format title");
    }

    #[test]
    fn missing_streams_and_odd_values() {
        assert_eq!(parse_probe(br#"{"streams":[],"format":{}}"#).unwrap(), None);
        assert_eq!(parse_probe(br#"{}"#).unwrap(), None);
        assert_eq!(
            parse_probe(br#"{"streams":[{"codec_name":""}]}"#).unwrap(),
            None
        );
        assert!(parse_probe(b"not json").is_err());
        assert!(parse_probe(br#"{"streams":[{"codec_name":"mp2""#).is_err());
        // 数の形の違い・範囲の外・負の値・文字列でないタグ
        let p = parse_probe(
            br#"{"streams":[{"codec_name":"flac","sample_rate":44100,"channels":"70000",
                "bits_per_raw_sample":"24","duration_ts":-5,"time_base":"1/0",
                "tags":{"a":"1","b":2,"c":null,"d":"4"}}],"format":{"duration":"2.5"}}"#,
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            (p.sample_rate, p.channels, p.bits_per_raw_sample),
            (Some(44_100), None, Some(24))
        );
        assert_eq!(p.duration_secs, Some(2.5));
        assert_eq!(
            p.tags,
            [("a".to_string(), "1".to_string()), ("d".into(), "4".into())]
        );
    }
}
