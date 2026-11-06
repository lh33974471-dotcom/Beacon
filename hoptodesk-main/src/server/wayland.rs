use super::*;
use hbb_common::platform::linux::DISTRO;
use scrap::{
    is_cursor_embedded, set_map_err,
    wayland::pipewire::{fill_displays, try_fix_logical_size},
    Capturer, Display, Frame, TraitCapturer,
};
use std::collections::HashMap;
use std::io;

use crate::{
    client::{
        SCRAP_OTHER_VERSION_OR_X11_REQUIRED, SCRAP_UBUNTU_HIGHER_REQUIRED,
        SCRAP_XDP_PORTAL_UNAVAILABLE, SCRAP_X11_REQUIRED,
    },
    platform::linux::is_x11,
};

lazy_static::lazy_static! {
    static ref CAP_DISPLAY_INFO: RwLock<HashMap<usize, u64>> = RwLock::new(HashMap::new());
    static ref PIPEWIRE_INITIALIZED: RwLock<bool> = RwLock::new(false);
    static ref LOG_SCRAP_COUNT: Mutex<u32> = Mutex::new(0);
    static ref LAST_STAGE_ERR: Mutex<Option<(String, std::time::Instant)>> = Mutex::new(None);
    static ref ACTIVE_DISPLAY_COUNT: RwLock<usize> = RwLock::new(0);
}

pub fn init() {
    set_map_err(map_err_scrap);
}

pub(super) fn increment_active_display_count() -> usize {
    let mut count = ACTIVE_DISPLAY_COUNT.write().unwrap();
    *count += 1;
    *count
}

pub(super) fn decrement_active_display_count() -> usize {
    let mut count = ACTIVE_DISPLAY_COUNT.write().unwrap();
    if *count > 0 {
        *count -= 1;
    }
    *count
}

fn ubuntu_version_lt_21(version_id: &str) -> bool {
    version_id
        .split('.')
        .next()
        .and_then(|s| s.trim().parse::<u32>().ok())
        .map(|major| major < 21)
        .unwrap_or(false)
}

fn map_err_scrap(err: String) -> io::Error {
    if err.starts_with("Did not receive a reply") {
        log::error!("pipewire dbus timeout, {}", &err);
        return io::Error::new(io::ErrorKind::Other, SCRAP_XDP_PORTAL_UNAVAILABLE);
    }

    if let Some(tag) = err.strip_prefix(WAYLAND_STAGE_TAG) {
        log_staged_once(&err);
        return io::Error::new(
            io::ErrorKind::Other,
            staged_message(tag, is_ubuntu_before_21()),
        );
    }

    if DISTRO.name.to_uppercase() == "Ubuntu".to_uppercase() {
        if ubuntu_version_lt_21(&DISTRO.version_id) {
            io::Error::new(io::ErrorKind::Other, SCRAP_UBUNTU_HIGHER_REQUIRED)
        } else {
            try_log(&err);
            io::Error::new(io::ErrorKind::Other, err)
        }
    } else {
        try_log(&err);
        let err_lower = err.to_ascii_lowercase();
        if err_lower.contains("org.freedesktop.portal")
            || err_lower.contains("dbus")
            || err_lower.contains("d-bus")
        {
            io::Error::new(io::ErrorKind::Other, SCRAP_XDP_PORTAL_UNAVAILABLE)
        } else if err_lower.contains("pipewire") {
            io::Error::new(io::ErrorKind::Other, SCRAP_OTHER_VERSION_OR_X11_REQUIRED)
        } else {
            io::Error::new(io::ErrorKind::Other, SCRAP_X11_REQUIRED)
        }
    }
}

fn map_staged_err(err: hbb_common::anyhow::Error) -> hbb_common::anyhow::Error {
    let text = err.to_string();
    match text.strip_prefix(WAYLAND_STAGE_TAG) {
        Some(tag) => {
            log_staged_once(&text);
            hbb_common::anyhow::anyhow!(staged_message(tag, is_ubuntu_before_21()))
        }
        None => err,
    }
}

const STAGE_ERR_REPEAT: std::time::Duration = std::time::Duration::from_secs(600);

fn log_staged_once(err: &str) {
    let now = std::time::Instant::now();
    let mut last = LAST_STAGE_ERR.lock().unwrap();
    let repeat = match last.as_ref() {
        Some((seen, at)) => seen != err || now.duration_since(*at) >= STAGE_ERR_REPEAT,
        None => true,
    };
    if repeat {
        log::error!("Wayland portal handshake failed: {}", err);
        *last = Some((err.to_owned(), now));
    }
}

