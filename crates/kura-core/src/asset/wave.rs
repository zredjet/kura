//! グリッドの実波形（kura:// の `/wave/{hash}/1`。M13 段 5。docs/design.md §4 のタイル・docs/architecture.md §4）。
//!
//! - ジャケットのない音声のタイルに出す小さな波形。解析のジョブが書いた波形の段のファイル（`kura-peaks` 版 1。
//!   `peaks/ab/<hash>.peaks`。並びは kura-engine の `analysis::audio::peaks`）の見出しと段の表、それに段 1 つだけを読み、
//!   [`BARS`] 本の棒をアルファの PNG（GrayA8。灰は 0、形はアルファ）にして返す。フロントはフォントのサムネイルと同じく
//!   CSS の `mask-image` にして、色を tokens から付ける（`src/features/library/TileWave.tsx`）。
//! - 読む段は、ビンの数が [`BARS`] 以上の段のうちいちばん粗いもの（なければいちばん細かい段）。長い音声は最も粗い段
//!   （65,536 フレーム/ビン。3 分で 121 ビン）で足りるが、短い効果音は最も粗い段ではビンが数個しかないので細かい段へ下りる
//!   （フロントの `src/lib/asset/peaks.ts` の `pickLevel` と同じ選び方）。
//! - 棒はチャンネルをまとめ、上下対称（掛かるビンの min・max の絶対値の大きいほう）。高さはいちばん大きい棒に合わせる
//!   （小さな絵で形を見せる。ただし [`QUIET_FLOOR`]（−40 dBFS）より静かなものは持ち上げない）。無音の棒も短い線を引く。
//! - 毎回作る（ファイルを増やさない）。中身はハッシュの段のファイルと形の版 [`WAVE_V`] で決まるので immutable。段の
//!   ファイルがなければ 404（解析の前・解析できない長い音声。フロントは飾りの波形のまま）、読めなければ 500。
//! - ジョブにしない（グリッドが見えている分だけ要る、その場の小さな表示。段のファイルの数 KB を読んで 1 枚書くだけ）。呼び手が
//!   `spawn_blocking` で呼ぶ。段のファイルを読むのは呼び手のスレッド、棒を数えて PNG に書くのはエンジンのプレビューの
//!   プールの中（`EngineRuntime::preview`）。予算は確保しない（大きさは音声の長さによらず [`WIDTH`] × [`HEIGHT`] の
//!   2 バイトと PNG の数倍で 1 MiB 未満。断ると飾りの波形に戻るだけなので、混んでいても作る）。

