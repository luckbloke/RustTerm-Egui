//! 右侧「SFTP」停靠面板（对齐参考实现：绑定当前 SSH 会话，浏览**远程**目录）。
//!
//! 结构（与参考前端一致）：
//! - 头部：`SFTP` + ↑ 上一级 / ⟳ 刷新 / ⇧ 上传 / ＋ 新建目录
//! - 路径栏（回车跳转）、🏠 跳远端家目录
//! - 选项：显示隐藏文件 + 被隐藏条数
//! - 列表：`[D]/[F] 名称` + 体积；双击目录进入、双击文件下载；右键菜单
//! - 进度条（可取消）+ 传输队列
//! - 状态：N 项 + 绑定会话
//!
//! 关键点：**复用当前 SSH 会话已认证的连接**（`SshHandle::open_sftp`），
//! 不重新建链、不重复认证，跳板机场景同样成立。

use std::sync::mpsc::Receiver;
use std::sync::Arc;

use crate::backends::ssh::SshHandle;
use crate::sftp_client::{
    human_size, join_remote, parent_remote, RemoteEntry, SftpClient, TransferCancel,
};

/// 一次传输的运行态。
struct TransferTask {
    label: String,
    sent: u64,
    total: u64,
    cancel: Arc<TransferCancel>,
    progress_rx: Receiver<(u64, u64)>,
    result_rx: Receiver<Result<(), String>>,
    error: Option<String>,
    finished: bool,
    /// 结束后自动刷新列表（上传 / 删除会改变远端内容）
    refresh_on_done: bool,
    refreshed: bool,
}

/// 连接结果：客户端 + 家目录 + 初次列表
type ConnectResult = (Arc<SftpClient>, String, Vec<RemoteEntry>);
/// 列目录结果：(被列出的路径, 条目)
type ListResult = Result<(String, Vec<RemoteEntry>), String>;

pub struct SftpPanel {
    pub open: bool,

    // ---- 会话绑定 ----
    /// 绑定的会话身份（`Arc::as_ptr`），用于检测切换标签
    bound_key: Option<usize>,
    /// 是否已针对 `bound_key` 做过一次绑定动作（避免每帧重复建链，
    /// 同时保证「无 SSH 会话」的提示能在首帧落地）
    pub bound_initialized: bool,
    bound_label: String,
    client: Option<Arc<SftpClient>>,
    connecting: bool,
    connect_rx: Option<Receiver<Result<ConnectResult, String>>>,
    bind_error: Option<String>,

    // ---- 目录 ----
    path: String,
    path_edit: String,
    entries: Vec<RemoteEntry>,
    show_hidden: bool,
    hidden_count: usize,
    loading: bool,
    list_rx: Option<Receiver<ListResult>>,
    selected: Option<usize>,
    hint: Option<String>,

    // ---- 传输 ----
    tasks: Vec<TransferTask>,

    /// 运行时的句柄快照（`request_list` 等内部动作需要 spawn）
    rt: Option<tokio::runtime::Handle>,

    // ---- 内联输入 ----
    renaming: Option<(String, String)>,
    mkdir_input: Option<String>,

    // ---- 拖拽上传 ----
    /// 面板矩形（上一帧记录，拖拽落点判定用）
    panel_rect: Option<egui::Rect>,
}

impl Default for SftpPanel {
    fn default() -> Self {
        Self {
            open: false,
            bound_key: None,
            bound_initialized: false,
            bound_label: String::new(),
            client: None,
            connecting: false,
            connect_rx: None,
            bind_error: None,
            path: ".".to_string(),
            path_edit: ".".to_string(),
            entries: Vec::new(),
            show_hidden: false,
            hidden_count: 0,
            loading: false,
            list_rx: None,
            selected: None,
            hint: None,
            tasks: Vec::new(),
            rt: None,
            renaming: None,
            mkdir_input: None,
            panel_rect: None,
        }
    }
}

impl SftpPanel {
    pub fn toggle(&mut self) {
        self.open = !self.open;
        // 关→开时重新绑定，让打开动作触发建链
        if self.open {
            self.bound_initialized = false;
        }
    }

