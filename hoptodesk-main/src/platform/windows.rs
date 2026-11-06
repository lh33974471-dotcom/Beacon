use super::{CursorData, ResultType};
use crate::{
    ipc,
    privacy_mode::win_topmost_window::{self},
};
use hbb_common::{
    allow_err,
    anyhow::anyhow,
    bail,
    config::{self, Config},
    libc::{c_int, wchar_t},
    log,
    message_proto::{DisplayInfo, Resolution, WindowsSession},
    sleep, timeout, tokio,
};
use std::{
    collections::HashMap,
    ffi::{CString, OsStr, OsString},
    fs, io,
    io::prelude::*,
    mem,
    os::windows::process::CommandExt,
    path::*,
    ptr::null_mut,
    sync::{atomic::Ordering, Arc, Mutex, OnceLock},
    time::{Duration, Instant},
};
use wallpaper;
use winapi::um::sysinfoapi::{GetNativeSystemInfo, SYSTEM_INFO};
use winapi::{
    ctypes::c_void,
    shared::{minwindef::*, ntdef::NULL, windef::*},
    um::{
        tlhelp32::{CreateToolhelp32Snapshot, Process32First, Process32Next, PROCESSENTRY32, TH32CS_SNAPPROCESS},
        handleapi::{CloseHandle, GetHandleInformation, INVALID_HANDLE_VALUE},
        libloaderapi::{
            GetProcAddress, LoadLibraryA, LoadLibraryExA, LOAD_LIBRARY_SEARCH_SYSTEM32,
        },        
        minwinbase::STILL_ACTIVE,
        processthreadsapi::{
            GetCurrentProcess,  GetCurrentProcessId, GetCurrentThreadId, GetExitCodeProcess, OpenProcess,
            OpenProcessToken, ProcessIdToSessionId, PROCESS_INFORMATION, STARTUPINFOW,
        },
        securitybaseapi::GetTokenInformation,
        shellapi::{ShellExecuteExW, ShellExecuteW, SEE_MASK_NOASYNC, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW},
        synchapi::WaitForSingleObject,
        winbase::*,
        wingdi::*,
        winnt::{
            TokenElevation, ES_AWAYMODE_REQUIRED, ES_CONTINUOUS, ES_DISPLAY_REQUIRED,
            ES_SYSTEM_REQUIRED, HANDLE, PROCESS_ALL_ACCESS, PROCESS_QUERY_LIMITED_INFORMATION, TOKEN_ELEVATION,
            TOKEN_QUERY,
        },
        winuser::*,
    },
};
use windows_service::{
    define_windows_service,
    service::{
        ServiceAccess, ServiceControl, ServiceControlAccept, ServiceErrorControl, ServiceExitCode,
        ServiceInfo, ServiceStartType, ServiceState, ServiceStatus, ServiceType,
    },
    service_control_handler::{self, ServiceControlHandlerResult},
    service_manager::{ServiceManager, ServiceManagerAccess},
};
use winreg::{enums::*, RegKey};
use std::env;
#[cfg(feature = "standalone")]
use crate::ui::{get_dllpm_bytes, get_dllph_bytes};

pub const FLUTTER_RUNNER_WIN32_WINDOW_CLASS: &'static str = "FLUTTER_RUNNER_WIN32_WINDOW"; // main window, install window
pub const EXPLORER_EXE: &'static str = "explorer.exe";
pub const ELEVATED_CHILD_ARG: &str = "--elevated-child";
pub const SET_FOREGROUND_WINDOW: &'static str = "SET_FOREGROUND_WINDOW";

const REG_NAME_INSTALL_DESKTOPSHORTCUTS: &str = "DESKTOPSHORTCUTS";
const REG_NAME_INSTALL_STARTMENUSHORTCUTS: &str = "STARTMENUSHORTCUTS";

pub fn get_focused_display(displays: Vec<DisplayInfo>) -> Option<usize> {
    unsafe {
        let hwnd = GetForegroundWindow();
        let mut rect: RECT = mem::zeroed();
        if GetWindowRect(hwnd, &mut rect as *mut RECT) == 0 {
            return None;
        }
        displays.iter().position(|display| {
            let center_x = rect.left + (rect.right - rect.left) / 2;
            let center_y = rect.top + (rect.bottom - rect.top) / 2;
            center_x >= display.x
                && center_x <= display.x + display.width
                && center_y >= display.y
                && center_y <= display.y + display.height
        })
    }
}

pub fn get_cursor_pos() -> Option<(i32, i32)> {
    unsafe {
        #[allow(invalid_value)]
        let mut out = mem::MaybeUninit::uninit().assume_init();
        if GetCursorPos(&mut out) == FALSE {
            return None;
        }
        return Some((out.x, out.y));
    }
}

pub fn reset_input_cache() {}

pub fn get_cursor() -> ResultType<Option<u64>> {
    unsafe {
        #[allow(invalid_value)]
        let mut ci: CURSORINFO = mem::MaybeUninit::uninit().assume_init();
        ci.cbSize = std::mem::size_of::<CURSORINFO>() as _;
        if crate::portable_service::client::get_cursor_info(&mut ci) == FALSE {
            return Err(io::Error::last_os_error().into());
        }
        if ci.flags & CURSOR_SHOWING == 0 {
            Ok(None)
        } else {
            Ok(Some(ci.hCursor as _))
        }
    }
}

struct IconInfo(ICONINFO);

impl IconInfo {
    fn new(icon: HICON) -> ResultType<Self> {
        unsafe {
            #[allow(invalid_value)]
            let mut ii = mem::MaybeUninit::uninit().assume_init();
            if GetIconInfo(icon, &mut ii) == FALSE {
                Err(io::Error::last_os_error().into())
            } else {
                let ii = Self(ii);
                if ii.0.hbmMask.is_null() {
                    bail!("Cursor bitmap handle is NULL");
                }
                return Ok(ii);
            }
        }
    }

    fn is_color(&self) -> bool {
        !self.0.hbmColor.is_null()
    }
}

impl Drop for IconInfo {
    fn drop(&mut self) {
        unsafe {
            if !self.0.hbmColor.is_null() {
                DeleteObject(self.0.hbmColor as _);
            }
            if !self.0.hbmMask.is_null() {
                DeleteObject(self.0.hbmMask as _);
            }
        }
    }
}

// https://github.com/TurboVNC/tightvnc/blob/a235bae328c12fd1c3aed6f3f034a37a6ffbbd22/vnc_winsrc/winvnc/vncEncoder.cpp
// https://github.com/TigerVNC/tigervnc/blob/master/win/rfb_win32/DeviceFrameBuffer.cxx
pub fn get_cursor_data(hcursor: u64) -> ResultType<CursorData> {
    unsafe {
        let mut ii = IconInfo::new(hcursor as _)?;
        let bm_mask = get_bitmap(ii.0.hbmMask)?;
        let mut width = bm_mask.bmWidth;
        let mut height = if ii.is_color() {
            bm_mask.bmHeight
        } else {
            bm_mask.bmHeight / 2
        };
        let cbits_size = width * height * 4;
        if cbits_size < 16 {
            bail!("Invalid icon: too small"); // solve some crash
        }
        let mut cbits: Vec<u8> = Vec::new();
        cbits.resize(cbits_size as _, 0);
        let mut mbits: Vec<u8> = Vec::new();
        mbits.resize((bm_mask.bmWidthBytes * bm_mask.bmHeight) as _, 0);
        let r = GetBitmapBits(ii.0.hbmMask, mbits.len() as _, mbits.as_mut_ptr() as _);
        if r == 0 {
            bail!("Failed to copy bitmap data");
        }
        if r != (mbits.len() as i32) {
            bail!(
                "Invalid mask cursor buffer size, got {} bytes, expected {}",
                r,
                mbits.len()
            );
        }
        let do_outline;
        if ii.is_color() {
            get_rich_cursor_data(ii.0.hbmColor, width, height, &mut cbits)?;
            do_outline = fix_cursor_mask(
                &mut mbits,
                &mut cbits,
                width as _,
                height as _,
                bm_mask.bmWidthBytes as _,
            );
        } else {
            do_outline = handleMask(
                cbits.as_mut_ptr(),
                mbits.as_ptr(),
                width,
                height,
                bm_mask.bmWidthBytes,
                bm_mask.bmHeight,
            ) > 0;
        }
        if do_outline {
            let mut outline = Vec::new();
            outline.resize(((width + 2) * (height + 2) * 4) as _, 0);
            drawOutline(
                outline.as_mut_ptr(),
                cbits.as_ptr(),
                width,
                height,
                outline.len() as _,
            );
            cbits = outline;
            width += 2;
            height += 2;
            ii.0.xHotspot += 1;
            ii.0.yHotspot += 1;
        }

        Ok(CursorData {
            id: hcursor,
            colors: cbits.into(),
            hotx: ii.0.xHotspot as _,
            hoty: ii.0.yHotspot as _,
            width: width as _,
            height: height as _,
            ..Default::default()
        })
    }
}

#[inline]
fn get_bitmap(handle: HBITMAP) -> ResultType<BITMAP> {
    unsafe {
        let mut bm: BITMAP = mem::zeroed();
        if GetObjectA(
            handle as _,
            std::mem::size_of::<BITMAP>() as _,
            &mut bm as *mut BITMAP as *mut _,
        ) == FALSE
        {
            return Err(io::Error::last_os_error().into());
        }
        if bm.bmPlanes != 1 {
            bail!("Unsupported multi-plane cursor");
        }
        if bm.bmBitsPixel != 1 {
            bail!("Unsupported cursor mask format");
        }
        Ok(bm)
    }
}

struct DC(HDC);

impl DC {
    fn new() -> ResultType<Self> {
        unsafe {
            let dc = GetDC(0 as _);
            if dc.is_null() {
                bail!("Failed to get a drawing context");
            }
            Ok(Self(dc))
        }
    }
}

impl Drop for DC {
    fn drop(&mut self) {
        unsafe {
            if !self.0.is_null() {
                ReleaseDC(0 as _, self.0);
            }
        }
    }
}

struct CompatibleDC(HDC);

impl CompatibleDC {
    fn new(existing: HDC) -> ResultType<Self> {
        unsafe {
            let dc = CreateCompatibleDC(existing);
            if dc.is_null() {
                bail!("Failed to get a compatible drawing context");
            }
            Ok(Self(dc))
        }
    }
}

impl Drop for CompatibleDC {
    fn drop(&mut self) {
        unsafe {
            if !self.0.is_null() {
                DeleteDC(self.0);
            }
        }
    }
}

struct BitmapDC(CompatibleDC, HBITMAP);

impl BitmapDC {
    fn new(hdc: HDC, hbitmap: HBITMAP) -> ResultType<Self> {
        unsafe {
            let dc = CompatibleDC::new(hdc)?;
            let oldbitmap = SelectObject(dc.0, hbitmap as _) as HBITMAP;
            if oldbitmap.is_null() {
                bail!("Failed to select CompatibleDC");
            }
            Ok(Self(dc, oldbitmap))
        }
    }

    fn dc(&self) -> HDC {
        (self.0).0
    }
}

impl Drop for BitmapDC {
    fn drop(&mut self) {
        unsafe {
            if !self.1.is_null() {
                SelectObject((self.0).0, self.1 as _);
            }
        }
    }
}

#[inline]
fn get_rich_cursor_data(
    hbm_color: HBITMAP,
    width: i32,
    height: i32,
    out: &mut Vec<u8>,
) -> ResultType<()> {
    unsafe {
        let dc = DC::new()?;
        let bitmap_dc = BitmapDC::new(dc.0, hbm_color)?;
        if get_di_bits(out.as_mut_ptr(), bitmap_dc.dc(), hbm_color, width, height) > 0 {
            bail!("Failed to get di bits: {}", io::Error::last_os_error());
        }
    }
    Ok(())
}

fn fix_cursor_mask(
    mbits: &mut Vec<u8>,
    cbits: &mut Vec<u8>,
    width: usize,
    height: usize,
    bm_width_bytes: usize,
) -> bool {
    let mut pix_idx = 0;
    for _ in 0..height {
        for _ in 0..width {
            if cbits[pix_idx + 3] != 0 {
                return false;
            }
            pix_idx += 4;
        }
    }

    let packed_width_bytes = (width + 7) >> 3;
    let bm_size = mbits.len();
    let c_size = cbits.len();

    // Pack and invert bitmap data (mbits)
    // borrow from tigervnc
    for y in 0..height {
        for x in 0..packed_width_bytes {
            let a = y * packed_width_bytes + x;
            let b = y * bm_width_bytes + x;
            if a < bm_size && b < bm_size {
                mbits[a] = !mbits[b];
            }
        }
    }

    // Replace "inverted background" bits with black color to ensure
    // cross-platform interoperability. Not beautiful but necessary code.
    // borrow from tigervnc
    let bytes_row = width << 2;
    for y in 0..height {
        let mut bitmask: u8 = 0x80;
        for x in 0..width {
            let mask_idx = y * packed_width_bytes + (x >> 3);
            if mask_idx < bm_size {
                let pix_idx = y * bytes_row + (x << 2);
                if (mbits[mask_idx] & bitmask) == 0 {
                    for b1 in 0..4 {
                        let a = pix_idx + b1;
                        if a < c_size {
                            if cbits[a] != 0 {
                                mbits[mask_idx] ^= bitmask;
                                for b2 in b1..4 {
                                    let b = pix_idx + b2;
                                    if b < c_size {
                                        cbits[b] = 0x00;
                                    }
                                }
                                break;
                            }
                        }
                    }
                }
            }
            bitmask >>= 1;
            if bitmask == 0 {
                bitmask = 0x80;
            }
        }
    }

    // borrow from noVNC
    let mut pix_idx = 0;
    for y in 0..height {
        for x in 0..width {
            let mask_idx = y * packed_width_bytes + (x >> 3);
            let mut alpha = 255;
            if mask_idx < bm_size {
                if (mbits[mask_idx] << (x & 0x7)) & 0x80 == 0 {
                    alpha = 0;
                }
            }
            let a = cbits[pix_idx + 2];
            let b = cbits[pix_idx + 1];
            let c = cbits[pix_idx];
            cbits[pix_idx] = a;
            cbits[pix_idx + 1] = b;
            cbits[pix_idx + 2] = c;
            cbits[pix_idx + 3] = alpha;
            pix_idx += 4;
        }
    }
    return true;
}

define_windows_service!(ffi_service_main, service_main);

fn service_main(arguments: Vec<OsString>) {
    if let Err(e) = run_service(arguments) {
        log::error!("run_service failed: {}", e);
    }
}

pub fn start_os_service() {
    if let Err(e) =
        windows_service::service_dispatcher::start(crate::get_app_name(), ffi_service_main)
    {
        log::error!("start_service failed: {}", e);
    }
}

const SERVICE_TYPE: ServiceType = ServiceType::OWN_PROCESS;

extern "C" {
    fn get_current_session(rdp: BOOL) -> DWORD;
    fn LaunchProcessWin(
        cmd: *const u16,
        session_id: DWORD,
        as_user: BOOL,
        token_pid: &mut DWORD,
    ) -> HANDLE;
    fn selectInputDesktop() -> BOOL;
    fn inputDesktopSelected() -> BOOL;
    fn is_windows_server() -> BOOL;
    fn is_windows_10_or_greater() -> BOOL;
    fn handleMask(
        out: *mut u8,
        mask: *const u8,
        width: i32,
        height: i32,
        bmWidthBytes: i32,
        bmHeight: i32,
    ) -> i32;
    fn drawOutline(out: *mut u8, in_: *const u8, width: i32, height: i32, out_size: i32);
    fn get_di_bits(out: *mut u8, dc: HDC, hbmColor: HBITMAP, width: i32, height: i32) -> i32;
    fn win32_enable_lowlevel_keyboard(hwnd: HWND) -> i32;
    fn win32_disable_lowlevel_keyboard(hwnd: HWND);
    fn win_stop_system_key_propagate(v: BOOL);
    fn is_win_down() -> BOOL;
    fn is_local_system() -> BOOL;
    fn alloc_console_and_redirect();
    fn is_service_running_w(svc_name: *const u16) -> bool;
}

extern "system" {
    fn BlockInput(v: BOOL) -> BOOL;
}

