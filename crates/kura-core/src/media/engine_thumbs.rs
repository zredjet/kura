//! エンジンの道のサムネイル（M11 の決定 2・3・4・9、plan の手順 17・defaults 6・21）。
//!
//! - 画像（png・apng・gif・webp・aseprite・psd・psb・svg）：kura-engine のコーデックの組で読み、先頭のフレームをキャンバスに置き、
//!   Box で長辺 1600・512 に縮める（拡大しない。ドット絵の拡大は表示の側の pixelated と整数倍）。長辺 1600 に透明な画素
//!   （α < 255）があれば PNG（fast）、なければ JPEG（品質 82。Rgb8 に落として書く）。
//! - 3D（glb）：kura-engine の model（自前の読み手とソフトウェアの描画。SceneKit を使わない。決定 9）で長辺 1600・512 に描く。
//! - メモリ（決定 2）：ジョブのスレッド（プールの外）で 1 件ずつ形を読んで見積もり、同時の確保が [`INFLIGHT_CAP`]（256 MiB）に
//!   収まる小分けにして、一括の予算（`Class::Bulk`）から確保してから、実行の場の一括のプール（`kura-edit-bulk-*`）の中で
//!   アイテムを並べて描く。サムネイルのプール（`kura-thumb-*`）の中ではエンジンを呼ばない（.claude/rules/engine.md）。
//! - 値が [`VALUE_CAP`]（1 GiB）を超えるもの・`Limit`・`Unsupported`・読めないもの・予算に収まらないものは、画像なら
//!   [`EngineDone::Fallback`] で ImageIO の道へ回す（ImageIO は縮めて読める）。3D は回す先がないので失敗（種類の絵）。
//!   ただし別のプロセスで読む SVG が時間の上限を超えたもの（`svg_worker::is_time_limit`）は回さずに失敗（種類の絵）にする
//!   （ImageIO の道は SVG のサムネイルを作れず、時間を使う SVG をプロセスの中でもう一度開くだけ。決定 23）。SVG の形を読む
//!   子は、見積もりの段でも中止で止める（`CodecSet::shape_path_monitored`）。
//! - SVG の働き手を使えない（起動できない・別のビルド。`svg_worker::is_unavailable`。M11 の 3 回目の確認 W2）ものは、アイテムの
//!   せいではないので [`EngineDone::Unavailable`] にする（何も書かない。pending・stale のまま）。働き手は呼ぶたびに同じく
//!   使えないので、まだ見積もっていない残りは試さずに同じ結果にする（呼び手のジョブはその場で止める）。

use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rayon::prelude::*;

use kura_engine::budget::Class;
use kura_engine::codec::CodecSet;
use kura_engine::doc::DecodeOptions;
use kura_engine::imaging::convert::for_each_rgba8_key;
use kura_engine::imaging::pixels::is_gray;
use kura_engine::imaging::resample::{Filter, resize_smooth, smooth_format, smooth_work};
use kura_engine::model::{self, ModelLimits, RenderOptions, ViewDir};
use kura_engine::value::{Image, ImageMeta, PixelFormat, Raster};
use kura_engine::{EngineError, Limits, Monitor, Shape, Value};

use super::{ModelInfo, ThumbFormat, ThumbTargets};
use crate::codec_ext::svg_worker;
use crate::edit::{EngineRuntime, EngineServices};
use crate::library::ThumbSize;

/// 同時に確保するメモリの上限（M11 の決定 2：サムネイルの同時の確保は 256 MiB まで）。
pub const INFLIGHT_CAP: u64 = 256 << 20;
/// エンジンで読む値の上限（plan defaults 21。超えたら ImageIO の道へ回す）。
pub const VALUE_CAP: u64 = 1 << 30;
/// 透明のない画像の JPEG の品質（ImageIO の道の 0.82 と同じ）。
pub const JPEG_QUALITY: u8 = 82;
/// kura-engine の model の描画の帯の行の数（`model::raster` の `BAND_ROWS`。見積もりに使う）。
const MODEL_BAND_ROWS: u64 = 16;
/// glb の見出し（12 バイト）と最初の塊の見出し（8 バイト）。
const GLB_HEAD: usize = 20;
/// glb の JSON の塊の種類（"JSON"）。
const CHUNK_JSON: u32 = 0x4E4F_534A;

/// エンジンで作る 1 件。
#[derive(Debug, Clone)]
pub struct EngineJob {
    pub path: PathBuf,
    pub what: EngineWhat,
    pub targets: ThumbTargets,
}

/// 何を描くか。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EngineWhat {
    /// 画像（形式の表の ID）。
    Image { format: &'static str },
    /// glb。
    Model,
}

/// 1 件の結果。
#[derive(Debug, Clone, PartialEq)]
pub enum EngineDone {
    /// 書いた。`size` は読めた画像（キャンバス）の幅・高さ（3D は None）。
    Written {
        format: ThumbFormat,
        size: Option<(u32, u32)>,
        model: Option<Box<ModelInfo>>,
    },
    /// エンジンでは作らない・作れない（画像だけ）。次の道（ImageIO）へ回す。理由はログに出す。
    Fallback(String),
    /// 作れない（3D の描けないもの・書けないなど）。読めた 3D の情報は返す。
    Failed {
        error: String,
        model: Option<Box<ModelInfo>>,
    },
    /// 中止・一時停止（何も書かない。pending・stale のまま）。
    Canceled,
    /// SVG の働き手を使えない（一時的な誤り。理由）。何も書かない（pending・stale のまま）。呼び手はジョブを止める。
    Unavailable(String),
}

/// 見積もり：描く全体で持つ分と、プールの仕事ごとの分。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Estimate {
    pub fixed: u64,
    pub per_task: u64,
}

/// 見積もった 1 件。
struct Planned {
    index: usize,
    est: Estimate,
    model: Option<model::ModelInfo>,
}

/// エンジンの道（実行の場・コーデックの組・上限）。ジョブが最初にエンジンの道のアイテムに出会ったときに作る。
pub struct EngineThumbs {
    runtime: Arc<EngineRuntime>,
    codecs: Arc<CodecSet>,
    limits: Limits,
    model_limits: ModelLimits,
    cap: u64,
}

impl EngineThumbs {
    /// ライブラリのエンジンの部品から作る（実行の場・コーデックの組をまだ作っていなければ、ここで作る）。
    pub fn new(services: &EngineServices) -> crate::Result<Self> {
        Ok(Self {
            runtime: services.runtime()?.clone(),
            codecs: services.codecs(),
            limits: Limits::default(),
            model_limits: ModelLimits::default(),
            cap: INFLIGHT_CAP,
        })
    }

    /// 同時の確保の上限を変える（テスト用）。
    pub fn with_cap(mut self, cap: u64) -> Self {
        self.cap = cap.max(1);
        self
    }

