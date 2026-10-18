//! ffmpeg の読み取り（`Tier::External`）。形式ごとに 1 つ（識別子 `<形式>-ffmpeg-dec/1`）。
//!
//! 約束（識別子の版に入る）：
//! - 前の段が断った音声だけを読む。中身のコーデック（ffprobe の `codec_name`）が形式の表（[`KINDS`]）にないものは
//!   `Unsupported`。分離器とデコーダは表のものを強制する（デコーダは候補の先から、ffmpeg にあるもの。Opus は libopus を
//!   先に）。引数は [`super::args::decode`]。
//! - 形（`shape_file`）：ffprobe（30 秒・出力 1 MiB＋`max_meta_bytes`）で中身・チャンネル・レートを確かめ、1 回デコードして
//!   標準出力の WAV を数えて `Exact` にする。値（`decode_file`）はもう 1 回デコードする。レート・チャンネルは WAV のヘッダを
//!   正とする（ffprobe は暗黙の SBR などで違いうる）。2 回の長さが違えば `CodecSet` の照合が「ヘッダと中身が合わない」にする。
//! - 失敗：標準エラーに `[error]` 以上の行が 1 つでもある・終了コードが 0 でない → `Decode`（ffmpeg は壊れた Ogg のページを
//!   捨てても終了コード 0 で短い出力を返すため）。上限 → `Limit`。中止 → `Canceled`。ffmpeg が消えた → `CodecMissing`。
//!   ffprobe が開けない・音声がない → `Unsupported`（次がなければ「読めません」）。
//! - タグ・ループ点は ffprobe のタグから、固定の段と同じ規則で（[`super::tags`]）。`AudioMeta.source` は FLAC・ALAC なら
//!   標本のビット数、ほかは非可逆の扱い（16 ビットの整数・出力のレート）。
//! - 読み口の口（`shape`・`decode`）は、`max_value_bytes` までを作業フォルダの一時ファイルに写してからパスの口を呼ぶ。

use std::io::Write;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use kura_engine::codec::pcm::container_bits;
use kura_engine::codec::{CodecEnv, Decoder, ReadSeek, Stage, Tier};
use kura_engine::doc::DecodeOptions;
use kura_engine::error::EngineError;
use kura_engine::limits::Limits;
use kura_engine::monitor::{Monitor, NoMonitor};
use kura_engine::shape::{Dim, Shape};
use kura_engine::value::{Audio, AudioSource, Value};

use super::env::Detected;
use super::probe::{Probe, parse_probe};
use super::tags::{TagStyle, meta_from_tags};
use super::wavpipe::{WavError, WavStream};
use super::{STALL, Shared, TempFile, args, deadline_for, failure, io_error, run_error};
use crate::tools::{Budget, RunError, Stopped, run_binary, run_capped};

/// ffprobe の時間の上限（既存の `media::ffmpeg` と同じ）。
const PROBE_TIMEOUT: Duration = Duration::from_secs(30);
/// ffprobe の出力の上限の基本（これに `max_meta_bytes` を足す）。
const PROBE_MAX_BASE: u64 = 1 << 20;
/// 標準出力の上限のうち、ヘッダなどの余裕（標本の上限は WAV の読み手が先に止める）。
const STDOUT_SLACK: u64 = 1 << 20;

/// 形式 1 つの読み方（識別子・強制する分離器・中身のコーデックごとのデコーダの候補・タグの読み方）。
#[derive(Debug)]
pub struct DecodeKind {
    /// 形式の表の識別子（`opus` など）。
    pub format: &'static str,
    /// コーデックの識別子。
    pub id: &'static str,
    /// 強制する分離器（`-f`・`-format_whitelist`）。
    pub demuxer: &'static str,
    /// ffprobe の `codec_name` → 使うデコーダの候補（先が優先。ffmpeg にあるものを使う）。
    pub codecs: &'static [(&'static str, &'static [&'static str])],
    pub tags: TagStyle,
}

/// 読む形式の表（段 9）。Vorbis（Ogg）・ALAC 以外の m4a の中身・MP3 の layer 3 などは前の段が読む。
pub const KINDS: [DecodeKind; 7] = [
    DecodeKind {
        format: "opus",
        id: "opus-ffmpeg-dec/1",
        demuxer: "ogg",
        codecs: &[("opus", &["libopus", "opus"])],
        tags: TagStyle::Vorbis,
    },
    DecodeKind {
        format: "ogg",
        id: "ogg-ffmpeg-dec/1",
        demuxer: "ogg",
        codecs: &[
            ("opus", &["libopus", "opus"]),
            ("flac", &["flac"]),
            ("speex", &["libspeex", "speex"]),
        ],
        tags: TagStyle::Vorbis,
    },
    DecodeKind {
        format: "wav",
        id: "wav-ffmpeg-dec/1",
        demuxer: "wav",
        codecs: &[
            ("adpcm_ms", &["adpcm_ms"]),
            ("adpcm_ima_wav", &["adpcm_ima_wav"]),
            ("pcm_alaw", &["pcm_alaw"]),
            ("pcm_mulaw", &["pcm_mulaw"]),
            ("gsm_ms", &["gsm_ms"]),
            ("adpcm_yamaha", &["adpcm_yamaha"]),
            ("mp3", &["mp3float", "mp3"]),
        ],
        tags: TagStyle::Wav,
    },
    DecodeKind {
        format: "aiff",
        id: "aiff-ffmpeg-dec/1",
        demuxer: "aiff",
        codecs: &[
            ("adpcm_ima_qt", &["adpcm_ima_qt"]),
            ("pcm_alaw", &["pcm_alaw"]),
            ("pcm_mulaw", &["pcm_mulaw"]),
            ("gsm", &["gsm"]),
            ("mace3", &["mace3"]),
            ("mace6", &["mace6"]),
            ("qdm2", &["qdm2"]),
            ("qdmc", &["qdmc"]),
        ],
        tags: TagStyle::Aiff,
    },
    DecodeKind {
        format: "m4a",
        id: "m4a-ffmpeg-dec/1",
        demuxer: "mov",
        codecs: &[("aac", &["aac"]), ("alac", &["alac"])],
        tags: TagStyle::Plain,
    },
    DecodeKind {
        format: "aac",
        id: "aac-ffmpeg-dec/1",
        demuxer: "aac",
        codecs: &[("aac", &["aac"])],
        tags: TagStyle::Plain,
    },
    DecodeKind {
        format: "mp3",
        id: "mp3-ffmpeg-dec/1",
        demuxer: "mp3",
        codecs: &[("mp1", &["mp1float", "mp1"]), ("mp2", &["mp2float", "mp2"])],
        tags: TagStyle::Plain,
    },
];

