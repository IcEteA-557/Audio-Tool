use std::io::{ErrorKind, Read, Write};
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, TcpListener, TcpStream, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(target_os = "macos")]
use std::sync::atomic::AtomicU32;
use std::sync::mpsc::{self, Receiver, Sender};
#[cfg(target_os = "macos")]
use std::sync::mpsc::SyncSender;
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use audiolink_engine::{mono_ns, PlaybackEngine};
use audiolink_net::{
    bind_udp, connect_tcp, ipv4_same_subnet, list_nics, listen_tcp, send_beacon, transport_ip,
    BeaconSocket, NicKind,
};
use audiolink_protocol::{
    encode_frame, AudioPacket, Beacon, ControlMessage, FrameDecoder, Role, CHANNELS, CONTROL_PORT,
    PROTOCOL_VERSION, SAMPLE_RATE,
};

#[cfg(target_os = "macos")]
use audiolink_engine::{LocalMonitor, Packetizer, SystemCapture};
#[cfg(target_os = "windows")]
use audiolink_engine::{default_render_device, list_render_devices, WasapiPlayback};

use crate::config::{normalize_latency, Config};
use crate::model::{Command, Model, Peer};

pub struct AppRuntime {
    model: Arc<Mutex<Model>>,
    commands: Sender<Command>,
    stop: Arc<AtomicBool>,
    threads: Vec<JoinHandle<()>>,
}

impl AppRuntime {
    pub fn start() -> Self {
        let config = Config::load();
        let role = local_role();
        let (playback_devices, selected_playback, follow_default, playback_note) =
            load_playback_devices();
        let model = Arc::new(Mutex::new(Model {
            local_name: computer_name(),
            role,
            interfaces: Vec::new(),
            peers: Vec::new(),
            prefer_wired: config.prefer_wired,
            target_latency_ms: normalize_latency(config.target_latency_ms),
            volume: config.volume.clamp(0.0, 1.0),
            mute_local: config.mute_local,
            local_monitor: false,
            status: "正在监听局域网".into(),
            error: String::new(),
            latency_ms: 0.0,
            jitter_ms: 0.0,
            buffer_ms: 0.0,
            loss_pct: 0.0,
            rtt_ms: 0.0,
            auto_note: String::new(),
            playback_devices,
            selected_playback,
            follow_default,
            playback_note,
            connected: false,
            connected_addr: None,
        }));
        let (commands, cmd_rx) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let discovery_model = Arc::clone(&model);
        let discovery_stop = Arc::clone(&stop);
        let supervisor_model = Arc::clone(&model);
        let supervisor_stop = Arc::clone(&stop);
        let threads = vec![
            thread::spawn(move || discovery_loop(discovery_model, discovery_stop)),
            thread::spawn(move || supervisor_loop(supervisor_model, cmd_rx, supervisor_stop)),
        ];
        Self {
            model,
            commands,
            stop,
            threads,
        }
    }

    pub fn snapshot(&self) -> crate::model::Snapshot {
        lock(&self.model).snapshot()
    }

    pub fn send(&self, command: Command) {
        let _ = self.commands.send(command);
    }
}

impl Drop for AppRuntime {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = self.commands.send(Command::Shutdown);
        for thread in self.threads.drain(..) {
            let _ = thread.join();
        }
    }
}

fn local_role() -> Role {
    #[cfg(target_os = "macos")]
    {
        Role::Sender
    }
    #[cfg(target_os = "windows")]
    {
        Role::Receiver
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        Role::Sender
    }
}

fn load_playback_devices() -> (Vec<String>, usize, bool, String) {
    #[cfg(target_os = "windows")]
    {
        match list_render_devices() {
            Ok(devices) => {
                let default_index = default_render_device()
                    .as_ref()
                    .and_then(|name| devices.iter().position(|item| item == name))
                    .unwrap_or(0);
                let config = Config::load();
                if config.playback_pinned {
                    if let Some(saved) = config.playback_device {
                        if let Some(index) = devices.iter().position(|name| name == &saved) {
                            return (devices, index, false, String::new());
                        }
                        let note = format!("上次的播放设备“{saved}”不在了，已改用默认设备");
                        return (devices, default_index, true, note);
                    }
                }
                (devices, default_index, true, String::new())
            }
            Err(err) => (Vec::new(), 0, true, err),
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        (Vec::new(), 0, true, String::new())
    }
}

fn computer_name() -> String {
    hostname::get()
        .ok()
        .and_then(|name| name.into_string().ok())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "这台电脑".into())
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|err| err.into_inner())
}