    /// `jobs` を描く（結果は同じ順）。ジョブのスレッド（プールの外）から呼ぶ。見積もりで SVG の働き手を使えないと分かったら、
    /// まだ見積もっていない残りは試さずに [`EngineDone::Unavailable`] にする（見積もり終えたものは描く）。
    pub fn render(&self, jobs: &[EngineJob], mon: &dyn Monitor) -> Vec<EngineDone> {
        let mut done: Vec<Option<EngineDone>> = vec![None; jobs.len()];
        let mut planned = Vec::new();
        let mut unavailable: Option<String> = None;
        for (index, job) in jobs.iter().enumerate() {
            if mon.canceled() {
                done[index] = Some(EngineDone::Canceled);
                continue;
            }
            if let Some(why) = &unavailable {
                done[index] = Some(EngineDone::Unavailable(why.clone()));
                continue;
            }
            match self.plan(job, mon) {
                Ok((est, model)) => planned.push(Planned { index, est, model }),
                Err(result) => {
                    if let EngineDone::Unavailable(why) = &result {
                        unavailable = Some(why.clone());
                    }
                    done[index] = Some(result);
                }
            }
        }
        let threads = self.runtime.bulk_threads() as u64;
        // 小分けは予算の全体にも収める（全体を超えるのは 1 件だけの小分けになり、それだけが確保で断られる）
        let cap = self.cap.min(self.runtime.budget().total());
        for group in groups(planned, cap, threads) {
            let cost = group_cost(&group, threads);
            let lease = if mon.canceled() {
                Err(EngineError::Canceled)
            } else {
                self.runtime.budget().acquire(cost, Class::Bulk, mon)
            };
            match lease {
                Ok(lease) => {
                    let drawn: Vec<(usize, EngineDone)> = self.runtime.bulk(|_| {
                        group
                            .par_iter()
                            .map(|p| (p.index, self.draw(&jobs[p.index], p, mon)))
                            .collect()
                    });
                    drop(lease);
                    for (index, result) in drawn {
                        done[index] = Some(result);
                    }
                }
                Err(err) => {
                    for p in &group {
                        done[p.index] = Some(not_drawn(&jobs[p.index], &err, p.model.as_ref()));
                    }
                }
            }
        }
        done.into_iter()
            .map(|d| {
                d.unwrap_or_else(|| EngineDone::Failed {
                    error: "サムネイルを描けませんでした（内部の誤り）".into(),
                    model: None,
                })
            })
            .collect()
    }

    /// 形を読んで見積もる（ジョブのスレッドで）。描かないと決まったものは結果を返す。形を読むのに時間のかかりうるデコーダ
    /// （別のプロセスで読む SVG）は、中止で止める。
    fn plan(
        &self,
        job: &EngineJob,
        mon: &dyn Monitor,
    ) -> Result<(Estimate, Option<model::ModelInfo>), EngineDone> {
        match job.what {
            EngineWhat::Image { format } => {
                let shape = || -> Result<(Shape, u64), EngineError> {
                    let len = std::fs::metadata(&job.path)?.len();
                    let (shape, _) = self.codecs.shape_path_monitored(
                        &job.path,
                        format,
                        &DecodeOptions::default(),
                        &self.limits,
                        mon,
                    )?;
                    Ok((shape, len))
                };
                let (shape, len) = shape().map_err(|e| not_drawn(job, &e, None))?;
                let value = shape.value_bytes();
                if value > VALUE_CAP {
                    return Err(EngineDone::Fallback(format!(
                        "エンジンで読むには大きすぎます（値 {value} バイト）"
                    )));
                }
                Ok((image_estimate(&self.codecs, format, &shape, len), None))
            }
            EngineWhat::Model => {
                let (info, len) = glb_info(&job.path, &self.model_limits)
                    .map_err(|e| not_drawn(job, &e, None))?;
                if !info.unsupported.is_empty() {
                    return Err(EngineDone::Failed {
                        error: format!("描けない 3D モデルです（{}）", info.unsupported.join("・")),
                        model: Some(Box::new(ModelInfo::from(&info))),
                    });
                }
                let side = u64::from(ThumbSize::Large.max_px());
                let png_work = self.codecs.encode_work_bytes(
                    "png",
                    &Shape::Image {
                        w: kura_engine::Dim::Exact(side),
                        h: kura_engine::Dim::Exact(side),
                        fmt: PixelFormat::Rgba8,
                    },
                    &png_opts(),
                );
                Ok((
                    model_estimate(&info, &self.model_limits, len, png_work),
                    Some(info),
                ))
            }
        }
    }

    /// 1 件を描いて書く（一括のプールの中で）。
    fn draw(&self, job: &EngineJob, planned: &Planned, mon: &dyn Monitor) -> EngineDone {
        let result = match job.what {
            EngineWhat::Image { format } => self.draw_image(job, format, mon),
            EngineWhat::Model => self.draw_model(job, mon),
        };
        let info = planned.model.as_ref();
        match result {
            Ok((format, size)) => EngineDone::Written {
                format,
                size,
                model: info.map(boxed),
            },
            Err(err) => not_drawn(job, &err, info),
        }
    }

    fn draw_image(
        &self,
        job: &EngineJob,
        format: &str,
        mon: &dyn Monitor,
    ) -> Result<(ThumbFormat, Option<(u32, u32)>), EngineError> {
        if mon.canceled() {
            return Err(EngineError::Canceled);
        }
        let (value, _) = self.codecs.decode_path(
            &job.path,
            format,
            &DecodeOptions::default(),
            &self.limits,
            mon,
        )?;
        let icc = color_icc(&value);
        let canvas = first_frame(&value)?;
        drop(value);
        let size = (canvas.width(), canvas.height());
        let large = shrink(&canvas, ThumbSize::Large.max_px(), mon)?;
        drop(canvas);
        let small = shrink(&large, ThumbSize::Small.max_px(), mon)?;
        let written = write_pair(&self.codecs, &large, &small, icc, &job.targets, mon)?;
        Ok((written, Some(size)))
    }

    fn draw_model(
        &self,
        job: &EngineJob,
        mon: &dyn Monitor,
    ) -> Result<(ThumbFormat, Option<(u32, u32)>), EngineError> {
        if mon.canceled() {
            return Err(EngineError::Canceled);
        }
        let bytes = std::fs::read(&job.path)?;
        let scene = model::load_scene(&bytes, &self.model_limits, mon)?;
        drop(bytes);
        let draw = |size: u32| -> Result<Raster, EngineError> {
            let opts = RenderOptions {
                size,
                samples: if size <= ThumbSize::Small.max_px() {
                    3
                } else {
                    2
                },
                view: ViewDir::ThreeQuarter,
            };
            match model::render(&scene, &opts, &self.model_limits, mon)? {
                Value::Image(img) => Ok(img.px.clone()),
                _ => Err(EngineError::Internal("3D の描画が画像でありません".into())),
            }
        };
        let large = draw(ThumbSize::Large.max_px())?;
        let small = draw(ThumbSize::Small.max_px())?;
        let written = write_pair(&self.codecs, &large, &small, None, &job.targets, mon)?;
        Ok((written, None))
    }
}

/// 3D の情報を結果に入れる形にする。
fn boxed(info: &model::ModelInfo) -> Box<ModelInfo> {
    Box::new(ModelInfo::from(info))
}

