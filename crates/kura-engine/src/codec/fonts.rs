//! SVG の文字を描くフォントの入れ物（M11 の決定 12）。
//!
//! - フォントのファイルの中身は、ファイルを読んだバイト列か mmap した領域を共有で受け（[`FontBytes`]）、複製しない。OS の
//!   ファイルを開く・写すのは呼び手（kura-mac がシステムのフォントを mmap する）で、エンジンは OS を知らない。
//! - 入れ物は名前（ファイルのパスなど）の順に登録する。同じ組なら、渡した順によらず同じ集まり（書体の並び）になる。
//! - 読めない入れ物（フォントでない・壊れている・中で panic した）は飛ばして数える（[`FontLoadReport`]）。
//! - [`SvgFonts::lazy`] は、文字を含む SVG を初めて読むときに入れ物を集めて作る（文字のない SVG だけなら作らない）。
//!   集まりは解析・サムネイル・アニメ・エディタで共通の 1 つを使い回す（`Clone` は同じ集まりを指す）。
//! - SVG の値は OS の libm に依存する（resvg）ので、もともと再現を約束しない（`reproducible()` は偽。アンカーを作る）。
//!   フォントの集まりが変わると文字の描き方も変わりうるが、それも同じ扱い。

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Arc, Mutex, OnceLock};

use resvg::usvg::fontdb;

/// フォントのファイルの中身（ファイルのバイト列か mmap した領域）。共有で受ける。
pub type FontBytes = Arc<dyn AsRef<[u8]> + Send + Sync>;

/// フォントの入れ物 1 つ。
#[derive(Clone)]
pub struct FontSource {
    /// 名前（ファイルのパスなど）。登録の順を決める。
    pub name: String,
    pub data: FontBytes,
}

impl std::fmt::Debug for FontSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FontSource")
            .field("name", &self.name)
            .field("bytes", &(*self.data).as_ref().len())
            .finish()
    }
}

/// CSS の総称の書体（`serif` など）に当てる書体名。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GenericFamilies {
    pub serif: String,
    pub sans_serif: String,
    pub cursive: String,
    pub fantasy: String,
    pub monospace: String,
}

impl Default for GenericFamilies {
    /// fontdb の既定（Times New Roman・Arial・Comic Sans MS・Impact・Courier New）。
    fn default() -> Self {
        Self {
            serif: "Times New Roman".into(),
            sans_serif: "Arial".into(),
            cursive: "Comic Sans MS".into(),
            fantasy: "Impact".into(),
            monospace: "Courier New".into(),
        }
    }
}

/// 集まりを作った結果の数。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FontLoadReport {
    /// 渡された入れ物の数。
    pub sources: usize,
    /// 登録した書体の数（コレクションは書体ごとに数える）。
    pub faces: usize,
    /// 読めずに飛ばした入れ物の数。
    pub skipped: usize,
    /// 入れ物の中身の合計のバイト数（mmap なら写した大きさで、常駐するメモリではない）。
    pub bytes: u64,
}

type Loader = Box<dyn FnOnce() -> Vec<FontSource> + Send>;

struct Inner {
    generic: GenericFamilies,
    loader: Mutex<Option<Loader>>,
    built: OnceLock<(Arc<fontdb::Database>, FontLoadReport)>,
}

/// SVG の文字を描くフォントの集まり（`PinnedOptions::svg_fonts` に渡す）。`Clone` は同じ集まりを指す。
#[derive(Clone)]
pub struct SvgFonts {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for SvgFonts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SvgFonts")
            .field("built", &self.inner.built.get().map(|b| b.1))
            .finish()
    }
}

impl SvgFonts {
    /// 入れ物の列から、すぐに作る。
    pub fn new(sources: Vec<FontSource>, generic: GenericFamilies) -> Self {
        let built = build(sources, &generic);
        Self {
            inner: Arc::new(Inner {
                generic,
                loader: Mutex::new(None),
                built: OnceLock::from(built),
            }),
        }
    }

