//! 素材の欄・アニメの帯（M11。docs/architecture.md §3。ロジックは kura-core の `asset`）。
//!
//! - `item_asset_get`：解析の事実・利用者の設定・実効値・フォントと 3D の情報（DB だけを読む）。
//! - `item_asset_settings_set`：右クリックの複数の選択にも使う（画像だけに付ける）。
//! - `item_anim_get`：見たものだけ、その場で帯の PNG を作る（ジョブにしない。決定 8）。帯は kura:// の `/anim/{hash}/1`
//!   （`rev` が 0 でなければ `?r={rev}` を付ける）で読む。大きすぎれば `TooLarge`、混んでいれば `Busy`。
//!
//! DTO はほかの班の dto.rs の変更とぶつからないよう、このファイルに置く（名前は `Asset`・`Anim` で始める）。

use kura_core::asset::anim::{AnimError, AnimInfo, AnimTag, TagDirection, request_of};
use kura_core::asset::{
    self, Alpha, AnalysisState, AssetInfo, AssetSettings, Change, GridSetting, Pivot, SettingsPatch,
};
use kura_core::time::now_ms;
use serde::{Deserialize, Serialize};
use specta::Type;
use tauri::State;

use super::blocking;
use crate::dto::ItemKindDto;
use crate::error::{AppError, AppErrorCode};
use crate::state::AppState;

// ===== 素材の欄 =====

/// 透過。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub enum AssetAlphaDto {
    /// 透明な画素がない。
    None,
    /// 透明か不透明だけ。
    Binary,
    /// 半透明がある。
    Partial,
}

/// 解析の進み具合（DB の `items.analysis_state` と同じ綴り）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum AssetAnalysisStateDto {
    Pending,
    Ready,
    Failed,
    NotNeeded,
}

/// フレームの半開区間 [start, end)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct AssetSpanDto {
    pub start: u64,
    pub end: u64,
}

/// 画像の事実（計っていないものは null）。
#[derive(Debug, Clone, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct AssetImageFactsDto {
    pub alpha: Option<AssetAlphaDto>,
    /// 1〜257（257 は 256 色を超える）。
    pub colors: Option<u32>,
    pub visible_colors: Option<u32>,
    pub pixel_kx: Option<u32>,
    pub pixel_ky: Option<u32>,
    pub pixel_lossless: Option<bool>,
    /// ドット絵の自動の判定。
    pub pixel_art: Option<bool>,
    pub pixel_rule: Option<u32>,
    pub frame_count: Option<u32>,
}

/// 音声の事実。
#[derive(Debug, Clone, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct AssetAudioFactsDto {
    /// 積分ラウドネス（LUFS）。計れなければ null で `loudnessReason`。
    pub loudness: Option<f64>,
    pub loudness_reason: Option<String>,
    /// LRA（LU）。
    pub loudness_range: Option<f64>,
    /// dBTP。
    pub true_peak: Option<f64>,
    pub sample_peak: Option<f64>,
    pub has_loop: Option<bool>,
    /// ループ点（解析のときの読み取り。試聴は /pcm の kura チャンクの値を使う）。
    pub loop_range: Option<AssetSpanDto>,
    pub sample_rate: Option<u32>,
    pub channels: Option<u32>,
    /// 非可逆の形式は null。
    pub bits: Option<u32>,
    pub clip_runs: Option<u64>,
}

/// 解析の事実（`media_facts`）。
#[derive(Debug, Clone, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct AssetFactsDto {
    /// 解析できたか（false なら `errorCode`：`unsupported`・`too_large`・`decode`）。
    pub ok: bool,
    pub error_code: Option<String>,
    /// 読んだデコーダ（形式だけで決めたときは null）。
    pub decoder: Option<String>,
    pub facts_version: i64,
    pub analyzed_at: i64,
    pub image: AssetImageFactsDto,
    pub audio: AssetAudioFactsDto,
}

/// スプライトシートの格子（`count` は使うコマの数。null はすべて）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct AssetGridDto {
    pub cell_w: u32,
    pub cell_h: u32,
    pub offset_x: u32,
    pub offset_y: u32,
    pub gap_x: u32,
    pub gap_y: u32,
    pub count: Option<u32>,
}

impl From<GridSetting> for AssetGridDto {
    fn from(g: GridSetting) -> Self {
        Self {
            cell_w: g.cell_w,
            cell_h: g.cell_h,
            offset_x: g.offset_x,
            offset_y: g.offset_y,
            gap_x: g.gap_x,
            gap_y: g.gap_y,
            count: g.count,
        }
    }
}

