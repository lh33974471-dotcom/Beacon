use super::drmtap_dl::{
    self, drmtap_config, drmtap_ctx, drmtap_cursor_info, drmtap_device, drmtap_display,
    drmtap_dmabuf_desc, drmtap_frame_info, DrmtapLib,
};
use hbb_common::log;
use std::ffi::CString;
use std::io;
use std::os::fd::{FromRawFd, OwnedFd};

pub(crate) const MAX_DIM: u32 = 16384;
pub(crate) const MAX_FRAME_BYTES: usize = 256 * 1024 * 1024;
pub(crate) const DRM_FORMAT_XRGB8888: u32 = 0x3432_5258;
pub(crate) const DRM_FORMAT_ARGB8888: u32 = 0x3432_5241;
pub(crate) const DRM_FORMAT_XBGR8888: u32 = 0x3432_4258;
pub(crate) const DRM_FORMAT_ABGR8888: u32 = 0x3432_4241;

pub const HIDDEN_CURSOR_ID: u64 = u64::MAX;

pub struct CursorSnapshot {
    pub id: u64,
    pub width: u32,
    pub height: u32,
    pub hotx: i32,
    pub hoty: i32,
    pub colors: Vec<u8>,
}

pub struct DisplaySnapshot {
    pub name: String,
    pub crtc_id: u32,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub active: bool,
}

pub struct DrmDevice {
    pub path: String,
    pub render_node: String,
    pub display_count: u32,
}

fn cstr_field(buf: &[std::os::raw::c_char]) -> String {
    let bytes: &[u8] =
        unsafe { std::slice::from_raw_parts(buf.as_ptr() as *const u8, buf.len()) };
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..end]).into_owned()
}

pub fn list_devices() -> Option<Vec<DrmDevice>> {
    let lib = drmtap_dl::get()?;
    let f = lib.list_devices?;
    const MAX: usize = 16;
    let mut raw: [drmtap_device; MAX] = unsafe { std::mem::zeroed() };
    let n = unsafe { f(raw.as_mut_ptr(), MAX as std::os::raw::c_int) };
    if n < 0 {
        log::warn!("drmtap_list_devices failed ({n}); using single-device auto-detect");
        return None;
    }
    let n = (n as usize).min(MAX);
    Some(
        raw[..n]
            .iter()
            .map(|d| DrmDevice {
                path: cstr_field(&d.path),
                render_node: cstr_field(&d.render_node),
                display_count: d.display_count,
            })
            .collect(),
    )
}

pub(super) fn device_under_dev_dri(path: &str) -> Option<std::path::PathBuf> {
    let p = std::fs::canonicalize(path).ok()?;
    if p.parent() == Some(std::path::Path::new("/dev/dri")) {
        Some(p)
    } else {
        None
    }
}

pub struct DrmReader {
    lib: &'static DrmtapLib,
    ctx: *mut drmtap_ctx,
    buf: Vec<u8>,
}

impl DrmReader {
    pub fn open(device: Option<&str>, crtc_id: u32) -> Option<DrmReader> {
        let lib = drmtap_dl::get()?;
        let device_cstr = match device {
            None => None,
            Some(d) => {
                let Some(canonical) = device_under_dev_dri(d) else {
                    log::warn!("DRM device {d:?} is not under /dev/dri; refusing to open");
                    return None;
                };
                match canonical.to_str().and_then(|s| CString::new(s).ok()) {
                    Some(c) => Some(c),
                    None => return None,
                }
            }
        };
        let cfg = drmtap_config {
            device_path: device_cstr.as_ref().map_or(std::ptr::null(), |c| c.as_ptr()),
            crtc_id,
            helper_path: std::ptr::null(),
            debug: 0,
        };
        let ctx = unsafe { (lib.open)(&cfg) };
        drop(device_cstr);
        if ctx.is_null() {
            log::info!("drmtap_open failed; DRM capture unavailable");
            return None;
        }
        Some(DrmReader {
            lib,
            ctx,
            buf: Vec::new(),
        })
    }

