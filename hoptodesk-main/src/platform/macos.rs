// https://developer.apple.com/documentation/appkit/nscursor
// https://github.com/servo/core-foundation-rs
// https://github.com/rust-windowing/winit

use super::{CursorData, ResultType};
use cocoa::{
    appkit::{NSApp, NSApplication, NSApplicationActivationPolicy::*},
    base::{id, nil, BOOL, NO, YES},
    foundation::{NSDictionary, NSPoint, NSSize, NSString, NSUInteger},
};
use core_foundation::{
    array::{CFArrayGetCount, CFArrayGetValueAtIndex},
    dictionary::CFDictionaryRef,
    string::CFStringRef,
};
use core_graphics::{
    display::{kCGNullWindowID, kCGWindowListOptionOnScreenOnly, CGWindowListCopyWindowInfo},
    window::{kCGWindowName, kCGWindowOwnerPID},
};
use hbb_common::{
	anyhow::anyhow,
    bail, log,
    message_proto::{DisplayInfo, Resolution},
    sysinfo::{Pid, Process, ProcessRefreshKind, System},
};
use include_dir::{include_dir, Dir};
use hbb_common::config::{Config, Config2};
use hbb_common::tokio;
use objc::rc::autoreleasepool;
use objc::{class, msg_send, sel, sel_impl};
use scrap::{libc::c_void, quartz::ffi::*};
use std::path::PathBuf;

static PRIVILEGES_SCRIPTS_DIR: Dir =
    include_dir!("$CARGO_MANIFEST_DIR/src/platform/privileges_scripts");
static mut LATEST_SEED: i32 = 0;

extern "C" {
    fn CGSCurrentCursorSeed() -> i32;
    fn CGEventCreate(r: *const c_void) -> *const c_void;
    fn CGEventGetLocation(e: *const c_void) -> CGPoint;
    static kAXTrustedCheckOptionPrompt: CFStringRef;
    fn AXIsProcessTrustedWithOptions(options: CFDictionaryRef) -> BOOL;
    fn InputMonitoringAuthStatus(_: BOOL) -> BOOL;
    fn IsCanScreenRecording(_: BOOL) -> BOOL;
    fn MicrophoneAuthStatus() -> i32;
    fn RequestMicrophoneAccess();
    fn OnScreenWindowsJson() -> *mut std::os::raw::c_char;
    fn CanUseNewApiForScreenCaptureCheck() -> BOOL;
    fn PostEventAuthStatus() -> BOOL;
    fn MacCheckAdminAuthorization() -> BOOL;
    fn MacGetModeNum(display: u32, numModes: *mut u32) -> BOOL;
    fn MacGetModes(
        display: u32,
        widths: *mut u32,
        heights: *mut u32,
        hidpis: *mut BOOL,
        max: u32,
        numModes: *mut u32,
    ) -> BOOL;
    fn majorVersion() -> u32;
    fn MacGetMode(display: u32, width: *mut u32, height: *mut u32) -> BOOL;
    fn MacSetMode(display: u32, width: u32, height: u32, tryHiDPI: bool) -> BOOL;
    fn MacSetBlockInput(on: bool) -> bool;
}

pub fn major_version() -> u32 {
    unsafe { majorVersion() }
}

pub fn is_process_trusted(prompt: bool) -> bool {
    autoreleasepool(|| unsafe_is_process_trusted(prompt))
}

fn unsafe_is_process_trusted(prompt: bool) -> bool {
    unsafe {
        let value = if prompt { YES } else { NO };
        let value: id = msg_send![class!(NSNumber), numberWithBool: value];
        let options = NSDictionary::dictionaryWithObject_forKey_(
            nil,
            value,
            kAXTrustedCheckOptionPrompt as _,
        );
        AXIsProcessTrustedWithOptions(options as _) == YES
    }
}

pub fn is_can_input_monitoring(prompt: bool) -> bool {
    unsafe {
        let value = if prompt { YES } else { NO };
        InputMonitoringAuthStatus(value) == YES
    }
}

const MICROPHONE_NOT_DETERMINED: i32 = 0;
const MICROPHONE_AUTHORIZED: i32 = 3;

pub fn is_microphone_authorized() -> bool {
    unsafe { MicrophoneAuthStatus() == MICROPHONE_AUTHORIZED }
}

pub fn microphone_access() -> &'static str {
    match unsafe { MicrophoneAuthStatus() } {
        MICROPHONE_AUTHORIZED => "allowed",
        MICROPHONE_NOT_DETERMINED => "undetermined",
        _ => "denied",
    }
}

pub fn request_microphone_access() {
    unsafe { RequestMicrophoneAccess() }
}