fn discovery_loop(model: Arc<Mutex<Model>>, stop: Arc<AtomicBool>) {
    let socket = match BeaconSocket::bind() {
        Ok(socket) => socket,
        Err(err) => {
            lock(&model).error = format!("信标端口绑定失败: {err}");
            log::error!("信标端口绑定失败: {err}");
            return;
        }
    };
    let mut last_send = Instant::now() - Duration::from_secs(5);
    let mut last_nics = Instant::now() - Duration::from_secs(5);
    while !stop.load(Ordering::Relaxed) {
        if last_nics.elapsed() >= Duration::from_secs(2) {
            refresh_nics(&model);
            last_nics = Instant::now();
        }
        if last_send.elapsed() >= Duration::from_secs(1) {
            send_our_beacon(&model);
            last_send = Instant::now();
        }
        loop {
            match socket.recv() {
                Ok((beacon, source)) => ingest_beacon(&model, beacon, source),
                Err(err) if err.kind() == ErrorKind::WouldBlock => break,
                Err(err) if err.kind() == ErrorKind::InvalidData => break,
                Err(err) => {
                    log::warn!("读取信标失败: {err}");
                    break;
                }
            }
        }
        expire_peers(&model);
        thread::sleep(Duration::from_millis(200));
    }
}

fn refresh_nics(model: &Mutex<Model>) {
    let prefer = lock(model).prefer_wired;
    let mut nics = list_nics();
    let bind = transport_ip(&nics, prefer);
    for nic in &mut nics {
        nic.active = if bind.is_unspecified() {
            nic.ipv4
                .is_some_and(|ip| nic.kind != NicKind::Other && announces_on(ip, nic.kind))
        } else {
            nic.ipv4 == Some(bind)
        };
    }
    lock(model).interfaces = nics;
}

fn send_our_beacon(model: &Mutex<Model>) {
    let guard = lock(model);
    let beacon = Beacon {
        role: guard.role,
        control_port: CONTROL_PORT,
        audio_port: 0,
        name: guard.local_name.clone(),
    };
    let prefer = guard.prefer_wired;
    let nics = guard.interfaces.clone();
    drop(guard);
    let Some(payload) = beacon.encode() else {
        return;
    };
    let bind = transport_ip(&nics, prefer);
    for nic in &nics {
        let Some(ip) = nic.ipv4 else {
            continue;
        };
        if nic.kind == NicKind::Other || !announces_on(ip, nic.kind) {
            continue;
        }
        if !bind.is_unspecified() && ip != bind {
            continue;
        }
        if let Err(err) = send_beacon(ip, nic.netmask, &payload) {
            log::debug!("从 {ip} 发送信标失败: {err}");
        }
    }
}

/// 169.254 自动地址在没接线的虚拟网卡上无法广播。有线直连仍保留，两台电脑可能靠它互通。
fn announces_on(ip: Ipv4Addr, kind: NicKind) -> bool {
    let link_local = matches!(ip.octets(), [169, 254, _, _]);
    !link_local || kind == NicKind::DirectCable
}

fn ingest_beacon(model: &Mutex<Model>, beacon: Beacon, source: Ipv4Addr) {
    let mut guard = lock(model);
    if beacon.role == guard.role || beacon.name == guard.local_name && beacon.role == guard.role {
        return;
    }
    if beacon.role == guard.role {
        return;
    }
    let seen = guard
        .interfaces
        .iter()
        .find(|nic| {
            matches!(
                (nic.ipv4, nic.netmask),
                (Some(ip), Some(mask)) if ipv4_same_subnet(ip, mask, source)
            )
        })
        .cloned();
    let (seen_on, seen_kind) = seen
        .map(|nic| (nic.name, nic.kind))
        .unwrap_or_else(|| ("未知网卡".into(), NicKind::Other));
    if let Some(peer) = guard
        .peers
        .iter_mut()
        .find(|peer| peer.addr == source && peer.control_port == beacon.control_port)
    {
        peer.name = beacon.name;
        peer.role = beacon.role;
        peer.seen_on = seen_on;
        peer.seen_kind = seen_kind;
        peer.last_seen = Instant::now();
        return;
    }
    guard.peers.push(Peer {
        name: beacon.name,
        addr: source,
        control_port: beacon.control_port,
        role: beacon.role,
        seen_on,
        seen_kind,
        last_seen: Instant::now(),
    });
}

fn expire_peers(model: &Mutex<Model>) {
    let mut guard = lock(model);
    guard
        .peers
        .retain(|peer| peer.last_seen.elapsed() < Duration::from_secs(10));
}

fn supervisor_loop(model: Arc<Mutex<Model>>, cmd_rx: Receiver<Command>, stop: Arc<AtomicBool>) {
    let mut supervisor = Supervisor::new(model, stop);
    while !supervisor.stop.load(Ordering::Relaxed) {
        while let Ok(command) = cmd_rx.try_recv() {
            if matches!(command, Command::Shutdown) {
                supervisor.stop.store(true, Ordering::Relaxed);
                break;
            }
            supervisor.handle_command(command);
        }
        supervisor.ensure_listener();
        supervisor.accept_one();
        supervisor.start_requested_connection();
        supervisor.refresh_playback_list();
        supervisor.sync_audio();
        supervisor.poll_session();
        supervisor.publish_status();
        thread::sleep(Duration::from_millis(2));
    }
}

