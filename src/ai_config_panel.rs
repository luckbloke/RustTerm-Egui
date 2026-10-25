//! 「AI 配置」面板：布局与字段对齐设计稿。
//!
//! 字段顺序：提供方 → API Key → Base URL → 模型 → aiMaxTokens →
//! aiTemperature → 只读模式 → 对话历史最多保留 → 终端上下文最近行数 →
//! 上下文单行最大字符数。

use crate::config::{AiProvider, AppConfig};

#[derive(Clone)]
pub struct AiConfigPanel {
    pub open: bool,
    /// API Key 明文显示开关
    pub show_key: bool,
}

impl Default for AiConfigPanel {
    fn default() -> Self {
        Self { open: false, show_key: false }
    }
}

impl AiConfigPanel {
    /// 渲染 AI 配置面板。
    ///
    /// 返回 `Some(消息)` = 需要在状态栏弹的 toast（保存成功 / 失败）。
    pub fn ui(&mut self, ctx: &egui::Context, config: &mut AppConfig) -> Option<String> {
        if !self.open {
            return None;
        }
        let mut win_open = self.open;
        let mut close_clicked = false;
        let mut save_clicked = false;

        egui::Window::new("🤖 AI 配置")
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .open(&mut win_open)
            .collapsible(false)
            .resizable(true)
            .default_size([380.0, 560.0])
            .show(ctx, |ui| {
                let ai = &mut config.ai;

                // ---- 提供方 ----
                ui.label("提供方");
                let prev_provider = ai.provider();
                let mut provider_changed = false;
                egui::ComboBox::from_id_salt("ai_provider_combo")
                    .selected_text(prev_provider.label())
                    .show_ui(ui, |ui| {
                        for p in AiProvider::ALL {
                            let label = p.label().to_string();
                            if ui
                                .selectable_value(
                                    &mut ai.provider,
                                    p.id().to_string(),
                                    label,
                                )
                                .changed()
                            {
                                provider_changed = true;
                            }
                        }
                    });
                if provider_changed {
                    let new_provider = ai.provider();
                    if new_provider.should_reset_url(&ai.base_url, prev_provider) {
                        ai.base_url = new_provider.default_base_url().to_string();
                    }
                    if new_provider.should_reset_model(&ai.model, prev_provider) {
                        ai.model = new_provider.default_model().to_string();
                    }
                }
                ui.add_space(2.0);

                // ---- API Key ----
                ui.label("API Key");
                let key_hint = match ai.provider() {
                    AiProvider::Ollama => "Ollama 不需要 Key",
                    _ => "sk-...",
                };
                crate::widgets::password_field(
                    ui,
                    "ai-api-key",
                    &mut ai.api_key,
                    f32::INFINITY,
                    key_hint,
                );
                if self.show_key {
                    ui.label(
                        egui::RichText::new(ai.api_key.as_str())
                            .size(11.0)
                            .color(ui.visuals().weak_text_color()),
                    );
                }

                // ---- Base URL ----
                ui.label("Base URL");
                let base_hint = ai.provider().default_base_url();
                ui.add(
                    egui::TextEdit::singleline(&mut ai.base_url)
                        .desired_width(f32::INFINITY)
                        .hint_text(base_hint),
                );

                // ---- 模型 ----
                ui.label("模型");
                let model_hint = ai.provider().default_model();
                ui.add(
                    egui::TextEdit::singleline(&mut ai.model)
                        .desired_width(f32::INFINITY)
                        .hint_text(model_hint),
                );

                ui.add_space(4.0);
                ui.separator();
                ui.add_space(4.0);

                // ---- aiMaxTokens ----
                ui.label("aiMaxTokens");
                let mut max_tokens = ai.max_tokens as i64;
                if ui
                    .add(egui::Slider::new(&mut max_tokens, 256..=16384).suffix(" tok"))
                    .changed()
                {
                    ai.max_tokens = max_tokens.clamp(1, 1_000_000) as u32;
                }

                // ---- aiTemperature ----
                ui.label("aiTemperature");
                let mut temperature = ai.temperature;
                if ui
                    .add(egui::Slider::new(&mut temperature, 0.0..=2.0).step_by(0.05))
                    .changed()
                {
                    ai.temperature = temperature;
                }

                ui.add_space(4.0);
                ui.separator();
                ui.add_space(4.0);

                // ---- 只读模式 ----
                ui.checkbox(&mut ai.read_only, "只读模式（禁用写命令）");

                // ---- 对话历史最多保留（条）----
                ui.label("对话历史最多保留（条）");
                let mut max_history = ai.max_history as i64;
                if ui
                    .add(egui::Slider::new(&mut max_history, 2..=100).suffix(" 条"))
                    .changed()
                {
                    ai.max_history = max_history.clamp(1, 1000) as usize;
                }

                // ---- 终端上下文最近行数 ----
                ui.label("终端上下文最近行数");
                let mut context_lines = ai.context_lines as i64;
                if ui
                    .add(egui::Slider::new(&mut context_lines, 5..=500).suffix(" 行"))
                    .changed()
                {
                    ai.context_lines = context_lines.clamp(0, 5000) as usize;
                }

                // ---- 上下文单行最大字符数 ----
                ui.label("上下文单行最大字符数");
                let mut context_max_line_len = ai.context_max_line_len as i64;
                if ui
                    .add(
                        egui::Slider::new(&mut context_max_line_len, 50..=2000).suffix(" 字符"),
                    )
                    .changed()
                {
                    ai.context_max_line_len = context_max_line_len.clamp(1, 100_000) as usize;
                }

                ui.add_space(8.0);

                // ---- 底部提示 ----
                ui.label(
                    egui::RichText::new("API Key 仅保存在本地。Ollama 不需要 Key。")
                        .size(11.0)
                        .color(ui.visuals().weak_text_color()),
                );

                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if ui.button("保存").clicked() {
                        save_clicked = true;
                    }
                    if ui.button("取消").clicked() {
                        close_clicked = true;
                    }
                });
            });

        if !win_open || close_clicked {
            self.open = false;
        }
        if save_clicked {
            self.open = false;
            return Some(match config.save() {
                Ok(_) => "AI 配置已保存".to_string(),
                Err(e) => e,
            });
        }
        None
    }
}
