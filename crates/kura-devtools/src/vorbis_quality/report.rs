//! `results.md` を作る。**表の形はこの冒頭が正**（指標の定義は [`metrics`] の冒頭）。出どころは M10 段 7 の品質の調査が
//! 出した記録の表の案（作業の記録。リポジトリにはない。決めたことは `docs/decisions.md` の ADR-149 (13)）。
//! 時刻を書かないので、同じ入力なら同じ文書になる（コミットの欄を除く）。
//!
//! 1. **頭書き**：道具の版（[`TOOL_ID`]）・実行の引数・kura のコミット・エンコーダの識別子と段・デコーダ（指標は symphonia、
//!    長さと値の照合は lewton）・乱数の種・聴取のレベルの仮定（1019.5 Hz の満幅の正弦 = 92 dB SPL）・同じビットレートでの
//!    比較の基準の族。続けて指標の短い定義、試験の音（作り方・レート・チャンネル・秒・SHA-256）、参照（音・ラベル・族・段・
//!    ファイル・SHA-256）。
//! 2. **表 0**：飛ばした（書けない段）・測れなかった測定を、エンコーダ・種類・理由ごとにまとめる（段・音）。
//! 3. **表 1：段ごとの記録**。1 行 1 測定（音 × 段 × エンコーダ）。欄：音声 kbps（音声のパケット（4 つ目から）のバイト × 8 ÷
//!    元の秒、（　）は識別ヘッダの公称）・ヘッダ B（識別・コメント・setup の 3 パケット）・ファイル B・上限 kHz・NMR 全体・
//!    p95・最悪 dB@秒・乱れたフレーム・LSD・長さ（symphonia／lewton が読んだフレームの数。「=」は元と同じ）・ずれ（標本）。
//!    並びは音・段・族（kura が先、参照は渡した順）・ラベル。
//! 4. **表 2：同じビットレートでの比較**。音ごと・族ごとに、基準の族の曲線に対する点の数・BD-rate・各点の ΔNMR（段@kbps）・
//!    同じ段の上限の差（kHz）。基準の族の点が 2 つ未満の音は作らない。
//! 5. **表 3：長さとずれ**。音ごとに、測定の数と、そのうち symphonia・lewton の長さが元と同じもの・ずれが 0 のもの・音声が
//!    1 ページだけのもの（lewton・ffmpeg が最後のページのグラニュールで末尾を切らない形）の数、lewton と symphonia の値の差の
//!    最大、例外（6 件まで）。
//! 6. **表 4：短い効果音**（[`SHORT_SECONDS`] 秒より短い音）。エンコーダ・段ごとに、音の数と、音声 kbps・ファイル kbps・
//!    ヘッダの割合の中央値、NMR 全体の中央値・最悪、フレームの最悪の NMR の中央値、乱れたフレームの中央値、長さが違う数。
//!
//! 数の書き方：dB は小数 1 桁（±290 dB を超えるもの（無音・同じもの）は ±∞）、上限は kHz の小数 1 桁（「なし」は低域通過が
//! 見つからない）、バイト・フレームの数は 3 桁ごとのカンマ、乱れたフレームは小数 2 桁、最悪の時刻は秒の小数 2 桁。

use std::collections::BTreeSet;

use super::metrics::{self, LISTENING_DB_SPL};
use super::{
    Context, KURA, LEWTON_ID, Measured, Outcome, Reference, Row, SHORT_SECONDS, Sound, Source,
    TOOL_ID,
};

/// 行を足す。
macro_rules! line {
    ($out:expr) => { $out.push('\n') };
    ($out:expr, $($arg:tt)*) => {{ $out.push_str(&format!($($arg)*)); $out.push('\n'); }};
}

fn db1(x: f64) -> String {
    if x <= -290.0 {
        "−∞".into()
    } else if x >= 290.0 {
        "+∞".into()
    } else {
        format!("{x:.1}")
    }
}

fn opt1(x: Option<f64>) -> String {
    x.map_or_else(|| "—".into(), db1)
}

fn khz(c: Option<f64>) -> String {
    c.map_or_else(|| "なし".into(), |v| format!("{:.1}", v / 1000.0))
}

fn thousands(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, ch) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

