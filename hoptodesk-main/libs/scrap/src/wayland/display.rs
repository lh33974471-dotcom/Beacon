use hbb_common::regex::Regex;
use lazy_static::lazy_static;
use std::sync::Mutex;
use std::{
    process::{Command, Output, Stdio},
    sync::Arc,
    time::{Duration, Instant},
};
use tracing::warn;

use hbb_common::platform::linux::{get_wayland_displays, WaylandDisplayInfo};

lazy_static! {
    static ref DISPLAYS: Mutex<Option<Arc<Displays>>> = Mutex::new(None);
}

static MISSING_LOGICAL_SIZE_WARNED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

const COMMAND_TIMEOUT: Duration = Duration::from_millis(1000);

#[cfg(any(test, feature = "drm"))]
const FAILED_LOOKUP_BACKOFF: Duration = Duration::from_secs(5);

#[cfg(any(test, feature = "drm"))]
static LAST_FAILED_LOOKUP: Mutex<Option<Instant>> = Mutex::new(None);

#[cfg(feature = "drm")]
static LOOKUP_FAILURE_WARNED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

pub struct Displays {
    pub primary: usize,
    pub displays: Vec<WaylandDisplayInfo>,
}

// We need this helper to run commands with a timeout, as some commands may hang.
// `kscreen-doctor -o` is known to hang when:
// 1. On Archlinux, Both GNOME and KDE Plasma are installed.
// 2. Run this command in a GNOME session.
fn run_with_timeout(
    program: &str,
    args: &[&str],
    timeout: Duration,
    label: &str,
) -> Option<Output> {
    let mut child = Command::new(program)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .ok()?;

    let start = Instant::now();
    loop {
        if let Ok(Some(_)) = child.try_wait() {
            break;
        }
        if start.elapsed() >= timeout {
            warn!("{} command timed out after {:?}", label, timeout);
            if let Err(e) = child.kill() {
                warn!("Failed to kill child process for '{}': {}", label, e);
            }
            if let Err(e) = child.wait() {
                warn!("Failed to wait for child process for '{}': {}", label, e);
            }
            return None;
        }
        std::thread::sleep(Duration::from_millis(30));
    }

    match child.wait_with_output() {
        Ok(output) => {
            if !output.status.success() {
                warn!("{} command failed with status: {}", label, output.status);
                return None;
            }
            Some(output)
        }
        Err(_) => None,
    }
}

// There are some limitations with xrandr method:
// 1. It only works when XWayland is running.
// 2. The distro may not have xrandr installed by default.
// 3. xrandr may not report "primary" in its output. eg. openSUSE Leap 15.6 KDE Plasma.
fn try_xrandr_primary() -> Option<String> {
    let output = Command::new("xrandr").output().ok()?;
    if !output.status.success() {
        return None;
    }

    let text = String::from_utf8_lossy(&output.stdout);
    for line in text.lines() {
        if line.contains("primary") && line.contains("connected") {
            if let Some(name) = line.split_whitespace().next() {
                return Some(name.to_string());
            }
        }
    }
    None
}

fn try_kscreen_primary() -> Option<String> {
    if !hbb_common::platform::linux::is_kde_session() {
        return None;
    }

    let output = run_with_timeout(
        "kscreen-doctor",
        &["-o"],
        COMMAND_TIMEOUT,
        "kscreen-doctor -o",
    )?;
    if !output.status.success() {
        return None;
    }

    let text = String::from_utf8_lossy(&output.stdout);

    // Remove ANSI color codes
    let re_ansi = Regex::new(r"\x1b\[[0-9;]*m").ok()?;
    let clean_text = re_ansi.replace_all(&text, "");

    // Split the text into blocks, each starting with "Output:".
    // The first element of the split will be empty, so we skip it.
    for block in clean_text.split("Output:").skip(1) {
        // Check if this block describes the primary monitor.
        if block.contains("priority 1") {
            // The monitor name is the second piece of text in the block, after the ID.
            // e.g., " 1 eDP-1 enabled..." -> "eDP-1"
            if let Some(name) = block.split_whitespace().nth(1) {
                return Some(name.to_string());
            }
        }
    }

    None
}