pub fn is_full_disk_access() -> bool {
    std::env::var_os("HOME").map_or(false, |home| {
        std::fs::File::open(PathBuf::from(home).join("Library/Application Support/com.apple.TCC/TCC.db")).is_ok()
    })
}

pub fn open_privacy_pane(pane: &str) {
    std::process::Command::new("open")
        .arg(format!("x-apple.systempreferences:com.apple.preference.security?{}", pane))
        .spawn()
        .ok();
}

pub fn on_screen_windows() -> Vec<serde_json::Value> {
    unsafe {
        let json = OnScreenWindowsJson();
        if json.is_null() {
            return vec![];
        }
        let windows = serde_json::from_slice(std::ffi::CStr::from_ptr(json).to_bytes()).unwrap_or_default();
        scrap::libc::free(json as _);
        windows
    }
}

pub fn is_can_screen_recording(prompt: bool) -> bool {
    autoreleasepool(|| unsafe_is_can_screen_recording(prompt))
}

// macOS >= 10.15
// https://stackoverflow.com/questions/56597221/detecting-screen-recording-settings-on-macos-catalina/
// remove just one app from all the permissions: tccutil reset All com.carriez.rustdesk
fn unsafe_is_can_screen_recording(prompt: bool) -> bool {
    // we got some report that we show no permission even after set it, so we try to use new api for screen recording check
    // the new api is only available on macOS >= 10.15, but on stackoverflow, some people said it works on >= 10.16 (crash on 10.15), 
    // but also some said it has bug on 10.16, so we just use it on 11.0.
    unsafe {
        if CanUseNewApiForScreenCaptureCheck() == YES {
            return IsCanScreenRecording(if prompt { YES } else { NO }) == YES;
        }
    }
    let mut can_record_screen: bool = false;
    unsafe {
        let our_pid: i32 = std::process::id() as _;
        let our_pid: id = msg_send![class!(NSNumber), numberWithInteger: our_pid];
        let window_list =
            CGWindowListCopyWindowInfo(kCGWindowListOptionOnScreenOnly, kCGNullWindowID);
        let n = CFArrayGetCount(window_list);
        let dock = NSString::alloc(nil).init_str("Dock");
        for i in 0..n {
            let w: id = CFArrayGetValueAtIndex(window_list, i) as _;
            let name: id = msg_send![w, valueForKey: kCGWindowName as id];
            if name.is_null() {
                continue;
            }
            let pid: id = msg_send![w, valueForKey: kCGWindowOwnerPID as id];
            let is_me: BOOL = msg_send![pid, isEqual: our_pid];
            if is_me == YES {
                continue;
            }
            let pid: i32 = msg_send![pid, intValue];
            let p: id = msg_send![
                class!(NSRunningApplication),
                runningApplicationWithProcessIdentifier: pid
            ];
            if p.is_null() {
                // ignore processes we don't have access to, such as WindowServer, which manages the windows named "Menubar" and "Backstop Menubar"
                continue;
            }
            let url: id = msg_send![p, executableURL];
            let exe_name: id = msg_send![url, lastPathComponent];
            if exe_name.is_null() {
                continue;
            }
            let is_dock: BOOL = msg_send![exe_name, isEqual: dock];
            if is_dock == YES {
                // ignore the Dock, which provides the desktop picture
                continue;
            }
            can_record_screen = true;
            break;
        }
    }
    if !can_record_screen && prompt {
        use scrap::{Capturer, Display};
        if let Ok(d) = Display::primary() {
            Capturer::new(d).ok();
        }
    }
    can_record_screen
}

pub fn install_service() -> bool {
    is_installed_daemon(false)
}

fn service_label() -> String {
    crate::get_full_name().replace(' ', "")
}

fn agent_plist_path() -> String {
    format!("/Library/LaunchAgents/{}_server.plist", service_label())
}

fn daemon_plist_path() -> String {
    format!("/Library/LaunchDaemons/{}_service.plist", service_label())
}

