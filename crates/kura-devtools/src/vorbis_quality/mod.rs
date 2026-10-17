//! `vorbis-quality`：自作の Ogg Vorbis のエンコーダの品質を記録する（M10 段 7。`docs/decisions.md` の ADR-149 (13)）。
//!
//! ```text
//! cargo run -p kura-devtools --release -- vorbis-quality --out <dir> [--quick] [--q <-1,3,…>]
//!     [--input [<名前>=]<wav・flac など>]... [--reference <音>/<ラベル>=<ogg>]... [--reference-dir <dir>]...
//!     [--curve-ref <族>] [--only <音の名前（末尾 * で前方一致）>]... [--seed <n>]
//! ```
//!
//! - 試験の音：記述から作る合成の音（[`signals`]）と `--input` のファイル（kura の `CodecSet` で読む）。
//!   合成の音は `<dir>/signals/<名前>.wav`（16 ビット）にも書く（外のエンコーダで参照を作るため）。
//! - 各音を kura の Ogg Vorbis のエンコーダ（`CodecSet::pinned().encode_bytes(v, "ogg", {"quality": q})`）で
//!   q −1〜10（`--quick` は −1・3・5・10）に書き（`<dir>/ogg/<音>.kura-q<q>.ogg`）、symphonia（kura の読み取り）で読んで
//!   指標を測る。lewton でも読み、長さと値の一致を見る。書けない q は飛ばし、理由を表に書く。
//! - `--reference <音>/<ラベル>=<ogg>`：外のエンコーダ（libvorbis など）で同じ音から作った ogg。ラベルの末尾の
//!   `-q<n>` を段、その前を族（`aotuv-q3` → 族 `aotuv`・段 3）として、同じ指標で測り同じ表に並べる。
//!   同じビットレートでの比較の基準は `--curve-ref` の族（既定は `aotuv`、なければ最初の参照の族）。
//!   `--reference-dir <dir>` は `<dir>/<音>.<ラベル>.ogg`（例 `music-48k-st.aotuv-q3.ogg`）をすべて参照にする
//!   （名前の順。測らない音のものは飛ばす）。
//! - 出力：`<dir>/results.jsonl`（1 行 1 測定）と `<dir>/results.md`（表）。同じ入力なら同じ数字。
//!
//! 指標の定義は [`metrics`] の冒頭、表の形は [`report`] の冒頭に書いた（どちらもそこが正）。出どころは M10 段 7 の品質の
//! 調査（作業の記録。リポジトリにはない）で、その調査の Python（numpy）の指標と同じ入力・同じ参照の ogg で照らし、表の桁で
//! 一致した（例：調査の合成の音楽（48 kHz・ステレオ・8 秒）の aoTuV q0 で 58.2 kbps・上限 14.9 kHz・NMR 全体 −9.78 dB・
//! p95 −5.6・最悪 −3.4@1.88 秒・乱れたフレーム 0.27・LSD 8.6、ffmpeg の内蔵のエンコーダの打撃の音の q4 で NMR 14.24 dB）。
//! 違うのは最後の半端のフレームを数えることだけ（1 秒の `crates/kura-engine/fixtures/audio-src16.wav` で NMR が 0.1 dB ほど動く）。

mod metrics;
mod ogg;
mod report;
mod signals;

use std::collections::BTreeMap;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use kura_engine::codec::{CodecSet, format_by_ext};
use kura_engine::doc::DecodeOptions;
use kura_engine::value::{Audio, AudioMeta};
use kura_engine::{Limits, NoMonitor, Value};
use rayon::prelude::*;
use serde_json::json;

use metrics::{Model, Nmr, Spectral};
use ogg::OggStats;

const USAGE: &str = "使い方: kura-devtools vorbis-quality --out <dir> [--quick] [--q <-1,3,…>] \
                     [--input [<名前>=]<file>]... [--reference <音>/<ラベル>=<ogg>]... \
                     [--reference-dir <dir>]... [--curve-ref <族>] [--only <名前>]... [--seed <n>]";

