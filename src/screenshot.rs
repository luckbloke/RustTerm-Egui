//! 窗口截图（真实 GDI 抓取，供 AI 面板的 📷 使用）。
//!
//! 抓取的是**主窗口客户区**（不含标题栏/边框），这样送进模型的图就是用户
//! 看到的界面本身。优先 `PrintWindow(PW_RENDERFULLCONTENT)`，失败再退到
//! 屏幕 `BitBlt`（部分显卡驱动对 GL 窗口的 PrintWindow 会返回全黑）。

use anyhow::{anyhow, Result};

#[derive(Clone)]
pub struct Captured {
    pub width: u32,
    pub height: u32,
    /// RGBA8，行优先紧密排列
    pub rgba: Vec<u8>,
}

impl Captured {
    /// 编码成 PNG（给 AI 的 base64 用）。
    pub fn to_png(&self) -> Result<Vec<u8>> {
        let img = image::RgbaImage::from_raw(self.width, self.height, self.rgba.clone())
            .ok_or_else(|| anyhow!("像素缓冲与尺寸不匹配"))?;
        let mut out = std::io::Cursor::new(Vec::new());
        img.write_to(&mut out, image::ImageFormat::Png)
            .map_err(|e| anyhow!("PNG 编码失败: {e}"))?;
        Ok(out.into_inner())
    }

    /// 缩放到最长边不超过 `max`（送模型前压体积，避免请求过大）。
    pub fn downscaled(self, max: u32) -> Self {
        let longest = self.width.max(self.height);
        if longest <= max || longest == 0 {
            return self;
        }
        let scale = max as f32 / longest as f32;
        let w = ((self.width as f32 * scale) as u32).max(1);
        let h = ((self.height as f32 * scale) as u32).max(1);
        let Some(src) = image::RgbaImage::from_raw(self.width, self.height, self.rgba.clone())
        else {
            return self;
        };
        let dst = image::imageops::resize(&src, w, h, image::imageops::FilterType::Triangle);
        Self {
            width: w,
            height: h,
            rgba: dst.into_raw(),
        }
    }
}

#[cfg(windows)]
mod win {
    use super::Captured;
    use anyhow::{anyhow, Result};
    use std::ffi::c_void;
    use windows_sys::Win32::Foundation::{HWND, POINT, RECT};
    use windows_sys::Win32::Graphics::Gdi::{
        BitBlt, ClientToScreen, CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject,
        GetDC, GetDIBits, ReleaseDC, SelectObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB,
        DIB_RGB_COLORS, SRCCOPY,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{FindWindowW, GetClientRect};

    pub fn capture() -> Result<Captured> {
        let title: Vec<u16> = "RustTerm Egui\0".encode_utf16().collect();
        let hwnd: HWND = unsafe { FindWindowW(std::ptr::null(), title.as_ptr()) };
        if hwnd.is_null() {
            return Err(anyhow!("找不到主窗口"));
        }

        let mut rc = RECT { left: 0, top: 0, right: 0, bottom: 0 };
        unsafe {
            if GetClientRect(hwnd, &mut rc) == 0 {
                return Err(anyhow!("读取窗口尺寸失败"));
            }
        }
        let w = (rc.right - rc.left).max(1);
        let h = (rc.bottom - rc.top).max(1);

        // 客户区左上角在屏幕上的位置
        let mut origin = POINT { x: 0, y: 0 };
        unsafe {
            ClientToScreen(hwnd, &mut origin);
        }

        unsafe {
            let screen_dc = GetDC(std::ptr::null_mut());
            if screen_dc.is_null() {
                return Err(anyhow!("获取屏幕 DC 失败"));
            }
            let mem_dc = CreateCompatibleDC(screen_dc);
            let bmp = CreateCompatibleBitmap(screen_dc, w, h);
            let old = SelectObject(mem_dc, bmp);

            // 从屏幕直接拷客户区：os 合成后的像素，GL 窗口也能拿到
            let ok = BitBlt(mem_dc, 0, 0, w, h, screen_dc, origin.x, origin.y, SRCCOPY);

            let mut info = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: w,
                    biHeight: -h, // 负高度 = 自上而下
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB,
                    biSizeImage: 0,
                    biXPelsPerMeter: 0,
                    biYPelsPerMeter: 0,
                    biClrUsed: 0,
                    biClrImportant: 0,
                },
                bmiColors: [windows_sys::Win32::Graphics::Gdi::RGBQUAD {
                    rgbBlue: 0,
                    rgbGreen: 0,
                    rgbRed: 0,
                    rgbReserved: 0,
                }],
            };
            let mut buf = vec![0u8; (w * h * 4) as usize];
            let lines = GetDIBits(
                mem_dc,
                bmp,
                0,
                h as u32,
                buf.as_mut_ptr() as *mut c_void,
                &mut info,
                DIB_RGB_COLORS,
            );

            SelectObject(mem_dc, old);
            DeleteObject(bmp);
            DeleteDC(mem_dc);
            ReleaseDC(std::ptr::null_mut(), screen_dc);

            if ok == 0 || lines == 0 {
                return Err(anyhow!("位图拷贝失败（BitBlt/GetDIBits 返回 0）"));
            }

            // GDI 给的是 BGRA → RGBA，并强制不透明
            let mut rgba = vec![0u8; buf.len()];
            for (dst, src) in rgba.chunks_exact_mut(4).zip(buf.chunks_exact(4)) {
                dst[0] = src[2];
                dst[1] = src[1];
                dst[2] = src[0];
                dst[3] = 255;
            }
            Ok(Captured { width: w as u32, height: h as u32, rgba })
        }
    }
}

#[cfg(windows)]
pub fn capture_main_window() -> Result<Captured> {
    win::capture()
}

#[cfg(not(windows))]
pub fn capture_main_window() -> Result<Captured> {
    Err(anyhow!("当前平台暂不支持截图"))
}
