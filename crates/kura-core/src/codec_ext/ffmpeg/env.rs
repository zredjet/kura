//! ffmpeg の環境と能力：版・ライブラリの版・指紋・使えるデコーダ・エンコーダ・分離器・多重化器・LAME のレートと版。
//!
//! 文字列の読み取りは純粋な関数（手元の ffmpeg 9.0.2 の出力でテストする。Linux でも回る）。`-encoders` などの出力は機械
//! 向けの約束ではないので寛容に読み、読めない行は飛ばす。検出（プロセスを動かす部分）は起動時と、実行ファイルが
//! 変わったとき（大きさ・更新日時）だけ動かし、コーデックの `env()`・`can_decode` は検出の結果だけを見る。

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use kura_engine::codec::CodecEnv;

use super::args;
use crate::tools::{RunError, run_binary, run_capped};

/// 問い合わせ 1 回の時間の上限（既存の `tools::inspect` と同じ）。
const QUERY_TIMEOUT: Duration = Duration::from_secs(5);
/// 問い合わせ 1 回の出力の上限。
const QUERY_MAX: u64 = 1 << 20;
/// 指紋の領域の名前（`-version` の全文の BLAKE3 に前置する）。
const FINGERPRINT_DOMAIN: &[u8] = b"kura-ffmpeg-env-v1\0";

/// `-version` から読んだもの。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VersionInfo {
    /// 1 行目の版（`ffmpeg version 9.0.2 …` の `9.0.2`）。
    pub version: String,
    /// ライブラリの実行時の版（`libavcodec` → `63.1.102`）。
    pub libs: BTreeMap<String, String>,
    /// `-version` の全文の BLAKE3（領域の名前を前置）の先頭 16 桁（configure の行・Homebrew の改訂まで区別する）。
    pub fingerprint: String,
}

/// `-version` の出力を読む。1 行目に版がなければ `None`。
pub fn parse_version(text: &str) -> Option<VersionInfo> {
    let first = text.lines().find(|l| !l.trim().is_empty())?;
    let words: Vec<&str> = first.split_whitespace().collect();
    if !matches!(words.first(), Some(&"ffmpeg" | &"ffprobe")) || words.get(1) != Some(&"version") {
        return None;
    }
    let version = words.get(2)?.to_string();
    let mut libs = BTreeMap::new();
    for line in text.lines() {
        let line = line.trim();
        let Some(name) = line
            .split_whitespace()
            .next()
            .filter(|n| n.starts_with("lib"))
        else {
            continue;
        };
        let rest = &line[name.len()..];
        // 「61.  1.102 / 61.  1.102」の後ろ（実行時の版）
        let runtime = rest.rsplit('/').next().unwrap_or(rest);
        let v: String = runtime.chars().filter(|c| !c.is_whitespace()).collect();
        if !v.is_empty() && v.chars().all(|c| c.is_ascii_digit() || c == '.') {
            libs.insert(name.to_string(), v);
        }
    }
    let mut h = blake3::Hasher::new();
    h.update(FINGERPRINT_DOMAIN);
    h.update(text.as_bytes());
    let fingerprint = h.finalize().to_hex()[..16].to_string();
    Some(VersionInfo {
        version,
        libs,
        fingerprint,
    })
}

/// 区切りの行（`------`・`---`）より後ろの行。
fn after_rule(text: &str) -> impl Iterator<Item = &str> {
    text.lines()
        .skip_while(|l| {
            let t = l.trim();
            !(t.len() >= 2 && t.chars().all(|c| c == '-'))
        })
        .skip(1)
}

/// `-encoders`・`-decoders` の出力から、音声で試験的でないものの名前を集める（行頭の 6 文字の旗 `A....D`）。
pub fn parse_codecs(text: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for line in after_rule(text) {
        let mut tokens = line.split_whitespace();
        let (Some(flags), Some(name)) = (tokens.next(), tokens.next()) else {
            continue;
        };
        let f: Vec<char> = flags.chars().collect();
        if f.len() == 6 && f[0] == 'A' && f[3] != 'X' {
            out.insert(name.to_string());
        }
    }
    out
}