/// 道具の版（指標の定義・表の形を変えたら上げる）。
pub const TOOL_ID: &str = "kura-devtools vorbis-quality/1";
/// 既定の乱数の種。
const DEFAULT_SEED: u64 = 20_261_017;
/// 全部の段。
const ALL_Q: [i32; 12] = [-1, 0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10];
/// `--quick` の段。
const QUICK_Q: [i32; 4] = [-1, 3, 5, 10];
/// kura のエンコーダの族の名前（表の「エンコーダ」の列）。
pub const KURA: &str = "kura";
/// lewton の版（Cargo.toml で固定）。
pub const LEWTON_ID: &str = "lewton 0.10.2";
/// これより短い音を「短い効果音」として表 4 にまとめる（秒）。
pub const SHORT_SECONDS: f64 = 2.0;

/// 音の出所。
#[derive(Debug, Clone)]
pub enum Source {
    Synth { description: &'static str },
    Input { path: PathBuf },
}

/// 試験の音（16 ビットに丸めた合成の音か、読んだファイルの値）。
pub struct Sound {
    pub name: String,
    pub source: Source,
    pub rate: u32,
    pub planes: Vec<Vec<f32>>,
    /// WAV（合成の音）か入力のファイルの SHA-256。
    pub sha256: String,
}

impl Sound {
    /// フレームの数。
    pub fn frames(&self) -> usize {
        self.planes.first().map_or(0, Vec::len)
    }

    /// 秒。
    pub fn seconds(&self) -> f64 {
        self.frames() as f64 / f64::from(self.rate)
    }
}

/// 外のエンコーダで作った参照。
pub struct Reference {
    pub sound: String,
    pub label: String,
    pub family: String,
    pub q: Option<i32>,
    pub path: PathBuf,
    pub sha256: String,
    bytes: Vec<u8>,
}

/// 1 つの測定の値。
#[derive(Debug, Clone)]
pub struct Measured {
    pub stats: OggStats,
    /// 指標に使った読み取り（symphonia）の識別子。
    pub decoder: String,
    pub frames_symphonia: usize,
    pub frames_lewton: Option<usize>,
    /// lewton が最後のグラニュールで切る前の長さ。
    pub frames_lewton_raw: Option<usize>,
    pub lewton_error: Option<String>,
    /// lewton と symphonia の値の差の最大（そろえずに、共通の長さで）。
    pub lewton_max_abs_diff: Option<f64>,
    pub lag: i64,
    pub nmr: Nmr,
    pub spectral: Spectral,
    pub snr_db: f64,
    pub kbps_audio: f64,
    pub kbps_file: f64,
    pub kbps_nominal: Option<f64>,
}

/// 測定の結果の種類。
#[derive(Debug, Clone)]
pub enum Outcome {
    Ok(Box<Measured>),
    /// 書けなかった（エンコーダが断った）。
    Skipped(String),
    /// 読めなかった・測れなかった。
    Failed(String),
}

/// 1 行（1 測定）。
#[derive(Debug, Clone)]
pub struct Row {
    pub sound: usize,
    pub family: String,
    pub label: String,
    pub q: Option<i32>,
    /// エンコーダの識別子（kura）か vendor（参照）。
    pub codec: String,
    /// ogg のファイル（kura は出力の中の相対の位置、参照は渡された位置）。
    pub file: Option<String>,
    pub outcome: Outcome,
}

impl Row {
    /// 測れた値。
    pub fn measured(&self) -> Option<&Measured> {
        match &self.outcome {
            Outcome::Ok(m) => Some(m),
            _ => None,
        }
    }
}

/// 実行の設定と、頭書きに書くもの。
pub struct Context {
    pub args: Vec<String>,
    pub commit: String,
    pub encoder_id: String,
    pub decoder_id: String,
    pub seed: u64,
    pub qs: Vec<i32>,
    pub curve_ref: Option<String>,
}

struct Options {
    out: PathBuf,
    qs: Vec<i32>,
    inputs: Vec<(Option<String>, PathBuf)>,
    references: Vec<(String, String, PathBuf)>,
    reference_dirs: Vec<PathBuf>,
    curve_ref: Option<String>,
    only: Vec<String>,
    seed: u64,
}

fn parse(args: &[String]) -> Result<Options, String> {
    let mut out = None;
    let mut quick = false;
    let mut qs: Option<Vec<i32>> = None;
    let mut inputs = Vec::new();
    let mut references = Vec::new();
    let mut reference_dirs = Vec::new();
    let mut curve_ref = None;
    let mut only = Vec::new();
    let mut seed = DEFAULT_SEED;
    let mut it = args.iter();
    let need = |v: Option<&String>, name: &str| -> Result<String, String> {
        v.cloned()
            .ok_or_else(|| format!("{name} の値がありません。{USAGE}"))
    };
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--out" => out = Some(PathBuf::from(need(it.next(), "--out")?)),
            "--quick" => quick = true,
            "--q" => {
                let v = need(it.next(), "--q")?;
                let list = v
                    .split(',')
                    .map(|s| s.trim().parse::<i32>().map_err(|e| format!("--q {v}: {e}")))
                    .collect::<Result<Vec<_>, _>>()?;
                qs = Some(list);
            }
            "--input" => {
                let v = need(it.next(), "--input")?;
                let named = v.split_once('=').filter(|(n, _)| {
                    !n.is_empty()
                        && n.chars()
                            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
                });
                match named {
                    Some((n, p)) => inputs.push((Some(n.to_string()), PathBuf::from(p))),
                    None => inputs.push((None, PathBuf::from(v))),
                }
            }
            "--reference" => {
                let v = need(it.next(), "--reference")?;
                let (name, path) = v
                    .split_once('=')
                    .ok_or_else(|| format!("--reference は <音>/<ラベル>=<ogg> の形です: {v}"))?;
                let (sound, label) = name
                    .split_once('/')
                    .filter(|(s, l)| !s.is_empty() && !l.is_empty())
                    .ok_or_else(|| format!("--reference は <音>/<ラベル>=<ogg> の形です: {v}"))?;
                references.push((sound.to_string(), label.to_string(), PathBuf::from(path)));
            }
            "--reference-dir" => {
                reference_dirs.push(PathBuf::from(need(it.next(), "--reference-dir")?))
            }
            "--curve-ref" => curve_ref = Some(need(it.next(), "--curve-ref")?),
            "--only" => only.push(need(it.next(), "--only")?),
            "--seed" => {
                let v = need(it.next(), "--seed")?;
                seed = v.parse().map_err(|e| format!("--seed {v}: {e}"))?;
            }
            other => return Err(format!("知らない引数 {other}。{USAGE}")),
        }
    }
    let qs = qs.unwrap_or_else(|| {
        if quick {
            QUICK_Q.to_vec()
        } else {
            ALL_Q.to_vec()
        }
    });
    if let Some(q) = qs.iter().find(|q| !(-1..=10).contains(*q)) {
        return Err(format!("段 {q} は −1〜10 の外です"));
    }
    Ok(Options {
        out: out.ok_or(USAGE)?,
        qs,
        inputs,
        references,
        reference_dirs,
        curve_ref,
        only,
        seed,
    })
}