fn try_gdbus_primary() -> Option<String> {
    let output = run_with_timeout(
        "gdbus",
        &[
            "call",
            "--session",
            "--dest",
            "org.gnome.Mutter.DisplayConfig",
            "--object-path",
            "/org/gnome/Mutter/DisplayConfig",
            "--method",
            "org.gnome.Mutter.DisplayConfig.GetCurrentState",
        ],
        COMMAND_TIMEOUT,
        "gdbus DisplayConfig.GetCurrentState",
    )?;

    if !output.status.success() {
        return None;
    }

    let text = String::from_utf8_lossy(&output.stdout);

    // Match logical monitor entries with primary=true
    // Pattern: (x, y, scale, transform, true, [('connector-name', ...), ...], ...)
    // Use regex to find entries where 5th field is true, then extract connector name
    // Example matched text: "(0, 0, 1.5, 0, true, [('HDMI-1', 'MHH', 'Monitor', '0x00000000')], ...)"
    let re = Regex::new(r"\([^()]*,\s*true,\s*\[\('([^']+)'").ok()?;

    if let Some(captures) = re.captures(&text) {
        return captures.get(1).map(|m| m.as_str().to_string());
    }

    None
}

fn get_primary_monitor() -> Option<String> {
    try_xrandr_primary()
        .or_else(try_kscreen_primary)
        .or_else(try_gdbus_primary)
}

#[cfg(any(test, feature = "drm"))]
fn lookup_allowed(failed_at: Option<Instant>, now: Instant) -> bool {
    failed_at.map_or(true, |at| {
        now.saturating_duration_since(at) >= FAILED_LOOKUP_BACKOFF
    })
}

#[cfg(feature = "drm")]
fn backed_off() -> bool {
    let failed_at = *LAST_FAILED_LOOKUP.lock().unwrap();
    !lookup_allowed(failed_at, Instant::now())
}

#[cfg(feature = "drm")]
fn endpoint_named() -> bool {
    use std::sync::atomic::{AtomicBool, Ordering};
    static WAS_NAMED: AtomicBool = AtomicBool::new(false);
    let named = ["WAYLAND_DISPLAY", "WAYLAND_SOCKET"]
        .iter()
        .any(|key| std::env::var_os(key).is_some_and(|value| !value.is_empty()));
    if named {
        WAS_NAMED.store(true, Ordering::Release);
    }
    WAS_NAMED.load(Ordering::Acquire)
}

fn enumerate_displays() -> hbb_common::ResultType<Vec<WaylandDisplayInfo>> {
    #[cfg(feature = "drm")]
    let named = endpoint_named();
    let probed = get_wayland_displays();
    #[cfg(feature = "drm")]
    {
        *LAST_FAILED_LOOKUP.lock().unwrap() = (probed.is_err() && !named).then(Instant::now);
        if let Err(err) = &probed {
            if !LOOKUP_FAILURE_WARNED.swap(true, std::sync::atomic::Ordering::Relaxed) {
                warn!("Failed to get wayland displays: {}", err);
            }
        } else {
            LOOKUP_FAILURE_WARNED.store(false, std::sync::atomic::Ordering::Relaxed);
        }
    }
    probed
}

#[cfg(feature = "drm")]
pub fn wayland_lookup_suppressed() -> bool {
    DISPLAYS.lock().unwrap().is_none() && backed_off()
}

#[cfg(feature = "drm")]
pub fn wayland_failure_stamped() -> bool {
    LAST_FAILED_LOOKUP.lock().unwrap().is_some()
}

