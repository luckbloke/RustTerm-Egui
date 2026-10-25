//! 系统字体注入：等宽主字体（Consolas/Cascadia）+ CJK 兜底（微软雅黑等）。
//!
//! egui 默认字体不含 CJK，必须在启动时把系统中文字体追加到
//! Proportional 与 Monospace 两个 family 的 fallback 末尾。

use eframe::egui;

pub fn setup_fonts(ctx: &egui::Context) {
    const MONO_CANDIDATES: [&str; 3] = [
        r"C:\Windows\Fonts\consola.ttf",
        r"C:\Windows\Fonts\CascadiaCode.ttf",
        r"C:\Windows\Fonts\CascadiaMono.ttf",
    ];
    const CJK_CANDIDATES: [&str; 4] = [
        r"C:\Windows\Fonts\msyh.ttc",
        r"C:\Windows\Fonts\msyh.ttf",
        r"C:\Windows\Fonts\simsun.ttc",
        // Linux 兜底（跨平台运行时可命中）
        "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf",
    ];

    let mut fonts = egui::FontDefinitions::default();

    for (i, path) in MONO_CANDIDATES.into_iter().enumerate() {
        if let Ok(bytes) = std::fs::read(path) {
            let name = format!("mono-{i}");
            fonts
                .font_data
                .insert(name.clone(), egui::FontData::from_owned(bytes).into());
            if let Some(family) = fonts.families.get_mut(&egui::FontFamily::Monospace) {
                family.insert(0, name); // 主字体放最前，CJK 兜底在后
            }
            eprintln!("[fonts] 等宽主字体: {path}");
            break;
        }
    }

    for (i, path) in CJK_CANDIDATES.into_iter().enumerate() {
        if let Ok(bytes) = std::fs::read(path) {
            let name = format!("cjk-{i}");
            fonts
                .font_data
                .insert(name.clone(), egui::FontData::from_owned(bytes).into());
            for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
                if let Some(list) = fonts.families.get_mut(&family) {
                    list.push(name.clone());
                }
            }
            eprintln!("[fonts] CJK 兜底字体: {path}");
            break;
        }
    }

    ctx.set_fonts(fonts);
}