fn sha256(bytes: &[u8]) -> String {
    use sha2::Digest as _;
    let h = sha2::Sha256::digest(bytes);
    h.iter().map(|b| format!("{b:02x}")).collect()
}

fn matches_only(only: &[String], name: &str) -> bool {
    only.is_empty()
        || only.iter().any(|p| match p.strip_suffix('*') {
            Some(prefix) => name.starts_with(prefix),
            None => name == p,
        })
}

/// 入力のファイルを kura の `CodecSet` で読む。
fn read_input(codecs: &CodecSet, path: &Path) -> Result<(u32, Vec<Vec<f32>>), String> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let format = format_by_ext(&ext)
        .map(|f| f.id)
        .ok_or_else(|| format!("{} の形式が分かりません", path.display()))?;
    let (value, _) = codecs
        .decode_path(
            path,
            format,
            &DecodeOptions::default(),
            &Limits::default(),
            &NoMonitor,
        )
        .map_err(|e| format!("{} を読めません: {e}", path.display()))?;
    match value {
        Value::Audio(a) => Ok((a.rate(), a.planes().iter().map(|p| p.to_vec()).collect())),
        _ => Err(format!("{} は音声ではありません", path.display())),
    }
}

fn audio_value(sound: &Sound) -> Result<Value, String> {
    let planes = sound.planes.iter().map(|p| Arc::new(p.clone())).collect();
    Audio::new(sound.rate, planes, AudioMeta::default())
        .map(|a| Value::Audio(Arc::new(a)))
        .map_err(|e| e.to_string())
}