pub fn is_installed_daemon(prompt: bool) -> bool {
    let agent_plist_file = agent_plist_path();

	if !prompt {
        return std::path::Path::new(&agent_plist_file).exists()
            && !std::path::Path::new(&daemon_plist_path()).exists();
    }

    let Some(install_script) = PRIVILEGES_SCRIPTS_DIR.get_file("install.scpt") else {
        return false;
    };
    let Some(install_script_body) = install_script.contents_utf8().map(correct_app_name) else {
        return false;
    };

    let Some(agent_plist) = PRIVILEGES_SCRIPTS_DIR.get_file("agent.plist") else {
        return false;
    };
    let Some(agent_plist_body) = agent_plist.contents_utf8().map(correct_app_name) else {
        return false;
    };

    std::thread::spawn(move || {
        match std::process::Command::new("osascript")
            .arg("-e")
            .arg(install_script_body)
            .arg(agent_plist_body)
            .status()
        {
            Err(e) => {
                log::error!("run osascript failed: {}", e);
            }
            _ => {
                let installed = std::path::Path::new(&agent_plist_file).exists();
                log::info!("Agent file {} installed: {}", agent_plist_file, installed);
                if installed {
                    log::info!("launch server");
                    std::process::Command::new("launchctl")
                        .args(&["load", "-w", &agent_plist_file])
                        .status()
                        .ok();
                }
            }
        }
    });
    false
}

fn correct_app_name(s: &str) -> String {
    let s = s.replace("hoptodesk", &crate::get_app_name_lower());
    let s = s.replace("HopToDesk", &crate::get_app_name().replace(' ', ""));
    s
}

pub fn uninstall_service(show_new_window: bool, sync: bool) -> bool {
    // to-do: do together with win/linux about refactory start/stop service
    if !std::path::Path::new(&agent_plist_path()).exists() {
        return false;
    }

    let Some(script_file) = PRIVILEGES_SCRIPTS_DIR.get_file("uninstall.scpt") else {
        return false;
    };
    let Some(script_body) = script_file.contents_utf8().map(correct_app_name) else {
        return false;
    };

    let func = move || {
        match std::process::Command::new("osascript")
            .arg("-e")
            .arg(script_body)
            .status()
        {
            Err(e) => {
                log::error!("run osascript failed: {}", e);
            }
            _ => {
                let agent_plist_file = agent_plist_path();
                let uninstalled = !std::path::Path::new(&agent_plist_file).exists();
                log::info!(
                    "Agent file {} uninstalled: {}",
                    agent_plist_file,
                    uninstalled
                );
                if uninstalled {
                    if !show_new_window {
                        let _ = crate::ipc::close_all_instances();
                        // leave ipc a little time
                        std::thread::sleep(std::time::Duration::from_millis(300));
                    }
                    crate::ipc::set_option("stop-service", "Y");
                    std::process::Command::new("launchctl")
                        .args(&["remove", &format!("{}_server", service_label())])
                        .status()
                        .ok();
                    if show_new_window {
                        std::process::Command::new("open")
                            .arg("-n")
                            .arg(&format!("/Applications/{}.app", crate::get_app_name()))
                            .spawn()
                            .ok();
                        // leave open a little time
                        std::thread::sleep(std::time::Duration::from_millis(300));
                    }
                    quit_gui();
                }
            }
        }
    };
    if sync {
        func();
    } else {
        std::thread::spawn(func);
    }
    true
}

pub fn get_cursor_pos() -> Option<(i32, i32)> {
    unsafe {
        let e = CGEventCreate(0 as _);
        let point = CGEventGetLocation(e);
        CFRelease(e);
        Some((point.x as _, point.y as _))
    }
}

pub fn get_focused_display(displays: Vec<DisplayInfo>) -> Option<usize> {
    autoreleasepool(|| unsafe_get_focused_display(displays))
}

fn unsafe_get_focused_display(displays: Vec<DisplayInfo>) -> Option<usize> {
    unsafe {
        let main_screen: id = msg_send![class!(NSScreen), mainScreen];
        let screen: id = msg_send![main_screen, deviceDescription];
        let id: id =
            msg_send![screen, objectForKey: NSString::alloc(nil).init_str("NSScreenNumber")];
        let display_name: u32 = msg_send![id, unsignedIntValue];

        displays
            .iter()
            .position(|d| d.name == display_name.to_string())
    }
}


pub fn get_cursor() -> ResultType<Option<u64>> {
    autoreleasepool(|| unsafe_get_cursor())
}

fn unsafe_get_cursor() -> ResultType<Option<u64>> {
	    unsafe {
        let seed = CGSCurrentCursorSeed();
        if seed == LATEST_SEED {
            return Ok(None);
        }
        LATEST_SEED = seed;
    }
    let c = get_cursor_id()?;
    Ok(Some(c.1))
}

pub fn reset_input_cache() {
    unsafe {
        LATEST_SEED = 0;
    }
}

