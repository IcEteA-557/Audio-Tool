use std::sync::Arc;

use audiolink_protocol::Role;

use crate::model::{Command, Snapshot};
use crate::runtime::AppRuntime;

pub fn install_fonts(ctx: &egui::Context) {
    let Some(bytes) = load_cjk_font() else {
        log::warn!("没有找到中文字体，界面文字可能无法显示");
        return;
    };
    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert(
        "cjk".into(),
        Arc::new(egui::FontData::from_owned(bytes)),
    );
    for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
        fonts
            .families
            .entry(family)
            .or_default()
            .insert(0, "cjk".into());
    }
    ctx.set_fonts(fonts);
}

fn load_cjk_font() -> Option<Vec<u8>> {
    #[cfg(target_os = "macos")]
    let candidates = ["/System/Library/Fonts/Supplemental/Arial Unicode.ttf"];
    #[cfg(target_os = "windows")]
    let candidates = [
        r"C:\Windows\Fonts\msyh.ttf",
        r"C:\Windows\Fonts\simhei.ttf",
        r"C:\Windows\Fonts\msyh.ttc",
    ];
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let candidates = [];
    for path in candidates {
        if let Ok(bytes) = std::fs::read(path) {
            return Some(bytes);
        }
    }
    None
}

pub fn draw(ui: &mut egui::Ui, snapshot: &Snapshot, selected: &mut Option<usize>, runtime: &AppRuntime) {
    ui.heading("音频互联");
    ui.label(format!(
        "本机 {}    角色 {}",
        snapshot.local_name, snapshot.role_label
    ));
    ui.label(format!("状态  {}", snapshot.status));
    if !snapshot.error.is_empty() {
        ui.colored_label(egui::Color32::from_rgb(220, 120, 80), &snapshot.error);
    }
    ui.separator();

    ui.label("网卡");
    if snapshot.interfaces.is_empty() {
        ui.label("还没有可用的 IPv4 网卡");
    }
    for nic in &snapshot.interfaces {
        let mark = if nic.active { "当前传输" } else { "" };
        ui.label(format!(
            "{}  {}  {}  {}",
            nic.kind, nic.name, nic.ipv4, mark
        ));
    }
    ui.add_space(8.0);

    ui.label("对端电脑");
    if snapshot.peers.is_empty() {
        ui.label("同一局域网里还没有另一台音频互联");
    }
    let mut clicked = None;
    for (index, peer) in snapshot.peers.iter().enumerate() {
        let state = if peer.online { "在线" } else { "离线" };
        let text = format!(
            "{}\n{}    {}    {}    往返 {}",
            peer.name, peer.addr, peer.nic, state, peer.rtt
        );
        if ui.selectable_label(selected == &Some(index), text).clicked() {
            clicked = Some(index);
        }
    }
    if let Some(index) = clicked {
        *selected = Some(index);
    }
    if snapshot.peers.is_empty() {
        *selected = None;
    } else if let Some(index) = *selected {
        if index >= snapshot.peers.len() {
            *selected = None;
        }
    }
    ui.horizontal(|ui| {
        let can_connect = selected
            .and_then(|index| snapshot.peers.get(index))
            .is_some_and(|peer| peer.online && !snapshot.connected);
        if ui.add_enabled(can_connect, egui::Button::new("连接")).clicked() {
            if let Some(peer) = selected.and_then(|index| snapshot.peers.get(index)) {
                if let Ok(addr) = peer.addr.parse() {
                    runtime.send(Command::Connect(addr));
                }
            }
        }
        if ui
            .add_enabled(snapshot.connected, egui::Button::new("断开"))
            .clicked()
        {
            runtime.send(Command::Disconnect);
        }
    });
    ui.add_space(8.0);

    let mut prefer = snapshot.prefer_wired;
    if ui.checkbox(&mut prefer, "优先有线直连").changed() {
        runtime.send(Command::SetPreferWired(prefer));
    }
    ui.label("目标延迟");
    ui.horizontal(|ui| {
        for ms in [15, 20, 30, 50] {
            if ui
                .selectable_label(snapshot.target_latency_ms == ms, format!("{ms} ms"))
                .clicked()
            {
                runtime.send(Command::SetLatency(ms));
            }
        }
    });
    let mut volume = snapshot.volume;
    if ui
        .add(egui::Slider::new(&mut volume, 0.0..=1.0).text("音量"))
        .changed()
    {
        runtime.send(Command::SetVolume(volume));
    }
    ui.label(format!(
        "实测延迟 {:.0} ms    抖动 {:.1} ms    丢包 {:.2}%    缓冲 {:.0} ms",
        snapshot.latency_ms, snapshot.jitter_ms, snapshot.loss_pct, snapshot.buffer_ms
    ));
    if snapshot.rtt_ms > 0.0 {
        ui.label(format!("往返 {:.1} ms", snapshot.rtt_ms));
    }
    if !snapshot.auto_note.is_empty() {
        ui.colored_label(egui::Color32::from_rgb(230, 180, 70), &snapshot.auto_note);
    }
    ui.add_space(8.0);

    if snapshot.role == Role::Sender {
        let mut mute = snapshot.mute_local;
        if ui.checkbox(&mut mute, "仅在 Windows 播放").changed() {
            runtime.send(Command::SetMuteLocal(mute));
        }
        let mut monitor = snapshot.local_monitor;
        if ui.checkbox(&mut monitor, "本机试听").changed() {
            runtime.send(Command::SetLocalMonitor(monitor));
        }
        ui.label("本机试听会暂时静音原来的扬声器，避免和延迟后的声音叠在一起。");
        if !snapshot.playback_devices.is_empty() {
            let name = snapshot
                .playback_devices
                .get(snapshot.selected_playback)
                .cloned()
                .unwrap_or_else(|| snapshot.playback_devices[0].clone());
            ui.label(format!("对端播放设备  {name}"));
        }
    } else {
        let device_name = snapshot
            .playback_devices
            .get(snapshot.selected_playback)
            .cloned()
            .unwrap_or_else(|| "默认设备".into());
        let current = if snapshot.follow_default {
            format!("系统默认 · {device_name}")
        } else {
            device_name
        };
        let mut chosen = None;
        let mut use_default = false;
        egui::ComboBox::from_label("播放设备")
            .selected_text(current)
            .show_ui(ui, |ui| {
                if ui
                    .selectable_label(snapshot.follow_default, "系统默认")
                    .clicked()
                {
                    use_default = true;
                }
                for (index, name) in snapshot.playback_devices.iter().enumerate() {
                    let picked = !snapshot.follow_default && index == snapshot.selected_playback;
                    if ui.selectable_label(picked, name).clicked() {
                        chosen = Some(index);
                    }
                }
            });
        if use_default {
            runtime.send(Command::UseSystemPlayback);
        } else if let Some(index) = chosen {
            runtime.send(Command::SetPlaybackDevice(index));
        }
        if !snapshot.playback_note.is_empty() {
            ui.colored_label(
                egui::Color32::from_rgb(220, 120, 80),
                &snapshot.playback_note,
            );
        }
    }

    ui.add_space(12.0);
    ui.separator();
    ui.label("Wi-Fi 抖动大时会自动放宽目标延迟。插上 USB 网卡或雷电线后，勾选优先有线直连就会改走那块网卡。");
}