fn to_f64(planes: &[Vec<f32>]) -> Vec<Vec<f64>> {
    planes
        .iter()
        .map(|p| p.iter().map(|&v| f64::from(v)).collect())
        .collect()
}

/// ogg のバイト列を読んで、元と比べる。
fn measure(
    codecs: &CodecSet,
    sound: &Sound,
    bytes: &[u8],
    model: &Model,
) -> Result<Measured, String> {
    let (packets, stats) = ogg::parse(bytes)?;
    let (value, stamp) = codecs
        .decode(
            &mut Cursor::new(bytes),
            "ogg",
            &DecodeOptions::default(),
            &Limits::default(),
            &NoMonitor,
        )
        .map_err(|e| format!("symphonia で読めません: {e}"))?;
    let Value::Audio(a) = value else {
        return Err("音声ではありません".into());
    };
    if a.rate() != sound.rate || a.channels() != sound.planes.len() {
        return Err(format!(
            "レート・チャンネルが元と違います（{} Hz・{} ch。元は {} Hz・{} ch）",
            a.rate(),
            a.channels(),
            sound.rate,
            sound.planes.len()
        ));
    }
    let sym: Vec<Vec<f32>> = a.planes().iter().map(|p| p.to_vec()).collect();
    let frames_symphonia = a.frames();

    let (frames_lewton, frames_lewton_raw, lewton_error, lewton_max_abs_diff) =
        match ogg::decode_lewton(&packets, &stats) {
            Ok(l) => {
                let len = l.planes.first().map_or(0, Vec::len);
                let mut diff = 0.0f64;
                for (x, y) in l.planes.iter().zip(&sym) {
                    for (u, v) in x.iter().zip(y) {
                        diff = diff.max((f64::from(*u) - f64::from(*v)).abs());
                    }
                }
                let same_ch = l.planes.len() == sym.len();
                (Some(len), Some(l.raw_frames), None, same_ch.then_some(diff))
            }
            Err(e) => (None, None, Some(e), None),
        };

    let src = to_f64(&sound.planes);
    let dec = to_f64(&sym);
    let lag = metrics::lag(&src, &dec)?;
    let mut t = metrics::align(&dec, lag);
    let n = sound.frames().min(t.first().map_or(0, Vec::len));
    let mut r = src;
    for c in r.iter_mut().chain(t.iter_mut()) {
        c.truncate(n);
    }
    if n == 0 {
        return Err("読んだ音が空です".into());
    }
    let nmr = model.nmr(&r, &t);
    let spectral = metrics::spectral(&r, &t, sound.rate)?;
    let snr_db = metrics::snr_db(&r, &t);
    let secs = sound.seconds();
    let kbps = |b: u64| b as f64 * 8.0 / secs / 1000.0;
    Ok(Measured {
        kbps_audio: kbps(stats.audio_bytes),
        kbps_file: kbps(stats.file_bytes),
        kbps_nominal: (stats.bitrate_nominal > 0)
            .then(|| f64::from(stats.bitrate_nominal) / 1000.0),
        stats,
        decoder: stamp.codec,
        frames_symphonia,
        frames_lewton,
        frames_lewton_raw,
        lewton_error,
        lewton_max_abs_diff,
        lag,
        nmr,
        spectral,
        snr_db,
    })
}

/// 測る仕事。
enum Job {
    Kura { sound: usize, q: i32 },
    Reference { index: usize, sound: usize },
}