struct Supervisor {
    model: Arc<Mutex<Model>>,
    stop: Arc<AtomicBool>,
    listener: Option<TcpListener>,
    listener_bind: Option<Ipv4Addr>,
    session: Option<LiveSession>,
    want_connect: Option<Ipv4Addr>,
    playback_engine: Option<Arc<Mutex<PlaybackEngine>>>,
    #[cfg(target_os = "macos")]
    capture: Option<CaptureWorker>,
    #[cfg(target_os = "macos")]
    monitor: Option<MonitorHold>,
    #[cfg(target_os = "windows")]
    playback: Option<WasapiPlayback>,
    #[cfg(target_os = "windows")]
    playback_note: Arc<Mutex<String>>,
    #[cfg(target_os = "windows")]
    playback_key: String,
    #[cfg(target_os = "windows")]
    last_default_check: Instant,
    #[cfg(target_os = "windows")]
    last_device_list: Instant,
}

impl Supervisor {
    fn new(model: Arc<Mutex<Model>>, stop: Arc<AtomicBool>) -> Self {
        Self {
            model,
            stop,
            listener: None,
            listener_bind: None,
            session: None,
            want_connect: None,
            playback_engine: None,
            #[cfg(target_os = "macos")]
            capture: None,
            #[cfg(target_os = "macos")]
            monitor: None,
            #[cfg(target_os = "windows")]
            playback: None,
            #[cfg(target_os = "windows")]
            playback_note: Arc::new(Mutex::new(String::new())),
            #[cfg(target_os = "windows")]
            playback_key: String::new(),
            #[cfg(target_os = "windows")]
            last_default_check: Instant::now() - Duration::from_secs(5),
            #[cfg(target_os = "windows")]
            last_device_list: Instant::now() - Duration::from_secs(5),
        }
    }

    fn handle_command(&mut self, command: Command) {
        match command {
            Command::Connect(addr) => self.want_connect = Some(addr),
            Command::Disconnect => self.close_session("已断开"),
            Command::SetPreferWired(value) => {
                lock(&self.model).prefer_wired = value;
                Config::save_from(&lock(&self.model));
            }
            Command::SetLatency(ms) => {
                let ms = normalize_latency(ms);
                {
                    let mut model = lock(&self.model);
                    model.target_latency_ms = ms;
                    model.auto_note.clear();
                }
                if let Some(engine) = &self.playback_engine {
                    lock(engine).set_user_target(ms);
                }
                if let Some(session) = &mut self.session {
                    let _ = session.queue(&ControlMessage::SetTargetLatency {
                        target_latency_ms: ms,
                        automatic: false,
                    });
                }
                Config::save_from(&lock(&self.model));
            }
            Command::SetVolume(volume) => {
                let volume = volume.clamp(0.0, 1.0);
                lock(&self.model).volume = volume;
                if let Some(session) = &mut self.session {
                    let _ = session.queue(&ControlMessage::SetVolume { volume });
                }
                Config::save_from(&lock(&self.model));
            }
            Command::SetMuteLocal(mute) => {
                lock(&self.model).mute_local = mute;
                Config::save_from(&lock(&self.model));
            }
            Command::SetLocalMonitor(enabled) => {
                lock(&self.model).local_monitor = enabled;
            }
            Command::SetPlaybackDevice(index) => {
                {
                    let mut model = lock(&self.model);
                    model.follow_default = false;
                    model.selected_playback = index;
                }
                Config::save_from(&lock(&self.model));
                #[cfg(target_os = "windows")]
                {
                    self.playback_key.clear();
                }
            }
            Command::UseSystemPlayback => {
                lock(&self.model).follow_default = true;
                Config::save_from(&lock(&self.model));
                #[cfg(target_os = "windows")]
                {
                    self.playback_key.clear();
                    self.last_default_check = Instant::now() - Duration::from_secs(5);
                }
            }
            Command::Shutdown => self.stop.store(true, Ordering::Relaxed),
        }
    }

    fn ensure_listener(&mut self) {
        let bind = {
            let model = lock(&self.model);
            transport_ip(&model.interfaces, model.prefer_wired)
        };
        if self.listener_bind == Some(bind) && self.listener.is_some() {
            return;
        }
        if self.session.is_some() && self.listener_bind.is_some() {
            self.close_session("传输网卡变了，连接已断开");
        }
        match listen_tcp(bind, CONTROL_PORT) {
            Ok(listener) => {
                self.listener = Some(listener);
                self.listener_bind = Some(bind);
                lock(&self.model).error.clear();
            }
            Err(err) => {
                self.listener = None;
                self.listener_bind = None;
                lock(&self.model).error = format!("控制端口 {CONTROL_PORT} 绑定失败: {err}");
            }
        }
    }

