//! ICC プロファイルの最小限の読み取り（sRGB かどうかの見当）。色の変換はしない（docs/editing.md §5.2）。

fn be32(b: &[u8], i: usize) -> Option<u32> {
    Some(u32::from_be_bytes(b.get(i..i + 4)?.try_into().ok()?))
}

/// プロファイルの説明（`desc` タグ）。`desc`（ASCII）と `mluc`（UTF-16BE。最初の記録）を読む。読めなければ None。
pub fn description(icc: &[u8]) -> Option<String> {
    let count = be32(icc, 128)? as usize;
    // タグの表は 1 つ 12 バイト。数が大きすぎれば壊れたプロファイル
    if count > 1_024 {
        return None;
    }
    for k in 0..count {
        let at = 132 + k * 12;
        if icc.get(at..at + 4)? != b"desc" {
            continue;
        }
        let off = be32(icc, at + 4)? as usize;
        let size = be32(icc, at + 8)? as usize;
        let tag = icc.get(off..off.checked_add(size)?)?;
        return match tag.get(0..4)? {
            b"desc" => {
                let n = be32(tag, 8)? as usize;
                let text = tag.get(12..12usize.checked_add(n)?)?;
                Some(
                    String::from_utf8_lossy(text)
                        .trim_end_matches('\0')
                        .to_string(),
                )
            }
            b"mluc" => {
                let records = be32(tag, 8)?;
                if records == 0 {
                    return None;
                }
                let len = be32(tag, 20)? as usize;
                let start = be32(tag, 24)? as usize;
                let units: Vec<u16> = tag
                    .get(start..start.checked_add(len)?)?
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|c| u16::from_be_bytes([c[0], c[1]]))
                    .collect();
                Some(String::from_utf16_lossy(&units))
            }
            _ => None,
        };
    }
    None
}

/// sRGB のプロファイルとみなせるか（説明に「sRGB」を含む）。読めないプロファイルは sRGB とみなさない。
pub fn is_srgb(icc: &[u8]) -> bool {
    description(icc).is_some_and(|d| d.contains("sRGB"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// タグの表に `desc` だけを持つ、最小のプロファイル。
    fn profile(tag: &[u8]) -> Vec<u8> {
        let mut p = vec![0u8; 128];
        p.extend_from_slice(&1u32.to_be_bytes());
        p.extend_from_slice(b"desc");
        p.extend_from_slice(&144u32.to_be_bytes());
        p.extend_from_slice(&(tag.len() as u32).to_be_bytes());
        p.extend_from_slice(tag);
        p
    }

    #[test]
    fn reads_desc_and_mluc() {
        let mut desc = b"desc\0\0\0\0".to_vec();
        let text = b"sRGB IEC61966-2.1\0";
        desc.extend_from_slice(&(text.len() as u32).to_be_bytes());
        desc.extend_from_slice(text);
        assert!(is_srgb(&profile(&desc)));
        let mut mluc = b"mluc\0\0\0\0".to_vec();
        mluc.extend_from_slice(&1u32.to_be_bytes());
        mluc.extend_from_slice(&12u32.to_be_bytes());
        let name: Vec<u8> = "Display P3"
            .encode_utf16()
            .flat_map(u16::to_be_bytes)
            .collect();
        mluc.extend_from_slice(b"enUS");
        mluc.extend_from_slice(&(name.len() as u32).to_be_bytes());
        mluc.extend_from_slice(&28u32.to_be_bytes());
        mluc.extend_from_slice(&name);
        let p = profile(&mluc);
        assert_eq!(description(&p).as_deref(), Some("Display P3"));
        assert!(!is_srgb(&p));
        // 壊れたもの・短いものは読まない
        assert!(!is_srgb(&[0u8; 10]));
        assert!(!is_srgb(&p[..150]));
    }
}