/// 1 つの仕事を回す。
fn run_job(
    job: &Job,
    codecs: &CodecSet,
    sounds: &[Sound],
    refs: &[Reference],
    models: &BTreeMap<u32, Model>,
    out: &Path,
) -> Row {
    let model_of = |s: &Sound| {
        models
            .get(&s.rate)
            .ok_or_else(|| format!("{} Hz のモデルがありません", s.rate))
    };
    match *job {
        Job::Kura { sound, q } => {
            let s = &sounds[sound];
            let label = format!("{KURA}-q{q}");
            let mut row = Row {
                sound,
                family: KURA.to_string(),
                label: label.clone(),
                q: Some(q),
                codec: String::new(),
                file: None,
                outcome: Outcome::Skipped(String::new()),
            };
            let value = match audio_value(s) {
                Ok(v) => v,
                Err(e) => {
                    row.outcome = Outcome::Failed(e);
                    return row;
                }
            };
            row.codec = codecs
                .encoder("ogg", &value.shape())
                .map(|e| e.id().to_string())
                .unwrap_or_else(|| "（ogg のエンコーダがない）".into());
            let encoded = codecs.encode_bytes(&value, "ogg", &json!({ "quality": q }), &NoMonitor);
            let bytes = match encoded {
                Ok((bytes, stamp)) => {
                    row.codec = stamp.codec;
                    bytes
                }
                Err(e) => {
                    row.outcome = Outcome::Skipped(e.to_string());
                    return row;
                }
            };
            let rel = format!("ogg/{}.{label}.ogg", s.name);
            if let Err(e) = std::fs::write(out.join(&rel), &bytes) {
                row.outcome = Outcome::Failed(format!("{rel} を書けません: {e}"));
                return row;
            }
            row.file = Some(rel);
            row.outcome = match model_of(s).and_then(|m| measure(codecs, s, &bytes, m)) {
                Ok(m) => Outcome::Ok(Box::new(m)),
                Err(e) => Outcome::Failed(e),
            };
            row
        }
        Job::Reference { index, sound } => {
            let r = &refs[index];
            let s = &sounds[sound];
            let vendor = ogg::parse(&r.bytes)
                .map(|(_, st)| st.vendor)
                .unwrap_or_default();
            Row {
                sound,
                family: r.family.clone(),
                label: r.label.clone(),
                q: r.q,
                codec: vendor,
                file: Some(r.path.display().to_string()),
                outcome: match model_of(s).and_then(|m| measure(codecs, s, &r.bytes, m)) {
                    Ok(m) => Outcome::Ok(Box::new(m)),
                    Err(e) => Outcome::Failed(e),
                },
            }
        }
    }
}

/// ラベルの末尾の `-q<n>` を段に、その前を族にする（`aotuv-q3` → (`aotuv`, 3)、`xiph-q-1` → (`xiph`, −1)）。
fn family_and_q(label: &str) -> (String, Option<i32>) {
    match label.rsplit_once("-q") {
        Some((f, q)) if !f.is_empty() => match q.parse::<i32>() {
            Ok(q) => (f.to_string(), Some(q)),
            Err(_) => (label.to_string(), None),
        },
        _ => (label.to_string(), None),
    }
}