    /// 绑定到当前活动 SSH 会话。每帧调用；会话变了才重新连接。
    pub fn bind(&mut self, rt: &tokio::runtime::Handle, label: &str, ssh: Option<Arc<SshHandle>>) {
        self.rt = Some(rt.clone());
        let key = ssh
            .as_ref()
            .map(|h| Arc::as_ptr(h) as *const u8 as usize);

        if key == self.bound_key && self.bound_initialized {
            return;
        }

        // 会话变了（或首次绑定）：清掉旧状态
        self.bound_key = key;
        self.bound_initialized = true;
        self.client = None;
        self.entries.clear();
        self.selected = None;
        self.list_rx = None;
        self.connect_rx = None;
        self.bind_error = None;
        self.loading = false;
        self.hint = None;
        self.renaming = None;
        self.mkdir_input = None;
        self.bound_label = ssh
            .as_ref()
            .map(|h| h.target.clone())
            .unwrap_or_else(|| label.to_string());

        let Some(ssh) = ssh else {
            self.hint = Some("SFTP 需要活动的 SSH 会话：先打开一个 SSH 终端标签".into());
            return;
        };
        if !self.open {
            return;
        }

        self.connecting = true;
        let (tx, rx) = std::sync::mpsc::channel::<Result<ConnectResult, String>>();
        self.connect_rx = Some(rx);
        rt.spawn(async move {
            let result = async {
                let session = ssh.open_sftp().await?;
                let client = Arc::new(SftpClient::new(session));
                let home = client.home_dir().await.unwrap_or_else(|_| ".".to_string());
                let entries = client.list_dir(&home).await?;
                Ok::<_, anyhow::Error>((client, home, entries))
            }
            .await
            .map_err(|e| format!("{e:#}"));
            let _ = tx.send(result);
        });
    }

