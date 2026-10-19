//! `kura-devtools bench edit --quick` の煙のテスト（E30。docs/editing.md §16・requirements.md の NFR-11〜17）。極小の大きさで
//! 2 回ずつ回し、すべての NFR のレコードを持つ JSON と、architecture.md §13 の形の Markdown を出すこと、正しさの照らし
//! （エディタの経路と一括の経路の一致・予算を超える入力の Limit）が通ることと、決めた範囲の外（32px より太い筆・減色の
//! プレビュー）が達成・未達でなく「範囲外」の記録になることを確かめる。debug でも数十秒で回る（macOS の CI）。
//! NFR-17 は子プロセスを `/usr/bin/time -l` で起こすので、それが使えない環境では、そのレコードを見送り（skip）として出す。

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeSet;
use std::process::Command;

#[test]
fn quick_writes_a_record_for_every_nfr() {
    let dir = tempfile::tempdir().unwrap();
    let json = dir.path().join("bench.json");
    let md = dir.path().join("bench.md");
    let out = Command::new(env!("CARGO_BIN_EXE_kura-devtools"))
        .args(["bench", "edit", "--quick", "--runs", "1", "--json"])
        .arg(&json)
        .arg("--markdown")
        .arg(&md)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "bench edit が失敗しました: {stderr}");
    let table = String::from_utf8_lossy(&out.stdout);
    assert!(
        table.contains("NFR-11") && table.contains("NFR-17"),
        "{table}"
    );

    let v: serde_json::Value = serde_json::from_slice(&std::fs::read(&json).unwrap()).unwrap();
    let records = v["records"].as_array().unwrap();
    let nfrs: BTreeSet<u64> = records.iter().map(|r| r["nfr"].as_u64().unwrap()).collect();
    assert_eq!(nfrs, (11..=17).collect::<BTreeSet<u64>>(), "{v:#}");
    let mut out_of_scope = BTreeSet::new();
    for r in records {
        for key in [
            "run", "nfr", "case", "item", "threads", "n", "target", "verdict", "note", "load",
        ] {
            assert!(r.get(key).is_some(), "{key} がありません: {r}");
        }
        let verdict = r["verdict"].as_str().unwrap();
        assert!(
            ["pass", "fail", "record", "out_of_scope", "skip"].contains(&verdict),
            "{r}"
        );
        // 決めた範囲の外（NFR-12 の 32px より太い筆・NFR-11 の減色のプレビュー）の時間は、達成・未達にしない（ADR-154。
        // 同じケースの正しさの照らし（NFR-16）は判定する）
        let case = r["case"].as_str().unwrap();
        let px = case
            .strip_suffix("px")
            .and_then(|c| c.rsplit('-').next())
            .and_then(|n| n.parse::<u32>().ok());
        let outside = matches!(r["nfr"].as_u64(), Some(11 | 12))
            && (px.is_some_and(|px| px > 32) || case == "pixel-convert-indexed8");
        if outside {
            assert!(["out_of_scope", "record"].contains(&verdict), "{r}");
            if verdict == "out_of_scope" {
                out_of_scope.insert(r["nfr"].as_u64().unwrap());
            }
        } else {
            assert_ne!(verdict, "out_of_scope", "{r}");
        }
        // 正しさの照らしは、時間と違って debug でも外れない
        if matches!(r["nfr"].as_u64(), Some(16)) {
            assert_eq!(verdict, "pass", "経路・スレッドで値が食い違いました: {r}");
        }
        if r["case"].as_str().unwrap().ends_with("-limit") {
            assert!(["pass", "skip"].contains(&verdict), "予算を超える入力: {r}");
        }
        // 時間のレコードは n と p50 を持つ
        if matches!(r["nfr"].as_u64(), Some(11..=15)) {
            assert!(r["n"].as_u64().unwrap() >= 1, "{r}");
            assert!(r["p50_ms"].as_f64().is_some(), "{r}");
        }
    }
    assert_eq!(
        out_of_scope,
        BTreeSet::from([11, 12]),
        "範囲外の記録（減色のプレビュー・64px の筆）"
    );
    // 環境とロードアベレージ
    for key in [
        "cpu",
        "cores",
        "build",
        "preview_threads",
        "bulk_threads",
        "budget_bytes",
    ] {
        assert!(
            v["env"].get(key).is_some(),
            "環境の {key} がありません: {v:#}"
        );
    }
    let runs = v["runs"].as_array().unwrap();
    assert_eq!(runs.len(), 1);
    assert!(runs[0].get("loadavg_before").is_some() && runs[0].get("loadavg_after").is_some());

    let md = std::fs::read_to_string(&md).unwrap();
    assert!(md.contains("| NFR | 項目 | 結果 | 目標 | 判定 |"), "{md}");
    for nfr in 11..=17 {
        assert!(
            md.contains(&format!("| {nfr} |")),
            "Markdown に NFR-{nfr} の行がありません: {md}"
        );
    }
    assert!(md.contains("| 範囲外 |"), "{md}");
}

/// 引数の誤りは使い方を返して失敗する（release でない --quick なしの実行も、説明つきで断る）。
#[test]
fn bad_arguments_fail_with_usage() {
    let out = Command::new(env!("CARGO_BIN_EXE_kura-devtools"))
        .args(["bench", "edit", "--only", "99", "--quick"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("--only"), "{stderr}");
}
