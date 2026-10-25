//! 统一标签页模型。
//!
//! 主界面的标签栏同时承载四种会话，与本地 / SSH 终端平级：
//! - `Terminal`：本地终端或 SSH 终端
//! - `Browser`：内置浏览器（独立 WebView2 子窗口）
//! - `RemoteDesktop`：VNC / RDP / SPICE 远程桌面
//! - `LocalFiles`：本地文件浏览器（宽栏）
//!
//! SFTP 不在标签页里：它是右侧停靠面板（见 `sftp.rs`）。
//!
//! 连接中的 SSH 标签用 `Connecting` 表示（结果经独立线程送回），
//! 失败用 `Failed` 保留标签供查看错误。

use crate::browser::BrowserTab;
use crate::filebrowser::FileBrowser;
use crate::rdp::RemoteDesktopTab;
use crate::term::TermPane;

/// 标签页类别。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TabType {
    Terminal,
    Browser,
    RemoteDesktop,
    LocalFiles,
}

impl TabType {
    /// 标签栏左侧图标。
    pub fn icon(&self) -> &'static str {
        match self {
            TabType::Terminal => "🖥",
            TabType::Browser => "🌐",
            TabType::RemoteDesktop => "🖥",
            TabType::LocalFiles => "📁",
        }
    }

    /// 状态栏 / 提示用的中文名。
    pub fn label(&self) -> &'static str {
        match self {
            TabType::Terminal => "终端",
            TabType::Browser => "浏览器",
            TabType::RemoteDesktop => "远程桌面",
            TabType::LocalFiles => "本地文件",
        }
    }
}

/// 标签页内容。
pub enum TabContent {
    /// SSH 连接中（结果经线程送回）
    Connecting {
        title: String,
        rx: std::sync::mpsc::Receiver<anyhow::Result<TermPane>>,
    },
    /// 终端（本地 / SSH）
    Terminal(TermPane),
    /// 连接 / 认证失败（保留 tab 供查看错误）
    Failed(String),
    /// 内置浏览器
    Browser(BrowserTab),
    /// 远程桌面
    RemoteDesktop(RemoteDesktopTab),
    /// 本地文件浏览器（宽栏）
    LocalFiles(FileBrowser),
}

impl TabContent {
    pub fn tab_type(&self) -> TabType {
        match self {
            TabContent::Connecting { .. } | TabContent::Terminal(_) | TabContent::Failed(_) => {
                TabType::Terminal
            }
            TabContent::Browser(_) => TabType::Browser,
            TabContent::RemoteDesktop(_) => TabType::RemoteDesktop,
            TabContent::LocalFiles(_) => TabType::LocalFiles,
        }
    }

    /// 标签栏显示的标题。
    pub fn title(&self) -> String {
        match self {
            TabContent::Connecting { title, .. } => title.clone(),
            TabContent::Terminal(pane) => pane.title.clone(),
            TabContent::Failed(_) => "连接失败".to_string(),
            TabContent::Browser(tab) => tab.display_title(),
            TabContent::RemoteDesktop(tab) => tab.title(),
            TabContent::LocalFiles(_) => "本地文件".to_string(),
        }
    }

    /// 终端面板（仅 Terminal 返回）。
    pub fn terminal(&self) -> Option<&TermPane> {
        match self {
            TabContent::Terminal(pane) => Some(pane),
            _ => None,
        }
    }

    pub fn terminal_mut(&mut self) -> Option<&mut TermPane> {
        match self {
            TabContent::Terminal(pane) => Some(pane),
            _ => None,
        }
    }

    /// 释放底层资源（关闭标签时调用，确保 WebView 子窗口被销毁）。
    pub fn shutdown(&mut self) {
        if let TabContent::Browser(tab) = self {
            tab.shutdown();
        }
    }
}