    /// 渲染（右侧停靠，必须在 CentralPanel 之前 show）。
    ///
    /// 拖拽上传（对齐参考实现）：文件拖到面板上释放 = 上传到当前远端目录，
    /// 拖入过程整个面板高亮；落点不在面板内则不接管。
    pub fn ui(&mut self, ui: &mut egui::Ui, rt: &tokio::runtime::Handle) {
        if !self.open {
            return;
        }
        self.rt = Some(rt.clone());
        self.poll();

        let egui_ctx = ui.ctx().clone();

        // ---- 拖拽判定 ----
        // 注意：拖拽过程中 egui 的指针位置停止更新（winit 的 DragOver 完全
        // 忽略 DragOver/给的位置），所以这里用系统光标真实位置换算。
        let panel_rect = self.panel_rect;
        let pointer = drop_cursor_logical(&egui_ctx);
        let inside_panel =
            panel_rect.is_some_and(|r| pointer.is_some_and(|p| r.contains(p)));

        let panel_resp = egui::Panel::right("sftp_sidebar")
            .default_size(300.0)
            .min_size(220.0)
            .resizable(true)
            .show(ui, |ui| {
                self.header_ui(ui);
                ui.separator();

                if self.connecting {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(egui::RichText::new("正在打开 SFTP 通道…").size(11.0));
                    });
                    return;
                }
                if let Some(err) = self.bind_error.clone() {
                    ui.colored_label(egui::Color32::from_rgb(230, 120, 120), &err);
                    if ui.small_button("重试").clicked() {
                        self.bound_key = None;
                    }
                    return;
                }
                if let Some(hint) = self.hint.clone() {
                    ui.add_space(6.0);
                    ui.label(
                        egui::RichText::new(hint)
                            .size(11.0)
                            .color(ui.visuals().weak_text_color()),
                    );
                    return;
                }

                self.path_ui(ui);
                self.options_ui(ui);
                ui.separator();
                self.list_ui(ui, rt);
                self.progress_ui(ui);
                self.status_ui(ui);
            });
        self.panel_rect = Some(panel_resp.response.rect);

        // ---- 命中释放 → 上传到当前远端目录 ----
        if inside_panel {
            let dropped: Vec<egui::DroppedFileHandle> =
                ui.input(|i| i.raw.dropped_files.clone());
            for f in dropped {
                self.start_upload(f.path().to_path_buf());
            }
        }

        // ---- 拖拽中的高亮浮层（show 返回后绘制 ⇒ 在面板内容之上）----
        if ui.input(|i| !i.raw.hovered_files.is_empty()) && inside_panel {
            let rect = panel_resp.response.rect;
            let painter = ui.painter_at(rect);
            painter.rect_filled(rect, 4.0, ui.visuals().hyperlink_color.gamma_multiply(0.12));
            painter.rect_stroke(
                rect,
                4.0,
                egui::Stroke::new(2.0, ui.visuals().hyperlink_color),
                egui::StrokeKind::Inside,
            );
            painter.text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                "⇧ 松开上传到当前目录",
                egui::FontId::proportional(13.0),
                ui.visuals().hyperlink_color,
            );
        }
    }

    /// 是否正把文件拖到本面板上（app 层用于避免其它组件抢走这次拖放）。
    pub fn takes_drops(&self, ctx: &egui::Context) -> bool {
        self.panel_rect.is_some()
            && ctx.input(|i| !i.raw.hovered_files.is_empty())
            && drop_cursor_logical(ctx)
                .is_some_and(|p| self.panel_rect.is_some_and(|r| r.contains(p)))
    }

    fn header_ui(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("SFTP").size(13.0).strong());
            if ui.small_button("↑").on_hover_text("上一级").clicked() {
                if let Some(p) = parent_remote(&self.path) {
                    self.navigate(p);
                }
            }
            if ui.small_button("⟳").on_hover_text("刷新").clicked() {
                self.request_list();
            }
            if ui
                .small_button("⇧")
                .on_hover_text("上传文件到当前目录")
                .clicked()
            {
                self.upload_clicked();
            }
            if ui.small_button("＋").on_hover_text("新建目录").clicked() {
                self.mkdir_input = Some(String::new());
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.small_button("✕").on_hover_text("关闭面板").clicked() {
                    self.open = false;
                }
            });
        });
    }

    fn path_ui(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if ui.small_button("🏠").on_hover_text("远端家目录").clicked() {
                self.request_home();
            }
            let resp = ui.add(
                egui::TextEdit::singleline(&mut self.path_edit)
                    .desired_width(f32::INFINITY)
                    .hint_text("远端路径，回车跳转")
                    .font(egui::TextStyle::Small),
            );
            if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                let p = self.path_edit.trim().to_string();
                if !p.is_empty() {
                    self.navigate(p);
                } else {
                    self.path_edit = self.path.clone();
                }
            }
        });
    }

    fn options_ui(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            let mut show = self.show_hidden;
            if ui
                .add(egui::Checkbox::new(
                    &mut show,
                    egui::RichText::new("显示隐藏文件").size(11.0),
                ))
                .changed()
            {
                self.show_hidden = show;
                self.selected = None;
            }
            if !self.show_hidden && self.hidden_count > 0 {
                ui.label(
                    egui::RichText::new(format!("已隐藏 {}", self.hidden_count))
                        .size(10.0)
                        .color(ui.visuals().weak_text_color()),
                );
            }
        });
    }

    fn list_ui(&mut self, ui: &mut egui::Ui, rt: &tokio::runtime::Handle) {
        let row_h = 18.0;
        let mut enter_dir: Option<String> = None;
        let mut download: Option<(String, String)> = None;
        let mut rename_to: Option<(String, String)> = None;
        let mut delete: Option<(String, bool)> = None;

        let list_h = (ui.available_height() - 90.0).max(60.0);
        egui::ScrollArea::vertical()
            .id_salt("sftp_remote_list")
            .auto_shrink([false, false])
            .max_height(list_h)
            .show(ui, |ui| {
                if self.loading {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(egui::RichText::new("加载中…").size(11.0));
                    });
                    return;
                }
                if self.entries.is_empty() {
                    ui.label(
                        egui::RichText::new(if self.hidden_count > 0 {
                            "目录内全是隐藏项"
                        } else {
                            "空目录"
                        })
                        .size(11.0)
                        .color(ui.visuals().weak_text_color()),
                    );
                    return;
                }

                for idx in 0..self.entries.len() {
                    let entry = self.entries[idx].clone();
                    if !self.show_hidden && entry.hidden {
                        continue;
                    }
                    let selected = self.selected == Some(idx);
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

                    let tag = if entry.is_dir { "[D]" } else { "[F]" };
                    let color = if entry.is_dir {
                        ui.visuals().hyperlink_color
                    } else {
                        ui.visuals().text_color()
                    };
                    // 文件名来自远端，只当纯文本绘制，绝不解析
                    ui.painter().text(
                        egui::pos2(rect.min.x + 4.0, rect.center().y),
                        egui::Align2::LEFT_CENTER,
                        format!("{tag} {}", entry.name),
                        egui::FontId::proportional(12.0),
                        color,
                    );
                    if !entry.is_dir {
                        ui.painter().text(
                            egui::pos2(rect.right() - 4.0, rect.center().y),
                            egui::Align2::RIGHT_CENTER,
                            human_size(entry.size),
                            egui::FontId::proportional(10.0),
                            ui.visuals().weak_text_color(),
                        );
                    }

                    if resp.clicked() {
                        self.selected = Some(idx);
                    }
                    if resp.double_clicked() {
                        if entry.is_dir {
                            enter_dir = Some(entry.path.clone());
                        } else {
                            download = Some((entry.path.clone(), entry.name.clone()));
                        }
                    }
                    resp.clone().context_menu(|ui| {
                        if !entry.is_dir && ui.button("下载…").clicked() {
                            download = Some((entry.path.clone(), entry.name.clone()));
                            ui.close();
                        }
                        if ui.button("重命名…").clicked() {
                            rename_to = Some((entry.path.clone(), entry.name.clone()));
                            ui.close();
                        }
                        if ui.button("删除").clicked() {
                            delete = Some((entry.path.clone(), entry.is_dir));
                            ui.close();
                        }
                    });
                    resp.on_hover_text(entry.path.clone());
                }
            });

        // ---- 内联输入：重命名 ----
        if let Some((old, name)) = self.renaming.clone() {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("重命名为").size(11.0));
                let mut input = name.clone();
                let resp = ui.add(
                    egui::TextEdit::singleline(&mut input)
                        .desired_width(f32::INFINITY)
                        .font(egui::TextStyle::Small),
                );
                resp.request_focus();
                let (enter, esc) = ui.input(|i| {
                    (
                        i.key_pressed(egui::Key::Enter),
                        i.key_pressed(egui::Key::Escape),
                    )
                });
                if enter && !input.trim().is_empty() {
                    self.renaming = Some((old, input));
                    self.apply_rename(rt);
                    self.renaming = None;
                } else if enter || esc {
                    self.renaming = None;
                } else {
                    self.renaming = Some((old, input));
                }
                if ui.small_button("✕").clicked() {
                    self.renaming = None;
                }
            });
        }

        // ---- 内联输入：新建目录 ----
        if let Some(name) = self.mkdir_input.clone() {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("新目录名").size(11.0));
                let mut input = name.clone();
                let resp = ui.add(
                    egui::TextEdit::singleline(&mut input)
                        .desired_width(f32::INFINITY)
                        .font(egui::TextStyle::Small),
                );
                resp.request_focus();
                let (enter, esc) = ui.input(|i| {
                    (
                        i.key_pressed(egui::Key::Enter),
                        i.key_pressed(egui::Key::Escape),
                    )
                });
                if enter && !input.trim().is_empty() {
                    self.mkdir_input = Some(input);
                    self.apply_mkdir(rt);
                    self.mkdir_input = None;
                } else if enter || esc {
                    self.mkdir_input = None;
                } else {
                    self.mkdir_input = Some(input);
                }
                if ui.small_button("✕").clicked() {
                    self.mkdir_input = None;
                }
            });
        }

        // ---- 动作（延迟到循环外，避免迭代中改集合）----
        if let Some(dir) = enter_dir {
            self.navigate(dir);
        }
        if let Some((path, name)) = download {
            self.start_download(rt, path, name);
        }
        if let Some((path, is_dir)) = delete {
            self.apply_delete(rt, path, is_dir);
        }
        if rename_to.is_some() {
            self.renaming = rename_to;
        }
    }

    fn progress_ui(&mut self, ui: &mut egui::Ui) {
        if self.tasks.is_empty() {
            return;
        }
        ui.separator();
        let mut remove: Option<usize> = None;
        for (i, task) in self.tasks.iter_mut().enumerate() {
            let ratio = if task.total > 0 {
                (task.sent as f32 / task.total as f32).clamp(0.0, 1.0)
            } else {
                0.0
            };
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new(&task.label)
                        .size(10.0)
                        .color(ui.visuals().weak_text_color()),
                );
                if task.finished && task.error.is_none() {
                    ui.label(
                        egui::RichText::new("✓")
                            .size(10.0)
                            .color(egui::Color32::GREEN),
                    );
                }
                if ui.small_button("✕").clicked() {
                    if task.finished {
                        remove = Some(i);
                    } else {
                        task.cancel.request_user_cancel();
                    }
                }
            });
            ui.add(
                egui::ProgressBar::new(ratio)
                    .desired_height(8.0)
                    .text(format!(
                        "{} / {}",
                        human_size(task.sent),
                        human_size(task.total)
                    )),
            );
            if let Some(err) = task.error.clone() {
                ui.label(
                    egui::RichText::new(err)
                        .size(10.0)
                        .color(egui::Color32::from_rgb(230, 120, 120)),
                );
            }
        }
        if let Some(i) = remove {
            self.tasks.remove(i);
        }
    }

    fn status_ui(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            let visible = self
                .entries
                .iter()
                .filter(|e| self.show_hidden || !e.hidden)
                .count();
            ui.label(
                egui::RichText::new(format!("{visible} 项"))
                    .size(10.0)
                    .color(ui.visuals().weak_text_color()),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(
                    egui::RichText::new(&self.bound_label)
                        .size(10.0)
                        .color(ui.visuals().weak_text_color()),
                );
            });
        });
    }

    // ------------------------------------------------------------------
    // 轮询后台结果
    // ------------------------------------------------------------------

    fn poll(&mut self) {
        let got = self.connect_rx.as_ref().and_then(|rx| rx.try_recv().ok());
        if let Some(result) = got {
            self.connect_rx = None;
            self.connecting = false;
            match result {
                Ok((client, home, entries)) => {
                    self.client = Some(client);
                    self.path = home.clone();
                    self.path_edit = home;
                    self.set_entries(entries);
                    self.bind_error = None;
                }
                Err(e) => self.bind_error = Some(e),
            }
        }

        let got = self.list_rx.as_ref().and_then(|rx| rx.try_recv().ok());
        if let Some(result) = got {
            self.list_rx = None;
            self.loading = false;
            match result {
                Ok((path, entries)) => {
                    self.path = path.clone();
                    self.path_edit = path;
                    self.set_entries(entries);
                }
                Err(e) => self.bind_error = Some(e),
            }
        }

        // 传输/删除完成且需要刷新的（多个同时完成只刷一次）
        let mut need_list = false;
        for task in self.tasks.iter_mut() {
            while let Ok((sent, total)) = task.progress_rx.try_recv() {
                task.sent = sent;
                task.total = total;
            }
            if let Ok(res) = task.result_rx.try_recv() {
                task.finished = true;
                task.error = res.err();
                if task.refresh_on_done && !task.refreshed {
                    task.refreshed = true;
                    need_list = true;
                }
            }
        }
        if need_list {
            self.request_list();
        }
    }

    fn set_entries(&mut self, entries: Vec<RemoteEntry>) {
        self.hidden_count = entries.iter().filter(|e| e.hidden).count();
        self.entries = entries;
        self.selected = None;
    }

    // ------------------------------------------------------------------
    // 动作
    // ------------------------------------------------------------------

    fn navigate(&mut self, path: String) {
        // 先清空：否则新目录返回前，列表还是旧目录的内容（还能被双击）
        self.path = path.clone();
        self.path_edit = path;
        self.entries.clear();
        self.selected = None;
        self.request_list();
    }

    fn request_list(&mut self) {
        let Some(client) = self.client.clone() else { return };
        let Some(rt) = self.rt.clone() else { return };
        self.loading = true;
        let (tx, rx) = std::sync::mpsc::channel();
        self.list_rx = Some(rx);
        let path = self.path.clone();
        rt.spawn(async move {
            let r = client
                .list_dir(&path)
                .await
                .map(|e| (path.clone(), e))
                .map_err(|e| format!("{e:#}"));
            let _ = tx.send(r);
        });
    }

    fn request_home(&mut self) {
        let Some(client) = self.client.clone() else { return };
        let Some(rt) = self.rt.clone() else { return };
        self.loading = true;
        let (tx, rx) = std::sync::mpsc::channel();
        self.list_rx = Some(rx);
        rt.spawn(async move {
            let r = async {
                let home = client.home_dir().await?;
                let entries = client.list_dir(&home).await?;
                Ok::<_, anyhow::Error>((home, entries))
            }
            .await
            .map_err(|e| format!("{e:#}"));
            let _ = tx.send(r);
        });
    }

    fn upload_clicked(&mut self) {
        let Some(files) = rfd::FileDialog::new().pick_files() else {
            return;
        };
        for p in files {
            self.start_upload(p);
        }
    }

    /// 把一个本地文件上传到当前远端目录（⇧ 按钮 / 拖拽共用）。
    fn start_upload(&mut self, local: std::path::PathBuf) {
        let Some(client) = self.client.clone() else { return };
        let Some(rt) = self.rt.clone() else { return };
        let dir = self.path.clone();

        let name = local
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "文件".to_string());
        self.spawn_transfer(
            rt.clone(),
            format!("上传 {name}"),
            true,
            move |cancel, tx| async move {
                client
                    .upload(&local, &dir, cancel, move |sent, total| {
                        let _ = tx.send((sent, total));
                    })
                    .await
            },
        );
    }

    fn start_download(&mut self, rt: &tokio::runtime::Handle, remote: String, name: String) {
        let Some(client) = self.client.clone() else { return };
        let Some(local) = rfd::FileDialog::new().set_file_name(&name).save_file() else {
            return;
        };
        let rt = rt.clone();
        self.spawn_transfer(rt, format!("下载 {name}"), false, move |cancel, tx| async move {
            client
                .download(&remote, &local, cancel, move |sent, total| {
                    let _ = tx.send((sent, total));
                })
                .await
        });
    }

    fn apply_delete(&mut self, rt: &tokio::runtime::Handle, path: String, is_dir: bool) {
        let Some(client) = self.client.clone() else { return };
        let rt = rt.clone();
        // 删除走传输队列：完成后自动刷新列表（此前立即列目录会读到删除前的旧状态）
        let label = if is_dir {
            format!("删除目录 {}", short_name(&path))
        } else {
            format!("删除 {}", short_name(&path))
        };
        self.spawn_transfer(rt, label, true, move |_cancel, _tx| async move {
            client.remove(&path, is_dir).await
        });
    }

    fn apply_mkdir(&mut self, rt: &tokio::runtime::Handle) {
        let Some(client) = self.client.clone() else { return };
        let Some(input) = self.mkdir_input.clone() else { return };
        let path = join_remote(&self.path, input.trim());
        let rt = rt.clone();
        self.spawn_transfer(
            rt,
            format!("新建目录 {}", short_name(&path)),
            true,
            move |_cancel, _tx| async move { client.mkdir(&path).await },
        );
    }

    fn apply_rename(&mut self, rt: &tokio::runtime::Handle) {
        let Some(client) = self.client.clone() else { return };
        let Some((old, name)) = self.renaming.clone() else { return };
        let old_name = short_name(&old).to_string();
        let name = name.trim().to_string();
        if name.is_empty() {
            return;
        }
        // 目录部分保持原样（根目录必须拼回 "/"，否则会变成相对路径被搬到 home）
        let parent = match old.rfind('/') {
            Some(0) => "/".to_string(),
            Some(i) => old[..i].to_string(),
            None => String::new(),
        };
        let new_path = join_remote(&parent, &name);
        let rt = rt.clone();
        self.spawn_transfer(
            rt,
            format!("重命名 {old_name} → {name}"),
            true,
            move |_cancel, _tx| async move { client.rename(&old, &new_path).await },
        );
    }

    /// 统一的传输启动：进度 + 结果各一条通道。
    /// `refresh_on_done`：结束后自动刷新远端列表（上传/删除等会改远端的操作）。
    fn spawn_transfer<F, Fut>(
        &mut self,
        rt: tokio::runtime::Handle,
        label: String,
        refresh_on_done: bool,
        make: F,
    ) where
        F: FnOnce(Arc<TransferCancel>, std::sync::mpsc::Sender<(u64, u64)>) -> Fut,
        Fut: std::future::Future<Output = anyhow::Result<()>> + Send + 'static,
    {
        let cancel = TransferCancel::new();
        let (prog_tx, prog_rx) = std::sync::mpsc::channel();
        let (res_tx, res_rx) = std::sync::mpsc::channel();
        let fut = make(cancel.clone(), prog_tx);
        rt.spawn(async move {
            let r = fut.await;
            let _ = res_tx.send(r.map_err(|e| format!("{e:#}")));
        });
        self.tasks.push(TransferTask {
            label,
            sent: 0,
            total: 0,
            cancel,
            progress_rx: prog_rx,
            result_rx: res_rx,
            error: None,
            finished: false,
            refresh_on_done,
            refreshed: false,
        });
    }
}

