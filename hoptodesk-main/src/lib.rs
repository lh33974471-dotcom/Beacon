#![recursion_limit = "256"]
mod keyboard;
/// cbindgen:ignore
pub mod platform;
mod rendezvous_ws;
#[cfg(not(any(target_os = "android", target_os = "ios")))]
pub use platform::{
    get_cursor, get_cursor_data, get_cursor_pos, get_focused_display, start_os_service,
};
/// cbindgen:ignore
mod server;
pub use self::server::*;
mod client;
mod lan;
mod rendezvous_mediator;
pub use self::rendezvous_mediator::*;
/// cbindgen:ignore
pub mod common;
pub mod ipc;
mod version;
pub use version::*;
#[cfg(feature = "fips")]
pub mod fips;
#[cfg(any(target_os = "android", target_os = "ios", feature = "flutter"))]
mod bridge_generated;
#[cfg(any(target_os = "android", target_os = "ios", feature = "flutter"))]
pub mod flutter;
#[cfg(any(target_os = "android", target_os = "ios", feature = "flutter"))]
pub mod flutter_ffi;
use common::*;
mod auth_2fa;
#[cfg(feature = "cli")]
pub mod cli;
#[cfg(not(target_os = "ios"))]
mod clipboard;
#[cfg(not(any(target_os = "android", target_os = "ios", feature = "cli")))]
pub mod core_main;
mod lang;
#[cfg(windows)]
mod send_file;
#[cfg(not(any(target_os = "android", target_os = "ios")))]
mod port_forward;

#[cfg(all(feature = "flutter", feature = "plugin_framework"))]
#[cfg(not(any(target_os = "android", target_os = "ios")))]
pub mod plugin;

#[cfg(not(any(target_os = "android", target_os = "ios")))]
mod tray;

#[cfg(not(any(target_os = "android", target_os = "ios")))]
mod whiteboard;

pub mod lan_only;
mod remote_printer;
pub mod dashboard;
pub mod desk;
#[cfg(not(any(target_os = "android", target_os = "ios", feature = "flutter")))]
pub mod invite_file;
#[cfg(target_os = "android")]
pub mod mcp_android;
mod rendezvous_messages;
mod turn_client;
mod ui_cm_interface;
mod ui_interface;
mod ui_session_interface;

#[cfg(any(target_os = "windows", target_os = "linux", target_os = "freebsd", target_os = "macos"))]
pub mod clipboard_file;

pub mod privacy_mode;

#[cfg(windows)]
pub mod virtual_display_manager;

