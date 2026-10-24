//! 書き出しのファイル名（純粋関数）。

use crate::import::format_id;

/// APFS・HFS+ のファイル名の上限（バイト）。
const MAX_NAME_BYTES: usize = 255;

/// ファイル名に使えるようにする：`/`・`:`・制御文字を `_` にし、前後の空白を除き、
/// 先頭の `.`（隠しファイルになる）を `_` にする。空になったら `fallback`。
pub fn safe_name(name: &str, fallback: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if c == '/' || c == ':' || c.is_control() {
                '_'
            } else {
                c
            }
        })
        .collect();
    let trimmed = cleaned.trim();
    let out = match trimmed.strip_prefix('.') {
        Some(rest) => format!("_{rest}"),
        None if trimmed.is_empty() => fallback.to_string(),
        None => trimmed.to_string(),
    };
    truncate_bytes(&out, MAX_NAME_BYTES)
}

/// `n` 番目の候補：1 ならそのまま、2 以降は拡張子の前に「 2」（Finder と同じ）。
/// 名前が長すぎるときは、番号と拡張子が収まるよう名前の側を詰める。
pub fn numbered_file(name: &str, n: u32) -> String {
    let (stem, ext) = split_ext(name);
    let suffix = if n <= 1 {
        String::new()
    } else {
        format!(" {n}")
    };
    let tail = match ext {
        Some(ext) => format!("{suffix}.{ext}"),
        None => suffix,
    };
    let room = MAX_NAME_BYTES.saturating_sub(tail.len());
    format!("{}{tail}", truncate_bytes(stem, room))
}

/// 書き出すときの名前：元の名前の拡張子が今の中身（`items.ext`）と同じ形式なら元の名前のまま（`jpg` と `jpeg` は同じ。
/// 形式の表の ID で比べる）、違えば元の名前の語幹に今の拡張子を付ける（形式を変えて上書きしたあとも、元の名前は
/// 変えずに書き出しで直す。M12 の段 2 の決定 10）。拡張子のない名前・知らない拡張子も「違う」とみなす。
pub fn export_name(original_name: &str, ext: &str) -> String {
    let (stem, old) = split_ext(original_name);
    let ext = ext.trim();
    if ext.is_empty() || stem.trim().is_empty() {
        return original_name.to_string();
    }
    let same = old.is_some_and(|old| match (format_id(old), format_id(ext)) {
        (Some(a), Some(b)) => a == b,
        _ => old.eq_ignore_ascii_case(ext),
    });
    if same {
        original_name.to_string()
    } else {
        format!("{stem}.{ext}")
    }
}

/// フォルダの `n` 番目の候補（拡張子を区別しない。「鎌倉 2」）。
pub fn numbered_dir(name: &str, n: u32) -> String {
    if n <= 1 {
        return truncate_bytes(name, MAX_NAME_BYTES);
    }
    let suffix = format!(" {n}");
    let room = MAX_NAME_BYTES.saturating_sub(suffix.len());
    format!("{}{suffix}", truncate_bytes(name, room))
}

/// 名前と拡張子に分ける（先頭の `.` と、最後の `.` のあとが空のものは拡張子とみなさない）。
fn split_ext(name: &str) -> (&str, Option<&str>) {
    match name.rfind('.') {
        Some(i) if i > 0 && i + 1 < name.len() => (&name[..i], Some(&name[i + 1..])),
        _ => (name, None),
    }
}

/// 文字の途中で切らないよう、`max` バイト以内に詰める。
fn truncate_bytes(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_string();
    }
    let mut end = max;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_name_replaces_separators_and_hidden_prefix() {
        assert_eq!(safe_name("旅行/鎌倉:夕方.jpg", "x"), "旅行_鎌倉_夕方.jpg");
        assert_eq!(safe_name("  .secret.png ", "x"), "_secret.png");
        assert_eq!(safe_name("a\u{0}b\nc.jpg", "x"), "a_b_c.jpg");
        assert_eq!(safe_name("   ", "item-3.jpg"), "item-3.jpg");
        assert_eq!(safe_name("", "item-3.jpg"), "item-3.jpg");
    }

    #[test]
    fn numbered_file_puts_the_number_before_the_extension() {
        assert_eq!(numbered_file("IMG_0001.JPG", 1), "IMG_0001.JPG");
        assert_eq!(numbered_file("IMG_0001.JPG", 2), "IMG_0001 2.JPG");
        assert_eq!(numbered_file("archive.tar.gz", 3), "archive.tar 3.gz");
        assert_eq!(numbered_file("README", 2), "README 2");
        assert_eq!(numbered_file("_secret", 2), "_secret 2");
    }

    #[test]
    fn export_name_keeps_the_name_when_the_format_is_the_same() {
        assert_eq!(export_name("IMG_0001.JPG", "jpg"), "IMG_0001.JPG");
        assert_eq!(export_name("photo.jpeg", "jpg"), "photo.jpeg");
        assert_eq!(export_name("anim.apng", "png"), "anim.apng");
        assert_eq!(export_name("scan.TIF", "tiff"), "scan.TIF");
    }

    #[test]
    fn export_name_uses_the_current_extension_when_the_format_changed() {
        // PSD を PNG で上書きしたあと（original_name は変えない）
        assert_eq!(export_name("hero.psd", "png"), "hero.png");
        assert_eq!(export_name("hero.png", "webp"), "hero.webp");
        assert_eq!(export_name("archive.v2.gif", "png"), "archive.v2.png");
        // 拡張子のない名前・知らない拡張子は今の拡張子を付ける
        assert_eq!(export_name("README", "png"), "README.png");
        assert_eq!(export_name("dump.dat", "png"), "dump.png");
        // 知らない拡張子どうしは文字で比べる
        assert_eq!(export_name("a.xyz", "XYZ"), "a.xyz");
    }

    #[test]
    fn export_name_leaves_empty_names_to_the_fallback() {
        assert_eq!(export_name("", "png"), "");
        assert_eq!(export_name("  ", "png"), "  ");
        assert_eq!(export_name("hero.png", ""), "hero.png");
    }

    #[test]
    fn long_names_stay_within_the_limit_without_breaking_characters() {
        let long = format!("{}.jpg", "あ".repeat(100));
        let first = numbered_file(&long, 1);
        assert!(
            first.len() <= MAX_NAME_BYTES && first.ends_with(".jpg"),
            "{first}"
        );
        let tenth = numbered_file(&long, 10);
        assert!(
            tenth.len() <= MAX_NAME_BYTES && tenth.ends_with(" 10.jpg"),
            "{tenth}"
        );
        let dir = numbered_dir(&"い".repeat(100), 2);
        assert!(dir.len() <= MAX_NAME_BYTES && dir.ends_with(" 2"), "{dir}");
        assert_eq!(numbered_dir("鎌倉", 1), "鎌倉");
        assert_eq!(numbered_dir("v1.2", 2), "v1.2 2");
    }
}