    /// 文字を含む SVG を初めて読むときに、`load` で入れ物を集めて作る（`load` は 1 度だけ呼ぶ。中で panic したら空の集まり）。
    pub fn lazy(
        generic: GenericFamilies,
        load: impl FnOnce() -> Vec<FontSource> + Send + 'static,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                generic,
                loader: Mutex::new(Some(Box::new(load))),
                built: OnceLock::new(),
            }),
        }
    }

    /// 作ってある fontdb の集まりをそのまま使う。
    pub fn from_database(db: Arc<fontdb::Database>) -> Self {
        let report = FontLoadReport {
            faces: db.len(),
            ..FontLoadReport::default()
        };
        Self {
            inner: Arc::new(Inner {
                generic: GenericFamilies::default(),
                loader: Mutex::new(None),
                built: OnceLock::from((db, report)),
            }),
        }
    }

    /// 総称の書体に当てる書体名（作らずに返す。kura-core の SVG の働き手が、親と子が同じ設定かを照らす印に入れる）。
    pub fn generic(&self) -> &GenericFamilies {
        &self.inner.generic
    }

    /// もう作ったか（`lazy` で、まだ文字のある SVG を読んでいなければ偽）。
    pub fn is_built(&self) -> bool {
        self.inner.built.get().is_some()
    }

    fn built(&self) -> &(Arc<fontdb::Database>, FontLoadReport) {
        self.inner.built.get_or_init(|| {
            let loader = match self.inner.loader.lock() {
                Ok(mut g) => g.take(),
                Err(poisoned) => poisoned.into_inner().take(),
            };
            let sources = loader
                .map(|load| catch_unwind(AssertUnwindSafe(load)).unwrap_or_default())
                .unwrap_or_default();
            build(sources, &self.inner.generic)
        })
    }

    /// fontdb の集まり（`lazy` なら、ここで作る）。
    pub fn database(&self) -> Arc<fontdb::Database> {
        self.built().0.clone()
    }

    /// 作った結果の数（`lazy` なら、ここで作る）。
    pub fn report(&self) -> FontLoadReport {
        self.built().1
    }

    /// その書体名（ファミリー）の書体があるか（`lazy` なら、ここで作る）。
    pub fn has_family(&self, family: &str) -> bool {
        self.built()
            .0
            .faces()
            .any(|f| f.families.iter().any(|(name, _)| name == family))
    }
}

/// 入れ物を名前の順に登録する。
fn build(
    mut sources: Vec<FontSource>,
    generic: &GenericFamilies,
) -> (Arc<fontdb::Database>, FontLoadReport) {
    sources.sort_by(|a, b| a.name.cmp(&b.name));
    let mut db = fontdb::Database::new();
    db.set_serif_family(generic.serif.clone());
    db.set_sans_serif_family(generic.sans_serif.clone());
    db.set_cursive_family(generic.cursive.clone());
    db.set_fantasy_family(generic.fantasy.clone());
    db.set_monospace_family(generic.monospace.clone());
    let mut report = FontLoadReport {
        sources: sources.len(),
        ..FontLoadReport::default()
    };
    for s in sources {
        report.bytes = report.bytes.saturating_add((*s.data).as_ref().len() as u64);
        // 第三者のパーサー（ttf-parser）の境界。panic したら、その入れ物を飛ばす
        let loaded = catch_unwind(AssertUnwindSafe(|| {
            db.load_font_source(fontdb::Source::Binary(s.data.clone()))
                .len()
        }));
        match loaded {
            Ok(n) if n > 0 => report.faces += n,
            _ => report.skipped += 1,
        }
    }
    (Arc::new(db), report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testfiles::tiny_ttf;

    fn source(name: &str, bytes: Vec<u8>) -> FontSource {
        FontSource {
            name: name.into(),
            data: Arc::new(bytes),
        }
    }

    #[test]
    fn sources_are_registered_in_name_order() {
        let a = source("a.ttf", tiny_ttf("KuraA", &['A']));
        let b = source("b.ttf", tiny_ttf("KuraB", &['B']));
        let families = |fonts: &SvgFonts| -> Vec<String> {
            fonts
                .database()
                .faces()
                .map(|f| f.families[0].0.clone())
                .collect()
        };
        let one = SvgFonts::new(vec![a.clone(), b.clone()], GenericFamilies::default());
        let two = SvgFonts::new(vec![b, a], GenericFamilies::default());
        assert_eq!(families(&one), vec!["KuraA", "KuraB"]);
        assert_eq!(families(&one), families(&two));
        assert!(one.has_family("KuraB") && !one.has_family("Kura"));
    }

    #[test]
    fn broken_sources_are_skipped_and_counted() {
        let fonts = SvgFonts::new(
            vec![
                source("good.ttf", tiny_ttf("Kura", &['A'])),
                source("bad.ttf", b"\0\x01\0\0 broken".to_vec()),
                source("empty.ttf", Vec::new()),
            ],
            GenericFamilies::default(),
        );
        let r = fonts.report();
        assert_eq!((r.sources, r.faces, r.skipped), (3, 1, 2));
        assert!(r.bytes > 100);
    }

    #[test]
    fn lazy_fonts_load_once_and_survive_a_panicking_loader() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let calls = Arc::new(AtomicUsize::new(0));
        let c = calls.clone();
        let fonts = SvgFonts::lazy(GenericFamilies::default(), move || {
            c.fetch_add(1, Ordering::SeqCst);
            vec![source("a.ttf", tiny_ttf("Kura", &['A']))]
        });
        let shared = fonts.clone();
        assert!(!fonts.is_built());
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert_eq!(shared.report().faces, 1);
        assert!(fonts.is_built());
        assert_eq!(fonts.database().len(), 1);
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        let panicking = SvgFonts::lazy(GenericFamilies::default(), || panic!("読めない"));
        assert!(panicking.database().is_empty());
    }
}