    fn accept_one(&mut self) {
        if self.session.is_some() {
            return;
        }
        let Some(listener) = &self.listener else {
            return;
        };
        match listener.accept() {
            Ok((stream, addr)) => {
                let SocketAddr::V4(addr) = addr else {
                    return;
                };
                match LiveSession::accepted(*addr.ip(), stream, &self.model) {
                    Ok(session) => self.session = Some(session),
                    Err(err) => lock(&self.model).error = err,
                }
            }
            Err(err) if err.kind() == ErrorKind::WouldBlock => {}
            Err(err) => log::warn!("接受连接失败: {err}"),
        }
    }

    fn start_requested_connection(&mut self) {
        let Some(addr) = self.want_connect.take() else {
            return;
        };
        if self.session.is_some() {
            self.close_session("正在切换连接");
        }
        let peer = {
            let model = lock(&self.model);
            model
                .peers
                .iter()
                .find(|peer| peer.addr == addr && peer.online())
                .cloned()
        };
        let Some(peer) = peer else {
            lock(&self.model).error = "这台电脑刚刚离线了".into();
            return;
        };
        let bind = self.listener_bind.unwrap_or(Ipv4Addr::UNSPECIFIED);
        match LiveSession::connect(&peer, bind, &self.model) {
            Ok(session) => {
                lock(&self.model).error.clear();
                lock(&self.model).status = format!("正在连接 {}", peer.name);
                self.session = Some(session);
            }
            Err(err) => lock(&self.model).error = err,
        }
    }

    fn sync_audio(&mut self) {
        #[cfg(target_os = "macos")]
        self.sync_mac_audio();
        #[cfg(target_os = "windows")]
        self.sync_windows_audio();
    }

    #[cfg(target_os = "macos")]
    fn sync_mac_audio(&mut self) {
        let (need_capture, monitor, target, volume, mute) = {
            let model = lock(&self.model);
            (
                model.connected || model.local_monitor || self.session.is_some(),
                model.local_monitor,
                model.target_latency_ms,
                model.volume,
                model.mute_local || model.local_monitor,
            )
        };
        if need_capture && self.capture.is_none() {
            self.capture = Some(CaptureWorker::spawn());
        }
        if !need_capture {
            self.capture = None;
        }
        if let Some(capture) = &self.capture {
            capture.set_volume(volume);
            capture.set_route(mute, monitor);
        }
        if monitor && self.monitor.is_none() {
            match PlaybackEngine::new(target, 5.0) {
                Ok(engine) => {
                    let engine = Arc::new(Mutex::new(engine));
                    match LocalMonitor::start(Arc::clone(&engine)) {
                        Ok(stream) => {
                            self.playback_engine = Some(Arc::clone(&engine));
                            self.monitor = Some(MonitorHold { _stream: stream });
                            lock(&self.model).error.clear();
                        }
                        Err(err) => lock(&self.model).error = err,
                    }
                }
                Err(err) => lock(&self.model).error = err,
            }
        }
        if !monitor {
            self.monitor = None;
            if local_role() == Role::Sender {
                self.playback_engine = None;
            }
        }
        if let Some(capture) = &self.capture {
            if let Some(err) = capture.last_error() {
                lock(&self.model).error = err;
            }
            while let Some(packet) = capture.try_recv() {
                if let Some(session) = &mut self.session {
                    session.send_audio(&packet);
                }
                if monitor {
                    if let Some(engine) = &self.playback_engine {
                        lock(engine).push_packet(packet, mono_ns());
                    }
                }
            }
        }
    }

    fn refresh_playback_list(&mut self) {
        #[cfg(target_os = "windows")]
        {
            if self.last_device_list.elapsed() < Duration::from_secs(2) {
                return;
            }
            self.last_device_list = Instant::now();
            let Ok(devices) = list_render_devices() else {
                return;
            };
            let mut model = lock(&self.model);
            if !model.follow_default {
                let current = model
                    .playback_devices
                    .get(model.selected_playback)
                    .cloned();
                if let Some(name) = current {
                    if let Some(index) = devices.iter().position(|item| item == &name) {
                        model.selected_playback = index;
                    }
                }
            }
            model.playback_devices = devices;
        }
    }

