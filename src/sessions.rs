//! 会话数据模型与持久化（config/rustterm-egui/sessions.json）。
//! 参照 XShell 风格：SSH 会话（可存密码）与 VNC/RDP/SPICE 远程桌面会话。

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SessionKind {
    Ssh,
    Vnc,
    Rdp,
    Spice,
}

impl SessionKind {
    pub fn label(&self) -> &'static str {
        match self {
            SessionKind::Ssh => "SSH",
            SessionKind::Vnc => "VNC",
            SessionKind::Rdp => "RDP",
            SessionKind::Spice => "SPICE",
        }
    }

    pub fn default_port(&self) -> u16 {
        match self {
            SessionKind::Ssh => 22,
            SessionKind::Vnc => 5900,
            SessionKind::Rdp => 3389,
            SessionKind::Spice => 5930,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionEntry {
    pub id: String,
    #[serde(default)]
    pub name: String,
    pub kind: SessionKind,
    pub host: String,
    pub port: u16,
    #[serde(default)]
    pub user: String,
    /// 密码（`auth_method == "password"` 时使用）
    #[serde(default)]
    pub password: String,
    /// 认证方式：`password`（默认）/ `key`
    #[serde(default)]
    pub auth_method: String,
    /// 私钥路径（`auth_method == "key"` 时使用）
    #[serde(default)]
    pub key_path: String,
    /// 域（RDP 登录用，可空）
    #[serde(default)]
    pub domain: String,
    #[serde(default = "default_group")]
    pub group: String,
}

impl SessionEntry {
    /// 是否使用密钥认证。
    pub fn uses_key(&self) -> bool {
        self.auth_method.eq_ignore_ascii_case("key")
    }
}

fn default_group() -> String {
    "默认分组".to_string()
}

impl SessionEntry {
    pub fn new_id() -> String {
        uuid::Uuid::new_v4().to_string()
    }

    /// 列表/最近会话里展示的名字。
    pub fn display(&self) -> String {
        if !self.name.is_empty() {
            return self.name.clone();
        }
        match self.kind {
            SessionKind::Ssh => {
                let cred = if self.uses_key() {
                    if self.key_path.is_empty() {
                        "未选密钥"
                    } else {
                        "密钥"
                    }
                } else if self.password.is_empty() {
                    "未存密码"
                } else {
                    "已存密码"
                };
                format!("{}@{}·{}", self.user, self.host, cred)
            }
            _ => format!("[{}] {}:{}", self.kind.label(), self.host, self.port),
        }
    }

    /// 搜索匹配（名称/主机/用户，忽略大小写）。
    pub fn matches_search(&self, query: &str) -> bool {
        let q = query.trim().to_lowercase();
        if q.is_empty() {
            return true;
        }
        self.display().to_lowercase().contains(&q)
            || self.host.to_lowercase().contains(&q)
            || self.user.to_lowercase().contains(&q)
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SessionStore {
    pub sessions: Vec<SessionEntry>,
    /// 最近会话（session id，最新在前）
    #[serde(default)]
    pub recent: Vec<String>,
}

impl SessionStore {
    pub fn path() -> Option<PathBuf> {
        dirs::config_dir().map(|p| p.join("rustterm-egui").join("sessions.json"))
    }

    /// 回退路径：程序所在目录旁的 `data/sessions.json`。
    ///
    /// 主配置目录（%APPDATA%）可能被安全软件（360/管家等主动防御）拦截写入，
    /// 此时回退到程序目录，保证会话仍然能落盘。
    pub fn fallback_path() -> Option<PathBuf> {
        std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|d| d.join("data").join("sessions.json")))
    }

    fn write_json(path: &PathBuf, data: &str) -> Result<(), String> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| format!("{} 创建失败: {e}", parent.display()))?;
        }
        fs::write(path, data)
            .map_err(|e| format!("{} 写入失败: {e}", path.display()))
    }

    pub fn load() -> Self {
        // 主路径优先；文件不存在时再试回退路径（主路径被拦截时 save 落在程序目录）
        if let Some(path) = Self::path() {
            if let Ok(data) = fs::read_to_string(&path) {
                if let Ok(store) = serde_json::from_str(&data) {
                    return store;
                }
            }
        }
        if let Some(path) = Self::fallback_path() {
            if let Ok(data) = fs::read_to_string(&path) {
                if let Ok(store) = serde_json::from_str(&data) {
                    return store;
                }
            }
        }
        Self::seed()
    }

    /// 保存会话。主路径失败时自动回退到程序目录 `data/`，
    /// 两条路径都失败才返回错误（调用方弹 toast 提示用户）。
    pub fn save(&self) -> Result<(), String> {
        let data = serde_json::to_string_pretty(self)
            .map_err(|e| format!("序列化失败: {e}"))?;
        if let Some(path) = Self::path() {
            if Self::write_json(&path, &data).is_ok() {
                return Ok(());
            }
        }
        if let Some(path) = Self::fallback_path() {
            if Self::write_json(&path, &data).is_ok() {
                return Ok(());
            }
        }
        Err("主配置目录与程序目录均写入失败（可能被安全软件拦截），请把本程序加入杀毒软件白名单".to_string())
    }

    /// 首次运行的示例会话（与设计稿一致）。
    fn seed() -> Self {
        let mk = |kind: SessionKind, host: &str, port: u16, user: &str, password: &str| {
            SessionEntry {
                id: SessionEntry::new_id(),
                name: String::new(),
                kind,
                host: host.to_string(),
                port,
                user: user.to_string(),
                password: password.to_string(),
                auth_method: "password".to_string(),
                key_path: String::new(),
                domain: String::new(),
                group: default_group(),
            }
        };
        let ssh = mk(SessionKind::Ssh, "192.168.1.19", 22, "root", "********");
        let vnc1 = mk(SessionKind::Vnc, "192.168.1.16", 5900, "", "");
        let rdp = mk(SessionKind::Rdp, "192.168.1.16", 3389, "", "");
        let vnc2 = mk(SessionKind::Vnc, "192.168.1.16", 5902, "", "");
        let spice = mk(SessionKind::Spice, "192.168.1.19", 5930, "", "");
        Self {
            recent: vec![ssh.id.clone(), vnc1.id.clone(), rdp.id.clone(), vnc2.id.clone(), spice.id.clone()],
            sessions: vec![ssh, vnc1, rdp, vnc2, spice],
        }
    }

    pub fn by_id(&self, id: &str) -> Option<&SessionEntry> {
        self.sessions.iter().find(|s| s.id == id)
    }

    /// 记录一次连接（最近会话置顶，最多 8 条）。
    pub fn touch_recent(&mut self, id: &str) {
        self.recent.retain(|x| x != id);
        self.recent.insert(0, id.to_string());
        self.recent.truncate(8);
    }

    pub fn remove(&mut self, id: &str) {
        self.sessions.retain(|s| s.id != id);
        self.recent.retain(|x| x != id);
    }

    /// 覆盖更新一条会话（编辑用）。
    pub fn update(&mut self, entry: SessionEntry) {
        if let Some(slot) = self.sessions.iter_mut().find(|s| s.id == entry.id) {
            *slot = entry;
        } else {
            self.sessions.push(entry);
        }
    }
}
