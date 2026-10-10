//! 取り込みオプションから、ファイルごとの所属（仮想フォルダ・タグ・ユニット）を決める純粋関数。

use serde::{Deserialize, Serialize};

use super::scan::ScanResult;
use crate::text::nfc;

/// 重複（内容が同一のファイルがすでにある）ときの扱い。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DuplicatePolicy {
    /// コピーはせず、フォルダ・タグ・ユニットへの所属だけ追加する（ゴミ箱にあれば戻す）。
    #[default]
    AddMemberships,
    /// 何もしない。
    Ignore,
}

/// 取り込みオプション（取り込みシートの「整理」と「すべてに付けるタグ」）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ImportOptions {
    /// 取り込み先の仮想フォルダ。
    pub dest_folder_id: Option<i64>,
    /// フォルダ構成を仮想フォルダとして再現する。
    pub reproduce_folders: bool,
    /// フォルダ名をタグとして付ける。
    pub folder_names_as_tags: bool,
    /// 末端フォルダごとに set ユニットを作る。
    pub units_per_folder: bool,
    /// ファイルの情報からタグを付ける（音楽のアーティスト・アルバム）。
    pub file_info_tags: bool,
    /// すべてに付けるタグ（「名前」または「名前空間:名前」）。
    pub tags: Vec<String>,
    pub duplicates: DuplicatePolicy,
}

/// タグの名前（名前空間なしは空文字）。
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TagName {
    pub namespace: String,
    pub name: String,
}

impl TagName {
    /// 名前空間なしのタグ。
    pub fn plain(name: impl Into<String>) -> Self {
        Self {
            namespace: String::new(),
            name: name.into(),
        }
    }

    /// 入力どおりの表記（別名の照合に使う）。
    pub fn display(&self) -> String {
        if self.namespace.is_empty() {
            self.name.clone()
        } else {
            format!("{}:{}", self.namespace, self.name)
        }
    }
}

/// 仮想フォルダの置き場所。`base`（なければいちばん上）の下に `path` の順にフォルダを作る。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FolderTarget {
    pub base: Option<i64>,
    pub path: Vec<String>,
}

/// ユニットを作る単位（末端フォルダ）。
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct UnitKey {
    pub root: usize,
    pub dirs: Vec<String>,
}

impl UnitKey {
    /// ユニット名（末端フォルダの名前）。
    pub fn name(&self) -> &str {
        self.dirs.last().map(String::as_str).unwrap_or_default()
    }

    /// 途中経過の JSON に保存するための文字列。
    pub fn storage_key(&self) -> String {
        format!("{}/{}", self.root, self.dirs.join("/"))
    }
}

/// 1 ファイルの所属。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Assignment {
    pub folder: Option<FolderTarget>,
    pub tags: Vec<TagName>,
    pub unit: Option<UnitKey>,
}

/// 「名前」または「名前空間:名前」を読む（全角の「：」も区切りとして扱う）。空なら None。
pub fn parse_tag(input: &str) -> Option<TagName> {
    let text = nfc(input.trim());
    let (namespace, name) = match text.char_indices().find(|(_, c)| *c == ':' || *c == '：') {
        Some((i, c)) => (
            text.get(..i).unwrap_or_default().trim(),
            text.get(i + c.len_utf8()..).unwrap_or_default().trim(),
        ),
        None => ("", text.as_str()),
    };
    if name.is_empty() {
        return None;
    }
    Some(TagName {
        namespace: namespace.to_string(),
        name: name.to_string(),
    })
}

/// 仮想フォルダの置き場所を決める。
///
/// - 再現する：ドロップしたフォルダを取り込み先に対応させ、その下にサブフォルダを作る。
///   取り込み先がなければ、ドロップしたフォルダ名でいちばん上に作る。
/// - 再現しない：取り込み先があればそこ。
pub fn folder_target(options: &ImportOptions, dirs: &[String]) -> Option<FolderTarget> {
    let base = options.dest_folder_id;
    if !options.reproduce_folders {
        return base.map(|b| FolderTarget {
            base: Some(b),
            path: Vec::new(),
        });
    }
    match base {
        Some(_) => Some(FolderTarget {
            base,
            path: dirs.iter().skip(1).cloned().collect(),
        }),
        None if dirs.is_empty() => None,
        None => Some(FolderTarget {
            base: None,
            path: dirs.to_vec(),
        }),
    }
}

