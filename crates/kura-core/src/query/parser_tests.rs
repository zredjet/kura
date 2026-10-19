//! 検索言語の解析のテスト（docs/search-query.md §7）。§1 の例をすべて含む。

use super::ast::*;
use super::parser::{Clause, parse};

fn tag(name: &str) -> Expr {
    Expr::Tag(TagTerm {
        ns: String::new(),
        name: name.into(),
    })
}

fn ns_tag(ns: &str, name: &str) -> Expr {
    Expr::Tag(TagTerm {
        ns: ns.into(),
        name: name.into(),
    })
}

fn not(e: Expr) -> Expr {
    Expr::Not(Box::new(e))
}

fn field(cond: FieldCond) -> Expr {
    Expr::Field(cond)
}

fn ok(input: &str) -> Expr {
    match parse(input) {
        Ok(parsed) => parsed.expr,
        Err(err) => panic!("「{input}」を解析できませんでした: {err:?}"),
    }
}

/// エラーの位置（UTF-16 の文字位置）。
fn err_span(input: &str) -> (usize, usize) {
    match parse(input) {
        Ok(parsed) => panic!("「{input}」はエラーになるはず: {:?}", parsed.expr),
        Err(err) => (err.start, err.end),
    }
}

// ---- §1 の例 ----

#[test]
fn example_single_word_is_a_tag() {
    assert_eq!(ok("夕景"), tag("夕景"));
}

#[test]
fn example_words_are_anded() {
    assert_eq!(ok("夕景 海"), Expr::And(vec![tag("夕景"), tag("海")]));
}

#[test]
fn example_pipe_is_or() {
    assert_eq!(ok("海 | 港"), Expr::Or(vec![tag("海"), tag("港")]));
}

#[test]
fn example_minus_excludes() {
    assert_eq!(ok("-未整理"), not(tag("未整理")));
}

#[test]
fn example_namespaced_tag() {
    assert_eq!(ok("作者:もりの"), ns_tag("作者", "もりの"));
}

#[test]
fn example_type_field() {
    assert_eq!(
        ok("種類:動画"),
        field(FieldCond::Type {
            op: Op::Has,
            value: vec![KindValue::Video]
        })
    );
}

#[test]
fn example_type_list() {
    assert_eq!(
        ok("種類:画像,動画"),
        field(FieldCond::Type {
            op: Op::Has,
            value: vec![KindValue::Image, KindValue::Video]
        })
    );
}

/// フォント・3D（M11）。日本語の語は「フォント」「3D」（`3d` も）、英語は `font`・`model`。
#[test]
fn type_field_accepts_font_and_model() {
    let kinds = |value: Vec<KindValue>| field(FieldCond::Type { op: Op::Has, value });
    assert_eq!(ok("種類:フォント"), kinds(vec![KindValue::Font]));
    assert_eq!(ok("種類:3D"), kinds(vec![KindValue::Model]));
    assert_eq!(ok("種類:3d"), kinds(vec![KindValue::Model]));
    assert_eq!(ok("type:font"), kinds(vec![KindValue::Font]));
    assert_eq!(ok("TYPE:Model"), kinds(vec![KindValue::Model]));
    assert_eq!(
        ok("種類:画像,フォント,3D"),
        kinds(vec![KindValue::Image, KindValue::Font, KindValue::Model])
    );
    // AST の JSON の名前は items.kind と同じ
    assert_eq!(
        serde_json::to_value(ok("種類:フォント,3D")).unwrap(),
        serde_json::json!({ "field": "type", "op": "has", "value": ["font", "model"] })
    );
    let err = parse("種類:3Dモデル").unwrap_err();
    assert!(err.message.contains("フォント・3D"), "{}", err.message);
}

#[test]
fn example_rating_comparison() {
    assert_eq!(
        ok("評価>=4"),
        field(FieldCond::Rating {
            op: Op::Ge,
            value: 4
        })
    );
}