/// 表のエンコーダの欄：ラベルが「族-q段」ならば族だけ（段は別の欄にある）。
fn encoder_cell(r: &Row) -> String {
    match r.q {
        Some(q) if r.label == format!("{}-q{q}", r.family) => r.family.clone(),
        _ => r.label.clone(),
    }
}

fn q_str(q: Option<i32>) -> String {
    q.map_or_else(|| "—".into(), |q| format!("q{q}"))
}

/// 表の欄に入れる文字（縦棒と改行を逃がす）。
fn cell(s: &str) -> String {
    s.replace('|', "\\|").replace('\n', " ")
}

fn len_cell(m: &Measured, frames_ref: usize) -> String {
    let one = |n: usize| {
        if n == frames_ref {
            "=".to_string()
        } else {
            thousands(n as u64)
        }
    };
    let lew = m.frames_lewton.map_or_else(|| "読めない".into(), one);
    format!("{}／{lew}", one(m.frames_symphonia))
}

/// 族の並び：kura が先、参照は渡した順。
fn family_order(refs: &[Reference]) -> Vec<String> {
    let mut v = vec![KURA.to_string()];
    for r in refs {
        if !v.contains(&r.family) {
            v.push(r.family.clone());
        }
    }
    v
}

fn rank(order: &[String], family: &str) -> usize {
    order
        .iter()
        .position(|f| f == family)
        .unwrap_or(order.len())
}

/// 文書を作る。
pub fn render(ctx: &Context, sounds: &[Sound], refs: &[Reference], rows: &[Row]) -> String {
    let mut out = String::new();
    let order = family_order(refs);
    header(&mut out, ctx, sounds, refs);
    skipped(&mut out, sounds, rows);
    table1(&mut out, sounds, rows, &order);
    table2(&mut out, ctx, sounds, rows, &order);
    table3(&mut out, sounds, rows);
    table4(&mut out, sounds, rows, &order);
    out
}

fn header(out: &mut String, ctx: &Context, sounds: &[Sound], refs: &[Reference]) {
    line!(out, "# Ogg Vorbis の品質の記録");
    line!(out);
    line!(out, "- 道具：`{TOOL_ID}`");
    line!(
        out,
        "- 実行：`cargo run -p kura-devtools --release -- vorbis-quality {}`",
        ctx.args.join(" ")
    );
    line!(out, "- kura のコミット：`{}`", ctx.commit);
    line!(
        out,
        "- エンコーダ：`{}`（段：{}）",
        ctx.encoder_id,
        ctx.qs
            .iter()
            .map(|q| q.to_string())
            .collect::<Vec<_>>()
            .join("・")
    );
    line!(
        out,
        "- デコーダ：指標は `{}`（symphonia。中で OS の libm を使うので、別の機械では小数の末尾が変わりうる）で読んだ値、長さと値の照合に {LEWTON_ID}（最後のページのグラニュールで切る）",
        ctx.decoder_id
    );
    line!(
        out,
        "- 乱数の種：{}（合成の音は ChaCha8 で記述から作り、16 ビットに丸めた値を元とする）",
        ctx.seed
    );
    line!(
        out,
        "- 聴取のレベルの仮定：1019.5 Hz の満幅の正弦 = {LISTENING_DB_SPL} dB SPL"
    );
    line!(
        out,
        "- 同じビットレートでの比較の基準：{}",
        ctx.curve_ref.as_deref().unwrap_or("（参照なし）")
    );
    line!(out);
    line!(out, "## 指標");
    line!(out);
    line!(
        out,
        "- **kbps**：音声のパケット（4 つ目から）のバイト × 8 ÷ 元の長さ。ヘッダ B は識別・コメント・setup の 3 パケット、ファイル B はファイル全体。（　）は識別ヘッダの公称のビットレート。"
    );
    line!(
        out,
        "- **上限**：長い時間の電力スペクトル（周期の Hann 2,048 点・半分の重なり、チャンネルの和）で、[f, fs/2] の出力のエネルギーが元の −20 dB を下回る最小の f（f > 1 kHz、元に全体の 1e-6 以上のエネルギーがある範囲）。「なし」は低域通過が見つからない。"
    );
    line!(
        out,
        "- **NMR**：PEAQ（BS.1387-1）の基本版の FFT の耳のモデルに近いもの（2,048 点・半分の重なりの Hann、外耳・中耳の重み、80 Hz〜18 kHz を 0.25 Bark の 109 帯域、周波数・時間の広がり、マスクの差 3 dB／0.25k dB）。雑音は振幅スペクトルの差。**全体**はチャンネルごとのフレームの線形の平均の dB をチャンネルで平均、**p95**・**最悪**はフレームの NMR（dB）の 95 百分位・最大（@ はそのフレームの始まりの秒）、**乱れたフレーム**は帯域の最大が 1.5 dB を超えるフレームの割合。負ほど良い。適合の試験はしていない。"
    );
    line!(
        out,
        "- **LSD**：50 Hz〜16 kHz、元のフレームの電力が最大の −60 dB 以上のフレーム、フレームの最大の −80 dB 以上のビンの dB の差の二乗平均の平方根の平均。"
    );
    line!(
        out,
        "- **長さ**：symphonia／lewton の読んだ長さ（「=」は元と同じ）。**ずれ**：相互相関（±4,096 標本、先頭 131,072 標本、チャンネルの和）の最大の遅れ。指標は ずれ をそろえ、共通の長さで測る。"
    );
    line!(
        out,
        "- フレームは信号の終わりまで覆う（最後の半端は 0 で埋める）。全体の値は `results.jsonl`（SNR・帯域ごとの SNR・ページの数なども）。"
    );
    line!(out);
    line!(out, "## 試験の音");
    line!(out);
    line!(out, "| 音 | 作り方 | レート | ch | 秒 | SHA-256 |");
    line!(out, "|---|---|---:|---:|---:|---|");
    for s in sounds {
        let how = match &s.source {
            Source::Synth { description } => {
                format!("合成：{description}（WAV は `signals/{}.wav`）", s.name)
            }
            Source::Input { path } => format!("入力：`{}`", path.display()),
        };
        line!(
            out,
            "| {} | {} | {} | {} | {:.3} | `{}` |",
            s.name,
            cell(&how),
            s.rate,
            s.planes.len(),
            s.seconds(),
            s.sha256
        );
    }
    line!(out);
    if !refs.is_empty() {
        line!(out, "## 参照");
        line!(out);
        line!(out, "| 音 | ラベル | 族 | 段 | ファイル | SHA-256 |");
        line!(out, "|---|---|---|---|---|---|");
        for r in refs {
            line!(
                out,
                "| {} | {} | {} | {} | `{}` | `{}` |",
                r.sound,
                r.label,
                r.family,
                q_str(r.q),
                cell(&r.path.display().to_string()),
                r.sha256
            );
        }
        line!(out);
    }
}

