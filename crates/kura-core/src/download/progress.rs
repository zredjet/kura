//! ツールが出す `KURA` の行（`args` で指定したもの）を読み、進捗の割合と画面の一行にする。

use std::path::PathBuf;

use super::args::MARK;

/// 読み取った 1 行。
#[derive(Debug, Clone, PartialEq)]
pub enum ToolLine {
    /// yt-dlp：これから取る 1 本（再生リストの番号・本数、形式ごとの大きさ、題名）。
    Plan {
        index: Option<u64>,
        count: Option<u64>,
        sizes: Vec<u64>,
        title: Option<String>,
    },
    /// yt-dlp：取得中の形式の進み具合。
    Download {
        downloaded: u64,
        total: Option<u64>,
        speed: Option<f64>,
        eta: Option<u64>,
        format_id: String,
    },
    /// gallery-dl：これから取る 1 ファイル（投稿の中の番号・枚数、題名）。
    Prepare {
        post: String,
        count: Option<u64>,
        title: Option<String>,
    },
    /// できたファイル。
    File(PathBuf),
    /// 取得済みとして飛ばしたファイル（gallery-dl）。
    Skip(PathBuf),
}

/// `KURA` の行なら読む。yt-dlp の「取得済みの記録にある」行は飛ばしたものとして読む。
/// それ以外（ツールの普段の出力）は None。
pub fn parse_line(line: &str) -> Option<ToolLine> {
    let line = line.trim_end_matches(['\r', '\n']);
    if let Some(rest) = line.strip_prefix("[download] ")
        && let Some(id) = rest.strip_suffix(": has already been recorded in the archive")
    {
        return Some(ToolLine::Skip(PathBuf::from(id)));
    }
    let mut fields = line.split('\t');
    if fields.next()? != MARK {
        return None;
    }
    let kind = fields.next()?;
    let rest: Vec<&str> = fields.collect();
    let field = |i: usize| rest.get(i).copied().unwrap_or("");
    Some(match kind {
        "plan" => {
            // 形式ごとの大きさ。わからない形式は見積もりで埋める
            let (exact, approx) = (number_list(field(3)), number_list(field(4)));
            let sizes: Vec<u64> = (0..exact.len().max(approx.len()))
                .filter_map(|i| {
                    exact
                        .get(i)
                        .copied()
                        .flatten()
                        .or_else(|| approx.get(i).copied().flatten())
                })
                .collect();
            let single = number(field(5));
            ToolLine::Plan {
                index: number(field(1)),
                count: number(field(2)),
                sizes: if sizes.is_empty() {
                    single.into_iter().collect()
                } else {
                    sizes
                },
                title: json_string(field(6)),
            }
        }
        "dl" => ToolLine::Download {
            downloaded: number(field(0))?,
            total: number(field(1)).or_else(|| number(field(2))),
            speed: field(3)
                .parse::<f64>()
                .ok()
                .filter(|s| s.is_finite() && *s >= 0.0),
            eta: number(field(4)),
            format_id: field(5).to_string(),
        },
        "prepare" => ToolLine::Prepare {
            post: format!("{}:{}", field(0), field(1)),
            count: number(field(3)),
            title: json_string(field(4)),
        },
        "file" if !rest.is_empty() => ToolLine::File(PathBuf::from(rest.join("\t"))),
        "skip" if !rest.is_empty() => ToolLine::Skip(PathBuf::from(rest.join("\t"))),
        _ => return None,
    })
}

/// 整数（`NA`・`None`・小数は小数点以下を切り捨て）。
fn number(s: &str) -> Option<u64> {
    let s = s.trim();
    s.parse::<u64>().ok().or_else(|| {
        s.parse::<f64>()
            .ok()
            .filter(|f| f.is_finite() && *f >= 0.0)
            .map(|f| f as u64)
    })
}

