//! 终端核心：alacritty_terminal（VT 状态机）+ egui 绘制 + 键鼠编码。
//!
//! 数据流：
//!   pty/ssh ─ mpsc ─► processor.advance(term) ─► renderable_content() ─► egui Painter
//!   键鼠事件 ─ VT 序列编码 ─► mpsc ─► pty/ssh（Term 产生的查询响应经 PtyProxy 回写）
//!
//! 选区是我们自己的状态（Point 起点/终点），复制文本用
//! `term.bounds_to_string()`，避免依赖 alacritty 的 Selection 类型。

use alacritty_terminal::event::{Event, EventListener};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Line, Point};
use alacritty_terminal::term;
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::color::Colors;
use alacritty_terminal::term::{Term, TermMode};
use anyhow::Result;
use eframe::egui::{self, Color32, Pos2, Vec2};
use tokio::sync::mpsc;
use vte::ansi::Processor;

use crate::backends::{self, TermIo};

const SCROLLBACK: usize = 10_000;
// const FONT_SIZE: f32 = 14.0; // 现在从配置读取

const BG: Color32 = Color32::from_rgb(20, 24, 33);
const SELECTION: Color32 = Color32::from_rgba_unmultiplied_const(58, 110, 190, 100);
const FG_DEFAULT: Color32 = Color32::from_rgb(220, 226, 238);

/// Term → UI 的事件（经 PtyProxy 转发）。
pub enum PaneEvent {
    /// OSC 0/2：窗口标题变更
    Title(String),
    /// OSC 52：终端请求把内容写入系统剪贴板
    ClipboardStore(String),
}

/// Term 尺寸。total_lines = 屏幕行数 + 回滚深度。
#[derive(Clone, Copy, Debug)]
pub struct TermSize {
    pub cols: u16,
    pub rows: u16,
}

impl Dimensions for TermSize {
    fn columns(&self) -> usize {
        self.cols as usize
    }
    fn screen_lines(&self) -> usize {
        self.rows as usize
    }
    fn total_lines(&self) -> usize {
        self.rows as usize + SCROLLBACK
    }
}

/// Term 的事件代理：
/// - `PtyWrite`（设备查询的应答）回写给后端输入通道；
/// - `Title` / `ClipboardStore` 转给 UI（app_tx）；
/// - `ColorRequest` 用内置色板应答；`ClipboardLoad`（OSC52 读）回空串。
pub struct PtyProxy {
    tx: Option<mpsc::UnboundedSender<Vec<u8>>>,
    app_tx: Option<mpsc::UnboundedSender<PaneEvent>>,
}

fn send_pty(tx: &Option<mpsc::UnboundedSender<Vec<u8>>>, text: &str) {
    if let Some(tx) = tx {
        let _ = tx.send(text.as_bytes().to_vec());
    }
}

fn color_request_rgb(index: usize) -> vte::ansi::Rgb {
    // 0..16 命名色 / 16..232 色块 / 232..256 灰阶 / 256+ 特殊色彩
    let c = match index {
        256 => FG_DEFAULT,                 // foreground
        257 => BG,                         // background
        258 => Color32::WHITE,             // cursor
        259..=266 => ANSI16[(index - 259) % 8],
        267 => ANSI16[14],                 // bright foreground（近似）
        268 => BG,                         // dim background
        i if i < 16 => ANSI16[i],
        i => palette_color((i as u8).min(255)),
    };
    vte::ansi::Rgb {
        r: c.r(),
        g: c.g(),
        b: c.b(),
    }
}

impl EventListener for PtyProxy {
    fn send_event(&self, event: Event) {
        match event {
            Event::PtyWrite(text) => send_pty(&self.tx, &text),
            Event::ColorRequest(index, formatter) => {
                send_pty(&self.tx, &formatter(color_request_rgb(index)));
            }
            Event::ClipboardLoad(_typ, formatter) => {
                // OSC52 读剪贴板：P1 不支持读取，回空串避免程序挂起
                send_pty(&self.tx, &formatter(""));
            }
            Event::Title(title) => {
                if let Some(app_tx) = &self.app_tx {
                    let _ = app_tx.send(PaneEvent::Title(title));
                }
            }
            Event::ResetTitle => {
                if let Some(app_tx) = &self.app_tx {
                    let _ = app_tx.send(PaneEvent::Title("终端".to_string()));
                }
            }
            Event::ClipboardStore(_typ, text) => {
                if let Some(app_tx) = &self.app_tx {
                    let _ = app_tx.send(PaneEvent::ClipboardStore(text));
                }
            }
            _ => {}
        }
    }
}

/// 一个终端标签页。
pub struct TermPane {
    pub title: String,
    pub term: Term<PtyProxy>,
    processor: Processor,
    pub dims: TermSize,
    io: TermIo,
    /// 本窗格的启动配置（分屏时按此复制一个新会话）
    pub spawn: SpawnCfg,
    /// 对端断开
    pub dead: bool,
    /// 选区：(起点, 终点)，绝对 Line 坐标
    pub selection: Option<(Point<Line>, Point<Line>)>,
    /// 是否为分屏中的聚焦窗格（由 app 层用于定位分屏/关闭目标）
    pub has_focus: bool,
    /// PaneEvent 接收端（标题/OSC52）
    events_rx: mpsc::UnboundedReceiver<PaneEvent>,
    /// OSC52 存剪贴板暂存（渲染帧内交给 ctx）
    pending_clip: Option<String>,
    /// IME 组合中的预编辑串（空 = 无组合）
    preedit: Option<String>,
    /// 本窗格是否已向系统请求开启 IME
    ime_on: bool,
    /// 查找关键字（空 = 未搜索）
    pub search: String,
    /// 匹配项：(绝对行, 起始列, 结束列)
    pub search_matches: Vec<(i32, usize, usize)>,
    /// 当前定位到的匹配序号
    pub search_index: usize,
    /// 输入镜像通道：开启「同步输入」时，把本窗格输入复制一份出去
    pub mirror: Option<std::sync::mpsc::Sender<Vec<u8>>>,
}

