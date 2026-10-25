//! 远程桌面标签页：VNC / RDP / SPICE 的真实连接与渲染。
//!
//! - 后端：`crate::remote`（每会话一个线程 + 事件通道）
//! - 渲染：整帧贴图 + 脏矩形 `set_partial` 增量更新
//! - 输入：键盘（扫描码 + keysym）、鼠标、滚轮转发到远端

use crate::remote::{self, RdCommand, RdEvent, RdParams, RdProtocol, RdSession, RdState};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RdTabProtocol {
    Vnc,
    Rdp,
    Spice,
}

impl RdTabProtocol {
    pub fn label(self) -> &'static str {
        match self {
            RdTabProtocol::Vnc => "VNC (RFB)",
            RdTabProtocol::Rdp => "RDP (Microsoft)",
            RdTabProtocol::Spice => "SPICE (QEMU)",
        }
    }

    pub fn default_port(self) -> u16 {
        match self {
            RdTabProtocol::Vnc => 5900,
            RdTabProtocol::Rdp => 3389,
            RdTabProtocol::Spice => 5930,
        }
    }

    fn to_backend(self) -> RdProtocol {
        match self {
            RdTabProtocol::Vnc => RdProtocol::Vnc,
            RdTabProtocol::Rdp => RdProtocol::Rdp,
            RdTabProtocol::Spice => RdProtocol::Spice,
        }
    }

    pub fn all() -> [RdTabProtocol; 3] {
        [RdTabProtocol::Vnc, RdTabProtocol::Rdp, RdTabProtocol::Spice]
    }
}

#[derive(Clone, Debug)]
pub struct RdpConnectionProfile {
    pub name: String,
    pub protocol: RdTabProtocol,
    pub host: String,
    pub port: u16,
    pub username: String,
    pub password: String,
    pub domain: String,
    pub color_depth: u8,
}

impl Default for RdpConnectionProfile {
    fn default() -> Self {
        Self {
            name: String::new(),
            protocol: RdTabProtocol::Vnc,
            host: String::new(),
            port: 5900,
            username: String::new(),
            password: String::new(),
            domain: String::new(),
            color_depth: 32,
        }
    }
}

/// 全局共享的连接档案表（远程桌面标签之间共用）。
#[derive(Clone)]
pub struct RdpProfiles {
    pub profiles: Vec<RdpConnectionProfile>,
}

impl Default for RdpProfiles {
    fn default() -> Self {
        Self {
            profiles: Vec::new(),
        }
    }
}

impl RdpProfiles {
    pub fn index_of(&self, name: &str) -> Option<usize> {
        self.profiles.iter().position(|p| p.name == name)
    }

    pub fn ensure(&mut self, profile: RdpConnectionProfile) -> usize {
        if let Some(i) = self.index_of(&profile.name) {
            self.profiles[i] = profile;
            i
        } else {
            self.profiles.push(profile);
            self.profiles.len() - 1
        }
    }
}

pub struct RemoteDesktopTab {
    pub profile: RdpConnectionProfile,
    pub state: RdState,
    /// 最近一次失败原因（后端会先发 Error 再发 Disconnected，
    /// 后者不能把错误信息冲掉，否则用户只看到「连不上」没有原因）
    pub last_error: Option<String>,
    pub frame_texture: Option<egui::TextureHandle>,
    pub frame_width: u32,
    pub frame_height: u32,
    /// 自适应窗口
    pub fit: bool,
    /// 非自适应时的缩放
    pub zoom: f32,
    pub fps: f32,
    pub clipboard_sync: bool,
    /// 捕获键盘（关闭后键盘事件不再发往远端）
    pub capture_keys: bool,
    session: Option<RdSession>,
    /// 上一帧的修饰键状态（shift, ctrl, alt）
    prev_mods: (bool, bool, bool),
    /// 当前按下的鼠标键位掩码
    buttons: u8,
    last_pointer: Option<(u16, u16)>,
    frames: u32,
    fps_t: std::time::Instant,
    /// 后端最初报上来的尺寸（重连时沿用）
    desired: (u16, u16),
}

