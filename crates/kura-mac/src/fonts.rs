//! SVG の文字を描くシステムのフォント（M11 の決定 12）。
//!
//! - `/System/Library/Fonts` とその `Supplemental` の ttf・otf・ttc を、読み取り専用の私的な写像（mmap）で開き、エンジンの
//!   `SvgFonts` に入れ物（`FontSource`）として渡す。中身をヒープに写さない（全部を読むと約 840 MB。写せば、触った頁だけが
//!   常駐し、ファイルに戻せる頁なのでメモリが足りなくなれば OS が捨てる）。
//! - 写すのは読み取り専用のボリュームにあるファイルだけ（システムのボリュームは封をした読み取り専用で、写したあとに中身が
//!   変わらない）。書き換えうるボリュームのファイルは、写したあとに縮められると読むときに SIGBUS で落ちるので、ヒープに読む。
//! - 集まりはプロセスで 1 つ（`system_svg_fonts`）。文字を含む SVG を初めて読むときに作る（`SvgFonts::lazy`）。解析・
//!   サムネイル・アニメ・エディタ（M12）で共通に使う（`pinned_options` を `codec_set`・`full_codec_set` に渡す）。
//! - 総称の書体は WebKit の macOS の既定に合わせる（serif = Times、sans-serif = Helvetica、monospace = Courier、
//!   cursive = Apple Chancery、fantasy = Papyrus）。
//! - unsafe は、このファイルの `map` の中（mmap・munmap・写した領域をスライスとして見る・Send と Sync・fstatfs）だけ。

use std::path::{Path, PathBuf};

use kura_engine::codec::{FontBytes, FontSource, GenericFamilies, PinnedOptions, SvgFonts};

/// システムのフォントのフォルダ（入れ子は見ない）。
pub const SYSTEM_FONT_DIRS: [&str; 2] = [
    "/System/Library/Fonts",
    "/System/Library/Fonts/Supplemental",
];

/// 入れ物にする拡張子（小文字で比べる）。
const FONT_EXTS: [&str; 3] = ["ttf", "otf", "ttc"];

/// 1 ファイルの上限（いちばん大きい Apple Color Emoji.ttc で約 190 MB）。
const MAX_FONT_FILE: u64 = 1 << 30;

/// WebKit の macOS の既定に合わせた総称の書体。
pub fn macos_generic_families() -> GenericFamilies {
    GenericFamilies {
        serif: "Times".into(),
        sans_serif: "Helvetica".into(),
        cursive: "Apple Chancery".into(),
        fantasy: "Papyrus".into(),
        monospace: "Courier".into(),
    }
}

/// システムのフォントの集まり（プロセスで 1 つ。文字を含む SVG を初めて読むときに作る）。macOS 以外は None。
pub fn system_svg_fonts() -> Option<SvgFonts> {
    #[cfg(target_os = "macos")]
    {
        use std::sync::OnceLock;
        static FONTS: OnceLock<SvgFonts> = OnceLock::new();
        Some(
            FONTS
                .get_or_init(|| {
                    SvgFonts::lazy(macos_generic_families(), || {
                        let dirs: Vec<PathBuf> =
                            SYSTEM_FONT_DIRS.iter().map(PathBuf::from).collect();
                        font_sources(&dirs)
                    })
                })
                .clone(),
        )
    }
    #[cfg(not(target_os = "macos"))]
    {
        None
    }
}

/// アプリの固定の段の設定（SVG の文字にシステムのフォントを使う）。`codec_set`・`full_codec_set` に渡す。
pub fn pinned_options() -> PinnedOptions {
    PinnedOptions {
        svg_fonts: system_svg_fonts(),
    }
}