const WAYLAND_DECLINED: &str = "The screen sharing request was declined on the remote device";
const WAYLAND_TIMED_OUT: &str = "The screen sharing request timed out on the remote device";
const WAYLAND_ENDED: &str =
    "The screen sharing request ended without completing on the remote device";
const WAYLAND_NO_SESSION: &str = "HopToDesk cannot reach the desktop session on the remote device, check that a desktop session is running and that HopToDesk can use it";
const WAYLAND_UNSUPPORTED: &str = "The desktop portal on the remote device is missing a capability needed for screen sharing or remote control, its backend may not be installed";
const WAYLAND_PIPEWIRE_HANDOVER: &str = "Screen sharing was approved on the remote device, but the PipeWire connection could not be opened";
const WAYLAND_NO_USABLE_SCREEN: &str = "HopToDesk could not obtain a usable screen from the XDG Desktop Portal, the PipeWire library may be too old";
const WAYLAND_GST_UNAVAILABLE: &str =
    "HopToDesk could not load a GStreamer component needed for screen capture";
const WAYLAND_GST_PIPEWIRE_UNAVAILABLE: &str = "HopToDesk could not load the GStreamer PipeWire plugin needed for screen capture, install pipewire-gstreamer (Fedora) or gstreamer1.0-pipewire (Debian, Ubuntu) on the remote device";

const WAYLAND_STAGE_TAG: &str = "wl-stage:";

fn is_ubuntu_before_21() -> bool {
    DISTRO.name.to_uppercase() == "Ubuntu".to_uppercase()
        && ubuntu_version_lt_21(&DISTRO.version_id)
}

fn staged_message(tag: &str, ubuntu_before_21: bool) -> String {
    let mut parts = tag.splitn(3, ':');
    let stage = parts.next().unwrap_or_default();
    let kind = parts.next().unwrap_or_default();
    let detail = parts.next().unwrap_or_default();

    let of_the_machine = |msg: &str| {
        if ubuntu_before_21 {
            SCRAP_UBUNTU_HIGHER_REQUIRED.to_owned()
        } else {
            msg.to_owned()
        }
    };

    match (stage, kind) {
        (_, "declined") => WAYLAND_DECLINED.to_owned(),
        (_, "ended") => WAYLAND_ENDED.to_owned(),
        (_, "no-response") => WAYLAND_TIMED_OUT.to_owned(),
        ("streams", _) => of_the_machine(WAYLAND_NO_USABLE_SCREEN),
        ("gst-plugin", _) if detail == "pipewiresrc" => {
            of_the_machine(WAYLAND_GST_PIPEWIRE_UNAVAILABLE)
        }
        ("gst-plugin", _) => of_the_machine(WAYLAND_GST_UNAVAILABLE),
        ("session-bus", _) => of_the_machine(WAYLAND_NO_SESSION),
        ("open-pipewire-remote", _) => of_the_machine(WAYLAND_PIPEWIRE_HANDOVER),
        (_, "unsupported") => of_the_machine(WAYLAND_UNSUPPORTED),
        _ => of_the_machine(SCRAP_XDP_PORTAL_UNAVAILABLE),
    }
}

fn try_log(err: &String) {
    let mut lock_count = LOG_SCRAP_COUNT.lock().unwrap();
    if *lock_count >= 1000000 {
        return;
    }
    if *lock_count % 10000 == 0 {
        log::error!("Failed scrap {}", err);
    }
    *lock_count += 1;
}

struct CapturerPtr(*mut Capturer);

impl Clone for CapturerPtr {
    fn clone(&self) -> Self {
        Self(self.0)
    }
}

impl TraitCapturer for CapturerPtr {
    fn frame<'a>(&'a mut self, timeout: std::time::Duration) -> std::io::Result<Frame<'a>> {
        unsafe { (*self.0).frame(timeout) }
    }
}

struct CapDisplayInfo {
    rects: Vec<((i32, i32), usize, usize)>,
    displays: Vec<DisplayInfo>,
    num: usize,
    primary: usize,
    current: usize,
    capturer: CapturerPtr,
}

#[cfg(feature = "drm")]
fn drm_desktop_rect_for_uinput() -> Option<(i32, i32, i32, i32)> {
    let displays = super::drm_capturer::get_display_infos()?;
    if displays.is_empty() {
        return None;
    }
    let minx = displays.iter().map(|d| d.x).min()?;
    let miny = displays.iter().map(|d| d.y).min()?;
    let maxx = displays.iter().map(|d| d.x + d.width).max()?;
    let maxy = displays.iter().map(|d| d.y + d.height).max()?;
    if maxx <= minx || maxy <= miny {
        return None;
    }
    Some((minx, maxx, miny, maxy))
}