/// `-demuxers`・`-muxers` の出力から、旗 `flag`（`D`・`E`）の付いた形式の名前を集める（`mov,mp4,m4a` はカンマで分ける）。
pub fn parse_formats(text: &str, flag: char) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for line in after_rule(text) {
        let mut flags = String::new();
        let mut names = None;
        for t in line.split_whitespace() {
            if names.is_none()
                && flags.len() + t.len() <= 3
                && t.chars().all(|c| matches!(c, 'D' | 'E' | 'd' | '.'))
            {
                flags.push_str(t);
            } else {
                names = Some(t);
                break;
            }
        }
        if let Some(names) = names
            && flags.contains(flag)
        {
            out.extend(
                names
                    .split(',')
                    .filter(|n| !n.is_empty())
                    .map(str::to_string),
            );
        }
    }
    out
}

/// `-h encoder=<名前>` の「Supported sample rates:」の値。
pub fn parse_sample_rates(help: &str) -> Vec<u32> {
    help.lines()
        .find_map(|l| l.trim().strip_prefix("Supported sample rates:"))
        .map(|rest| {
            rest.split_whitespace()
                .filter_map(|w| w.parse().ok())
                .collect()
        })
        .unwrap_or_default()
}

/// LAME が書いた MP3 から版の文字列（`LAME4.0` の `4.0`）を探す。
pub fn find_lame_version(mp3: &[u8]) -> Option<String> {
    let mut i = 0;
    while let Some(off) = mp3.get(i..)?.windows(4).position(|w| w == b"LAME") {
        let start = i + off + 4;
        let v: String = mp3
            .get(start..)?
            .iter()
            .take(16)
            .take_while(|b| b.is_ascii_digit() || **b == b'.')
            .map(|b| char::from(*b))
            .collect();
        let v = v.trim_end_matches('.');
        if v.starts_with(|c: char| c.is_ascii_digit()) {
            return Some(v.to_string());
        }
        i = start;
    }
    None
}

/// 実行ファイルの印（変わったら検出し直す）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExeStamp {
    pub path: PathBuf,
    pub len: u64,
    pub modified: Option<SystemTime>,
}

impl ExeStamp {
    /// 今の印（リンクの先で見る）。読めなければ `None`。
    pub fn of(path: &Path) -> Option<Self> {
        let real = std::fs::canonicalize(path).ok()?;
        let meta = std::fs::metadata(&real).ok()?;
        Some(Self {
            path: real,
            len: meta.len(),
            modified: meta.modified().ok(),
        })
    }
}

/// 検出した ffmpeg の環境と能力。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Detected {
    pub info: VersionInfo,
    /// ffprobe の版（ffmpeg と違うときだけ記録に入れる）。
    pub ffprobe_version: String,
    /// 音声のデコーダ（試験的でないもの）。
    pub decoders: BTreeSet<String>,
    /// 音声のエンコーダ（試験的でないもの）。
    pub encoders: BTreeSet<String>,
    pub demuxers: BTreeSet<String>,
    pub muxers: BTreeSet<String>,
    /// `libmp3lame` が書けるレート。
    pub lame_rates: Vec<u32>,
    /// LAME の版（調べられなければ `None`）。
    pub lame_version: Option<String>,
    /// 検出したときの ffmpeg・ffprobe の印。
    pub stamps: Vec<Option<ExeStamp>>,
}

impl Detected {
    /// 記録の環境（道具・版・ライブラリの版・CPU・指紋）。コーデックごとの細目は呼び手が足す。
    pub fn env(&self) -> CodecEnv {
        let mut env = CodecEnv::new("ffmpeg", self.info.version.clone())
            .with("arch", std::env::consts::ARCH)
            .with("fingerprint", self.info.fingerprint.clone());
        for (k, v) in &self.info.libs {
            env = env.with(k.clone(), v.clone());
        }
        if self.ffprobe_version != self.info.version {
            env = env.with("ffprobe", self.ffprobe_version.clone());
        }
        env
    }
}