impl DecodeKind {
    /// この ffmpeg で、中身 `codec_name` を読むデコーダ（候補の先から、あるもの）。
    pub fn decoder_for(&self, d: &Detected, codec_name: &str) -> Option<&'static str> {
        self.codecs
            .iter()
            .find(|(c, _)| *c == codec_name)
            .and_then(|(_, cands)| cands.iter().copied().find(|n| d.decoders.contains(*n)))
    }

    /// この ffmpeg で使えるか（分離器があり、どれかの中身のデコーダがある）。
    pub fn available(&self, d: &Detected) -> bool {
        d.demuxers.contains(self.demuxer)
            && self
                .codecs
                .iter()
                .any(|(c, _)| self.decoder_for(d, c).is_some())
    }

    /// ffprobe が形を調べるときに開いてよいデコーダ（候補のうち ffmpeg にあるもの。表の順）。
    pub fn probe_decoders(&self, d: &Detected) -> Vec<&'static str> {
        let mut out: Vec<&'static str> = Vec::new();
        for (_, cands) in self.codecs {
            for n in *cands {
                if d.decoders.contains(*n) && !out.contains(n) {
                    out.push(n);
                }
            }
        }
        out
    }

    /// 記録の細目：中身ごとに使うデコーダ（`flac=flac,opus=libopus`。中身の名前の順）。
    pub fn decoders_component(&self, d: &Detected) -> String {
        let mut pairs: Vec<(&str, &str)> = self
            .codecs
            .iter()
            .filter_map(|(c, _)| self.decoder_for(d, c).map(|n| (*c, n)))
            .collect();
        pairs.sort();
        pairs
            .iter()
            .map(|(c, n)| format!("{c}={n}"))
            .collect::<Vec<_>>()
            .join(",")
    }
}

/// ffprobe で決めた読み方。
struct Plan {
    probe: Probe,
    decoder: &'static str,
}

/// ffmpeg の読み取り。
pub struct FfmpegAudioDecoder {
    shared: Arc<Shared>,
    kind: &'static DecodeKind,
}

impl FfmpegAudioDecoder {
    pub(crate) fn new(shared: Arc<Shared>, kind: &'static DecodeKind) -> Self {
        Self { shared, kind }
    }

    /// 読む形式の表の行。
    pub fn kind(&self) -> &'static DecodeKind {
        self.kind
    }

    fn wav_error(&self, e: WavError) -> EngineError {
        match e {
            WavError::Limit(m) => EngineError::Limit(m),
            WavError::Bad(m) => io_error(self.kind.id, Stage::Decode, m),
        }
    }

    /// ffprobe で中身を確かめて、使うデコーダを決める。
    fn plan(
        &self,
        d: &Detected,
        path: &Path,
        limits: &Limits,
        canceled: &(dyn Fn() -> bool + Sync),
    ) -> Result<Plan, EngineError> {
        let k = self.kind;
        let a = args::probe(k.demuxer, &k.probe_decoders(d), path);
        let max = PROBE_MAX_BASE.saturating_add(limits.max_meta_bytes);
        let budget = Budget {
            max_stdout: max,
            stall: PROBE_TIMEOUT,
            deadline: PROBE_TIMEOUT,
        };
        let dir = self.shared.work_dir()?;
        let (outcome, out) =
            run_capped(self.shared.ffprobe(), &a, dir, PROBE_TIMEOUT, max, canceled).map_err(
                |e| match e {
                    RunError::Stopped(Stopped::OutputLimit) => {
                        EngineError::Limit("ffprobe の出力（メタデータ）が上限を超えます".into())
                    }
                    other => run_error(k.id, Stage::Decode, other, &budget),
                },
            )?;
        if !outcome.status.success() {
            let why = outcome
                .stderr_head
                .first()
                .map(|l| super::clean_line(l))
                .unwrap_or_default();
            return Err(EngineError::Unsupported(format!(
                "{}: ffmpeg も {} として開けません: {why}",
                k.id, k.format
            )));
        }
        let probe = parse_probe(&out)
            .map_err(|m| io_error(k.id, Stage::Decode, m))?
            .ok_or_else(|| {
                EngineError::Unsupported(format!("{}: 音声のストリームがありません", k.id))
            })?;
        let decoder = k.decoder_for(d, &probe.codec_name).ok_or_else(|| {
            EngineError::Unsupported(format!("{}: 中身 {} は扱いません", k.id, probe.codec_name))
        })?;
        if let Some(ch) = probe.channels
            && u64::from(ch) > limits.max_channels
        {
            return Err(EngineError::Limit(format!(
                "チャンネルの数が {} を超えます（{ch}）",
                limits.max_channels
            )));
        }
        if let Some(rate) = probe.sample_rate
            && rate > limits.max_sample_rate
        {
            return Err(EngineError::Limit(format!(
                "サンプルレートが {} を超えます（{rate}）",
                limits.max_sample_rate
            )));
        }
        Ok(Plan { probe, decoder })
    }

    /// ffprobe の長さ（秒）から見積もったフレームの数（上限で抑える。確保と進捗の目安だけ）。
    fn estimate_frames(plan: &Plan, limits: &Limits) -> u64 {
        match (plan.probe.duration_secs, plan.probe.sample_rate) {
            (Some(s), Some(r)) => {
                let f = (s * f64::from(r)).ceil();
                if f.is_finite() && f >= 0.0 {
                    (f as u64).min(limits.max_audio_frames)
                } else {
                    0
                }
            }
            _ => 0,
        }
    }

    /// ffmpeg を 1 回動かし、標準出力の WAV を `stream` に流す。
    fn run(
        &self,
        plan: &Plan,
        path: &Path,
        limits: &Limits,
        mon: &dyn Monitor,
        stream: &mut WavStream,
        progress_total: Option<u64>,
    ) -> Result<(), EngineError> {
        let id = self.kind.id;
        let a = args::decode(self.kind.demuxer, plan.decoder, path);
        let secs = plan.probe.duration_secs.unwrap_or(0.0);
        let cap = limits.max_value_bytes.min(
            limits
                .max_audio_frames
                .saturating_mul(4)
                .saturating_mul(limits.max_channels),
        );
        let budget = Budget {
            max_stdout: cap.saturating_add(STDOUT_SLACK),
            stall: STALL,
            deadline: deadline_for(secs),
        };
        let dir = self.shared.work_dir()?;
        let mut stash: Option<WavError> = None;
        let result = run_binary(
            self.shared.ffmpeg(),
            &a,
            dir,
            &budget,
            &|| mon.canceled(),
            None,
            &mut |chunk: &[u8]| match stream.push(chunk) {
                Ok(()) => {
                    if let Some(total) = progress_total {
                        let done = stream.frames();
                        mon.progress(done, total.max(done));
                    }
                    Ok(())
                }
                Err(e) => {
                    stash = Some(e);
                    Err(String::new())
                }
            },
        );
        match result {
            Err(RunError::Stopped(Stopped::Consumer)) => Err(match stash {
                Some(e) => self.wav_error(e),
                None => run_error(
                    id,
                    Stage::Decode,
                    RunError::Stopped(Stopped::Consumer),
                    &budget,
                ),
            }),
            Err(e) => Err(run_error(id, Stage::Decode, e, &budget)),
            Ok(outcome) => match failure(id, Stage::Decode, &outcome) {
                Some(e) => Err(e),
                None => Ok(()),
            },
        }
    }

    /// 読み口の中身を作業フォルダの一時ファイルに写す（`max_value_bytes` まで）。
    fn spool(
        &self,
        src: &mut dyn ReadSeek,
        limits: &Limits,
        mon: &dyn Monitor,
    ) -> Result<TempFile, EngineError> {
        let (tmp, mut f) = self.shared.create_temp("ffin", self.kind.format)?;
        let mut buf = vec![0u8; 1 << 20];
        let mut total = 0u64;
        loop {
            if mon.canceled() {
                return Err(EngineError::Canceled);
            }
            let n = match src.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => n,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e.into()),
            };
            total += n as u64;
            if total > limits.max_value_bytes {
                return Err(EngineError::Limit("ファイルが上限を超えます".into()));
            }
            f.write_all(&buf[..n])?;
        }
        f.flush()?;
        Ok(tmp)
    }
}