use std::fs::File;
use std::io::{ErrorKind, Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use kura_engine::analysis::audio::peaks::{FORMAT_VERSION, MAX_LEVELS};
use kura_engine::codec::Encoder;
use kura_engine::codec::png::PngCodec;
use kura_engine::value::{Image, ImageMeta, PixelFormat, Raster};
use kura_engine::{EngineError, NoMonitor, Value};

use crate::edit::{EngineRuntime, EngineServices};
use crate::library::Layout;

/// 形の版（`/wave/{hash}/{版}`）。棒の数・大きさ・描き方を変えたら上げる（フロントの `src/ipc/media.ts` の `WAVE_V` も）。
pub const WAVE_V: u32 = 1;

/// 棒の数（飾りの波形の 30 本より細かく、タイルの最小の段 110px（枠の幅 90px）でも棒と隙間が 1px 弱ずつ残る数）。
pub const BARS: usize = 48;

/// タイルの最大の一辺（CSS px）：グリッドの大きさの段（`src/stores/viewStore.ts` の `ROW_HEIGHTS`）の最大。
const TILE_MAX_CSS: u32 = 300;
/// 装置の画素の比（Retina の画面）。
const DPR: u32 = 2;
/// 波形の枠の左右の余白（CSS px。`Tile.module.css` の `.waveform` の `--space-2-5`）。
const INSET_CSS: u32 = 10;
/// 波形の枠の高さ（タイルの高さに対する百分率。`.waveform` の `height: 22%`）。
const HEIGHT_PERCENT: u32 = 22;

/// 棒の間隔（画素）。PNG の大きさは「最大のタイルの波形の枠を、DPR 2 の装置の画素で 1:1 以上に覆う」大きさにする：
/// 枠の幅は (300 − 2·10)·2 = 560 画素なので、棒の数で割って切り上げた 12 画素の間隔で 576 画素、高さは 300·0.22·2 =
/// 132 画素。これでどの段のタイルでも縮めて描くだけで、広げてぼかすことがない（ジャスティファイ配置で行が少し伸びた
/// ときだけ 1 割ほど広がる）。これより大きくしても、書く時間と WebKit が展開して持つメモリ（576 × 132 × 4 ≈ 300 KB/枚）が
/// 増えるだけ。
pub const PITCH: u32 = ((TILE_MAX_CSS - 2 * INSET_CSS) * DPR).div_ceil(BARS as u32);
/// PNG の幅（画素）。
pub const WIDTH: u32 = PITCH * BARS as u32;
/// PNG の高さ（画素）。
pub const HEIGHT: u32 = (TILE_MAX_CSS * HEIGHT_PERCENT * DPR).div_ceil(100);
/// 棒の幅（画素。間隔の半分。棒は間隔の中央に置く）。
const BAR_WIDTH: u32 = PITCH / 2;
/// 棒の最短の長さ（画素）。無音の棒も線が見えるように（インスペクタの波形の「無音の列も 1 単位の線を引く」と同じ考え）。
/// 最小の段のタイルでは 0.36 倍ほどに縮むので、装置の画素 1.5 ほどの線になる。
const MIN_BAR: f32 = 4.0;
/// 高さを合わせるときの分母の下限（i16 の振幅。32,768 × 10^(−40/20) ≈ 328、−40 dBFS）。これより静かな音声は持ち上げず、
/// 小さいまま描く（ほぼ無音のものが大きな波形に見えないように）。
pub const QUIET_FLOOR: u32 = 328;

/// 読む段の大きさの上限（バイト）。最も粗い段なら 65,536 フレーム/ビンなので、8 チャンネルで 10 時間を超えても 1 MiB に
/// 届かない。これを超えるのは壊れたファイルだけ。
const MAX_LEVEL_BYTES: u64 = 4 << 20;

/// `kura-peaks` の先頭（版 1 で凍結。kura-engine の `analysis::audio::peaks` の並び）。
const MAGIC: &[u8; 8] = b"kurapeak";
/// 見出しのバイト数。
const HEADER_BYTES: u64 = 32;
/// 段の表の 1 行のバイト数。
const LEVEL_ENTRY_BYTES: u64 = 24;
/// 値の型：i16 の min・max の組。
const VALUE_MIN_MAX_I16: u8 = 1;

/// 実波形を返せない理由。
#[derive(Debug, thiserror::Error)]
pub enum WaveError {
    /// 段のファイルがない（解析の前・解析できない長い音声）、形の版が今のものでない。
    #[error("波形の段がありません: {0}")]
    NotFound(String),
    /// エンジンを使えない。
    #[error("エンジンを使えません: {0}")]
    Unavailable(String),
    /// 段のファイルが読めない・壊れている、PNG を書けない。
    #[error("実波形を作れません: {0}")]
    Failed(String),
}

impl WaveError {
    /// HTTP の状態（404・503・500）。
    pub fn status(&self) -> u16 {
        match self {
            Self::NotFound(_) => 404,
            Self::Unavailable(_) => 503,
            Self::Failed(_) => 500,
        }
    }
}

/// 段のファイルから読んだ段 1 つ。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WaveLevel {
    /// 音声のフレーム数。
    pub frames: u64,
    /// チャンネル数（1 以上）。
    pub channels: u16,
    /// 1 ビンのフレーム数（1 以上）。
    pub bin: u32,
    /// ビンの順 → チャンネルの順 → [min, max]。長さは ⌈frames / bin⌉ × チャンネル数 × 2。
    pub data: Vec<i16>,
}

impl WaveLevel {
    /// ビンの数（データの長さから。チャンネル数が 0 なら 0）。
    fn bins(&self) -> u64 {
        match u64::from(self.channels) * 2 {
            0 => 0,
            stride => self.data.len() as u64 / stride,
        }
    }
}

