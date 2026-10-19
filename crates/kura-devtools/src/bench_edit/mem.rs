//! NFR-17（編集のメモリが予算を超えない。超える入力は落ちずにエラー）：自分を子プロセスとして `/usr/bin/time -l` で起こし
//! （`bench edit --child mem:<ケース>`）、peak memory footprint から基準（実行の場を作るだけの子。保存のケースはアプリの
//! コーデックの組も作る子。文書・セッションの履歴は実測の側に入る）を引いた量を、予算と見積もり（保存は `admit_bulk` の
//! 見積もり、エディタは描く間の確保の最大）と比べる。unsafe もアロケータの差し替えも使わない（mem_smoke.rs と同じ測り方。
//! あちらは小さな入力で見積もりの倍率を CI で見張る）。
//!
//! ケース：`save`（保存の通し）・`editor`（筆の文書のエディタ）は既定の予算で収まること、`save-limit`・`editor-limit` は
//! 小さい予算で落ちずに Limit になること。子は結果を 1 行の JSON で標準出力に書く。

use std::path::{Path, PathBuf};
use std::process::Command;

use kura_engine::session::{CommitMode, Edit};
use kura_engine::testgen::InputGen;
use kura_engine::testing::bench::cases::{
    flat_canvas, paint_doc, prior_strokes, save_doc, stroke_item,
};
use kura_engine::testing::bench::path::Step;
use kura_engine::{Digest, EngineError};
use serde_json::{Value as J, json};

use super::editor::Editor;
use super::report::{Record, Verdict, emit};
use super::{Ctx, MIB, Opts, Photo, Sizes, load1, runtime};

/// 子の結果。
#[derive(Debug, Clone)]
struct ChildResult {
    /// `ok`・`limit`・`error`・`base`。
    outcome: String,
    /// 見積もり（バイト）。
    estimate: u64,
    message: String,
}

/// `/usr/bin/time -l` の数（バイト）。
#[derive(Debug, Clone, Copy)]
struct Peak {
    footprint: u64,
    rss: Option<u64>,
}

/// `/usr/bin/time -l` の出力の行 `<数> <名前>` を読む。
fn read_count(stderr: &str, name: &str) -> Option<u64> {
    stderr
        .lines()
        .find(|l| l.contains(name))
        .and_then(|l| l.split_whitespace().next())
        .and_then(|n| n.parse().ok())
}

/// 子を起こして結果とピークを読む。`/usr/bin/time` が使えない・peak memory footprint を出さないなら、ピークは None。
fn spawn(
    exe: &Path,
    opts: &Opts,
    case: &str,
    budget: u64,
    photo: &Photo,
) -> Result<(Option<ChildResult>, Option<Peak>, bool, String), String> {
    let mut cmd = Command::new("/usr/bin/time");
    cmd.arg("-l")
        .arg(exe)
        .args(["bench", "edit", "--child"])
        .arg(format!("mem:{case}"))
        .arg("--budget")
        .arg(budget.to_string())
        .arg("--preview-threads")
        .arg(opts.preview_threads.to_string())
        .arg("--bulk-threads")
        .arg(opts.bulk_threads.to_string())
        .arg("--in")
        .arg(&photo.path)
        .arg("--format")
        .arg(photo.format);
    if opts.quick {
        cmd.arg("--quick");
    }
    let out = match cmd.output() {
        Ok(out) => out,
        Err(e) => {
            return Ok((
                None,
                None,
                false,
                format!("/usr/bin/time を起こせません: {e}"),
            ));
        }
    };
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let result = stdout
        .lines()
        .rev()
        .find_map(|l| serde_json::from_str::<J>(l).ok())
        .map(|v| ChildResult {
            outcome: v["outcome"].as_str().unwrap_or("error").to_string(),
            estimate: v["estimate"].as_u64().unwrap_or(0),
            message: v["message"].as_str().unwrap_or_default().to_string(),
        });
    let peak = read_count(&stderr, "peak memory footprint").map(|footprint| Peak {
        footprint,
        rss: read_count(&stderr, "maximum resident set size"),
    });
    Ok((result, peak, out.status.success(), stderr))
}

fn mib(b: u64) -> f64 {
    b as f64 / MIB as f64
}