fn get_cursor_id() -> ResultType<(id, u64)> {
    unsafe {
        let c: id = msg_send![class!(NSCursor), currentSystemCursor];
        if c == nil {
            bail!("Failed to call [NSCursor currentSystemCursor]");
        }
        let hotspot: NSPoint = msg_send![c, hotSpot];
        let img: id = msg_send![c, image];
        if img == nil {
            bail!("Failed to call [NSCursor image]");
        }
        let size: NSSize = msg_send![img, size];
        let tif: id = msg_send![img, TIFFRepresentation];
        if tif == nil {
            bail!("Failed to call [NSImage TIFFRepresentation]");
        }
        let rep: id = msg_send![class!(NSBitmapImageRep), imageRepWithData: tif];
        if rep == nil {
            bail!("Failed to call [NSBitmapImageRep imageRepWithData]");
        }
        let rep_size: NSSize = msg_send![rep, size];
        let mut hcursor =
            size.width + size.height + hotspot.x + hotspot.y + rep_size.width + rep_size.height;
        let x = (rep_size.width * hotspot.x / size.width) as usize;
        let y = (rep_size.height * hotspot.y / size.height) as usize;
        for i in 0..2 {
            let mut x2 = x + i;
            if x2 >= rep_size.width as usize {
                x2 = rep_size.width as usize - 1;
            }
            let mut y2 = y + i;
            if y2 >= rep_size.height as usize {
                y2 = rep_size.height as usize - 1;
            }
            let color: id = msg_send![rep, colorAtX:x2 y:y2];
            if color != nil {
                let r: f64 = msg_send![color, redComponent];
                let g: f64 = msg_send![color, greenComponent];
                let b: f64 = msg_send![color, blueComponent];
                let a: f64 = msg_send![color, alphaComponent];
                hcursor += (r + g + b + a) * (255 << i) as f64;
            }
        }
        Ok((c, hcursor as _))
    }
}

pub fn get_cursor_data(hcursor: u64) -> ResultType<CursorData> {
    autoreleasepool(|| unsafe_get_cursor_data(hcursor))
}

// https://github.com/stweil/OSXvnc/blob/master/OSXvnc-server/mousecursor.c
fn unsafe_get_cursor_data(hcursor: u64) -> ResultType<CursorData> {
    unsafe {
        let (c, hcursor2) = get_cursor_id()?;
        if hcursor != hcursor2 {
            bail!("cursor changed");
        }
        let hotspot: NSPoint = msg_send![c, hotSpot];
        let img: id = msg_send![c, image];
        let size: NSSize = msg_send![img, size];
        let reps: id = msg_send![img, representations];
        if reps == nil {
            bail!("Failed to call [NSImage representations]");
        }
        let nreps: usize = msg_send![reps, count];
        if nreps == 0 {
            bail!("Get empty [NSImage representations]");
        }
        let rep: id = msg_send![reps, objectAtIndex: 0];
        let mut colors: Vec<u8> = Vec::new();
        colors.reserve((size.height * size.width) as usize * 4);
        // TIFF is rgb colorspace, no need to convert
        // let cs: id = msg_send![class!(NSColorSpace), sRGBColorSpace];
        for y in 0..(size.height as _) {
            for x in 0..(size.width as _) {
                let color: id = msg_send![rep, colorAtX:x as cocoa::foundation::NSInteger y:y as cocoa::foundation::NSInteger];
                if color == nil {
                    continue;
                }
                let r: f64 = msg_send![color, redComponent];
                let g: f64 = msg_send![color, greenComponent];
                let b: f64 = msg_send![color, blueComponent];
                let a: f64 = msg_send![color, alphaComponent];
                colors.push((r * 255.) as _);
                colors.push((g * 255.) as _);
                colors.push((b * 255.) as _);
                colors.push((a * 255.) as _);
            }
        }
        Ok(CursorData {
            id: hcursor,
            colors: colors.into(),
            hotx: hotspot.x as _,
            hoty: hotspot.y as _,
            width: size.width as _,
            height: size.height as _,
            ..Default::default()
        })
    }
}

fn get_active_user(t: &str) -> String {
    if let Ok(output) = std::process::Command::new("ls")
        .args(vec![t, "/dev/console"])
        .output()
    {
        for line in String::from_utf8_lossy(&output.stdout).lines() {
            if let Some(n) = line.split_whitespace().nth(2) {
                return n.to_owned();
            }
        }
    }
    "".to_owned()
}

pub fn is_logon_screen() -> bool {
    is_prelogin() || is_locked()
}

pub fn get_active_username() -> String {
    get_active_user("-l")
}

pub fn get_active_userid() -> String {
    get_active_user("-n")
}

pub fn get_active_user_home() -> Option<PathBuf> {
    let username = get_active_username();
    if !username.is_empty() {
        let home = PathBuf::from(format!("/Users/{}", username));
        if home.exists() {
            return Some(home);
        }
    }
    None
}

pub fn is_prelogin() -> bool {
    get_active_userid() == "0"
}

