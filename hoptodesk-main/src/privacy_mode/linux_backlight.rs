use hbb_common::{bail, log, ResultType};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

const BACKLIGHT_ROOT: &str = "/sys/class/backlight";

lazy_static::lazy_static! {
    static ref SAVED: Mutex<Vec<(PathBuf, String)>> = Mutex::new(Vec::new());
    static ref GRABBED: Mutex<Vec<evdev::Device>> = Mutex::new(Vec::new());
}

pub fn backlight_devices() -> Vec<PathBuf> {
    let mut devices = Vec::new();
    let entries = match std::fs::read_dir(BACKLIGHT_ROOT) {
        Ok(entries) => entries,
        Err(_) => return devices,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.join("brightness").exists() && path.join("max_brightness").exists() {
            devices.push(path);
        }
    }
    devices
}

fn read_trimmed(path: &Path) -> ResultType<String> {
    Ok(std::fs::read_to_string(path)?.trim().to_owned())
}

fn physical_input_devices() -> Vec<(PathBuf, evdev::Device)> {
    let mut devices = Vec::new();
    let entries = match std::fs::read_dir("/dev/input") {
        Ok(entries) => entries,
        Err(_) => return devices,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path
            .file_name()
            .map(|n| n.to_string_lossy().starts_with("event"))
            .unwrap_or(false)
        {
            continue;
        }
        let device = match evdev::Device::open(&path) {
            Ok(device) => device,
            Err(_) => continue,
        };
        let name = device.name().unwrap_or_default().to_owned();
        if name.starts_with("HopToDesk") {
            continue;
        }
        let keys = device
            .supported_keys()
            .map(|k| k.contains(evdev::Key::KEY_A) || k.contains(evdev::Key::BTN_LEFT))
            .unwrap_or(false);
        let pointer = device
            .supported_relative_axes()
            .map(|a| a.contains(evdev::RelativeAxisType::REL_X))
            .unwrap_or(false)
            || device
                .supported_absolute_axes()
                .map(|a| a.contains(evdev::AbsoluteAxisType::ABS_X))
                .unwrap_or(false);
        if keys || pointer {
            devices.push((path, device));
        }
    }
    devices
}

pub fn blank(block_input: bool) -> ResultType<()> {
    let mut saved = SAVED.lock().unwrap();
    if !saved.is_empty() {
        return Ok(());
    }
    let devices = backlight_devices();
    if devices.is_empty() {
        bail!("No backlight device");
    }
    let mut applied = Vec::new();
    for device in devices {
        let brightness = device.join("brightness");
        let previous = match read_trimmed(&brightness) {
            Ok(previous) => previous,
            Err(err) => {
                log::warn!("privacy: cannot read {}: {}", brightness.display(), err);
                continue;
            }
        };
        if let Err(err) = std::fs::write(&brightness, b"0") {
            log::warn!("privacy: cannot blank {}: {}", brightness.display(), err);
            continue;
        }
        applied.push((brightness, previous));
    }
    if applied.is_empty() {
        bail!("Failed to blank any display");
    }
    *saved = applied;
    drop(saved);
    if block_input {
        let mut grabbed = GRABBED.lock().unwrap();
        for (path, mut device) in physical_input_devices() {
            match device.grab() {
                Ok(()) => grabbed.push(device),
                Err(err) => log::warn!("privacy: cannot grab {}: {}", path.display(), err),
            }
        }
    }
    Ok(())
}

pub fn restore() {
    let mut grabbed = GRABBED.lock().unwrap();
    for mut device in grabbed.drain(..) {
        let _ = device.ungrab();
    }
    drop(grabbed);
    let mut saved = SAVED.lock().unwrap();
    for (path, previous) in saved.drain(..) {
        if let Err(err) = std::fs::write(&path, previous.as_bytes()) {
            log::error!("privacy: failed to restore {}: {}", path.display(), err);
        }
    }
}

pub fn is_blanked() -> bool {
    !SAVED.lock().unwrap().is_empty()
}