    pub fn grab(&mut self) -> io::Result<(&[u8], usize, usize)> {
        unsafe {
            let mut frame: drmtap_frame_info = std::mem::zeroed();
            let ret = (self.lib.grab_mapped)(self.ctx, &mut frame);
            if ret < 0 {
                let errno = -ret;
                if errno == hbb_common::libc::EAGAIN
                    || errno == hbb_common::libc::EBUSY
                    || errno == hbb_common::libc::EINTR
                {
                    return Err(io::ErrorKind::WouldBlock.into());
                }
                return Err(io::Error::new(
                    io::ErrorKind::Other,
                    format!("drmtap_grab_mapped failed: errno {errno}"),
                ));
            }
            if frame.data.is_null() || frame.width == 0 || frame.height == 0 {
                (self.lib.frame_release)(self.ctx, &mut frame);
                return Err(io::ErrorKind::WouldBlock.into());
            }
            let w = frame.width;
            let h = frame.height;
            let stride = frame.stride as usize;
            if w > MAX_DIM || h > MAX_DIM || stride < (w as usize) * 4 {
                log::warn!(
                    "DRM scanout not 32-bit BGRA-compatible ({w}x{h} stride {stride} fourcc {:#010x}); falling back",
                    frame.format
                );
                (self.lib.frame_release)(self.ctx, &mut frame);
                return Err(io::Error::new(
                    io::ErrorKind::Other,
                    "unsupported DRM scanout format",
                ));
            }
            if frame.format != 0
                && frame.format != DRM_FORMAT_XRGB8888
                && frame.format != DRM_FORMAT_ARGB8888
            {
                log::warn!(
                    "DRM scanout fourcc {:#010x} is not BGRA-compatible; falling back",
                    frame.format
                );
                (self.lib.frame_release)(self.ctx, &mut frame);
                return Err(io::Error::new(
                    io::ErrorKind::Other,
                    "unsupported DRM scanout format",
                ));
            }
            let (w, h) = (w as usize, h as usize);
            let frame_size = match w.checked_mul(4).and_then(|x| x.checked_mul(h)) {
                Some(sz) if sz > 0 && sz <= MAX_FRAME_BYTES => sz,
                other => {
                    log::warn!(
                        "DRM scanout geometry {w}x{h} yields an out-of-range frame ({other:?} bytes); falling back"
                    );
                    (self.lib.frame_release)(self.ctx, &mut frame);
                    return Err(io::Error::new(
                        io::ErrorKind::Other,
                        "DRM scanout frame too large",
                    ));
                }
            };
            match stride.checked_mul(h) {
                Some(sz) if sz > 0 && sz <= MAX_FRAME_BYTES => {}
                other => {
                    log::warn!(
                        "DRM scanout stride {stride} x {h} rows is out of range ({other:?} bytes); falling back"
                    );
                    (self.lib.frame_release)(self.ctx, &mut frame);
                    return Err(io::Error::new(
                        io::ErrorKind::Other,
                        "DRM scanout stride out of range",
                    ));
                }
            }
            if self.buf.len() != frame_size {
                self.buf.resize(frame_size, 0);
            }
            let src = frame.data as *const u8;
            let dst = self.buf.as_mut_ptr();
            if stride == w * 4 {
                std::ptr::copy_nonoverlapping(src, dst, frame_size);
            } else {
                for y in 0..h {
                    std::ptr::copy_nonoverlapping(src.add(y * stride), dst.add(y * w * 4), w * 4);
                }
            }
            (self.lib.frame_release)(self.ctx, &mut frame);
            Ok((&self.buf, w, h))
        }
    }

