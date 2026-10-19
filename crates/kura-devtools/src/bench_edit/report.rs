//! bench edit のレコードと出力（人が読む表・JSON・architecture.md §13 の形の Markdown）。

use std::collections::BTreeMap;
use std::io::Write as _;

use kura_engine::testing::bench::{Judge, Stats, Target, target};
use serde_json::{Value as J, json};

/// 判定。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// 判定の条件で目標を満たした（一致・予算以内も）。
    Pass,
    /// 判定の条件で目標を満たさなかった。
    Fail,
    /// 記録だけ（判定のスレッドの数でない・参考のケース）。
    Record,
    /// 判定の条件で測ったが、目標の範囲の外（決めた範囲。NFR-12 の 32px より太い筆・NFR-11 の減色のプレビュー）。
    OutOfScope,
    /// 測れなかった（`/usr/bin/time -l` がないなど）。
    Skip,
}

impl Verdict {
    /// JSON の値。
    pub fn key(self) -> &'static str {
        match self {
            Self::Pass => "pass",
            Self::Fail => "fail",
            Self::Record => "record",
            Self::OutOfScope => "out_of_scope",
            Self::Skip => "skip",
        }
    }

    /// 表の文言。
    pub fn label(self) -> &'static str {
        match self {
            Self::Pass => "達成",
            Self::Fail => "未達",
            Self::Record => "記録",
            Self::OutOfScope => "範囲外",
            Self::Skip => "見送り",
        }
    }
}

/// レコード 1 つ（JSON の `records` の要素・表の 1 行）。
#[derive(Debug, Clone)]
pub struct Record {
    pub run: usize,
    pub nfr: u8,
    pub case: String,
    pub item: String,
    /// 描いた段（プレビュー。ほかは None）。
    pub stage: Option<u8>,
    pub threads: usize,
    /// 一括を同時に流したか（段 10 では流さない）。
    pub contention: bool,
    pub stats: Option<Stats>,
    /// 目標（`testing::bench::TARGETS`）。
    pub target: Option<&'static Target>,
    /// 判定に使うレコードか（判定のスレッドの数で、参考でない）。
    pub judged: bool,
    pub verdict: Verdict,
    pub note: String,
    /// ケースを始めたときの 1 分のロードアベレージ。
    pub load: Option<f64>,
    /// ケースに固有の値（メモリのバイト数・経路の照合など）。
    pub extra: J,
}

impl Record {
    /// 時間のレコード。`judged` で目標と比べ、そうでなければ記録。
    #[allow(clippy::too_many_arguments)]
    pub fn timed(
        run: usize,
        nfr: u8,
        target_case: &str,
        case: &str,
        item: &str,
        threads: usize,
        stats: Stats,
        judged: bool,
        load: Option<f64>,
    ) -> Self {
        let goal = target(nfr, target_case);
        let verdict = match (judged, goal.and_then(|g| g.verdict(&stats))) {
            (true, Some(true)) => Verdict::Pass,
            (true, Some(false)) => Verdict::Fail,
            _ => Verdict::Record,
        };
        Self {
            run,
            nfr,
            case: case.into(),
            item: item.into(),
            stage: None,
            threads,
            contention: false,
            stats: Some(stats),
            target: goal,
            judged,
            verdict,
            note: String::new(),
            load,
            extra: json!({}),
        }
    }

    /// 時間で判定しないレコード（一致・予算）。
    #[allow(clippy::too_many_arguments)]
    pub fn check(
        run: usize,
        nfr: u8,
        target_case: &str,
        case: &str,
        item: &str,
        threads: usize,
        verdict: Verdict,
        note: String,
        load: Option<f64>,
        extra: J,
    ) -> Self {
        Self {
            run,
            nfr,
            case: case.into(),
            item: item.into(),
            stage: None,
            threads,
            contention: false,
            stats: None,
            target: target(nfr, target_case),
            judged: true,
            verdict,
            note,
            load,
            extra,
        }
    }

    /// 参考の記録にする（判定の条件で測ったが、仮の値・段 0 に落ちたもの。表には「記録」で載せる）。
    pub fn into_reference(mut self) -> Self {
        if matches!(self.verdict, Verdict::Pass | Verdict::Fail) {
            self.verdict = Verdict::Record;
        }
        self
    }

    /// 範囲外の記録にする（判定の条件で測ったが、決めた目標の範囲の外。表の判定の列は「範囲外」）。判定のスレッドの数で
    /// ないもの（記録）はそのまま。
    pub fn out_of_scope(mut self) -> Self {
        if matches!(self.verdict, Verdict::Pass | Verdict::Fail) {
            self.verdict = Verdict::OutOfScope;
        }
        self
    }