#[cfg(feature = "drm")]
pub(super) async fn update_uinput_resolution() {
    if !crate::input_service::wayland_use_uinput() {
        return;
    }

    let (rect, layout) = match hbb_common::tokio::task::spawn_blocking(|| {
        scrap::wayland::display::clear_wayland_displays_cache();
        match scrap::wayland::display::get_desktop_rect_for_uinput() {
            Some(rect) => Some((rect, scrap::wayland::display::get_display_rects_for_uinput())),
            None => drm_desktop_rect_for_uinput().map(|rect| (rect, Vec::new())),
        }
    })
    .await
    {
        Ok(Some(pair)) => pair,
        Ok(None) => {
            log::warn!("Failed to get desktop rect for uinput");
            return;
        }
        Err(err) => {
            log::warn!("The desktop rect probe task failed: {err}");
            return;
        }
    };

    let snapshot_layout = || {
        super::display_service::set_wayland_layout_baseline(layout.clone());
    };

    if super::display_service::wayland_uinput_rect() == Some(rect) {
        snapshot_layout();
        return;
    }
    let (minx, maxx, miny, maxy) = rect;
    log::info!("update mouse resolution: ({minx}, {maxx}), ({miny}, {maxy})");
    match hbb_common::timeout(
        3_000,
        input_service::update_mouse_resolution(minx, maxx, miny, maxy),
    )
    .await
    {
        Ok(Ok(())) => {
            super::display_service::set_wayland_uinput_rect(rect);
            snapshot_layout();
        }
        Ok(Err(err)) => log::error!("Failed to update mouse resolution: {}", err),
        Err(err) => log::error!("Failed to update mouse resolution: {}", err),
    }
}

#[tokio::main(flavor = "current_thread")]
pub(super) async fn ensure_inited() -> ResultType<()> {
    #[cfg(feature = "drm")]
    if super::drm_capturer::is_available() {
        update_uinput_resolution().await;
        return Ok(());
    }
    check_init().await
}

#[cfg(feature = "drm")]
pub(super) fn unattended_switch_off() -> bool {
    !super::drm_capturer::drm_capture_enabled()
}

#[cfg(not(feature = "drm"))]
pub(super) fn unattended_switch_off() -> bool {
    false
}

pub(super) fn is_inited() -> Option<Message> {
    if is_x11() {
        None
    } else {
        #[cfg(feature = "drm")]
        if super::drm_capturer::is_available() {
            return None;
        }
        if !matches!(CAP_DISPLAY_INFO.try_read(), Ok(map) if !map.is_empty()) {
            let mut msg_out = Message::new();
            let res = MessageBox {
                msgtype: "nook-nocancel-hasclose".to_owned(),
                title: "Wayland".to_owned(),
                text: if unattended_switch_off() {
                    crate::client::WAYLAND_SELECT_SCREEN_SWITCH_OFF
                } else {
                    "Please Select the screen to be shared(Operate on the peer side)."
                }
                .to_owned(),
                link: "".to_owned(),
                ..Default::default()
            };
            msg_out.set_message_box(res);
            Some(msg_out)
        } else {
            None
        }
    }
}

pub(super) async fn check_init() -> ResultType<()> {
    if !is_x11() {
        let looked = CAP_DISPLAY_INFO.try_read().map(|map| map.is_empty()).ok();
        let empty = match looked {
            Some(empty) => empty,
            None => hbb_common::tokio::task::spawn_blocking(|| {
                CAP_DISPLAY_INFO.read().unwrap().is_empty()
            })
            .await
            .map_err(|err| hbb_common::anyhow::anyhow!("Wayland display state task failed: {err}"))?,
        };
        if empty {
            if crate::input_service::wayland_use_uinput() {
                scrap::wayland::display::clear_wayland_displays_cache();
                if let Some((minx, maxx, miny, maxy)) =
                    scrap::wayland::display::get_desktop_rect_for_uinput()
                {
                    log::info!(
                        "update mouse resolution: ({}, {}), ({}, {})",
                        minx,
                        maxx,
                        miny,
                        maxy
                    );
                    match hbb_common::timeout(
                        3_000,
                        input_service::update_mouse_resolution(minx, maxx, miny, maxy),
                    )
                    .await
                    {
                        Ok(Ok(())) => {
                            super::display_service::set_wayland_uinput_rect((
                                minx, maxx, miny, maxy,
                            ));
                            super::display_service::set_wayland_layout_baseline(
                                scrap::wayland::display::get_display_rects_for_uinput(),
                            );
                        }
                        Ok(Err(err)) => log::error!("Failed to update mouse resolution: {}", err),
                        Err(err) => log::error!("Failed to update mouse resolution: {}", err),
                    }
                } else {
                    log::warn!("Failed to get desktop rect for uinput");
                }
            }

            hbb_common::tokio::task::spawn_blocking(init_displays)
                .await
                .map_err(|err| {
                    hbb_common::anyhow::anyhow!("Wayland display init task failed: {err}")
                })??;
        }
    }
    Ok(())
}

