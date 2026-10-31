//! 書ける形式の一覧（M13 段 4C。IPC の `edit_formats`。docs/editing.md §14）：保存が `FormatUnavailable` で断られたときに、
//! フロントが形式を選ぶシート（`EditorSheet` の `format`）に使う。
//!
//! - 並びはエンジンの形式の表（`codec/formats.rs` の `FORMATS`）の順で、アイテムの種類（今の出力の形が音声なら音声、ほかは
//!   画像）の行すべて。書けない形式も `available: false` で並べる（MP3 は ffmpeg がなければ書けない、など）。どの形式を
//!   選択肢に出すかはフロントの形式の表が決める。
//! - `available` は `edit::save::writable_formats`（その形式の書き手が今の出力の形を書けるか）。保存できるもの（可逆・ループ
//!   点・マーカー・高いビット）は形式の表の `MetaCaps`。
//! - `fallback` は、今の出力の形式に書けないときに保存が使う代わりの既定（`edit::save::save_format` と同じ：音声は Ogg Vorbis、
//!   書けなければ WAV、画像は PNG）。今の出力の形式に書ければ None。
//! - 確かめは保存のジョブと同じ今のコーデックの組（`EngineServices::codecs`。ツールの場所を変えたら作り直した組）で行う
//!   （エディタを開いたときの組ではない）。出力は確定した頭の文書（保存するもの）。

use kura_engine::codec::CodecSet;
use kura_engine::codec::formats::{FORMATS, MetaCaps};
use kura_engine::doc::FormatChoice;
use kura_engine::{EngineError, Shape};

use super::{EditorEnv, EditorError, editor_of};
use crate::edit::save::{FormatProblem, save_format, writable_formats};
use crate::edit::versions;
use crate::import::{MediaKind, format_id};

/// 形式 1 つ（形式の表の行）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WritableChoice {
    /// 形式の ID（`FormatChoice::To` の `format`）。
    pub format: String,
    /// 代表の拡張子。
    pub ext: String,
    /// 今のコーデックの組で、今の出力の形を書けるか。
    pub available: bool,
    /// 形式の表の保存できるもの。
    pub caps: MetaCaps,
}

/// 書ける形式の一覧と、代わりの既定。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditFormats {
    pub formats: Vec<WritableChoice>,
    /// 今の出力の形式に書けないときに保存が使う形式（書ければ None）。
    pub fallback: Option<String>,
}

/// 開いているエディタの確定した頭の出力（形式の選びと形）で、書ける形式の一覧を作る（モジュールの説明）。
pub fn formats(env: &EditorEnv<'_>, session: &str) -> Result<EditFormats, EditorError> {
    let editor = editor_of(env, session)?;
    // 組は状態の錠の外で取る（最初に使うときは ffmpeg を探して問い合わせる）
    let codecs = env.engine.codecs();
    let source = env
        .library
        .db()
        .read(|c| versions::original(c, editor.item_id))?
        .and_then(|o| format_id(&o.ext))
        .ok_or_else(|| EditorError::Failed("元の形式を読めません".into()))?;
    let (choice, shape) = {
        let guard = editor.lock_state();
        let st = guard.as_ref().ok_or(EditorError::NoSession)?;
        let graph = st.session.head().to_graph();
        let choice = graph
            .outputs
            .first()
            .map(|o| o.format.clone())
            .ok_or_else(|| EditorError::Failed("文書に出力がありません".into()))?;
        let plan = editor
            .runtime
            .preview(|e| e.compile(&graph, &st.shapes))
            .map_err(|issues| {
                EditorError::from_engine("文書を描けません", EngineError::Invalid(issues))
            })?;
        let shape = plan
            .output_shapes()
            .first()
            .map(|(_, s)| *s)
            .ok_or_else(|| EditorError::Failed("文書に出力がありません".into()))?;
        (choice, shape)
    };
    Ok(formats_for(&codecs, &choice, source, &shape))
}

/// 組 `codecs` で、出力の形式の選び `choice`（`Keep` は元の形式 `source_format`）・形 `shape` の書ける形式の一覧（純粋な関数）。
pub fn formats_for(
    codecs: &CodecSet,
    choice: &FormatChoice,
    source_format: &str,
    shape: &Shape,
) -> EditFormats {
    let kind = match shape {
        Shape::Audio { .. } => MediaKind::Audio,
        Shape::Image { .. } | Shape::Frames { .. } => MediaKind::Image,
    };
    let writable: Vec<String> = writable_formats(codecs, kind, shape)
        .into_iter()
        .map(|w| w.format)
        .collect();
    let formats = FORMATS
        .iter()
        .filter(|f| MediaKind::from(f.item_kind) == kind && !f.decodes_to.is_empty())
        .map(|f| WritableChoice {
            format: f.id.to_string(),
            ext: f.exts.first().copied().unwrap_or(f.id).to_string(),
            available: writable.iter().any(|w| w == f.id),
            caps: f.caps,
        })
        .collect();
    let fallback = match save_format(codecs, choice, source_format, shape) {
        Ok(_) => None,
        Err(FormatProblem::Unavailable { fallback, .. }) => fallback,
    };
    EditFormats { formats, fallback }
}