/// 表 0：飛ばした・測れなかった測定（エンコーダ・理由ごと）。
fn skipped(out: &mut String, sounds: &[Sound], rows: &[Row]) {
    struct Group {
        family: String,
        kind: &'static str,
        reason: String,
        qs: BTreeSet<i32>,
        other_q: bool,
        sounds: Vec<usize>,
    }
    let mut groups: Vec<Group> = Vec::new();
    for row in rows {
        let (kind, reason) = match &row.outcome {
            Outcome::Ok(_) => continue,
            Outcome::Skipped(r) => ("書けない", r),
            Outcome::Failed(r) => ("測れない", r),
        };
        let at = groups
            .iter()
            .position(|g| g.family == row.family && g.kind == kind && &g.reason == reason);
        let g = match at {
            Some(i) => &mut groups[i],
            None => {
                groups.push(Group {
                    family: row.family.clone(),
                    kind,
                    reason: reason.clone(),
                    qs: BTreeSet::new(),
                    other_q: false,
                    sounds: Vec::new(),
                });
                let last = groups.len() - 1;
                &mut groups[last]
            }
        };
        match row.q {
            Some(q) => {
                g.qs.insert(q);
            }
            None => g.other_q = true,
        }
        if !g.sounds.contains(&row.sound) {
            g.sounds.push(row.sound);
        }
    }
    line!(out, "## 表 0：飛ばした・測れなかった測定");
    line!(out);
    if groups.is_empty() {
        line!(out, "なし。");
        line!(out);
        return;
    }
    line!(out, "| エンコーダ | 種類 | 段 | 理由 | 音 |");
    line!(out, "|---|---|---|---|---|");
    for g in &groups {
        let mut qs: Vec<String> = g.qs.iter().map(|q| q.to_string()).collect();
        if g.other_q {
            qs.push("—".into());
        }
        let names = if g.sounds.len() == sounds.len() && sounds.len() > 1 {
            format!("すべて（{} 音）", sounds.len())
        } else {
            g.sounds
                .iter()
                .map(|&i| sounds[i].name.as_str())
                .collect::<Vec<_>>()
                .join("、")
        };
        line!(
            out,
            "| {} | {} | {} | {} | {} |",
            g.family,
            g.kind,
            qs.join("・"),
            cell(&g.reason),
            names
        );
    }
    line!(out);
}

