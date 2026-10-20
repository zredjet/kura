//! 素材の欄（M11。docs/design.md §3.1・docs/data-model.md §3）：中身ごとの解析の事実（`media_facts`）、アイテムごとの
//! 利用者の設定（`asset_settings`）、その実効値、フォントと 3D の情報（`items.codec_info`）。アニメの帯は [`anim`]、試聴の
//! PCM は [`pcm`]。
//!
//! - **実効値の規則はここの関数だけ**：ドット絵は [`effective_pixel_art`]（設定 ?? 自動の判定 ?? 偽）。一覧のタイルの SQL
//!   （items.rs の SUMMARY）と検索の SQL（query の compile）は、同じ規則であることをテストでこの関数と照らす
//!   （3 つの値 × 3 つの値の表は [`TRISTATE`]）。
//! - 設定は画像だけに付ける（音声・動画・フォント・3D の ID は飛ばす。検索の「ドット絵:あり」に画像以外が入らないように）。
//!   すべての欄が自動（NULL）に戻れば行を消す（「変えたアイテムにだけ行がある」）。
//! - 格子は、セルが 1 つもできない値と、セルの数を超えるコマの数を断る（大きさの分からない画像は、セルの幅・高さが
//!   1 以上かだけを見る）。帯で使うコマの数は [`anim::MAX_FRAMES`] まで。
//! - DB だけを読む（ファイルは読まない）。中身に依る事実は台帳の行（blobs.id）を経て引く。

pub mod anim;
pub mod pcm;

use std::collections::BTreeSet;

use kura_engine::ops::sprite::split_grid::cells_on_axis;
use rusqlite::{Connection, OptionalExtension, Row, params};
use serde::{Deserialize, Serialize};

use crate::import::MediaKind;
use crate::library::hex;
use crate::{CoreError, Result};

/// 設定の 3 つの状態（自動・はい・いいえ）と、自動の判定の 3 つの状態（未解析・はい・いいえ）。実効値の規則を照らす表に使う。
pub const TRISTATE: [Option<bool>; 3] = [None, Some(false), Some(true)];

/// ドット絵の実効値：利用者の設定（`asset_settings.pixel_art`）があればそれ、なければ自動の判定（`media_facts.pixel_art`）、
/// どちらもなければ（未解析）偽。表示（pixelated）と検索の「ドット絵:あり」はこの規則で決める（決定 7）。
pub fn effective_pixel_art(setting: Option<bool>, auto: Option<bool>) -> bool {
    setting.or(auto).unwrap_or(false)
}

/// アニメとして回せるか：コマが 2 つ以上あるか、格子を設定してあるか（[`anim`] が帯を作れる見込み）。
pub fn effective_animated(frame_count: Option<u32>, has_grid: bool) -> bool {
    has_grid || frame_count.is_some_and(|n| n >= 2)
}

/// 透過（`media_facts.alpha`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Alpha {
    /// 透明な画素がない（0）。
    None,
    /// 透明か不透明だけ（1）。
    Binary,
    /// 半透明がある（2）。
    Partial,
}

impl Alpha {
    /// DB の値（0・1・2）から。ほかの値は None。
    pub fn from_code(code: i64) -> Option<Self> {
        match code {
            0 => Some(Self::None),
            1 => Some(Self::Binary),
            2 => Some(Self::Partial),
            _ => None,
        }
    }
}

/// 解析の進み具合（`items.analysis_state`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AnalysisState {
    Pending,
    Ready,
    Failed,
    NotNeeded,
}

impl AnalysisState {
    /// DB の値から（知らない値は Pending として扱う）。
    pub fn parse(text: &str) -> Self {
        match text {
            "ready" => Self::Ready,
            "failed" => Self::Failed,
            "not_needed" => Self::NotNeeded,
            _ => Self::Pending,
        }
    }
}

/// 画像の事実（`media_facts` の画像の列。計っていないものは None）。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ImageFacts {
    pub alpha: Option<Alpha>,
    /// 1〜257（257 は 256 色を超える）。
    pub colors: Option<u32>,
    pub visible_colors: Option<u32>,
    pub pixel_kx: Option<u32>,
    pub pixel_ky: Option<u32>,
    pub pixel_lossless: Option<bool>,
    /// ドット絵の自動の判定（`pixel_rule` の版の規則）。
    pub pixel_art: Option<bool>,
    pub pixel_rule: Option<u32>,
    pub frame_count: Option<u32>,
}

/// 音声の事実（`media_facts` の音声の列）。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AudioFacts {
    /// 積分ラウドネス（LUFS）。計れなければ None で `loudness_reason`。
    pub loudness: Option<f64>,
    pub loudness_reason: Option<String>,
    /// LRA（LU）。
    pub loudness_range: Option<f64>,
    /// dBTP。
    pub true_peak: Option<f64>,
    pub sample_peak: Option<f64>,
    pub has_loop: Option<bool>,
    /// ループ点（フレームの半開区間）。
    pub loop_range: Option<(u64, u64)>,
    pub sample_rate: Option<u32>,
    pub channels: Option<u32>,
    /// 非可逆の形式は None。
    pub bits: Option<u32>,
    pub clip_runs: Option<u64>,
}

/// 解析の事実（`media_facts` の 1 行）。
#[derive(Debug, Clone, PartialEq)]
pub struct Facts {
    /// 解析できたか（false なら `error_code`）。
    pub ok: bool,
    pub error_code: Option<String>,
    /// 読んだデコーダ（形式だけで決めたときは None）。
    pub decoder: Option<String>,
    pub facts_v: i64,
    pub analyzed_at: i64,
    pub image: ImageFacts,
    pub audio: AudioFacts,
}

/// スプライトシートの格子の設定（`asset_settings.grid_json` の版 1）。`count` は使うコマの数（None はすべて）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GridSetting {
    pub cell_w: u32,
    pub cell_h: u32,
    pub offset_x: u32,
    pub offset_y: u32,
    pub gap_x: u32,
    pub gap_y: u32,
    pub count: Option<u32>,
}

impl GridSetting {
    /// エンジンの格子（`sprite.split_grid` と同じセルの切り方）。
    pub fn engine_grid(&self) -> kura_engine::value::Grid {
        kura_engine::value::Grid {
            cell_w: self.cell_w,
            cell_h: self.cell_h,
            offset_x: self.offset_x,
            offset_y: self.offset_y,
            gap_x: self.gap_x,
            gap_y: self.gap_y,
        }
    }