#[test]
fn example_duration_comparison() {
    assert_eq!(
        ok("長さ>3分"),
        field(FieldCond::Dur {
            op: Op::Gt,
            value: 180_000
        })
    );
}

#[test]
fn example_relative_date() {
    assert_eq!(
        ok("追加:7日以内"),
        field(FieldCond::Added {
            op: Op::Has,
            value: DateSpec::WithinDays(7)
        })
    );
}

#[test]
fn example_never_played() {
    assert_eq!(
        ok("再生:未"),
        field(FieldCond::Played {
            op: Op::Has,
            value: PlayedSpec::Never
        })
    );
}

#[test]
fn example_folder_path() {
    // 空白を含むパスは引用符で囲む（ADR-52）
    assert_eq!(
        ok("フォルダ:\"写真/2026 旅行\""),
        field(FieldCond::Folder {
            op: Op::Has,
            value: vec!["写真".into(), "2026 旅行".into()]
        })
    );
    assert_eq!(
        ok("フォルダ:写真/2026"),
        field(FieldCond::Folder {
            op: Op::Has,
            value: vec!["写真".into(), "2026".into()]
        })
    );
}

#[test]
fn example_no_unit() {
    assert_eq!(
        ok("ユニット:なし"),
        field(FieldCond::Unit {
            op: Op::Has,
            value: UnitSpec::None
        })
    );
}

#[test]
fn example_quoted_phrase_is_full_text() {
    assert_eq!(ok("\"駅前 夜\""), Expr::Text("駅前 夜".into()));
}

#[test]
fn example_parentheses() {
    assert_eq!(
        ok("(海 | 港) 評価>=3 -ラフ"),
        Expr::And(vec![
            Expr::Or(vec![tag("海"), tag("港")]),
            field(FieldCond::Rating {
                op: Op::Ge,
                value: 3
            }),
            not(tag("ラフ")),
        ])
    );
}

// ---- 全角記号の正規化 ----

#[test]
fn full_width_symbols_work_like_half_width() {
    assert_eq!(ok("海｜港"), Expr::Or(vec![tag("海"), tag("港")]));
    assert_eq!(ok("－未整理"), not(tag("未整理")));
    assert_eq!(
        ok("評価＞＝4"),
        field(FieldCond::Rating {
            op: Op::Ge,
            value: 4
        })
    );
    assert_eq!(ok("作者：もりの"), ns_tag("作者", "もりの"));
    assert_eq!(
        ok("種類＝動画"),
        field(FieldCond::Type {
            op: Op::Eq,
            value: vec![KindValue::Video]
        })
    );
    assert_eq!(
        ok("（海　｜　港）　夜"),
        Expr::And(vec![Expr::Or(vec![tag("海"), tag("港")]), tag("夜")])
    );
    assert_eq!(ok("“駅前 夜”"), Expr::Text("駅前 夜".into()));
}

// ---- 名前空間付きタグとフィールドの区別 ----

#[test]
fn namespace_or_field_depends_on_the_left_side() {
    assert_eq!(ok("作者:もりの"), ns_tag("作者", "もりの"));
    assert!(matches!(
        ok("種類:動画"),
        Expr::Field(FieldCond::Type { .. })
    ));
    // 英語のフィールド名（大文字小文字は問わない）
    assert_eq!(
        ok("type:video"),
        field(FieldCond::Type {
            op: Op::Has,
            value: vec![KindValue::Video]
        })
    );
    assert_eq!(
        ok("RATING>=4"),
        field(FieldCond::Rating {
            op: Op::Ge,
            value: 4
        })
    );
    // 名前空間の値も引用符で囲める
    assert_eq!(ok("作者:\"もりの 森\""), ns_tag("作者", "もりの 森"));
    // 語の途中の - は除外ではない
    assert_eq!(ok("IMG-001"), tag("IMG-001"));
}

// ---- かっこと優先順位 ----

