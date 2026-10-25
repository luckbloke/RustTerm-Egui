//! 内置浏览器（WebView2 子窗口），每个浏览器标签页持有独立 WebView。
//!
//! 标签页内嵌在主窗口客户区：WebView 作为主窗口的子窗口，通过
//! `set_bounds` 与 egui 绘制的占位矩形对齐，实现"浏览器像普通标签页一样
//! 出现在标签栏里"的观感。
//!
//! 关键点：
//! - 非活动标签页的 WebView 会被 `set_visible(false)` 隐藏，避免叠在终端上。
//! - WebView 事件（标题/URL）通过 mpsc 通道回传，避免闭包借用 self。

use std::collections::VecDeque;
use std::sync::mpsc::{channel, Receiver};
use urlencoding;

/// about:blank 新标签页展示的欢迎页（真实 WebView 中渲染）
const WELCOME_HTML: &str = r#"<!DOCTYPE html>
<html><head><meta charset="utf-8"><title>新标签页</title>
<style>
  html,body{margin:0;height:100%;background:#1e1f24;color:#d0d0d5;
    font-family:"Segoe UI","Microsoft YaHei",sans-serif;
    display:flex;flex-direction:column;align-items:center;justify-content:center;}
  h1{font-size:42px;font-weight:300;margin-bottom:8px;color:#e8e8ee;}
  p{color:#8a8a95;margin:4px 0;}
  .tip{margin-top:28px;font-size:13px;color:#6a6a75;}
  .keys{margin-top:8px;font-size:12px;color:#55555f;}
</style></head>
<body>
  <h1>RustTerm 浏览器</h1>
  <p>基于 WebView2 的内置浏览器</p>
  <div class="tip">在上方地址栏输入网址或搜索关键词，回车访问</div>
  <div class="keys">Ctrl+T 新标签页 · Ctrl+W 关闭标签 · F5 刷新 · Ctrl+H 历史</div>
</body></html>"#;

/// WebView → UI 的回传事件
#[derive(Debug)]
enum WebEvent {
    /// 页面标题变化
    Title(String),
    /// 页面 URL 导航变化
    Url(String),
}

/// 主窗口 HWND（egui 主画布 == 客户区，用于子窗口坐标换算）
pub(crate) fn main_hwnd() -> Option<isize> {
    use windows_sys::Win32::UI::WindowsAndMessaging::FindWindowW;
    let title: Vec<u16> = "RustTerm Egui\0".encode_utf16().collect();
    let hwnd = unsafe { FindWindowW(std::ptr::null(), title.as_ptr()) };
    if hwnd.is_null() {
        None
    } else {
        Some(hwnd as isize)
    }
}

/// 把键盘焦点交回主窗口（egui 画布）。
///
/// WebView2 子窗口创建后会抢占焦点，导致 egui 的输入框
/// （如地址栏）收不到键盘事件。点击地址栏 / 按 Ctrl+L 时必须抢回来。
pub fn focus_main_window() {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::SetFocus;
    if let Some(hwnd) = main_hwnd() {
        unsafe {
            SetFocus(hwnd as _);
        }
    }
}

/// 书签
#[derive(Clone, Debug)]
pub struct Bookmark {
    pub name: String,
    pub url: String,
    pub folder: String,
}

/// 单个浏览器标签页：状态 + 自己的 WebView 子窗口。
pub struct BrowserTab {
    pub id: usize,
    pub url: String,
    pub title: String,
    pub loading: bool,
    pub can_go_back: bool,
    pub can_go_forward: bool,
    pub history: VecDeque<String>,
    pub history_index: usize,
    /// 地址栏编辑缓冲：必须与 url 分开保存，否则每帧会被 url 覆盖，
    /// 导致用户输入被丢弃。
    pub address_bar: String,
    /// 地址栏当前是否获得焦点（获得焦点时不用 url 覆盖编辑缓冲）
    pub address_focused: bool,
    /// 真实 WebView 子窗口（惰性创建）
    webview: Option<wry::WebView>,
    /// WebView 事件接收端
    event_rx: Option<Receiver<WebEvent>>,
    /// WebView2 初始化失败原因
    pub webview_error: Option<String>,
    /// 上一次设置的边界（逻辑坐标），避免每帧重复 set_bounds
    last_px: Option<[f32; 4]>,
}

impl BrowserTab {
    pub fn new(id: usize, url: String) -> Self {
        let mut tab = Self {
            id,
            url: url.clone(),
            title: "新标签页".to_string(),
            loading: false,
            can_go_back: false,
            can_go_forward: false,
            history: VecDeque::new(),
            history_index: 0,
            address_bar: url.clone(),
            address_focused: false,
            webview: None,
            event_rx: None,
            webview_error: None,
            last_px: None,
        };
        tab.history.push_back(url);
        tab.update_title();
        tab
    }

    fn update_title(&mut self) {
        if self.url == "about:blank" || self.url.is_empty() {
            self.title = "新标签页".to_string();
        } else if let Ok(parsed) = url::Url::parse(&self.url) {
            self.title = parsed
                .host_str()
                .unwrap_or(self.url.as_str())
                .to_string();
        } else {
            self.title = self.url.clone();
        }
    }

    /// 标签栏上显示的名字（页面标题优先，回退到 URL）。
    pub fn display_title(&self) -> String {
        if self.title == "新标签页" && self.url != "about:blank" && !self.url.is_empty() {
            self.url.clone()
        } else {
            self.title.clone()
        }
    }

    pub fn navigate(&mut self, url: String) {
        while self.history.len() > self.history_index + 1 {
            self.history.pop_back();
        }
        self.history.push_back(url.clone());
        self.history_index = self.history.len() - 1;
        self.url = url;
        self.update_title();
        self.can_go_back = self.history_index > 0;
        self.can_go_forward = self.history_index < self.history.len() - 1;
        self.address_bar = self.url.clone();
    }

    pub fn go_back(&mut self) -> Option<String> {
        if self.can_go_back {
            self.history_index -= 1;
            self.url = self.history[self.history_index].clone();
            self.update_title();
            self.can_go_back = self.history_index > 0;
            self.can_go_forward = true;
            self.address_bar = self.url.clone();
            Some(self.url.clone())
        } else {
            None
        }
    }

    pub fn go_forward(&mut self) -> Option<String> {
        if self.can_go_forward {
            self.history_index += 1;
            self.url = self.history[self.history_index].clone();
            self.update_title();
            self.can_go_forward = self.history_index < self.history.len() - 1;
            self.can_go_back = true;
            self.address_bar = self.url.clone();
            Some(self.url.clone())
        } else {
            None
        }
    }

    /// 惰性创建 WebView 子窗口（挂在主窗口 HWND 下）。
    pub fn ensure_webview(&mut self) {
        if self.webview.is_some() || self.webview_error.is_some() {
            return;
        }
        let Some(hwnd) = main_hwnd() else {
            self.webview_error = Some("无法获取主窗口句柄".to_string());
            return;
        };

        // 把 HWND 包装成 HasWindowHandle
        struct HwndParent(raw_window_handle::Win32WindowHandle);
        impl raw_window_handle::HasWindowHandle for HwndParent {
            fn window_handle(
                &self,
            ) -> Result<raw_window_handle::WindowHandle<'_>, raw_window_handle::HandleError>
            {
                Ok(unsafe {
                    raw_window_handle::WindowHandle::borrow_raw(
                        raw_window_handle::RawWindowHandle::Win32(self.0),
                    )
                })
            }
        }
        let Some(nz) = std::num::NonZeroIsize::new(hwnd) else {
            self.webview_error = Some("主窗口句柄无效".to_string());
            return;
        };
        let parent = HwndParent(raw_window_handle::Win32WindowHandle::new(nz));

        let (tx_title, rx) = channel::<WebEvent>();
        let tx_nav = tx_title.clone();
        let url = self.url.clone();

        let mut builder = wry::WebViewBuilder::new()
            .with_transparent(false)
            .with_navigation_handler(move |u: String| {
                // load_html 会产生 data:text/html 导航，不应覆盖用户可见的地址
                if !u.starts_with("data:") {
                    let _ = tx_nav.send(WebEvent::Url(u));
                }
                true
            })
            .with_document_title_changed_handler(move |t: String| {
                let _ = tx_title.send(WebEvent::Title(t));
            });
        if url != "about:blank" && !url.is_empty() {
            builder = builder.with_url(url.clone());
        }

        match builder.build_as_child(&parent) {
            Ok(wv) => {
                if url == "about:blank" || url.is_empty() {
                    let _ = wv.load_html(WELCOME_HTML);
                }
                // 初始隐藏，等标签页被激活时再显示
                let _ = wv.set_visible(false);
                self.webview = Some(wv);
                self.event_rx = Some(rx);
                self.last_px = None;
                // WebView2 创建后会抢焦点，交回主窗口
                focus_main_window();
            }
            Err(e) => {
                self.webview_error = Some(format!(
                    "WebView2 初始化失败: {e}\n\n请安装 Microsoft Edge WebView2 Runtime 后重试。"
                ));
            }
        }
    }

    /// 轮询 WebView 事件，同步标题 / 地址。
    pub fn poll_events(&mut self) {
        let events: Vec<WebEvent> = match &self.event_rx {
            Some(rx) => {
                let mut v = Vec::new();
                while let Ok(ev) = rx.try_recv() {
                    v.push(ev);
                }
                v
            }
            None => return,
        };
        if events.is_empty() {
            return;
        }
        let mut loading_done = false;
        for ev in events {
            match ev {
                WebEvent::Title(t) => {
                    if self.title != t {
                        self.title = t;
                    }
                    loading_done = true;
                }
                WebEvent::Url(u) => {
                    // 忽略内部 data: 导航（load_html 产生）
                    if u.starts_with("data:") {
                        continue;
                    }
                    if self.url != u {
                        self.url = u.clone();
                        self.update_title();
                        // 用户正在地址栏输入时不要覆盖
                        if !self.address_focused {
                            self.address_bar = self.url.clone();
                        }
                    }
                    loading_done = false;
                }
            }
        }
        if loading_done {
            self.loading = false;
        }
    }

    /// 把 egui 内容区矩形同步到 WebView 子窗口边界。
    ///
    /// 坐标系说明：
    /// - egui 的 `rect` 以**逻辑点**表示，原点在客户区左上角。
    /// - `SetWindowPos` 设置**子窗口**位置时使用父窗口客户区坐标，
    ///   因此不能加屏幕原点偏移（加了就会整体偏移）。
    /// - wry 的 `LogicalPosition`/`LogicalSize` 会再乘一次 DPI 缩放因子，
    ///   正好把逻辑点换算成物理像素。
    pub fn sync_bounds(&mut self, ctx: &egui::Context, rect: egui::Rect) {
        let Some(wv) = &self.webview else { return };
        let lx = rect.min.x;
        let ly = rect.min.y;
        let lw = rect.width().max(1.0);
        let lh = rect.height().max(1.0);
        let bounds = [lx, ly, lw, lh];
        if self.last_px != Some(bounds) {
            let _ = wv.set_bounds(wry::Rect {
                position: wry::dpi::LogicalPosition::new(lx, ly).into(),
                size: wry::dpi::LogicalSize::new(lw, lh).into(),
            });
            self.last_px = Some(bounds);
        }
        let _ = ctx;
    }

    /// WebView 是否已成功创建。
    pub fn has_webview(&self) -> bool {
        self.webview.is_some()
    }

    /// 显示 / 隐藏 WebView（非活动标签页必须隐藏，否则会盖住终端）。
    pub fn set_visible(&self, visible: bool) {
        if let Some(wv) = &self.webview {
            let _ = wv.set_visible(visible);
        }
    }

    pub fn reload(&mut self) {
        self.loading = true;
        if let Some(wv) = &self.webview {
            let _ = wv.evaluate_script("location.reload()");
        } else {
            self.loading = false;
        }
    }

    pub fn load_url(&mut self, url: &str) {
        if let Some(wv) = &self.webview {
            if url == "about:blank" || url.is_empty() {
                let _ = wv.load_html(WELCOME_HTML);
            } else {
                let _ = wv.load_url(url);
            }
        }
    }

    pub fn go_back_web(&self) {
        if let Some(wv) = &self.webview {
            let _ = wv.evaluate_script("window.history.back()");
        }
    }

    pub fn go_forward_web(&self) {
        if let Some(wv) = &self.webview {
            let _ = wv.evaluate_script("window.history.forward()");
        }
    }

    /// 标签页关闭时销毁 WebView。
    pub fn shutdown(&mut self) {
        self.webview = None;
        self.event_rx = None;
        self.last_px = None;
    }
}

/// 浏览器共享状态（书签、历史、搜索引擎）——所有浏览器标签页共用。
#[derive(Clone)]
pub struct BrowserService {
    pub bookmarks: Vec<Bookmark>,
    pub history: VecDeque<String>,
    pub search_engine: String,
    pub show_bookmarks: bool,
}

impl Default for BrowserService {
    fn default() -> Self {
        Self {
            bookmarks: vec![
                Bookmark { name: "GitHub".into(), url: "https://github.com".into(), folder: "开发".into() },
                Bookmark { name: "Rust".into(), url: "https://rust-lang.org".into(), folder: "开发".into() },
                Bookmark { name: "Crates.io".into(), url: "https://crates.io".into(), folder: "开发".into() },
                Bookmark { name: "Google".into(), url: "https://google.com".into(), folder: "搜索".into() },
                Bookmark { name: "Bing".into(), url: "https://www.bing.com".into(), folder: "搜索".into() },
            ],
            history: VecDeque::new(),
            search_engine: "https://www.bing.com/search?q=".to_string(),
            show_bookmarks: false,
        }
    }
}

impl BrowserService {
    pub fn add_to_history(&mut self, url: String) {
        if !url.starts_with("about:") {
            self.history.push_front(url);
            if self.history.len() > 1000 {
                self.history.pop_back();
            }
        }
    }

    /// 把地址栏输入解析为 URL（域名直接访问，其余按搜索处理）。
    pub fn resolve_address(&self, input: &str) -> String {
        let input = input.trim();
        if input.starts_with("http://")
            || input.starts_with("https://")
            || input.starts_with("file://")
            || input.starts_with("about:")
        {
            return input.to_string();
        }
        if input.contains('.') && !input.contains(' ') && !input.contains('/') {
            format!("https://{input}")
        } else {
            format!("{}{}", self.search_engine, urlencoding::encode(input))
        }
    }
}
