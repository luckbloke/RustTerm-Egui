//! SFTP 远程文件系统操作（复用已认证的 SSH 连接）。
//!
//! 与参考实现对齐的取舍：
//! - 隐藏属性由后端标出（`.xxx` 或 OpenSSH 的 0o10000000 位），
//!   是否显示由 UI 过滤——切换「显示隐藏文件」不必重新列目录；
//! - 传输支持进度回调与取消；用户主动取消才删除未完成的目标文件，
//!   网络错误保留半截文件供续传判断。

use anyhow::{anyhow, Result};
use russh_sftp::client::SftpSession;
use russh_sftp::protocol::OpenFlags;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::Mutex;

#[derive(Clone, Debug)]
pub struct RemoteEntry {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
    pub size: u64,
    /// 隐藏项（点开头，或 OpenSSH 扩展的隐藏位）
    pub hidden: bool,
}

/// 传输取消标志：区分「用户取消」与「IO 失败」，后者要留断点。
#[derive(Default)]
pub struct TransferCancel {
    cancelled: AtomicBool,
    by_user: AtomicBool,
}

impl TransferCancel {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub fn request_user_cancel(&self) {
        self.by_user.store(true, Ordering::SeqCst);
        self.cancelled.store(true, Ordering::SeqCst);
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Relaxed)
    }

    pub fn is_user_cancel(&self) -> bool {
        self.by_user.load(Ordering::Relaxed)
    }
}

/// 拼接远端路径（避免 `//` 与相对路径混用）。
pub fn join_remote(parent: &str, name: &str) -> String {
    if parent.is_empty() || parent == "." {
        name.to_string()
    } else if parent == "/" {
        format!("/{name}")
    } else {
        format!("{}/{}", parent.trim_end_matches('/'), name)
    }
}

/// 取父目录：`/a/b` → `/a`，`/a` → `/`，`a/b` → `a`。
pub fn parent_remote(path: &str) -> Option<String> {
    let p = path.trim_end_matches('/');
    if p.is_empty() || p == "." {
        return None;
    }
    match p.rfind('/') {
        Some(0) => Some("/".to_string()),
        Some(idx) => Some(p[..idx].to_string()),
        None => Some(".".to_string()),
    }
}

/// 从权限位判断隐藏（OpenSSH sftp-server 会额外置 0o10000000）。
fn is_hidden(name: &str, permissions: Option<u32>) -> bool {
    name.starts_with('.') || permissions.is_some_and(|m| m & 0o10000000 != 0)
}

pub struct SftpClient {
    session: Arc<Mutex<SftpSession>>,
}

impl SftpClient {
    pub fn new(session: SftpSession) -> Self {
        Self {
            session: Arc::new(Mutex::new(session)),
        }
    }