/// 新窗格的启动方式（预留：分屏复制会话时按此重建）。
#[derive(Clone)]
#[allow(dead_code)] // 字段用于记录来源，分屏复制会话时会读取
pub enum SpawnCfg {
    Local,
    Ssh {
        host: String,
        port: u16,
        user: String,
        auth: backends::ssh::SshAuth,
        /// 跳板机（ProxyJump），None = 直连
        jump: Option<backends::ssh::JumpHost>,
    },
    /// 在 PTY 里运行任意命令（Ping / traceroute 等）
    Command {
        program: String,
        args: Vec<String>,
    },
    /// 原始 TCP（Telnet）
    Tcp {
        host: String,
        port: u16,
    },
}

impl TermPane {
    pub fn new_local(title: String, cols: u16, rows: u16) -> Result<Self> {
        let mut pane = Self::new(title, backends::local::spawn(cols, rows)?, cols, rows);
        pane.spawn = SpawnCfg::Local;
        Ok(pane)
    }

    /// 在 PTY 中运行任意命令（Ping 等），复用终端渲染与交互。
    pub fn new_command(
        title: String,
        program: &str,
        args: &[String],
        cols: u16,
        rows: u16,
    ) -> Result<Self> {
        let io = backends::local::spawn_command(program, args, cols, rows)?;
        let mut pane = Self::new(title, io, cols, rows);
        pane.spawn = SpawnCfg::Command {
            program: program.to_string(),
            args: args.to_vec(),
        };
        Ok(pane)
    }

    /// 原始 TCP（Telnet）会话。
    pub fn new_tcp(
        title: String,
        host: String,
        port: u16,
        cols: u16,
        rows: u16,
    ) -> Result<Self> {
        let io = backends::tcp::spawn(&host, port)?;
        let mut pane = Self::new(title, io, cols, rows);
        pane.spawn = SpawnCfg::Tcp {
            host,
            port,
        };
        Ok(pane)
    }

    pub async fn new_ssh(
        title: String,
        host: String,
        port: u16,
        user: String,
        auth: backends::ssh::SshAuth,
        jump: Option<backends::ssh::JumpHost>,
        cols: u16,
        rows: u16,
    ) -> Result<Self> {
        let io = backends::ssh::spawn(
            host.clone(),
            port,
            user.clone(),
            auth.clone(),
            jump.clone(),
            cols,
            rows,
        )
        .await?;
        let mut pane = Self::new(title, io, cols, rows);
        pane.spawn = SpawnCfg::Ssh { host, port, user, auth, jump };
        Ok(pane)
    }

    fn new(title: String, io: TermIo, cols: u16, rows: u16) -> Self {
        let dims = TermSize { cols, rows };
        let (events_tx, events_rx) = mpsc::unbounded_channel::<PaneEvent>();
        let proxy = PtyProxy {
            tx: Some(io.input_tx.clone()),
            app_tx: Some(events_tx),
        };
        let term = Term::new(term::Config::default(), &dims, proxy);
        Self {
            title,
            term,
            processor: Processor::new(),
            dims,
            io,
            spawn: SpawnCfg::Local,
            dead: false,
            selection: None,
            has_focus: false,
            events_rx,
            pending_clip: None,
            preedit: None,
            ime_on: false,
            search: String::new(),
            search_matches: Vec::new(),
            search_index: 0,
            mirror: None,
        }
    }

    /// 吸收后端字节流，返回 true 表示有新内容。
    pub fn pump(&mut self) -> bool {
        let mut dirty = false;
        let mut budget = 16; // 每帧最多 16 批，避免大吞吐打满 UI
        loop {
            if budget == 0 {
                break;
            }
            match self.io.output_rx.try_recv() {
                Ok(data) => {
                    self.processor.advance(&mut self.term, &data);
                    dirty = true;
                    budget -= 1;
                }
                Err(mpsc::error::TryRecvError::Empty) => break,
                Err(mpsc::error::TryRecvError::Disconnected) => {
                    self.dead = true;
                    return true;
                }
            }
        }
        dirty
    }

    /// 吸收 PaneEvent（标题 / OSC52）。
    fn drain_pane_events(&mut self) -> bool {
        let mut changed = false;
        while let Ok(ev) = self.events_rx.try_recv() {
            match ev {
                PaneEvent::Title(t) => {
                    self.title = t;
                    changed = true;
                }
                PaneEvent::ClipboardStore(text) => {
                    self.pending_clip = Some(text);
                }
            }
        }
        changed
    }

    /// 终端 resize（含通知后端），返回是否发生变化。
    pub fn resize(&mut self, cols: u16, rows: u16) -> bool {
        if cols < 2 || rows < 2 || (self.dims.cols, self.dims.rows) == (cols, rows) {
            return false;
        }
        self.dims = TermSize { cols, rows };
        self.term.resize(self.dims);
        let _ = self.io.resize_tx.send((cols, rows));
        true
    }