    /// 大きさ `w × h` の画像に切れるセルの数（行の数 × 列の数。`sprite.split_grid` の `cells_on_axis` と同じ式）。
    pub fn cells(&self, w: u32, h: u32) -> u64 {
        let cols = cells_on_axis(u64::from(w), self.offset_x, self.cell_w, self.gap_x);
        let rows = cells_on_axis(u64::from(h), self.offset_y, self.cell_h, self.gap_y);
        cols.saturating_mul(rows)
    }

    /// 列の数（1 行に並ぶセルの数）。
    pub fn cols(&self, w: u32) -> u64 {
        cells_on_axis(u64::from(w), self.offset_x, self.cell_w, self.gap_x)
    }

    /// 帯で使うコマの数：min(設定のコマの数 ?? すべて, セルの数, [`anim::MAX_FRAMES`])。
    pub fn frames_used(&self, w: u32, h: u32) -> u32 {
        let cells = self.cells(w, h);
        let want = self.count.map_or(cells, u64::from);
        u32::try_from(want.min(cells).min(u64::from(anim::MAX_FRAMES))).unwrap_or(anim::MAX_FRAMES)
    }
}

/// 格子を確かめる。`size` が分かればセルが 1 つ以上できること、コマの数がセルの数以下であることも見る。理由は画面に出す文。
pub fn validate_grid(
    grid: &GridSetting,
    size: Option<(u32, u32)>,
) -> std::result::Result<(), String> {
    if grid.cell_w == 0 || grid.cell_h == 0 {
        return Err("セルの幅と高さは 1 以上にしてください".into());
    }
    if grid.count == Some(0) {
        return Err("使うコマの数は 1 以上にしてください（空ならすべて）".into());
    }
    let Some((w, h)) = size else {
        return Ok(());
    };
    let cells = grid.cells(w, h);
    if cells == 0 {
        return Err(format!(
            "この格子では {w} × {h} の画像にセルが 1 つもできません"
        ));
    }
    match grid.count {
        Some(count) if u64::from(count) > cells => Err(format!(
            "使うコマの数（{count}）がセルの数（{cells}）を超えています"
        )),
        _ => Ok(()),
    }
}

/// ピボット（画素の座標。小数を許す）。
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Pivot {
    pub x: f64,
    pub y: f64,
}

/// 利用者の設定（`asset_settings` の 1 行。行がなければすべて None）。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AssetSettings {
    /// ドット絵として表示（None は自動）。
    pub pixel_art: Option<bool>,
    /// 倍率の上書き（1〜64）。
    pub pixel_scale: Option<u32>,
    pub grid: Option<GridSetting>,
    /// 格子のアニメのコマの長さ（ms）。
    pub frame_ms: Option<u32>,
    pub pivot: Option<Pivot>,
    /// 試聴のループ（M13 で書く。M11 では読むだけ）。
    pub loop_range: Option<(u64, u64)>,
    /// aseprite の再生するタグ。
    pub anim_tag: Option<String>,
    /// 最後に変えた時刻（行がなければ None）。
    pub updated_at: Option<i64>,
}

impl AssetSettings {
    /// どの欄も自動（行が要らない）か。
    pub fn is_empty(&self) -> bool {
        self.pixel_art.is_none()
            && self.pixel_scale.is_none()
            && self.grid.is_none()
            && self.frame_ms.is_none()
            && self.pivot.is_none()
            && self.loop_range.is_none()
            && self.anim_tag.is_none()
    }
}

/// 実効値（表示と検索に使う値）。
#[derive(Debug, Clone, PartialEq)]
pub struct Effective {
    /// ドット絵として表示するか（[`effective_pixel_art`]）。
    pub pixel_art: bool,
    /// アニメとして回せるか（[`effective_animated`]）。
    pub animated: bool,
    /// コマの数（格子があれば帯で使うコマの数、なければ解析のコマの数）。
    pub frame_count: Option<u32>,
}

/// フォントの情報（`items.codec_info` の `font`。取り込みのあとの probe が入れる）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct FontFacts {
    /// 書体名（ファミリー）。
    pub family: Option<String>,
    /// スタイル（Regular・Bold など）。
    pub style: Option<String>,
    pub full_name: Option<String>,
    pub postscript_name: Option<String>,
    /// 字の数。
    pub glyphs: Option<u32>,
    /// ファイルの中の書体の数（コレクションは 2 以上）。
    pub faces: Option<u32>,
    /// 可変フォントか。
    pub variable: Option<bool>,
    /// 形式（`codec_info.codec`。`ttf`・`otf`・`ttc`・`woff`・`woff2`）。
    #[serde(skip)]
    pub format: Option<String>,
}

/// 3D モデルの情報（`items.codec_info` の `model`。サムネイルのジョブが `media::ModelInfo` の形で入れる）。
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ModelFacts {
    pub vertices: Option<u64>,
    pub triangles: Option<u64>,
    pub materials: Option<u32>,
    pub textures: Option<u32>,
    pub meshes: Option<u32>,
    /// 大きさ（範囲の max − min。x・y・z の順。glb の単位はメートル）。数えられなければ None。
    pub size: Option<[f64; 3]>,
    /// 描けない理由（必須の Draco・meshopt・知らない拡張の名前、`skin`）。空なら描ける。
    pub unsupported: Vec<String>,
}

/// 素材の欄の中身（`item_asset_get`）。
#[derive(Debug, Clone, PartialEq)]
pub struct AssetInfo {
    pub item_id: i64,
    /// 中身のハッシュ（16 進）。
    pub hash: String,
    pub kind: MediaKind,
    pub ext: String,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub analysis_state: AnalysisState,
    /// 解析の事実（まだなければ None）。
    pub facts: Option<Facts>,
    pub settings: AssetSettings,
    pub effective: Effective,
    pub font: Option<FontFacts>,
    pub model: Option<ModelFacts>,
}

/// 設定の列（`asset_settings` の列の並び。[`settings_from_row`] が読む）。
const SETTINGS_COLUMNS: &str =
    "s.item_id, s.pixel_art, s.pixel_scale, s.grid_json, s.frame_ms, s.pivot_x, s.pivot_y,
     s.loop_start, s.loop_end, s.anim_tag, s.updated_at";

