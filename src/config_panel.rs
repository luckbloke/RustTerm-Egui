use crate::config::AppConfig;
use crate::i18n::I18n;

#[derive(Clone)]
pub struct ConfigPanel {
    pub open: bool,
}

impl Default for ConfigPanel {
    fn default() -> Self {
        Self { open: false }
    }
}

impl ConfigPanel {
    /// 渲染配置面板；保存失败时返回错误文案（调用方弹 toast）。
    pub fn ui(
        &mut self,
        ui: &mut egui::Ui,
        config: &mut AppConfig,
        i18n: &mut I18n,
    ) -> Option<String> {
        if !self.open {
            return None;
        }
        let mut win_open = self.open;
        let mut close_clicked = false;
        let mut open_ai = false;
        let mut save_err: Option<String> = None;
        egui::Window::new("配置面板")
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .open(&mut win_open) // 标题栏 × 关闭按钮
            .default_size([450.0, 420.0])
            .resizable(true)
            .show(ui.ctx(), |ui| {
                ui.style_mut().spacing.item_spacing = egui::vec2(8.0, 6.0);

                // 使用 CollapsingHeader 替代 Tabs
                egui::CollapsingHeader::new("🔧 常规设置")
                    .default_open(true)
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label("字体大小");
                            ui.add(egui::Slider::new(&mut config.font_size, 10.0..=24.0).text("pt"));
                        });
                        ui.horizontal(|ui| {
                            ui.label("字体族");
                            ui.text_edit_singleline(&mut config.font_family);
                        });
                        ui.horizontal(|ui| {
                            ui.label("语言");
                            egui::ComboBox::from_label("")
                                .selected_text(&config.language)
                                .show_ui(ui, |ui| {
                                    if ui.selectable_value(&mut config.language, "zh-CN".to_string(), "简体中文").clicked() {
                                        i18n.set_lang("zh-CN");
                                    }
                                    if ui.selectable_value(&mut config.language, "en-US".to_string(), "English").clicked() {
                                        i18n.set_lang("en-US");
                                    }
                                });
                        });
                        ui.checkbox(&mut config.show_status_bar, "显示底部状态栏");
                        ui.checkbox(&mut config.enable_key_macros, "启用键宏功能");
                    });

                egui::CollapsingHeader::new("🔗 连接设置")
                    .default_open(false)
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label("Keep-Alive 间隔");
                            ui.add(egui::Slider::new(&mut config.keep_alive_interval, 10..=300).text("秒"));
                        });
                        ui.horizontal(|ui| {
                            ui.label("默认主题");
                            egui::ComboBox::from_label("")
                                .selected_text(&config.theme)
                                .show_ui(ui, |ui| {
                                    if ui.selectable_value(&mut config.theme, "dark".to_string(), "深色").clicked() {
                                        ui.ctx().set_visuals(egui::Visuals::dark());
                                    }
                                    if ui.selectable_value(&mut config.theme, "light".to_string(), "浅色").clicked() {
                                        ui.ctx().set_visuals(egui::Visuals::light());
                                    }
                                    ui.selectable_value(&mut config.theme, "auto".to_string(), "跟随系统");
                                });
                        });
                    });

                egui::CollapsingHeader::new("🤖 AI 设置")
                    .default_open(false)
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label("提供方");
                            egui::ComboBox::from_id_salt("cfg_ai_provider")
                                .selected_text(config.ai.provider().label())
                                .show_ui(ui, |ui| {
                                    for p in crate::config::AiProvider::ALL {
                                        ui.selectable_value(
                                            &mut config.ai.provider,
                                            p.id().to_string(),
                                            p.label(),
                                        );
                                    }
                                });
                        });
                        ui.horizontal(|ui| {
                            ui.label("模型");
                            ui.text_edit_singleline(&mut config.ai.model);
                        });
                        ui.checkbox(&mut config.ai.read_only, "只读模式（禁用写命令）");
                        if ui.button("打开 AI 配置面板…").clicked() {
                            open_ai = true;
                        }
                    });

                egui::CollapsingHeader::new("ℹ️ 关于")
                    .default_open(false)
                    .show(ui, |ui| {
                        ui.label("RustTerm Egui P4");
                        ui.label("全量纯 Rust 终端客户端");
                        ui.add_space(8.0);
                        ui.label("版本: 0.1.0");
                        ui.label("架构: egui + alacritty_terminal + russh");
                    });

                ui.separator();
                ui.horizontal(|ui| {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("关闭").clicked() {
                            close_clicked = true;
                        }
                        if ui.button("保存并应用").clicked() {
                            if let Err(e) = config.save() {
                                ui.colored_label(egui::Color32::from_rgb(234, 78, 102), &e);
                                save_err = Some(e);
                            }
                        }
                    });
                });
            });
        if !win_open || close_clicked {
            self.open = false;
        }
        if open_ai {
            if let Err(e) = config.save() {
                save_err = Some(e);
            }
        }
        save_err
    }
}