    /// SSH 会话句柄（本地 / Telnet 窗格返回 None），供 SFTP 面板复用连接。
    pub fn ssh_handle(&self) -> Option<std::sync::Arc<backends::ssh::SshHandle>> {
        self.io.ssh.clone()
    }

    pub fn write_input(&self, bytes: Vec<u8>) {
        // 同步输入：先复制给其它窗格，再写本窗格
        if let Some(mx) = &self.mirror {
            let _ = mx.send(bytes.clone());
        }
        let _ = self.io.input_tx.send(bytes);
    }

    /// 只写本窗格（广播回放时用，避免二次镜像造成回环）。
    pub fn write_input_local(&self, bytes: Vec<u8>) {
        let _ = self.io.input_tx.send(bytes);
    }

    // ------------------------------------------------------------------
    // 查找
    // ------------------------------------------------------------------

    /// 设置查找关键字并重新计算匹配项（大小写不敏感）。
    pub fn set_search(&mut self, needle: &str) {
        self.search = needle.to_string();
        self.search_matches.clear();
        self.search_index = 0;
        let needle = needle.trim().to_lowercase();
        if needle.is_empty() {
            return;
        }
        let grid = self.term.grid();
        let top = grid.topmost_line().0;
        let bottom = grid.bottommost_line().0;
        for line in top..=bottom {
            if let Some((start, end)) = self.find_in_line(Line(line), &needle) {
                self.search_matches.push((line, start, end));
            }
        }
        // 默认定位到最靠近屏幕底部（最新）的一处
        if !self.search_matches.is_empty() {
            self.search_index = self.search_matches.len() - 1;
            self.scroll_to_search_match();
        }
    }

    /// 在当前行内查找，返回 (起始列, 结束列)——按列而非字符索引。
    fn find_in_line(&self, line: Line, needle: &str) -> Option<(usize, usize)> {
        let grid = self.term.grid();
        let mut text = String::new();
        // char 序号 → 列号映射（跳过双宽字符的占位格）
        let mut col_of_char: Vec<usize> = Vec::new();
        for col in 0..self.dims.cols as usize {
            let cd = &grid[line][Column(col)];
            if cd.flags.contains(Flags::WIDE_CHAR_SPACER) {
                continue;
            }
            text.push(cd.c);
            col_of_char.push(col);
        }
        let lower = text.to_lowercase();
        // 用字符级查找（to_lowercase 可能改变长度，这里对 ASCII 场景安全）
        let pos = lower.find(needle)?;
        let char_start = text[..pos].chars().count();
        let char_len = needle.chars().count();
        let char_end = (char_start + char_len).min(col_of_char.len());
        if char_start >= col_of_char.len() {
            return None;
        }
        let start_col = col_of_char[char_start];
        let end_col = col_of_char
            .get(char_end.saturating_sub(1))
            .copied()
            .unwrap_or(start_col);
        Some((start_col, end_col))
    }

    /// 把视口滚动到当前匹配项。
    pub fn scroll_to_search_match(&mut self) {
        let Some(&(line, _, _)) = self.search_matches.get(self.search_index) else {
            return;
        };
        let content = self.term.renderable_content();
        let offset = content.display_offset as i32;
        drop(content);
        // 视口底行绝对行号
        let bottom_line = self.dims.rows as i32 - 1 - offset;
        // 目标行与视口中线的差
        let mid = self.dims.rows as i32 / 2;
        let delta = line - (bottom_line - mid);
        if delta != 0 {
            self.term
                .scroll_display(Scroll::Delta(-delta));
        }
    }

    /// 下一个 / 上一个匹配项。
    pub fn search_next(&mut self) {
        if self.search_matches.is_empty() {
            return;
        }
        self.search_index = (self.search_index + 1) % self.search_matches.len();
        self.scroll_to_search_match();
    }

    pub fn search_prev(&mut self) {
        if self.search_matches.is_empty() {
            return;
        }
        self.search_index = if self.search_index == 0 {
            self.search_matches.len() - 1
        } else {
            self.search_index - 1
        };
        self.scroll_to_search_match();
    }

    pub fn clear_search(&mut self) {
        self.search.clear();
        self.search_matches.clear();
        self.search_index = 0;
    }

    pub fn selection_text(&self) -> Option<String> {
        let (a, b) = normalized_selection(&self.selection)?;
        Some(self.term.bounds_to_string(a, b))
    }

    #[allow(dead_code)] // 后续清除选区用
    pub fn clear_selection(&mut self) {
        self.selection = None;
    }

    /// 取终端最近若干行可见文本（供 AI 上下文使用）。
    ///
    /// - `max_lines`：最多取多少行（从当前视口底行往上数）
    /// - `max_line_len`：每行最多保留多少字符（超出截断并加省略号）
    pub fn context_text(&self, max_lines: usize, max_line_len: usize) -> String {
        if max_lines == 0 {
            return String::new();
        }
        let content = self.term.renderable_content();
        let display_offset = content.display_offset;
        drop(content);

        let grid = self.term.grid();
        // 视口底行（display_offset > 0 时视口上移，绝对行号变小）
        // 与 render() 中 `Line(row - display_offset)` 的取行方式保持一致。
        let bottom = self.dims.rows as i32 - 1 - display_offset as i32;
        let topmost = grid.topmost_line().0;
        let mut lines: Vec<String> = Vec::new();
        for i in 0..max_lines as i32 {
            let row = bottom - i;
            if row < topmost {
                break;
            }
            let text = self.line_text(Line(row), max_line_len);
            if !text.trim().is_empty() {
                lines.push(text);
            }
        }
        lines.reverse();
        lines.join("\n")
    }