impl From<AssetGridDto> for GridSetting {
    fn from(g: AssetGridDto) -> Self {
        Self {
            cell_w: g.cell_w,
            cell_h: g.cell_h,
            offset_x: g.offset_x,
            offset_y: g.offset_y,
            gap_x: g.gap_x,
            gap_y: g.gap_y,
            count: g.count,
        }
    }
}

/// ピボット（画素の座標）。
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct AssetPivotDto {
    pub x: f64,
    pub y: f64,
}

/// 利用者の設定（null は自動・設定なし）。
#[derive(Debug, Clone, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct AssetSettingsDto {
    /// ドット絵として表示（null は自動）。
    pub pixel_art: Option<bool>,
    pub pixel_scale: Option<u32>,
    pub grid: Option<AssetGridDto>,
    /// 格子のアニメのコマの長さ（ms）。
    pub frame_ms: Option<u32>,
    pub pivot: Option<AssetPivotDto>,
    /// 試聴のループ（M13 で書く）。
    pub loop_range: Option<AssetSpanDto>,
    pub anim_tag: Option<String>,
    pub updated_at: Option<i64>,
}

/// 実効値（表示と検索の値）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct AssetEffectiveDto {
    /// ドット絵として表示するか（設定 ?? 自動の判定 ?? 偽）。
    pub pixel_art: bool,
    /// アニメとして回せるか（コマが 2 つ以上か、格子がある）。
    pub animated: bool,
    /// コマの数（格子があれば帯で使う数）。
    pub frame_count: Option<u32>,
}

/// フォントの情報。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct AssetFontDto {
    pub family: Option<String>,
    pub style: Option<String>,
    pub full_name: Option<String>,
    pub postscript_name: Option<String>,
    pub glyphs: Option<u32>,
    pub faces: Option<u32>,
    pub variable: Option<bool>,
    /// 形式（`ttf`・`otf`・`ttc`・`woff`・`woff2`）。
    pub format: Option<String>,
}

/// 3D モデルの情報。
#[derive(Debug, Clone, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct AssetModelDto {
    pub vertices: Option<u64>,
    pub triangles: Option<u64>,
    pub materials: Option<u32>,
    pub textures: Option<u32>,
    pub meshes: Option<u32>,
    /// 大きさ（範囲の max − min。x・y・z の順。glb の単位はメートル）。
    pub size: Option<[f64; 3]>,
    /// 描けない理由（必須の Draco・meshopt・知らない拡張の名前、`skin`）。空なら描ける。
    pub unsupported: Vec<String>,
}

/// 素材の欄の中身。
#[derive(Debug, Clone, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct AssetDto {
    pub item_id: i64,
    pub hash: String,
    pub kind: ItemKindDto,
    pub ext: String,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub analysis_state: AssetAnalysisStateDto,
    /// 解析の事実（まだなければ null）。
    pub facts: Option<AssetFactsDto>,
    pub settings: AssetSettingsDto,
    pub effective: AssetEffectiveDto,
    /// フォントのときだけ。
    pub font: Option<AssetFontDto>,
    /// 3D のときだけ。
    pub model: Option<AssetModelDto>,
}

fn span((start, end): (u64, u64)) -> AssetSpanDto {
    AssetSpanDto { start, end }
}

fn settings_dto(s: AssetSettings) -> AssetSettingsDto {
    AssetSettingsDto {
        pixel_art: s.pixel_art,
        pixel_scale: s.pixel_scale,
        grid: s.grid.map(Into::into),
        frame_ms: s.frame_ms,
        pivot: s.pivot.map(|p| AssetPivotDto { x: p.x, y: p.y }),
        loop_range: s.loop_range.map(span),
        anim_tag: s.anim_tag,
        updated_at: s.updated_at,
    }
}