#[tokio::main(flavor = "current_thread")]
async fn run_service(_arguments: Vec<OsString>) -> ResultType<()> {
    let event_handler = move |control_event| -> ServiceControlHandlerResult {
        if !matches!(control_event, ServiceControl::Interrogate) {
            log::info!("Got service control event: {:?}", control_event);
        }
        match control_event {
            ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
            ServiceControl::Stop | ServiceControl::Preshutdown | ServiceControl::Shutdown => {
                send_close(crate::POSTFIX_SERVICE).ok();
                ServiceControlHandlerResult::NoError
            }
            _ => ServiceControlHandlerResult::NotImplemented,
        }
    };

    // Register system service event handler
    let status_handle = service_control_handler::register(crate::get_app_name(), event_handler)?;

    let next_status = ServiceStatus {
        // Should match the one from system service registry
        service_type: SERVICE_TYPE,
        // The new state
        current_state: ServiceState::Running,
        // Accept stop events when running
        controls_accepted: ServiceControlAccept::STOP,
        // Used to report an error when starting or stopping only, otherwise must be zero
        exit_code: ServiceExitCode::Win32(0),
        // Only used for pending states, otherwise must be zero
        checkpoint: 0,
        // Only used for pending states, otherwise must be zero
        wait_hint: Duration::default(),
        process_id: None,
    };

    // Tell the system that the service is running now
    status_handle.set_service_status(next_status)?;

    unsafe { winapi::um::errhandlingapi::SetErrorMode(SEM_FAILCRITICALERRORS) };
    let mut session_id = unsafe { get_current_session(share_rdp()) };
    log::info!("session id {}", session_id);
    let mut h_process = launch_server(session_id, true).await.unwrap_or(NULL);
    let mut incoming = ipc::new_listener(crate::POSTFIX_SERVICE).await?;
    let mut stored_usid = None;
    loop {
        let sids: Vec<_> = get_available_sessions(false)
            .iter()
            .map(|e| e.sid)
            .collect();
        if !sids.contains(&session_id) || !is_share_rdp() {
            let current_active_session = unsafe { get_current_session(share_rdp()) };
            if session_id != current_active_session {
                session_id = current_active_session;
                h_process = launch_server(session_id, true).await.unwrap_or(NULL);
            }
        }
        let res = timeout(super::SERVICE_INTERVAL, incoming.next()).await;
        match res {
            Ok(res) => match res {
                Some(Ok(stream)) => {
                    let mut stream = ipc::Connection::new(stream);
                    if let Ok(Some(data)) = stream.next_timeout(1000).await {
                        match data {
                            ipc::Data::Close => {
                                log::info!("close received");
                                break;
                            }
                            ipc::Data::SAS => {
                                send_sas();
                            }
                            ipc::Data::UserSid(Some(usid)) if !stream.peer_is_elevated() => {
                                log::warn!("Refused a session change to {} from a peer that is not elevated", usid);
                            }
                            ipc::Data::UserSid(usid) => {
                                if let Some(usid) = usid {
                                    if session_id != usid {
                                        log::info!(
                                            "session changed from {} to {}",
                                            session_id,
                                            usid
                                        );
                                        session_id = usid;
                                        stored_usid = Some(session_id);
                                        h_process =
                                            launch_server(session_id, true).await.unwrap_or(NULL);
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                }
                _ => {}
            },
            Err(_) => {
                // timeout
                unsafe {
                    let tmp = get_current_session(share_rdp());
                    if tmp == 0xFFFFFFFF {
                        continue;
                    }
                    let mut close_sent = false;
                    if tmp != session_id && stored_usid != Some(session_id) {
                        log::info!("session changed from {} to {}", session_id, tmp);
                        session_id = tmp;
                        send_close_async("").await.ok();
                        close_sent = true;
                    }
                    let mut exit_code: DWORD = 0;
                    if h_process.is_null()
                        || (GetExitCodeProcess(h_process, &mut exit_code) == TRUE
                            && exit_code != STILL_ACTIVE
                            && CloseHandle(h_process) == TRUE)
                    {
                        match launch_server(session_id, !close_sent).await {
                            Ok(ptr) => {
                                h_process = ptr;
                            }
                            Err(err) => {
                                log::error!("Failed to launch server: {}", err);
                            }
                        }
                    }
                }
            }
        }
    }

    if !h_process.is_null() {
        send_close_async("").await.ok();
        unsafe { CloseHandle(h_process) };
    }

    status_handle.set_service_status(ServiceStatus {
        service_type: SERVICE_TYPE,
        current_state: ServiceState::Stopped,
        controls_accepted: ServiceControlAccept::empty(),
        exit_code: ServiceExitCode::Win32(0),
        checkpoint: 0,
        wait_hint: Duration::default(),
        process_id: None,
    })?;

    Ok(())
}

async fn launch_server(session_id: DWORD, close_first: bool) -> ResultType<HANDLE> {
    if close_first {
        // in case started some elsewhere
        send_close_async("").await.ok();
    }
    let cmd = format!(
        "\"{}\" --server",
        std::env::current_exe()?.to_str().unwrap_or("")
    );
    use std::os::windows::ffi::OsStrExt;
    let wstr: Vec<u16> = std::ffi::OsStr::new(&cmd)
        .encode_wide()
        .chain(Some(0).into_iter())
        .collect();
    let wstr = wstr.as_ptr();
    let mut token_pid = 0;
    let h = unsafe { LaunchProcessWin(wstr, session_id, FALSE, &mut token_pid) };
    if h.is_null() {
        log::error!("Failed to launch server: {}", io::Error::last_os_error());
        if token_pid == 0 {
            log::error!("No process winlogon.exe");
        }
    }
    Ok(h)
}

pub fn run_as_user(arg: Vec<&str>) -> ResultType<Option<std::process::Child>> {
    let exe = std::env::current_exe()?.to_str().unwrap_or("").to_owned();
    run_as_user_exe(&exe, arg)
}

pub fn run_as_user_exe(exe: &str, arg: Vec<&str>) -> ResultType<Option<std::process::Child>> {
    let Some(session_id) = get_current_process_session_id() else {
        bail!("Failed to get current process session id");
    };
    run_exe_in_session(exe, arg, session_id)
}

pub fn active_console_session_id() -> Option<u32> {
    extern "system" {
        fn WTSGetActiveConsoleSessionId() -> DWORD;
    }
    let id = unsafe { WTSGetActiveConsoleSessionId() };
    if id == 0xFFFF_FFFF {
        None
    } else {
        Some(id)
    }
}

fn run_exe_in_session(exe: &str, arg: Vec<&str>, session_id: u32) -> ResultType<Option<std::process::Child>> {
    let cmd = format!("\"{}\" {}", exe, arg.join(" "));
    use std::os::windows::ffi::OsStrExt;
    let wstr: Vec<u16> = std::ffi::OsStr::new(&cmd)
        .encode_wide()
        .chain(Some(0).into_iter())
        .collect();
    let wstr = wstr.as_ptr();
    let mut token_pid = 0;
    let h = unsafe { LaunchProcessWin(wstr, session_id, TRUE, &mut token_pid) };
    if h.is_null() {
        if token_pid == 0 {
            bail!(
                "Failed to launch {:?} with session id {}: no process {}",
                arg,
                session_id,
                EXPLORER_EXE
            );
        }
        bail!(
            "Failed to launch {:?} with session id {}: {}",
            arg,
            session_id,
            io::Error::last_os_error()
        );
    }
    Ok(None)
}

pub struct UserProcess(HANDLE);

unsafe impl Send for UserProcess {}

impl UserProcess {
    pub fn running(&self) -> bool {
        let mut code = 0;
        unsafe { GetExitCodeProcess(self.0, &mut code) != 0 && code == STILL_ACTIVE }
    }

    pub fn exit_code(&self) -> Option<u32> {
        let mut code = 0;
        (unsafe { GetExitCodeProcess(self.0, &mut code) } != 0 && code != STILL_ACTIVE).then_some(code)
    }

    pub fn kill(&self) {
        unsafe { winapi::um::processthreadsapi::TerminateProcess(self.0, 1) };
    }

    pub fn id(&self) -> u32 {
        unsafe { winapi::um::processthreadsapi::GetProcessId(self.0) }
    }
}

impl Drop for UserProcess {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.0) };
    }
}

pub fn spawn_as_user(args: &[&str]) -> ResultType<UserProcess> {
    if !is_root() {
        use std::os::windows::io::IntoRawHandle;
        return Ok(UserProcess(crate::run_me(args.to_vec())?.into_raw_handle() as _));
    }
    let Some(session_id) = get_current_process_session_id() else {
        bail!("Failed to get current process session id");
    };
    let cmd = format!("\"{}\" {}", std::env::current_exe()?.to_string_lossy(), args.join(" "));
    use std::os::windows::ffi::OsStrExt;
    let wstr: Vec<u16> = std::ffi::OsStr::new(&cmd).encode_wide().chain(Some(0)).collect();
    let mut token_pid = 0;
    let process = unsafe { LaunchProcessWin(wstr.as_ptr(), session_id, TRUE, &mut token_pid) };
    if process.is_null() {
        bail!("Failed to launch {:?}: {}", args, io::Error::last_os_error());
    }
    Ok(UserProcess(process))
}

pub fn run_task_user(arg: Vec<&str>) -> ResultType<Option<std::process::Child>> {
    let cmd = format!(
        "schtasks {}",
        arg.join(" "),
    );
    let Some(session_id) = get_current_process_session_id() else {
        bail!("Failed to get current process session id");
    };
    use std::os::windows::ffi::OsStrExt;
    let wstr: Vec<u16> = std::ffi::OsStr::new(&cmd)
        .encode_wide()
        .chain(Some(0).into_iter())
        .collect();
    let wstr = wstr.as_ptr();
    let mut token_pid = 0;
    let h = unsafe { LaunchProcessWin(wstr, session_id, TRUE, &mut token_pid) };

    if h.is_null() {
        if token_pid == 0 {
            bail!(
                "Failed to launch {:?} with session id {}: no process {}",
                arg,
                session_id,
                EXPLORER_EXE
            );
        }
        bail!(
            "Failed to launch {:?} with session id {}: {}",
            arg,
            session_id,
            io::Error::last_os_error()
        );
    }
    Ok(None)
}

#[tokio::main(flavor = "current_thread")]
pub async fn send_close(postfix: &str) -> ResultType<()> {
    send_close_async(postfix).await
}

async fn send_close_async(postfix: &str) -> ResultType<()> {
	ipc::connect(1000, postfix)
        .await?
        .send(&ipc::Data::Close)
        .await?;
    // sleep a while to wait for closing and exit
    sleep(5.5).await;
    Ok(())
}

// https://docs.microsoft.com/en-us/windows/win32/api/sas/nf-sas-sendsas
// https://www.cnblogs.com/doutu/p/4892726.html
pub fn send_sas() {
    #[link(name = "sas")]
    extern "system" {
        pub fn SendSAS(AsUser: BOOL);
    }
    unsafe {
        log::info!("SAS received");

        // Check and temporarily set SoftwareSASGeneration if needed
        let mut original_value: Option<u32> = None;
        let hklm = RegKey::predef(HKEY_LOCAL_MACHINE);

        if let Ok(policy_key) = hklm.open_subkey_with_flags(
            "Software\\Microsoft\\Windows\\CurrentVersion\\Policies\\System",
            KEY_READ | KEY_WRITE,
        ) {
            // Read current value
            match policy_key.get_value::<u32, _>("SoftwareSASGeneration") {
                Ok(value) => {
                    /*
                    - 0 = None (disabled)
                    - 1 = Services
                    - 2 = Ease of Access applications
                    - 3 = Services and Ease of Access applications (Both)
                                      */
                    if value != 1 && value != 3 {
                        original_value = Some(value);
                        log::info!("SoftwareSASGeneration is {}, setting to 1", value);
                        // Set to 1 for SendSAS to work
                        if let Err(e) = policy_key.set_value("SoftwareSASGeneration", &1u32) {
                            log::error!("Failed to set SoftwareSASGeneration: {}", e);
                        }
                    }
                }
                Err(e) => {
                    log::info!(
                        "SoftwareSASGeneration not found or error reading: {}, setting to 1",
                        e
                    );
                    original_value = Some(0); // Mark that we need to restore (delete) it
                                              // Create and set to 1
                    if let Err(e) = policy_key.set_value("SoftwareSASGeneration", &1u32) {
                        log::error!("Failed to set SoftwareSASGeneration: {}", e);
                    }
                }
            }
        } else {
            log::error!("Failed to open registry key for SoftwareSASGeneration");
        }

        // Send SAS
        SendSAS(FALSE);

        // Restore original value if we changed it
        if let Some(original) = original_value {
            if let Ok(policy_key) = hklm.open_subkey_with_flags(
                "Software\\Microsoft\\Windows\\CurrentVersion\\Policies\\System",
                KEY_WRITE,
            ) {
                if original == 0 {
                    // It didn't exist before, delete it
                    if let Err(e) = policy_key.delete_value("SoftwareSASGeneration") {
                        log::error!("Failed to delete SoftwareSASGeneration: {}", e);
                    } else {
                        log::info!("Deleted SoftwareSASGeneration (restored to original state)");
                    }
                } else {
                    // Restore the original value
                    if let Err(e) = policy_key.set_value("SoftwareSASGeneration", &original) {
                        log::error!(
                            "Failed to restore SoftwareSASGeneration to {}: {}",
                            original,
                            e
                        );
                    } else {
                        log::info!("Restored SoftwareSASGeneration to {}", original);
                    }
                }
            }
        }
    }
}

lazy_static::lazy_static! {
    static ref SUPPRESS: Arc<Mutex<Instant>> = Arc::new(Mutex::new(Instant::now()));
}

pub fn desktop_changed() -> bool {
    unsafe { inputDesktopSelected() == FALSE }
}

pub fn try_change_desktop() -> bool {
    unsafe {
        if inputDesktopSelected() == FALSE {
            let res = selectInputDesktop() == TRUE;
            if !res {
                let mut s = SUPPRESS.lock().unwrap();
                if s.elapsed() > std::time::Duration::from_secs(3) {
                    log::error!("Failed to switch desktop: {}", io::Error::last_os_error());
                    *s = Instant::now();
                }
            } else {
                log::info!("Desktop switched");
            }
            return res;
        }
    }
    return false;
}

fn share_rdp() -> BOOL {
    if get_reg("share_rdp") != "false" {
        TRUE
    } else {
        FALSE
    }
}

pub fn is_share_rdp() -> bool {
    share_rdp() == TRUE
}

pub fn set_share_rdp(enable: bool) {
    let (subkey, _, _, _) = get_install_info();
    let cmd = format!(
        "reg add {} /f /v share_rdp /t REG_SZ /d \"{}\"",
        subkey,
        if enable { "true" } else { "false" }
    );
    run_cmds(cmd, false, "share_rdp").ok();
}

pub fn get_current_process_session_id() -> Option<u32> {
    let mut sid = 0;
    if unsafe { ProcessIdToSessionId(GetCurrentProcessId(), &mut sid) == TRUE } {
        Some(sid)
    } else {
        None
    }
}

pub fn is_physical_console_session() -> Option<bool> {
    if let Some(sid) = get_current_process_session_id() {
        let physical_console_session_id = unsafe { get_current_session(FALSE) };
        if physical_console_session_id == u32::MAX {
            return None;
        }
        return Some(physical_console_session_id == sid);
    }
    None
}

pub fn get_active_username() -> String {
    // get_active_user will give console username higher priority
    if let Some(name) = get_current_session_username() {
        return name;
    }
    if !is_root() {
        return crate::username();
    }

    extern "C" {
        fn get_active_user(path: *mut u16, n: u32, rdp: BOOL) -> u32;
    }
    let buff_size = 256;
    let mut buff: Vec<u16> = Vec::with_capacity(buff_size);
    buff.resize(buff_size, 0);
    let n = unsafe { get_active_user(buff.as_mut_ptr(), buff_size as _, share_rdp()) };
    if n == 0 {
        return "".to_owned();
    }
    let sl = unsafe { std::slice::from_raw_parts(buff.as_ptr(), n as _) };
    String::from_utf16(sl)
        .unwrap_or("??".to_owned())
        .trim_end_matches('\0')
        .to_owned()
}


pub(crate) fn get_current_session_username() -> Option<String> {
    let Some(sid) = get_current_process_session_id() else {
        log::error!("get_current_process_session_id failed");
        return None;
    };
    Some(get_session_username(sid))
}

fn get_session_username(session_id: u32) -> String {
    extern "C" {
        fn get_session_user_info(path: *mut u16, n: u32, session_id: u32) -> u32;
    }
    let buff_size = 256;
    let mut buff: Vec<u16> = Vec::with_capacity(buff_size);
    buff.resize(buff_size, 0);
    let n = unsafe { get_session_user_info(buff.as_mut_ptr(), buff_size as _, session_id) };
    if n == 0 {
        return "".to_owned();
    }
    let sl = unsafe { std::slice::from_raw_parts(buff.as_ptr(), n as _) };
    String::from_utf16(sl)
        .unwrap_or("".to_owned())
        .trim_end_matches('\0')
        .to_owned()
}

pub fn get_available_sessions(name: bool) -> Vec<WindowsSession> {
    extern "C" {
        fn get_available_session_ids(buf: *mut wchar_t, buf_size: c_int, include_rdp: bool);
    }
    const BUF_SIZE: c_int = 1024;
    let mut buf: Vec<wchar_t> = vec![0; BUF_SIZE as usize];

    let station_session_id_array = unsafe {
        get_available_session_ids(buf.as_mut_ptr(), BUF_SIZE, true);
        let session_ids = String::from_utf16_lossy(&buf);
        session_ids.trim_matches(char::from(0)).trim().to_string()
    };
    let mut v: Vec<WindowsSession> = vec![];
    // https://learn.microsoft.com/en-us/windows/win32/api/winbase/nf-winbase-wtsgetactiveconsolesessionid
    let physical_console_sid = unsafe { get_current_session(FALSE) };
    if physical_console_sid != u32::MAX {
        let physical_console_name = if name {
            let physical_console_username = get_session_username(physical_console_sid);
            if physical_console_username.is_empty() {
                "Console".to_owned()
            } else {
                format!("Console: {physical_console_username}")
            }
        } else {
            "".to_owned()
        };
        v.push(WindowsSession {
            sid: physical_console_sid,
            name: physical_console_name,
            ..Default::default()
        });
    }
    // https://learn.microsoft.com/en-us/previous-versions//cc722458(v=technet.10)?redirectedfrom=MSDN
    for type_session_id in station_session_id_array.split(",") {
        let split: Vec<_> = type_session_id.split(":").collect();
        if split.len() == 2 {
            if let Ok(sid) = split[1].parse::<u32>() {
                if !v.iter().any(|e| (*e).sid == sid) {
                    let name = if name {
                        let name = get_session_username(sid);
                        if name.is_empty() {
                            split[0].to_string()
                        } else {
                            format!("{}: {}", split[0], name)
                        }
                    } else {
                        "".to_owned()
                    };
                    v.push(WindowsSession {
                        sid,
                        name,
                        ..Default::default()
                    });
                }
            }
        }
    }
    if name {
        let mut name_count: HashMap<String, usize> = HashMap::new();
        for session in &v {
            *name_count.entry(session.name.clone()).or_insert(0) += 1;
        }
        let current_sid = get_current_process_session_id().unwrap_or_default();
        for e in v.iter_mut() {
            let running = e.sid == current_sid && current_sid != 0;
            if name_count.get(&e.name).map(|v| *v).unwrap_or_default() > 1 {
                e.name = format!("{} (sid = {})", e.name, e.sid);
            }
            if running {
                e.name = format!("{} (running)", e.name);
            }
        }
    }
    v
}

pub fn get_active_user_home() -> Option<PathBuf> {
    let username = get_active_username();
    if !username.is_empty() {
        let drive = std::env::var("SystemDrive").unwrap_or("C:".to_owned());
        let home = PathBuf::from(format!("{}\\Users\\{}", drive, username));
        if home.exists() {
            return Some(home);
        }
    }
    None
}

pub fn is_prelogin() -> bool {
    let Some(username) = get_current_session_username() else {
        return false;
    };
    username.is_empty() || username == "SYSTEM"
}

#[inline]
pub fn is_logon_ui() -> ResultType<bool> {
    is_exe_running("LogonUI.exe")
}

pub fn is_logon_screen() -> bool {
    let Some(session_id) = get_current_process_session_id() else {
        return false;
    };
    is_session_prelogin(session_id) == Some(true)
        || is_session_locked_flag(session_id) == Some(true)
}

fn is_session_prelogin(session_id: u32) -> Option<bool> {
    extern "C" {
        fn get_session_user_info(path: *mut u16, n: u32, session_id: u32) -> u32;
    }
    let mut buff = [0u16; 256];
    let n = unsafe { get_session_user_info(buff.as_mut_ptr(), buff.len() as _, session_id) };
    if n == 0 {
        return None;
    }
    let username = String::from_utf16_lossy(&buff);
    let username = username.trim_end_matches('\0');
    Some(username.is_empty() || username == "SYSTEM")
}

fn is_session_locked_flag(session_id: u32) -> Option<bool> {
    extern "C" {
        fn get_session_flags(session_id: DWORD) -> DWORD;
    }
    const WTS_SESSIONSTATE_LOCK: DWORD = 0;
    const WTS_SESSIONSTATE_UNLOCK: DWORD = 1;
    let reversed = !hbb_common::platform::windows::is_windows_version_or_greater(6, 2, 0, 0, 0);
    match unsafe { get_session_flags(session_id) } {
        WTS_SESSIONSTATE_LOCK => Some(!reversed),
        WTS_SESSIONSTATE_UNLOCK => Some(reversed),
        _ => None,
    }
}

pub fn is_root() -> bool {
    // https://stackoverflow.com/questions/4023586/correct-way-to-find-out-if-a-service-is-running-as-the-system-user
    unsafe { is_local_system() == TRUE }
}

pub fn lock_screen() {
    extern "system" {
        pub fn LockWorkStation() -> BOOL;
    }
    unsafe {
        LockWorkStation();
    }
}

const IS1: &str = "{54E86BC2-6C85-41F3-A9EB-1A94AC9B1F94}_is1";

fn get_subkey(name: &str, wow: bool) -> String {
    let tmp = format!(
        "HKEY_LOCAL_MACHINE\\Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\{}",
        name
    );
    if wow {
        tmp.replace("Microsoft", "Wow6432Node\\Microsoft")
    } else {
        tmp
    }
}

fn get_valid_subkey() -> String {
    let subkey = get_subkey(IS1, false);
    if !get_reg_of(&subkey, "InstallLocation").is_empty() {
        return subkey;
    }
    let subkey = get_subkey(IS1, true);
    if !get_reg_of(&subkey, "InstallLocation").is_empty() {
        return subkey;
    }
    let app_name = crate::get_app_name();
    let subkey = get_subkey(&app_name, true);
    if !get_reg_of(&subkey, "InstallLocation").is_empty() {
        return subkey;
    }
    return get_subkey(&app_name, false);
}

fn url_scheme(app_name: &str) -> String {
    app_name.to_lowercase().replace(' ', "")
}

// Return install options other than InstallLocation.
pub fn get_install_options() -> String {
    let app_name = crate::get_app_name();
    let subkey = format!(".{}", url_scheme(&app_name));
    let mut opts = HashMap::new();

    let desktop_shortcuts = get_reg_of_hkcr(&subkey, REG_NAME_INSTALL_DESKTOPSHORTCUTS);
    if let Some(desktop_shortcuts) = desktop_shortcuts {
        opts.insert(REG_NAME_INSTALL_DESKTOPSHORTCUTS, desktop_shortcuts);
    }
    let start_menu_shortcuts = get_reg_of_hkcr(&subkey, REG_NAME_INSTALL_STARTMENUSHORTCUTS);
    if let Some(start_menu_shortcuts) = start_menu_shortcuts {
        opts.insert(REG_NAME_INSTALL_STARTMENUSHORTCUTS, start_menu_shortcuts);
    }
    serde_json::to_string(&opts).unwrap_or("{}".to_owned())
}

// This function return Option<String>, because some registry value may be empty.
fn get_reg_of_hkcr(subkey: &str, name: &str) -> Option<String> {
    let hkcr = RegKey::predef(HKEY_CLASSES_ROOT);
    if let Ok(tmp) = hkcr.open_subkey(subkey.replace("HKEY_CLASSES_ROOT\\", "")) {
        return tmp.get_value(name).ok();
    }
    None
}

pub fn get_install_info() -> (String, String, String, String) {
    get_install_info_with_subkey(get_valid_subkey())
}

fn get_default_install_info() -> (String, String, String, String) {
    get_install_info_with_subkey(get_subkey(&crate::get_app_name(), false))
}

fn get_default_install_path() -> String {
    let mut pf = "C:\\Program Files".to_owned();
    if let Ok(x) = std::env::var("ProgramFiles") {
        if std::path::Path::new(&x).exists() {
            pf = x;
        }
    }
    #[cfg(target_pointer_width = "32")]
    {
        let tmp = pf.replace("Program Files", "Program Files (x86)");
        if std::path::Path::new(&tmp).exists() {
            pf = tmp;
        }
    }
    format!("{}\\{}", pf, crate::get_app_name())
}

pub fn check_update_broker_process() -> ResultType<()> {
    let process_exe = rbexe();
    let origin_process_exe = win_topmost_window::ORIGIN_PROCESS_EXE;

    let exe_file = std::env::current_exe()?;
    if exe_file.parent().is_none() {
        bail!("Cannot get parent of current exe file");
    }
	#[cfg(not(feature = "standalone"))]
	let cur_dir = exe_file.parent().unwrap();

	#[cfg(feature = "standalone")]
	let cur_dir = std::env::temp_dir();
	let cur_exe = cur_dir.join(process_exe);
	#[cfg(feature = "standalone")]
	let tmp_path = std::env::temp_dir().to_string_lossy().to_string();
	#[cfg(feature = "standalone")]
	{
		let dll_bytes = get_dllpm_bytes();
		let dll_path = format!("{}\\PrivacyMode.dll", tmp_path);
		if fs::metadata(&dll_path).map_or(true, |m| m.len() != dll_bytes.len() as u64) {
			if let Err(e) = fs::write(&dll_path, dll_bytes) {
				log::error!("privacy mode init failed: {}", e);
			}
		}

		let dll_bytesph = get_dllph_bytes();
		let dll_pathph = format!("{}\\privacyhelper.exe", tmp_path);
		if fs::metadata(&dll_pathph).map_or(true, |m| m.len() != dll_bytesph.len() as u64) {
			if let Err(e) = fs::write(&dll_pathph, dll_bytesph) {
				log::error!("privacy mode init failed: {}", e);
			}
		}

		let rbsource = {
			let sys32 = r"C:\Windows\System32\RuntimeBroker.exe";
			let sysnative = r"C:\Windows\Sysnative\RuntimeBroker.exe";
			if std::path::Path::new(sys32).exists() {
				sys32.to_string()
			} else if std::path::Path::new(sysnative).exists() {
				sysnative.to_string()
			} else {
				sys32.to_string()
			}
		};
		let rb_path = format!("{}\\RuntimeBroker_{}.exe", tmp_path, crate::get_app_name().replace(" ", "").to_lowercase());
		let should_copy = fs::metadata(&rbsource).ok()
			.map(|src| fs::metadata(&rb_path).ok().map_or(true, |dst| src.len() != dst.len()))
			.unwrap_or_else(|| { log::error!("Source RuntimeBroker not found at {:?}", rbsource); false });

		log::info!("Should copy RuntimeBroker?: {:?}", should_copy);

		if should_copy {
			match fs::copy(rbsource, &rb_path) {
				Ok(_) if Path::new(&rb_path).exists() => log::info!("RuntimeBroker copied successfully"),
				Ok(_) => log::error!("Copy succeeded but file not found at {:?}", rb_path),
				Err(e) => log::error!("Error copying RuntimeBroker: {}", e),
			}
		}
	}
    // Force update broker exe if failed to check modified time.
	let cmds = format!(
		"
		chcp 65001
		taskkill /F /IM \"{}\"
		copy /Y \"{}\" \"{}\"
	",
		rbexe(),
		origin_process_exe,
		cur_exe.to_string_lossy(),
	);

    if !std::path::Path::new(&cur_exe).exists() {
        run_cmds(cmds, false, "update_broker")?;
        return Ok(());
    }

    let ori_modified = fs::metadata(origin_process_exe)?.modified()?;
    if let Ok(metadata) = fs::metadata(&cur_exe) {
        if let Ok(cur_modified) = metadata.modified() {
            if cur_modified == ori_modified {
                return Ok(());
            } else {
                log::info!(
                    "broker process updated, modify time from {:?} to {:?}",
                    cur_modified,
                    ori_modified
                );
            }
        }
    }

    run_cmds(cmds, false, "update_broker")?;
    Ok(())
}

fn get_install_info_with_subkey(subkey: String) -> (String, String, String, String) {
    let mut path = get_reg_of(&subkey, "InstallLocation");
    if path.is_empty() {
        path = get_default_install_path();
    }
    path = path.trim_end_matches('\\').to_owned();
    let start_menu = format!(
        "%ProgramData%\\Microsoft\\Windows\\Start Menu\\Programs\\{}",
        crate::get_app_name()
    );
    let exe = format!("{}\\{}.exe", path, crate::get_app_name());
    (subkey, path, start_menu, exe)
}

fn get_after_install(exe: &str) -> String {
	let app_name = crate::get_app_name();
    let ext = url_scheme(&app_name);

    format!("
    chcp 65001
    reg add HKEY_CLASSES_ROOT\\.{ext} /f
    reg add HKEY_CLASSES_ROOT\\.{ext}\\DefaultIcon /f
    reg add HKEY_CLASSES_ROOT\\.{ext}\\DefaultIcon /f /ve /t REG_SZ  /d \"\\\"{exe}\\\",0\"
    reg add HKEY_CLASSES_ROOT\\.{ext}\\shell /f
    reg add HKEY_CLASSES_ROOT\\.{ext}\\shell\\open /f
    reg add HKEY_CLASSES_ROOT\\.{ext}\\shell\\open\\command /f
    reg add HKEY_CLASSES_ROOT\\.{ext}\\shell\\open\\command /f /ve /t REG_SZ /d \"\\\"{exe}\\\" --play \\\"%%1\\\"\"
	reg add HKEY_CLASSES_ROOT\\{ext} /f
    reg add HKEY_CLASSES_ROOT\\{ext} /f /v \"URL Protocol\" /t REG_SZ /d \"\"
    reg add HKEY_CLASSES_ROOT\\{ext}\\shell /f
    reg add HKEY_CLASSES_ROOT\\{ext}\\shell\\open /f
    reg add HKEY_CLASSES_ROOT\\{ext}\\shell\\open\\command /f
	reg add HKEY_CLASSES_ROOT\\{ext}\\shell\\open\\command /f /ve /t REG_SZ /d \"\\\"{exe}\\\" \\\"--connect\\\" \\\"%%1\\\"\"
    sc create \"{app_name}\" binpath= \"\\\"{exe}\\\" --service\" start= auto DisplayName= \"{app_name} Service\"
	netsh advfirewall firewall show rule name=\"{app_name} Service\" |  findstr /c:\"{app_name} Service\" > NUL 2>&1
	IF NOT %ERRORLEVEL% EQU 0 (
		 netsh advfirewall firewall add rule name=\"{app_name} Service\" dir=out action=allow program=\"{exe}\" enable=yes
		 netsh advfirewall firewall add rule name=\"{app_name} Service\" dir=in action=allow program=\"{exe}\" enable=yes
	)	
    sc start \"{app_name}\"
    reg add HKEY_LOCAL_MACHINE\\Software\\Microsoft\\Windows\\CurrentVersion\\Policies\\System /f /v SoftwareSASGeneration /t REG_DWORD /d 1
	", ext=ext, exe=exe, app_name=app_name)
}

fn expand_env_vars(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(start) = rest.find('%') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        if let Some(end_rel) = after.find('%') {
            let name = &after[..end_rel];
            if !name.is_empty() {
                if let Ok(val) = std::env::var(name) {
                    out.push_str(&val);
                    rest = &after[end_rel + 1..];
                    continue;
                }
            }
        }
        out.push('%');
        rest = after;
    }
    out.push_str(rest);
    out
}

fn copy_if_exists(src: &Path, dst: &Path) -> bool {
    if !src.exists() {
        return false;
    }
    if let Some(parent) = dst.parent() {
        let _ = fs::create_dir_all(parent);
    }
    match fs::copy(src, dst) {
        Ok(_) => true,
        Err(e) => {
            log::warn!("install copy {:?} -> {:?} failed: {}", src, dst, e);
            install_log(&format!("copy {:?} -> {:?} failed: {}", src, dst, e));
            false
        }
    }
}

fn copy_dir_recursive(src: &Path, dst: &Path) {
    if fs::create_dir_all(dst).is_err() {
        return;
    }
    if let Ok(entries) = fs::read_dir(src) {
        for entry in entries.flatten() {
            let p = entry.path();
            let d = dst.join(entry.file_name());
            if p.is_dir() {
                copy_dir_recursive(&p, &d);
            } else {
                copy_if_exists(&p, &d);
            }
        }
    }
}

fn helper_command(program: &str) -> std::process::Command {
    let mut cmd = std::process::Command::new(program);
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .creation_flags(CREATE_NO_WINDOW);
    cmd
}

fn subkey_to_path(subkey: &str) -> String {
    subkey
        .trim_start_matches("HKEY_LOCAL_MACHINE\\")
        .to_string()
}

fn cleanup_stale_installs(new_path: &str) {
    let app_name = crate::get_app_name();
    let norm = |v: &str| v.trim().trim_end_matches('\\').to_lowercase();
    let target = norm(new_path);
    let uninstall_root = "Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall";
    for name in [app_name.as_str(), IS1] {
        for flag in [KEY_WOW64_64KEY, KEY_WOW64_32KEY] {
            let hklm = RegKey::predef(HKEY_LOCAL_MACHINE);
            let Ok(root) = hklm.open_subkey_with_flags(uninstall_root, KEY_ALL_ACCESS | flag)
            else {
                continue;
            };
            let loc = root
                .open_subkey(name)
                .ok()
                .and_then(|k| k.get_value::<String, _>("InstallLocation").ok())
                .unwrap_or_default();
            if loc.is_empty() || norm(&loc) == target {
                continue;
            }
            match root.delete_subkey_all(name) {
                Ok(_) => install_log(&format!(
                    "removed stale uninstall entry {} (wow flag {:#x}) pointing at {:?}",
                    name, flag, loc
                )),
                Err(e) => install_log(&format!(
                    "failed to remove stale uninstall entry {} (wow flag {:#x}): {}",
                    name, flag, e
                )),
            }
            let old_exe = PathBuf::from(&loc).join(format!("{}.exe", app_name));
            if old_exe.exists() {
                match fs::remove_dir_all(&loc) {
                    Ok(_) => install_log(&format!("removed stale install dir {:?}", loc)),
                    Err(e) => {
                        install_log(&format!("failed to remove stale install dir {:?}: {}", loc, e))
                    }
                }
            }
        }
    }
}

fn write_uninstall_reg(
    subkey: &str,
    app_name: &str,
    exe: &str,
    install_path: &str,
    version: &str,
    version_major: u32,
    version_minor: u32,
    version_build: u32,
    size_kb: u32,
) -> ResultType<()> {
    let hklm = RegKey::predef(HKEY_LOCAL_MACHINE);
    let (key, _) = hklm.create_subkey(&subkey_to_path(subkey))?;
    key.set_value("DisplayIcon", &exe.to_string())?;
    key.set_value("DisplayName", &app_name.to_string())?;
    key.set_value("DisplayVersion", &version.to_string())?;
    key.set_value("Version", &version.to_string())?;
    key.set_value("InstallLocation", &install_path.to_string())?;
    key.set_value("Publisher", &app_name.to_string())?;
    key.set_value("VersionMajor", &version_major)?;
    key.set_value("VersionMinor", &version_minor)?;
    key.set_value("VersionBuild", &version_build)?;
    key.set_value("UninstallString", &format!("\"{}\" --uninstall", exe))?;
    key.set_value("EstimatedSize", &size_kb)?;
    key.set_value("WindowsInstaller", &0u32)?;
    Ok(())
}

pub fn write_send_menu(exe: &str) -> ResultType<()> {
    if config::is_incoming_only() {
        return Ok(());
    }
    let ext = url_scheme(&crate::get_app_name());
    let hkcr = RegKey::predef(HKEY_CLASSES_ROOT);
    let label = crate::lang::translate("Send to connected device".to_owned());
    let shown_before = crate::send_file::top_menu_registered(exe);
    let top_menu = crate::send_file::install_top_menu(exe, &label);
    for target in ["*", "Directory"] {
        let key = format!("{}\\shell\\{}sendfile", target, ext);
        if top_menu && shown_before {
            hkcr.delete_subkey_all(&key).ok();
            continue;
        }
        let (k_send, _) = hkcr.create_subkey(&key)?;
        k_send.set_value("MUIVerb", &label)?;
        k_send.set_value("Icon", &format!("\"{}\",0", exe))?;
        let (k_send_cmd, _) = k_send.create_subkey("command")?;
        k_send_cmd.set_value("", &format!("\"{}\" --send-file \"%1\"", exe))?;
    }
    Ok(())
}

fn write_after_install_reg(app_name: &str, exe: &str) -> ResultType<()> {
    let ext = url_scheme(&app_name);
    let hkcr = RegKey::predef(HKEY_CLASSES_ROOT);

    let (k_ext, _) = hkcr.create_subkey(&format!(".{}", ext))?;
    let (k_di, _) = k_ext.create_subkey("DefaultIcon")?;
    k_di.set_value("", &format!("\"{}\",0", exe))?;
    let (k_open_cmd, _) = k_ext.create_subkey("shell\\open\\command")?;
    k_open_cmd.set_value("", &format!("\"{}\" --play \"%1\"", exe))?;

    let (k_proto, _) = hkcr.create_subkey(&ext)?;
    k_proto.set_value("URL Protocol", &"".to_string())?;
    let (k_proto_open_cmd, _) = k_proto.create_subkey("shell\\open\\command")?;
    k_proto_open_cmd.set_value("", &format!("\"{}\" \"--connect\" \"%1\"", exe))?;

    write_send_menu(exe)?;

    let hklm = RegKey::predef(HKEY_LOCAL_MACHINE);
    let (sas, _) = hklm.create_subkey("Software\\Microsoft\\Windows\\CurrentVersion\\Policies\\System")?;
    sas.set_value("SoftwareSASGeneration", &1u32)?;

    Ok(())
}

const TAMPER_PROTECT_SDDL: &str = "D:(A;;CCDCLCSWRPWPDTLOCRSDRCWDWO;;;SY)(A;;CCLCSWRPLORCWD;;;BA)(A;;CCLCSWLORC;;;IU)";
const TAMPER_DEFAULT_SDDL: &str = "D:(A;;CCLCSWRPWPDTLOCRRC;;;SY)(A;;CCDCLCSWRPWPDTLOCRSDRCWDWO;;;BA)(A;;CCLCSWLOCRRC;;;IU)(A;;CCLCSWLOCRRC;;;SU)";

fn run_sc(args: &[&str]) {
    let _ = helper_command("sc").args(args).status();
}

fn apply_service_protection(name: &str) {
    run_sc(&["failure", name, "reset=", "0", "actions=", "restart/5000"]);
    run_sc(&["sdset", name, TAMPER_PROTECT_SDDL]);
}

fn reset_service_protection(name: &str) {
    run_sc(&["sdset", name, TAMPER_DEFAULT_SDDL]);
}

fn stop_and_delete_service(name: &str) {
    if hbb_common::config::is_tamper_protected() {
        reset_service_protection(name);
    }
    let manager = match ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT) {
        Ok(m) => m,
        Err(e) => {
            log::warn!("ServiceManager::local_computer failed: {}", e);
            return;
        }
    };
    let access = ServiceAccess::QUERY_STATUS | ServiceAccess::STOP | ServiceAccess::DELETE;
    let service = match manager.open_service(name, access) {
        Ok(s) => s,
        Err(_) => return,
    };
    if let Ok(status) = service.query_status() {
        if status.current_state != ServiceState::Stopped {
            let _ = service.stop();
            for _ in 0..20 {
                std::thread::sleep(Duration::from_millis(200));
                match service.query_status() {
                    Ok(s) if s.current_state == ServiceState::Stopped => break,
                    Ok(_) => continue,
                    Err(_) => break,
                }
            }
        }
    }
    let _ = service.delete();
    std::thread::sleep(Duration::from_millis(500));
}

fn create_and_start_service(
    name: &str,
    display_name: &str,
    exe: &str,
    args: &[&str],
) -> ResultType<()> {
    let manager = ServiceManager::local_computer(
        None::<&str>,
        ServiceManagerAccess::CONNECT | ServiceManagerAccess::CREATE_SERVICE,
    )?;
    let info = ServiceInfo {
        name: OsString::from(name),
        display_name: OsString::from(display_name),
        service_type: ServiceType::OWN_PROCESS,
        start_type: ServiceStartType::AutoStart,
        error_control: ServiceErrorControl::Normal,
        executable_path: PathBuf::from(exe),
        launch_arguments: args.iter().map(|a| OsString::from(*a)).collect(),
        dependencies: vec![],
        account_name: None,
        account_password: None,
    };
    let access = ServiceAccess::START | ServiceAccess::STOP | ServiceAccess::DELETE | ServiceAccess::QUERY_STATUS;
    let service = manager.create_service(&info, access)?;
    let empty: &[&OsStr] = &[];
    let _ = service.start(empty);
    if args.contains(&"--service") && hbb_common::config::is_tamper_protected() {
        apply_service_protection(name);
    }
    Ok(())
}

fn netsh_firewall_rule_exists(rule_name: &str) -> bool {
    match helper_command("netsh")
        .args(&[
            "advfirewall",
            "firewall",
            "show",
            "rule",
            &format!("name={}", rule_name),
        ])
        .stdout(std::process::Stdio::piped())
        .output()
    {
        Ok(o) => String::from_utf8_lossy(&o.stdout).contains(rule_name),
        Err(e) => {
            install_log(&format!("netsh show rule failed: {}", e));
            false
        }
    }
}

fn netsh_add_firewall_rule(rule_name: &str, exe: &str, direction: &str) {
    match helper_command("netsh")
        .args(&[
            "advfirewall",
            "firewall",
            "add",
            "rule",
            &format!("name={}", rule_name),
            &format!("dir={}", direction),
            "action=allow",
            &format!("program={}", exe),
            "enable=yes",
        ])
        .status()
    {
        Ok(st) if st.success() => {}
        Ok(st) => install_log(&format!("netsh add rule {} {} exited {}", rule_name, direction, st)),
        Err(e) => install_log(&format!("netsh add rule {} {} failed: {}", rule_name, direction, e)),
    }
}

fn taskkill_force(image_name: &str, exclude_current_pid: bool) {
    let mut cmd = helper_command("taskkill");
    cmd.args(&["/F", "/IM", image_name]);
    if exclude_current_pid {
        let filter = format!("PID ne {}", get_current_pid());
        cmd.args(&["/FI", &filter]);
    }
    if let Err(e) = cmd.status() {
        install_log(&format!("taskkill {} failed: {}", image_name, e));
    }
}

fn create_lnk_via_cscript(
    work_dir: &Path,
    vbs_stem: &str,
    target_lnk: &Path,
    target_exe: &str,
    arguments: &str,
    icon_location: &str,
) -> ResultType<()> {
    let _ = fs::create_dir_all(work_dir);
    let vbs_path = work_dir.join(format!("{}.vbs", vbs_stem));
    let lnk_intermediate = work_dir.join(format!("{}.lnk", vbs_stem));
    let mut script = format!(
        "Set oWS = WScript.CreateObject(\"WScript.Shell\")\r\nsLinkFile = \"{lnk}\"\r\nSet oLink = oWS.CreateShortcut(sLinkFile)\r\noLink.TargetPath = \"{exe}\"\r\n",
        lnk = lnk_intermediate.to_string_lossy(),
        exe = target_exe,
    );
    if !arguments.is_empty() {
        script.push_str(&format!("oLink.Arguments = \"{}\"\r\n", arguments));
    }
    if !icon_location.is_empty() {
        script.push_str(&format!("oLink.IconLocation = \"{}\"\r\n", icon_location));
    }
    script.push_str("oLink.Save\r\n");
    fs::write(&vbs_path, script.as_bytes())?;
    let status = helper_command("cscript")
        .arg("//Nologo")
        .arg(&vbs_path)
        .status();
    let _ = fs::remove_file(&vbs_path);
    if !lnk_intermediate.exists() {
        match status {
            Ok(st) => install_log(&format!("shortcut {} not created, cscript exited {}", vbs_stem, st)),
            Err(e) => install_log(&format!("shortcut {} not created, cscript failed: {}", vbs_stem, e)),
        }
    }
    if lnk_intermediate.exists() && lnk_intermediate != target_lnk {
        if let Some(parent) = target_lnk.parent() {
            let _ = fs::create_dir_all(parent);
        }
        if let Err(e) = fs::rename(&lnk_intermediate, target_lnk) {
            let _ = fs::copy(&lnk_intermediate, target_lnk);
            let _ = fs::remove_file(&lnk_intermediate);
            log::debug!("shortcut rename fallback: {}", e);
        }
    }
    Ok(())
}

fn install_log(msg: &str) {
    let log_path = Config::shared_path("install.log");
    if let Some(parent) = log_path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
    {
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let _ = writeln!(f, "[{}] {}", ts, msg);
    }
}

pub fn install_me(options: &str, path: String, silent: bool, _debug: bool, no_startup: bool) -> ResultType<()> {
    if !is_elevated(None)? {
        let mut arg = String::from("--silent-install");
        if !silent {
            arg.push_str(" --launch-after");
        }
        if no_startup {
            arg.push_str(" --no-startup");
        }
        arg.push_str(" --install-options ");
        arg.push_str(&cli_quote(options));
        arg.push_str(" --install-path ");
        arg.push_str(&cli_quote(&path));
        arg.push(' ');
        arg.push_str(ELEVATED_CHILD_ARG);
        let exe = std::env::current_exe()?.to_string_lossy().to_string();
        match run_uac(&exe, &arg) {
            Ok(true) => std::process::exit(0),
            Ok(false) => bail!("UAC elevation cancelled or denied"),
            Err(e) => bail!("Failed to request elevation: {}", e),
        }
    }
    let (subkey, _path, start_menu, exe) = get_default_install_info();
    let origin_process_exe = win_topmost_window::ORIGIN_PROCESS_EXE;
    let mut path = path.trim_end_matches('\\').to_owned();
    let mut exe = exe;
    if path.is_empty() {
        path = _path;
    } else {
        exe = exe.replace(&_path, &path);
    }
    let app_name = crate::get_app_name();
    install_log(&format!(
        "install_me start: options={:?} path={:?} silent={} no_startup={} src_exe={:?}",
        options,
        path,
        silent,
        no_startup,
        std::env::current_exe().ok()
    ));
    let broker_exe = rbexe();
    let config_path = Config::file().to_str().unwrap_or("").to_owned();
    let src_exe = std::env::current_exe()?;
    let tmp_path = std::env::temp_dir();
    let cur_dir = env::current_dir()?;

    let versions: Vec<&str> = crate::VERSION.split(".").collect();
    let version_major: u32 = versions.get(0).and_then(|s| s.parse().ok()).unwrap_or(0);
    let version_minor: u32 = versions.get(1).and_then(|s| s.parse().ok()).unwrap_or(0);
    let version_build: u32 = versions.get(2).and_then(|s| s.parse().ok()).unwrap_or(0);
    let size_kb: u32 = fs::symlink_metadata(&src_exe)
        .map(|m| (m.len() / 1024) as u32)
        .unwrap_or(0);

    stop_and_delete_service(&app_name);
    taskkill_force(&broker_exe, false);
    taskkill_force(&format!("{}.exe", app_name), true);
    std::thread::sleep(Duration::from_millis(500));
    let _ = fs::remove_file(tmp_path.join("PrivacyMode.dll"));
    let _ = fs::remove_file(tmp_path.join("privacyhelper.exe"));
    let _ = fs::remove_file(tmp_path.join(&broker_exe));
    crate::send_file::stop_menu_host();
    // Flutter build ships DLLs and a data\ folder next to the exe, so the
    // payload is the whole directory. When already running from the install
    // dir (e.g. invoked by an Inno Setup package), register in place without
    // touching the files.
    let in_place = src_exe
        .parent()
        .map(|p| p.to_string_lossy().trim_end_matches('\\').to_lowercase())
        == Some(path.trim_end_matches('\\').to_lowercase());
    if !in_place {
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path)?;

        let dst_exe = PathBuf::from(&exe);
        copy_if_exists(&src_exe, &dst_exe);
        if let Some(src_dir) = src_exe.parent() {
            if let Ok(entries) = fs::read_dir(src_dir) {
                for entry in entries.flatten() {
                    let name = entry.file_name().to_string_lossy().to_string();
                    let src_p = entry.path();
                    if src_p.is_file() {
                        if name.eq_ignore_ascii_case(&format!("{}.exe", app_name)) {
                            continue;
                        }
                        copy_if_exists(&src_p, &PathBuf::from(format!("{}\\{}", path, name)));
                    } else if src_p.is_dir() && name == "data" {
                        copy_dir_recursive(&src_p, &PathBuf::from(format!("{}\\{}", path, name)));
                    }
                }
            }
        }
    }

    let pm_dst = PathBuf::from(format!("{}\\PrivacyMode.dll", path));
    let ph_dst = PathBuf::from(format!("{}\\privacyhelper.exe", path));
    #[cfg(feature = "standalone")]
    {
        if let Err(e) = fs::write(&pm_dst, get_dllpm_bytes()) {
            log::warn!("install write PrivacyMode.dll failed: {}", e);
        }
        if let Err(e) = fs::write(&ph_dst, get_dllph_bytes()) {
            log::warn!("install write privacyhelper.exe failed: {}", e);
        }
    }
    #[cfg(not(feature = "standalone"))]
    {
        copy_if_exists(&cur_dir.join("PrivacyMode.dll"), &pm_dst);
        copy_if_exists(&cur_dir.join("privacyhelper.exe"), &ph_dst);
    }

    let broker_dst = PathBuf::from(format!("{}\\{}", path, broker_exe));
    copy_if_exists(&tmp_path.join(&broker_exe), &broker_dst);
    copy_if_exists(&PathBuf::from(origin_process_exe), &broker_dst);
    copy_if_exists(
        &PathBuf::from("C:\\Windows\\Sysnative\\RuntimeBroker.exe"),
        &broker_dst,
    );

    let local_service_cfg = format!(
        "C:\\Windows\\ServiceProfiles\\LocalService\\AppData\\Roaming\\{}\\config",
        app_name
    );
    let _ = fs::create_dir_all(&local_service_cfg);
    let shared_cfg = Config::shared_path("");
    if let Err(e) = secure_shared_config_dir() {
        install_log(&format!("failed to secure shared config dir: {}", e));
    }

    let resolve_source = |name: &str| -> Option<PathBuf> {
        let primary = Config::path(name);
        if primary.exists() {
            return Some(primary);
        }
        let shared = Config::shared_path(name);
        if shared.exists() {
            return Some(shared);
        }
        if let Ok(appdata) = std::env::var("APPDATA") {
            let legacy = PathBuf::from(format!("{}\\{}\\config\\{}", appdata, app_name, name));
            if legacy.exists() {
                return Some(legacy);
            }
        }
        None
    };

    for name in ["InviteCode.toml"] {
        if let Some(src) = resolve_source(name) {
            copy_if_exists(
                &src,
                &PathBuf::from(format!("{}\\{}", local_service_cfg, name)),
            );
            copy_if_exists(&src, &shared_cfg.join(name));
        }
    }
    let session_user_key = || {
        get_active_user_home()
            .map(|home| home.join(format!("AppData\\Roaming\\{}\\config\\Enrolled.key", app_name)))
            .filter(|p| p.exists())
    };
    let service_key = PathBuf::from(format!("{}\\Enrolled.key", local_service_cfg));
    let source_key = resolve_source("Enrolled.key")
        .or_else(|| if service_key.exists() { None } else { session_user_key() });
    if let Some(src) = source_key {
        copy_if_exists(&src, &service_key);
    }

    write_uninstall_reg(
        &subkey,
        &app_name,
        &exe,
        &path,
        crate::VERSION,
        version_major,
        version_minor,
        version_build,
        size_kb,
    )?;
    cleanup_stale_installs(&path);

    let work_dir = PathBuf::from(&path);
    let main_lnk = work_dir.join(format!("{}.lnk", app_name));
    create_lnk_via_cscript(&work_dir, "shortcut_main", &main_lnk, &exe, "", "")?;
    let uninstall_lnk = work_dir.join(format!("Uninstall {}.lnk", app_name));
    create_lnk_via_cscript(
        &work_dir,
        "shortcut_uninstall",
        &uninstall_lnk,
        &exe,
        "--uninstall",
        "msiexec.exe",
    )?;
    let tray_lnk = work_dir.join(format!("{} Tray.lnk", app_name));
    create_lnk_via_cscript(&work_dir, "shortcut_tray", &tray_lnk, &exe, "--tray", "")?;

    if !no_startup {
        copy_if_exists(
            &tray_lnk,
            &PathBuf::from(expand_env_vars(&format!(
                "%PROGRAMDATA%\\Microsoft\\Windows\\Start Menu\\Programs\\Startup\\{} Tray.lnk",
                app_name
            ))),
        );
    }
    if options.contains("desktopicon") {
        copy_if_exists(
            &main_lnk,
            &PathBuf::from(expand_env_vars(&format!(
                "%PUBLIC%\\Desktop\\{}.lnk",
                app_name
            ))),
        );
    }
    if options.contains("startmenu") {
        let sm = expand_env_vars(&start_menu);
        let _ = fs::create_dir_all(&sm);
        copy_if_exists(
            &main_lnk,
            &PathBuf::from(format!("{}\\{}.lnk", sm, app_name)),
        );
    }

    if !config::is_outgoing_only() {
        stop_and_delete_service(&app_name);
        let _ = create_and_start_service(
            &app_name,
            &format!("{} Service", app_name),
            &exe,
            &["--import-config", config_path.as_str()],
        );
        std::thread::sleep(Duration::from_millis(1500));
        stop_and_delete_service(&app_name);
    }

    if let Err(e) = write_after_install_reg(&app_name, &exe) {
        log::warn!("write_after_install_reg failed: {}", e);
    }

    if !config::is_outgoing_only() {
        let rule_name = format!("{} Service", app_name);
        if !netsh_firewall_rule_exists(&rule_name) {
            netsh_add_firewall_rule(&rule_name, &exe, "out");
            netsh_add_firewall_rule(&rule_name, &exe, "in");
        }
        match create_and_start_service(
            &app_name,
            &format!("{} Service", app_name),
            &exe,
            &["--service"],
        ) {
            Ok(_) => {
                install_log("service registered and started");
                if silent {
                    start_tray_after_install(&exe);
                }
            }
            Err(e) => {
                log::warn!("create service --service failed: {}", e);
                install_log(&format!("service start failed: {}", e));
            }
        }
    }

    std::thread::sleep(Duration::from_millis(2000));
    if !silent {
        // Detach stdio: inherited handles would make Inno Setup's
        // waituntilterminated block until the UI process exits.
        match std::process::Command::new(&exe)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
        {
            Ok(_) => log::info!("Successfully launched app after install: {}", exe),
            Err(e) => log::error!("Failed to launch app after install: {} - {}", exe, e),
        }
        std::thread::sleep(Duration::from_millis(300));
    }
    install_log("install_me done");
    Ok(())
}

