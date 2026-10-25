//! 应用外壳：菜单栏 + 工具栏 + 快速连接栏 + 左侧会话栏 + 中央工作区（标签页）
//! + 右侧 AI 助手面板 + 底部状态栏。
//!
//! 标签页统一承载四类会话：本地 / SSH 终端、内置浏览器、远程桌面、SFTP，
//! 与设计稿中「浏览器像普通标签页一样出现在标签栏」的观感一致。

use crate::ai_assistant::AiAssistantPanel;
use crate::ai_config_panel::AiConfigPanel;
use crate::browser::{BrowserService, BrowserTab};
use crate::config::AppConfig;
use crate::config_panel::ConfigPanel;
use crate::i18n::{tr, I18n};
use crate::macros::MacroPanel;
use crate::rdp::{RdTabProtocol, RdpConnectionProfile, RdpProfiles, RemoteDesktopTab};
use crate::sessions::{SessionKind, SessionStore};
use crate::sftp::SftpPanel;
use crate::tabs::{TabContent, TabType};
use crate::term::{normalized_selection, SpawnCfg, TermPane};
use eframe::egui;
use tokio::runtime::Runtime;

/// 全局快捷键动作
enum GlobalAction {
    NextTab,
    PrevTab,
    JumpTab(usize),
    NewLocalTab,
    NewBrowserTab,
    ToggleSftp,
    ToggleLocalFiles,
    ToggleRdp,
    ToggleBrowser,
    ToggleAi,
    CloseTab,
    ToggleFullscreen,
    FocusAddress,
    FindInTerminal,
}

/// SSH 连接表单状态
pub struct ConnectForm {
    pub open: bool,
    pub host: String,
    pub port: String,
    pub user: String,
    pub password: String,
    /// 认证方式：password / key
    pub auth_method: String,
    /// 私钥路径
    pub key_path: String,
    /// 私钥 passphrase
    pub key_passphrase: String,
    pub error: Option<String>,
}

impl Default for ConnectForm {
    fn default() -> Self {
        Self {
            open: false,
            host: String::new(),
            port: "22".to_string(),
            user: String::new(),
            password: String::new(),
            auth_method: "password".to_string(),
            key_path: String::new(),
            key_passphrase: String::new(),
            error: None,
        }
    }
}

impl ConnectForm {
    /// 转成后端认证方式。
    fn to_auth(&self) -> crate::backends::ssh::SshAuth {
        if self.auth_method.eq_ignore_ascii_case("key") {
            crate::backends::ssh::SshAuth::Key {
                path: self.key_path.trim().to_string(),
                passphrase: self.key_passphrase.clone(),
            }
        } else {
            crate::backends::ssh::SshAuth::Password(self.password.clone())
        }
    }
}

/// 新建会话表单
struct AddSessionForm {
    kind: SessionKind,
    name: String,
    host: String,
    port: String,
    user: String,
    password: String,
    auth_method: String,
    key_path: String,
    /// 域（RDP 登录用）
    domain: String,
    /// Some(id) = 编辑已有会话；None = 新建
    editing: Option<String>,
}

#[derive(Clone, Copy, PartialEq)]
enum SidebarTab {
    Sessions,
    Tools,
    Macros,
}

/// 工具栏/侧栏里的功能入口 → 打开对应标签页。
#[derive(Clone, Copy, PartialEq)]
enum ToolAction {
    LocalTerminal,
    SshClient,
    SftpClient,
    TelnetClient,
    RdpClient,
    VncClient,
    SpiceClient,
    LocalFiles,
    Browser,
    Ping,
    Find,
    Sync,
    Batch,
    Tunnel,
    Macro,
}

impl ToolAction {
    fn label(&self) -> &'static str {
        match self {
            ToolAction::LocalTerminal => "本地会话",
            ToolAction::SshClient => "SSH 客户端",
            ToolAction::SftpClient => "SFTP 客户端",
            ToolAction::TelnetClient => "Telnet 客户端",
            ToolAction::RdpClient => "RDP 客户端",
            ToolAction::VncClient => "VNC 客户端",
            ToolAction::SpiceClient => "Spice 客户端",
            ToolAction::LocalFiles => "本地文件",
            ToolAction::Browser => "内置浏览器",
            ToolAction::Ping => "Ping",
            ToolAction::Find => "查找",
            ToolAction::Sync => "同步",
            ToolAction::Batch => "批量",
            ToolAction::Tunnel => "隧道",
            ToolAction::Macro => "键宏",
        }
    }

    fn icon(&self) -> &'static str {
        match self {
            ToolAction::LocalTerminal => "🖥",
            ToolAction::SshClient => "🔗",
            ToolAction::SftpClient => "📁",
            ToolAction::TelnetClient => "📡",
            ToolAction::RdpClient => "🖥",
            ToolAction::VncClient => "🖥",
            ToolAction::SpiceClient => "🖥",
            ToolAction::LocalFiles => "📂",
            ToolAction::Browser => "🌐",
            ToolAction::Ping => "📶",
            ToolAction::Find => "🔍",
            ToolAction::Sync => "🔄",
            ToolAction::Batch => "📦",
            ToolAction::Tunnel => "🔀",
            ToolAction::Macro => "⌨",
        }
    }
}

pub struct RustTermApp {
    runtime: std::sync::Arc<Runtime>,
    /// 统一标签页集合（终端 / 浏览器 / 远程桌面 / SFTP）
    tabs: Vec<TabContent>,
    active: usize,
    form: ConnectForm,
    /// 正在拖拽的标签下标（拖拽排序）
    dragging: Option<usize>,
    /// 拖拽起始指针位置，用于计算浮动预览偏移
    drag_start: Option<egui::Pos2>,
    /// 内联重命名会话：(标签下标, 编辑文本, 是否本帧请求焦点)
    renaming: Option<(usize, String, bool)>,
    // 功能面板
    pub config: AppConfig,
    pub i18n: I18n,
    pub config_panel: ConfigPanel,
    pub ai_config_panel: AiConfigPanel,
    pub ai_assistant: AiAssistantPanel,
    /// 右侧 SFTP 停靠面板（本地文件浏览）
    pub sftp: SftpPanel,
    pub macros: MacroPanel,
    /// 远程桌面连接档案（所有远程桌面标签页共用）
    pub rdp_profiles: RdpProfiles,
    /// 浏览器共享状态（书签 / 历史 / 搜索引擎）
    pub browser_service: BrowserService,
    /// 下一个浏览器标签 id
    next_browser_id: usize,
    /// 浏览器历史浮层是否展开
    show_browser_history: bool,
    /// 主题是否已按配置初始化（首帧应用，避免每帧 set_visuals）
    theme_initialized: bool,
    /// 会话存储（含最近会话），持久化到 sessions.json
    sessions: SessionStore,
    /// 侧栏当前标签
    sidebar_tab: SidebarTab,
    /// 侧栏选中的会话 id
    selected_session: Option<String>,
    /// 快速连接输入（user@host:22）
    quick_connect: String,
    /// 跳板机输入（ProxyJump: user@host:port）
    jump_host: String,
    /// 快速连接认证方式：password / key
    quick_auth: String,
    /// 快速连接凭据：密码 或 私钥路径
    quick_secret: String,
    /// 会话搜索（侧栏与欢迎页共用）
    search: String,
    /// 新建会话窗口（Some = 打开）
    add_form: Option<AddSessionForm>,
    /// 关于窗口
    about_open: bool,
    /// 临时提示（状态栏显示，操作后更新）
    toast: Option<String>,
    /// 终端查找栏是否展开
    find_open: bool,
    /// 请求下一帧把焦点给浏览器地址栏
    focus_address: bool,
    /// 请求下一帧把焦点给当前对话框的输入框
    focus_dialog: bool,
    /// 上一帧的活动标签，用于切换时收回键盘焦点
    last_active: usize,
    find_text: String,
    /// Telnet 连接表单
    telnet_open: bool,
    telnet_host: String,
    telnet_port: String,
    /// Ping 表单
    ping_open: bool,
    ping_host: String,
    /// 批量执行命令
    batch_open: bool,
    batch_cmd: String,
    /// 同步输入（把活动终端输入镜像到其它终端）
    sync_input: bool,
    sync_rx: Option<std::sync::mpsc::Receiver<Vec<u8>>>,
    sync_tx: Option<std::sync::mpsc::Sender<Vec<u8>>>,
    /// 上一个活动标签的 SSH 会话身份（用于 SFTP 面板自动弹出/收起）
    last_ssh_key: Option<usize>,
    /// 端口隧道
    tunnel_open: bool,
    tunnels: Vec<crate::tunnel::TunnelHandle>,
    tunnel_local: String,
    tunnel_host: String,
    tunnel_port: String,
    tunnel_next_id: u64,
}

impl RustTermApp {
    pub fn new() -> Self {
        let runtime = std::sync::Arc::new(
            tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .expect("tokio runtime"),
        );
        let config = AppConfig::load();
        let mut i18n = I18n::default();
        i18n.set_lang(&config.language);
        Self {
            runtime,
            tabs: Vec::new(),
            active: 0,
            form: ConnectForm::default(),
            dragging: None,
            drag_start: None,
            renaming: None,
            config,
            i18n,
            config_panel: ConfigPanel::default(),
            ai_config_panel: AiConfigPanel::default(),
            ai_assistant: AiAssistantPanel::default(),
            sftp: SftpPanel::default(),
            macros: MacroPanel::default(),
            rdp_profiles: RdpProfiles::default(),
            browser_service: BrowserService::default(),
            next_browser_id: 1,
            show_browser_history: false,
            theme_initialized: false,
            sessions: SessionStore::load(),
            sidebar_tab: SidebarTab::Sessions,
            selected_session: None,
            quick_connect: String::new(),
            jump_host: String::new(),
            quick_auth: "password".to_string(),
            quick_secret: String::new(),
            search: String::new(),
            add_form: None,
            about_open: false,
            toast: None,
            find_open: false,
            focus_address: false,
            focus_dialog: false,
            last_active: 0,
            find_text: String::new(),
            telnet_open: false,
            telnet_host: String::new(),
            telnet_port: "23".to_string(),
            ping_open: false,
            ping_host: String::new(),
            batch_open: false,
            batch_cmd: String::new(),
            sync_input: false,
            sync_rx: None,
            sync_tx: None,
            last_ssh_key: None,
            tunnel_open: false,
            tunnels: Vec::new(),
            tunnel_local: "8080".to_string(),
            tunnel_host: String::new(),
            tunnel_port: "80".to_string(),
            tunnel_next_id: 1,
        }
    }

    // ------------------------------------------------------------------
    // 标签页创建
    // ------------------------------------------------------------------

    fn open_local(&mut self) {
        let title = format!("本地 {}", self.tabs.len() + 1);
        match TermPane::new_local(title, 80, 24) {
            Ok(pane) => {
                self.push_tab(TabContent::Terminal(pane));
            }
            Err(e) => {
                self.toast = Some(format!("本地终端启动失败: {e:#}"));
            }
        }
    }

    fn submit_ssh(
        &mut self,
        host: &str,
        port: u16,
        user: &str,
        auth: crate::backends::ssh::SshAuth,
        jump: Option<crate::backends::ssh::JumpHost>,
    ) {
        let title = match &jump {
            Some(j) => format!("{user}@{host}（经 {}）", j.host),
            None => format!("{user}@{host}"),
        };
        let host = host.trim().to_string();
        let user = user.trim().to_string();
        let runtime = self.runtime.clone();
        let tab_title = format!("{user}@{host}");

        let (tx, rx) = std::sync::mpsc::channel::<anyhow::Result<TermPane>>();
        std::thread::spawn(move || {
            let result = runtime.block_on(TermPane::new_ssh(
                tab_title, host, port, user, auth, jump, 80, 24,
            ));
            let _ = tx.send(result);
        });

        self.push_tab(TabContent::Connecting { title, rx });
    }

    /// 打开一个新的内置浏览器标签页。
    fn open_browser(&mut self, url: &str) {
        let tab = BrowserTab::new(self.next_browser_id, url.to_string());
        self.next_browser_id += 1;
        self.push_tab(TabContent::Browser(tab));
    }

    /// 打开一个新的远程桌面标签页。
    fn open_remote_desktop(&mut self, protocol: RdTabProtocol) {
        let profile = RdpConnectionProfile {
            name: format!("新建 {}", protocol.label()),
            protocol,
            host: String::new(),
            port: protocol.default_port(),
            ..Default::default()
        };
        self.push_tab(TabContent::RemoteDesktop(RemoteDesktopTab::new(profile)));
    }

    /// 切换右侧 SFTP 面板。
    fn open_sftp(&mut self) {
        self.sftp.toggle();
    }

    /// 打开「本地文件」标签页（宽栏）。
    fn open_local_files(&mut self) {
        // 已在标签里则直接切换过去，避免开一堆重复标签
        if let Some(idx) = self
            .tabs
            .iter()
            .position(|t| matches!(t, TabContent::LocalFiles(_)))
        {
            self.active = idx;
            return;
        }
        self.push_tab(TabContent::LocalFiles(crate::filebrowser::FileBrowser::default()));
    }

    /// 追加标签并激活。
    fn push_tab(&mut self, content: TabContent) {
        self.tabs.push(content);
        self.active = self.tabs.len() - 1;
        self.renaming = None;
    }

    /// 解析快速连接输入 "user@host:port" / "host" / "host:port"。
    fn parse_quick_connect(s: &str) -> Option<(Option<String>, String, u16)> {
        let s = s.trim();
        if s.is_empty() {
            return None;
        }
        let (user, rest) = match s.split_once('@') {
            Some((u, r)) => (Some(u.trim().to_string()), r.trim().to_string()),
            None => (None, s.to_string()),
        };
        let (host, port) = match rest.rsplit_once(':') {
            Some((h, p)) => (h.trim().to_string(), p.parse().unwrap_or(22)),
            None => (rest, 22),
        };
        if host.is_empty() {
            None
        } else {
            Some((user, host, port))
        }
    }

