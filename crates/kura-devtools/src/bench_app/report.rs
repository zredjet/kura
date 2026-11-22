//! アプリの道のベンチの記録（行）と、JSON・Markdown の出力。
//!
//! 時間の行は 1 回ごとの値（ナノ秒）から最近順位法の p50・p95（`testing::bench::Stats`。20 回より少なければ p95 を出さず最大で
//! 判定）。判定は、判定のスレッドの数で目標のある行だけ（p95 か最大 ≤ 目標なら達成）。ほかは記録。Markdown は回をまたいだ
//! 幅（最小〜最大）を出す。

use std::collections::BTreeMap;

use kura_engine::testing::bench::Stats;
use serde_json::{Value as J, json};

/// 判定。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Verdict {
    Pass,
    Fail,
    /// 記録だけ（目標がない・判定のスレッドの数でない・参考）。
    Record,
    /// 決めた目標の範囲の外（太さ 32px を超える筆・減色のプレビュー）。
    OutOfScope,
}

impl Verdict {
    fn key(self) -> &'static str {
        match self {
            Self::Pass => "pass",
            Self::Fail => "fail",
            Self::Record => "record",
            Self::OutOfScope => "out_of_scope",
        }
    }
}

/// 行 1 つ（ケース × 指標 × スレッドの数 × 回）。
#[derive(Debug, Clone)]
pub(crate) struct Row {
    pub run: usize,
    pub nfr: u8,
    pub case: String,
    pub item: String,
    /// 指標の名前（`step`・`tiles-rgba` など）。
    pub metric: &'static str,
    /// 指標の表の名前。
    pub label: &'static str,
    pub threads: usize,
    /// 時間（ナノ秒）。
    pub stats: Option<Stats>,
    /// 時間でない値（MiB・件数）と単位。
    pub value: Option<(f64, &'static str)>,
    /// 目標（ミリ秒）。
    pub target_ms: Option<f64>,
    pub verdict: Verdict,
    pub note: String,
    /// 測り始めの 1 分のロードアベレージ。
    pub load: Option<f64>,
    pub extra: J,
}

impl Row {
    /// 時間の行。`judge` が真で目標があれば判定する。
    #[allow(clippy::too_many_arguments)]
    pub fn timed(
        run: usize,
        nfr: u8,
        case: &str,
        item: &str,
        metric: &'static str,
        label: &'static str,
        threads: usize,
        stats: Stats,
        target_ms: Option<f64>,
        judge: bool,
        load: Option<f64>,
    ) -> Self {
        let verdict = match (judge, target_ms) {
            (true, Some(t)) => {
                if stats.judged() as f64 <= t * 1e6 {
                    Verdict::Pass
                } else {
                    Verdict::Fail
                }
            }
            _ => Verdict::Record,
        };
        Self {
            run,
            nfr,
            case: case.into(),
            item: item.into(),
            metric,
            label,
            threads,
            stats: Some(stats),
            value: None,
            target_ms,
            verdict,
            note: String::new(),
            load,
            extra: J::Null,
        }
    }

    /// 時間でない値の行（判定は呼び手が決める）。
    #[allow(clippy::too_many_arguments)]
    pub fn value(
        run: usize,
        nfr: u8,
        case: &str,
        item: &str,
        metric: &'static str,
        label: &'static str,
        threads: usize,
        value: f64,
        unit: &'static str,
        verdict: Verdict,
        load: Option<f64>,
    ) -> Self {
        Self {
            run,
            nfr,
            case: case.into(),
            item: item.into(),
            metric,
            label,
            threads,
            stats: None,
            value: Some((value, unit)),
            target_ms: None,
            verdict,
            note: String::new(),
            load,
            extra: J::Null,
        }
    }

    pub fn with_note(mut self, note: &str) -> Self {
        if !note.is_empty() {
            if !self.note.is_empty() {
                self.note.push('・');
            }
            self.note.push_str(note);
        }
        self
    }

    pub fn with_extra(mut self, extra: J) -> Self {
        self.extra = extra;
        self
    }

    /// 中央値で判定し直す（NFR-13 の保存は中央値で判定する。M10 の bench edit と同じ）。
    pub fn judge_median(mut self) -> Self {
        if let (Some(s), Some(t)) = (&self.stats, self.target_ms) {
            self.verdict = if s.p50 as f64 <= t * 1e6 {
                Verdict::Pass
            } else {
                Verdict::Fail
            };
            self = self.with_note("中央値で判定");
        }
        self
    }

