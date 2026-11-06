//! Beacon LAN build stub: the remote printer driver is not bundled.
//! Keeps the FFI surface compiling; always reports "not installed".

use hbb_common::ResultType;

#[cfg(target_os = "windows")]
pub fn is_rd_printer_installed(_app_name: &str) -> ResultType<bool> {
    Ok(false)
}