pub fn get_displays() -> Arc<Displays> {
    let mut lock = DISPLAYS.lock().unwrap();
    match lock.as_ref() {
        Some(displays) => displays.clone(),
        None => match enumerate_displays() {
            Ok(displays) => {
                let mut primary_index = None;
                if let Some(name) = get_primary_monitor() {
                    for (i, display) in displays.iter().enumerate() {
                        if display.name == name {
                            primary_index = Some(i);
                            break;
                        }
                    }
                };
                if primary_index.is_none() {
                    for (i, display) in displays.iter().enumerate() {
                        if display.x == 0 && display.y == 0 {
                            primary_index = Some(i);
                            break;
                        }
                    }
                }
                let displays = Arc::new(Displays {
                    primary: primary_index.unwrap_or(0),
                    displays,
                });
                *lock = Some(displays.clone());
                displays
            }
            Err(_err) => {
                #[cfg(not(feature = "drm"))]
                warn!("Failed to get wayland displays: {}", _err);
                Arc::new(Displays {
                    primary: 0,
                    displays: Vec::new(),
                })
            }
        },
    }
}

#[inline]
pub fn clear_wayland_displays_cache() {
    let _ = DISPLAYS.lock().unwrap().take();
}

// Return (min_x, max_x, min_y, max_y)
pub fn get_desktop_rect_for_uinput() -> Option<(i32, i32, i32, i32)> {
    let wayland_displays = get_displays();
    desktop_rect_of(&wayland_displays.displays)
}

pub fn get_layout_for_uinput_live() -> Option<((i32, i32, i32, i32), Vec<DisplayRect>)> {
    #[cfg(feature = "drm")]
    if backed_off() {
        return None;
    }
    match enumerate_displays() {
        Ok(displays) => {
            desktop_rect_of(&displays).map(|rect| (rect, logical_rects_of(&displays)))
        }
        Err(_err) => {
            #[cfg(not(feature = "drm"))]
            warn!("Failed to get wayland displays: {}", _err);
            None
        }
    }
}

fn desktop_rect_of(displays: &[WaylandDisplayInfo]) -> Option<(i32, i32, i32, i32)> {
    if displays.is_empty() {
        return None;
    }

    // For compatibility, if only one display, we use the physical size for `uinput`.
    // Otherwise, we use the logical size for `uinput`.
    if displays.len() == 1 {
        let d = &displays[0];
        return Some((d.x, d.x + d.width, d.y, d.y + d.height));
    }

    let mut min_x = i32::MAX;
    let mut min_y = i32::MAX;
    let mut max_x = i32::MIN;
    let mut max_y = i32::MIN;
    for d in displays.iter() {
        min_x = min_x.min(d.x);
        min_y = min_y.min(d.y);
        let size = if let Some(logical_size) = d.logical_size {
            logical_size
        } else {
            if !MISSING_LOGICAL_SIZE_WARNED.swap(true, std::sync::atomic::Ordering::Relaxed) {
                warn!(
                    "Display at ({}, {}) is missing logical_size; falling back to physical size ({}, {}).",
                    d.x, d.y, d.width, d.height
                );
            }
            (d.width, d.height)
        };
        max_x = max_x.max(d.x + size.0);
        max_y = max_y.max(d.y + size.1);
    }
    Some((min_x, max_x, min_y, max_y))
}