/// 描かなかった・描けなかったときの結果：中止はそのまま、SVG の働き手を使えないのは一時的な誤り、画像は ImageIO へ回し、
/// 3D は失敗。
fn not_drawn(job: &EngineJob, err: &EngineError, info: Option<&model::ModelInfo>) -> EngineDone {
    match (err, job.what) {
        (EngineError::Canceled, _) => EngineDone::Canceled,
        // 起動できない・別のビルド（M11 の 3 回目の確認 W2）：アイテムのせいではないので失敗にも ImageIO にもしない
        (e, _) if svg_worker::is_unavailable(e) => EngineDone::Unavailable(e.to_string()),
        (EngineError::Io(e), _) if is_write_error(&job.targets, e) => EngineDone::Failed {
            error: format!("サムネイルを書けません: {e}"),
            model: info.map(boxed),
        },
        // 別のプロセスで読む SVG が時間の上限を超えた：止めて種類の絵にする（決定 23）。ImageIO の道は SVG のサムネイルを
        // 作れず（確認の S の形 12 件はどれも 0.3 秒で失敗）、時間を使う SVG をプロセスの中でもう一度開くだけなので回さない
        (e, _) if svg_worker::is_time_limit(e) => EngineDone::Failed {
            error: e.to_string(),
            model: info.map(boxed),
        },
        (_, EngineWhat::Image { .. }) => EngineDone::Fallback(err.to_string()),
        (_, EngineWhat::Model) => EngineDone::Failed {
            error: err.to_string(),
            model: info.map(boxed),
        },
    }
}

/// 書き先のフォルダに書けないなど（ImageIO に回しても書けない）。読み取りの失敗とは分ける。
fn is_write_error(targets: &ThumbTargets, err: &std::io::Error) -> bool {
    matches!(
        err.kind(),
        std::io::ErrorKind::PermissionDenied
            | std::io::ErrorKind::StorageFull
            | std::io::ErrorKind::ReadOnlyFilesystem
    ) && targets
        .large
        .parent()
        .is_some_and(|p| !p.as_os_str().is_empty())
}

/// 見積もった件を、同時の確保が `cap` に収まる小分けにする（順を保つ。1 件で `cap` を超えるものは 1 件だけの小分け）。
fn groups(planned: Vec<Planned>, cap: u64, threads: u64) -> Vec<Vec<Planned>> {
    let mut out: Vec<Vec<Planned>> = Vec::new();
    let mut current: Vec<Planned> = Vec::new();
    for p in planned {
        if !current.is_empty() {
            let mut next = Estimate::default();
            for q in current.iter().chain(std::iter::once(&p)) {
                next.fixed = next.fixed.saturating_add(q.est.fixed);
                next.per_task = next.per_task.max(q.est.per_task);
            }
            if next
                .fixed
                .saturating_add(next.per_task.saturating_mul(threads))
                > cap
            {
                out.push(std::mem::take(&mut current));
            }
        }
        current.push(p);
    }
    if !current.is_empty() {
        out.push(current);
    }
    out
}

/// 小分けの確保の量：持つ分の和と、仕事ごとの分の最大 × 一括のプールのスレッドの数（同時に動く仕事はスレッドの数まで）。
fn group_cost(group: &[Planned], threads: u64) -> u64 {
    let fixed = group
        .iter()
        .fold(0u64, |a, p| a.saturating_add(p.est.fixed));
    let per_task = group.iter().map(|p| p.est.per_task).max().unwrap_or(0);
    fixed.saturating_add(per_task.saturating_mul(threads))
}

/// 長辺を `max` 以下に（拡大しない）。四捨五入、1 以上。
pub fn fit(w: u32, h: u32, max: u32) -> (u32, u32) {
    let long = w.max(h);
    if long <= max || long == 0 {
        return (w, h);
    }
    let scale = |v: u32| {
        let v = (u64::from(v) * u64::from(max) + u64::from(long) / 2) / u64::from(long);
        u32::try_from(v).unwrap_or(max).max(1)
    };
    (scale(w), scale(h))
}

/// 画像の見積もり：読んだ値と読む間の作業、キャンバスに置く写し、縮めた 2 枚と縮める作業、RGBA8 にした 2 枚と、書く作業
/// （PNG・JPEG の多いほう）と書いたバイト列。
pub fn image_estimate(codecs: &CodecSet, format: &str, shape: &Shape, len: u64) -> Estimate {
    let (w, h, fmt, frames) = match shape {
        Shape::Image { w, h, fmt } => (w.max(), h.max(), *fmt, false),
        Shape::Frames { w, h, fmt, .. } => (w.max(), h.max(), *fmt, true),
        Shape::Audio { .. } => (0, 0, PixelFormat::Rgba8, false),
    };
    let to32 = |v: u64| u32::try_from(v).unwrap_or(u32::MAX);
    let canvas_fmt = if frames { PixelFormat::Rgba8 } else { fmt };
    let bpp = smooth_format(canvas_fmt).bytes_per_pixel() as u64;
    let (lw, lh) = fit(to32(w), to32(h), ThumbSize::Large.max_px());
    let (sw, sh) = fit(lw, lh, ThumbSize::Small.max_px());
    let (lw, lh, sw, sh) = (u64::from(lw), u64::from(lh), u64::from(sw), u64::from(sh));
    let in_bpp = canvas_fmt.bytes_per_pixel() as u64;
    let (shared_l, per_l) = smooth_work(w, h, in_bpp, lw, lh, Filter::Box);
    let (shared_s, per_s) = smooth_work(lw, lh, bpp, sw, sh, Filter::Box);
    let canvas_rgba = w.saturating_mul(h).saturating_mul(4);
    let place = if frames {
        canvas_rgba.saturating_mul(3)
    } else {
        0
    };
    let large_rgba = lw * lh * 4;
    let small_rgba = sw * sh * 4;
    let rgba_shape = Shape::Image {
        w: kura_engine::Dim::Exact(lw),
        h: kura_engine::Dim::Exact(lh),
        fmt: PixelFormat::Rgba8,
    };
    let png = codecs.encode_work_bytes("png", &rgba_shape, &png_opts());
    let rgb_shape = Shape::Image {
        w: kura_engine::Dim::Exact(lw),
        h: kura_engine::Dim::Exact(lh),
        fmt: PixelFormat::Rgb8,
    };
    let jpeg = codecs.encode_work_bytes("jpeg", &rgb_shape, &jpeg_opts());
    let fixed = [
        shape.value_bytes(),
        codecs.decode_file_work_bytes(format, shape, len),
        place,
        (lw * lh + sw * sh).saturating_mul(bpp),
        shared_l,
        shared_s,
        (large_rgba + small_rgba).saturating_mul(2),
        png.max(jpeg),
        large_rgba,
    ]
    .into_iter()
    .fold(0u64, u64::saturating_add);
    Estimate {
        fixed,
        per_task: per_l.max(per_s),
    }
}