/// `[1, 2, null]` の形の JSON の数の列。読めなければ空。null は None。
fn number_list(s: &str) -> Vec<Option<u64>> {
    serde_json::from_str::<Vec<Option<f64>>>(s)
        .unwrap_or_default()
        .into_iter()
        .map(|v| v.filter(|f| f.is_finite() && *f >= 0.0).map(|f| f as u64))
        .collect()
}

/// JSON の文字列（`"題名"`）。null・空・読めないものは None。
fn json_string(s: &str) -> Option<String> {
    serde_json::from_str::<Option<String>>(s)
        .ok()
        .flatten()
        .filter(|t| !t.trim().is_empty() && t != "None")
}

/// ダウンロードの進み具合。行を順に `apply` し、`fraction` と `message` で読む。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Progress {
    /// いま取っている 1 本・1 投稿の題名（最初のもの）。
    pub title: Option<String>,
    /// できたファイルの数。
    pub files: u64,
    /// 取得済みとして飛ばしたファイルの数。
    pub skipped: u64,
    // yt-dlp
    index: u64,
    count: u64,
    sizes: Vec<u64>,
    format_pos: usize,
    format_id: Option<String>,
    done_bytes: u64,
    current_bytes: u64,
    current_total: Option<u64>,
    speed: Option<f64>,
    eta: Option<u64>,
    // gallery-dl
    posts: Vec<String>,
    post_count: Option<u64>,
}

impl Progress {
    /// 1 行を反映する。
    pub fn apply(&mut self, line: &ToolLine) {
        match line {
            ToolLine::Plan {
                index,
                count,
                sizes,
                title,
            } => {
                self.index = index.unwrap_or(1).max(1);
                self.count = count.unwrap_or(1).max(1);
                self.sizes = sizes.clone();
                self.format_pos = 0;
                self.format_id = None;
                self.done_bytes = 0;
                self.current_bytes = 0;
                self.current_total = None;
                self.speed = None;
                self.eta = None;
                if self.title.is_none() {
                    self.title = title.clone();
                }
            }
            ToolLine::Download {
                downloaded,
                total,
                speed,
                eta,
                format_id,
            } => {
                if self.format_id.as_deref() != Some(format_id.as_str()) {
                    if self.format_id.is_some() {
                        // 前の形式（映像のあとの音声など）は取り終えた
                        self.done_bytes += self
                            .current_total
                            .unwrap_or(self.current_bytes)
                            .max(self.current_bytes);
                        self.format_pos += 1;
                    }
                    self.format_id = Some(format_id.clone());
                }
                self.current_bytes = *downloaded;
                self.current_total = *total;
                self.speed = *speed;
                self.eta = *eta;
            }
            ToolLine::Prepare { post, count, title } => {
                if !self.posts.contains(post) {
                    self.posts.push(post.clone());
                    self.post_count = *count;
                }
                if self.title.is_none() {
                    self.title = title.clone();
                }
            }
            ToolLine::File(_) => self.files += 1,
            ToolLine::Skip(_) => self.skipped += 1,
        }
    }

    /// 全体の割合（わからなければ None）。
    pub fn fraction(&self) -> Option<f64> {
        if self.count > 0 {
            let item = self.item_fraction()?;
            return Some(((self.index - 1) as f64 + item) / self.count as f64);
        }
        // gallery-dl：1 投稿だけなら枚数で
        match (self.posts.len(), self.post_count) {
            (1, Some(total)) if total > 0 => {
                Some(((self.files + self.skipped) as f64 / total as f64).min(1.0))
            }
            _ => None,
        }
    }

    /// いまの 1 本の割合。
    fn item_fraction(&self) -> Option<f64> {
        let total: u64 = self.sizes.iter().sum();
        let done = self.done_bytes + self.current_bytes;
        if total > 0 {
            return Some((done as f64 / total as f64).min(1.0));
        }
        let current_total = self.current_total.filter(|t| *t > 0)?;
        Some((self.current_bytes as f64 / current_total as f64).min(1.0))
    }