    /// 取某一绝对行的文本（按 max_len 截断）。
    fn line_text(&self, line: Line, max_len: usize) -> String {
        let grid = self.term.grid();
        let mut s = String::new();
        for col in 0..self.dims.cols as usize {
            let cd = &grid[line][Column(col)];
            if cd.flags.contains(Flags::WIDE_CHAR_SPACER) {
                continue;
            }
            s.push(cd.c);
        }
        let trimmed = s.trim_end().to_string();
        if trimmed.chars().count() > max_len {
            let head: String = trimmed.chars().take(max_len).collect();
            format!("{head}…")
        } else {
            trimmed
        }
    }

    /// 渲染标签页内容。返回本轮是否有新字节（或标题变化）。
    pub fn render(&mut self, ctx: &egui::Context, ui: &mut egui::Ui, available: Vec2, font_size: f32) -> bool {
        let title_changed = self.drain_pane_events();
        let dirty = self.pump();

        // ---- 单元格尺寸：按等宽字体测量 ----
        // 的宽度做基准，"中" 为 2 格宽的对照（CJK 双宽）。
        let font_id = egui::FontId::monospace(font_size);
        let (w, h) = ui.painter().layout_no_wrap("W".to_string(), font_id.clone(), Color32::WHITE)
            .rect
            .size()
            .into();
        let cell = Vec2::new(w, h + 1.0); // +1 视觉行距

        let cols = ((available.x / cell.x).floor() as u16).max(4);
        let rows = ((available.y / cell.y).floor() as u16).max(3);
        self.resize(cols, rows);

        let (rect, response) = ui.allocate_exact_size(available, egui::Sense::click_and_drag());
        ui.painter().rect_filled(rect, 0.0, BG);

        if response.hovered() {
            ui.output_mut(|o| o.cursor_icon = egui::CursorIcon::Text);
        }
        if response.clicked() {
            response.request_focus();
        }

        let origin = rect.min;

        // ---- 内容快照（复制出需要的坐标，避免长期借用 self.term）----
        let content = self.term.renderable_content();
        let cursor = content.cursor;
        let display_offset = content.display_offset;
        let mode: TermMode = content.mode;
        let sel = normalized_selection(&self.selection);
        let cols_i = self.dims.cols;
        let rows_i = self.dims.rows;
        let colors: Colors = *content.colors;
        // content 借用到此结束
        drop(content);

        let painter = ui.painter();

        // ---- 选区高亮 ----
        if let Some((a, b)) = sel {
            for line in a.line.0..=b.line.0 {
                let row = line + display_offset as i32;
                if row < 0 || row >= rows_i as i32 {
                    continue;
                }
                let col_start = if line == a.line.0 { a.column.0 } else { 0 };
                let col_end = if line == b.line.0 {
                    (b.column.0 + 1).min(cols_i as usize)
                } else {
                    cols_i as usize
                };
                if col_start >= col_end {
                    continue;
                }
                let p0 = origin + Vec2::new(col_start as f32 * cell.x, row as f32 * cell.y);
                let p1 = p0 + Vec2::new((col_end - col_start) as f32 * cell.x, cell.y);
                painter.rect_filled(egui::Rect::from_two_pos(p0, p1), 1.0, SELECTION);
            }
        }

        // ---- 网格绘制 ----
        for row in 0..rows_i as i32 {
            let line = Line(row - display_offset as i32);
            for col in 0..cols_i as usize {
                let cd = &self.term.grid()[line][Column(col)];
                let flags = cd.flags;

                if flags.contains(Flags::WIDE_CHAR_SPACER) {
                    continue; // 双宽字符的后半格
                }

                let wide = flags.contains(Flags::WIDE_CHAR);
                let span = if wide { 2.0 } else { 1.0 };
                let mut fg = resolve_color(cd.fg, &colors, true, flags);
                let mut bg = resolve_color(cd.bg, &colors, false, flags);
                if flags.contains(Flags::INVERSE) {
                    std::mem::swap(&mut fg, &mut bg);
                }

                let empty_glyph = cd.c == ' ' && cd.zerowidth().is_none();
                if bg != BG {
                    let p0 = origin + Vec2::new(col as f32 * cell.x, row as f32 * cell.y);
                    let p1 = p0 + Vec2::new(span * cell.x, cell.y);
                    painter.rect_filled(egui::Rect::from_two_pos(p0, p1), 0.0, bg);
                }

                if !empty_glyph {
                    let mut s: String = cd.c.into();
                    if let Some(zw) = cd.zerowidth() {
                        for z in zw.iter() {
                            s.push(*z);
                        }
                    }
                    let pos = origin + Vec2::new(col as f32 * cell.x, row as f32 * cell.y);
                    painter.text(pos, egui::Align2::LEFT_TOP, &s, font_id.clone(), fg);
                }

                if flags.contains(Flags::UNDERLINE) {
                    let y = origin.y + row as f32 * cell.y + cell.y - 1.5;
                    let x0 = origin.x + col as f32 * cell.x;
                    painter.line_segment(
                        [Pos2::new(x0, y), Pos2::new(x0 + span * cell.x, y)],
                        egui::Stroke::new(1.0, fg),
                    );
                }

                // 宽字符的 spacer 格由循环头部的 WIDE_CHAR_SPACER continue 跳过
            }
        }

        // ---- IME：聚焦时允许输入法，候选窗贴游标 ----
        let focused = response.has_focus();
        self.has_focus = focused;
        if focused && !self.ime_on {
            ctx.send_viewport_cmd(egui::ViewportCommand::IMEAllowed(true));
            ctx.send_viewport_cmd(egui::ViewportCommand::IMEPurpose(egui::IMEPurpose::Terminal));
            self.ime_on = true;
        } else if !focused && self.ime_on {
            ctx.send_viewport_cmd(egui::ViewportCommand::IMEAllowed(false));
            self.ime_on = false;
        }

        // ---- 查找高亮 ----
        if !self.search_matches.is_empty() {
            let current = self.search_matches.get(self.search_index).copied();
            for (idx, &(line, c0, c1)) in self.search_matches.iter().enumerate() {
                let row = line + display_offset as i32;
                if row < 0 || row >= rows_i as i32 {
                    continue;
                }
                let is_current = current.map(|c| c.0 == line).unwrap_or(false)
                    && idx == self.search_index;
                let color = if is_current {
                    Color32::from_rgba_unmultiplied(255, 190, 60, 120)
                } else {
                    Color32::from_rgba_unmultiplied(150, 150, 60, 70)
                };
                let p0 = origin + Vec2::new(c0 as f32 * cell.x, row as f32 * cell.y);
                let p1 = p0 + Vec2::new(((c1 - c0 + 1) as f32) * cell.x, cell.y);
                painter.rect_filled(egui::Rect::from_two_pos(p0, p1), 1.0, color);
            }
            // 高亮画在文字下方：重绘这一行的文字代价太高，
            // 这里改用半透明叠加，文字仍然可读。
        }

        // ---- 游标 ----
        let cursor_row = cursor.point.line.0 + display_offset as i32;
        let cursor_screen_ok = cursor_row >= 0 && cursor_row < rows_i as i32;
        if cursor_screen_ok {
            let cx = origin.x + cursor.point.column.0 as f32 * cell.x;
            let cy = origin.y + cursor_row as f32 * cell.y;
            let selected_color = if focused {
                Color32::from_rgba_unmultiplied(230, 235, 245, 235)
            } else {
                Color32::from_rgba_unmultiplied(170, 170, 170, 90)
            };
            match cursor.shape {
                vte::ansi::CursorShape::Hidden => {}
                vte::ansi::CursorShape::Block => {
                    let p0 = Pos2::new(cx, cy);
                    let p1 = Pos2::new(cx + cell.x, cy + cell.y);
                    painter.rect_filled(
                        egui::Rect::from_two_pos(p0, p1),
                        0.0,
                        Color32::from_rgba_unmultiplied(220, 228, 245, 70),
                    );
                    painter.rect_stroke(
                        egui::Rect::from_two_pos(p0, p1),
                        0.0,
                        egui::Stroke::new(1.0, selected_color),
                        egui::StrokeKind::Inside,
                    );
                }
                vte::ansi::CursorShape::HollowBlock => {
                    let p0 = Pos2::new(cx, cy);
                    let p1 = Pos2::new(cx + cell.x, cy + cell.y);
                    painter.rect_stroke(
                        egui::Rect::from_two_pos(p0, p1),
                        0.0,
                        egui::Stroke::new(1.5, selected_color),
                        egui::StrokeKind::Inside,
                    );
                }
                vte::ansi::CursorShape::Beam => {
                    let p0 = Pos2::new(cx, cy);
                    let p1 = Pos2::new(cx + cell.x * 0.18, cy + cell.y);
                    painter.rect_filled(egui::Rect::from_two_pos(p0, p1), 0.5, selected_color);
                }
                vte::ansi::CursorShape::Underline => {
                    let p0 = Pos2::new(cx, cy + cell.y * 0.78);
                    let p1 = Pos2::new(cx + cell.x, cy + cell.y);
                    painter.rect_filled(egui::Rect::from_two_pos(p0, p1), 1.0, selected_color);
                }
            }

            // ---- IME 预编辑串（组合态）----
            if focused {
                if let Some(p) = &self.preedit {
                    let p0 = Pos2::new(cx, cy);
                    let p1 = Pos2::new(cx + cell.x, cy + cell.y);
                    painter.rect_filled(
                        egui::Rect::from_two_pos(p0, p1),
                        0.0,
                        Color32::from_rgba_unmultiplied(70, 90, 140, 160),
                    );
                    painter.text(p0, egui::Align2::LEFT_TOP, p, font_id.clone(), FG_DEFAULT);
                }
            }
        }

        // ---- 输入 ----
        self.handle_input(ctx, &response, mode, display_offset, &rect, cell, cursor.point);

        title_changed || dirty
    }