// https://stackoverflow.com/questions/11505255/osx-check-if-the-screen-is-locked
// No "CGSSessionScreenIsLocked" can be found when macOS is not locked.
//
// `ioreg -n Root -d1` returns `"CGSSessionScreenIsLocked"=Yes`
// `ioreg -n Root -d1 -a` returns
// ```
// ...
//    <key>CGSSessionScreenIsLocked</key>
//    <true/>
// ...
// ```
pub fn is_locked() -> bool {
    match std::process::Command::new("ioreg")
        .arg("-n")
        .arg("Root")
        .arg("-d1")
        .output()
    {
        Ok(output) => {
            let output_str = String::from_utf8_lossy(&output.stdout);
            // Although `"CGSSessionScreenIsLocked"=Yes` was printed on my macOS,
            // I also check `"CGSSessionScreenIsLocked"=true` for better compability.
            output_str.contains("\"CGSSessionScreenIsLocked\"=Yes")
                || output_str.contains("\"CGSSessionScreenIsLocked\"=true")
        }
        Err(e) => {
            log::error!("Failed to query ioreg for the lock state: {}", e);
            false
        }
    }
}

pub fn is_root() -> bool {
    crate::username() == "root"
}

pub fn run_as_user(arg: Vec<&str>) -> ResultType<Option<std::process::Child>> {
    let uid = get_active_userid();
    let cmd = std::env::current_exe()?;
    let mut args = vec!["asuser", &uid, cmd.to_str().unwrap_or("")];
    args.append(&mut arg.clone());
    let task = std::process::Command::new("launchctl").args(args).spawn()?;
    Ok(Some(task))
}

pub fn lock_screen() {
    std::process::Command::new(
        "/System/Library/CoreServices/Menu Extras/User.menu/Contents/Resources/CGSession",
    )
    .arg("-suspend")
    .output()
    .ok();
}

pub fn start_os_service() {
    log::info!("Username: {}", crate::username());
    if let Err(err) = crate::ipc::start("_service") {
        log::error!("Failed to start ipc_service: {}", err);
    }

}

pub fn block_input(v: bool) -> (bool, String) {
    (unsafe { MacSetBlockInput(v) }, "".to_owned())
}

pub fn is_installed() -> bool {
    if let Ok(p) = std::env::current_exe() {
        return p
            .to_str()
            .unwrap_or_default()
            .starts_with(&format!("/Applications/{}.app", crate::get_app_name()));
    }
    false
}

pub fn quit_gui() {
    unsafe {
        let app = cocoa::appkit::NSApp();
        let () = msg_send![app, hide: nil];
        std::thread::sleep(std::time::Duration::from_millis(50));
        let () = msg_send![app, terminate: nil];
    }
}

pub fn get_double_click_time() -> u32 {
    // to-do: https://github.com/servo/core-foundation-rs/blob/786895643140fa0ee4f913d7b4aeb0c4626b2085/cocoa/src/appkit.rs#L2823
    500 as _
}

pub fn hide_dock() {
    unsafe {
        NSApp().setActivationPolicy_(NSApplicationActivationPolicyAccessory);
    }
}

pub fn disable_app_nap() {
    unsafe {
        let process_info: id = msg_send![class!(NSProcessInfo), processInfo];
        let reason = NSString::alloc(nil).init_str("HopToDesk session active");
        let activity: id =
            msg_send![process_info, beginActivityWithOptions: 0x00EFFFFFu64 reason: reason];
        let _: id = msg_send![activity, retain];
    }
}

pub fn set_cm_window_over_fullscreen(view: *mut c_void) {
    if view.is_null() {
        return;
    }
    unsafe {
        let view: id = view as id;
        let window: id = msg_send![view, window];
        if window == nil {
            return;
        }
        let can_join_all_spaces: NSUInteger = 1 << 0;
        let full_screen_auxiliary: NSUInteger = 1 << 8;
        let current: NSUInteger = msg_send![window, collectionBehavior];
        let _: () =
            msg_send![window, setCollectionBehavior: current | can_join_all_spaces | full_screen_auxiliary];
        log::info!("[CM] over-fullscreen window behavior + accessory/no-activate applied");
    }
}

#[inline]
fn get_server_start_time_of(p: &Process, path: &PathBuf) -> Option<i64> {
    let cmd = p.cmd();
    if cmd.len() <= 1 {
        return None;
    }
    if &cmd[1] != "--server" {
        return None;
    }
    let Ok(cur) = std::fs::canonicalize(p.exe()) else {
        return None;
    };
    if &cur != path {
        return None;
    }
    Some(p.start_time() as _)
}

