use std::cell::RefCell;
use std::collections::HashMap;
use vello::peniko;

thread_local! {
    static ICON_CACHE: RefCell<HashMap<(String, u32), Option<peniko::Image>>> =
        RefCell::new(HashMap::new());
}

pub fn icon_image(filename: &str, is_dir: bool, target_px: u32) -> Option<peniko::Image> {
    if target_px == 0 {
        return None;
    }
    let key = (cache_key(filename, is_dir), target_px);
    if let Some(hit) = ICON_CACHE.with(|c| c.borrow().get(&key).cloned()) {
        return hit;
    }
    let img = fetch_os_icon(filename, is_dir, target_px).map(|(w, h, rgba)| {
        let (w, h, rgba) = resize_to_target(w, h, rgba, target_px);
        peniko::Image::new(
            peniko::Blob::new(std::sync::Arc::new(rgba)),
            peniko::Format::Rgba8,
            w,
            h,
        )
    });
    ICON_CACHE.with(|c| {
        let mut c = c.borrow_mut();
        if c.len() >= 512 {
            c.clear();
        }
        c.insert(key, img.clone());
    });
    img
}

fn file_extension(filename: &str) -> String {
    let name = filename
        .rsplit(|c| c == '/' || c == '\\')
        .next()
        .unwrap_or(filename);
    match name.rfind('.') {
        Some(i) if i > 0 => name[i + 1..].to_ascii_lowercase(),
        _ => String::new(),
    }
}

#[cfg(windows)]
fn cache_key(filename: &str, is_dir: bool) -> String {
    if is_dir {
        return format!("d:{}", filename);
    }
    let ext = file_extension(filename);
    match ext.as_str() {
        "exe" | "lnk" | "ico" | "url" | "scr" | "msi" | "cur" | "ani" | "appref-ms" => {
            format!("p:{}", filename)
        }
        _ => format!("e:{}", ext),
    }
}

#[cfg(not(windows))]
fn cache_key(filename: &str, is_dir: bool) -> String {
    if is_dir {
        format!("d:{}", filename)
    } else {
        format!("e:{}", file_extension(filename))
    }
}

fn resize_to_target(w: u32, h: u32, rgba: Vec<u8>, target: u32) -> (u32, u32, Vec<u8>) {
    if (w == target && h == target) || w == 0 || h == 0 {
        return (w, h, rgba);
    }
    match image::RgbaImage::from_raw(w, h, rgba) {
        Some(src) => {
            let resized =
                image::imageops::resize(&src, target, target, image::imageops::FilterType::Lanczos3);
            (target, target, resized.into_raw())
        }
        None => (0, 0, Vec::new()),
    }
}

#[cfg(windows)]
fn fetch_os_icon(filename: &str, is_dir: bool, target_px: u32) -> Option<(u32, u32, Vec<u8>)> {
    use std::os::windows::ffi::OsStrExt;
    use winapi::um::shellapi::{
        SHGetFileInfoW, SHFILEINFOW, SHGFI_ICON, SHGFI_LARGEICON, SHGFI_SMALLICON,
        SHGFI_USEFILEATTRIBUTES,
    };
    use winapi::um::winnt::{FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_NORMAL};
    use winapi::um::winuser::DestroyIcon;

    let wide: Vec<u16> = std::ffi::OsStr::new(filename)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let size_flag = if target_px <= 20 {
        SHGFI_SMALLICON
    } else {
        SHGFI_LARGEICON
    };
    unsafe {
        let cb = std::mem::size_of::<SHFILEINFOW>() as u32;
        let mut sfi: SHFILEINFOW = std::mem::zeroed();
        let has_path = filename.contains(|c| c == '/' || c == '\\');
        let mut got = if has_path {
            SHGetFileInfoW(wide.as_ptr(), 0, &mut sfi, cb, SHGFI_ICON | size_flag)
        } else {
            0
        };
        if got == 0 || sfi.hIcon.is_null() {
            let attrs = if is_dir {
                FILE_ATTRIBUTE_DIRECTORY
            } else {
                FILE_ATTRIBUTE_NORMAL
            };
            sfi = std::mem::zeroed();
            got = SHGetFileInfoW(
                wide.as_ptr(),
                attrs,
                &mut sfi,
                cb,
                SHGFI_ICON | size_flag | SHGFI_USEFILEATTRIBUTES,
            );
        }
        if got == 0 || sfi.hIcon.is_null() {
            return None;
        }
        let rgba = hicon_to_rgba(sfi.hIcon);
        DestroyIcon(sfi.hIcon);
        rgba
    }
}