    /// 範囲の外にする（判定しない）。
    pub fn out_of_scope(mut self) -> Self {
        self.verdict = Verdict::OutOfScope;
        self
    }

    fn to_json(&self) -> J {
        json!({
            "run": self.run,
            "nfr": self.nfr,
            "case": self.case,
            "item": self.item,
            "metric": self.metric,
            "label": self.label,
            "threads": self.threads,
            "stats_ns": self.stats.as_ref().map(|s| json!({
                "n": s.n, "min": s.min, "p50": s.p50, "p95": s.p95, "max": s.max, "mean": s.mean,
                "samples": s.samples,
            })),
            "value": self.value.map(|(v, u)| json!({ "value": v, "unit": u })),
            "target_ms": self.target_ms,
            "verdict": self.verdict.key(),
            "note": self.note,
            "load1": self.load,
            "extra": self.extra,
        })
    }

    /// 標準出力の 1 行。
    pub fn line(&self) -> String {
        let body = match (&self.stats, self.value) {
            (Some(s), _) => format!(
                "p50 {}・p95 {}・最大 {}（{} 回）",
                ms(s.p50),
                s.p95.map(ms).unwrap_or_else(|| "—".into()),
                ms(s.max),
                s.n
            ),
            (None, Some((v, u))) => format!("{v:.1} {u}"),
            (None, None) => String::new(),
        };
        format!(
            "[{}] NFR-{} {} {}・{}（{} スレッド）：{body}{}{}",
            self.run + 1,
            self.nfr,
            self.case,
            self.label,
            self.item,
            self.threads,
            self.target_ms
                .map(|t| format!("・目標 {t} ms・{}", self.verdict.key()))
                .unwrap_or_default(),
            if self.note.is_empty() {
                String::new()
            } else {
                format!("（{}）", self.note)
            }
        )
    }
}

/// 標準エラーに 1 行（ログと混ぜない）。
pub(crate) fn emit(line: &str) {
    use std::io::Write as _;
    let mut out = std::io::stderr().lock();
    let _ = writeln!(out, "{line}");
}

/// ミリ秒の文字（3 桁ほど）。
pub(crate) fn ms(ns: u64) -> String {
    let v = ns as f64 / 1e6;
    if v < 10.0 {
        format!("{v:.2}")
    } else if v < 100.0 {
        format!("{v:.1}")
    } else {
        format!("{v:.0}")
    }
}

fn span(values: &[u64]) -> String {
    match (values.iter().min(), values.iter().max()) {
        (Some(a), Some(b)) if ms(*a) == ms(*b) => ms(*a),
        (Some(a), Some(b)) => format!("{}〜{}", ms(*a), ms(*b)),
        _ => "—".into(),
    }
}

fn vspan(values: &[f64]) -> String {
    let fmt = |v: f64| format!("{v:.1}");
    let min = values.iter().copied().fold(f64::INFINITY, f64::min);
    let max = values.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    if values.is_empty() {
        "—".into()
    } else if fmt(min) == fmt(max) {
        fmt(min)
    } else {
        format!("{}〜{}", fmt(min), fmt(max))
    }
}

/// 中央の値（回をまたいだ代表。スレッドの数の表）。
fn median(mut v: Vec<u64>) -> Option<u64> {
    v.sort();
    v.get(v.len().saturating_sub(1) / 2).copied()
}

fn verdict_text(vs: &[Verdict]) -> String {
    let fails = vs.iter().filter(|v| **v == Verdict::Fail).count();
    if vs.contains(&Verdict::OutOfScope) {
        "範囲外".into()
    } else if fails > 0 {
        format!("未達（{} 回のうち {fails} 回）", vs.len())
    } else if vs.iter().all(|v| *v == Verdict::Pass) && !vs.is_empty() {
        "達成".into()
    } else {
        "記録".into()
    }
}

/// 全体の記録。
pub(crate) struct Report {
    pub title: &'static str,
    pub env: J,
    pub runs: Vec<J>,
    pub rows: Vec<Row>,
    /// 判定のスレッドの数。
    pub judged_threads: usize,
    /// スレッドの数の表に出す指標。
    pub thread_metrics: &'static [&'static str],
}