/// フォルダ名のタグ（ドロップしたフォルダからファイルの親まで）。
pub fn folder_tags(dirs: &[String]) -> Vec<TagName> {
    dirs.iter()
        .map(|d| nfc(d.trim()))
        .filter(|d| !d.is_empty())
        .map(TagName::plain)
        .collect()
}

/// 表示名の初期値（ファイル名から拡張子を除いたもの）。
pub fn title_from_name(name: &str) -> String {
    match name.rfind('.') {
        Some(i) if i > 0 => name.get(..i).unwrap_or(name).to_string(),
        _ => name.to_string(),
    }
}

/// 走査結果の各ファイルの所属を決める（`scan.files` と同じ並び）。
pub fn assign(scan: &ScanResult, options: &ImportOptions) -> Vec<Assignment> {
    let mut common: Vec<TagName> = Vec::new();
    for tag in options.tags.iter().filter_map(|t| parse_tag(t)) {
        if !common.contains(&tag) {
            common.push(tag);
        }
    }
    scan.files
        .iter()
        .map(|file| {
            let mut tags = common.clone();
            if options.folder_names_as_tags {
                for tag in folder_tags(&file.dirs) {
                    if !tags.contains(&tag) {
                        tags.push(tag);
                    }
                }
            }
            let is_leaf = !file.dirs.is_empty()
                && !scan.parent_dirs.contains(&(file.root, file.dirs.clone()));
            let unit = (options.units_per_folder && is_leaf).then(|| UnitKey {
                root: file.root,
                dirs: file.dirs.clone(),
            });
            Assignment {
                folder: folder_target(options, &file.dirs),
                tags,
                unit,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::import::kind::MediaKind;
    use crate::import::scan::ScannedFile;
    use std::path::PathBuf;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    fn tag(ns: &str, name: &str) -> TagName {
        TagName {
            namespace: ns.into(),
            name: name.into(),
        }
    }

    #[test]
    fn parses_tags_with_namespaces() {
        assert_eq!(parse_tag("旅行"), Some(tag("", "旅行")));
        assert_eq!(parse_tag("作者:もりの"), Some(tag("作者", "もりの")));
        assert_eq!(parse_tag("作者：もりの"), Some(tag("作者", "もりの")));
        assert_eq!(parse_tag("  海  "), Some(tag("", "海")));
        assert_eq!(parse_tag(" 作者 : もりの "), Some(tag("作者", "もりの")));
        assert_eq!(parse_tag(":海"), Some(tag("", "海")));
        assert_eq!(parse_tag("時刻:12:30"), Some(tag("時刻", "12:30")));
        assert_eq!(parse_tag(""), None);
        assert_eq!(parse_tag("   "), None);
        assert_eq!(parse_tag("作者:"), None);
        // 分解形は NFC にする
        assert_eq!(parse_tag("\u{30AB}\u{3099}"), Some(tag("", "ガ")));
    }

    #[test]
    fn reproduces_folders_under_destination() {
        let opts = ImportOptions {
            dest_folder_id: Some(5),
            reproduce_folders: true,
            ..Default::default()
        };
        // 旅行2026/鎌倉 → 写真/2026 旅行（id 5）/鎌倉
        assert_eq!(
            folder_target(&opts, &s(&["旅行2026", "鎌倉"])),
            Some(FolderTarget {
                base: Some(5),
                path: s(&["鎌倉"])
            })
        );
        assert_eq!(
            folder_target(&opts, &s(&["旅行2026"])),
            Some(FolderTarget {
                base: Some(5),
                path: vec![]
            })
        );
        // ファイルを直接ドロップしたときは取り込み先
        assert_eq!(
            folder_target(&opts, &[]),
            Some(FolderTarget {
                base: Some(5),
                path: vec![]
            })
        );
    }

    #[test]
    fn reproduces_folders_at_top_without_destination() {
        let opts = ImportOptions {
            reproduce_folders: true,
            ..Default::default()
        };
        assert_eq!(
            folder_target(&opts, &s(&["旅行2026", "鎌倉"])),
            Some(FolderTarget {
                base: None,
                path: s(&["旅行2026", "鎌倉"])
            })
        );
        assert_eq!(folder_target(&opts, &[]), None);
    }

    #[test]
    fn without_reproduction_uses_destination_only() {
        let with_dest = ImportOptions {
            dest_folder_id: Some(7),
            ..Default::default()
        };
        assert_eq!(
            folder_target(&with_dest, &s(&["旅行2026", "鎌倉"])),
            Some(FolderTarget {
                base: Some(7),
                path: vec![]
            })
        );
        assert_eq!(folder_target(&ImportOptions::default(), &s(&["a"])), None);
    }

    #[test]
    fn folder_tags_cover_every_level() {
        assert_eq!(
            folder_tags(&s(&["旅行2026", "鎌倉"])),
            vec![tag("", "旅行2026"), tag("", "鎌倉")]
        );
        assert!(folder_tags(&[]).is_empty());
    }

    #[test]
    fn titles_drop_only_the_last_extension() {
        assert_eq!(title_from_name("夕方の海.jpg"), "夕方の海");
        assert_eq!(title_from_name("archive.tar.gz"), "archive.tar");
        assert_eq!(title_from_name("noext"), "noext");
        assert_eq!(title_from_name(".jpg"), ".jpg");
    }

    fn file(root: usize, dirs: &[&str], name: &str) -> ScannedFile {
        ScannedFile {
            path: PathBuf::from(name),
            root,
            dirs: s(dirs),
            name: name.into(),
            ext: "jpg".into(),
            kind: MediaKind::Image,
            size: 1,
        }
    }

    #[test]
    fn assigns_tags_and_leaf_units() {
        let mut scan = ScanResult {
            files: vec![
                file(0, &["旅行2026"], "a.jpg"),
                file(0, &["旅行2026", "鎌倉"], "b.jpg"),
                file(0, &["旅行2026", "鎌倉"], "c.jpg"),
                file(1, &[], "d.jpg"),
            ],
            ..Default::default()
        };
        scan.parent_dirs.insert((0, s(&["旅行2026"])));
        let opts = ImportOptions {
            folder_names_as_tags: true,
            units_per_folder: true,
            tags: s(&["旅行", "作者:もりの", "旅行", " "]),
            ..Default::default()
        };
        let plan = assign(&scan, &opts);
        assert_eq!(plan.len(), 4);

        // すべてに付けるタグ（重複は 1 つ）＋フォルダ名
        assert_eq!(
            plan[1].tags,
            vec![
                tag("", "旅行"),
                tag("作者", "もりの"),
                tag("", "旅行2026"),
                tag("", "鎌倉"),
            ]
        );
        assert_eq!(plan[3].tags, vec![tag("", "旅行"), tag("作者", "もりの")]);

        // 旅行2026 は下にフォルダがあるので末端ではない
        assert_eq!(plan[0].unit, None);
        let unit = UnitKey {
            root: 0,
            dirs: s(&["旅行2026", "鎌倉"]),
        };
        assert_eq!(plan[1].unit.as_ref(), Some(&unit));
        assert_eq!(plan[2].unit.as_ref(), Some(&unit));
        assert_eq!(unit.name(), "鎌倉");
        assert_eq!(plan[3].unit, None);
        // 再現しない・取り込み先なし
        assert!(plan.iter().all(|a| a.folder.is_none()));
    }

    #[test]
    fn options_default_from_partial_json() {
        let opts: ImportOptions = serde_json::from_str(r#"{"reproduceFolders":true}"#).unwrap();
        assert!(opts.reproduce_folders);
        assert_eq!(opts.duplicates, DuplicatePolicy::AddMemberships);
        let opts: ImportOptions = serde_json::from_str(r#"{"duplicates":"ignore"}"#).unwrap();
        assert_eq!(opts.duplicates, DuplicatePolicy::Ignore);
    }
}