impl RemoteDesktopTab {
    pub fn new(profile: RdpConnectionProfile) -> Self {
        Self {
            desired: (1280, 800),
            profile,
            state: RdState::Disconnected,
            last_error: None,
            frame_texture: None,
            frame_width: 0,
            frame_height: 0,
            fit: true,
            zoom: 1.0,
            fps: 0.0,
            clipboard_sync: true,
            capture_keys: true,
            session: None,
            prev_mods: (false, false, false),
            buttons: 0,
            last_pointer: None,
            frames: 0,
            fps_t: std::time::Instant::now(),
        }
    }

    pub fn title(&self) -> String {
        if self.profile.name.is_empty() {
            format!("{}:{}", self.profile.host, self.profile.port)
        } else {
            self.profile.name.clone()
        }
    }

    pub fn is_active(&self) -> bool {
        matches!(self.state, RdState::Active)
    }

    pub fn is_connecting(&self) -> bool {
        matches!(self.state, RdState::Connecting | RdState::Authenticating)
    }

    pub fn error(&self) -> Option<&str> {
        if let Some(e) = &self.last_error {
            return Some(e.as_str());
        }
        match &self.state {
            RdState::Failed(e) => Some(e.as_str()),
            _ => None,
        }
    }

    /// 建立连接（已有会话先断开）。
    pub fn connect(&mut self) {
        if self.is_connecting() || self.is_active() {
            return;
        }
        if self.profile.host.trim().is_empty() {
            self.state = RdState::Failed("请先填写主机地址".into());
            return;
        }
        self.disconnect();
        self.frame_texture = None;
        self.frame_width = 0;
        self.frame_height = 0;

        let params = RdParams {
            protocol: self.profile.protocol.to_backend(),
            host: self.profile.host.trim().to_string(),
            port: self.profile.port,
            username: self.profile.username.clone(),
            password: self.profile.password.clone(),
            domain: self.profile.domain.clone(),
            width: self.desired.0,
            height: self.desired.1,
            color_depth: self.profile.color_depth,
        };
        self.state = RdState::Connecting;
        self.session = Some(remote::spawn(params));
        self.prev_mods = (false, false, false);
        self.buttons = 0;
        self.last_pointer = None;
        self.fps_t = std::time::Instant::now();
        self.frames = 0;
    }

    pub fn disconnect(&mut self) {
        if let Some(mut s) = self.session.take() {
            s.close();
        }
        self.last_error = None;
        self.state = RdState::Disconnected;
        self.frame_texture = None;
        self.frame_width = 0;
        self.frame_height = 0;
        self.prev_mods = (false, false, false);
        self.buttons = 0;
    }

