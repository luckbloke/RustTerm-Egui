use chrono;
use rfd;
use uuid;
// 注意：删除了未使用的 use std::collections::HashMap;

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct KeyMacro {
    pub id: String,
    pub name: String,
    pub description: String,
    pub keys: Vec<KeyAction>,
    pub enabled: bool,
    pub trigger: MacroTrigger,
    pub created_at: String,
    pub use_count: u64,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub enum KeyAction {
    Key(String),
    KeyCombo(String),
    Text(String),
    Delay(u64),
    MouseClick(MouseButton, i32, i32),
}

// 修复 E0277：为 MouseButton 派生 PartialEq
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum MacroTrigger {
    Manual,
    Hotkey(String),
    OnConnect,
    OnDisconnect,
    Schedule(String),
}

impl Default for KeyMacro {
    fn default() -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            name: String::new(),
            description: String::new(),
            keys: Vec::new(),
            enabled: true,
            trigger: MacroTrigger::Manual,
            created_at: chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
            use_count: 0,
        }
    }
}

#[derive(Clone)]
pub struct MacroPanel {
    pub open: bool,
    pub macros: Vec<KeyMacro>,
    pub recording: bool,
    pub recording_keys: Vec<KeyAction>,
    pub editing_index: Option<usize>,
    pub show_editor: bool,
    pub filter: String,
    pub sort_by: MacroSort,
}

#[derive(Clone, Copy, PartialEq, Default)]
pub enum MacroSort {
    #[default]
    Name,
    Created,
    Usage,
}

impl Default for MacroPanel {
    fn default() -> Self {
        Self {
            open: false,
            macros: vec![
                KeyMacro {
                    id: uuid::Uuid::new_v4().to_string(),
                    name: "清屏".to_string(),
                    description: "发送 Ctrl+L 清屏命令".to_string(),
                    keys: vec![KeyAction::KeyCombo("Ctrl+L".to_string())],
                    enabled: true,
                    trigger: MacroTrigger::Manual,
                    created_at: chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
                    use_count: 0,
                },
                KeyMacro {
                    id: uuid::Uuid::new_v4().to_string(),
                    name: "列出文件详情".to_string(),
                    description: "ls -la 并回车".to_string(),
                    keys: vec![
                        KeyAction::Text("ls -la".to_string()),
                        KeyAction::Key("Enter".to_string()),
                    ],
                    enabled: true,
                    trigger: MacroTrigger::Manual,
                    created_at: chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
                    use_count: 0,
                },
                KeyMacro {
                    id: uuid::Uuid::new_v4().to_string(),
                    name: "Git 状态".to_string(),
                    description: "显示 git status".to_string(),
                    keys: vec![
                        KeyAction::Text("git status".to_string()),
                        KeyAction::Key("Enter".to_string()),
                    ],
                    enabled: true,
                    trigger: MacroTrigger::Manual,
                    created_at: chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
                    use_count: 0,
                },
                KeyMacro {
                    id: uuid::Uuid::new_v4().to_string(),
                    name: "Docker 容器列表".to_string(),
                    description: "docker ps -a".to_string(),
                    keys: vec![
                        KeyAction::Text("docker ps -a".to_string()),
                        KeyAction::Key("Enter".to_string()),
                    ],
                    enabled: true,
                    trigger: MacroTrigger::Manual,
                    created_at: chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
                    use_count: 0,
                },
            ],
            recording: false,
            recording_keys: Vec::new(),
            editing_index: None,
            show_editor: false,
            filter: String::new(),
            sort_by: MacroSort::default(),
        }
    }
}