impl From<AssetInfo> for AssetDto {
    fn from(a: AssetInfo) -> Self {
        Self {
            item_id: a.item_id,
            hash: a.hash,
            kind: a.kind.into(),
            ext: a.ext,
            width: a.width,
            height: a.height,
            analysis_state: match a.analysis_state {
                AnalysisState::Pending => AssetAnalysisStateDto::Pending,
                AnalysisState::Ready => AssetAnalysisStateDto::Ready,
                AnalysisState::Failed => AssetAnalysisStateDto::Failed,
                AnalysisState::NotNeeded => AssetAnalysisStateDto::NotNeeded,
            },
            facts: a.facts.map(|f| AssetFactsDto {
                ok: f.ok,
                error_code: f.error_code,
                decoder: f.decoder,
                facts_version: f.facts_v,
                analyzed_at: f.analyzed_at,
                image: AssetImageFactsDto {
                    alpha: f.image.alpha.map(|a| match a {
                        Alpha::None => AssetAlphaDto::None,
                        Alpha::Binary => AssetAlphaDto::Binary,
                        Alpha::Partial => AssetAlphaDto::Partial,
                    }),
                    colors: f.image.colors,
                    visible_colors: f.image.visible_colors,
                    pixel_kx: f.image.pixel_kx,
                    pixel_ky: f.image.pixel_ky,
                    pixel_lossless: f.image.pixel_lossless,
                    pixel_art: f.image.pixel_art,
                    pixel_rule: f.image.pixel_rule,
                    frame_count: f.image.frame_count,
                },
                audio: AssetAudioFactsDto {
                    loudness: f.audio.loudness,
                    loudness_reason: f.audio.loudness_reason,
                    loudness_range: f.audio.loudness_range,
                    true_peak: f.audio.true_peak,
                    sample_peak: f.audio.sample_peak,
                    has_loop: f.audio.has_loop,
                    loop_range: f.audio.loop_range.map(span),
                    sample_rate: f.audio.sample_rate,
                    channels: f.audio.channels,
                    bits: f.audio.bits,
                    clip_runs: f.audio.clip_runs,
                },
            }),
            settings: settings_dto(a.settings),
            effective: AssetEffectiveDto {
                pixel_art: a.effective.pixel_art,
                animated: a.effective.animated,
                frame_count: a.effective.frame_count,
            },
            font: a.font.map(|f| AssetFontDto {
                family: f.family,
                style: f.style,
                full_name: f.full_name,
                postscript_name: f.postscript_name,
                glyphs: f.glyphs,
                faces: f.faces,
                variable: f.variable,
                format: f.format,
            }),
            model: a.model.map(|m| AssetModelDto {
                vertices: m.vertices,
                triangles: m.triangles,
                materials: m.materials,
                textures: m.textures,
                meshes: m.meshes,
                size: m.size,
                unsupported: m.unsupported,
            }),
        }
    }
}

/// 「ドット絵として表示」の 3 択。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub enum AssetChoiceDto {
    Auto,
    Yes,
    No,
}

/// 格子の変え方（`"clear"` か `{ set: … }`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub enum AssetGridChangeDto {
    Clear,
    Set(AssetGridDto),
}

/// ピボットの変え方（`"clear"` か `{ set: … }`）。
#[derive(Debug, Clone, Copy, PartialEq, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub enum AssetPivotChangeDto {
    Clear,
    Set(AssetPivotDto),
}

/// 素材の設定の変更（省いた項目は変えない）。数の 0 と文字の空は「自動に戻す」。ループ点は M13 で書くのでここにはない。
#[derive(Debug, Clone, Default, PartialEq, Deserialize, Type)]
#[serde(rename_all = "camelCase", default)]
pub struct AssetSettingsPatchDto {
    /// ドット絵として表示（自動・はい・いいえ）。
    pub pixel_art: Option<AssetChoiceDto>,
    /// 倍率（1〜64。0 は自動）。
    pub pixel_scale: Option<u32>,
    pub grid: Option<AssetGridChangeDto>,
    /// 格子のアニメのコマの長さ（ms。0 は自動 = ファイルの長さか 100ms）。
    pub frame_ms: Option<u32>,
    pub pivot: Option<AssetPivotChangeDto>,
    /// 再生するタグ（空文字は自動）。
    pub anim_tag: Option<String>,
}