    /// 注を足す（「・」でつなぐ）。
    pub fn with_note(mut self, note: &str) -> Self {
        if !note.is_empty() {
            if !self.note.is_empty() {
                self.note.push('・');
            }
            self.note.push_str(note);
        }
        self
    }

    /// JSON のレコード。
    pub fn to_json(&self) -> J {
        let ms = |v: u64| v as f64 / 1e6;
        let s = self.stats.as_ref();
        json!({
            "run": self.run,
            "nfr": self.nfr,
            "case": self.case,
            "item": self.item,
            "stage": self.stage,
            "threads": self.threads,
            "contention": self.contention,
            "n": s.map_or(0, |s| s.n),
            "min_ms": s.map(|s| ms(s.min)),
            "p50_ms": s.map(|s| ms(s.p50)),
            "p95_ms": s.and_then(|s| s.p95).map(ms),
            "max_ms": s.map(|s| ms(s.max)),
            "mean_ms": s.map(|s| ms(s.mean)),
            "target": self.target.map(goal_text),
            "target_ms": self.target.and_then(|t| t.limit_ns).map(ms),
            "judge": self.target.map(|t| judge_key(t.judge)),
            "judged": self.judged,
            "verdict": self.verdict.key(),
            "note": self.note,
            "load": self.load,
            "extra": self.extra,
        })
    }

    /// 人が読む表の 1 行。
    pub fn line(&self) -> String {
        let ms = |v: u64| format!("{:.3}", v as f64 / 1e6);
        let times = match &self.stats {
            Some(s) => format!(
                "n {:>3} | p50 {:>9} | p95 {:>9} | 最大 {:>9} ms",
                s.n,
                ms(s.p50),
                s.p95.map_or("-".to_string(), ms),
                ms(s.max)
            ),
            None => format!("{:<48}", "-"),
        };
        format!(
            "NFR-{} | {:<26} | {:>2} スレッド | {} | 目標 {:<16} | {:<4} | {}{}",
            self.nfr,
            self.case,
            self.threads,
            times,
            self.target.map_or("-".to_string(), goal_text),
            self.verdict.label(),
            self.item,
            if self.note.is_empty() {
                String::new()
            } else {
                format!("（{}）", self.note)
            }
        )
    }
}

fn judge_key(j: Judge) -> &'static str {
    match j {
        Judge::P95 => "p95",
        Judge::Median => "median",
        Judge::Total => "total",
        Judge::Match => "match",
        Judge::Budget => "budget",
    }
}

/// 時間の書き方（1 秒以上・100 ms 以上は秒、ほかは ms）。
fn time_text(ns: u64) -> String {
    let ms = ns as f64 / 1e6;
    if ms >= 100.0 {
        format!("{:.2} 秒", ms / 1000.0)
    } else {
        format!("{ms:.0} ms")
    }
}

/// 目標の文言。
pub fn goal_text(t: &Target) -> String {
    match (t.judge, t.limit_ns) {
        (Judge::P95, Some(l)) => format!("p95 {}", time_text(l)),
        (Judge::Median, Some(l)) => format!("中央値 {}", time_text(l)),
        (Judge::Total, Some(l)) => format!("通し {}", time_text(l)),
        (Judge::Match, _) => "一致".into(),
        (Judge::Budget, _) => "予算以内・超える入力は Limit".into(),
        (_, None) => "-".into(),
    }
}

/// 環境と、走らせた回ごとのロードアベレージ。
pub struct Report {
    pub env: J,
    pub runs: Vec<J>,
    pub records: Vec<Record>,
}

impl Report {
    /// JSON の全体。
    pub fn to_json(&self) -> J {
        json!({
            "env": self.env,
            "runs": self.runs,
            "records": self.records.iter().map(Record::to_json).collect::<Vec<_>>(),
        })
    }