/// 3D の見積もり：glb のバイト列、読み込み（`ModelInfo::estimate_load_bytes`）、長辺 1600 と 512 の描画、RGBA8 の 2 枚、書く作業
/// （`png_work`：長辺 1600 の RGBA8 を PNG に書く作業領域）と書いたバイト列。
pub fn model_estimate(
    info: &model::ModelInfo,
    limits: &ModelLimits,
    len: u64,
    png_work: u64,
) -> Estimate {
    let render_shared = |size: u64| {
        (size * size * 4 * 2)
            .saturating_add(info.vertices.saturating_mul(24))
            .saturating_add(info.triangles.saturating_mul(28))
    };
    let (l, s) = (
        u64::from(ThumbSize::Large.max_px()),
        u64::from(ThumbSize::Small.max_px()),
    );
    let per_task =
        |size: u64, k: u64| size * k * MODEL_BAND_ROWS * k * 20 + size * MODEL_BAND_ROWS * 4;
    let rgba = (l * l + s * s) * 4;
    let fixed = [
        len,
        info.estimate_load_bytes(limits),
        render_shared(l),
        render_shared(s),
        rgba.saturating_mul(2),
        png_work,
        l * l * 4,
    ]
    .into_iter()
    .fold(0u64, u64::saturating_add);
    Estimate {
        fixed,
        per_task: per_task(l, 2).max(per_task(s, 3)),
    }
}

/// glb の情報を、見出しと JSON の塊だけを読んで数える（BIN は読まない。ジョブのスレッドで見積もりと probe に使う）。
/// ファイルの長さも返す。JSON の塊の長さは、読む前に上限で確かめる。
pub fn glb_info(path: &Path, limits: &ModelLimits) -> Result<(model::ModelInfo, u64), EngineError> {
    let bad = |message: &str| EngineError::Decode {
        codec: model::GLB_READER.to_string(),
        message: message.to_string(),
    };
    let mut file = File::open(path)?;
    let len = file.metadata()?.len();
    if len > limits.max_file_bytes {
        return Err(EngineError::Limit(format!(
            "glb が大きすぎます（{len} バイト）"
        )));
    }
    let mut head = [0u8; GLB_HEAD];
    file.read_exact(&mut head)
        .map_err(|_| bad("glb の見出しが短すぎます"))?;
    let word = |at: usize| u32::from_le_bytes([head[at], head[at + 1], head[at + 2], head[at + 3]]);
    if word(16) != CHUNK_JSON {
        // 先頭が JSON の塊でない・glb でないものは、読み手にそのまま断らせる（理由をそろえる）
        let mut short = head.to_vec();
        short.truncate(GLB_HEAD);
        return model::read_info(&short, limits).map(|info| (info, len));
    }
    let json_len = u64::from(word(12));
    if json_len > limits.max_json_bytes {
        return Err(EngineError::Limit(format!(
            "glb の JSON が大きすぎます（{json_len} バイト）"
        )));
    }
    let total = GLB_HEAD as u64 + json_len;
    if total > len {
        return Err(bad("JSON の塊がファイルを超えます"));
    }
    let total_usize = usize::try_from(total).map_err(|_| bad("JSON の塊がファイルを超えます"))?;
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(total_usize)
        .map_err(|_| EngineError::Limit("glb の JSON を読む領域を取れません".into()))?;
    bytes.extend_from_slice(&head);
    file.take(json_len).read_to_end(&mut bytes)?;
    if bytes.len() != total_usize {
        return Err(bad("JSON の塊が途中で切れています"));
    }
    // 見出しの長さを、JSON の塊までにする（読み手は BIN を読まない。情報は JSON だけから数える）
    let total_u32 = u32::try_from(total).map_err(|_| bad("glb が大きすぎます"))?;
    bytes[8..12].copy_from_slice(&total_u32.to_le_bytes());
    model::read_info(&bytes, limits).map(|info| (info, len))
}

/// 先頭のフレームを、キャンバスの大きさの画像にする（フレームの列は offset の位置に置き、外側は透明。キャンバスいっぱいの
/// フレームはそのまま）。
pub fn first_frame(value: &Value) -> Result<Raster, EngineError> {
    match value {
        Value::Image(img) => Ok(img.px.clone()),
        Value::Frames(frames) => {
            let first = frames.first();
            let canvas = frames.canvas();
            let full = first.offset.x == 0
                && first.offset.y == 0
                && first.px.width() == canvas.w
                && first.px.height() == canvas.h;
            if full {
                return Ok(first.px.clone());
            }
            let rows = rgba8_rows(&first.px)?;
            let (cw, ch) = (canvas.w as usize, canvas.h as usize);
            let mut out = kura_engine::value::raster::alloc_zeroed(
                cw.checked_mul(ch)
                    .and_then(|n| n.checked_mul(4))
                    .ok_or_else(|| EngineError::Limit("キャンバスが大きすぎます".into()))?,
            )?;
            let (fw, fh) = (first.px.width() as usize, first.px.height() as usize);
            // Frames::new がキャンバスの中に収まることを確かめてある（offset は 0 以上）
            let ox = usize::try_from(first.offset.x).unwrap_or(0);
            let oy = usize::try_from(first.offset.y).unwrap_or(0);
            for y in 0..fh.min(ch.saturating_sub(oy)) {
                let src = &rows[y * fw * 4..(y + 1) * fw * 4];
                let at = ((oy + y) * cw + ox) * 4;
                let n = src.len().min(out.len().saturating_sub(at));
                out[at..at + n].copy_from_slice(&src[..n]);
            }
            Raster::from_rows(canvas.w, canvas.h, PixelFormat::Rgba8, None, &out)
        }
        Value::Audio(_) => Err(EngineError::Unsupported(
            "音声からはサムネイルを作りません".into(),
        )),
    }
}

/// 色の ICC プロファイル（グレーの画像のものは RGB の出力に合わないので捨てる）。
fn color_icc(value: &Value) -> Option<Arc<Vec<u8>>> {
    let (fmt, meta) = match value {
        Value::Image(img) => (img.px.format(), &img.meta),
        Value::Frames(f) => (f.first().px.format(), &f.meta),
        Value::Audio(_) => return None,
    };
    if is_gray(fmt) {
        return None;
    }
    meta.icc.clone()
}

/// 長辺を `max` 以下に Box で縮める（拡大しない）。
pub fn shrink(px: &Raster, max: u32, mon: &dyn Monitor) -> Result<Raster, EngineError> {
    let (w, h) = fit(px.width(), px.height(), max);
    if (w, h) == (px.width(), px.height()) {
        Ok(px.clone())
    } else {
        resize_smooth(px, w, h, Filter::Box, mon)
    }
}

/// どの形式（16 ビット・グレー・パレットを含む）でも、行を詰めた RGBA8 にする（16 ビットは四捨五入）。
pub fn rgba8_rows(px: &Raster) -> Result<Vec<u8>, EngineError> {
    let rows = px.to_rows()?;
    let n = (px.width() as usize)
        .checked_mul(px.height() as usize)
        .and_then(|n| n.checked_mul(4))
        .ok_or_else(|| EngineError::Limit("画像が大きすぎます".into()))?;
    let mut out = Vec::new();
    out.try_reserve_exact(n)
        .map_err(|_| EngineError::Limit("画像が大きすぎます".into()))?;
    for_each_rgba8_key(
        px.format(),
        px.palette().map(|p| p.colors.as_slice()),
        &rows,
        |key| {
            out.extend_from_slice(&key.to_be_bytes());
            true
        },
    )?;
    Ok(out)
}

