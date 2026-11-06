use hbb_common::log;
use lazy_static::lazy_static;
use std::sync::Mutex;
use std::time::{Duration, Instant};

lazy_static! {
    static ref VIDEO_RAW: Mutex<FrameRaw> = Mutex::new(FrameRaw::new("video", MAX_VIDEO_FRAME_TIMEOUT));
    static ref SCREEN_SIZE: Mutex<(u16, u16, u16)> = Mutex::new((0, 0, 0));
}

const MAX_VIDEO_FRAME_TIMEOUT: Duration = Duration::from_millis(500);

static FRAME_ROTATION: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

// Diagnostic state — written by Rust, read by Swift via ios_get_diagnostic_state()
static DIAG_FRAMES_RECEIVED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static DIAG_FRAMES_ENABLED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static DIAG_SCREEN_W: std::sync::atomic::AtomicU16 = std::sync::atomic::AtomicU16::new(0);
static DIAG_SCREEN_H: std::sync::atomic::AtomicU16 = std::sync::atomic::AtomicU16::new(0);
// Deeper pipeline diagnostics
static DIAG_TAKE_CALLS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static DIAG_TAKE_SUCCESS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static DIAG_TAKE_EMPTY: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static DIAG_TAKE_TIMEOUT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static DIAG_TAKE_EQUAL: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static DIAG_VIDEO_SVC_STARTED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static DIAG_VIDEO_SVC_ERROR: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
lazy_static! {
    // Stores last error message from video_service (up to 200 chars)
    static ref DIAG_LAST_ERROR: Mutex<Option<String>> = Mutex::new(None);
}

/// Returns a diagnostic string for the current Rust video pipeline state.
/// Called from Swift to get debugging info without relying on callback logging.
/// Caller must free the returned pointer with `ios_free_diagnostic_string`.
#[no_mangle]
pub extern "C" fn ios_get_diagnostic_state() -> *mut std::os::raw::c_char {
    let frames = DIAG_FRAMES_RECEIVED.load(std::sync::atomic::Ordering::Relaxed);
    let enabled = DIAG_FRAMES_ENABLED.load(std::sync::atomic::Ordering::Relaxed);
    let w = DIAG_SCREEN_W.load(std::sync::atomic::Ordering::Relaxed);
    let h = DIAG_SCREEN_H.load(std::sync::atomic::Ordering::Relaxed);
    let screen = get_screen_size();
    let raw_enabled = VIDEO_RAW.lock().map(|r| r.enable).unwrap_or(false);
    let raw_data_len = VIDEO_RAW.lock().map(|r| r.data.len()).unwrap_or(0);

    let take_calls = DIAG_TAKE_CALLS.load(std::sync::atomic::Ordering::Relaxed);
    let take_success = DIAG_TAKE_SUCCESS.load(std::sync::atomic::Ordering::Relaxed);
    let take_empty = DIAG_TAKE_EMPTY.load(std::sync::atomic::Ordering::Relaxed);
    let take_timeout = DIAG_TAKE_TIMEOUT.load(std::sync::atomic::Ordering::Relaxed);
    let take_equal = DIAG_TAKE_EQUAL.load(std::sync::atomic::Ordering::Relaxed);
    let svc_started = DIAG_VIDEO_SVC_STARTED.load(std::sync::atomic::Ordering::Relaxed);
    let svc_error = DIAG_VIDEO_SVC_ERROR.load(std::sync::atomic::Ordering::Relaxed);
    let last_err = DIAG_LAST_ERROR.lock().map(|e| e.clone()).unwrap_or(None);
    let last_err_str = last_err.unwrap_or_default().replace('\0', "");

    let msg = format!(
        "recv={}, en={}, scr={}x{}, rot={}, raw_en={}, raw_len={}, take={}/{}/{}/{}/{}, svc={}/{}, err={}",
        frames, enabled, screen.0, screen.1,
        FRAME_ROTATION.load(std::sync::atomic::Ordering::Relaxed),
        raw_enabled, raw_data_len,
        take_calls, take_success, take_empty, take_timeout, take_equal,
        svc_started, svc_error, last_err_str
    );
    // Replace any null bytes in the message to ensure CString::new succeeds
    let msg = msg.replace('\0', "");
    match std::ffi::CString::new(msg) {
        Ok(c) => c.into_raw(),
        Err(_) => std::ptr::null_mut(),
    }
}