    /// 判定に使ったレコードを (NFR, ケース) ごとにまとめた Markdown（architecture.md §13 の M9 の表の形。回をまたいだ幅）。
    pub fn markdown(&self) -> String {
        let env = &self.env;
        let loads: Vec<f64> = self
            .runs
            .iter()
            .flat_map(|r| {
                ["loadavg_before", "loadavg_after"]
                    .into_iter()
                    .filter_map(|k| r[k].get(0).and_then(J::as_f64))
                    .collect::<Vec<_>>()
            })
            .collect();
        let load = match (
            loads.iter().copied().reduce(f64::min),
            loads.iter().copied().reduce(f64::max),
        ) {
            (Some(a), Some(b)) => format!("{a:.1}〜{b:.1}"),
            _ => "不明".into(),
        };
        let mut out = format!(
            "M10 の実測（NFR-11〜17。`kura-devtools bench edit`。{}（{} コア）、{} GB、{}ビルド、プレビューのプール {}・一括のプール {}、\
             予算 {} MiB、ロードアベレージ {} で測った。{} 回の幅{}）：\n\n",
            env["cpu"].as_str().unwrap_or("不明"),
            env["cores"],
            env["memory_bytes"].as_u64().map_or(0, |b| b >> 30),
            if env["build"] == "release" {
                "リリース"
            } else {
                "デバッグ"
            },
            env["preview_threads"],
            env["bulk_threads"],
            env["budget_bytes"].as_u64().map_or(0, |b| b >> 20),
            load,
            self.runs.len(),
            if env["quick"] == true {
                "。--quick の極小の大きさ"
            } else {
                ""
            },
        );
        out.push_str("| NFR | 項目 | 結果 | 目標 | 判定 |\n|---|---|---|---|---|\n");
        // NFR の順、同じ NFR の中は測った順
        let mut order: Vec<(u8, String)> = Vec::new();
        let mut groups: BTreeMap<(u8, String), Vec<&Record>> = BTreeMap::new();
        for r in self.records.iter().filter(|r| r.judged) {
            let key = (r.nfr, r.case.clone());
            if !groups.contains_key(&key) {
                order.push(key.clone());
            }
            groups.entry(key).or_default().push(r);
        }
        order.sort_by_key(|(nfr, _)| *nfr);
        let mut notes = Vec::new();
        for key in &order {
            let Some(rs) = groups.get(key) else { continue };
            let (nfr, case) = key;
            let Some(first) = rs.first() else { continue };
            let result = result_text(rs);
            let verdicts: Vec<Verdict> = rs.iter().map(|r| r.verdict).collect();
            let verdict = summary_verdict(&verdicts);
            out.push_str(&format!(
                "| {nfr} | {}（{case}、{} スレッド） | {result} | {} | {verdict} |\n",
                first.item,
                first.threads,
                first.target.map_or("-".to_string(), goal_text),
            ));
            let mut seen: Vec<&str> = Vec::new();
            for r in rs {
                if !r.note.is_empty() && !seen.contains(&r.note.as_str()) {
                    seen.push(&r.note);
                }
            }
            for n in seen {
                notes.push(format!("- NFR-{nfr} {case}：{n}"));
            }
        }
        if !notes.is_empty() {
            out.push('\n');
            out.push_str(&notes.join("\n"));
            out.push('\n');
        }
        out
    }
}

/// 回をまたいだ幅の文言（a〜b。1 回なら a）。
fn span(values: &[u64], f: impl Fn(u64) -> String) -> String {
    match (values.iter().min(), values.iter().max()) {
        (Some(a), Some(b)) if a == b => f(*a),
        (Some(a), Some(b)) => format!("{}〜{}", f(*a), f(*b)),
        _ => "-".into(),
    }
}

fn ms_text(ns: u64) -> String {
    let ms = ns as f64 / 1e6;
    if ms >= 100.0 {
        format!("{ms:.0}")
    } else if ms >= 10.0 {
        format!("{ms:.1}")
    } else {
        format!("{ms:.2}")
    }
}