    /// 画面の一行（「1.2 GB / 2.9 GB · 12.4 MB/s · 残り 2分」「38 / 112 枚」など）。
    pub fn message(&self) -> String {
        let mut parts = Vec::new();
        if self.count > 0 {
            if self.count > 1 {
                parts.push(format!("{} / {} 本", self.index, self.count));
            }
            let total: u64 = self.sizes.iter().sum();
            let done = self.done_bytes + self.current_bytes;
            if total > 0 {
                parts.push(format!("{} / {}", bytes(done), bytes(total)));
            } else if let Some(t) = self.current_total {
                parts.push(format!("{} / {}", bytes(self.current_bytes), bytes(t)));
            } else if done > 0 {
                parts.push(bytes(done));
            }
            if let Some(speed) = self.speed {
                parts.push(format!("{}/s", bytes(speed as u64)));
            }
            if let Some(eta) = self.eta {
                parts.push(format!("残り {}", duration(eta)));
            }
        } else if !self.posts.is_empty() || self.files + self.skipped > 0 {
            let done = self.files + self.skipped;
            match (self.posts.len(), self.post_count) {
                (1, Some(total)) => parts.push(format!("{done} / {total} 枚")),
                _ => parts.push(format!("{done} 枚")),
            }
            if self.skipped > 0 {
                parts.push(format!("取得済みの {} 枚はスキップ", self.skipped));
            }
        }
        if parts.is_empty() {
            "準備中".to_string()
        } else {
            parts.join(" · ")
        }
    }
}