#[inline]
#[allow(dead_code)]
fn get_server_start_time(sys: &mut System, path: &PathBuf) -> Option<(i64, Pid)> {
    sys.refresh_processes_specifics(ProcessRefreshKind::new());
    for (_, p) in sys.processes() {
        if let Some(t) = get_server_start_time_of(p, path) {
            return Some((t, p.pid() as _));
        }
    }
    None
}

pub fn handle_application_should_open_untitled_file() {
    hbb_common::log::debug!("icon clicked on finder");
    let x = std::env::args().nth(1).unwrap_or_default();
    if x == "--server" || x == "--cm" || x == "--tray" {
        std::thread::spawn(move || crate::handle_url_scheme("".to_lowercase()));
    }
}

pub fn resolutions(name: &str) -> Vec<Resolution> {
    let mut v = vec![];
    if let Ok(display) = name.parse::<u32>() {
        let mut num = 0;
        unsafe {
            if YES == MacGetModeNum(display, &mut num) {
                let (mut widths, mut heights, mut _hidpis) =
                    (vec![0; num as _], vec![0; num as _], vec![NO; num as _]);
                let mut real_num = 0;
                if YES
                    == MacGetModes(
                        display,
                        widths.as_mut_ptr(),
                        heights.as_mut_ptr(),
                        _hidpis.as_mut_ptr(),
                        num,
                        &mut real_num,
                    )
                {
                    if real_num <= num {
                        v = (0..real_num)
                            .map(|i| Resolution {
                                width: widths[i as usize] as _,
                                height: heights[i as usize] as _,
                                ..Default::default()
                            })
                            .collect::<Vec<_>>();
                        // Sort by (w, h), desc
                        v.sort_by(|a, b| {
                            if a.width == b.width {
                                b.height.cmp(&a.height)
                            } else {
                                b.width.cmp(&a.width)
                            }
                        });
                        // Remove duplicates
                        v.dedup_by(|a, b| a.width == b.width && a.height == b.height);
                        // Filter out the ones that are less than width 800 (800x600) if there are too many.
                        // We can also do this filtering on the client side, but it is better not to change the client side to reduce the impact.
                        if v.len() > 15 {
                            // Most width > 800, so it's ok to remove the small ones.
                            v.retain(|r| r.width >= 800);
                        }
                        if v.len() > 15 {
                            // Ignore if the length is still too long.
                        }
                    }
                }
            }
        }
    }
    v
}

pub fn current_resolution(name: &str) -> ResultType<Resolution> {
    let display = name.parse::<u32>().map_err(|e| anyhow!(e))?;
    unsafe {
        let (mut width, mut height) = (0, 0);
        if NO == MacGetMode(display, &mut width, &mut height) {
            bail!("MacGetMode failed");
        }
        Ok(Resolution {
            width: width as _,
            height: height as _,
            ..Default::default()
        })
    }
}

pub fn change_resolution_directly(name: &str, width: usize, height: usize) -> ResultType<()> {
    let display = name.parse::<u32>().map_err(|e| anyhow!(e))?;
    unsafe {
        if NO == MacSetMode(display, width as _, height as _, true) {
            bail!("MacSetMode failed");
        }
    }
    Ok(())
}

pub fn check_super_user_permission() -> ResultType<bool> {
    unsafe { Ok(MacCheckAdminAuthorization() == YES) }
}

pub fn elevate(args: Vec<&str>, prompt: &str) -> ResultType<bool> {
    let cmd = std::env::current_exe()?;
    match cmd.to_str() {
        Some(cmd) => {
            let mut cmd_with_args = cmd.to_string();
            for arg in args {
                cmd_with_args = format!("{} {}", cmd_with_args, arg);
            }
            let script = format!(
                r#"do shell script "{}" with prompt "{}" with administrator privileges"#,
                cmd_with_args, prompt
            );
            match std::process::Command::new("osascript")
                .arg("-e")
                .arg(script)
                .arg(&get_active_username())
                .status()
            {
                Err(e) => {
                    bail!("Failed to run osascript: {}", e);
                }
                Ok(status) => Ok(status.success() && status.code() == Some(0)),
            }
        }
        None => {
            bail!("Failed to get current exe str");
        }
    }
}