#[test]
fn and_binds_tighter_than_or() {
    assert_eq!(
        ok("a b | c"),
        Expr::Or(vec![Expr::And(vec![tag("a"), tag("b")]), tag("c")])
    );
    assert_eq!(
        ok("a | b c"),
        Expr::Or(vec![tag("a"), Expr::And(vec![tag("b"), tag("c")])])
    );
    assert_eq!(ok("a|b"), Expr::Or(vec![tag("a"), tag("b")]));
    assert_eq!(ok("-a b"), Expr::And(vec![not(tag("a")), tag("b")]));
    assert_eq!(ok("-(a | b)"), not(Expr::Or(vec![tag("a"), tag("b")])));
    assert_eq!(
        ok("(a (b | c))"),
        Expr::And(vec![tag("a"), Expr::Or(vec![tag("b"), tag("c")])])
    );
    assert_eq!(ok(""), Expr::all());
    assert_eq!(ok("   "), Expr::all());
}

// ---- 解析エラーの位置 ----

#[test]
fn errors_point_at_the_problem() {
    assert_eq!(err_span("(海 | 港"), (0, 1));
    assert_eq!(err_span("海 )"), (2, 3));
    assert_eq!(err_span("夜 \"駅前"), (2, 5));
    assert_eq!(err_span("海 |"), (2, 3));
    assert_eq!(err_span("| 海"), (0, 1));
    assert_eq!(err_span("夜 -"), (2, 3));
    assert_eq!(err_span("()"), (0, 2));
    // 値の誤りは値の位置
    assert_eq!(err_span("夜 評価>=9"), (6, 7));
    assert_eq!(err_span("種類:写真"), (3, 5));
    assert_eq!(err_span("長さ>そこそこ"), (3, 7));
    assert_eq!(err_span("追加:きのう"), (3, 6));
    // 値がない
    assert_eq!(err_span("評価>="), (0, 4));
    // 大小の比較は知っているフィールドだけ
    assert_eq!(err_span("夜 大きさ>3"), (2, 5));
    // 名前のない名前空間タグ
    assert_eq!(err_span("作者:"), (0, 3));
}

#[test]
fn errors_have_messages() {
    let err = parse("(海 | 港").unwrap_err();
    assert!(err.message.contains("かっこ"), "{}", err.message);
    let err = parse("評価>=9").unwrap_err();
    assert!(err.message.contains("評価"), "{}", err.message);
}

// ---- 値の書き方 ----