    fn handle_input(
        &mut self,
        ctx: &egui::Context,
        response: &egui::Response,
        mode: TermMode,
        display_offset: usize,
        rect: &egui::Rect,
        cell: Vec2,
        cursor_point: Point<Line>,
    ) {
        let origin = rect.min;
        let focused = response.has_focus();
        let mouse_mode = mode.intersects(
            TermMode::MOUSE_REPORT_CLICK | TermMode::MOUSE_DRAG | TermMode::MOUSE_MOTION,
        );

        // OSC52 存剪贴板（经事件通道暂存）
        if let Some(text) = self.pending_clip.take() {
            ctx.copy_text(text);
        }

        let events = ctx.input(|i| i.events.clone());
        for event in events {
            match event {
                egui::Event::Key {
                    key, pressed, modifiers, ..
                } if pressed && focused => {
                    self.scroll_to_bottom(display_offset);
                    if let Some(bytes) = self.encode_key(ctx, key, modifiers) {
                        self.write_input(bytes);
                    }
                }
                egui::Event::Text(text) if focused => {
                    self.scroll_to_bottom(display_offset);
                    self.write_input(text.into_bytes());
                }
                egui::Event::Ime(egui::ImeEvent::Preedit { text, .. }) if focused => {
                    self.preedit = (!text.is_empty()).then_some(text);
                }
                egui::Event::Ime(egui::ImeEvent::Commit(text)) if focused => {
                    self.preedit = None;
                    self.scroll_to_bottom(display_offset);
                    self.write_input(text.into_bytes());
                }
                egui::Event::Ime(egui::ImeEvent::DeleteSurrounding { .. }) => {
                    // 终端行编辑在行内无前向删除概念，忽略
                }
                egui::Event::MouseWheel {
                    delta, modifiers, ..
                } => {
                    let up = delta.y > 0.0;
                    if mouse_mode {
                        let button = if up { 64 } else { 65 };
                        let pos = ctx
                            .input(|i| i.pointer.latest_pos())
                            .unwrap_or(rect.center());
                        self.mouse_report(modifiers, origin, cell, button, pos, true);
                    } else if mode.contains(TermMode::ALT_SCREEN) {
                        // alternate scroll：换 ↑/↓
                        let seq: &[u8] = if up { b"\x1b[A" } else { b"\x1b[B" };
                        for _ in 0..3 {
                            self.write_input(seq.to_vec());
                        }
                    } else {
                        let d: i32 = if up { 3 } else { -3 };
                        self.term.scroll_display(Scroll::Delta(d));
                    }
                }
                egui::Event::PointerButton {
                    pos,
                    button,
                    pressed,
                    modifiers,
                    ..
                } => {
                    if mouse_mode {
                        let b = match button {
                            egui::PointerButton::Primary => 0,
                            egui::PointerButton::Secondary => 2,
                            egui::PointerButton::Middle => 1,
                            _ => 3,
                        };
                        self.mouse_report(modifiers, origin, cell, b, pos, pressed);
                    } else if button == egui::PointerButton::Primary {
                        let (point, _, _) = self.pointer_cell_at(pos, origin, cell, display_offset);
                        if pressed {
                            self.selection = Some((point, point));
                        } else {
                            self.selection = self.selection.map(|(a, _)| (a, point));
                        }
                    }
                }
                egui::Event::PointerMoved(pos) => {
                    if response.dragged_by(egui::PointerButton::Primary) && !mouse_mode && focused {
                        let (point, _, _) = self.pointer_cell_at(pos, origin, cell, display_offset);
                        self.selection = self.selection.map(|(a, _)| (a, point));
                    } else if mouse_mode && mode.intersects(TermMode::MOUSE_DRAG) {
                        self.mouse_report(
                            egui::Modifiers::default(),
                            origin,
                            cell,
                            32,
                            pos,
                            true,
                        );
                    }
                }
                egui::Event::Copy if focused => {
                    // egui_winit 拦截了 Ctrl+C（不再转发 Key 事件）：
                    // 有选区 → 复制选区；无选区 → 发 ^C（SIGINT）
                    match self.selection_text() {
                        Some(text) if !text.is_empty() => ctx.copy_text(text),
                        _ => self.write_input(vec![0x03]),
                    }
                }
                egui::Event::Cut if focused => {
                    // Ctrl+X 被拦截：无 egui 选区时终端语义是 ^X
                    match self.selection_text() {
                        Some(text) if !text.is_empty() => ctx.copy_text(text),
                        _ => self.write_input(vec![0x18]),
                    }
                }
                egui::Event::Paste(text) if focused => {
                    self.scroll_to_bottom(display_offset);
                    self.write_input(text.into_bytes());
                }
                _ => {}
            }
        }

        // IME 候选窗跟随游标（每帧校正一次代价可忽略）
        if focused {
            let cursor_row = cursor_point.line.0 + display_offset as i32;
            if (0..self.dims.rows as i32).contains(&cursor_row) {
                let x = origin.x + cursor_point.column.0 as f32 * cell.x;
                let y = origin.y + cursor_row as f32 * cell.y;
                ctx.send_viewport_cmd(egui::ViewportCommand::IMERect(
                    egui::Rect::from_min_size(
                        Pos2::new(x, y + cell.y),
                        Vec2::new(cell.x.max(60.0), cell.y),
                    ),
                ));
            }
        }
    }

