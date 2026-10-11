//! プレイリストの型（docs/playlists.md §3、docs/data-model.md `playlists` / `playlist_entries`）。

use serde::{Deserialize, Serialize};

/// 手動か、条件で自動か。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlaylistKind {
    Manual,
    Smart,
}

impl PlaylistKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Manual => "manual",
            Self::Smart => "smart",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "manual" => Some(Self::Manual),
            "smart" => Some(Self::Smart),
            _ => None,
        }
    }
}

/// 単位（§3.3）：ファイル単位かユニット単位か。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Granularity {
    File,
    Unit,
}

impl Granularity {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::File => "file",
            Self::Unit => "unit",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "file" => Some(Self::File),
            "unit" => Some(Self::Unit),
            _ => None,
        }
    }
}

/// 繰り返し。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RepeatMode {
    Off,
    All,
    One,
}

impl RepeatMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::All => "all",
            Self::One => "one",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "off" => Some(Self::Off),
            "all" => Some(Self::All),
            "one" => Some(Self::One),
            _ => None,
        }
    }
}

/// スマート × ユニットのタグ条件の判定方法（§4.2）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnitMatch {
    /// ユニットのタグ、またはメンバーのどれかにある。
    Any,
    /// ユニットのタグにある、または全メンバーにある。
    All,
    /// ユニットのタグだけを見る。
    UnitOnly,
}

impl UnitMatch {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Any => "any",
            Self::All => "all",
            Self::UnitOnly => "unit_only",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "any" => Some(Self::Any),
            "all" => Some(Self::All),
            "unit_only" => Some(Self::UnitOnly),
            _ => None,
        }
    }
}

/// スマートの上限の数え方。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LimitKind {
    /// 件数（ユニット単位ではユニット数）。
    Count,
    /// 合計時間（ミリ秒）。
    Duration,
}

impl LimitKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Count => "count",
            Self::Duration => "duration",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "count" => Some(Self::Count),
            "duration" => Some(Self::Duration),
            _ => None,
        }
    }
}

/// エントリが参照するもの（§3.2）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryTarget {
    Item(i64),
    Unit(i64),
    /// 名前のない folder ユニットとして扱う。
    Folder {
        folder_id: i64,
        recursive: bool,
    },
}

/// エントリ 1 行。
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub id: i64,
    pub pos: f64,
    pub target: EntryTarget,
    /// 画像の表示時間の上書き。
    pub image_duration_ms: Option<i64>,
    /// 音楽・動画のトリム。
    pub start_ms: Option<i64>,
    pub end_ms: Option<i64>,
}

impl Entry {
    /// 上書きのないエントリ。
    pub fn new(id: i64, pos: f64, target: EntryTarget) -> Self {
        Self {
            id,
            pos,
            target,
            image_duration_ms: None,
            start_ms: None,
            end_ms: None,
        }
    }
}

/// キューを作るときの設定（プレイリストの属性から。§3.1）。
#[derive(Debug, Clone, PartialEq)]
pub struct QueueOptions {
    pub granularity: Granularity,
    pub shuffle: bool,
    pub weight_by_rating: bool,
    /// 同じ種類が続いてよい最大数。0 は制限なし。
    pub max_same_kind_run: u32,
    pub dedup_items: bool,
    /// プレイリスト（またはアプリ設定）の画像の表示時間。
    pub image_duration_ms: i64,
    /// 「最終再生が 7 日以内」の判定に使う今の時刻。
    pub now_ms: i64,
}