/// 結果の文言：時間は判定の仕方ごと、時間のないものは注と固有の値から。
fn result_text(rs: &[&Record]) -> String {
    let stats: Vec<&Stats> = rs.iter().filter_map(|r| r.stats.as_ref()).collect();
    let judge = rs.first().and_then(|r| r.target).map(|t| t.judge);
    if stats.is_empty() {
        let first = rs.first().map(|r| &r.extra);
        return match judge {
            Some(Judge::Match) => format!(
                "{}（{}）",
                if rs.iter().all(|r| r.verdict == Verdict::Pass) {
                    "一致"
                } else {
                    "不一致あり"
                },
                first
                    .and_then(|e| e["paths"].as_str())
                    .unwrap_or("エディタと一括")
            ),
            Some(Judge::Budget) => {
                let nets: Vec<u64> = rs
                    .iter()
                    .filter_map(|r| r.extra["net_bytes"].as_u64())
                    .collect();
                let ests: Vec<u64> = rs
                    .iter()
                    .filter_map(|r| r.extra["estimate_bytes"].as_u64())
                    .collect();
                let budget = first.and_then(|e| e["budget_bytes"].as_u64()).unwrap_or(0);
                let outcome = first.and_then(|e| e["outcome"].as_str()).unwrap_or("-");
                let mib = |b: u64| format!("{:.0}", b as f64 / (1 << 20) as f64);
                if outcome == "limit" {
                    if budget < 1 << 20 {
                        format!("Limit（予算 {} KiB）", budget >> 10)
                    } else {
                        format!("Limit（予算 {} MiB）", mib(budget))
                    }
                } else if nets.is_empty() {
                    "測れず".into()
                } else {
                    format!(
                        "実測 {} MiB／見積もり {} MiB／予算 {} MiB",
                        span(&nets, mib),
                        span(&ests, mib),
                        mib(budget)
                    )
                }
            }
            _ => "-".into(),
        };
    }
    let p50: Vec<u64> = stats.iter().map(|s| s.p50).collect();
    let max: Vec<u64> = stats.iter().map(|s| s.max).collect();
    match judge {
        Some(Judge::P95) => {
            let p95: Vec<u64> = stats.iter().map(|s| s.judged()).collect();
            let label = if stats.iter().all(|s| s.p95.is_some()) {
                "p95"
            } else {
                "最大"
            };
            format!(
                "p50 {}・{label} {} ms",
                span(&p50, ms_text),
                span(&p95, ms_text)
            )
        }
        Some(Judge::Median) => format!(
            "中央値 {}・最大 {} ms",
            span(&p50, ms_text),
            span(&max, ms_text)
        ),
        Some(Judge::Total) => format!("通し {} ms（各回の最大）", span(&max, ms_text)),
        _ => format!("p50 {} ms", span(&p50, ms_text)),
    }
}

/// 回をまたいだ判定の文言。
fn summary_verdict(vs: &[Verdict]) -> String {
    let pass = vs.iter().filter(|v| **v == Verdict::Pass).count();
    let fail = vs.iter().filter(|v| **v == Verdict::Fail).count();
    match (pass, fail) {
        (_, 0) if pass > 0 => "達成".into(),
        (0, f) if f > 0 => "未達".into(),
        (p, f) if p > 0 && f > 0 => format!("未達（{} 回のうち {f} 回）", p + f),
        _ => vs.first().map_or("-", |v| v.label()).into(),
    }
}

