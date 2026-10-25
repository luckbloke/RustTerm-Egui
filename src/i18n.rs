//! 界面文案（中/英）。
//!
//! 只覆盖「界面骨架」：菜单、工具栏、侧栏、欢迎页、状态栏、快速连接栏。
//! 对话框正文与终端输出不翻译（终端输出本来就是远端内容）。
//!
//! 用 `tr(lang, key)` 而不是 HashMap：条目集中在一处，改文案时不用两头对齐。

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Lang {
    Zh,
    En,
}

impl Lang {
    pub fn id(self) -> &'static str {
        match self {
            Lang::Zh => "zh-CN",
            Lang::En => "en-US",
        }
    }

    pub fn from_id(s: &str) -> Self {
        if s.starts_with("en") {
            Lang::En
        } else {
            Lang::Zh
        }
    }

    pub fn toggled(self) -> Self {
        match self {
            Lang::Zh => Lang::En,
            Lang::En => Lang::Zh,
        }
    }
}

#[derive(Clone, Copy)]
pub struct I18n {
    pub lang: Lang,
}

impl Default for I18n {
    fn default() -> Self {
        Self { lang: Lang::Zh }
    }
}

impl I18n {
    pub fn set_lang(&mut self, id: &str) {
        self.lang = Lang::from_id(id);
    }

    pub fn id(&self) -> &'static str {
        self.lang.id()
    }

    pub fn toggle(&mut self) {
        self.lang = self.lang.toggled();
    }

    #[allow(dead_code)]
    pub fn t(&self, key: &str) -> String {
        tr(self.lang, key)
    }
}

