use std::sync::{Arc, Mutex};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

use crate::playback::PlaybackEngine;

pub struct LocalMonitor {
    _stream: cpal::Stream,
}

impl LocalMonitor {
    pub fn start(engine: Arc<Mutex<PlaybackEngine>>) -> Result<Self, String> {
        let host = cpal::default_host();
        let device = host
            .default_output_device()
            .ok_or("找不到本机播放设备")?;
        let supported = device
            .supported_output_configs()
            .map_err(|err| err.to_string())?
            .find(|range| {
                range.channels() >= 2
                    && range.min_sample_rate() <= cpal::SampleRate(48_000)
                    && range.max_sample_rate() >= cpal::SampleRate(48_000)
            })
            .ok_or("本机播放设备不支持 48kHz 立体声")?;
        let config = supported.with_sample_rate(cpal::SampleRate(48_000)).config();
        let stream = match build_stream(&device, &config, true, Arc::clone(&engine)) {
            Ok(stream) => stream,
            Err(err) => {
                log::warn!("5ms 播放缓冲不可用，改用设备默认缓冲: {err}");
                build_stream(&device, &config, false, engine)?
            }
        };
        stream.play().map_err(|err| err.to_string())?;
        Ok(Self { _stream: stream })
    }
}

fn build_stream(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    small_buffer: bool,
    engine: Arc<Mutex<PlaybackEngine>>,
) -> Result<cpal::Stream, String> {
    let mut config = config.clone();
    config.buffer_size = if small_buffer {
        cpal::BufferSize::Fixed(240)
    } else {
        cpal::BufferSize::Default
    };
    device
        .build_output_stream(
            &config,
            move |data: &mut [f32], _| {
                let mut engine = engine.lock().unwrap_or_else(|err| err.into_inner());
                if let Err(err) = engine.fill(data) {
                    log::error!("本机试听填充失败: {err}");
                    data.fill(0.0);
                }
            },
            |err| log::error!("本机试听错误: {err}"),
            None,
        )
        .map_err(|err| err.to_string())
}
