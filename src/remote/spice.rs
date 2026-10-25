//! SPICE 客户端 —— 基于 vendor 的 capsaicin。
//!
//! 只维护主 surface（primary）作为桌面画面；光标与多 surface 叠层暂不合成
//! （与参考实现保持同一取舍）。帧输出仍是 RGBA8 脏矩形。

use anyhow::{anyhow, Result};
use capsaicin_client::{
    ClientEvent, DisplayEvent, InputEvent, MouseMode, RegionPixels, SpiceClient, SurfaceFormat,
};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::time::{Duration, Instant};

use super::{RdCommand, RdEvent, RdParams, RdState};

const FRAME_INTERVAL: Duration = Duration::from_millis(33);
const POLL_INTERVAL: Duration = Duration::from_millis(8);

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
    width: u32,
    height: u32,
    rgba: Vec<u8>,
}

impl Fb {
    fn new(width: u32, height: u32) -> Result<Self> {
        if width == 0 || height == 0 || width > 8192 || height > 8192 {
            return Err(anyhow!("SPICE 分辨率异常: {width}x{height}"));
        }
        Ok(Self {
            width,
            height,
            rgba: vec![0u8; width as usize * height as usize * 4],
        })
    }

    fn blit_raw(
        &mut self,
        x: i32,
        y: i32,
        w: u32,
        h: u32,
        data: &[u8],
        stride: u32,
        format: SurfaceFormat,
    ) {
        if w == 0 || h == 0 {
            return;
        }
        let dst_stride = self.width as usize * 4;
        for row in 0..h as usize {
            let dy = y as i64 + row as i64;
            if dy < 0 || dy >= self.height as i64 {
                continue;
            }
            let src_off = row * stride as usize;
            for col in 0..w as usize {
                let dx = x as i64 + col as i64;
                if dx < 0 || dx >= self.width as i64 {
                    continue;
                }
                let so = src_off + col * 4;
                if so + 4 > data.len() {
                    continue;
                }
                let (r, g, b) = convert_px(&data[so..so + 4], format);
                let doff = dy as usize * dst_stride + dx as usize * 4;
                self.rgba[doff] = r;
                self.rgba[doff + 1] = g;
                self.rgba[doff + 2] = b;
                self.rgba[doff + 3] = 255;
            }
        }
    }

    fn fill_solid(&mut self, x: i32, y: i32, w: u32, h: u32, color: u32) {
        let r = ((color >> 16) & 0xff) as u8;
        let g = ((color >> 8) & 0xff) as u8;
        let b = (color & 0xff) as u8;
        let stride = self.width as usize * 4;
        for row in 0..h as usize {
            let dy = y as i64 + row as i64;
            if dy < 0 || dy >= self.height as i64 {
                continue;
            }
            for col in 0..w as usize {
                let dx = x as i64 + col as i64;
                if dx < 0 || dx >= self.width as i64 {
                    continue;
                }
                let off = dy as usize * stride + dx as usize * 4;
                self.rgba[off] = r;
                self.rgba[off + 1] = g;
                self.rgba[off + 2] = b;
                self.rgba[off + 3] = 255;
            }
        }
    }