    #[cfg(target_os = "windows")]
    fn sync_windows_audio(&mut self) {
        let streaming = self.session.as_ref().is_some_and(|session| session.is_streaming());
        if !streaming {
            self.playback = None;
            self.playback_engine = None;
            self.playback_key.clear();
            return;
        }
        let (target, follow) = {
            let model = lock(&self.model);
            (model.target_latency_ms, model.follow_default)
        };
        let (device_name, key) = if follow {
            if self.playback.is_some()
                && self.last_default_check.elapsed() < Duration::from_millis(500)
            {
                (None, self.playback_key.clone())
            } else {
                self.last_default_check = Instant::now();
                let name = default_render_device().unwrap_or_default();
                {
                    let mut model = lock(&self.model);
                    if let Some(index) = model.playback_devices.iter().position(|item| item == &name)
                    {
                        model.selected_playback = index;
                    }
                }
                (None, format!("default:{name}"))
            }
        } else {
            let name = {
                let model = lock(&self.model);
                model
                    .playback_devices
                    .get(model.selected_playback)
                    .cloned()
            };
            let key = name.clone().unwrap_or_default();
            (name, key)
        };
        if self.playback.is_some() && self.playback_key == key {
            if let Ok(note) = self.playback_note.lock() {
                lock(&self.model).playback_note = note.clone();
            }
            return;
        }
        self.playback = None;
        let engine = match &self.playback_engine {
            Some(engine) => Arc::clone(engine),
            None => match PlaybackEngine::new(target, 10.0) {
                Ok(engine) => {
                    let engine = Arc::new(Mutex::new(engine));
                    self.playback_engine = Some(Arc::clone(&engine));
                    engine
                }
                Err(err) => {
                    lock(&self.model).error = err;
                    return;
                }
            },
        };
        lock(&engine).set_user_target(target);
        self.playback = Some(WasapiPlayback::start(
            engine,
            device_name,
            Arc::clone(&self.playback_note),
        ));
        self.playback_key = key;
    }

    fn poll_session(&mut self) {
        let Some(session) = self.session.as_mut() else {
            return;
        };
        let engine = self.playback_engine.clone();
        if let Err(err) = session.poll(&self.model, engine.as_ref()) {
            self.close_session(&err);
            return;
        }
        let stale_beacon = {
            let model = lock(&self.model);
            model
                .peers
                .iter()
                .find(|peer| Some(peer.addr) == model.connected_addr)
                .map(|peer| !peer.online())
                .unwrap_or(false)
        };
        let tcp_stale = self
            .session
            .as_ref()
            .is_some_and(|session| session.last_io.elapsed() > Duration::from_secs(3));
        if stale_beacon && tcp_stale {
            self.close_session("对端已离线");
        }
    }

    fn publish_status(&mut self) {
        let streaming = self.session.as_ref().is_some_and(|session| session.is_streaming());
        let peer_addr = self.session.as_ref().filter(|session| session.is_streaming()).map(|session| session.peer);
        {
            let mut model = lock(&self.model);
            model.connected = streaming;
            model.connected_addr = peer_addr;
        }
        self.publish_local_stats();
        let mut model = lock(&self.model);
        let online = model.peers.iter().filter(|peer| peer.online()).count();
        model.status = if streaming {
            "正在播放".into()
        } else if self.session.is_some() {
            "正在连接".into()
        } else if model.local_monitor {
            "本机试听中".into()
        } else if online > 0 {
            format!("已发现 {online} 台电脑")
        } else {
            "正在监听局域网".into()
        };
    }

    fn publish_local_stats(&mut self) {
        let use_local = local_role() == Role::Receiver || !lock(&self.model).connected;
        if !use_local {
            return;
        }
        let Some(engine) = self.playback_engine.clone() else {
            return;
        };
        let (stats, raised) = {
            let mut engine = lock(&engine);
            let raised = engine.take_auto_target();
            (engine.stats(), raised)
        };
        if let Some(ms) = raised {
            if let Some(session) = &mut self.session {
                let _ = session.queue(&ControlMessage::SetTargetLatency {
                    target_latency_ms: ms,
                    automatic: true,
                });
            }
            let mut model = lock(&self.model);
            model.target_latency_ms = ms;
            model.auto_note = stats.auto_note.clone();
        }
        let mut model = lock(&self.model);
        model.latency_ms = stats.latency_ms;
        model.jitter_ms = stats.jitter_ms;
        model.buffer_ms = stats.buffer_ms;
        model.loss_pct = stats.loss_pct;
        if stats.rtt_ms > 0.0 {
            model.rtt_ms = stats.rtt_ms;
        }
        if !stats.auto_note.is_empty() {
            model.auto_note = stats.auto_note;
        }
    }

    fn close_session(&mut self, reason: &str) {
        if let Some(session) = &mut self.session {
            let _ = session.queue(&ControlMessage::StopStream);
            let _ = session.flush();
        }
        self.session = None;
        let mut model = lock(&self.model);
        model.connected = false;
        model.connected_addr = None;
        model.error = reason.to_string();
        model.latency_ms = 0.0;
        model.jitter_ms = 0.0;
        model.buffer_ms = 0.0;
        model.loss_pct = 0.0;
    }
}

#[cfg(target_os = "macos")]
struct MonitorHold {
    _stream: LocalMonitor,
}

#[cfg(target_os = "macos")]
struct CaptureWorker {
    stop: Arc<AtomicBool>,
    mute: Arc<AtomicBool>,
    exclude: Arc<AtomicBool>,
    volume: Arc<AtomicU32>,
    error: Arc<Mutex<Option<String>>>,
    rx: Receiver<AudioPacket>,
    thread: Option<JoinHandle<()>>,
}