fn query(exe: &Path, args: &[std::ffi::OsString], cwd: &Path) -> Result<String, String> {
    let name = exe.display();
    let (outcome, out) =
        run_capped(exe, args, cwd, QUERY_TIMEOUT, QUERY_MAX, &|| false).map_err(|e| match e {
            RunError::Spawn(e) => format!("{name} を起動できません: {e}"),
            other => format!("{name} の問い合わせに失敗しました: {other}"),
        })?;
    if !outcome.status.success() {
        return Err(format!(
            "{name} の問い合わせに失敗しました（{}）: {}",
            outcome.status,
            outcome.stderr_head.first().map_or("", String::as_str)
        ));
    }
    Ok(String::from_utf8_lossy(&out).into_owned())
}

/// ffmpeg・ffprobe を問い合わせて、環境と能力を調べる（合わせて 0.2 秒ほど。起動時に裏で 1 回）。
pub fn detect(ffmpeg: &Path, ffprobe: &Path, cwd: &Path) -> Result<Detected, String> {
    let version_text = query(ffmpeg, &args::version(), cwd)?;
    let info = parse_version(&version_text).ok_or("ffmpeg の版を読めません")?;
    let probe_text = query(ffprobe, &args::version(), cwd)?;
    let ffprobe_version = parse_version(&probe_text)
        .ok_or("ffprobe の版を読めません")?
        .version;
    let decoders = parse_codecs(&query(ffmpeg, &args::listing("decoders"), cwd)?);
    let encoders = parse_codecs(&query(ffmpeg, &args::listing("encoders"), cwd)?);
    let demuxers = parse_formats(&query(ffmpeg, &args::listing("demuxers"), cwd)?, 'D');
    let muxers = parse_formats(&query(ffmpeg, &args::listing("muxers"), cwd)?, 'E');
    let (lame_rates, lame_version) = if encoders.contains("libmp3lame") {
        let rates = parse_sample_rates(&query(ffmpeg, &args::encoder_help("libmp3lame"), cwd)?);
        (rates, lame_version(ffmpeg, cwd))
    } else {
        (Vec::new(), None)
    };
    Ok(Detected {
        info,
        ffprobe_version,
        decoders,
        encoders,
        demuxers,
        muxers,
        lame_rates,
        lame_version,
        stamps: vec![ExeStamp::of(ffmpeg), ExeStamp::of(ffprobe)],
    })
}