pub fn run_after_install() -> ResultType<()> {
    let (_, _, _, exe) = get_install_info();
    run_cmds(get_after_install(&exe), true, "after_install")
}

pub fn run_before_uninstall() -> ResultType<()> {
    run_cmds(get_before_uninstall(true), false, "before_install")
}

fn get_before_uninstall(kill_self: bool) -> String {
    let app_name = crate::get_app_name();
    let ext = url_scheme(&app_name);
    let filter = if kill_self {
        "".to_string()
    } else {
        format!(" /FI \"PID ne {}\"", get_current_pid())
    };
    let reset_protection = if hbb_common::config::is_tamper_protected() {
        format!("    sc sdset \"{}\" {}\n", app_name, TAMPER_DEFAULT_SDDL)
    } else {
        String::new()
    };
    format!(
        "
    chcp 65001
{reset_protection}    sc stop \"{app_name}\"
    sc delete \"{app_name}\"
	taskkill /F /IM {broker_exe}
    taskkill /F /IM \"{app_name}.exe\"{filter}
    reg delete HKEY_CLASSES_ROOT\\.{ext} /f
    reg delete HKEY_CLASSES_ROOT\\{ext} /f
    reg delete \"HKEY_CLASSES_ROOT\\*\\shell\\{ext}sendfile\" /f
    reg delete \"HKEY_CLASSES_ROOT\\Directory\\shell\\{ext}sendfile\" /f
    reg delete \"HKEY_LOCAL_MACHINE\\SYSTEM\\CurrentControlSet\\Control\\SafeBoot\\Network\\{app_name}\" /f
    netsh advfirewall firewall delete rule name=\"{app_name} Service\"
    ",
        app_name = app_name,
		broker_exe = rbexe(),
        ext = ext,
        filter = filter,
        reset_protection = reset_protection,
    )
}