    pub fn render_node(&mut self) -> Option<String> {
        let f = self.lib.render_node?;
        let ptr = unsafe { f(self.ctx) };
        if ptr.is_null() {
            return None;
        }
        unsafe { std::ffi::CStr::from_ptr(ptr) }
            .to_str()
            .ok()
            .map(|s| s.to_owned())
    }

    pub fn grab_desc(&mut self) -> io::Result<(OwnedFd, drmtap_dmabuf_desc)> {
        let grab_desc = self.lib.grab_desc;
        unsafe {
            let mut desc: drmtap_dmabuf_desc = std::mem::zeroed();
            let mut frame: drmtap_frame_info = std::mem::zeroed();
            let ret = grab_desc(self.ctx, &mut desc, &mut frame);
            if ret < 0 {
                let errno = -ret;
                if errno == hbb_common::libc::EAGAIN
                    || errno == hbb_common::libc::EBUSY
                    || errno == hbb_common::libc::EINTR
                {
                    return Err(io::ErrorKind::WouldBlock.into());
                }
                if errno == hbb_common::libc::ENOTSUP {
                    return Err(io::Error::new(
                        io::ErrorKind::Unsupported,
                        "drmtap_grab_desc: no transferable dma-buf (ENOTSUP)",
                    ));
                }
                return Err(io::Error::new(
                    io::ErrorKind::Other,
                    format!("drmtap_grab_desc failed: errno {errno}"),
                ));
            }
            let raw_fd = if desc.dma_buf_fd >= 0 {
                desc.dma_buf_fd
            } else {
                frame.dma_buf_fd
            };
            if raw_fd < 0 {
                (self.lib.frame_release)(self.ctx, &mut frame);
                return Err(io::ErrorKind::WouldBlock.into());
            }
            let w = desc.width;
            let h = desc.height;
            if w == 0 || h == 0 || w > MAX_DIM || h > MAX_DIM {
                (self.lib.frame_release)(self.ctx, &mut frame);
                return Err(io::Error::new(
                    io::ErrorKind::Other,
                    format!("DRM scanout geometry {w}x{h} out of range"),
                ));
            }
            let planes = if desc.num_planes == 0 { 1 } else { desc.num_planes };
            if planes > 4 {
                (self.lib.frame_release)(self.ctx, &mut frame);
                return Err(io::Error::new(
                    io::ErrorKind::Other,
                    format!("DRM scanout num_planes {} out of range (1..=4)", desc.num_planes),
                ));
            }
            for p in 0..(planes as usize) {
                let extent = (desc.pitches[p] as usize)
                    .checked_mul(h as usize)
                    .and_then(|rows| rows.checked_add(desc.offsets[p] as usize));
                match extent {
                    Some(end) if end <= MAX_FRAME_BYTES => {}
                    other => {
                        (self.lib.frame_release)(self.ctx, &mut frame);
                        return Err(io::Error::new(
                            io::ErrorKind::Other,
                            format!(
                                "DRM scanout plane {p} out of range (offset {} pitch {} over {h} rows -> {other:?}, cap {MAX_FRAME_BYTES})",
                                desc.offsets[p], desc.pitches[p]
                            ),
                        ));
                    }
                }
            }
            let dup_fd = hbb_common::libc::fcntl(raw_fd, hbb_common::libc::F_DUPFD_CLOEXEC, 0);
            if dup_fd < 0 {
                let e = io::Error::last_os_error();
                (self.lib.frame_release)(self.ctx, &mut frame);
                return Err(e);
            }
            let owned = OwnedFd::from_raw_fd(dup_fd);
            (self.lib.frame_release)(self.ctx, &mut frame);
            desc.num_planes = planes;
            desc.dma_buf_fd = -1;
            Ok((owned, desc))
        }
    }