    /// 快速连接：解析输入，用快速连接栏的认证方式建立 SSH 会话。
    fn quick_connect_submit(&mut self) {
        let parsed = Self::parse_quick_connect(&self.quick_connect);
        let Some((user, host, port)) = parsed else {
            self.toast = Some("快速连接格式: user@host:22".to_string());
            return;
        };
        let user = user.unwrap_or_else(|| "root".to_string());

        let is_key = self.quick_auth.eq_ignore_ascii_case("key");
        let auth = if is_key {
            crate::backends::ssh::SshAuth::Key {
                path: self.quick_secret.trim().to_string(),
                passphrase: String::new(),
            }
        } else {
            crate::backends::ssh::SshAuth::Password(self.quick_secret.clone())
        };

        // 凭据缺失时提示（密码留空仍允许尝试，交给服务器决定）
        if auth.is_empty() {
            self.toast = Some(if is_key {
                "请先选择或填写私钥路径".to_string()
            } else {
                "未填写密码，将尝试空密码登录".to_string()
            });
            if is_key {
                return;
            }
        }

        // 跳板机：真正的 direct-tcpip 通道（ProxyJump）。凭据沿用当前认证方式。
        let jump = crate::backends::ssh::JumpHost::parse(
            &self.jump_host,
            &user,
            auth.clone(),
        );
        if jump.is_some() {
            self.toast = Some(format!(
                "经跳板机 {} 连接 {host}:{port}",
                jump.as_ref().map(|j| j.host.clone()).unwrap_or_default()
            ));
        }

        self.submit_ssh(&host, port, &user, auth, jump);
    }

    /// 连接已保存的会话（SSH → 终端标签；VNC/RDP/SPICE → 远程桌面标签）。
    fn connect_session(&mut self, id: &str) {
        let Some(s) = self.sessions.by_id(id).cloned() else {
            return;
        };
        self.sessions.touch_recent(&s.id);
        if let Err(e) = self.sessions.save() {
            self.toast = Some(e);
        }
        match s.kind {
            SessionKind::Ssh => {
                let auth = if s.uses_key() {
                    crate::backends::ssh::SshAuth::Key {
                        path: s.key_path.clone(),
                        passphrase: String::new(),
                    }
                } else {
                    crate::backends::ssh::SshAuth::Password(s.password.clone())
                };
                let jump = crate::backends::ssh::JumpHost::parse(
                    &self.jump_host,
                    &s.user,
                    auth.clone(),
                );
                self.submit_ssh(&s.host, s.port, &s.user, auth, jump);
            }
            kind => {
                let proto = match kind {
                    SessionKind::Vnc => RdTabProtocol::Vnc,
                    SessionKind::Rdp => RdTabProtocol::Rdp,
                    SessionKind::Spice => RdTabProtocol::Spice,
                    SessionKind::Ssh => unreachable!(),
                };
                let name = s.display();
                let profile = RdpConnectionProfile {
                    name: name.clone(),
                    protocol: proto,
                    host: s.host.clone(),
                    port: s.port,
                    username: s.user.clone(),
                    password: s.password.clone(),
                    domain: s.domain.clone(),
                    ..Default::default()
                };
                self.rdp_profiles.ensure(profile.clone());
                let mut tab = RemoteDesktopTab::new(profile);
                // 从已保存会话直接点开：立即建连，少一次点击
                tab.connect();
                self.push_tab(TabContent::RemoteDesktop(tab));
            }
        }
    }

    /// 执行工具栏 / 侧栏的动作。
    fn run_tool_action(&mut self, action: ToolAction) {
        match action {
            ToolAction::LocalTerminal => self.open_local(),
            ToolAction::SshClient => {
                self.form.open = true;
                self.form.error = None;
            }
            ToolAction::SftpClient => {
                if self.last_ssh_key.is_some() {
                    self.sftp.open = !self.sftp.open;
                } else {
                    self.toast =
                        Some("SFTP 需要活动的 SSH 会话：先打开一个 SSH 终端标签".to_string());
                }
            }
            ToolAction::TelnetClient => {
                self.telnet_open = true;
                self.focus_dialog = true;
            }
            ToolAction::RdpClient => self.open_remote_desktop(RdTabProtocol::Rdp),
            ToolAction::VncClient => self.open_remote_desktop(RdTabProtocol::Vnc),
            ToolAction::SpiceClient => self.open_remote_desktop(RdTabProtocol::Spice),
            ToolAction::LocalFiles => self.open_local_files(),
            ToolAction::Browser => self.open_browser("about:blank"),
            ToolAction::Ping => {
                self.ping_open = true;
                self.focus_dialog = true;
            }
            ToolAction::Find => {
                self.find_open = !self.find_open;
            }
            ToolAction::Sync => {
                self.toggle_sync_input();
            }
            ToolAction::Batch => {
                self.batch_open = true;
                self.focus_dialog = true;
            }
            ToolAction::Tunnel => {
                self.tunnel_open = true;
            }
            ToolAction::Macro => {
                self.macros.open = !self.macros.open;
            }
        }
    }

    /// 切换「同步输入到所有终端」。
    fn toggle_sync_input(&mut self) {
        self.sync_input = !self.sync_input;
        if self.sync_input {
            let (tx, rx) = std::sync::mpsc::channel::<Vec<u8>>();
            self.sync_tx = Some(tx);
            self.sync_rx = Some(rx);
            self.toast = Some("同步输入已开启：活动终端的按键会复制到其它终端".to_string());
        } else {
            self.sync_tx = None;
            self.sync_rx = None;
            for tab in self.tabs.iter_mut() {
                if let Some(pane) = tab.terminal_mut() {
                    pane.mirror = None;
                }
            }
            self.toast = Some("同步输入已关闭".to_string());
        }
    }

    /// 每帧维护同步输入的镜像通道：只有活动终端挂 mirror，
    /// 再把收到的字节广播给其它终端（用 write_input_local 避免回环）。
    fn pump_sync_input(&mut self) {
        if !self.sync_input {
            return;
        }
        let Some(tx) = self.sync_tx.clone() else {
            return;
        };
        let active = self.active;
        // 只有活动终端作为「源」
        for (i, tab) in self.tabs.iter_mut().enumerate() {
            if let Some(pane) = tab.terminal_mut() {
                if i == active {
                    if pane.mirror.is_none() {
                        pane.mirror = Some(tx.clone());
                    }
                } else if pane.mirror.is_some() {
                    pane.mirror = None;
                }
            }
        }
        // 收集并广播
        let mut batch: Vec<Vec<u8>> = Vec::new();
        if let Some(rx) = &self.sync_rx {
            while let Ok(bytes) = rx.try_recv() {
                batch.push(bytes);
            }
        }
        if batch.is_empty() {
            return;
        }
        for (i, tab) in self.tabs.iter_mut().enumerate() {
            if i == active {
                continue;
            }
            if let Some(pane) = tab.terminal_mut() {
                if pane.dead {
                    continue;
                }
                for b in &batch {
                    pane.write_input_local(b.clone());
                }
            }
        }
    }

    /// 打开 Telnet（原始 TCP）标签页。
    fn open_telnet(&mut self, host: &str, port: u16) {
        let host = host.trim().to_string();
        if host.is_empty() {
            self.toast = Some("Telnet 主机不能为空".to_string());
            return;
        }
        let title = format!("Telnet {host}:{port}");
        match TermPane::new_tcp(title, host.clone(), port, 80, 24) {
            Ok(pane) => {
                self.push_tab(TabContent::Terminal(pane));
                self.toast = Some(format!("已连接 {host}:{port}"));
            }
            Err(e) => {
                self.toast = Some(format!("Telnet 连接失败: {e:#}"));
            }
        }
    }

    /// 打开 Ping 标签页（在 PTY 里跑系统 ping，持续输出）。
    fn open_ping(&mut self, host: &str) {
        let host = host.trim().to_string();
        if host.is_empty() {
            self.toast = Some("Ping 目标不能为空".to_string());
            return;
        }
        #[cfg(windows)]
        let (program, args) = (
            "ping.exe",
            vec!["-t".to_string(), host.clone()],
        );
        #[cfg(not(windows))]
        let (program, args) = ("ping", vec![host.clone()]);

        match TermPane::new_command(format!("Ping {host}"), program, &args, 100, 24) {
            Ok(pane) => {
                self.push_tab(TabContent::Terminal(pane));
            }
            Err(e) => {
                self.toast = Some(format!("Ping 启动失败: {e:#}"));
            }
        }
    }

    /// 把一条命令发送到所有终端标签。
    fn send_batch_command(&mut self) {
        let cmd = self.batch_cmd.trim().to_string();
        if cmd.is_empty() {
            return;
        }
        let mut n = 0usize;
        for tab in self.tabs.iter_mut() {
            if let Some(pane) = tab.terminal_mut() {
                if !pane.dead {
                    pane.write_input_local(format!("{cmd}\r").into_bytes());
                    n += 1;
                }
            }
        }
        self.batch_cmd.clear();
        self.batch_open = false;
        self.toast = Some(format!("已把命令发送到 {n} 个终端"));
    }

    /// 把宏动作发送到当前活动终端。
    fn send_macro_to_active(&mut self, idx: usize) {
        if let Some(bytes) = self.macros.send_macro_bytes(idx) {
            if let Some(pane) = self.tabs.get_mut(self.active).and_then(|t| t.terminal_mut()) {
                if !pane.dead {
                    pane.write_input(bytes);
                    self.toast = Some("宏已发送到当前终端".to_string());
                }
            }
        }
    }

    fn close_tab(&mut self, index: usize) {
        if index < self.tabs.len() {
            // 关闭前销毁底层资源（WebView 子窗口等）
            self.tabs[index].shutdown();
            self.tabs.remove(index);
            self.renaming = None;
            if !self.tabs.is_empty() {
                // 关闭的标签在 active 之前（含 active 本身）时焦点左移一格，否则不动
                self.active = if index <= self.active {
                    self.active.saturating_sub(1).min(self.tabs.len() - 1)
                } else {
                    self.active.min(self.tabs.len() - 1)
                };
            } else {
                self.active = 0;
            }
        }
    }

    /// 把标签从 from 移动到插入位置 ins（ins 为移除 from 后的序列下标）。
    /// active 始终跟随同一个逻辑标签。
    fn move_tab(&mut self, from: usize, ins: usize) {
        if from >= self.tabs.len() || ins >= self.tabs.len() {
            return;
        }
        let mut a = self.active;
        if a == from {
            a = ins;
        } else {
            if a > from {
                a -= 1;
            }
            if a >= ins {
                a += 1;
            }
        }
        let tab = self.tabs.remove(from);
        self.tabs.insert(ins, tab);
        self.active = a;
    }