/// 取文案。未知 key 原样返回（便于发现漏配）。
pub fn tr(lang: Lang, key: &str) -> String {
    use Lang::{En, Zh};
    let s = match (lang, key) {
        // ---- 菜单栏 ----
        (Zh, "menu.terminal") => "终端",
        (En, "menu.terminal") => "Terminal",
        (Zh, "menu.session") => "会话",
        (En, "menu.session") => "Session",
        (Zh, "menu.view") => "视图",
        (En, "menu.view") => "View",
        (Zh, "menu.tools") => "工具",
        (En, "menu.tools") => "Tools",
        (Zh, "menu.settings") => "设置",
        (En, "menu.settings") => "Settings",
        (Zh, "menu.help") => "帮助",
        (En, "menu.help") => "Help",
        (Zh, "menu.x11") => "X 服务",
        (En, "menu.x11") => "X Service",

        // ---- 终端菜单 ----
        (Zh, "item.local_connect") => "本地连接",
        (En, "item.local_connect") => "Local Connection",
        (Zh, "item.ssh_connect") => "SSH 连接...",
        (En, "item.ssh_connect") => "SSH Connection...",
        (Zh, "item.close_tab") => "关闭当前标签",
        (En, "item.close_tab") => "Close Tab",

        // ---- 会话菜单 ----
        (Zh, "item.new_session") => "新建会话...",
        (En, "item.new_session") => "New Session...",
        (Zh, "item.session_list") => "会话列表",
        (En, "item.session_list") => "Session List",
        (Zh, "item.refresh_sessions") => "刷新会话列表",
        (En, "item.refresh_sessions") => "Refresh Sessions",

        // ---- 视图菜单 ----
        (Zh, "item.browser") => "内置浏览器",
        (En, "item.browser") => "Built-in Browser",
        (Zh, "item.remote_desktop") => "远程桌面",
        (En, "item.remote_desktop") => "Remote Desktop",
        (Zh, "item.local_files") => "本地文件浏览器",
        (En, "item.local_files") => "Local Files",
        (Zh, "item.sftp_panel") => "SFTP 面板",
        (En, "item.sftp_panel") => "SFTP Panel",
        (Zh, "item.ai_assistant") => "AI 助手",
        (En, "item.ai_assistant") => "AI Assistant",
        (Zh, "item.status_bar") => "底部状态栏",
        (En, "item.status_bar") => "Status Bar",

        // ---- 工具 / 设置 / 帮助 ----
        (Zh, "item.ping") => "Ping",
        (En, "item.ping") => "Ping",
        (Zh, "item.telnet") => "Telnet",
        (En, "item.telnet") => "Telnet",
        (Zh, "item.macro_manager") => "键宏管理器",
        (En, "item.macro_manager") => "Macro Manager",
        (Zh, "item.config_panel") => "配置面板",
        (En, "item.config_panel") => "Settings Panel",
        (Zh, "item.ai_config") => "AI 配置",
        (En, "item.ai_config") => "AI Settings",
        (Zh, "item.dark_theme") => "深色主题",
        (En, "item.dark_theme") => "Dark Theme",
        (Zh, "item.light_theme") => "浅色主题",
        (En, "item.light_theme") => "Light Theme",
        (Zh, "item.about") => "关于 RustTerm",
        (En, "item.about") => "About RustTerm",
        (Zh, "item.x11_forward") => "启用 X11 转发",
        (En, "item.x11_forward") => "Enable X11 Forwarding",

        // ---- 工具栏 ----
        (Zh, "tb.local") => "本地会话",
        (En, "tb.local") => "Local",
        (Zh, "tb.ssh") => "SSH会话",
        (En, "tb.ssh") => "SSH",
        (Zh, "tb.sync") => "同步",
        (En, "tb.sync") => "Sync",
        (Zh, "tb.batch") => "批量",
        (En, "tb.batch") => "Batch",
        (Zh, "tb.tunnel") => "隧道",
        (En, "tb.tunnel") => "Tunnel",
        (Zh, "tb.find") => "查找",
        (En, "tb.find") => "Find",
        (Zh, "tb.files") => "本地文件",
        (En, "tb.files") => "Files",
        (Zh, "tb.browser") => "浏览器",
        (En, "tb.browser") => "Browser",
        (Zh, "tb.ai") => "AI",
        (En, "tb.ai") => "AI",
        (Zh, "tb.settings") => "设置",
        (En, "tb.settings") => "Settings",
        (Zh, "tb.language") => "语言",
        (En, "tb.language") => "Language",
        (Zh, "tb.theme") => "主题",
        (En, "tb.theme") => "Theme",
        (Zh, "tb.help") => "帮助",
        (En, "tb.help") => "Help",

        // ---- 侧栏 ----
        (Zh, "side.sessions") => "会话",
        (En, "side.sessions") => "Sessions",
        (Zh, "side.tools") => "工具",
        (En, "side.tools") => "Tools",
        (Zh, "side.macros") => "宏",
        (En, "side.macros") => "Macros",
        (Zh, "side.search") => "搜索会话…",
        (En, "side.search") => "Search sessions…",
        (Zh, "side.new") => "新建",
        (En, "side.new") => "New",

        // ---- 欢迎页 ----
        (Zh, "welcome.subtitle") => "全功能终端客户端 · 本地 / SSH / 浏览器 / 远程桌面",
        (En, "welcome.subtitle") => "All-in-one terminal · Local / SSH / Browser / Remote Desktop",
        (Zh, "welcome.start_local") => "启动本地终端",
        (En, "welcome.start_local") => "Start Local Terminal",
        (Zh, "welcome.restore") => "恢复上次会话",
        (En, "welcome.restore") => "Restore Last Session",
        (Zh, "welcome.recent") => "最近会话",
        (En, "welcome.recent") => "Recent Sessions",
        (Zh, "welcome.search") => "搜索会话…（回车连接）",
        (En, "welcome.search") => "Search sessions… (Enter to connect)",

        // ---- 状态栏 ----
        (Zh, "status.opened") => "已打开",
        (En, "status.opened") => "Open",
        (Zh, "status.none") => "无",
        (En, "status.none") => "None",
        (Zh, "status.tabs") => "标签",
        (En, "status.tabs") => "Tabs",
        (Zh, "status.sessions") => "会话",
        (En, "status.sessions") => "Sessions",
        (Zh, "status.connected") => "已连接",
        (En, "status.connected") => "Connected",
        (Zh, "status.disconnected") => "已断开",
        (En, "status.disconnected") => "Disconnected",
        (Zh, "status.connecting") => "连接中...",
        (En, "status.connecting") => "Connecting...",
        (Zh, "status.browser") => "浏览器",
        (En, "status.browser") => "Browser",
        (Zh, "status.remote_active") => "远程桌面已连接",
        (En, "status.remote_active") => "Remote desktop connected",
        (Zh, "status.remote_connecting") => "远程桌面连接中",
        (En, "status.remote_connecting") => "Remote desktop connecting",
        (Zh, "status.remote_idle") => "远程桌面未连接",
        (En, "status.remote_idle") => "Remote desktop disconnected",
        (Zh, "status.failed") => "失败",
        (En, "status.failed") => "Failed",
        (Zh, "status.files") => "本地文件",
        (En, "status.files") => "Local Files",

        // ---- 快速连接栏 ----
        (Zh, "qc.hint") => "快速连接...  例如: user@host:22",
        (En, "qc.hint") => "Quick connect...  e.g. user@host:22",
        (Zh, "qc.jump_hint") => "跳板机（可空）",
        (En, "qc.jump_hint") => "Jump host (optional)",
        (Zh, "qc.password") => "密码",
        (En, "qc.password") => "Password",
        (Zh, "qc.key") => "密钥",
        (En, "qc.key") => "Key",
        (Zh, "qc.connect") => "连接",
        (En, "qc.connect") => "Connect",
        (Zh, "qc.password_hint") => "密码（可空，连接后提示）",
        (En, "qc.password_hint") => "Password (optional)",

        // ---- 通用 ----
        (Zh, "common.language_switch") => "切换到 English",
        (En, "common.language_switch") => "Switch to 中文",

        (_, other) => return other.to_string(),
    };
    s.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_key_translates() {
        assert_eq!(tr(Lang::Zh, "menu.terminal"), "终端");
        assert_eq!(tr(Lang::En, "menu.terminal"), "Terminal");
    }

    #[test]
    fn unknown_key_falls_back_to_key() {
        assert_eq!(tr(Lang::En, "no.such.key"), "no.such.key");
    }

    #[test]
    fn lang_id_roundtrip() {
        assert_eq!(Lang::from_id("en-US"), Lang::En);
        assert_eq!(Lang::from_id("zh-CN"), Lang::Zh);
        assert_eq!(Lang::Zh.toggled(), Lang::En);
    }
}
