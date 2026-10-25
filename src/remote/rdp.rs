//! RDP 客户端 —— 基于 vendor 的 IronRDP。
//!
//! 与 VNC 一样把帧统一成 RGBA8 脏矩形，UI 侧用同一套贴图逻辑。
//! 证书验证采用「首次信任」策略：不弹窗、不落盘，直接接受
//! （与参考实现里 accept-temporarily 的策略等价）。

use anyhow::{anyhow, Result};
use ironrdp::client::config::{
    ConfigBuilder, Destination, DirectTransport, ServerCertificateVerifier,
    ServerCertificateVerifyFuture, TransportKind,
};
use ironrdp::client::rdp::{RdpClient, RdpInputEvent, RdpOutputEvent};
use ironrdp::input::{Database, MouseButton, MousePosition, Operation, Scancode, WheelRotations};
use ironrdp::pdu::geometry::InclusiveRectangle;
use ironrdp::pdu::rdp::capability_sets::MajorPlatformType;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::time::{Duration, Instant};

use super::{KeyInput, RdCommand, RdEvent, RdParams, RdState};

const FRAME_INTERVAL: Duration = Duration::from_millis(33);
const POLL_INTERVAL: Duration = Duration::from_millis(8);

/// 接受任意服务器证书。
#[derive(Debug)]
struct TrustAnyCert;

impl ServerCertificateVerifier for TrustAnyCert {
    fn verify_server_certificate<'a>(
        &'a self,
        _host: &'a str,
        _port: u16,
        _der: Vec<u8>,
    ) -> ServerCertificateVerifyFuture<'a> {
        Box::pin(async move { Ok(()) })
    }
}

fn client_build() -> u32 {
    env!("CARGO_PKG_VERSION")
        .split('.')
        .take(3)
        .fold(0_u32, |acc, part| {
            acc.saturating_mul(100)
                .saturating_add(part.parse::<u32>().unwrap_or(0))
        })
}

fn client_name() -> String {
    std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "RustTerm".to_string())
}

fn build_config(params: &RdParams) -> Result<ironrdp::client::config::Config> {
    let connector_host = params.host.clone();
    let connector_port = params.port;
    let color_depth = match params.color_depth {
        16 => 16u32,
        24 => 24,
        _ => 32,
    };
    ConfigBuilder::new()
        .with_destination(Destination::from_parts(params.host.clone(), params.port))
        .with_transport(TransportKind::Direct)
        .with_username(params.username.clone())
        .with_domain(params.domain.clone())
        .with_password(params.password.clone())
        .with_desktop_width(params.width)
        .with_desktop_height(params.height)
        .with_desktop_scale_factor(100)
        .with_color_depth(color_depth)
        .with_credssp(true)
        .with_tls(true)
        .with_platform(MajorPlatformType::WINDOWS)
        .with_client_build(client_build())
        .with_client_dir("C:\\Windows\\System32\\mstscax.dll")
        .with_client_name(client_name())
        .with_server_certificate_verifier(std::sync::Arc::new(TrustAnyCert))
        // 与参考实现一致：自己拨号，绕开 connector 内部的 DNS/地址处理差异
        .with_direct_transport_connector(std::sync::Arc::new(move |_destination| {
            let host = connector_host.clone();
            let port = connector_port;
            Box::pin(async move {
                let stream = tokio::net::TcpStream::connect((host.as_str(), port))
                    .await
                    .map_err(|e| {
                        std::io::Error::new(
                            e.kind(),
                            format!("无法连接 RDP 服务器 {host}:{port}: {e}"),
                        )
                    })?;
                Ok(DirectTransport {
                    stream: Box::new(stream),
                    local_addr: None,
                    peer_addr: None,
                })
            })
        }))
        .build()
        .map_err(|e| anyhow!("RDP 配置失败: {e:?}"))
}

type Dirty = Option<(u32, u32, u32, u32)>;

fn union(dirty: Dirty, r: (u32, u32, u32, u32)) -> Dirty {
    if r.2 == 0 || r.3 == 0 {
        return dirty;
    }
    Some(match dirty {
        None => r,
        Some(d) => {
            let x1 = d.0.min(r.0);
            let y1 = d.1.min(r.1);
            let x2 = (d.0 + d.2).max(r.0 + r.2);
            let y2 = (d.1 + d.3).max(r.1 + r.3);
            (x1, y1, x2 - x1, y2 - y1)
        }
    })
}