/// 素材の欄の問い合わせ：アイテム（主キー）→ 台帳（hash の UNIQUE）→ 事実（blob_id の主キー）、設定（item_id の主キー）。
const GET_SQL: &str = "SELECT i.id, i.hash, i.ext, i.kind, i.width, i.height, i.analysis_state, i.codec_info,
            f.state, f.error_code, f.decoder, f.facts_v, f.analyzed_at,
            f.alpha, f.colors, f.visible_colors, f.pixel_kx, f.pixel_ky, f.pixel_lossless, f.pixel_art,
            f.pixel_rule, f.frame_count,
            f.loudness, f.loudness_reason, f.loudness_range, f.true_peak, f.sample_peak, f.has_loop,
            f.loop_start, f.loop_end, f.sample_rate, f.channels, f.bits, f.clip_runs,
            s.item_id, s.pixel_art, s.pixel_scale, s.grid_json, s.frame_ms, s.pivot_x, s.pivot_y,
            s.loop_start, s.loop_end, s.anim_tag, s.updated_at
     FROM items i
     LEFT JOIN blobs b ON b.hash = i.hash
     LEFT JOIN media_facts f ON f.blob_id = b.id
     LEFT JOIN asset_settings s ON s.item_id = i.id
     WHERE i.id = ?1";

/// 設定の列が始まる位置（[`GET_SQL`] の中）。
const GET_SETTINGS_AT: usize = 34;

/// 0 以上で u32 に収まる整数（ほかは None）。
fn u32_of(v: Option<i64>) -> Option<u32> {
    v.and_then(|v| u32::try_from(v).ok())
}

/// 0 以上の整数（ほかは None）。
fn u64_of(v: Option<i64>) -> Option<u64> {
    v.and_then(|v| u64::try_from(v).ok())
}

/// 0/1 の列（ほかの値は真）。
fn bool_of(v: Option<i64>) -> Option<bool> {
    v.map(|v| v != 0)
}

/// ループ点の 2 つの列（片方だけなら None。CHECK が断るので起きない）。
fn span_of(start: Option<i64>, end: Option<i64>) -> Option<(u64, u64)> {
    Some((u64_of(start)?, u64_of(end)?))
}

/// grid_json の版 1。
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct GridJson {
    v: u32,
    cell_w: u32,
    cell_h: u32,
    offset_x: u32,
    offset_y: u32,
    gap_x: u32,
    gap_y: u32,
    count: Option<u32>,
}

/// grid_json の版。
const GRID_JSON_V: u32 = 1;

fn grid_to_json(g: &GridSetting) -> Result<String> {
    Ok(serde_json::to_string(&GridJson {
        v: GRID_JSON_V,
        cell_w: g.cell_w,
        cell_h: g.cell_h,
        offset_x: g.offset_x,
        offset_y: g.offset_y,
        gap_x: g.gap_x,
        gap_y: g.gap_y,
        count: g.count,
    })?)
}

/// grid_json を読む（知らない版・壊れたものは格子なしとして warn を出す。設定し直せば直る）。
pub(crate) fn grid_from_json(text: &str) -> Option<GridSetting> {
    match serde_json::from_str::<GridJson>(text) {
        Ok(g) if g.v == GRID_JSON_V => Some(GridSetting {
            cell_w: g.cell_w,
            cell_h: g.cell_h,
            offset_x: g.offset_x,
            offset_y: g.offset_y,
            gap_x: g.gap_x,
            gap_y: g.gap_y,
            count: g.count,
        }),
        Ok(g) => {
            tracing::warn!(
                v = g.v,
                "知らない版の格子の設定なので、格子なしとして扱います"
            );
            None
        }
        Err(err) => {
            tracing::warn!(%err, "格子の設定を読めないので、格子なしとして扱います");
            None
        }
    }
}

/// 設定の列（`at` から [`SETTINGS_COLUMNS`] の 11 個）。行がなければ（item_id が NULL）既定。
fn settings_from_row(row: &Row<'_>, at: usize) -> rusqlite::Result<AssetSettings> {
    if row.get::<_, Option<i64>>(at)?.is_none() {
        return Ok(AssetSettings::default());
    }
    let pivot = match (
        row.get::<_, Option<f64>>(at + 5)?,
        row.get::<_, Option<f64>>(at + 6)?,
    ) {
        (Some(x), Some(y)) => Some(Pivot { x, y }),
        _ => None,
    };
    Ok(AssetSettings {
        pixel_art: bool_of(row.get(at + 1)?),
        pixel_scale: u32_of(row.get(at + 2)?),
        grid: row
            .get::<_, Option<String>>(at + 3)?
            .as_deref()
            .and_then(grid_from_json),
        frame_ms: u32_of(row.get(at + 4)?),
        pivot,
        loop_range: span_of(row.get(at + 7)?, row.get(at + 8)?),
        anim_tag: row.get(at + 9)?,
        updated_at: row.get(at + 10)?,
    })
}

/// 事実の列（[`GET_SQL`] の 8〜33）。行がなければ（state が NULL）None。
fn facts_from_row(row: &Row<'_>) -> rusqlite::Result<Option<Facts>> {
    let Some(state) = row.get::<_, Option<String>>(8)? else {
        return Ok(None);
    };
    Ok(Some(Facts {
        ok: state == "ok",
        error_code: row.get(9)?,
        decoder: row.get(10)?,
        facts_v: row.get(11)?,
        analyzed_at: row.get(12)?,
        image: ImageFacts {
            alpha: row.get::<_, Option<i64>>(13)?.and_then(Alpha::from_code),
            colors: u32_of(row.get(14)?),
            visible_colors: u32_of(row.get(15)?),
            pixel_kx: u32_of(row.get(16)?),
            pixel_ky: u32_of(row.get(17)?),
            pixel_lossless: bool_of(row.get(18)?),
            pixel_art: bool_of(row.get(19)?),
            pixel_rule: u32_of(row.get(20)?),
            frame_count: u32_of(row.get(21)?),
        },
        audio: AudioFacts {
            loudness: row.get(22)?,
            loudness_reason: row.get(23)?,
            loudness_range: row.get(24)?,
            true_peak: row.get(25)?,
            sample_peak: row.get(26)?,
            has_loop: bool_of(row.get(27)?),
            loop_range: span_of(row.get(28)?, row.get(29)?),
            sample_rate: u32_of(row.get(30)?),
            channels: u32_of(row.get(31)?),
            bits: u32_of(row.get(32)?),
            clip_runs: u64_of(row.get(33)?),
        },
    }))
}