    /// 轮询 Connecting 标签的连接结果。
    fn poll_connecting(&mut self) {
        for i in 0..self.tabs.len() {
            let mut done: Option<std::result::Result<TermPane, String>> = None;
            if let TabContent::Connecting { rx, .. } = &self.tabs[i] {
                match rx.try_recv() {
                    Ok(Ok(pane)) => done = Some(Ok(pane)),
                    Ok(Err(e)) => done = Some(Err(format!("{e:#}"))),
                    Err(std::sync::mpsc::TryRecvError::Empty) => {}
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                        done = Some(Err("连接线程异常退出".into()))
                    }
                }
            }
            if let Some(result) = done {
                match result {
                    Ok(pane) => self.tabs[i] = TabContent::Terminal(pane),
                    Err(msg) => self.tabs[i] = TabContent::Failed(msg),
                }
            }
        }
    }

    /// 切换主题（工具栏"主题"按钮 / 设置菜单共用）。
    fn toggle_theme(&mut self, ctx: &egui::Context) {
        self.config.theme = if self.config.theme == "dark" {
            "light".to_string()
        } else {
            "dark".to_string()
        };
        match self.config.theme.as_str() {
            "dark" => ctx.set_visuals(egui::Visuals::dark()),
            _ => ctx.set_visuals(egui::Visuals::light()),
        }
        if let Err(e) = self.config.save() {
            self.toast = Some(e);
        }
    }

    /// 切换语言。
    fn toggle_language(&mut self) {
        self.i18n.toggle();
        self.config.language = self.i18n.id().to_string();
        if let Err(e) = self.config.save() {
            self.toast = Some(e);
        }
        // 明确反馈：菜单/工具栏文案会立刻跟着变，状态栏再给一句提示
        self.toast = Some(match self.i18n.lang {
            crate::i18n::Lang::Zh => "界面语言：中文".to_string(),
            crate::i18n::Lang::En => "UI language: English".to_string(),
        });
    }

    /// 工具栏图标按钮（图标在上、文字在下的竖排小按钮）。
    ///
    /// 注意：`Sense::click()` 是必须的——只用 `Sense::hover()` 的控件
    /// 永远不会产生 `clicked()`，按钮看起来能点其实毫无反应。
    fn tool_button(ui: &mut egui::Ui, icon: &str, label: &str) -> egui::Response {
        let (rect, resp) = ui.allocate_exact_size(egui::vec2(52.0, 42.0), egui::Sense::click());
        if resp.hovered() {
            ui.painter().rect_filled(
                rect,
                4.0,
                ui.visuals().widgets.hovered.bg_fill.gamma_multiply(0.6),
            );
        }
        let painter = ui.painter();
        painter.text(
            rect.center() - egui::vec2(0.0, 9.0),
            egui::Align2::CENTER_CENTER,
            icon,
            egui::FontId::proportional(16.0),
            ui.visuals().text_color(),
        );
        painter.text(
            rect.center() + egui::vec2(0.0, 9.0),
            egui::Align2::CENTER_CENTER,
            label,
            egui::FontId::proportional(10.0),
            ui.visuals().weak_text_color(),
        );
        resp.on_hover_cursor(egui::CursorIcon::PointingHand)
    }

    /// 侧栏工具列表项（图标 + 名称，点击打开对应标签页）。
    ///
    /// 同样必须显式 `interact(Sense::click())`，否则 `ui.horizontal`
    /// 返回的 response 不带点击语义。
    fn side_tool_button(ui: &mut egui::Ui, action: ToolAction) -> egui::Response {
        let resp = ui
            .horizontal(|ui| {
                ui.label(egui::RichText::new(action.icon()).size(13.0));
                ui.label(egui::RichText::new(action.label()).size(12.0));
            })
            .response;
        resp.interact(egui::Sense::click())
            .on_hover_cursor(egui::CursorIcon::PointingHand)
    }

    // ------------------------------------------------------------------
    // 各标签页内容渲染
    // ------------------------------------------------------------------

    /// 渲染浏览器标签页：地址栏 + 书签栏 + WebView 区域。
    fn browser_tab_ui(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, tab_index: usize) {
        // 先取出需要的数据，避免与 &mut self.tabs 的借用冲突
        let (url, title, can_back, can_forward) = match &self.tabs[tab_index] {
            TabContent::Browser(b) => (
                b.url.clone(),
                b.title.clone(),
                b.can_go_back,
                b.can_go_forward,
            ),
            _ => return,
        };

        // ---- 导航栏 ----
        ui.horizontal(|ui| {
            if ui
                .add_enabled(can_back, egui::Button::new("◀"))
                .on_hover_text("后退 (Alt+Left)")
                .clicked()
            {
                if let TabContent::Browser(b) = &mut self.tabs[tab_index] {
                    b.go_back_web();
                    let u = b.go_back().unwrap_or_default();
                    b.load_url(&u);
                }
            }
            if ui
                .add_enabled(can_forward, egui::Button::new("▶"))
                .on_hover_text("前进 (Alt+Right)")
                .clicked()
            {
                if let TabContent::Browser(b) = &mut self.tabs[tab_index] {
                    b.go_forward_web();
                    let u = b.go_forward().unwrap_or_default();
                    b.load_url(&u);
                }
            }
            if ui.button("⟳").on_hover_text("刷新 (F5)").clicked() {
                if let TabContent::Browser(b) = &mut self.tabs[tab_index] {
                    b.reload();
                }
            }
            if ui.button("🏠").on_hover_text("主页").clicked() {
                let home = "https://www.bing.com".to_string();
                if let TabContent::Browser(b) = &mut self.tabs[tab_index] {
                    b.navigate(home.clone());
                    b.loading = true;
                    b.load_url(&home);
                }
                self.browser_service.add_to_history(home);
            }

            // 地址栏：必须绑定到标签页自己的编辑缓冲，
            // 否则每帧都会被 url 覆盖，用户输入会被丢弃。
            let mut navigate_to: Option<String> = None;
            let want_focus = std::mem::take(&mut self.focus_address);
            if let TabContent::Browser(b) = &mut self.tabs[tab_index] {
                // 未聚焦时跟随真实 URL（前进/后退/跳转会更新）
                if !b.address_focused && b.address_bar != b.url && !b.loading {
                    b.address_bar = b.url.clone();
                }
                let resp = ui.add(
                    egui::TextEdit::singleline(&mut b.address_bar)
                        .desired_width(f32::INFINITY)
                        .hint_text("输入网址或搜索关键词（回车访问）"),
                );
                // Ctrl+L：聚焦并全选，方便直接覆盖输入
                if want_focus {
                    resp.request_focus();
                    // WebView2 子窗口可能持有系统焦点，必须抢回主窗口，
                    // 否则键盘事件到不了 egui。
                    crate::browser::focus_main_window();
                    if let Some(mut state) =
                        egui::text_edit::TextEditState::load(ui.ctx(), resp.id)
                    {
                        state.cursor.set_char_range(Some(egui::text::CCursorRange::two(
                            egui::text::CCursor::new(0),
                            egui::text::CCursor::new(b.address_bar.chars().count()),
                        )));
                        state.store(ui.ctx(), resp.id);
                    }
                }
                // 地址栏获得焦点期间持续保证系统焦点在主窗口
                if resp.has_focus() && !b.address_focused {
                    crate::browser::focus_main_window();
                }
                b.address_focused = resp.has_focus();
                if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    navigate_to = Some(b.address_bar.clone());
                }
            }

            // 安全图标
            if url.starts_with("https://") {
                ui.label(egui::RichText::new("🔒").size(13.0)).on_hover_text("安全连接");
            } else if url.starts_with("http://") {
                ui.label(egui::RichText::new("⚠").size(13.0)).on_hover_text("不安全连接");
            }

            // 书签 / 历史开关
            if ui.button("🔖").on_hover_text("书签栏").clicked() {
                self.browser_service.show_bookmarks = !self.browser_service.show_bookmarks;
            }
            if ui.button("🕒").on_hover_text("历史记录").clicked() {
                self.show_browser_history = !self.show_browser_history;
            }

            // 书签星标
            let is_bookmarked = self.browser_service.bookmarks.iter().any(|b| b.url == url);
            if ui
                .add(egui::Button::new(if is_bookmarked { "★" } else { "☆" }))
                .on_hover_text(if is_bookmarked { "取消书签" } else { "添加书签" })
                .clicked()
            {
                if is_bookmarked {
                    self.browser_service.bookmarks.retain(|b| b.url != url);
                } else {
                    self.browser_service.bookmarks.push(crate::browser::Bookmark {
                        name: title.clone(),
                        url: url.clone(),
                        folder: "未分类".to_string(),
                    });
                }
            }

            // 真正导航
            if let Some(input) = navigate_to {
                let target = self.browser_service.resolve_address(&input);
                if let TabContent::Browser(b) = &mut self.tabs[tab_index] {
                    b.navigate(target.clone());
                    b.loading = true;
                    b.load_url(&target);
                    b.address_focused = false;
                }
                self.browser_service.add_to_history(target);
            }
        });

        // ---- 历史记录浮层 ----
        if self.show_browser_history {
            let history: Vec<String> = self.browser_service.history.iter().take(50).cloned().collect();
            egui::Frame::new()
                .fill(ui.visuals().panel_fill)
                .stroke(ui.visuals().widgets.noninteractive.bg_stroke)
                .corner_radius(6.0)
                .inner_margin(10.0)
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.strong("历史记录");
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.button("✕").clicked() {
                                self.show_browser_history = false;
                            }
                            if ui.button("清空").clicked() {
                                self.browser_service.history.clear();
                            }
                        });
                    });
                    ui.separator();
                    egui::ScrollArea::vertical()
                        .id_salt("browser_history")
                        .max_height(220.0)
                        .show(ui, |ui| {
                            if history.is_empty() {
                                ui.label(
                                    egui::RichText::new("暂无历史记录")
                                        .size(11.0)
                                        .color(egui::Color32::GRAY),
                                );
                            }
                            for h in &history {
                                if ui.selectable_label(false, h).clicked() {
                                    let target = h.clone();
                                    if let TabContent::Browser(b) = &mut self.tabs[tab_index] {
                                        b.navigate(target.clone());
                                        b.loading = true;
                                        b.load_url(&target);
                                    }
                                    self.show_browser_history = false;
                                }
                            }
                        });
                });
        }

        // ---- 书签栏 ----
        if self.browser_service.show_bookmarks {
            ui.horizontal_wrapped(|ui| {
                ui.label(egui::RichText::new("书签: ").size(11.0).strong());
                let bookmarks = self.browser_service.bookmarks.clone();
                for bm in &bookmarks {
                    if ui
                        .button(&bm.name)
                        .on_hover_text(format!("{}\n分组: {}", bm.url, bm.folder))
                        .clicked()
                    {
                        let target = bm.url.clone();
                        if let TabContent::Browser(b) = &mut self.tabs[tab_index] {
                            b.navigate(target.clone());
                            b.loading = true;
                            b.load_url(&target);
                        }
                        self.browser_service.add_to_history(target);
                    }
                    ui.separator();
                }
            });
        }

        ui.separator();

        // ---- WebView 区域 ----
        let mut rect = ui.allocate_exact_size(ui.available_size(), egui::Sense::hover()).0;

        // 一致性兜底：排查发现 egui 0.36 的 CentralPanel 在「右侧面板可拖宽」时
        // 偶发未完全让位，而 WebView 是原生子窗口、永远绘制在 egui 画布之上，
        // 一旦重叠 AI 助手面板的左缘就会被浏览器画面盖住。
        // 因此把 WebView 区域右缘钳到 AI 面板左缘左侧 2px（同帧面板矩形已知）。
        if self.ai_assistant.open {
            if let Some(panel_rect) = self.ai_assistant.last_layout {
                let new_max_x = rect.max.x.min(panel_rect.min.x - 2.0);
                if new_max_x < rect.max.x {
                    rect = egui::Rect::from_min_max(rect.min, egui::pos2(new_max_x, rect.max.y));
                }
            }
        }

        if let TabContent::Browser(b) = &mut self.tabs[tab_index] {
            b.ensure_webview();
            b.poll_events();

            if b.has_webview() {
                // 真实内容由 WebView 子窗口呈现，这里只画边框并同步边界
                b.set_visible(true);
                b.sync_bounds(ctx, rect);
                ui.painter().rect_stroke(
                    rect,
                    0.0,
                    egui::Stroke::new(1.0, ui.visuals().extreme_bg_color),
                    egui::StrokeKind::Inside,
                );
            } else if let Some(err) = &b.webview_error {
                ui.painter().rect_filled(rect, 0.0, egui::Color32::from_rgb(30, 32, 38));
                ui.painter().text(
                    rect.center(),
                    egui::Align2::CENTER_CENTER,
                    err,
                    egui::FontId::proportional(13.0),
                    egui::Color32::from_rgb(220, 120, 120),
                );
            } else {
                ui.painter().rect_filled(rect, 0.0, egui::Color32::from_rgb(30, 32, 38));
                ui.painter().text(
                    rect.center(),
                    egui::Align2::CENTER_CENTER,
                    "正在创建 WebView…",
                    egui::FontId::proportional(14.0),
                    egui::Color32::GRAY,
                );
            }

            // 加载指示器
            if b.loading {
                ui.painter().rect_filled(
                    egui::Rect::from_min_size(rect.min, egui::vec2(rect.width() * 0.3, 2.0)),
                    0.0,
                    egui::Color32::BLUE,
                );
            }
        }
    }

    /// 渲染远程桌面标签页：工具栏 + 显示区 / 连接表单。
    fn remote_desktop_tab_ui(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, tab_index: usize) {
        // 让出上一帧会话的指针状态：切走时不再转发输入
        if let Some(TabContent::RemoteDesktop(tab)) = self.tabs.get_mut(tab_index) {
            tab.ui(ui, ctx);
        }
    }

    // ------------------------------------------------------------------
    // 欢迎页 / 侧栏
    // ------------------------------------------------------------------

    /// 欢迎页（无标签页时显示）。
    fn welcome_ui(&mut self, ui: &mut egui::Ui) {
        let lang = self.i18n.lang;
        ui.vertical_centered(|ui| {
            ui.add_space(ui.available_height() * 0.12);
            ui.label(
                egui::RichText::new("RustTerm")
                    .size(38.0)
                    .strong()
                    .color(ui.visuals().strong_text_color()),
            );
            ui.add_space(16.0);

            ui.horizontal(|ui| {
                if ui
                    .add(
                        egui::Button::new(tr(lang, "welcome.start_local"))
                            .min_size(egui::vec2(130.0, 32.0)),
                    )
                    .clicked()
                {
                    self.open_local();
                }
                ui.add_space(10.0);
                if ui
                    .add(
                        egui::Button::new(tr(lang, "tb.ssh"))
                            .min_size(egui::vec2(130.0, 32.0)),
                    )
                    .clicked()
                {
                    self.form.open = true;
                    self.form.error = None;
                }
                ui.add_space(10.0);
                if ui
                    .add(
                        egui::Button::new(tr(lang, "item.browser"))
                            .min_size(egui::vec2(130.0, 32.0)),
                    )
                    .clicked()
                {
                    self.open_browser("about:blank");
                }
            });

            ui.add_space(16.0);
            ui.add(
                egui::TextEdit::singleline(&mut self.search)
                    .desired_width(400.0)
                    .hint_text(tr(lang, "welcome.search")),
            );

            ui.add_space(12.0);
            let recent_ids: Vec<String> = self
                .sessions
                .recent
                .iter()
                .filter(|id| {
                    self.sessions
                        .by_id(id)
                        .map(|s| s.matches_search(&self.search))
                        .unwrap_or(false)
                })
                .cloned()
                .collect();
            if !recent_ids.is_empty() {
                egui::Frame::new()
                    .inner_margin(14.0)
                    .stroke(ui.visuals().widgets.noninteractive.bg_stroke)
                    .corner_radius(6.0)
                    .show(ui, |ui| {
                        ui.set_min_width(440.0);
                        ui.label(
                            egui::RichText::new(tr(lang, "welcome.recent"))
                                .size(13.0)
                                .color(ui.visuals().weak_text_color()),
                        );
                        ui.add_space(6.0);
                        let mut connect_id: Option<String> = None;
                        egui::Grid::new("recent_sessions_grid")
                            .num_columns(2)
                            .spacing([40.0, 6.0])
                            .show(ui, |ui| {
                                for (n, id) in recent_ids.iter().enumerate() {
                                    if n % 2 == 0 && n > 0 {
                                        ui.end_row();
                                    }
                                    if let Some(s) = self.sessions.by_id(id) {
                                        if ui
                                            .link(egui::RichText::new(s.display()).size(12.0))
                                            .clicked()
                                        {
                                            connect_id = Some(id.clone());
                                        }
                                    }
                                }
                                ui.end_row();
                            });
                        if let Some(id) = connect_id {
                            self.connect_session(&id);
                        }
                    });
            } else if !self.search.trim().is_empty() {
                ui.label(
                    egui::RichText::new("没有匹配的会话")
                        .size(12.0)
                        .color(ui.visuals().weak_text_color()),
                );
            }
        });
    }

    /// 左侧会话栏（会话 / 工具 / 宏 三个标签页）。
    fn sidebar_ui(&mut self, ui: &mut egui::Ui) {
        let lang = self.i18n.lang;
        egui::Panel::left("session_sidebar")
            .default_size(210.0)
            .resizable(true)
            .frame(
                egui::Frame::new()
                    .inner_margin(egui::Margin::same(6))
                    .fill(ui.visuals().panel_fill),
            )
            .show(ui, |ui| {
                // 顶部标签切换
                ui.horizontal(|ui| {
                    let tabs = [
                        (SidebarTab::Sessions, tr(lang, "side.sessions")),
                        (SidebarTab::Tools, tr(lang, "side.tools")),
                        (SidebarTab::Macros, tr(lang, "side.macros")),
                    ];
                    for (tab, label) in tabs {
                        if ui
                            .selectable_label(self.sidebar_tab == tab, label)
                            .clicked()
                        {
                            self.sidebar_tab = tab;
                        }
                    }
                });
                ui.separator();

                match self.sidebar_tab {
                    SidebarTab::Sessions => self.sidebar_sessions_ui(ui),
                    SidebarTab::Tools => self.sidebar_tools_ui(ui),
                    SidebarTab::Macros => self.sidebar_macros_ui(ui),
                }
            });
    }

    /// 会话标签页：分组树 + 会话列表。
    fn sidebar_sessions_ui(&mut self, ui: &mut egui::Ui) {
        let lang = self.i18n.lang;
        ui.horizontal(|ui| {
            ui.strong(tr(lang, "side.sessions"));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.small_button("＋").on_hover_text("新建会话").clicked() {
                    self.add_form = Some(AddSessionForm {
                        kind: SessionKind::Ssh,
                        name: String::new(),
                        host: String::new(),
                        port: "22".to_string(),
                        user: String::new(),
                        password: String::new(),
                        auth_method: "password".to_string(),
                        key_path: String::new(),
                        domain: String::new(),
                        editing: None,
                    });
                }
                if ui
                    .small_button("⟳")
                    .on_hover_text("刷新（重新加载 sessions.json）")
                    .clicked()
                {
                    self.sessions = SessionStore::load();
                }
            });
        });
        ui.separator();

        let mut connect_id: Option<String> = None;
        let mut delete_id: Option<String> = None;
        let mut edit_id: Option<String> = None;
        egui::ScrollArea::vertical()
            .id_salt("sidebar_sessions")
            .auto_shrink(false)
            .show(ui, |ui| {
                let mut ids: Vec<String> = self
                    .sessions
                    .sessions
                    .iter()
                    .filter(|s| s.matches_search(&self.search))
                    .map(|s| s.id.clone())
                    .collect();
                ids.sort_by(|a, b| {
                    let da = self.sessions.by_id(a).map(|s| s.display()).unwrap_or_default();
                    let db = self.sessions.by_id(b).map(|s| s.display()).unwrap_or_default();
                    da.cmp(&db)
                });
                for id in ids {
                    let Some(s) = self.sessions.by_id(&id) else {
                        continue;
                    };
                    let display = s.display();
                    let kind_tag = match s.kind {
                        SessionKind::Ssh => "⌨",
                        _ => "🖥",
                    };
                    let selected = self.selected_session.as_deref() == Some(id.as_str());
                    let resp = ui.selectable_label(selected, format!("{kind_tag} {display}"));
                    if resp.clicked() {
                        self.selected_session = Some(id.clone());
                    }
                    if resp.double_clicked() {
                        connect_id = Some(id.clone());
                    }
                    resp.context_menu(|ui| {
                        if ui.button("连接").clicked() {
                            connect_id = Some(id.clone());
                            ui.close();
                        }
                        if ui.button("编辑…").clicked() {
                            edit_id = Some(id.clone());
                            ui.close();
                        }
                        if ui.button("删除").clicked() {
                            delete_id = Some(id.clone());
                            ui.close();
                        }
                    });
                }
            });

        if let Some(id) = connect_id {
            self.connect_session(&id);
        }
        if let Some(id) = edit_id {
            // 编辑：把已有会话填进表单（含密码/域，方便补上远程桌面凭据）
            if let Some(s) = self.sessions.by_id(&id).cloned() {
                self.add_form = Some(AddSessionForm {
                    kind: s.kind,
                    name: s.name,
                    host: s.host,
                    port: s.port.to_string(),
                    user: s.user,
                    password: s.password,
                    auth_method: s.auth_method,
                    key_path: s.key_path,
                    domain: s.domain,
                    editing: Some(s.id),
                });
            }
        }
        if let Some(id) = delete_id {
            self.sessions.remove(&id);
            if let Err(e) = self.sessions.save() {
                self.toast = Some(e);
            }
            if self.selected_session.as_deref() == Some(id.as_str()) {
                self.selected_session = None;
            }
        }
    }

    /// 工具标签页：各客户端入口（点击打开对应标签页）。
    fn sidebar_tools_ui(&mut self, ui: &mut egui::Ui) {
        let actions = [
            ToolAction::SshClient,
            ToolAction::SftpClient,
            ToolAction::TelnetClient,
            ToolAction::RdpClient,
            ToolAction::VncClient,
            ToolAction::SpiceClient,
            ToolAction::LocalFiles,
            ToolAction::Browser,
        ];
        let mut pending: Option<ToolAction> = None;
        egui::ScrollArea::vertical()
            .id_salt("sidebar_tools")
            .auto_shrink(false)
            .show(ui, |ui| {
            for action in actions {
                if Self::side_tool_button(ui, action).clicked() {
                    pending = Some(action);
                }
            }
        });
        if let Some(a) = pending {
            self.run_tool_action(a);
        }
    }

    /// 宏标签页：宏列表 + 执行按钮。
    fn sidebar_macros_ui(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.strong("键宏");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.small_button("管理").clicked() {
                    self.macros.open = !self.macros.open;
                }
            });
        });
        ui.separator();

        egui::ScrollArea::vertical()
            .id_salt("sidebar_macros")
            .auto_shrink(false)
            .show(ui, |ui| {
                let entries: Vec<(usize, String, String, bool)> = self
                    .macros
                    .macros
                    .iter()
                    .enumerate()
                    .map(|(i, m)| (i, m.name.clone(), m.description.clone(), m.enabled))
                    .collect();
                let mut exec_idx: Option<usize> = None;
                for (i, name, desc, enabled) in entries {
                    ui.horizontal(|ui| {
                        let label = if enabled {
                            egui::RichText::new(&name).size(12.0)
                        } else {
                            egui::RichText::new(&name).size(12.0).weak()
                        };
                        ui.label(label);
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.add_enabled(enabled, egui::Button::new("▶ 执行").small()).clicked() {
                                exec_idx = Some(i);
                            }
                        });
                    });
                    if !desc.is_empty() {
                        ui.label(
                            egui::RichText::new(&desc)
                                .size(10.0)
                                .color(egui::Color32::GRAY),
                        );
                    }
                    ui.separator();
                }
                if let Some(i) = exec_idx {
                    self.send_macro_to_active(i);
                }
            });
    }

    // ------------------------------------------------------------------
    // 标签条
    // ------------------------------------------------------------------

    /// 顶部标签栏：所有类型标签统一渲染（图标 + 标题 + 关闭按钮 + 拖拽排序）。
    fn tab_strip_ui(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        egui::Panel::top("tab_strip")
            .frame(
                egui::Frame::new()
                    .inner_margin(egui::Margin::symmetric(8, 2))
                    .fill(ui.visuals().extreme_bg_color),
            )
            .show(ui, |ui| {
                ui.add_space(2.0);
                ui.horizontal(|ui| {
                    let mut click_switch: Option<usize> = None;
                    let mut click_close: Option<usize> = None;
                    let mut click_rename: Option<usize> = None;
                    let mut drag_started: Option<usize> = None;
                    let mut tab_rects: Vec<(usize, egui::Rect)> = Vec::new();

                    // 重命名编辑态：预取文本供循环内 TextEdit 使用
                    let edit_index = self.renaming.as_ref().map(|(i, _, _)| *i);
                    let mut edit_text =
                        self.renaming.as_ref().map(|(_, t, _)| t.clone()).unwrap_or_default();
                    let mut rename_need_focus =
                        self.renaming.as_ref().map(|(_, _, f)| *f).unwrap_or(false);
                    let mut rename_done = false;
                    let mut rename_commit: Option<String> = None;

                    egui::ScrollArea::horizontal()
                        .id_salt("tab_scroll")
                        .auto_shrink(false)
                        .show(ui, |ui| {
                            for (i, tab) in self.tabs.iter().enumerate() {
                                // 重命名：把标题替换为编辑框
                                if edit_index == Some(i) {
                                    let resp = ui.add(
                                        egui::TextEdit::singleline(&mut edit_text)
                                            .desired_width(120.0)
                                            .font(egui::TextStyle::Monospace),
                                    );
                                    if rename_need_focus {
                                        resp.request_focus();
                                        rename_need_focus = false;
                                    }
                                    let lost = resp.lost_focus();
                                    let (enter, esc) = ctx.input(|inp| {
                                        (
                                            inp.key_pressed(egui::Key::Enter),
                                            inp.key_pressed(egui::Key::Escape),
                                        )
                                    });
                                    if enter && !esc {
                                        rename_commit = Some(edit_text.clone());
                                        rename_done = true;
                                    } else if lost {
                                        if !esc {
                                            // 点击别处同样提交
                                            rename_commit = Some(edit_text.clone());
                                        }
                                        rename_done = true;
                                    }
                                    tab_rects.push((i, resp.rect));
                                    continue;
                                }

                                let tab_type = tab.tab_type();
                                let is_active = i == self.active;
                                let label = match tab {
                                    TabContent::Connecting { title, .. } => {
                                        format!("⟳ {title}")
                                    }
                                    TabContent::Terminal(pane) => {
                                        if pane.dead {
                                            format!("✕ {}", pane.title)
                                        } else {
                                            pane.title.clone()
                                        }
                                    }
                                    TabContent::Failed(_) => "✕ 连接失败".to_string(),
                                    _ => tab.title(),
                                };
                                let drag_active = edit_index.is_none() && self.dragging == Some(i);

                                // 浏览器标签显示页面标题，其余显示图标 + 标题
                                let label_text = match tab_type {
                                    TabType::Browser => {
                                        egui::RichText::new(format!("🌐 {label}"))
                                    }
                                    TabType::RemoteDesktop => {
                                        egui::RichText::new(format!("🖥 {label}"))
                                    }
                                    TabType::LocalFiles => egui::RichText::new(format!("📁 {label}")),
                                    TabType::Terminal => egui::RichText::new(label),
                                };
                                let styled = match tab {
                                    TabContent::Failed(_) => {
                                        label_text.color(egui::Color32::from_rgb(234, 78, 102))
                                    }
                                    _ if drag_active => {
                                        // 拖拽中原标签半透明，表示已"拿起"
                                        label_text.color(egui::Color32::from_rgba_unmultiplied(
                                            160, 160, 160, 90,
                                        ))
                                    }
                                    _ => label_text,
                                };
                                let resp = ui.selectable_label(is_active, styled);
                                tab_rects.push((i, resp.rect));

                                if resp.drag_started()
                                    && edit_index.is_none()
                                    && self.dragging.is_none()
                                {
                                    drag_started = Some(i);
                                }
                                if resp.clicked() && edit_index.is_none() {
                                    click_switch = Some(i);
                                }
                                if resp.clicked_by(egui::PointerButton::Middle) && !drag_active {
                                    click_close = Some(i);
                                }
                                if resp.double_clicked() && !drag_active {
                                    // 终端标签支持重命名；其它类型暂不支持
                                    if matches!(tab, TabContent::Terminal(_)) {
                                        click_rename = Some(i);
                                    }
                                }

                                // 关闭按钮：激活/悬停时可见（固定占位，避免宽度抖动）
                                let show_close = is_active || resp.hovered() && self.dragging.is_none();
                                let (close_rect, close_r) = ui.allocate_exact_size(
                                    egui::vec2(16.0, 18.0),
                                    egui::Sense::click(),
                                );
                                if show_close && !drag_active {
                                    ui.painter_at(close_rect).text(
                                        close_rect.center(),
                                        egui::Align2::CENTER_CENTER,
                                        "×",
                                        egui::FontId::proportional(12.0),
                                        ui.visuals().text_color(),
                                    );
                                    if close_r.clicked() {
                                        click_close = Some(i);
                                    }
                                }

                                // 浏览器标签悬停显示完整 URL（放在最后，on_hover_text 会消耗 resp）
                                if let TabContent::Browser(b) = tab {
                                    let tip = format!("{}\n（浏览器标签 #{})", b.url, b.id);
                                    resp.on_hover_text(tip);
                                }
                            }
                        });

                    // ---- 拖拽状态应用（含浮动预览绘制） ----
                    let pointer_pos = ctx.input(|inp| inp.pointer.interact_pos());
                    let primary_down = ctx.input(|inp| inp.pointer.primary_down());

                    if let Some(i) = drag_started {
                        self.dragging = Some(i);
                        self.drag_start = pointer_pos;
                    }

                    if let Some(d) = self.dragging {
                        // 浮动预览：标签卡片跟手绘制，覆盖在其他标签之上
                        if let (Some(start), Some(cur)) = (self.drag_start, pointer_pos) {
                            if let Some((_, base)) = tab_rects.iter().find(|(i, _)| *i == d) {
                                let delta = cur.x - start.x;
                                let drawn = base.translate(egui::vec2(delta, 0.0));
                                let title = match &self.tabs[d] {
                                    TabContent::Connecting { title, .. } => format!("⟳ {title}"),
                                    TabContent::Terminal(pane) => {
                                        if pane.dead {
                                            format!("✕ {}", pane.title)
                                        } else {
                                            pane.title.clone()
                                        }
                                    }
                                    _ => self.tabs[d].title(),
                                };
                                // 横向钳制在整条条带范围内
                                let strip_min = tab_rects
                                    .iter()
                                    .map(|(_, r)| r.min.x)
                                    .fold(f32::INFINITY, f32::min);
                                let strip_max = tab_rects
                                    .iter()
                                    .map(|(_, r)| r.max.x)
                                    .fold(f32::NEG_INFINITY, f32::max);
                                let drawn_x = drawn.min.x.clamp(
                                    strip_min.min(base.min.x),
                                    (strip_max.max(base.max.x) - drawn.width()).max(strip_min),
                                );
                                let drawn = drawn.with_min_x(drawn_x);
                                let painter = ui.painter();
                                painter.rect_filled(drawn, 4.0, ui.visuals().selection.bg_fill);
                                painter.text(
                                    drawn.left_center(),
                                    egui::Align2::LEFT_CENTER,
                                    title,
                                    egui::FontId::monospace(14.0),
                                    ui.visuals().strong_text_color(),
                                );
                            }
                        }

                        // 松开即按落点重排
                        if !primary_down {
                            let pivot = pointer_pos.unwrap_or(egui::Pos2::ZERO);
                            let mut others: Vec<(usize, egui::Rect)> = tab_rects
                                .iter()
                                .copied()
                                .filter(|(i, _)| *i != d && edit_index != Some(*i))
                                .collect();
                            others.sort_by(|a, b| a.1.left().total_cmp(&b.1.left()));
                            let ins = others
                                .iter()
                                .filter(|(_, r)| r.center().x < pivot.x)
                                .count();
                            self.move_tab(d, ins);
                            self.dragging = None;
                            self.drag_start = None;
                        }
                    }

                    if let Some(i) = click_switch {
                        self.active = i;
                    }
                    if let Some(i) = click_close {
                        self.close_tab(i);
                    }
                    if let Some(i) = click_rename {
                        if let TabContent::Terminal(pane) = &self.tabs[i] {
                            self.renaming = Some((i, pane.title.clone(), true));
                        }
                    }
                    if rename_done {
                        self.renaming = None;
                    }
                    if let Some(title) = rename_commit {
                        if let Some(i) = edit_index {
                            if let TabContent::Terminal(pane) = &mut self.tabs[i] {
                                let title = title.trim().to_string();
                                if !title.is_empty() {
                                    pane.title = title;
                                }
                            }
                        }
                    } else if let Some(i) = edit_index {
                        // 未提交：回写编辑文本，下一帧继续编辑
                        self.renaming = Some((i, edit_text.clone(), rename_need_focus));
                    }
                });
            });
    }
}