struct Fb {
    pixels: Vec<u32>,
    width: u32,
    height: u32,
}

impl Fb {
    fn new(width: u32, height: u32) -> Self {
        Self {
            pixels: vec![0u32; width as usize * height as usize],
            width,
            height,
        }
    }

    fn apply_region(&mut self, buffer: &[u32], region: InclusiveRectangle) -> (u32, u32, u32, u32) {
        let rx = region.left as usize;
        let ry = region.top as usize;
        let rw = (region.right as usize).saturating_sub(rx) + 1;
        let rh = (region.bottom as usize).saturating_sub(ry) + 1;
        let fw = self.width as usize;
        let fh = self.height as usize;
        let cw = rw.min(fw.saturating_sub(rx));
        let ch = rh.min(fh.saturating_sub(ry));
        if cw == 0 || ch == 0 {
            return (0, 0, 0, 0);
        }
        for row in 0..ch {
            let so = row * rw;
            if so + cw > buffer.len() {
                break;
            }
            let doff = (ry + row) * fw + rx;
            self.pixels[doff..doff + cw].copy_from_slice(&buffer[so..so + cw]);
        }
        (rx as u32, ry as u32, cw as u32, ch as u32)
    }

    fn sub_rgba(&self, x: u32, y: u32, w: u32, h: u32) -> Vec<u8> {
        let stride = self.width as usize;
        let mut out = Vec::with_capacity(w as usize * h as usize * 4);
        for row in 0..h as usize {
            let off = (y as usize + row) * stride + x as usize;
            for col in 0..w as usize {
                let p = self.pixels[off + col];
                // ironrdp 的 u32 像素是 0x00RRGGBB
                out.push(((p >> 16) & 0xff) as u8);
                out.push(((p >> 8) & 0xff) as u8);
                out.push((p & 0xff) as u8);
                out.push(255);
            }
        }
        out
    }
}

/// 把归一化键输入转成 ironrdp 操作。
fn key_operations(k: &KeyInput) -> Vec<Operation> {
    let extended = k.scancode & 0xFF00 == 0xE000;
    let code = (k.scancode & 0x00FF) as u8;
    // 有扫描码就按物理键发；否则退回 Unicode 通道（输入法/特殊字符）
    if code != 0 {
        let sc = Scancode::from_u8(extended, code);
        return vec![if k.down {
            Operation::KeyPressed(sc)
        } else {
            Operation::KeyReleased(sc)
        }];
    }
    match k.ch {
        Some(ch) if k.down => vec![
            Operation::UnicodeKeyPressed(ch),
            Operation::UnicodeKeyReleased(ch),
        ],
        _ => Vec::new(),
    }
}

/// 一批操作交给 Database 转成 fast-path 输入事件再发出去。
fn flush_ops(
    db: &mut Database,
    sender: &ironrdp::client::rdp::RdpInputSender,
    ops: Vec<Operation>,
) {
    if ops.is_empty() {
        return;
    }
    let fast = db.apply(ops);
    if !fast.is_empty() {
        let _ = sender.try_send(RdpInputEvent::FastPath(fast));
    }
}

