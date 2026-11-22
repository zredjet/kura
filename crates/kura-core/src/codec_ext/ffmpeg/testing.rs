//! テストの補助：偽の ffmpeg・ffprobe（sh のスクリプト。Linux でも回る）と、実物の ffmpeg を探す口。

use std::path::{Path, PathBuf};

use super::FfmpegCodecs;
use super::env::tests::{
    DECODERS_902, DEMUXERS_902, ENCODERS_902, LAME_HELP_902, MUXERS_902, VERSION_902,
};

/// 実行できる sh のスクリプトを書く。
pub(crate) fn script(path: &Path, body: &str) {
    std::fs::write(path, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(path, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
}

/// sh の単一引用符の中に入れる形。
pub(crate) fn sq(p: &Path) -> String {
    format!("'{}'", p.display().to_string().replace('\'', r"'\''"))
}

/// 偽の ffmpeg・ffprobe の置き場所。
pub(crate) struct FakeTools {
    pub dir: tempfile::TempDir,
    pub ffmpeg: PathBuf,
    pub ffprobe: PathBuf,
    pub work: PathBuf,
}

impl FakeTools {
    /// 問い合わせ（`-version`・一覧・`-h encoder=`・LAME の版）には 9.0.2 の出力で答え、読み取り（`pipe:1` への WAV）は
    /// `decode_body`、書き出し（`file:` の MP3）は `encode_body`（`$out` が出力のパス）を動かす偽物。ffprobe は
    /// `probe_body` を動かす。
    pub(crate) fn new(probe_body: &str, decode_body: &str, encode_body: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        for (name, text) in [
            ("version.txt", VERSION_902),
            ("decoders.txt", DECODERS_902),
            ("encoders.txt", ENCODERS_902),
            ("demuxers.txt", DEMUXERS_902),
            ("muxers.txt", MUXERS_902),
            ("lame_help.txt", LAME_HELP_902),
        ] {
            std::fs::write(d.join(name), text).unwrap();
        }
        let ffmpeg = d.join("ffmpeg");
        let ffprobe = d.join("ffprobe");
        let q = |n: &str| sq(&d.join(n));
        script(
            &ffmpeg,
            &format!(
                r#"args=" $* "
case "$args" in
  *" -version "*) cat {v}; exit 0;;
  *" -decoders "*) cat {de}; exit 0;;
  *" -encoders "*) cat {en}; exit 0;;
  *" -demuxers "*) cat {dm}; exit 0;;
  *" -muxers "*) cat {mx}; exit 0;;
  *" encoder=libmp3lame "*) cat {lh}; exit 0;;
  *" -f f32le "*" pipe:1 "*) cat > /dev/null; printf 'xxLAME4.0UUU'; exit 0;;
esac
case "$args" in
  *" -f f32le "*)
    for a in "$@"; do last="$a"; done
    out="${{last#file:}}"
    {encode_body}
    ;;
  *)
    {decode_body}
    ;;
esac"#,
                v = q("version.txt"),
                de = q("decoders.txt"),
                en = q("encoders.txt"),
                dm = q("demuxers.txt"),
                mx = q("muxers.txt"),
                lh = q("lame_help.txt"),
            ),
        );
        script(
            &ffprobe,
            &format!(
                r#"case " $* " in
  *" -version "*) printf 'ffprobe version 9.0.2 Copyright\n'; exit 0;;
esac
{probe_body}"#
            ),
        );
        let work = d.join("work");
        Self {
            dir,
            ffmpeg,
            ffprobe,
            work,
        }
    }

    /// この偽物を検出したコーデック。
    pub(crate) fn codecs(&self) -> FfmpegCodecs {
        FfmpegCodecs::detect_at(&self.ffmpeg, &self.ffprobe, &self.work).unwrap()
    }

    /// 置き場所の中のファイル。
    pub(crate) fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }
}

/// 実物の ffmpeg・ffprobe：`KURA_FFMPEG_DIR` の場所か、Apple Silicon の Homebrew（`/opt/homebrew/bin`）。なければ `None`
/// （`KURA_FFMPEG_REQUIRED=1` なら失敗にする）。Linux の CI の古い ffmpeg（`-ch_layout` がないなど）では回さない。
pub(crate) fn real_tools() -> Option<(PathBuf, PathBuf)> {
    let dir = std::env::var_os("KURA_FFMPEG_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/opt/homebrew/bin"));
    let found = Some(dir.as_path())
        .filter(|d| d.join("ffmpeg").is_file() && d.join("ffprobe").is_file())
        .map(|d: &Path| (d.join("ffmpeg"), d.join("ffprobe")));
    if found.is_none() && std::env::var_os("KURA_FFMPEG_REQUIRED").is_some_and(|v| v == "1") {
        panic!("ffmpeg が見つかりません（KURA_FFMPEG_REQUIRED=1）");
    }
    found
}