pub struct WakeLock(#[allow(dead_code)] Option<()>);

impl WakeLock {
    pub fn new(_display: bool, _idle: bool, _sleep: bool) -> Self {
        WakeLock(
			Some(())
        )
    }

    pub fn set_display(&mut self, _display: bool) -> ResultType<()> {
        Ok(())
    }
}

// ── Remote Printing (macOS via CUPS) ──

pub fn virtual_printer_name() -> String {
    format!("{}-Printer", crate::get_app_name().replace(' ', ""))
}

fn cups_backend_path() -> String {
    format!("/usr/libexec/cups/backend/{}", crate::get_app_name_lower())
}

fn cups_backend_script() -> String {
    let scheme = crate::get_app_name_lower();
    format!(
        r#"#!/bin/sh
# HopToDesk virtual printer backend — pipes print data to Unix socket
if [ $# -eq 0 ]; then
    echo "direct {scheme} \"Unknown\" \"HopToDesk Printer\""
    exit 0
fi
SOCKET_OWNER=$(/usr/bin/stat -f %u /tmp/hoptodesk-printer.sock 2>/dev/null)
CONSOLE_OWNER=$(/usr/bin/stat -f %u /dev/console 2>/dev/null)
if [ -n "$SOCKET_OWNER" ] && [ -n "$CONSOLE_OWNER" ] && [ "$SOCKET_OWNER" != "0" ] && [ "$SOCKET_OWNER" != "$CONSOLE_OWNER" ]; then
    exit 0
fi
# CUPS passes filename as $6 if available, otherwise data is on stdin
if [ -n "$6" ]; then
    cat "$6" | /usr/bin/nc -U /tmp/hoptodesk-printer.sock
else
    cat | /usr/bin/nc -U /tmp/hoptodesk-printer.sock
fi
exit 0
"#
    )
}

const CUPS_PPD_CONTENT: &str = r#"*PPD-Adobe: "4.3"
*FormatVersion: "4.3"
*FileVersion: "1.0"
*LanguageVersion: English
*Manufacturer: "HopToDesk"
*ModelName: "HopToDesk Printer"
*ShortNickName: "HopToDesk Printer"
*NickName: "HopToDesk Printer"
*PCFileName: "hoptodesk.ppd"
*Product: "(HopToDesk Printer)"
*cupsFilter: "application/pdf 0 -"
*ColorDevice: True
*DefaultColorSpace: RGB
*FileSystem: False
*Throughput: "1"
*LandscapeOrientation: Plus90
*TTRasterizer: Type42
*OpenUI *PageSize/Page Size: PickOne
*DefaultPageSize: Letter
*PageSize Letter/US Letter: "<</PageSize[612 792]/ImagingBBox null>>setpagedevice"
*PageSize A4/A4: "<</PageSize[595 842]/ImagingBBox null>>setpagedevice"
*CloseUI: *PageSize
*OpenUI *PageRegion: PickOne
*DefaultPageRegion: Letter
*PageRegion Letter/US Letter: "<</PageSize[612 792]/ImagingBBox null>>setpagedevice"
*PageRegion A4/A4: "<</PageSize[595 842]/ImagingBBox null>>setpagedevice"
*CloseUI: *PageRegion
*DefaultImageableArea: Letter
*ImageableArea Letter/US Letter: "0 0 612 792"
*ImageableArea A4/A4: "0 0 595 842"
*DefaultPaperDimension: Letter
*PaperDimension Letter/US Letter: "612 792"
*PaperDimension A4/A4: "595 842"
"#;

/// List available printers via `lpstat -a`
pub fn get_printers() -> Vec<String> {
    let output = match std::process::Command::new("lpstat").arg("-a").output() {
        Ok(o) => o,
        Err(e) => {
            log::warn!("lpstat failed: {}", e);
            return Vec::new();
        }
    };
    let stdout = String::from_utf8_lossy(&output.stdout);
    let own = virtual_printer_name();
    stdout
        .lines()
        .filter_map(|line| {
            // Format: "PrinterName accepting requests since ..."
            let name = line.split_whitespace().next()?;
            if name == own {
                None // hide our virtual printer
            } else {
                Some(name.to_string())
            }
        })
        .collect()
}

/// Get the default printer via `lpstat -d`
pub fn get_default_printer() -> Option<String> {
    let output = std::process::Command::new("lpstat").arg("-d").output().ok()?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    // Format: "system default destination: PrinterName"
    stdout.trim().rsplit(": ").next().map(|s| s.to_string())
}

/// Send print data (PDF) to a local printer
pub fn send_raw_data_to_printer(printer: Option<String>, data: Vec<u8>) -> ResultType<()> {
    let tmp_path = hbb_common::fs::write_private_temp_file("HopToDesk_Print", "pdf", &data)?;

    let mut cmd = std::process::Command::new("lp");
    if let Some(ref name) = printer {
        cmd.arg("-d").arg(name);
    }
    cmd.arg(&tmp_path);

    let status = cmd.status()?;
    let _ = std::fs::remove_file(&tmp_path);
    if status.success() {
        log::info!("Print job sent successfully");
        Ok(())
    } else {
        bail!("lp command failed with status {}", status)
    }
}

/// Check if our virtual printer is installed
pub fn is_virtual_printer_installed() -> bool {
    std::process::Command::new("lpstat")
        .arg("-p")
        .arg(virtual_printer_name())
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Install the CUPS virtual printer backend + printer.
/// Skips if already installed with current backend. One-time admin prompt.
pub fn install_virtual_printer() -> ResultType<()> {
    if is_virtual_printer_installed() {
        // Check if backend script is current — if not, we need to update
        let needs_update = std::fs::read_to_string(cups_backend_path())
            .map(|content| content != cups_backend_script())
            .unwrap_or(true);
        if !needs_update {
            log::info!("Virtual printer already installed and up to date");
            return Ok(());
        }
        log::info!("Virtual printer installed but backend outdated, updating...");
    }

    // Write backend script and PPD to temp files
    let tmp_script = hbb_common::fs::write_private_temp_file(
        "hoptodesk-cups-backend",
        "sh",
        cups_backend_script().as_bytes(),
    )?;
    let tmp_ppd = hbb_common::fs::write_private_temp_file(
        "hoptodesk-cups",
        "ppd",
        CUPS_PPD_CONTENT.as_bytes(),
    )?;

    let install_cmd = format!(
        r#"do shell script "cp '{tmp_script}' {backend} && chmod 700 {backend} && /usr/sbin/lpadmin -p {printer} -v {uri} -E -P '{tmp_ppd}' -o printer-is-shared=false" with administrator privileges"#,
        tmp_script = tmp_script.display(),
        backend = cups_backend_path(),
        tmp_ppd = tmp_ppd.display(),
        printer = virtual_printer_name(),
        uri = crate::get_uri_prefix(),
    );

    let status = std::process::Command::new("osascript")
        .arg("-e")
        .arg(&install_cmd)
        .status()?;

    let _ = std::fs::remove_file(&tmp_script);
    let _ = std::fs::remove_file(&tmp_ppd);

    if status.success() {
        log::info!("Virtual printer installed successfully");
        Ok(())
    } else {
        bail!("Failed to install virtual printer (osascript exit {})", status)
    }
}

/// Uninstall the CUPS virtual printer
pub fn uninstall_virtual_printer() -> ResultType<()> {
    if !is_virtual_printer_installed() {
        return Ok(());
    }

    let cmd = format!(
        r#"do shell script "/usr/sbin/lpadmin -x {} && rm -f {}" with administrator privileges"#,
        virtual_printer_name(), cups_backend_path(),
    );

    let status = std::process::Command::new("osascript")
        .arg("-e")
        .arg(&cmd)
        .status()?;

    if status.success() {
        log::info!("Virtual printer uninstalled");
        Ok(())
    } else {
        bail!("Failed to uninstall virtual printer")
    }
}

fn prelogin_owner_config_dirs() -> Vec<PathBuf> {
    let app_name = hbb_common::config::APP_NAME.read().unwrap().clone();
    let org = hbb_common::config::ORG.read().unwrap().clone();
    let sub = format!("Library/Preferences/{}.{}", org, app_name);
    let mut found = Vec::new();
    if let Ok(entries) = std::fs::read_dir("/Users") {
        for entry in entries.flatten() {
            let dir = entry.path().join(&sub);
            if let Ok(meta) = std::fs::metadata(dir.join(format!("{}.toml", app_name))) {
                if let Ok(modified) = meta.modified() {
                    found.push((modified, dir));
                }
            }
        }
    }
    found.sort_by(|a, b| b.0.cmp(&a.0));
    found.into_iter().map(|(_, dir)| dir).collect()
}

#[tokio::main(flavor = "current_thread")]
pub async fn sync_prelogin_identity() -> bool {
    if crate::ipc::connect(300, "").await.is_ok() {
        return false;
    }
    for dir in prelogin_owner_config_dirs() {
        let Some(config) = Config::load_from_dir(&dir) else {
            continue;
        };
        Config::set(config);
        if let Some(config2) = Config2::load_from_dir(&dir) {
            Config2::set(config2);
        }
        for name in ["LastToken.toml", "TeamID.toml"] {
            let src = dir.join(name);
            if src.is_file() {
                std::fs::copy(&src, Config::path(name)).ok();
            }
        }
        log::info!("prelogin identity mirrored from {}", dir.display());
        break;
    }
    unsafe {
        if PostEventAuthStatus() != YES {
            log::warn!("post event access denied, input will not work before login");
        }
    }
    true
}

pub async fn reboot() -> ResultType<()> {
    system_shutdown::reboot().map_err(|err| anyhow!("{}", err))
}