impl From<AssetSettingsPatchDto> for SettingsPatch {
    fn from(p: AssetSettingsPatchDto) -> Self {
        let number = |v: u32| {
            if v == 0 {
                Change::Clear
            } else {
                Change::Set(v)
            }
        };
        Self {
            pixel_art: p.pixel_art.map(|c| match c {
                AssetChoiceDto::Auto => Change::Clear,
                AssetChoiceDto::Yes => Change::Set(true),
                AssetChoiceDto::No => Change::Set(false),
            }),
            pixel_scale: p.pixel_scale.map(number),
            grid: p.grid.map(|g| match g {
                AssetGridChangeDto::Clear => Change::Clear,
                AssetGridChangeDto::Set(g) => Change::Set(g.into()),
            }),
            frame_ms: p.frame_ms.map(number),
            pivot: p.pivot.map(|v| match v {
                AssetPivotChangeDto::Clear => Change::Clear,
                AssetPivotChangeDto::Set(p) => Change::Set(Pivot { x: p.x, y: p.y }),
            }),
            anim_tag: p.anim_tag.map(|t| {
                if t.is_empty() {
                    Change::Clear
                } else {
                    Change::Set(t)
                }
            }),
        }
    }
}

/// 素材の欄の中身（DB だけを読む）。
#[tauri::command]
#[specta::specta]
pub async fn item_asset_get(
    state: State<'_, AppState>,
    item_id: i64,
) -> Result<AssetDto, AppError> {
    let session = state.session()?;
    blocking(move || {
        Ok(session
            .library()
            .db()
            .read(|conn| asset::get(conn, item_id))?
            .into())
    })
    .await
}

/// 素材の設定を変える（右クリックの複数の選択にも使う。画像だけに付け、ほかは飛ばす）。変えた数を返す。格子が合わない画像が
/// あれば、どれも変えずに `Invalid`。
#[tauri::command]
#[specta::specta]
pub async fn item_asset_settings_set(
    state: State<'_, AppState>,
    ids: Vec<i64>,
    patch: AssetSettingsPatchDto,
) -> Result<u32, AppError> {
    let session = state.session()?;
    let patch = SettingsPatch::from(patch);
    patch.validate()?;
    blocking(move || {
        Ok(session
            .library()
            .db()
            .write(move |tx| asset::set_settings(tx, &ids, &patch, now_ms()))?)
    })
    .await
}

// ===== アニメの帯 =====

/// タグの再生の向き（`src/lib/asset/timeline.ts` の `Direction`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub enum AnimDirectionDto {
    Forward,
    Reverse,
    PingPong,
    PingPongReverse,
}

/// アニメのタグ（`timeline.ts` の `AnimTag`。`from`・`to` は両端を含む。`repeat` の 0 は無限）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct AnimTagDto {
    pub name: String,
    pub from: u32,
    pub to: u32,
    pub direction: AnimDirectionDto,
    pub repeat: u32,
}

impl From<AnimTag> for AnimTagDto {
    fn from(t: AnimTag) -> Self {
        Self {
            name: t.name,
            from: t.from,
            to: t.to,
            direction: match t.direction {
                TagDirection::Forward => AnimDirectionDto::Forward,
                TagDirection::Reverse => AnimDirectionDto::Reverse,
                TagDirection::PingPong => AnimDirectionDto::PingPong,
                TagDirection::PingPongReverse => AnimDirectionDto::PingPongReverse,
            },
            repeat: t.repeat,
        }
    }
}

/// アニメの帯の情報。帯は `/anim/{hash}/{version}`（`rev` が 0 でなければ `?r={rev}`）。帯の中のコマ f の左上は
/// `((f % cols) × canvasW, ⌊f / cols⌋ × canvasH)`。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct AnimDto {
    pub hash: String,
    /// 帯の並べ方の版（今は 1）。
    pub version: u32,
    /// 格子の帯なら 1 以上（格子を変えると変わる）、ファイルのコマなら 0。
    pub rev: u32,
    pub canvas_w: u32,
    pub canvas_h: u32,
    pub cols: u32,
    pub frame_count: u32,
    /// コマの長さ（ms。0 はそのまま。フロントが 100ms にする）。
    pub durations: Vec<u32>,
    pub tags: Vec<AnimTagDto>,
}

impl From<AnimInfo> for AnimDto {
    fn from(a: AnimInfo) -> Self {
        Self {
            hash: a.hash,
            version: a.version,
            rev: a.rev,
            canvas_w: a.canvas_w,
            canvas_h: a.canvas_h,
            cols: a.cols,
            frame_count: a.frame_count,
            durations: a.durations,
            tags: a.tags.into_iter().map(Into::into).collect(),
        }
    }
}