/// Free a string returned by `ios_get_diagnostic_state`.
#[no_mangle]
pub unsafe extern "C" fn ios_free_diagnostic_string(ptr: *mut std::os::raw::c_char) {
    if !ptr.is_null() {
        drop(std::ffi::CString::from_raw(ptr));
    }
}

static MANAGED_ENROLLMENT_TOKEN: std::sync::RwLock<Option<String>> = std::sync::RwLock::new(None);

#[no_mangle]
pub unsafe extern "C" fn ios_set_managed_enrollment_token(token: *const std::os::raw::c_char) {
    if token.is_null() {
        return;
    }
    let token = std::ffi::CStr::from_ptr(token).to_string_lossy().trim().to_string();
    if !token.is_empty() {
        *MANAGED_ENROLLMENT_TOKEN.write().unwrap() = Some(token);
    }
}

pub fn get_managed_enrollment_token() -> Option<String> {
    MANAGED_ENROLLMENT_TOKEN.read().unwrap().clone()
}

static MANAGED_API_URL: std::sync::RwLock<Option<String>> = std::sync::RwLock::new(None);

#[no_mangle]
pub unsafe extern "C" fn ios_set_managed_api_url(url: *const std::os::raw::c_char) {
    if url.is_null() {
        return;
    }
    let url = std::ffi::CStr::from_ptr(url).to_string_lossy().trim().to_string();
    if !url.is_empty() {
        *MANAGED_API_URL.write().unwrap() = Some(url);
    }
}

pub fn get_managed_api_url() -> Option<String> {
    MANAGED_API_URL.read().unwrap().clone()
}

/// Stub — kept for ABI compat with Swift call in AppDelegate.
#[no_mangle]
pub extern "C" fn ios_set_log_callback(_cb: Option<unsafe extern "C" fn(*const std::os::raw::c_char)>) {
    // Callback approach had encoding issues. Using ios_get_diagnostic_state() instead.
}

/// nslog is now a no-op since the callback had encoding issues.
/// Diagnostic data is exposed via ios_get_diagnostic_state() which Swift polls.
pub fn nslog(_msg: &str) {
    // no-op — Swift polls ios_get_diagnostic_state() instead
}

struct FrameRaw {
    name: &'static str,
    data: Vec<u8>,
    last_update: Instant,
    timeout: Duration,
    enable: bool,
}

impl FrameRaw {
    fn new(name: &'static str, timeout: Duration) -> Self {
        FrameRaw {
            name,
            data: Vec::new(),
            last_update: Instant::now(),
            timeout,
            enable: false,
        }
    }

    fn set_enable(&mut self, value: bool) {
        self.enable = value;
        self.data.clear();
    }

    fn update(&mut self, data: &[u8], src_w: usize, src_h: usize, rotation: u32) {
        if !self.enable || data.is_empty() {
            return;
        }
        let src_stride = if src_h > 0 { data.len() / src_h } else { 0 };
        if rotation != 0 && src_w > 0 && src_h > 0 && src_stride >= src_w * 4 {
            self.data.resize(src_w * src_h * 4, 0);
            rotate_bgra(data, src_w, src_h, src_stride, rotation, &mut self.data);
        } else {
            self.data.resize(data.len(), 0);
            self.data.copy_from_slice(data);
        }
        self.last_update = Instant::now();
    }

    fn take(&mut self, dst: &mut Vec<u8>, last: &mut Vec<u8>) -> Option<()> {
        DIAG_TAKE_CALLS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);

        if !self.enable || self.data.is_empty() {
            DIAG_TAKE_EMPTY.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            return None;
        }

        if self.last_update.elapsed() > self.timeout {
            log::trace!("Failed to take {} raw, timeout!", self.name);
            DIAG_TAKE_TIMEOUT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            self.data.clear();
            return None;
        }

        if last.len() == self.data.len()
            && crate::would_block_if_equal(last, &self.data).is_err()
        {
            DIAG_TAKE_EQUAL.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            self.data.clear();
            return None;
        }

        std::mem::swap(dst, &mut self.data);
        self.data.clear();
        DIAG_TAKE_SUCCESS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);

        Some(())
    }
}

