//! キャプチャ（docs/architecture.md §10）。ルール・URL パターン・撮影の手順は Chrome がなくても確かめられるように分け、
//! ジョブが Chrome の起動・DevTools への接続・登録を順に行う。

pub mod cdp;
pub mod chrome;
pub mod job;
pub mod login;
pub mod rules;
pub mod shoot;

pub use chrome::{CHROME_TOOL, profile_dir, profile_in_use};
pub use job::{
    CaptureHandler, CaptureInput, CaptureResult, capture_meta, register_png, start_capture,
};
pub use login::{LoginBrowser, OpenPage};
pub use rules::{CaptureMode, CaptureRule, RuleInput, pattern_matches};

#[cfg(test)]
#[path = "chrome_tests.rs"]
mod chrome_tests;
