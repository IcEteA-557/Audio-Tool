//! 抖动缓冲、时钟校正和两端共用的播放引擎。

mod clock;
mod jitter;
mod latency;
mod packetizer;
mod playback;
mod resample;

#[cfg(target_os = "macos")]
mod capture_macos;
#[cfg(target_os = "macos")]
mod monitor;
#[cfg(target_os = "windows")]
mod play_windows;

pub use clock::{mono_ns, ntp_offset_smoothed};
pub use jitter::JitterBuffer;
pub use latency::AutoLatency;
pub use packetizer::Packetizer;
pub use playback::{PlaybackEngine, PlaybackStats};
pub use resample::speed_ratio;

#[cfg(target_os = "macos")]
pub use capture_macos::SystemCapture;
#[cfg(target_os = "macos")]
pub use monitor::LocalMonitor;

#[cfg(target_os = "windows")]
pub use play_windows::{default_render_device, list_render_devices, WasapiPlayback};