/// 表 1：段ごとの記録。
fn table1(out: &mut String, sounds: &[Sound], rows: &[Row], order: &[String]) {
    let mut idx: Vec<usize> = (0..rows.len())
        .filter(|&i| rows[i].measured().is_some())
        .collect();
    idx.sort_by_key(|&i| {
        let r = &rows[i];
        (
            r.sound,
            r.q.unwrap_or(i32::MAX),
            rank(order, &r.family),
            r.label.clone(),
        )
    });
    line!(out, "## 表 1：段ごとの記録");
    line!(out);
    if idx.is_empty() {
        line!(out, "測れた測定がありません（表 0）。");
        line!(out);
        return;
    }
    line!(
        out,
        "| 音 | 段 | エンコーダ | 音声 kbps（公称） | ヘッダ B | ファイル B | 上限 kHz | NMR 全体 dB | NMR p95 dB | NMR 最悪 dB@秒 | 乱れたフレーム | LSD dB | 長さ（symphonia／lewton） | ずれ |"
    );
    line!(
        out,
        "|---|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---|---:|"
    );
    for i in idx {
        let r = &rows[i];
        let Some(m) = r.measured() else { continue };
        let s = &sounds[r.sound];
        let nominal = m
            .kbps_nominal
            .map_or_else(String::new, |k| format!("（{k:.0}）"));
        line!(
            out,
            "| {} | {} | {} | {:.1}{} | {} | {} | {} | {} | {} | {}@{:.2} | {:.2} | {} | {} | {} |",
            s.name,
            q_str(r.q),
            encoder_cell(r),
            m.kbps_audio,
            nominal,
            thousands(m.stats.header_bytes),
            thousands(m.stats.file_bytes),
            khz(m.spectral.cutoff_hz),
            db1(m.nmr.total_db),
            db1(m.nmr.p95_db),
            db1(m.nmr.max_db),
            m.nmr.worst_at_s,
            m.nmr.rel_dist_frames,
            opt1(m.spectral.lsd_db),
            len_cell(m, s.frames()),
            m.lag
        );
    }
    line!(out);
}

