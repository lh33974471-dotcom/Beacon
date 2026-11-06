use wireui::dom::Element;
use std::sync::Mutex;

lazy_static::lazy_static! {
    static ref ACTIVE_ELEMENT: Mutex<Option<Element>> = Mutex::new(None);
}

pub fn register_remote_view(element: Element) {
    *ACTIVE_ELEMENT.lock().unwrap() = Some(element);
}

pub fn arm_zoom_hook() {
    wireui::host::set_window_created_hook(|handle| {
        install_zoom_hook(handle as *mut std::ffi::c_void);
    });
}

pub fn unregister_remote_view() {
    *ACTIVE_ELEMENT.lock().unwrap() = None;
}

pub fn dispatch_zoom_delta(factor: f64) -> bool {
    if factor <= 0.0 || !factor.is_finite() {
        return false;
    }
    let element_opt = ACTIVE_ELEMENT.lock().unwrap().clone();
    match element_opt {
        Some(e) => {
            e.call_method_queued("applyZoomDelta", &wireui::make_args!(factor));
            true
        }
        None => false,
    }
}

pub fn dispatch_zoom_toggle() {
    let element_opt = ACTIVE_ELEMENT.lock().unwrap().clone();
    if let Some(e) = element_opt {
        e.call_method_queued("toggleZoom", &wireui::make_args!());
    }
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
pub fn install_zoom_hook(window_handle: *mut std::ffi::c_void) {
    #[cfg(target_os = "macos")]
    {
        let _ = window_handle;
        macos::install();
    }
    #[cfg(windows)]
    windows_impl::install(window_handle);
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub fn install_zoom_hook(_window_handle: *mut std::ffi::c_void) {}

#[cfg(target_os = "macos")]
mod macos {
    use block::ConcreteBlock;
    use cocoa::base::id;
    use objc::runtime::Class;
    use objc::{class, msg_send, sel, sel_impl};
    use std::sync::Mutex;

    const NS_EVENT_TYPE_MAGNIFY: u64 = 30;
    const NS_EVENT_TYPE_SMART_MAGNIFY: u64 = 32;

    lazy_static::lazy_static! {
        static ref MONITOR_ADDRS: Mutex<Vec<usize>> = Mutex::new(Vec::new());
    }

    pub fn install() {
        let mut addrs = MONITOR_ADDRS.lock().unwrap();
        if !addrs.is_empty() {
            return;
        }
        unsafe {
            let cls: &Class = class!(NSEvent);

            let mask_magnify: u64 = 1u64 << NS_EVENT_TYPE_MAGNIFY;
            let block_magnify = ConcreteBlock::new(move |event: id| -> id {
                if event.is_null() {
                    return event;
                }
                let magnification: f64 = msg_send![event, magnification];
                if magnification.is_finite() && magnification != 0.0 {
                    super::dispatch_zoom_delta(1.0 + magnification);
                }
                cocoa::base::nil
            });
            let block_magnify = block_magnify.copy();
            let monitor: id = msg_send![cls,
                addLocalMonitorForEventsMatchingMask: mask_magnify
                handler: &*block_magnify];
            if !monitor.is_null() {
                let _: id = msg_send![monitor, retain];
                addrs.push(monitor as usize);
            }

            let mask_smart: u64 = 1u64 << NS_EVENT_TYPE_SMART_MAGNIFY;
            let block_smart = ConcreteBlock::new(move |event: id| -> id {
                if event.is_null() {
                    return event;
                }
                super::dispatch_zoom_toggle();
                cocoa::base::nil
            });
            let block_smart = block_smart.copy();
            let monitor2: id = msg_send![cls,
                addLocalMonitorForEventsMatchingMask: mask_smart
                handler: &*block_smart];
            if !monitor2.is_null() {
                let _: id = msg_send![monitor2, retain];
                addrs.push(monitor2 as usize);
            }
        }
    }
}

#[cfg(windows)]
mod windows_impl {
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;
    use windows::Win32::Devices::HumanInterfaceDevice::{
        HidP_GetCaps, HidP_GetUsageValue, HidP_GetUsages, HidP_GetValueCaps, HidP_Input, HIDP_CAPS,
        HIDP_VALUE_CAPS, PHIDP_PREPARSED_DATA,
    };
    use windows::Win32::Foundation::{HANDLE, HWND, LPARAM, LRESULT, WPARAM};
    use windows::Win32::UI::Input::Touch::{
        CloseGestureInfoHandle, GetGestureInfo, SetGestureConfig, GESTURECONFIG, GESTUREINFO,
        GID_ZOOM, HGESTUREINFO,
    };
    use windows::Win32::UI::Input::{
        GetRawInputData, GetRawInputDeviceInfoW, RegisterRawInputDevices, HRAWINPUT, RAWINPUT,
        RAWINPUTDEVICE, RAWINPUTHEADER, RIDEV_INPUTSINK, RIDI_PREPARSEDDATA, RID_INPUT, RIM_TYPEHID,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        CallWindowProcW, SetWindowLongPtrW, GF_BEGIN, GF_END, GWLP_WNDPROC, WM_GESTURE, WM_INPUT,
        WNDPROC,
    };

    const GC_ZOOM_FLAG: u32 = 1;
    const HID_USAGE_PAGE_GENERIC_DESKTOP: u16 = 0x01;
    const HID_USAGE_PAGE_DIGITIZER: u16 = 0x0D;
    const HID_USAGE_TOUCH_PAD: u16 = 0x05;
    const HID_USAGE_X: u16 = 0x30;
    const HID_USAGE_Y: u16 = 0x31;
    const HID_USAGE_TIP_SWITCH: u16 = 0x42;
    const PINCH_FACTOR_THRESHOLD: f64 = 0.012;
    const PINCH_FACTOR_CLAMP_MIN: f64 = 0.88;
    const PINCH_FACTOR_CLAMP_MAX: f64 = 1.15;
    const PINCH_DAMP: f64 = 0.85;
    const PINCH_DOMINANCE: f64 = 2.0;
    const PINCH_MIN_ABS_DELTA: f64 = 10.0;
    const PINCH_MAX_MID_MOTION: f64 = 8.0;

    static PREV_WNDPROC: AtomicUsize = AtomicUsize::new(0);
    static INSTALLED_HWND: AtomicUsize = AtomicUsize::new(0);
    static LAST_TWO_FINGER_MS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

    const WHEEL_SUPPRESS_MS: u64 = 250;
    const WM_MOUSEWHEEL_MSG: u32 = 0x020A;
    const WM_MOUSEHWHEEL_MSG: u32 = 0x020E;
    const MK_CONTROL: usize = 0x0008;
    const WHEEL_ZOOM_STEP: f64 = 0.1;

    fn now_ms() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0)
    }

    struct DeviceInfo {
        preparsed: Vec<u8>,
        finger_collections: Vec<u16>,
    }
    unsafe impl Send for DeviceInfo {}

    #[derive(Default, Clone, Copy)]
    struct PinchState {
        dist: f64,
        mid_x: f64,
        mid_y: f64,
    }

    lazy_static::lazy_static! {
        static ref DEVICE_CACHE: Mutex<HashMap<isize, DeviceInfo>> = Mutex::new(HashMap::new());
        static ref LAST_STATE: Mutex<Option<PinchState>> = Mutex::new(None);
    }

    lazy_static::lazy_static! {
        static ref ZOOM_BASE: Mutex<Option<f64>> = Mutex::new(None);
    }

    pub fn install(raw: *mut std::ffi::c_void) {
        if raw.is_null() {
            return;
        }
        if INSTALLED_HWND.load(Ordering::Acquire) != 0 {
            return;
        }
        let hwnd = HWND(raw);
        unsafe {
            let cfg = GESTURECONFIG {
                dwID: GID_ZOOM,
                dwWant: GC_ZOOM_FLAG,
                dwBlock: 0,
            };
            let _ = SetGestureConfig(
                hwnd,
                0,
                std::slice::from_ref(&cfg),
                std::mem::size_of::<GESTURECONFIG>() as u32,
            );

            let prev = SetWindowLongPtrW(hwnd, GWLP_WNDPROC, wnd_proc as *const () as usize as _);
            if prev == 0 {
                return;
            }
            PREV_WNDPROC.store(prev as usize, Ordering::Release);
            INSTALLED_HWND.store(raw as usize, Ordering::Release);

            let rid = RAWINPUTDEVICE {
                usUsagePage: HID_USAGE_PAGE_DIGITIZER,
                usUsage: HID_USAGE_TOUCH_PAD,
                dwFlags: RIDEV_INPUTSINK,
                hwndTarget: hwnd,
            };
            let _ = RegisterRawInputDevices(
                std::slice::from_ref(&rid),
                std::mem::size_of::<RAWINPUTDEVICE>() as u32,
            );
        }
    }

    unsafe extern "system" fn wnd_proc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        if msg == WM_INPUT {
            handle_hid_input(HRAWINPUT(lparam.0 as *mut std::ffi::c_void));
        }
        if msg == WM_MOUSEWHEEL_MSG || msg == WM_MOUSEHWHEEL_MSG {
            let last = LAST_TWO_FINGER_MS.load(Ordering::Acquire);
            if last != 0 && now_ms().saturating_sub(last) < WHEEL_SUPPRESS_MS {
                return LRESULT(0);
            }
        }
        if msg == WM_MOUSEWHEEL_MSG && (wparam.0 & MK_CONTROL) != 0 {
            let notches = (((wparam.0 >> 16) & 0xFFFF) as u16) as i16 as f64 / 120.0;
            if notches != 0.0 {
                let factor = (1.0 + notches * WHEEL_ZOOM_STEP)
                    .clamp(PINCH_FACTOR_CLAMP_MIN, PINCH_FACTOR_CLAMP_MAX);
                if super::dispatch_zoom_delta(factor) {
                    return LRESULT(0);
                }
            }
        }
        if msg == WM_GESTURE {
            let mut gi: GESTUREINFO = std::mem::zeroed();
            gi.cbSize = std::mem::size_of::<GESTUREINFO>() as u32;
            let hgi = HGESTUREINFO(lparam.0 as *mut std::ffi::c_void);
            if GetGestureInfo(hgi, &mut gi).is_ok() {
                if gi.dwID == GID_ZOOM.0 {
                    let distance = gi.ullArguments as f64;
                    let mut base = ZOOM_BASE.lock().unwrap();
                    let begin = (gi.dwFlags & GF_BEGIN) != 0;
                    let end = (gi.dwFlags & GF_END) != 0;
                    if begin || base.is_none() {
                        *base = Some(distance);
                    } else if let Some(prev) = *base {
                        if prev > 0.0 && distance > 0.0 {
                            let factor = distance / prev;
                            if factor.is_finite() && factor > 0.0 && factor != 1.0 {
                                super::dispatch_zoom_delta(factor);
                            }
                            *base = Some(distance);
                        }
                    }
                    if end {
                        *base = None;
                    }
                }
                let _ = CloseGestureInfoHandle(hgi);
                return LRESULT(0);
            }
        }

        let prev = PREV_WNDPROC.load(Ordering::Acquire);
        if prev != 0 {
            let wnd: WNDPROC = std::mem::transmute(prev as *const ());
            CallWindowProcW(wnd, hwnd, msg, wparam, lparam)
        } else {
            LRESULT(0)
        }
    }

    unsafe fn handle_hid_input(hraw: HRAWINPUT) {
        let header_size = std::mem::size_of::<RAWINPUTHEADER>() as u32;
        let mut size: u32 = 0;
        if GetRawInputData(hraw, RID_INPUT, None, &mut size, header_size) == u32::MAX
            || size == 0
        {
            return;
        }
        let mut buf = vec![0u8; size as usize];
        let got = GetRawInputData(
            hraw,
            RID_INPUT,
            Some(buf.as_mut_ptr() as *mut std::ffi::c_void),
            &mut size,
            header_size,
        );
        if got == u32::MAX || got == 0 {
            return;
        }
        let raw_ptr = buf.as_ptr() as *const RAWINPUT;
        let header = &(*raw_ptr).header;
        if header.dwType != RIM_TYPEHID.0 {
            return;
        }
        let hdevice = header.hDevice;
        if hdevice.0.is_null() {
            return;
        }
        let hid_data = &(*raw_ptr).data.hid;
        let report_size = hid_data.dwSizeHid as usize;
        let report_count = hid_data.dwCount as usize;
        if report_size == 0 || report_count == 0 {
            return;
        }

        let mut cache = DEVICE_CACHE.lock().unwrap();
        let key = hdevice.0 as isize;
        if !cache.contains_key(&key) {
            match init_device_info(hdevice) {
                Some(info) => {
                    cache.insert(key, info);
                }
                None => return,
            }
        }
        let info = cache.get(&key).unwrap();
        let preparsed = PHIDP_PREPARSED_DATA(info.preparsed.as_ptr() as isize);

        for r_idx in 0..report_count {
            let report_offset = r_idx * report_size;
            let report_ptr = (&hid_data.bRawData as *const u8).add(report_offset) as *mut u8;
            let report = std::slice::from_raw_parts_mut(report_ptr, report_size);

            let mut contacts: Vec<(f64, f64)> = Vec::with_capacity(5);
            for &coll in &info.finger_collections {
                let mut usages: [u16; 8] = [0; 8];
                let mut count: u32 = usages.len() as u32;
                let r = HidP_GetUsages(
                    HidP_Input,
                    HID_USAGE_PAGE_DIGITIZER,
                    Some(coll),
                    usages.as_mut_ptr(),
                    &mut count,
                    preparsed,
                    report,
                );
                if r.is_err() {
                    continue;
                }
                let tip_pressed =
                    usages[..count as usize].iter().any(|&u| u == HID_USAGE_TIP_SWITCH);
                if !tip_pressed {
                    continue;
                }
                let mut x: u32 = 0;
                if HidP_GetUsageValue(
                    HidP_Input,
                    HID_USAGE_PAGE_GENERIC_DESKTOP,
                    Some(coll),
                    HID_USAGE_X,
                    &mut x,
                    preparsed,
                    report,
                )
                .is_err()
                {
                    continue;
                }
                let mut y: u32 = 0;
                if HidP_GetUsageValue(
                    HidP_Input,
                    HID_USAGE_PAGE_GENERIC_DESKTOP,
                    Some(coll),
                    HID_USAGE_Y,
                    &mut y,
                    preparsed,
                    report,
                )
                .is_err()
                {
                    continue;
                }
                contacts.push((x as f64, y as f64));
            }

            let mut state = LAST_STATE.lock().unwrap();
            if contacts.len() == 2 {
                let dx = contacts[0].0 - contacts[1].0;
                let dy = contacts[0].1 - contacts[1].1;
                let dist = (dx * dx + dy * dy).sqrt();
                let mid_x = (contacts[0].0 + contacts[1].0) * 0.5;
                let mid_y = (contacts[0].1 + contacts[1].1) * 0.5;
                if let Some(prev) = *state {
                    if prev.dist > 1.0 && dist > 1.0 {
                        let d_dist = dist - prev.dist;
                        let mid_dx = (mid_x - prev.mid_x).abs();
                        let mid_dy = (mid_y - prev.mid_y).abs();
                        let mid_motion = mid_dx + mid_dy;
                        let is_pinch = d_dist.abs() > PINCH_MIN_ABS_DELTA
                            && d_dist.abs() > mid_motion * PINCH_DOMINANCE
                            && mid_motion < PINCH_MAX_MID_MOTION;
                        let raw_factor = dist / prev.dist;
                        let factor = 1.0 + (raw_factor - 1.0) * PINCH_DAMP;
                        if is_pinch
                            && factor.is_finite()
                            && (factor - 1.0).abs() > PINCH_FACTOR_THRESHOLD
                        {
                            let clamped =
                                factor.clamp(PINCH_FACTOR_CLAMP_MIN, PINCH_FACTOR_CLAMP_MAX);
                            LAST_TWO_FINGER_MS.store(now_ms(), Ordering::Release);
                            super::dispatch_zoom_delta(clamped);
                        }
                    }
                }
                *state = Some(PinchState {
                    dist,
                    mid_x,
                    mid_y,
                });
            } else {
                *state = None;
            }
        }
    }

    unsafe fn init_device_info(hdevice: HANDLE) -> Option<DeviceInfo> {
        let mut size: u32 = 0;
        if GetRawInputDeviceInfoW(Some(hdevice), RIDI_PREPARSEDDATA, None, &mut size) == u32::MAX
            || size == 0
        {
            return None;
        }
        let mut buf = vec![0u8; size as usize];
        let got = GetRawInputDeviceInfoW(
            Some(hdevice),
            RIDI_PREPARSEDDATA,
            Some(buf.as_mut_ptr() as *mut std::ffi::c_void),
            &mut size,
        );
        if got == u32::MAX {
            return None;
        }
        let preparsed = PHIDP_PREPARSED_DATA(buf.as_ptr() as isize);

        let mut caps = HIDP_CAPS::default();
        if HidP_GetCaps(preparsed, &mut caps).is_err() {
            return None;
        }
        let mut vc_count_in_out = caps.NumberInputValueCaps;
        let mut vc_buf: Vec<HIDP_VALUE_CAPS> =
            vec![HIDP_VALUE_CAPS::default(); vc_count_in_out as usize];
        if vc_count_in_out == 0
            || HidP_GetValueCaps(
                HidP_Input,
                vc_buf.as_mut_ptr(),
                &mut vc_count_in_out,
                preparsed,
            )
            .is_err()
        {
            return None;
        }
        vc_buf.truncate(vc_count_in_out as usize);

        let mut finger_collections: Vec<u16> = Vec::new();
        for vc in &vc_buf {
            if vc.UsagePage == HID_USAGE_PAGE_GENERIC_DESKTOP && !vc.IsRange {
                let usage = vc.Anonymous.NotRange.Usage;
                if usage == HID_USAGE_X && !finger_collections.contains(&vc.LinkCollection) {
                    finger_collections.push(vc.LinkCollection);
                }
            }
        }
        if finger_collections.is_empty() {
            return None;
        }
        Some(DeviceInfo {
            preparsed: buf,
            finger_collections,
        })
    }
}