/// `/wave/{hash}/{version}` の PNG を作る（モジュールの説明）。形の版が [`WAVE_V`] でなければ 404。
pub fn serve(
    engine: &EngineServices,
    layout: &Layout,
    hash: &str,
    version: u32,
) -> Result<Vec<u8>, WaveError> {
    if version != WAVE_V {
        return Err(WaveError::NotFound(format!("形の版 {version}")));
    }
    let started = Instant::now();
    let level = read_level(&layout.peaks_path(hash), BARS)?;
    let runtime = engine
        .runtime()
        .map_err(|err| WaveError::Unavailable(err.to_string()))?;
    let png = draw(runtime, &level)?;
    tracing::debug!(
        hash,
        bin = level.bin,
        bytes = png.len(),
        ms = started.elapsed().as_secs_f64() * 1000.0,
        "グリッドの実波形を作りました"
    );
    Ok(png)
}

/// 段 1 つから棒を数え、PNG に書く（プレビューのプールの中で）。プールの中から呼ばない。
pub fn draw(runtime: &EngineRuntime, level: &WaveLevel) -> Result<Vec<u8>, WaveError> {
    runtime
        .preview(|_| encode_png(&render(&bar_heights(level, BARS))))
        .map_err(|err| WaveError::Failed(err.to_string()))
}

/// GrayA8 の行（[`WIDTH`] × [`HEIGHT`] × 2）を PNG にする。
fn encode_png(rows: &[u8]) -> Result<Vec<u8>, EngineError> {
    let px = Raster::from_rows(WIDTH, HEIGHT, PixelFormat::GrayA8, None, rows)?;
    let value = Value::Image(Arc::new(Image {
        px,
        meta: ImageMeta::default(),
    }));
    let mut out = Vec::new();
    PngCodec.encode(
        &value,
        "png",
        &serde_json::json!({ "compression": "fast" }),
        &mut out,
        &NoMonitor,
    )?;
    Ok(out)
}

fn invalid(message: impl Into<String>) -> WaveError {
    WaveError::Failed(format!("波形の段が正しくありません：{}", message.into()))
}

fn u16_at(bytes: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([bytes[at], bytes[at + 1]])
}

fn u32_at(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
}

fn u64_at(bytes: &[u8], at: usize) -> u64 {
    let mut b = [0u8; 8];
    b.copy_from_slice(&bytes[at..at + 8]);
    u64::from_le_bytes(b)
}

