mod config;
mod model;
mod runtime;
mod selftest;
mod ui;

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use model::Command;
use runtime::AppRuntime;
use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};

fn main() -> eframe::Result<()> {
    if std::env::args().any(|arg| arg == "--self-test") {
        let code = selftest::run();
        std::process::exit(code);
    }
    if std::env::var_os("RUST_LOG").is_none() {
        std::env::set_var("RUST_LOG", "info");
    }
    env_logger::init();

    let runtime = AppRuntime::start();
    let tray = build_tray(false);
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([560.0, 780.0])
            .with_min_inner_size([480.0, 640.0])
            .with_title("音频互联"),
        ..Default::default()
    };
    eframe::run_native(
        "音频互联",
        options,
        Box::new(move |cc| {
            ui::install_fonts(&cc.egui_ctx);
            cc.egui_ctx.set_visuals(egui::Visuals::dark());
            Ok(Box::new(DesktopApp {
                runtime,
                tray,
                selected: None,
                allow_close: false,
                tray_active: false,
                saved_volume: 1.0,
                muted: AtomicBool::new(false),
            }))
        }),
    )
}

struct DesktopApp {
    runtime: AppRuntime,
    tray: TrayIcon,
    selected: Option<usize>,
    allow_close: bool,
    tray_active: bool,
    saved_volume: f32,
    muted: AtomicBool,
}

impl eframe::App for DesktopApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.handle_tray(ctx);
        if ctx.input(|input| input.viewport().close_requested()) && !self.allow_close {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
        }
        let snapshot = self.runtime.snapshot();
        let active = snapshot.connected || snapshot.local_monitor;
        if active != self.tray_active {
            self.tray_active = active;
            let _ = self.tray.set_icon(Some(tray_icon(active)));
        }
        let _ = self.tray.set_tooltip(Some(tray_tooltip(&snapshot)));
        egui::CentralPanel::default().show(ctx, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                ui::draw(ui, &snapshot, &mut self.selected, &self.runtime);
            });
        });
        ctx.request_repaint_after(Duration::from_millis(200));
    }
}

impl DesktopApp {
    fn handle_tray(&mut self, ctx: &egui::Context) {
        while let Ok(event) = MenuEvent::receiver().try_recv() {
            match event.id().0.as_str() {
                "show" => {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
                    ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
                }
                "mute" => {
                    let snapshot = self.runtime.snapshot();
                    if self.muted.load(Ordering::Relaxed) {
                        self.runtime.send(Command::SetVolume(self.saved_volume));
                        self.muted.store(false, Ordering::Relaxed);
                    } else {
                        self.saved_volume = snapshot.volume.max(0.05);
                        self.runtime.send(Command::SetVolume(0.0));
                        self.muted.store(true, Ordering::Relaxed);
                    }
                }
                "quit" => {
                    self.allow_close = true;
                    self.runtime.send(Command::Shutdown);
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
                _ => {}
            }
        }
    }
}

fn build_tray(active: bool) -> TrayIcon {
    let menu = Menu::new();
    let show = MenuItem::with_id("show", "显示窗口", true, None);
    let mute = MenuItem::with_id("mute", "静音", true, None);
    let quit = MenuItem::with_id("quit", "退出", true, None);
    let _ = menu.append(&show);
    let _ = menu.append(&mute);
    let _ = menu.append(&PredefinedMenuItem::separator());
    let _ = menu.append(&quit);
    TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_tooltip("音频互联：未连接")
        .with_icon(tray_icon(active))
        .build()
        .expect("创建菜单栏图标失败")
}

fn tray_icon(active: bool) -> Icon {
    let color = if active {
        [46, 168, 96, 255]
    } else {
        [96, 104, 116, 255]
    };
    let mut rgba = vec![0u8; 32 * 32 * 4];
    for y in 0..32 {
        for x in 0..32 {
            let dx = x as i32 - 15;
            let dy = y as i32 - 15;
            if dx * dx + dy * dy <= 13 * 13 {
                let index = (y * 32 + x) * 4;
                rgba[index..index + 4].copy_from_slice(&color);
            }
        }
    }
    Icon::from_rgba(rgba, 32, 32).expect("图标尺寸不正确")
}

fn tray_tooltip(snapshot: &model::Snapshot) -> String {
    if snapshot.connected {
        "音频互联：正在播放".into()
    } else if snapshot.local_monitor {
        "音频互联：本机试听".into()
    } else if snapshot.peers.iter().any(|peer| peer.online) {
        "音频互联：已发现设备".into()
    } else {
        "音频互联：未连接".into()
    }
}