#[cfg(target_os = "macos")]
impl CaptureWorker {
    fn spawn() -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let mute = Arc::new(AtomicBool::new(false));
        let exclude = Arc::new(AtomicBool::new(false));
        let volume = Arc::new(AtomicU32::new(1.0f32.to_bits()));
        let error = Arc::new(Mutex::new(None));
        let (tx, rx) = mpsc::sync_channel(32);
        let thread_stop = Arc::clone(&stop);
        let thread_mute = Arc::clone(&mute);
        let thread_exclude = Arc::clone(&exclude);
        let thread_volume = Arc::clone(&volume);
        let thread_error = Arc::clone(&error);
        let thread = thread::spawn(move || {
            capture_loop(thread_stop, thread_mute, thread_exclude, thread_volume, thread_error, tx);
        });
        Self {
            stop,
            mute,
            exclude,
            volume,
            error,
            rx,
            thread: Some(thread),
        }
    }

    fn set_volume(&self, volume: f32) {
        self.volume.store(volume.to_bits(), Ordering::Relaxed);
    }

    fn set_route(&self, mute: bool, exclude: bool) {
        self.mute.store(mute, Ordering::Relaxed);
        self.exclude.store(exclude, Ordering::Relaxed);
    }

    fn try_recv(&self) -> Option<AudioPacket> {
        self.rx.try_recv().ok()
    }

    fn last_error(&self) -> Option<String> {
        lock(&self.error).clone()
    }
}

#[cfg(target_os = "macos")]
impl Drop for CaptureWorker {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[cfg(target_os = "macos")]
fn capture_loop(
    stop: Arc<AtomicBool>,
    mute: Arc<AtomicBool>,
    exclude: Arc<AtomicBool>,
    volume: Arc<AtomicU32>,
    error: Arc<Mutex<Option<String>>>,
    tx: SyncSender<AudioPacket>,
) {
    let mut capture: Option<SystemCapture> = None;
    let mut applied = (false, false);
    let mut packetizer = Packetizer::new();
    let mut buffer = vec![0.0f32; 4096];
    while !stop.load(Ordering::Relaxed) {
        let desired = (mute.load(Ordering::Relaxed), exclude.load(Ordering::Relaxed));
        if capture.is_none() || applied != desired {
            capture = None;
            match SystemCapture::start(desired.0, desired.1) {
                Ok(next) => {
                    capture = Some(next);
                    applied = desired;
                    *lock(&error) = None;
                }
                Err(err) => {
                    *lock(&error) = Some(err);
                    thread::sleep(Duration::from_millis(500));
                    continue;
                }
            }
        }
        let Some(capture) = capture.as_mut() else {
            continue;
        };
        packetizer.set_volume(f32::from_bits(volume.load(Ordering::Relaxed)));
        let count = capture.pop_slice(&mut buffer);
        if count == 0 {
            thread::sleep(Duration::from_millis(1));
            continue;
        }
        match packetizer.push_interleaved(&buffer[..count], capture.channels(), capture.sample_rate()) {
            Ok(packets) => {
                for mut packet in packets {
                    packet.timestamp_ns = mono_ns();
                    if tx.try_send(packet).is_err() {
                        break;
                    }
                }
            }
            Err(err) => *lock(&error) = Some(err),
        }
    }
}

struct LiveSession {
    stream: TcpStream,
    udp: UdpSocket,
    decoder: FrameDecoder,
    write_buf: Vec<u8>,
    peer: Ipv4Addr,
    remote_audio_port: u16,
    local_audio_port: u16,
    initiator: bool,
    sent_hello: bool,
    announced: bool,
    sent_devices: bool,
    phase: Phase,
    last_io: Instant,
    last_ping: Instant,
    last_stats: Instant,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    WaitHello,
    Streaming,
}

impl LiveSession {
    fn connect(peer: &Peer, bind_ip: Ipv4Addr, model: &Mutex<Model>) -> Result<Self, String> {
        let role = lock(model).role;
        let stream = connect_tcp(
            bind_ip,
            SocketAddrV4::new(peer.addr, peer.control_port),
            Duration::from_secs(2),
        )
        .map_err(|err| format!("连接 {} 失败: {err}", peer.name))?;
        Self::new(peer.addr, stream, true, role, bind_ip)
    }

    fn accepted(peer: Ipv4Addr, stream: TcpStream, model: &Mutex<Model>) -> Result<Self, String> {
        // 必须只锁一次。同一条语句里再锁会把这把不可重入的锁永久占住，界面线程跟着卡死。
        let (role, bind_ip) = {
            let model = lock(model);
            (
                model.role,
                transport_ip(&model.interfaces, model.prefer_wired),
            )
        };
        stream.set_nodelay(true).ok();
        stream.set_nonblocking(true).map_err(|err| err.to_string())?;
        Self::new(peer, stream, false, role, bind_ip)
    }