/// 段のファイルの見出しと段の表を確かめ、ビンの数が `bars` 以上の段のうちいちばん粗いもの（なければいちばん細かい段）の
/// データだけを読む。ファイルがなければ [`WaveError::NotFound`]、並び（magic・並びの版・値の型・段の数・ビンの大きさが
/// 増えていくこと・ビンの数・データの位置・全体の長さ）が合わなければ [`WaveError::Failed`]。解析の版（見出しの 10 バイト目）
/// は見ない（並びの版が同じなら値の意味も同じ）。
pub fn read_level(path: &Path, bars: usize) -> Result<WaveLevel, WaveError> {
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(err) if err.kind() == ErrorKind::NotFound => {
            return Err(WaveError::NotFound(path.display().to_string()));
        }
        Err(err) => return Err(WaveError::Failed(err.to_string())),
    };
    let io = |err: std::io::Error| match err.kind() {
        ErrorKind::UnexpectedEof => invalid("短すぎます"),
        _ => WaveError::Failed(err.to_string()),
    };
    let len = file.metadata().map_err(io)?.len();
    let mut head = [0u8; HEADER_BYTES as usize];
    file.read_exact(&mut head).map_err(io)?;
    if &head[..8] != MAGIC {
        return Err(invalid("kura-peaks ではありません"));
    }
    if u16_at(&head, 8) != FORMAT_VERSION {
        return Err(invalid("知らない並びの版です"));
    }
    let channels = u16_at(&head, 12);
    let levels = usize::from(head[14]);
    let frames = u64_at(&head, 24);
    if channels == 0 || levels == 0 || levels > MAX_LEVELS {
        return Err(invalid("チャンネル数・段の数"));
    }
    if head[15] != VALUE_MIN_MAX_I16 || u32_at(&head, 20) != 0 {
        return Err(invalid("値の型・予約の欄"));
    }
    let mut table = vec![0u8; levels * LEVEL_ENTRY_BYTES as usize];
    file.read_exact(&mut table).map_err(io)?;
    // 段ごとの (ビンのフレーム数, データの位置, データのバイト数)。位置は先頭から 8 の倍数で、前の段の終わりの直後
    let mut entries: Vec<(u32, u64, u64)> = Vec::with_capacity(levels);
    let mut end = HEADER_BYTES + LEVEL_ENTRY_BYTES * levels as u64;
    for (i, row) in table
        .as_chunks::<{ LEVEL_ENTRY_BYTES as usize }>()
        .0
        .iter()
        .enumerate()
    {
        let (bin, zero, count, at) = (
            u32_at(row, 0),
            u32_at(row, 4),
            u64_at(row, 8),
            u64_at(row, 16),
        );
        if bin == 0 || zero != 0 || entries.last().is_some_and(|(last, _, _)| *last >= bin) {
            return Err(invalid("段の表"));
        }
        let expected_at = if i == 0 {
            end
        } else {
            end.checked_add(7)
                .map(|v| v & !7)
                .ok_or_else(|| invalid("位置"))?
        };
        let bytes = count
            .checked_mul(u64::from(channels))
            .and_then(|v| v.checked_mul(4))
            .ok_or_else(|| invalid("大きさがあふれます"))?;
        if count != frames.div_ceil(u64::from(bin)) || at != expected_at {
            return Err(invalid("ビンの数・データの位置"));
        }
        end = at
            .checked_add(bytes)
            .ok_or_else(|| invalid("大きさがあふれます"))?;
        entries.push((bin, at, bytes));
    }
    if end != len {
        return Err(invalid("長さが合いません"));
    }
    let bins_of = |bin: u32| frames.div_ceil(u64::from(bin));
    let (bin, at, bytes) = entries
        .iter()
        .rev()
        .find(|(bin, _, _)| bins_of(*bin) >= bars as u64)
        .or(entries.first())
        .copied()
        .ok_or_else(|| invalid("段がありません"))?;
    if bytes > MAX_LEVEL_BYTES {
        return Err(invalid(format!("段が大きすぎます（{bytes} バイト）")));
    }
    let mut raw = vec![0u8; bytes as usize];
    file.seek(SeekFrom::Start(at)).map_err(io)?;
    file.read_exact(&mut raw).map_err(io)?;
    let data = raw
        .as_chunks::<2>()
        .0
        .iter()
        .map(|b| i16::from_le_bytes(*b))
        .collect();
    Ok(WaveLevel {
        frames,
        channels,
        bin,
        data,
    })
}

/// 棒ごとの高さ（0〜1）。棒 c はフレーム [⌊c·N/n⌋, ⌊(c+1)·N/n⌋) を受け持ち、掛かるビンのすべてのチャンネルの min・max の
/// 絶対値の最大を振幅にする（ビンが棒より粗い短い音声は、棒の始まりのビンを使う。`peaks.ts` の `columns` と同じ）。
/// 振幅はいちばん大きい棒で割る（分母は [`QUIET_FLOOR`] 以上）。長さ 0 の音声はすべて 0。
pub fn bar_heights(level: &WaveLevel, bars: usize) -> Vec<f32> {
    let mut amps = vec![0u32; bars];
    let bins = level.bins();
    let n = bars as u64;
    if level.frames > 0 && bins > 0 && level.bin > 0 {
        let span = level.frames;
        let bin = u64::from(level.bin);
        let channels = usize::from(level.channels);
        for (c, amp) in amps.iter_mut().enumerate() {
            let c = c as u64;
            // c·span は u64 に収まる（span は 2^64 / 48 未満のフレーム。収まらなければ棒は 0 のまま）
            let (Some(a), Some(b)) = (c.checked_mul(span), (c + 1).checked_mul(span)) else {
                continue;
            };
            let (a, b) = (a / n, b / n);
            let b0 = a / bin;
            let b1 = bins.min((b0 + 1).max(b.div_ceil(bin)));
            for k in b0..b1 {
                let Ok(k) = usize::try_from(k) else { break };
                let at = k * channels * 2;
                for pair in level.data[at..at + channels * 2].as_chunks::<2>().0 {
                    let v = i32::from(pair[0])
                        .unsigned_abs()
                        .max(i32::from(pair[1]).unsigned_abs());
                    *amp = (*amp).max(v);
                }
            }
        }
    }
    let peak = amps.iter().copied().max().unwrap_or(0).max(QUIET_FLOOR);
    amps.iter()
        .map(|a| (*a as f32 / peak as f32).min(1.0))
        .collect()
}