    /// 键入时若停在回滚历史之上，自动跳回底部（xterm 习惯）。
    fn scroll_to_bottom(&mut self, display_offset: usize) {
        if display_offset > 0 {
            self.term.scroll_display(Scroll::Bottom);
        }
    }

    /// 键盘 → VT 字节。返回 None 表示该事件被 UI 功能吃掉（复制/粘贴）。
    /// 注意：可打印字符（含 AltGr/中文输入）走集成层的 `Event::Text`，
    /// 这里只处理命名键与 Ctrl 控制码。
    fn encode_key(
        &mut self,
        ctx: &egui::Context,
        key: egui::Key,
        mods: egui::Modifiers,
    ) -> Option<Vec<u8>> {
        // ---- Ctrl 修饰键特殊语义 ----
        if mods.ctrl && !mods.alt {
            // Ctrl+Shift+C / Ctrl+Insert：复制选区（不发 ^C，避免误杀进程）
            if (mods.shift && key == egui::Key::C) || (key == egui::Key::Insert && !mods.shift) {
                if let Some(text) = self.selection_text() {
                    if !text.is_empty() {
                        ctx.copy_text(text);
                    }
                }
                return None;
            }
            // Ctrl+V / Shift+Insert：粘贴由集成层 Event::Paste 完成
            if key == egui::Key::V || (key == egui::Key::Insert && mods.shift) {
                return None;
            }
            // Ctrl+字母 → 控制字符 ^code
            if let Some(code) = ctrl_code(key) {
                return Some(vec![code]);
            }
        }

        // ---- 命名键 → CSI 序列 ----
        let mod_param = 1
            + (mods.shift as i32)
            + ((mods.alt as i32) << 1)
            + ((mods.ctrl as i32) << 2);
        let mod_suffix = if mod_param > 1 {
            format!(";{mod_param}")
        } else {
            String::new()
        };

        // 带字母的 CSI 键（A-D/H/F）
        if let Some(csi_char) = match key {
            egui::Key::ArrowUp => Some(if mod_param > 1 { 'a' } else { 'A' }),
            egui::Key::ArrowDown => Some(if mod_param > 1 { 'b' } else { 'B' }),
            egui::Key::ArrowRight => Some(if mod_param > 1 { 'c' } else { 'C' }),
            egui::Key::ArrowLeft => Some(if mod_param > 1 { 'd' } else { 'D' }),
            egui::Key::Home => Some(if mod_param > 1 { 'h' } else { 'H' }),
            egui::Key::End => Some(if mod_param > 1 { 'f' } else { 'F' }),
            _ => None,
        } {
            use std::fmt::Write as _;
            let mut seq = String::from("\x1b[");
            if mod_param > 1 {
                let _ = write!(seq, "1{mod_suffix}");
            }
            seq.push(csi_char);
            return Some(seq.into_bytes());
        }

        // ~ 结尾的 CSI 键
        let csi_tilde: Option<u32> = match key {
            egui::Key::Insert => Some(2),
            egui::Key::Delete => Some(3),
            egui::Key::PageUp => Some(5),
            egui::Key::PageDown => Some(6),
            egui::Key::F5 => Some(15),
            egui::Key::F6 => Some(17),
            egui::Key::F7 => Some(18),
            egui::Key::F8 => Some(19),
            egui::Key::F9 => Some(20),
            egui::Key::F10 => Some(21),
            egui::Key::F11 => Some(23),
            egui::Key::F12 => Some(24),
            _ => None,
        };
        if let Some(n) = csi_tilde {
            use std::fmt::Write as _;
            let mut seq = String::from("\x1b[");
            let _ = write!(seq, "{n}{mod_suffix}~");
            return Some(seq.into_bytes());
        }
        // F1-F4（SS 前缀）
        if mod_param == 1 {
            match key {
                egui::Key::F1 => return Some(b"\x1bOP".to_vec()),
                egui::Key::F2 => return Some(b"\x1bOQ".to_vec()),
                egui::Key::F3 => return Some(b"\x1bOR".to_vec()),
                egui::Key::F4 => return Some(b"\x1bOS".to_vec()),
                _ => {}
            }
        }

        match key {
            egui::Key::Enter => Some(b"\r".to_vec()),
            egui::Key::Tab => Some(b"\t".to_vec()),
            egui::Key::Backspace => Some(vec![0x7f]),
            egui::Key::Escape => Some(b"\x1b".to_vec()),
            _ => None,
        }
    }

