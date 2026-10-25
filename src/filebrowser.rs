//! 本地文件浏览器（单栏，真实文件系统）。
//!
//! 同时服务两处：
//! - 右侧「SFTP」停靠面板（窄栏）
//! - 「本地文件」主标签页（宽栏）
//!
//! 只做本地文件系统浏览，不伪造远程目录：远程 SFTP 需要 SSH 会话里的
//! sftp 子系统，尚未接入时不显示假数据。

use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub struct FileEntry {
    pub name: String,
    pub path: PathBuf,
    pub is_dir: bool,
    pub size: u64,
    pub modified: Option<String>,
}

impl FileEntry {
    pub fn size_label(&self) -> String {
        if self.is_dir {
            String::new()
        } else {
            format_size(self.size)
        }
    }
}

pub struct FileBrowser {
    pub path: PathBuf,
    /// 路径栏可编辑文本（绑定临时值会导致输入被丢弃）
    pub path_edit: String,
    pub entries: Vec<FileEntry>,
    pub selected: Option<usize>,
    pub show_hidden: bool,
    /// 是否已加载过一次（首帧自动填目录）
    loaded: bool,
    /// 状态行（错误提示）
    pub status: Option<String>,
}

impl Default for FileBrowser {
    fn default() -> Self {
        Self::new(dirs::home_dir().unwrap_or_else(|| PathBuf::from(".")))
    }
}

impl FileBrowser {
    pub fn new(path: PathBuf) -> Self {
        let mut me = Self {
            path_edit: path.to_string_lossy().to_string(),
            path,
            entries: Vec::new(),
            selected: None,
            show_hidden: false,
            loaded: false,
            status: None,
        };
        me.refresh();
        me
    }

    /// 重新读取当前目录。
    pub fn refresh(&mut self) {
        self.entries.clear();
        self.selected = None;
        self.path_edit = self.path.to_string_lossy().to_string();
        self.status = None;

        let rd = match std::fs::read_dir(&self.path) {
            Ok(rd) => rd,
            Err(e) => {
                self.status = Some(format!("无法读取 {}: {e}", self.path.display()));
                return;
            }
        };

        let mut list: Vec<FileEntry> = Vec::new();
        for entry in rd.filter_map(|e| e.ok()) {
            let name = entry.file_name().to_string_lossy().to_string();
            let meta = entry.metadata().ok();
            let is_dir = meta.as_ref().map(|m| m.is_dir()).unwrap_or(false);
            let size = meta.as_ref().map(|m| m.len()).unwrap_or(0);
            let modified = meta
                .as_ref()
                .and_then(|m| m.modified().ok())
                .map(|t| {
                    let dt: chrono::DateTime<chrono::Local> = t.into();
                    dt.format("%Y-%m-%d %H:%M").to_string()
                });
            let hidden = is_hidden(&name, entry.path().as_path());
            if hidden && !self.show_hidden {
                continue;
            }
            list.push(FileEntry {
                name,
                path: entry.path(),
                is_dir,
                size,
                modified,
            });
        }

        // 目录优先 + 名称自然序（忽略大小写）
        list.sort_by(|a, b| match (a.is_dir, b.is_dir) {
            (true, false) => std::cmp::Ordering::Less,
            (false, true) => std::cmp::Ordering::Greater,
            _ => a
                .name
                .to_lowercase()
                .cmp(&b.name.to_lowercase()),
        });

        self.entries = list;
        self.loaded = true;
    }

    fn enter(&mut self, path: PathBuf) {
        if path.is_dir() {
            self.path = path;
            self.refresh();
        }
    }

    fn go_up(&mut self) {
        if let Some(parent) = self.path.parent() {
            let parent = parent.to_path_buf();
            self.enter(parent);
        }
    }

    fn go_home(&mut self) {
        if let Some(home) = dirs::home_dir() {
            self.enter(home);
        }
    }

    /// 渲染浏览器。`compact` = 窄栏模式（隐藏大小/时间列）。
    pub fn ui(&mut self, ui: &mut egui::Ui, compact: bool) {
        if !self.loaded {
            self.refresh();
        }

        // ---- 工具栏 ----
        ui.horizontal(|ui| {
            if ui
                .add(egui::Button::new("⬆").small())
                .on_hover_text("上一级")
                .clicked()
            {
                self.go_up();
            }
            if ui
                .add(egui::Button::new("🏠").small())
                .on_hover_text("主目录")
                .clicked()
            {
                self.go_home();
            }
            if ui
                .add(egui::Button::new("⟳").small())
                .on_hover_text("刷新")
                .clicked()
            {
                self.refresh();
            }

            // 路径栏占满剩余宽度（预留右侧复选框）
            let reserve = if compact { 0.0 } else { 118.0 };
            let w = (ui.available_width() - reserve).max(60.0);
            let resp = ui.add_sized(
                [w, 20.0],
                egui::TextEdit::singleline(&mut self.path_edit)
                    .hint_text("输入路径后回车")
                    .font(egui::TextStyle::Small),
            );
            if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                let p = PathBuf::from(self.path_edit.trim());
                if p.is_dir() {
                    self.enter(p);
                } else {
                    self.status = Some(format!("不是有效目录: {}", self.path_edit.trim()));
                    self.path_edit = self.path.to_string_lossy().to_string();
                }
            }

            if !compact {
                let mut hidden = self.show_hidden;
                if ui.checkbox(&mut hidden, "显示隐藏文件").changed() {
                    self.show_hidden = hidden;
                    self.refresh();
                }
            }
        });

