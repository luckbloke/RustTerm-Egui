//! 通用 UI 小组件。
//!
//! 目前只有密码输入框：右侧一个眼睛按钮，点击在「密文 / 明文」间切换。
//! 显示状态保存在 egui Memory 里（按 id 区分多个输入框，各自的切换互不影响）。

use eframe::egui;

/// 带「眼睛」图标的密码输入框。
///
/// - `key`：区分页面上多个密码框的唯一 id（同一版面用不同字符串）。
/// - 返回输入框自身的 `Response`（调用方可用 `lost_focus` 等做提交逻辑）。
pub fn password_field(
    ui: &mut egui::Ui,
    key: &str,
    value: &mut String,
    desired_width: f32,
    hint_text: impl Into<String>,
) -> egui::Response {
    let toggle_id = egui::Id::new(("pwd-eye", key));
    let show = ui.memory(|m| m.data.get_temp::<bool>(toggle_id).unwrap_or(false));
    let hint: String = hint_text.into();
    // 占满整行的调用（f32::INFINITY）改为「剩余宽度减去眼睛按钮」，
    // 否则按钮会被挤出屏幕
    let width = if desired_width.is_infinite() {
        (ui.available_width() - 30.0).max(40.0)
    } else {
        desired_width
    };

    ui.horizontal(|ui| {
        let resp = ui.add(
            egui::TextEdit::singleline(value)
                .desired_width(width)
                .password(!show)
                .hint_text(hint),
        );
        let eye = if show { "👁" } else { "🙈" };
        if ui
            .add(
                egui::Button::new(egui::RichText::new(eye).size(13.0)).frame(false),
            )
            .on_hover_text(if show { "隐藏密码" } else { "显示密码" })
            .clicked()
        {
            ui.memory_mut(|m| m.data.insert_temp(toggle_id, !show));
        }
        resp
    })
    .inner
}