fn get_uninstall(kill_self: bool) -> String {
    let reg_uninstall_string = get_reg("UninstallString");
    if reg_uninstall_string.to_lowercase().contains("msiexec.exe") {
        return reg_uninstall_string;
    }
    let (subkey, path, start_menu, _) = get_install_info();
    let app_name = crate::get_app_name();
    let broker_exe = rbexe();
    format!(
        "
    {before_uninstall}
    {uninstall_idd}
    {remove_top_menu}
    reg delete \"{subkey}\" /f
	schtasks /delete /tn PrivacyHelper /f
    if exist \"{path}\" rd /s /q \"{path}\"
    if exist \"{start_menu}\" rd /s /q \"{start_menu}\"
    if exist \"%PUBLIC%\\Desktop\\{app_name}.lnk\" del /f /q \"%PUBLIC%\\Desktop\\{app_name}.lnk\"
    netsh advfirewall firewall delete rule name=\"{app_name} Direct TCP\" >nul 2>nul
    netsh advfirewall firewall delete rule name=\"{app_name} Discovery UDP\" >nul 2>nul
    if exist \"%PROGRAMDATA%\\Microsoft\\Windows\\Start Menu\\Programs\\Startup\\{app_name} Tray.lnk\" del /f /q \"%PROGRAMDATA%\\Microsoft\\Windows\\Start Menu\\Programs\\Startup\\{app_name} Tray.lnk\"
    if exist \"C:\\Windows\\ServiceProfiles\\LocalService\\AppData\\Roaming\\{app_name}\" rd /s /q \"C:\\Windows\\ServiceProfiles\\LocalService\\AppData\\Roaming\\{app_name}\"
    if exist \"%PROGRAMDATA%\\{app_name}\" rd /s /q \"%PROGRAMDATA%\\{app_name}\"
    if exist \"%TEMP%\\PrivacyMode.dll\" del /f /q \"%TEMP%\\PrivacyMode.dll\"
    if exist \"%TEMP%\\privacyhelper.exe\" del /f /q \"%TEMP%\\privacyhelper.exe\"
    if exist \"%TEMP%\\{broker_exe}\" del /f /q \"%TEMP%\\{broker_exe}\"
    ",
        before_uninstall=get_before_uninstall(kill_self),
        uninstall_idd=get_uninstall_htd_idd(),
        remove_top_menu=crate::send_file::top_menu_remove_cmds(),
        subkey=subkey,
        app_name = app_name,
        broker_exe = broker_exe,
        path = path,
        start_menu = start_menu,
    )
}

pub fn uninstall_me(kill_self: bool) -> ResultType<()> {
    run_cmds(get_uninstall(kill_self), false, "uninstall")
}

fn write_cmds(cmds: String, ext: &str, tip: &str) -> ResultType<std::path::PathBuf> {
    let mut cmds = cmds;
    let mut tmp = std::env::temp_dir();
    // When dir contains these characters, the bat file will not execute in elevated mode.
    if vec!["&", "@", "^"]
        .drain(..)
        .any(|s| tmp.to_string_lossy().to_string().contains(s))
    {
        if let Ok(dir) = user_accessible_folder() {
            tmp = dir;
        }
    }
    tmp.push(format!(
        "{}_{}_{}.{}",
        crate::get_app_name(),
        tip,
        uuid::Uuid::new_v4().simple(),
        ext
    ));
    let mut file = hbb_common::fs::create_private_file(&tmp)?;
    if ext == "bat" {
        let tmp2 = get_undone_file(&tmp)?;
        std::fs::File::create(&tmp2).ok();
        cmds = format!(
            "
{cmds}
if exist \"{path}\" del /f /q \"{path}\"
",
            path = tmp2.to_string_lossy()
        );
    }
    // in case cmds mixed with \r\n and \n, make sure all ending with \r\n
    // in some windows, \r\n required for cmd file to run
    cmds = cmds.replace("\r\n", "\n").replace("\n", "\r\n");
    if ext == "vbs" {
        let mut v: Vec<u16> = cmds.encode_utf16().collect();
        // utf8 -> utf16le which vbs support it only
        file.write_all(to_le(&mut v))?;
    } else {
        file.write_all(cmds.as_bytes())?;
    }
    file.sync_all()?;
    return Ok(tmp);
}

fn to_le(v: &mut [u16]) -> &[u8] {
    for b in v.iter_mut() {
        *b = b.to_le()
    }
    unsafe { v.align_to().1 }
}

fn get_undone_file(tmp: &PathBuf) -> ResultType<PathBuf> {
    let mut tmp1 = tmp.clone();
    tmp1.set_file_name(format!(
        "{}.undone",
        tmp.file_name()
            .ok_or(anyhow!("Failed to get filename of {:?}", tmp))?
            .to_string_lossy()
    ));
    Ok(tmp1)
}

pub fn run_cmds(cmds: String, show: bool, tip: &str) -> ResultType<()> {
    let tmp = write_cmds(cmds, "bat", tip)?;
    let tmp_fn = tmp.to_str().unwrap_or("");
    let res = runas::Command::new("cmd.exe")
        .args(&["/C", &tmp_fn])
        .show(show)
        .force_prompt(true)
        .status();
    let _ = fs::remove_file(&tmp);
    let _ = res?;
    Ok(())
}

pub fn block_input(v: bool) -> (bool, String) {
    let v = if v { TRUE } else { FALSE };
    unsafe {
        if BlockInput(v) == TRUE {
            (true, "".to_owned())
        } else {
            (false, format!("Error: {}", io::Error::last_os_error()))
        }
    }
}

pub fn add_recent_document(path: &str) {
    extern "C" {
        fn AddRecentDocument(path: *const u16);
    }
    use std::os::windows::ffi::OsStrExt;
    let wstr: Vec<u16> = std::ffi::OsStr::new(path)
        .encode_wide()
        .chain(Some(0).into_iter())
        .collect();
    let wstr = wstr.as_ptr();
    unsafe {
        AddRecentDocument(wstr);
    }
}

pub fn is_installed() -> bool {
    let (_, _, _, exe) = get_install_info();
    std::fs::metadata(exe).is_ok()
}

/// Beacon single-instance support: bring the already-running main window
/// back to the foreground. The main window may be hidden (closed to tray) or
/// minimized, so restore it explicitly.
pub fn show_existing_main_window() {
    let title = crate::get_app_name();
    unsafe {
        let window_name = wide_string(&title);
        let hwnd = FindWindowW(std::ptr::null(), window_name.as_ptr());
        if hwnd.is_null() {
            return;
        }
        if IsIconic(hwnd) != 0 {
            ShowWindow(hwnd, SW_RESTORE);
        } else {
            ShowWindow(hwnd, SW_SHOW);
        }
        // Attach to the foreground thread so SetForegroundWindow is allowed
        // to take focus instead of just flashing the taskbar button.
        let fg = GetForegroundWindow();
        let cur_tid = GetCurrentThreadId();
        let fg_tid = if fg.is_null() {
            0
        } else {
            GetWindowThreadProcessId(fg, std::ptr::null_mut())
        };
        let target_tid = GetWindowThreadProcessId(hwnd, std::ptr::null_mut());
        if fg_tid != 0 && fg_tid != cur_tid {
            AttachThreadInput(cur_tid, fg_tid, 1);
        }
        if target_tid != 0 && target_tid != cur_tid {
            AttachThreadInput(cur_tid, target_tid, 1);
        }
        SetForegroundWindow(hwnd);
        if target_tid != 0 && target_tid != cur_tid {
            AttachThreadInput(cur_tid, target_tid, 0);
        }
        if fg_tid != 0 && fg_tid != cur_tid {
            AttachThreadInput(cur_tid, fg_tid, 0);
        }
    }
}