impl eframe::App for RustTermApp {
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();

        self.poll_connecting();
        // 同步输入：维护镜像通道并广播
        self.pump_sync_input();

        // ---- 主题：首帧按配置应用一次 ----
        if !self.theme_initialized {
            self.theme_initialized = true;
            match self.config.theme.as_str() {
                "dark" => ctx.set_visuals(egui::Visuals::dark()),
                "light" => ctx.set_visuals(egui::Visuals::light()),
                _ => {}
            }
        }

        // ---- 全局快捷键处理 ----
        {
            let n = self.tabs.len();
            let mut action: Option<GlobalAction> = None;

            ui.input(|i| {
                let ctrl = i.modifiers.ctrl;
                let shift = i.modifiers.shift;
                let alt = i.modifiers.alt;

                // Ctrl+Tab / Ctrl+Shift+Tab: 切换标签
                if ctrl && !shift && !alt && i.key_pressed(egui::Key::Tab) {
                    action = Some(GlobalAction::NextTab);
                }
                if ctrl && shift && !alt && i.key_pressed(egui::Key::Tab) {
                    action = Some(GlobalAction::PrevTab);
                }

                // Ctrl+1..9: 跳转到指定标签
                if ctrl && !shift && !alt {
                    const NUM_KEYS: [egui::Key; 9] = [
                        egui::Key::Num1, egui::Key::Num2, egui::Key::Num3,
                        egui::Key::Num4, egui::Key::Num5, egui::Key::Num6,
                        egui::Key::Num7, egui::Key::Num8, egui::Key::Num9,
                    ];
                    if let Some(idx) = NUM_KEYS.iter().position(|k| i.key_pressed(*k)).filter(|&k| k < n) {
                        action = Some(GlobalAction::JumpTab(idx));
                    }
                }

                // Ctrl+N: 新建本地终端
                if ctrl && !shift && !alt && i.key_pressed(egui::Key::N) {
                    action = Some(GlobalAction::NewLocalTab);
                }
                // Ctrl+T: 新建浏览器标签页
                if ctrl && !shift && !alt && i.key_pressed(egui::Key::T) {
                    action = Some(GlobalAction::NewBrowserTab);
                }
                // Ctrl+Shift+S: SFTP 标签页
                if ctrl && shift && !alt && i.key_pressed(egui::Key::S) {
                    action = Some(GlobalAction::ToggleSftp);
                }
                // Ctrl+Shift+E: 本地文件标签页
                if ctrl && shift && !alt && i.key_pressed(egui::Key::E) {
                    action = Some(GlobalAction::ToggleLocalFiles);
                }
                // Ctrl+Shift+R: 远程桌面标签页
                if ctrl && shift && !alt && i.key_pressed(egui::Key::R) {
                    action = Some(GlobalAction::ToggleRdp);
                }
                // Ctrl+Shift+B: 浏览器标签页
                if ctrl && shift && !alt && i.key_pressed(egui::Key::B) {
                    action = Some(GlobalAction::ToggleBrowser);
                }
                // Ctrl+Shift+A: AI 助手面板
                if ctrl && shift && !alt && i.key_pressed(egui::Key::A) {
                    action = Some(GlobalAction::ToggleAi);
                }
                // Ctrl+W: 关闭当前标签
                if ctrl && !shift && !alt && i.key_pressed(egui::Key::W) {
                    action = Some(GlobalAction::CloseTab);
                }
                // F11: 全屏切换
                if !ctrl && !shift && !alt && i.key_pressed(egui::Key::F11) {
                    action = Some(GlobalAction::ToggleFullscreen);
                }
                // Ctrl+L: 聚焦浏览器地址栏
                if ctrl && !shift && !alt && i.key_pressed(egui::Key::L) {
                    action = Some(GlobalAction::FocusAddress);
                }
                // Ctrl+F: 终端查找
                if ctrl && !shift && !alt && i.key_pressed(egui::Key::F) {
                    action = Some(GlobalAction::FindInTerminal);
                }
            });

            // 执行动作
            if let Some(act) = action {
                match act {
                    GlobalAction::NextTab => {
                        if n > 0 { self.active = (self.active + 1) % n; }
                    }
                    GlobalAction::PrevTab => {
                        if n > 0 { self.active = (self.active + n - 1) % n; }
                    }
                    GlobalAction::JumpTab(idx) => {
                        self.active = idx;
                    }
                    GlobalAction::NewLocalTab => {
                        self.open_local();
                    }
                    GlobalAction::NewBrowserTab => {
                        self.open_browser("about:blank");
                    }
                    GlobalAction::ToggleSftp => {
                        self.open_sftp();
                    }
                    GlobalAction::ToggleLocalFiles => {
                        self.open_local_files();
                    }
                    GlobalAction::ToggleRdp => {
                        self.open_remote_desktop(RdTabProtocol::Rdp);
                    }
                    GlobalAction::ToggleBrowser => {
                        self.open_browser("about:blank");
                    }
                    GlobalAction::ToggleAi => {
                        self.ai_assistant.open = !self.ai_assistant.open;
                    }
                    GlobalAction::CloseTab => {
                        if !self.tabs.is_empty() {
                            self.close_tab(self.active);
                        }
                    }
                    GlobalAction::ToggleFullscreen => {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(
                            !ctx.input(|i| i.viewport().fullscreen.unwrap_or(false)),
                        ));
                    }
                    GlobalAction::FocusAddress => {
                        if matches!(
                            self.tabs.get(self.active),
                            Some(TabContent::Browser(_))
                        ) {
                            self.focus_address = true;
                        } else {
                            self.toast = Some("Ctrl+L 需要先打开浏览器标签页".to_string());
                        }
                    }
                    GlobalAction::FindInTerminal => {
                        if matches!(
                            self.tabs.get(self.active),
                            Some(TabContent::Terminal(_))
                        ) {
                            self.find_open = true;
                            self.focus_dialog = true;
                        } else {
                            self.toast = Some("Ctrl+F 仅在终端标签页可用".to_string());
                        }
                    }
                }
            }
        }

        // ---- 菜单栏 ----
        let lang = self.i18n.lang;
        egui::Panel::top("menubar")
            .frame(egui::Frame::new().inner_margin(egui::Margin::symmetric(6, 1)).fill(ui.visuals().panel_fill))
            .show(ui, |ui| {
                egui::MenuBar::new().ui(ui, |ui| {
                    ui.menu_button(tr(lang, "menu.terminal"), |ui| {
                        if ui.button(format!("{}\tCtrl+N", tr(lang, "item.local_connect"))).clicked() {
                            self.open_local();
                            ui.close();
                        }
                        if ui.button(tr(lang, "item.ssh_connect")).clicked() {
                            self.form.open = true;
                            self.form.error = None;
                            ui.close();
                        }
                        ui.separator();
                        if ui.button(format!("{}\tCtrl+W", tr(lang, "item.close_tab"))).clicked() {
                            if !self.tabs.is_empty() {
                                self.close_tab(self.active);
                            }
                            ui.close();
                        }
                    });
                    ui.menu_button(tr(lang, "menu.session"), |ui| {
                        // 新建会话放第一项（与终端菜单不再重复）
                        if ui.button(tr(lang, "item.new_session")).clicked() {
                            self.add_form = Some(AddSessionForm {
                                kind: SessionKind::Ssh,
                                name: String::new(),
                                host: String::new(),
                                port: "22".to_string(),
                                user: String::new(),
                                password: String::new(),
                                auth_method: "password".to_string(),
                                key_path: String::new(),
                                domain: String::new(),
                                editing: None,
                            });
                            ui.close();
                        }
                        ui.separator();
                        if ui.button(tr(lang, "item.session_list")).clicked() {
                            self.sidebar_tab = SidebarTab::Sessions;
                            ui.close();
                        }
                        if ui.button(tr(lang, "item.refresh_sessions")).clicked() {
                            self.sessions = SessionStore::load();
                            ui.close();
                        }
                    });
                    ui.menu_button(tr(lang, "menu.view"), |ui| {
                        if ui.button(format!("{}\tCtrl+Shift+B", tr(lang, "item.browser"))).clicked() {
                            self.open_browser("about:blank");
                            ui.close();
                        }
                        if ui.button(format!("{}\tCtrl+Shift+R", tr(lang, "item.remote_desktop"))).clicked() {
                            self.open_remote_desktop(RdTabProtocol::Rdp);
                            ui.close();
                        }
                        if ui.button(format!("{}\tCtrl+Shift+E", tr(lang, "item.local_files"))).clicked() {
                            self.open_local_files();
                            ui.close();
                        }
                        if ui.button(format!("{}\tCtrl+Shift+S", tr(lang, "item.sftp_panel"))).clicked() {
                            self.open_sftp();
                            ui.close();
                        }
                        ui.separator();
                        if ui.button(format!("{}\tCtrl+Shift+A", tr(lang, "item.ai_assistant"))).clicked() {
                            self.ai_assistant.open = !self.ai_assistant.open;
                            ui.close();
                        }
                        // 截屏已并入 AI 面板（面板顶栏的 📷 按钮）
                        ui.separator();
                        if ui.checkbox(&mut self.config.show_status_bar, tr(lang, "item.status_bar")).clicked() {
                            if let Err(e) = self.config.save() {
                                self.toast = Some(e);
                            }
                            ui.close();
                        }
                    });
                    ui.menu_button(tr(lang, "menu.tools"), |ui| {
                        if ui.button(tr(lang, "item.ping")).clicked() {
                            self.run_tool_action(ToolAction::Ping);
                            ui.close();
                        }
                        if ui.button(tr(lang, "item.telnet")).clicked() {
                            self.run_tool_action(ToolAction::TelnetClient);
                            ui.close();
                        }
                        if ui.button(tr(lang, "item.macro_manager")).clicked() {
                            self.run_tool_action(ToolAction::Macro);
                            ui.close();
                        }
                    });
                    ui.menu_button(tr(lang, "menu.settings"), |ui| {
                        if ui.button(tr(lang, "item.config_panel")).clicked() {
                            self.config_panel.open = true;
                            ui.close();
                        }
                        if ui.button(tr(lang, "item.ai_config")).clicked() {
                            self.ai_config_panel.open = true;
                            ui.close();
                        }
                        ui.separator();
                        if ui.button(tr(lang, "item.dark_theme")).clicked() {
                            if self.config.theme != "dark" {
                                self.toggle_theme(&ctx);
                            }
                            ui.close();
                        }
                        if ui.button(tr(lang, "item.light_theme")).clicked() {
                            if self.config.theme != "light" {
                                self.toggle_theme(&ctx);
                            }
                            ui.close();
                        }
                        ui.separator();
                        if ui.button(tr(lang, "common.language_switch")).clicked() {
                            self.toggle_language();
                            ui.close();
                        }
                    });
                    ui.menu_button(tr(lang, "menu.help"), |ui| {
                        if ui.button(tr(lang, "item.about")).clicked() {
                            self.about_open = true;
                            ui.close();
                        }
                    });
                });
            });

        // ---- 工具栏（图标 + 文字竖排按钮） ----
        egui::Panel::top("toolbar")
            .frame(egui::Frame::new().inner_margin(egui::Margin::symmetric(8, 3)).fill(ui.visuals().panel_fill))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    let mut pending: Option<ToolAction> = None;
                    let toolbar_items = [
                        (ToolAction::LocalTerminal, tr(lang, "tb.local")),
                        (ToolAction::SshClient, tr(lang, "tb.ssh")),
                        (ToolAction::Sync, tr(lang, "tb.sync")),
                        (ToolAction::Batch, tr(lang, "tb.batch")),
                        (ToolAction::Tunnel, tr(lang, "tb.tunnel")),
                        (ToolAction::TelnetClient, "Telnet".to_string()),
                        (ToolAction::Ping, "Ping".to_string()),
                        (ToolAction::Find, tr(lang, "tb.find")),
                        (ToolAction::LocalFiles, tr(lang, "tb.files")),
                        (ToolAction::Browser, tr(lang, "tb.browser")),
                    ];
                    for (action, label) in toolbar_items {
                        if Self::tool_button(ui, action.icon(), &label).clicked() {
                            pending = Some(action);
                        }
                    }
                    ui.separator();
                    if Self::tool_button(ui, "🤖", &tr(lang, "tb.ai")).clicked() {
                        self.ai_assistant.open = !self.ai_assistant.open;
                    }
                    if Self::tool_button(ui, "⚙", &tr(lang, "tb.settings")).clicked() {
                        self.config_panel.open = true;
                    }
                    if Self::tool_button(ui, "文", &tr(lang, "tb.language"))
                        .on_hover_text(tr(lang, "common.language_switch"))
                        .clicked()
                    {
                        self.toggle_language();
                    }
                    if Self::tool_button(ui, "◐", &tr(lang, "tb.theme")).clicked() {
                        self.toggle_theme(&ctx);
                    }
                    if Self::tool_button(ui, "?", &tr(lang, "tb.help")).clicked() {
                        self.about_open = true;
                    }
                    if let Some(a) = pending {
                        self.run_tool_action(a);
                    }
                });
            });

        // ---- 快速连接栏 ----
        // 结构：地址 | 跳板机 | 认证方式 | 密码/私钥 | 连接
        egui::Panel::top("quick_connect")
            .frame(egui::Frame::new().inner_margin(egui::Margin::symmetric(8, 4)).fill(ui.visuals().panel_fill))
            .show(ui, |ui| {
                let mut submit = false;
                ui.horizontal(|ui| {
                    // 目标地址
                    let avail = ui.available_width();
                    let quick_resp = ui.add(
                        egui::TextEdit::singleline(&mut self.quick_connect)
                            .desired_width((avail * 0.42).clamp(180.0, 460.0))
                            .hint_text(tr(lang, "qc.hint")),
                    );
                    if quick_resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                        submit = true;
                    }

                    // 跳板机
                    let jump_w = (avail * 0.16).clamp(90.0, 180.0);
                    ui.add(
                        egui::TextEdit::singleline(&mut self.jump_host)
                            .desired_width(jump_w)
                            .hint_text(tr(lang, "qc.jump_hint")),
                    )
                    .on_hover_text(
                        "ProxyJump：优先 user@host:port 写法。\n                         经跳板机的 SSH 连接上开 direct-tcpip 通道到目标机。\n                         凭据沿用右侧选择的认证方式（密码 / 私钥）。",
                    );

                    // 认证方式
                    let is_key = self.quick_auth.eq_ignore_ascii_case("key");
                    egui::ComboBox::from_id_salt("quick_auth_combo")
                        .selected_text(if is_key { tr(lang, "qc.key") } else { tr(lang, "qc.password") })
                        .width(72.0)
                        .show_ui(ui, |ui| {
                            ui.selectable_value(
                                &mut self.quick_auth,
                                "password".to_string(),
                                "密码",
                            );
                            ui.selectable_value(&mut self.quick_auth, "key".to_string(), "密钥");
                        });

                    // 凭据：密码 或 私钥路径
                    let secret_w = (ui.available_width() - 78.0).clamp(120.0, 320.0);
                    if is_key {
                        let resp = ui.add(
                            egui::TextEdit::singleline(&mut self.quick_secret)
                                .desired_width(secret_w)
                                .hint_text("私钥路径 ~/.ssh/id_rsa"),
                        );
                        if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                            submit = true;
                        }
                        if ui.button("…").on_hover_text("选择私钥文件").clicked() {
                            if let Some(p) = rfd::FileDialog::new()
                                .set_title("选择 SSH 私钥")
                                .pick_file()
                            {
                                self.quick_secret = p.to_string_lossy().to_string();
                            }
                        }
                    } else {
                        let resp = crate::widgets::password_field(
                            ui,
                            "quick-password",
                            &mut self.quick_secret,
                            secret_w,
                            "密码（可空，连接后提示）",
                        );
                        if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                            submit = true;
                        }
                    }

                    // 连接按钮
                    if ui
                        .add(
                            egui::Button::new(egui::RichText::new("连接").strong())
                                .min_size(egui::vec2(56.0, 22.0)),
                        )
                        .on_hover_text("建立 SSH 连接（回车亦可）")
                        .clicked()
                    {
                        submit = true;
                    }
                });
                if submit {
                    self.quick_connect_submit();
                }
            });

        // ---- 左侧会话栏 ----
        self.sidebar_ui(ui);

        // ---- 标签栏（所有类型统一） ----
        self.tab_strip_ui(ui, &ctx);

        // ---- 底部状态栏 ----
        // 注意：egui 的面板按 show 顺序依次占用父区域，CentralPanel 必须放在
        // 最后，否则会把底部/右侧面板压缩成 0 高度（状态栏就看不见了）。
        self.status_bar_ui(ui);

        // ---- 右侧 SFTP 面板（最右，绑定当前活动 SSH 会话）----
        {
            let rt = self.runtime.handle().clone();
            let (label, ssh) = match self
                .tabs
                .get(self.active.min(self.tabs.len().saturating_sub(1)))
            {
                Some(TabContent::Terminal(pane)) => (pane.title.clone(), pane.ssh_handle()),
                _ => (String::new(), None),
            };

            // 与参考实现一致：切到 SSH 标签自动弹出 SFTP 面板，
            // 切到非 SSH 标签自动收起。
            let ssh_key = ssh.as_ref().map(|h| std::sync::Arc::as_ptr(h) as *const u8 as usize);
            if ssh_key != self.last_ssh_key {
                self.last_ssh_key = ssh_key;
                if ssh_key.is_some() {
                    self.sftp.open = true;
                    self.sftp.bound_initialized = false;
                } else {
                    self.sftp.open = false;
                }
            }

            self.sftp.bind(&rt, &label, ssh);
            self.sftp.ui(ui, &rt);
        }

        // ---- 右侧 AI 助手面板（SFTP 左侧）----
        let ai_attach_hint = self.active_terminal_hint();
        let sftp_takes_drop = self.sftp.takes_drops(&ctx);
        {
            let RustTermApp {
                ai_assistant,
                config,
                ai_config_panel,
                ..
            } = self;
            ai_assistant.attach_hint = ai_attach_hint;
            ai_assistant.ui(ui, &mut config.ai, &mut ai_config_panel.open, sftp_takes_drop);
        }

        // 用户请求发送 → 附加当前终端上下文后真正发出
        if self.ai_assistant.take_pending_send() {
            let context = self.current_terminal_context();
            let ai = self.config.ai.clone();
            self.ai_assistant.send(&ai, context);
        }

        // ---- 中央工作区（必须放在所有面板之后） ----
        egui::CentralPanel::default()
            .frame(egui::Frame::new().inner_margin(0.0))
            .show(ui, |ui| {
                if self.tabs.is_empty() {
                    self.welcome_ui(ui);
                    return;
                }
                let idx = self.active.min(self.tabs.len() - 1);
                match &mut self.tabs[idx] {
                    TabContent::Terminal(pane) => {
                        // ---- 查找栏 ----
                        if self.find_open {
                            ui.horizontal(|ui| {
                                ui.label("🔍 查找:");
                                let resp = ui.add(
                                    egui::TextEdit::singleline(&mut self.find_text)
                                        .desired_width(240.0)
                                        .hint_text("终端内容（回车下一个，Shift+回车上一个）"),
                                );
                                if self.focus_dialog {
                                    resp.request_focus();
                                    self.focus_dialog = false;
                                }
                                if resp.changed() {
                                    let needle = self.find_text.clone();
                                    pane.set_search(&needle);
                                }
                                let total = pane.search_matches.len();
                                let cur = if total > 0 { pane.search_index + 1 } else { 0 };
                                ui.label(
                                    egui::RichText::new(format!("{cur}/{total}"))
                                        .size(11.0)
                                        .color(ui.visuals().weak_text_color()),
                                );
                                if ui
                                    .add_enabled(total > 0, egui::Button::new("↑"))
                                    .on_hover_text("上一个")
                                    .clicked()
                                {
                                    pane.search_prev();
                                }
                                if ui
                                    .add_enabled(total > 0, egui::Button::new("↓"))
                                    .on_hover_text("下一个")
                                    .clicked()
                                {
                                    pane.search_next();
                                }
                                if ui.button("✕").on_hover_text("关闭查找").clicked() {
                                    pane.clear_search();
                                    self.find_open = false;
                                }

                                let (enter, shift, esc) = ui.input(|i| {
                                    (
                                        i.key_pressed(egui::Key::Enter),
                                        i.modifiers.shift,
                                        i.key_pressed(egui::Key::Escape),
                                    )
                                });
                                if enter {
                                    if shift {
                                        pane.search_prev();
                                    } else {
                                        pane.search_next();
                                    }
                                }
                                if esc {
                                    pane.clear_search();
                                    self.find_open = false;
                                }
                            });
                            ui.separator();
                        }

                        let available = ui.available_size();
                        let dirty = pane.render(&ctx, ui, available, self.config.font_size);
                        if dirty && !pane.dead {
                            ctx.request_repaint();
                        }
                    }
                    TabContent::Connecting { title, .. } => {
                        ui.vertical_centered(|ui| {
                            ui.add_space(80.0);
                            ui.heading(format!("⟳ 正在连接 {title} …"));
                            ui.add_space(6.0);
                            ui.label("SSH 握手与认证进行中（IO 不阻塞 UI）");
                        });
                    }
                    TabContent::Failed(msg) => {
                        ui.vertical_centered(|ui| {
                            ui.add_space(80.0);
                            ui.heading("连接失败");
                            ui.add_space(6.0);
                            ui.colored_label(egui::Color32::from_rgb(234, 78, 102), msg);
                            ui.label("可关闭此标签后重试；新主机已按 accept-new 记入 known_hosts");
                        });
                    }
                    TabContent::Browser(_) => {
                        self.browser_tab_ui(ui, &ctx, idx);
                    }
                    TabContent::RemoteDesktop(_) => {
                        self.remote_desktop_tab_ui(ui, &ctx, idx);
                    }
                    TabContent::LocalFiles(browser) => {
                        browser.ui(ui, false);
                    }
                }
            });

        // 处理 AI 面板的「发送到终端」请求
        if let Some((cmd, dangerous)) = self.ai_assistant.command_request.take() {
            if self.config.ai.read_only {
                self.toast = Some("只读模式：写命令已被禁用".to_string());
            } else if dangerous {
                self.toast = Some(format!("⚠ 危险命令已拦截: {cmd}"));
            } else if let Some(pane) = self.tabs.get_mut(self.active).and_then(|t| t.terminal_mut()) {
                if !pane.dead {
                    pane.write_input(format!("{cmd}\r").into_bytes());
                    self.toast = Some("命令已发送到终端".to_string());
                }
            }
        }

        // ---- SSH 连接对话框 ----
        if self.form.open {
            let mut submit = false;
            let mut win_open = self.form.open;
            egui::Window::new("SSH 连接")
                .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
                .open(&mut win_open) // 标题栏 × 关闭按钮
                .collapsible(false)
                .show(&ctx, |ui| {
                    egui::Grid::new("ssh_form")
                        .num_columns(2)
                        .spacing([8.0, 6.0])
                        .show(ui, |ui| {
                            ui.label("主机");
                            ui.add(egui::TextEdit::singleline(&mut self.form.host));
                            ui.end_row();
                            ui.label("端口");
                            ui.add(egui::TextEdit::singleline(&mut self.form.port));
                            ui.end_row();
                            ui.label("用户");
                            ui.add(egui::TextEdit::singleline(&mut self.form.user));
                            ui.end_row();

                            // 认证方式
                            ui.label("认证");
                            let is_key = self.form.auth_method.eq_ignore_ascii_case("key");
                            egui::ComboBox::from_id_salt("ssh_auth_method")
                                .selected_text(if is_key { tr(lang, "qc.key") } else { tr(lang, "qc.password") })
                                .show_ui(ui, |ui| {
                                    ui.selectable_value(
                                        &mut self.form.auth_method,
                                        "password".to_string(),
                                        "密码",
                                    );
                                    ui.selectable_value(
                                        &mut self.form.auth_method,
                                        "key".to_string(),
                                        "密钥",
                                    );
                                });
                            ui.end_row();

                            if self.form.auth_method.eq_ignore_ascii_case("key") {
                                ui.label("私钥");
                                ui.horizontal(|ui| {
                                    ui.add(
                                        egui::TextEdit::singleline(&mut self.form.key_path)
                                            .desired_width(180.0)
                                            .hint_text("~/.ssh/id_rsa"),
                                    );
                                    if ui.small_button("…").on_hover_text("选择私钥文件").clicked()
                                    {
                                        if let Some(p) = rfd::FileDialog::new()
                                            .set_title("选择 SSH 私钥")
                                            .pick_file()
                                        {
                                            self.form.key_path =
                                                p.to_string_lossy().to_string();
                                        }
                                    }
                                });
                                ui.end_row();
                                ui.label("Passphrase");
                                crate::widgets::password_field(
                                    ui,
                                    "form-passphrase",
                                    &mut self.form.key_passphrase,
                                    260.0,
                                    "私钥有密码时填写",
                                );
                                ui.end_row();
                            } else {
                                ui.label("密码");
                                crate::widgets::password_field(
                                    ui,
                                    "form-password",
                                    &mut self.form.password,
                                    260.0,
                                    "",
                                );
                                ui.end_row();
                            }
                        });
                    if let Some(err) = &self.form.error {
                        ui.colored_label(egui::Color32::from_rgb(234, 78, 102), err);
                    }
                    ui.horizontal(|ui| {
                        submit = ui.button("连接").clicked();
                        if ui.button("取消").clicked() {
                            self.form.open = false;
                        }
                    });
                    // 回车提交 / Esc 取消
                    let (enter, esc) = ctx.input(|i| {
                        (
                            i.key_pressed(egui::Key::Enter),
                            i.key_pressed(egui::Key::Escape),
                        )
                    });
                    if esc {
                        self.form.open = false;
                    } else if enter {
                        submit = true;
                    }
                });
            if !win_open {
                self.form.open = false;
            }
            if submit && self.form.open {
                self.form.error = None;
                let use_key = self.form.auth_method.eq_ignore_ascii_case("key");
                if self.form.host.trim().is_empty() || self.form.user.trim().is_empty() {
                    self.form.error = Some("主机与用户名不能为空".into());
                } else if use_key && self.form.key_path.trim().is_empty() {
                    self.form.error = Some("请选择私钥文件".into());
                } else {
                    let port: u16 = self.form.port.parse().unwrap_or(22);
                    let h = self.form.host.clone();
                    let u = self.form.user.clone();
                    let auth = self.form.to_auth();
                    self.form.open = false;
                    self.form.password = String::new();
                    self.form.key_passphrase = String::new();
                    let jump = crate::backends::ssh::JumpHost::parse(
                        &self.jump_host,
                        &u,
                        auth.clone(),
                    );
                    self.submit_ssh(&h, port, &u, auth, jump);
                }
            }
        }

        // ---- 新建会话窗口 ----
        if let Some(form) = &mut self.add_form {
            let mut save = false;
            let mut connect = false;
            let mut cancel = false;
            let mut win_open = true;
            egui::Window::new(if form.editing.is_some() {
                "编辑会话"
            } else {
                "新建会话"
            })
                .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
                .open(&mut win_open)
                .collapsible(false)
                .show(&ctx, |ui| {
                    egui::Grid::new("add_session_form")
                        .num_columns(2)
                        .spacing([8.0, 6.0])
                        .show(ui, |ui| {
                            ui.label("类型");
                            let old_kind = form.kind;
                            egui::ComboBox::from_id_salt("add_session_kind")
                                .selected_text(form.kind.label())
                                .show_ui(ui, |ui| {
                                    for k in [SessionKind::Ssh, SessionKind::Vnc, SessionKind::Rdp, SessionKind::Spice] {
                                        ui.selectable_value(&mut form.kind, k, k.label());
                                    }
                                });
                            if form.kind != old_kind {
                                form.port = form.kind.default_port().to_string();
                            }
                            ui.end_row();
                            ui.label("名称（可空）");
                            ui.add(egui::TextEdit::singleline(&mut form.name));
                            ui.end_row();
                            ui.label("主机");
                            ui.add(egui::TextEdit::singleline(&mut form.host));
                            ui.end_row();
                            ui.label("端口");
                            ui.add(egui::TextEdit::singleline(&mut form.port));
                            ui.end_row();
                            if form.kind == SessionKind::Ssh {
                                ui.label("用户");
                                ui.add(egui::TextEdit::singleline(&mut form.user));
                                ui.end_row();
                                ui.label("认证");
                                let is_key = form.auth_method.eq_ignore_ascii_case("key");
                                egui::ComboBox::from_id_salt("add_session_auth")
                                    .selected_text(if is_key { tr(lang, "qc.key") } else { tr(lang, "qc.password") })
                                    .show_ui(ui, |ui| {
                                        ui.selectable_value(
                                            &mut form.auth_method,
                                            "password".to_string(),
                                            "密码",
                                        );
                                        ui.selectable_value(
                                            &mut form.auth_method,
                                            "key".to_string(),
                                            "密钥",
                                        );
                                    });
                                ui.end_row();
                                if form.auth_method.eq_ignore_ascii_case("key") {
                                    ui.label("私钥");
                                    ui.horizontal(|ui| {
                                        ui.add(
                                            egui::TextEdit::singleline(&mut form.key_path)
                                                .desired_width(160.0)
                                                .hint_text("~/.ssh/id_rsa"),
                                        );
                                        if ui.small_button("…").clicked() {
                                            if let Some(p) = rfd::FileDialog::new()
                                                .set_title("选择 SSH 私钥")
                                                .pick_file()
                                            {
                                                form.key_path =
                                                    p.to_string_lossy().to_string();
                                            }
                                        }
                                    });
                                    ui.end_row();
                                } else {
                                    ui.label("密码");
                                    crate::widgets::password_field(
                                        ui,
                                        "addform-ssh-password",
                                        &mut form.password,
                                        200.0,
                                        "",
                                    );
                                    ui.end_row();
                                }
                            } else {
                                // 远程桌面：VNC / SPICE 只需要密码，RDP 还要域
                                if form.kind == SessionKind::Rdp {
                                    ui.label("用户");
                                    ui.add(egui::TextEdit::singleline(&mut form.user));
                                    ui.end_row();
                                }
                                ui.label("密码");
                                crate::widgets::password_field(
                                    ui,
                                    "addform-rd-password",
                                    &mut form.password,
                                    200.0,
                                    "",
                                );
                                ui.end_row();
                                if form.kind == SessionKind::Rdp {
                                    ui.label("域");
                                    ui.add(
                                        egui::TextEdit::singleline(&mut form.domain)
                                            .hint_text("可空"),
                                    );
                                    ui.end_row();
                                }
                            }
                        });
                    ui.horizontal(|ui| {
                        connect = ui
                            .button("🔗 连接")
                            .on_hover_text("保存会话并立即连接")
                            .clicked();
                        save = ui.button("保存").clicked();
                        cancel = ui.button("取消").clicked();
                    });
                });
            // 先快照表单数据，结束对 add_form 的借用后再修改
            let snapshot = (save || connect).then(|| {
                (
                    form.kind,
                    form.name.trim().to_string(),
                    form.host.trim().to_string(),
                    form.port.trim().to_string(),
                    form.user.trim().to_string(),
                    form.password.clone(),
                    form.auth_method.clone(),
                    form.key_path.trim().to_string(),
                    form.domain.trim().to_string(),
                    form.editing.clone(),
                )
            });
            if !win_open || cancel {
                self.add_form = None;
            }
            if let Some((
                kind,
                name,
                host,
                port,
                user,
                password,
                auth_method,
                key_path,
                domain,
                editing,
            )) = snapshot
            {
                if host.is_empty() {
                    self.toast = Some("主机不能为空".to_string());
                } else {
                    use crate::sessions::SessionEntry;
                    let mut entry = SessionEntry {
                        id: editing.clone().unwrap_or_else(SessionEntry::new_id),
                        name,
                        kind,
                        host,
                        port: port.parse().unwrap_or_else(|_| kind.default_port()),
                        user,
                        password,
                        auth_method,
                        key_path,
                        domain,
                        group: "默认分组".to_string(),
                    };
                    if entry.name.is_empty() {
                        entry.name = entry.display();
                    }
                    let entry_id = entry.id.clone();
                    if editing.is_some() {
                        self.sessions.update(entry);
                    } else {
                        self.sessions.sessions.push(entry);
                    }
                    let save_err = self.sessions.save().err();
                    self.add_form = None;
                    // 「🔗 连接」：保存后立即按新会话发起连接
                    if connect {
                        self.connect_session(&entry_id);
                    }
                    let msg = if connect {
                        "会话已保存并连接".to_string()
                    } else if editing.is_some() {
                        "会话已更新".to_string()
                    } else {
                        "会话已保存".to_string()
                    };
                    self.toast = Some(save_err.unwrap_or(msg));
                }
            }
        }

        // ---- Telnet 对话框 ----
        if self.telnet_open {
            let mut win_open = self.telnet_open;
            let mut submit = false;
            egui::Window::new("Telnet 连接")
                .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
                .open(&mut win_open)
                .collapsible(false)
                .resizable(false)
                .show(&ctx, |ui| {
                    egui::Grid::new("telnet_form")
                        .num_columns(2)
                        .spacing([8.0, 6.0])
                        .show(ui, |ui| {
                            ui.label("主机");
                            let host_resp = ui.add(
                                egui::TextEdit::singleline(&mut self.telnet_host)
                                    .desired_width(220.0)
                                    .hint_text("192.168.1.1"),
                            );
                            if self.focus_dialog {
                                host_resp.request_focus();
                                self.focus_dialog = false;
                            }
                            ui.end_row();
                            ui.label("端口");
                            ui.add(
                                egui::TextEdit::singleline(&mut self.telnet_port)
                                    .desired_width(220.0),
                            );
                            ui.end_row();
                        });
                    ui.label(
                        egui::RichText::new("使用原始 TCP + 最小 Telnet 协商，适合网络设备 CLI")
                            .size(10.0)
                            .color(ui.visuals().weak_text_color()),
                    );
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        submit = ui.button("连接").clicked();
                        if ui.button("取消").clicked() {
                            self.telnet_open = false;
                        }
                    });
                    let enter = ctx.input(|i| i.key_pressed(egui::Key::Enter));
                    if enter {
                        submit = true;
                    }
                });
            if !win_open {
                self.telnet_open = false;
            }
            if submit {
                let port: u16 = self.telnet_port.trim().parse().unwrap_or(23);
                let host = self.telnet_host.clone();
                self.telnet_open = false;
                self.open_telnet(&host, port);
            }
        }

        // ---- Ping 对话框 ----
        if self.ping_open {
            let mut win_open = self.ping_open;
            let mut submit = false;
            egui::Window::new("Ping")
                .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
                .open(&mut win_open)
                .collapsible(false)
                .resizable(false)
                .show(&ctx, |ui| {
                    ui.horizontal(|ui| {
                        ui.label("目标");
                        let resp = ui.add(
                            egui::TextEdit::singleline(&mut self.ping_host)
                                .desired_width(240.0)
                                .hint_text("域名或 IP"),
                        );
                        if self.focus_dialog {
                            resp.request_focus();
                            self.focus_dialog = false;
                        }
                    });
                    ui.label(
                        egui::RichText::new("在新标签页里持续 ping（Ctrl+C 停止）")
                            .size(10.0)
                            .color(ui.visuals().weak_text_color()),
                    );
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        if ui.button("开始").clicked() {
                            submit = true;
                        }
                        if ui.button("取消").clicked() {
                            self.ping_open = false;
                        }
                    });
                    // 回车直接开始
                    if ctx.input(|i| i.key_pressed(egui::Key::Enter)) {
                        submit = true;
                    }
                });
            if !win_open {
                self.ping_open = false;
            }
            if submit {
                let host = self.ping_host.clone();
                self.ping_open = false;
                self.open_ping(&host);
            }
        }

        // ---- 批量执行 ----
        if self.batch_open {
            let mut win_open = self.batch_open;
            let mut submit = false;
            egui::Window::new("批量执行命令")
                .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
                .open(&mut win_open)
                .collapsible(false)
                .default_width(420.0)
                .show(&ctx, |ui| {
                    let terminals = self
                        .tabs
                        .iter()
                        .filter(|t| matches!(t, TabContent::Terminal(_)))
                        .count();
                    ui.label(format!("将发送到 {terminals} 个终端标签"));
                    ui.add_space(4.0);
                    let resp = ui.add(
                        egui::TextEdit::singleline(&mut self.batch_cmd)
                            .desired_width(f32::INFINITY)
                            .hint_text("例如: uptime"),
                    );
                    if self.focus_dialog {
                        resp.request_focus();
                        self.focus_dialog = false;
                    }
                    if ctx.input(|i| i.key_pressed(egui::Key::Enter)) {
                        submit = true;
                    }
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        if ui
                            .add_enabled(terminals > 0, egui::Button::new("发送"))
                            .clicked()
                        {
                            submit = true;
                        }
                        if ui.button("取消").clicked() {
                            self.batch_open = false;
                        }
                    });
                });
            if !win_open {
                self.batch_open = false;
            }
            if submit {
                self.send_batch_command();
            }
        }

        // ---- 端口隧道 ----
        if self.tunnel_open {
            let mut win_open = self.tunnel_open;
            egui::Window::new("端口隧道")
                .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
                .open(&mut win_open)
                .collapsible(false)
                .default_width(520.0)
                .show(&ctx, |ui| {
                    ui.label(
                        egui::RichText::new(
                            "本地端口 → 目标主机端口（直连 TCP 转发）",
                        )
                        .size(11.0)
                        .color(ui.visuals().weak_text_color()),
                    );
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        ui.label("本地端口");
                        ui.add(
                            egui::TextEdit::singleline(&mut self.tunnel_local)
                                .desired_width(70.0),
                        );
                        ui.label("→");
                        ui.add(
                            egui::TextEdit::singleline(&mut self.tunnel_host)
                                .desired_width(150.0)
                                .hint_text("目标主机"),
                        );
                        ui.label(":");
                        ui.add(
                            egui::TextEdit::singleline(&mut self.tunnel_port)
                                .desired_width(60.0),
                        );
                        if ui.button("启动").clicked() {
                            let lp: u16 = self.tunnel_local.trim().parse().unwrap_or(0);
                            let tp: u16 = self.tunnel_port.trim().parse().unwrap_or(0);
                            let host = self.tunnel_host.trim().to_string();
                            if host.is_empty() || tp == 0 {
                                self.toast = Some("请填写目标主机与端口".to_string());
                            } else {
                                let id = self.tunnel_next_id;
                                match crate::tunnel::start(id, lp, &host, tp) {
                                    Ok(h) => {
                                        self.tunnel_next_id += 1;
                                        self.toast = Some(format!(
                                            "隧道已启动: 127.0.0.1:{} → {host}:{tp}",
                                            h.local_port
                                        ));
                                        self.tunnels.push(h);
                                    }
                                    Err(e) => {
                                        self.toast = Some(format!("隧道启动失败: {e}"));
                                    }
                                }
                            }
                        }
                    });
                    ui.separator();
                    if self.tunnels.is_empty() {
                        ui.label(
                            egui::RichText::new("暂无隧道")
                                .size(11.0)
                                .color(ui.visuals().weak_text_color()),
                        );
                    }
                    let mut stop_id: Option<u64> = None;
                    egui::Grid::new("tunnel_grid")
                        .num_columns(5)
                        .striped(true)
                        .spacing([10.0, 5.0])
                        .show(ui, |ui| {
                            ui.strong("监听");
                            ui.strong("目标");
                            ui.strong("连接数");
                            ui.strong("状态");
                            ui.strong("");
                            ui.end_row();
                            for t in &self.tunnels {
                                ui.label(format!("127.0.0.1:{}", t.local_port));
                                ui.label(format!("{}:{}", t.target_host, t.target_port));
                                ui.label(t.connections().to_string());
                                let (text, color) = if let Some(e) = t.error() {
                                    (format!("错误: {e}"), egui::Color32::from_rgb(234, 78, 102))
                                } else if t.running() {
                                    ("运行中".to_string(), egui::Color32::from_rgb(69, 197, 112))
                                } else {
                                    ("已停止".to_string(), egui::Color32::GRAY)
                                };
                                ui.label(egui::RichText::new(text).size(10.0).color(color));
                                if ui.small_button("停止").clicked() {
                                    stop_id = Some(t.id);
                                }
                                ui.end_row();
                            }
                        });
                    if let Some(id) = stop_id {
                        if let Some(t) = self.tunnels.iter().find(|t| t.id == id) {
                            t.stop();
                        }
                        self.tunnels.retain(|t| t.id != id);
                        self.toast = Some("隧道已停止".to_string());
                    }
                });
            if !win_open {
                self.tunnel_open = false;
            }
        }

        // ---- 关于窗口 ----
        if self.about_open {
            let mut win_open = self.about_open;
            egui::Window::new("关于 RustTerm")
                .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
                .open(&mut win_open)
                .collapsible(false)
                .resizable(false)
                .show(&ctx, |ui| {
                    ui.vertical_centered(|ui| {
                        ui.heading("RustTerm");
                        ui.label("全量纯 Rust 终端客户端");
                        ui.add_space(6.0);
                        ui.label(format!("版本: {} ", env!("CARGO_PKG_VERSION")));
                        ui.label("架构: egui + alacritty_terminal + russh");
                    });
                });
            if !win_open {
                self.about_open = false;
            }
        }

        // ---- 浮动面板（Window 方式） ----
        // 键宏管理器窗口
        if let Some(bytes) = self.macros.ui(&ctx) {
            if let Some(pane) = self.tabs.get_mut(self.active).and_then(|t| t.terminal_mut()) {
                if !pane.dead {
                    pane.write_input(bytes);
                }
            }
        }
        if let Some(e) = self.config_panel.ui(ui, &mut self.config, &mut self.i18n) {
            self.toast = Some(e);
        }
        if let Some(msg) = self.ai_config_panel.ui(&ctx, &mut self.config) {
            self.toast = Some(msg);
        }

        // WebView 可见性同步放在最后：需要本帧所有浮层的开关状态
        self.sync_browser_visibility();

        let _ = frame;
    }
}