pub async fn run(
    params: &RdParams,
    ev: &Sender<RdEvent>,
    cmds: &Receiver<RdCommand>,
    cancel: &AtomicBool,
) -> Result<()> {
    let _ = ev.send(RdEvent::State(RdState::Connecting));
    let config = build_config(params)?;
    let (out_tx, mut out_rx) = ironrdp::client::output_channel::output_channel(2);
    let client = RdpClient::new(config, out_tx).with_desktop_updates();
    let input_sender = client.input_sender();

    let _ = ev.send(RdEvent::State(RdState::Authenticating));

    // client.run() 不是 Send：按参考实现放到独立线程 + 独立单线程运行时
    std::thread::Builder::new()
        .name("rdp-client".into())
        .spawn(move || {
            let rt = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    eprintln!("[RDP] runtime 创建失败: {e}");
                    return;
                }
            };
            rt.block_on(client.run());
        })
        .map_err(|e| anyhow!("启动 RDP 客户端线程失败: {e}"))?;

    let mut fb: Option<Fb> = None;
    let mut dirty: Dirty = None;
    let mut last_frame = Instant::now() - FRAME_INTERVAL;
    let mut db = Database::new();
    let mut buttons: u8 = 0;
    let mut last_pos = (0u16, 0u16);
    let mut active = false;

    loop {
        if cancel.load(Ordering::Relaxed) {
            return Ok(());
        }

        // ---- 上行输入 ----
        while let Ok(cmd) = cmds.try_recv() {
            match cmd {
                RdCommand::Close => return Ok(()),
                RdCommand::Key(k) => {
                    let ops = key_operations(&k);
                    if !ops.is_empty() {
                        flush_ops(&mut db, &input_sender, ops);
                    }
                }
                RdCommand::Pointer { x, y, buttons: nb } => {
                    let mut ops = vec![Operation::MouseMove(MousePosition { x, y })];
                    if nb != buttons {
                        for (bit, btn) in [
                            (1u8, MouseButton::Left),
                            (2u8, MouseButton::Middle),
                            (4u8, MouseButton::Right),
                        ] {
                            let was = buttons & bit != 0;
                            let now = nb & bit != 0;
                            if was != now {
                                ops.push(if now {
                                    Operation::MouseButtonPressed(btn)
                                } else {
                                    Operation::MouseButtonReleased(btn)
                                });
                            }
                        }
                        buttons = nb;
                    }
                    last_pos = (x, y);
                    flush_ops(&mut db, &input_sender, ops);
                }
                RdCommand::Wheel { dy } => {
                    if dy != 0 {
                        let ops = vec![
                            Operation::MouseMove(MousePosition {
                                x: last_pos.0,
                                y: last_pos.1,
                            }),
                            Operation::WheelRotations(WheelRotations {
                                is_vertical: true,
                                rotation_units: dy.saturating_mul(120),
                            }),
                        ];
                        flush_ops(&mut db, &input_sender, ops);
                    }
                }
            }
        }

        // ---- 下行帧 ----
        match tokio::time::timeout(POLL_INTERVAL, out_rx.recv()).await {
            Ok(Some(out)) => match out {
                RdpOutputEvent::DesktopUpdate(update) => {
                    let (buffer, w, h, region) = update.into_parts();
                    let (w, h) = (w.get() as u32, h.get() as u32);
                    let created = fb.is_none();
                    let f = fb.get_or_insert_with(|| Fb::new(w, h));
                    if f.width != w || f.height != h {
                        *f = Fb::new(w, h);
                        let _ = ev.send(RdEvent::Resized { width: w, height: h });
                        dirty = None;
                    } else if created {
                        let _ = ev.send(RdEvent::Resized { width: w, height: h });
                    }
                    let r = f.apply_region(&buffer, region);
                    dirty = union(dirty, r);
                    if !active {
                        active = true;
                        let _ = ev.send(RdEvent::State(RdState::Active));
                    }
                }
                RdpOutputEvent::Image { buffer, width, height } => {
                    let (w, h) = (width.get() as u32, height.get() as u32);
                    let mut f = Fb::new(w, h);
                    let region = InclusiveRectangle {
                        left: 0,
                        top: 0,
                        right: w.saturating_sub(1) as u16,
                        bottom: h.saturating_sub(1) as u16,
                    };
                    f.apply_region(&buffer, region);
                    fb = Some(f);
                    let _ = ev.send(RdEvent::Resized { width: w, height: h });
                    dirty = Some((0, 0, w, h));
                    if !active {
                        active = true;
                        let _ = ev.send(RdEvent::State(RdState::Active));
                    }
                }
                RdpOutputEvent::Terminated(reason) => {
                    return Err(anyhow!("RDP 会话终止: {reason:?}"));
                }
                RdpOutputEvent::ConnectionFailure(err) => {
                    return Err(anyhow!("RDP 连接失败: {err:?}"));
                }
                _ => {}
            },
            Ok(None) => return Err(anyhow!("RDP 输出通道关闭")),
            Err(_) => {}
        }

        // ---- 推送脏区 ----
        if let (Some(rect), Some(f)) = (dirty, fb.as_ref()) {
            if last_frame.elapsed() >= FRAME_INTERVAL {
                last_frame = Instant::now();
                dirty = None;
                let (x, y, w, h) = rect;
                let w = w.min(f.width.saturating_sub(x));
                let h = h.min(f.height.saturating_sub(y));
                if w > 0 && h > 0 {
                    let rgba = f.sub_rgba(x, y, w, h);
                    let _ = ev.send(RdEvent::Frame { x, y, w, h, rgba });
                }
            }
        }
    }
}
