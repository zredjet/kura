//! 進捗イベントの間引き。

/// 前回から `interval_ms` 以上たったときだけ通す。
#[derive(Debug, Clone)]
pub struct Throttle {
    interval_ms: i64,
    last: Option<i64>,
}

impl Throttle {
    /// 間隔（ミリ秒）を指定して作る。
    pub fn new(interval_ms: i64) -> Self {
        Self {
            interval_ms,
            last: None,
        }
    }

    /// `now_ms` に通してよいか。通すときは時刻を記録する。
    pub fn ready(&mut self, now_ms: i64) -> bool {
        match self.last {
            Some(last) if now_ms - last < self.interval_ms => false,
            _ => {
                self.last = Some(now_ms);
                true
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passes_first_then_waits_for_interval() {
        let mut t = Throttle::new(200);
        assert!(t.ready(1_000));
        assert!(!t.ready(1_100));
        assert!(!t.ready(1_199));
        assert!(t.ready(1_200));
        assert!(!t.ready(1_300));
    }
}