    fn copy_rect(&mut self, src_x: i32, src_y: i32, dst_x: i32, dst_y: i32, w: u32, h: u32) {
        let stride = self.width as usize * 4;
        let bytes = w as usize * 4;
        let mut tmp = vec![0u8; bytes];
        for row in 0..h as usize {
            let sy = src_y as i64 + row as i64;
            let dy = dst_y as i64 + row as i64;
            if sy < 0 || dy < 0 || sy >= self.height as i64 || dy >= self.height as i64 {
                continue;
            }
            if src_x < 0 || dst_x < 0 {
                continue;
            }
            let so = sy as usize * stride + src_x as usize * 4;
            let doff = dy as usize * stride + dst_x as usize * 4;
            if so + bytes > self.rgba.len() || doff + bytes > self.rgba.len() {
                continue;
            }
            tmp.copy_from_slice(&self.rgba[so..so + bytes]);
            self.rgba[doff..doff + bytes].copy_from_slice(&tmp);
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

fn convert_px(px: &[u8], format: SurfaceFormat) -> (u8, u8, u8) {
    match format {
        // 内存序 B,G,R,A
        SurfaceFormat::Xrgb8888 | SurfaceFormat::Argb8888 => (px[2], px[1], px[0]),
        SurfaceFormat::Rgb565 => {
            let v = u16::from_le_bytes([px[0], px[1]]) as u32;
            let r = ((v >> 11) & 0x1f) as u8;
            let g = ((v >> 5) & 0x3f) as u8;
            let b = (v & 0x1f) as u8;
            (r << 3 | r >> 2, g << 2 | g >> 4, b << 3 | b >> 2)
        }
        SurfaceFormat::Rgb555 => {
            let v = u16::from_le_bytes([px[0], px[1]]) as u32;
            let r = ((v >> 10) & 0x1f) as u8;
            let g = ((v >> 5) & 0x1f) as u8;
            let b = (v & 0x1f) as u8;
            (r << 3 | r >> 2, g << 3 | g >> 2, b << 3 | b >> 2)
        }
        _ => (0, 0, 0),
    }
}

pub async fn run(
    params: &RdParams,
    ev: &Sender<RdEvent>,
    cmds: &Receiver<RdCommand>,
    cancel: &AtomicBool,
) -> Result<()> {
    let _ = ev.send(RdEvent::State(RdState::Connecting));
    let addr = format!("{}:{}", params.host, params.port);
    let mut client = tokio::time::timeout(
        Duration::from_secs(20),
        SpiceClient::connect(&addr, &params.password),
    )
    .await
    .map_err(|_| anyhow!("SPICE 连接 {addr} 超时"))?
    .map_err(|e| anyhow!("SPICE 连接 {addr} 失败: {e}"))?;

    let _ = ev.send(RdEvent::State(RdState::Active));

    let mut fb: Option<Fb> = None;
    let mut fb_format = SurfaceFormat::Xrgb8888;
    let mut dirty: Dirty = None;
    let mut last_frame = Instant::now() - FRAME_INTERVAL;
    let mut buttons: u8 = 0;
    let mut last_pos = (0u16, 0u16);
    let mut absolute_mouse = true;

    loop {
        if cancel.load(Ordering::Relaxed) {
            client.close().await;
            return Ok(());
        }

        // ---- 上行输入 ----
        while let Ok(cmd) = cmds.try_recv() {
            match cmd {
                RdCommand::Close => {
                    client.close().await;
                    return Ok(());
                }
                RdCommand::Key(k) => {
                    let code = if k.scancode & 0xFF00 == 0xE000 {
                        0xE000u32 | (k.scancode & 0xFF) as u32
                    } else {
                        k.scancode as u32
                    };
                    if code != 0 {
                        let input = if k.down {
                            InputEvent::KeyDown(code)
                        } else {
                            InputEvent::KeyUp(code)
                        };
                        let _ = client.send_input(input).await;
                    }
                }
                RdCommand::Pointer { x, y, buttons: nb } => {
                    if absolute_mouse {
                        let _ = client
                            .send_input(InputEvent::MousePosition {
                                x: x as u32,
                                y: y as u32,
                                buttons: nb as u32,
                                display: 0,
                            })
                            .await;
                    } else {
                        let dx = x as i32 - last_pos.0 as i32;
                        let dy = y as i32 - last_pos.1 as i32;
                        if dx != 0 || dy != 0 {
                            let _ = client
                                .send_input(InputEvent::MouseMotion {
                                    dx,
                                    dy,
                                    buttons: nb as u32,
                                })
                                .await;
                        }
                    }
                    last_pos = (x, y);
                    // 按键状态变化
                    if nb != buttons {
                        for (bit, spice_btn) in [(1u8, 1u8), (2u8, 2u8), (4u8, 3u8)] {
                            let was = buttons & bit != 0;
                            let now = nb & bit != 0;
                            if was != now {
                                let input = if now {
                                    InputEvent::MousePress {
                                        button: spice_btn,
                                        buttons: nb as u32,
                                    }
                                } else {
                                    InputEvent::MouseRelease {
                                        button: spice_btn,
                                        buttons: nb as u32,
                                    }
                                };
                                let _ = client.send_input(input).await;
                            }
                        }
                        buttons = nb;
                    }
                }
                RdCommand::Wheel { dy } => {
                    // SPICE 滚轮用 4/5 号键
                    let button = if dy > 0 { 4u8 } else { 5u8 };
                    let _ = client
                        .send_input(InputEvent::MousePress {
                            button,
                            buttons: buttons as u32,
                        })
                        .await;
                    let _ = client
                        .send_input(InputEvent::MouseRelease {
                            button,
                            buttons: buttons as u32,
                        })
                        .await;
                }
            }
        }

        // ---- 下行事件 ----
        match tokio::time::timeout(POLL_INTERVAL, client.next_event()).await {
            Ok(Some(event)) => match event {
                ClientEvent::Display(DisplayEvent::SurfaceCreated {
                    width,
                    height,
                    format,
                    primary,
                    ..
                }) => {
                    if primary || fb.is_none() {
                        fb = Some(Fb::new(width, height)?);
                        fb_format = format;
                        let _ = ev.send(RdEvent::Resized { width, height });
                        dirty = Some((0, 0, width, height));
                    }
                }
                ClientEvent::Display(DisplayEvent::Mode { width, height, .. }) => {
                    if fb.is_none() && width > 0 && height > 0 {
                        fb = Some(Fb::new(width, height)?);
                        let _ = ev.send(RdEvent::Resized { width, height });
                        dirty = Some((0, 0, width, height));
                    }
                }
                ClientEvent::Display(DisplayEvent::Region {
                    surface_id,
                    rect,
                    pixels,
                    surface_format,
                }) => {
                    if surface_id != 0 {
                        continue;
                    }
                    let Some(f) = fb.as_mut() else { continue };
                    let x = rect.left;
                    let y = rect.top;
                    let w = rect.width() as u32;
                    let h = rect.height() as u32;
                    match pixels {
                        RegionPixels::Raw { data, stride } => {
                            f.blit_raw(x, y, w, h, &data, stride, surface_format)
                        }
                        RegionPixels::SolidColor(color) => f.fill_solid(x, y, w, h, color),
                    }
                    dirty = union(
                        dirty,
                        (x.max(0) as u32, y.max(0) as u32, w, h),
                    );
                }
                ClientEvent::Display(DisplayEvent::CopyRect {
                    surface_id,
                    src_x,
                    src_y,
                    dest_rect,
                }) => {
                    if surface_id != 0 {
                        continue;
                    }
                    if let Some(f) = fb.as_mut() {
                        f.copy_rect(
                            src_x,
                            src_y,
                            dest_rect.left,
                            dest_rect.top,
                            dest_rect.width() as u32,
                            dest_rect.height() as u32,
                        );
                        dirty = union(
                            dirty,
                            (
                                dest_rect.left.max(0) as u32,
                                dest_rect.top.max(0) as u32,
                                dest_rect.width() as u32,
                                dest_rect.height() as u32,
                            ),
                        );
                    }
                }
                ClientEvent::Display(DisplayEvent::StreamFrame { dest_rect, pixels, .. }) => {
                    if let Some(f) = fb.as_mut() {
                        let w = dest_rect.width() as u32;
                        let h = dest_rect.height() as u32;
                        match pixels {
                            RegionPixels::Raw { data, stride } => f.blit_raw(
                                dest_rect.left,
                                dest_rect.top,
                                w,
                                h,
                                &data,
                                stride,
                                SurfaceFormat::Xrgb8888,
                            ),
                            RegionPixels::SolidColor(color) => f.fill_solid(
                                dest_rect.left,
                                dest_rect.top,
                                w,
                                h,
                                color,
                            ),
                        }
                        dirty = union(
                            dirty,
                            (
                                dest_rect.left.max(0) as u32,
                                dest_rect.top.max(0) as u32,
                                w,
                                h,
                            ),
                        );
                    }
                }
                ClientEvent::Display(DisplayEvent::Reset) => {
                    if let Some(f) = fb.as_mut() {
                        f.rgba.iter_mut().for_each(|b| *b = 0);
                        dirty = Some((0, 0, f.width, f.height));
                    }
                }
                ClientEvent::MouseMode(mode) => {
                    absolute_mouse = matches!(mode, MouseMode::Client);
                }
                ClientEvent::Closed(err) => {
                    return Err(anyhow!(
                        "SPICE 连接关闭{}",
                        err.map(|e| format!(": {e}")).unwrap_or_default()
                    ));
                }
                _ => {}
            },
            Ok(None) => return Err(anyhow!("SPICE 事件流结束")),
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
        let _ = fb_format;
    }
}
