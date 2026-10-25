//! RustTerm Egui - 全功能终端客户端
//!
//! P1: 产品骨架（统一标签页/SFTP双栏/状态栏/配置面板/i18n）
//! P2: 远程桌面（VNC/RDP/SPICE 帧纹理帧渲染，后端零改动）
//! P3: 兼容层（wry浏览器标签 + AI 助手多提供方 + AI 截屏分析面板）
//! P4: 打磨（键宏、keep-alive、装配打包、性能与内存调优）

// Windows 下不挂载控制台：纯 GUI 程序，双击/启动时不再弹出宿主控制台窗口
#![cfg_attr(windows, windows_subsystem = "windows")]

mod ai;
mod ai_assistant;
mod ai_config_panel;
mod app;
mod backends;
mod browser;
mod config;
mod config_panel;
mod filebrowser;
mod fonts;
mod i18n;
mod macros;
mod rdp;
mod remote;
mod screenshot;
mod sessions;
mod sftp;
mod sftp_client;
mod tabs;
mod term;
mod tunnel;
mod widgets;

use eframe::egui;

/// 计算初始窗口尺寸与位置，保证整个窗口（含底部状态栏）落在屏幕工作区内。
///
/// 直接把 inner_size 固定成 1500×950 在 1080p 屏幕上会超出工作区
/// （任务栏占掉约 40px），窗口底部状态栏会被挡在屏幕外。
#[cfg(windows)]
fn initial_geometry() -> (egui::Vec2, Option<egui::Pos2>) {
    use windows_sys::Win32::UI::WindowsAndMessaging::SystemParametersInfoW;

    #[repr(C)]
    struct WRect {
        left: i32,
        top: i32,
        right: i32,
        bottom: i32,
    }

    let mut r = WRect { left: 0, top: 0, right: 0, bottom: 0 };
    let ok = unsafe {
        SystemParametersInfoW(
            0x0030, // SPI_GETWORKAREA
            0,
            &mut r as *mut _ as *mut core::ffi::c_void,
            0,
        )
    };
    if ok == 0 {
        return (egui::vec2(1400.0, 860.0), None);
    }

    let scale = unsafe { windows_sys::Win32::UI::HiDpi::GetDpiForSystem() } as f32 / 96.0;
    let scale = if scale > 0.0 { scale } else { 1.0 };

    // 工作区（物理像素）→ 逻辑点
    let wl = r.left as f32 / scale;
    let wt = r.top as f32 / scale;
    let ww = (r.right - r.left) as f32 / scale;
    let wh = (r.bottom - r.top) as f32 / scale;

    // 留出边框/标题栏余量，最多 1500×940
    let iw = (ww - 24.0).min(1500.0).max(900.0);
    let ih = (wh - 70.0).min(940.0).max(560.0);
    let x = wl + (ww - iw) * 0.5;
    let y = wt + (wh - ih) * 0.5;
    (egui::vec2(iw, ih), Some(egui::pos2(x, y)))
}

/// 加载应用图标（RGBA），用于窗口标题栏 / 任务栏。
///
/// build.rs 已把 icon.ico 嵌进 PE 资源（资源管理器里的 exe 图标），
/// 运行时窗口图标需要在这里用 `ViewportBuilder::with_icon` 另行设置。
fn load_window_icon() -> egui::IconData {
    let png = include_bytes!("../icons/128x128.png");
    let img = image::load_from_memory(png)
        .expect("解析应用图标失败")
        .into_rgba8();
    let (w, h) = (img.width(), img.height());
    egui::IconData {
        width: w,
        height: h,
        rgba: img.into_raw(),
    }
}

fn main() -> Result<(), eframe::Error> {
    env_logger::init();

    #[cfg(windows)]
    let (size, pos) = initial_geometry();
    #[cfg(not(windows))]
    let (size, pos) = (egui::vec2(1400.0, 880.0), None::<egui::Pos2>);

    let mut viewport = egui::ViewportBuilder::default()
        .with_title("RustTerm Egui")
        .with_inner_size(size)
        .with_min_inner_size([900.0, 560.0])
        .with_icon(load_window_icon());
    if let Some(p) = pos {
        viewport = viewport.with_position(p);
    }

    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };

    eframe::run_native(
        "RustTerm Egui",
        options,
        Box::new(|cc| {
            fonts::setup_fonts(&cc.egui_ctx);
            Ok(Box::new(app::RustTermApp::new()))
        }),
    )
}