pub fn get_run_at_startup() -> bool {
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    if let Ok(run) = hkcu.open_subkey(r"Software\Microsoft\Windows\CurrentVersion\Run") {
        return run
            .get_value::<String, _>(crate::get_app_name())
            .is_ok();
    }
    false
}

pub fn set_run_at_startup(enabled: bool) {
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let name = crate::get_app_name();
    match hkcu.create_subkey(r"Software\Microsoft\Windows\CurrentVersion\Run") {
        Ok((run, _)) => {
            if enabled {
                match std::env::current_exe() {
                    Ok(exe) => {
                        let cmd = format!("\"{}\"", exe.display());
                        if let Err(e) = run.set_value(name, &cmd) {
                            log::error!("set_run_at_startup failed: {}", e);
                        }
                    }
                    Err(e) => log::error!("set_run_at_startup current_exe failed: {}", e),
                }
            } else {
                let _ = run.delete_value(name);
            }
        }
        Err(e) => log::error!("set_run_at_startup open Run key failed: {}", e),
    }
}

pub fn installed_shortcut_options() -> Option<String> {
    let (_, _, start_menu, exe) = get_install_info();
    let installed_exe = std::fs::canonicalize(&exe).ok()?;
    let current_exe = std::env::current_exe().and_then(std::fs::canonicalize).ok()?;
    if current_exe == installed_exe {
        return None;
    }
    let app_name = crate::get_app_name();
    let desktop_lnk = expand_env_vars(&format!("%PUBLIC%\\Desktop\\{}.lnk", app_name));
    let start_menu_lnk = format!("{}\\{}.lnk", expand_env_vars(&start_menu), app_name);
    let mut options = Vec::new();
    if Path::new(&desktop_lnk).exists() {
        options.push("desktopicon");
    }
    if Path::new(&start_menu_lnk).exists() {
        options.push("startmenu");
    }
    Some(options.join(" "))
}

pub fn get_installed_version() -> String {
    let (_, _, _, exe) = get_install_info();
    if let Ok(output) = std::process::Command::new(exe).arg("--version").output() {
        for line in String::from_utf8_lossy(&output.stdout).lines() {
            return line.to_owned();
        }
    }
    "".to_owned()
}

fn get_reg(name: &str) -> String {
    let (subkey, _, _, _) = get_install_info();
    get_reg_of(&subkey, name)
}

fn get_reg_of(subkey: &str, name: &str) -> String {
    let hklm = RegKey::predef(HKEY_LOCAL_MACHINE);
    if let Ok(tmp) = hklm.open_subkey(subkey.replace("HKEY_LOCAL_MACHINE\\", "")) {
        if let Ok(v) = tmp.get_value(name) {
            return v;
        }
    }
    "".to_owned()
}

#[inline]
pub fn is_win_server() -> bool {
    unsafe { is_windows_server() > 0 }
}

#[inline]
pub fn is_win_10_or_greater() -> bool {
    unsafe { is_windows_10_or_greater() > 0 }
}

pub fn is_win7() -> bool {
    !hbb_common::platform::windows::is_windows_version_or_greater(6, 2, 0, 0, 0)
}

