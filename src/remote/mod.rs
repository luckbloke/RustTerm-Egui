//! 远程桌面后端：VNC / RDP / SPICE 统一接口。
//!
//! 三种协议各有自己的线程内运行时，统一向上层暴露：
//! - 下行：`RdCommand`（键盘 / 指针 / 滚轮 / 关闭）
//! - 上行：`RdEvent`（状态 / 分辨率 / 脏矩形 RGBA 帧 / 错误）
//!
//! 帧以 **RGBA8 + 脏矩形** 传递，UI 侧只需 `TextureHandle::set_partial`，
//! 不必整帧重传（各协议内部维护完整帧缓冲，脏区合并后节流推送）。

pub mod rdp;
pub mod spice;
pub mod vnc;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RdProtocol {
    Vnc,
    Rdp,
    Spice,
}

#[derive(Clone, Debug)]
pub struct RdParams {
    pub protocol: RdProtocol,
    pub host: String,
    pub port: u16,
    pub username: String,
    pub password: String,
    pub domain: String,
    /// 初始桌面尺寸（RDP 会按此协商）
    pub width: u16,
    pub height: u16,
    /// 16 / 24 / 32
    pub color_depth: u8,
}

impl Default for RdParams {
    fn default() -> Self {
        Self {
            protocol: RdProtocol::Vnc,
            host: String::new(),
            port: 5900,
            username: String::new(),
            password: String::new(),
            domain: String::new(),
            width: 1280,
            height: 800,
            color_depth: 32,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RdState {
    Connecting,
    Authenticating,
    Active,
    Disconnected,
    Failed(String),
}

/// 后端 → UI
#[derive(Debug)]
pub enum RdEvent {
    State(RdState),
    /// 桌面尺寸（首帧或分辨率变化）
    Resized { width: u32, height: u32 },
    /// 脏矩形（RGBA8，行优先紧密排列）
    Frame {
        x: u32,
        y: u32,
        w: u32,
        h: u32,
        rgba: Vec<u8>,
    },
    Error(String),
}

/// 归一化键盘输入。
#[derive(Clone, Copy, Debug)]
pub struct KeyInput {
    /// PC/AT set-1 扫描码，扩展键为 0xE000 | code
    pub scancode: u16,
    /// X11 keysym（VNC 用）
    pub keysym: u32,
    /// 可打印字符（RDP 走 Unicode 通道）
    pub ch: Option<char>,
    pub down: bool,
}

/// UI → 后端
#[derive(Debug)]
pub enum RdCommand {
    Key(KeyInput),
    /// 绝对指针位置 + 按键位掩码（1=左 2=中 4=右）
    Pointer { x: u16, y: u16, buttons: u8 },
    Wheel { dy: i16 },
    Close,
}

pub struct RdSession {
    tx: Sender<RdCommand>,
    pub events: Receiver<RdEvent>,
    cancel: Arc<AtomicBool>,
}

impl RdSession {
    pub fn send(&self, cmd: RdCommand) {
        if self.cancel.load(Ordering::Relaxed) && !matches!(cmd, RdCommand::Close) {
            return;
        }
        let _ = self.tx.send(cmd);
    }

    pub fn close(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
        let _ = self.tx.send(RdCommand::Close);
    }
}

impl Drop for RdSession {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
        let _ = self.tx.send(RdCommand::Close);
    }
}

/// 把后端原始错误翻译成「用户能照着做」的提示。
///
/// 远程桌面最常见的失败其实都不是协议问题，而是**没填凭据**：
/// VNC 没填密码 → 服务器拒绝认证；RDP 没填用户名 → CredSSP 报
/// "Got empty identity"。原始报错对普通用户毫无意义，这里补一句怎么办。
fn friendly_error(protocol: RdProtocol, msg: &str) -> String {
    let lower = msg.to_lowercase();
    let hint = if lower.contains("authentication failed") || lower.contains("wrongpassword") {
        match protocol {
            RdProtocol::Vnc => Some("VNC 密码错误或未填写：右键会话 → 编辑… 填上密码"),
            RdProtocol::Spice => Some("SPICE 密码错误或未填写：右键会话 → 编辑… 填上密码"),
            RdProtocol::Rdp => Some("RDP 认证失败：右键会话 → 编辑… 填上用户名与密码"),
        }
    } else if lower.contains("empty identity") || lower.contains("credssp") {
        Some("RDP 需要用户名：右键会话 → 编辑… 填上用户名与密码")
    } else if lower.contains("10061") || lower.contains("actively refused") {
        Some("目标端口没有服务在监听：请确认地址/端口与服务是否已启动")
    } else if lower.contains("超时") || lower.contains("timed out") {
        Some("连接超时：请检查网络与防火墙")
    } else if lower.contains("unsupported vnc security type") {
        Some("服务器要求的认证方式不支持：请确认是否需要密码")
    } else {
        None
    };
    match hint {
        Some(h) => format!("{msg}\n提示：{h}"),
        None => msg.to_string(),
    }
}

/// 启动一个远程桌面会话（每次调用起一个独立线程 + 单线程运行时）。
pub fn spawn(params: RdParams) -> RdSession {
    let (cmd_tx, cmd_rx) = channel::<RdCommand>();
    let (ev_tx, ev_rx) = channel::<RdEvent>();
    let cancel = Arc::new(AtomicBool::new(false));
    let cancel_thread = cancel.clone();

    std::thread::Builder::new()
        .name(format!("remote-{:?}", params.protocol))
        .spawn(move || {
            let rt = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    let _ = ev_tx.send(RdEvent::Error(format!("创建运行时失败: {e}")));
                    return;
                }
            };
            let result = rt.block_on(async {
                match params.protocol {
                    RdProtocol::Vnc => {
                        vnc::run(&params, &ev_tx, &cmd_rx, &cancel_thread).await
                    }
                    RdProtocol::Rdp => {
                        rdp::run(&params, &ev_tx, &cmd_rx, &cancel_thread).await
                    }
                    RdProtocol::Spice => {
                        spice::run(&params, &ev_tx, &cmd_rx, &cancel_thread).await
                    }
                }
            });
            if let Err(e) = result {
                let msg = friendly_error(params.protocol, &format!("{e:#}"));
                eprintln!("[remote] {:?} 连接失败: {msg}", params.protocol);
                let _ = ev_tx.send(RdEvent::Error(msg));
            }
            let _ = ev_tx.send(RdEvent::State(RdState::Disconnected));
        })
        .ok();

