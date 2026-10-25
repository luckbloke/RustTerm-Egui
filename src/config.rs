use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

/// AI 提供方。
///
/// - `openai`：OpenAI 兼容协议（`{base}/chat/completions`），也覆盖
///   各类中转网关 / GLM / Kilo 等兼容端点。
/// - `ollama`：本地 Ollama（`{base}/api/chat`），不需要 API Key。
/// - `anthropic`：Anthropic Messages API（`{base}/v1/messages`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AiProvider {
    Openai,
    Ollama,
    Anthropic,
}

impl AiProvider {
    pub const ALL: [AiProvider; 3] =
        [AiProvider::Openai, AiProvider::Ollama, AiProvider::Anthropic];

    /// 下拉框显示名。
    pub fn label(&self) -> &'static str {
        match self {
            AiProvider::Openai => "OpenAI",
            AiProvider::Ollama => "Ollama（本地）",
            AiProvider::Anthropic => "Anthropic",
        }
    }

    /// 持久化用的稳定标识。
    pub fn id(&self) -> &'static str {
        match self {
            AiProvider::Openai => "openai",
            AiProvider::Ollama => "ollama",
            AiProvider::Anthropic => "anthropic",
        }
    }

    pub fn from_id(s: &str) -> Self {
        match s.trim().to_lowercase().as_str() {
            "ollama" => AiProvider::Ollama,
            "anthropic" | "claude" => AiProvider::Anthropic,
            _ => AiProvider::Openai,
        }
    }

    /// 该提供方的默认 Base URL。
    pub fn default_base_url(&self) -> &'static str {
        match self {
            AiProvider::Openai => "https://api.openai.com/v1",
            AiProvider::Ollama => "http://localhost:11434",
            AiProvider::Anthropic => "https://api.anthropic.com",
        }
    }

    /// 该提供方的默认模型名。
    pub fn default_model(&self) -> &'static str {
        match self {
            AiProvider::Openai => "gpt-4o-mini",
            AiProvider::Ollama => "qwen2.5:7b",
            AiProvider::Anthropic => "claude-3-5-sonnet-latest",
        }
    }

    /// 是否需要 API Key（Ollama 本地推理不需要）。
    pub fn needs_api_key(&self) -> bool {
        !matches!(self, AiProvider::Ollama)
    }

    /// 切换提供方时是否要重置 Base URL / 模型。
    ///
    /// 用户手动填过中转网关地址时不应被覆盖，因此只有当当前值
    /// 恰好等于「上一个提供方的默认值」时才跟随切换。
    pub fn should_reset_url(&self, current: &str, prev: AiProvider) -> bool {
        current.trim().is_empty() || current.trim() == prev.default_base_url()
    }

    pub fn should_reset_model(&self, current: &str, prev: AiProvider) -> bool {
        current.trim().is_empty() || current.trim() == prev.default_model()
    }
}

/// AI 相关配置（对应「AI 配置」面板的全部字段）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AiSettings {
    /// 提供方：`openai` / `ollama` / `anthropic`
    #[serde(default = "default_ai_provider")]
    pub provider: String,
    /// API Key（仅保存在本地；Ollama 不需要）
    #[serde(default)]
    pub api_key: String,
    /// 接口根地址
    #[serde(default = "default_ai_base")]
    pub base_url: String,
    /// 模型名
    #[serde(default = "default_ai_model")]
    pub model: String,
    /// 单次回复最大 Token 数
    #[serde(default = "default_ai_max_tokens")]
    pub max_tokens: u32,
    /// 采样温度
    #[serde(default = "default_ai_temperature")]
    pub temperature: f32,
    /// 只读模式：为 true 时禁止把 AI 给出的写命令发到终端
    #[serde(default)]
    pub read_only: bool,
    /// 对话历史最多保留多少条
    #[serde(default = "default_ai_max_history")]
    pub max_history: usize,
    /// 每次附带最近多少行终端上下文
    #[serde(default = "default_ai_context_lines")]
    pub context_lines: usize,
    /// 上下文中单行最多保留多少字符（超出截断）
    #[serde(default = "default_ai_context_max_line_len")]
    pub context_max_line_len: usize,
}

fn default_ai_provider() -> String {
    "openai".to_string()
}
fn default_ai_model() -> String {
    AiProvider::Openai.default_model().to_string()
}
fn default_ai_base() -> String {
    AiProvider::Openai.default_base_url().to_string()
}
fn default_ai_max_tokens() -> u32 {
    2048
}
fn default_ai_temperature() -> f32 {
    0.3
}
fn default_ai_max_history() -> usize {
    20
}
fn default_ai_context_lines() -> usize {
    50
}
fn default_ai_context_max_line_len() -> usize {
    200
}

impl Default for AiSettings {
    fn default() -> Self {
        Self {
            provider: default_ai_provider(),
            api_key: String::new(),
            base_url: default_ai_base(),
            model: default_ai_model(),
            max_tokens: default_ai_max_tokens(),
            temperature: default_ai_temperature(),
            read_only: false,
            max_history: default_ai_max_history(),
            context_lines: default_ai_context_lines(),
            context_max_line_len: default_ai_context_max_line_len(),
        }
    }
}