    fn new(
        peer: Ipv4Addr,
        stream: TcpStream,
        initiator: bool,
        role: Role,
        bind_ip: Ipv4Addr,
    ) -> Result<Self, String> {
        let udp = bind_udp(bind_ip).map_err(|err| format!("音频端口绑定失败: {err}"))?;
        let local_audio_port = if role == Role::Receiver {
            udp.local_addr().map(|addr| addr.port()).unwrap_or(0)
        } else {
            0
        };
        Ok(Self {
            stream,
            udp,
            decoder: FrameDecoder::new(),
            write_buf: Vec::new(),
            peer,
            remote_audio_port: 0,
            local_audio_port,
            initiator,
            sent_hello: false,
            announced: false,
            sent_devices: false,
            phase: Phase::WaitHello,
            last_io: Instant::now(),
            last_ping: Instant::now(),
            last_stats: Instant::now(),
        })
    }

    fn is_streaming(&self) -> bool {
        self.phase == Phase::Streaming
    }

    fn queue(&mut self, message: &ControlMessage) -> Result<(), String> {
        self.write_buf.extend(encode_frame(message)?);
        Ok(())
    }

    fn flush(&mut self) -> Result<(), String> {
        while !self.write_buf.is_empty() {
            match self.stream.write(&self.write_buf) {
                Ok(0) => return Err("对端关闭了连接".into()),
                Ok(size) => {
                    self.write_buf.drain(..size);
                }
                Err(err) if err.kind() == ErrorKind::WouldBlock => break,
                Err(err) if err.kind() == ErrorKind::Interrupted => continue,
                Err(err) => return Err(format!("发送控制消息失败: {err}")),
            }
        }
        Ok(())
    }

    #[cfg(target_os = "macos")]
    fn send_audio(&mut self, packet: &AudioPacket) {
        if self.phase != Phase::Streaming || self.remote_audio_port == 0 {
            return;
        }
        let Some(bytes) = packet.encode() else {
            return;
        };
        let _ = self
            .udp
            .send_to(&bytes, SocketAddrV4::new(self.peer, self.remote_audio_port));
    }

    fn poll(
        &mut self,
        model: &Mutex<Model>,
        engine: Option<&Arc<Mutex<PlaybackEngine>>>,
    ) -> Result<(), String> {
        if self.initiator && !self.sent_hello {
            self.queue(&hello_from(model, self.local_audio_port))?;
            self.sent_hello = true;
        }
        self.flush()?;
        self.read_control(model, engine)?;
        self.flush()?;
        if self.phase == Phase::Streaming {
            self.exchange_audio(model, engine)?;
        }
        if self.phase == Phase::Streaming && self.last_io.elapsed() > Duration::from_secs(3) {
            return Err("对端没有响应".into());
        }
        Ok(())
    }

    fn read_control(
        &mut self,
        model: &Mutex<Model>,
        engine: Option<&Arc<Mutex<PlaybackEngine>>>,
    ) -> Result<(), String> {
        let mut buffer = [0u8; 4096];
        loop {
            match self.stream.read(&mut buffer) {
                Ok(0) => return Err("对端关闭了连接".into()),
                Ok(size) => {
                    self.last_io = Instant::now();
                    let messages = self.decoder.push(&buffer[..size])?;
                    for message in messages {
                        self.handle_message(message, model, engine)?;
                    }
                }
                Err(err) if err.kind() == ErrorKind::WouldBlock => break,
                Err(err) if err.kind() == ErrorKind::Interrupted => continue,
                Err(err) => return Err(format!("读取控制消息失败: {err}")),
            }
        }
        Ok(())
    }

