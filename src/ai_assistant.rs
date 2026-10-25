//! AI 助手面板（右侧停靠式，对齐参考设计稿）。
//!
//! - 顶部栏：标题 + Agent 开关 + 清空 / 设置按钮
//! - 对话区：用户消息右侧气泡、助手消息左侧气泡
//! - 底部：多行输入框 + 发送按钮（Enter 发送，Shift+Enter 换行）
//! - 后端：多提供方（OpenAI 兼容 / Ollama / Anthropic），阻塞调用放线程，
//!   结果经通道回传。

use crate::ai::{self, AiError, AiMessage};
use crate::config::AiSettings;
use eframe::egui;
use std::sync::mpsc::{channel, Receiver};

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Role {
    User,
    Assistant,
    Error,
}

/// 待发送的截图附件。
pub struct Attachment {
    pub name: String,
    /// PNG 的 base64（不含 data: 前缀）
    pub b64: String,
    pub texture: egui::TextureHandle,
    pub width: u32,
    pub height: u32,
}

/// 待发送的文件附件（📎 选择 / 拖入窗口）。
pub struct FileAttachment {
    pub name: String,
    pub payload: FilePayload,
}

/// 文件附件的载荷形态。
pub enum FilePayload {
    /// 文本文件：内容以代码块内联进 prompt
    Text(String),
    /// 图片文件：统一转成 PNG base64（不含 data: 前缀），走视觉通路
    ImageB64(String),
}

/// 文本附件截断字符数（几十 KB 足够 AI 理解，过多会挤爆 token）。
const FILE_TEXT_MAX_CHARS: usize = 64_000;

#[derive(Clone, Debug)]
pub struct ChatMessage {
    pub role: Role,
    pub content: String,
    pub time: String,
    /// AI 回复中提取出的命令，供 UI 提供「发送到终端」
    pub commands: Vec<String>,
}

pub struct AiAssistantPanel {
    pub open: bool,
    pub agent_mode: bool,
    pub messages: Vec<ChatMessage>,
    pub input: String,
    pub waiting: bool,
    /// 上一轮发给 AI 的终端上下文（展示用）
    pub last_context: String,
    /// 用户点击「发送到终端」产生的请求（命令, 是否危险）
    pub command_request: Option<(String, bool)>,
    /// 待发送的截图（最多 3 张）
    pub attachments: Vec<Attachment>,
    /// 待发送的文件附件（文本内联 / 图片走视觉通路）
    pub files: Vec<FileAttachment>,
    /// 输入区上方的一条提示：发送时会自动附带的终端信息
    /// （SSH user@host 等，由 app 层每帧更新；None = 当前无终端可附带）
    pub attach_hint: Option<String>,
    /// 用户按下发送（由 app 层取走并附加终端上下文后真正发出）
    pending_send: bool,
    rx: Option<Receiver<Result<String, AiError>>>,
    scroll_to_bottom: bool,
    /// 请求输入框聚焦
    focus_input: bool,
    /// 本帧 AI 面板矩形（app 层钳制 WebView 右缘用）
    pub last_layout: Option<egui::Rect>,
}

impl Default for AiAssistantPanel {
    fn default() -> Self {
        Self {
            open: false,
            agent_mode: false,
            messages: Vec::new(),
            input: String::new(),
            waiting: false,
            last_context: String::new(),
            command_request: None,
            attachments: Vec::new(),
            files: Vec::new(),
            attach_hint: None,
            pending_send: false,
            rx: None,
            scroll_to_bottom: false,
            focus_input: false,
            last_layout: None,
        }
    }
}

impl AiAssistantPanel {
    /// 取走「待发送」标记（app 层据此附加终端上下文后真正调用 send）。
    pub fn take_pending_send(&mut self) -> bool {
        std::mem::take(&mut self.pending_send)
    }
}

impl AiAssistantPanel {
    /// 右侧停靠面板（随主窗口布局，占据右侧固定宽度）。
    ///
    /// `sftp_takes_drop`：SFTP 面板正在接管本次文件拖放（指针悬在 SFTP 上），
    /// 此时 AI 不吞拖入的文件，避免「拖文件想上传却被附加进对话」。
    pub fn ui(
        &mut self,
        ui: &mut egui::Ui,
        ai: &mut AiSettings,
        open_config: &mut bool,
        sftp_takes_drop: bool,
    ) {
        if !self.open {
            self.rx = None;
            return;
        }
        self.poll_result();

        let layout_resp = egui::Panel::right("ai_assistant_panel")
            .default_size(360.0)
            .resizable(true)
            .show(ui, |ui| {
                self.render_panel(ui, ai, open_config, sftp_takes_drop);
            });
        // 记录面板矩形（app 层钳制 WebView 右缘用）
        self.last_layout = Some(layout_resp.response.rect);
    }