/// 表の行を出力に書く（進みが見えるように 1 行ずつ）。
pub fn emit(line: &str) {
    let mut out = std::io::stdout().lock();
    // 書けなくても測定は続ける（表は JSON にも残る）
    let _ = out
        .write_all(line.as_bytes())
        .and_then(|()| out.write_all(b"\n"))
        .and_then(|()| out.flush());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stats(v: Vec<u64>) -> Stats {
        Stats::from_samples(v).unwrap()
    }

    #[test]
    fn timed_records_follow_the_targets() {
        let fast = Record::timed(
            0,
            11,
            "preview",
            "pixel-trim",
            "トリム",
            3,
            stats(vec![1_000_000; 20]),
            true,
            Some(4.0),
        );
        assert_eq!(fast.verdict, Verdict::Pass);
        let slow = Record::timed(
            0,
            12,
            "stroke",
            "s",
            "筆",
            3,
            stats(vec![9_000_000; 20]),
            true,
            None,
        );
        assert_eq!(slow.verdict, Verdict::Fail);
        let other = Record::timed(
            0,
            12,
            "stroke",
            "s",
            "筆",
            16,
            stats(vec![9_000_000; 20]),
            false,
            None,
        );
        assert_eq!(other.verdict, Verdict::Record);
        let j = fast.to_json();
        assert_eq!(j["verdict"], "pass");
        assert_eq!(j["p95_ms"], 1.0);
        assert_eq!(j["target_ms"], 25.0);
        assert_eq!(j["load"], 4.0);
        assert!(fast.line().contains("NFR-11"));
    }

    #[test]
    fn markdown_groups_runs_into_ranges() {
        let rs = vec![
            Record::timed(
                0,
                11,
                "preview",
                "pixel-trim",
                "トリム",
                3,
                stats(vec![1_000_000; 20]),
                true,
                None,
            ),
            Record::timed(
                1,
                11,
                "preview",
                "pixel-trim",
                "トリム",
                3,
                stats(vec![2_000_000; 20]),
                true,
                None,
            ),
            Record::timed(
                0,
                13,
                "save",
                "save",
                "保存",
                3,
                stats(vec![300_000_000; 3]),
                true,
                None,
            ),
            Record::check(
                0,
                16,
                "determinism",
                "d",
                "一致",
                3,
                Verdict::Pass,
                String::new(),
                None,
                json!({ "paths": "エディタ（3）＝一括（1・16）" }),
            ),
            Record::check(
                0,
                17,
                "memory",
                "save-limit",
                "保存",
                3,
                Verdict::Pass,
                "落ちずに Limit".into(),
                None,
                json!({ "outcome": "limit", "budget_bytes": 128u64 << 20 }),
            ),
            Record::timed(
                0,
                11,
                "preview",
                "pixel-trim",
                "トリム",
                16,
                stats(vec![1_000_000; 20]),
                false,
                None,
            ),
        ];
        let report = Report {
            env: json!({ "cpu": "Apple M4 Max", "cores": 16, "memory_bytes": 128u64 << 30, "build": "release", "preview_threads": 3, "bulk_threads": 3, "budget_bytes": 4u64 << 30, "quick": false }),
            runs: vec![
                json!({ "loadavg_before": [3.5, 3.0, 2.0], "loadavg_after": [5.3, 4.0, 3.0] }),
            ],
            records: rs,
        };
        let md = report.markdown();
        assert!(md.contains("ロードアベレージ 3.5〜5.3"), "{md}");
        assert!(md.contains("| 11 | トリム（pixel-trim、3 スレッド） | p50 1.00〜2.00・p95 1.00〜2.00 ms | p95 25 ms | 達成 |"), "{md}");
        assert!(md.contains("| 13 | 保存（save、3 スレッド） | 中央値 300・最大 300 ms | 中央値 0.50 秒 | 達成 |"), "{md}");
        assert!(
            md.contains("| 16 |") && md.contains("一致（エディタ（3）＝一括（1・16））"),
            "{md}"
        );
        assert!(md.contains("Limit（予算 128 MiB）"), "{md}");
        assert!(md.contains("- NFR-17 save-limit：落ちずに Limit"), "{md}");
        // 判定に使わないレコードは載せない
        assert!(!md.contains("16 スレッド"), "{md}");
        assert_eq!(
            summary_verdict(&[Verdict::Pass, Verdict::Fail]),
            "未達（2 回のうち 1 回）"
        );
    }

    /// 決めた範囲の外（32px より太い筆・減色のプレビュー）は、判定の条件で測っても達成・未達にせず「範囲外」の記録にする
    /// （JSON は out_of_scope、表の判定の列は「範囲外」、注は残す）。判定のスレッドの数でないものは記録のまま。
    #[test]
    fn out_of_scope_records_are_not_judged() {
        let slow = |threads: usize, judged: bool| {
            Record::timed(
                0,
                12,
                "stroke",
                "stroke-0-64px",
                "筆 1 本（64px）",
                threads,
                stats(vec![20_000_000; 20]),
                judged,
                None,
            )
        };
        let r = slow(3, true)
            .with_note(kura_engine::testing::bench::OUT_OF_SCOPE_STROKE)
            .out_of_scope();
        assert_eq!(r.verdict, Verdict::OutOfScope);
        assert_eq!(r.to_json()["verdict"], "out_of_scope");
        assert!(r.line().contains("範囲外"), "{}", r.line());
        assert_eq!(slow(16, false).out_of_scope().verdict, Verdict::Record);
        let fast = Record::timed(
            0,
            11,
            "preview",
            "pixel-convert-indexed8",
            "減色",
            3,
            stats(vec![1_000_000; 20]),
            true,
            None,
        )
        .out_of_scope();
        assert_eq!(fast.verdict, Verdict::OutOfScope, "達成でも範囲外");
        let report = Report {
            env: json!({ "cpu": "Apple M4 Max", "cores": 16, "memory_bytes": 128u64 << 30, "build": "release", "preview_threads": 3, "bulk_threads": 3, "budget_bytes": 4u64 << 30, "quick": false }),
            runs: vec![],
            records: vec![r, fast],
        };
        let md = report.markdown();
        assert!(
            md.contains("| 12 | 筆 1 本（64px）（stroke-0-64px、3 スレッド） | p50 20.0・p95 20.0 ms | p95 8 ms | 範囲外 |"),
            "{md}"
        );
        assert!(
            md.contains("| 減色（pixel-convert-indexed8、3 スレッド）") && !md.contains("未達"),
            "{md}"
        );
        assert!(
            md.contains("- NFR-12 stroke-0-64px：範囲外（NFR-12 は太さ 32px まで"),
            "{md}"
        );
    }
}