#[cfg(windows)]
unsafe fn hicon_to_rgba(
    hicon: winapi::shared::windef::HICON,
) -> Option<(u32, u32, Vec<u8>)> {
    use winapi::um::wingdi::{
        DeleteObject, GetDIBits, GetObjectW, BITMAP, BITMAPINFO, BITMAPINFOHEADER, BI_RGB,
        DIB_RGB_COLORS,
    };
    use winapi::um::winuser::{GetDC, GetIconInfo, ReleaseDC, ICONINFO};

    let mut info: ICONINFO = std::mem::zeroed();
    if GetIconInfo(hicon, &mut info) == 0 {
        return None;
    }
    let color = info.hbmColor;
    let mask = info.hbmMask;
    let mut out = None;
    if !color.is_null() {
        let mut bm: BITMAP = std::mem::zeroed();
        if GetObjectW(
            color as _,
            std::mem::size_of::<BITMAP>() as i32,
            &mut bm as *mut _ as _,
        ) != 0
            && bm.bmWidth > 0
            && bm.bmHeight > 0
            && bm.bmWidth <= 512
            && bm.bmHeight <= 512
        {
            let (w, h) = (bm.bmWidth, bm.bmHeight);
            let hdc = GetDC(std::ptr::null_mut());
            if !hdc.is_null() {
                let mut bi: BITMAPINFO = std::mem::zeroed();
                bi.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
                bi.bmiHeader.biWidth = w;
                bi.bmiHeader.biHeight = -h;
                bi.bmiHeader.biPlanes = 1;
                bi.bmiHeader.biBitCount = 32;
                bi.bmiHeader.biCompression = BI_RGB;
                let len = (w as usize) * (h as usize) * 4;
                let mut px = vec![0u8; len];
                if GetDIBits(
                    hdc,
                    color,
                    0,
                    h as u32,
                    px.as_mut_ptr() as _,
                    &mut bi,
                    DIB_RGB_COLORS,
                ) != 0
                {
                    if px.chunks_exact(4).all(|p| p[3] == 0) && !mask.is_null() {
                        let mut mi: BITMAPINFO = std::mem::zeroed();
                        mi.bmiHeader = bi.bmiHeader;
                        let mut mpx = vec![0u8; len];
                        if GetDIBits(
                            hdc,
                            mask,
                            0,
                            h as u32,
                            mpx.as_mut_ptr() as _,
                            &mut mi,
                            DIB_RGB_COLORS,
                        ) != 0
                        {
                            for (p, m) in px.chunks_exact_mut(4).zip(mpx.chunks_exact(4)) {
                                p[3] = if m[0] == 0 { 255 } else { 0 };
                            }
                        }
                    }
                    for p in px.chunks_exact_mut(4) {
                        p.swap(0, 2);
                    }
                    out = Some((w as u32, h as u32, px));
                }
                ReleaseDC(std::ptr::null_mut(), hdc);
            }
        }
    }
    if !color.is_null() {
        DeleteObject(color as _);
    }
    if !mask.is_null() {
        DeleteObject(mask as _);
    }
    out
}