fn rotate_bgra(src: &[u8], w: usize, h: usize, src_stride: usize, rotation: u32, dst: &mut [u8]) {
    let dst_w = if rotation == 180 { w } else { h };
    for y in 0..h {
        let row = &src[y * src_stride..y * src_stride + w * 4];
        for (x, px) in row.chunks_exact(4).enumerate() {
            let (dx, dy) = match rotation {
                90 => (h - 1 - y, x),
                180 => (w - 1 - x, h - 1 - y),
                _ => (y, w - 1 - x),
            };
            let i = (dy * dst_w + dx) * 4;
            dst[i..i + 4].copy_from_slice(px);
        }
    }
}

pub fn get_video_raw(dst: &mut Vec<u8>, last: &mut Vec<u8>) -> Option<()> {
    VIDEO_RAW.lock().ok()?.take(dst, last)
}

/// Called by video_service to report it has started running
pub fn diag_video_svc_started() {
    DIAG_VIDEO_SVC_STARTED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
}

/// Called by video_service to report an error
pub fn diag_video_svc_error(err: &str) {
    DIAG_VIDEO_SVC_ERROR.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    if let Ok(mut last) = DIAG_LAST_ERROR.lock() {
        let truncated: String = err.chars().take(200).collect();
        *last = Some(truncated);
    }
}

pub fn set_frame_raw_enable(name: &str, value: bool) {
    DIAG_FRAMES_ENABLED.store(value, std::sync::atomic::Ordering::Relaxed);
    if name == "video" {
        if let Ok(mut raw) = VIDEO_RAW.lock() {
            raw.set_enable(value);
        }
    }
}

pub fn set_screen_size(w: u16, h: u16, scale: u16) {
    DIAG_SCREEN_W.store(w, std::sync::atomic::Ordering::Relaxed);
    DIAG_SCREEN_H.store(h, std::sync::atomic::Ordering::Relaxed);
    let mut size = SCREEN_SIZE.lock().unwrap();
    *size = (w, h, scale);
}

pub fn get_screen_size() -> (u16, u16, u16) {
    let (w, h, scale) = *SCREEN_SIZE.lock().unwrap();
    if is_quarter_turn() {
        (h, w, scale)
    } else {
        (w, h, scale)
    }
}

fn is_quarter_turn() -> bool {
    let rotation = FRAME_ROTATION.load(std::sync::atomic::Ordering::Relaxed);
    rotation == 90 || rotation == 270
}

/// Called from Swift/ObjC to push a video frame into the Rust pipeline.
/// This is the iOS equivalent of Android's `Java_ffi_FFI_onVideoFrameUpdate`.
///
/// # Safety
/// `data` must point to a valid buffer of at least `len` bytes.
static FRAME_LOG_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

#[no_mangle]
pub unsafe extern "C" fn ios_on_video_frame_update(data: *const u8, len: usize) {
    if data.is_null() || len == 0 {
        return;
    }
    let count = FRAME_LOG_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    DIAG_FRAMES_RECEIVED.store(count + 1, std::sync::atomic::Ordering::Relaxed);
    let slice = std::slice::from_raw_parts(data, len);
    let (src_w, src_h, _) = *SCREEN_SIZE.lock().unwrap();
    let rotation = FRAME_ROTATION.load(std::sync::atomic::Ordering::Relaxed);
    if let Ok(mut raw) = VIDEO_RAW.lock() {
        raw.update(slice, src_w as usize, src_h as usize, rotation);
    }
}

#[no_mangle]
pub extern "C" fn ios_set_frame_rotation(degrees: u32) {
    FRAME_ROTATION.store(degrees, std::sync::atomic::Ordering::Relaxed);
}

/// Called from Swift to set the screen dimensions.
#[no_mangle]
pub extern "C" fn ios_set_screen_size(w: u16, h: u16, scale: u16) {
    set_screen_size(w, h, scale);
}

/// Called from Swift to enable/disable frame capture.
/// `name` must be a null-terminated C string ("video").
///
/// # Safety
/// `name` must be a valid null-terminated C string.
#[no_mangle]
pub unsafe extern "C" fn ios_set_frame_raw_enable(name: *const std::os::raw::c_char, value: bool) {
    if name.is_null() {
        return;
    }
    let name = std::ffi::CStr::from_ptr(name).to_str().unwrap_or("");
    set_frame_raw_enable(name, value);
}
