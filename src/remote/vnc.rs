//! VNC (RFB 3.x) 客户端 —— 基于 vendor 的 vnc-rs。
//!
//! 帧格式固定为 **RGBA8**：连接时强制 `PixelFormat::bgra()`，因此 32bpp
//! 原始数据只需把首尾字节对调即可；Tight 编码的 JPEG 分块单独解码后贴回
//! 帧缓冲，保证后续增量帧与全屏一致。

use anyhow::{anyhow, Result};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::time::{Duration, Instant};
use tokio::net::TcpStream;
use vnc::{
    ClientKeyEvent, ClientMouseEvent, PixelFormat, Rect, VncConnector, VncEncoding, VncError,
    VncEvent, VncLimits, VncSecurityPolicy, VncServerKey, X11Event,
};

use super::{RdCommand, RdEvent, RdParams, RdState};

const FRAME_INTERVAL: Duration = Duration::from_millis(33);
const REFRESH_INTERVAL: Duration = Duration::from_millis(16);
/// 兜底的全量刷新间隔（增量请求失效时靠它兜住）
const FULL_REFRESH_INTERVAL: Duration = Duration::from_millis(2000);
/// 与参考实现一致的画面尺寸上限
const MAX_FRAMEBUFFER_WIDTH: u16 = 7680;
const MAX_FRAMEBUFFER_HEIGHT: u16 = 4320;
const POLL_INTERVAL: Duration = Duration::from_millis(8);

struct Framebuffer {
    width: u32,
    height: u32,
    rgba: Vec<u8>,
}

impl Framebuffer {
    fn new(width: u32, height: u32) -> Result<Self> {
        if width == 0 || height == 0 || width > 8192 || height > 8192 {
            return Err(anyhow!("VNC 分辨率异常: {width}x{height}"));
        }
        Ok(Self {
            width,
            height,
            rgba: vec![0u8; width as usize * height as usize * 4],
        })
    }

    /// 原始像素写入帧缓冲（32bpp 按 BGRA 处理，其余按 shift 通用解码）。
    fn blit(&mut self, rect: Rect, data: &[u8], pf: &PixelFormat) -> Result<()> {
        let bpp = (pf.bits_per_pixel / 8).max(1) as usize;
        let (rx, ry) = (rect.x as u32, rect.y as u32);
        let (rw, rh) = (rect.width as u32, rect.height as u32);
        if rw == 0 || rh == 0 {
            return Ok(());
        }
        if rx + rw > self.width || ry + rh > self.height {
            // 服务器偶尔会发越界矩形（缩放/桌面尺寸切换中），裁剪掉
            return Ok(());
        }
        let src_stride = rw as usize * bpp;
        if data.len() < src_stride * rh as usize {
            return Err(anyhow!(
                "VNC 数据长度不足: {} < {} (bpp={bpp})",
                data.len(),
                src_stride * rh as usize
            ));
        }
        let dst_stride = self.width as usize * 4;
        for row in 0..rh as usize {
            let src = &data[row * src_stride..row * src_stride + src_stride];
            let dst_off = (ry as usize + row) * dst_stride + rx as usize * 4;
            let dst = &mut self.rgba[dst_off..dst_off + rw as usize * 4];
            for col in 0..rw as usize {
                let px = &src[col * bpp..col * bpp + bpp];
                let (r, g, b) = decode_pixel(px, pf);
                dst[col * 4] = r;
                dst[col * 4 + 1] = g;
                dst[col * 4 + 2] = b;
                dst[col * 4 + 3] = 255;
            }
        }
        Ok(())
    }

    /// 合并一块已解码好的 RGBA 图像（Tight/JPEG 分块）。
    fn blit_rgba(&mut self, rect: Rect, img: &[u8], iw: u32, ih: u32) {
        let (rx, ry) = (rect.x as u32, rect.y as u32);
        if rx >= self.width || ry >= self.height {
            return;
        }
        let w = iw.min(self.width - rx) as usize;
        let h = ih.min(self.height - ry) as usize;
        let dst_stride = self.width as usize * 4;
        for row in 0..h {
            let src_off = row * iw as usize * 4;
            let dst_off = (ry as usize + row) * dst_stride + rx as usize * 4;
            self.rgba[dst_off..dst_off + w * 4]
                .copy_from_slice(&img[src_off..src_off + w * 4]);
        }
    }