    /// pos → (绝对 Line 起点, 视口行)
    fn pointer_cell_at(
        &self,
        pos: Pos2,
        origin: Pos2,
        cell: Vec2,
        display_offset: usize,
    ) -> (Point<Line>, usize, i32) {
        let col = (((pos.x - origin.x) / cell.x).floor().max(0.0) as usize)
            .min(self.dims.cols as usize - 1);
        let row = (((pos.y - origin.y) / cell.y).floor().max(0.0) as i32)
            .min(self.dims.rows as i32 - 1);
        let line = Line(row - display_offset as i32);
        (Point::new(line, Column(col)), col, row)
    }

    /// SGR(1006) 鼠标报告：positions 为视口坐标（1-based）。
    fn mouse_report(
        &mut self,
        mods: egui::Modifiers,
        origin: Pos2,
        cell: Vec2,
        button: u8,
        pos: Pos2,
        pressed: bool,
    ) {
        let (_, col, row) = self.pointer_cell_at(pos, origin, cell, 0);
        let mod_extra = (mods.shift as u8) | ((mods.alt as u8) << 1) | ((mods.ctrl as u8) << 2);
        let code = button + mod_extra * 4;
        let action = if pressed { 'M' } else { 'm' };
        let seq = format!("\x1b[<{code};{};{}{}", col + 1, row + 1, action);
        self.write_input(seq.into_bytes());
    }
}

/// 归一化选区（保证 a 在 b 之前）。
pub fn normalized_selection(
    sel: &Option<(Point<Line>, Point<Line>)>,
) -> Option<(Point<Line>, Point<Line>)> {
    let (a, b) = sel.clone()?;
    let (la, ca) = (a.line.0, a.column.0);
    let (lb, cb) = (b.line.0, b.column.0);
    if (la, ca) <= (lb, cb) {
        Some((a, b))
    } else {
        Some((b, a))
    }
}