impl From<AnimError> for AppError {
    fn from(err: AnimError) -> Self {
        let code = match &err {
            AnimError::NotFound(_) => AppErrorCode::NotFound,
            AnimError::NotAnimated(_) | AnimError::Failed(_) => AppErrorCode::Invalid,
            AnimError::TooLarge(_) => AppErrorCode::TooLarge,
            AnimError::Busy(_) => AppErrorCode::Busy,
            AnimError::Unavailable(_) => AppErrorCode::Internal,
            AnimError::Core(_) => AppErrorCode::Internal,
        };
        match err {
            AnimError::Core(core) => core.into(),
            other => Self::new(code, other.to_string()),
        }
    }
}

/// アニメの帯の情報。帯がまだなければ、その場で作って `anim/ab/<hash>-1.png` に書く（長くても 0.4 秒ほど。ジョブにしない）。
#[tauri::command]
#[specta::specta]
pub async fn item_anim_get(state: State<'_, AppState>, item_id: i64) -> Result<AnimDto, AppError> {
    let session = state.session()?;
    blocking(move || {
        let engine = session
            .engine()
            .cloned()
            .ok_or_else(|| AppError::internal("このライブラリではエンジンを使えません"))?;
        let req = session
            .library()
            .db()
            .read(|conn| request_of(conn, item_id))?;
        Ok(kura_core::asset::anim::get(&engine, session.library().layout(), &req)?.into())
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 変更の DTO：0 と空文字は自動に戻す。格子・ピボットは `"clear"` か `{ set }`。省いた項目は変えない。
    #[test]
    fn patch_dto_maps_zero_and_empty_to_auto() {
        let p: AssetSettingsPatchDto = serde_json::from_str(
            r#"{"pixelArt":"auto","pixelScale":0,"grid":"clear","frameMs":120,"animTag":""}"#,
        )
        .unwrap();
        let p = SettingsPatch::from(p);
        assert_eq!(p.pixel_art, Some(Change::Clear));
        assert_eq!(p.pixel_scale, Some(Change::Clear));
        assert_eq!(p.grid, Some(Change::Clear));
        assert_eq!(p.frame_ms, Some(Change::Set(120)));
        assert_eq!(p.anim_tag, Some(Change::Clear));
        assert_eq!(p.pivot, None);
        let p: AssetSettingsPatchDto = serde_json::from_str(
            r#"{"pixelArt":"yes","grid":{"set":{"cellW":16,"cellH":8,"offsetX":0,"offsetY":1,"gapX":2,"gapY":0,"count":null}},"pivot":{"set":{"x":1.5,"y":2}}}"#,
        )
        .unwrap();
        let p = SettingsPatch::from(p);
        assert_eq!(p.pixel_art, Some(Change::Set(true)));
        assert_eq!(
            p.grid,
            Some(Change::Set(GridSetting {
                cell_w: 16,
                cell_h: 8,
                offset_x: 0,
                offset_y: 1,
                gap_x: 2,
                gap_y: 0,
                count: None
            }))
        );
        assert_eq!(p.pivot, Some(Change::Set(Pivot { x: 1.5, y: 2.0 })));
        assert_eq!(
            SettingsPatch::from(AssetSettingsPatchDto::default()),
            SettingsPatch::default()
        );
    }

    /// 帯の情報とタグの向きは timeline.ts と同じ綴り。帯の失敗は画面で分ける code にする。
    #[test]
    fn anim_dto_and_errors_serialize_for_the_front() {
        let dto = AnimDto::from(AnimInfo {
            hash: "ab".repeat(32),
            version: 1,
            rev: 0,
            canvas_w: 16,
            canvas_h: 8,
            cols: 4,
            frame_count: 6,
            durations: vec![0, 100],
            tags: vec![AnimTag {
                name: "走る".into(),
                from: 1,
                to: 3,
                direction: TagDirection::PingPongReverse,
                repeat: 0,
            }],
        });
        let json = serde_json::to_value(&dto).unwrap();
        assert_eq!(json["canvasW"], 16);
        assert_eq!(json["frameCount"], 6);
        assert_eq!(json["tags"][0]["direction"], "pingPongReverse");
        for (err, code) in [
            (AnimError::TooLarge("x".into()), AppErrorCode::TooLarge),
            (AnimError::Busy("x".into()), AppErrorCode::Busy),
            (AnimError::NotAnimated("x".into()), AppErrorCode::Invalid),
            (AnimError::NotFound("x".into()), AppErrorCode::NotFound),
            (
                AnimError::Core(kura_core::CoreError::NotFound("x".into())),
                AppErrorCode::NotFound,
            ),
        ] {
            assert_eq!(AppError::from(err).code, code);
        }
    }
}