#[derive(Clone, Debug, PartialEq)]
pub struct DisplayRect {
    pub name: String,
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

fn logical_rects_of(displays: &[WaylandDisplayInfo]) -> Vec<DisplayRect> {
    let single = displays.len() == 1;
    displays
        .iter()
        .map(|d| {
            let (w, h) = if single {
                (d.width, d.height)
            } else {
                d.logical_size.unwrap_or((d.width, d.height))
            };
            DisplayRect {
                name: d.name.clone(),
                x: d.x,
                y: d.y,
                w,
                h,
            }
        })
        .collect()
}

pub fn get_display_rects_for_uinput() -> Vec<DisplayRect> {
    logical_rects_of(&get_displays().displays)
}

pub fn remap_to_live_layout(
    x: i32,
    y: i32,
    baseline: &[DisplayRect],
    live: &[DisplayRect],
) -> (i32, i32) {
    let Some((bi, b)) = baseline
        .iter()
        .enumerate()
        .find(|(_, r)| x >= r.x && x < r.x + r.w && y >= r.y && y < r.y + r.h)
    else {
        return (x, y);
    };
    let matched = if b.name.is_empty() {
        if baseline.len() == live.len() {
            live.get(bi)
        } else {
            None
        }
    } else {
        live.iter().find(|r| r.name == b.name)
    };
    let Some(l) = matched else {
        return (x, y);
    };
    let nx = map_axis(x, b.x, b.w, l.x, l.w);
    let ny = map_axis(y, b.y, b.h, l.y, l.h);
    (nx, ny)
}

fn map_axis(v: i32, base_origin: i32, base_extent: i32, live_origin: i32, live_extent: i32) -> i32 {
    if base_extent <= 1 || live_extent <= 1 {
        return live_origin;
    }
    live_origin + ((v - base_origin) as i64 * (live_extent - 1) as i64 / (base_extent - 1) as i64) as i32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_lookup_backoff_boundaries() {
        let failed_at = Instant::now();
        assert!(lookup_allowed(None, failed_at));
        assert!(!lookup_allowed(
            Some(failed_at),
            failed_at + FAILED_LOOKUP_BACKOFF / 2
        ));
        assert!(lookup_allowed(
            Some(failed_at),
            failed_at + FAILED_LOOKUP_BACKOFF
        ));
    }

    #[test]
    fn test_lookup_stamp_from_the_future_only_waits() {
        let now = Instant::now();
        assert!(!lookup_allowed(Some(now + FAILED_LOOKUP_BACKOFF), now));
    }

    #[test]
    fn test_clear_keeps_the_failure_stamp() {
        *LAST_FAILED_LOOKUP.lock().unwrap() = Some(Instant::now());
        clear_wayland_displays_cache();
        let stamp = *LAST_FAILED_LOOKUP.lock().unwrap();
        assert!(stamp.is_some());
        *LAST_FAILED_LOOKUP.lock().unwrap() = None;
    }

    fn display(
        x: i32,
        y: i32,
        width: i32,
        height: i32,
        logical_size: Option<(i32, i32)>,
    ) -> WaylandDisplayInfo {
        WaylandDisplayInfo {
            name: "".to_owned(),
            x,
            y,
            width,
            height,
            logical_size,
            refresh_rate: 60,
        }
    }

    #[test]
    fn test_desktop_rect_empty() {
        assert_eq!(desktop_rect_of(&[]), None);
    }

    #[test]
    fn test_desktop_rect_single_display_uses_physical_size() {
        let displays = [display(0, 0, 2880, 1800, Some((1859, 1162)))];
        assert_eq!(desktop_rect_of(&displays), Some((0, 2880, 0, 1800)));
    }

    #[test]
    fn test_desktop_rect_multi_display_uses_logical_size() {
        let displays = [
            display(0, 718, 2880, 1800, Some((1859, 1162))),
            display(1859, 0, 1920, 1080, Some((1920, 1080))),
            display(1859, 1080, 1920, 1080, Some((1920, 1080))),
        ];
        assert_eq!(desktop_rect_of(&displays), Some((0, 3779, 0, 2160)));
    }

    #[test]
    fn test_desktop_rect_missing_logical_size_falls_back_to_physical() {
        let displays = [
            display(0, 0, 2560, 1440, None),
            display(2560, 0, 2560, 1440, Some((2560, 1440))),
        ];
        assert_eq!(desktop_rect_of(&displays), Some((0, 5120, 0, 1440)));
    }

    fn rect(name: &str, x: i32, y: i32, w: i32, h: i32) -> DisplayRect {
        DisplayRect {
            name: name.to_owned(),
            x,
            y,
            w,
            h,
        }
    }

    #[test]
    fn test_remap_primary_rescale_shifts_second_display() {
        let baseline = [
            rect("DP-1", 0, 0, 2560, 1440),
            rect("DP-2", 2560, 0, 2560, 1440),
        ];
        let live = [
            rect("DP-1", 0, 0, 2048, 1440),
            rect("DP-2", 2048, 0, 2560, 1440),
        ];
        assert_eq!(remap_to_live_layout(2560, 0, &baseline, &live), (2048, 0));
        assert_eq!(
            remap_to_live_layout(3840, 720, &baseline, &live),
            (3328, 720)
        );
    }

    #[test]
    fn test_remap_scales_within_resized_display() {
        let baseline = [rect("DP-1", 0, 0, 2560, 1440), rect("DP-2", 2560, 0, 2560, 1440)];
        let live = [rect("DP-1", 0, 0, 2048, 1440), rect("DP-2", 2048, 0, 2560, 1440)];
        assert_eq!(remap_to_live_layout(1280, 500, &baseline, &live), (1023, 500));
    }

    #[test]
    fn test_remap_enlarged_display_reaches_far_edge() {
        let baseline = [rect("DP-1", 0, 0, 2048, 1440), rect("DP-2", 2048, 0, 1920, 1080)];
        let live = [rect("DP-1", 0, 0, 2560, 1440), rect("DP-2", 2560, 0, 1920, 1080)];
        assert_eq!(remap_to_live_layout(2047, 0, &baseline, &live), (2559, 0));
        assert_eq!(remap_to_live_layout(0, 0, &baseline, &live), (0, 0));
    }

    #[test]
    fn test_remap_identity_when_unchanged() {
        let layout = [rect("DP-1", 0, 0, 2560, 1440), rect("DP-2", 2560, 0, 2560, 1440)];
        assert_eq!(remap_to_live_layout(3000, 700, &layout, &layout), (3000, 700));
    }

    #[test]
    fn test_remap_point_outside_all_displays_unchanged() {
        let baseline = [rect("DP-1", 0, 0, 2560, 1440)];
        let live = [rect("DP-1", 0, 0, 2048, 1440)];
        assert_eq!(remap_to_live_layout(9000, 9000, &baseline, &live), (9000, 9000));
    }

    #[test]
    fn test_remap_display_removed_unchanged() {
        let baseline = [rect("DP-1", 0, 0, 2560, 1440), rect("DP-2", 2560, 0, 2560, 1440)];
        let live = [rect("DP-1", 0, 0, 2560, 1440)];
        assert_eq!(remap_to_live_layout(2600, 100, &baseline, &live), (2600, 100));
    }

    #[test]
    fn test_remap_nameless_index_fallback() {
        let baseline = [rect("", 0, 0, 2560, 1440), rect("", 2560, 0, 2560, 1440)];
        let live = [rect("", 0, 0, 2048, 1440), rect("", 2048, 0, 2560, 1440)];
        assert_eq!(remap_to_live_layout(2560, 0, &baseline, &live), (2048, 0));
    }

    #[test]
    fn test_remap_nameless_count_changed_unchanged() {
        let baseline = [rect("", 0, 0, 2560, 1440), rect("", 2560, 0, 2560, 1440)];
        let live = [rect("", 0, 0, 2048, 1440)];
        assert_eq!(remap_to_live_layout(2560, 0, &baseline, &live), (2560, 0));
    }

    #[test]
    fn test_remap_named_miss_equal_count_unchanged() {
        let baseline = [rect("DP-1", 0, 0, 2560, 1440), rect("DP-2", 2560, 0, 2560, 1440)];
        let live = [rect("DP-1", 0, 0, 2048, 1440), rect("HDMI-1", 2048, 0, 1920, 1080)];
        assert_eq!(remap_to_live_layout(2600, 100, &baseline, &live), (2600, 100));
    }

    #[test]
    fn test_logical_rects_single_display_uses_physical() {
        let displays = [display(0, 0, 2560, 1440, Some((2048, 1152)))];
        assert_eq!(
            logical_rects_of(&displays),
            vec![rect("", 0, 0, 2560, 1440)]
        );
    }

    #[test]
    fn test_logical_rects_multi_display_uses_logical() {
        let displays = [
            display(0, 0, 2560, 1440, Some((2048, 1152))),
            display(2048, 0, 1920, 1080, None),
        ];
        assert_eq!(
            logical_rects_of(&displays),
            vec![rect("", 0, 0, 2048, 1152), rect("", 2048, 0, 1920, 1080)]
        );
    }
}