    RdSession {
        tx: cmd_tx,
        events: ev_rx,
        cancel,
    }
}

// ---------------------------------------------------------------------------
// 键盘映射
// ---------------------------------------------------------------------------

/// egui 键 → (扫描码, X11 keysym)。
pub fn map_key(key: egui::Key) -> Option<(u16, u32)> {
    use egui::Key as K;
    // 常用按键表（PC/AT set-1 扫描码 + X11 keysym）
    let v = match key {
        K::Escape => (0x01, 0xff1b),
        K::Num1 => (0x02, 0x31),
        K::Num2 => (0x03, 0x32),
        K::Num3 => (0x04, 0x33),
        K::Num4 => (0x05, 0x34),
        K::Num5 => (0x06, 0x35),
        K::Num6 => (0x07, 0x36),
        K::Num7 => (0x08, 0x37),
        K::Num8 => (0x09, 0x38),
        K::Num9 => (0x0a, 0x39),
        K::Num0 => (0x0b, 0x30),
        K::Minus => (0x0c, 0x2d),
        K::Equals | K::Plus => (0x0d, 0x3d),
        K::Backspace => (0x0e, 0xff08),
        K::Tab => (0x0f, 0xff09),
        K::Q => (0x10, 0x71),
        K::W => (0x11, 0x77),
        K::E => (0x12, 0x65),
        K::R => (0x13, 0x72),
        K::T => (0x14, 0x74),
        K::Y => (0x15, 0x79),
        K::U => (0x16, 0x75),
        K::I => (0x17, 0x69),
        K::O => (0x18, 0x6f),
        K::P => (0x19, 0x70),
        K::OpenBracket => (0x1a, 0x5b),
        K::CloseBracket => (0x1b, 0x5d),
        K::Enter => (0x1c, 0xff0d),
        K::A => (0x1e, 0x61),
        K::S => (0x1f, 0x73),
        K::D => (0x20, 0x64),
        K::F => (0x21, 0x66),
        K::G => (0x22, 0x67),
        K::H => (0x23, 0x68),
        K::J => (0x24, 0x6a),
        K::K => (0x25, 0x6b),
        K::L => (0x26, 0x6c),
        K::Semicolon | K::Colon => (0x27, 0x3b),
        K::Quote => (0x28, 0x27),
        K::Backtick => (0x29, 0x60),
        K::Backslash | K::Pipe => (0x2b, 0x5c),
        K::Z => (0x2c, 0x7a),
        K::X => (0x2d, 0x78),
        K::C => (0x2e, 0x63),
        K::V => (0x2f, 0x76),
        K::B => (0x30, 0x62),
        K::N => (0x31, 0x6e),
        K::M => (0x32, 0x6d),
        K::Comma => (0x33, 0x2c),
        K::Period => (0x34, 0x2e),
        K::Slash | K::Questionmark => (0x35, 0x2f),
        K::Space => (0x39, 0x20),
        K::F1 => (0x3b, 0xffbe),
        K::F2 => (0x3c, 0xffbf),
        K::F3 => (0x3d, 0xffc0),
        K::F4 => (0x3e, 0xffc1),
        K::F5 => (0x3f, 0xffc2),
        K::F6 => (0x40, 0xffc3),
        K::F7 => (0x41, 0xffc4),
        K::F8 => (0x42, 0xffc5),
        K::F9 => (0x43, 0xffc6),
        K::F10 => (0x44, 0xffc7),
        K::F11 => (0x57, 0xffc8),
        K::F12 => (0x58, 0xffc9),
        K::Home => (0xE047, 0xff50),
        K::ArrowUp => (0xE048, 0xff52),
        K::PageUp => (0xE049, 0xff55),
        K::ArrowLeft => (0xE04b, 0xff51),
        K::ArrowRight => (0xE04d, 0xff53),
        K::End => (0xE04f, 0xff57),
        K::ArrowDown => (0xE050, 0xff54),
        K::PageDown => (0xE051, 0xff56),
        K::Insert => (0xE052, 0xff63),
        K::Delete => (0xE053, 0xffff),
        _ => return None,
    };
    Some(v)
}