    /// 消费后端事件并更新纹理。返回是否需要继续重绘。
    pub fn poll(&mut self, ctx: &egui::Context) -> bool {
        let mut alive = false;
        let mut got_frame = false;

        // 先把事件全部取出来，避免与 self 的可变借用冲突
        let mut events: Vec<RdEvent> = Vec::new();
        if let Some(s) = &self.session {
            while let Ok(ev) = s.events.try_recv() {
                events.push(ev);
            }
        }

        for ev in events {
            match ev {
                RdEvent::State(st) => {
                    match st {
                        RdState::Disconnected => {
                            // 会话线程在报错后还会再发一次 Disconnected：
                            // 有错误信息时保留错误，别把提示冲掉
                            if self.last_error.is_none() {
                                self.state = RdState::Disconnected;
                            }
                            self.session = None;
                        }
                        other => {
                            self.state = other;
                            alive = true;
                        }
                    }
                }
                RdEvent::Resized { width, height } => {
                    self.frame_width = width;
                    self.frame_height = height;
                    let img = egui::ColorImage::filled(
                        [width as usize, height as usize],
                        egui::Color32::BLACK,
                    );
                    self.frame_texture = Some(ctx.load_texture(
                        format!("rd-{}", self.title()),
                        img,
                        egui::TextureOptions::LINEAR,
                    ));
                }
                RdEvent::Frame { x, y, w, h, rgba } => {
                    let Some(tex) = self.frame_texture.as_mut() else {
                        continue;
                    };
                    // 越界裁剪：纹理局部更新会 debug_assert
                    if x >= self.frame_width || y >= self.frame_height {
                        continue;
                    }
                    let w = w.min(self.frame_width - x) as usize;
                    let h = h.min(self.frame_height - y) as usize;
                    if w == 0 || h == 0 {
                        continue;
                    }
                    if rgba.len() < w * h * 4 {
                        continue;
                    }
                    let image = egui::ColorImage::from_rgba_unmultiplied([w, h], &rgba);
                    tex.set_partial(
                        [x as usize, y as usize],
                        image,
                        egui::TextureOptions::LINEAR,
                    );
                    self.frames += 1;
                    got_frame = true;
                }
                RdEvent::Error(msg) => {
                    self.last_error = Some(msg.clone());
                    self.state = RdState::Failed(msg);
                }
            }
        }

        if got_frame {
            if self.fps_t.elapsed().as_secs_f32() >= 0.5 {
                self.fps = self.frames as f32 / self.fps_t.elapsed().as_secs_f32();
                self.frames = 0;
                self.fps_t = std::time::Instant::now();
            }
        }

        if alive || self.fps_t.elapsed().as_millis() < 800 {
            ctx.request_repaint_after(std::time::Duration::from_millis(16));
            true
        } else {
            got_frame
        }
    }

    fn send(&self, cmd: RdCommand) {
        if let Some(s) = &self.session {
            s.send(cmd);
        }
    }

    /// 按目标状态同步修饰键（Shift/Ctrl/Alt）。
    ///
    /// `sent` 记录的是「远端当前认为按下的状态」：
    /// 目标为按下而远端没按 → 发按下；目标为松开而远端还按 → 发抬起。
    fn apply_mods(&mut self, target: (bool, bool, bool)) {
        const MODS: [remote::KeyInput; 3] =
            [remote::MOD_SHIFT, remote::MOD_CTRL, remote::MOD_ALT];
        for (idx, proto) in MODS.iter().enumerate() {
            let want = match idx {
                0 => target.0,
                1 => target.1,
                _ => target.2,
            };
            let held = match idx {
                0 => self.prev_mods.0,
                1 => self.prev_mods.1,
                _ => self.prev_mods.2,
            };
            if want == held {
                continue;
            }
            self.send(RdCommand::Key(remote::KeyInput {
                down: want,
                ..*proto
            }));
            match idx {
                0 => self.prev_mods.0 = want,
                1 => self.prev_mods.1 = want,
                _ => self.prev_mods.2 = want,
            }
        }
    }