fn init_displays() -> ResultType<()> {
    let mut lock = CAP_DISPLAY_INFO.write().unwrap();
    if lock.is_empty() {
        if *PIPEWIRE_INITIALIZED.read().unwrap() {
            *PIPEWIRE_INITIALIZED.write().unwrap() = false;
        }

        let mut all = Display::all()?;
        log::debug!("Initializing displays with fill_displays()");
        {
            let temp_mouse_move_handle = input_service::TemporaryMouseMoveHandle::new();
            let move_mouse_to = |x, y| temp_mouse_move_handle.move_mouse_to(x, y);
            fill_displays(move_mouse_to, crate::get_cursor_pos, &mut all)
                .map_err(map_staged_err)?;
        }
        log::debug!("Attempting to fix logical size with try_fix_logical_size()");
        try_fix_logical_size(&mut all);
        *PIPEWIRE_INITIALIZED.write().unwrap() = true;
        let num = all.len();
        let primary = super::display_service::get_primary_2(&all);
        super::display_service::check_update_displays(&all);
        let mut displays = super::display_service::get_sync_displays();
        for display in displays.iter_mut() {
            display.cursor_embedded = is_cursor_embedded();
        }

        let mut rects: Vec<((i32, i32), usize, usize)> = Vec::new();
        for d in &all {
            rects.push((d.origin(), d.width(), d.height()));
        }

        log::debug!(
            "#displays={}, primary={}, rects: {:?}, cpus={}/{}",
            num,
            primary,
            rects,
            num_cpus::get_physical(),
            num_cpus::get()
        );

        // Create individual CapDisplayInfo for each display with its own capturer
        for (idx, display) in all.into_iter().enumerate() {
            let capturer = Box::into_raw(Box::new(Capturer::new(display)?));
            let capturer = CapturerPtr(capturer);

            let cap_display_info = Box::into_raw(Box::new(CapDisplayInfo {
                rects: rects.clone(),
                displays: displays.clone(),
                num,
                primary,
                current: idx,
                capturer,
            }));

            lock.insert(idx, cap_display_info as u64);
        }
    }
    Ok(())
}

pub(super) async fn get_displays_and_primary() -> ResultType<(Vec<DisplayInfo>, usize)> {
    #[cfg(feature = "drm")]
    if hbb_common::tokio::task::spawn_blocking(super::drm_capturer::is_available_after_warm)
        .await
        .unwrap_or(false)
    {
        super::drm_capturer::refresh_displays_for_login().await;
        let snapshot = hbb_common::tokio::task::spawn_blocking(
            super::drm_capturer::get_display_infos_and_primary,
        )
        .await
        .map_err(|err| hbb_common::anyhow::anyhow!("Wayland display probe task failed: {err}"))?;
        if let Some(snapshot) = snapshot {
            return Ok(snapshot);
        }
    }
    check_init().await?;
    hbb_common::tokio::task::spawn_blocking(|| -> ResultType<(Vec<DisplayInfo>, usize)> {
        let cap_map = CAP_DISPLAY_INFO.read().unwrap();
        if let Some(addr) = cap_map.values().next() {
            let cap_display_info: *const CapDisplayInfo = *addr as _;
            unsafe {
                let cap_display_info = &*cap_display_info;
                Ok((cap_display_info.displays.clone(), cap_display_info.primary))
            }
        } else {
            bail!("Failed to get capturer display info");
        }
    })
    .await
    .map_err(|err| hbb_common::anyhow::anyhow!("Wayland display info task failed: {err}"))?
}

pub(super) fn get_primary() -> ResultType<usize> {
    let cap_map = CAP_DISPLAY_INFO.read().unwrap();
    if let Some(addr) = cap_map.values().next() {
        let cap_display_info: *const CapDisplayInfo = *addr as _;
        unsafe {
            let cap_display_info = &*cap_display_info;
            Ok(cap_display_info.primary)
        }
    } else {
        bail!("Failed to get capturer display info");
    }
}