/// NFR-17 の親：基準と 4 つのケースを子で回して記録する。
pub(super) fn memory(ctx: &mut Ctx) -> Result<(), String> {
    let exe: PathBuf = std::env::current_exe().map_err(|e| e.to_string())?;
    let photo = ctx.photo()?.clone();
    let budget = ctx.budget_total();
    let z = ctx.sizes.clone();
    let (p, b) = (ctx.opts.preview_threads, ctx.opts.bulk_threads);
    let load = load1();
    // 基準：実行の場を作るだけの子（エディタ）と、アプリのコーデックの組も作る子（保存）
    let (_, base, _, base_err) = spawn(&exe, &ctx.opts, "base", budget, &photo)?;
    let (_, base_codecs, _, _) = spawn(&exe, &ctx.opts, "base-codecs", budget, &photo)?;
    let cases: [(&str, u64, usize, String); 4] = [
        (
            "save",
            budget,
            b,
            format!(
                "保存の通し（{} → Lanczos3 50% → JPEG q90、予算の確保つき）",
                photo.label
            ),
        ),
        (
            "editor",
            budget,
            p,
            format!(
                "筆の文書のエディタ（{0}²・前の筆 {1} 本 ＋ 8px を {2} 本、キャッシュの確保つき）",
                z.mem_canvas, z.mem_prior, z.mem_strokes
            ),
        ),
        (
            "save-limit",
            z.limit_save_budget,
            b,
            format!("小さい予算での保存（{}）", photo.label),
        ),
        (
            "editor-limit",
            z.limit_editor_budget,
            p,
            format!(
                "小さい予算でエディタを開く（{}×{} の写真）",
                z.photo.0, z.photo.1
            ),
        ),
    ];
    for (case, budget, threads, item) in cases {
        let (result, peak, success, stderr) = spawn(&exe, &ctx.opts, case, budget, &photo)?;
        let limit_case = case.ends_with("-limit");
        let base = if case.starts_with("save") {
            base_codecs
        } else {
            base
        };
        let (verdict, note, extra) = match (&base, peak, &result) {
            (Some(base), Some(peak), Some(res)) => {
                let net = peak.footprint.saturating_sub(base.footprint);
                let mut notes = Vec::new();
                let verdict = if limit_case {
                    if success && res.outcome == "limit" {
                        notes.push("落ちずに Limit".to_string());
                        Verdict::Pass
                    } else {
                        notes.push(format!(
                            "Limit にならなかった（{}：{}）",
                            res.outcome, res.message
                        ));
                        Verdict::Fail
                    }
                } else if !success || res.outcome != "ok" {
                    notes.push(format!("{}：{}", res.outcome, res.message));
                    Verdict::Fail
                } else {
                    notes.push(format!(
                        "実測 {:.1} MiB（基準 {:.1} MiB を引いた量）・見積もり {:.1} MiB",
                        mib(net),
                        mib(base.footprint),
                        mib(res.estimate)
                    ));
                    if net > res.estimate.saturating_mul(5) / 4 + 4 * MIB {
                        notes.push("見積もり × 1.25 ＋ 4 MiB を上回った".into());
                    }
                    if net <= budget {
                        Verdict::Pass
                    } else {
                        Verdict::Fail
                    }
                };
                (
                    verdict,
                    notes.join("・"),
                    json!({
                        "outcome": res.outcome,
                        "peak_bytes": peak.footprint,
                        "rss_bytes": peak.rss,
                        "base_bytes": base.footprint,
                        "net_bytes": net,
                        "estimate_bytes": res.estimate,
                        "budget_bytes": budget,
                        "message": res.message,
                    }),
                )
            }
            _ => {
                let why = if base.is_none() || peak.is_none() {
                    format!(
                        "/usr/bin/time -l のピークが読めない（{}）",
                        if base_err.trim().is_empty() {
                            stderr.trim().to_string()
                        } else {
                            base_err.trim().chars().take(200).collect()
                        }
                    )
                } else {
                    format!(
                        "子の結果が読めない：{}",
                        stderr.trim().chars().take(400).collect::<String>()
                    )
                };
                let verdict = if result.is_none() && peak.is_some() {
                    Verdict::Fail
                } else {
                    Verdict::Skip
                };
                (verdict, why, json!({ "budget_bytes": budget }))
            }
        };
        let r = Record::check(
            ctx.run, 17, "memory", case, &item, threads, verdict, note, load, extra,
        );
        ctx.push(r);
    }
    Ok(())
}

/// 子の結果を 1 行の JSON で書く。
fn report(case: &str, outcome: &str, estimate: u64, budget: u64, message: &str) {
    emit(
        &json!({
            "case": case,
            "outcome": outcome,
            "estimate": estimate,
            "budget": budget,
            "message": message,
        })
        .to_string(),
    );
}

