//! 保存 1 件の場（M14 段 2）：保存の手順（上書き・別名で保存・版に戻す、GC の照合の作り直し）が頼るものを、ジョブの文脈から
//! 切り離す。保存のジョブは自分の文脈を [`JobUnit`] で包んで渡し、一括のジョブ（M14 段 4）はアイテムごとの場を作って渡す。
//!
//! 1 件の手順は 2 つに分かれる（[`Saver`](super::Saver)）：準備（段 0〜5。読む・描く・staging に書く。DB は読むだけで、
//! 書くのは OS のデコーダの印だけ）と、コミット（段 6〜8。台帳の門を持って予告 → 配置 → トランザクション 2）。結果の記録
//! （[`Record`]）は呼び手が渡し、トランザクション 2 の中で書く（保存のジョブは途中経過の `committed`、一括はアイテムの行）。
//! 後のジョブ（サムネイル・解析・プロキシ・GC）を積むのは呼び手。

use std::sync::Arc;

use kura_engine::Monitor;
use rusqlite::Connection;

use crate::Result;
use crate::import::MediaKind;
use crate::jobs::{Control, JobContext};
use crate::library::Library;

/// 保存 1 件の場（描画のプールの中から止める指示を読むので `Sync`）。
pub(crate) trait UnitCx: Sync {
    /// 開いているライブラリ。
    fn library(&self) -> &Library;
    /// 止める指示（一時停止・中止）。
    fn control(&self) -> Control;
    /// 1 件の中の進み（割合 0〜1 と一言。DB には書かない）。
    fn progress(&self, progress: f64, message: &str);
    /// ログの 1 行。
    fn log(&self, line: &str);
    /// 途中の印（OS のデコーダで読む前後の `decoding`）をコミットする。進みと一言は画面へ送るだけ。
    fn mark(&self, result_json: &str, progress: f64, message: &str) -> Result<()>;
    /// 持ち主のジョブ：台帳の予告・版の `job_id`・下書きの `after_job` の持ち主。
    fn job(&self) -> i64;
    /// staging の書きかけの名前に入れる印（`edit-<印>-out.<拡張子>.part`）。同じジョブで並べて描く 1 件ずつは別の印にする。
    fn tag(&self) -> String {
        self.job().to_string()
    }
}

/// 保存のジョブの文脈を 1 件の場にする（書きかけの印は既定のジョブの ID）。
pub(crate) struct JobUnit<'a>(pub(crate) &'a JobContext);

impl UnitCx for JobUnit<'_> {
    fn library(&self) -> &Library {
        self.0.library()
    }

    fn control(&self) -> Control {
        self.0.control()
    }

    fn progress(&self, progress: f64, message: &str) {
        self.0.progress(progress, message);
    }

    fn log(&self, line: &str) {
        self.0.log(line);
    }

    fn mark(&self, result_json: &str, progress: f64, message: &str) -> Result<()> {
        self.0.mark(result_json, progress, message)
    }

    fn job(&self) -> i64 {
        self.0.id()
    }
}

/// 止める指示（一時停止・中止）をエンジンの中止に写す。
pub(crate) struct UnitMonitor<'a>(pub(crate) &'a dyn UnitCx);

impl Monitor for UnitMonitor<'_> {
    fn canceled(&self) -> bool {
        self.0.control() != Control::Continue
    }
}

/// 結果の記録：トランザクション 2 の中で、書いた版の結果（`outcome`）を呼び手の行に書く（保存のジョブは途中経過の
/// `committed`）。誤りを返すと、トランザクション 2 ごと戻り、置いたファイルと予告も片付ける（版は残らない）。
pub(crate) type Record = Arc<dyn Fn(&Connection, &serde_json::Value) -> Result<()> + Send + Sync>;

/// 1 件のコミットの結果。
#[derive(Debug, Clone)]
pub(crate) struct Committed {
    /// 結果（保存のジョブの `result_json`。`outcome` は `saved`・`unchanged`・`existing`）。
    pub(crate) outcome: serde_json::Value,
    /// 一言（ログに書いたもの。保存のジョブの終わりの言葉）。
    pub(crate) message: String,
    /// 版を足したアイテムの種類（後のジョブを積む）。同じ中身のアイテムがあって版を足さなかったら None。
    pub(crate) saved: Option<MediaKind>,
}