/// フォルダ（入れ子は見ない）の ttf・otf・ttc を開いて入れ物にする（名前はファイルのパス）。シンボリックリンク・
/// 開けないもの・大きすぎるものは飛ばす。
pub fn font_sources(dirs: &[PathBuf]) -> Vec<FontSource> {
    let mut paths: Vec<PathBuf> = dirs
        .iter()
        .filter_map(|d| std::fs::read_dir(d).ok())
        .flat_map(|entries| entries.filter_map(Result::ok))
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .map(|e| e.path())
        .filter(|p| {
            p.extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| FONT_EXTS.contains(&e.to_ascii_lowercase().as_str()))
        })
        .collect();
    paths.sort();
    paths
        .into_iter()
        .filter_map(|p| match open_font(&p) {
            Ok((data, _)) => Some(FontSource {
                name: p.to_string_lossy().into_owned(),
                data,
            }),
            Err(e) => {
                tracing::warn!("フォント {} を開けません: {e}", p.display());
                None
            }
        })
        .collect()
}

/// フォントのファイルを開く。読み取り専用のボリュームなら写し（真）、そうでなければヒープに読む（偽）。
fn open_font(path: &Path) -> std::io::Result<(FontBytes, bool)> {
    let file = std::fs::File::open(path)?;
    let len = file.metadata()?.len();
    if len == 0 || len > MAX_FONT_FILE {
        return Err(std::io::Error::other(format!(
            "大きさが範囲の外です（{len} バイト）"
        )));
    }
    #[cfg(target_os = "macos")]
    {
        if map::read_only_volume(&file)? {
            return Ok((std::sync::Arc::new(map::Mapped::new(&file, len)?), true));
        }
    }
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(len as usize)
        .map_err(std::io::Error::other)?;
    std::io::Read::read_to_end(&mut std::io::Read::take(file, len), &mut bytes)?;
    Ok((std::sync::Arc::new(bytes), false))
}

#[cfg(target_os = "macos")]
mod map {
    use std::fs::File;
    use std::os::fd::AsRawFd;
    use std::ptr::NonNull;

    /// 読み取り専用の私的な写像（PROT_READ・MAP_PRIVATE）。落とすと解く。
    pub(super) struct Mapped {
        ptr: NonNull<u8>,
        len: usize,
    }

    // SAFETY: 写像は読み取り専用で、作ったあとは誰も書かない（PROT_READ）。どのスレッドから読んでも、落とす（munmap）のは
    // 最後の持ち主の 1 回だけ。
    unsafe impl Send for Mapped {}
    // SAFETY: 同上（共有の参照からは読むだけ）。
    unsafe impl Sync for Mapped {}

    impl Mapped {
        /// `file` の先頭から `len` バイトを写す（`len` はファイルの大きさ、0 でない）。
        pub(super) fn new(file: &File, len: u64) -> std::io::Result<Self> {
            let len = usize::try_from(len).map_err(std::io::Error::other)?;
            // SAFETY: 開いている fd を、アドレスを任せて読み取り専用・私的に写す。失敗は MAP_FAILED で返る（下で確かめる）。
            // 写像は fd を閉じても残る。
            let ptr = unsafe {
                libc::mmap(
                    std::ptr::null_mut(),
                    len,
                    libc::PROT_READ,
                    libc::MAP_PRIVATE,
                    file.as_raw_fd(),
                    0,
                )
            };
            if ptr == libc::MAP_FAILED {
                return Err(std::io::Error::last_os_error());
            }
            let ptr = NonNull::new(ptr.cast::<u8>())
                .ok_or_else(|| std::io::Error::other("mmap が NULL を返しました"))?;
            Ok(Self { ptr, len })
        }
    }

    impl AsRef<[u8]> for Mapped {
        fn as_ref(&self) -> &[u8] {
            // SAFETY: ptr から len バイトは、この値が生きているあいだ写してあり（Drop まで解かない）、読み取り専用で誰も書かない。
            // 写したのは読み取り専用のボリュームのファイルなので、中身が縮んで SIGBUS になることもない。
            unsafe { std::slice::from_raw_parts(self.ptr.as_ptr(), self.len) }
        }
    }

    impl Drop for Mapped {
        fn drop(&mut self) {
            // SAFETY: new で写した領域を、ちょうどその大きさで 1 度だけ解く。
            unsafe {
                libc::munmap(self.ptr.as_ptr().cast(), self.len);
            }
        }
    }