impl Report {
    pub fn to_json(&self) -> J {
        json!({
            "title": self.title,
            "env": self.env,
            "runs": self.runs,
            "rows": self.rows.iter().map(Row::to_json).collect::<Vec<_>>(),
        })
    }

    /// Markdown：判定のスレッドの数の表（回をまたいだ幅）と、スレッドの数ごとの表（回の中央）。
    pub fn markdown(&self) -> String {
        let mut out = String::new();
        let env = &self.env;
        out.push_str(&format!(
            "{}（{}・{} コア・{}・コミット {}・回 {}）\n\n",
            self.title,
            env["cpu"].as_str().unwrap_or("不明"),
            env["cores"],
            env["build"].as_str().unwrap_or(""),
            env["commit"].as_str().unwrap_or("?"),
            self.runs.len()
        ));
        // 判定のスレッドの数：ケース × 指標の順（現れた順）に回をまたいで束ねる
        let mut order: Vec<(u8, String, &'static str)> = Vec::new();
        let mut groups: BTreeMap<(u8, String, &'static str), Vec<&Row>> = BTreeMap::new();
        for r in self
            .rows
            .iter()
            .filter(|r| r.threads == self.judged_threads)
        {
            let key = (r.nfr, r.case.clone(), r.metric);
            if !groups.contains_key(&key) {
                order.push(key.clone());
            }
            groups.entry(key).or_default().push(r);
        }
        out.push_str(&format!(
            "判定のスレッドの数 {}（回をまたいだ幅。時間は ms）：\n\n",
            self.judged_threads
        ));
        out.push_str(
            "| NFR | 項目 | 指標 | p50 | p95（20 回より少なければ最大） | 目標 | 判定 | 注 |\n",
        );
        out.push_str("|---|---|---|---|---|---|---|---|\n");
        for key in &order {
            let rows = &groups[key];
            let first = rows[0];
            let (p50, p95) = if rows.iter().all(|r| r.stats.is_some()) {
                let p50: Vec<u64> = rows
                    .iter()
                    .filter_map(|r| r.stats.as_ref())
                    .map(|s| s.p50)
                    .collect();
                let p95: Vec<u64> = rows
                    .iter()
                    .filter_map(|r| r.stats.as_ref())
                    .map(Stats::judged)
                    .collect();
                (span(&p50), span(&p95))
            } else {
                let v: Vec<f64> = rows
                    .iter()
                    .filter_map(|r| r.value.map(|(v, _)| v))
                    .collect();
                let unit = first.value.map(|(_, u)| u).unwrap_or("");
                (format!("{} {unit}", vspan(&v)), "—".into())
            };
            let mut notes: Vec<&str> = Vec::new();
            for r in rows {
                if !r.note.is_empty() && !notes.contains(&r.note.as_str()) {
                    notes.push(&r.note);
                }
            }
            let verdicts: Vec<Verdict> = rows.iter().map(|r| r.verdict).collect();
            out.push_str(&format!(
                "| {} | {} | {} | {p50} | {p95} | {} | {} | {} |\n",
                first.nfr,
                first.item,
                first.label,
                first
                    .target_ms
                    .map(|t| format!("{t} ms"))
                    .unwrap_or_else(|| "—".into()),
                verdict_text(&verdicts),
                notes.join("・")
            ));
        }
        // スレッドの数ごと
        let threads: Vec<usize> = {
            let mut t: Vec<usize> = self.rows.iter().map(|r| r.threads).collect();
            t.sort();
            t.dedup();
            t
        };
        if threads.len() > 1 {
            out.push_str("\nスレッドの数ごと（p50／p95、ms。回の中央の値）：\n\n| 項目 | 指標 |");
            for t in &threads {
                out.push_str(&format!(" {t} |"));
            }
            out.push_str("\n|---|---|");
            for _ in &threads {
                out.push_str("---|");
            }
            out.push('\n');
            for key in order.iter().filter(|k| self.thread_metrics.contains(&k.2)) {
                let first = groups[key][0];
                out.push_str(&format!("| {} | {} |", first.item, first.label));
                for t in &threads {
                    let rows: Vec<&Row> = self
                        .rows
                        .iter()
                        .filter(|r| {
                            r.threads == *t
                                && r.nfr == key.0
                                && r.case == key.1
                                && r.metric == key.2
                        })
                        .collect();
                    let p50 = median(
                        rows.iter()
                            .filter_map(|r| r.stats.as_ref())
                            .map(|s| s.p50)
                            .collect(),
                    );
                    let p95 = median(
                        rows.iter()
                            .filter_map(|r| r.stats.as_ref())
                            .map(Stats::judged)
                            .collect(),
                    );
                    match (p50, p95) {
                        (Some(a), Some(b)) => out.push_str(&format!(" {}／{} |", ms(a), ms(b))),
                        _ => out.push_str(" — |"),
                    }
                }
                out.push('\n');
            }
        }
        // 回ごとのロードアベレージ
        out.push_str("\n回ごとのロードアベレージ（1・5・15 分。始め → 終わり）：");
        for r in &self.runs {
            out.push_str(&format!(
                " {}回目 {} → {}；",
                r["run"].as_u64().unwrap_or(0) + 1,
                r["loadavg_before"],
                r["loadavg_after"]
            ));
        }
        let loads: Vec<f64> = self.rows.iter().filter_map(|r| r.load).collect();
        if !loads.is_empty() {
            out.push_str(&format!(" ケースごとの 1 分の値 {}。", vspan(&loads)));
        }
        out.push('\n');
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stats(v: Vec<u64>) -> Stats {
        Stats::from_samples(v).unwrap()
    }

    #[test]
    fn timed_rows_are_judged_only_with_a_target() {
        let fast = stats((1..=30).map(|i| i * 100_000).collect());
        let r = Row::timed(
            0,
            11,
            "c",
            "項目",
            "step",
            "1 手",
            3,
            fast.clone(),
            Some(25.0),
            true,
            None,
        );
        assert_eq!(r.verdict, Verdict::Pass);
        let r = Row::timed(
            0,
            11,
            "c",
            "項目",
            "step",
            "1 手",
            3,
            fast.clone(),
            Some(1.0),
            true,
            None,
        );
        assert_eq!(r.verdict, Verdict::Fail);
        let r = Row::timed(
            0,
            11,
            "c",
            "項目",
            "step",
            "1 手",
            4,
            fast.clone(),
            Some(1.0),
            false,
            None,
        );
        assert_eq!(r.verdict, Verdict::Record);
        let r = Row::timed(
            0, 11, "c", "項目", "step", "1 手", 3, fast, None, true, None,
        );
        assert_eq!(r.verdict, Verdict::Record);
    }

    #[test]
    fn markdown_spans_runs_and_threads() {
        let mut rows = Vec::new();
        for run in 0..2 {
            for t in [3, 16] {
                let base = if t == 3 { 10_000_000 } else { 4_000_000 };
                let s = stats(
                    (0..30)
                        .map(|i| base + run as u64 * 1_000_000 + i * 10_000)
                        .collect(),
                );
                rows.push(Row::timed(
                    run,
                    11,
                    "pixel",
                    "ドット絵",
                    "step",
                    "1 手",
                    t,
                    s,
                    Some(25.0),
                    t == 3,
                    Some(4.0),
                ));
            }
        }
        rows.push(
            Row::value(
                0,
                17,
                "mem",
                "24MP",
                "peak",
                "最大",
                3,
                512.0,
                "MiB",
                Verdict::Record,
                None,
            )
            .with_note("注"),
        );
        let report = Report {
            title: "bench editor",
            env: json!({ "cpu": "テスト", "cores": 16, "build": "debug", "commit": "abc" }),
            runs: vec![
                json!({ "run": 0, "loadavg_before": [1.0, 1.0, 1.0], "loadavg_after": [1.0, 1.0, 1.0] }),
                json!({ "run": 1, "loadavg_before": [1.0, 1.0, 1.0], "loadavg_after": [1.0, 1.0, 1.0] }),
            ],
            rows,
            judged_threads: 3,
            thread_metrics: &["step"],
        };
        let md = report.markdown();
        assert!(
            md.contains("| 11 | ドット絵 | 1 手 | 10.1〜11.1 | 10.3〜11.3 |"),
            "{md}"
        );
        assert!(md.contains("達成"), "{md}");
        assert!(
            md.contains("| ドット絵 | 1 手 | 10.1／10.3 | 4.14／4.28 |"),
            "{md}"
        );
        assert!(md.contains("512.0 MiB"), "{md}");
        let j = report.to_json();
        assert_eq!(j["rows"].as_array().unwrap().len(), 5);
    }
}
