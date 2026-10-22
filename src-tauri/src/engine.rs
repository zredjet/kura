//! エンジンの部品を作る口（kura-core の `EngineFactory`）の macOS の実装。ライブラリを開くたびに渡し、kura-core が
//! ライブラリごとの `EngineServices` を作る（中身は最初に使うときに作る）。

use std::path::Path;

use kura_core::codec_ext::svg_worker::{self, WorkerCommand};
use kura_core::edit::{CodecSet, EngineFactory, RuntimeConfig};

/// 物理メモリは kura-mac の `physical_memory`、コーデックの組は固定の段（SVG の文字はシステムのフォントを mmap して渡す）→
/// macOS（ImageIO・AudioToolbox）→ ffmpeg（設定で指定した場所を含めて探す）。SVG は、アプリ自身を `--kura-svg-worker` 付きで
/// 起動した別のプロセスで読み、時間の上限で止める（決定 23。`main.rs` が働き手の入口）。
pub struct MacEngineFactory;

impl EngineFactory for MacEngineFactory {
    fn runtime_config(&self) -> RuntimeConfig {
        RuntimeConfig {
            physical_memory: kura_mac::physical_memory(),
            ..RuntimeConfig::default()
        }
    }

    fn codecs(&self, work_dir: &Path) -> CodecSet {
        let mut set = kura_mac::full_codec_set(&kura_mac::pinned_options(), work_dir);
        if let Err(e) = svg_worker::install(&mut set, WorkerCommand::current_exe()) {
            tracing::error!("SVG の働き手を組めません: {e}");
        }
        set
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 予算の元は物理メモリ、コーデックの組は macOS の段まで持つ。
    #[test]
    fn the_mac_factory_uses_physical_memory_and_platform_codecs() {
        let config = MacEngineFactory.runtime_config();
        assert_eq!(config.physical_memory, kura_mac::physical_memory());
        assert!(config.physical_memory.is_some());
        let dir = tempfile::tempdir().unwrap();
        let codecs = MacEngineFactory.codecs(&dir.path().join("engine"));
        let heic = codecs
            .capabilities()
            .into_iter()
            .find(|c| c.format == "heic")
            .unwrap();
        assert!(
            heic.read
                .iter()
                .any(|r| r.codec == kura_mac::codec::IMAGEIO_DEC),
            "{:?}",
            heic.read
        );
    }
}
