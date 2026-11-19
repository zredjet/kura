//! エンジンの部品を作る口（kura-core の `EngineFactory`）の macOS の実装。ライブラリを開くたびに渡し、kura-core が
//! ライブラリごとの `EngineServices` を作る（中身は最初に使うときに作る）。

use std::path::Path;

use kura_core::codec_ext::svg_worker::{self, WorkerCommand};
use kura_core::edit::{CodecSet, EngineFactory, RuntimeConfig};

/// 物理メモリは kura-mac の `physical_memory`、コーデックの組は固定の段（SVG の文字はシステムのフォントを mmap して渡す）→
/// macOS（ImageIO・AudioToolbox）→ ffmpeg（設定で指定した場所を含めて探す）。SVG は、アプリ自身を `--kura-svg-worker` 付きで
/// 起動した別のプロセスで読み、時間の上限で止める（決定 23。`main.rs` が働き手の入口）。働き手の実行ファイルは起動のたびに
/// kura-mac の `current_executable`（今の場所。動いているアプリを移す・改名しても追う）で求める。
pub struct MacEngineFactory;

impl EngineFactory for MacEngineFactory {
    fn runtime_config(&self) -> RuntimeConfig {
        RuntimeConfig {
            physical_memory: kura_mac::physical_memory(),
            ..RuntimeConfig::default()
        }
    }

    fn codecs(&self, work_dir: &Path) -> CodecSet {
        let opts = kura_mac::pinned_options();
        let mut set = kura_mac::full_codec_set(&opts, work_dir);
        let worker = WorkerCommand::own_exe(kura_mac::current_executable);
        if let Err(e) = svg_worker::install(&mut set, &opts, worker) {
            tracing::error!("SVG の働き手を組めません: {e}");
        }
        set
    }

    fn durability(&self) -> std::sync::Arc<dyn kura_core::edit::Durability> {
        std::sync::Arc::new(kura_mac::MacDurability)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// アプリは libmalloc の省メモリの設定で動く（Info.plist の `LSEnvironment`。ADR-183。外すと音声の保存・エディタのメモリが
    /// 見積もりを超える）。
    #[test]
    fn the_app_runs_with_the_space_efficient_malloc() {
        let plist = include_str!("../Info.plist");
        let env = plist
            .split_once("<key>LSEnvironment</key>")
            .map(|(_, rest)| rest)
            .expect("LSEnvironment がない");
        let expected = format!("<key>{}</key>", kura_mac::SPACE_EFFICIENT_ENV);
        let (_, value) = env.split_once(&expected).expect("設定がない");
        assert!(value.trim_start().starts_with("<string>1</string>"));
    }

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