/// 文件拖拽时光标的真实位置，换算成主窗口 egui 逻辑坐标。
///
/// OLE 拖放期间 winit 会忽略 `DragOver` 的位置参数、也不转发鼠标移动，
/// egui 的 `pointer.hover_pos()` 停在拖拽开始前的旧值，不可用。
/// 这里直接调 `GetCursorPos`（OS 层光标位置在拖拽中保持真实）。
#[cfg(windows)]
fn drop_cursor_logical(ctx: &egui::Context) -> Option<egui::Pos2> {
    use windows_sys::Win32::Foundation::POINT;
    use windows_sys::Win32::Graphics::Gdi::ScreenToClient;
    use windows_sys::Win32::UI::WindowsAndMessaging::GetCursorPos;

    let hwnd = crate::browser::main_hwnd()?;
    let mut pt = POINT { x: 0, y: 0 };
    unsafe {
        if GetCursorPos(&mut pt) == 0 {
            return None;
        }
        // 屏幕物理像素 → 主窗口客户区物理像素 → egui 逻辑点
        ScreenToClient(hwnd as *mut core::ffi::c_void, &mut pt);
    }
    let ppp = ctx.pixels_per_point();
    Some(egui::pos2(pt.x as f32 / ppp, pt.y as f32 / ppp))
}

#[cfg(not(windows))]
fn drop_cursor_logical(_ctx: &egui::Context) -> Option<egui::Pos2> {
    // 非 Windows 平台退回 egui 指针位置（Linux/macOS 的拖放链路会更新）
    // 无 ctx 拿不到输入，这里返回 None 表示未知
    None
}

/// 从远端路径取末段名称（显示用）。
fn short_name(path: &str) -> &str {
    let trimmed = path.trim_end_matches('/');
    match trimmed.rfind('/') {
        Some(i) => &trimmed[i + 1..],
        None => trimmed,
    }
}