/// 棒の高さ（[`BARS`] 個、0〜1）から GrayA8 の行（[`WIDTH`] × [`HEIGHT`] × 2。灰は 0、形はアルファ）を作る。棒 c は
/// 横 [c·PITCH + PITCH/4, +BAR_WIDTH)、縦は中央から上下に 高さ·HEIGHT/2（全体で [`MIN_BAR`] 画素以上）。縦の端の画素は
/// 覆う割合のアルファにする（縮めて描いても段差が出ない）。棒の数が [`BARS`] でなければ、足りない棒は 0、多い棒は捨てる。
pub fn render(heights: &[f32]) -> Vec<u8> {
    let (w, h) = (WIDTH as usize, HEIGHT as usize);
    let mut rows = vec![0u8; w * h * 2];
    let mid = HEIGHT as f32 / 2.0;
    let left = (PITCH - BAR_WIDTH) / 2;
    for c in 0..BARS {
        let height = heights.get(c).copied().unwrap_or(0.0);
        let half = (height.clamp(0.0, 1.0) * mid).max(MIN_BAR / 2.0);
        let (top, bottom) = (mid - half, mid + half);
        let x0 = c * PITCH as usize + left as usize;
        for y in 0..h {
            let cover = (bottom.min(y as f32 + 1.0) - top.max(y as f32)).clamp(0.0, 1.0);
            if cover <= 0.0 {
                continue;
            }
            let alpha = (cover * 255.0).round() as u8;
            let row = &mut rows[y * w * 2..(y + 1) * w * 2];
            for x in x0..x0 + BAR_WIDTH as usize {
                row[x * 2 + 1] = alpha;
            }
        }
    }
    rows
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use kura_engine::Limits;
    use kura_engine::analysis::audio::peaks::{PeakLevel, Peaks};
    use kura_engine::analysis::output::AnalysisOutput;
    use kura_engine::codec::CodecSet;
    use kura_engine::doc::DecodeOptions;

    use super::*;
    use crate::edit::{EngineFactory, RuntimeConfig};

    const HASH: &str = "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789";

    /// `frames` フレーム・`channels` チャンネルの段（ビン k・チャンネル ch は ±(k·37 + ch·11) mod 32,768 の振幅。最も
    /// 細かい段から上の段を 4 ビンずつまとめる。エンジンの決まりと同じ）。
    fn peaks(frames: u64, channels: u16, bins: &[u32]) -> Peaks {
        let finest = bins[0];
        let n = frames.div_ceil(u64::from(finest));
        let mut data = Vec::new();
        for k in 0..n {
            for ch in 0..u64::from(channels) {
                let a = ((k * 37 + ch * 11) % 32_768) as i16;
                data.extend([-a, a]);
            }
        }
        let mut levels = vec![PeakLevel { bin: finest, data }];
        for b in &bins[1..] {
            let prev = levels.last().unwrap();
            let ratio = (*b / prev.bin) as usize;
            let stride = usize::from(channels) * 2;
            let mut up = Vec::new();
            for group in prev.data.chunks(stride * ratio) {
                for ch in 0..usize::from(channels) {
                    let lo = group.chunks(stride).map(|s| s[ch * 2]).min().unwrap();
                    let hi = group.chunks(stride).map(|s| s[ch * 2 + 1]).max().unwrap();
                    up.extend([lo, hi]);
                }
            }
            levels.push(PeakLevel { bin: *b, data: up });
        }
        Peaks {
            rate: 44_100,
            channels,
            frames,
            levels,
        }
    }

    fn write_peaks(dir: &Path, p: &Peaks) -> PathBuf {
        let path = dir.join("x.peaks");
        std::fs::write(&path, p.to_canonical().unwrap()).unwrap();
        path
    }

    fn level(frames: u64, channels: u16, bin: u32, amps: &[(i16, i16)]) -> WaveLevel {
        WaveLevel {
            frames,
            channels,
            bin,
            data: amps.iter().flat_map(|(lo, hi)| [*lo, *hi]).collect(),
        }
    }

    /// 読む段：ビンの数が棒の数以上の段のうちいちばん粗いもの（3 分なら最も粗い段）。短い音声は細かい段へ下り、どの段も
    /// 足りなければいちばん細かい段。読んだ値はエンジンの正規の形の段と同じ。
    #[test]
    fn reads_the_coarsest_level_with_enough_bins() {
        let dir = tempfile::tempdir().unwrap();
        let all = [256, 1_024, 4_096, 16_384, 65_536];
        for (frames, bins, want) in [
            (180 * 44_100, &all[2..], 65_536), // 3 分（解析の段は 4,096〜65,536）：121 ビン
            (5 * 44_100, &all[..], 4_096), // 5 秒：65,536 は 4 ビン、16,384 は 14 ビン、4,096 は 54 ビン
            (2_000, &all[..], 256),        // どの段も 48 ビンに届かない：最も細かい段
            (48 * 65_536, &all[4..], 65_536), // ちょうど 48 ビン
        ] {
            let p = peaks(frames, 2, bins);
            let path = write_peaks(dir.path(), &p);
            let got = read_level(&path, BARS).unwrap();
            let idx = p.levels.iter().position(|l| l.bin == want).unwrap();
            assert_eq!(got.bin, want, "{frames}");
            assert_eq!(got.frames, frames);
            assert_eq!(got.channels, 2);
            assert_eq!(got.data, p.levels[idx].data, "{frames}");
        }
    }

    /// ないファイルは NotFound（404）、壊れた並びは Failed（500）。どの 1 バイトを変えても、受け付けるか Failed で、panic
    /// しない。
    #[test]
    fn missing_and_broken_files() {
        let dir = tempfile::tempdir().unwrap();
        let missing = read_level(&dir.path().join("none.peaks"), BARS).unwrap_err();
        assert!(matches!(missing, WaveError::NotFound(_)), "{missing}");
        assert_eq!(missing.status(), 404);
        let good = peaks(5 * 44_100, 1, &[256, 1_024, 4_096, 16_384, 65_536])
            .to_canonical()
            .unwrap();
        let path = dir.path().join("x.peaks");
        let mut broken = vec![
            (b"notpeaks".to_vec(), "magic"),
            (good[..20].to_vec(), "短い"),
            (good[..good.len() - 1].to_vec(), "長さ"),
            ([good.as_slice(), &[0]].concat(), "長い"),
        ];
        for (at, v, what) in [
            (8, 2u8, "並びの版"),
            (12, 0, "チャンネル 0"),
            (14, 0, "段 0"),
            (15, 2, "値の型"),
        ] {
            let mut b = good.clone();
            b[at] = v;
            if what == "チャンネル 0" {
                b[13] = 0;
            }
            broken.push((b, what));
        }
        for (bytes, what) in broken {
            std::fs::write(&path, &bytes).unwrap();
            let err = read_level(&path, BARS).unwrap_err();
            assert!(matches!(err, WaveError::Failed(_)), "{what}: {err}");
            assert_eq!(err.status(), 500, "{what}");
        }
        for i in 0..good.len().min(256) {
            for flip in [0x01u8, 0x80, 0xff] {
                let mut b = good.clone();
                b[i] ^= flip;
                std::fs::write(&path, &b).unwrap();
                if let Err(err) = read_level(&path, BARS) {
                    assert!(matches!(err, WaveError::Failed(_)), "{i}: {err}");
                }
            }
        }
    }

    /// 棒の高さ：棒の受け持つビンの min・max の絶対値の最大（チャンネルをまとめる）を、いちばん大きい棒で割る。
    #[test]
    fn bar_heights_take_the_largest_magnitude_and_normalize() {
        // 4 ビン・2 チャンネル・ビン 10 フレーム（40 フレーム）を 4 本に：1 本 1 ビン
        let l = level(
            40,
            2,
            10,
            &[
                (-100, 50),
                (0, 0), // ビン 0：チャンネル 0 の min の絶対値 100
                (-10, 400),
                (-20, 20), // ビン 1：400
                (-32_768, 0),
                (0, 0), // ビン 2：i16::MIN は 32,768
                (0, 0),
                (0, 0), // ビン 3：無音
            ],
        );
        let h = bar_heights(&l, 4);
        assert_eq!(h, vec![100.0 / 32_768.0, 400.0 / 32_768.0, 1.0, 0.0]);
        // 2 本に：1 本が 2 ビンを受け持つ
        assert_eq!(bar_heights(&l, 2), vec![400.0 / 32_768.0, 1.0]);
        // ビンが棒より粗い（2 ビンを 4 本に）：棒の始まりのビン
        let coarse = level(20, 1, 10, &[(-1_000, 1_000), (-2_000, 500)]);
        assert_eq!(bar_heights(&coarse, 4), vec![0.5, 0.5, 1.0, 1.0]);
    }

    /// 静かな音声は −40 dBFS より持ち上げない。長さ 0・無音はすべて 0。半端なビン・半端な棒も範囲の外を読まない。
    #[test]
    fn quiet_empty_and_ragged_levels() {
        let quiet = level(20, 1, 10, &[(-82, 82), (-164, 0)]);
        assert_eq!(
            bar_heights(&quiet, 2),
            vec![82.0 / QUIET_FLOOR as f32, 164.0 / QUIET_FLOOR as f32]
        );
        assert_eq!(bar_heights(&level(0, 1, 10, &[]), 3), vec![0.0; 3]);
        assert_eq!(
            bar_heights(&level(20, 1, 10, &[(0, 0), (0, 0)]), 3),
            vec![0.0; 3]
        );
        // 25 フレーム・ビン 10（3 ビン、最後は半端）を 48 本に
        let ragged = level(
            25,
            2,
            10,
            &[(-1, 1), (-1, 1), (-2, 2), (-2, 2), (-3, 3), (-3, 3)],
        );
        let h = bar_heights(&ragged, BARS);
        assert_eq!(h.len(), BARS);
        assert!(h.iter().all(|v| (0.0..=1.0).contains(v)));
        // フレームが棒より少ない（5 フレームを 48 本に）
        let tiny = level(5, 1, 256, &[(-300, 600)]);
        assert!(
            bar_heights(&tiny, BARS)
                .iter()
                .all(|v| *v == 600.0 / QUIET_FLOOR as f32 || *v == 1.0)
        );
    }

    /// 画素の行：棒は間隔の中央・上下対称・縦の端は覆う割合。無音でも短い線。棒の外と灰はすべて 0。
    #[test]
    fn rendered_rows_are_symmetric_bars() {
        assert_eq!((WIDTH, HEIGHT, PITCH, BAR_WIDTH), (576, 132, 12, 6));
        let mut heights = vec![0.0f32; BARS];
        heights[0] = 1.0;
        heights[1] = 0.5;
        heights[47] = 0.25;
        let rows = render(&heights);
        assert_eq!(rows.len(), (WIDTH * HEIGHT * 2) as usize);
        let alpha = |x: u32, y: u32| rows[((y * WIDTH + x) * 2 + 1) as usize];
        assert!(rows.iter().step_by(2).all(|g| *g == 0), "灰は 0");
        // 棒 0 は x 3〜8、全体の高さ
        for y in [0, 65, 66, 131] {
            assert_eq!(alpha(2, y), 0);
            assert_eq!(alpha(3, y), 255);
            assert_eq!(alpha(8, y), 255);
            assert_eq!(alpha(9, y), 0);
        }
        // 棒 1 は中央 66 から ±33（y 33〜98）
        let x = PITCH + 3;
        assert_eq!(alpha(x, 32), 0);
        assert_eq!(alpha(x, 33), 255);
        assert_eq!(alpha(x, 98), 255);
        assert_eq!(alpha(x, 99), 0);
        // 棒 47 は ±16.5：端の画素は半分
        let x = 47 * PITCH + 3;
        assert_eq!(alpha(x, 48), 0);
        assert_eq!(alpha(x, 49), 128);
        assert_eq!(alpha(x, 50), 255);
        assert_eq!(alpha(x, 82), 128);
        assert_eq!(alpha(x, 83), 0);
        // 上下対称
        for c in 0..BARS as u32 {
            for y in 0..HEIGHT / 2 {
                let x = c * PITCH + 3;
                assert_eq!(alpha(x, y), alpha(x, HEIGHT - 1 - y), "{c} {y}");
            }
        }
        // 無音の棒（2〜46）は中央の 4 画素
        let x = 2 * PITCH + 3;
        let lit: Vec<u32> = (0..HEIGHT).filter(|y| alpha(x, *y) > 0).collect();
        assert_eq!(lit, vec![64, 65, 66, 67]);
    }

    struct Factory;

    impl EngineFactory for Factory {
        fn runtime_config(&self) -> RuntimeConfig {
            RuntimeConfig {
                cores: Some(2),
                budget: Some(64 << 20),
                ..RuntimeConfig::default()
            }
        }

        fn codecs(&self, _work_dir: &Path) -> CodecSet {
            CodecSet::pinned()
        }
    }

    /// serve：段のファイルから PNG（576 × 132、GrayA8、アルファが棒）。ないファイル・違う形の版は 404。読み直した画素は
    /// render の行と同じ。
    #[test]
    fn serves_a_png_of_the_bars() {
        let dir = tempfile::tempdir().unwrap();
        let layout = Layout::new(dir.path());
        let engine = EngineServices::new(Arc::new(Factory), layout.staging_dir());
        let missing = serve(&engine, &layout, HASH, WAVE_V).unwrap_err();
        assert_eq!(missing.status(), 404, "{missing}");
        assert!(
            engine.runtime_if_built().is_none(),
            "ないものは実行の場を作らない"
        );

        let p = peaks(180 * 44_100, 2, &[4_096, 16_384, 65_536]);
        let path = layout.peaks_path(HASH);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, p.to_canonical().unwrap()).unwrap();
        assert_eq!(serve(&engine, &layout, HASH, 2).unwrap_err().status(), 404);

        let started = Instant::now();
        let png = serve(&engine, &layout, HASH, WAVE_V).unwrap();
        let ms = started.elapsed().as_secs_f64() * 1000.0;
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
        assert!(png.len() < 8 * 1024, "{} バイト", png.len());
        let file = dir.path().join("wave.png");
        std::fs::write(&file, &png).unwrap();
        let (value, _) = CodecSet::pinned()
            .decode_path(
                &file,
                "png",
                &DecodeOptions::default(),
                &Limits::default(),
                &NoMonitor,
            )
            .unwrap();
        let Value::Image(img) = value else {
            panic!("画像ではありません");
        };
        assert_eq!((img.px.width(), img.px.height()), (WIDTH, HEIGHT));
        assert_eq!(img.px.format(), PixelFormat::GrayA8);
        let level = read_level(&path, BARS).unwrap();
        assert_eq!(
            img.px.to_rows().unwrap(),
            render(&bar_heights(&level, BARS))
        );
        // 2 回目から（実行の場はある）：段のファイルを読んで PNG を書くだけ
        let rounds = 20;
        let started = Instant::now();
        for _ in 0..rounds {
            assert_eq!(serve(&engine, &layout, HASH, WAVE_V).unwrap(), png);
        }
        let each = started.elapsed().as_secs_f64() * 1000.0 / f64::from(rounds);
        eprintln!(
            "実波形：{} バイト、最初 {ms:.2} ms（実行の場を作る分を含む）、2 回目から 1 枚 {each:.3} ms",
            png.len()
        );
    }
}