    pub fn cursor(&mut self) -> Option<CursorSnapshot> {
        unsafe {
            let mut c: drmtap_cursor_info = std::mem::zeroed();
            let cret = (self.lib.get_cursor)(self.ctx, &mut c);
            if cret != 0 {
                return None;
            }
            let out = if c.visible == 0 {
                Some(CursorSnapshot {
                    id: HIDDEN_CURSOR_ID,
                    width: 1,
                    height: 1,
                    hotx: 0,
                    hoty: 0,
                    colors: vec![0, 0, 0, 0],
                })
            } else if !c.pixels.is_null()
                && c.width > 0
                && c.height > 0
                && (c.width as i64) * (c.height as i64) <= 256 * 256
            {
                let cw = c.width as i32;
                let ch = c.height as i32;
                let n = (cw * ch) as usize;
                let src = std::slice::from_raw_parts(c.pixels, n);
                let mut hash: u64 = 1469598103934665603;
                let mut colors = Vec::with_capacity(n * 4);
                let (mut minx, mut miny, mut maxx, mut maxy) = (cw, ch, -1i32, -1i32);
                for (i, &p) in src.iter().enumerate() {
                    let a = ((p >> 24) & 0xff) as u8;
                    let r = ((p >> 16) & 0xff) as u8;
                    let g = ((p >> 8) & 0xff) as u8;
                    let b = (p & 0xff) as u8;
                    colors.push(r);
                    colors.push(g);
                    colors.push(b);
                    colors.push(a);
                    hash ^= p as u64;
                    hash = hash.wrapping_mul(1099511628211);
                    if a >= 128 {
                        let x = (i as i32) % cw;
                        let y = (i as i32) / cw;
                        if x < minx { minx = x; }
                        if x > maxx { maxx = x; }
                        if y < miny { miny = y; }
                        if y > maxy { maxy = y; }
                    }
                }
                let (hotx, hoty) = if c.hot_x != 0 || c.hot_y != 0 {
                    (c.hot_x, c.hot_y)
                } else if maxx >= minx && maxy >= miny {
                    let (bw, bh) = (maxx - minx + 1, maxy - miny + 1);
                    if bh > bw * 2 {
                        ((minx + maxx) / 2, (miny + maxy) / 2)
                    } else {
                        (minx, miny)
                    }
                } else {
                    (0, 0)
                };
                let mut id = hash;
                for v in [cw as u32 as u64, ch as u32 as u64, hotx as u32 as u64, hoty as u32 as u64] {
                    id ^= v;
                    id = id.wrapping_mul(1099511628211);
                }
                Some(CursorSnapshot {
                    id,
                    width: cw as u32,
                    height: ch as u32,
                    hotx,
                    hoty,
                    colors,
                })
            } else {
                None
            };
            (self.lib.cursor_release)(self.ctx, &mut c);
            out
        }
    }

    pub fn displays(&mut self) -> Vec<DisplaySnapshot> {
        unsafe {
            let mut raw = vec![std::mem::zeroed::<drmtap_display>(); 16];
            let cap = raw.len() as i32;
            let n = (self.lib.list_displays)(self.ctx, raw.as_mut_ptr(), cap);
            if n <= 0 {
                return Vec::new();
            }
            let count = (n as usize).min(raw.len());
            (0..count)
                .map(|i| {
                    let name_bytes: Vec<u8> = raw[i]
                        .name
                        .iter()
                        .take_while(|&&ch| ch != 0)
                        .map(|&ch| ch as u8)
                        .collect();
                    DisplaySnapshot {
                        name: String::from_utf8_lossy(&name_bytes).to_string(),
                        crtc_id: raw[i].crtc_id,
                        x: raw[i].x as i32,
                        y: raw[i].y as i32,
                        width: raw[i].width,
                        height: raw[i].height,
                        active: raw[i].active != 0,
                    }
                })
                .collect()
        }
    }
}

impl Drop for DrmReader {
    fn drop(&mut self) {
        if !self.ctx.is_null() {
            unsafe { (self.lib.close)(self.ctx) };
            self.ctx = std::ptr::null_mut();
        }
    }
}