/// codec_info（JSON）の `font`・`model`（種類が合うものだけ）。壊れた JSON は情報なし（欄そのものは出す）。
fn font_and_model(
    kind: MediaKind,
    codec_info: Option<&str>,
    ext: &str,
) -> (Option<FontFacts>, Option<ModelFacts>) {
    let json: serde_json::Value = codec_info
        .and_then(|text| serde_json::from_str(text).ok())
        .unwrap_or(serde_json::Value::Null);
    let codec = json
        .get("codec")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string);
    match kind {
        MediaKind::Font => {
            let mut font: FontFacts = json
                .get("font")
                .and_then(|v| serde_json::from_value(v.clone()).ok())
                .unwrap_or_default();
            font.format = codec.or_else(|| Some(ext.to_string()));
            (Some(font), None)
        }
        MediaKind::Model => {
            let model = json
                .get("model")
                .and_then(|v| serde_json::from_value(v.clone()).ok())
                .unwrap_or_default();
            (None, Some(model))
        }
        MediaKind::Image | MediaKind::Video | MediaKind::Audio => (None, None),
    }
}

/// 1 件の素材の欄の中身。なければ NotFound。
pub fn get(conn: &Connection, item_id: i64) -> Result<AssetInfo> {
    let mut stmt = conn.prepare_cached(GET_SQL)?;
    let found = stmt
        .query_row([item_id], |row| {
            let hash: Vec<u8> = row.get(1)?;
            let kind: MediaKind = row.get(3)?;
            let ext: String = row.get(2)?;
            let codec_info: Option<String> = row.get(7)?;
            let (font, model) = font_and_model(kind, codec_info.as_deref(), &ext);
            Ok(AssetInfo {
                item_id: row.get(0)?,
                hash: hex(&hash),
                kind,
                ext,
                width: u32_of(row.get(4)?),
                height: u32_of(row.get(5)?),
                analysis_state: AnalysisState::parse(&row.get::<_, String>(6)?),
                facts: facts_from_row(row)?,
                settings: settings_from_row(row, GET_SETTINGS_AT)?,
                effective: Effective {
                    pixel_art: false,
                    animated: false,
                    frame_count: None,
                },
                font,
                model,
            })
        })
        .optional()?;
    let mut info = found.ok_or_else(|| CoreError::NotFound(format!("アイテム {item_id}")))?;
    info.effective = effective_of(&info);
    Ok(info)
}

/// 実効値（画像でなければドット絵・アニメは偽）。
fn effective_of(info: &AssetInfo) -> Effective {
    let image = info.facts.as_ref().map(|f| &f.image);
    effective(
        info.kind,
        (info.width, info.height),
        EffectiveInputs {
            auto_pixel_art: image.and_then(|i| i.pixel_art),
            frame_count: image.and_then(|i| i.frame_count),
            pixel_art_setting: info.settings.pixel_art,
            grid: info.settings.grid.as_ref(),
        },
    )
}

/// 実効値の材料（事実と設定のうち、実効値に効く列だけ）。
#[derive(Debug, Clone, Copy, Default)]
pub struct EffectiveInputs<'a> {
    /// ドット絵の自動の判定（`media_facts.pixel_art`。未解析・材料不足は None）。
    pub auto_pixel_art: Option<bool>,
    /// 解析のコマの数（`media_facts.frame_count`）。
    pub frame_count: Option<u32>,
    /// ドット絵の設定（`asset_settings.pixel_art`。None は自動）。
    pub pixel_art_setting: Option<bool>,
    /// 格子の設定。
    pub grid: Option<&'a GridSetting>,
}

/// 実効値（素材の欄と一覧のタイル（items.rs の SUMMARY）で同じ規則）。画像でなければドット絵・アニメは偽で、コマの数は None。
/// コマの数は、格子があれば帯で使う数（大きさが分からなければ設定のコマの数）、なければ解析のコマの数。
pub fn effective(
    kind: MediaKind,
    size: (Option<u32>, Option<u32>),
    inputs: EffectiveInputs<'_>,
) -> Effective {
    if kind != MediaKind::Image {
        return Effective {
            pixel_art: false,
            animated: false,
            frame_count: None,
        };
    }
    let frame_count = match (inputs.grid, size) {
        (Some(grid), (Some(w), Some(h))) => Some(grid.frames_used(w, h)),
        // 大きさが分からなければ設定のコマの数（帯を作るときに画像から数える）
        (Some(grid), _) => grid.count,
        (None, _) => inputs.frame_count,
    };
    Effective {
        pixel_art: effective_pixel_art(inputs.pixel_art_setting, inputs.auto_pixel_art),
        animated: effective_animated(inputs.frame_count, inputs.grid.is_some()),
        frame_count,
    }
}

/// 設定の欄の変え方。
#[derive(Debug, Clone, PartialEq)]
pub enum Change<T> {
    /// 自動（設定なし）に戻す。
    Clear,
    /// この値にする。
    Set(T),
}

impl<T: Clone> Change<T> {
    fn apply(&self) -> Option<T> {
        match self {
            Self::Clear => None,
            Self::Set(v) => Some(v.clone()),
        }
    }
}

/// 設定の変更（省いた欄は変えない）。ループ点は M13 で書くので、ここでは変えない。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SettingsPatch {
    pub pixel_art: Option<Change<bool>>,
    pub pixel_scale: Option<Change<u32>>,
    pub grid: Option<Change<GridSetting>>,
    pub frame_ms: Option<Change<u32>>,
    pub pivot: Option<Change<Pivot>>,
    pub anim_tag: Option<Change<String>>,
}

/// タグの名前の長さの上限（バイト）。
const MAX_TAG_BYTES: usize = 256;

