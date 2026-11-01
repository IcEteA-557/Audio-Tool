#[cfg(target_os = "macos")]
use std::f32::consts::TAU;
#[cfg(target_os = "macos")]
use std::path::PathBuf;
#[cfg(target_os = "macos")]
use std::time::{Duration, Instant};

#[cfg(target_os = "macos")]
pub fn run() -> i32 {
    match capture_probe() {
        Ok(report) => {
            println!("{report}");
            0
        }
        Err(err) => {
            eprintln!("采集自检失败: {err}");
            1
        }
    }
}

#[cfg(not(target_os = "macos"))]
pub fn run() -> i32 {
    eprintln!("采集自检只在 Mac 上运行");
    1
}

#[cfg(target_os = "macos")]
fn capture_probe() -> Result<String, String> {
    use audiolink_engine::SystemCapture;

    eprintln!("正在打开系统音频采集…");
    let mut capture = SystemCapture::start(false, false)?;
    eprintln!("采集已启动，播放 2 秒测试音…");
    let path = PathBuf::from(std::env::temp_dir()).join("audiolink-tone.wav");
    write_tone(&path)?;
    let player = std::process::Command::new("afplay")
        .arg(&path)
        .spawn()
        .map_err(|err| format!("无法播放测试音: {err}"))?;
    let started = Instant::now();
    let mut buffer = vec![0.0f32; 4096];
    let mut samples = 0usize;
    let mut sum_squares = 0.0f64;
    let mut peak = 0.0f32;
    while started.elapsed() < Duration::from_secs(2) {
        let count = capture.pop_slice(&mut buffer);
        if count == 0 {
            std::thread::sleep(Duration::from_millis(2));
            continue;
        }
        for sample in &buffer[..count] {
            samples += 1;
            let value = *sample as f64;
            sum_squares += value * value;
            peak = peak.max(sample.abs());
        }
    }
    let _ = player.wait_with_output();
    let _ = std::fs::remove_file(&path);
    if samples == 0 {
        return Err("两秒内没有收到采集回调。请确认系统已允许音频互联录制系统音频。".into());
    }
    let rms = (sum_squares / samples as f64).sqrt();
    let heard = if rms > 0.01 {
        "检测到正在播放的系统声音。"
    } else {
        "采集回调正常，但这几秒里系统混音几乎是静音。若系统弹出授权框，请允许后重新运行自检。"
    };
    Ok(format!(
        "采集采样率 {} Hz，声道 {}，样本 {samples}，RMS {rms:.4}，峰值 {peak:.3}。{heard} 丢弃样本 {}",
        capture.sample_rate(),
        capture.channels(),
        capture.dropped.load(std::sync::atomic::Ordering::Relaxed)
    ))
}

#[cfg(target_os = "macos")]
fn write_tone(path: &PathBuf) -> Result<(), String> {
    let rate = 48_000u32;
    let frames = rate * 2;
    let mut pcm = Vec::with_capacity(frames as usize * 4);
    for index in 0..frames {
        let sample = ((index as f32 / rate as f32) * 440.0 * TAU).sin() * 0.4;
        let value = (sample * 32767.0) as i16;
        pcm.extend_from_slice(&value.to_le_bytes());
        pcm.extend_from_slice(&value.to_le_bytes());
    }
    let data_bytes = pcm.len() as u32;
    let mut wav = Vec::with_capacity(44 + pcm.len());
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + data_bytes).to_le_bytes());
    wav.extend_from_slice(b"WAVE");
    wav.extend_from_slice(b"fmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&2u16.to_le_bytes());
    wav.extend_from_slice(&rate.to_le_bytes());
    wav.extend_from_slice(&(rate * 4).to_le_bytes());
    wav.extend_from_slice(&4u16.to_le_bytes());
    wav.extend_from_slice(&16u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_bytes.to_le_bytes());
    wav.extend_from_slice(&pcm);
    std::fs::write(path, wav).map_err(|err| format!("写测试音频失败: {err}"))
}
