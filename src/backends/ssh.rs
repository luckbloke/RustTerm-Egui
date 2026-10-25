//! SSH 后端：与主工程 src-tauri/src/ssh.rs 相同的 vendor russh（0.62 fork）。
//!
//! P1 内容：
//! - 算法协商与主工程一致：默认安全算法之后追加 SHA-1/CBC/des 兜底；
//! - 主机密钥校验：accept-new（TOFU）策略，密钥变更一律拒绝；
//! - 认证：password 失败后 fallback 到 keyboard-interactive（PAM 场景）。

use anyhow::{anyhow, bail, Result};
use russh::client::{self, Handle};
use russh::keys::PrivateKeyWithHashAlg;
use russh::{Channel, ChannelMsg};
use std::borrow::Cow;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::mpsc;

use super::TermIo;

/// SSH 认证方式。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SshAuth {
    /// 密码（失败后自动 fallback 到 keyboard-interactive）
    Password(String),
    /// 私钥文件（可带 passphrase）
    Key {
        path: String,
        passphrase: String,
    },
}

impl SshAuth {
    pub fn is_empty(&self) -> bool {
        match self {
            SshAuth::Password(p) => p.is_empty(),
            SshAuth::Key { path, .. } => path.is_empty(),
        }
    }
}

/// 跳板机（ProxyJump）配置。
///
/// 实现方式：先连跳板机并完成认证，再用 `direct-tcpip` 通道把
/// 「跳板机 → 目标机」的 TCP 流接到目标机的 SSH 握手上（真正的 ProxyJump，
/// 不是本地端口转发）。
#[derive(Clone, Debug)]
pub struct JumpHost {
    pub host: String,
    pub port: u16,
    pub user: String,
    /// 跳板机凭据；多数场景与目标机相同，故沿用同一种认证方式
    pub auth: SshAuth,
}

impl Default for JumpHost {
    fn default() -> Self {
        Self {
            host: String::new(),
            port: 22,
            user: String::new(),
            auth: SshAuth::Password(String::new()),
        }
    }
}

impl JumpHost {
    /// 解析 `user@host:port` 形式（各部分可省略）。
    pub fn parse(spec: &str, default_user: &str, auth: SshAuth) -> Option<Self> {
        let spec = spec.trim();
        if spec.is_empty() {
            return None;
        }
        let (user, rest) = match spec.rsplit_once('@') {
            Some((u, r)) => (u.to_string(), r),
            None => (default_user.to_string(), spec),
        };
        let (host, port) = match rest.rsplit_once(':') {
            Some((h, p)) => (
                h.to_string(),
                p.trim().parse::<u16>().unwrap_or(22),
            ),
            None => (rest.to_string(), 22),
        };
        if host.trim().is_empty() {
            return None;
        }
        Some(Self {
            host: host.trim().to_string(),
            port,
            user: if user.trim().is_empty() {
                default_user.to_string()
            } else {
                user.trim().to_string()
            },
            auth,
        })
    }
}

/// known_hosts 路径：显式取主目录（不用 russh 默认的 env::home_dir）。
fn known_hosts_path() -> Option<PathBuf> {
    let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME"))?;
    Some(Path::new(&home).join(".ssh").join("known_hosts"))
}

/// accept-new 策略的主机密钥校验：
/// - 命中记录 → 信任；
/// - 全新主机 → 写入 known_hosts 后放行（TOFU）；
/// - 记录不一致 → 拒绝（防中间人，无可协商）。
struct HostKeyChecker {
    host: String,
    port: u16,
    path: Option<PathBuf>,
}

impl client::Handler for HostKeyChecker {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        key: &russh::keys::PublicKey,
    ) -> Result<bool, Self::Error> {
        let Some(path) = self.path.clone() else {
            return Ok(true); // 取不到主目录时退化为全放行（与 P0 行为一致）
        };
        match russh::keys::check_known_hosts_path(&self.host, self.port, key, &path) {
            // 命中已有记录
            Ok(true) => Ok(true),
            // 全新主机：写入失败不阻断连接（accept-new 语义）
            Ok(false) => {
                match russh::keys::known_hosts::learn_known_hosts_path(
                    &self.host,
                    self.port,
                    key,
                    &path,
                ) {
                    Ok(()) => {
                        eprintln!("[ssh] 首次连接 {host}:{port}，已记录主机密钥", host = self.host, port = self.port);
                        Ok(true)
                    }
                    Err(e) => {
                        eprintln!("[ssh] 警告：主机密钥写入 known_hosts 失败: {e}");
                        Ok(true)
                    }
                }
            }
            // 已有记录但密钥不一致：一律拒绝
            Err(russh::keys::Error::KeyChanged { line }) => {
                eprintln!(
                    "[ssh] 拒绝连接 {host}:{port}：known_hosts 第 {line} 行的主机密钥已变更",
                    host = self.host,
                    port = self.port
                );
                Ok(false)
            }
            Err(e) => {
                eprintln!("[ssh] 主机密钥校验 IO 错误: {e}");
                Ok(false)
            }
        }
    }
}