/// 結果を書く（Limit は結果の 1 つで、子は成功で終える。ほかのエラーは子の失敗）。
fn finish(case: &str, budget: u64, r: Result<u64, EngineError>) -> Result<(), String> {
    match r {
        Ok(estimate) => {
            report(case, "ok", estimate, budget, "");
            Ok(())
        }
        Err(EngineError::Limit(m)) => {
            report(case, "limit", 0, budget, &m);
            Ok(())
        }
        Err(e) => {
            report(case, "error", 0, budget, &e.to_string());
            Err(format!("{case}: {e}"))
        }
    }
}

/// NFR-17 の子（`--child mem:<ケース>`）。
pub(super) fn child(opts: &Opts, spec: &str) -> Result<(), String> {
    let case = spec
        .strip_prefix("mem:")
        .ok_or_else(|| format!("--child は mem:<ケース> です: {spec}"))?;
    let z = Sizes::new(opts.quick);
    let budget = opts
        .budget
        .ok_or_else(|| "子には --budget が要ります".to_string())?;
    let rt = runtime(opts.preview_threads, opts.bulk_threads, Some(budget))?;
    match case {
        "base" => {
            report(case, "base", 0, budget, "");
            Ok(())
        }
        "base-codecs" => {
            // 保存の子と同じ準備（アプリのコーデックの組を作る）まで
            let codecs = kura_mac::full_codec_set(
                &kura_engine::codec::PinnedOptions::default(),
                &std::env::temp_dir().join("kura-bench-edit-codec"),
            );
            report(case, "base", 0, budget, "");
            drop(codecs);
            Ok(())
        }
        "save" | "save-limit" => {
            let path = opts
                .input
                .clone()
                .ok_or_else(|| "子には --in が要ります".to_string())?;
            let format = match opts.format.as_deref() {
                Some(f) => kura_engine::codec::format_by_id(f)
                    .map(|d| d.id)
                    .ok_or_else(|| format!("形式 {f} を知りません"))?,
                None => "jpeg",
            };
            let codecs = kura_mac::full_codec_set(
                &kura_engine::codec::PinnedOptions::default(),
                &std::env::temp_dir().join("kura-bench-edit-codec"),
            );
            let graph = save_doc("lanczos3").map_err(|e| e.to_string())?;
            let out = std::env::temp_dir()
                .join(format!("kura-bench-edit-mem-{}.jpg", std::process::id()));
            let r = super::batch::save_once(&rt, &codecs, &graph, &path, format, &out)
                .map(|(_, est)| est);
            let _ = std::fs::remove_file(&out);
            finish(case, budget, r)
        }
        "editor" => {
            let r = (|| -> Result<u64, EngineError> {
                let canvas = flat_canvas(z.mem_canvas)?;
                let (g, paint) = paint_doc(prior_strokes(z.mem_prior, z.mem_canvas)?, None)?;
                let mut editor = Editor::open(&rt, &g, canvas, Digest([2; 32]), paint, 0)?;
                for i in 0..z.mem_strokes {
                    let item = stroke_item(5_000 + i as u64, 8, z.mem_canvas)?;
                    editor.step(
                        Step::Commit(
                            Edit::AppendItems {
                                node: paint,
                                field: "actions".into(),
                                items: vec![item],
                            },
                            CommitMode::Step,
                        ),
                        0,
                    )?;
                }
                Ok(editor.max_need)
            })();
            finish(case, budget, r)
        }
        "editor-limit" => {
            let r = (|| -> Result<u64, EngineError> {
                let (w, h) = z.photo;
                let photo = InputGen::Photo { w, h, seed: 7 }.generate()?;
                let (g, node) = {
                    let mut g = kura_engine::Graph::new_single();
                    let node =
                        g.push_linear("image.flip", 1, json!({ "axis": "horizontal" }), 0)?;
                    (g, node)
                };
                let editor = Editor::open(&rt, &g, photo, Digest([3; 32]), node, 0)?;
                Ok(editor.max_need)
            })();
            finish(case, budget, r)
        }
        other => Err(format!("知らないケースです: {other}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn time_output_is_read() {
        let stderr = "        0.52 real         0.40 user         0.10 sys\n\
                      123456789  maximum resident set size\n\
                      0  average shared memory size\n\
                      98765432  peak memory footprint\n";
        assert_eq!(
            read_count(stderr, "peak memory footprint"),
            Some(98_765_432)
        );
        assert_eq!(
            read_count(stderr, "maximum resident set size"),
            Some(123_456_789)
        );
        assert_eq!(read_count("", "peak memory footprint"), None);
    }
}