    fn copy_rect(&mut self, dst: Rect, src: Rect) {
        let w = src.width.min(dst.width) as usize;
        let h = src.height.min(dst.height) as usize;
        if w == 0 || h == 0 {
            return;
        }
        let stride = self.width as usize * 4;
        let mut tmp = vec![0u8; w * 4];
        for y in 0..h {
            let sy = src.y as usize + y;
            let dy = dst.y as usize + y;
            if sy >= self.height as usize || dy >= self.height as usize {
                break;
            }
            let so = sy * stride + src.x as usize * 4;
            if so + w * 4 > self.rgba.len() {
                break;
            }
            tmp.copy_from_slice(&self.rgba[so..so + w * 4]);
            let doff = dy * stride + dst.x as usize * 4;
            if doff + w * 4 > self.rgba.len() {
                break;
            }
            self.rgba[doff..doff + w * 4].copy_from_slice(&tmp);
        }
    }

    fn sub_rgba(&self, x: u32, y: u32, w: u32, h: u32) -> Vec<u8> {
        let stride = self.width as usize * 4;
        let mut out = Vec::with_capacity(w as usize * h as usize * 4);
        for row in 0..h as usize {
            let off = (y as usize + row) * stride + x as usize * 4;
            out.extend_from_slice(&self.rgba[off..off + w as usize * 4]);
        }
        out
    }
}

