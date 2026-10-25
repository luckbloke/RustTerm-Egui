//! 本地 Shell 后端（Windows: powershell，其他: bash）。
//! portable-pty 是同步 API，用 std 线程桥接到 tokio mpsc。
//!
//! 同时提供 `spawn_command`：在 PTY 里运行任意命令（Ping / traceroute 等），
//! 让这些工具复用终端的完整 VT 渲染与交互能力。

use anyhow::Result;
use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use std::io::{Read, Write};
use tokio::sync::mpsc;

use super::TermIo;

/// 打开默认 Shell。
pub fn spawn(cols: u16, rows: u16) -> Result<TermIo> {
    #[cfg(windows)]
    let cmd = CommandBuilder::new("powershell.exe");
    #[cfg(not(windows))]
    let cmd = CommandBuilder::new("bash");
    spawn_builder(cmd, cols, rows)
}

/// 在 PTY 中运行指定命令（参数已展开）。
pub fn spawn_command(program: &str, args: &[String], cols: u16, rows: u16) -> Result<TermIo> {
    let mut cmd = CommandBuilder::new(program);
    for a in args {
        cmd.arg(a);
    }
    spawn_builder(cmd, cols, rows)
}

fn spawn_builder(cmd: CommandBuilder, cols: u16, rows: u16) -> Result<TermIo> {
    let pty_system = native_pty_system();
    let pair = pty_system.openpty(PtySize {
        rows,
        cols,
        pixel_width: 0,
        pixel_height: 0,
    })?;

    let mut child = pair.slave.spawn_command(cmd)?;

    let mut reader = pair.master.try_clone_reader()?;
    let mut writer = pair.master.take_writer()?;

    let (input_tx, mut input_rx) = mpsc::unbounded_channel::<Vec<u8>>();
    let (output_tx, output_rx) = mpsc::unbounded_channel::<Vec<u8>>();
    let (resize_tx, mut resize_rx) = mpsc::unbounded_channel::<(u16, u16)>();

    // 输出：pty reader → mpsc（同步线程）
    std::thread::spawn(move || {
        let mut buf = [0u8; 8192];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    if output_tx.send(buf[..n].to_vec()).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
        let _ = child.wait();
    });

    // 输入：mpsc → pty writer（同步线程）
    std::thread::spawn(move || {
        while let Some(data) = input_rx.blocking_recv() {
            if writer.write_all(&data).is_err() {
                break;
            }
            let _ = writer.flush();
        }
    });

    // resize：master 句柄移入独立线程消费
    std::thread::spawn(move || {
        let master = pair.master;
        while let Some((c, r)) = resize_rx.blocking_recv() {
            let _ = master.resize(PtySize {
                rows: r,
                cols: c,
                pixel_width: 0,
                pixel_height: 0,
            });
        }
    });

    Ok(TermIo {
        input_tx,
        resize_tx,
        output_rx,
        ssh: None,
    })
}