impl SettingsPatch {
    /// 画像の大きさに依らない値を確かめる（倍率 1〜64、コマの長さ 1 以上、ピボットは有限、タグの名前は空でなく 256 バイトまで）。
    pub fn validate(&self) -> Result<()> {
        let bad = |m: &str| Err(CoreError::Invalid(m.to_string()));
        if let Some(Change::Set(scale)) = self.pixel_scale
            && !(1..=64).contains(&scale)
        {
            return bad("ドット絵の倍率は 1〜64 にしてください");
        }
        if let Some(Change::Set(0)) = self.frame_ms {
            return bad("コマの長さは 1 ms 以上にしてください");
        }
        if let Some(Change::Set(p)) = self.pivot
            && !(p.x.is_finite() && p.y.is_finite())
        {
            return bad("ピボットの位置が数ではありません");
        }
        if let Some(Change::Set(tag)) = &self.anim_tag
            && (tag.is_empty() || tag.len() > MAX_TAG_BYTES)
        {
            return bad("タグの名前は 1〜256 バイトにしてください");
        }
        if let Some(Change::Set(grid)) = &self.grid {
            validate_grid(grid, None).map_err(CoreError::Invalid)?;
        }
        Ok(())
    }

    /// 今の設定に当てたもの。
    pub fn apply_to(&self, current: &AssetSettings) -> AssetSettings {
        let mut next = current.clone();
        if let Some(c) = &self.pixel_art {
            next.pixel_art = c.apply();
        }
        if let Some(c) = &self.pixel_scale {
            next.pixel_scale = c.apply();
        }
        if let Some(c) = &self.grid {
            next.grid = c.apply();
        }
        if let Some(c) = &self.frame_ms {
            next.frame_ms = c.apply();
        }
        if let Some(c) = &self.pivot {
            next.pivot = c.apply();
        }
        if let Some(c) = &self.anim_tag {
            next.anim_tag = c.apply();
        }
        next
    }
}

