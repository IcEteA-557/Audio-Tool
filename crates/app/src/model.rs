use std::net::Ipv4Addr;
use std::time::{Duration, Instant};

use audiolink_net::{NicInfo, NicKind};
use audiolink_protocol::Role;

#[derive(Clone)]
pub struct Peer {
    pub name: String,
    pub addr: Ipv4Addr,
    pub control_port: u16,
    pub role: Role,
    pub seen_on: String,
    pub seen_kind: NicKind,
    pub last_seen: Instant,
}

impl Peer {
    pub fn online(&self) -> bool {
        self.last_seen.elapsed() < Duration::from_secs(3)
    }
}

pub struct Model {
    pub local_name: String,
    pub role: Role,
    pub interfaces: Vec<NicInfo>,
    pub peers: Vec<Peer>,
    pub prefer_wired: bool,
    pub target_latency_ms: u16,
    pub volume: f32,
    pub mute_local: bool,
    pub local_monitor: bool,
    pub status: String,
    pub error: String,
    pub latency_ms: f32,
    pub jitter_ms: f32,
    pub buffer_ms: f32,
    pub loss_pct: f32,
    pub rtt_ms: f32,
    pub auto_note: String,
    pub playback_devices: Vec<String>,
    pub selected_playback: usize,
    pub playback_note: String,
    pub connected: bool,
    pub connected_addr: Option<Ipv4Addr>,
}

#[derive(Clone)]
pub struct Snapshot {
    pub local_name: String,
    pub role_label: String,
    pub role: Role,
    pub interfaces: Vec<NicLine>,
    pub peers: Vec<PeerLine>,
    pub prefer_wired: bool,
    pub target_latency_ms: u16,
    pub volume: f32,
    pub mute_local: bool,
    pub local_monitor: bool,
    pub status: String,
    pub error: String,
    pub latency_ms: f32,
    pub jitter_ms: f32,
    pub buffer_ms: f32,
    pub loss_pct: f32,
    pub rtt_ms: f32,
    pub auto_note: String,
    pub playback_devices: Vec<String>,
    pub selected_playback: usize,
    pub playback_note: String,
    pub connected: bool,
}

#[derive(Clone)]
pub struct NicLine {
    pub name: String,
    pub kind: String,
    pub ipv4: String,
    pub active: bool,
}

#[derive(Clone)]
pub struct PeerLine {
    pub name: String,
    pub addr: String,
    pub nic: String,
    pub online: bool,
    pub rtt: String,
}

impl Model {
    pub fn snapshot(&self) -> Snapshot {
        Snapshot {
            local_name: self.local_name.clone(),
            role_label: self.role.label().to_string(),
            role: self.role,
            interfaces: self
                .interfaces
                .iter()
                .map(|nic| NicLine {
                    name: nic.name.clone(),
                    kind: nic.kind.label().to_string(),
                    ipv4: nic
                        .ipv4
                        .map(|ip| ip.to_string())
                        .unwrap_or_else(|| "无地址".into()),
                    active: nic.active,
                })
                .collect(),
            peers: self
                .peers
                .iter()
                .map(|peer| {
                    let rtt = if self.connected_addr == Some(peer.addr) && self.rtt_ms > 0.0 {
                        format!("{:.1} ms", self.rtt_ms)
                    } else {
                        "—".into()
                    };
                    PeerLine {
                        name: peer.name.clone(),
                        addr: peer.addr.to_string(),
                        nic: format!("{} · {}", peer.seen_kind.label(), peer.seen_on),
                        online: peer.online(),
                        rtt,
                    }
                })
                .collect(),
            prefer_wired: self.prefer_wired,
            target_latency_ms: self.target_latency_ms,
            volume: self.volume,
            mute_local: self.mute_local,
            local_monitor: self.local_monitor,
            status: self.status.clone(),
            error: self.error.clone(),
            latency_ms: self.latency_ms,
            jitter_ms: self.jitter_ms,
            buffer_ms: self.buffer_ms,
            loss_pct: self.loss_pct,
            rtt_ms: self.rtt_ms,
            auto_note: self.auto_note.clone(),
            playback_devices: self.playback_devices.clone(),
            selected_playback: self.selected_playback,
            playback_note: self.playback_note.clone(),
            connected: self.connected,
        }
    }
}

#[derive(Debug)]
pub enum Command {
    Connect(Ipv4Addr),
    Disconnect,
    SetPreferWired(bool),
    SetLatency(u16),
    SetVolume(f32),
    SetMuteLocal(bool),
    SetLocalMonitor(bool),
    SetPlaybackDevice(usize),
    Shutdown,
}
