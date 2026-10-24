//! 編集のエンジンを手で確かめる（docs/editing.md）。
//!
//! - `edit shape --in <file>`：形式と形（ヘッダだけ）を表示する
//! - `edit caps`：今の環境で読める・書ける形式の表（固定の段・macOS・ffmpeg。docs/editing.md §9.3）を JSON で表示する
//! - `edit render --doc <文書.json> --in <file> --out <file> [--format <形式>] [--options <JSON>]`：
//!   文書を当てて書き出し、値のダイジェスト・読み取りと書き出しの記録・警告を表示する
//! - `edit crash-loop --library <新しい場所> [--runs 50] [--seed 1] [--min-ms 10] [--max-ms 500]`：保存を繰り返す子を
//!   SIGKILL しては開き直して確かめる（M12。`crate::crash_loop`。子は `edit crash-loop-child`）
//!
//! コーデックはアプリと同じ組（`crate::app_codecs`：固定の段 → macOS → ffmpeg。SVG は別のプロセスで読む）で選ぶ。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use kura_engine::codec::{CodecSet, format_by_ext};
use kura_engine::doc::{DecodeOptions, InputId};
use kura_engine::render::{Engine, InputValue};
use kura_engine::{Digest, NoMonitor};

const USAGE: &str = "使い方: kura-devtools edit shape --in <file> | edit caps | edit render --doc <文書.json> --in <file> --out <file> [--format <形式>] [--options <JSON>] | edit crash-loop --library <新しい場所> [--runs 50] [--seed 1]";

/// `edit` の入口。
pub fn run(args: &[String]) -> Result<(), String> {
    match args.first().map(String::as_str) {
        Some("shape") => shape(&args[1..]),
        Some("render") => render(&args[1..]),
        Some("caps") => caps(),
        Some("crash-loop") => crate::crash_loop::run(&args[1..]),
        Some("crash-loop-child") => crate::crash_loop::child(&args[1..]),
        _ => Err(USAGE.to_string()),
    }
}

fn opt(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1).cloned())
}

fn format_of(path: &Path) -> Result<&'static str, String> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default();
    format_by_ext(ext)
        .map(|f| f.id)
        .ok_or_else(|| format!("{} の形式が分かりません", path.display()))
}

/// アプリと同じコーデックの組（SVG の文字はシステムのフォントで描き、SVG は別のプロセスで読む。`crate::app_codecs`）。
/// ffmpeg の一時ファイルは OS の一時フォルダの下に置く。
fn codecs() -> CodecSet {
    crate::app_codecs(&std::env::temp_dir().join("kura-devtools-codec"))
}

fn caps() -> Result<(), String> {
    let table = codecs().capabilities();
    let text = serde_json::to_string_pretty(&table).map_err(|e| e.to_string())?;
    use std::io::Write as _;
    let mut out = std::io::stdout().lock();
    out.write_all(text.as_bytes())
        .and_then(|()| out.write_all(b"\n"))
        .map_err(|e| e.to_string())
}

fn shape(args: &[String]) -> Result<(), String> {
    let input = PathBuf::from(opt(args, "--in").ok_or(USAGE)?);
    let format = format_of(&input)?;
    let engine = Engine::new().map_err(|e| e.to_string())?;
    let (shape, codec) = codecs()
        .shape_path(&input, format, &DecodeOptions::default(), engine.limits())
        .map_err(|e| e.to_string())?;
    tracing::info!(format, codec, ?shape, "形");
    Ok(())
}

fn render(args: &[String]) -> Result<(), String> {
    let doc = PathBuf::from(opt(args, "--doc").ok_or(USAGE)?);
    let input = PathBuf::from(opt(args, "--in").ok_or(USAGE)?);
    let output = PathBuf::from(opt(args, "--out").ok_or(USAGE)?);
    let in_format = format_of(&input)?;
    let out_format = match opt(args, "--format") {
        Some(f) => kura_engine::codec::format_by_id(&f)
            .map(|d| d.id)
            .ok_or(format!("形式 {f} を知りません"))?,
        None => format_of(&output)?,
    };
    let options: serde_json::Value = match opt(args, "--options") {
        Some(text) => {
            serde_json::from_str(&text).map_err(|e| format!("--options が読めません: {e}"))?
        }
        None => serde_json::json!({}),
    };
    let engine = Engine::new().map_err(|e| e.to_string())?;
    let codecs = codecs();
    let graph = engine
        .load(&std::fs::read(&doc).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    let bytes = std::fs::read(&input).map_err(|e| e.to_string())?;
    let (value, decoded) = codecs
        .decode(
            &mut std::io::Cursor::new(&bytes),
            in_format,
            &DecodeOptions::default(),
            engine.limits(),
            &NoMonitor,
        )
        .map_err(|e| e.to_string())?;
    let shapes = BTreeMap::from([(InputId(0), value.shape())]);
    let plan = engine.compile(&graph, &shapes).map_err(|e| e.to_string())?;
    let source = Digest(*blake3::hash(&bytes).as_bytes());
    let rendered = engine
        .render(
            &plan,
            &BTreeMap::from([(InputId(0), InputValue { value, source })]),
            &NoMonitor,
        )
        .map_err(|e| e.to_string())?;
    let out = rendered.outputs.first().ok_or("出力がありません")?;
    let (encoded, stamp) = codecs
        .encode_bytes(&out.value, out_format, &options, &NoMonitor)
        .map_err(|e| e.to_string())?;
    std::fs::write(&output, &encoded).map_err(|e| e.to_string())?;
    tracing::info!(codec = decoded.codec, value_digest = %decoded.value_digest, "読み取り");
    tracing::info!(digest = %out.digest, shape = ?out.value.shape(), "描画");
    tracing::info!(codec = stamp.codec, bytes = stamp.bytes, file_digest = %stamp.file_digest, path = %output.display(), "書き出し");
    for w in &rendered.warnings {
        tracing::warn!(node = ?w.node, code = ?w.code, "{}", w.message);
    }
    Ok(())
}