pub fn is_dwm_composition_enabled() -> bool {
    #[link(name = "dwmapi")]
    extern "system" {
        fn DwmIsCompositionEnabled(enabled: *mut BOOL) -> winapi::shared::ntdef::HRESULT;
    }
    let mut enabled: BOOL = FALSE;
    unsafe { DwmIsCompositionEnabled(&mut enabled) == 0 && enabled != FALSE }
}
pub fn create_shortcut(id: &str) -> ResultType<()> {
    if !crate::common::is_valid_untrusted_peer_id(id) {
        bail!("Invalid peer id for shortcut");
    }
    let exe = std::env::current_exe()?.to_str().unwrap_or("").to_owned();
    let filename = id.replace(':', "_");
    let shortcut = write_cmds(
        format!(
            "
Set oWS = WScript.CreateObject(\"WScript.Shell\")
strDesktop = oWS.SpecialFolders(\"Desktop\")
Set objFSO = CreateObject(\"Scripting.FileSystemObject\")
sLinkFile = objFSO.BuildPath(strDesktop, \"{filename}.lnk\")
Set oLink = oWS.CreateShortcut(sLinkFile)
    oLink.TargetPath = \"{exe}\"
    oLink.Arguments = \"--connect {id}\"
oLink.Save
        ",
            exe = exe,
            id = id,
        ),
        "vbs",
        "connect_shortcut",
    )?
    .to_str()
    .unwrap_or("")
    .to_owned();
    std::process::Command::new("cscript")
        .arg(&shortcut)
        .creation_flags(CREATE_NO_WINDOW)
        .output()?;
    allow_err!(std::fs::remove_file(shortcut));
    Ok(())
}

pub fn enable_lowlevel_keyboard(hwnd: HWND) {
    let ret = unsafe { win32_enable_lowlevel_keyboard(hwnd) };
    if ret != 0 {
        log::error!("Failure grabbing keyboard");
        return;
    }
}

pub fn disable_lowlevel_keyboard(hwnd: HWND) {
    unsafe { win32_disable_lowlevel_keyboard(hwnd) };
}

pub fn stop_system_key_propagate(v: bool) {
    unsafe { win_stop_system_key_propagate(if v { TRUE } else { FALSE }) };
}

pub fn get_win_key_state() -> bool {
    unsafe { is_win_down() == TRUE }
}

pub fn quit_gui() {
    std::process::exit(0);
}
pub fn run_background(exe: &str, arg: &str) -> ResultType<bool> {
    let wexe = wide_string(exe);
    let warg;
    unsafe {
        let ret = ShellExecuteW(
            NULL as _,
            NULL as _,
            wexe.as_ptr() as _,
            if arg.is_empty() {
                NULL as _
            } else {
                warg = wide_string(arg);
                warg.as_ptr() as _
            },
            NULL as _,
            SW_HIDE,
        );
        return Ok(ret as i32 > 32);
    }
}

pub fn run_uac(exe: &str, arg: &str) -> ResultType<bool> {
    let wop = wide_string("runas");
    let wexe = wide_string(exe);
    let warg;
    unsafe {
        let ret = ShellExecuteW(
            NULL as _,
            wop.as_ptr() as _,
            wexe.as_ptr() as _,
            if arg.is_empty() {
                NULL as _
            } else {
                warg = wide_string(arg);
                warg.as_ptr() as _
            },
            NULL as _,
            SW_SHOWNORMAL,
        );
        return Ok(ret as i32 > 32);
    }
}

pub fn run_uac_wait(exe: &str, arg: &str) -> ResultType<i32> {
    let wop = wide_string("runas");
    let wexe = wide_string(exe);
    let warg = wide_string(arg);
    unsafe {
        let mut sei: SHELLEXECUTEINFOW = mem::zeroed();
        sei.cbSize = mem::size_of::<SHELLEXECUTEINFOW>() as u32;
        sei.fMask = SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NOASYNC;
        sei.lpVerb = wop.as_ptr();
        sei.lpFile = wexe.as_ptr();
        sei.lpParameters = warg.as_ptr();
        sei.nShow = SW_SHOWNORMAL;
        if ShellExecuteExW(&mut sei) == FALSE {
            bail!("elevation request failed: {}", io::Error::last_os_error());
        }
        if sei.hProcess.is_null() {
            bail!("elevation request returned no process");
        }
        WaitForSingleObject(sei.hProcess, INFINITE);
        let mut code: DWORD = 1;
        GetExitCodeProcess(sei.hProcess, &mut code);
        CloseHandle(sei.hProcess);
        Ok(code as i32)
    }
}

pub fn relaunch_install_elevated(args: &[String]) -> i32 {
    if args.iter().any(|a| a == ELEVATED_CHILD_ARG) {
        install_log("installation needs administrator rights; the elevated relaunch was not elevated");
        log::error!("Installation needs administrator rights; the elevated relaunch was not elevated");
        return 3;
    }
    let exe = match std::env::current_exe() {
        Ok(p) => p.to_string_lossy().to_string(),
        Err(e) => {
            install_log(&format!("cannot resolve own path for elevation: {}", e));
            return 1;
        }
    };
    let mut forwarded: Vec<String> = args.iter().map(|a| cli_quote(a)).collect();
    forwarded.push(ELEVATED_CHILD_ARG.to_owned());
    match run_uac_wait(&exe, &forwarded.join(" ")) {
        Ok(code) => code,
        Err(e) => {
            install_log(&format!("elevation failed: {}", e));
            log::error!("Elevation failed: {}", e);
            1
        }
    }
}

pub fn sanitize_std_handles() {
    use winapi::um::processenv::{GetStdHandle, SetStdHandle};
    for id in [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
        unsafe {
            let h = GetStdHandle(id);
            if h.is_null() || h == INVALID_HANDLE_VALUE {
                continue;
            }
            let mut flags: DWORD = 0;
            if GetHandleInformation(h, &mut flags) == FALSE {
                SetStdHandle(id, NULL as _);
            }
        }
    }
}

fn start_tray_after_install(exe: &str) {
    let launched = if is_root() {
        let session = match get_current_process_session_id() {
            Some(id) if id != 0 => Some(id),
            _ => active_console_session_id(),
        };
        match session {
            Some(id) => run_exe_in_session(exe, vec!["--tray"], id).map(|_| ()),
            None => Err(anyhow!("no interactive session")),
        }
    } else {
        std::process::Command::new(exe)
            .arg("--tray")
            .spawn()
            .map(|_| ())
            .map_err(|e| e.into())
    };
    match launched {
        Ok(()) => install_log("tray started"),
        Err(e) => install_log(&format!("tray start failed: {}", e)),
    }
}

pub fn run_uac_hide(exe: &str, arg: &str) -> ResultType<bool> {
    let wop = wide_string("runas");
    let wexe = wide_string(exe);
    let warg;
    unsafe {
        let ret = ShellExecuteW(
            NULL as _,
            wop.as_ptr() as _,
            wexe.as_ptr() as _,
            if arg.is_empty() {
                NULL as _
            } else {
                warg = wide_string(arg);
                warg.as_ptr() as _
            },
            NULL as _,
            SW_HIDE,
        );
        return Ok(ret as i32 > 32);
    }
}

pub fn check_super_user_permission() -> ResultType<bool> {
    run_uac(
        std::env::current_exe()?
            .to_string_lossy()
            .to_string()
            .as_str(),
        "--version",
    )
}

pub fn elevate(arg: &str) -> ResultType<bool> {
    run_uac(
        std::env::current_exe()?
            .to_string_lossy()
            .to_string()
            .as_str(),
        arg,
    )
}

pub fn run_as_system(arg: &str) -> ResultType<()> {
    let exe = std::env::current_exe()?.to_string_lossy().to_string();
    if impersonate_system::run_as_system(&exe, arg).is_err() {
        bail!(format!("Failed to run {} as system", exe));
    }
    Ok(())
}

pub fn elevate_or_run_as_system(is_setup: bool, is_elevate: bool, is_run_as_system: bool) {
    // avoid possible run recursively due to failed run.
    log::info!(
        "elevate:{}->{:?}, run_as_system:{}->{}",
        is_elevate,
        is_elevated(None),
        is_run_as_system,
        crate::username(),
    );
    let arg_elevate = if is_setup {
        "--noinstall --elevate"
    } else {
        "--elevate"
    };
    let arg_run_as_system = if is_setup {
        "--noinstall --run-as-system"
    } else {
        "--run-as-system"
    };
    if is_root() {
        if is_run_as_system {
            log::info!("run portable service");
            crate::portable_service::server::run_portable_service();
        }
    } else {
        match is_elevated(None) {
            Ok(elevated) => {
                if elevated {
                    if !is_run_as_system {
                        if run_as_system(arg_run_as_system).is_ok() {
                            std::process::exit(0);
                        } else {
                            log::error!(
                                "Failed to run as system, error {}",
                                io::Error::last_os_error()
                            );
                        }
                    }
                } else {
                    if !is_elevate {
                        if let Ok(true) = elevate(arg_elevate) {
                            std::process::exit(0);
                        } else {
                            log::error!("Failed to elevate, error {}", io::Error::last_os_error());
                        }
                    }
                }
            }
            Err(_) => log::error!(
                "Failed to get elevation status, error {}",
                io::Error::last_os_error()
            ),
        }
    }
}

pub fn is_elevated(process_id: Option<DWORD>) -> ResultType<bool> {
    use hbb_common::platform::windows::RAIIHandle;
    unsafe {
        let handle: HANDLE = match process_id {
            Some(process_id) => OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, FALSE, process_id),
            None => GetCurrentProcess(),
        };
        if handle == NULL {
            bail!(
                "Failed to open process, error {}",
                io::Error::last_os_error()
            )
        }
        let _handle = RAIIHandle(handle);
        let mut token: HANDLE = mem::zeroed();
        if OpenProcessToken(handle, TOKEN_QUERY, &mut token) == FALSE {
            bail!(
                "Failed to open process token, error {}",
                io::Error::last_os_error()
            )
        }
        let _token = RAIIHandle(token);
        let mut token_elevation: TOKEN_ELEVATION = mem::zeroed();
        let mut size: DWORD = 0;
        if GetTokenInformation(
            token,
            TokenElevation,
            (&mut token_elevation) as *mut _ as *mut c_void,
            mem::size_of::<TOKEN_ELEVATION>() as _,
            &mut size,
        ) == FALSE
        {
            bail!(
                "Failed to get token information, error {}",
                io::Error::last_os_error()
            )
        }

        Ok(token_elevation.TokenIsElevated != 0)
    }
}

static SESSION_WINDOW: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

pub fn remember_session_window() {
    if SESSION_WINDOW.load(Ordering::Relaxed) != 0 {
        return;
    }
    unsafe {
        let hwnd = GetForegroundWindow();
        if hwnd.is_null() {
            return;
        }
        let mut process_id: DWORD = 0;
        GetWindowThreadProcessId(hwnd, &mut process_id);
        if process_id != 0 && process_id == GetCurrentProcessId() {
            SESSION_WINDOW.store(root_owner(hwnd) as usize, Ordering::Relaxed);
        }
    }
}

unsafe fn root_owner(hwnd: HWND) -> HWND {
    let root = GetAncestor(hwnd, GA_ROOTOWNER);
    if root.is_null() {
        hwnd
    } else {
        root
    }
}

unsafe fn is_session_window(hwnd: HWND, process_id: DWORD) -> bool {
    if process_id == 0 || process_id != GetCurrentProcessId() {
        return false;
    }
    let session = SESSION_WINDOW.load(std::sync::atomic::Ordering::Relaxed);
    session == 0 || root_owner(hwnd) as usize == session
}

pub fn is_foreground_window_ours() -> bool {
    unsafe {
        let hwnd = GetForegroundWindow();
        if hwnd.is_null() {
            return false;
        }
        let mut process_id: DWORD = 0;
        GetWindowThreadProcessId(hwnd, &mut process_id);
        is_session_window(hwnd, process_id)
    }
}

pub fn is_pointer_over_our_window() -> bool {
    let pos = match get_cursor_pos() {
        Some(pos) => pos,
        None => return false,
    };
    unsafe {
        let hwnd = WindowFromPoint(POINT { x: pos.0, y: pos.1 });
        if hwnd.is_null() {
            return false;
        }
        let mut process_id: DWORD = 0;
        GetWindowThreadProcessId(hwnd, &mut process_id);
        is_session_window(hwnd, process_id)
    }
}

pub fn is_foreground_window_elevated() -> ResultType<bool> {
    unsafe {
        let mut process_id: DWORD = 0;
        GetWindowThreadProcessId(GetForegroundWindow(), &mut process_id);
        if process_id == 0 {
            bail!(
                "Failed to get processId, error {}",
                io::Error::last_os_error()
            )
        }
        is_elevated(Some(process_id))
    }
}

fn get_current_pid() -> u32 {
    unsafe { GetCurrentProcessId() }
}

pub fn get_double_click_time() -> u32 {
    unsafe { GetDoubleClickTime() }
}

pub fn wide_string(s: &str) -> Vec<u16> {
    use std::os::windows::prelude::OsStrExt;
    std::ffi::OsStr::new(s)
        .encode_wide()
        .chain(Some(0).into_iter())
        .collect()
}

pub fn cli_quote(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    let mut i = 0;
    while i < chars.len() {
        let mut bs = 0;
        while i < chars.len() && chars[i] == '\\' {
            bs += 1;
            i += 1;
        }
        if i == chars.len() {
            for _ in 0..(bs * 2) {
                out.push('\\');
            }
        } else if chars[i] == '"' {
            for _ in 0..(bs * 2 + 1) {
                out.push('\\');
            }
            out.push('"');
            i += 1;
        } else {
            for _ in 0..bs {
                out.push('\\');
            }
            out.push(chars[i]);
            i += 1;
        }
    }
    out.push('"');
    out
}

/// send message to currently shown window
pub fn send_message_to_hnwd(
    class_name: &str,
    window_name: &str,
    dw_data: usize,
    data: &str,
    show_window: bool,
) -> bool {
    unsafe {
        let class_name_utf16 = wide_string(class_name);
        let window_name_utf16 = wide_string(window_name);
        let window = FindWindowW(class_name_utf16.as_ptr(), window_name_utf16.as_ptr());
        if window.is_null() {
            log::warn!("no such window {}:{}", class_name, window_name);
            return false;
        }
        let mut data_struct = COPYDATASTRUCT::default();
        data_struct.dwData = dw_data;
        let mut data_zero: String = data.chars().chain(Some('\0').into_iter()).collect();
        println!("send {:?}", data_zero);
        data_struct.cbData = data_zero.len() as _;
        data_struct.lpData = data_zero.as_mut_ptr() as _;
        SendMessageW(
            window,
            WM_COPYDATA,
            0,
            &data_struct as *const COPYDATASTRUCT as _,
        );
        if show_window {
            ShowWindow(window, SW_NORMAL);
            SetForegroundWindow(window);
        }
    }
    return true;
}
pub fn create_process_with_logon(user: &str, pwd: &str, exe: &str, arg: &str) -> ResultType<()> {
    unsafe {
        let user_split = user.split("\\").collect::<Vec<&str>>();
        let wuser = wide_string(user_split.get(1).unwrap_or(&user));
        let wpc = wide_string(user_split.get(0).unwrap_or(&""));
        let wpwd = wide_string(pwd);
        let cmd = if arg.is_empty() {
            format!("\"{}\"", exe)
        } else {
            format!("\"{}\" {}", exe, arg)
        };
        let mut wcmd = wide_string(&cmd);
        let mut si: STARTUPINFOW = mem::zeroed();
        si.wShowWindow = SW_HIDE as _;
        si.lpDesktop = NULL as _;
        si.cb = std::mem::size_of::<STARTUPINFOW>() as _;
        si.dwFlags = STARTF_USESHOWWINDOW;
        let mut pi: PROCESS_INFORMATION = mem::zeroed();
        let wexe = wide_string(exe);
        if FALSE
            == CreateProcessWithLogonW(
                wuser.as_ptr(),
                wpc.as_ptr(),
                wpwd.as_ptr(),
                LOGON_WITH_PROFILE,
                wexe.as_ptr(),
                wcmd.as_mut_ptr(),
                CREATE_UNICODE_ENVIRONMENT,
                NULL,
                NULL as _,
                &mut si as *mut STARTUPINFOW,
                &mut pi as *mut PROCESS_INFORMATION,
            )
        {
            bail!("CreateProcessWithLogonW failed: {}", io::Error::last_os_error());
        }
    }
    return Ok(());
}

pub fn set_path_permission(dir: &Path, permission: &str) -> ResultType<()> {
    std::process::Command::new("icacls")
        .arg(dir.as_os_str())
        .arg("/grant")
        .arg(format!("*S-1-1-0:(OI)(CI){}", permission))
        .arg("/T")
        .spawn()?;
    Ok(())
}

pub fn restrict_dir_to_system_admins(dir: &Path) -> ResultType<()> {
    let dir = dir.to_string_lossy().trim_end_matches('\\').to_owned();
    let children = format!("{}\\*", dir);
    let steps: [&[&str]; 3] = [
        &["takeown", "/F", &dir, "/A", "/R"],
        &[
            "icacls",
            &dir,
            "/inheritance:r",
            "/grant:r",
            "*S-1-5-18:(OI)(CI)(F)",
            "*S-1-5-32-544:(OI)(CI)(F)",
            "*S-1-5-32-545:(OI)(CI)(RX)",
        ],
        &["icacls", &children, "/reset", "/T", "/C"],
    ];
    let mut errors = Vec::new();
    for step in steps {
        let output = std::process::Command::new(step[0])
            .args(&step[1..])
            .creation_flags(CREATE_NO_WINDOW)
            .output()?;
        if !output.status.success() {
            errors.push(format!(
                "{}: {}",
                step[0],
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
    }
    if !errors.is_empty() {
        bail!("securing {} failed: {}", dir, errors.join("; "));
    }
    Ok(())
}

pub fn secure_shared_config_dir() -> ResultType<()> {
    let dir = Config::shared_path("");
    fs::create_dir_all(&dir)?;
    restrict_dir_to_system_admins(&dir)
}

#[inline]
fn str_to_device_name(name: &str) -> [u16; 32] {
    let mut device_name: Vec<u16> = wide_string(name);
    if device_name.len() < 32 {
        device_name.resize(32, 0);
    }
    let mut result = [0; 32];
    result.copy_from_slice(&device_name[..32]);
    result
}

pub fn resolutions(name: &str) -> Vec<Resolution> {
    unsafe {
        let mut dm: DEVMODEW = std::mem::zeroed();
        let mut v = vec![];
        let mut num = 0;
        let device_name = str_to_device_name(name);
        loop {
            if EnumDisplaySettingsW(device_name.as_ptr(), num, &mut dm) == 0 {
                break;
            }
            let r = Resolution {
                width: dm.dmPelsWidth as _,
                height: dm.dmPelsHeight as _,
                ..Default::default()
            };
            if !v.contains(&r) {
                v.push(r);
            }
            num += 1;
        }
        v
    }
}

pub fn current_resolution(name: &str) -> ResultType<Resolution> {
    let device_name = str_to_device_name(name);
    unsafe {
        let mut dm: DEVMODEW = std::mem::zeroed();
        dm.dmSize = std::mem::size_of::<DEVMODEW>() as _;
        if EnumDisplaySettingsW(device_name.as_ptr(), ENUM_CURRENT_SETTINGS, &mut dm) == 0 {
            bail!(
                "failed to get current resolution, error {}",
                io::Error::last_os_error()
            );
        }
        let r = Resolution {
            width: dm.dmPelsWidth as _,
            height: dm.dmPelsHeight as _,
            ..Default::default()
        };
        Ok(r)
    }
}

pub fn current_refresh_rate(name: &str) -> u32 {
    let device_name = str_to_device_name(name);
    unsafe {
        let mut dm: DEVMODEW = std::mem::zeroed();
        dm.dmSize = std::mem::size_of::<DEVMODEW>() as _;
        if EnumDisplaySettingsW(device_name.as_ptr(), ENUM_CURRENT_SETTINGS, &mut dm) == 0 {
            return 0;
        }
        dm.dmDisplayFrequency
    }
}

pub fn restore_display_mode(name: &str, width: usize, height: usize, hz: u32) -> ResultType<()> {
    let cur = current_resolution(name)?;
    let same_size = cur.width as usize == width && cur.height as usize == height;
    if same_size && (hz <= 1 || current_refresh_rate(name) == hz) {
        return Ok(());
    }
    set_display_mode(name, width, height, hz)
}

pub(super) fn change_resolution_directly(
    name: &str,
    width: usize,
    height: usize,
) -> ResultType<()> {
    set_display_mode(name, width, height, 0)
}

fn set_display_mode(name: &str, width: usize, height: usize, hz: u32) -> ResultType<()> {
    let device_name = str_to_device_name(name);
    unsafe {
        let mut dm: DEVMODEW = std::mem::zeroed();
        dm.dmSize = std::mem::size_of::<DEVMODEW>() as _;
        dm.dmPelsWidth = width as _;
        dm.dmPelsHeight = height as _;
        dm.dmFields = DM_PELSHEIGHT | DM_PELSWIDTH;
        if hz > 1 {
            dm.dmDisplayFrequency = hz;
            dm.dmFields |= DM_DISPLAYFREQUENCY;
        }
        let res = ChangeDisplaySettingsExW(
            device_name.as_ptr(),
            &mut dm,
            NULL as _,
            CDS_UPDATEREGISTRY | CDS_GLOBAL | CDS_RESET,
            NULL,
        );
        if res != DISP_CHANGE_SUCCESSFUL {
            bail!(
                "ChangeDisplaySettingsExW failed, res={}, error {}",
                res,
                io::Error::last_os_error()
            );
        }
        Ok(())
    }
}

pub fn user_accessible_folder() -> ResultType<PathBuf> {
    let disk = std::env::var("SystemDrive").unwrap_or("C:".to_string());
    let dir1 = PathBuf::from(format!("{}\\ProgramData", disk));
    // NOTICE: "C:\Windows\Temp" requires permanent authorization.
    let dir2 = PathBuf::from(format!("{}\\Windows\\Temp", disk));
    let dir;
    if dir1.exists() {
        dir = dir1;
    } else if dir2.exists() {
        dir = dir2;
    } else {
        bail!("no vaild user accessible folder");
    }
    Ok(dir)
}

#[inline]
pub fn get_char_from_vk(vk: u32) -> Option<char> {
    get_char_from_unicode(get_unicode_from_vk(vk)?)
}

pub fn get_char_from_unicode(unicode: u16) -> Option<char> {
    let buff = [unicode];
    if let Some(chr) = String::from_utf16(&buff[..1]).ok()?.chars().next() {
        if chr.is_control() {
            return None;
        } else {
            Some(chr)
        }
    } else {
        None
    }
}

pub fn get_unicode_from_vk(vk: u32) -> Option<u16> {
    const BUF_LEN: i32 = 32;
    let mut buff = [0_u16; BUF_LEN as usize];
    let buff_ptr = buff.as_mut_ptr();
    let len = unsafe {
        let current_window_thread_id = GetWindowThreadProcessId(GetForegroundWindow(), null_mut());
        let layout = GetKeyboardLayout(current_window_thread_id);

        // refs: https://github.com/fufesou/rdev/blob/25a99ce71ab42843ad253dd51e6a35e83e87a8a4/src/windows/keyboard.rs#L115
        let press_state = 129;
        let mut state: [BYTE; 256] = [0; 256];
        let shift_left = rdev::get_modifier(rdev::Key::ShiftLeft);
        let shift_right = rdev::get_modifier(rdev::Key::ShiftRight);
        if shift_left {
            state[VK_LSHIFT as usize] = press_state;
        }
        if shift_right {
            state[VK_RSHIFT as usize] = press_state;
        }
        if shift_left || shift_right {
            state[VK_SHIFT as usize] = press_state;
        }
        ToUnicodeEx(vk, 0x00, &state as _, buff_ptr, BUF_LEN, 0, layout)
    };
    if len == 1 {
        Some(buff[0])
    } else {
        None
    }
}

pub fn is_process_consent_running() -> ResultType<bool> {
    use hbb_common::platform::windows::RAIIHandle;
    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snapshot.is_null() {
            return Ok(false);
        }
        let _snapshot = RAIIHandle(snapshot);

        let mut pe32: PROCESSENTRY32 = mem::zeroed();
        pe32.dwSize = mem::size_of::<PROCESSENTRY32>() as DWORD;

        if Process32First(snapshot, &mut pe32) != FALSE {
            loop {
                let exe_name = std::ffi::CStr::from_ptr(pe32.szExeFile.as_ptr())
                    .to_string_lossy()
                    .into_owned();
                if exe_name.to_lowercase() == "consent.exe" {
                    return Ok(true);
                }
                if Process32Next(snapshot, &mut pe32) == FALSE {
                    break;
                }
            }
        }
        Ok(false)
    }
}

pub fn is_exe_running(exe_name: &str) -> ResultType<bool> {
    use hbb_common::platform::windows::RAIIHandle;
    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snapshot.is_null() {
            return Ok(false);
        }
        let _snapshot = RAIIHandle(snapshot);

        let mut pe32: PROCESSENTRY32 = mem::zeroed();
        pe32.dwSize = mem::size_of::<PROCESSENTRY32>() as DWORD;

        if Process32First(snapshot, &mut pe32) != FALSE {
            loop {
                let current_exe = std::ffi::CStr::from_ptr(pe32.szExeFile.as_ptr())
                    .to_string_lossy()
                    .into_owned();
                if current_exe.to_lowercase() == exe_name.to_lowercase() {
                    return Ok(true);
                }
                if Process32Next(snapshot, &mut pe32) == FALSE {
                    break;
                }
            }
        }
        Ok(false)
    }
}

pub struct WakeLock(u32);
// Failed to compile keepawake-rs on i686
impl WakeLock {
    pub fn new(display: bool, idle: bool, sleep: bool) -> Self {
        let mut flag = ES_CONTINUOUS;
        if display {
            flag |= ES_DISPLAY_REQUIRED;
        }
        if idle {
            flag |= ES_SYSTEM_REQUIRED;
        }
        if sleep {
            flag |= ES_AWAYMODE_REQUIRED;
        }
        unsafe { SetThreadExecutionState(flag) };
        WakeLock(flag)
    }

    pub fn set_display(&mut self, display: bool) -> ResultType<()> {
        let flag = if display {
            self.0 | ES_DISPLAY_REQUIRED
        } else {
            self.0 & !ES_DISPLAY_REQUIRED
        };
        if flag != self.0 {
            unsafe { SetThreadExecutionState(flag) };
            self.0 = flag;
        }
        Ok(())
    }
}

impl Drop for WakeLock {
    fn drop(&mut self) {
        unsafe { SetThreadExecutionState(ES_CONTINUOUS) };
    }
}

pub fn uninstall_service(show_new_window: bool, _: bool) -> bool {
    log::info!("Uninstalling service...");
    let filter = format!(" /FI \"PID ne {}\"", get_current_pid());
    Config::set_option("stop-service".into(), "Y".into());
    let cmds = format!(
        "
    chcp 65001
    sc stop \"{app_name}\"
    sc delete \"{app_name}\"
    if exist \"%PROGRAMDATA%\\Microsoft\\Windows\\Start Menu\\Programs\\Startup\\{app_name} Tray.lnk\" del /f /q \"%PROGRAMDATA%\\Microsoft\\Windows\\Start Menu\\Programs\\Startup\\{app_name} Tray.lnk\"
    taskkill /F /IM {broker_exe}
    taskkill /F /IM \"{app_name}.exe\"{filter}
    ",
        app_name = crate::get_app_name(),
        broker_exe = rbexe(),
    );
    if let Err(err) = run_cmds(cmds, false, "uninstall") {
        Config::set_option("stop-service".into(), "".into());
        log::debug!("{err}");
        return true;
    }
    run_after_run_cmds(!show_new_window);
    Config::set_option("stop-service".into(), "".into());	
    std::process::exit(0);
}

pub fn install_service() -> bool {
    log::info!("Installing service...");
    let (_, _, _, exe) = get_install_info();
    let tmp_dir = std::env::temp_dir().join(format!(
        "{}_{}",
        crate::get_app_name(),
        uuid::Uuid::new_v4().simple()
    ));
    if let Err(err) = fs::create_dir(&tmp_dir) {
        log::error!("Failed to create the install folder: {}", err);
        return true;
    }
    let tmp_path = tmp_dir.to_string_lossy().to_string();
    let tray_shortcut = get_tray_shortcut(&exe, &tmp_path).unwrap_or_default();
    let filter = format!(" /FI \"PID ne {}\"", get_current_pid());
    Config::set_option("stop-service".into(), "".into());
    crate::ipc::EXIT_RECV_CLOSE.store(false, Ordering::Relaxed);
    let cmds = format!(
        "
chcp 65001
taskkill /F /IM \"{app_name}.exe\"{filter}
cscript \"{tray_shortcut}\"
copy /Y \"{tmp_path}\\{app_name} Tray.lnk\" \"%PROGRAMDATA%\\Microsoft\\Windows\\Start Menu\\Programs\\Startup\\\"
{import_config}
{create_service}
{firewall_rules}
if exist \"{tray_shortcut}\" del /f /q \"{tray_shortcut}\"
rmdir /s /q \"{tmp_path}\"
    ",
        app_name = crate::get_app_name(),
        import_config = get_import_config(&exe),
        create_service = get_create_service(&exe),
        firewall_rules = get_firewall_rules(),
    );
    if let Err(err) = run_cmds(cmds, false, "install") {
        crate::ipc::EXIT_RECV_CLOSE.store(true, Ordering::Relaxed);
        log::debug!("{err}");
        return true;
    }
    run_after_run_cmds(false);
    std::process::exit(0);
}

pub fn get_tray_shortcut(exe: &str, tmp_path: &str) -> ResultType<String> {
    Ok(write_cmds(
        format!(
            "
Set oWS = WScript.CreateObject(\"WScript.Shell\")
sLinkFile = \"{tmp_path}\\{app_name} Tray.lnk\"

Set oLink = oWS.CreateShortcut(sLinkFile)
    oLink.TargetPath = \"{exe}\"
    oLink.Arguments = \"--tray\"
oLink.Save
        ",
            app_name = crate::get_app_name(),
        ),
        "vbs",
        "tray_shortcut",
    )?
    .to_str()
    .unwrap_or("")
    .to_owned())
}

// Beacon LAN-only: allow inbound direct access (TCP) and LAN discovery
// (UDP) through Windows Firewall on install.
fn get_firewall_rules() -> String {
    if config::is_outgoing_only() {
        return "".to_string();
    }
    let direct_port = hbb_common::config::RENDEZVOUS_PORT + 2;
    let discovery_port = hbb_common::config::RENDEZVOUS_PORT + 3;
    format!(
        "
netsh advfirewall firewall delete rule name=\"{app_name} Direct TCP\" >nul 2>nul
netsh advfirewall firewall add rule name=\"{app_name} Direct TCP\" dir=in action=allow protocol=TCP localport={direct_port}
netsh advfirewall firewall delete rule name=\"{app_name} Discovery UDP\" >nul 2>nul
netsh advfirewall firewall add rule name=\"{app_name} Discovery UDP\" dir=in action=allow protocol=UDP localport={discovery_port}
",
        app_name = crate::get_app_name(),
    )
}

fn get_import_config(exe: &str) -> String {
    if config::is_outgoing_only() {
        return "".to_string();
    }
    format!("
sc stop \"{app_name}\"
sc delete \"{app_name}\"
sc create \"{app_name}\" binpath= \"\\\"{exe}\\\" --import-config \\\"{config_path}\\\"\" start= auto DisplayName= \"{app_name} Service\"
sc start \"{app_name}\"
sc stop \"{app_name}\"
sc delete \"{app_name}\"
",
    app_name = crate::get_app_name(),
    config_path=Config::file().to_str().unwrap_or(""),
)
}

fn get_create_service(exe: &str) -> String {
    if config::is_outgoing_only() {
        return "".to_string();
    }
    let stop = Config::get_option("stop-service") == "Y";
    if stop {
        format!("
if exist \"%PROGRAMDATA%\\Microsoft\\Windows\\Start Menu\\Programs\\Startup\\{app_name} Tray.lnk\" del /f /q \"%PROGRAMDATA%\\Microsoft\\Windows\\Start Menu\\Programs\\Startup\\{app_name} Tray.lnk\"
", app_name = crate::get_app_name())
    } else {
        let protect = if hbb_common::config::is_tamper_protected() {
            format!("sc failure \"{app_name}\" reset= 0 actions= restart/5000\nsc sdset \"{app_name}\" {TAMPER_PROTECT_SDDL}\n", app_name = crate::get_app_name())
        } else {
            String::new()
        };
        format!("
sc create \"{app_name}\" binpath= \"\\\"{exe}\\\" --service\" start= auto DisplayName= \"{app_name} Service\"
sc start \"{app_name}\"
{protect}",
    app_name = crate::get_app_name())
    }
}

fn run_after_run_cmds(silent: bool) {
    let (_, _, _, exe) = get_install_info();
    if !silent {
        log::debug!("Spawn new window");
        allow_err!(std::process::Command::new("cmd")
            .arg("/c")
            .arg("timeout /t 2 & start hoptodesk://")
            .creation_flags(winapi::um::winbase::CREATE_NO_WINDOW)
            .spawn());
    }
    if Config::get_option("stop-service") != "Y" {
        allow_err!(std::process::Command::new(&exe).arg("--tray").spawn());
    }
    std::thread::sleep(std::time::Duration::from_millis(300));
}

#[inline]
pub fn try_kill_broker() {
    allow_err!(std::process::Command::new("cmd")
        .arg("/c")
        .arg(&format!(
            "taskkill /F /IM {}",
            rbexe()
        ))
        .creation_flags(winapi::um::winbase::CREATE_NO_WINDOW)
        .spawn());
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn test_uninstall_cert() {
        println!("uninstall driver certs: {:?}", cert::uninstall_cert());
    }

    #[test]
    fn test_get_unicode_char_by_vk() {
        let chr = get_char_from_vk(0x41); // VK_A
        assert_eq!(chr, Some('a'));
        let chr = get_char_from_vk(VK_ESCAPE as u32); // VK_ESC
        assert_eq!(chr, None)
    }
}

pub fn alloc_console() {
    unsafe {
        alloc_console_and_redirect();
    }
}


pub struct WallPaperRemover {
    old_path: String,
}

impl WallPaperRemover {
    pub fn new() -> ResultType<Self> {
        let start = std::time::Instant::now();
        if !Self::need_remove() {
            bail!("already solid color");
        }
        let old_path = match Self::get_recent_wallpaper() {
            Ok(old_path) => old_path,
            Err(e) => {
                log::info!("Failed to get recent wallpaper:{:?}, use fallback", e);
                wallpaper::get().map_err(|e| anyhow!(e.to_string()))?
            }
        };
        Self::set_wallpaper(None)?;
        log::info!(
            "created wallpaper remover,  old_path:{:?},  elapsed:{:?}",
            old_path,
            start.elapsed(),
        );
        Ok(Self { old_path })
    }

    pub fn support() -> bool {
        wallpaper::get().is_ok() || !Self::get_recent_wallpaper().unwrap_or_default().is_empty()
    }

    fn get_recent_wallpaper() -> ResultType<String> {
        // SystemParametersInfoW may return %appdata%\Microsoft\Windows\Themes\TranscodedWallpaper, not real path and may not real cache
        // https://www.makeuseof.com/find-desktop-wallpapers-file-location-windows-11/
        // https://superuser.com/questions/1218413/write-to-current-users-registry-through-a-different-admin-account
        let (hkcu, sid) = if is_root() {
            let sid = get_current_process_session_id().ok_or(anyhow!("failed to get sid"))?;
            (RegKey::predef(HKEY_USERS), format!("{}\\", sid))
        } else {
            (RegKey::predef(HKEY_CURRENT_USER), "".to_string())
        };
        let explorer_key = hkcu.open_subkey_with_flags(
            &format!(
                "{}Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\Wallpapers",
                sid
            ),
            KEY_READ,
        )?;
        Ok(explorer_key.get_value("BackgroundHistoryPath0")?)
    }

    fn need_remove() -> bool {
        if let Ok(wallpaper) = wallpaper::get() {
            return !wallpaper.is_empty();
        }
        false
    }

    fn set_wallpaper(path: Option<String>) -> ResultType<()> {
        wallpaper::set_from_path(&path.unwrap_or_default()).map_err(|e| anyhow!(e.to_string()))
    }
}

impl Drop for WallPaperRemover {
    fn drop(&mut self) {
        // If the old background is a slideshow, it will be converted into an image. AnyDesk does the same.
        allow_err!(Self::set_wallpaper(Some(self.old_path.clone())));
    }
}
fn get_uninstall_htd_idd() -> String {
    match std::env::current_exe() {
        Ok(path) => format!("\"{}\" --uninstall-htd-idd", path.to_str().unwrap_or("")),
        Err(e) => {
            log::warn!("Failed to get the current exe path, cannot uninstall the virtual display driver: {:?}", e);
            "".to_string()
        }
    }
}
#[inline]
pub fn is_self_service_running() -> bool {
    is_service_running(&crate::get_app_name())
}

pub fn is_service_running(service_name: &str) -> bool {
    unsafe {
        let service_name = wide_string(service_name);
        is_service_running_w(service_name.as_ptr() as _)
    }
}

const SAFE_MODE_RESTART_VALUE: &str = "SafeModeRestart";

fn service_parameters_key() -> String {
    format!(
        "SYSTEM\\CurrentControlSet\\Services\\{}\\Parameters",
        crate::get_app_name()
    )
}

fn bcdedit(args: &[&str]) -> ResultType<()> {
    let sysnative = expand_env_vars("%SystemRoot%\\Sysnative\\bcdedit.exe");
    let program = if Path::new(&sysnative).exists() {
        sysnative
    } else {
        expand_env_vars("%SystemRoot%\\System32\\bcdedit.exe")
    };
    let output = std::process::Command::new(program)
        .args(args)
        .creation_flags(CREATE_NO_WINDOW)
        .output()?;
    if !output.status.success() {
        bail!(
            "bcdedit {:?}: {}",
            args,
            String::from_utf8_lossy(&output.stdout).trim()
        );
    }
    Ok(())
}

pub fn restart_in_safe_mode() -> ResultType<()> {
    let hklm = RegKey::predef(HKEY_LOCAL_MACHINE);
    let (safe_boot, _) = hklm.create_subkey(format!(
        "SYSTEM\\CurrentControlSet\\Control\\SafeBoot\\Network\\{}",
        crate::get_app_name()
    ))?;
    safe_boot.set_value("", &"Service")?;
    let (parameters, _) = hklm.create_subkey(service_parameters_key())?;
    parameters.set_value(SAFE_MODE_RESTART_VALUE, &1u32)?;
    if let Err(err) = bcdedit(&["/set", "{current}", "safeboot", "network"]) {
        allow_err!(parameters.delete_value(SAFE_MODE_RESTART_VALUE));
        return Err(err);
    }
    if let Err(err) = system_shutdown::force_reboot() {
        allow_err!(bcdedit(&["/deletevalue", "{current}", "safeboot"]));
        allow_err!(parameters.delete_value(SAFE_MODE_RESTART_VALUE));
        bail!("{}", err);
    }
    Ok(())
}

pub fn finish_safe_mode_restart() {
    if unsafe { GetSystemMetrics(SM_CLEANBOOT) } == 0 {
        return;
    }
    let Ok(parameters) = RegKey::predef(HKEY_LOCAL_MACHINE)
        .open_subkey_with_flags(service_parameters_key(), KEY_READ | KEY_WRITE)
    else {
        return;
    };
    if parameters.get_value::<u32, _>(SAFE_MODE_RESTART_VALUE).is_err() {
        return;
    }
    match bcdedit(&["/deletevalue", "{current}", "safeboot"]) {
        Ok(()) => {
            allow_err!(parameters.delete_value(SAFE_MODE_RESTART_VALUE));
            log::info!("Safe Mode restart done, the next restart is a normal one");
        }
        Err(err) => log::error!("Failed to clear the Safe Mode boot flag: {}", err),
    }
}

pub fn is_x64() -> bool {
    const PROCESSOR_ARCHITECTURE_AMD64: u16 = 9;

    let mut sys_info = SYSTEM_INFO::default();
    unsafe {
        GetNativeSystemInfo(&mut sys_info as _);
    }
    unsafe { sys_info.u.s().wProcessorArchitecture == PROCESSOR_ARCHITECTURE_AMD64 }
}

pub fn try_kill_rustdesk_main_window_process() -> ResultType<()> {
    // Kill rustdesk.exe without extra arg, should only be called by --server
    // We can find the exact process which occupies the ipc, see more from https://github.com/winsiderss/systeminformer
    log::info!("try kill hoptodesk main window process");
    use hbb_common::sysinfo::System;
    let mut sys = System::new();
    sys.refresh_processes();
    let my_uid = sys
        .process((std::process::id() as usize).into())
        .map(|x| x.user_id())
        .unwrap_or_default();
    let my_pid = std::process::id();
    let app_name = crate::get_app_name().to_lowercase();
    if app_name.is_empty() {
        bail!("app name is empty");
    }
    for (_, p) in sys.processes().iter() {
        let p_name = p.name().to_lowercase();
        // name equal
        if !(p_name == app_name || p_name == app_name.clone() + ".exe") {
            continue;
        }
        // arg more than 1
        if p.cmd().len() < 1 {
            continue;
        }
        // first arg contain app name
        if !p.cmd()[0].to_lowercase().contains(&p_name) {
            continue;
        }
        // only one arg or the second arg is empty uni link
        let is_empty_uni = p.cmd().len() == 2 && crate::common::is_empty_uni_link(&p.cmd()[1]);
        if !(p.cmd().len() == 1 || is_empty_uni) {
            continue;
        }
        // skip self
        if p.pid().as_u32() == my_pid {
            continue;
        }
        // because we call it with --server, so we can check user_id, remove this if call it with user process
        if p.user_id() == my_uid {
            log::info!("user id equal, continue");
            continue;
        }
        log::info!("try kill process: {:?}, pid = {:?}", p.cmd(), p.pid());
        nt_terminate_process(p.pid().as_u32())?;
        log::info!("kill process success: {:?}, pid = {:?}", p.cmd(), p.pid());
        return Ok(());
    }
    bail!("failed to find hoptodesk main window process");
}

fn nt_terminate_process(process_id: DWORD) -> ResultType<()> {
    type NtTerminateProcess = unsafe extern "system" fn(HANDLE, DWORD) -> DWORD;
    unsafe {
        let h_module = if is_win_10_or_greater() {
            LoadLibraryExA(
                CString::new("ntdll.dll")?.as_ptr(),
                std::ptr::null_mut(),
                LOAD_LIBRARY_SEARCH_SYSTEM32,
            )
        } else {
            LoadLibraryA(CString::new("ntdll.dll")?.as_ptr())
        };
        if !h_module.is_null() {
            let f_nt_terminate_process: NtTerminateProcess = std::mem::transmute(GetProcAddress(
                h_module,
                CString::new("NtTerminateProcess")?.as_ptr(),
            ));
            let h_token = OpenProcess(PROCESS_ALL_ACCESS, 0, process_id);
            if !h_token.is_null() {
                if f_nt_terminate_process(h_token, 1) == 0 {
                    log::info!("terminate process {} success", process_id);
                    CloseHandle(h_token);
                    return Ok(());
                } else {
                    CloseHandle(h_token);
                    bail!("NtTerminateProcess {} failed", process_id);
                }
            } else {
                bail!("OpenProcess {} failed", process_id);
            }
        } else {
            bail!("Failed to load ntdll.dll");
        }
    }
}

pub fn try_set_window_foreground(window: HWND) {
    let env_key = SET_FOREGROUND_WINDOW;
    if let Ok(value) = std::env::var(env_key) {
        if value == "1" {
            unsafe {
                SetForegroundWindow(window);
            }
            std::env::remove_var(env_key);
        }
    }
}

pub mod reg_display_settings {
    use hbb_common::ResultType;
    use serde_derive::{Deserialize, Serialize};
    use std::collections::HashMap;
    use winreg::{enums::*, RegValue};
    const REG_GRAPHICS_DRIVERS_PATH: &str = "SYSTEM\\CurrentControlSet\\Control\\GraphicsDrivers";
    const REG_CONNECTIVITY_PATH: &str = "Connectivity";

    #[derive(Serialize, Deserialize, Debug)]
    pub struct RegRecovery {
        path: String,
        key: String,
        old: (Vec<u8>, isize),
        new: (Vec<u8>, isize),
    }

    pub fn read_reg_connectivity() -> ResultType<HashMap<String, HashMap<String, RegValue>>> {
        let hklm = winreg::RegKey::predef(HKEY_LOCAL_MACHINE);
        let reg_connectivity = hklm.open_subkey_with_flags(
            format!("{}\\{}", REG_GRAPHICS_DRIVERS_PATH, REG_CONNECTIVITY_PATH),
            KEY_READ,
        )?;

        let mut map_connectivity = HashMap::new();
        for key in reg_connectivity.enum_keys() {
            let key = key?;
            let mut map_item = HashMap::new();
            let reg_item = reg_connectivity.open_subkey_with_flags(&key, KEY_READ)?;
            for value in reg_item.enum_values() {
                let (name, value) = value?;
                map_item.insert(name, value);
            }
            map_connectivity.insert(key, map_item);
        }
        Ok(map_connectivity)
    }

    pub fn diff_recent_connectivity(
        map1: HashMap<String, HashMap<String, RegValue>>,
        map2: HashMap<String, HashMap<String, RegValue>>,
    ) -> Option<RegRecovery> {
        for (subkey, map_item2) in map2 {
            if let Some(map_item1) = map1.get(&subkey) {
                let key = "Recent";
                if let Some(value1) = map_item1.get(key) {
                    if let Some(value2) = map_item2.get(key) {
                        if value1 != value2 {
                            return Some(RegRecovery {
                                path: format!(
                                    "{}\\{}\\{}",
                                    REG_GRAPHICS_DRIVERS_PATH, REG_CONNECTIVITY_PATH, subkey
                                ),
                                key: key.to_owned(),
                                old: (value1.bytes.clone(), value1.vtype.clone() as isize),
                                new: (value2.bytes.clone(), value2.vtype.clone() as isize),
                            });
                        }
                    }
                }
            }
        }
        None
    }

    pub fn restore_reg_connectivity(reg_recovery: RegRecovery, force: bool) -> ResultType<()> {
        let hklm = winreg::RegKey::predef(HKEY_LOCAL_MACHINE);
        let reg_item = hklm.open_subkey_with_flags(&reg_recovery.path, KEY_READ | KEY_WRITE)?;
        if !force {
            let cur_reg_value = reg_item.get_raw_value(&reg_recovery.key)?;
            let new_reg_value = RegValue {
                bytes: reg_recovery.new.0,
                vtype: isize_to_reg_type(reg_recovery.new.1),
            };
            // Compare if the current value is the same as the new value.
            // If they are not the same, the registry value has been changed by other processes.
            // So we do not restore the registry value.
            if cur_reg_value != new_reg_value {
                return Ok(());
            }
        }
        let reg_value = RegValue {
            bytes: reg_recovery.old.0,
            vtype: isize_to_reg_type(reg_recovery.old.1),
        };
        reg_item.set_raw_value(&reg_recovery.key, &reg_value)?;
        Ok(())
    }

    #[inline]
    fn isize_to_reg_type(i: isize) -> RegType {
        match i {
            0 => RegType::REG_NONE,
            1 => RegType::REG_SZ,
            2 => RegType::REG_EXPAND_SZ,
            3 => RegType::REG_BINARY,
            4 => RegType::REG_DWORD,
            5 => RegType::REG_DWORD_BIG_ENDIAN,
            6 => RegType::REG_LINK,
            7 => RegType::REG_MULTI_SZ,
            8 => RegType::REG_RESOURCE_LIST,
            9 => RegType::REG_FULL_RESOURCE_DESCRIPTOR,
            10 => RegType::REG_RESOURCE_REQUIREMENTS_LIST,
            11 => RegType::REG_QWORD,
            _ => RegType::REG_NONE,
        }
    }
}

pub fn rbexe() -> String {
    format!("RuntimeBroker_{}.exe", crate::get_app_name().replace(" ", "").to_lowercase())
}

// ============================================================================
// Remote Printing Support
// ============================================================================

/// Get list of available printers on this Windows machine
pub fn get_printers() -> Vec<String> {
    use std::ptr;

    #[repr(C)]
    #[allow(non_snake_case)]
    struct PRINTER_INFO_2W {
        pServerName: *mut u16,
        pPrinterName: *mut u16,
        pShareName: *mut u16,
        pPortName: *mut u16,
        pDriverName: *mut u16,
        pComment: *mut u16,
        pLocation: *mut u16,
        pDevMode: *mut c_void,
        pSepFile: *mut u16,
        pPrintProcessor: *mut u16,
        pDatatype: *mut u16,
        pParameters: *mut u16,
        pSecurityDescriptor: *mut c_void,
        Attributes: u32,
        Priority: u32,
        DefaultPriority: u32,
        StartTime: u32,
        UntilTime: u32,
        Status: u32,
        cJobs: u32,
        AveragePPM: u32,
    }

    #[link(name = "winspool")]
    extern "system" {
        fn EnumPrintersW(
            Flags: u32,
            Name: *const u16,
            Level: u32,
            pPrinterEnum: *mut u8,
            cbBuf: u32,
            pcbNeeded: *mut u32,
            pcReturned: *mut u32,
        ) -> i32;
    }

    const PRINTER_ENUM_LOCAL: u32 = 0x00000002;
    const PRINTER_ENUM_CONNECTIONS: u32 = 0x00000004;

    let mut printers = Vec::new();

    unsafe {
        let mut needed: u32 = 0;
        let mut returned: u32 = 0;

        // First call to get required buffer size
        EnumPrintersW(
            PRINTER_ENUM_LOCAL | PRINTER_ENUM_CONNECTIONS,
            ptr::null(),
            2,
            ptr::null_mut(),
            0,
            &mut needed,
            &mut returned,
        );

        if needed == 0 {
            return printers;
        }

        // Allocate buffer and get printer info
        let mut buffer: Vec<u8> = vec![0; needed as usize];
        let result = EnumPrintersW(
            PRINTER_ENUM_LOCAL | PRINTER_ENUM_CONNECTIONS,
            ptr::null(),
            2,
            buffer.as_mut_ptr(),
            needed,
            &mut needed,
            &mut returned,
        );

        if result == 0 {
            log::error!("EnumPrintersW failed: {}", io::Error::last_os_error());
            return printers;
        }

        // Parse results
        let printer_info = buffer.as_ptr() as *const PRINTER_INFO_2W;
        for i in 0..returned as isize {
            let info = &*printer_info.offset(i);
            if !info.pPrinterName.is_null() {
                let name = wide_string_to_string(info.pPrinterName);
                if !name.is_empty() {
                    printers.push(name);
                }
            }
        }
    }

    log::info!("Found {} printers", printers.len());
    printers
}

/// Get the default printer name
pub fn get_default_printer() -> Option<String> {
    #[link(name = "winspool")]
    extern "system" {
        fn GetDefaultPrinterW(pszBuffer: *mut u16, pcchBuffer: *mut u32) -> i32;
    }

    unsafe {
        let mut size: u32 = 0;
        GetDefaultPrinterW(std::ptr::null_mut(), &mut size);

        if size == 0 {
            return None;
        }

        let mut buffer: Vec<u16> = vec![0; size as usize];
        if GetDefaultPrinterW(buffer.as_mut_ptr(), &mut size) != 0 {
            Some(wide_string_to_string(buffer.as_ptr()))
        } else {
            None
        }
    }
}

pub fn is_printer_online(printer_name: &str) -> bool {
    #[repr(C)]
    #[allow(non_snake_case)]
    struct PRINTER_INFO_6 {
        dwStatus: u32,
    }

    #[link(name = "winspool")]
    extern "system" {
        fn OpenPrinterW(pPrinterName: *const u16, phPrinter: *mut HANDLE, pDefault: *const c_void) -> i32;
        fn GetPrinterW(
            hPrinter: HANDLE,
            Level: u32,
            pPrinter: *mut u8,
            cbBuf: u32,
            pcbNeeded: *mut u32,
        ) -> i32;
        fn ClosePrinter(hPrinter: HANDLE) -> i32;
    }

    const PRINTER_STATUS_OFFLINE: u32 = 0x00000080;

    unsafe {
        let name = wide_string(printer_name);
        let mut handle: HANDLE = std::ptr::null_mut();
        if OpenPrinterW(name.as_ptr(), &mut handle, std::ptr::null()) == 0 {
            return false;
        }
        let mut info = PRINTER_INFO_6 { dwStatus: 0 };
        let mut needed: u32 = 0;
        let ok = GetPrinterW(
            handle,
            6,
            &mut info as *mut PRINTER_INFO_6 as *mut u8,
            std::mem::size_of::<PRINTER_INFO_6>() as u32,
            &mut needed,
        );
        ClosePrinter(handle);
        if ok == 0 {
            return true;
        }
        info.dwStatus & PRINTER_STATUS_OFFLINE == 0
    }
}

pub fn send_raw_data_to_printer(printer_name: Option<String>, data: Vec<u8>) -> ResultType<()> {
    let target_printer = match printer_name {
        Some(name) if !name.is_empty() => name,
        _ => get_default_printer().ok_or_else(|| anyhow!("No default printer found"))?,
    };

    log::info!("Sending {} bytes to printer: {}", data.len(), target_printer);

    let is_pdf_data = data.len() >= 5 && &data[..5] == b"%PDF-";
    let is_pdf_printer = target_printer.to_lowercase().contains("print to pdf");

    if is_pdf_data {
        log::info!("Detected PDF data ({} bytes)", data.len());
        if is_pdf_printer {
            let docs = std::env::var("USERPROFILE").unwrap_or_else(|_| "C:\\Users\\Public".into());
            let ts = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            let path = format!("{}\\Documents\\HopToDesk_Print_{}.pdf", docs, ts);
            log::info!("Writing PDF directly to: {}", path);
            std::fs::write(&path, &data)?;
            log::info!("Successfully saved PDF ({} bytes) to {}", data.len(), path);
            return Ok(());
        } else {
            return print_pdf_to_printer(&data, &target_printer);
        }
    }

    log::info!("Using RAW spooler passthrough for non-PDF data");
    send_raw_data_to_printer_spooler(&target_printer, &data)
}

fn print_pdf_to_printer(data: &[u8], printer_name: &str) -> ResultType<()> {
    print_pdf_via_shell(data, printer_name)
}

fn print_pdf_via_shell(data: &[u8], printer_name: &str) -> ResultType<()> {
    #[link(name = "shell32")]
    extern "system" {
        fn ShellExecuteW(
            hwnd: HANDLE,
            lpOperation: *const u16,
            lpFile: *const u16,
            lpParameters: *const u16,
            lpDirectory: *const u16,
            nShowCmd: i32,
        ) -> isize;
    }

    const SW_HIDE: i32 = 0;

    let temp_dir = std::env::temp_dir();
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let temp_path = temp_dir.join(format!("HopToDesk_Print_{}.pdf", ts));
    std::fs::write(&temp_path, data)?;
    log::info!("Saved temp PDF to: {}", temp_path.display());

    let verb = wide_string("printto");
    let file = wide_string(&temp_path.to_string_lossy());
    let params = wide_string(printer_name);

    let result = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            verb.as_ptr(),
            file.as_ptr(),
            params.as_ptr(),
            std::ptr::null(),
            SW_HIDE,
        )
    };

    if result <= 32 {
        let err = io::Error::last_os_error();
        log::error!("ShellExecuteW printto failed (result={}): {}", result, err);
        let _ = std::fs::remove_file(&temp_path);
        bail!("Failed to print PDF via ShellExecute: {}", err);
    }

    log::info!("Successfully sent PDF to printer '{}' via ShellExecute", printer_name);
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(120));
        let _ = std::fs::remove_file(&temp_path);
    });
    Ok(())
}

fn send_raw_data_to_printer_spooler(target_printer: &str, data: &[u8]) -> ResultType<()> {
    use std::ptr;

    #[repr(C)]
    #[allow(non_snake_case)]
    struct DOC_INFO_1W {
        pDocName: *const u16,
        pOutputFile: *const u16,
        pDatatype: *const u16,
    }

    #[link(name = "winspool")]
    extern "system" {
        fn OpenPrinterW(pPrinterName: *const u16, phPrinter: *mut HANDLE, pDefault: *const c_void) -> i32;
        fn StartDocPrinterW(hPrinter: HANDLE, Level: u32, pDocInfo: *const DOC_INFO_1W) -> u32;
        fn StartPagePrinter(hPrinter: HANDLE) -> i32;
        fn WritePrinter(hPrinter: HANDLE, pBuf: *const c_void, cbBuf: u32, pcWritten: *mut u32) -> i32;
        fn EndPagePrinter(hPrinter: HANDLE) -> i32;
        fn EndDocPrinter(hPrinter: HANDLE) -> i32;
        fn ClosePrinter(hPrinter: HANDLE) -> i32;
    }

    let printer_name_wide = wide_string(target_printer);
    let doc_name = wide_string("HopToDesk Remote Print Job");
    let datatype = wide_string("RAW");

    unsafe {
        let mut printer_handle: HANDLE = ptr::null_mut();

        if OpenPrinterW(printer_name_wide.as_ptr(), &mut printer_handle, ptr::null()) == 0 {
            bail!("Failed to open printer '{}': {}", target_printer, io::Error::last_os_error());
        }

        let doc_info = DOC_INFO_1W {
            pDocName: doc_name.as_ptr(),
            pOutputFile: ptr::null(),
            pDatatype: datatype.as_ptr(),
        };

        let job_id = StartDocPrinterW(printer_handle, 1, &doc_info);
        if job_id == 0 {
            let err = io::Error::last_os_error();
            ClosePrinter(printer_handle);
            bail!("Failed to start document: {}", err);
        }

        if StartPagePrinter(printer_handle) == 0 {
            let err = io::Error::last_os_error();
            EndDocPrinter(printer_handle);
            ClosePrinter(printer_handle);
            bail!("Failed to start page: {}", err);
        }

        let mut written: u32 = 0;
        let write_result = WritePrinter(
            printer_handle,
            data.as_ptr() as *const c_void,
            data.len() as u32,
            &mut written,
        );

        if write_result == 0 {
            let err = io::Error::last_os_error();
            EndPagePrinter(printer_handle);
            EndDocPrinter(printer_handle);
            ClosePrinter(printer_handle);
            bail!("Failed to write to printer: {}", err);
        }

        if written != data.len() as u32 {
            log::warn!("Only wrote {} of {} bytes to printer", written, data.len());
        }

        if EndPagePrinter(printer_handle) == 0 {
            log::warn!("EndPagePrinter failed: {}", io::Error::last_os_error());
        }

        if EndDocPrinter(printer_handle) == 0 {
            log::warn!("EndDocPrinter failed: {}", io::Error::last_os_error());
        }

        ClosePrinter(printer_handle);

        log::info!("Successfully sent print job (ID: {}) to printer", job_id);
    }

    Ok(())
}

/// Helper function to convert wide string pointer to Rust String
fn wide_string_to_string(ptr: *const u16) -> String {
    if ptr.is_null() {
        return String::new();
    }
    unsafe {
        let mut len = 0;
        while *ptr.offset(len) != 0 {
            len += 1;
        }
        let slice = std::slice::from_raw_parts(ptr, len as usize);
        String::from_utf16_lossy(slice)
    }
}

// ============================================================================
// Named Pipe Print Server (for capturing print jobs on remote machine)
// ============================================================================

pub const PRINTER_PIPE_NAME: &str = r"\\.\pipe\HopToDeskPrinter";
pub const VIRTUAL_PRINTER_NAME: &str = "HopToDesk Printer";

/// Callback type for when a print job is received
pub type PrintJobCallback = Box<dyn Fn(Vec<u8>) + Send + Sync>;

/// Start a named pipe server to receive print jobs
/// Returns a handle that can be used to stop the server
pub fn start_print_pipe_server<F>(callback: F) -> ResultType<std::sync::mpsc::Sender<()>>
where
    F: Fn(Vec<u8>) + Send + Sync + 'static,
{
    use std::sync::mpsc;
    use winapi::um::fileapi::{ReadFile, FlushFileBuffers};

    // Named pipe functions - declared here as they may not be in all winapi versions
    #[link(name = "kernel32")]
    extern "system" {
        fn CreateNamedPipeW(
            lpName: *const u16,
            dwOpenMode: u32,
            dwPipeMode: u32,
            nMaxInstances: u32,
            nOutBufferSize: u32,
            nInBufferSize: u32,
            nDefaultTimeOut: u32,
            lpSecurityAttributes: *mut c_void,
        ) -> HANDLE;
        fn ConnectNamedPipe(hNamedPipe: HANDLE, lpOverlapped: *mut c_void) -> i32;
        fn DisconnectNamedPipe(hNamedPipe: HANDLE) -> i32;
    }

    // Pipe constants
    const PIPE_ACCESS_INBOUND: u32 = 0x00000001;
    const PIPE_TYPE_BYTE: u32 = 0x00000000;
    const PIPE_READMODE_BYTE: u32 = 0x00000000;
    const PIPE_WAIT: u32 = 0x00000000;
    const PIPE_BUFFER_SIZE: u32 = 65536;
    const PIPE_UNLIMITED_INSTANCES: u32 = 255;
    const INVALID_HANDLE_VALUE: HANDLE = -1isize as HANDLE;

    let (stop_tx, stop_rx) = mpsc::channel::<()>();
    let callback = std::sync::Arc::new(callback);

    std::thread::spawn(move || {
        let pipe_name = wide_string(PRINTER_PIPE_NAME);
        let mut create_failures = 0u32;

        loop {
            if stop_rx.try_recv().is_ok() {
                log::info!("Print pipe server stopping (channel signal)");
                break;
            }
            if !crate::server::printer_service::is_print_service_running() {
                log::info!("Print pipe server stopping (service flag cleared)");
                break;
            }

            unsafe {
                let pipe_handle = CreateNamedPipeW(
                    pipe_name.as_ptr(),
                    PIPE_ACCESS_INBOUND,
                    PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT,
                    PIPE_UNLIMITED_INSTANCES,
                    PIPE_BUFFER_SIZE,
                    PIPE_BUFFER_SIZE,
                    0,
                    std::ptr::null_mut(),
                );

                if pipe_handle == INVALID_HANDLE_VALUE {
                    create_failures += 1;
                    log::error!("Failed to create named pipe: {}", io::Error::last_os_error());
                    if create_failures > 60 {
                        log::error!("Print pipe server giving up after repeated create failures");
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(500));
                    continue;
                }
                create_failures = 0;

                log::info!("Print pipe server waiting for connection...");

                if ConnectNamedPipe(pipe_handle, std::ptr::null_mut()) == 0 {
                    let error = io::Error::last_os_error();
                    if error.raw_os_error() != Some(535) {
                        log::error!("ConnectNamedPipe failed: {}", error);
                        CloseHandle(pipe_handle);
                        continue;
                    }
                }

                if !crate::server::printer_service::is_print_service_running() {
                    DisconnectNamedPipe(pipe_handle);
                    CloseHandle(pipe_handle);
                    log::info!("Print pipe server stopping after unblock");
                    break;
                }

                log::info!("Print client connected");

                let handle_usize = pipe_handle as usize;
                let callback = callback.clone();
                std::thread::spawn(move || {
                    let pipe_handle = handle_usize as HANDLE;
                    let mut all_data = Vec::new();
                    let mut buffer = vec![0u8; PIPE_BUFFER_SIZE as usize];

                    loop {
                        let mut bytes_read: u32 = 0;
                        let result = ReadFile(
                            pipe_handle,
                            buffer.as_mut_ptr() as *mut c_void,
                            PIPE_BUFFER_SIZE,
                            &mut bytes_read,
                            std::ptr::null_mut(),
                        );

                        if result == 0 || bytes_read == 0 {
                            break;
                        }

                        all_data.extend_from_slice(&buffer[..bytes_read as usize]);
                    }

                    log::info!("Received print job: {} bytes", all_data.len());

                    if !all_data.is_empty() {
                        callback(all_data);
                    }

                    FlushFileBuffers(pipe_handle);
                    DisconnectNamedPipe(pipe_handle);
                    CloseHandle(pipe_handle);
                });
            }
        }
        log::info!("Print pipe server thread exiting");
    });

    Ok(stop_tx)
}

pub fn is_print_pipe_alive() -> bool {
    #[link(name = "kernel32")]
    extern "system" {
        fn WaitNamedPipeW(lpNamedPipeName: *const u16, nTimeOut: u32) -> i32;
    }
    const ERROR_SEM_TIMEOUT: i32 = 121;
    let name = wide_string(PRINTER_PIPE_NAME);
    unsafe {
        if WaitNamedPipeW(name.as_ptr(), 1) != 0 {
            return true;
        }
        io::Error::last_os_error().raw_os_error() == Some(ERROR_SEM_TIMEOUT)
    }
}

/// Check if the HopToDesk virtual printer is installed
pub fn is_virtual_printer_installed() -> bool {
    get_printers().iter().any(|p| p == VIRTUAL_PRINTER_NAME)
}

/// Get list of available printer drivers
pub fn get_printer_drivers() -> Vec<String> {
    use std::ptr;

    #[repr(C)]
    #[allow(non_snake_case)]
    struct DRIVER_INFO_1W {
        pName: *mut u16,
    }

    #[link(name = "winspool")]
    extern "system" {
        fn EnumPrinterDriversW(
            pName: *const u16,
            pEnvironment: *const u16,
            Level: u32,
            pDriverInfo: *mut u8,
            cbBuf: u32,
            pcbNeeded: *mut u32,
            pcReturned: *mut u32,
        ) -> i32;
    }

    let mut drivers = Vec::new();

    unsafe {
        let mut needed: u32 = 0;
        let mut returned: u32 = 0;

        // First call to get required buffer size
        EnumPrinterDriversW(
            ptr::null(),
            ptr::null(),
            1,
            ptr::null_mut(),
            0,
            &mut needed,
            &mut returned,
        );

        if needed == 0 {
            return drivers;
        }

        // Allocate buffer and get driver info
        let mut buffer: Vec<u8> = vec![0; needed as usize];
        let result = EnumPrinterDriversW(
            ptr::null(),
            ptr::null(),
            1,
            buffer.as_mut_ptr(),
            needed,
            &mut needed,
            &mut returned,
        );

        if result == 0 {
            log::error!("EnumPrinterDriversW failed: {}", io::Error::last_os_error());
            return drivers;
        }

        // Parse results
        let driver_info = buffer.as_ptr() as *const DRIVER_INFO_1W;
        for i in 0..returned as isize {
            let info = &*driver_info.offset(i);
            if !info.pName.is_null() {
                let name = wide_string_to_string(info.pName);
                if !name.is_empty() {
                    drivers.push(name);
                }
            }
        }
    }

    log::info!("Found {} printer drivers", drivers.len());
    drivers
}

const PDF_PRINTER_DRIVER: &str = "microsoft print to pdf";

pub fn is_remote_printing_available() -> bool {
    static PDF_DRIVER_PRESENT: OnceLock<bool> = OnceLock::new();
    *PDF_DRIVER_PRESENT.get_or_init(|| {
        get_printer_drivers()
            .iter()
            .any(|d| d.to_lowercase() == PDF_PRINTER_DRIVER)
    })
}

/// Find a suitable printer driver for the virtual printer
fn find_suitable_driver() -> Option<String> {
    let drivers = get_printer_drivers();
    match drivers
        .iter()
        .find(|d| d.to_lowercase() == PDF_PRINTER_DRIVER)
    {
        Some(driver) => {
            log::info!("Using printer driver: {}", driver);
            Some(driver.clone())
        }
        None => {
            log::warn!(
                "No PDF printer driver found ({} drivers present); remote printing requires Windows 10 or newer",
                drivers.len()
            );
            None
        }
    }
}

/// Install the HopToDesk virtual printer
/// This creates a printer that redirects output to our named pipe
pub fn install_virtual_printer() -> ResultType<()> {
    if is_virtual_printer_installed() {
        log::info!("Virtual printer already installed");
        return Ok(());
    }

    // Find a suitable driver
    let driver = find_suitable_driver()
        .ok_or_else(|| anyhow!("No suitable printer driver found"))?;

    #[repr(C)]
    #[allow(non_snake_case)]
    struct PRINTER_INFO_2W {
        pServerName: *mut u16,
        pPrinterName: *mut u16,
        pShareName: *mut u16,
        pPortName: *mut u16,
        pDriverName: *mut u16,
        pComment: *mut u16,
        pLocation: *mut u16,
        pDevMode: *mut c_void,
        pSepFile: *mut u16,
        pPrintProcessor: *mut u16,
        pDatatype: *mut u16,
        pParameters: *mut u16,
        pSecurityDescriptor: *mut c_void,
        Attributes: u32,
        Priority: u32,
        DefaultPriority: u32,
        StartTime: u32,
        UntilTime: u32,
        Status: u32,
        cJobs: u32,
        AveragePPM: u32,
    }

    #[link(name = "winspool")]
    extern "system" {
        fn AddPrinterW(pName: *const u16, Level: u32, pPrinter: *const PRINTER_INFO_2W) -> HANDLE;
        fn XcvDataW(
            hXcv: HANDLE,
            pszDataName: *const u16,
            pInputData: *const u8,
            cbInputData: u32,
            pOutputData: *mut u8,
            cbOutputData: u32,
            pcbOutputNeeded: *mut u32,
            pdwStatus: *mut u32,
        ) -> i32;
        fn OpenPrinterW(pPrinterName: *const u16, phPrinter: *mut HANDLE, pDefault: *const c_void) -> i32;
        fn ClosePrinter(hPrinter: HANDLE) -> i32;
    }

    // First, add the local port using XcvData
    // Open the XcvMonitor for Local Port
    let xcv_name = wide_string(",XcvMonitor Local Port");
    let mut xcv_handle: HANDLE = std::ptr::null_mut();

    unsafe {
        // Open XcvMonitor handle with admin access
        #[repr(C)]
        #[allow(non_snake_case)]
        struct PRINTER_DEFAULTS {
            pDatatype: *mut u16,
            pDevMode: *mut c_void,
            DesiredAccess: u32,
        }

        const SERVER_ACCESS_ADMINISTER: u32 = 0x00000001;
        let defaults = PRINTER_DEFAULTS {
            pDatatype: std::ptr::null_mut(),
            pDevMode: std::ptr::null_mut(),
            DesiredAccess: SERVER_ACCESS_ADMINISTER,
        };

        if OpenPrinterW(xcv_name.as_ptr(), &mut xcv_handle, &defaults as *const _ as *const c_void) == 0 {
            log::warn!("Failed to open XcvMonitor: {}. Trying alternative method.", io::Error::last_os_error());
            // Fall back to using existing port or FILE: port
        } else {
            // Add the port using XcvData
            let port_name = wide_string(PRINTER_PIPE_NAME);
            let port_data: Vec<u8> = port_name.iter()
                .flat_map(|&w| w.to_le_bytes())
                .collect();

            let mut output_needed: u32 = 0;
            let mut status: u32 = 0;
            let add_port_cmd = wide_string("AddPort");

            let result = XcvDataW(
                xcv_handle,
                add_port_cmd.as_ptr(),
                port_data.as_ptr(),
                port_data.len() as u32,
                std::ptr::null_mut(),
                0,
                &mut output_needed,
                &mut status,
            );

            ClosePrinter(xcv_handle);

            if result == 0 && status != 0 {
                log::warn!("XcvData AddPort returned status: {}", status);
            }
        }

        // Now add the printer
        let mut printer_name = wide_string(VIRTUAL_PRINTER_NAME);
        let mut port_name = wide_string(PRINTER_PIPE_NAME);
        let mut driver_name = wide_string(&driver);
        let mut print_processor = wide_string("winprint");
        let mut datatype = wide_string("RAW");

        let printer_info = PRINTER_INFO_2W {
            pServerName: std::ptr::null_mut(),
            pPrinterName: printer_name.as_mut_ptr(),
            pShareName: std::ptr::null_mut(),
            pPortName: port_name.as_mut_ptr(),
            pDriverName: driver_name.as_mut_ptr(),
            pComment: std::ptr::null_mut(),
            pLocation: std::ptr::null_mut(),
            pDevMode: std::ptr::null_mut(),
            pSepFile: std::ptr::null_mut(),
            pPrintProcessor: print_processor.as_mut_ptr(),
            pDatatype: datatype.as_mut_ptr(),
            pParameters: std::ptr::null_mut(),
            pSecurityDescriptor: std::ptr::null_mut(),
            Attributes: 0,
            Priority: 0,
            DefaultPriority: 0,
            StartTime: 0,
            UntilTime: 0,
            Status: 0,
            cJobs: 0,
            AveragePPM: 0,
        };

        let printer_handle = AddPrinterW(std::ptr::null(), 2, &printer_info);
        if printer_handle.is_null() {
            let error = io::Error::last_os_error();
            // Error code 1796 means port doesn't exist, try with FILE: port
            if error.raw_os_error() == Some(1796) {
                log::info!("Port not found, using FILE: port instead");
                let mut file_port = wide_string("FILE:");
                let printer_info_file = PRINTER_INFO_2W {
                    pPortName: file_port.as_mut_ptr(),
                    ..printer_info
                };
                let printer_handle2 = AddPrinterW(std::ptr::null(), 2, &printer_info_file);
                if printer_handle2.is_null() {
                    bail!("Failed to add printer with FILE: port: {}", io::Error::last_os_error());
                }
                ClosePrinter(printer_handle2);
                log::info!("Virtual printer installed with FILE: port (user will select output location)");
                return Ok(());
            }
            bail!("Failed to add printer: {}", error);
        }

        ClosePrinter(printer_handle);
    }

    log::info!("Virtual printer installed successfully");
    Ok(())
}

pub fn uninstall_virtual_printer() -> ResultType<()> {
    if !is_virtual_printer_installed() {
        return Ok(());
    }

    #[link(name = "winspool")]
    extern "system" {
        fn OpenPrinterW(pPrinterName: *const u16, phPrinter: *mut HANDLE, pDefault: *const c_void) -> i32;
        fn DeletePrinter(hPrinter: HANDLE) -> i32;
        fn ClosePrinter(hPrinter: HANDLE) -> i32;
    }

    let printer_name = wide_string(VIRTUAL_PRINTER_NAME);

    unsafe {
        let mut printer_handle: HANDLE = std::ptr::null_mut();

        if OpenPrinterW(printer_name.as_ptr(), &mut printer_handle, std::ptr::null()) == 0 {
            log::warn!("Failed to open printer for deletion: {}", io::Error::last_os_error());
            return Ok(());
        }

        if DeletePrinter(printer_handle) == 0 {
            let error = io::Error::last_os_error();
            ClosePrinter(printer_handle);
            log::warn!("Failed to delete printer: {}", error);
            return Ok(());
        }

        ClosePrinter(printer_handle);
    }

    log::info!("Virtual printer uninstalled");
    Ok(())
}

pub fn attach_console_for_stdio() {
    use winapi::um::wincon::{AttachConsole, ATTACH_PARENT_PROCESS};
    use winapi::um::fileapi::{CreateFileA, GetFileType, OPEN_EXISTING};
    use winapi::um::processenv::{GetStdHandle, SetStdHandle};
    use winapi::um::winnt::{GENERIC_READ, GENERIC_WRITE, FILE_SHARE_READ, FILE_SHARE_WRITE};

    unsafe fn is_redirected(std_handle_id: DWORD) -> bool {
        let h = GetStdHandle(std_handle_id);
        if h.is_null() || h == winapi::um::handleapi::INVALID_HANDLE_VALUE {
            return false;
        }
        let ft = GetFileType(h);
        ft == FILE_TYPE_DISK || ft == FILE_TYPE_PIPE
    }

    unsafe {
        let stdin_redirected  = is_redirected(STD_INPUT_HANDLE);
        let stdout_redirected = is_redirected(STD_OUTPUT_HANDLE);
        let stderr_redirected = is_redirected(STD_ERROR_HANDLE);

        if stdin_redirected && stdout_redirected && stderr_redirected {
            return;
        }

        if AttachConsole(ATTACH_PARENT_PROCESS) == 0 {
            return;
        }

        if !stdin_redirected {
            let conin = CreateFileA(
                b"CONIN$\0".as_ptr() as *const i8,
                GENERIC_READ,
                FILE_SHARE_READ,
                std::ptr::null_mut(),
                OPEN_EXISTING,
                0,
                std::ptr::null_mut(),
            );
            if conin != winapi::um::handleapi::INVALID_HANDLE_VALUE {
                SetStdHandle(STD_INPUT_HANDLE, conin);
            }
        }
        if !stdout_redirected || !stderr_redirected {
            let conout = CreateFileA(
                b"CONOUT$\0".as_ptr() as *const i8,
                GENERIC_WRITE,
                FILE_SHARE_WRITE,
                std::ptr::null_mut(),
                OPEN_EXISTING,
                0,
                std::ptr::null_mut(),
            );
            if conout != winapi::um::handleapi::INVALID_HANDLE_VALUE {
                if !stdout_redirected {
                    SetStdHandle(STD_OUTPUT_HANDLE, conout);
                }
                if !stderr_redirected {
                    SetStdHandle(STD_ERROR_HANDLE, conout);
                }
            }
        }
    }
}

pub async fn reboot() -> ResultType<()> {
    system_shutdown::force_reboot().map_err(|err| hbb_common::anyhow::anyhow!("{}", err))
}
