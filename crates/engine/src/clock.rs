use std::sync::OnceLock;
use std::time::Instant;

fn origin() -> Instant {
    static ORIGIN: OnceLock<Instant> = OnceLock::new();
    *ORIGIN.get_or_init(Instant::now)
}

/// 本机单调时钟，单位纳秒。同一进程内的采集时间戳和 Ping 共用这个起点。
pub fn mono_ns() -> u64 {
    origin().elapsed().as_nanos() as u64
}

/// 平滑 NTP offset。offset 的定义与 `ntp_offset_and_rtt` 相同。
pub fn ntp_offset_smoothed(previous: Option<i64>, sample: i64) -> i64 {
    match previous {
        Some(prev) => prev + (sample - prev) / 8,
        None => sample,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn smoothed_offset_moves_toward_sample() {
        let next = ntp_offset_smoothed(Some(0), 800);
        assert!(next > 0 && next < 800);
        assert_eq!(ntp_offset_smoothed(None, 42), 42);
    }
}