fn png_opts() -> serde_json::Value {
    serde_json::json!({ "compression": "fast" })
}

fn jpeg_opts() -> serde_json::Value {
    serde_json::json!({ "quality": JPEG_QUALITY })
}

/// 長辺 1600 と 512 を書く：長辺 1600 に透明な画素があれば PNG、なければ JPEG（Rgb8 に落として）。書いたら、ほかの形式の
/// 古いファイルを消す。
pub fn write_pair(
    codecs: &CodecSet,
    large: &Raster,
    small: &Raster,
    icc: Option<Arc<Vec<u8>>>,
    targets: &ThumbTargets,
    mon: &dyn Monitor,
) -> Result<ThumbFormat, EngineError> {
    let large_rows = rgba8_rows(large)?;
    let transparent = large_rows
        .as_chunks::<4>()
        .0
        .iter()
        .any(|p| p[3] != u8::MAX);
    let format = if transparent {
        ThumbFormat::Png
    } else {
        ThumbFormat::Jpeg
    };
    let small_rows = rgba8_rows(small)?;
    let encode = |px: &Raster, rows: Vec<u8>| -> Result<Vec<u8>, EngineError> {
        let meta = ImageMeta {
            icc: icc.clone(),
            ..ImageMeta::default()
        };
        let (w, h) = (px.width(), px.height());
        let (raster, id, opts) = match format {
            ThumbFormat::Png => (
                Raster::from_rows(w, h, PixelFormat::Rgba8, None, &rows)?,
                "png",
                png_opts(),
            ),
            ThumbFormat::Jpeg => {
                let rgb: Vec<u8> = rows
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .flat_map(|p| [p[0], p[1], p[2]])
                    .collect();
                (
                    Raster::from_rows(w, h, PixelFormat::Rgb8, None, &rgb)?,
                    "jpeg",
                    jpeg_opts(),
                )
            }
        };
        let value = Value::Image(Arc::new(Image { px: raster, meta }));
        Ok(codecs.encode_bytes(&value, id, &opts, mon)?.0)
    };
    let large_bytes = encode(large, large_rows)?;
    let small_bytes = encode(small, small_rows)?;
    let (large_path, small_path) = targets.files(format);
    if let Some(parent) = large_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&large_path, large_bytes)?;
    std::fs::write(&small_path, small_bytes)?;
    targets.remove_others(format)?;
    Ok(format)
}

