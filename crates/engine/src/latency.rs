use std::collections::VecDeque;
use std::time::{Duration, Instant};

fn next_target(ms: u16) -> u16 {
    match ms {
        0..=15 => 20,
        16..=20 => 30,
        21..=30 => 50,
        _ => ms,
    }
}

/// 两秒内欠载达到 5 次就放宽一档目标延迟。
#[derive(Debug)]
pub struct AutoLatency {
    pub target_ms: u16,
    events: VecDeque<Instant>,
    pub note: String,
}

impl AutoLatency {
    pub fn new(target_ms: u16) -> Self {
        Self {
            target_ms,
            events: VecDeque::new(),
            note: String::new(),
        }
    }

    pub fn set_user_target(&mut self, target_ms: u16) {
        self.target_ms = target_ms;
        self.events.clear();
        self.note.clear();
    }

    pub fn record_underruns(&mut self, count: u64, now: Instant) -> bool {
        for _ in 0..count {
            self.events.push_back(now);
        }
        while self
            .events
            .front()
            .is_some_and(|stamp| now.duration_since(*stamp) > Duration::from_secs(2))
        {
            self.events.pop_front();
        }
        if self.events.len() < 5 {
            return false;
        }
        let raised = next_target(self.target_ms);
        self.events.clear();
        if raised == self.target_ms {
            self.note = "网络抖动较大，目标延迟已在 50ms".into();
            return false;
        }
        self.target_ms = raised;
        self.note = format!("网络抖动较大，已自动放宽到 {raised}ms");
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn five_underruns_raise_one_step() {
        let mut latency = AutoLatency::new(15);
        let now = Instant::now();
        assert!(!latency.record_underruns(4, now));
        assert!(latency.record_underruns(1, now));
        assert_eq!(latency.target_ms, 20);
        assert!(latency.note.contains("20"));
    }

    #[test]
    fn user_target_clears_the_note() {
        let mut latency = AutoLatency::new(30);
        let now = Instant::now();
        latency.record_underruns(5, now);
        assert_eq!(latency.target_ms, 50);
        latency.set_user_target(20);
        assert_eq!(latency.target_ms, 20);
        assert!(latency.note.is_empty());
    }
}