impl RustTermApp {
    /// 当前活动终端的概要（SSH user@host / 本地终端…），供 AI 面板展示「会自动附带什么」。
    fn active_terminal_hint(&self) -> Option<String> {
        let tab = self.tabs.get(self.active)?;
        let pane = tab.terminal()?;
        Some(match &pane.spawn {
            SpawnCfg::Ssh { host, port, user, .. } => {
                format!("SSH {user}@{host}:{port}")
            }
            SpawnCfg::Tcp { host, port } => format!("Telnet {host}:{port}"),
            SpawnCfg::Command { program, .. } => format!("命令 {program}"),
            SpawnCfg::Local => "本地终端".to_string(),
        })
    }

    /// 取当前活动终端的最近输出，作为 AI 对话的上下文。
    ///
    /// 除屏幕内容外还附上「终端信息」头（本地 / SSH user@host / Telnet），
    /// 让 AI 知道自己在什么环境下给命令（Windows 本机 vs 远端 Linux 等）。
    fn current_terminal_context(&self) -> Option<String> {
        let tab = self.tabs.get(self.active)?;
        let pane = tab.terminal()?;
        let text = pane.context_text(
            self.config.ai.context_lines,
            self.config.ai.context_max_line_len,
        );
        if text.trim().is_empty() {
            return None;
        }
        let header = match &pane.spawn {
            SpawnCfg::Ssh { host, port, user, .. } => {
                format!(
                    "【终端信息】SSH 连接 {}@{}:{}（远端）\n",
                    user, host, port
                )
            }
            SpawnCfg::Tcp { host, port } => {
                format!("【终端信息】Telnet 连接 {}:{}（远端）\n", host, port)
            }
            SpawnCfg::Command { program, .. } => {
                format!("【终端信息】本地运行命令 {program} 的输出\n")
            }
            SpawnCfg::Local => "【终端信息】本地终端\n".to_string(),
        };
        Some(format!("{header}{}", text))
    }