/// 「820 KB」「63.5 MB」「1.2 GB」。
pub fn bytes(n: u64) -> String {
    const UNITS: [&str; 4] = ["KB", "MB", "GB", "TB"];
    if n < 1000 {
        return format!("{n} B");
    }
    let mut value = n as f64 / 1000.0;
    let mut unit = 0;
    while value >= 1000.0 && unit + 1 < UNITS.len() {
        value /= 1000.0;
        unit += 1;
    }
    if value >= 100.0 || unit == 0 {
        format!("{} {}", value.round() as u64, UNITS[unit])
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// 「45秒」「2分」「1時間5分」。
pub fn duration(seconds: u64) -> String {
    match seconds {
        0..60 => format!("{seconds}秒"),
        60..3600 => format!("{}分", seconds.div_ceil(60)),
        _ => {
            let minutes = seconds.div_ceil(60);
            format!("{}時間{}分", minutes / 60, minutes % 60)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(s: &str) -> ToolLine {
        parse_line(s).unwrap()
    }

    #[test]
    fn reads_marked_lines_only() {
        assert_eq!(parse_line("[download] 10%"), None);
        assert_eq!(
            parse_line("[download] 9fcbOOzzFWM: has already been recorded in the archive"),
            Some(ToolLine::Skip("9fcbOOzzFWM".into()))
        );
        assert_eq!(parse_line("/path/to/file.jpg"), None);
        assert_eq!(parse_line("KURA\tunknown\t1"), None);
        assert_eq!(
            line("KURA\tfile\t/a/b c.mp4\n"),
            ToolLine::File("/a/b c.mp4".into())
        );
        assert_eq!(
            line(
                "KURA\tplan\tabc\tNA\tNA\t[61062647, 2478967]\t[61062646, null]\t63541614\t\"題名\""
            ),
            ToolLine::Plan {
                index: None,
                count: None,
                sizes: vec![61062647, 2478967],
                title: Some("題名".into()),
            }
        );
        // 大きさがわからなければ見積もり、それもなければ 1 本の大きさ
        assert_eq!(
            line("KURA\tplan\tabc\t2\t5\tNA\t[100, 200]\tNA\tnull"),
            ToolLine::Plan {
                index: Some(2),
                count: Some(5),
                sizes: vec![100, 200],
                title: None,
            }
        );
        assert_eq!(
            line("KURA\tplan\tabc\tNA\tNA\tNA\tNA\t5000\t\"x\""),
            ToolLine::Plan {
                index: None,
                count: None,
                sizes: vec![5000],
                title: Some("x".into()),
            }
        );
        assert_eq!(
            line("KURA\tdl\t1024\tNA\t2000.5\t70659.5\t864\t401"),
            ToolLine::Download {
                downloaded: 1024,
                total: Some(2000),
                speed: Some(70659.5),
                eta: Some(864),
                format_id: "401".into(),
            }
        );
        assert_eq!(
            line("KURA\tprepare\tpixiv\t147501205\t0\t2\t\"ひとやすみ\""),
            ToolLine::Prepare {
                post: "pixiv:147501205".into(),
                count: Some(2),
                title: Some("ひとやすみ".into()),
            }
        );
        assert_eq!(
            line("KURA\tprepare\ttwitter\t1\tNone\tNone\tNone"),
            ToolLine::Prepare {
                post: "twitter:1".into(),
                count: None,
                title: None,
            }
        );
    }

    #[test]
    fn ytdlp_progress_spans_video_and_audio() {
        let mut p = Progress::default();
        p.apply(&line(
            "KURA\tplan\tid\tNA\tNA\t[900, 100]\tNA\tNA\t\"動画\"",
        ));
        p.apply(&line("KURA\tdl\t450\t900\tNA\t1000000\t10\t401"));
        assert_eq!(p.fraction(), Some(0.45));
        assert_eq!(p.title.as_deref(), Some("動画"));
        assert_eq!(p.message(), "450 B / 1 KB · 1.0 MB/s · 残り 10秒");
        p.apply(&line("KURA\tdl\t900\t900\tNA\tNA\t0\t401"));
        p.apply(&line("KURA\tdl\t50\t100\tNA\tNA\tNA\t140"));
        assert_eq!(p.fraction(), Some(0.95));
        p.apply(&line("KURA\tfile\t/w/id.mp4"));
        assert_eq!(p.files, 1);
    }

    #[test]
    fn ytdlp_playlists_count_videos() {
        let mut p = Progress::default();
        p.apply(&line("KURA\tplan\ta\t3\t4\tNA\tNA\t1000\t\"1 本目\""));
        p.apply(&line("KURA\tdl\t500\t1000\tNA\tNA\tNA\t18"));
        assert_eq!(p.fraction(), Some(0.625));
        assert!(p.message().starts_with("3 / 4 本 · "));
    }

    #[test]
    fn gallerydl_counts_files() {
        let mut p = Progress::default();
        assert_eq!(p.message(), "準備中");
        p.apply(&line("KURA\tprepare\tpixiv\t1\t0\t4\t\"作品\""));
        p.apply(&line("KURA\tfile\t/w/1_p0.jpg"));
        p.apply(&line("KURA\tprepare\tpixiv\t1\t1\t4\t\"作品\""));
        p.apply(&line("KURA\tskip\t/w/1_p1.jpg"));
        assert_eq!(p.fraction(), Some(0.5));
        assert_eq!(p.message(), "2 / 4 枚 · 取得済みの 1 枚はスキップ");
        // 投稿が複数なら全体の枚数はわからない
        p.apply(&line("KURA\tprepare\tpixiv\t2\t0\t3\t\"別\""));
        assert_eq!(p.fraction(), None);
        assert_eq!(p.message(), "2 枚 · 取得済みの 1 枚はスキップ");
    }

    #[test]
    fn formats_sizes_and_durations() {
        assert_eq!(bytes(999), "999 B");
        assert_eq!(bytes(820_000), "820 KB");
        assert_eq!(bytes(63_541_614), "63.5 MB");
        assert_eq!(bytes(2_900_000_000), "2.9 GB");
        assert_eq!(duration(45), "45秒");
        assert_eq!(duration(90), "2分");
        assert_eq!(duration(3900), "1時間5分");
    }
}