#[test]
fn parses_field_values() {
    let c = |input: &str| match ok(input) {
        Expr::Field(cond) => cond,
        other => panic!("{input}: {other:?}"),
    };
    assert_eq!(
        c("長さ:1:30"),
        FieldCond::Dur {
            op: Op::Has,
            value: 90_000
        }
    );
    assert_eq!(
        c("長さ<1時間"),
        FieldCond::Dur {
            op: Op::Lt,
            value: 3_600_000
        }
    );
    assert_eq!(
        c("長さ>=30秒"),
        FieldCond::Dur {
            op: Op::Ge,
            value: 30_000
        }
    );
    assert_eq!(
        c("長さ<1:02:03"),
        FieldCond::Dur {
            op: Op::Lt,
            value: 3_723_000
        }
    );
    assert_eq!(
        c("dur>90s"),
        FieldCond::Dur {
            op: Op::Gt,
            value: 90_000
        }
    );
    assert_eq!(
        c("追加:今日"),
        FieldCond::Added {
            op: Op::Has,
            value: DateSpec::Today
        }
    );
    assert_eq!(
        c("追加:昨日"),
        FieldCond::Added {
            op: Op::Has,
            value: DateSpec::Yesterday
        }
    );
    assert_eq!(
        c("撮影:30日以上前"),
        FieldCond::Taken {
            op: Op::Has,
            value: DateSpec::OlderThanDays(30)
        }
    );
    let date = |y, m, d| CivilDate { y, m, d };
    assert_eq!(
        c("追加:2026-10"),
        FieldCond::Added {
            op: Op::Has,
            value: DateSpec::Range {
                from: date(2026, 10, 1),
                to: date(2026, 10, 31)
            }
        }
    );
    assert_eq!(
        c("追加:2026-10-01..2026-10-08"),
        FieldCond::Added {
            op: Op::Has,
            value: DateSpec::Range {
                from: date(2026, 10, 1),
                to: date(2026, 10, 8)
            }
        }
    );
    assert_eq!(
        c("撮影<2024"),
        FieldCond::Taken {
            op: Op::Lt,
            value: DateSpec::Range {
                from: date(2024, 1, 1),
                to: date(2024, 12, 31)
            }
        }
    );
    assert_eq!(
        c("追加:2024-02"),
        FieldCond::Added {
            op: Op::Has,
            value: DateSpec::Range {
                from: date(2024, 2, 1),
                to: date(2024, 2, 29)
            }
        }
    );
    assert_eq!(
        c("再生:30日以上前"),
        FieldCond::Played {
            op: Op::Has,
            value: PlayedSpec::Date(DateSpec::OlderThanDays(30))
        }
    );
    assert_eq!(
        c("再生回数:0"),
        FieldCond::Plays {
            op: Op::Has,
            value: 0
        }
    );
    assert_eq!(
        c("フォルダ=写真"),
        FieldCond::Folder {
            op: Op::Eq,
            value: vec!["写真".into()]
        }
    );
    assert_eq!(
        c("ユニット:鎌倉"),
        FieldCond::Unit {
            op: Op::Has,
            value: UnitSpec::Name("鎌倉".into())
        }
    );
    assert_eq!(
        c("ユニット:あり"),
        FieldCond::Unit {
            op: Op::Has,
            value: UnitSpec::Any
        }
    );
    assert_eq!(
        c("取得元:pixiv"),
        FieldCond::Source {
            op: Op::Has,
            value: "pixiv".into()
        }
    );
    assert_eq!(
        c("名前:IMG_"),
        FieldCond::Name {
            op: Op::Has,
            value: "IMG_".into()
        }
    );
    assert_eq!(
        c("url:example.com"),
        FieldCond::Url {
            op: Op::Has,
            value: "example.com".into()
        }
    );
    assert_eq!(
        c("メモ:構図"),
        FieldCond::Note {
            op: Op::Has,
            value: "構図".into()
        }
    );
    assert_eq!(
        c("タグ数:0"),
        FieldCond::Tags {
            op: Op::Has,
            value: 0
        }
    );
    assert_eq!(
        c("幅>=3000"),
        FieldCond::W {
            op: Op::Ge,
            value: 3000
        }
    );
    assert_eq!(
        c("高さ!=1080"),
        FieldCond::H {
            op: Op::Ne,
            value: 1080
        }
    );
    assert_eq!(
        c("向き:縦"),
        FieldCond::Orient {
            op: Op::Has,
            value: Orientation::Portrait
        }
    );
    assert_eq!(
        c("向き:正方形"),
        FieldCond::Orient {
            op: Op::Has,
            value: Orientation::Square
        }
    );
}

// ---- チップに分けた条件 ----

#[test]
fn splits_top_level_clauses_for_chips() {
    let parsed = parse("夕景  種類:画像,動画 -未整理 (海 | 港)").unwrap();
    let clauses: Vec<(&str, bool, Option<&str>)> = parsed
        .clauses
        .iter()
        .map(|c: &Clause| (c.text.as_str(), c.negated, c.field))
        .collect();
    assert_eq!(
        clauses,
        vec![
            ("夕景", false, None),
            ("種類:画像,動画", false, Some("type")),
            ("-未整理", true, None),
            ("(海 | 港)", false, None),
        ]
    );
    assert_eq!((parsed.clauses[1].start, parsed.clauses[1].end), (4, 12));

    // OR だけの式は全体で 1 つ
    let parsed = parse("海 | 港").unwrap();
    assert_eq!(parsed.clauses.len(), 1);
    assert!(parse("").unwrap().clauses.is_empty());
}