fn git(args: &[&str]) -> Option<String> {
    let out = std::process::Command::new("git").args(args).output().ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// 1 行の JSON。
fn row_json(row: &Row, sound: &Sound) -> serde_json::Value {
    let mut v = json!({
        "tool": TOOL_ID,
        "sound": sound.name,
        "rate": sound.rate,
        "channels": sound.planes.len(),
        "frames_ref": sound.frames(),
        "encoder": row.family,
        "label": row.label,
        "q": row.q,
        "codec": row.codec,
        "file": row.file,
    });
    let extra = match &row.outcome {
        Outcome::Skipped(reason) => json!({ "status": "skipped", "reason": reason }),
        Outcome::Failed(reason) => json!({ "status": "error", "reason": reason }),
        Outcome::Ok(m) => {
            let bands: serde_json::Map<String, serde_json::Value> = metrics::SNR_BANDS
                .iter()
                .zip(&m.spectral.band_snr_db)
                .map(|(&(lo, hi), v)| (format!("{}-{}k", lo / 1000, hi / 1000), json!(v)))
                .collect();
            json!({
                "status": "ok",
                "decoder": m.decoder,
                "frames_symphonia": m.frames_symphonia,
                "frames_lewton": m.frames_lewton,
                "frames_lewton_raw": m.frames_lewton_raw,
                "lewton_error": m.lewton_error,
                "lewton_max_abs_diff": m.lewton_max_abs_diff,
                "lag": m.lag,
                "file_bytes": m.stats.file_bytes,
                "header_bytes": m.stats.header_bytes,
                "audio_bytes": m.stats.audio_bytes,
                "audio_packets": m.stats.audio_packets,
                "pages": m.stats.pages,
                "audio_pages": m.stats.audio_pages,
                "last_granule": m.stats.last_granule,
                "eos": m.stats.eos,
                "vendor": m.stats.vendor,
                "kbps_audio": m.kbps_audio,
                "kbps_file": m.kbps_file,
                "kbps_nominal": m.kbps_nominal,
                "cutoff_hz": m.spectral.cutoff_hz,
                "nmr_total_db": m.nmr.total_db,
                "nmr_median_db": m.nmr.median_db,
                "nmr_p95_db": m.nmr.p95_db,
                "nmr_max_db": m.nmr.max_db,
                "nmr_worst_at_s": m.nmr.worst_at_s,
                "nmr_worst_ch": m.nmr.worst_ch,
                "rel_dist_frames": m.nmr.rel_dist_frames,
                "nmr_frames": m.nmr.frames,
                "lsd_db": m.spectral.lsd_db,
                "snr_db": m.snr_db,
                "snr_bands_db": bands,
            })
        }
    };
    if let (Some(a), serde_json::Value::Object(b)) = (v.as_object_mut(), extra) {
        a.extend(b);
    }
    v
}

/// `vorbis-quality` の入口。
pub fn run(args: &[String]) -> Result<(), String> {
    let opts = parse(args)?;
    let codecs = CodecSet::pinned();
    std::fs::create_dir_all(opts.out.join("signals")).map_err(|e| e.to_string())?;
    std::fs::create_dir_all(opts.out.join("ogg")).map_err(|e| e.to_string())?;

    // 音を集める（合成の音 → 入力の順）
    let mut sounds: Vec<Sound> = Vec::new();
    for spec in signals::default_set() {
        if !matches_only(&opts.only, &spec.name) {
            continue;
        }
        let ints = signals::generate(&spec, opts.seed);
        let wav = signals::wav16(spec.rate, &ints);
        let path = opts.out.join("signals").join(format!("{}.wav", spec.name));
        std::fs::write(&path, &wav).map_err(|e| format!("{} を書けません: {e}", path.display()))?;
        sounds.push(Sound {
            name: spec.name.clone(),
            source: Source::Synth {
                description: spec.kind.describe(),
            },
            rate: spec.rate,
            planes: ints
                .iter()
                .map(|c| c.iter().map(|&v| f32::from(v) / 32768.0).collect())
                .collect(),
            sha256: sha256(&wav),
        });
    }
    for (name, path) in &opts.inputs {
        let name = match name {
            Some(n) => n.clone(),
            None => path
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .ok_or_else(|| format!("{} の名前が分かりません", path.display()))?,
        };
        if !matches_only(&opts.only, &name) {
            continue;
        }
        if sounds.iter().any(|s| s.name == name) {
            return Err(format!(
                "音の名前 {name} が重なります（--input <名前>=<file> で名前を付けてください）"
            ));
        }
        let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let (rate, planes) = read_input(&codecs, path)?;
        sounds.push(Sound {
            name,
            source: Source::Input { path: path.clone() },
            rate,
            planes,
            sha256: sha256(&bytes),
        });
    }
    if sounds.is_empty() {
        return Err("測る音がありません（--only を確かめてください）".into());
    }

    // 参照（明示したもの → フォルダのもの）
    let mut wanted: Vec<(String, String, PathBuf, bool)> = opts
        .references
        .iter()
        .map(|(s, l, p)| (s.clone(), l.clone(), p.clone(), true))
        .collect();
    for dir in &opts.reference_dirs {
        let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
            .map_err(|e| format!("{}: {e}", dir.display()))?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("ogg")))
            .collect();
        files.sort();
        for path in files {
            let stem = path
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            if let Some((sound, label)) = stem
                .split_once('.')
                .filter(|(s, l)| !s.is_empty() && !l.is_empty())
            {
                wanted.push((sound.to_string(), label.to_string(), path.clone(), false));
            }
        }
    }
    let mut refs: Vec<Reference> = Vec::new();
    for (sound, label, path, explicit) in &wanted {
        if !sounds.iter().any(|s| &s.name == sound) {
            if *explicit && matches_only(&opts.only, sound) {
                return Err(format!("参照 {sound}/{label} の音 {sound} がありません"));
            }
            continue;
        }
        if refs.iter().any(|r| &r.sound == sound && &r.label == label) {
            return Err(format!("参照 {sound}/{label} が重なります"));
        }
        let (family, q) = family_and_q(label);
        if family == KURA {
            // フォルダの中の kura の出力（`ogg/`）は参照にしない
            if *explicit {
                return Err(format!("参照のラベルに {KURA} は使えません: {label}"));
            }
            continue;
        }
        let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
        refs.push(Reference {
            sound: sound.clone(),
            label: label.clone(),
            family,
            q,
            path: path.clone(),
            sha256: sha256(&bytes),
            bytes,
        });
    }

    // サンプルレートごとの耳のモデル
    let mut models = BTreeMap::new();
    for s in &sounds {
        if let std::collections::btree_map::Entry::Vacant(e) = models.entry(s.rate) {
            e.insert(Model::new(s.rate)?);
        }
    }

    // 仕事：音ごとに kura の段、続けてその音の参照
    let mut jobs = Vec::new();
    for (i, s) in sounds.iter().enumerate() {
        for &q in &opts.qs {
            jobs.push(Job::Kura { sound: i, q });
        }
        for (index, r) in refs.iter().enumerate() {
            if r.sound == s.name {
                jobs.push(Job::Reference { index, sound: i });
            }
        }
    }
    tracing::info!(
        sounds = sounds.len(),
        references = refs.len(),
        jobs = jobs.len(),
        "測ります"
    );
    let rows: Vec<Row> = jobs
        .par_iter()
        .map(|job| {
            let row = run_job(job, &codecs, &sounds, &refs, &models, &opts.out);
            match &row.outcome {
                Outcome::Ok(m) => tracing::info!(
                    sound = %sounds[row.sound].name,
                    label = %row.label,
                    kbps = format!("{:.1}", m.kbps_audio),
                    nmr = format!("{:.2}", m.nmr.total_db),
                    "測りました"
                ),
                Outcome::Skipped(r) => {
                    tracing::debug!(sound = %sounds[row.sound].name, label = %row.label, "飛ばしました：{r}")
                }
                Outcome::Failed(r) => {
                    tracing::warn!(sound = %sounds[row.sound].name, label = %row.label, "測れません：{r}")
                }
            }
            row
        })
        .collect();

    let mut jsonl = String::new();
    for row in &rows {
        jsonl.push_str(&row_json(row, &sounds[row.sound]).to_string());
        jsonl.push('\n');
    }
    let jsonl_path = opts.out.join("results.jsonl");
    std::fs::write(&jsonl_path, jsonl).map_err(|e| e.to_string())?;

    let curve_ref = opts.curve_ref.clone().or_else(|| {
        if refs.iter().any(|r| r.family == "aotuv") {
            Some("aotuv".to_string())
        } else {
            refs.first().map(|r| r.family.clone())
        }
    });
    let probe = sounds.first().map(audio_value).transpose()?;
    let encoder_id = probe
        .as_ref()
        .and_then(|v| codecs.encoder("ogg", &v.shape()))
        .map_or_else(|| "（なし）".to_string(), |e| e.id().to_string());
    let decoder_id = codecs
        .decoders_for("ogg")
        .first()
        .map_or_else(|| "（なし）".to_string(), |d| d.id().to_string());
    let dirty =
        git(&["status", "--porcelain", "--untracked-files=no"]).is_some_and(|s| !s.is_empty());
    let commit = git(&["rev-parse", "HEAD"]).map_or_else(
        || "不明".to_string(),
        |c| {
            if dirty {
                format!("{c}（作業ツリーに変更あり）")
            } else {
                c
            }
        },
    );
    let ctx = Context {
        args: args.to_vec(),
        commit,
        encoder_id,
        decoder_id,
        seed: opts.seed,
        qs: opts.qs.clone(),
        curve_ref,
    };
    let md = report::render(&ctx, &sounds, &refs, &rows);
    let md_path = opts.out.join("results.md");
    std::fs::write(&md_path, md).map_err(|e| e.to_string())?;
    let ok = rows.iter().filter(|r| r.measured().is_some()).count();
    let skipped = rows
        .iter()
        .filter(|r| matches!(r.outcome, Outcome::Skipped(_)))
        .count();
    let failed = rows.len() - ok - skipped;
    tracing::info!(
        ok,
        skipped,
        failed,
        jsonl = %jsonl_path.display(),
        markdown = %md_path.display(),
        "書きました"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_and_options() {
        assert_eq!(family_and_q("aotuv-q3"), ("aotuv".into(), Some(3)));
        assert_eq!(family_and_q("xiph-q-1"), ("xiph".into(), Some(-1)));
        assert_eq!(family_and_q("ffvorbis-q10"), ("ffvorbis".into(), Some(10)));
        assert_eq!(family_and_q("lame-abr112"), ("lame-abr112".into(), None));
        assert_eq!(family_and_q("-q3"), ("-q3".into(), None));
        let args: Vec<String> = [
            "--out",
            "/tmp/x",
            "--quick",
            "--input",
            "a=/p/a.wav",
            "--input",
            "/p/b=c.flac",
            "--reference",
            "music-48k-st/aotuv-q3=/r/m.ogg",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let o = parse(&args).unwrap();
        assert_eq!(o.qs, QUICK_Q.to_vec());
        assert_eq!(o.inputs[0], (Some("a".into()), PathBuf::from("/p/a.wav")));
        assert_eq!(o.inputs[1], (None, PathBuf::from("/p/b=c.flac")));
        assert_eq!(o.references[0].0, "music-48k-st");
        assert_eq!(o.references[0].1, "aotuv-q3");
        assert!(parse(&["--out".to_string(), "x".into(), "--q".into(), "11".into()]).is_err());
        assert!(parse(&["--reference".to_string(), "x=y".into()]).is_err());
        assert!(matches_only(&[], "x"));
        assert!(matches_only(&["music*".into()], "music-48k-st"));
        assert!(!matches_only(&["music".into()], "music-48k-st"));
        assert_eq!(
            sha256(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    /// 全体の流れ：短い合成の音を書いて読む。今の段階でエンコーダが断るなら、飛ばした理由が残る。
    /// 書けるなら、長さ・位置が元と同じで、指標が有限。
    #[test]
    fn pipeline_on_a_short_sound() {
        let codecs = CodecSet::pinned();
        let spec = signals::Spec {
            name: "t".into(),
            kind: signals::Kind::Music,
            rate: 44_100,
            channels: 2,
            seconds: 0.5,
        };
        let ints = signals::generate(&spec, 1);
        let sound = Sound {
            name: "t".into(),
            source: Source::Synth {
                description: spec.kind.describe(),
            },
            rate: spec.rate,
            planes: ints
                .iter()
                .map(|c| c.iter().map(|&v| f32::from(v) / 32768.0).collect())
                .collect(),
            sha256: String::new(),
        };
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("ogg")).unwrap();
        let models = BTreeMap::from([(44_100, Model::new(44_100).unwrap())]);
        let sounds = [sound];
        let row = run_job(
            &Job::Kura { sound: 0, q: 3 },
            &codecs,
            &sounds,
            &[],
            &models,
            dir.path(),
        );
        assert_eq!(row.label, "kura-q3");
        let json = row_json(&row, &sounds[0]);
        match &row.outcome {
            Outcome::Skipped(reason) => {
                assert!(!reason.is_empty());
                assert_eq!(json["status"], "skipped");
            }
            Outcome::Failed(reason) => panic!("測れません: {reason}"),
            Outcome::Ok(m) => {
                assert_eq!(m.frames_symphonia, sounds[0].frames());
                assert_eq!(m.lag, 0);
                assert!(m.nmr.total_db.is_finite() && m.kbps_audio > 0.0);
                assert!(m.stats.header_bytes > 0);
                assert_eq!(json["status"], "ok");
            }
        }
        let md = report::render(
            &Context {
                args: vec![],
                commit: "x".into(),
                encoder_id: row.codec.clone(),
                decoder_id: "d".into(),
                seed: 1,
                qs: vec![3],
                curve_ref: None,
            },
            &sounds,
            &[],
            &[row],
        );
        assert!(md.contains("表 1"));
        // 文書はリポジトリの外（作業の記録）を参照しない（定義は metrics・report の冒頭に書き写した）
        for outside in ["stage7", "調査 §", "quality.md", "qprobe"] {
            assert!(
                !md.contains(outside),
                "results.md が {outside} を参照している"
            );
        }
    }
}
