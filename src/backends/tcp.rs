//! 原始 TCP 后端（Telnet 客户端）。
//!
//! 实现最小 Telnet 协商：
//! - 收到 `IAC DO x`  → 回 `IAC WONT x`
//! - 收到 `IAC WILL x` → 回 `IAC DONT x`
//! - `IAC SB … IAC SE` 子协商整段丢弃
//!
//! 这样对大多数网络设备的 telnet 服务（字符模式）即可交互；
//! 不支持协商的裸 TCP 服务（如 SMTP/HTTP）字节原样透传。

use anyhow::{anyhow, Result};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Duration;
use tokio::sync::mpsc;

use super::TermIo;

const IAC: u8 = 0xFF; // Interpret As Command
const DONT: u8 = 0xFE;
const DO: u8 = 0xFD;
const WONT: u8 = 0xFC;
const WILL: u8 = 0xFB;
const SB: u8 = 0xFA; // 子协商开始
const SE: u8 = 0xF0; // 子协商结束

pub fn spawn(host: &str, port: u16) -> Result<TermIo> {
    let addr = format!("{host}:{port}");
    let stream =
        TcpStream::connect(&addr).map_err(|e| anyhow!("连接 {addr} 失败: {e}"))?;
    stream.set_nodelay(true).ok();

    let read_stream = stream.try_clone()?;
    let reply_stream = stream.try_clone()?;
    let mut write_stream = stream;
    write_stream
        .set_write_timeout(Some(Duration::from_secs(30)))
        .ok();

    let (input_tx, mut input_rx) = mpsc::unbounded_channel::<Vec<u8>>();
    let (output_tx, output_rx) = mpsc::unbounded_channel::<Vec<u8>>();
    let (resize_tx, mut resize_rx) = mpsc::unbounded_channel::<(u16, u16)>();

    // 读线程：Telnet 协商过滤后送 UI
    std::thread::spawn(move || {
        let mut reader = read_stream;
        let mut replies = reply_stream;
        let mut buf = [0u8; 8192];
        let mut pending: Vec<u8> = Vec::with_capacity(8192);
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    pending.extend_from_slice(&buf[..n]);
                    let (clean, reply, consumed) = filter_telnet(&pending);
                    if consumed > 0 {
                        pending.drain(..consumed);
                    }
                    if !reply.is_empty() {
                        let _ = replies.write_all(&reply);
                        let _ = replies.flush();
                    }
                    if !clean.is_empty() && output_tx.send(clean).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
        let _ = output_tx
            .send("\r\n\x1b[90m[TCP 连接已关闭]\x1b[0m".as_bytes().to_vec());
    });

    // 写线程
    std::thread::spawn(move || {
        while let Some(data) = input_rx.blocking_recv() {
            if write_stream.write_all(&data).is_err() || write_stream.flush().is_err() {
                break;
            }
        }
    });

    // resize：Telnet 不发送窗口尺寸（NAWS 需协商，这里忽略）
    std::thread::spawn(move || {
        while resize_rx.blocking_recv().is_some() {}
    });

    Ok(TermIo {
        input_tx,
        resize_tx,
        output_rx,
        ssh: None,
    })
}

/// 从 `buf` 中剥离 Telnet 命令。
///
/// 返回 `(可见数据, 需要回给对端的应答, 已消费字节数)`。
/// 末尾不完整的命令不消费，等待下一次读取补齐。
fn filter_telnet(buf: &[u8]) -> (Vec<u8>, Vec<u8>, usize) {
    let mut out = Vec::with_capacity(buf.len());
    let mut reply = Vec::new();
    let mut i = 0usize;
    while i < buf.len() {
        let b = buf[i];
        if b != IAC {
            out.push(b);
            i += 1;
            continue;
        }
        if i + 1 >= buf.len() {
            break; // 只剩 IAC，等更多数据
        }
        let cmd = buf[i + 1];
        match cmd {
            IAC => {
                out.push(IAC); // 转义的字面 0xFF
                i += 2;
            }
            DO | DONT | WILL | WONT => {
                if i + 2 >= buf.len() {
                    break;
                }
                let opt = buf[i + 2];
                match cmd {
                    DO => reply.extend_from_slice(&[IAC, WONT, opt]),
                    WILL => reply.extend_from_slice(&[IAC, DONT, opt]),
                    _ => {}
                }
                i += 3;
            }
            SB => {
                // 跳过子协商直到 IAC SE
                let mut j = i + 2;
                let mut closed = false;
                while j + 1 < buf.len() {
                    if buf[j] == IAC && buf[j + 1] == SE {
                        j += 2;
                        closed = true;
                        break;
                    }
                    j += 1;
                }
                if !closed {
                    break; // 子协商未结束，等更多数据
                }
                i = j;
            }
            _ => i += 2, // 其他两字节命令
        }
    }
    (out, reply, i)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passes_plain_text() {
        let (out, reply, n) = filter_telnet(b"hello");
        assert_eq!(out, b"hello");
        assert!(reply.is_empty());
        assert_eq!(n, 5);
    }

    #[test]
    fn answers_negotiation() {
        // IAC DO ECHO(1) 后跟 "abc"
        let data = [IAC, DO, 1, b'a', b'b', b'c'];
        let (out, reply, n) = filter_telnet(&data);
        assert_eq!(out, b"abc");
        assert_eq!(reply, vec![IAC, WONT, 1]);
        assert_eq!(n, 6);
    }

    #[test]
    fn keeps_incomplete_command() {
        let (_, _, n) = filter_telnet(&[b'x', IAC]);
        assert_eq!(n, 1, "末尾不完整的 IAC 不应被消费");
    }
}