/// 显式配置：在默认安全算法之后，追加旧算法兜底（与主工程一致）。
fn hardened_config() -> client::Config {
    let mut config = client::Config::default();
    {
        let p = &mut config.preferred;

        // 旧 KEX：DH group1 / group14 (SHA-1)
        let mut kex: Vec<russh::kex::Name> = p.kex.iter().copied().collect();
        kex.push(russh::kex::DH_G1_SHA1);
        kex.push(russh::kex::DH_G14_SHA1);
        kex.push(russh::kex::DH_GEX_SHA1);
        p.kex = Cow::Owned(kex);

        // 旧 cipher：AES-CBC / 3DES
        let mut cipher: Vec<russh::cipher::Name> = p.cipher.iter().copied().collect();
        cipher.push(russh::cipher::AES_128_CBC);
        cipher.push(russh::cipher::AES_192_CBC);
        cipher.push(russh::cipher::AES_256_CBC);
        cipher.push(russh::cipher::TRIPLE_DES_CBC);
        p.cipher = Cow::Owned(cipher);

        // 旧 MAC：HMAC-SHA1
        let mut mac: Vec<russh::mac::Name> = p.mac.iter().copied().collect();
        mac.push(russh::mac::HMAC_SHA1);
        p.mac = Cow::Owned(mac);

        // 旧主机密钥算法：ssh-rsa (SHA-1)
        let mut key: Vec<russh::keys::Algorithm> = p.key.iter().cloned().collect();
        key.push(russh::keys::Algorithm::Rsa { hash: None });
        p.key = Cow::Owned(key);
    }
    config
}

/// 认证：密码优先，失败后 fallback 到 keyboard-interactive；或直接用私钥。
async fn authenticate(
    handle: &mut Handle<HostKeyChecker>,
    user: &str,
    auth: &SshAuth,
) -> Result<()> {
    match auth {
        SshAuth::Password(password) => {
            // 1. password
            let resp = handle
                .authenticate_password(user, password)
                .await
                .map_err(|e| anyhow!("password 认证失败: {e}"))?;
            if resp.success() {
                return Ok(());
            }
            eprintln!("[ssh] password 认证失败: {resp:?}，尝试 keyboard-interactive");

            // 2. keyboard-interactive：把密码作为提示响应（PAM 常见 "Password: " 提示）
            let resp = handle
                .authenticate_keyboard_interactive_start(user, None::<String>)
                .await
                .map_err(|e| anyhow!("keyboard-interactive 失败: {e}"))?;

            match resp {
                KeyboardInteractiveAuthResponse::Success => Ok(()),
                KeyboardInteractiveAuthResponse::Failure { .. } => {
                    bail!("认证失败：密码错误")
                }
                KeyboardInteractiveAuthResponse::InfoRequest { .. } => {
                    bail!("认证失败：服务器要求多轮键盘交互")
                }
            }
        }
        SshAuth::Key { path, passphrase } => {
            if path.trim().is_empty() {
                bail!("未填写私钥路径");
            }
            let pp = if passphrase.is_empty() {
                None
            } else {
                Some(passphrase.as_str())
            };
            // 读私钥（能解出 passphrase 保护的 OpenSSH/PEM 私钥）
            let key = russh::keys::load_secret_key(path.trim(), pp)
                .map_err(|e| anyhow!("读取私钥 {path} 失败: {e}"))?;
            // RSA 需要协商签名哈希（sha-rsa / rsa-sha2-256/512）
            let hash_alg = handle.best_supported_rsa_hash().await.ok().flatten().flatten();
            let key = PrivateKeyWithHashAlg::new(Arc::new(key), hash_alg);
            let resp = handle
                .authenticate_publickey(user, key)
                .await
                .map_err(|e| anyhow!("公钥认证失败: {e}"))?;
            if resp.success() {
                Ok(())
            } else {
                bail!("密钥认证失败：请确认该私钥已在服务器的 authorized_keys 中")
            }
        }
    }
}

use russh::client::KeyboardInteractiveAuthResponse;

pub async fn spawn(
    host: String,
    port: u16,
    user: String,
    auth: SshAuth,
    jump: Option<JumpHost>,
    cols: u16,
    rows: u16,
) -> Result<TermIo> {
    let jump = jump.filter(|j| !j.host.trim().is_empty());
    match jump {
        None => {
            // 直连：把 TCP 流交给统一下游处理
            let tcp = tokio::time::timeout(
                std::time::Duration::from_secs(15),
                tokio::net::TcpStream::connect((host.as_str(), port)),
            )
            .await
            .map_err(|_| anyhow!("连接 {host}:{port} 超时"))?
            .map_err(|e| anyhow!("连接 {host}:{port} 失败: {e}"))?;
            let _ = tcp.set_nodelay(true);
            open_shell(tcp, host, port, user, auth, cols, rows, None).await
        }
        Some(j) => {
            // 经跳板机：在跳板机连接上开 direct-tcpip 通道
            let jcfg = Arc::new(hardened_config());
            let checker = HostKeyChecker {
                host: j.host.clone(),
                port: j.port,
                path: known_hosts_path(),
            };
            let mut jh: Handle<HostKeyChecker> = tokio::time::timeout(
                std::time::Duration::from_secs(15),
                client::connect(jcfg, (j.host.as_str(), j.port), checker),
            )
            .await
            .map_err(|_| anyhow!("连接跳板机 {}:{} 超时", j.host, j.port))?
            .map_err(|e| anyhow!("连接跳板机 {}:{} 失败: {e}", j.host, j.port))?;

            authenticate(&mut jh, &j.user, &j.auth)
                .await
                .map_err(|e| anyhow!("跳板机 {} 认证失败: {e}", j.host))?;

            let channel = jh
                .channel_open_direct_tcpip(host.clone(), port as u32, "127.0.0.1", 0)
                .await
                .map_err(|e| {
                    anyhow!("无法在跳板机上打开到 {host}:{port} 的通道: {e}")
                })?;
            let stream = channel.into_stream();
            open_shell(
                stream,
                host,
                port,
                user,
                auth,
                cols,
                rows,
                Some(Arc::new(jh)),
            )
            .await
        }
    }
}