#[cfg(target_os = "macos")]
fn fetch_os_icon(filename: &str, is_dir: bool, target_px: u32) -> Option<(u32, u32, Vec<u8>)> {
    use objc::runtime::{Object, NO, YES};
    use objc::{class, msg_send, sel, sel_impl};

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct NSPoint {
        x: f64,
        y: f64,
    }
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct NSRect {
        origin: NSPoint,
        size: NSPoint,
    }

    let has_path = filename.contains('/') && std::path::Path::new(filename).exists();
    let lookup = if has_path {
        filename.to_string()
    } else if is_dir {
        "public.folder".to_string()
    } else {
        match file_extension(filename) {
            ext if ext.is_empty() => "public.data".to_string(),
            ext => ext,
        }
    };
    let c_lookup = std::ffi::CString::new(lookup).ok()?;
    objc::rc::autoreleasepool(|| unsafe {
        let ns_lookup: *mut Object =
            msg_send![class!(NSString), stringWithUTF8String: c_lookup.as_ptr()];
        if ns_lookup.is_null() {
            return None;
        }
        let workspace: *mut Object = msg_send![class!(NSWorkspace), sharedWorkspace];
        let icon: *mut Object = if has_path {
            msg_send![workspace, iconForFile: ns_lookup]
        } else {
            msg_send![workspace, iconForFileType: ns_lookup]
        };
        if icon.is_null() {
            return None;
        }
        let side = target_px as isize;
        let color_space: *mut Object = msg_send![
            class!(NSString),
            stringWithUTF8String: b"NSCalibratedRGBColorSpace\0".as_ptr()
        ];
        let rep: *mut Object = msg_send![class!(NSBitmapImageRep), alloc];
        let rep: *mut Object = msg_send![rep,
            initWithBitmapDataPlanes: std::ptr::null_mut::<*mut u8>()
            pixelsWide: side
            pixelsHigh: side
            bitsPerSample: 8isize
            samplesPerPixel: 4isize
            hasAlpha: YES
            isPlanar: NO
            colorSpaceName: color_space
            bytesPerRow: (target_px * 4) as isize
            bitsPerPixel: 32isize];
        if rep.is_null() {
            return None;
        }
        let ctx: *mut Object =
            msg_send![class!(NSGraphicsContext), graphicsContextWithBitmapImageRep: rep];
        if ctx.is_null() {
            let _: () = msg_send![rep, release];
            return None;
        }
        let _: () = msg_send![class!(NSGraphicsContext), saveGraphicsState];
        let _: () = msg_send![class!(NSGraphicsContext), setCurrentContext: ctx];
        let dest = NSRect {
            origin: NSPoint { x: 0.0, y: 0.0 },
            size: NSPoint {
                x: target_px as f64,
                y: target_px as f64,
            },
        };
        let zero = NSRect {
            origin: NSPoint { x: 0.0, y: 0.0 },
            size: NSPoint { x: 0.0, y: 0.0 },
        };
        let nil: *mut Object = std::ptr::null_mut();
        let _: () = msg_send![icon,
            drawInRect: dest
            fromRect: zero
            operation: 2usize
            fraction: 1.0f64
            respectFlipped: YES
            hints: nil];
        let _: () = msg_send![class!(NSGraphicsContext), restoreGraphicsState];
        let data: *mut u8 = msg_send![rep, bitmapData];
        if data.is_null() {
            let _: () = msg_send![rep, release];
            return None;
        }
        let len = (target_px as usize) * (target_px as usize) * 4;
        let mut px = std::slice::from_raw_parts(data, len).to_vec();
        let _: () = msg_send![rep, release];
        for p in px.chunks_exact_mut(4) {
            let a = p[3] as u32;
            if a > 0 && a < 255 {
                for c in p[..3].iter_mut() {
                    *c = ((*c as u32 * 255) / a).min(255) as u8;
                }
            }
        }
        Some((target_px, target_px, px))
    })
}

#[cfg(not(any(windows, target_os = "macos")))]
fn fetch_os_icon(_filename: &str, _is_dir: bool, _target_px: u32) -> Option<(u32, u32, Vec<u8>)> {
    None
}