    fn render_panel(
        &mut self,
        ui: &mut egui::Ui,
        ai: &mut AiSettings,
        open_config: &mut bool,
        sftp_takes_drop: bool,
    ) {
        // ---- 顶栏 ----
        ui.horizontal(|ui| {
            ui.heading("AI 助手");
            ui.checkbox(&mut self.agent_mode, "Agent");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("✕").on_hover_text("关闭面板").clicked() {
                    self.open = false;
                }
                if ui.button("⚙").on_hover_text("AI 配置").clicked() {
                    *open_config = true;
                }
                if ui.button("🗑").on_hover_text("清空对话").clicked() {
                    self.messages.clear();
                    self.last_context.clear();
                    self.attachments.clear();
                    self.files.clear();
                }
                if ui
                    .button("📷")
                    .on_hover_text("截取当前界面附到对话里（需视觉模型）")
                    .clicked()
                {
                    self.capture_screenshot(ui.ctx());
                }
            });
        });
        ui.separator();

        // ---- 提供方状态条 ----
        ui.horizontal(|ui| {
            let provider = ai.provider();
            ui.label(
                egui::RichText::new(format!("{} · {}", provider.label(), ai.model))
                    .size(11.0)
                    .color(ui.visuals().weak_text_color()),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ai.read_only {
                    ui.label(
                        egui::RichText::new("🔒 只读")
                            .size(11.0)
                            .color(egui::Color32::from_rgb(230, 170, 80)),
                    );
                }
            });
        });
        ui.separator();

        // ---- 输入区（底部停靠） ----
        // 必须先于消息区 show：egui 的面板按 show 顺序占用父区域，
        // 若先画消息区（auto_shrink=false 会吃满高度），输入框会被挤到可视区之外。
        let mut send = false;
        egui::Panel::bottom("ai_input_dock")
            .frame(
                egui::Frame::new()
                    .inner_margin(egui::Margin::symmetric(6, 6))
                    .fill(ui.visuals().panel_fill),
            )
            .show(ui, |ui| {
                // ---- 拖拽文件到窗口直接附加（SFTP 面板接管的除外）----
                if !sftp_takes_drop {
                    let dropped: Vec<egui::DroppedFileHandle> =
                        ui.input(|i| i.raw.dropped_files.clone());
                    if !dropped.is_empty() {
                        for f in dropped {
                            self.ingest_file(f.path());
                        }
                    }
                }

                // ---- 自动附带提示：发送时会把哪些终端信息带给 AI ----
                if let Some(hint) = self.attach_hint.as_ref() {
                    if !hint.is_empty() {
                        ui.label(
                            egui::RichText::new(format!(
                                "📎 发送时自动附带：{hint} + 最近 {} 行终端输出",
                                ai.context_lines
                            ))
                            .size(10.0)
                            .color(ui.visuals().weak_text_color()),
                        );
                    }
                }

                // ---- 已附加的文件（点击移除）----
                if !self.files.is_empty() {
                    let mut drop_idx: Option<usize> = None;
                    ui.horizontal_wrapped(|ui| {
                        for (i, f) in self.files.iter().enumerate() {
                            let size = match &f.payload {
                                FilePayload::Text(t) => format!("{} 字", t.len()),
                                FilePayload::ImageB64(b) => {
                                    format!("图片 {} KB", b.len() * 3 / 4 / 1024)
                                }
                            };
                            let btn = ui.button(format!("📄 {}", f.name));
                            if btn.clicked() {
                                drop_idx = Some(i);
                            }
                            btn.on_hover_text(format!(
                                "{}（{}）\n点击移除",
                                f.name, size
                            ));
                        }
                    });
                    if let Some(i) = drop_idx {
                        self.files.remove(i);
                    }
                }

                // ---- 已附加的截图（点击移除）----
                if !self.attachments.is_empty() {
                    let mut drop_idx: Option<usize> = None;
                    ui.horizontal_wrapped(|ui| {
                        for (i, att) in self.attachments.iter().enumerate() {
                            let (rect, resp) = ui.allocate_exact_size(
                                egui::vec2(56.0, 34.0),
                                egui::Sense::click(),
                            );
                            ui.painter().rect_filled(
                                rect,
                                3.0,
                                ui.visuals().extreme_bg_color,
                            );
                            ui.painter().image(
                                att.texture.id(),
                                rect.shrink(2.0),
                                egui::Rect::from_min_max(
                                    egui::pos2(0.0, 0.0),
                                    egui::pos2(1.0, 1.0),
                                ),
                                egui::Color32::WHITE,
                            );
                            if resp.clicked() {
                                drop_idx = Some(i);
                            }
                            resp.on_hover_text(format!(
                                "{}（{}×{}）\n点击移除",
                                att.name, att.width, att.height
                            ));
                        }
                    });
                    if let Some(i) = drop_idx {
                        self.attachments.remove(i);
                    }
                }

                // 输入框占满面板宽度，按钮单独一行——避免水平布局里
                // add_sized 与 available_width 互相挤压，导致输入框被压窄。
                let edit_w = ui.available_width();
                let resp = ui.add_sized(
                    [edit_w, 46.0],
                    egui::TextEdit::multiline(&mut self.input)
                        .desired_rows(2)
                        .hint_text("问点什么...（Enter 发送，Shift+Enter 换行）"),
                );
                if self.focus_input {
                    resp.request_focus();
                    self.focus_input = false;
                }
                if resp.has_focus() {
                    let (enter, shift) =
                        ui.input(|i| (i.key_pressed(egui::Key::Enter), i.modifiers.shift));
                    if enter && !shift {
                        send = true;
                        resp.request_focus();
                    }
                }

                ui.horizontal(|ui| {
                    if ui
                        .small_button("📎 附加")
                        .on_hover_text("附加文件：文本内联发给 AI，图片走视觉通路；也可直接拖文件进窗口")
                        .clicked()
                    {
                        self.pick_files();
                    }
                    let btn = ui.add_sized(
                        [64.0, 24.0],
                        egui::Button::new(egui::RichText::new("发送").size(12.0)),
                    );
                    if btn.clicked() {
                        send = true;
                    }
                    if ui.small_button("清空输入").clicked() {
                        self.input.clear();
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(
                            egui::RichText::new(format!("历史 {} 条", self.messages.len()))
                                .size(10.0)
                                .color(egui::Color32::GRAY),
                        );
                    });
                });
            });


        // ---- 对话区（占满剩余空间） ----
        egui::ScrollArea::vertical()
            .id_salt("ai_messages")
            .auto_shrink([false, false])
            .stick_to_bottom(true)
            .show(ui, |ui| {
                if self.messages.is_empty() && !self.waiting {
                    ui.vertical_centered(|ui| {
                        ui.add_space(50.0);
                        ui.label(
                            egui::RichText::new("💬").size(30.0).color(egui::Color32::GRAY),
                        );
                        ui.add_space(6.0);
                        ui.label(
                            egui::RichText::new("你好，我是 RustTerm AI 助手")
                                .size(14.0)
                                .strong(),
                        );
                        ui.label(
                            egui::RichText::new("可以询问运维命令、排查问题、分析日志等")
                                .size(11.0)
                                .color(egui::Color32::GRAY),
                        );
                        if ai.api_key.trim().is_empty() && ai.provider().needs_api_key() {
                            ui.add_space(10.0);
                            ui.label(
                                egui::RichText::new(
                                    "ℹ 当前未填 API Key，将按无鉴权方式请求（本地网关可用）",
                                )
                                .size(11.0)
                                .color(egui::Color32::from_rgb(150, 170, 200)),
                            );
                        }
                    });
                }

                // 先快照消息，避免渲染时借用 self.messages 与 &mut self 冲突
                let snapshot: Vec<(Role, String, String, Vec<String>)> = self
                    .messages
                    .iter()
                    .map(|m| {
                        (
                            m.role,
                            m.content.clone(),
                            m.time.clone(),
                            m.commands.clone(),
                        )
                    })
                    .collect();
                for (role, content, time, commands) in snapshot {
                    let msg = ChatMessage { role, content, time, commands };
                    self.render_message(ui, &msg, ai);
                }

                if self.waiting {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label("思考中…");
                    });
                }
                ui.add_space(4.0);
            });

        if self.scroll_to_bottom {
            self.scroll_to_bottom = false;
        }

        if send {
            // 由 app 层取走：附加当前终端上下文后再真正发送
            self.pending_send = true;
        }
    }

    fn render_message(&mut self, ui: &mut egui::Ui, msg: &ChatMessage, ai: &AiSettings) {
        match msg.role {
            Role::User => {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::LEFT), |ui| {
                    egui::Frame::new()
                        .fill(egui::Color32::from_rgb(38, 79, 138))
                        .corner_radius(8.0)
                        .inner_margin(egui::Margin::symmetric(10, 6))
                        .show(ui, |ui| {
                            ui.set_max_width(ui.available_width() * 0.75);
                            ui.label(&msg.content);
                        });
                });
            }
            Role::Assistant => {
                egui::Frame::new()
                    .fill(ui.visuals().faint_bg_color)
                    .corner_radius(8.0)
                    .inner_margin(egui::Margin::symmetric(10, 6))
                    .show(ui, |ui| {
                        ui.set_max_width(ui.available_width() * 0.85);
                        ui.label(&msg.content);

                        // 任务完成标记
                        if ai::is_task_done(&msg.content) {
                            ui.label(
                                egui::RichText::new("✅ 任务已完成")
                                    .size(11.0)
                                    .color(egui::Color32::from_rgb(69, 197, 112)),
                            );
                        }

                        // AI 建议的命令：提供「发送到终端」按钮
                        if !msg.commands.is_empty() {
                            ui.add_space(4.0);
                            ui.separator();
                            for cmd in &msg.commands {
                                ui.horizontal(|ui| {
                                    let dangerous = ai::is_dangerous(cmd);
                                    let is_write = ai::is_write_command(cmd);
                                    let blocked = ai.read_only && is_write;

                                    ui.label(
                                        egui::RichText::new(format!("$ {cmd}"))
                                            .size(11.0)
                                            .monospace()
                                            .color(if dangerous {
                                                egui::Color32::from_rgb(234, 78, 102)
                                            } else {
                                                ui.visuals().text_color()
                                            }),
                                    );

                                    if blocked {
                                        ui.label(
                                            egui::RichText::new("🔒 只读模式已禁用")
                                                .size(10.0)
                                                .color(egui::Color32::from_rgb(230, 170, 80)),
                                        );
                                    } else if dangerous {
                                        ui.label(
                                            egui::RichText::new("⚠ 危险命令")
                                                .size(10.0)
                                                .color(egui::Color32::from_rgb(234, 78, 102)),
                                        );
                                    } else {
                                        let mut req = None;
                                        if ui
                                            .small_button("▶ 发送到终端")
                                            .on_hover_text("把该命令写入当前终端")
                                            .clicked()
                                        {
                                            req = Some(cmd.clone());
                                        }
                                        if let Some(c) = req {
                                            self.command_request = Some((c, dangerous));
                                        }
                                    }
                                });
                            }
                        }
                    });
            }
            Role::Error => {
                egui::Frame::new()
                    .fill(egui::Color32::from_rgb(80, 40, 40))
                    .corner_radius(8.0)
                    .inner_margin(egui::Margin::symmetric(10, 6))
                    .show(ui, |ui| {
                        ui.set_max_width(ui.available_width() * 0.85);
                        ui.label(
                            egui::RichText::new(&msg.content)
                                .color(egui::Color32::from_rgb(240, 160, 160)),
                        );
                    });
            }
        }
        ui.label(
            egui::RichText::new(&msg.time).size(10.0).color(egui::Color32::GRAY),
        );
        ui.add_space(5.0);
    }

    /// 发送一条消息。
    ///
    /// `terminal_context` 为终端最近输出（可为空），会作为上下文附加到
    /// 用户消息之前，让 AI 了解当前终端状态。
    pub fn send(&mut self, ai: &AiSettings, terminal_context: Option<String>) {
        let text = self.input.trim().to_string();
        if text.is_empty() || self.waiting {
            return;
        }
        self.input.clear();

        // 组装发给模型的消息（含终端上下文）
        let mut history: Vec<AiMessage> = Vec::new();
        let recent: Vec<&ChatMessage> = self.messages.iter().collect();
        let keep = ai.max_history.max(1);
        let start = recent.len().saturating_sub(keep);
        for m in &recent[start..] {
            if m.role == Role::Error {
                continue;
            }
            let role = if m.role == Role::User { "user" } else { "assistant" };
            history.push(AiMessage {
                role: role.to_string(),
                content: m.content.clone(),
                images: Vec::new(),
            });
        }

        // 终端上下文
        let mut ctx_for_display = terminal_context.unwrap_or_default();
        let mut prompt = if !ctx_for_display.trim().is_empty() {
            format!(
                "【当前终端输出（最近 {} 行）】\n{}\n\n【用户问题】\n{}",
                ai.context_lines, ctx_for_display, text
            )
        } else {
            text.clone()
        };

        // 截图附件（📷）：随本条用户消息一起发给模型
        let mut images: Vec<String> = self.attachments.iter().map(|a| a.b64.clone()).collect();
        // 文件附件（📎）：文本内联进 prompt；图片并入视觉通路（截图之后）
        for f in &self.files {
            match &f.payload {
                FilePayload::Text(t) => {
                    prompt.push_str(&format!(
                        "\n\n【附件文件：{}】\n```text\n{}\n```",
                        f.name, t
                    ));
                }
                FilePayload::ImageB64(b) => images.push(b.clone()),
            }
        }
        if !images.is_empty() {
            prompt.push_str(&format!(
                "\n\n（附 {} 张界面截图，请结合截图内容回答）",
                images.len()
            ));
        }
        let display = prompt.clone();
        history.push(AiMessage::user_with_images(prompt, images.clone()));
        self.last_context = std::mem::take(&mut ctx_for_display);

        // 用户气泡直接展示实际发送的完整内容（终端上下文 + 问题），
        // 让用户清楚这次请求带了什么。
        self.messages.push(ChatMessage {
            role: Role::User,
            content: display,
            time: chrono::Local::now().format("%H:%M:%S").to_string(),
            commands: Vec::new(),
        });

        // 预检配置（API Key 允许为空：本地无鉴权网关常见）
        if !ai.has_endpoint() {
            self.push_error("未配置 Base URL，请点击右上角 ⚙ 打开 AI 配置。".into());
            return;
        }
        if ai.model.trim().is_empty() {
            self.push_error("尚未配置模型名，请点击右上角 ⚙ 打开 AI 配置。".into());
            return;
        }

        self.attachments.clear();
        self.files.clear();
        let agent_mode = self.agent_mode;
        let cfg = ai.clone();
        let (tx, rx) = channel::<Result<String, AiError>>();
        self.rx = Some(rx);
        self.waiting = true;
        self.scroll_to_bottom = true;

        std::thread::spawn(move || {
            let result = ai::chat(&cfg, agent_mode, &history);
            let _ = tx.send(result);
        });
    }

    /// 打开多选文件对话框，选中的文件附加到对话。
    fn pick_files(&mut self) {
        let Some(paths) = rfd::FileDialog::new()
            .set_title("选择要附加的文件")
            .pick_files()
        else {
            return;
        };
        for p in paths {
            self.ingest_file(&p);
        }
    }

    /// 读取并附加一个文件：PNG 直接用；JPEG 转 PNG；其余按文本尝试，失败报错。
    fn ingest_file(&mut self, path: &std::path::Path) {
        use base64::Engine as _;
        // 附件总数（截图 + 文件）上限，避免一次消息塞爆
        if self.attachments.len() + self.files.len() >= 6 {
            self.push_error("附件太多（最多 6 个：截图 + 文件），先移除一些再试。".into());
            return;
        }
        let Some(name) = path.file_name().and_then(|n| n.to_str()).map(String::from) else {
            return;
        };
        let bytes = match std::fs::read(path) {
            Ok(b) => b,
            Err(e) => {
                self.push_error(format!("读取 {} 失败：{e}", name));
                return;
            }
        };
        if bytes.is_empty() {
            self.push_error(format!("{name} 是空文件，跳过。"));
            return;
        }

        // ---- 图片文件：PNG 直接用；JPEG 用 image crate 转 PNG（ai.rs 视觉通路统一 PNG）----
        if bytes.starts_with(&[0x89, b'P', b'N', b'G']) {
            self.files.push(FileAttachment {
                name,
                payload: FilePayload::ImageB64(
                    base64::engine::general_purpose::STANDARD.encode(&bytes),
                ),
            });
            return;
        }
        if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
            match image::load_from_memory(&bytes) {
                Ok(img) => {
                    // 有损输入重编码为 PNG，消息气泡缩略图与各家 API 通路一致
                    let mut png_buf: Vec<u8> = Vec::new();
                    match image::DynamicImage::ImageRgb8(img.into_rgb8()).write_to(
                        &mut std::io::Cursor::new(&mut png_buf),
                        image::ImageFormat::Png,
                    ) {
                        Ok(()) => {
                            self.files.push(FileAttachment {
                                name,
                                payload: FilePayload::ImageB64(
                                    base64::engine::general_purpose::STANDARD.encode(&png_buf),
                                ),
                            });
                            return;
                        }
                        Err(e) => {
                            self.push_error(format!("{name} 转换 PNG 失败：{e}"));
                            return;
                        }
                    }
                }
                Err(e) => {
                    self.push_error(format!("解析图片 {name} 失败：{e}"));
                    return;
                }
            }
        }

        // ---- 文本文件：UTF-8（或近 ASCII）→ 以代码块内联 ----
        match String::from_utf8(bytes) {
            Ok(text) => {
                let mut text = text;
                if text.chars().count() > FILE_TEXT_MAX_CHARS {
                    let total = text.chars().count();
                    text = text.chars().take(FILE_TEXT_MAX_CHARS).collect();
                    text.push_str(&format!(
                        "\n（……内容过长已截断，共 {total} 字符，仅展示前 {} 个）",
                        FILE_TEXT_MAX_CHARS
                    ));
                }
                self.files.push(FileAttachment {
                    name,
                    payload: FilePayload::Text(text),
                });
            }
            Err(_) => {
                self.push_error(format!(
                    "{name} 不是文本文件（也不是 PNG/JPEG 图片），暂不支持。"
                ));
            }
        }
    }

    /// 抓取主窗口客户区，压到最长边 1280 后作为附件。
    fn capture_screenshot(&mut self, ctx: &egui::Context) {
        if self.attachments.len() >= 3 {
            self.push_error("最多附加 3 张截图，先移除一张再截。".into());
            return;
        }
        match crate::screenshot::capture_main_window() {
            Ok(cap) => {
                let cap = cap.downscaled(1280);
                let (w, h) = (cap.width, cap.height);
                match cap.to_png() {
                    Ok(png) => {
                        use base64::Engine as _;
                        let b64 = base64::engine::general_purpose::STANDARD.encode(&png);
                        let img = egui::ColorImage::from_rgba_unmultiplied(
                            [w as usize, h as usize],
                            &cap.rgba,
                        );
                        let texture = ctx.load_texture(
                            format!("ai-shot-{}", self.attachments.len()),
                            img,
                            egui::TextureOptions::LINEAR,
                        );
                        self.attachments.push(Attachment {
                            name: chrono::Local::now().format("shot-%H%M%S.png").to_string(),
                            b64,
                            texture,
                            width: w,
                            height: h,
                        });
                    }
                    Err(e) => self.push_error(format!("截图编码失败: {e}")),
                }
            }
            Err(e) => self.push_error(format!("截图失败: {e}")),
        }
    }

    fn push_error(&mut self, msg: String) {
        self.messages.push(ChatMessage {
            role: Role::Error,
            content: msg,
            time: chrono::Local::now().format("%H:%M:%S").to_string(),
            commands: Vec::new(),
        });
        self.scroll_to_bottom = true;
    }

    fn poll_result(&mut self) {
        let got = match &self.rx {
            Some(rx) => rx.try_recv().ok(),
            None => None,
        };
        if let Some(result) = got {
            self.rx = None;
            self.waiting = false;
            let (role, content) = match result {
                Ok(text) => (Role::Assistant, text),
                Err(e) => {
                    // 配置类错误额外提示去哪里配置
                    if e.config_error {
                        (Role::Error, format!("{}\n\n（点击右上角 ⚙ 打开 AI 配置）", e.message))
                    } else {
                        (Role::Error, e.message)
                    }
                }
            };
            let commands = if role == Role::Assistant {
                ai::extract_commands(&content)
            } else {
                Vec::new()
            };
            self.messages.push(ChatMessage {
                role,
                content,
                time: chrono::Local::now().format("%H:%M:%S").to_string(),
                commands,
            });
            self.scroll_to_bottom = true;
        }
    }
}