#[cfg(test)]
mod tests {
    use kura_engine::Dim;
    use serde_json::json;

    use super::*;
    use crate::codec_ext::ffmpeg::testing::FakeTools;

    fn stereo() -> Shape {
        Shape::Audio {
            rate: 44_100,
            channels: 2,
            frames: Dim::Exact(1_000),
        }
    }

    fn row<'a>(f: &'a EditFormats, id: &str) -> &'a WritableChoice {
        f.formats
            .iter()
            .find(|w| w.format == id)
            .unwrap_or_else(|| panic!("{id} がない"))
    }

    fn to(format: &str) -> FormatChoice {
        FormatChoice::To {
            format: format.into(),
            options: json!({}),
        }
    }

    /// A17：ffmpeg がない組では、mp3 の元を元の形式のまま保存できない（mp3 は `available: false`、代わりは ogg）。並びは
    /// 形式の表の音声の行。保存できるものは形式の表のもの。書ける組（偽の ffmpeg）なら mp3 も書けて、代わりは要らない。
    #[test]
    fn mp3_without_ffmpeg_is_unavailable_and_falls_back_to_ogg() {
        let pinned = CodecSet::pinned();
        let f = formats_for(&pinned, &FormatChoice::Keep, "mp3", &stereo());
        let ids: Vec<&str> = f.formats.iter().map(|w| w.format.as_str()).collect();
        assert_eq!(
            ids,
            ["wav", "aiff", "flac", "mp3", "m4a", "aac", "ogg", "opus"]
        );
        assert!(!row(&f, "mp3").available);
        assert_eq!(f.fallback.as_deref(), Some("ogg"));
        for id in ["wav", "aiff", "flac", "ogg"] {
            assert!(row(&f, id).available, "{id}");
        }
        for id in ["m4a", "aac", "opus"] {
            assert!(!row(&f, id).available, "{id}");
        }
        let wav = row(&f, "wav");
        assert_eq!(wav.ext, "wav");
        assert!(wav.caps.lossless && wav.caps.loop_points && wav.caps.markers);
        assert!(wav.caps.high_bit_depth);
        assert_eq!(row(&f, "aiff").ext, "aif", "代表の拡張子");
        let ogg = row(&f, "ogg").caps;
        assert!(!ogg.lossless && ogg.loop_points && !ogg.markers);
        // 書ける形式を選んでいれば代わりは要らない
        assert_eq!(
            formats_for(&pinned, &to("flac"), "mp3", &stereo()).fallback,
            None
        );

        let tools = FakeTools::new("exit 1", "exit 1", "cksum > \"$out\"");
        let mut with = CodecSet::pinned();
        tools.codecs().add_to(&mut with).unwrap();
        let f = formats_for(&with, &FormatChoice::Keep, "mp3", &stereo());
        assert!(row(&f, "mp3").available);
        assert_eq!(f.fallback, None);
    }

    /// 代わりの既定は保存の確かめ（`save_format`）と同じ：Ogg Vorbis に書けない形（9 チャンネル）は WAV、画像は PNG。
    #[test]
    fn fallback_follows_the_save_check() {
        let pinned = CodecSet::pinned();
        let nine = Shape::Audio {
            rate: 44_100,
            channels: 9,
            frames: Dim::Exact(1_000),
        };
        let f = formats_for(&pinned, &FormatChoice::Keep, "mp3", &nine);
        assert!(!row(&f, "ogg").available);
        assert_eq!(f.fallback.as_deref(), Some("wav"));
        for (choice, shape) in [(FormatChoice::Keep, &stereo()), (to("opus"), &nine)] {
            let want = match save_format(&pinned, &choice, "mp3", shape) {
                Ok(_) => None,
                Err(FormatProblem::Unavailable { fallback, .. }) => fallback,
            };
            assert_eq!(formats_for(&pinned, &choice, "mp3", shape).fallback, want);
        }

        let image = Shape::Image {
            w: Dim::Exact(8),
            h: Dim::Exact(8),
            fmt: kura_engine::value::PixelFormat::Rgba8,
        };
        let f = formats_for(&pinned, &to("heic"), "png", &image);
        assert_eq!(f.formats[0].format, "png");
        assert!(f.formats.iter().all(|w| w.format != "wav"), "画像の行だけ");
        assert!(row(&f, "png").available);
        assert!(!row(&f, "heic").available);
        assert_eq!(f.fallback.as_deref(), Some("png"));
        assert_eq!(
            formats_for(&pinned, &FormatChoice::Keep, "png", &image).fallback,
            None
        );
    }
}
