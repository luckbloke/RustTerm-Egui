//! 端口隧道：本地 TCP 监听 → 远端目标中转。
//!
//! 用于把本机某端口转发到可达的远端主机端口（例如临时暴露内网服务）。
//! 纯 std::net 实现，每个隧道独立线程 accept + 双向 relay。
//!
//! 说明：这里实现的是**直连 TCP 转发**，不依赖 SSH 会话；
//! 需要经跳板机转发时，请把目标填成跳板机可达的地址。

use std::io::{Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// 单条隧道的运行状态。
pub struct TunnelHandle {
    pub id: u64,
    pub local_port: u16,
    pub target_host: String,
    pub target_port: u16,
    /// 已接受的连接数
    pub connections: Arc<AtomicU64>,
    /// 最近一次错误
    pub last_error: Arc<std::sync::Mutex<Option<String>>>,
    stop: Arc<AtomicBool>,
}

impl TunnelHandle {
    pub fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }

    pub fn running(&self) -> bool {
        !self.stop.load(Ordering::Relaxed)
    }

    pub fn connections(&self) -> u64 {
        self.connections.load(Ordering::Relaxed)
    }

    pub fn error(&self) -> Option<String> {
        self.last_error.lock().ok().and_then(|g| g.clone())
    }
}

/// 启动一条隧道。`local_port == 0` 表示由系统分配。
pub fn start(
    id: u64,
    local_port: u16,
    target_host: &str,
    target_port: u16,
) -> Result<TunnelHandle, String> {
    let listener = TcpListener::bind(("127.0.0.1", local_port))
        .map_err(|e| format!("监听 127.0.0.1:{local_port} 失败: {e}"))?;
    let actual_port = listener
        .local_addr()
        .map_err(|e| format!("读取监听端口失败: {e}"))?
        .port();
    listener
        .set_nonblocking(false)
        .map_err(|e| format!("设置监听模式失败: {e}"))?;

    let stop = Arc::new(AtomicBool::new(false));
    let connections = Arc::new(AtomicU64::new(0));
    let last_error = Arc::new(std::sync::Mutex::new(None));

    let target = format!("{target_host}:{target_port}");
    let stop_c = stop.clone();
    let conn_c = connections.clone();
    let err_c = last_error.clone();

    std::thread::spawn(move || {
        // 用超时轮询代替阻塞 accept，便于响应 stop
        listener.set_nonblocking(true).ok();
        loop {
            if stop_c.load(Ordering::Relaxed) {
                break;
            }
            match listener.accept() {
                Ok((client, _addr)) => {
                    conn_c.fetch_add(1, Ordering::Relaxed);
                    let target = target.clone();
                    let err = err_c.clone();
                    std::thread::spawn(move || {
                        if let Err(e) = relay(client, &target) {
                            if let Ok(mut g) = err.lock() {
                                *g = Some(e);
                            }
                        }
                    });
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(120));
                }
                Err(e) => {
                    if let Ok(mut g) = err_c.lock() {
                        *g = Some(format!("accept 失败: {e}"));
                    }
                    std::thread::sleep(Duration::from_millis(200));
                }
            }
        }
    });

    Ok(TunnelHandle {
        id,
        local_port: actual_port,
        target_host: target_host.to_string(),
        target_port,
        connections,
        last_error,
        stop,
    })
}

/// 客户端 ↔ 目标双向转接。
fn relay(client: TcpStream, target: &str) -> Result<(), String> {
    let upstream =
        TcpStream::connect(target).map_err(|e| format!("连接目标 {target} 失败: {e}"))?;
    client.set_nodelay(true).ok();
    upstream.set_nodelay(true).ok();

    let c2 = client
        .try_clone()
        .map_err(|e| format!("复制客户端句柄失败: {e}"))?;
    let mut u_read = upstream
        .try_clone()
        .map_err(|e| format!("复制上游句柄失败: {e}"))?;

    // 客户端 → 上游
    let mut c_read = client;
    let mut u_write = upstream;
    let t = std::thread::spawn(move || {
        let mut buf = [0u8; 8192];
        loop {
            match c_read.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    if u_write.write_all(&buf[..n]).is_err() {
                        break;
                    }
                    let _ = u_write.flush();
                }
                Err(_) => break,
            }
        }
        let _ = u_write.shutdown(Shutdown::Both);
    });

    // 上游 → 客户端
    let mut c_write = c2;
    let mut buf = [0u8; 8192];
    loop {
        match u_read.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                if c_write.write_all(&buf[..n]).is_err() {
                    break;
                }
                let _ = c_write.flush();
            }
            Err(_) => break,
        }
    }
    let _ = c_write.shutdown(Shutdown::Both);
    let _ = t.join();
    Ok(())
}