impl Decoder for FfmpegAudioDecoder {
    fn id(&self) -> &'static str {
        self.kind.id
    }

    fn tier(&self) -> Tier {
        Tier::External
    }

    fn env(&self) -> Option<CodecEnv> {
        let d = self.shared.detected();
        Some(d.env().with("decoders", self.kind.decoders_component(&d)))
    }

    fn can_decode(&self, format: &str) -> bool {
        format == self.kind.format
    }

    fn shape(
        &self,
        src: &mut dyn ReadSeek,
        format: &str,
        opts: &DecodeOptions,
        limits: &Limits,
    ) -> Result<Shape, EngineError> {
        self.shape_monitored(src, format, opts, limits, &NoMonitor)
    }

    /// 形は 1 回デコードして数えるので、写し・ffprobe・枠の待ち・数えるデコードのどこでも中止を見る。
    fn shape_monitored(
        &self,
        src: &mut dyn ReadSeek,
        format: &str,
        opts: &DecodeOptions,
        limits: &Limits,
        mon: &dyn Monitor,
    ) -> Result<Shape, EngineError> {
        let tmp = self.spool(src, limits, mon)?;
        self.shape_file_monitored(tmp.path(), format, opts, limits, mon)
    }

    fn decode(
        &self,
        src: &mut dyn ReadSeek,
        format: &str,
        opts: &DecodeOptions,
        limits: &Limits,
        mon: &dyn Monitor,
    ) -> Result<Value, EngineError> {
        let tmp = self.spool(src, limits, mon)?;
        self.decode_file(tmp.path(), format, opts, limits, mon)
    }

    fn shape_file(
        &self,
        path: &Path,
        format: &str,
        opts: &DecodeOptions,
        limits: &Limits,
    ) -> Result<Shape, EngineError> {
        self.shape_file_monitored(path, format, opts, limits, &NoMonitor)
    }

    fn shape_file_monitored(
        &self,
        path: &Path,
        _format: &str,
        _opts: &DecodeOptions,
        limits: &Limits,
        mon: &dyn Monitor,
    ) -> Result<Shape, EngineError> {
        let d = self.shared.refresh()?;
        let plan = self.plan(&d, path, limits, &|| mon.canceled())?;
        let _permit = self.shared.acquire(mon)?;
        let mut stream = WavStream::count(limits.into());
        self.run(&plan, path, limits, mon, &mut stream, None)?;
        if mon.canceled() {
            return Err(EngineError::Canceled);
        }
        let done = stream.finish().map_err(|e| self.wav_error(e))?;
        Ok(Shape::Audio {
            rate: done.format.rate,
            channels: done.format.channels,
            frames: Dim::Exact(done.frames),
        })
    }

    fn decode_file(
        &self,
        path: &Path,
        _format: &str,
        _opts: &DecodeOptions,
        limits: &Limits,
        mon: &dyn Monitor,
    ) -> Result<Value, EngineError> {
        let d = self.shared.refresh()?;
        let plan = self.plan(&d, path, limits, &|| mon.canceled())?;
        let _permit = self.shared.acquire(mon)?;
        let estimate = Self::estimate_frames(&plan, limits);
        let mut stream = WavStream::store(limits.into(), estimate);
        self.run(&plan, path, limits, mon, &mut stream, Some(estimate))?;
        if mon.canceled() {
            return Err(EngineError::Canceled);
        }
        let done = stream.finish().map_err(|e| self.wav_error(e))?;
        let rate = done.format.rate;
        let mut planes = done.planes.unwrap_or_default();
        for p in &mut planes {
            // 見積もりが大きすぎたときの余りを返す
            if p.capacity() - p.len() > 1 << 16 {
                p.shrink_to_fit();
            }
        }
        let mut meta = meta_from_tags(&plan.probe.tags, done.frames, self.kind.tags, limits);
        let bits = match plan.probe.codec_name.as_str() {
            "flac" | "alac" => plan
                .probe
                .bits_per_raw_sample
                .filter(|b| (1..=32).contains(b))
                .map_or(16, container_bits),
            _ => 16,
        };
        meta.source = Some(AudioSource {
            bits,
            float: false,
            rate,
        });
        Ok(Value::Audio(Arc::new(Audio::new(
            rate,
            planes.into_iter().map(Arc::new).collect(),
            meta,
        )?)))
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Instant;

    use kura_engine::codec::{CodecSet, DecodeStamp};
    use kura_engine::monitor::CancelFlag;

    use super::*;
    use crate::codec_ext::ffmpeg::testing::{FakeTools, real_tools};
    use crate::codec_ext::ffmpeg::wavpipe::tests::wav_bytes;
    use crate::codec_ext::ffmpeg::{FfmpegCodecs, TOOL};

    fn planes(ch: usize, frames: usize) -> Vec<Vec<f32>> {
        (0..ch)
            .map(|c| {
                (0..frames)
                    .map(|i| ((i * 13 + c * 101) % 2000) as f32 / 1000.0 - 1.0)
                    .collect()
            })
            .collect()
    }

    fn probe_json(codec: &str, rate: u32, ch: u16, extra_tags: &str) -> String {
        format!(
            r#"{{"streams":[{{"index":0,"codec_name":"{codec}","sample_rate":"{rate}","channels":{ch},"time_base":"1/{rate}","duration_ts":5,"tags":{{"title":"T","LOOPSTART":"1","LOOPLENGTH":"2"{extra_tags}}}}}],"format":{{"format_name":"ogg","duration":"0.1"}}}}"#
        )
    }

    /// 偽物：ffprobe は置き場所の probe.json を出し、ffmpeg（読み取り）は `decode_body` を動かす。
    fn fake(decode_body: &str) -> FakeTools {
        FakeTools::new(
            "cat \"$(dirname \"$0\")/probe.json\"",
            decode_body,
            "exit 1",
        )
    }

    /// 6 チャンネル・5 フレームの WAV を出し、引数を args.log に書く偽物。
    fn standard() -> (FakeTools, Vec<Vec<f32>>) {
        let t = fake(
            "printf '%s\\n' \"$@\" > \"$(dirname \"$0\")/args.log\"; cat \"$(dirname \"$0\")/out.wav\"",
        );
        let p = planes(6, 5);
        std::fs::write(t.path("out.wav"), wav_bytes(48_000, &p, None)).unwrap();
        std::fs::write(t.path("probe.json"), probe_json("opus", 48_000, 6, "")).unwrap();
        std::fs::write(t.path("in.opus"), b"not really opus").unwrap();
        (t, p)
    }

    fn set_with(codecs: &FfmpegCodecs) -> CodecSet {
        let mut set = CodecSet::pinned();
        codecs.add_to(&mut set).unwrap();
        set
    }

    fn read(
        set: &CodecSet,
        path: &Path,
        format: &str,
        limits: &Limits,
    ) -> Result<(Value, DecodeStamp), EngineError> {
        set.decode_path(path, format, &DecodeOptions::default(), limits, &NoMonitor)
    }

    fn audio(v: &Value) -> &Audio {
        match v {
            Value::Audio(a) => a,
            _ => panic!("音声ではありません"),
        }
    }

    /// 偽の ffmpeg の WAV がチャンネルに分かれて値になり、記録は External・再現なし・環境つき。引数は分離器・デコーダの
    /// 強制とホワイトリスト、`file:` の絶対パス。読み口の口は一時ファイルを残さない。
    #[test]
    fn fake_output_becomes_the_value_with_an_external_stamp() {
        let (t, p) = standard();
        let codecs = t.codecs();
        let set = set_with(&codecs);
        let (v, stamp) = read(&set, &t.path("in.opus"), "opus", &Limits::default()).unwrap();
        let a = audio(&v);
        assert_eq!((a.rate(), a.channels(), a.frames()), (48_000, 6, 5));
        for (got, want) in a.planes().iter().zip(&p) {
            assert_eq!(got.as_slice(), want.as_slice());
        }
        assert_eq!(a.meta.tags.get("title").map(String::as_str), Some("T"));
        assert_eq!(
            a.meta.loop_range,
            Some(kura_engine::value::LoopRange { start: 1, end: 3 })
        );
        assert_eq!(
            a.meta.source,
            Some(AudioSource {
                bits: 16,
                float: false,
                rate: 48_000
            })
        );
        assert_eq!(
            (stamp.codec.as_str(), stamp.tier, stamp.reproducible),
            ("opus-ffmpeg-dec/1", Tier::External, false)
        );
        let env = stamp.env.clone().unwrap();
        assert_eq!(
            (env.tool.as_str(), env.version.as_str()),
            ("ffmpeg", "9.0.2")
        );
        assert_eq!(
            env.components.get("decoders").map(String::as_str),
            Some("opus=libopus")
        );
        assert_eq!(
            env.components.get("libavcodec").map(String::as_str),
            Some("63.1.102")
        );
        let log = std::fs::read_to_string(t.path("args.log")).unwrap();
        let args: Vec<&str> = log.lines().collect();
        let url = format!("file:{}", t.path("in.opus").display());
        for w in [
            ["-protocol_whitelist", "file"],
            ["-format_whitelist", "ogg"],
            ["-codec_whitelist", "libopus"],
            ["-f", "ogg"],
            ["-c:a", "libopus"],
            ["-i", url.as_str()],
            ["-c:a", "pcm_f32le"],
            ["-protocol_whitelist", "pipe"],
        ] {
            assert!(args.windows(2).any(|x| x == w), "{w:?} {args:?}");
        }
        assert_eq!(args.last(), Some(&"pipe:1"));
        // 形（パスの口）と、読み口の口（一時ファイルに写して読む）
        let (shape, id) = set
            .shape_path(
                &t.path("in.opus"),
                "opus",
                &DecodeOptions::default(),
                &Limits::default(),
            )
            .unwrap();
        assert_eq!(id, "opus-ffmpeg-dec/1");
        assert!(shape.admits(&v.shape()));
        let (v2, s2) = set
            .decode(
                &mut Cursor::new(b"bytes".to_vec()),
                "opus",
                &DecodeOptions::default(),
                &Limits::default(),
                &NoMonitor,
            )
            .unwrap();
        assert_eq!((v2, s2.value_digest), (v, stamp.value_digest));
        let left: Vec<_> = std::fs::read_dir(&t.work).unwrap().collect();
        assert!(left.is_empty(), "{left:?}");
        // 能力の表に External の読み手として出る
        let caps = set.format_caps("opus").unwrap();
        assert!(caps.read.iter().any(|r| r.codec == "opus-ffmpeg-dec/1"
            && r.tier == Tier::External
            && !r.reproducible));
    }

    /// 2 回のデコードで長さが違う・端数・WAV でない・[error] の行・終了コード・シグナルは、どれも読み取りの失敗。
    #[test]
    fn broken_output_and_failures_are_decode_errors() {
        let dir_of = "\"$(dirname \"$0\")\"";
        let cases = [
            // 1 回目と 2 回目で長さが違う
            format!(
                "n=$(cat {dir_of}/cnt 2>/dev/null || echo 0); echo $((n+1)) > {dir_of}/cnt; if [ $n = 0 ]; then cat {dir_of}/out.wav; else cat {dir_of}/short.wav; fi"
            ),
            // 端数
            format!("cat {dir_of}/out.wav; printf 'xyz'"),
            // WAV でない
            "printf 'hello world, not a wav file'".to_string(),
            // 終了コード 0 でも [error] の行
            format!("echo '[in#0/ogg @ 0x1] [error] CRC mismatch!' >&2; cat {dir_of}/out.wav"),
            // 終了コード
            format!("cat {dir_of}/out.wav; exit 1"),
            // シグナル（abort）
            "kill -6 $$".to_string(),
        ];
        for (i, body) in cases.iter().enumerate() {
            let t = fake(body);
            std::fs::write(t.path("out.wav"), wav_bytes(48_000, &planes(2, 10), None)).unwrap();
            std::fs::write(t.path("short.wav"), wav_bytes(48_000, &planes(2, 9), None)).unwrap();
            std::fs::write(t.path("probe.json"), probe_json("opus", 48_000, 2, "")).unwrap();
            let set = set_with(&t.codecs());
            let r = read(&set, &t.path("probe.json"), "opus", &Limits::default());
            match r {
                Err(EngineError::Decode { codec, message }) => {
                    assert_eq!(codec, "opus-ffmpeg-dec/1");
                    if i == 3 {
                        assert!(
                            message.contains("CRC mismatch") && !message.contains("0x1"),
                            "{message}"
                        );
                    }
                    if i == 5 {
                        assert!(message.contains("シグナル 6"), "{message}");
                    }
                }
                other => panic!("{i}: {other:?}"),
            }
        }
    }

    /// 上限：長さ・値の大きさ・チャンネル・レート・ffprobe の出力の大きさ。
    #[test]
    fn limits_are_enforced() {
        let (t, _) = standard();
        let set = set_with(&t.codecs());
        let path = t.path("in.opus");
        let tight = |f: fn(&mut Limits)| {
            let mut l = Limits::default();
            f(&mut l);
            l
        };
        for l in [
            tight(|l| l.max_audio_frames = 4),
            tight(|l| l.max_value_bytes = 6 * 4 * 4),
            tight(|l| l.max_channels = 5),
            tight(|l| l.max_sample_rate = 44_100),
        ] {
            assert!(
                matches!(read(&set, &path, "opus", &l), Err(EngineError::Limit(_))),
                "{l:?}"
            );
        }
        assert!(read(&set, &path, "opus", &tight(|l| l.max_audio_frames = 5)).is_ok());
        // ffprobe の出力の上限は 1 MiB + max_meta_bytes
        let l = Limits {
            max_meta_bytes: 0,
            ..Limits::default()
        };
        let big = format!(",\"x\":\"{}\"", "y".repeat(3_000));
        std::fs::write(t.path("probe.json"), probe_json("opus", 48_000, 6, &big)).unwrap();
        assert!(read(&set, &path, "opus", &l).is_ok());
        let huge = format!(",\"x\":\"{}\"", "y".repeat(1 << 20));
        std::fs::write(t.path("probe.json"), probe_json("opus", 48_000, 6, &huge)).unwrap();
        assert!(matches!(
            read(&set, &path, "opus", &l),
            Err(EngineError::Limit(_))
        ));
    }

    /// 中身が表にない・音声がない・ffprobe が開けないものは `Unsupported`（次の段へ。ここが最後なら「読めません」）。
    #[test]
    fn unknown_contents_are_unsupported() {
        let (t, _) = standard();
        let set = set_with(&t.codecs());
        let path = t.path("in.opus");
        for json in [
            probe_json("vorbis", 48_000, 2, ""),
            r#"{"streams":[],"format":{}}"#.to_string(),
        ] {
            std::fs::write(t.path("probe.json"), json).unwrap();
            assert!(matches!(
                read(&set, &path, "opus", &Limits::default()),
                Err(EngineError::Unsupported(_))
            ));
        }
        // JSON が壊れている
        std::fs::write(t.path("probe.json"), "{oops").unwrap();
        assert!(matches!(
            read(&set, &path, "opus", &Limits::default()),
            Err(EngineError::Decode { .. })
        ));
        // ffprobe が失敗する
        let t2 = FakeTools::new(
            "echo 'Invalid data found when processing input' >&2; exit 1",
            "exit 1",
            "exit 1",
        );
        let set2 = set_with(&t2.codecs());
        match read(&set2, &path, "opus", &Limits::default()) {
            Err(EngineError::Unsupported(m)) => assert!(m.contains("Invalid data"), "{m}"),
            other => panic!("{other:?}"),
        }
    }

    struct CancelOnProgress {
        after: u64,
        flag: AtomicBool,
        at: std::sync::Mutex<Option<Instant>>,
    }

    impl CancelOnProgress {
        fn new(after: u64) -> Self {
            Self {
                after,
                flag: AtomicBool::new(false),
                at: std::sync::Mutex::new(None),
            }
        }
    }

    impl Monitor for CancelOnProgress {
        fn canceled(&self) -> bool {
            self.flag.load(Ordering::SeqCst)
        }
        fn progress(&self, done: u64, _: u64) {
            if done >= self.after
                && !self.flag.swap(true, Ordering::SeqCst)
                && let Ok(mut t) = self.at.lock()
            {
                *t = Some(Instant::now());
            }
        }
    }

    /// 中止は見張りが 20 ms ごとに見る（進捗を出したあとに中止を頼む）。始める前の中止も効く。
    #[test]
    fn decoding_can_be_canceled() {
        let t = fake(
            "cat \"$(dirname \"$0\")/out.wav\"; while true; do head -c 4096 /dev/zero; sleep 0.05; done",
        );
        std::fs::write(t.path("out.wav"), wav_bytes(48_000, &planes(2, 10), None)).unwrap();
        std::fs::write(t.path("probe.json"), probe_json("opus", 48_000, 2, "")).unwrap();
        let codecs = t.codecs();
        let d = codecs
            .decoders()
            .into_iter()
            .find(|d| d.id() == "opus-ffmpeg-dec/1")
            .unwrap();
        let mon = CancelOnProgress::new(1);
        let started = Instant::now();
        let r = d.decode_file(
            &t.path("probe.json"),
            "opus",
            &DecodeOptions::default(),
            &Limits::default(),
            &mon,
        );
        assert!(matches!(r, Err(EngineError::Canceled)), "{r:?}");
        assert!(started.elapsed() < Duration::from_secs(3));
        let flag = CancelFlag::default();
        flag.cancel();
        let r = d.decode_file(
            &t.path("probe.json"),
            "opus",
            &DecodeOptions::default(),
            &Limits::default(),
            &flag,
        );
        assert!(matches!(r, Err(EngineError::Canceled)), "{r:?}");
    }

    /// 時間がたったら中止を頼む監視（形の回は進捗を出さないので、時間で頼む）。
    struct CancelAfter(Instant, Duration);

    impl Monitor for CancelAfter {
        fn canceled(&self) -> bool {
            self.0.elapsed() >= self.1
        }
    }

    /// 形を決める回（1 回目のデコードで数える）も中止が効く：数えている ffmpeg が終わらなくても、中止から見張りの
    /// 刻みほどで `Canceled` で戻る。`CodecSet` で読むときも、形の回に同じ監視が渡る。
    #[test]
    fn counting_for_the_shape_can_be_canceled() {
        let t = fake(
            "cat \"$(dirname \"$0\")/out.wav\"; while true; do head -c 4096 /dev/zero; sleep 0.05; done",
        );
        std::fs::write(t.path("out.wav"), wav_bytes(48_000, &planes(2, 10), None)).unwrap();
        std::fs::write(t.path("probe.json"), probe_json("opus", 48_000, 2, "")).unwrap();
        let codecs = t.codecs();
        let d = codecs
            .decoders()
            .into_iter()
            .find(|d| d.id() == "opus-ffmpeg-dec/1")
            .unwrap();
        let wait = Duration::from_millis(300);
        let mon = CancelAfter(Instant::now(), wait);
        let r = d.shape_file_monitored(
            &t.path("probe.json"),
            "opus",
            &DecodeOptions::default(),
            &Limits::default(),
            &mon,
        );
        assert!(matches!(r, Err(EngineError::Canceled)), "{r:?}");
        assert!(
            mon.0.elapsed() < wait + Duration::from_secs(2),
            "{:?}",
            mon.0.elapsed()
        );

        let set = set_with(&codecs);
        let mon = CancelAfter(Instant::now(), wait);
        let r = set.decode_path(
            &t.path("probe.json"),
            "opus",
            &DecodeOptions::default(),
            &Limits::default(),
            &mon,
        );
        assert!(matches!(r, Err(EngineError::Canceled)), "{r:?}");
        assert!(
            mon.0.elapsed() < wait + Duration::from_secs(2),
            "{:?}",
            mon.0.elapsed()
        );
    }

    /// ffmpeg が消えたら `CodecMissing`。実行ファイルが変わったら検出し直し、記録の環境も新しい版になる。
    #[test]
    fn missing_and_changed_executables() {
        let (t, _) = standard();
        let codecs = t.codecs();
        let set = set_with(&codecs);
        let path = t.path("in.opus");
        assert!(!codecs.changed());
        // 版を変えて、実行ファイルも書き換える（大きさが変わる）
        let v = std::fs::read_to_string(t.path("version.txt")).unwrap();
        std::fs::write(t.path("version.txt"), v.replace("9.0.2", "9.0.3")).unwrap();
        let script = std::fs::read_to_string(&t.ffmpeg).unwrap();
        std::fs::write(&t.ffmpeg, format!("{script}\n# changed\n")).unwrap();
        assert!(codecs.changed());
        let (_, stamp) = read(&set, &path, "opus", &Limits::default()).unwrap();
        assert_eq!(stamp.env.unwrap().version, "9.0.3");
        assert!(!codecs.changed());
        // 消えた
        std::fs::remove_file(&t.ffmpeg).unwrap();
        match read(&set, &path, "opus", &Limits::default()) {
            Err(EngineError::CodecMissing { tool }) => assert_eq!(tool, TOOL),
            other => panic!("{other:?}"),
        }
    }

    /// 能力の調べ方：分離器・デコーダのない形式は足さない。Opus は libopus を先に、なければ内蔵の opus。
    #[test]
    fn kinds_follow_the_detected_capabilities() {
        let (t, _) = standard();
        let codecs = t.codecs();
        let d = codecs.detected();
        let ids: Vec<&str> = codecs.decoders().iter().map(|d| d.id()).collect();
        // 9.0.2 の抜き出しの一覧には aac・adpcm_ms・flac・mp2float・opus・libopus がある（AIFF の中身はない）
        assert_eq!(
            ids,
            [
                "opus-ffmpeg-dec/1",
                "ogg-ffmpeg-dec/1",
                "wav-ffmpeg-dec/1",
                "m4a-ffmpeg-dec/1",
                "aac-ffmpeg-dec/1",
                "mp3-ffmpeg-dec/1"
            ]
        );
        let ogg = &KINDS[1];
        assert_eq!(ogg.decoders_component(&d), "flac=flac,opus=libopus");
        assert_eq!(ogg.probe_decoders(&d), ["libopus", "opus", "flac"]);
        let mut without = (*d).clone();
        without.decoders.remove("libopus");
        assert_eq!(KINDS[0].decoder_for(&without, "opus"), Some("opus"));
        without.decoders.remove("opus");
        assert!(!KINDS[0].available(&without));
        without.demuxers.remove("ogg");
        assert!(!KINDS[1].available(&without));
        // 識別子は重ならず、形式の表にある形式だけ
        let mut seen = std::collections::BTreeSet::new();
        for k in &KINDS {
            assert!(seen.insert(k.id), "{}", k.id);
            assert!(
                kura_engine::codec::format_by_id(k.format).is_some(),
                "{}",
                k.format
            );
            assert!(k.id.starts_with(&format!("{}-ffmpeg-dec/", k.format)));
        }
    }

    // ---- 実物の ffmpeg（あるときだけ。KURA_FFMPEG_REQUIRED=1 ならないときに失敗） ----

    fn real() -> Option<(FfmpegCodecs, PathBuf, tempfile::TempDir)> {
        let (ffmpeg, ffprobe) = real_tools()?;
        let dir = tempfile::tempdir().unwrap();
        let codecs = FfmpegCodecs::detect_at(&ffmpeg, &ffprobe, &dir.path().join("work")).unwrap();
        Some((codecs, ffmpeg, dir))
    }

    fn fixture(name: &str) -> PathBuf {
        PathBuf::from(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../kura-engine/fixtures"
        ))
        .join(name)
    }

    /// 実物の ffmpeg で、決まった PCM（f32le）から `args` の形式のファイルを作る。
    pub(crate) fn make(
        ffmpeg: &Path,
        dir: &Path,
        name: &str,
        rate: u32,
        layout: &str,
        p: &[Vec<f32>],
        args: &[&str],
    ) -> PathBuf {
        let raw = dir.join(format!("{name}.f32"));
        let mut b = Vec::new();
        for i in 0..p[0].len() {
            for c in p {
                b.extend_from_slice(&c[i].to_le_bytes());
            }
        }
        std::fs::write(&raw, b).unwrap();
        let out = dir.join(name);
        let st = std::process::Command::new(ffmpeg)
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-y",
                "-f",
                "f32le",
                "-ar",
            ])
            .arg(rate.to_string())
            .args(["-ch_layout", layout, "-i"])
            .arg(&raw)
            .args(args)
            .arg(&out)
            .status()
            .unwrap();
        assert!(st.success(), "{name}");
        out
    }

    pub(crate) fn sine(ch: usize, frames: usize, rate: u32) -> Vec<Vec<f32>> {
        (0..ch)
            .map(|c| {
                (0..frames)
                    .map(|i| {
                        0.4 * (std::f32::consts::TAU * (220.0 * (c + 1) as f32) * i as f32
                            / rate as f32)
                            .sin()
                    })
                    .collect()
            })
            .collect()
    }

    /// 前の段が断った形式を実物の ffmpeg が読む：Ogg の Opus（ステレオ）、5.1 の Opus、MP2 の .mp3、HE-AAC の m4a、
    /// ADPCM の WAV、Ogg の FLAC（可逆なので 16 ビットに丸めた元とちょうど同じ値）。何度読んでも同じ値。
    #[test]
    fn real_ffmpeg_reads_what_earlier_tiers_refuse() {
        let Some((codecs, ffmpeg, dir)) = real() else {
            return;
        };
        let set = set_with(&codecs);
        let l = Limits::default();
        // ステレオの Opus（フィクスチャ。固定の段の Vorbis は断る）
        let (v, s) = read(&set, &fixture("ogg-opus.ogg"), "ogg", &l).unwrap();
        assert_eq!(s.codec, "ogg-ffmpeg-dec/1");
        let a = audio(&v);
        assert_eq!((a.rate(), a.channels(), a.frames()), (48_000, 2, 48_000));
        let env = s.env.clone().unwrap();
        assert!(
            env.components
                .get("decoders")
                .is_some_and(|c| c.contains("opus=libopus")),
            "{env:?}"
        );
        let (v2, s2) = read(&set, &fixture("ogg-opus.ogg"), "ogg", &l).unwrap();
        assert_eq!((s2.value_digest, &v2), (s.value_digest, &v));
        // 5.1 の Opus（mapping family 1）
        let p = sine(6, 20_000, 48_000);
        let op6 = make(
            &ffmpeg,
            dir.path(),
            "c6.opus",
            48_000,
            "5.1",
            &p,
            &[
                "-c:a",
                "libopus",
                "-b:a",
                "256k",
                "-mapping_family",
                "1",
                "-f",
                "ogg",
            ],
        );
        let (v, s) = read(&set, &op6, "opus", &l).unwrap();
        assert_eq!(s.codec, "opus-ffmpeg-dec/1");
        assert_eq!((audio(&v).channels(), audio(&v).frames()), (6, 20_000));
        // MP2 の .mp3（nanomp3 は断る）
        let (v, s) = read(&set, &fixture("mp2-layer2.mp3"), "mp3", &l).unwrap();
        assert_eq!(s.codec, "mp3-ffmpeg-dec/1");
        assert_eq!((audio(&v).rate(), audio(&v).channels()), (44_100, 2));
        assert!(audio(&v).frames() >= 44_100);
        // HE-AAC の m4a（固定の段の AAC は断る）
        let (v, s) = read(&set, &fixture("aac-he.m4a"), "m4a", &l).unwrap();
        assert_eq!(s.codec, "m4a-ffmpeg-dec/1");
        assert_eq!((audio(&v).rate(), audio(&v).channels()), (44_100, 2));
        // ADPCM の WAV（固定の段の WAV は断る）
        let p = sine(2, 10_000, 44_100);
        let adpcm = make(
            &ffmpeg,
            dir.path(),
            "ms.wav",
            44_100,
            "stereo",
            &p,
            &["-c:a", "adpcm_ms", "-f", "wav"],
        );
        let (v, s) = read(&set, &adpcm, "wav", &l).unwrap();
        assert_eq!(s.codec, "wav-ffmpeg-dec/1");
        assert!(audio(&v).frames() >= 10_000);
        // Ogg の FLAC：16 ビットに丸めた値とちょうど同じ
        let p = sine(1, 5_000, 44_100);
        let oflac = make(
            &ffmpeg,
            dir.path(),
            "f.ogg",
            44_100,
            "mono",
            &p,
            &["-c:a", "flac", "-sample_fmt", "s16", "-f", "ogg"],
        );
        let (v, s) = read(&set, &oflac, "ogg", &l).unwrap();
        assert_eq!(s.codec, "ogg-ffmpeg-dec/1");
        let a = audio(&v);
        assert_eq!(a.frames(), 5_000);
        assert_eq!(a.meta.source.map(|s| s.bits), Some(16));
        for (got, want) in a.planes()[0].iter().zip(&p[0]) {
            let q = (want * 32_768.0).round().clamp(-32_768.0, 32_767.0) / 32_768.0;
            assert_eq!(*got, q);
        }
    }

    /// 実物の ffmpeg：壊れた Ogg（CRC の誤り。ffmpeg は終了コード 0 で短い出力を返す）・途中で切れたファイルは失敗、
    /// 上限・中止が効く。
    #[test]
    fn real_ffmpeg_failures_limits_and_cancel() {
        let Some((codecs, ffmpeg, dir)) = real() else {
            return;
        };
        let set = set_with(&codecs);
        let l = Limits::default();
        let p = sine(2, 96_000, 48_000);
        let good = make(
            &ffmpeg,
            dir.path(),
            "g.opus",
            48_000,
            "stereo",
            &p,
            &["-c:a", "libopus", "-b:a", "96k", "-f", "ogg"],
        );
        let bytes = std::fs::read(&good).unwrap();
        // 中ほどの 40 バイトを壊す
        let mut bad = bytes.clone();
        let mid = bad.len() / 2;
        for b in &mut bad[mid..mid + 40] {
            *b ^= 0x5A;
        }
        let bad_path = dir.path().join("bad.opus");
        std::fs::write(&bad_path, &bad).unwrap();
        let r = read(&set, &bad_path, "opus", &l);
        assert!(matches!(r, Err(EngineError::Decode { .. })), "{r:?}");
        // 途中で切る
        let cut_path = dir.path().join("cut.opus");
        std::fs::write(&cut_path, &bytes[..bytes.len() / 3]).unwrap();
        assert!(read(&set, &cut_path, "opus", &l).is_err());
        // 上限
        let tight = Limits {
            max_audio_frames: 1_000,
            ..Limits::default()
        };
        assert!(matches!(
            read(&set, &good, "opus", &tight),
            Err(EngineError::Limit(_))
        ));
        // 中止（長いファイルを読み始めたところで）
        let long = make(
            &ffmpeg,
            dir.path(),
            "long.opus",
            48_000,
            "stereo",
            &sine(2, 48_000 * 120, 48_000),
            &["-c:a", "libopus", "-b:a", "64k", "-f", "ogg"],
        );
        let d = codecs
            .decoders()
            .into_iter()
            .find(|d| d.id() == "opus-ffmpeg-dec/1")
            .unwrap();
        let mon = CancelOnProgress::new(48_000);
        let r = d.decode_file(&long, "opus", &DecodeOptions::default(), &l, &mon);
        assert!(matches!(r, Err(EngineError::Canceled)), "{r:?}");
        let at = mon.at.lock().unwrap().unwrap();
        assert!(
            at.elapsed() < Duration::from_millis(500),
            "{:?}",
            at.elapsed()
        );
    }

    /// 実測（手で回す）：`cargo test -p kura-core --release --lib measure_real_ffmpeg -- --ignored --nocapture`。
    /// 検出、3 分・44.1 kHz・ステレオの元から作った Opus（48 kHz）の形と値（ffprobe 2 回・デコード 2 回）、同じ長さの MP3 の
    /// 書き出し、中止の反応。
    #[test]
    #[ignore = "実測。手で回す"]
    fn measure_real_ffmpeg() {
        let Some((ffmpeg, ffprobe)) = real_tools() else {
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        let t = Instant::now();
        let codecs = FfmpegCodecs::detect_at(&ffmpeg, &ffprobe, &dir.path().join("work")).unwrap();
        println!("検出: {:?}", t.elapsed());
        let set = set_with(&codecs);
        let src = sine(2, 44_100 * 180, 44_100);
        let opus = make(
            &ffmpeg,
            dir.path(),
            "m.opus",
            44_100,
            "stereo",
            &src,
            &["-c:a", "libopus", "-b:a", "128k", "-f", "ogg"],
        );
        let t = Instant::now();
        let (v, _) = read(&set, &opus, "opus", &Limits::default()).unwrap();
        println!(
            "3 分の Opus の読み取り（形＋値）: {:?}（{} フレーム）",
            t.elapsed(),
            audio(&v).frames()
        );
        let t = Instant::now();
        set.shape_path(&opus, "opus", &DecodeOptions::default(), &Limits::default())
            .unwrap();
        println!("  うち形（ffprobe＋数える回）: {:?}", t.elapsed());
        let value = Value::Audio(Arc::new(
            Audio::new(
                44_100,
                src.into_iter().map(Arc::new).collect(),
                kura_engine::value::AudioMeta::default(),
            )
            .unwrap(),
        ));
        let t = Instant::now();
        let (mp3, _) = set
            .encode_bytes(&value, "mp3", &serde_json::json!({}), &NoMonitor)
            .unwrap();
        println!(
            "3 分の MP3 の書き出し（CBR 192）: {:?}（{} バイト）",
            t.elapsed(),
            mp3.len()
        );
        let d = codecs
            .decoders()
            .into_iter()
            .find(|d| d.id() == "opus-ffmpeg-dec/1")
            .unwrap();
        let mon = CancelOnProgress::new(48_000);
        let r = d.decode_file(
            &opus,
            "opus",
            &DecodeOptions::default(),
            &Limits::default(),
            &mon,
        );
        assert!(matches!(r, Err(EngineError::Canceled)));
        println!(
            "中止の反応: {:?}",
            mon.at.lock().unwrap().unwrap().elapsed()
        );
    }
}