fn decode_pixel(px: &[u8], pf: &PixelFormat) -> (u8, u8, u8) {
    // 我们请求的是 bgra()：内存序 B,G,R,A
    if pf.bits_per_pixel == 32 && px.len() >= 4 {
        return (px[2], px[1], px[0]);
    }
    let value: u32 = match pf.bits_per_pixel {
        8 => px[0] as u32,
        16 if px.len() >= 2 => {
            if pf.big_endian_flag != 0 {
                u16::from_be_bytes([px[0], px[1]]) as u32
            } else {
                u16::from_le_bytes([px[0], px[1]]) as u32
            }
        }
        _ => return (0, 0, 0),
    };
    if pf.true_color_flag == 0 {
        return (0, 0, 0);
    }
    let scale = |v: u32, max: u16| -> u8 {
        if max == 0 {
            0
        } else {
            ((v * 255) / max as u32) as u8
        }
    };
    let r = (value >> pf.red_shift) & pf.red_max as u32;
    let g = (value >> pf.green_shift) & pf.green_max as u32;
    let b = (value >> pf.blue_shift) & pf.blue_max as u32;
    (
        scale(r, pf.red_max),
        scale(g, pf.green_max),
        scale(b, pf.blue_max),
    )
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

pub async fn run(
    params: &RdParams,
    ev: &Sender<RdEvent>,
    cmds: &Receiver<RdCommand>,
    cancel: &AtomicBool,
) -> Result<()> {
    let _ = ev.send(RdEvent::State(RdState::Connecting));
    let username = params.username.clone();
    let addr = (params.host.as_str(), params.port);
    let password = params.password.clone();

    let _ = ev.send(RdEvent::State(RdState::Authenticating));

    // 握手要试两轮：
    //   1) 带凭据（服务器要 VncAuth / RA2 时走这条）
    //   2) 不带凭据（服务器用 None 无密码时走这条）
    // 只按「有没有密码」决定策略会在两种服务器上各错一半：
    // 有密码但没填 → 服务器只认 VncAuth，报 "Unsupported security type"；
    // 没密码但填了 → 服务器只给 None，同样报不支持。
    let mut client: Option<vnc::VncClient> = None;
    let mut last_err: Option<anyhow::Error> = None;

    for attempt in 0..2 {
        let use_credentials = attempt == 0;

        let tcp = tokio::time::timeout(Duration::from_secs(15), TcpStream::connect(addr))
            .await
            .map_err(|_| anyhow!("连接 {}:{} 超时", params.host, params.port))?
            .map_err(|e| anyhow!("连接 {}:{} 失败: {e}", params.host, params.port))?;
        let _ = tcp.set_nodelay(true);

        let pwd = password.clone();
        let connector = VncConnector::new(tcp)
            .set_auth_method(async move { Ok(pwd) })
            .set_credentials_available(use_credentials)
            .set_username(username.clone())
            .set_security_policy(VncSecurityPolicy::Auto)
            // RA2 / VeNCrypt 服务器必须提供校验回调，否则握手直接失败
            .set_server_key_verifier(|key: VncServerKey| async move {
                eprintln!("[VNC] 接受服务器密钥（{} bit）", key.bits());
                Ok(())
            })
            .set_pixel_format(PixelFormat::bgra())
            .add_encoding(VncEncoding::Zrle)
            .add_encoding(VncEncoding::Tight)
            .add_encoding(VncEncoding::CopyRect)
            .add_encoding(VncEncoding::Raw)
            .add_encoding(VncEncoding::DesktopSizePseudo)
            .allow_shared(true)
            .set_limits(VncLimits {
                max_framebuffer_width: MAX_FRAMEBUFFER_WIDTH,
                max_framebuffer_height: MAX_FRAMEBUFFER_HEIGHT,
                max_framebuffer_pixels: usize::from(MAX_FRAMEBUFFER_WIDTH)
                    * usize::from(MAX_FRAMEBUFFER_HEIGHT),
                max_rectangles_per_update: 1024,
                max_encoded_payload_bytes: 64 * 1024 * 1024,
                max_decoded_payload_bytes: usize::from(MAX_FRAMEBUFFER_WIDTH)
                    * usize::from(MAX_FRAMEBUFFER_HEIGHT)
                    * 4,
                channel_capacity: 32,
                ..VncLimits::default()
            })
            .build()
            .map_err(|e| anyhow!("VNC 初始化失败: {e}"))?;

        match tokio::time::timeout(Duration::from_secs(20), connector.try_start()).await {
            Ok(Ok(handshake)) => {
                match handshake.finish() {
                    Ok(c) => {
                        client = Some(c);
                        break;
                    }
                    Err(e) => {
                        return Err(match e {
                            VncError::WrongPassword => anyhow!("VNC 密码错误"),
                            other => anyhow!("VNC 认证失败: {other}"),
                        });
                    }
                }
            }
            Ok(Err(VncError::UnsupportedSecurityType)) if use_credentials => {
                // 服务器不认「带密码」这条路，再试一次无密码
                last_err = Some(anyhow!(
                    "服务器要求密码认证但握手未通过：请在会话或连接表单中填写 VNC 密码"
                ));
                continue;
            }
            Ok(Err(e)) => return Err(anyhow!("VNC 握手失败: {e}")),
            Err(_) => return Err(anyhow!("VNC 握手超时")),
        }
    }

    let client = match client {
        Some(c) => c,
        None => return Err(last_err.unwrap_or_else(|| anyhow!("VNC 握手失败"))),
    };

    let _ = ev.send(RdEvent::State(RdState::Active));

    let mut fb: Option<Framebuffer> = None;
    let mut pf = PixelFormat::bgra();
    let mut dirty: Dirty = None;
    let mut last_frame = Instant::now() - FRAME_INTERVAL;
    let mut last_refresh = Instant::now() - REFRESH_INTERVAL;
    let mut buttons: u8 = 0;
    let mut active = false;
    // 首次必须请求「非增量」画面：只发增量请求时服务器可能一直不回帧
    let mut need_full = true;
    let mut last_full = Instant::now() - FULL_REFRESH_INTERVAL;

    loop {
        if cancel.load(Ordering::Relaxed) {
            let _ = client.close().await;
            return Ok(());
        }

        // ---- 上行输入 ----
        while let Ok(cmd) = cmds.try_recv() {
            match cmd {
                RdCommand::Close => {
                    let _ = client.close().await;
                    return Ok(());
                }
                RdCommand::Key(k) => {
                    let _ = client
                        .input(X11Event::KeyEvent(ClientKeyEvent {
                            keycode: k.keysym,
                            down: k.down,
                        }))
                        .await;
                }
                RdCommand::Pointer { x, y, buttons: new_buttons } => {
                    buttons = new_buttons;
                    let _ = client
                        .input(X11Event::PointerEvent(ClientMouseEvent {
                            position_x: x,
                            position_y: y,
                            bottons: buttons,
                        }))
                        .await;
                }
                RdCommand::Wheel { dy } => {
                    // 滚轮：按位 3/4 发一次按下+抬起（VNC 约定）
                    let bit: u8 = if dy > 0 { 0x08 } else { 0x10 };
                    for mask in [buttons | bit, buttons] {
                        let _ = client
                            .input(X11Event::PointerEvent(ClientMouseEvent {
                                position_x: 0,
                                position_y: 0,
                                bottons: mask,
                            }))
                            .await;
                    }
                }
            }
        }

        // ---- 下行事件 ----
        match tokio::time::timeout(POLL_INTERVAL, client.poll_event()).await {
            Ok(Ok(Some(event))) => match event {
                VncEvent::SetResolution(screen) => {
                    fb = Some(Framebuffer::new(screen.width as u32, screen.height as u32)?);
                    let _ = ev.send(RdEvent::Resized {
                        width: screen.width as u32,
                        height: screen.height as u32,
                    });
                    dirty = Some((0, 0, screen.width as u32, screen.height as u32));
                    need_full = true;
                    active = false;
                }
                VncEvent::SetPixelFormat(p) => pf = p,
                VncEvent::RawImage(rect, data) => {
                    if let Some(f) = fb.as_mut() {
                        f.blit(rect, &data, &pf)?;
                        dirty = union(
                            dirty,
                            (rect.x as u32, rect.y as u32, rect.width as u32, rect.height as u32),
                        );
                    }
                }
                VncEvent::Copy(dst, src) => {
                    if let Some(f) = fb.as_mut() {
                        f.copy_rect(dst, src);
                        dirty = union(
                            dirty,
                            (dst.x as u32, dst.y as u32, dst.width as u32, dst.height as u32),
                        );
                    }
                }
                VncEvent::JpegImage(rect, data) => {
                    if let Some(f) = fb.as_mut() {
                        if let Ok(img) = image::load_from_memory(&data) {
                            let rgba = img.to_rgba8();
                            let (w, h) = (rgba.width(), rgba.height());
                            f.blit_rgba(rect, rgba.as_raw(), w, h);
                            dirty = union(dirty, (rect.x as u32, rect.y as u32, w, h));
                        }
                    }
                }
                VncEvent::Error(msg) => return Err(anyhow!("VNC 服务器错误: {msg}")),
                _ => {}
            },
            Ok(Ok(None)) => {}
            Ok(Err(e)) => {
                let msg = match e {
                    VncError::IoError(e) => format!("连接中断: {e}"),
                    other => format!("{other}"),
                };
                return Err(anyhow!("{msg}"));
            }
            Err(_) => {} // 轮询超时：正常
        }

        // ---- 定时请求画面更新 ----
        if last_refresh.elapsed() >= REFRESH_INTERVAL {
            last_refresh = Instant::now();
            if fb.is_some() {
                let full = need_full || last_full.elapsed() >= FULL_REFRESH_INTERVAL;
                let r = if full {
                    need_full = false;
                    last_full = Instant::now();
                    client.input(X11Event::FullRefresh).await
                } else {
                    client.input(X11Event::Refresh).await
                };
                if let Err(e) = r {
                    return Err(anyhow!("发送画面更新请求失败: {e}"));
                }
            }
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
                    if !active {
                        active = true;
                        let _ = ev.send(RdEvent::State(RdState::Active));
                    }
                }
            }
        }
    }
}