/// 已在认证后的 SSH 连接上开子通道。
///
/// 终端的 IO 走独立的 session channel，SFTP 需要在这条**同一条 TCP 连接**上
/// 再开一个 channel 并请求 `sftp` 子系统——这样不用重新认证，
/// 也不用再连一次（跳板机场景尤其重要）。
pub struct SshHandle {
    handle: Arc<Handle<HostKeyChecker>>,
    /// 建链信息，供 UI 显示（user@host:port）
    pub target: String,
}

impl SshHandle {
    /// 打开一个 SFTP 子系统会话。
    pub async fn open_sftp(
        &self,
    ) -> Result<russh_sftp::client::SftpSession> {
        let channel = self
            .handle
            .channel_open_session()
            .await
            .map_err(|e| anyhow!("打开 SFTP 通道失败: {e}"))?;
        channel
            .request_subsystem(true, "sftp")
            .await
            .map_err(|e| anyhow!("请求 sftp 子系统失败: {e}"))?;
        russh_sftp::client::SftpSession::new(channel.into_stream())
            .await
            .map_err(|e| anyhow!("SFTP 握手失败: {e}"))
    }
}

/// 在给定字节流上完成目标机 SSH 握手、认证并开 shell。
async fn open_shell<S>(
    stream: S,
    host: String,
    port: u16,
    user: String,
    auth: SshAuth,
    cols: u16,
    rows: u16,
    keep_jump: Option<Arc<Handle<HostKeyChecker>>>,
) -> Result<TermIo>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let config = Arc::new(hardened_config());
    let checker = HostKeyChecker {
        host: host.clone(),
        port,
        path: known_hosts_path(),
    };
    let mut handle: Handle<HostKeyChecker> =
        tokio::time::timeout(
            std::time::Duration::from_secs(20),
            client::connect_stream(config, stream, checker),
        )
        .await
        .map_err(|_| anyhow!("到 {host}:{port} 的 SSH 握手超时"))?
        .map_err(|e| anyhow!("连接 {host}:{port} 失败: {e}"))?;

    authenticate(&mut handle, &user, &auth).await?;

    let mut channel: Channel<client::Msg> = handle.channel_open_session().await?;
    channel
        .request_pty(false, "xterm-256color", cols as u32, rows as u32, 0, 0, &[])
        .await?;
    channel.request_shell(false).await?;

    let (input_tx, mut input_rx) = mpsc::unbounded_channel::<Vec<u8>>();
    let (resize_tx, mut resize_rx) = mpsc::unbounded_channel::<(u16, u16)>();
    let (output_tx, output_rx) = mpsc::unbounded_channel::<Vec<u8>>();

    // 保持 Handle 存活直到循环退出（跳板机 Handle 也要一并保活）
    let keep = Arc::new(handle);
    // 同一个连接上给 SFTP 面板留一个句柄（不额外建链、不重复认证）
    let ssh_handle = Arc::new(SshHandle {
        handle: keep.clone(),
        target: format!("{}@{}:{}", user, host, port),
    });
    let keep_for_task = keep.clone();

    tokio::spawn(async move {
        let _keep_alive = (keep_for_task, keep_jump);
        loop {
            tokio::select! {
                msg = channel.wait() => {
                    match msg {
                        Some(ChannelMsg::Data { ref data }) => {
                            if output_tx.send(data.to_vec()).is_err() {
                                break;
                            }
                        }
                        Some(ChannelMsg::Eof) | Some(ChannelMsg::Close) | None => {
                            let _ = output_tx.send(
                                "\r\n\x1b[90m[SSH 连接已断开]\x1b[0m".as_bytes().to_vec(),
                            );
                            break;
                        }
                        _ => {}
                    }
                }
                Some(data) = input_rx.recv() => {
                    let _ = channel.data(&data[..]).await;
                }
                Some((c, r)) = resize_rx.recv() => {
                    let _ = channel.window_change(c as u32, r as u32, 0, 0).await;
                }
            }
        }
    });

    Ok(TermIo {
        input_tx,
        resize_tx,
        output_rx,
        ssh: Some(ssh_handle),
    })
}