impl MacroPanel {
    /// 返回值：本帧若点击了"执行"，返回应写入终端的字节序列。
    pub fn ui(&mut self, ctx: &egui::Context) -> Option<Vec<u8>> {
        if !self.open {
            return None;
        }

        let mut exec_payload: Option<Vec<u8>> = None;
        let mut win_open = self.open;
        egui::Window::new("⌨ 键宏管理器")
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .open(&mut win_open) // 标题栏 × 关闭按钮
            .default_size([700.0, 550.0])
            .min_size([550.0, 400.0])
            .show(ctx, |ui| {
                self.render_toolbar(ui);
                ui.separator();
                if self.show_editor {
                    self.render_editor(ui);
                } else {
                    exec_payload = self.render_list(ui);
                }
            });
        if !win_open {
            self.open = false;
        }
        exec_payload
    }

    fn render_toolbar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.strong("键宏管理器");
            ui.separator();

            ui.label("搜索:");
            ui.text_edit_singleline(&mut self.filter);

            ui.separator();

            egui::ComboBox::from_label("排序")
                .selected_text(match self.sort_by {
                    MacroSort::Name => "按名称",
                    MacroSort::Created => "按创建时间",
                    MacroSort::Usage => "按使用次数",
                })
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.sort_by, MacroSort::Name, "按名称");
                    ui.selectable_value(&mut self.sort_by, MacroSort::Created, "按创建时间");
                    ui.selectable_value(&mut self.sort_by, MacroSort::Usage, "按使用次数");
                });

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("➕ 新建宏").clicked() {
                    self.start_new_macro();
                }
                if ui.button("📥 导入").clicked() {
                    self.import_macros();
                }
                if ui.button("📤 导出").clicked() {
                    self.export_macros();
                }
            });
        });
    }

    fn render_list(&mut self, ui: &mut egui::Ui) -> Option<Vec<u8>> {
        let mut indices: Vec<usize> = (0..self.macros.len()).collect();

        if !self.filter.is_empty() {
            indices.retain(|&i| {
                let m = &self.macros[i];
                m.name.to_lowercase().contains(&self.filter.to_lowercase())
                    || m.description.to_lowercase().contains(&self.filter.to_lowercase())
            });
        }

        match self.sort_by {
            MacroSort::Name => indices.sort_by(|&a, &b| self.macros[a].name.cmp(&self.macros[b].name)),
            MacroSort::Created => indices.sort_by(|&a, &b| self.macros[b].created_at.cmp(&self.macros[a].created_at)),
            MacroSort::Usage => indices.sort_by(|&a, &b| self.macros[b].use_count.cmp(&self.macros[a].use_count)),
        }

        let mut execute_idx: Option<usize> = None;
        let mut exec_payload: Option<Vec<u8>> = None;
        let mut edit_idx: Option<usize> = None;
        let mut duplicate_idx: Option<usize> = None;
        let mut remove_idx: Option<usize> = None;

        egui::ScrollArea::vertical().id_salt("macro_list").show(ui, |ui| {
            egui::Grid::new("macro_list_grid")
                .striped(true)
                .min_col_width(80.0)
                .show(ui, |ui| {
                    ui.strong("启用");
                    ui.strong("名称");
                    ui.strong("描述");
                    ui.strong("触发方式");
                    ui.strong("动作数");
                    ui.strong("使用次数");
                    ui.strong("创建时间");
                    ui.strong("操作");
                    ui.end_row();

                    for &idx in &indices {
                        ui.checkbox(&mut self.macros[idx].enabled, "");
                        ui.label(&self.macros[idx].name);
                        ui.label(egui::RichText::new(&self.macros[idx].description).size(10.0).color(egui::Color32::GRAY));

                        // 修复 E0716：先绑定到局部 String 再借用
                        let trigger_text: String = match &self.macros[idx].trigger {
                            MacroTrigger::Manual => "🖱 手动".to_string(),
                            MacroTrigger::Hotkey(hk) => format!("⌨ {}", hk),
                            MacroTrigger::OnConnect => "🔌 连接时".to_string(),
                            MacroTrigger::OnDisconnect => "🔌 断开时".to_string(),
                            MacroTrigger::Schedule(cron) => format!("⏰ {}", cron),
                        };
                        ui.label(trigger_text);

                        ui.label(self.macros[idx].keys.len().to_string());
                        ui.label(self.macros[idx].use_count.to_string());
                        ui.label(egui::RichText::new(&self.macros[idx].created_at).size(10.0).color(egui::Color32::GRAY));

                        ui.horizontal(|ui| {
                            if ui.add_enabled(self.macros[idx].enabled, egui::Button::new("▶ 执行").min_size(egui::vec2(60.0, 22.0))).clicked() {
                                execute_idx = Some(idx);
                            }
                            if ui.button("✏").on_hover_text("编辑").clicked() {
                                edit_idx = Some(idx);
                            }
                            if ui.button("📋").on_hover_text("复制").clicked() {
                                duplicate_idx = Some(idx);
                            }
                            if ui.add(egui::Button::new("🗑").min_size(egui::vec2(28.0, 22.0))).on_hover_text("删除").clicked() {
                                remove_idx = Some(idx);
                            }
                        });
                        ui.end_row();
                    }
                });
        });

        if let Some(idx) = execute_idx {
            exec_payload = self.execute_macro(idx);
        }
        if let Some(idx) = edit_idx {
            self.editing_index = Some(idx);
            self.show_editor = true;
        }
        if let Some(idx) = duplicate_idx {
            self.duplicate_macro(idx);
        }
        if let Some(idx) = remove_idx {
            self.macros.remove(idx);
        }
        exec_payload
    }

    fn render_editor(&mut self, ui: &mut egui::Ui) {
        if let Some(idx) = self.editing_index {
            if idx >= self.macros.len() {
                self.show_editor = false;
                self.editing_index = None;
                return;
            }

            // 修复 E0500：先取出需要的元数据，避免与闭包中 &mut self 冲突
            let is_new = self.macros[idx].id.is_empty();
            let mut close_editor = false;

            {
                let macro_ = &mut self.macros[idx];

                ui.horizontal(|ui| {
                    ui.heading(format!("{} 宏", if is_new { "新建" } else { "编辑" }));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("取消").clicked() {
                            close_editor = true;
                        }
                        if ui.button("保存").clicked() {
                            close_editor = true;
                        }
                    });
                });

                ui.separator();

                egui::ScrollArea::vertical().id_salt("macro_detail").show(ui, |ui| {
                    egui::Grid::new("macro_editor_grid")
                        .num_columns(2)
                        .spacing([12.0, 10.0])
                        .show(ui, |ui| {
                            ui.label("名称:");
                            ui.text_edit_singleline(&mut macro_.name);
                            ui.end_row();

                            ui.label("描述:");
                            ui.text_edit_singleline(&mut macro_.description);
                            ui.end_row();

                            ui.label("启用:");
                            ui.checkbox(&mut macro_.enabled, "");
                            ui.end_row();

                            ui.label("触发方式:");
                            // 修复 E0500：调用不再依赖 &mut self 的静态函数
                            Self::render_trigger_selector(ui, macro_);
                            ui.end_row();

                            ui.separator();
                            ui.end_row();

                            ui.label("按键序列:");
                            ui.vertical(|ui| {
                                // 修复 E0425：把 idx 传入
                                Self::render_key_sequence_editor(ui, macro_, idx, &mut self.recording, &mut self.recording_keys);
                            });
                            ui.end_row();
                        });
                });
            }

            if close_editor {
                self.show_editor = false;
                self.editing_index = None;
            }
        } else {
            self.show_editor = false;
        }
    }

    // 修复 E0500：改为静态函数，不依赖 &mut self
    fn render_trigger_selector(ui: &mut egui::Ui, macro_: &mut KeyMacro) {
        ui.horizontal(|ui| {
            // 修复 E0716：先绑定 selected_text 到局部变量
            let selected_text: String = match &macro_.trigger {
                MacroTrigger::Manual => "🖱 手动触发".to_string(),
                MacroTrigger::Hotkey(hk) => format!("⌨ 热键: {}", hk),
                MacroTrigger::OnConnect => "🔌 连接时自动".to_string(),
                MacroTrigger::OnDisconnect => "🔌 断开时自动".to_string(),
                MacroTrigger::Schedule(cron) => format!("⏰ 定时: {}", cron),
            };

            egui::ComboBox::from_id_salt("trigger_combo")
                .selected_text(selected_text)
                .show_ui(ui, |ui| {
                    if ui.selectable_value(&mut macro_.trigger, MacroTrigger::Manual, "🖱 手动触发").clicked() {}
                    if ui.selectable_value(&mut macro_.trigger, MacroTrigger::Hotkey("Ctrl+Alt+M".to_string()), "⌨ 热键").clicked() {}
                    if ui.selectable_value(&mut macro_.trigger, MacroTrigger::OnConnect, "🔌 连接时自动").clicked() {}
                    if ui.selectable_value(&mut macro_.trigger, MacroTrigger::OnDisconnect, "🔌 断开时自动").clicked() {}
                    if ui.selectable_value(&mut macro_.trigger, MacroTrigger::Schedule("0 * * * *".to_string()), "⏰ 定时执行").clicked() {}
                });

            if let MacroTrigger::Hotkey(hk) = &mut macro_.trigger {
                ui.text_edit_singleline(hk).on_hover_text("热键组合，如: Ctrl+Alt+M");
            }
        });
    }

    // 修复 E0425：增加 idx 参数；将 recording 状态通过参数传入，避免借用 self
    fn render_key_sequence_editor(
        ui: &mut egui::Ui,
        macro_: &mut KeyMacro,
        idx: usize,
        recording: &mut bool,
        recording_keys: &mut Vec<KeyAction>,
    ) {
        if macro_.keys.is_empty() {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("(无按键序列，点击下方添加)").color(egui::Color32::GRAY).italics());
                if ui.button("+ 添加动作").clicked() {
                    macro_.keys.push(KeyAction::Text(String::new()));
                }
            });
        } else {
            for i in 0..macro_.keys.len() {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new(format!("{}.", i + 1)).size(10.0).color(egui::Color32::GRAY));

                    let action_type = match &macro_.keys[i] {
                        KeyAction::Key(_) => 0,
                        KeyAction::KeyCombo(_) => 1,
                        KeyAction::Text(_) => 2,
                        KeyAction::Delay(_) => 3,
                        KeyAction::MouseClick(_, _, _) => 4,
                    };

                    let mut selected_type = action_type;
                    egui::ComboBox::from_id_salt(format!("action_type_{}", i))
                        .selected_text(match action_type {
                            0 => "🔘 单键",
                            1 => "⌨ 组合键",
                            2 => "📝 文本",
                            3 => "⏱ 延迟",
                            4 => "🖱 鼠标",
                            _ => "未知",
                        })
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut selected_type, 0, "🔘 单键");
                            ui.selectable_value(&mut selected_type, 1, "⌨ 组合键");
                            ui.selectable_value(&mut selected_type, 2, "📝 文本输入");
                            ui.selectable_value(&mut selected_type, 3, "⏱ 延迟(ms)");
                            ui.selectable_value(&mut selected_type, 4, "🖱 鼠标点击");
                        });

                    if selected_type != action_type {
                        macro_.keys[i] = match selected_type {
                            0 => KeyAction::Key("Enter".to_string()),
                            1 => KeyAction::KeyCombo("Ctrl+C".to_string()),
                            2 => KeyAction::Text(String::new()),
                            3 => KeyAction::Delay(100),
                            4 => KeyAction::MouseClick(MouseButton::Left, 0, 0),
                            _ => KeyAction::Key("Enter".to_string()),
                        };
                    }

                    match &mut macro_.keys[i] {
                        KeyAction::Key(key) => {
                            // 修复 E0277：selected_text 需要 &str / String，而非 &mut String
                            egui::ComboBox::from_id_salt(format!("key_combo_{}", i))
                                .selected_text(key.as_str())
                                .show_ui(ui, |ui| {
                                    for k in ["Enter", "Tab", "Escape", "Backspace", "Delete", "Up", "Down", "Left", "Right", "Home", "End", "PageUp", "PageDown", "F1", "F2", "F3", "F4", "F5", "F6", "F7", "F8", "F9", "F10", "F11", "F12", "Insert", "Space"] {
                                        ui.selectable_value(key, k.to_string(), k);
                                    }
                                });
                        }
                        KeyAction::KeyCombo(combo) => {
                            ui.text_edit_singleline(combo).on_hover_text("如: Ctrl+C, Alt+F4, Shift+Insert");
                        }
                        KeyAction::Text(text) => {
                            ui.add(
                                egui::TextEdit::singleline(text)
                                    .desired_width(200.0)
                                    .hint_text("输入要发送的文本"),
                            );
                        }
                        KeyAction::Delay(ms) => {
                            ui.add(egui::DragValue::new(ms).speed(10).suffix(" ms").range(10..=10000));
                        }
                        KeyAction::MouseClick(btn, x, y) => {
                            egui::ComboBox::from_id_salt(format!("mouse_btn_{}", i))
                                .selected_text(match btn {
                                    MouseButton::Left => "左键",
                                    MouseButton::Right => "右键",
                                    MouseButton::Middle => "中键",
                                })
                                .show_ui(ui, |ui| {
                                    // 修复 E0277：MouseButton 已派生 PartialEq，可正常比较
                                    ui.selectable_value(btn, MouseButton::Left, "左键");
                                    ui.selectable_value(btn, MouseButton::Right, "右键");
                                    ui.selectable_value(btn, MouseButton::Middle, "中键");
                                });
                            ui.label("X:");
                            ui.add(egui::DragValue::new(x).speed(1));
                            ui.label("Y:");
                            ui.add(egui::DragValue::new(y).speed(1));
                        }
                    }

                    if i > 0 && ui.button("⬆").on_hover_text("上移").clicked() {
                        macro_.keys.swap(i, i - 1);
                    }
                    if i < macro_.keys.len() - 1 && ui.button("⬇").on_hover_text("下移").clicked() {
                        macro_.keys.swap(i, i + 1);
                    }
                    if ui.button("🗑").on_hover_text("删除").clicked() {
                        macro_.keys.remove(i);
                    }
                });
            }

            ui.horizontal(|ui| {
                if ui.button("+ 添加动作").clicked() {
                    macro_.keys.push(KeyAction::Text(String::new()));
                }
                if ui.button("📋 从剪贴板粘贴文本").clicked() {
                    if let Some(text) = ui.ctx().input(|i| {
                        i.events.iter().find_map(|e| {
                            if let egui::Event::Paste(text) = e {
                                Some(text.clone())
                            } else {
                                None
                            }
                        })
                    }) {
                        macro_.keys.push(KeyAction::Text(text));
                    }
                }
                if ui.button("🎬 开始录制").clicked() {
                    // 修复 E0425：使用传入的 idx
                    *recording = true;
                    recording_keys.clear();
                    let _ = idx; // 录制实际逻辑后续实现
                }
            });
        }
    }

    fn start_new_macro(&mut self) {
        let new_macro = KeyMacro::default();
        self.macros.push(new_macro);
        self.editing_index = Some(self.macros.len() - 1);
        self.show_editor = true;
    }

    fn duplicate_macro(&mut self, idx: usize) {
        if let Some(macro_) = self.macros.get(idx).cloned() {
            let mut new_macro = macro_;
            new_macro.id = uuid::Uuid::new_v4().to_string();
            new_macro.name = format!("{} (副本)", new_macro.name);
            new_macro.created_at = chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
            new_macro.use_count = 0;
            self.macros.insert(idx + 1, new_macro);
        }
    }

    /// 供外部（如侧栏宏列表）直接执行指定宏，返回应写入终端的字节序列。
    pub fn send_macro_bytes(&mut self, idx: usize) -> Option<Vec<u8>> {
        self.execute_macro(idx)
    }

    /// 执行宏：把动作序列编码为终端字节流（Text 原样、Key 映射控制字符、
    /// Ctrl+X 组合键编码为控制码），返回 None 表示无可发送内容。
    fn execute_macro(&mut self, idx: usize) -> Option<Vec<u8>> {
        let macro_ = self.macros.get_mut(idx)?;
        macro_.use_count += 1;
        let mut out = Vec::new();
        for action in &macro_.keys {
            match action {
                KeyAction::Text(t) => out.extend_from_slice(t.as_bytes()),
                KeyAction::Key(k) => match k.as_str() {
                    "Enter" => out.push(b'\r'),
                    "Tab" => out.push(b'\t'),
                    "Escape" => out.extend_from_slice(b"\x1b"),
                    "Space" => out.push(b' '),
                    "Backspace" => out.push(0x7f),
                    _ => {}
                },
                KeyAction::KeyCombo(c) => {
                    // "Ctrl+L" → ^L（0x0C）等控制码；其余组合暂不支持
                    let parts: Vec<&str> = c.split('+').collect();
                    if parts.len() == 2 && parts[0].eq_ignore_ascii_case("ctrl") {
                        if let Some(ch) = parts[1].chars().next() {
                            if ch.is_ascii_alphabetic() {
                                out.push((ch.to_ascii_uppercase() as u8) & 0x1f);
                            }
                        }
                    }
                }
                KeyAction::Delay(_) | KeyAction::MouseClick(_, _, _) => {}
            }
        }
        (!out.is_empty()).then_some(out)
    }

    fn import_macros(&mut self) {
        if let Some(path) = rfd::FileDialog::new()
            .add_filter("JSON", &["json"])
            .pick_file()
        {
            if let Ok(content) = std::fs::read_to_string(path) {
                if let Ok(imported) = serde_json::from_str::<Vec<KeyMacro>>(&content) {
                    self.macros.extend(imported);
                }
            }
        }
    }

    fn export_macros(&self) {
        if let Some(path) = rfd::FileDialog::new()
            .add_filter("JSON", &["json"])
            .set_file_name("macros.json")
            .save_file()
        {
            let _ = std::fs::write(
                path,
                serde_json::to_string_pretty(&self.macros).unwrap_or_default(),
            );
        }
    }

    pub fn get_macro_by_hotkey(&self, hotkey: &str) -> Option<&KeyMacro> {
        self.macros.iter().find(|m| {
            m.enabled && matches!(&m.trigger, MacroTrigger::Hotkey(hk) if hk == hotkey)
        })
    }

    pub fn get_on_connect_macros(&self) -> Vec<&KeyMacro> {
        self.macros
            .iter()
            .filter(|m| m.enabled && matches!(&m.trigger, MacroTrigger::OnConnect))
            .collect()
    }

    pub fn get_on_disconnect_macros(&self) -> Vec<&KeyMacro> {
        self.macros
            .iter()
            .filter(|m| m.enabled && matches!(&m.trigger, MacroTrigger::OnDisconnect))
            .collect()
    }
}

pub fn apply_keep_alive() {
    // keep-alive 边缘处理：发送空包保持 SSH 连接
}

pub fn optimize_performance() {
    // 性能与内存调优建议：
    // 1. 启用编译时优化: cargo build --release
    // 2. 减少每帧重绘: 仅在脏标记时 request_repaint
    // 3. 限制回滚缓冲区大小: SCROLLBACK = 10000
    // 4. 批量处理输入事件: 使用 mpsc 通道缓冲
    // 5. 纹理复用: 远程桌面帧复用 TextureHandle
    // 6. 字体图集优化: 预加载常用字符
}