    /// 是否有 egui 浮层（对话框 / 面板）正在显示。
    ///
    /// WebView 是原生子窗口，永远绘制在 egui 画布之上，
    /// 所以只要有任何对话框打开，就必须先把 WebView 藏起来，
    /// 否则对话框会被浏览器画面整个盖住。
    fn overlay_open(&self) -> bool {
        self.config_panel.open
            || self.ai_config_panel.open
            || self.form.open
            || self.add_form.is_some()
            || self.about_open
            || self.telnet_open
            || self.ping_open
            || self.batch_open
            || self.tunnel_open
            || self.macros.open
    }

    /// 只有活动标签页的 WebView 可见，其余隐藏（否则会覆盖终端画面）。
    fn sync_browser_visibility(&mut self) {
        let active = self.active;
        // 有浮层时整体隐藏浏览器画面，让对话框可见
        let allow_visible = !self.overlay_open();
        for (i, tab) in self.tabs.iter().enumerate() {
            if let TabContent::Browser(b) = tab {
                b.set_visible(allow_visible && i == active);
            }
        }
        // 从浏览器标签切走时，把系统焦点从 WebView 收回主窗口，
        // 否则终端收不到键盘输入。
        if self.last_active != active {
            if !matches!(self.tabs.get(active), Some(TabContent::Browser(_))) {
                crate::browser::focus_main_window();
            }
            self.last_active = active;
        }
    }