/// 0.2 秒の無音を MP3 にして、LAME の版の文字列を探す（約 20 ms。見つからなければ `None`）。
fn lame_version(ffmpeg: &Path, cwd: &Path) -> Option<String> {
    let budget = crate::tools::Budget {
        max_stdout: QUERY_MAX,
        stall: QUERY_TIMEOUT,
        deadline: QUERY_TIMEOUT,
    };
    let mut out = Vec::new();
    let silence = vec![0u8; args::LAME_PROBE_BYTES];
    let outcome = run_binary(
        ffmpeg,
        &args::lame_probe(),
        cwd,
        &budget,
        &|| false,
        Some(&mut |w: &mut dyn std::io::Write| w.write_all(&silence)),
        &mut |c: &[u8]| {
            out.extend_from_slice(c);
            Ok(())
        },
    )
    .ok()?;
    if !outcome.status.success() {
        return None;
    }
    find_lame_version(&out)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// 手元の ffmpeg 9.0.2（Homebrew）の `-version`。
    pub(crate) const VERSION_902: &str = "ffmpeg version 9.0.2 Copyright (c) 2000-2026 the FFmpeg developers
built with Apple clang version 21.0.0 (clang-2100.3.34.2)
configuration: --prefix=/opt/homebrew/Cellar/ffmpeg/9.0.2_1 --enable-shared --enable-libopus --enable-libmp3lame
libavutil      61.  1.102 / 61.  1.102
libavcodec     63.  1.102 / 63.  1.102
libavformat    63.  1.102 / 63.  1.102
libavdevice    63.  1.102 / 63.  1.102
libavfilter    12.  1.102 / 12.  1.102
libswscale     10.  1.102 / 10.  1.102
libswresample   7.  1.102 /  7.  1.102

Exiting with exit code 0
";

    /// `-decoders` の抜き出し（9.0.2）。
    pub(crate) const DECODERS_902: &str = "Decoders:
 V..... = Video
 A..... = Audio
 S..... = Subtitle
 .F.... = Frame-level multithreading
 ..S... = Slice-level multithreading
 ...X.. = Codec is experimental
 ....B. = Supports draw_horiz_band
 .....D = Supports direct rendering method 1
 ------
 V....D a64multi             Multicolor charset for Commodore 64 (codec a64_multi)
 A....D aac                  AAC (Advanced Audio Coding)
 A....D aac_at               aac (AudioToolbox) (codec aac)
 A....D adpcm_ms             ADPCM Microsoft
 AF...D flac                 FLAC (Free Lossless Audio Codec)
 A....D mp2float             MP2 (MPEG audio layer 2) (codec mp2)
 A....D opus                 Opus
 A....D libopus              libopus Opus (codec opus)
 A..X.D kura_test_x          experimental
 S..... ass                  ASS (Advanced SSA) subtitle

Exiting with exit code 0
";

    /// `-encoders` の抜き出し（9.0.2）。
    pub(crate) const ENCODERS_902: &str = "Encoders:
 V..... = Video
 A..... = Audio
 ------
 A....D libmp3lame           libmp3lame MP3 (MPEG audio layer 3) (codec mp3)
 A..X.D opus                 Opus
 A....D libopus              libopus Opus (codec opus)
 A....D pcm_f32le            PCM 32-bit floating point little-endian
";

    /// `-demuxers` の抜き出し（9.0.2）。
    pub(crate) const DEMUXERS_902: &str = "Formats:
 D.. = Demuxing supported
 .E. = Muxing supported
 ..d = Is a device
 ---
 D   aac             raw ADTS AAC (Advanced Audio Coding)
 D   aiff            Audio IFF
 D d avfoundation    AVFoundation input device
 D   f32le           PCM 32-bit floating-point little-endian
 D   mov,mp4,m4a,3gp,3g2,mj2 QuickTime / MOV
 D   mp3             MP2/3 (MPEG audio layer 2/3)
 D   ogg             Ogg
 D   wav             WAV / WAVE (Waveform Audio)
";

    /// `-muxers` の抜き出し（9.0.2）。
    pub(crate) const MUXERS_902: &str = "Formats:
 D.. = Demuxing supported
 .E. = Muxing supported
 ..d = Is a device
 ---
  E  aiff            Audio IFF
  E  mp3             MP3 (MPEG audio layer 3)
  E  ogg             Ogg
";

    /// `-h encoder=libmp3lame`（9.0.2）。
    pub(crate) const LAME_HELP_902: &str =
        "Encoder libmp3lame [libmp3lame MP3 (MPEG audio layer 3)]:
    General capabilities: dr1 delay small
    Threading capabilities: none
    Supported sample rates: 44100 48000 32000 22050 24000 16000 11025 12000 8000
    Supported sample formats: s32p fltp s16p
    Supported channel layouts: mono stereo
libmp3lame encoder AVOptions:
  -reservoir         <boolean>    E...A...... use bit reservoir (default true)


Exiting with exit code 0
";

    #[test]
    fn reads_the_version_and_libraries() {
        let v = parse_version(VERSION_902).unwrap();
        assert_eq!(v.version, "9.0.2");
        assert_eq!(
            v.libs.get("libavcodec").map(String::as_str),
            Some("63.1.102")
        );
        assert_eq!(
            v.libs.get("libswresample").map(String::as_str),
            Some("7.1.102")
        );
        assert_eq!(v.libs.len(), 7);
        assert_eq!(v.fingerprint.len(), 16);
        // configure の行が違えば指紋も違う
        let other = parse_version(&VERSION_902.replace("9.0.2_1", "9.0.2_2")).unwrap();
        assert_ne!(other.fingerprint, v.fingerprint);
        assert_eq!(
            parse_version("ffprobe version 9.0.2 Copyright")
                .unwrap()
                .version,
            "9.0.2"
        );
        assert_eq!(parse_version(""), None);
        assert_eq!(parse_version("hello world"), None);
        assert_eq!(parse_version("ffmpeg version"), None);
    }

    #[test]
    fn reads_codec_and_format_listings() {
        let d = parse_codecs(DECODERS_902);
        let names: Vec<&str> = d.iter().map(String::as_str).collect();
        assert_eq!(
            names,
            [
                "aac", "aac_at", "adpcm_ms", "flac", "libopus", "mp2float", "opus"
            ]
        );
        let e = parse_codecs(ENCODERS_902);
        assert!(e.contains("libmp3lame") && e.contains("libopus") && !e.contains("opus"));
        let dm = parse_formats(DEMUXERS_902, 'D');
        for n in [
            "aac",
            "aiff",
            "f32le",
            "mov",
            "m4a",
            "mp3",
            "ogg",
            "wav",
            "avfoundation",
        ] {
            assert!(dm.contains(n), "{n}");
        }
        assert!(parse_formats(DEMUXERS_902, 'E').is_empty());
        let mx = parse_formats(MUXERS_902, 'E');
        assert_eq!(
            mx.iter().map(String::as_str).collect::<Vec<_>>(),
            ["aiff", "mp3", "ogg"]
        );
        // 壊れた・空の出力
        assert!(parse_codecs("").is_empty());
        assert!(parse_codecs("no rule here\n A....D aac x").is_empty());
        assert!(parse_formats("---\n\n   \n", 'D').is_empty());
    }

    #[test]
    fn reads_lame_rates_and_version() {
        assert_eq!(
            parse_sample_rates(LAME_HELP_902),
            [
                44_100, 48_000, 32_000, 22_050, 24_000, 16_000, 11_025, 12_000, 8_000
            ]
        );
        assert!(parse_sample_rates("").is_empty());
        let mut mp3 = vec![0xFFu8, 0xFB, 0x90];
        mp3.extend_from_slice(b"UUUULAME.xLAME4.0UUUU");
        assert_eq!(find_lame_version(&mp3).as_deref(), Some("4.0"));
        assert_eq!(find_lame_version(b"LAME3.100.").as_deref(), Some("3.100"));
        assert_eq!(find_lame_version(b"LAME"), None);
        assert_eq!(find_lame_version(b"xxLAMEy"), None);
        assert_eq!(find_lame_version(b""), None);
    }

    #[test]
    fn env_carries_versions_and_fingerprint() {
        let d = Detected {
            info: parse_version(VERSION_902).unwrap(),
            ffprobe_version: "9.0.1".into(),
            decoders: BTreeSet::new(),
            encoders: BTreeSet::new(),
            demuxers: BTreeSet::new(),
            muxers: BTreeSet::new(),
            lame_rates: Vec::new(),
            lame_version: None,
            stamps: Vec::new(),
        };
        let env = d.env();
        assert_eq!(
            (env.tool.as_str(), env.version.as_str()),
            ("ffmpeg", "9.0.2")
        );
        assert_eq!(
            env.components.get("libavformat").map(String::as_str),
            Some("63.1.102")
        );
        assert_eq!(
            env.components.get("ffprobe").map(String::as_str),
            Some("9.0.1")
        );
        assert_eq!(
            env.components.get("arch").map(String::as_str),
            Some(std::env::consts::ARCH)
        );
        assert!(env.components.contains_key("fingerprint"));
    }
}