pub fn clear() {
    if is_x11() {
        return;
    }
    #[cfg(feature = "drm")]
    if super::drm_capturer::is_available_cached() {
        scrap::wayland::display::clear_wayland_displays_cache();
    }
    let mut write_lock = CAP_DISPLAY_INFO.write().unwrap();
    for (_, addr) in write_lock.iter() {
        let cap_display_info: *mut CapDisplayInfo = *addr as _;
        unsafe {
            let _box_capturer = Box::from_raw((*cap_display_info).capturer.0);
            let _box_cap_display_info = Box::from_raw(cap_display_info);
        }
    }
    write_lock.clear();

    // Reset PipeWire initialization flag to allow recreation on next init
    *PIPEWIRE_INITIALIZED.write().unwrap() = false;
}

pub(super) fn try_close_portal_session() {
    static CLOSER: Mutex<Option<std::thread::JoinHandle<()>>> = Mutex::new(None);
    if matches!(
        scrap::wayland::pipewire::RDP_SESSION_INFO.try_lock(),
        Ok(info) if info.is_none()
    ) {
        return;
    }
    let mut closer = CLOSER.lock().unwrap();
    if matches!(&*closer, Some(close) if !close.is_finished()) {
        return;
    }
    match std::thread::Builder::new()
        .name("portal-session-close".into())
        .spawn(|| {
            if !AUTHED_CONNS
                .lock()
                .unwrap()
                .iter()
                .any(|c| c.1 == AuthConnType::Remote)
            {
                scrap::wayland::pipewire::try_close_session();
            }
        }) {
        Ok(close) => *closer = Some(close),
        Err(err) => log::error!("Failed to spawn the portal session close: {}", err),
    }
}

#[cfg(feature = "drm")]
#[tokio::main(flavor = "current_thread")]
async fn ensure_pipewire_inited() -> ResultType<()> {
    check_init().await
}

pub(super) fn get_capturer_for_display(
    display_idx: usize,
) -> ResultType<super::video_service::CapturerInfo> {
    if is_x11() {
        bail!("Do not call this function if not wayland");
    }

    #[cfg(feature = "drm")]
    if super::drm_capturer::is_available() {
        match super::drm_capturer::get_capturer_info(display_idx) {
            Ok(info) => return Ok(info),
            Err(e) => {
                log::warn!(
                    "drm capturer for display {} unavailable ({:#}); falling back to PipeWire",
                    display_idx,
                    e
                );
                ensure_pipewire_inited()?;
            }
        }
    }

    #[cfg(feature = "drm")]
    let drm_advertised = if super::drm_capturer::is_available_cached() {
        match super::drm_capturer::get_display_infos() {
            Some(list) => Some((list.get(display_idx).cloned(), list.len() == 1)),
            None => Some((None, false)),
        }
    } else {
        None
    };
    let cap_map = CAP_DISPLAY_INFO.read().unwrap();
    if let Some(addr) = cap_map.get(&display_idx) {
        let cap_display_info: *const CapDisplayInfo = *addr as _;
        unsafe {
            let cap_display_info = &*cap_display_info;
            let rect = cap_display_info.rects[cap_display_info.current];
            #[cfg(feature = "drm")]
            if let Some((advertised, single_display)) = drm_advertised {
                if let Some(advertised) = advertised {
                    let single_display = single_display && cap_display_info.num == 1;
                    let consistent = advertised.x == rect.0 .0
                        && advertised.y == rect.0 .1
                        && (single_display
                            || (advertised.width as usize == rect.1
                                && advertised.height as usize == rect.2));
                    if !consistent {
                        bail!(
                            "drm display {} demoted with no geometry-consistent PipeWire stream (advertised {}x{}+{}+{} vs stream {}x{}+{}+{}); advertised offline",
                            display_idx,
                            advertised.width,
                            advertised.height,
                            advertised.x,
                            advertised.y,
                            rect.1,
                            rect.2,
                            rect.0 .0,
                            rect.0 .1
                        );
                    }
                }
            }
            Ok(super::video_service::CapturerInfo {
                origin: rect.0,
                width: rect.1,
                height: rect.2,
                ndisplay: cap_display_info.num,
                current: cap_display_info.current,
                privacy_mode_id: 0,
                _capturer_privacy_mode_id: 0,
                capturer: Box::new(cap_display_info.capturer.clone()),
            })
        }
    } else {
        bail!(
            "Failed to get capturer display info for display {}",
            display_idx
        );
    }
}

pub fn common_get_error() -> String {
    if DISTRO.name.to_uppercase() == "Ubuntu".to_uppercase() {
        if ubuntu_version_lt_21(&DISTRO.version_id) {
            return SCRAP_UBUNTU_HIGHER_REQUIRED.to_owned();
        }
    }
    return "".to_owned();
}