/// 選んだアイテム（右クリックで複数）の設定を変える。画像でない ID・ない ID は飛ばす。格子が合わない画像が 1 つでもあれば、
/// どれも変えずに Invalid。変えた（行の中身が変わった）アイテムの数を返す。
pub fn set_settings(
    conn: &Connection,
    ids: &[i64],
    patch: &SettingsPatch,
    now: i64,
) -> Result<u32> {
    patch.validate()?;
    let mut target = conn.prepare_cached(&format!(
        "SELECT i.kind, i.width, i.height, {SETTINGS_COLUMNS}
         FROM items i LEFT JOIN asset_settings s ON s.item_id = i.id WHERE i.id = ?1"
    ))?;
    // 先にすべてを確かめてから書く（呼び手のトランザクションの中なので、途中で断っても戻るが、書く前に断る）
    let mut writes: Vec<(i64, AssetSettings)> = Vec::new();
    for id in ids.iter().copied().collect::<BTreeSet<i64>>() {
        let found = target
            .query_row([id], |row| {
                Ok((
                    row.get::<_, MediaKind>(0)?,
                    u32_of(row.get(1)?).zip(u32_of(row.get(2)?)),
                    settings_from_row(row, 3)?,
                ))
            })
            .optional()?;
        let Some((MediaKind::Image, size, current)) = found else {
            continue;
        };
        let next = patch.apply_to(&current);
        if next.pixel_art == current.pixel_art
            && next.pixel_scale == current.pixel_scale
            && next.grid == current.grid
            && next.frame_ms == current.frame_ms
            && next.pivot == current.pivot
            && next.anim_tag == current.anim_tag
        {
            continue;
        }
        if let (Some(grid), Some(Change::Set(_))) = (&next.grid, &patch.grid) {
            validate_grid(grid, size)
                .map_err(|why| CoreError::Invalid(format!("アイテム {id}：{why}")))?;
        }
        writes.push((id, next));
    }
    let mut delete = conn.prepare_cached("DELETE FROM asset_settings WHERE item_id = ?1")?;
    let mut upsert = conn.prepare_cached(
        "INSERT INTO asset_settings (item_id, pixel_art, pixel_scale, grid_json, frame_ms, pivot_x, pivot_y,
                                     loop_start, loop_end, anim_tag, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
         ON CONFLICT (item_id) DO UPDATE SET
           pixel_art = excluded.pixel_art, pixel_scale = excluded.pixel_scale, grid_json = excluded.grid_json,
           frame_ms = excluded.frame_ms, pivot_x = excluded.pivot_x, pivot_y = excluded.pivot_y,
           loop_start = excluded.loop_start, loop_end = excluded.loop_end, anim_tag = excluded.anim_tag,
           updated_at = excluded.updated_at",
    )?;
    for (id, next) in &writes {
        if next.is_empty() {
            delete.execute([id])?;
            continue;
        }
        let grid_json = next.grid.as_ref().map(grid_to_json).transpose()?;
        let span = |v: Option<u64>| v.and_then(|v| i64::try_from(v).ok());
        upsert.execute(params![
            id,
            next.pixel_art.map(i64::from),
            next.pixel_scale,
            grid_json,
            next.frame_ms,
            next.pivot.map(|p| p.x),
            next.pivot.map(|p| p.y),
            span(next.loop_range.map(|l| l.0)),
            span(next.loop_range.map(|l| l.1)),
            next.anim_tag,
            now,
        ])?;
    }
    Ok(u32::try_from(writes.len()).unwrap_or(u32::MAX))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{query_plan, test_conn};

    /// 中身のハッシュ（`b` を 32 個）。
    fn hash(b: u8) -> Vec<u8> {
        vec![b; 32]
    }

    /// アイテムを 1 つ入れる（台帳にはトリガが載せる）。
    fn item(
        conn: &Connection,
        id: i64,
        kind: &str,
        size: Option<(i64, i64)>,
        codec_info: Option<&str>,
    ) {
        conn.execute(
            "INSERT INTO items (id, hash, ext, kind, size_bytes, width, height, codec_info, title, original_name,
                                imported_at, updated_at, proxy_state, analysis_state)
             VALUES (?1, ?2, ?3, ?4, 10, ?5, ?6, ?7, 't', 't', 1, 1, 'not_needed', 'pending')",
            params![
                id,
                hash(u8::try_from(id).unwrap()),
                match kind {
                    "audio" => "ogg",
                    "font" => "otf",
                    "model" => "glb",
                    _ => "png",
                },
                kind,
                size.map(|s| s.0),
                size.map(|s| s.1),
                codec_info,
            ],
        )
        .unwrap();
    }

    fn blob_id(conn: &Connection, id: i64) -> i64 {
        conn.query_row(
            "SELECT b.id FROM items i JOIN blobs b ON b.hash = i.hash WHERE i.id = ?1",
            [id],
            |r| r.get(0),
        )
        .unwrap()
    }

    fn settings_rows(conn: &Connection) -> i64 {
        conn.query_row("SELECT COUNT(*) FROM asset_settings", [], |r| r.get(0))
            .unwrap()
    }

    fn grid(cell: u32, count: Option<u32>) -> GridSetting {
        GridSetting {
            cell_w: cell,
            cell_h: cell,
            offset_x: 0,
            offset_y: 0,
            gap_x: 0,
            gap_y: 0,
            count,
        }
    }

    /// 実効値は「設定 ?? 自動の判定 ?? 偽」（9 通りすべて）。設定は自動の判定に勝ち、未解析は偽。
    #[test]
    fn effective_pixel_art_prefers_the_setting_then_the_auto_judgement() {
        for setting in TRISTATE {
            for auto in TRISTATE {
                let want = match (setting, auto) {
                    (Some(s), _) => s,
                    (None, Some(a)) => a,
                    (None, None) => false,
                };
                assert_eq!(
                    effective_pixel_art(setting, auto),
                    want,
                    "設定 {setting:?}・自動 {auto:?}"
                );
            }
        }
        assert!(effective_animated(Some(2), false));
        assert!(!effective_animated(Some(1), false));
        assert!(!effective_animated(None, false));
        assert!(effective_animated(None, true), "格子があれば回せる");
    }

    /// 格子は、セルが 1 つもできない値と、セルの数を超えるコマの数を断る。大きさが分からなければセルの幅・高さだけを見る。
    #[test]
    fn grid_validation_rejects_grids_without_cells() {
        let size = Some((64, 32));
        assert_eq!(grid(16, None).cells(64, 32), 8);
        assert_eq!(grid(16, Some(5)).frames_used(64, 32), 5);
        assert_eq!(grid(16, Some(50)).frames_used(64, 32), 8, "セルの数まで");
        assert_eq!(grid(1, None).frames_used(8192, 8192), anim::MAX_FRAMES);
        assert!(validate_grid(&grid(16, None), size).is_ok());
        assert!(validate_grid(&grid(16, Some(8)), size).is_ok());
        assert!(
            validate_grid(&grid(32, None), size).is_ok(),
            "ちょうど 2 × 1"
        );
        // 端の欠けたセルは数えない：offset と gap も効く
        let shifted = GridSetting {
            offset_x: 1,
            gap_x: 1,
            ..grid(16, None)
        };
        assert_eq!(
            shifted.cells(64, 32),
            6,
            "1 + 16 + 1 + 16 + 1 + 16 = 51 ≤ 64 で 3 列"
        );
        for (bad, why) in [
            (grid(0, None), "幅 0"),
            (
                GridSetting {
                    cell_h: 0,
                    ..grid(16, None)
                },
                "高さ 0",
            ),
            (grid(33, None), "高さ 33 は 32 に入らない"),
            (
                GridSetting {
                    offset_x: 60,
                    ..grid(16, None)
                },
                "ずれが大きすぎる",
            ),
            (grid(16, Some(0)), "コマ 0"),
            (grid(16, Some(9)), "セル 8 を超える"),
        ] {
            assert!(validate_grid(&bad, size).is_err(), "{why}");
        }
        // 大きさが分からない：セルの幅・高さとコマの数だけ
        assert!(validate_grid(&grid(4096, Some(9)), None).is_ok());
        assert!(validate_grid(&grid(0, None), None).is_err());
        assert!(validate_grid(&grid(16, Some(0)), None).is_err());
        // u32 の端でもあふれない
        let huge = GridSetting {
            cell_w: u32::MAX,
            offset_x: u32::MAX,
            gap_x: u32::MAX,
            ..grid(16, None)
        };
        assert_eq!(huge.cells(u32::MAX, 32), 0);
        assert!(validate_grid(&huge, Some((u32::MAX, 32))).is_err());
    }

    /// 素材の欄：台帳の行を経た事実、アイテムの設定、実効値、解析の進み具合。事実も設定もなければ None と既定。
    #[test]
    fn get_reads_facts_settings_and_effective_values() {
        let conn = test_conn();
        item(
            &conn,
            1,
            "image",
            Some((64, 32)),
            Some(r#"{"codec":"png"}"#),
        );
        item(&conn, 2, "audio", None, None);
        let blob = blob_id(&conn, 1);
        conn.execute(
            "INSERT INTO media_facts (blob_id, state, decoder, facts_v, alpha, colors, visible_colors, pixel_kx,
                                      pixel_ky, pixel_lossless, pixel_art, pixel_rule, frame_count, analyzed_at)
             VALUES (?1, 'ok', 'png-rs-dec/1', 1, 2, 16, 15, 4, 4, 1, 1, 1, 1, 99)",
            [blob],
        )
        .unwrap();
        conn.execute("UPDATE items SET analysis_state = 'ready' WHERE id = 1", [])
            .unwrap();

        let info = get(&conn, 1).unwrap();
        assert_eq!(info.hash, "01".repeat(32));
        assert_eq!(info.kind, MediaKind::Image);
        assert_eq!((info.width, info.height), (Some(64), Some(32)));
        assert_eq!(info.analysis_state, AnalysisState::Ready);
        let facts = info.facts.clone().unwrap();
        assert!(facts.ok);
        assert_eq!(facts.decoder.as_deref(), Some("png-rs-dec/1"));
        assert_eq!(facts.image.alpha, Some(Alpha::Partial));
        assert_eq!(facts.image.colors, Some(16));
        assert_eq!(facts.image.pixel_kx, Some(4));
        assert_eq!(facts.image.pixel_art, Some(true));
        assert_eq!(info.settings, AssetSettings::default());
        assert_eq!(
            info.effective,
            Effective {
                pixel_art: true,
                animated: false,
                frame_count: Some(1)
            }
        );
        assert_eq!((info.font.clone(), info.model.clone()), (None, None));

        // 設定は自動の判定に勝つ。格子があればアニメとして回せ、コマの数は帯で使う数
        conn.execute(
            r#"INSERT INTO asset_settings (item_id, pixel_art, grid_json, frame_ms, anim_tag, updated_at)
               VALUES (1, 0, '{"v":1,"cellW":16,"cellH":16,"offsetX":0,"offsetY":0,"gapX":0,"gapY":0,"count":6}',
                       120, 'walk', 5)"#,
            [],
        )
        .unwrap();
        let info = get(&conn, 1).unwrap();
        assert_eq!(info.settings.pixel_art, Some(false));
        assert_eq!(info.settings.grid, Some(grid(16, Some(6))));
        assert_eq!(info.settings.frame_ms, Some(120));
        assert_eq!(info.settings.anim_tag.as_deref(), Some("walk"));
        assert_eq!(info.settings.updated_at, Some(5));
        assert_eq!(
            info.effective,
            Effective {
                pixel_art: false,
                animated: true,
                frame_count: Some(6)
            }
        );

        // 音声：未解析（事実なし）
        let info = get(&conn, 2).unwrap();
        assert_eq!(info.analysis_state, AnalysisState::Pending);
        assert_eq!(info.facts, None);
        assert!(!info.effective.pixel_art);
        conn.execute(
            "INSERT INTO media_facts (blob_id, state, facts_v, loudness, loudness_range, true_peak, has_loop,
                                      loop_start, loop_end, sample_rate, channels, bits, analyzed_at)
             VALUES (?1, 'ok', 1, -14.25, 6.5, -1.0, 1, 100, 4000, 48000, 2, NULL, 7)",
            [blob_id(&conn, 2)],
        )
        .unwrap();
        let audio = get(&conn, 2).unwrap().facts.unwrap().audio;
        assert_eq!(audio.loudness, Some(-14.25));
        assert_eq!(audio.loop_range, Some((100, 4000)));
        assert_eq!(audio.has_loop, Some(true));
        assert_eq!(
            (audio.sample_rate, audio.channels, audio.bits),
            (Some(48_000), Some(2), None)
        );

        assert!(matches!(get(&conn, 99), Err(CoreError::NotFound(_))));
    }

    /// 解析に失敗した事実も返す（理由の欄）。壊れた grid_json は格子なしとして読む。
    #[test]
    fn get_reports_failed_facts_and_ignores_broken_grids() {
        let conn = test_conn();
        item(&conn, 1, "image", Some((8, 8)), None);
        conn.execute(
            "INSERT INTO media_facts (blob_id, state, error_code, facts_v, analyzed_at)
             VALUES (?1, 'failed', 'too_large', 1, 3)",
            [blob_id(&conn, 1)],
        )
        .unwrap();
        conn.execute(
            r#"INSERT INTO asset_settings (item_id, grid_json, updated_at) VALUES (1, '{"v":2,"cellW":1}', 1)"#,
            [],
        )
        .unwrap();
        let info = get(&conn, 1).unwrap();
        let facts = info.facts.unwrap();
        assert!(!facts.ok);
        assert_eq!(facts.error_code.as_deref(), Some("too_large"));
        assert_eq!(info.settings.grid, None);
        assert!(!info.effective.animated);
    }

    /// フォント・3D の情報は codec_info の `font`・`model` から読む（ない欄は None。知らない欄は無視する）。
    #[test]
    fn get_reads_font_and_model_info_from_codec_info() {
        let conn = test_conn();
        item(
            &conn,
            1,
            "font",
            None,
            Some(
                r#"{"codec":"otf","font":{"family":"Kura Sans","style":"Bold","glyphs":2340,"faces":1,"variable":false,"extra":1}}"#,
            ),
        );
        item(
            &conn,
            2,
            "model",
            None,
            Some(
                r#"{"codec":"glb","model":{"vertices":12480,"triangles":8320,"materials":3,"size":[1.2,0.8,0.45],"unsupported":["KHR_draco_mesh_compression"],"generator":"x"}}"#,
            ),
        );
        item(&conn, 3, "font", None, Some("壊れた JSON"));
        let font = get(&conn, 1).unwrap().font.unwrap();
        assert_eq!(font.family.as_deref(), Some("Kura Sans"));
        assert_eq!(font.style.as_deref(), Some("Bold"));
        assert_eq!(font.glyphs, Some(2340));
        assert_eq!(font.format.as_deref(), Some("otf"));
        let model = get(&conn, 2).unwrap().model.unwrap();
        assert_eq!(
            (model.vertices, model.triangles),
            (Some(12_480), Some(8_320))
        );
        assert_eq!(model.materials, Some(3));
        assert_eq!(model.size, Some([1.2, 0.8, 0.45]));
        assert_eq!(model.unsupported, ["KHR_draco_mesh_compression"]);
        // 壊れていても素材の欄は出す（情報なし）。形式は拡張子から
        let broken = get(&conn, 3).unwrap();
        assert_eq!(broken.font.map(|f| f.family), Some(None));
    }

    /// 書き手（取り込みの probe・サムネイルのジョブが `MediaInfo::codec_json` で書く codec_info）と読み手（素材の欄）が
    /// 同じ形であること。3D の大きさは `size`（範囲の max − min）で書かれる。
    #[test]
    fn font_and_model_written_by_media_info_are_read_back() {
        use crate::media::{FontInfo, MediaInfo, ModelInfo};
        let conn = test_conn();
        let font = MediaInfo {
            codec: Some("ttc".into()),
            font: Some(FontInfo {
                family: Some("Kura Mono".into()),
                style: Some("Italic".into()),
                full_name: Some("Kura Mono Italic".into()),
                postscript_name: Some("KuraMono-Italic".into()),
                glyphs: 512,
                faces: 3,
                variable: true,
            }),
            ..MediaInfo::default()
        };
        let model = MediaInfo {
            codec: Some("glb".into()),
            model: Some(ModelInfo {
                vertices: 24,
                triangles: 12,
                meshes: 1,
                materials: 2,
                textures: 1,
                size: Some([1.0, 0.5, 0.25]),
                unsupported: vec!["skin".into()],
                ..ModelInfo::default()
            }),
            ..MediaInfo::default()
        };
        item(&conn, 1, "font", None, font.codec_json().as_deref());
        item(&conn, 2, "model", None, model.codec_json().as_deref());
        assert_eq!(
            get(&conn, 1).unwrap().font,
            Some(FontFacts {
                family: Some("Kura Mono".into()),
                style: Some("Italic".into()),
                full_name: Some("Kura Mono Italic".into()),
                postscript_name: Some("KuraMono-Italic".into()),
                glyphs: Some(512),
                faces: Some(3),
                variable: Some(true),
                format: Some("ttc".into()),
            })
        );
        assert_eq!(
            get(&conn, 2).unwrap().model,
            Some(ModelFacts {
                vertices: Some(24),
                triangles: Some(12),
                materials: Some(2),
                textures: Some(1),
                meshes: Some(1),
                size: Some([1.0, 0.5, 0.25]),
                unsupported: vec!["skin".into()],
            })
        );
    }

    /// 右クリックの複数の選択：画像だけに付け、ほかの種類とない ID は飛ばす。すべて自動に戻れば行を消す。変えた数を返す。
    #[test]
    fn settings_apply_to_images_only_and_rows_go_away_when_all_auto() {
        let conn = test_conn();
        item(&conn, 1, "image", Some((64, 64)), None);
        item(&conn, 2, "image", None, None);
        item(&conn, 3, "audio", None, None);
        let yes = SettingsPatch {
            pixel_art: Some(Change::Set(true)),
            ..SettingsPatch::default()
        };
        assert_eq!(set_settings(&conn, &[1, 2, 3, 99, 1], &yes, 10).unwrap(), 2);
        assert_eq!(settings_rows(&conn), 2);
        assert_eq!(get(&conn, 1).unwrap().settings.pixel_art, Some(true));
        assert_eq!(get(&conn, 1).unwrap().settings.updated_at, Some(10));
        assert_eq!(get(&conn, 3).unwrap().settings, AssetSettings::default());
        // 同じ値にしても変わらない（数えない・時刻も変えない）
        assert_eq!(set_settings(&conn, &[1], &yes, 20).unwrap(), 0);
        assert_eq!(get(&conn, 1).unwrap().settings.updated_at, Some(10));
        // 格子とコマの長さ
        let sheet = SettingsPatch {
            grid: Some(Change::Set(grid(16, Some(12)))),
            frame_ms: Some(Change::Set(80)),
            ..SettingsPatch::default()
        };
        assert_eq!(set_settings(&conn, &[1], &sheet, 30).unwrap(), 1);
        let s = get(&conn, 1).unwrap().settings;
        assert_eq!(
            (s.grid, s.frame_ms, s.pixel_art),
            (Some(grid(16, Some(12))), Some(80), Some(true))
        );
        let stored: String = conn
            .query_row(
                "SELECT grid_json FROM asset_settings WHERE item_id = 1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            stored,
            r#"{"v":1,"cellW":16,"cellH":16,"offsetX":0,"offsetY":0,"gapX":0,"gapY":0,"count":12}"#
        );
        // 自動に戻す：残る欄があれば行は残り、すべて自動なら消える
        let auto = SettingsPatch {
            pixel_art: Some(Change::Clear),
            ..SettingsPatch::default()
        };
        assert_eq!(set_settings(&conn, &[1, 2], &auto, 40).unwrap(), 2);
        assert_eq!(settings_rows(&conn), 1, "2 の行は消え、1 は格子が残る");
        let clear = SettingsPatch {
            grid: Some(Change::Clear),
            frame_ms: Some(Change::Clear),
            ..SettingsPatch::default()
        };
        assert_eq!(set_settings(&conn, &[1], &clear, 50).unwrap(), 1);
        assert_eq!(settings_rows(&conn), 0);
    }

    /// 格子が合わない画像が 1 つでもあれば、どれも変えずに断る（理由に ID を入れる）。値の範囲も確かめる。
    #[test]
    fn settings_reject_bad_values_without_changing_anything() {
        let conn = test_conn();
        item(&conn, 1, "image", Some((64, 64)), None);
        item(&conn, 2, "image", Some((8, 8)), None);
        let sheet = SettingsPatch {
            pixel_art: Some(Change::Set(true)),
            grid: Some(Change::Set(grid(16, None))),
            ..SettingsPatch::default()
        };
        let err = set_settings(&conn, &[1, 2], &sheet, 1).unwrap_err();
        assert!(
            matches!(&err, CoreError::Invalid(m) if m.contains('2')),
            "{err}"
        );
        assert_eq!(settings_rows(&conn), 0);
        for bad in [
            SettingsPatch {
                pixel_scale: Some(Change::Set(0)),
                ..SettingsPatch::default()
            },
            SettingsPatch {
                pixel_scale: Some(Change::Set(65)),
                ..SettingsPatch::default()
            },
            SettingsPatch {
                frame_ms: Some(Change::Set(0)),
                ..SettingsPatch::default()
            },
            SettingsPatch {
                pivot: Some(Change::Set(Pivot {
                    x: f64::NAN,
                    y: 0.0,
                })),
                ..SettingsPatch::default()
            },
            SettingsPatch {
                anim_tag: Some(Change::Set(String::new())),
                ..SettingsPatch::default()
            },
            SettingsPatch {
                anim_tag: Some(Change::Set("x".repeat(257))),
                ..SettingsPatch::default()
            },
        ] {
            assert!(bad.validate().is_err(), "{bad:?}");
            assert!(set_settings(&conn, &[1], &bad, 1).is_err(), "{bad:?}");
        }
        assert_eq!(settings_rows(&conn), 0);
        let ok = SettingsPatch {
            pixel_scale: Some(Change::Set(64)),
            pivot: Some(Change::Set(Pivot { x: 7.5, y: -1.0 })),
            anim_tag: Some(Change::Set("走る".into())),
            ..SettingsPatch::default()
        };
        assert!(ok.validate().is_ok());
        assert_eq!(set_settings(&conn, &[1], &ok, 1).unwrap(), 1);
        let s = get(&conn, 1).unwrap().settings;
        assert_eq!(s.pixel_scale, Some(64));
        assert_eq!(s.pivot, Some(Pivot { x: 7.5, y: -1.0 }));
        assert_eq!(s.anim_tag.as_deref(), Some("走る"));
    }

    /// 素材の欄の問い合わせは主キーと台帳の索引で引く（全件を走査しない）。
    #[test]
    fn asset_query_uses_keys() {
        let conn = test_conn();
        let plan = query_plan(&conn, &GET_SQL.replace("?1", "1"));
        assert!(!plan.contains("SCAN"), "{plan}");
    }
}
