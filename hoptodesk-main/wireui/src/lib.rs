#![allow(non_camel_case_types)]

#[macro_use]
pub mod macros;

pub mod bridge;
pub mod capi;
pub mod dom;
pub mod engine;
pub mod host;
pub mod script;
pub mod value;
pub mod video;
pub mod window;

pub mod types {
    pub use crate::capi::sctypes::*;
}

pub use capi::scdef::{GFX_LAYER, SCRIPT_RUNTIME_FEATURES};
pub use capi::scdom::HELEMENT;
pub use dom::event::EventHandler;
pub use dom::Element;
pub use host::{Archive, Host, HostHandler};
pub use value::{FromValue, Value};
pub use window::Window;

pub type WindowBuilder = window::Builder;

#[derive(Copy, Clone)]
pub enum RuntimeOptions<'a> {
    LibraryPath(&'a str),
    GfxLayer(GFX_LAYER),
    UxTheming(bool),
    DebugMode(bool),
    ScriptFeatures(u8),
    ConnectionTimeout(u32),
    OnHttpsError(u8),
    InitScript(&'a str),
    MaxHttpDataLength(usize),
    LogicalPixel(bool),
}

pub fn set_library(custom_path: &str) -> std::result::Result<(), String> {
    let _ = custom_path;
    Ok(())
}

pub fn set_options(options: RuntimeOptions) -> std::result::Result<(), ()> {
    let _ = options;
    Ok(())
}

static STATE_DIR: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();

pub fn set_state_dir(dir: std::path::PathBuf) {
    let _ = STATE_DIR.set(dir);
}

pub fn state_dir() -> std::path::PathBuf {
    STATE_DIR.get().cloned().unwrap_or_else(std::env::temp_dir)
}

static CPU_BACKEND_FORCED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn force_cpu_backend() {
    CPU_BACKEND_FORCED.store(true, std::sync::atomic::Ordering::Relaxed);
}

pub fn cpu_backend_forced() -> bool {
    CPU_BACKEND_FORCED.load(std::sync::atomic::Ordering::Relaxed)
}

pub fn wait_for_relaunching_parent() {
    let Some(pid) = std::env::var("WIREUI_RELAUNCH_FROM").ok().and_then(|v| v.parse::<u32>().ok()) else {
        return;
    };
    std::env::remove_var("WIREUI_RELAUNCH_FROM");
    wait_for_exit(pid, std::time::Duration::from_secs(5));
}

#[cfg(windows)]
fn wait_for_exit(pid: u32, timeout: std::time::Duration) {
    #[link(name = "kernel32")]
    extern "system" {
        fn OpenProcess(access: u32, inherit: i32, pid: u32) -> *mut std::ffi::c_void;
        fn WaitForSingleObject(handle: *mut std::ffi::c_void, ms: u32) -> u32;
        fn CloseHandle(handle: *mut std::ffi::c_void) -> i32;
    }
    const SYNCHRONIZE: u32 = 0x0010_0000;
    unsafe {
        let handle = OpenProcess(SYNCHRONIZE, 0, pid);
        if !handle.is_null() {
            WaitForSingleObject(handle, timeout.as_millis() as u32);
            CloseHandle(handle);
        }
    }
}

#[cfg(unix)]
fn wait_for_exit(pid: u32, timeout: std::time::Duration) {
    extern "C" {
        fn kill(pid: i32, sig: i32) -> i32;
    }
    let start = std::time::Instant::now();
    while start.elapsed() < timeout && unsafe { kill(pid as i32, 0) } == 0 {
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

pub fn version() -> String {
    "wireui 0.0.1".to_owned()
}

pub fn version_num() -> u64 {
    0x0401_0A00
}