impl AiSettings {
    /// 当前提供方枚举。
    pub fn provider(&self) -> AiProvider {
        AiProvider::from_id(&self.provider)
    }

    /// Base URL（去掉尾部斜杠）。
    pub fn trimmed_base(&self) -> String {
        self.base_url.trim().trim_end_matches('/').to_string()
    }

    /// 接口根地址是否为空（空表示未配置）。
    pub fn has_endpoint(&self) -> bool {
        !self.trimmed_base().is_empty()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    pub font_size: f32,
    pub font_family: String,
    pub theme: String,
    pub language: String,
    pub show_status_bar: bool,
    pub keep_alive_interval: u64,
    pub enable_key_macros: bool,
    /// AI 助手配置（提供方 / 模型 / 上下文策略）
    #[serde(default)]
    pub ai: AiSettings,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            font_size: 14.0,
            font_family: "monospace".to_string(),
            theme: "dark".to_string(),
            language: "zh-CN".to_string(),
            show_status_bar: true,
            keep_alive_interval: 30,
            enable_key_macros: true,
            ai: AiSettings::default(),
        }
    }
}

impl AppConfig {
    pub fn config_path() -> Option<PathBuf> {
        dirs::config_dir().map(|p| p.join("rustterm-egui").join("config.json"))
    }

    /// 回退路径：程序目录旁的 `data/config.json`（主配置目录被安全软件
    /// 拦截写入时用，与 sessions.json 的回退策略一致）。
    pub fn fallback_path() -> Option<PathBuf> {
        std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|d| d.join("data").join("config.json")))
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
        let Some(path) = Self::config_path() else {
            return Self::default();
        };
        let data = match fs::read_to_string(&path) {
            Ok(d) => d,
            Err(_) => {
                // 主路径不存在时尝试回退路径（主路径被拦截时 save 落在程序目录）
                if let Some(fb) = Self::fallback_path() {
                    if let Ok(d) = fs::read_to_string(&fb) {
                        return Self::parse_or_legacy(&d);
                    }
                }
                return Self::default();
            }
        };
        Self::parse_or_legacy(&data)
    }

    fn parse_or_legacy(data: &str) -> Self {
        match serde_json::from_str::<Self>(data) {
            Ok(cfg) => cfg,
            Err(_) => {
                // 兼容旧版扁平字段（ai_api_key / ai_model / ai_api_base）。
                if let Some(cfg) = Self::load_legacy(data) {
                    return cfg;
                }
                Self::default()
            }
        }
    }

    /// 读取旧格式配置并迁移到新的 `ai` 子结构。
    fn load_legacy(data: &str) -> Option<Self> {
        let v: serde_json::Value = serde_json::from_str(data).ok()?;
        let obj = v.as_object()?;
        let mut cfg = Self::default();
        if let Some(n) = obj.get("font_size").and_then(|x| x.as_f64()) {
            cfg.font_size = n as f32;
        }
        if let Some(s) = obj.get("font_family").and_then(|x| x.as_str()) {
            cfg.font_family = s.to_string();
        }
        if let Some(s) = obj.get("theme").and_then(|x| x.as_str()) {
            cfg.theme = s.to_string();
        }
        if let Some(s) = obj.get("language").and_then(|x| x.as_str()) {
            cfg.language = s.to_string();
        }
        if let Some(b) = obj.get("show_status_bar").and_then(|x| x.as_bool()) {
            cfg.show_status_bar = b;
        }
        if let Some(n) = obj.get("keep_alive_interval").and_then(|x| x.as_u64()) {
            cfg.keep_alive_interval = n;
        }
        if let Some(b) = obj.get("enable_key_macros").and_then(|x| x.as_bool()) {
            cfg.enable_key_macros = b;
        }
        // 旧 AI 字段 → 新结构
        if let Some(s) = obj.get("ai_api_key").and_then(|x| x.as_str()) {
            cfg.ai.api_key = s.to_string();
        }
        if let Some(s) = obj.get("ai_model").and_then(|x| x.as_str()) {
            cfg.ai.model = s.to_string();
        }
        if let Some(s) = obj.get("ai_api_base").and_then(|x| x.as_str()) {
            cfg.ai.base_url = s.to_string();
        }
        if let Some(s) = obj.get("ai_provider").and_then(|x| x.as_str()) {
            cfg.ai.provider = s.to_string();
        }
        Some(cfg)
    }

    /// 保存配置。主路径失败时回退到程序目录 `data/`；
    /// 两条路径都失败才返回错误（调用方应弹 toast 提示用户检查白名单）。
    pub fn save(&self) -> Result<(), String> {
        let data = serde_json::to_string_pretty(self)
            .map_err(|e| format!("序列化失败: {e}"))?;
        if let Some(path) = Self::config_path() {
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
}