    /// 底部状态栏：会话数 / 连接状态 / 当前标签信息 / 行列 / AI 状态。
    fn status_bar_ui(&mut self, ui: &mut egui::Ui) {
        if !self.config.show_status_bar {
            return;
        }
        let lang = self.i18n.lang;
        egui::Panel::bottom("status_bar")
            .frame(egui::Frame::new().inner_margin(egui::Margin::symmetric(8, 2)).fill(ui.visuals().panel_fill))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    // 左侧：当前打开的内容
                    let current = if self.tabs.is_empty() {
                        tr(lang, "status.none")
                    } else {
                        let t = &self.tabs[self.active.min(self.tabs.len() - 1)];
                        format!("{} [{}] {}", t.tab_type().icon(), t.tab_type().label(), t.title())
                    };
                    ui.label(
                        egui::RichText::new(format!("{}: {current}", tr(lang, "status.opened")))
                            .size(11.0)
                            .color(ui.visuals().weak_text_color()),
                    );
                    ui.separator();

                    // 连接状态
                    let (status_text, status_color) = if self.tabs.is_empty() {
                        (format!("● {}", tr(lang, "status.none")), egui::Color32::GRAY)
                    } else {
                        match &self.tabs[self.active.min(self.tabs.len() - 1)] {
                            TabContent::Terminal(pane) => {
                                if pane.dead {
                                    (format!("● {}", tr(lang, "status.disconnected")), egui::Color32::RED)
                                } else {
                                    (format!("● {}", tr(lang, "status.connected")), egui::Color32::GREEN)
                                }
                            }
                            TabContent::Connecting { .. } => {
                                (format!("◐ {}", tr(lang, "status.connecting")), egui::Color32::YELLOW)
                            }
                            TabContent::Failed(_) => {
                                (format!("● {}", tr(lang, "status.failed")), egui::Color32::RED)
                            }
                            TabContent::Browser(_) => {
                                (format!("● {}", tr(lang, "status.browser")), egui::Color32::from_rgb(86, 143, 238))
                            }
                            TabContent::RemoteDesktop(t) => {
                                if t.is_active() {
                                    (format!("● {}", tr(lang, "status.remote_active")), egui::Color32::GREEN)
                                } else if t.is_connecting() {
                                    (format!("◐ {}", tr(lang, "status.remote_connecting")), egui::Color32::YELLOW)
                                } else if t.error().is_some() {
                                    (format!("● {}", tr(lang, "status.failed")), egui::Color32::RED)
                                } else {
                                    (format!("○ {}", tr(lang, "status.remote_idle")), egui::Color32::GRAY)
                                }
                            }
                            TabContent::LocalFiles(_) => {
                                (format!("● {}", tr(lang, "status.files")), egui::Color32::from_rgb(150, 190, 120))
                            }
                        }
                    };
                    ui.label(egui::RichText::new(status_text).size(11.0).color(status_color));
                    ui.add_space(4.0);
                    ui.separator();

                    // 终端专属信息
                    if let Some(TabContent::Terminal(pane)) =
                        self.tabs.get(self.active.min(self.tabs.len().saturating_sub(1)))
                    {
                        ui.label(
                            egui::RichText::new(format!("{}×{}", pane.dims.cols, pane.dims.rows))
                                .size(10.0)
                                .color(egui::Color32::GRAY),
                        );
                        ui.add_space(4.0);
                        ui.separator();
                        let content = pane.term.renderable_content();
                        let cursor_row = content.cursor.point.line.0 + content.display_offset as i32 + 1;
                        let cursor_col = content.cursor.point.column.0 + 1;
                        ui.label(
                            egui::RichText::new(format!("行:{}, 列:{}", cursor_row, cursor_col))
                                .size(10.0)
                                .color(egui::Color32::GRAY),
                        );
                        ui.add_space(4.0);
                        if content.display_offset > 0 {
                            ui.label(
                                egui::RichText::new(format!("↑ 回滚: {}", content.display_offset))
                                    .size(10.0)
                                    .color(egui::Color32::YELLOW),
                            );
                            ui.add_space(4.0);
                        }
                        if let Some((a, b)) = normalized_selection(&pane.selection) {
                            let lines = (b.line.0 - a.line.0 + 1) as usize;
                            ui.label(
                                egui::RichText::new(format!("选区: {lines} 行"))
                                    .size(10.0)
                                    .color(egui::Color32::from_rgb(58, 110, 190)),
                            );
                            ui.add_space(4.0);
                        }
                    } else if !self.tabs.is_empty() {
                        ui.label(
                            egui::RichText::new(format!(
                                "标签: {}/{}",
                                self.active + 1,
                                self.tabs.len()
                            ))
                            .size(11.0)
                            .color(egui::Color32::GRAY),
                        );
                        ui.add_space(4.0);
                    }

                    // 会话总数
                    ui.separator();
                    ui.label(
                        egui::RichText::new(format!("会话 {}", self.sessions.sessions.len()))
                            .size(10.0)
                            .color(egui::Color32::GRAY),
                    );

                    // 临时提示
                    if let Some(t) = &self.toast {
                        ui.separator();
                        ui.label(
                            egui::RichText::new(format!("ℹ {t}"))
                                .size(11.0)
                                .color(ui.visuals().warn_fg_color),
                        );
                    }

                    // 右侧：AI 状态 + 快捷键提示
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(
                            egui::RichText::new(format!(
                                "AI {} · {}",
                                self.config.ai.provider().label(),
                                self.config.ai.model
                            ))
                            .size(10.0)
                            .color(egui::Color32::GRAY),
                        );
                        ui.label(
                            egui::RichText::new("Ctrl+N 新建 | Ctrl+T 浏览器 | Ctrl+W 关闭")
                                .size(10.0)
                                .color(egui::Color32::GRAY),
                        );
                    });
                });
            });
    }
}