        ui.separator();

        // ---- 文件列表 ----
        let row_h = 18.0;
        egui::ScrollArea::vertical()
            .id_salt(("filebrowser", self.path.to_string_lossy().to_string()))
            .auto_shrink([false, false])
            .show(ui, |ui| {
                let mut enter_dir: Option<PathBuf> = None;
                let entries = std::mem::take(&mut self.entries);
                for (idx, file) in entries.iter().enumerate() {
                    let selected = self.selected == Some(idx);
                    let icon = if file.is_dir { "📁" } else { "📄" };
                    let label = format!("{icon} {}", file.name);

                    let (rect, resp) = ui.allocate_exact_size(
                        egui::vec2(ui.available_width(), row_h),
                        egui::Sense::click(),
                    );
                    if selected {
                        ui.painter().rect_filled(
                            rect,
                            2.0,
                            ui.visuals().selection.bg_fill.gamma_multiply(0.45),
                        );
                    } else if resp.hovered() {
                        ui.painter().rect_filled(
                            rect,
                            2.0,
                            ui.visuals().widgets.hovered.bg_fill.gamma_multiply(0.5),
                        );
                    }

                    let text_color = if file.is_dir {
                        ui.visuals().hyperlink_color
                    } else {
                        ui.visuals().text_color()
                    };
                    ui.painter().text(
                        egui::pos2(rect.min.x + 4.0, rect.center().y),
                        egui::Align2::LEFT_CENTER,
                        &label,
                        egui::FontId::proportional(12.0),
                        text_color,
                    );

                    // 右侧：大小 / 时间
                    if !compact {
                        if let Some(modi) = &file.modified {
                            ui.painter().text(
                                egui::pos2(rect.right() - 4.0, rect.center().y),
                                egui::Align2::RIGHT_CENTER,
                                modi,
                                egui::FontId::proportional(10.0),
                                ui.visuals().weak_text_color(),
                            );
                        }
                        let size = file.size_label();
                        if !size.is_empty() {
                            ui.painter().text(
                                egui::pos2(rect.right() - 110.0, rect.center().y),
                                egui::Align2::RIGHT_CENTER,
                                size,
                                egui::FontId::proportional(10.0),
                                ui.visuals().weak_text_color(),
                            );
                        }
                    }

                    if resp.clicked() {
                        self.selected = Some(idx);
                    }
                    if resp.double_clicked() && file.is_dir {
                        enter_dir = Some(file.path.clone());
                    }
                    if resp.double_clicked() && !file.is_dir {
                        open_path(&file.path);
                    }
                    resp.on_hover_text(file.path.to_string_lossy());
                }
                self.entries = entries;

                if let Some(dir) = enter_dir {
                    self.enter(dir);
                }
            });

        ui.separator();
        // ---- 状态行 ----
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new(format!("{} 项", self.entries.len()))
                    .size(10.0)
                    .color(ui.visuals().weak_text_color()),
            );
            if compact {
                let mut hidden = self.show_hidden;
                if ui
                    .add(egui::Checkbox::new(&mut hidden, "隐藏"))
                    .on_hover_text("显示隐藏文件（含系统文件）")
                    .changed()
                {
                    self.show_hidden = hidden;
                    self.refresh();
                }
            }
            if let Some(s) = &self.status {
                ui.colored_label(egui::Color32::from_rgb(230, 140, 90), s);
            }
        });
    }
}

/// 隐藏判定：Unix 点前缀；Windows 额外看 FILE_ATTRIBUTE_HIDDEN。
fn is_hidden(name: &str, path: &Path) -> bool {
    if name.starts_with('.') {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if let Ok(meta) = std::fs::metadata(path) {
            const FILE_ATTRIBUTE_HIDDEN: u32 = 0x2;
            const FILE_ATTRIBUTE_SYSTEM: u32 = 0x4;
            let attr = meta.file_attributes();
            if attr & FILE_ATTRIBUTE_HIDDEN != 0 || attr & FILE_ATTRIBUTE_SYSTEM != 0 {
                return true;
            }
        }
    }
    let _ = path;
    false
}

/// 用系统默认程序打开文件。
fn open_path(path: &Path) {
    #[cfg(windows)]
    {
        let _ = std::process::Command::new("cmd")
            .args(["/C", "start", ""])
            .arg(path)
            .spawn();
    }
    #[cfg(not(windows))]
    {
        let _ = std::process::Command::new("xdg-open").arg(path).spawn();
    }
}

pub fn format_size(size: u64) -> String {
    const UNITS: &[&str] = &["B", "KB", "MB", "GB", "TB"];
    let mut size = size as f64;
    let mut unit_idx = 0;
    while size >= 1024.0 && unit_idx < UNITS.len() - 1 {
        size /= 1024.0;
        unit_idx += 1;
    }
    if unit_idx == 0 {
        format!("{} {}", size as u64, UNITS[unit_idx])
    } else {
        format!("{:.1} {}", size, UNITS[unit_idx])
    }
}