    /// 渲染整个标签页内容。
    pub fn ui(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        self.poll(ctx);

        // ---- 顶部工具条 ----
        ui.horizontal(|ui| {
            match &self.state {
                RdState::Disconnected | RdState::Failed(_) => {
                    if ui.button("🔌 连接").clicked() {
                        self.connect();
                    }
                }
                RdState::Connecting | RdState::Authenticating => {
                    let connecting = self.state == RdState::Connecting;
                    ui.spinner();
                    ui.label(if connecting { "正在连接…" } else { "正在认证…" });
                    if ui.button("取消").clicked() {
                        self.disconnect();
                    }
                }
                RdState::Active => {
                    if ui.button("🔌 断开").clicked() {
                        self.disconnect();
                    }
                }
            }

            ui.separator();
            ui.checkbox(&mut self.fit, "自适应");
            if !self.fit {
                ui.add(
                    egui::Slider::new(&mut self.zoom, 0.25..=3.0)
                        .show_value(false)
                        .text("缩放"),
                );
                ui.label(format!("{:.0}%", self.zoom * 100.0));
            }
            ui.checkbox(&mut self.capture_keys, "捕获键盘");
            ui.checkbox(&mut self.clipboard_sync, "剪贴板");

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if self.is_active() {
                    ui.label(
                        egui::RichText::new(format!(
                            "{}×{}  {:.0} fps",
                            self.frame_width, self.frame_height, self.fps
                        ))
                        .size(10.0)
                        .color(ui.visuals().weak_text_color()),
                    );
                }
            });

            // 失败原因直接显示在工具条上（表单区容易被滚动条挡住）
            if let Some(err) = self.error().map(|e| e.to_string()) {
                ui.horizontal(|ui| {
                    ui.colored_label(
                        egui::Color32::from_rgb(240, 130, 130),
                        format!("⚠ {err}"),
                    );
                    if ui.small_button("复制").on_hover_text("复制错误信息").clicked() {
                        ctx.copy_text(err.clone());
                    }
                    if ui.small_button("重试").clicked() {
                        self.connect();
                    }
                });
            }
        });
        ui.separator();

        if !self.is_active() && self.frame_texture.is_none() {
            self.connect_form(ui);
            return;
        }

        // ---- 画面 ----
        let avail = ui.available_size();
        let (rect, response) =
            ui.allocate_exact_size(avail, egui::Sense::click_and_drag());
        ui.painter()
            .rect_filled(rect, 0.0, egui::Color32::from_rgb(20, 20, 24));

        let Some(tex) = self.frame_texture.as_ref() else {
            return;
        };
        let tex_size = tex.size_vec2();
        let scale = if self.fit || self.zoom <= 0.0 {
            (avail.x / tex_size.x).min(avail.y / tex_size.y).min(1.0)
        } else {
            self.zoom
        };
        let draw_rect = egui::Rect::from_center_size(
            rect.center(),
            egui::vec2(tex_size.x * scale, tex_size.y * scale),
        );
        ui.painter().image(
            tex.id(),
            draw_rect,
            egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
            egui::Color32::WHITE,
        );

        if !self.is_active() {
            if let Some(err) = self.error() {
                ui.painter().text(
                    rect.center(),
                    egui::Align2::CENTER_CENTER,
                    format!("已断开：{err}"),
                    egui::FontId::proportional(13.0),
                    egui::Color32::from_rgb(230, 120, 120),
                );
            }
        }

        // ---- 输入转发 ----
        self.forward_input(ui, &response, draw_rect);
    }

    fn connect_form(&mut self, ui: &mut egui::Ui) {
        // 用显式宽度居中：`vertical_centered` + `Frame` + `Grid` 组合下
        // 内层 Ui 的可用宽度会被压成 0，导致整个表单只占位、不绘制。
        let form_w = 440.0f32;
        let margin = ((ui.available_width() - form_w) * 0.5).max(8.0);

        ui.horizontal_top(|ui| {
            ui.add_space(margin);
            ui.vertical(|ui| {
                ui.set_width(form_w);
                ui.add_space(18.0);
                ui.heading(format!("🖥 {}", self.title()));
                ui.add_space(10.0);

                egui::Frame::group(ui.style())
                    .inner_margin(12.0)
                    .show(ui, |ui| {
                        ui.set_width(form_w - 24.0);
                        egui::Grid::new("rd_connect_form")
                            .num_columns(2)
                            .spacing([10.0, 8.0])
                            .min_col_width(64.0)
                            .show(ui, |ui| {
                                ui.label("协议");
                                egui::ComboBox::from_id_salt("rd_protocol")
                                    .width(220.0)
                                    .selected_text(self.profile.protocol.label())
                                    .show_ui(ui, |ui| {
                                        for p in RdTabProtocol::all() {
                                            if ui
                                                .selectable_value(
                                                    &mut self.profile.protocol,
                                                    p,
                                                    p.label(),
                                                )
                                                .changed()
                                            {
                                                // 端口跟随协议默认值（仅当仍是旧的默认端口）
                                                const OLD: [u16; 3] = [5900, 3389, 5930];
                                                if OLD.contains(&self.profile.port) {
                                                    self.profile.port = p.default_port();
                                                }
                                            }
                                        }
                                    });
                                ui.end_row();

                                ui.label("主机");
                                ui.add(
                                    egui::TextEdit::singleline(&mut self.profile.host)
                                        .desired_width(220.0)
                                        .hint_text("192.168.1.16"),
                                );
                                ui.end_row();

                                ui.label("端口");
                                let mut port = self.profile.port.to_string();
                                if ui
                                    .add(
                                        egui::TextEdit::singleline(&mut port)
                                            .desired_width(220.0),
                                    )
                                    .changed()
                                {
                                    if let Ok(p) = port.trim().parse::<u16>() {
                                        self.profile.port = p;
                                    }
                                }
                                ui.end_row();

                                if self.profile.protocol == RdTabProtocol::Rdp {
                                    ui.label("用户");
                                    ui.add(
                                        egui::TextEdit::singleline(&mut self.profile.username)
                                            .desired_width(220.0),
                                    );
                                    ui.end_row();
                                    ui.label("密码");
                                    crate::widgets::password_field(
                                        ui,
                                        "rdp-proto-password",
                                        &mut self.profile.password,
                                        220.0,
                                        "",
                                    );
                                    ui.end_row();
                                    ui.label("域");
                                    ui.add(
                                        egui::TextEdit::singleline(&mut self.profile.domain)
                                            .desired_width(220.0)
                                            .hint_text("可空"),
                                    );
                                    ui.end_row();
                                } else {
                                    ui.label("密码");
                                    crate::widgets::password_field(
                                        ui,
                                        "rdp-vncspice-password",
                                        &mut self.profile.password,
                                        220.0,
                                        "VNC/SPICE 密码，可空",
                                    );
                                    ui.end_row();
                                }

                                ui.label("分辨率");
                                ui.horizontal(|ui| {
                                    let mut w = self.desired.0.to_string();
                                    if ui
                                        .add(
                                            egui::TextEdit::singleline(&mut w)
                                                .desired_width(56.0),
                                        )
                                        .changed()
                                    {
                                        if let Ok(v) = w.trim().parse::<u16>() {
                                            self.desired.0 = v.clamp(640, 3840);
                                        }
                                    }
                                    ui.label("×");
                                    let mut h = self.desired.1.to_string();
                                    if ui
                                        .add(
                                            egui::TextEdit::singleline(&mut h)
                                                .desired_width(56.0),
                                        )
                                        .changed()
                                    {
                                        if let Ok(v) = h.trim().parse::<u16>() {
                                            self.desired.1 = v.clamp(480, 2160);
                                        }
                                    }
                                });
                                ui.end_row();
                            });
                    });

                ui.add_space(10.0);
                if let Some(err) = self.error() {
                    ui.colored_label(egui::Color32::from_rgb(230, 120, 120), err);
                    ui.add_space(6.0);
                }
                if self.is_connecting() {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label("正在连接…");
                    });
                } else if ui
                    .add(
                        egui::Button::new(egui::RichText::new("🔌 连接").size(14.0))
                            .min_size(egui::vec2(140.0, 32.0)),
                    )
                    .clicked()
                {
                    self.connect();
                }
            });
        });
    }

    fn forward_input(
        &mut self,
        ui: &mut egui::Ui,
        response: &egui::Response,
        draw_rect: egui::Rect,
    ) {
        if !self.is_active() || self.session.is_none() {
            return;
        }

        // ---- 键盘 ----
        // egui 的 Key 枚举里**没有** Shift/Ctrl/Alt，它们只出现在 Modifiers 中。
        // 之前只靠 Event::ModifiersChanged 补发修饰键，实际很多情况下该事件
        // 不会随按键一起到达 → 远端收不到 Shift，于是大写字母和
        // ! @ # $ 这类符号全都输不进去。
        // 现在改成：每个 Key 事件自带 modifiers，先按事件里的状态补发
        // 缺失的修饰键（保证 Shift 在字母之前按下），帧末再按当前
        // modifiers 补发抬起。
        if self.capture_keys {
            let events = ui.input(|i| i.events.clone());
            let mut last_key_mapped = true;
            for ev in events {
                match ev {
                    egui::Event::Key {
                        key,
                        pressed,
                        modifiers,
                        ..
                    } => {
                        self.apply_mods((modifiers.shift, modifiers.ctrl, modifiers.alt));
                        match remote::map_key(key) {
                            Some((scancode, keysym)) => {
                                // 可打印字符按 Shift 换算真实 keysym：
                                // VNC 远端以 keysym 决定字符，恒用小写基准值会让
                                // 大写字母输成小写、@/# 等上档符号直接丢失。
                                let keysym = remote::printable_keysym(key, modifiers.shift)
                                    .unwrap_or(keysym);
                                self.send(RdCommand::Key(remote::KeyInput {
                                    scancode,
                                    keysym,
                                    ch: None,
                                    down: pressed,
                                }));
                                last_key_mapped = true;
                            }
                            None => {
                                // 没有物理键映射（小键盘 / 输入法）：
                                // 交给 Text 事件走 Unicode 通道
                                last_key_mapped = false;
                            }
                        }
                    }
                    egui::Event::Text(text) => {
                        // 文本兜底：上一个键没有物理映射时用 Unicode 送字符
                        if !last_key_mapped {
                            for ch in text.chars() {
                                let key = remote::KeyInput {
                                    scancode: 0,
                                    // X11 用 0x01000000 + 码点 表示 Unicode 键
                                    keysym: 0x0100_0000 + ch as u32,
                                    ch: Some(ch),
                                    down: true,
                                };
                                self.send(RdCommand::Key(key));
                                // 补一个抬起，否则远端会认为这个键一直按着
                                self.send(RdCommand::Key(remote::KeyInput {
                                    down: false,
                                    ..key
                                }));
                            }
                        }
                        last_key_mapped = true;
                    }
                    _ => {}
                }
            }

            // 帧末：把已经松开的修饰键补发抬起
            let cur = ui.input(|i| (i.modifiers.shift, i.modifiers.ctrl, i.modifiers.alt));
            self.apply_mods(cur);
        }

        // ---- 指针 ----
        if let Some(pos) = response.hover_pos() {
            let rel_x = ((pos.x - draw_rect.min.x) / draw_rect.width())
                .clamp(0.0, 0.999)
                .max(0.0);
            let rel_y = ((pos.y - draw_rect.min.y) / draw_rect.height())
                .clamp(0.0, 0.999)
                .max(0.0);
            let rx = (rel_x * self.frame_width as f32) as u16;
            let ry = (rel_y * self.frame_height as f32) as u16;
            if self.last_pointer != Some((rx, ry)) || self.buttons != 0 {
                self.last_pointer = Some((rx, ry));
                self.send(RdCommand::Pointer {
                    x: rx,
                    y: ry,
                    buttons: self.buttons,
                });
            }
        }

        // 按键状态
        let (primary, secondary, middle) = ui.input(|i| {
            (
                i.pointer.primary_down(),
                i.pointer.secondary_down(),
                i.pointer.middle_down(),
            )
        });
        let mut mask = 0u8;
        if primary {
            mask |= 1;
        }
        if middle {
            mask |= 2;
        }
        if secondary {
            mask |= 4;
        }
        if mask != self.buttons {
            self.buttons = mask;
            if let Some((x, y)) = self.last_pointer {
                self.send(RdCommand::Pointer { x, y, buttons: mask });
            }
        }

        // 滚轮
        if response.hovered() {
            let dy = ui.input(|i| i.smooth_scroll_delta.y);
            if dy.abs() > 0.5 {
                self.send(RdCommand::Wheel {
                    dy: if dy > 0.0 { 1 } else { -1 },
                });
            }
        }
    }
}