    fn handle_message(
        &mut self,
        message: ControlMessage,
        model: &Mutex<Model>,
        engine: Option<&Arc<Mutex<PlaybackEngine>>>,
    ) -> Result<(), String> {
        match message {
            ControlMessage::Hello {
                version,
                role,
                audio_port,
                ..
            } => {
                if version != PROTOCOL_VERSION {
                    return Err(format!("对端协议版本是 {version}，本机是 {PROTOCOL_VERSION}"));
                }
                if role == Role::Receiver {
                    self.remote_audio_port = audio_port;
                }
                if !self.sent_hello {
                    self.queue(&hello_from(model, self.local_audio_port))?;
                    self.sent_hello = true;
                }
                self.become_streaming(model)?;
            }
            ControlMessage::StartStream { target_latency_ms } => {
                let ms = normalize_latency(target_latency_ms);
                lock(model).target_latency_ms = ms;
                if let Some(engine) = engine {
                    lock(engine).set_user_target(ms);
                }
                self.become_streaming(model)?;
            }
            ControlMessage::StopStream => return Err("对端结束了播放".into()),
            ControlMessage::SetVolume { volume } => {
                lock(model).volume = volume.clamp(0.0, 1.0);
            }
            ControlMessage::SetMuteLocal { mute } => {
                if lock(model).role == Role::Sender {
                    lock(model).mute_local = mute;
                }
            }
            ControlMessage::PlaybackDevices { devices, selected } => {
                if lock(model).role == Role::Sender {
                    let mut model = lock(model);
                    model.playback_devices = devices;
                    model.selected_playback = selected as usize;
                }
            }
            ControlMessage::Stats {
                loss_pct,
                jitter_ms,
                buffer_ms,
                latency_ms,
                rtt_ms,
                target_latency_ms,
                ..
            } => {
                let mut model = lock(model);
                model.loss_pct = loss_pct;
                model.jitter_ms = jitter_ms;
                model.buffer_ms = buffer_ms;
                model.latency_ms = latency_ms;
                model.rtt_ms = rtt_ms;
                model.target_latency_ms = target_latency_ms;
            }
            ControlMessage::Ping { t_sender_ns } => {
                let t_recv_ns = mono_ns();
                let t_reply_ns = mono_ns();
                self.queue(&ControlMessage::Pong {
                    t_sender_ns,
                    t_recv_ns,
                    t_reply_ns,
                })?;
            }
            ControlMessage::Pong {
                t_sender_ns,
                t_recv_ns,
                t_reply_ns,
            } => {
                let t4 = mono_ns();
                let (offset, rtt) =
                    audiolink_protocol::ntp_offset_and_rtt(t_sender_ns, t_recv_ns, t_reply_ns, t4);
                if let Some(engine) = engine {
                    lock(engine).observe_clock(offset, rtt);
                }
                lock(model).rtt_ms = rtt as f32 / 1_000_000.0;
                self.last_io = Instant::now();
            }
            ControlMessage::SetTargetLatency {
                target_latency_ms,
                automatic,
            } => {
                let ms = normalize_latency(target_latency_ms);
                if let Some(engine) = engine {
                    if automatic {
                        lock(engine).set_user_target(ms);
                    } else {
                        lock(engine).set_user_target(ms);
                    }
                }
                let mut model = lock(model);
                model.target_latency_ms = ms;
                model.auto_note = if automatic {
                    format!("网络抖动较大，已自动放宽到 {ms}ms")
                } else {
                    String::new()
                };
            }
        }
        Ok(())
    }

    fn become_streaming(&mut self, model: &Mutex<Model>) -> Result<(), String> {
        let role = lock(model).role;
        if role == Role::Receiver && !self.sent_devices {
            let (devices, selected) = {
                let model = lock(model);
                (model.playback_devices.clone(), model.selected_playback as u32)
            };
            self.queue(&ControlMessage::PlaybackDevices { devices, selected })?;
            self.sent_devices = true;
        }
        if role == Role::Sender && self.remote_audio_port != 0 && !self.announced {
            let target = lock(model).target_latency_ms;
            self.queue(&ControlMessage::StartStream {
                target_latency_ms: target,
            })?;
            let volume = lock(model).volume;
            self.queue(&ControlMessage::SetVolume { volume })?;
            self.announced = true;
        }
        self.phase = Phase::Streaming;
        Ok(())
    }

    fn exchange_audio(
        &mut self,
        model: &Mutex<Model>,
        engine: Option<&Arc<Mutex<PlaybackEngine>>>,
    ) -> Result<(), String> {
        if lock(model).role == Role::Receiver {
            if let Some(engine) = engine {
                let mut buffer = [0u8; 2048];
                loop {
                    match self.udp.recv_from(&mut buffer) {
                        Ok((size, _)) => {
                            if let Some(packet) = AudioPacket::decode(&buffer[..size]) {
                                lock(engine).push_packet(packet, mono_ns());
                            }
                        }
                        Err(err) if err.kind() == ErrorKind::WouldBlock => break,
                        Err(err) => {
                            log::warn!("接收音频失败: {err}");
                            break;
                        }
                    }
                }
            }
            if self.last_ping.elapsed() >= Duration::from_millis(200) {
                self.queue(&ControlMessage::Ping {
                    t_sender_ns: mono_ns(),
                })?;
                self.last_ping = Instant::now();
            }
            if self.last_stats.elapsed() >= Duration::from_millis(200) {
                if let Some(engine) = engine {
                    let stats = lock(engine).stats();
                    let target = lock(model).target_latency_ms;
                    self.queue(&ControlMessage::Stats {
                        loss_pct: stats.loss_pct,
                        jitter_ms: stats.jitter_ms,
                        buffer_ms: stats.buffer_ms,
                        latency_ms: stats.latency_ms,
                        rtt_ms: stats.rtt_ms,
                        underruns: stats.underruns.min(u32::MAX as u64) as u32,
                        target_latency_ms: target,
                    })?;
                }
                self.last_stats = Instant::now();
            }
        }
        Ok(())
    }
}

fn hello_from(model: &Mutex<Model>, audio_port: u16) -> ControlMessage {
    let model = lock(model);
    ControlMessage::Hello {
        name: model.local_name.clone(),
        version: PROTOCOL_VERSION,
        role: model.role,
        sample_rate: SAMPLE_RATE,
        channels: CHANNELS,
        audio_port,
    }
}