/// Ctrl+字母 → 控制码（^A=1 … ^Z=26）。
fn ctrl_code(key: egui::Key) -> Option<u8> {
    use egui::Key;
    let letter = match key {
        Key::A => 'A',
        Key::B => 'B',
        Key::C => 'C',
        Key::D => 'D',
        Key::E => 'E',
        Key::F => 'F',
        Key::G => 'G',
        Key::H => 'H',
        Key::I => 'I',
        Key::J => 'J',
        Key::K => 'K',
        Key::L => 'L',
        Key::M => 'M',
        Key::N => 'N',
        Key::O => 'O',
        Key::P => 'P',
        Key::Q => 'Q',
        Key::R => 'R',
        Key::S => 'S',
        Key::T => 'T',
        Key::U => 'U',
        Key::V => 'V',
        Key::W => 'W',
        Key::X => 'X',
        Key::Y => 'Y',
        Key::Z => 'Z',
        _ => return None,
    };
    Some(letter as u8 & 0x1f)
}

/* ---- 颜色 ---- */

const ANSI16: [Color32; 16] = [
    Color32::from_rgb(30, 30, 34),    // black
    Color32::from_rgb(234, 78, 102),  // red
    Color32::from_rgb(69, 197, 112),  // green
    Color32::from_rgb(238, 190, 70),  // yellow
    Color32::from_rgb(86, 143, 238),  // blue
    Color32::from_rgb(197, 108, 234), // magenta
    Color32::from_rgb(72, 202, 224),  // cyan
    Color32::from_rgb(200, 208, 220), // white
    Color32::from_rgb(90, 96, 110),   // bright black
    Color32::from_rgb(255, 120, 140),
    Color32::from_rgb(120, 240, 160),
    Color32::from_rgb(255, 220, 120),
    Color32::from_rgb(130, 180, 255),
    Color32::from_rgb(230, 160, 255),
    Color32::from_rgb(130, 240, 255),
    Color32::from_rgb(240, 244, 252),
];

fn named_color(n: vte::ansi::NamedColor) -> Color32 {
    use vte::ansi::NamedColor;
    let i = match n {
        NamedColor::Black | NamedColor::DimBlack => 0,
        NamedColor::Red | NamedColor::DimRed => 1,
        NamedColor::Green | NamedColor::DimGreen => 2,
        NamedColor::Yellow | NamedColor::DimYellow => 3,
        NamedColor::Blue | NamedColor::DimBlue => 4,
        NamedColor::Magenta | NamedColor::DimMagenta => 5,
        NamedColor::Cyan | NamedColor::DimCyan => 6,
        NamedColor::White | NamedColor::DimWhite => 7,
        NamedColor::BrightBlack => 8,
        NamedColor::BrightRed => 9,
        NamedColor::BrightGreen => 10,
        NamedColor::BrightYellow => 11,
        NamedColor::BrightBlue => 12,
        NamedColor::BrightMagenta => 13,
        NamedColor::BrightCyan => 14,
        NamedColor::BrightWhite | NamedColor::BrightForeground => 15,
        NamedColor::Foreground => return FG_DEFAULT,
        NamedColor::Background => return BG,
        NamedColor::Cursor => return Color32::from_rgb(240, 244, 252),
        NamedColor::DimForeground => return FG_DEFAULT.gamma_multiply(0.5),
    };
    ANSI16[i]
}

/// xterm 256 色表（内置静态色板，P0 不接 term.colors() 动态调色）。
fn palette_color(i: u8) -> Color32 {
    if i < 16 {
        return ANSI16[i as usize];
    }
    if i < 232 {
        let i = i - 16;
        let [r, g, b] = [((i / 36) % 6) as u8, ((i / 6) % 6) as u8, (i % 6) as u8];
        let ch = |v: u8| -> u8 {
            if v == 0 {
                0
            } else {
                v * 40 + 55
            }
        };
        return Color32::from_rgb(ch(r), ch(g), ch(b));
    }
    let v = 8 + (i - 232) * 10;
    Color32::from_rgb(v, v, v)
}

fn resolve_color(c: vte::ansi::Color, colors: &Colors, is_fg: bool, flags: Flags) -> Color32 {
    // OSC10/11/256 改色后的动态覆盖优先，未覆盖时才落回内置色板
    let base = match c {
        vte::ansi::Color::Spec(rgb) => Color32::from_rgb(rgb.r, rgb.g, rgb.b),
        vte::ansi::Color::Indexed(i) => match colors[i as usize] {
            Some(rgb) => Color32::from_rgb(rgb.r, rgb.g, rgb.b),
            None => palette_color(i),
        },
        vte::ansi::Color::Named(named) => match colors[named] {
            Some(rgb) => Color32::from_rgb(rgb.r, rgb.g, rgb.b),
            None => match named {
                vte::ansi::NamedColor::Foreground => {
                    if is_fg {
                        FG_DEFAULT
                    } else {
                        FG_DEFAULT
                    }
                }
                vte::ansi::NamedColor::Background => BG,
                other => named_color(other),
            },
        },
    };
    // BOLD 前景提亮 +8；DIM 减半
    if is_fg && flags.contains(Flags::BOLD) {
        if let vte::ansi::Color::Indexed(i) = c {
            if i < 8 {
                return palette_color(i + 8);
            }
        }
    }
    if flags.contains(Flags::DIM) {
        Color32::from_rgba_unmultiplied(
            base.r() / 2,
            base.g() / 2,
            base.b() / 2,
            base.a(),
        )
    } else {
        base
    }
}