/// テスト用の PNG（RGBA8。`alpha` が真なら外周 2 画素を透明に、偽ならすべて不透明）。
#[cfg(test)]
pub(crate) fn test_png(w: u32, h: u32, alpha: bool) -> Vec<u8> {
    let mut rows = Vec::new();
    for y in 0..h {
        for x in 0..w {
            let edge = x < 2 || y < 2 || x + 2 >= w || y + 2 >= h;
            let a = if alpha && edge { 0 } else { 255 };
            rows.extend_from_slice(&[(x * 255 / w) as u8, (y * 255 / h) as u8, 200, a]);
        }
    }
    let px = Raster::from_rows(w, h, PixelFormat::Rgba8, None, &rows).unwrap();
    let v = Value::Image(Arc::new(Image {
        px,
        meta: ImageMeta::default(),
    }));
    CodecSet::pinned()
        .encode_bytes(&v, "png", &serde_json::json!({}), &kura_engine::NoMonitor)
        .unwrap()
        .0
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::sync::Mutex;

    use kura_engine::NoMonitor;
    use kura_engine::geom::{IPoint, Size};
    use kura_engine::value::{Frame, Frames};

    use super::*;
    use crate::edit::{EngineFactory, RuntimeConfig};

    /// 小さな実行の場（一括 1 スレッド・予算を指定）と固定の段のコーデックの組。
    struct SmallFactory {
        budget: u64,
    }

    impl EngineFactory for SmallFactory {
        fn runtime_config(&self) -> RuntimeConfig {
            RuntimeConfig {
                cores: Some(2),
                preview_threads: Some(1),
                bulk_threads: Some(1),
                budget: Some(self.budget),
                ..RuntimeConfig::default()
            }
        }
        fn codecs(&self, _: &Path) -> CodecSet {
            CodecSet::pinned()
        }
    }

    fn thumbs(budget: u64) -> EngineThumbs {
        let services = EngineServices::new(Arc::new(SmallFactory { budget }), PathBuf::from("/x"));
        EngineThumbs::new(&services).unwrap()
    }

    /// RGBA8 の画像（`alpha` が偽ならすべて不透明。真なら外周 2 画素を透明に）を PNG で `path` に書く。
    fn write_png(path: &Path, w: u32, h: u32, alpha: bool) {
        std::fs::write(path, test_png(w, h, alpha)).unwrap();
    }

    /// 書いたサムネイルを読む（幅・高さ・RGBA8）。
    fn read(path: &Path, format: &str) -> (u32, u32, Vec<u8>) {
        let (v, _) = CodecSet::pinned()
            .decode_path(
                path,
                format,
                &DecodeOptions::default(),
                &Limits::default(),
                &NoMonitor,
            )
            .unwrap();
        let Value::Image(img) = v else {
            panic!("画像でない")
        };
        (
            img.px.width(),
            img.px.height(),
            rgba8_rows(&img.px).unwrap(),
        )
    }

    fn job(dir: &Path, name: &str) -> EngineJob {
        EngineJob {
            path: dir.join(format!("{name}.png")),
            what: EngineWhat::Image { format: "png" },
            targets: ThumbTargets {
                large: dir.join(format!("t/{name}_l")),
                small: dir.join(format!("t/{name}_s")),
            },
        }
    }

    /// plan tests 12：透明があれば PNG（l・s とも、外周の α は 0 のまま）、なければ JPEG。長辺は 1600・512 に縮め、小さいものは
    /// 拡大しない（l も s も元の大きさ）。ほかの形式の古いファイルは消える。
    #[test]
    fn writes_png_when_transparent_and_jpeg_when_opaque() {
        let dir = tempfile::tempdir().unwrap();
        let jobs = [
            job(dir.path(), "alpha"),
            job(dir.path(), "opaque"),
            job(dir.path(), "tiny"),
        ];
        write_png(&jobs[0].path, 2000, 1000, true);
        write_png(&jobs[1].path, 900, 1800, false);
        write_png(&jobs[2].path, 40, 30, true);
        // 前の作り方の JPEG（作り直したら消える）
        std::fs::create_dir_all(dir.path().join("t")).unwrap();
        for f in [
            jobs[0].targets.files(ThumbFormat::Jpeg),
            jobs[1].targets.files(ThumbFormat::Png),
        ] {
            std::fs::write(&f.0, b"old").unwrap();
            std::fs::write(&f.1, b"old").unwrap();
        }
        let done = thumbs(1 << 30).render(&jobs, &NoMonitor);
        assert_eq!(
            done[0],
            EngineDone::Written {
                format: ThumbFormat::Png,
                size: Some((2000, 1000)),
                model: None
            }
        );
        assert_eq!(
            done[1],
            EngineDone::Written {
                format: ThumbFormat::Jpeg,
                size: Some((900, 1800)),
                model: None
            }
        );
        assert!(matches!(
            done[2],
            EngineDone::Written {
                format: ThumbFormat::Png,
                ..
            }
        ));

        let (l, s) = jobs[0].targets.files(ThumbFormat::Png);
        let large = read(&l, "png");
        let small = read(&s, "png");
        assert_eq!((large.0, large.1), (1600, 800));
        assert_eq!((small.0, small.1), (512, 256));
        assert_eq!(large.2[3], 0, "角が透明でない");
        assert_eq!(large.2[(400 * 1600 + 800) * 4 + 3], 255, "中が不透明でない");
        let (jl, js) = jobs[0].targets.files(ThumbFormat::Jpeg);
        assert!(!jl.exists() && !js.exists(), "古い JPEG が残っている");

        let (l, s) = jobs[1].targets.files(ThumbFormat::Jpeg);
        assert_eq!(&std::fs::read(&l).unwrap()[..2], &[0xFF, 0xD8]);
        let large = read(&l, "jpeg");
        let small = read(&s, "jpeg");
        assert_eq!((large.0, large.1), (800, 1600));
        assert_eq!((small.0, small.1), (256, 512));
        assert!(!jobs[1].targets.files(ThumbFormat::Png).0.exists());

        let (l, s) = jobs[2].targets.files(ThumbFormat::Png);
        assert_eq!(
            (read(&l, "png").0, read(&l, "png").1),
            (40, 30),
            "拡大しない"
        );
        assert_eq!(
            (read(&s, "png").0, read(&s, "png").1),
            (40, 30),
            "拡大しない"
        );
    }

    /// plan tests 12：フレームの列の先頭のフレームは、キャンバスの offset の位置に置く（外側は透明）。キャンバスいっぱいの
    /// フレームはそのまま。
    #[test]
    fn first_frame_is_placed_on_the_canvas() {
        let red = Raster::from_rows(3, 2, PixelFormat::Rgb8, None, &[255, 0, 0].repeat(6)).unwrap();
        let blue =
            Raster::from_rows(3, 2, PixelFormat::Rgb8, None, &[0, 0, 255].repeat(6)).unwrap();
        let frames = Frames::new(
            vec![
                Frame {
                    px: red,
                    offset: IPoint { x: 4, y: 3 },
                    duration_ms: 100,
                },
                Frame {
                    px: blue,
                    offset: IPoint { x: 0, y: 0 },
                    duration_ms: 100,
                },
            ],
            Size { w: 8, h: 6 },
            ImageMeta::default(),
            0,
        )
        .unwrap();
        let canvas = first_frame(&Value::Frames(Arc::new(frames))).unwrap();
        assert_eq!((canvas.width(), canvas.height()), (8, 6));
        let rows = rgba8_rows(&canvas).unwrap();
        let at = |x: usize, y: usize| &rows[(y * 8 + x) * 4..(y * 8 + x) * 4 + 4];
        assert_eq!(at(4, 3), [255, 0, 0, 255]);
        assert_eq!(at(6, 4), [255, 0, 0, 255]);
        for (x, y) in [(0, 0), (3, 3), (7, 5), (4, 2), (7, 2)] {
            assert_eq!(at(x, y)[3], 0, "({x}, {y}) が透明でない");
        }
        let opaque = rows
            .as_chunks::<4>()
            .0
            .iter()
            .filter(|p| p[3] == 255)
            .count();
        assert_eq!(opaque, 6, "2 枚目のフレームを描いた");

        // キャンバスいっぱいのフレームは写さずにそのまま
        let full = Frames::full(
            vec![(
                Raster::filled(5, 4, PixelFormat::Gray8, None, &[7]).unwrap(),
                50,
            )],
            ImageMeta::default(),
            0,
        )
        .unwrap();
        let px = first_frame(&Value::Frames(Arc::new(full))).unwrap();
        assert_eq!(
            (px.width(), px.height(), px.format()),
            (5, 4, PixelFormat::Gray8)
        );
    }

    /// 呼ばれた一括のプールのスレッドの名前を覚える（プールの外の呼び出し（予算の確保を待つ間）は数えない）。
    #[derive(Default)]
    struct Threads(Mutex<BTreeSet<String>>);

    impl Monitor for Threads {
        fn canceled(&self) -> bool {
            if rayon::current_thread_index().is_some() {
                let name = std::thread::current()
                    .name()
                    .unwrap_or("名前なし")
                    .to_string();
                self.0.lock().unwrap().insert(name);
            }
            false
        }
    }

    /// plan tests 12：描くのは一括のプール（`kura-edit-bulk-*`）の中。予算の全体を超える 1 件は確保せずに ImageIO の道へ回し
    /// （panic しない）、収まるものは描く。描き終えたら予算を返す。1 GiB を超える値は形だけで回す。
    #[test]
    fn engine_route_runs_in_the_bulk_pool_and_respects_the_budget() {
        let dir = tempfile::tempdir().unwrap();
        let jobs = [job(dir.path(), "small"), job(dir.path(), "big")];
        write_png(&jobs[0].path, 64, 48, true);
        write_png(&jobs[1].path, 1024, 1024, false);
        let engine = thumbs(8 << 20);
        let small = image_estimate(
            &engine.codecs,
            "png",
            &engine
                .codecs
                .shape_path(
                    &jobs[0].path,
                    "png",
                    &DecodeOptions::default(),
                    &Limits::default(),
                )
                .unwrap()
                .0,
            0,
        );
        assert!(small.fixed + small.per_task <= 8 << 20, "{small:?}");
        let mon = Threads::default();
        let done = engine.render(&jobs, &mon);
        assert!(
            matches!(
                done[0],
                EngineDone::Written {
                    format: ThumbFormat::Png,
                    ..
                }
            ),
            "{done:?}"
        );
        let EngineDone::Fallback(reason) = &done[1] else {
            panic!("{done:?}")
        };
        assert!(reason.contains("大きすぎます"), "{reason}");
        assert!(!jobs[1].targets.files(ThumbFormat::Jpeg).0.exists());
        let names = mon.0.lock().unwrap();
        assert!(!names.is_empty());
        assert!(
            names.iter().all(|n| n.starts_with("kura-edit-bulk")),
            "{names:?}"
        );
        assert_eq!(
            engine.runtime.budget().used().unwrap(),
            0,
            "予算を返していない"
        );

        // 同時の上限を小さくしても、1 件ずつ描ける（小分けは 1 件だけになる）
        let one_by_one = thumbs(1 << 30).with_cap(1);
        let done = one_by_one.render(&jobs, &NoMonitor);
        assert!(
            done.iter().all(|d| matches!(d, EngineDone::Written { .. })),
            "{done:?}"
        );
    }

    /// 中止を頼まれていれば何も描かない（pending・stale のまま残す）。読めないものは ImageIO の道へ回す。
    #[test]
    fn cancel_and_unreadable_files_do_not_write() {
        let dir = tempfile::tempdir().unwrap();
        let jobs = [job(dir.path(), "a"), job(dir.path(), "broken")];
        write_png(&jobs[0].path, 32, 32, true);
        std::fs::write(&jobs[1].path, b"\x89PNG\r\n\x1a\n not really").unwrap();
        let flag = kura_engine::monitor::CancelFlag::default();
        flag.cancel();
        let engine = thumbs(1 << 30);
        assert!(
            engine
                .render(&jobs, &flag)
                .iter()
                .all(|d| *d == EngineDone::Canceled)
        );
        assert!(!dir.path().join("t").exists());
        let done = engine.render(&jobs, &NoMonitor);
        assert!(matches!(done[0], EngineDone::Written { .. }));
        assert!(matches!(done[1], EngineDone::Fallback(_)), "{done:?}");
    }

    /// plan tests 13（model のサムネイル）：glb は kura-engine の描画で、透明の地に描いた PNG になる。情報（頂点・三角形・大きさ）
    /// も返す。描けないもの（必須の Draco）は描かずに情報だけ。
    #[test]
    fn model_thumbnails_are_drawn_by_the_engine() {
        let dir = tempfile::tempdir().unwrap();
        let samples = model::sample_glbs().unwrap();
        let cube = &samples.iter().find(|(n, _)| *n == "cube").unwrap().1;
        let path = dir.path().join("cube.glb");
        std::fs::write(&path, cube).unwrap();
        let draco_path = dir.path().join("draco.glb");
        std::fs::write(
            &draco_path,
            model::glb_bytes(
                &serde_json::json!({
                    "asset": { "version": "2.0" },
                    "extensionsUsed": ["KHR_draco_mesh_compression"],
                    "extensionsRequired": ["KHR_draco_mesh_compression"]
                }),
                &[],
            ),
        )
        .unwrap();
        let jobs = [
            EngineJob {
                path: path.clone(),
                what: EngineWhat::Model,
                targets: ThumbTargets {
                    large: dir.path().join("t/cube_l"),
                    small: dir.path().join("t/cube_s"),
                },
            },
            EngineJob {
                path: draco_path,
                what: EngineWhat::Model,
                targets: ThumbTargets {
                    large: dir.path().join("t/draco_l"),
                    small: dir.path().join("t/draco_s"),
                },
            },
        ];
        let done = thumbs(1 << 30).render(&jobs, &NoMonitor);
        let EngineDone::Written {
            format: ThumbFormat::Png,
            size: None,
            model: Some(info),
        } = &done[0]
        else {
            panic!("{done:?}")
        };
        assert_eq!((info.vertices, info.triangles, info.materials), (24, 12, 6));
        assert_eq!(info.size, Some([1.0, 1.0, 1.0]));
        let (l, s) = jobs[0].targets.files(ThumbFormat::Png);
        let large = read(&l, "png");
        let small = read(&s, "png");
        assert_eq!(large.0.max(large.1), 1600);
        assert_eq!(small.0.max(small.1), 512);
        for px in [&large, &small] {
            assert_eq!(px.2[3], 0, "角が透明でない");
            assert!(
                px.2.as_chunks::<4>().0.iter().any(|p| p[3] == 255),
                "何も描いていない"
            );
        }
        let EngineDone::Failed {
            error,
            model: Some(info),
        } = &done[1]
        else {
            panic!("{done:?}")
        };
        assert!(error.contains("KHR_draco_mesh_compression"), "{error}");
        assert!(!info.unsupported.is_empty());
        assert!(!jobs[1].targets.files(ThumbFormat::Png).0.exists());

        // 情報は見出しと JSON の塊だけから数え、ファイル全体を読んだときと同じ
        let (prefix, len) = glb_info(&path, &ModelLimits::default()).unwrap();
        assert_eq!(len, cube.len() as u64);
        assert_eq!(
            prefix,
            model::read_info(cube, &ModelLimits::default()).unwrap()
        );
        // 壊れたものは panic せずに断る
        for bad in [
            &b"glTF"[..],
            b"",
            &cube[..30],
            b"not a glb file at all, really",
        ] {
            let p = dir.path().join("bad.glb");
            std::fs::write(&p, bad).unwrap();
            assert!(glb_info(&p, &ModelLimits::default()).is_err());
        }
    }

    /// 働き手（/bin/sh の偽物など）か、決まった答えを返す偽のデコーダに SVG を頼む組。
    struct SvgFactory {
        worker: Option<crate::codec_ext::svg_worker::WorkerCommand>,
        deadlines: crate::codec_ext::svg_worker::Deadlines,
    }

    /// `/bin/sh -c script` の偽の働き手。
    fn sh(script: &str) -> crate::codec_ext::svg_worker::WorkerCommand {
        crate::codec_ext::svg_worker::WorkerCommand::new("/bin/sh", ["-c", script])
    }

    /// 形は答えるが、値は時間の上限を超えたと答える SVG のデコーダ（描く段の時間切れ）。
    struct SlowSvg;

    impl kura_engine::codec::Decoder for SlowSvg {
        fn id(&self) -> &'static str {
            kura_engine::codec::svg::DEC_ID
        }
        fn tier(&self) -> kura_engine::codec::Tier {
            kura_engine::codec::Tier::Pinned
        }
        fn reproducible(&self) -> bool {
            false
        }
        fn can_decode(&self, format: &str) -> bool {
            format == "svg"
        }
        fn shape(
            &self,
            _: &mut dyn kura_engine::codec::ReadSeek,
            _: &str,
            _: &DecodeOptions,
            _: &Limits,
        ) -> Result<Shape, EngineError> {
            Ok(Shape::Image {
                w: kura_engine::Dim::Exact(8),
                h: kura_engine::Dim::Exact(8),
                fmt: PixelFormat::Rgba8,
            })
        }
        fn decode(
            &self,
            _: &mut dyn kura_engine::codec::ReadSeek,
            _: &str,
            _: &DecodeOptions,
            _: &Limits,
            _: &dyn Monitor,
        ) -> Result<Value, EngineError> {
            Err(EngineError::Limit(
                crate::codec_ext::svg_worker::TIME_LIMIT.into(),
            ))
        }
    }

    impl EngineFactory for SvgFactory {
        fn runtime_config(&self) -> RuntimeConfig {
            SmallFactory { budget: 1 << 30 }.runtime_config()
        }
        fn codecs(&self, _: &Path) -> CodecSet {
            use crate::codec_ext::svg_worker::install_with;
            let mut set = CodecSet::pinned();
            match &self.worker {
                Some(worker) => install_with(
                    &mut set,
                    &kura_engine::codec::PinnedOptions::default(),
                    worker.clone(),
                    self.deadlines,
                )
                .unwrap(),
                None => drop(set.replace_decoder(Arc::new(SlowSvg)).unwrap()),
            }
            set
        }
    }

    /// 時刻が来たら中止を頼む見張り。
    struct CancelAfter(std::time::Instant, std::time::Duration);

    impl Monitor for CancelAfter {
        fn canceled(&self) -> bool {
            self.0.elapsed() > self.1
        }
    }

    /// `worker` の働き手（None なら値で時間切れになる偽のデコーダ）で SVG を読むエンジンの道（時間の上限 `ms`）。
    fn svg_engine(
        worker: Option<crate::codec_ext::svg_worker::WorkerCommand>,
        ms: u64,
    ) -> EngineThumbs {
        use crate::codec_ext::svg_worker::Deadlines;
        use std::time::Duration;
        let services = EngineServices::new(
            Arc::new(SvgFactory {
                worker,
                deadlines: Deadlines {
                    shape: Duration::from_millis(ms),
                    decode: Duration::from_millis(ms),
                },
            }),
            PathBuf::from("/x"),
        );
        EngineThumbs::new(&services).unwrap()
    }

    /// 決定 23：別のプロセスで読む SVG が時間の上限を超えたら、ImageIO へ回さずに失敗（種類の絵）にする（形を読む見積もりの段
    /// でも、描く段でも）。見積もりの段で形を読む子も、ジョブの中止で止めて Canceled にする（ライブラリを閉じるのを待たせない）。
    #[test]
    fn svg_time_limits_fail_without_fallback_and_cancel_stops_the_worker() {
        use crate::codec_ext::svg_worker::TIME_LIMIT;
        use std::time::{Duration, Instant};
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.svg");
        std::fs::write(
            &path,
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="8" height="8"/>"#,
        )
        .unwrap();
        let job = EngineJob {
            path,
            what: EngineWhat::Image { format: "svg" },
            targets: ThumbTargets {
                large: dir.path().join("t/a_l"),
                small: dir.path().join("t/a_s"),
            },
        };
        let engine = |script: Option<&str>, ms| svg_engine(script.map(sh), ms);
        let timed_out = |done: &EngineDone| matches!(done, EngineDone::Failed { error, model: None } if error.contains(TIME_LIMIT));
        let jobs = std::slice::from_ref(&job);
        // 形を読む段（見積もり）：何も返さない子
        let started = Instant::now();
        let done = engine(Some("sleep 38.4159"), 200).render(jobs, &NoMonitor);
        assert!(timed_out(&done[0]), "{done:?}");
        assert!(started.elapsed() < Duration::from_secs(2));
        // 描く段：値で時間切れ
        let done = engine(None, 200).render(jobs, &NoMonitor);
        assert!(timed_out(&done[0]), "{done:?}");
        // 中止：見積もりの段の子を止める
        let started = Instant::now();
        let mon = CancelAfter(Instant::now(), Duration::from_millis(200));
        let done = engine(Some("sleep 39.4159"), 30_000).render(jobs, &mon);
        assert_eq!(done, vec![EngineDone::Canceled]);
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "{:?}",
            started.elapsed()
        );
        // 時間切れでない失敗（壊れた返事）は、今までどおり ImageIO へ回す
        let done = engine(Some("cat >/dev/null; printf x"), 5_000).render(jobs, &NoMonitor);
        assert!(matches!(&done[0], EngineDone::Fallback(_)), "{done:?}");
    }

    /// M11 の 3 回目の確認 W2：SVG の働き手を使えない（起動できない・別のビルド）のは一時的な誤り。失敗にも ImageIO にも
    /// せずに `Unavailable`（何も書かない）にし、まだ見積もっていない残りは働き手を起動せずに同じ結果にする。
    #[test]
    fn an_unavailable_svg_worker_is_not_a_failure_and_stops_the_rest() {
        let dir = tempfile::tempdir().unwrap();
        let svg = br#"<svg xmlns="http://www.w3.org/2000/svg" width="8" height="8"/>"#;
        let jobs: Vec<EngineJob> = ["a", "b", "c"]
            .iter()
            .map(|name| {
                let path = dir.path().join(format!("{name}.svg"));
                std::fs::write(&path, svg).unwrap();
                EngineJob {
                    path,
                    what: EngineWhat::Image { format: "svg" },
                    targets: ThumbTargets {
                        large: dir.path().join(format!("t/{name}_l")),
                        small: dir.path().join(format!("t/{name}_s")),
                    },
                }
            })
            .collect();
        let count = dir.path().join("spawned");
        let counted = |reply: &str| {
            sh(&format!(
                "echo x >> '{}'; cat >/dev/null; printf '{reply}'",
                count.display()
            ))
        };
        let spawned = || {
            std::fs::read_to_string(&count)
                .map(|t| t.lines().count())
                .unwrap_or(0)
        };
        let workers = [
            // 前の版のやり取り（版 1）で答える働き手（置き換えた前のビルド）
            counted(r"KURASVG<\001\000\000\000\000\000\000\000"),
            // 同じ版で、ビルドの印が違う働き手
            counted(r"KURASVG<\002\000\000\000\002\000\000\000{}"),
            // 働き手でないもの（印が違う）
            counted("this is not a kura svg worker"),
        ];
        for worker in workers {
            let _ = std::fs::remove_file(&count);
            let done = svg_engine(Some(worker), 5_000).render(&jobs, &NoMonitor);
            assert!(
                done.iter().all(|d| matches!(d, EngineDone::Unavailable(why) if why.contains(crate::codec_ext::svg_worker::WORKER_TOOL))),
                "{done:?}"
            );
            assert_eq!(spawned(), 1, "使えないと分かったら、残りは起動しない");
            assert!(!jobs[0].targets.large.with_extension("png").exists());
        }
        // 起動できない（実行ファイルがない）
        let missing = crate::codec_ext::svg_worker::WorkerCommand::new(
            dir.path().join("moved/kura-app"),
            [crate::codec_ext::svg_worker::WORKER_ARG],
        );
        let done = svg_engine(Some(missing), 5_000).render(&jobs, &NoMonitor);
        assert!(
            done.iter().all(
                |d| matches!(d, EngineDone::Unavailable(why) if why.contains("起動できません"))
            ),
            "{done:?}"
        );
    }

    #[test]
    fn fit_never_enlarges() {
        assert_eq!(fit(2000, 1000, 1600), (1600, 800));
        assert_eq!(fit(1000, 2000, 512), (256, 512));
        assert_eq!(fit(40, 30, 512), (40, 30));
        assert_eq!(fit(1600, 1, 512), (512, 1));
        assert_eq!(fit(0, 0, 512), (0, 0));
    }

    /// 小分けは順を保ち、同時の確保が上限に収まるように切る（1 件で上限を超えるものは 1 件だけ）。
    #[test]
    fn groups_keep_the_order_and_the_cap() {
        let p = |index, fixed| Planned {
            index,
            est: Estimate {
                fixed,
                per_task: 10,
            },
            model: None,
        };
        let gs = groups(
            vec![p(0, 40), p(1, 40), p(2, 200), p(3, 10), p(4, 10)],
            100,
            2,
        );
        let ids: Vec<Vec<usize>> = gs
            .iter()
            .map(|g| g.iter().map(|p| p.index).collect())
            .collect();
        assert_eq!(ids, vec![vec![0, 1], vec![2], vec![3, 4]]);
        assert_eq!(group_cost(&gs[0], 2), 100);
        assert_eq!(group_cost(&gs[1], 2), 220);
    }
}