/// 表 2：同じビットレートでの比較（基準の族の率と品質の曲線に対して）。
fn table2(out: &mut String, ctx: &Context, sounds: &[Sound], rows: &[Row], order: &[String]) {
    line!(out, "## 表 2：同じビットレートでの比較");
    line!(out);
    let Some(base) = ctx.curve_ref.as_deref() else {
        line!(out, "参照（`--reference`）がないので作らなかった。");
        line!(out);
        return;
    };
    line!(
        out,
        "基準は {base} の曲線（音声 kbps の log に対して区分線形、率の順に NMR が良くなる点だけの包絡）。ΔNMR は同じビットレートの基準との差（正は悪い）、BD-rate は NMR を品質の軸にして重なる範囲で同じ NMR に要るビットの差（正は多く要る）、上限の差は同じ段の基準との差。"
    );
    line!(out);
    let mut body = Vec::new();
    for (si, s) in sounds.iter().enumerate() {
        let points = |family: &str| -> Vec<(&Row, &Measured)> {
            let mut v: Vec<(&Row, &Measured)> = rows
                .iter()
                .filter(|r| r.sound == si && r.family == family)
                .filter_map(|r| r.measured().map(|m| (r, m)))
                .collect();
            v.sort_by(|a, b| a.1.kbps_audio.total_cmp(&b.1.kbps_audio));
            v
        };
        let base_pts = points(base);
        if base_pts.len() < 2 {
            continue;
        }
        let curve = |pts: &[(&Row, &Measured)]| {
            metrics::envelope(
                &pts.iter()
                    .map(|(_, m)| (m.kbps_audio, m.nmr.total_db))
                    .collect::<Vec<_>>(),
            )
        };
        let env = curve(&base_pts);
        for family in order.iter().filter(|f| f.as_str() != base) {
            let pts = points(family);
            if pts.is_empty() {
                continue;
            }
            let bd = if pts.len() < 2 {
                "—（点が 1 つ）".to_string()
            } else {
                metrics::bd_rate(&env, &curve(&pts))
                    .map_or_else(|| "重ならない".into(), |v| format!("{v:+.0}%"))
            };
            let deltas: Vec<String> = pts
                .iter()
                .map(|(r, m)| {
                    let at = format!("{}@{:.0}", q_str(r.q), m.kbps_audio);
                    match metrics::nmr_at_rate(&env, m.kbps_audio) {
                        Some(v) => format!("{at} {:+.1}", m.nmr.total_db - v),
                        None => format!("{at} 範囲外"),
                    }
                })
                .collect();
            let cut: Vec<String> = pts
                .iter()
                .filter_map(|(r, m)| {
                    let q = r.q?;
                    let (_, b) = base_pts.iter().find(|(br, _)| br.q == Some(q))?;
                    Some(match (m.spectral.cutoff_hz, b.spectral.cutoff_hz) {
                        (Some(x), Some(y)) => format!("q{q} {:+.1}", (x - y) / 1000.0),
                        (None, None) => format!("q{q} ともになし"),
                        (x, y) => format!("q{q} {}／{}", khz(x), khz(y)),
                    })
                })
                .collect();
            body.push(format!(
                "| {} | {} | {} | {} | {} | {} |",
                s.name,
                family,
                pts.len(),
                bd,
                deltas.join("、"),
                if cut.is_empty() {
                    "—".into()
                } else {
                    cut.join("、")
                }
            ));
        }
    }
    if body.is_empty() {
        line!(
            out,
            "基準の族の点が 2 つ以上ある音がないか、比べる側が測れていない。"
        );
        line!(out);
        return;
    }
    line!(
        out,
        "| 音 | 比べる側 | 点 | BD-rate（NMR） | 同じビットレートでの ΔNMR dB（段@kbps） | 同じ段の上限の差 kHz |"
    );
    line!(out, "|---|---|---:|---:|---|---|");
    for b in body {
        line!(out, "{b}");
    }
    line!(out);
}

/// 表 3：長さとずれ（音ごと）。
fn table3(out: &mut String, sounds: &[Sound], rows: &[Row]) {
    line!(out, "## 表 3：長さとずれ");
    line!(out);
    let mut body = Vec::new();
    for (si, s) in sounds.iter().enumerate() {
        let ms: Vec<(&Row, &Measured)> = rows
            .iter()
            .filter(|r| r.sound == si)
            .filter_map(|r| r.measured().map(|m| (r, m)))
            .collect();
        if ms.is_empty() {
            continue;
        }
        let n = s.frames();
        let count = |f: &dyn Fn(&Measured) -> bool| ms.iter().filter(|(_, m)| f(m)).count();
        let sym = count(&|m| m.frames_symphonia == n);
        let lew = count(&|m| m.frames_lewton == Some(n));
        let lag0 = count(&|m| m.lag == 0);
        let one_page = count(&|m| m.stats.audio_pages == 1);
        let diff = ms
            .iter()
            .filter_map(|(_, m)| m.lewton_max_abs_diff)
            .fold(None, |a: Option<f64>, d| Some(a.map_or(d, |a| a.max(d))));
        let mut ex: Vec<String> = ms
            .iter()
            .filter(|(_, m)| m.frames_symphonia != n || m.frames_lewton != Some(n) || m.lag != 0)
            .map(|(r, m)| {
                let lew = m
                    .frames_lewton
                    .map_or_else(|| "読めない".into(), |v| thousands(v as u64));
                format!(
                    "{}（symphonia {}・lewton {}・ずれ {}・音声 {} ページ）",
                    r.label,
                    thousands(m.frames_symphonia as u64),
                    lew,
                    m.lag,
                    m.stats.audio_pages
                )
            })
            .collect();
        if ex.len() > 6 {
            let more = ex.len() - 6;
            ex.truncate(6);
            ex.push(format!("ほか {more}"));
        }
        body.push(format!(
            "| {} | {} | {} | {} | {} | {} | {} | {} | {} |",
            s.name,
            thousands(n as u64),
            ms.len(),
            sym,
            lew,
            lag0,
            one_page,
            diff.map_or_else(|| "—".into(), |d| format!("{d:.1e}")),
            if ex.is_empty() {
                "—".into()
            } else {
                ex.join("、")
            }
        ));
    }
    if body.is_empty() {
        line!(out, "測れた測定がありません（表 0）。");
        line!(out);
        return;
    }
    line!(
        out,
        "測定の数のうち、symphonia・lewton の長さが元と同じもの、ずれが 0 のもの、音声が 1 ページだけのもの（lewton・ffmpeg が最後のページのグラニュールで末尾を切らない形）の数。"
    );
    line!(out);
    line!(
        out,
        "| 音 | 元の長さ | 測定 | symphonia＝元 | lewton＝元 | ずれ 0 | 音声 1 ページ | lewton と symphonia の差の最大 | 例外 |"
    );
    line!(out, "|---|---:|---:|---:|---:|---:|---:|---:|---|");
    for b in body {
        line!(out, "{b}");
    }
    line!(out);
}

