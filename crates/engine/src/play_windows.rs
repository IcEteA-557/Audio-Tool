use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use wasapi::{
    Device, DeviceEnumerator, Direction, SampleType, StreamMode, WaveFormat, initialize_mta,
    initialize_sta,
};

use crate::playback::PlaybackEngine;

fn ensure_com() {
    // 窗口在主线程上创建。winit 会调用 OleInitialize，线程必须留在单线程套间。
    // 若这里先把主线程初始化成多线程套间，窗口会直接创建失败。
    // 播放在单独的线程里，那边继续用多线程套间。
    if std::thread::current().name() == Some("main") {
        let _ = initialize_sta();
    } else {
        let _ = initialize_mta();
    }
}

fn enumerator() -> Result<DeviceEnumerator, String> {
    ensure_com();
    DeviceEnumerator::new().map_err(|err| err.to_string())
}

pub fn list_render_devices() -> Result<Vec<String>, String> {
    let collection = enumerator()?
        .get_device_collection(&Direction::Render)
        .map_err(|err| err.to_string())?;
    let count = collection.get_nbr_devices().map_err(|err| err.to_string())?;
    let mut names = Vec::new();
    for index in 0..count {
        let device = collection
            .get_device_at_index(index)
            .map_err(|err| err.to_string())?;
        names.push(device.get_friendlyname().map_err(|err| err.to_string())?);
    }
    Ok(names)
}

pub fn default_render_device() -> Option<String> {
    enumerator()
        .ok()?
        .get_default_device(&Direction::Render)
        .ok()?
        .get_friendlyname()
        .ok()
}

pub struct WasapiPlayback {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl WasapiPlayback {
    pub fn start(
        engine: Arc<Mutex<PlaybackEngine>>,
        device_name: Option<String>,
        note: Arc<Mutex<String>>,
    ) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        let thread = thread::spawn(move || {
            let mut preferred = device_name;
            loop {
                if thread_stop.load(Ordering::Relaxed) {
                    break;
                }
                match play_loop(
                    Arc::clone(&engine),
                    preferred.clone(),
                    Arc::clone(&note),
                    Arc::clone(&thread_stop),
                ) {
                    Ok(()) => break,
                    Err(err) => {
                        if preferred.is_none() || thread_stop.load(Ordering::Relaxed) {
                            if let Ok(mut note) = note.lock() {
                                *note = err;
                            }
                            break;
                        }
                        preferred = None;
                        if let Ok(mut note) = note.lock() {
                            *note = "播放设备已断开，已改用默认设备".into();
                        }
                        thread::sleep(Duration::from_millis(400));
                    }
                }
            }
        });
        Self {
            stop,
            thread: Some(thread),
        }
    }
}

impl Drop for WasapiPlayback {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn play_loop(
    engine: Arc<Mutex<PlaybackEngine>>,
    device_name: Option<String>,
    note: Arc<Mutex<String>>,
    stop: Arc<AtomicBool>,
) -> Result<(), String> {
    let devices = enumerator()?;
    let mut device = match &device_name {
        Some(name) => find_device(&devices, name)?.ok_or_else(|| {
            let fallback = default_render_device().unwrap_or_else(|| "默认设备".into());
            if let Ok(mut note) = note.lock() {
                *note = format!("播放设备“{name}”不可用，已改用{fallback}");
            }
            String::new()
        }),
        None => devices
            .get_default_device(&Direction::Render)
            .map_err(|err| err.to_string()),
    };
    if device.is_err() {
        if let Ok(mut note) = note.lock() {
            if note.is_empty() {
                *note = "播放设备已断开，已改用默认设备".into();
            }
        }
        device = devices
            .get_default_device(&Direction::Render)
            .map_err(|err| err.to_string());
    }
    let device = device?;
    let mut client = device.get_iaudioclient().map_err(|err| err.to_string())?;
    let format = WaveFormat::new(32, 32, &SampleType::Float, 48_000, 2, None);
    if client
        .initialize_client(
            &format,
            &Direction::Render,
            &StreamMode::EventsShared {
                autoconvert: true,
                buffer_duration_hns: 50_000,
            },
        )
        .is_err()
    {
        client = device.get_iaudioclient().map_err(|err| err.to_string())?;
        client
            .initialize_client(
                &format,
                &Direction::Render,
                &StreamMode::EventsShared {
                    autoconvert: true,
                    buffer_duration_hns: 100_000,
                },
            )
            .map_err(|err| format!("初始化 WASAPI 失败: {err}"))?;
    }
    let event = client.set_get_eventhandle().map_err(|err| err.to_string())?;
    let render = client.get_audiorenderclient().map_err(|err| err.to_string())?;
    let align = format.get_blockalign() as usize;
    let warmup = client
        .get_available_space_in_frames()
        .map_err(|err| err.to_string())? as usize;
    render
        .write_to_device(warmup, &vec![0u8; warmup * align], None)
        .map_err(|err| err.to_string())?;
    client.start_stream().map_err(|err| err.to_string())?;

    while !stop.load(Ordering::Relaxed) {
        if event.wait_for_event(200).is_err() {
            continue;
        }
        let frames = match client.get_available_space_in_frames() {
            Ok(frames) => frames as usize,
            Err(_) => break,
        };
        if frames == 0 {
            continue;
        }
        let mut bytes = vec![0u8; frames * align];
        {
            let mut engine = engine.lock().unwrap_or_else(|err| err.into_inner());
            let mut samples = vec![0.0f32; frames * 2];
            if let Err(err) = engine.fill(&mut samples) {
                log::error!("WASAPI 填充失败: {err}");
            }
            for (index, sample) in samples.iter().enumerate() {
                let raw = sample.to_le_bytes();
                let start = index * 4;
                bytes[start..start + 4].copy_from_slice(&raw);
            }
        }
        if render.write_to_device(frames, &bytes, None).is_err() {
            return Err("播放设备已断开，已改用默认设备".into());
        }
    }
    let _ = client.stop_stream();
    Ok(())
}

fn find_device(devices: &DeviceEnumerator, name: &str) -> Result<Option<Device>, String> {
    let collection = devices
        .get_device_collection(&Direction::Render)
        .map_err(|err| err.to_string())?;
    let count = collection.get_nbr_devices().map_err(|err| err.to_string())?;
    for index in 0..count {
        let device = collection
            .get_device_at_index(index)
            .map_err(|err| err.to_string())?;
        let friendly = device.get_friendlyname().map_err(|err| err.to_string())?;
        if friendly == name {
            return Ok(Some(device));
        }
    }
    Ok(None)
}
