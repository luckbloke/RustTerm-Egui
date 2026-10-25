//! 会话后端：SSH（vendor russh）、本地 Shell（portable-pty）与原始 TCP（Telnet）。
//!
//! 所有后端拿到相同的 IO 抽象：
//! - `output_rx`: 向 UI 喂 VT 字节流
//! - `input_tx`:  接收 UI 编码后的键盘/鼠标字节
//! - `resize_tx`: 接收 (cols, rows) 变更

pub mod local;
pub mod ssh;
pub mod tcp;

use std::sync::Arc;
use tokio::sync::mpsc;

pub struct TermIo {
    pub input_tx: mpsc::UnboundedSender<Vec<u8>>,
    pub resize_tx: mpsc::UnboundedSender<(u16, u16)>,
    pub output_rx: mpsc::UnboundedReceiver<Vec<u8>>,
    /// SSH 会话句柄（用于在同一连接上另开 SFTP 子系统；本地/Telnet 为 None）
    pub ssh: Option<Arc<ssh::SshHandle>>,
}