/// 修饰键：左 Shift / 左 Ctrl / 左 Alt。
pub const MOD_SHIFT: KeyInput = KeyInput {
    scancode: 0x2a,
    keysym: 0xffe1,
    ch: None,
    down: false,
};
pub const MOD_CTRL: KeyInput = KeyInput {
    scancode: 0x1d,
    keysym: 0xffe3,
    ch: None,
    down: false,
};
pub const MOD_ALT: KeyInput = KeyInput {
    scancode: 0x38,
    keysym: 0xffe9,
    ch: None,
    down: false,
};

/// 计算可打印键在 US 键盘布局下的真实字符（考虑 Shift 按下）→ X11 keysym。
///
/// 背景：`map_key` 给的 keysym 恒为「未按 Shift」的基准字符（如 't' = 0x74）。
/// VNC 远端以 keysym 为准来决定敲出的字符：
/// - 上档字符（`T` / `!` / `@` …）必须改用上档 keysym（'T' = 0x54、'@' = 0x40），
///   否则大写字母输成小写、符号直接丢失；
/// - 数据链路上伴随补发的 Shift 按下/抬起（`apply_mods`）对 RDP（扫描码路径）
///   仍然有效；VNC 只看 keysym，补发 Shift 属冗余但无害。
///
/// 返回 `None` 表示该键不是可打印字符（功能键/方向键等沿用 `map_key` 的 keysym）。
pub fn printable_keysym(key: egui::Key, shift: bool) -> Option<u32> {
    use egui::Key as K;
    // 字母键：Shift → 大写（CapsLock 状态 egui 拿不到，暂不支持）
    let lower: char = match key {
        K::Q => 'q', K::W => 'w', K::E => 'e', K::R => 'r', K::T => 't',
        K::Y => 'y', K::U => 'u', K::I => 'i', K::O => 'o', K::P => 'p',
        K::A => 'a', K::S => 's', K::D => 'd', K::F => 'f', K::G => 'g',
        K::H => 'h', K::J => 'j', K::K => 'k', K::L => 'l',
        K::Z => 'z', K::X => 'x', K::C => 'c', K::V => 'v', K::B => 'b',
        K::N => 'n', K::M => 'm',
        _ => {
            // 数字行 / 符号键：US 布局的（基准字符, Shift 上档字符）
            let (base, shifted) = match key {
                K::Num1 => ('1', '!'),  K::Num2 => ('2', '@'),   K::Num3 => ('3', '#'),
                K::Num4 => ('4', '$'),  K::Num5 => ('5', '%'),   K::Num6 => ('6', '^'),
                K::Num7 => ('7', '&'),  K::Num8 => ('8', '*'),   K::Num9 => ('9', '('),
                K::Num0 => ('0', ')'),
                K::Minus => ('-', '_'),
                K::Equals | K::Plus => ('=', '+'),
                K::Comma => (',', '<'),
                K::Period => ('.', '>'),
                K::Slash | K::Questionmark => ('/', '?'),
                K::Semicolon | K::Colon => (';', ':'),
                K::Quote => ('\'', '"'),
                K::Backtick => ('`', '~'),
                K::OpenBracket => ('[', '{'),
                K::CloseBracket => (']', '}'),
                K::Backslash | K::Pipe => ('\\', '|'),
                K::Space => (' ', ' '),
                _ => return None,
            };
            let ch = if shift { shifted } else { base };
            return Some(ch as u32);
        }
    };
    Some(if shift {
        lower.to_ascii_uppercase() as u32
    } else {
        lower as u32
    })
}