    /// `file` のあるボリュームが読み取り専用か（fstatfs の MNT_RDONLY）。
    pub(super) fn read_only_volume(file: &File) -> std::io::Result<bool> {
        let mut st = std::mem::MaybeUninit::<libc::statfs>::zeroed();
        // SAFETY: 開いている fd と、書き込み先の statfs の領域を渡す。成功（0）したときだけ中身を読む。
        let rc = unsafe { libc::fstatfs(file.as_raw_fd(), st.as_mut_ptr()) };
        if rc != 0 {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: fstatfs が成功したので st は書き込まれている（0 で埋めた領域でもある）。
        let st = unsafe { st.assume_init() };
        Ok(st.f_flags & (libc::MNT_RDONLY as u32) != 0)
    }
}

#[cfg(test)]
#[cfg(target_os = "macos")]
mod tests {
    use super::*;

    use kura_engine::Value;
    use kura_engine::codec::CodecSet;

    fn system_dirs() -> Vec<PathBuf> {
        SYSTEM_FONT_DIRS.iter().map(PathBuf::from).collect()
    }

    /// システムのフォントは写し、中身はファイルと同じ。書き換えうるボリューム（一時フォルダ）のファイルはヒープに読む。
    #[test]
    fn system_fonts_are_mapped_and_others_are_read() {
        let geneva = Path::new("/System/Library/Fonts/Geneva.ttf");
        let (data, mapped) = open_font(geneva).unwrap();
        assert!(mapped, "システムのボリュームは読み取り専用のはず");
        assert_eq!((*data).as_ref(), std::fs::read(geneva).unwrap().as_slice());

        let dir = tempfile::tempdir().unwrap();
        let copy = dir.path().join("geneva.ttf");
        std::fs::copy(geneva, &copy).unwrap();
        let (data, mapped) = open_font(&copy).unwrap();
        assert!(!mapped);
        assert_eq!((*data).as_ref(), std::fs::read(geneva).unwrap().as_slice());

        let empty = dir.path().join("empty.ttf");
        std::fs::write(&empty, b"").unwrap();
        assert!(open_font(&empty).is_err());
    }