/// 表 4：短い効果音（`SHORT_SECONDS` 秒より短い音）をエンコーダ・段ごとにまとめる。
fn table4(out: &mut String, sounds: &[Sound], rows: &[Row], order: &[String]) {
    line!(out, "## 表 4：短い効果音（{SHORT_SECONDS} 秒より短い音）");
    line!(out);
    let mut keys: Vec<(usize, i32, String)> = Vec::new();
    for r in rows {
        if r.measured().is_some() && sounds[r.sound].seconds() < SHORT_SECONDS {
            let k = (
                rank(order, &r.family),
                r.q.unwrap_or(i32::MAX),
                r.family.clone(),
            );
            if !keys.contains(&k) {
                keys.push(k);
            }
        }
    }
    keys.sort();
    if keys.is_empty() {
        line!(out, "短い音の測定がありません。");
        line!(out);
        return;
    }
    line!(
        out,
        "| エンコーダ | 段 | 音 | 音声 kbps（中央値） | ファイル kbps（中央値） | ヘッダの割合（中央値） | NMR 全体 dB（中央値・最悪） | NMR 最悪のフレーム dB（中央値） | 乱れたフレーム（中央値） | 長さが違う |"
    );
    line!(out, "|---|---|---:|---:|---:|---:|---|---:|---:|---:|");
    for (_, q, family) in keys {
        let ms: Vec<(&Row, &Measured)> = rows
            .iter()
            .filter(|r| r.family == family && r.q.unwrap_or(i32::MAX) == q)
            .filter(|r| sounds[r.sound].seconds() < SHORT_SECONDS)
            .filter_map(|r| r.measured().map(|m| (r, m)))
            .collect();
        let col =
            |f: &dyn Fn(&Measured) -> f64| -> Vec<f64> { ms.iter().map(|(_, m)| f(m)).collect() };
        let total = col(&|m| m.nmr.total_db);
        let worst_total = total.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        let differ = ms
            .iter()
            .filter(|(r, m)| {
                let n = sounds[r.sound].frames();
                m.frames_symphonia != n || m.frames_lewton != Some(n)
            })
            .count();
        line!(
            out,
            "| {} | {} | {} | {:.1} | {:.1} | {:.0}% | {}・{} | {} | {:.2} | {} |",
            family,
            q_str((q != i32::MAX).then_some(q)),
            ms.len(),
            metrics::median(&col(&|m| m.kbps_audio)),
            metrics::median(&col(&|m| m.kbps_file)),
            100.0
                * metrics::median(&col(
                    &|m| m.stats.header_bytes as f64 / m.stats.file_bytes.max(1) as f64
                )),
            db1(metrics::median(&total)),
            db1(worst_total),
            db1(metrics::median(&col(&|m| m.nmr.max_db))),
            metrics::median(&col(&|m| m.nmr.rel_dist_frames)),
            differ
        );
    }
    line!(out);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formatting() {
        assert_eq!(thousands(384_000), "384,000");
        assert_eq!(thousands(999), "999");
        assert_eq!(thousands(1_000), "1,000");
        assert_eq!(db1(-300.0), "−∞");
        assert_eq!(db1(-9.78), "-9.8");
        assert_eq!(khz(Some(14_929.7)), "14.9");
        assert_eq!(khz(None), "なし");
        assert_eq!(q_str(Some(-1)), "q-1");
        assert_eq!(cell("a|b\nc"), "a\\|b c");
    }
}
