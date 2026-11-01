use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::model::Model;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub prefer_wired: bool,
    pub target_latency_ms: u16,
    pub volume: f32,
    pub mute_local: bool,
    pub playback_device: Option<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            prefer_wired: true,
            target_latency_ms: 30,
            volume: 1.0,
            mute_local: false,
            playback_device: None,
        }
    }
}

impl Config {
    pub fn load() -> Self {
        let Ok(text) = fs::read_to_string(path()) else {
            return Self::default();
        };
        serde_json::from_str(&text).unwrap_or_default()
    }

    pub fn save_from(model: &Model) {
        let playback_device = model
            .playback_devices
            .get(model.selected_playback)
            .cloned();
        let config = Self {
            prefer_wired: model.prefer_wired,
            target_latency_ms: model.target_latency_ms,
            volume: model.volume,
            mute_local: model.mute_local,
            playback_device,
        };
        if let Some(dir) = path().parent() {
            let _ = fs::create_dir_all(dir);
        }
        if let Ok(text) = serde_json::to_string_pretty(&config) {
            let _ = fs::write(path(), text);
        }
    }
}

#[cfg(target_os = "windows")]
pub fn saved_playback_device() -> Option<String> {
    Config::load().playback_device
}

fn path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("audiolink")
        .join("config.json")
}

pub fn normalize_latency(ms: u16) -> u16 {
    let mut best = 30u16;
    let mut best_diff = u16::MAX;
    for candidate in [15u16, 20, 30, 50] {
        let diff = candidate.abs_diff(ms);
        if diff < best_diff {
            best = candidate;
            best_diff = diff;
        }
    }
    best
}