    #[test]
    fn font_sources_are_sorted_fonts_only() {
        let dir = tempfile::tempdir().unwrap();
        let geneva = std::fs::read("/System/Library/Fonts/Geneva.ttf").unwrap();
        for name in ["b.TTF", "a.otf", "c.txt", "d"] {
            std::fs::write(dir.path().join(name), &geneva).unwrap();
        }
        std::fs::create_dir(dir.path().join("sub.ttf")).unwrap();
        std::os::unix::fs::symlink(dir.path().join("a.otf"), dir.path().join("e.ttf")).unwrap();
        let names: Vec<String> = font_sources(&[dir.path().to_path_buf()])
            .into_iter()
            .map(|s| {
                Path::new(&s.name)
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        assert_eq!(names, ["a.otf", "b.TTF"]);
    }

    /// 決定 12：システムのフォントを写して渡すと、日本語と英字の文字を含む SVG を描ける。総称の書体はどれも集まりにある。
    #[test]
    fn system_fonts_draw_svg_text() {
        let fonts = system_svg_fonts().unwrap();
        let set = CodecSet::pinned_with(&pinned_options());
        let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" width="200" height="60"><text x="4" y="40" font-family="Hiragino Sans" font-size="32" fill="#000">永あAa</text><text x="150" y="40" font-family="sans-serif" font-size="32">Kq</text></svg>"##;
        let (v, _) =
            crate::codec::testutil::decode(&set, svg.as_bytes(), "svg").expect("文字の SVG");
        let Value::Image(img) = v else {
            panic!("画像でない")
        };
        let rgba = kura_engine::imaging::convert::to_rgba8_rows(&img.px).unwrap();
        let ink = |x0: u32, x1: u32| {
            (0..60u32)
                .flat_map(|y| (x0..x1).map(move |x| (x, y)))
                .filter(|(x, y)| rgba[((y * 200 + x) * 4 + 3) as usize] > 128)
                .count()
        };
        assert!(ink(4, 140) > 500, "日本語の字が描かれていない");
        assert!(ink(150, 200) > 100, "総称の書体の字が描かれていない");
        assert!(fonts.is_built());
        let r = fonts.report();
        assert!(r.faces > 300, "{r:?}");
        assert!(r.skipped * 20 < r.sources, "{r:?}");
        let g = macos_generic_families();
        for family in [g.serif, g.sans_serif, g.cursive, g.fantasy, g.monospace] {
            assert!(fonts.has_family(&family), "{family} がない");
        }
        // 同じ集まりを指す
        assert!(system_svg_fonts().unwrap().is_built());
    }

    /// プロセスの物理フットプリントと常駐のバイト数（proc_pid_rusage）。
    fn footprint() -> (u64, u64) {
        let mut info = std::mem::MaybeUninit::<libc::rusage_info_v2>::zeroed();
        // SAFETY: 自分のプロセスの番号と、書き込み先の rusage_info_v2 の領域を渡す。成功したときだけ読む。
        let rc = unsafe {
            libc::proc_pid_rusage(
                std::process::id() as i32,
                libc::RUSAGE_INFO_V2,
                info.as_mut_ptr().cast(),
            )
        };
        assert_eq!(rc, 0);
        // SAFETY: 成功したので書き込まれている。
        let info = unsafe { info.assume_init() };
        (info.ri_phys_footprint, info.ri_resident_size)
    }

    /// メモリの増え方を測る（`cargo test -p kura-mac --release fonts::tests::measure -- --ignored --nocapture`）。
    /// 写す道とヒープに読む道を、同じプロセスで順に測る（写す道を先に）。
    #[test]
    #[ignore]
    fn measure_font_memory() {
        let mib = |b: u64| b as f64 / (1024.0 * 1024.0);
        let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" width="400" height="80"><text x="4" y="50" font-family="Hiragino Sans" font-size="40">永あAa</text><text x="200" y="50" font-family="serif" font-size="40">Kq</text></svg>"##;
        for heap in [false, true] {
            let before = footprint();
            let t = std::time::Instant::now();
            let sources = if heap {
                system_dirs()
                    .iter()
                    .filter_map(|d| std::fs::read_dir(d).ok())
                    .flat_map(|e| e.filter_map(Result::ok))
                    .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
                    .filter(|e| {
                        e.path()
                            .extension()
                            .and_then(|x| x.to_str())
                            .is_some_and(|x| FONT_EXTS.contains(&x.to_ascii_lowercase().as_str()))
                    })
                    .map(|e| FontSource {
                        name: e.path().to_string_lossy().into_owned(),
                        data: std::sync::Arc::new(std::fs::read(e.path()).unwrap()),
                    })
                    .collect()
            } else {
                font_sources(&system_dirs())
            };
            let fonts = SvgFonts::new(sources, macos_generic_families());
            let built = (footprint(), t.elapsed());
            let set = CodecSet::pinned_with(&PinnedOptions {
                svg_fonts: Some(fonts.clone()),
            });
            crate::codec::testutil::decode(&set, svg.as_bytes(), "svg").unwrap();
            let drawn = footprint();
            let r = fonts.report();
            eprintln!(
                "{}: 書体 {}・入れ物 {}（{:.0} MiB）・作る {:?}／フットプリント +{:.1} MiB（作った後）+{:.1} MiB（描いた後）・常駐 +{:.1} MiB（描いた後）",
                if heap { "ヒープに読む" } else { "写す" },
                r.faces,
                r.sources,
                mib(r.bytes),
                built.1,
                mib(built.0.0.saturating_sub(before.0)),
                mib(drawn.0.saturating_sub(before.0)),
                mib(drawn.1.saturating_sub(before.1)),
            );
            drop(set);
            drop(fonts);
        }
    }
}