    /// 列目录。返回未过滤的完整列表（含隐藏项）。
    pub async fn list_dir(&self, path: &str) -> Result<Vec<RemoteEntry>> {
        let sftp = self.session.lock().await;
        let query = if path.is_empty() { "." } else { path };
        let dir = sftp
            .read_dir(query.to_string())
            .await
            .map_err(|e| anyhow!("列目录 {query} 失败: {e}"))?;

        let mut entries = Vec::new();
        for entry in dir {
            let name = entry.file_name();
            if name == "." || name == ".." {
                continue;
            }
            let file_type = entry.file_type();
            let is_dir = file_type.is_dir();
            let metadata = entry.metadata();
            let perms = metadata.permissions;
            entries.push(RemoteEntry {
                path: join_remote(query, &name),
                hidden: is_hidden(&name, perms),
                name,
                is_dir,
                size: metadata.size.unwrap_or(0),
            });
        }
        entries.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then_with(|| a.name.cmp(&b.name)));
        Ok(entries)
    }

    /// 远端家目录（用于「🏠」按钮）。
    pub async fn home_dir(&self) -> Result<String> {
        let sftp = self.session.lock().await;
        let p = sftp
            .canonicalize(".")
            .await
            .map_err(|e| anyhow!("获取远端家目录失败: {e}"))?;
        Ok(p)
    }

    pub async fn mkdir(&self, path: &str) -> Result<()> {
        let sftp = self.session.lock().await;
        sftp.create_dir(path.to_string())
            .await
            .map_err(|e| anyhow!("创建目录失败: {e}"))
    }

    pub async fn remove(&self, path: &str, is_dir: bool) -> Result<()> {
        let sftp = self.session.lock().await;
        if is_dir {
            sftp.remove_dir(path.to_string())
                .await
                .map_err(|e| anyhow!("删除目录失败: {e}"))
        } else {
            sftp.remove_file(path.to_string())
                .await
                .map_err(|e| anyhow!("删除文件失败: {e}"))
        }
    }

    pub async fn rename(&self, old: &str, new: &str) -> Result<()> {
        let sftp = self.session.lock().await;
        sftp.rename(old.to_string(), new.to_string())
            .await
            .map_err(|e| anyhow!("重命名失败: {e}"))
    }

    /// 下载远端文件到本地。`progress` 每块调用一次 (已传字节, 总字节)。
    pub async fn download(
        &self,
        remote: &str,
        local: &Path,
        cancel: Arc<TransferCancel>,
        progress: impl Fn(u64, u64) + Send,
    ) -> Result<()> {
        let mut file = {
            let sftp = self.session.lock().await;
            sftp.open_with_flags(remote.to_string(), OpenFlags::READ)
                .await
                .map_err(|e| anyhow!("打开远端文件失败: {e}"))?
        };
        let total = {
            let sftp = self.session.lock().await;
            sftp.metadata(remote.to_string())
                .await
                .ok()
                .and_then(|m| m.size)
                .unwrap_or(0)
        };

        let mut out = tokio::fs::File::create(local)
            .await
            .map_err(|e| anyhow!("创建本地文件失败: {e}"))?;

        let mut buf = vec![0u8; 64 * 1024];
        let mut sent: u64 = 0;
        progress(0, total);
        loop {
            if cancel.is_cancelled() {
                drop(out);
                if cancel.is_user_cancel() {
                    let _ = tokio::fs::remove_file(local).await;
                }
                return Err(anyhow!("传输已取消"));
            }
            let n = file
                .read(&mut buf)
                .await
                .map_err(|e| anyhow!("读取远端数据失败: {e}"))?;
            if n == 0 {
                break;
            }
            out.write_all(&buf[..n])
                .await
                .map_err(|e| anyhow!("写入本地文件失败: {e}"))?;
            sent += n as u64;
            progress(sent, total.max(sent));
        }
        out.flush().await.ok();
        progress(total.max(sent), total.max(sent));
        Ok(())
    }

    /// 上传本地文件到远端目录，目标名取本地文件名。
    pub async fn upload(
        &self,
        local: &Path,
        remote_dir: &str,
        cancel: Arc<TransferCancel>,
        progress: impl Fn(u64, u64) + Send,
    ) -> Result<()> {
        let name = local
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .ok_or_else(|| anyhow!("无法获取本地文件名"))?;
        let remote_path = join_remote(remote_dir, &name);

        let mut src = tokio::fs::File::open(local)
            .await
            .map_err(|e| anyhow!("打开本地文件失败: {e}"))?;
        let total = src.metadata().await.map(|m| m.len()).unwrap_or(0);

        let mut dst = {
            let sftp = self.session.lock().await;
            sftp.open_with_flags(
                remote_path.clone(),
                OpenFlags::WRITE | OpenFlags::CREATE | OpenFlags::TRUNCATE,
            )
            .await
            .map_err(|e| anyhow!("创建远端文件失败: {e}"))?
        };

        let mut buf = vec![0u8; 64 * 1024];
        let mut sent: u64 = 0;
        progress(0, total);
        loop {
            if cancel.is_cancelled() {
                if cancel.is_user_cancel() {
                    drop(dst);
                    let sftp = self.session.lock().await;
                    let _ = sftp.remove_file(remote_path.clone()).await;
                }
                return Err(anyhow!("传输已取消"));
            }
            let n = src
                .read(&mut buf)
                .await
                .map_err(|e| anyhow!("读取本地文件失败: {e}"))?;
            if n == 0 {
                break;
            }
            dst.write_all(&buf[..n])
                .await
                .map_err(|e| anyhow!("写入远端失败: {e}"))?;
            sent += n as u64;
            progress(sent, total.max(sent));
        }
        dst.flush().await.ok();
        progress(total.max(sent), total.max(sent));
        Ok(())
    }
}

/// 人类可读体积。
pub fn human_size(size: u64) -> String {
    const UNITS: &[&str] = &["B", "KB", "MB", "GB", "TB"];
    let mut v = size as f64;
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{} {}", v as u64, UNITS[i])
    } else {
        format!("{:.1} {}", v, UNITS[i])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn join_remote_handles_root_and_relative() {
        assert_eq!(join_remote("/", "etc"), "/etc");
        assert_eq!(join_remote("/home/u/", "a.txt"), "/home/u/a.txt");
        assert_eq!(join_remote(".", "a"), "a");
        assert_eq!(join_remote("", "a"), "a");
    }

    #[test]
    fn parent_remote_stops_at_root() {
        assert_eq!(parent_remote("/a/b").as_deref(), Some("/a"));
        assert_eq!(parent_remote("/a").as_deref(), Some("/"));
        assert_eq!(parent_remote("/").as_deref(), None);
        assert_eq!(parent_remote(".").as_deref(), None);
        assert_eq!(parent_remote("a/b").as_deref(), Some("a"));
    }

    #[test]
    fn hidden_detection_covers_dot_and_openssh_bit() {
        assert!(is_hidden(".bashrc", None));
        assert!(is_hidden("secret", Some(0o10000000 | 0o644)));
        assert!(!is_hidden("normal.txt", Some(0o100644)));
    }

    #[test]
    fn human_size_formats() {
        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(2048), "2.0 KB");
        assert_eq!(human_size(5 * 1024 * 1024), "5.0 MB");
    }
}
