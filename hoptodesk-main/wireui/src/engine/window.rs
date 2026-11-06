use super::dom::{Document, NodeKey};
use super::host::{self, EngineRef, LoadedPage};
use super::layout::TextSystem;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Instant;
use winit::event::{ElementState, Event, MouseButton, WindowEvent};
use winit::event_loop::{ControlFlow, EventLoop};
use winit::keyboard::{Key, NamedKey};
use winit::window::{Window, WindowBuilder};

// The UI DLL this engine replaces called WSAStartup when it loaded and never
// released it, so Winsock stayed initialized for the whole process. Nothing else
// holds that reference now, so an unbalanced WSACleanup elsewhere in the process
// (a dependency tearing a socket down at the end of a session) drops the count to
// zero and every later socket call fails with WSANOTINITIALISED -- the signal
// connection dies moments after a session disconnects. Take one reference at load
// and deliberately never release it, restoring the old behaviour.
#[cfg(target_os = "windows")]
fn pin_winsock() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        #[link(name = "ws2_32")]
        extern "system" {
            fn WSAStartup(version: u16, data: *mut u8) -> i32;
        }
        // WSADATA's layout differs between 32- and 64-bit; a zeroed over-sized
        // buffer is valid for either and we never read the contents back.
        let mut wsa_data = [0u8; 512];
        unsafe {
            WSAStartup(0x0202, wsa_data.as_mut_ptr());
        }
    });
}

#[cfg(not(target_os = "windows"))]
fn pin_winsock() {}

// winit insists on owning the NSApplication principal class, but the client's
// mac startup (show_dock/make_menubar) touches AppKit before run_app() ever
// runs. Creating the event loop pre-main -- before any of that -- is the only
// point the frozen client source guarantees; run_window then consumes it.
#[cfg(target_os = "macos")]
std::thread_local! {
    static PREMADE_LOOP: std::cell::RefCell<Option<EventLoop<()>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(target_os = "macos")]
static WINIT_DELEGATE: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

#[cfg(target_os = "macos")]
#[ctor::ctor]
fn gpu_probe_ctor() {
    if std::env::var_os("WIREUI_GPU_PROBE").is_some() {
        run_gpu_probe();
    }
}

#[cfg(target_os = "macos")]
#[ctor::ctor]
fn premake_event_loop() {
    if std::env::var_os("WIREUI_GPU_PROBE").is_some() {
        return;
    }
    // winit's EventLoop::new PANICS (not Err) if NSApplication was already
    // initialized as the plain AppKit class before this ctor ran (a ctor-order
    // hazard). Catch it so the process degrades to creating the loop later
    // instead of aborting at load.
    if let Ok(Ok(el)) = std::panic::catch_unwind(|| EventLoop::new()) {
        unsafe {
            use objc::runtime::Object;
            use objc::{class, msg_send, sel, sel_impl};
            let app: *mut Object = msg_send![class!(NSApplication), sharedApplication];
            let del: *mut Object = msg_send![app, delegate];
            WINIT_DELEGATE.store(del as usize, std::sync::atomic::Ordering::SeqCst);
        }
        PREMADE_LOOP.with(|p| *p.borrow_mut() = Some(el));
    }
}

// The client's mac startup replaces NSApp.delegate with its own AppDelegate
// (menubar actions, dock reopen, URL scheme, terminate cleanup), which starves
// winit of applicationDidFinishLaunching -- the run loop never starts pumping
// and every input event is dropped. Install a runtime-built delegate that
// keeps BOTH alive: launch/terminate go to both, everything else forwards to
// whichever delegate implements it (the client's first).
#[cfg(target_os = "macos")]
fn install_delegate_mux() {
    use objc::declare::ClassDecl;
    use objc::runtime::{Class, Object, Sel, BOOL, NO, YES};
    use objc::{class, msg_send, sel, sel_impl};

    unsafe {
        let winit_del = WINIT_DELEGATE.load(std::sync::atomic::Ordering::SeqCst) as *mut Object;
        if winit_del.is_null() {
            return;
        }
        let app: *mut Object = msg_send![class!(NSApplication), sharedApplication];
        let current: *mut Object = msg_send![app, delegate];
        if current.is_null() || current == winit_del {
            return;
        }

        const IVAR_APP: &str = "muxAppDelegate";
        const IVAR_WINIT: &str = "muxWinitDelegate";

        extern "C" fn responds(this: &Object, _: Sel, sel: Sel) -> BOOL {
            unsafe {
                let a = *this.get_ivar::<*mut Object>("muxAppDelegate");
                let w = *this.get_ivar::<*mut Object>("muxWinitDelegate");
                let ra: BOOL = msg_send![a, respondsToSelector: sel];
                let rw: BOOL = msg_send![w, respondsToSelector: sel];
                if ra != NO || rw != NO {
                    YES
                } else {
                    NO
                }
            }
        }
        extern "C" fn forward_target(this: &Object, _: Sel, sel: Sel) -> *mut Object {
            unsafe {
                let a = *this.get_ivar::<*mut Object>("muxAppDelegate");
                let w = *this.get_ivar::<*mut Object>("muxWinitDelegate");
                let ra: BOOL = msg_send![a, respondsToSelector: sel];
                if ra != NO {
                    a
                } else {
                    w
                }
            }
        }
        extern "C" fn did_finish(this: &Object, _: Sel, note: *mut Object) {
            unsafe {
                let a = *this.get_ivar::<*mut Object>("muxAppDelegate");
                let w = *this.get_ivar::<*mut Object>("muxWinitDelegate");
                let ra: BOOL = msg_send![a, respondsToSelector: sel!(applicationDidFinishLaunching:)];
                if ra != NO {
                    let _: () = msg_send![a, applicationDidFinishLaunching: note];
                }
                let _: () = msg_send![w, applicationDidFinishLaunching: note];
            }
        }
        extern "C" fn will_terminate(this: &Object, _: Sel, note: *mut Object) {
            unsafe {
                let a = *this.get_ivar::<*mut Object>("muxAppDelegate");
                let w = *this.get_ivar::<*mut Object>("muxWinitDelegate");
                let _: () = msg_send![w, applicationWillTerminate: note];
                let ra: BOOL = msg_send![a, respondsToSelector: sel!(applicationWillTerminate:)];
                if ra != NO {
                    let _: () = msg_send![a, applicationWillTerminate: note];
                }
            }
        }

        let cls: *const Class = {
            let existing = Class::get("WireuiDelegateMux");
            match existing {
                Some(c) => c,
                None => {
                    let mut decl = match ClassDecl::new("WireuiDelegateMux", class!(NSObject)) {
                        Some(d) => d,
                        None => return,
                    };
                    decl.add_ivar::<*mut Object>(IVAR_APP);
                    decl.add_ivar::<*mut Object>(IVAR_WINIT);
                    decl.add_method(
                        sel!(respondsToSelector:),
                        responds as extern "C" fn(&Object, Sel, Sel) -> BOOL,
                    );
                    decl.add_method(
                        sel!(forwardingTargetForSelector:),
                        forward_target as extern "C" fn(&Object, Sel, Sel) -> *mut Object,
                    );
                    decl.add_method(
                        sel!(applicationDidFinishLaunching:),
                        did_finish as extern "C" fn(&Object, Sel, *mut Object),
                    );
                    decl.add_method(
                        sel!(applicationWillTerminate:),
                        will_terminate as extern "C" fn(&Object, Sel, *mut Object),
                    );
                    decl.register()
                }
            }
        };

        let mux: *mut Object = msg_send![cls, alloc];
        let mux: *mut Object = msg_send![mux, init];
        let _: *mut Object = msg_send![current, retain];
        let _: *mut Object = msg_send![winit_del, retain];
        (*mux).set_ivar::<*mut Object>(IVAR_APP, current);
        (*mux).set_ivar::<*mut Object>(IVAR_WINIT, winit_del);
        let _: () = msg_send![app, setDelegate: mux];
    }
}

fn take_event_loop() -> Result<EventLoop<()>, String> {
    #[cfg(target_os = "macos")]
    {
        install_delegate_mux();
        if let Some(el) = PREMADE_LOOP.with(|p| p.borrow_mut().take()) {
            el.listen_device_events(winit::event_loop::DeviceEvents::Never);
            return Ok(el);
        }
    }
    let el = EventLoop::new().map_err(|e| format!("event loop: {}", e))?;
    el.listen_device_events(winit::event_loop::DeviceEvents::Never);
    Ok(el)
}

// A secondary (child) window opened via view.window — the chat window. Its own
// engine + surface; the interpreter is shared with the main window.
struct ChildWin {
    window: Arc<Window>,
    gpu: Gpu,
    text_system: TextSystem,
    engine: EngineRef,
    scale: f32,
    cursor: (f32, f32),
    press_hit: Option<NodeKey>,
    scrollbar_drag: Option<host::ScrollDrag>,
}

pub fn is_shortcut_chord(alt: bool, ctrl: bool, cmd: bool, mac: bool) -> bool {
    if mac {
        cmd
    } else {
        ctrl && !alt
    }
}

pub fn modifiers_suppress_text(alt: bool, ctrl: bool, mac: bool) -> bool {
    if mac {
        alt || ctrl
    } else {
        alt != ctrl
    }
}

// Layout + paint a window's engine into its surface. Shared by the main window's
// inline repaint and the child window.
fn repaint_win(gpu: &mut Gpu, engine: &EngineRef, ts: &mut TextSystem, scale: f32) {
    let styles_rc = host::cached_computed_styles(engine);
    let styles = styles_rc.as_ref();
    let pseudo_rc = host::cached_pseudo_boxes(engine, styles);
    let vw = gpu.width() as f32 / scale;
    let vh = gpu.height() as f32 / scale;
    let layout = {
        let e = engine.borrow();
        super::layout::layout_document(&e.doc, &styles, ts, (vw, vh), scale)
    };
    host::sync_textarea_scroll(engine, &layout);
    let (scene, layout_rects, cursors) = {
        let e = engine.borrow();
        let pseudo = pseudo_rc.as_ref();
        let scene = super::paint::paint_document_overlaid(
            &e.doc, &styles, &layout, scale, &e.video_sinks, &pseudo, e.now_ms,
            &e.content_overlays,
        );
        let cursors: std::collections::HashMap<NodeKey, super::style::Cursor> =
            styles.iter().map(|(k, s)| (*k, s.cursor)).collect();
        (scene, layout.rects.clone(), cursors)
    };
    let (screen, order) = host::compute_screen_geometry(engine, &layout_rects, &styles);
    {
        let mut em = engine.borrow_mut();
        em.last_rects = layout_rects;
        em.screen_rects = screen;
        em.screen_order = order;
        em.hover_cursors = cursors;
    }
    gpu.present(&scene);
}

#[cfg(target_os = "windows")]
fn native_window_handle(window: &Window) -> usize {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    let Ok(handle) = window.window_handle() else { return 0 };
    let RawWindowHandle::Win32(w) = handle.as_raw() else { return 0 };
    w.hwnd.get() as usize
}

#[cfg(not(target_os = "windows"))]
fn native_window_handle(_window: &Window) -> usize {
    0
}

fn publish_main_window_handle(window: &Window) {
    crate::host::set_main_window_handle(native_window_handle(window));
}

// A custom-chrome (decorations-off) window has square corners on Windows 11 by
// default; opt into the rounded-corner + shadow treatment normal windows get.
#[cfg(target_os = "windows")]
fn apply_rounded_corners(window: &Window) {
    // Win11-only attribute; skip on anything older to avoid poking DWM on
    // versions that never round corners anyway.
    if os_needs_cpu_backend() {
        return;
    }
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    let Ok(handle) = window.window_handle() else { return };
    let RawWindowHandle::Win32(w) = handle.as_raw() else { return };
    #[link(name = "dwmapi")]
    extern "system" {
        fn DwmSetWindowAttribute(
            hwnd: isize,
            attr: u32,
            pv: *const std::ffi::c_void,
            cb: u32,
        ) -> i32;
    }
    const DWMWA_WINDOW_CORNER_PREFERENCE: u32 = 33;
    const DWMWCP_ROUND: u32 = 2;
    let pref: u32 = DWMWCP_ROUND;
    unsafe {
        DwmSetWindowAttribute(
            w.hwnd.get(),
            DWMWA_WINDOW_CORNER_PREFERENCE,
            &pref as *const u32 as *const std::ffi::c_void,
            std::mem::size_of::<u32>() as u32,
        );
    }
}

#[cfg(not(target_os = "windows"))]
fn apply_rounded_corners(_window: &Window) {}

#[cfg(target_os = "windows")]
fn set_window_cloak(window: &Window, cloaked: bool) {
    if os_needs_cpu_backend() {
        return;
    }
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    let Ok(handle) = window.window_handle() else { return };
    let RawWindowHandle::Win32(w) = handle.as_raw() else { return };
    #[link(name = "dwmapi")]
    extern "system" {
        fn DwmSetWindowAttribute(
            hwnd: isize,
            attr: u32,
            pv: *const std::ffi::c_void,
            cb: u32,
        ) -> i32;
    }
    const DWMWA_CLOAK: u32 = 13;
    let on: i32 = if cloaked { 1 } else { 0 };
    unsafe {
        DwmSetWindowAttribute(
            w.hwnd.get(),
            DWMWA_CLOAK,
            &on as *const i32 as *const std::ffi::c_void,
            std::mem::size_of::<i32>() as u32,
        );
    }
}

#[cfg(not(target_os = "windows"))]
fn set_window_cloak(_window: &Window, _cloaked: bool) {}

// The caption icon is a DIFFERENT size from the taskbar/alt-tab icon (16px at
// 100% DPI, 20px at 125%, 24px at 150%, vs 32px), and winit's set_window_icon
// feeds one bitmap to both ICON_SMALL and ICON_BIG -- so the 32x32 PNG gets
// squashed by USER32's crude scaler and the title bar looks ragged. res/icon.ico
// is embedded by winres under resource id 1 with hand-tuned 16/24/32/48 frames,
// so ask Windows for each size straight from that group the way Sciter does.
#[cfg(target_os = "windows")]
fn apply_window_icon(window: &Window) {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    let Ok(handle) = window.window_handle() else { return };
    let RawWindowHandle::Win32(w) = handle.as_raw() else { return };
    let hwnd = w.hwnd.get();

    #[link(name = "user32")]
    extern "system" {
        fn GetSystemMetrics(index: i32) -> i32;
        fn LoadImageW(
            hinst: isize,
            name: *const u16,
            image_type: u32,
            cx: i32,
            cy: i32,
            load: u32,
        ) -> isize;
        fn SendMessageW(hwnd: isize, msg: u32, wparam: usize, lparam: isize) -> isize;
    }
    #[cfg(target_pointer_width = "64")]
    #[link(name = "user32")]
    extern "system" {
        fn SetClassLongPtrW(hwnd: isize, index: i32, value: isize) -> usize;
    }
    #[cfg(target_pointer_width = "32")]
    #[link(name = "user32")]
    extern "system" {
        fn SetClassLongW(hwnd: isize, index: i32, value: i32) -> u32;
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn GetModuleHandleW(name: *const u16) -> isize;
        fn LoadLibraryA(name: *const u8) -> isize;
        fn GetProcAddress(module: isize, name: *const u8) -> *const std::ffi::c_void;
    }

    const SM_CXICON: i32 = 11;
    const SM_CYICON: i32 = 12;
    const SM_CXSMICON: i32 = 49;
    const SM_CYSMICON: i32 = 50;
    const WM_SETICON: u32 = 0x0080;
    const ICON_SMALL: usize = 0;
    const ICON_BIG: usize = 1;
    const GCLP_HICON: i32 = -14;
    const GCLP_HICONSM: i32 = -34;
    const IMAGE_ICON: u32 = 1;
    const ICON_RESOURCE_ID: *const u16 = 1 as *const u16;

    type LoadIconWithScaleDownFn =
        unsafe extern "system" fn(isize, *const u16, i32, i32, *mut isize) -> i32;

    unsafe {
        let hinst = GetModuleHandleW(std::ptr::null());
        if hinst == 0 {
            return;
        }
        // LoadIconWithScaleDown (comctl32) picks the best frame and scales it
        // down smoothly for the sizes the icon has no exact frame for (20px at
        // 125% DPI); LoadImageW is the fallback and stretches instead.
        let comctl = LoadLibraryA(b"comctl32.dll\0".as_ptr());
        let scale_down: Option<LoadIconWithScaleDownFn> = if comctl != 0 {
            let proc = GetProcAddress(comctl, b"LoadIconWithScaleDown\0".as_ptr());
            if proc.is_null() {
                None
            } else {
                Some(std::mem::transmute::<
                    *const std::ffi::c_void,
                    LoadIconWithScaleDownFn,
                >(proc))
            }
        } else {
            None
        };

        let load = |cx: i32, cy: i32| -> isize {
            if let Some(f) = scale_down {
                let mut hicon: isize = 0;
                if f(hinst, ICON_RESOURCE_ID, cx, cy, &mut hicon) == 0 && hicon != 0 {
                    return hicon;
                }
            }
            LoadImageW(hinst, ICON_RESOURCE_ID, IMAGE_ICON, cx, cy, 0)
        };

        #[cfg(target_pointer_width = "64")]
        let set_class_icon = |index: i32, icon: isize| {
            SetClassLongPtrW(hwnd, index, icon);
        };
        #[cfg(target_pointer_width = "32")]
        let set_class_icon = |index: i32, icon: isize| {
            SetClassLongW(hwnd, index, icon as i32);
        };

        let small = load(GetSystemMetrics(SM_CXSMICON), GetSystemMetrics(SM_CYSMICON));
        if small != 0 {
            SendMessageW(hwnd, WM_SETICON, ICON_SMALL, small);
            set_class_icon(GCLP_HICONSM, small);
        }
        let big = load(GetSystemMetrics(SM_CXICON), GetSystemMetrics(SM_CYICON));
        if big != 0 {
            SendMessageW(hwnd, WM_SETICON, ICON_BIG, big);
            set_class_icon(GCLP_HICON, big);
        }
    }
}

#[cfg(not(target_os = "windows"))]
fn apply_window_icon(_window: &Window) {}

// Pop the native window system menu (Restore/Move/Size/Minimize/Maximize/Close)
// for a custom-chrome window that has no OS title bar to summon it, the way
// Sciter's role="window-icon" does. `icon` is the icon element's rect (x,y,w,h)
// in logical view px; the menu opens at its bottom-left. A double-click on the
// icon closes the window, matching a native title bar: the second click
// dismisses the just-opened menu, so it is reconstructed here as a fast dismiss
// with the button still down and the pointer still over the icon (those guards
// keep a stray dismiss from ever closing a live session). Returns true when the
// menu was shown (Windows only), so the caller falls back to the drag gesture on
// other platforms and when the window has no system menu.
#[cfg(target_os = "windows")]
fn show_window_system_menu(window: &Window, icon: (f32, f32, f32, f32), scale: f32) -> bool {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    let Ok(handle) = window.window_handle() else { return false };
    let RawWindowHandle::Win32(w) = handle.as_raw() else { return false };
    let hwnd = w.hwnd.get();
    let Ok(origin) = window.inner_position() else { return false };

    #[repr(C)]
    struct POINT {
        x: i32,
        y: i32,
    }
    #[link(name = "user32")]
    extern "system" {
        fn GetSystemMenu(hwnd: isize, revert: i32) -> isize;
        fn EnableMenuItem(hmenu: isize, item: u32, flags: u32) -> i32;
        fn SetForegroundWindow(hwnd: isize) -> i32;
        fn TrackPopupMenu(
            hmenu: isize,
            flags: u32,
            x: i32,
            y: i32,
            reserved: i32,
            hwnd: isize,
            rect: *const std::ffi::c_void,
        ) -> i32;
        fn PostMessageW(hwnd: isize, msg: u32, wparam: usize, lparam: isize) -> i32;
        fn GetCursorPos(pt: *mut POINT) -> i32;
        fn GetAsyncKeyState(vkey: i32) -> i16;
        fn GetDoubleClickTime() -> u32;
    }

    const MF_BYCOMMAND: u32 = 0x0000;
    const MF_ENABLED: u32 = 0x0000;
    const MF_GRAYED: u32 = 0x0001;
    const TPM_RETURNCMD: u32 = 0x0100;
    const TPM_LEFTBUTTON: u32 = 0x0000;
    const WM_NULL: u32 = 0x0000;
    const WM_SYSCOMMAND: u32 = 0x0112;
    const VK_LBUTTON: i32 = 0x01;
    const SC_SIZE: u32 = 0xF000;
    const SC_MOVE: u32 = 0xF010;
    const SC_MINIMIZE: u32 = 0xF020;
    const SC_MAXIMIZE: u32 = 0xF030;
    const SC_CLOSE: u32 = 0xF060;
    const SC_RESTORE: u32 = 0xF120;

    let maximized = window.is_maximized();
    let en = |on: bool| MF_BYCOMMAND | (if on { MF_ENABLED } else { MF_GRAYED });
    let ix = origin.x + (icon.0 * scale) as i32;
    let iy = origin.y + (icon.1 * scale) as i32;
    let iw = (icon.2 * scale) as i32;
    let ih = (icon.3 * scale) as i32;

    unsafe {
        let menu = GetSystemMenu(hwnd, 0);
        if menu == 0 {
            return false;
        }
        // Enable/disable to match the native menu for the current window state.
        EnableMenuItem(menu, SC_RESTORE, en(maximized));
        EnableMenuItem(menu, SC_MOVE, en(!maximized));
        EnableMenuItem(menu, SC_SIZE, en(!maximized));
        EnableMenuItem(menu, SC_MINIMIZE, MF_BYCOMMAND | MF_ENABLED);
        EnableMenuItem(menu, SC_MAXIMIZE, en(!maximized));
        EnableMenuItem(menu, SC_CLOSE, MF_BYCOMMAND | MF_ENABLED);
        // TrackPopupMenu needs a foreground window to dismiss on an outside click,
        // and the trailing WM_NULL works around the documented dismissal quirk.
        SetForegroundWindow(hwnd);
        let opened = Instant::now();
        let cmd = TrackPopupMenu(
            menu,
            TPM_RETURNCMD | TPM_LEFTBUTTON,
            ix,
            iy + ih,
            0,
            hwnd,
            std::ptr::null(),
        );
        PostMessageW(hwnd, WM_NULL, 0, 0);
        if cmd != 0 {
            PostMessageW(hwnd, WM_SYSCOMMAND, cmd as usize, 0);
            return true;
        }
        // No selection: a second click on the icon (double-click) dismisses the
        // menu. Reconstruct that as a fast dismiss with the left button still
        // down and the pointer still over the icon -> close. The button-down and
        // on-icon guards stop an Escape or click-elsewhere dismiss from ever
        // closing the window.
        let fast = opened.elapsed().as_millis() as u32 <= GetDoubleClickTime();
        let btn_down = (GetAsyncKeyState(VK_LBUTTON) as u16 & 0x8000) != 0;
        let mut pt = POINT { x: 0, y: 0 };
        let on_icon = GetCursorPos(&mut pt) != 0
            && pt.x >= ix
            && pt.x < ix + iw
            && pt.y >= iy
            && pt.y < iy + ih;
        if fast && btn_down && on_icon {
            PostMessageW(hwnd, WM_SYSCOMMAND, SC_CLOSE as usize, 0);
        }
    }
    true
}

#[cfg(not(target_os = "windows"))]
fn show_window_system_menu(_window: &Window, _icon: (f32, f32, f32, f32), _scale: f32) -> bool {
    false
}

type SharedLayoutCache =
    Rc<std::cell::RefCell<Option<(u64, u32, u32, super::layout::LayoutResult)>>>;

// The main-window frame painter. A plain fn (not a closure) so the modal drag
// loop registered as a host hook can repaint through shared Rc state while the
// winit handler is blocked inside view.doEvent(#untilMouseUp).
fn clamp_view_size(
    w: f64,
    h: f64,
    monitor: Option<winit::monitor::MonitorHandle>,
) -> (f64, f64) {
    let Some(m) = monitor else { return (w, h) };
    let sf = m.scale_factor();
    let max_w = m.size().width as f64 / sf;
    let max_h = m.size().height as f64 / sf;
    if max_w < 1.0 || max_h < 1.0 {
        return (w, h);
    }
    (w.min(max_w), h.min(max_h))
}

fn monitor_contains(m: &winit::monitor::MonitorHandle, x: f64, y: f64) -> bool {
    let p = m.position();
    let s = m.size();
    x >= p.x as f64
        && y >= p.y as f64
        && x < p.x as f64 + s.width as f64
        && y < p.y as f64 + s.height as f64
}

fn primary_workarea_physical(
    primary: Option<winit::monitor::MonitorHandle>,
) -> Option<(f64, f64, f64, f64)> {
    os_workarea_physical().or_else(|| {
        primary.map(|m| {
            let p = m.position();
            let s = m.size();
            (p.x as f64, p.y as f64, s.width as f64, s.height as f64)
        })
    })
}

fn offscreen_rescue_physical(
    monitors: &[winit::monitor::MonitorHandle],
    primary: Option<winit::monitor::MonitorHandle>,
    (x, y, w, h): (f64, f64, f64, f64),
) -> Option<(f64, f64)> {
    let (cx, cy) = (x + w / 2.0, y + h / 2.0);
    if cfg!(target_os = "macos")
        || monitors.is_empty()
        || monitors.iter().any(|m| monitor_contains(m, cx, cy))
    {
        return None;
    }
    let (ax, ay, aw, ah) = primary_workarea_physical(primary)?;
    Some((
        ax + ((aw - w) / 2.0).max(0.0),
        ay + ((ah - h) / 2.0).max(0.0),
    ))
}

fn visible_logical_position(
    monitors: &[winit::monitor::MonitorHandle],
    primary: Option<winit::monitor::MonitorHandle>,
    (x, y, w, h): (f64, f64, f64, f64),
    sf: f64,
) -> (f64, f64) {
    match offscreen_rescue_physical(monitors, primary, (x * sf, y * sf, w * sf, h * sf)) {
        Some((px, py)) => (px / sf, py / sf),
        None => (x, y),
    }
}

fn bring_onto_a_monitor(w: &Window) {
    let Ok(p) = w.outer_position() else { return };
    let s = w.outer_size();
    let monitors: Vec<_> = w.available_monitors().collect();
    if let Some((x, y)) = offscreen_rescue_physical(
        &monitors,
        w.primary_monitor(),
        (p.x as f64, p.y as f64, s.width as f64, s.height as f64),
    ) {
        w.set_outer_position(winit::dpi::PhysicalPosition::new(x, y));
    }
}

fn repaint_main(
    gpu: &mut Gpu,
    engine: &EngineRef,
    ts: &mut TextSystem,
    rects_out: &mut std::collections::HashMap<NodeKey, (f32, f32, f32, f32)>,
    cache: &mut Option<(u64, u32, u32, super::layout::LayoutResult)>,
    scale: f32,
) {
    let scene = build_main_scene(gpu.width(), gpu.height(), engine, ts, cache, scale);
    if let Some((_, _, _, layout)) = cache.as_ref() {
        *rects_out = layout.rects.clone();
    }
    #[cfg(any(target_os = "windows", target_os = "macos"))]
    {
        let e = engine.borrow();
        super::a11y::push(&e.doc, rects_out, scale);
    }
    gpu.present(&scene);
}

fn build_main_scene(
    dev_w: u32,
    dev_h: u32,
    engine: &EngineRef,
    ts: &mut TextSystem,
    cache: &mut Option<(u64, u32, u32, super::layout::LayoutResult)>,
    scale: f32,
) -> super::paint_backend::PaintScene {
    let styles_rc = host::cached_computed_styles(engine);
    let styles = styles_rc.as_ref();
    let pseudo_rc = host::cached_pseudo_boxes(engine, styles);
    let vw = dev_w as f32 / scale;
    let vh = dev_h as f32 / scale;
    let epoch = super::dom::layout_epoch();
    let stale = match cache {
        Some((ep, w, h, _)) => *ep != epoch || *w != dev_w || *h != dev_h,
        None => true,
    };
    if stale {
        let layout = {
            let e = engine.borrow();
            super::layout::layout_document(&e.doc, &styles, ts, (vw, vh), scale)
        };
        *cache = Some((epoch, dev_w, dev_h, layout));
    }
    if let Some((_, _, _, layout)) = cache.as_ref() {
        host::sync_textarea_scroll(engine, layout);
    }
    let e = engine.borrow();
    let layout = &cache.as_ref().unwrap().3;
    let pseudo = pseudo_rc.as_ref();
    let mut scene = super::paint::paint_document_overlaid(
        &e.doc,
        &styles,
        layout,
        scale,
        &e.video_sinks,
        &pseudo,
        e.now_ms,
        &e.content_overlays,
    );
    let layout_rects = layout.rects.clone();
    let cursors: std::collections::HashMap<NodeKey, super::style::Cursor> =
        styles.iter().map(|(k, s)| (*k, s.cursor)).collect();
    let tooltip = e.tooltip.clone();
    drop(e);
    if let Some((text, tx, ty)) = tooltip {
        super::paint::paint_tooltip(&mut scene, ts, &text, tx, ty, (vw, vh), scale);
    }
    let (screen, order) = host::compute_screen_geometry(engine, &layout_rects, &styles);
    host::update_layout_rects(engine, &layout_rects);
    {
        let mut em = engine.borrow_mut();
        em.screen_rects = screen;
        em.screen_order = order;
        em.hover_cursors = cursors;
    }
    // paintForeground overlays (the file-transfer drag ghost) above the page.
    {
        let e = engine.borrow();
        for (iw, ih, rgba, x, y, w, h, op) in e.fg_overlays.iter() {
            super::paint::paint_overlay_image(
                &mut scene,
                *iw,
                *ih,
                rgba.clone(),
                *x as f64,
                *y as f64,
                *w as f64,
                *h as f64,
                *op,
                scale as f64,
            );
        }
    }
    // Custom pointer image (element.style.cursor -- the remote host's
    // cursor shape) painted at the pointer when it's over the element. Skipped
    // on the software renderer, where the OS cursor is left drawn natively so a
    // mouse move needn't trigger a full-scene repaint (see the CursorMoved
    // handler).
    {
        let e = engine.borrow();
        if !cfg!(windows) && !super::paint_backend::backend_is_cpu() && !e.cursor_images.is_empty() {
            let (px, py) = e.pointer;
            let hit = hit_test_ordered(&e.doc, &e.screen_rects, &e.screen_order, px, py);
            if let Some((w, h, rgba, hotx, hoty)) = cursor_image_at(&e, hit) {
                super::paint::paint_pointer_image(
                    &mut scene,
                    w,
                    h,
                    rgba,
                    (px - hotx) as f64 * scale as f64,
                    (py - hoty) as f64 * scale as f64,
                    scale as f64,
                );
            }
        }
    }
    scene
}

// Poll the OS for the pointer position (window-logical coords) and left-button
// state without pumping the event queue -- the modal drag loop runs while the
// winit handler is blocked, so queued events must stay queued.
#[cfg(target_os = "windows")]
fn os_mouse_state(window: &Window) -> Option<(f32, f32, bool)> {
    use winapi::shared::windef::POINT;
    use winapi::um::winuser::{GetAsyncKeyState, GetCursorPos, VK_LBUTTON};
    let mut pt = POINT { x: 0, y: 0 };
    if unsafe { GetCursorPos(&mut pt) } == 0 {
        return None;
    }
    let origin = window.inner_position().ok()?;
    let sf = window.scale_factor() as f32;
    let x = (pt.x - origin.x) as f32 / sf;
    let y = (pt.y - origin.y) as f32 / sf;
    let down = (unsafe { GetAsyncKeyState(VK_LBUTTON) } as u16 & 0x8000) != 0;
    Some((x, y, down))
}

#[cfg(target_os = "macos")]
fn os_mouse_state(window: &Window) -> Option<(f32, f32, bool)> {
    use objc::runtime::Object;
    use objc::{class, msg_send, sel, sel_impl};
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct NSPoint {
        x: f64,
        y: f64,
    }
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct NSRect {
        origin: NSPoint,
        size: NSPoint,
    }
    unsafe {
        let loc: NSPoint = msg_send![class!(NSEvent), mouseLocation];
        let buttons: u64 = msg_send![class!(NSEvent), pressedMouseButtons];
        let screens: *mut Object = msg_send![class!(NSScreen), screens];
        let primary: *mut Object = msg_send![screens, firstObject];
        if primary.is_null() {
            return None;
        }
        let frame: NSRect = msg_send![primary, frame];
        // mouseLocation is bottom-left-origin points; winit positions are
        // top-left-origin physical px.
        let sf = window.scale_factor();
        let gx = loc.x;
        let gy = frame.size.y - loc.y;
        let origin = window.inner_position().ok()?;
        let x = (gx - origin.x as f64 / sf) as f32;
        let y = (gy - origin.y as f64 / sf) as f32;
        Some((x, y, buttons & 1 != 0))
    }
}

// X11: XQueryPointer on the window gives pointer coords relative to it plus the
// button mask, without touching winit's connection or event queue. A dedicated
// Display (cached per thread) keeps the modal-loop poll off winit's socket.
#[cfg(any(target_os = "linux", target_os = "freebsd"))]
fn os_mouse_state(window: &Window) -> Option<(f32, f32, bool)> {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use std::cell::RefCell;
    thread_local! {
        static X11: RefCell<Option<(x11_dl::xlib::Xlib, *mut x11_dl::xlib::Display)>> =
            const { RefCell::new(None) };
    }
    let win: std::os::raw::c_ulong = match window.window_handle().ok()?.as_raw() {
        RawWindowHandle::Xlib(h) => h.window,
        RawWindowHandle::Xcb(h) => h.window.get() as std::os::raw::c_ulong,
        _ => return None,
    };
    X11.with(|cell| {
        let mut slot = cell.borrow_mut();
        if slot.is_none() {
            let xlib = x11_dl::xlib::Xlib::open().ok()?;
            let display = unsafe { (xlib.XOpenDisplay)(std::ptr::null()) };
            if display.is_null() {
                return None;
            }
            *slot = Some((xlib, display));
        }
        let (xlib, display) = slot.as_ref().unwrap();
        let mut root = 0;
        let mut child = 0;
        let (mut root_x, mut root_y, mut win_x, mut win_y) = (0, 0, 0, 0);
        let mut mask = 0u32;
        let ok = unsafe {
            (xlib.XQueryPointer)(
                *display, win, &mut root, &mut child, &mut root_x, &mut root_y, &mut win_x,
                &mut win_y, &mut mask,
            )
        };
        if ok == 0 {
            // Pointer left the window's screen; fall back to root-relative so
            // the drag still tracks toward the other pane.
            let sf = window.scale_factor() as f32;
            let origin = window.inner_position().ok()?;
            let x = (root_x - origin.x) as f32 / sf;
            let y = (root_y - origin.y) as f32 / sf;
            return Some((x, y, mask & (1 << 8) != 0));
        }
        let sf = window.scale_factor() as f32;
        Some((win_x as f32 / sf, win_y as f32 / sf, mask & (1 << 8) != 0))
    })
}

#[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux", target_os = "freebsd")))]
fn os_mouse_state(_window: &Window) -> Option<(f32, f32, bool)> {
    None
}

// Windows marks a thread "Not Responding" if it stops calling Peek/GetMessage
// for ~5s; a NOREMOVE peek keeps it alive without dispatching (queued input
// stays queued for winit to handle after the modal loop returns).
#[cfg(target_os = "windows")]
fn keep_queue_alive() {
    use winapi::um::winuser::{PeekMessageW, MSG, PM_NOREMOVE};
    let mut msg: MSG = unsafe { std::mem::zeroed() };
    unsafe { PeekMessageW(&mut msg, std::ptr::null_mut(), 0, 0, PM_NOREMOVE) };
}

#[cfg(not(target_os = "windows"))]
fn keep_queue_alive() {}

#[cfg(target_os = "windows")]
fn heal_fullscreen_taskbar(window: &Window) {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use std::cell::Cell;
    use winapi::shared::windef::{HWND, RECT};
    use winapi::um::winuser::{
        FindWindowW, GetForegroundWindow, GetWindow, GetWindowRect, IntersectRect, GW_HWNDPREV,
    };

    thread_local! {
        static LAST_HEAL_CHECK: Cell<Option<Instant>> = const { Cell::new(None) };
    }

    let due = LAST_HEAL_CHECK.with(|c| match c.get() {
        Some(t) if t.elapsed().as_millis() < 1000 => false,
        _ => {
            c.set(Some(Instant::now()));
            true
        }
    });
    if !due {
        return;
    }

    let hwnd: HWND = match window.window_handle().map(|h| h.as_raw()) {
        Ok(RawWindowHandle::Win32(w)) => w.hwnd.get() as HWND,
        _ => return,
    };

    let tray_above = unsafe {
        if GetForegroundWindow() != hwnd {
            return;
        }
        let mut wr: RECT = std::mem::zeroed();
        if GetWindowRect(hwnd, &mut wr) == 0 {
            return;
        }
        let overlapping_tray = |tray: HWND| {
            if tray.is_null() {
                return false;
            }
            let mut tr: RECT = std::mem::zeroed();
            let mut out: RECT = std::mem::zeroed();
            GetWindowRect(tray, &mut tr) != 0 && IntersectRect(&mut out, &wr, &tr) != 0
        };
        let tray_class: Vec<u16> = "Shell_TrayWnd".encode_utf16().chain(Some(0)).collect();
        let tray2_class: Vec<u16> = "Shell_SecondaryTrayWnd"
            .encode_utf16()
            .chain(Some(0))
            .collect();
        let tray = FindWindowW(tray_class.as_ptr(), std::ptr::null());
        let tray2 = FindWindowW(tray2_class.as_ptr(), std::ptr::null());
        let mut found = false;
        let mut above = GetWindow(hwnd, GW_HWNDPREV);
        let mut steps = 0;
        while !above.is_null() && steps < 512 {
            if (above == tray || above == tray2) && overlapping_tray(above) {
                found = true;
                break;
            }
            above = GetWindow(above, GW_HWNDPREV);
            steps += 1;
        }
        found
    };
    if !tray_above {
        return;
    }

    window.set_fullscreen(None);
    window.set_fullscreen(Some(winit::window::Fullscreen::Borderless(None)));
}

#[cfg(any(target_os = "linux", target_os = "freebsd"))]
mod raise {
    use winit::window::{Window, WindowId};

    const FRAMES: u8 = 60;
    thread_local! {
        static PENDING: std::cell::Cell<Option<(WindowId, u8)>> =
            const { std::cell::Cell::new(None) };
    }

    pub fn request(w: &Window) {
        PENDING.with(|c| c.set(Some((w.id(), FRAMES))));
    }

    pub fn when_mapped(w: &Window) {
        let Some((id, left)) = PENDING.with(|c| c.get()) else {
            return;
        };
        if id != w.id() {
            return;
        }
        if w.is_visible() == Some(true) && w.is_minimized() != Some(true) {
            w.focus_window();
            PENDING.with(|c| c.set(None));
        } else if left == 1 {
            PENDING.with(|c| c.set(None));
        } else {
            PENDING.with(|c| c.set(Some((id, left - 1))));
        }
    }
}

#[cfg(not(any(target_os = "linux", target_os = "freebsd")))]
mod raise {
    use winit::window::Window;

    pub fn request(_: &Window) {}
    pub fn when_mapped(_: &Window) {}
}

fn apply_window_command(w: &Window, cmd: host::WindowCommand) {
    match cmd {
        // Publish the target state immediately: script sequences read
        // view.windowState right after assigning it (the fullscreen toggle),
        // and the AboutToWait sync from the real window lags the OS.
        host::WindowCommand::State(s) => {
            host::set_current_window_state(s);
            match s {
                1 => {
                    let was_minimized = w.is_minimized() == Some(true);
                    let was_hidden = w.is_visible() == Some(false) && !was_minimized;
                    w.set_minimized(false);
                    w.set_fullscreen(None);
                    w.set_maximized(false);
                    if was_hidden {
                        bring_onto_a_monitor(w);
                    }
                    w.set_visible(true);
                    if was_hidden || was_minimized {
                        raise::request(w);
                    }
                }
                2 => {
                    let hidden = w.is_visible() == Some(false);
                    if cfg!(any(target_os = "linux", target_os = "freebsd")) && hidden {
                        w.set_visible(true);
                    }
                    w.set_minimized(true);
                    if cfg!(target_os = "windows") && hidden {
                        w.set_visible(true);
                    }
                }
                3 => {
                    w.set_fullscreen(None);
                    w.set_maximized(true);
                }
                4 => w.set_visible(false),
                5 => w.set_fullscreen(Some(winit::window::Fullscreen::Borderless(None))),
                _ => {}
            }
        }
        host::WindowCommand::Topmost(t) => w.set_window_level(if t {
            winit::window::WindowLevel::AlwaysOnTop
        } else {
            winit::window::WindowLevel::Normal
        }),
        // Sciter's windowMinSize/MaxSize take raw device pixels (the UI applies
        // scaleIt() itself where it wants DPI scaling).
        host::WindowCommand::MinSize(mw, mh) => {
            w.set_min_inner_size(Some(winit::dpi::PhysicalSize::new(mw, mh)));
        }
        host::WindowCommand::MaxSize(mw, mh) => {
            w.set_max_inner_size(Some(winit::dpi::PhysicalSize::new(mw, mh)));
        }
        host::WindowCommand::Resizable(t) => w.set_resizable(t),
        host::WindowCommand::Maximizable(t) => {
            let mut buttons = w.enabled_buttons();
            buttons.set(winit::window::WindowButtons::MAXIMIZE, t);
            w.set_enabled_buttons(buttons);
        }
        host::WindowCommand::Activate => {
            w.set_minimized(false);
            w.set_visible(true);
            w.focus_window();
        }
        host::WindowCommand::Caption(s) => w.set_title(&s),
        host::WindowCommand::Monitor(n) => {
            let target = usize::try_from(n).ok().and_then(|i| w.available_monitors().nth(i));
            if let Some(m) = target {
                w.set_outer_position(m.position());
            }
        }
        host::WindowCommand::Icon(iw, ih, rgba) => {
            if let Ok(icon) = winit::window::Icon::from_rgba(rgba.as_ref().clone(), iw, ih) {
                w.set_window_icon(Some(icon));
            }
        }
    }
}

fn load_app_icon() -> Option<winit::window::Icon> {
    static ICON_PNG: &[u8] = include_bytes!("../../../res/32x32.png");
    let img = image::load_from_memory(ICON_PNG).ok()?.to_rgba8();
    let (w, h) = img.dimensions();
    winit::window::Icon::from_rgba(img.into_raw(), w, h).ok()
}

// The GPU device/queue/renderer are SHARED across windows (main + chat). Creating
// a second independent device+surface tripped a fatal D3D12 error (DXGI
// INVALID_CALL in write_texture) that crashed the whole app; one device with a
// surface per window is the correct multi-window pattern and far lighter.
struct GpuCore {
    instance: wgpu::Instance,
    adapter: wgpu::Adapter,
    device: wgpu::Device,
    queue: wgpu::Queue,
    renderer: std::cell::RefCell<vello::Renderer>,
    format: wgpu::TextureFormat,
}

// CPU presentation (no usable GPU adapter, or WIREUI_FORCE_CPU=1): the
// display list is rasterized with tiny-skia and blitted via softbuffer.
struct CpuCtx {
    _context: softbuffer::Context<Arc<Window>>,
    surface: softbuffer::Surface<Arc<Window>, Arc<Window>>,
    width: u32,
    height: u32,
}

enum GpuBackend {
    Wgpu {
        core: std::rc::Rc<GpuCore>,
        surface: wgpu::Surface<'static>,
        config: wgpu::SurfaceConfiguration,
    },
    Cpu(CpuCtx),
}

struct Gpu {
    backend: GpuBackend,
}

fn surface_config(
    format: wgpu::TextureFormat,
    caps: &wgpu::SurfaceCapabilities,
    size: winit::dpi::PhysicalSize<u32>,
) -> wgpu::SurfaceConfiguration {
    wgpu::SurfaceConfiguration {
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        format,
        width: size.width.max(1),
        height: size.height.max(1),
        present_mode: wgpu::PresentMode::AutoVsync,
        alpha_mode: caps.alpha_modes[0],
        view_formats: vec![],
        desired_maximum_frame_latency: 2,
    }
}

// wgpu's dx12/vulkan probing is not survivable on Windows 7/8 (access
// violation inside the backend loaders, before request_adapter can fail
// cleanly), and no wgpu backend is viable there anyway -- go straight to the
// softbuffer CPU path on anything older than Windows 10.
#[cfg(target_os = "windows")]
fn os_needs_cpu_backend() -> bool {
    #[repr(C)]
    struct OsVersionInfoW {
        size: u32,
        major: u32,
        minor: u32,
        build: u32,
        platform_id: u32,
        csd: [u16; 128],
    }
    #[link(name = "ntdll")]
    extern "system" {
        fn RtlGetVersion(info: *mut OsVersionInfoW) -> i32;
    }
    unsafe {
        let mut info: OsVersionInfoW = std::mem::zeroed();
        info.size = std::mem::size_of::<OsVersionInfoW>() as u32;
        let ok = RtlGetVersion(&mut info) == 0;
        ok && info.major < 10
    }
}

#[cfg(not(target_os = "windows"))]
fn os_needs_cpu_backend() -> bool {
    false
}

#[cfg(target_os = "macos")]
fn macos_major_version() -> Option<i64> {
    unsafe {
        use objc::runtime::Object;
        use objc::{class, msg_send, sel, sel_impl};
        #[repr(C)]
        struct NSOperatingSystemVersion {
            major: i64,
            minor: i64,
            patch: i64,
        }
        let pi: *mut Object = msg_send![class!(NSProcessInfo), processInfo];
        if pi.is_null() {
            return None;
        }
        let v: NSOperatingSystemVersion = msg_send![pi, operatingSystemVersion];
        Some(v.major)
    }
}

#[cfg(target_os = "macos")]
fn macos_gpu_needs_cpu_backend() -> bool {
    if std::env::var("WIREUI_FORCE_GPU").map_or(false, |v| v == "1") {
        return false;
    }
    let instance = wgpu::Instance::default();
    pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        compatible_surface: None,
        force_fallback_adapter: false,
    }))
    .map_or(false, |a| {
        let info = a.get_info();
        let name = info.name.to_lowercase();
        let intel_integrated_before_monterey =
            matches!(info.device_type, wgpu::DeviceType::IntegratedGpu)
                && name.contains("intel")
                && macos_major_version().map_or(false, |v| v < 12);
        let amd_discrete = matches!(info.device_type, wgpu::DeviceType::DiscreteGpu)
            && (name.contains("amd") || name.contains("radeon"));
        let needs_cpu = intel_integrated_before_monterey || amd_discrete;
        needs_cpu
    })
}

#[cfg(not(target_os = "macos"))]
fn macos_gpu_needs_cpu_backend() -> bool {
    false
}

// Whether to enumerate the OS font collection. The scan is the only source of
// glyphs outside the embedded Latin @font-face, so without it every CJK, Thai,
// Arabic, Hebrew and Devanagari run shapes blank. It used to be off below
// Windows 10 because fontique's DirectWrite backend unwrapped the Windows
// 8.1-only IDWriteFactory2; that is optional now, so Win7/8 scan too.
fn os_system_fonts_ok() -> bool {
    std::env::var("WIREUI_NO_SYSTEM_FONTS").map_or(true, |v| v != "1")
}

// The primary monitor's workarea (screen minus taskbar) in physical px.
// winit has no workarea API; view.screenBox(#workarea) needs the real thing on
// Windows so the saved-maximized restore comparison can pass.
#[cfg(target_os = "windows")]
fn os_workarea_physical() -> Option<(f64, f64, f64, f64)> {
    #[repr(C)]
    struct Rect {
        left: i32,
        top: i32,
        right: i32,
        bottom: i32,
    }
    #[link(name = "user32")]
    extern "system" {
        fn SystemParametersInfoW(
            action: u32,
            param: u32,
            pv: *mut std::ffi::c_void,
            win_ini: u32,
        ) -> i32;
    }
    const SPI_GETWORKAREA: u32 = 0x0030;
    unsafe {
        let mut r = Rect { left: 0, top: 0, right: 0, bottom: 0 };
        if SystemParametersInfoW(SPI_GETWORKAREA, 0, &mut r as *mut Rect as *mut _, 0) != 0 {
            Some((
                r.left as f64,
                r.top as f64,
                (r.right - r.left) as f64,
                (r.bottom - r.top) as f64,
            ))
        } else {
            None
        }
    }
}

#[cfg(not(target_os = "windows"))]
fn os_workarea_physical() -> Option<(f64, f64, f64, f64)> {
    None
}

fn window_instance() -> wgpu::Instance {
    let mut desc = wgpu::InstanceDescriptor::default();
    if cfg!(target_os = "windows") {
        desc.backends -= wgpu::Backends::GL;
    }
    wgpu::Instance::new(desc)
}

fn window_renderer_options(format: wgpu::TextureFormat) -> vello::RendererOptions {
    vello::RendererOptions {
        surface_format: Some(format),
        use_cpu: false,
        antialiasing_support: vello::AaSupport::area_only(),
        num_init_threads: if cfg!(target_os = "macos") {
            std::num::NonZeroUsize::new(1)
        } else {
            None
        },
    }
}

// vello's GPU renderer needs Vulkan/DX12/Metal compute on REAL hardware.
// Every software path through wgpu is worse than our own CPU raster:
// the GL software adapters (VM "SVGA3D; Mesa", llvmpipe) hang vello, and
// DX12 WARP runs it but interaction-breaking slow (seconds per paint in a
// VM). So the ladder is: real hardware GPU, else the softbuffer CPU
// backend. adapter_is_suitable rejects every software/virtual adapter.
fn adapter_is_suitable(a: &wgpu::Adapter) -> bool {
    let info = a.get_info();
    let name = info.name.to_lowercase();
    let broadwell = intel_broadwell_on_dx12(&info)
        && std::env::var("WIREUI_FORCE_GPU").map_or(true, |v| v != "1");
    !broadwell
        && !matches!(info.backend, wgpu::Backend::Gl)
        && !matches!(info.device_type, wgpu::DeviceType::Cpu)
        && !name.contains("llvmpipe")
        && !name.contains("softpipe")
        && !name.contains("svga3d")
        && !name.contains("lavapipe")
        && !name.contains("basic render")
        && !name.contains("microsoft basic")
}

fn intel_broadwell_on_dx12(info: &wgpu::AdapterInfo) -> bool {
    matches!(info.backend, wgpu::Backend::Dx12)
        && info.vendor == 0x8086
        && info.device >> 8 == 0x16
}

enum GpuPrewarm {
    ForceCpu,
    Ready {
        instance: wgpu::Instance,
        adapter: wgpu::Adapter,
        device: wgpu::Device,
        queue: wgpu::Queue,
        renderer: vello::Renderer,
        format: wgpu::TextureFormat,
    },
    Failed(String),
}

fn spawn_gpu_prewarm() -> std::sync::mpsc::Receiver<GpuPrewarm> {
    let (tx, rx) = std::sync::mpsc::channel();
    let _ = std::thread::Builder::new()
        .name("wireui-gpu-prewarm".into())
        .spawn(move || {
            let force_cpu = crate::cpu_backend_forced()
                || std::env::var("WIREUI_FORCE_CPU").map_or(false, |v| v == "1")
                || os_needs_cpu_backend()
                || gpu_is_blocked()
                || macos_gpu_needs_cpu_backend()
                || !gpu_probe_passes();
            if force_cpu {
                let _ = tx.send(GpuPrewarm::ForceCpu);
                return;
            }
            relaunch_on_gpu_panic();
            // Mark before touching the GPU; cleared after the first frame renders
            // (see run_window_source). A hard abort here leaves the mark for the
            // next launch to count as a strike.
            gpu_mark_attempt();
            let _ = tx.send(pollster::block_on(Gpu::prewarm_async()));
        });
    rx
}

impl Gpu {
    fn new(window: Arc<Window>, prewarm: std::sync::mpsc::Receiver<GpuPrewarm>) -> Result<Gpu, String> {
        match prewarm.recv() {
            Ok(GpuPrewarm::Ready { instance, adapter, device, queue, renderer, format }) => {
                match Self::from_prewarm(window.clone(), instance, adapter, device, queue, renderer, format) {
                    Ok(gpu) => {
                        super::paint_backend::set_backend_cpu(false);
                        return Ok(gpu);
                    }
                    Err(_) => {
                        match pollster::block_on(Self::new_async(window.clone())) {
                            Ok(gpu) => {
                                super::paint_backend::set_backend_cpu(false);
                                return Ok(gpu);
                            }
                            Err(e) => {
                                eprintln!("wireui: gpu init failed ({}), falling back to cpu rendering", e);
                                gpu_mark_success();
                            }
                        }
                    }
                }
            }
            Ok(GpuPrewarm::Failed(e)) => {
                eprintln!("wireui: gpu init failed ({}), falling back to cpu rendering", e);
                gpu_mark_success();
            }
            Ok(GpuPrewarm::ForceCpu) | Err(_) => {}
        }
        let gpu = Self::new_cpu(window)?;
        super::paint_backend::set_backend_cpu(true);
        Ok(gpu)
    }

    async fn prewarm_async() -> GpuPrewarm {
        let instance = window_instance();
        let adapter = match instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: None,
                force_fallback_adapter: false,
            })
            .await
            .filter(adapter_is_suitable)
        {
            Some(a) => a,
            None => {
                return GpuPrewarm::Failed(
                    "no hardware gpu adapter; using cpu rendering".into(),
                )
            }
        };
        let (device, queue) = match adapter
            .request_device(
                &wgpu::DeviceDescriptor {
                    label: Some("wireui-window"),
                    required_features: wgpu::Features::empty(),
                    required_limits: wgpu::Limits::default(),
                    memory_hints: wgpu::MemoryHints::default(),
                },
                None,
            )
            .await
        {
            Ok(v) => v,
            Err(e) => return GpuPrewarm::Failed(format!("device: {}", e)),
        };
        // Never let a wgpu error abort the process (the default handler panics).
        device.on_uncaptured_error(Box::new(note_gpu_error));
        let format = wgpu::TextureFormat::Bgra8Unorm;
        match vello::Renderer::new(&device, window_renderer_options(format)) {
            Ok(renderer) => GpuPrewarm::Ready { instance, adapter, device, queue, renderer, format },
            Err(e) => GpuPrewarm::Failed(format!("renderer: {}", e)),
        }
    }

    fn from_prewarm(
        window: Arc<Window>,
        instance: wgpu::Instance,
        adapter: wgpu::Adapter,
        device: wgpu::Device,
        queue: wgpu::Queue,
        renderer: vello::Renderer,
        prewarm_format: wgpu::TextureFormat,
    ) -> Result<Gpu, String> {
        let size = window.inner_size();
        let surface = instance
            .create_surface(window)
            .map_err(|e| format!("surface: {}", e))?;
        if !adapter.is_surface_supported(&surface) {
            return Err("prewarmed adapter cannot present to the window surface".into());
        }
        let caps = surface.get_capabilities(&adapter);
        if caps.formats.is_empty() {
            return Err("prewarmed adapter reports no surface formats".into());
        }
        let format = caps
            .formats
            .iter()
            .copied()
            .find(|f| !f.is_srgb())
            .unwrap_or(caps.formats[0]);
        let renderer = if format == prewarm_format {
            renderer
        } else {
            vello::Renderer::new(&device, window_renderer_options(format))
                .map_err(|e| format!("renderer: {}", e))?
        };
        let config = surface_config(format, &caps, size);
        surface.configure(&device, &config);
        let core = std::rc::Rc::new(GpuCore {
            instance,
            adapter,
            device,
            queue,
            renderer: std::cell::RefCell::new(renderer),
            format,
        });
        Ok(Gpu {
            backend: GpuBackend::Wgpu { core, surface, config },
        })
    }

    fn new_cpu(window: Arc<Window>) -> Result<Gpu, String> {
        let size = window.inner_size();
        let context = softbuffer::Context::new(window.clone())
            .map_err(|e| format!("softbuffer context: {}", e))?;
        let mut surface = softbuffer::Surface::new(&context, window)
            .map_err(|e| format!("softbuffer surface: {}", e))?;
        let (w, h) = (size.width.max(1), size.height.max(1));
        surface
            .resize(
                std::num::NonZeroU32::new(w).unwrap(),
                std::num::NonZeroU32::new(h).unwrap(),
            )
            .map_err(|e| format!("softbuffer resize: {}", e))?;
        Ok(Gpu {
            backend: GpuBackend::Cpu(CpuCtx {
                _context: context,
                surface,
                width: w,
                height: h,
            }),
        })
    }

    async fn new_async(window: Arc<Window>) -> Result<Gpu, String> {
        let size = window.inner_size();
        let instance = window_instance();
        let surface = instance
            .create_surface(window)
            .map_err(|e| format!("surface: {}", e))?;
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: Some(&surface),
                force_fallback_adapter: false,
            })
            .await
            .filter(adapter_is_suitable)
            .ok_or("no hardware gpu adapter; using cpu rendering")?;
        let (device, queue) = adapter
            .request_device(
                &wgpu::DeviceDescriptor {
                    label: Some("wireui-window"),
                    required_features: wgpu::Features::empty(),
                    required_limits: wgpu::Limits::default(),
                    memory_hints: wgpu::MemoryHints::default(),
                },
                None,
            )
            .await
            .map_err(|e| format!("device: {}", e))?;
        // Never let a wgpu error abort the process (the default handler panics).
        device.on_uncaptured_error(Box::new(note_gpu_error));
        let caps = surface.get_capabilities(&adapter);
        let format = caps
            .formats
            .iter()
            .copied()
            .find(|f| !f.is_srgb())
            .unwrap_or(caps.formats[0]);
        let renderer = vello::Renderer::new(&device, window_renderer_options(format))
            .map_err(|e| format!("renderer: {}", e))?;
        let config = surface_config(format, &caps, size);
        surface.configure(&device, &config);
        let core = std::rc::Rc::new(GpuCore {
            instance,
            adapter,
            device,
            queue,
            renderer: std::cell::RefCell::new(renderer),
            format,
        });
        Ok(Gpu {
            backend: GpuBackend::Wgpu { core, surface, config },
        })
    }

    // A second window (the chat window) reuses the shared wgpu core (two
    // independent D3D12 devices trip fatal validation); the CPU backend has
    // no shared state beyond the thread-local raster caches.
    fn for_window(core: Option<std::rc::Rc<GpuCore>>, window: Arc<Window>) -> Result<Gpu, String> {
        let core = match core {
            Some(c) => c,
            None => return Self::new_cpu(window),
        };
        let size = window.inner_size();
        let surface = core
            .instance
            .create_surface(window)
            .map_err(|e| format!("surface: {}", e))?;
        let caps = surface.get_capabilities(&core.adapter);
        let config = surface_config(core.format, &caps, size);
        surface.configure(&core.device, &config);
        Ok(Gpu {
            backend: GpuBackend::Wgpu { core, surface, config },
        })
    }

    fn core(&self) -> Option<std::rc::Rc<GpuCore>> {
        match &self.backend {
            GpuBackend::Wgpu { core, .. } => Some(core.clone()),
            GpuBackend::Cpu(_) => None,
        }
    }

    fn adapter_stamp(&self) -> Option<String> {
        match &self.backend {
            GpuBackend::Wgpu { core, .. } => {
                let info = core.adapter.get_info();
                Some(format!(
                    "{}|{}|{}|{}",
                    exe_stamp(),
                    info.name,
                    info.driver,
                    info.driver_info
                ))
            }
            GpuBackend::Cpu(_) => None,
        }
    }

    fn width(&self) -> u32 {
        match &self.backend {
            GpuBackend::Wgpu { config, .. } => config.width,
            GpuBackend::Cpu(c) => c.width,
        }
    }

    fn height(&self) -> u32 {
        match &self.backend {
            GpuBackend::Wgpu { config, .. } => config.height,
            GpuBackend::Cpu(c) => c.height,
        }
    }

    fn resize(&mut self, w: u32, h: u32) {
        if w == 0 || h == 0 {
            return;
        }
        match &mut self.backend {
            GpuBackend::Wgpu { core, surface, config } => {
                exit_to_cpu_if_gpu_faulted();
                let max = core.device.limits().max_texture_dimension_2d;
                let (w, h) = (w.min(max), h.min(max));
                if config.width == w && config.height == h {
                    return;
                }
                config.width = w;
                config.height = h;
                surface.configure(&core.device, config);
            }
            GpuBackend::Cpu(c) => {
                c.width = w;
                c.height = h;
                let _ = c.surface.resize(
                    std::num::NonZeroU32::new(w).unwrap(),
                    std::num::NonZeroU32::new(h).unwrap(),
                );
            }
        }
    }

    // Wait for the GPU to finish the work present() submitted. Called only at
    // first show; present() is otherwise non-blocking so steady-state frames
    // never stall the UI thread waiting on the GPU.
    fn flush(&self) {
        if let GpuBackend::Wgpu { core, .. } = &self.backend {
            exit_to_cpu_if_gpu_faulted();
            core.device.poll(wgpu::Maintain::Wait);
        }
    }

    // Render a scene to an offscreen texture and read it back (the drag-ghost
    // element snapshot). Blocks on the GPU; used once per drag start.
    fn render_offscreen(
        &self,
        scene: &super::paint_backend::PaintScene,
        w: u32,
        h: u32,
    ) -> Option<Vec<u8>> {
        if w == 0 || h == 0 {
            return None;
        }
        if let Some(list) = scene.as_cpu() {
            let pm = super::cpu_raster::rasterize(list, w, h);
            return Some(pm.take());
        }
        exit_to_cpu_if_gpu_faulted();
        let scene = scene.as_vello()?;
        let (core, ..) = match &self.backend {
            GpuBackend::Wgpu { core, .. } => (core, ()),
            GpuBackend::Cpu(_) => return None,
        };
        let device = &core.device;
        let queue = &core.queue;
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("wireui-snapshot"),
            size: wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = texture.create_view(&Default::default());
        core.renderer
            .borrow_mut()
            .render_to_texture(
                device,
                queue,
                scene,
                &view,
                &vello::RenderParams {
                    base_color: vello::peniko::Color::WHITE,
                    width: w,
                    height: h,
                    antialiasing_method: vello::AaConfig::Area,
                },
            )
            .ok()?;
        let bytes_per_row = (w * 4 + 255) & !255;
        let buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("wireui-snapshot-read"),
            size: bytes_per_row as u64 * h as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut enc = device.create_command_encoder(&Default::default());
        enc.copy_texture_to_buffer(
            wgpu::ImageCopyTexture {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::ImageCopyBuffer {
                buffer: &buf,
                layout: wgpu::ImageDataLayout {
                    offset: 0,
                    bytes_per_row: Some(bytes_per_row),
                    rows_per_image: Some(h),
                },
            },
            wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
        );
        queue.submit([enc.finish()]);
        let slice = buf.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| {
            tx.send(r).ok();
        });
        device.poll(wgpu::Maintain::Wait);
        // Bounded wait: a wedged readback must degrade to "no ghost", never
        // hang the UI thread.
        rx.recv_timeout(std::time::Duration::from_secs(2)).ok()?.ok()?;
        let data = slice.get_mapped_range();
        let mut out = Vec::with_capacity((w * h * 4) as usize);
        for row in 0..h {
            let s = (row * bytes_per_row) as usize;
            out.extend_from_slice(&data[s..s + (w * 4) as usize]);
        }
        drop(data);
        buf.unmap();
        Some(out)
    }

    fn present(&mut self, scene: &super::paint_backend::PaintScene) {
        match &mut self.backend {
            GpuBackend::Wgpu { core, surface, config } => {
                exit_to_cpu_if_gpu_faulted();
                let scene = match scene.as_vello() {
                    Some(s) => s,
                    None => return,
                };
                let surface_tex = match surface.get_current_texture() {
                    Ok(t) => t,
                    Err(_) => {
                        surface.configure(&core.device, config);
                        match surface.get_current_texture() {
                            Ok(t) => t,
                            Err(_) => return,
                        }
                    }
                };
                core.renderer
                    .borrow_mut()
                    .render_to_surface(
                        &core.device,
                        &core.queue,
                        scene,
                        &surface_tex,
                        &vello::RenderParams {
                            base_color: vello::peniko::Color::WHITE,
                            width: config.width,
                            height: config.height,
                            antialiasing_method: vello::AaConfig::Area,
                        },
                    )
                    .ok();
                surface_tex.present();
            }
            GpuBackend::Cpu(c) => {
                let list = match scene.as_cpu() {
                    Some(l) => l,
                    None => return,
                };
                let pm = super::cpu_raster::rasterize(list, c.width, c.height);
                if c.surface
                    .resize(
                        std::num::NonZeroU32::new(c.width.max(1)).unwrap(),
                        std::num::NonZeroU32::new(c.height.max(1)).unwrap(),
                    )
                    .is_err()
                {
                    return;
                }
                let mut buf = match c.surface.buffer_mut() {
                    Ok(b) => b,
                    Err(_) => return,
                };
                // Final alpha is 255 everywhere (opaque white base), so the
                // premultiplied bytes are the straight color values.
                for (dst, px) in buf.iter_mut().zip(pm.pixels()) {
                    *dst = 0xFF00_0000
                        | ((px.red() as u32) << 16)
                        | ((px.green() as u32) << 8)
                        | px.blue() as u32;
                }
                let _ = buf.present();
            }
        }
    }
}

pub fn hit_test(
    doc: &Document,
    rects: &std::collections::HashMap<NodeKey, (f32, f32, f32, f32)>,
    x: f32,
    y: f32,
) -> Option<NodeKey> {
    // Document-order fallback used when no paint order is cached (tests, first
    // frame). Prefer hit_test_ordered with Engine.screen_order.
    let order = doc.descendants(doc.root);
    hit_test_ordered(doc, rects, &order, x, y)
}

/// Topmost element at a point = the LAST one containing it in PAINT order
/// (z-aware, from compute_screen_geometry). Depth- or document-order picks are
/// both wrong once overlays rely on z-index: content behind the settings panel
/// stole its clicks, and a popup's backdrop (appended after the menu) stole the
/// menu's.
pub fn hit_test_ordered(
    doc: &Document,
    rects: &std::collections::HashMap<NodeKey, (f32, f32, f32, f32)>,
    order: &[NodeKey],
    x: f32,
    y: f32,
) -> Option<NodeKey> {
    let mut best: Option<NodeKey> = None;
    for &key in order {
        match doc.arena.get(key) {
            Some(n) if !n.is_text() => {}
            _ => continue,
        }
        if let Some(&(rx, ry, rw, rh)) = rects.get(&key) {
            if x >= rx && x < rx + rw && y >= ry && y < ry + rh {
                best = Some(key);
            }
        }
    }
    best
}

// For a custom-chrome (window-frame=extended) window: is this point on the
// draggable caption? True when walking up from the hit reaches a caption /
// role="window-caption" element without first passing an interactive control
// (so the toolbar buttons and the min/max/close controls still click).
fn hit_is_msgbox_overlay(doc: &Document, hit: NodeKey) -> bool {
    let mut k = Some(hit);
    while let Some(cur) = k {
        let node = match doc.arena.get(cur) {
            Some(n) => n,
            None => return false,
        };
        if node.attr("id") == Some("msgbox-overlay") {
            return true;
        }
        k = node.parent;
    }
    false
}

fn caption_drag_target(doc: &Document, hit: NodeKey) -> bool {
    let mut k = Some(hit);
    while let Some(cur) = k {
        let node = match doc.arena.get(cur) {
            Some(n) => n,
            None => return false,
        };
        match node.tag.as_str() {
            "button" | "input" | "select" | "textarea" | "a" => return false,
            // The <header> IS the titlebar in an extended-frame window; the whole
            // band drags (the caption element itself is often empty/narrow, so the
            // rest of the header must drag too), except over interactive controls
            // which are caught above.
            "caption" | "header" => return true,
            _ => {}
        }
        if node.attr("role") == Some("window-caption") || node.attr("role") == Some("window-icon") {
            return true;
        }
        k = node.parent;
    }
    false
}

// Fallback for caption drag: the point lies within the on-screen rectangle of a
// caption / role="window-caption" element. caption_drag_target only fires when
// the caption is the TOPMOST hit, so a non-interactive element painted over the
// caption band (e.g. an absolute overlay) would silently kill the drag; a
// rectangle test keeps the whole titlebar draggable regardless of paint order.
fn point_in_caption_region(e: &host::Engine, x: f32, y: f32) -> bool {
    for (&k, &(rx, ry, rw, rh)) in &e.screen_rects {
        if x >= rx && x < rx + rw && y >= ry && y < ry + rh {
            if let Some(n) = e.doc.arena.get(k) {
                if n.tag == "caption"
                    || n.tag == "header"
                    || n.attr("role") == Some("window-caption")
                {
                    return true;
                }
            }
        }
    }
    false
}

#[cfg(windows)]
#[derive(Default)]
struct NativeCursor {
    key: Option<(usize, u32, u32, u32, u32)>,
    handle: Option<isize>,
}

#[cfg(windows)]
impl Drop for NativeCursor {
    fn drop(&mut self) {
        if let Some(h) = self.handle.take() {
            unsafe {
                winapi::um::winuser::DestroyIcon(h as winapi::shared::windef::HICON);
            }
        }
    }
}

pub fn scale_cursor_rgba(
    w: u32,
    h: u32,
    rgba: &[u8],
    scale: f32,
) -> Option<(u32, u32, Vec<u8>)> {
    if w == 0 || h == 0 || rgba.len() < (w as usize) * (h as usize) * 4 {
        return None;
    }
    if scale <= 1.0 {
        return Some((w, h, rgba[..(w as usize) * (h as usize) * 4].to_vec()));
    }
    let nw = ((w as f32 * scale).round() as u32).max(1);
    let nh = ((h as f32 * scale).round() as u32).max(1);
    let mut out = vec![0u8; (nw as usize) * (nh as usize) * 4];
    for y in 0..nh {
        let sy = ((y as f32 + 0.5) / scale) as u32;
        let sy = sy.min(h - 1);
        for x in 0..nw {
            let sx = ((x as f32 + 0.5) / scale) as u32;
            let sx = sx.min(w - 1);
            let src = ((sy * w + sx) * 4) as usize;
            let dst = ((y * nw + x) * 4) as usize;
            out[dst..dst + 4].copy_from_slice(&rgba[src..src + 4]);
        }
    }
    Some((nw, nh, out))
}

#[cfg(windows)]
fn build_hcursor(w: u32, h: u32, rgba: &[u8], hotx: f32, hoty: f32, scale: f32) -> Option<isize> {
    let (w, h, scaled) = scale_cursor_rgba(w, h, rgba, scale)?;
    let rgba: &[u8] = &scaled;
    let (hotx, hoty) = (hotx * scale.max(1.0), hoty * scale.max(1.0));
    use winapi::shared::windef::{HBITMAP, HICON};
    use winapi::um::wingdi::{CreateBitmap, DeleteObject};
    use winapi::um::winuser::{CreateIconIndirect, ICONINFO};
    let mut bgra = Vec::with_capacity(rgba.len());
    for px in rgba.chunks_exact(4) {
        bgra.extend_from_slice(&[px[2], px[1], px[0], px[3]]);
    }
    unsafe {
        let color: HBITMAP = CreateBitmap(w as i32, h as i32, 1, 32, bgra.as_ptr() as *const _);
        if color.is_null() {
            return None;
        }
        let mask_stride = ((w as usize) + 15) / 16 * 2;
        let mask_bits = vec![0u8; mask_stride * (h as usize)];
        let mask: HBITMAP = CreateBitmap(w as i32, h as i32, 1, 1, mask_bits.as_ptr() as *const _);
        if mask.is_null() {
            DeleteObject(color as *mut _);
            return None;
        }
        let mut info = ICONINFO {
            fIcon: 0,
            xHotspot: hotx.max(0.0) as u32,
            yHotspot: hoty.max(0.0) as u32,
            hbmMask: mask,
            hbmColor: color,
        };
        let hicon: HICON = CreateIconIndirect(&mut info);
        DeleteObject(color as *mut _);
        DeleteObject(mask as *mut _);
        if hicon.is_null() {
            None
        } else {
            Some(hicon as isize)
        }
    }
}

fn cursor_image_at(
    e: &host::Engine,
    hit: Option<NodeKey>,
) -> Option<(u32, u32, std::sync::Arc<Vec<u8>>, f32, f32)> {
    let mut cur = hit;
    while let Some(k) = cur {
        if let Some(v) = e.cursor_images.get(&k) {
            return Some(v.clone());
        }
        cur = e.doc.arena.get(k).and_then(|n| n.parent);
    }
    None
}

#[cfg(windows)]
static ACTIVE_CURSOR: std::sync::atomic::AtomicIsize = std::sync::atomic::AtomicIsize::new(0);

#[cfg(windows)]
static BASE_WNDPROC: std::sync::atomic::AtomicIsize = std::sync::atomic::AtomicIsize::new(0);

#[cfg(windows)]
static OS_DROPPED_FILES: std::sync::Mutex<Vec<std::path::PathBuf>> =
    std::sync::Mutex::new(Vec::new());

#[cfg(windows)]
unsafe fn read_dropped_files(wparam: usize) {
    use std::os::windows::ffi::OsStringExt;
    use winapi::um::shellapi::{DragFinish, DragQueryFileW, HDROP};
    let hdrop = wparam as HDROP;
    let count = DragQueryFileW(hdrop, u32::MAX, std::ptr::null_mut(), 0);
    let files = (0..count)
        .map(|i| {
            let len = DragQueryFileW(hdrop, i, std::ptr::null_mut(), 0) as usize;
            let mut buf = vec![0u16; len + 1];
            let copied = DragQueryFileW(hdrop, i, buf.as_mut_ptr(), buf.len() as u32);
            buf.truncate(copied as usize);
            std::path::PathBuf::from(std::ffi::OsString::from_wide(&buf))
        })
        .collect();
    DragFinish(hdrop);
    if let Ok(mut pending) = OS_DROPPED_FILES.lock() {
        *pending = files;
    }
}

#[cfg(windows)]
fn take_dropped_files() -> Vec<std::path::PathBuf> {
    OS_DROPPED_FILES
        .lock()
        .map(|mut pending| std::mem::take(&mut *pending))
        .unwrap_or_default()
}

#[cfg(windows)]
fn accept_os_file_drops(window: &Window) {
    #[link(name = "ole32")]
    extern "system" {
        fn OleInitialize(reserved: *mut std::ffi::c_void) -> i32;
    }
    unsafe {
        OleInitialize(std::ptr::null_mut());
        winapi::um::shellapi::DragAcceptFiles(native_window_handle(window) as _, 1);
    }
}

#[cfg(windows)]
unsafe extern "system" fn engine_wndproc(
    hwnd: isize,
    msg: u32,
    wparam: usize,
    lparam: isize,
) -> isize {
    #[link(name = "user32")]
    extern "system" {
        fn CallWindowProcW(
            prev: isize,
            hwnd: isize,
            msg: u32,
            wparam: usize,
            lparam: isize,
        ) -> isize;
        fn DefWindowProcW(hwnd: isize, msg: u32, wparam: usize, lparam: isize) -> isize;
    }
    const WM_SETCURSOR: u32 = 0x0020;
    const WM_DROPFILES: u32 = 0x0233;
    const WM_POINTERUPDATE: u32 = 0x0245;
    const WM_POINTERUP: u32 = 0x0247;
    const HTCLIENT: isize = 1;
    let ordering = std::sync::atomic::Ordering::Relaxed;
    let touch_or_pen_input = (WM_POINTERUPDATE..=WM_POINTERUP).contains(&msg);
    if touch_or_pen_input {
        return DefWindowProcW(hwnd, msg, wparam, lparam);
    }
    if msg == WM_DROPFILES {
        read_dropped_files(wparam);
        return 0;
    }
    let is_main_window = hwnd as usize == crate::host::main_window_handle();
    if is_main_window && msg == WM_SETCURSOR && (lparam & 0xffff) == HTCLIENT {
        let active = ACTIVE_CURSOR.load(ordering);
        if active != 0 {
            winapi::um::winuser::SetCursor(active as winapi::shared::windef::HCURSOR);
            return 1;
        }
    }
    let base = BASE_WNDPROC.load(ordering);
    if base == 0 {
        return DefWindowProcW(hwnd, msg, wparam, lparam);
    }
    CallWindowProcW(base, hwnd, msg, wparam, lparam)
}

#[cfg(windows)]
fn subclass_window(window: &Window) {
    #[cfg(target_pointer_width = "64")]
    #[link(name = "user32")]
    extern "system" {
        fn SetWindowLongPtrW(hwnd: isize, index: i32, value: isize) -> isize;
    }
    #[cfg(target_pointer_width = "32")]
    #[link(name = "user32")]
    extern "system" {
        fn SetWindowLongW(hwnd: isize, index: i32, value: i32) -> i32;
    }
    #[link(name = "user32")]
    extern "system" {
        fn UnregisterTouchWindow(hwnd: isize) -> i32;
    }
    const GWLP_WNDPROC: i32 = -4;
    let hwnd = native_window_handle(window) as isize;
    if hwnd == 0 {
        return;
    }
    unsafe { UnregisterTouchWindow(hwnd) };
    let installed = engine_wndproc as *const () as usize as isize;
    #[cfg(target_pointer_width = "64")]
    let previous = unsafe { SetWindowLongPtrW(hwnd, GWLP_WNDPROC, installed) };
    #[cfg(target_pointer_width = "32")]
    let previous = unsafe { SetWindowLongW(hwnd, GWLP_WNDPROC, installed as i32) as isize };
    if previous != 0 && previous != installed {
        let ordering = std::sync::atomic::Ordering::Relaxed;
        let _ = BASE_WNDPROC.compare_exchange(0, previous, ordering, ordering);
    }
}

#[cfg(windows)]
fn apply_native_cursor(
    cache: &mut NativeCursor,
    img: Option<(u32, u32, std::sync::Arc<Vec<u8>>, f32, f32)>,
    scale: f32,
) {
    let Some((w, h, rgba, hotx, hoty)) = img else {
        cache.key = None;
        ACTIVE_CURSOR.store(0, std::sync::atomic::Ordering::Relaxed);
        if let Some(old) = cache.handle.take() {
            unsafe {
                winapi::um::winuser::DestroyIcon(old as winapi::shared::windef::HICON);
            }
        }
        return;
    };
    let key = (
        std::sync::Arc::as_ptr(&rgba) as usize,
        w,
        h,
        hotx.to_bits(),
        hoty.to_bits().wrapping_add(scale.to_bits()),
    );
    if cache.key != Some(key) {
        let Some(next) = build_hcursor(w, h, &rgba, hotx, hoty, scale) else {
            return;
        };
        if let Some(old) = cache.handle.take() {
            unsafe {
                winapi::um::winuser::DestroyIcon(old as winapi::shared::windef::HICON);
            }
        }
        cache.handle = Some(next);
        cache.key = Some(key);
    }
    if let Some(h) = cache.handle {
        ACTIVE_CURSOR.store(h, std::sync::atomic::Ordering::Relaxed);
        unsafe {
            winapi::um::winuser::SetCursor(h as winapi::shared::windef::HCURSOR);
        }
    }
}

fn resize_cursor_icon(dir: winit::window::ResizeDirection) -> winit::window::CursorIcon {
    use winit::window::CursorIcon as C;
    use winit::window::ResizeDirection::*;
    match dir {
        North | South => C::NsResize,
        East | West => C::EwResize,
        NorthEast | SouthWest => C::NeswResize,
        NorthWest | SouthEast => C::NwseResize,
    }
}

// Window-control role (window-minimize/maximize/close) on the hit or an
// ancestor button -- Sciter treats these roles as the action itself.
fn window_control_role(doc: &Document, hit: NodeKey) -> Option<String> {
    let mut k = Some(hit);
    while let Some(cur) = k {
        let node = doc.arena.get(cur)?;
        if let Some(r) = node.attr("role") {
            if matches!(r, "window-minimize" | "window-maximize" | "window-close") {
                return Some(r.to_string());
            }
        }
        k = node.parent;
    }
    None
}

fn window_control_at_point(e: &host::Engine, x: f32, y: f32) -> Option<String> {
    for (&k, &(rx, ry, rw, rh)) in &e.screen_rects {
        if x >= rx && x < rx + rw && y >= ry && y < ry + rh {
            if let Some(n) = e.doc.arena.get(k) {
                if let Some(r) = n.attr("role") {
                    if matches!(r, "window-minimize" | "window-maximize" | "window-close") {
                        return Some(r.to_string());
                    }
                }
            }
        }
    }
    None
}

// The caption's app-icon element (Sciter's role="window-icon"). A left-click on
// it opens the native window system menu, matching the stock client.
fn window_icon_at(doc: &Document, hit: NodeKey) -> Option<NodeKey> {
    let mut k = Some(hit);
    while let Some(cur) = k {
        let node = doc.arena.get(cur)?;
        if node.attr("role") == Some("window-icon") {
            return Some(cur);
        }
        k = node.parent;
    }
    None
}

// True if the hit is (or is inside) an interactive control -- such a hit always
// behaves as a click, never as a window drag/resize gesture.
fn hit_is_interactive(doc: &Document, hit: NodeKey) -> bool {
    let mut k = Some(hit);
    while let Some(cur) = k {
        let node = match doc.arena.get(cur) {
            Some(n) => n,
            None => return false,
        };
        if matches!(node.tag.as_str(), "button" | "input" | "select" | "textarea" | "a") {
            return true;
        }
        k = node.parent;
    }
    false
}

fn has_resize_edges(window: &Window) -> bool {
    !window.is_maximized() && window.fullscreen().is_none()
}

// Resize edge/corner the cursor is over, within `EDGE` logical px of a border.
fn resize_direction(x: f32, y: f32, w: f32, h: f32) -> Option<winit::window::ResizeDirection> {
    use winit::window::ResizeDirection::*;
    const EDGE: f32 = 6.0;
    let (l, r) = (x < EDGE, x > w - EDGE);
    let (t, b) = (y < EDGE, y > h - EDGE);
    Some(match (l, r, t, b) {
        (true, _, true, _) => NorthWest,
        (_, true, true, _) => NorthEast,
        (true, _, _, true) => SouthWest,
        (_, true, _, true) => SouthEast,
        (true, ..) => West,
        (_, true, ..) => East,
        (_, _, true, _) => North,
        (_, _, _, true) => South,
        _ => return None,
    })
}

fn click_target_for(doc: &Document, press: NodeKey, release: NodeKey) -> Option<NodeKey> {
    let mut k = release;
    loop {
        if host::node_in_subtree(doc, k, press) {
            return Some(k);
        }
        match doc.arena.get(k).and_then(|n| n.parent) {
            Some(p) => k = p,
            None => return None,
        }
    }
}

fn raw_mouse(
    etype: i64,
    cursor: (f32, f32),
    buttons: i64,
    wheel: (f64, f64),
    mods: (bool, bool, bool, bool),
) -> host::RawMouse {
    host::RawMouse {
        etype,
        x: cursor.0,
        y: cursor.1,
        buttons,
        wheel,
        alt: mods.0,
        ctrl: mods.1,
        shift: mods.2,
        meta: mods.3,
    }
}

fn numpad_vk(code: winit::keyboard::KeyCode) -> Option<i64> {
    use winit::keyboard::KeyCode as C;
    Some(match code {
        C::Numpad0 => 0x60,
        C::Numpad1 => 0x61,
        C::Numpad2 => 0x62,
        C::Numpad3 => 0x63,
        C::Numpad4 => 0x64,
        C::Numpad5 => 0x65,
        C::Numpad6 => 0x66,
        C::Numpad7 => 0x67,
        C::Numpad8 => 0x68,
        C::Numpad9 => 0x69,
        C::NumpadMultiply => 0x6A,
        C::NumpadAdd => 0x6B,
        C::NumpadSubtract => 0x6D,
        C::NumpadDecimal => 0x6E,
        C::NumpadDivide => 0x6F,
        C::NumpadComma => 0xC2,
        _ => return None,
    })
}

fn positional_vk(code: winit::keyboard::KeyCode) -> Option<i64> {
    use winit::keyboard::KeyCode as C;
    Some(match code {
        C::KeyA => 0x41,
        C::KeyB => 0x42,
        C::KeyC => 0x43,
        C::KeyD => 0x44,
        C::KeyE => 0x45,
        C::KeyF => 0x46,
        C::KeyG => 0x47,
        C::KeyH => 0x48,
        C::KeyI => 0x49,
        C::KeyJ => 0x4A,
        C::KeyK => 0x4B,
        C::KeyL => 0x4C,
        C::KeyM => 0x4D,
        C::KeyN => 0x4E,
        C::KeyO => 0x4F,
        C::KeyP => 0x50,
        C::KeyQ => 0x51,
        C::KeyR => 0x52,
        C::KeyS => 0x53,
        C::KeyT => 0x54,
        C::KeyU => 0x55,
        C::KeyV => 0x56,
        C::KeyW => 0x57,
        C::KeyX => 0x58,
        C::KeyY => 0x59,
        C::KeyZ => 0x5A,
        C::Digit0 => 0x30,
        C::Digit1 => 0x31,
        C::Digit2 => 0x32,
        C::Digit3 => 0x33,
        C::Digit4 => 0x34,
        C::Digit5 => 0x35,
        C::Digit6 => 0x36,
        C::Digit7 => 0x37,
        C::Digit8 => 0x38,
        C::Digit9 => 0x39,
        C::Semicolon => 0xBA,
        C::Equal => 0xBB,
        C::Comma => 0xBC,
        C::Minus => 0xBD,
        C::Period => 0xBE,
        C::Slash => 0xBF,
        C::Backquote => 0xC0,
        C::IntlRo => 0xC1,
        C::BracketLeft => 0xDB,
        C::Backslash | C::IntlYen => 0xDC,
        C::BracketRight => 0xDD,
        C::Quote => 0xDE,
        C::IntlBackslash => 0xE2,
        _ => return None,
    })
}

fn vk_keycode(kev: &winit::event::KeyEvent) -> i64 {
    let physical = match kev.physical_key {
        winit::keyboard::PhysicalKey::Code(c) => Some(c),
        _ => None,
    };
    vk_keycode_of(&kev.logical_key, physical)
}

// Sciter keyCode: VK codes (letters = uppercase ASCII).
pub fn vk_keycode_of(key: &Key, physical: Option<winit::keyboard::KeyCode>) -> i64 {
    use winit::keyboard::NamedKey as N;
    match key {
        Key::Character(s) => {
            if let Some(vk) = physical.and_then(numpad_vk) {
                return vk;
            }
            match s.chars().next() {
                Some(c) if c.is_ascii_alphanumeric() => c.to_ascii_uppercase() as i64,
                _ => physical.and_then(positional_vk).unwrap_or(0),
            }
        }
        Key::Named(n) => match n {
            N::Shift => 16,
            N::Control => 17,
            N::Alt => 18,
            N::AltGraph => 165,
            N::Super | N::Meta => 91,
            N::ContextMenu => 93,
            N::Enter => 13,
            N::Escape => 27,
            N::Tab => 9,
            N::Space => 32,
            N::Backspace => 8,
            N::Delete => 46,
            N::Insert => 45,
            N::Home => 36,
            N::End => 35,
            N::PageUp => 33,
            N::PageDown => 34,
            N::ArrowUp => 38,
            N::ArrowDown => 40,
            N::ArrowLeft => 37,
            N::ArrowRight => 39,
            N::F1 => 112,
            N::F2 => 113,
            N::F3 => 114,
            N::F4 => 115,
            N::F5 => 116,
            N::F6 => 117,
            N::F7 => 118,
            N::F8 => 119,
            N::F9 => 120,
            N::F10 => 121,
            N::F11 => 122,
            N::F12 => 123,
            N::PrintScreen => 44,
            N::Pause => 19,
            _ => physical.and_then(control_vk).unwrap_or(0),
        },
        _ => physical.and_then(control_vk).unwrap_or(0),
    }
}

fn control_vk(code: winit::keyboard::KeyCode) -> Option<i64> {
    use winit::keyboard::KeyCode as C;
    Some(match code {
        C::PrintScreen => 44,
        C::Pause => 19,
        _ => return None,
    })
}

fn set_hover_chain(engine: &EngineRef, hovered: Option<NodeKey>) -> bool {
    let mut e = engine.borrow_mut();
    let mut want: Vec<NodeKey> = Vec::new();
    if let Some(h) = hovered {
        let mut cur = Some(h);
        while let Some(k) = cur {
            want.push(k);
            cur = e.doc.arena.get(k).and_then(|n| n.parent);
        }
    }
    if want == e.hover_chain {
        return false;
    }
    let old = std::mem::replace(&mut e.hover_chain, want);
    let mut changed = false;
    for key in old {
        if !e.hover_chain.contains(&key) {
            if let Some(node) = e.doc.arena.get_mut(key) {
                if node.states.hover {
                    node.states.hover = false;
                    changed = true;
                }
            }
        }
    }
    let chain = e.hover_chain.clone();
    for key in chain {
        if let Some(node) = e.doc.arena.get_mut(key) {
            if !node.states.hover {
                node.states.hover = true;
                changed = true;
            }
        }
    }
    changed
}

pub fn run_window(
    page_path: &std::path::Path,
    extra_base: Option<std::path::PathBuf>,
    platform: &str,
    size: (u32, u32),
    title: &str,
    eval: Option<&str>,
) -> Result<(), String> {
    run_window_inner(page_path, extra_base, platform, size, title, eval, None, Vec::new())
}

pub type BehaviorFactory = std::rc::Rc<dyn Fn() -> crate::bridge::SharedHandler>;

pub enum PageSource {
    Path {
        page: std::path::PathBuf,
        base: Option<std::path::PathBuf>,
    },
    Archive {
        archive: crate::engine::archive::Archive,
        page: String,
    },
}

pub fn run_window_bridged(
    page_path: &std::path::Path,
    extra_base: Option<std::path::PathBuf>,
    platform: &str,
    size: (u32, u32),
    title: &str,
    handler: Option<crate::bridge::SharedHandler>,
    behaviors: Vec<(String, BehaviorFactory)>,
) -> Result<(), String> {
    let source = PageSource::Path {
        page: page_path.to_path_buf(),
        base: extra_base,
    };
    run_window_source(source, platform, size, title, None, handler, behaviors)
}

#[allow(clippy::too_many_arguments)]
fn run_window_inner(
    page_path: &std::path::Path,
    extra_base: Option<std::path::PathBuf>,
    platform: &str,
    size: (u32, u32),
    title: &str,
    eval: Option<&str>,
    handler: Option<crate::bridge::SharedHandler>,
    behaviors: Vec<(String, BehaviorFactory)>,
) -> Result<(), String> {
    let source = PageSource::Path {
        page: page_path.to_path_buf(),
        base: extra_base,
    };
    run_window_source(source, platform, size, title, eval, handler, behaviors)
}

// GPU crash sentinel. wgpu/vello driving a broken GPU (an old/patched Mac's
// Metal, a bad Windows driver) can hard-abort during device creation or the
// first frame -- an OS-level abort we cannot catch and fall back from in
// process. So: mark before trying the GPU, clear after the first frame renders.
// Two consecutive launches dying with the mark still set block the GPU (the
// softbuffer CPU backend takes over). The block is stamped with the binary's
// identity, so an updated binary retries the GPU from scratch.
fn sentinel_dir() -> std::path::PathBuf {
    crate::state_dir()
}
fn gpu_attempt_marker() -> std::path::PathBuf {
    sentinel_dir().join("wireui-gpu-attempt")
}
fn gpu_blocked_marker() -> std::path::PathBuf {
    sentinel_dir().join("wireui-gpu-blocked")
}
fn gpu_strikes_marker() -> std::path::PathBuf {
    sentinel_dir().join("wireui-gpu-strikes")
}
fn gpu_verified_marker() -> std::path::PathBuf {
    sentinel_dir().join("wireui-gpu-verified")
}

fn exe_stamp() -> String {
    std::env::current_exe()
        .and_then(|p| std::fs::metadata(p))
        .map(|m| {
            let mtime = m
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs())
                .unwrap_or(0);
            format!("{}-{}", m.len(), mtime)
        })
        .unwrap_or_else(|_| "unknown".to_owned())
}

fn gpu_block_active() -> bool {
    let Ok(stamp) = std::fs::read_to_string(gpu_blocked_marker()) else {
        return false;
    };
    if stamp.trim() == exe_stamp() {
        return true;
    }
    let _ = std::fs::remove_file(gpu_blocked_marker());
    let _ = std::fs::remove_file(gpu_strikes_marker());
    false
}

fn gpu_is_blocked() -> bool {
    if std::env::var("WIREUI_FORCE_GPU").map_or(false, |v| v == "1") {
        return false;
    }
    if gpu_block_active() {
        return true;
    }
    if gpu_attempt_marker().exists() {
        let strikes = std::fs::read_to_string(gpu_strikes_marker())
            .ok()
            .and_then(|s| s.trim().parse::<u32>().ok())
            .unwrap_or(0)
            + 1;
        let _ = std::fs::remove_file(gpu_attempt_marker());
        if strikes >= 2 {
            let _ = std::fs::write(gpu_blocked_marker(), exe_stamp());
            let _ = std::fs::remove_file(gpu_strikes_marker());
            return true;
        }
        let _ = std::fs::write(gpu_strikes_marker(), strikes.to_string());
        return false;
    }
    false
}
fn gpu_mark_attempt() {
    let _ = std::fs::create_dir_all(sentinel_dir());
    let _ = std::fs::write(gpu_attempt_marker(), b"1");
}
fn gpu_mark_success() {
    let _ = std::fs::remove_file(gpu_attempt_marker());
    let _ = std::fs::remove_file(gpu_strikes_marker());
}

fn gpu_first_frame_drops_content(
    gpu: &Gpu,
    engine: &EngineRef,
    ts: &mut TextSystem,
    cache: &mut Option<(u64, u32, u32, super::layout::LayoutResult)>,
    scale: f32,
) -> bool {
    let Some(stamp) = gpu.adapter_stamp() else {
        return false;
    };
    if std::fs::read_to_string(gpu_verified_marker()).map_or(false, |s| s.trim() == stamp) {
        return false;
    }
    let (w, h) = (gpu.width(), gpu.height());
    if w == 0 || h == 0 {
        return false;
    }
    let reference = super::paint_backend::with_cpu_scene(|| {
        build_main_scene(w, h, engine, ts, cache, scale)
    });
    let Some(list) = reference.as_cpu() else {
        return false;
    };
    let reference_px = super::cpu_raster::rasterize(list, w, h).take();
    if !super::paint_backend::frame_has_content(&reference_px, w, h) {
        return false;
    }
    let scene = build_main_scene(w, h, engine, ts, cache, scale);
    let fake_blank = std::env::var("WIREUI_FAKE_BLANK_GPU").map_or(false, |v| v == "1");
    for _ in 0..2 {
        let Some(px) = gpu.render_offscreen(&scene, w, h) else {
            return false;
        };
        let loss = super::paint_backend::content_loss(&reference_px, &px, w, h);
        if !fake_blank && !loss.is_broken() {
            let _ = std::fs::create_dir_all(sentinel_dir());
            let _ = std::fs::write(gpu_verified_marker(), &stamp);
            return false;
        }
    }
    true
}

static GPU_FAULT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn note_gpu_error(e: wgpu::Error) {
    let text = e.to_string();
    if text.contains("lost") || text.contains("Creation of a resource failed") {
        GPU_FAULT.store(true, std::sync::atomic::Ordering::Relaxed);
    }
    eprintln!("wgpu error (non-fatal): {:?}", e);
}

fn exit_to_cpu_if_gpu_faulted() {
    if GPU_FAULT.load(std::sync::atomic::Ordering::Relaxed) && relaunch_with_cpu_backend() {
        std::process::exit(0);
    }
}

fn relaunch_on_gpu_panic() {
    static HOOK: std::sync::Once = std::sync::Once::new();
    HOOK.call_once(|| {
        let prev = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            prev(info);
            let file = info.location().map_or("", |l| l.file());
            if file.contains("wgpu") || file.contains("vello") {
                relaunch_with_cpu_backend();
            }
        }));
    });
}

fn relaunch_with_cpu_backend() -> bool {
    static RELAUNCHED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if RELAUNCHED.swap(true, std::sync::atomic::Ordering::SeqCst) {
        return false;
    }
    let _ = std::fs::create_dir_all(sentinel_dir());
    let _ = std::fs::write(gpu_blocked_marker(), exe_stamp());
    let _ = std::fs::remove_file(gpu_strikes_marker());
    let _ = std::fs::remove_file(gpu_attempt_marker());
    let Ok(exe) = std::env::current_exe() else {
        return false;
    };
    std::process::Command::new(exe)
        .args(std::env::args_os().skip(1))
        .env("WIREUI_FORCE_CPU", "1")
        .env("WIREUI_RELAUNCH_FROM", std::process::id().to_string())
        .spawn()
        .is_ok()
}

// GPU probe (macOS). Some old/patched Intel Macs have a Metal shader compiler
// (MTLCompilerService) that hard-aborts when vello's pipelines are built --
// an out-of-process abort that also tears down our process, uncatchable. The
// crash-sentinel above recovers on the NEXT launch, but the first launch still
// crashes. So before touching the GPU in-process, run the same wgpu+vello init
// in a throwaway CHILD process: if that child dies, the GPU is unusable and we
// go straight to the CPU backend -- crash-free from the very first launch. The
// result is cached in the sentinel markers so the probe runs at most once.
#[cfg(target_os = "macos")]
fn gpu_ok_marker() -> std::path::PathBuf {
    sentinel_dir().join("wireui-gpu-ok")
}

#[cfg(target_os = "macos")]
fn gpu_probe_passes() -> bool {
    let _ = std::fs::create_dir_all(sentinel_dir());
    if gpu_ok_marker().exists() {
        return true;
    }
    if gpu_block_active() {
        return false;
    }
    let ok = probe_gpu_child();
    if ok {
        let _ = std::fs::write(gpu_ok_marker(), b"1");
    } else {
        let _ = std::fs::write(gpu_blocked_marker(), exe_stamp());
    }
    ok
}

#[cfg(not(target_os = "macos"))]
fn gpu_probe_passes() -> bool {
    true
}

#[cfg(target_os = "macos")]
fn probe_gpu_child() -> bool {
    let exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(_) => return true,
    };
    std::process::Command::new(exe)
        .env("WIREUI_GPU_PROBE", "1")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map_or(true, |s| s.success())
}

#[cfg(target_os = "macos")]
fn run_gpu_probe() -> ! {
    let res = std::panic::catch_unwind(|| pollster::block_on(probe_render_async()))
        .unwrap_or_else(|_| Err("probe panicked".to_string()));
    std::process::exit(if res.is_ok() { 0 } else { 3 })
}

#[cfg(target_os = "macos")]
async fn probe_render_async() -> Result<(), String> {
    let instance = wgpu::Instance::default();
    fn is_suitable(a: &wgpu::Adapter) -> bool {
        let info = a.get_info();
        let name = info.name.to_lowercase();
        !matches!(info.backend, wgpu::Backend::Gl)
            && !matches!(info.device_type, wgpu::DeviceType::Cpu)
            && !name.contains("llvmpipe")
            && !name.contains("softpipe")
            && !name.contains("lavapipe")
            && !name.contains("basic render")
            && !name.contains("microsoft basic")
    }
    let adapter = instance
        .request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: None,
            force_fallback_adapter: false,
        })
        .await
        .filter(is_suitable)
        .ok_or("no hardware gpu adapter")?;
    let (device, queue) = adapter
        .request_device(
            &wgpu::DeviceDescriptor {
                label: Some("wireui-gpu-probe"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::default(),
                memory_hints: wgpu::MemoryHints::default(),
            },
            None,
        )
        .await
        .map_err(|e| format!("device: {}", e))?;
    let mut renderer = vello::Renderer::new(
        &device,
        vello::RendererOptions {
            surface_format: None,
            use_cpu: false,
            antialiasing_support: vello::AaSupport::area_only(),
            num_init_threads: std::num::NonZeroUsize::new(1),
        },
    )
    .map_err(|e| format!("renderer: {}", e))?;
    let (width, height) = (64u32, 64u32);
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("probe-target"),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    let scene = vello::Scene::new();
    renderer
        .render_to_texture(
            &device,
            &queue,
            &scene,
            &view,
            &vello::RenderParams {
                base_color: vello::peniko::Color::WHITE,
                width,
                height,
                antialiasing_method: vello::AaConfig::Area,
            },
        )
        .map_err(|e| format!("render: {}", e))?;
    device.poll(wgpu::Maintain::Wait);
    Ok(())
}

#[cfg(any(target_os = "linux", target_os = "freebsd"))]
fn with_desktop_app_id(builder: WindowBuilder) -> WindowBuilder {
    use winit::platform::wayland::WindowBuilderExtWayland;
    let exe_name = std::env::args_os().next().and_then(|arg0| {
        std::path::Path::new(&arg0)
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
    });
    match exe_name {
        Some(name) => builder.with_name(name.clone(), name),
        None => builder,
    }
}

#[cfg(not(any(target_os = "linux", target_os = "freebsd")))]
fn with_desktop_app_id(builder: WindowBuilder) -> WindowBuilder {
    builder
}

pub fn run_window_source(
    source: PageSource,
    platform: &str,
    size: (u32, u32),
    title: &str,
    eval: Option<&str>,
    handler: Option<crate::bridge::SharedHandler>,
    behaviors: Vec<(String, BehaviorFactory)>,
) -> Result<(), String> {
    pin_winsock();
    let event_loop = take_event_loop()?;
    let gpu_prewarm = spawn_gpu_prewarm();
    // Seed the monitor work-area BEFORE the page loads: self.ready() runs at
    // load time and sizes/positions the window from view.screenBox. Without
    // this it computed against the 1440x900 fallback, so a saved size at or
    // above that "filled the screen" and restored maximized instead.
    if let Some(m) = event_loop.primary_monitor() {
        let sf = m.scale_factor();
        let p = m.position();
        let s = m.size();
        host::set_monitor_rect((
            p.x as f64 / sf,
            p.y as f64 / sf,
            s.width as f64 / sf,
            s.height as f64 / sf,
        ));
        if let Some((wx, wy, ww, wh)) = os_workarea_physical() {
            host::set_workarea_rect((wx / sf, wy / sf, ww / sf, wh / sf));
        }
    }
    let LoadedPage {
        engine,
        mut interp,
        body: _,
    } = match source {
        PageSource::Path { page, base } => {
            host::load_page_bridged_with_behaviors(&page, base, platform, handler, behaviors)?
        }
        PageSource::Archive { archive, page } => {
            host::load_page_archive_with_behaviors(archive, &page, platform, handler, behaviors)?
        }
    };
    if let Some(code) = eval {
        interp
            .run_source(code)
            .map_err(|e| format!("eval: {}", e.0))?;
        host::drain_events(&mut interp, &engine);
    }
    let mut child_seen_epoch = super::dom::layout_epoch();
    let mut caret_blink_phase: i64 = -1;
    let mut last_input = Instant::now();
    let mut caret_solid = false;
    let frame_pending = Arc::new(std::sync::atomic::AtomicBool::new(false));
    {
        let proxy = std::sync::Mutex::new(event_loop.create_proxy());
        let pending = frame_pending.clone();
        let waker: crate::video::FrameWaker = Arc::new(move || {
            if !pending.swap(true, std::sync::atomic::Ordering::SeqCst) {
                if let Ok(p) = proxy.lock() {
                    p.send_event(()).ok();
                }
            }
        });
        host::install_video_wake(&engine, waker);
    }
    {
        // Wake the UI loop when the io thread queues an off-thread call_method.
        let proxy = std::sync::Mutex::new(event_loop.create_proxy());
        let waker: std::sync::Arc<dyn Fn() + Send + Sync> = std::sync::Arc::new(move || {
            if let Ok(p) = proxy.lock() {
                p.send_event(()).ok();
            }
        });
        host::set_offthread_waker(waker);
    }
    // window-frame="extended" (the remote window) means custom chrome: no OS
    // title bar, the page draws its own <caption> with the toolbar. Without this
    // the OS title bar and the in-page header stacked into a double header.
    // The parser wraps the page in a synthetic root <html>, so the real
    // <html window-frame="extended"> is a child -- find the attr wherever it is.
    let extended_frame = {
        let e = engine.borrow();
        let nodes: Vec<_> = e
            .doc
            .descendants(e.doc.root)
            .into_iter()
            .filter_map(|k| e.doc.arena.get(k))
            .collect();
        // Explicit window-frame="extended|none" (remote.html), OR the custom-
        // chrome structure: a page carrying its own window caption (the CM
        // window has role="window-caption"/"window-icon" but no attr, and Sciter
        // renders it frameless). index/install have neither and stay decorated.
        nodes
            .iter()
            .find_map(|n| n.attr("window-frame"))
            .map_or(false, |v| v == "extended" || v == "none")
            || nodes.iter().any(|n| {
                matches!(n.attr("role"), Some("window-caption") | Some("window-icon"))
            })
    };
    // A view.move queued during load (self.ready restoring the saved rect, or
    // centerize) becomes the CREATION geometry, so the window appears directly
    // at its final place instead of jumping after the first frame.
    let initial_move = host::take_view_move();
    let creation_monitor = event_loop
        .primary_monitor()
        .or_else(|| event_loop.available_monitors().next());
    let (initial_w, initial_h) = match initial_move {
        Some((_, _, w, h)) if w > 0.0 && h > 0.0 => clamp_view_size(w, h, creation_monitor),
        _ => (size.0 as f64, size.1 as f64),
    };
    let mut builder = WindowBuilder::new()
        .with_title(title)
        .with_inner_size(winit::dpi::LogicalSize::new(initial_w, initial_h))
        .with_window_icon(load_app_icon())
        .with_visible(false);
    // Custom-chrome windows (remote / file-transfer / CM: window-frame="extended"
    // or role="window-caption") draw their own titlebar. On Windows/Linux that
    // means decorations off. On macOS the UI relies on the NATIVE traffic-light
    // buttons instead (header.tis never renders window-buttons when is_osx), so
    // keep decorations and just extend the content under a transparent titlebar.
    #[cfg(target_os = "macos")]
    {
        use winit::platform::macos::WindowBuilderExtMacOS;
        if extended_frame {
            builder = builder
                .with_fullsize_content_view(true)
                .with_titlebar_transparent(true)
                .with_title_hidden(true);
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        builder = builder
            .with_decorations(!extended_frame)
            .with_theme(Some(winit::window::Theme::Light));
    }
    #[cfg(windows)]
    {
        use winit::platform::windows::WindowBuilderExtWindows;
        builder = builder.with_drag_and_drop(false);
    }
    builder = with_desktop_app_id(builder);
    if let Some((x, y, _, _)) = initial_move {
        let monitors: Vec<_> = event_loop.available_monitors().collect();
        let primary = event_loop.primary_monitor();
        let sf = primary.as_ref().map_or(1.0, |m| m.scale_factor());
        let (x, y) = visible_logical_position(&monitors, primary, (x, y, initial_w, initial_h), sf);
        builder = builder.with_position(winit::dpi::LogicalPosition::new(x, y));
    }
    let window = Arc::new(
        builder
            .build(&event_loop)
            .map_err(|e| format!("window: {}", e))?,
    );
    publish_main_window_handle(&window);
    #[cfg(windows)]
    subclass_window(&window);
    #[cfg(windows)]
    accept_os_file_drops(&window);
    apply_rounded_corners(&window);
    apply_window_icon(&window);
    set_window_cloak(&window, true);
    // gpu / text_system / last_rects / layout_cache are shared between the
    // event-loop closure and the modal-drag + snapshot hooks (which run while
    // the closure is blocked inside script), hence Rc<RefCell<...>>.
    let gpu = Rc::new(std::cell::RefCell::new(Gpu::new(window.clone(), gpu_prewarm)?));
    let text_system = Rc::new(std::cell::RefCell::new(TextSystem::new_with(
        os_system_fonts_ok(),
    )));
    {
        let e = engine.borrow();
        let mut ts = text_system.borrow_mut();
        for sheet in &e.sheets {
            for (family, data) in &sheet.font_faces {
                ts.register_font(family, data.clone());
            }
        }
    }

    let start = Instant::now();
    let mut cursor = (0.0f32, 0.0f32);
    let mut current_cursor = winit::window::CursorIcon::Default;
    let mut last_click: Option<(Instant, NodeKey)> = None;
    let mut last_caption_press: Option<Instant> = None;
    // Cursor position of a caption press whose OS move is deferred until the
    // pointer actually moves (see the Pressed handler). None = no press pending.
    let mut caption_pressed: Option<(f32, f32)> = None;
    // Raw-event (onMouse/onKey expando) delivery state: current modifier keys,
    // held mouse buttons (Sciter mask: 1 left, 2 right, 4 middle), the element
    // whose onMouse currently owns the cursor (for ENTER/LEAVE), and the last
    // onMouse DOWN instant (for DCLICK).
    let mut key_mods = (false, false, false, false);
    let mut buttons_down: i64 = 0;
    let mut onmouse_over: Option<NodeKey> = None;
    let mut last_onmouse_down: Option<Instant> = None;
    let mut text_drag: Option<NodeKey> = None;
    let mut left_press_hit: Option<NodeKey> = None;
    let mut last_edit_click: Option<(Instant, NodeKey, u8, (f32, f32))> = None;
    let mut last_window_state: i64 = 1;
    let mut pending_statechange: u8 = 0;
    let mut ime_on = false;
    let mut ime_area: Option<(f64, f64, f64, f64)> = None;
    let mut ime_pw = false;
    let mut main_caret_rect: Option<(f64, f64, f64, f64)> = None;
    #[cfg(not(windows))]
    let mut os_cursor_hidden = false;
    #[cfg(windows)]
    let mut native_cursor = NativeCursor::default();
    let mut pointer_inside = false;
    let mut cursor_epoch_seen = 0u64;
    let mut col_drag: Option<host::ColumnDrag> = None;
    let mut scrollbar_drag: Option<host::ScrollDrag> = None;
    let mut slider_drag: Option<NodeKey> = None;
    let mut os_drop_files: Vec<std::path::PathBuf> = Vec::new();
    let mut os_drop_hovering = false;
    let mut os_drop_ready = false;
    // The chat window, once view.window opens it (None until then).
    let mut child: Option<ChildWin> = None;
    let main_id = host::engine_id(&engine);
    // (title-bearing element under cursor, when the cursor entered it).
    let mut hover_title: Option<(NodeKey, Instant)> = None;
    let last_rects: Rc<
        std::cell::RefCell<std::collections::HashMap<NodeKey, (f32, f32, f32, f32)>>,
    > = Rc::new(std::cell::RefCell::new(std::collections::HashMap::new()));
    let mut scale = window.scale_factor() as f32;
    engine.borrow_mut().scale = scale;
    if super::paint_backend::backend_is_cpu() {
        host::set_scroll_ease_default(1.0);
    }
    {
        let pos = window.outer_position().unwrap_or_default();
        let sz = window.inner_size();
        host::set_window_rect((
            pos.x as f64 / scale as f64,
            pos.y as f64 / scale as f64,
            sz.width as f64 / scale as f64,
            sz.height as f64 / scale as f64,
        ));
    }

    host::pump_timers(&mut interp, &engine, 5.0).ok();
    host::focus_first_input(&engine);
    let cloak_hides_window = cfg!(target_os = "windows") && !os_needs_cpu_backend();
    for (target, cmd) in host::take_window_commands() {
        let is_state = matches!(cmd, host::WindowCommand::State(_));
        if target == host::engine_id(&engine) && (cloak_hides_window || !is_state) {
            apply_window_command(&window, cmd);
        } else {
            host::push_window_command(target, cmd);
        }
    }

    // Cached layout: taffy + parley shaping are the expensive stages, and hover /
    // scroll / selection / caret changes don't move geometry -- only DOM
    // mutations do (tracked by the layout epoch). Reusing the cached layout makes
    // hover highlights and wheel scrolling paint-only, which is what keeps big
    // pages (the home window) as responsive as the tiny remote-window menus.
    // Known limit: a :hover rule that changes geometry (padding/size) won't
    // reflow until the next real mutation -- this UI's hover rules are colors.
    let layout_cache: SharedLayoutCache =
        Rc::new(std::cell::RefCell::new(None));

    // view.doEvent(#untilMouseUp) -- the file-transfer drag's modal loop. Runs
    // while the winit closure is blocked inside script: poll the OS mouse,
    // deliver MOUSE_MOVE to raw subscriptions, re-record paintForeground
    // overlays, repaint. Returns when the left button releases; the queued OS
    // mouse-up then reaches winit normally after the stack unwinds.
    {
        let window = window.clone();
        let main_engine = engine.clone();
        let gpu = gpu.clone();
        let ts = text_system.clone();
        let lr = last_rects.clone();
        let lc = layout_cache.clone();
        host::set_modal_hook(Box::new(move |interp, target| {
            if !Rc::ptr_eq(target, &main_engine) {
                return;
            }
            let mut last_pos: Option<(f32, f32)> = None;
            let started = Instant::now();
            loop {
                let Some((x, y, down)) = os_mouse_state(&window) else { break };
                // Safety valve: a modal drag can never wedge the window for
                // good, whatever the OS reports about the button.
                if !down || started.elapsed().as_secs() > 60 {
                    break;
                }
                if last_pos != Some((x, y)) {
                    last_pos = Some((x, y));
                    main_engine.borrow_mut().pointer = (x, y);
                    let hit = {
                        let e = main_engine.borrow();
                        hit_test_ordered(&e.doc, &e.screen_rects, &e.screen_order, x, y)
                    };
                    host::dispatch_mouse_subscriptions(interp, &main_engine, 0x1003, hit, x, y);
                    host::drain_events(interp, &main_engine);
                    host::record_fg_overlays(interp, &main_engine);
                    let sc = main_engine.borrow().scale;
                    repaint_main(
                        &mut gpu.borrow_mut(),
                        &main_engine,
                        &mut ts.borrow_mut(),
                        &mut lr.borrow_mut(),
                        &mut lc.borrow_mut(),
                        sc,
                    );
                }
                keep_queue_alive();
                std::thread::sleep(std::time::Duration::from_millis(8));
            }
        }));
    }
    // Layout-only refresh for script geometry reads (element.box) that land
    // between a DOM rebuild and the next paint.
    {
        let main_engine = engine.clone();
        let gpu = gpu.clone();
        let ts = text_system.clone();
        let lc = layout_cache.clone();
        host::set_layout_hook(Box::new(move |target| {
            if !Rc::ptr_eq(target, &main_engine) {
                let viewport = {
                    let e = target.borrow();
                    e.last_rects.get(&e.doc.root).map(|r| (r.2, r.3, e.scale))
                };
                let Some((vw, vh, sc)) = viewport else { return };
                let styles = host::cached_computed_styles(target);
                let layout = {
                    let e = target.borrow();
                    super::layout::layout_document(&e.doc, &styles, &mut ts.borrow_mut(), (vw, vh), sc)
                };
                host::update_layout_rects(target, &layout.rects);
                return;
            }
            let (dev_w, dev_h) = {
                let g = gpu.borrow();
                (g.width(), g.height())
            };
            let epoch = super::dom::layout_epoch();
            let mut cache = lc.borrow_mut();
            let stale = match &*cache {
                Some((ep, w, h, _)) => *ep != epoch || *w != dev_w || *h != dev_h,
                None => true,
            };
            let styles = host::cached_computed_styles(&main_engine);
            let styles = styles.as_ref();
            if stale {
                let e = main_engine.borrow();
                let sc = e.scale;
                let layout = super::layout::layout_document(
                    &e.doc,
                    &styles,
                    &mut ts.borrow_mut(),
                    (dev_w as f32 / sc, dev_h as f32 / sc),
                    sc,
                );
                drop(e);
                *cache = Some((epoch, dev_w, dev_h, layout));
            }
            let rects = match &*cache {
                Some((_, _, _, l)) => l.rects.clone(),
                None => return,
            };
            if let Some((_, _, _, l)) = &*cache {
                host::sync_textarea_scroll(&main_engine, l);
            }
            drop(cache);
            let (screen, order) = host::compute_screen_geometry(&main_engine, &rects, &styles);
            host::update_layout_rects(&main_engine, &rects);
            let mut em = main_engine.borrow_mut();
            em.screen_rects = screen;
            em.screen_order = order;
        }));
    }
    // new Image(w, h, element): render the current page offscreen, crop the
    // element's rect, and resize -- the drag ghost row snapshot.
    {
        let main_engine = engine.clone();
        let gpu = gpu.clone();
        let ts = text_system.clone();
        let lc = layout_cache.clone();
        host::set_snapshot_hook(Box::new(move |target, key, w, h| {
            if !Rc::ptr_eq(target, &main_engine) {
                return None;
            }
            let rect = target.borrow().screen_rects.get(&key).copied()?;
            let sc = main_engine.borrow().scale;
            let gpu = gpu.borrow();
            let (dev_w, dev_h) = (gpu.width(), gpu.height());
            let scene = build_main_scene(
                dev_w,
                dev_h,
                &main_engine,
                &mut ts.borrow_mut(),
                &mut lc.borrow_mut(),
                sc,
            );
            let full = gpu.render_offscreen(&scene, dev_w, dev_h)?;
            let page: image::RgbaImage =
                image::ImageBuffer::from_raw(dev_w, dev_h, full)?;
            let cx = ((rect.0 * sc).max(0.0) as u32).min(dev_w.saturating_sub(1));
            let cy = ((rect.1 * sc).max(0.0) as u32).min(dev_h.saturating_sub(1));
            let cw = ((rect.2 * sc) as u32).clamp(1, dev_w - cx);
            let ch = ((rect.3 * sc) as u32).clamp(1, dev_h - cy);
            let crop = image::imageops::crop_imm(&page, cx, cy, cw, ch).to_image();
            let out = image::imageops::resize(
                &crop,
                w.max(1),
                h.max(1),
                image::imageops::FilterType::Lanczos3,
            );
            Some((w.max(1), h.max(1), std::sync::Arc::new(out.into_raw())))
        }));
    }

    // Paint the initial frame BEFORE showing the window, so the user never sees
    // a blank/unpainted flash (Sciter shows only after its first render). The
    // window was created with visible(false); after making it visible we must
    // re-focus it, otherwise a window shown after creation is not made key on
    // macOS and never receives mouse/keyboard events (all clicks would be
    // dropped).
    // The real client size can differ from the one the surface was built with:
    // the page's self.ready() restores a saved rect (and possibly a maximized
    // state) while loading, and the DPI settles as the window lands on its
    // monitor. Painting at the stale size leaves that first frame stretched
    // across the real window until something triggers another redraw, which is
    // the distortion visible while the app starts.
    let sync_surface = |gpu: &Rc<std::cell::RefCell<Gpu>>| -> bool {
        let size = window.inner_size();
        let mut g = gpu.borrow_mut();
        let stale = g.width() != size.width || g.height() != size.height;
        if stale {
            g.resize(size.width, size.height);
        }
        stale
    };
    sync_surface(&gpu);
    repaint_main(
        &mut gpu.borrow_mut(),
        &engine,
        &mut text_system.borrow_mut(),
        &mut last_rects.borrow_mut(),
        &mut layout_cache.borrow_mut(),
        scale,
    );
    // present() returns once the frame is SUBMITTED, not finished: on a cold
    // first launch the driver is still compiling vello's pipelines and running
    // that first render for a few hundred ms, so showing the window now would
    // composite the swapchain before the frame lands -- the ~0.5s "distorted"
    // flash seen only on the first (cold) run, gone once the driver's pipeline
    // cache is warm. Block until the GPU is done so the window appears already
    // painted (no-op on the CPU backend, which fills its buffer synchronously).
    gpu.borrow().flush();
    if gpu_first_frame_drops_content(
        &gpu.borrow(),
        &engine,
        &mut text_system.borrow_mut(),
        &mut layout_cache.borrow_mut(),
        scale,
    ) && relaunch_with_cpu_backend()
    {
        std::process::exit(0);
    }
    if gpu.borrow().core().is_some() {
        if std::env::var("WIREUI_FAKE_GPU_FAULT").map_or(false, |v| v == "1") {
            GPU_FAULT.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        exit_to_cpu_if_gpu_faulted();
        gpu_mark_success();
    }
    #[cfg(target_os = "windows")]
    {
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};
        if let Ok(h) = window.window_handle() {
            if let RawWindowHandle::Win32(w) = h.as_raw() {
                let redraw = window.clone();
                super::a11y::install(
                    w.hwnd.get(),
                    std::sync::Arc::new(move || redraw.request_redraw()),
                );
            }
        }
    }
    #[cfg(target_os = "macos")]
    {
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};
        if let Ok(h) = window.window_handle() {
            if let RawWindowHandle::AppKit(v) = h.as_raw() {
                let redraw = window.clone();
                super::a11y::install(
                    v.ns_view.as_ptr(),
                    std::sync::Arc::new(move || redraw.request_redraw()),
                );
            }
        }
    }
    let start_hidden = host::current_window_state() == 4
        || host::pending_window_state(host::engine_id(&engine)) == Some(4);
    if start_hidden {
        host::set_current_window_state(4);
    } else {
        window.set_visible(true);
        window.focus_window();
    }
    // Showing the window is itself a chance for the client size to settle.
    if sync_surface(&gpu) {
        repaint_main(
            &mut gpu.borrow_mut(),
            &engine,
            &mut text_system.borrow_mut(),
            &mut last_rects.borrow_mut(),
            &mut layout_cache.borrow_mut(),
            scale,
        );
    }
    window.request_redraw();
    let mut uncloak_pending = true;

    event_loop
        .run(move |event, elwt| {
            elwt.set_control_flow(ControlFlow::Wait);
            host::set_current_interp(&mut interp as *mut _);
            host::set_current_engine(&engine);
            // view.close() from any script that ran (event handler, timer)
            // requests window teardown; honour it before doing anything else.
            {
                let redraw = {
                    let mut e = engine.borrow_mut();
                    std::mem::take(&mut e.repaint_requested)
                };
                if redraw {
                    window.request_redraw();
                }
            }
            if host::take_view_close() {
                if host::run_window_closing(&mut interp, &engine) {
                    window.set_visible(false);
                    host::clear_window_hooks();
                    host::detach_behaviors(&engine);
                    elwt.exit();
                    return;
                }
            }
            // A view.window() call (the chat window) asked for a native window.
            for req in host::take_child_windows() {
                #[allow(unused_mut)]
                let mut child_builder = with_desktop_app_id(
                    WindowBuilder::new()
                        .with_title(&req.title)
                        .with_inner_size(winit::dpi::LogicalSize::new(req.width, req.height))
                        .with_window_icon(load_app_icon())
                        .with_visible(false),
                );
                #[cfg(not(target_os = "macos"))]
                {
                    child_builder =
                        child_builder.with_theme(Some(winit::window::Theme::Light));
                }
                #[cfg(windows)]
                {
                    use winit::platform::windows::WindowBuilderExtWindows;
                    child_builder = child_builder.with_drag_and_drop(false);
                }
                if let Ok(w) = child_builder.build(elwt)
                {
                    let w = Arc::new(w);
                    #[cfg(windows)]
                    subclass_window(&w);
                    apply_rounded_corners(&w);
                    apply_window_icon(&w);
                    if let Ok(mut cgpu) = Gpu::for_window(gpu.borrow().core(), w.clone()) {
                        // core() is None on the CPU backend; the child gets its
                        // own softbuffer surface and follows the process mode.
                        let mut cts = TextSystem::new_with(os_system_fonts_ok());
                        {
                            let e = req.engine.borrow();
                            for sheet in &e.sheets {
                                for (f, d) in &sheet.font_faces {
                                    cts.register_font(f, d.clone());
                                }
                            }
                        }
                        let cscale = w.scale_factor() as f32;
                        req.engine.borrow_mut().scale = cscale;
                        host::set_current_engine(&req.engine);
                        host::pump_timers(&mut interp, &req.engine, 5.0).ok();
                        repaint_win(&mut cgpu, &req.engine, &mut cts, cscale);
                        w.set_visible(true);
                        w.set_ime_allowed(true);
                        w.focus_window();
                        w.request_redraw();
                        host::set_current_engine(&engine);
                        child = Some(ChildWin {
                            window: w,
                            gpu: cgpu,
                            text_system: cts,
                            engine: req.engine,
                            scale: cscale,
                            cursor: (0.0, 0.0),
                            press_hit: None,
                            scrollbar_drag: None,
                        });
                    }
                }
            }
            if let Some(dark) = host::take_native_theme() {
                #[cfg(not(target_os = "macos"))]
                window.set_theme(Some(if dark {
                    winit::window::Theme::Dark
                } else {
                    winit::window::Theme::Light
                }));
                #[cfg(target_os = "macos")]
                let _ = dark;
            }
            // view.move(x,y,w,h) resizes/positions the window (the CM window sizes
            // itself to its content this way).
            if let Some((x, y, w, h)) = host::take_view_move() {
                let (w, h) = clamp_view_size(
                    w,
                    h,
                    window.current_monitor().or_else(|| window.primary_monitor()),
                );
                let monitors: Vec<_> = window.available_monitors().collect();
                let (x, y) = visible_logical_position(
                    &monitors,
                    window.primary_monitor(),
                    (x, y, w, h),
                    window.scale_factor(),
                );
                window.set_outer_position(winit::dpi::LogicalPosition::new(x, y));
                match window.request_inner_size(winit::dpi::LogicalSize::new(w, h)) {
                    Some(sz) => {
                        gpu.borrow_mut().resize(sz.width, sz.height);
                        host::fire_view_event(&mut interp, &engine, "size");
                        repaint_main(
                            &mut gpu.borrow_mut(),
                            &engine,
                            &mut text_system.borrow_mut(),
                            &mut last_rects.borrow_mut(),
                            &mut layout_cache.borrow_mut(),
                            scale,
                        );
                    }
                    None => {
                        gpu.borrow_mut().resize(
                            (w as f32 * scale) as u32,
                            (h as f32 * scale) as u32,
                        );
                    }
                }
                window.request_redraw();
            }
            // view.windowState / view.windowTopmost assignments drive the OS window
            // they were issued on (main or the chat child).
            for (target, cmd) in host::take_window_commands() {
                if target == main_id {
                    apply_window_command(&window, cmd);
                    window.request_redraw();
                } else if let Some(cw) = child.as_ref() {
                    if target == host::engine_id(&cw.engine) {
                        apply_window_command(&cw.window, cmd);
                        cw.window.request_redraw();
                    }
                }
            }
            match event {
                Event::UserEvent(()) => {
                    let had_frame =
                        frame_pending.swap(false, std::sync::atomic::Ordering::SeqCst);
                    let epoch_before = super::dom::layout_epoch();
                    host::drain_offthread_calls(&mut interp, &engine);
                    host::drain_events(&mut interp, &engine);
                    host::pump_video_events(&engine);
                    // Video frames always paint; io-thread wakeups only when the
                    // drained script actually changed the document. Idle signal
                    // traffic otherwise repaints the full window per wakeup, which
                    // pins a core on the CPU raster backend.
                    if had_frame
                        || super::dom::layout_epoch() != epoch_before
                        || engine.borrow().repaint_requested
                    {
                        window.request_redraw();
                    }
                }
                Event::WindowEvent { event, window_id } => {
                    if matches!(
                        event,
                        WindowEvent::KeyboardInput { .. } | WindowEvent::MouseInput { .. }
                    ) {
                        last_input = Instant::now();
                    }
                    // Route events for the chat child window to its own engine.
                    if let Some(cw) = child.as_mut() {
                        if window_id == cw.window.id() {
                            host::set_current_engine(&cw.engine);
                            match event {
                                WindowEvent::CloseRequested => {
                                    if host::run_window_closing(&mut interp, &cw.engine) {
                                        child = None;
                                    }
                                }
                                WindowEvent::Resized(sz) => {
                                    cw.gpu.resize(sz.width, sz.height);
                                    cw.window.request_redraw();
                                }
                                WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                                    cw.scale = scale_factor as f32;
                                    cw.engine.borrow_mut().scale = cw.scale;
                                    cw.window.request_redraw();
                                }
                                WindowEvent::RedrawRequested => {
                                    repaint_win(&mut cw.gpu, &cw.engine, &mut cw.text_system, cw.scale);
                                    if cw.window.has_focus() {
                                        if let Some(r) = host::caret_device_rect() {
                                            cw.window.set_ime_cursor_area(
                                                winit::dpi::PhysicalPosition::new(r.0, r.1),
                                                winit::dpi::PhysicalSize::new(
                                                    (r.2 - r.0).max(1.0),
                                                    (r.3 - r.1).max(1.0),
                                                ),
                                            );
                                        }
                                    }
                                }
                                WindowEvent::CursorMoved { position, .. } => {
                                    cw.cursor = (
                                        position.x as f32 / cw.scale,
                                        position.y as f32 / cw.scale,
                                    );
                                    if let Some(d) = &cw.scrollbar_drag {
                                        if host::scrollbar_drag_to(&cw.engine, d, cw.cursor.1) {
                                            cw.window.request_redraw();
                                        }
                                    }
                                }
                                WindowEvent::MouseWheel { delta, .. } => {
                                    let dy = match delta {
                                        winit::event::MouseScrollDelta::LineDelta(_, y) => -y * host::scroll_step(),
                                        winit::event::MouseScrollDelta::PixelDelta(p) => -(p.y as f32 / cw.scale),
                                    };
                                    let styles = host::cached_computed_styles(&cw.engine);
                                    let screen = cw.engine.borrow().screen_rects.clone();
                                    if host::scroll_at(&cw.engine, &screen, cw.cursor.0, cw.cursor.1, dy, &styles) {
                                        cw.window.request_redraw();
                                    }
                                }
                                WindowEvent::MouseInput {
                                    state: ElementState::Pressed,
                                    button: MouseButton::Left,
                                    ..
                                } => {
                                    if let Some(d) = host::scrollbar_hit(&cw.engine, cw.cursor.0, cw.cursor.1) {
                                        host::scrollbar_drag_to(&cw.engine, &d, cw.cursor.1);
                                        cw.scrollbar_drag = Some(d);
                                    } else {
                                        let hit = {
                                            let e = cw.engine.borrow();
                                            hit_test_ordered(&e.doc, &e.screen_rects, &e.screen_order, cw.cursor.0, cw.cursor.1)
                                        };
                                        host::set_active(&cw.engine, hit);
                                        host::set_focus(&cw.engine, hit);
                                        cw.press_hit = hit;
                                        if let Some(t) = hit {
                                            host::dispatch_dom_event(&mut interp, &cw.engine, "mousedown", t).ok();
                                        }
                                    }
                                    cw.window.request_redraw();
                                }
                                WindowEvent::MouseInput {
                                    state: ElementState::Released,
                                    button: MouseButton::Left,
                                    ..
                                } => {
                                    if cw.scrollbar_drag.take().is_none() {
                                        host::set_active(&cw.engine, None);
                                        let press = cw.press_hit.take();
                                        let hit = {
                                            let e = cw.engine.borrow();
                                            hit_test_ordered(&e.doc, &e.screen_rects, &e.screen_order, cw.cursor.0, cw.cursor.1)
                                        };
                                        let target = match (press, hit) {
                                            (Some(p), Some(r)) => {
                                                let e = cw.engine.borrow();
                                                click_target_for(&e.doc, p, r)
                                            }
                                            _ => None,
                                        };
                                        if let Some(t) = target {
                                            host::dispatch_dom_event(&mut interp, &cw.engine, "click", t).ok();
                                        }
                                    }
                                    cw.window.request_redraw();
                                }
                                WindowEvent::ModifiersChanged(m) => {
                                    let s = m.state();
                                    key_mods = (s.alt_key(), s.control_key(), s.shift_key(), s.super_key());
                                    host::set_key_mods(key_mods);
                                }
                                WindowEvent::KeyboardInput { event: kev, is_synthetic: true, .. }
                                    if kev.state == ElementState::Pressed => {}
                                WindowEvent::KeyboardInput { event: kev, .. } => {
                                    if kev.state == ElementState::Pressed {
                                        use crate::script::interp::SV;
                                        let focus_target = {
                                            let e = cw.engine.borrow();
                                            host::key_event_target(&e)
                                        };
                                        let key_extra = |kc: i64| -> Vec<(String, SV)> {
                                            vec![
                                                ("keyCode".into(), SV::Int(kc)),
                                                ("altKey".into(), SV::Bool(key_mods.0)),
                                                ("ctrlKey".into(), SV::Bool(key_mods.1)),
                                                ("shiftKey".into(), SV::Bool(key_mods.2)),
                                                ("commandKey".into(), SV::Bool(key_mods.3)),
                                                ("metaKey".into(), SV::Bool(key_mods.3)),
                                            ]
                                        };
                                        let code = vk_keycode(&kev);
                                        let mut consumed = false;
                                        if code != 0 {
                                            consumed = host::dispatch_dom_event_with(
                                                &mut interp, &cw.engine, "keydown", focus_target, &key_extra(code),
                                            )
                                            .unwrap_or(false);
                                        }
                                        if consumed {
                                            host::drain_events(&mut interp, &cw.engine);
                                            cw.window.request_redraw();
                                        } else {
                                            let mut dirty = false;
                                            let shift = key_mods.2;
                                            let shortcut = is_shortcut_chord(
                                                key_mods.0,
                                                key_mods.1,
                                                key_mods.3,
                                                cfg!(target_os = "macos"),
                                            );
                                            match &kev.logical_key {
                                                Key::Named(NamedKey::Backspace) => {
                                                    dirty = host::backspace(&cw.engine);
                                                }
                                                Key::Named(NamedKey::Delete) => {
                                                    dirty = host::delete_forward(&cw.engine);
                                                }
                                                Key::Named(NamedKey::ArrowLeft) => {
                                                    if host::move_caret(&cw.engine, -1, false, false, shift) {
                                                        cw.window.request_redraw();
                                                    }
                                                }
                                                Key::Named(NamedKey::ArrowRight) => {
                                                    if host::move_caret(&cw.engine, 1, false, false, shift) {
                                                        cw.window.request_redraw();
                                                    }
                                                }
                                                Key::Named(NamedKey::Home) => {
                                                    if host::move_caret(&cw.engine, 0, true, false, shift) {
                                                        cw.window.request_redraw();
                                                    }
                                                }
                                                Key::Named(NamedKey::End) => {
                                                    if host::move_caret(&cw.engine, 0, false, true, shift) {
                                                        cw.window.request_redraw();
                                                    }
                                                }
                                                Key::Named(NamedKey::Space) => {
                                                    dirty = host::type_char(&cw.engine, ' ');
                                                }
                                                Key::Named(NamedKey::Enter) => {
                                                    dirty = host::insert_newline(&cw.engine);
                                                }
                                                Key::Character(s) if shortcut && s.eq_ignore_ascii_case("a") => {
                                                    if host::select_all(&cw.engine) {
                                                        cw.window.request_redraw();
                                                    }
                                                }
                                                Key::Character(s) if shortcut && s.eq_ignore_ascii_case("c") => {
                                                    let sel = { let e = cw.engine.borrow(); host::selected_text(&e) };
                                                    if let Some(text) = sel {
                                                        let _ = arboard::Clipboard::new()
                                                            .and_then(|mut c| c.set_text(text));
                                                    }
                                                }
                                                Key::Character(s) if shortcut && s.eq_ignore_ascii_case("x") => {
                                                    let sel = { let e = cw.engine.borrow(); host::selected_text(&e) };
                                                    if let Some(text) = sel {
                                                        let _ = arboard::Clipboard::new()
                                                            .and_then(|mut c| c.set_text(text));
                                                        dirty = host::delete_forward(&cw.engine);
                                                    }
                                                }
                                                Key::Character(s) if shortcut && s.eq_ignore_ascii_case("v") => {
                                                    if let Ok(text) =
                                                        arboard::Clipboard::new().and_then(|mut c| c.get_text())
                                                    {
                                                        let clean = host::clean_pasted_text(
                                                            &text,
                                                            host::focused_input_is_multiline(&cw.engine),
                                                        );
                                                        if !clean.is_empty() {
                                                            dirty = host::insert_text(&cw.engine, &clean);
                                                        }
                                                    }
                                                }
                                                _ if shortcut => {}
                                                _ => {
                                                    if let Some(txt) = &kev.text {
                                                        for ch in txt.chars() {
                                                            if host::type_char(&cw.engine, ch) {
                                                                dirty = true;
                                                            }
                                                        }
                                                    }
                                                }
                                            }
                                            if dirty {
                                                let focused = { let e = cw.engine.borrow(); host::focused_input(&e) };
                                                if let Some(fk) = focused {
                                                    host::dispatch_dom_event(&mut interp, &cw.engine, "change", fk).ok();
                                                    host::drain_events(&mut interp, &cw.engine);
                                                }
                                                cw.window.request_redraw();
                                            }
                                        }
                                    }
                                }
                                WindowEvent::Ime(ime) => {
                                    use winit::event::Ime;
                                    let dirty = match ime {
                                        Ime::Enabled | Ime::Disabled => {
                                            host::clear_preedit(&cw.engine)
                                        }
                                        Ime::Preedit(text, cursor) => host::set_preedit(
                                            &cw.engine,
                                            &text,
                                            cursor.map(|(s, _)| s),
                                        ),
                                        Ime::Commit(text) => {
                                            host::clear_preedit(&cw.engine);
                                            let clean: String =
                                                text.chars().filter(|c| !c.is_control()).collect();
                                            host::insert_text(&cw.engine, &clean)
                                        }
                                    };
                                    if dirty {
                                        cw.window.request_redraw();
                                    }
                                }
                                _ => {}
                            }
                            host::set_current_engine(&engine);
                            return;
                        }
                    }
                    match event {
                    WindowEvent::CloseRequested => {
                        if host::run_window_closing(&mut interp, &engine) {
                            window.set_visible(false);
                            host::clear_window_hooks();
                            host::detach_behaviors(&engine);
                            elwt.exit();
                        }
                    }
                    WindowEvent::Resized(sz) => {
                        gpu.borrow_mut().resize(sz.width, sz.height);
                        host::fire_view_event(&mut interp, &engine, "size");
                        // Repaint NOW: waiting for the scheduled redraw leaves
                        // the freshly exposed right/bottom strips black for a
                        // frame or two on every resize and at first show.
                        repaint_main(
                            &mut gpu.borrow_mut(),
                            &engine,
                            &mut text_system.borrow_mut(),
                            &mut last_rects.borrow_mut(),
                            &mut layout_cache.borrow_mut(),
                            scale,
                        );
                    }
                    WindowEvent::Focused(focused) => {
                        host::fire_view_event(
                            &mut interp,
                            &engine,
                            if focused { "windowfocus" } else { "windowblur" },
                        );
                    }
                    WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                        scale = scale_factor as f32;
                        engine.borrow_mut().scale = scale;
                        host::fire_view_event(&mut interp, &engine, "resolutionchange");
                        host::fire_view_event(&mut interp, &engine, "size");
                        window.request_redraw();
                    }
                    WindowEvent::CursorMoved { position, .. } => {
                        #[cfg(target_os = "windows")]
                        if window.fullscreen().is_some() {
                            heal_fullscreen_taskbar(&window);
                        }
                        cursor = (position.x as f32 / scale as f64 as f32, position.y as f32 / scale);
                        if let Some(d) = &col_drag {
                            host::column_resize_apply(&engine, d, cursor.0);
                            window.request_redraw();
                            return;
                        }
                        if let Some(d) = &scrollbar_drag {
                            if host::scrollbar_drag_to(&engine, d, cursor.1) {
                                window.request_redraw();
                            }
                            return;
                        }
                        if let Some(k) = slider_drag {
                            host::slider_drag_to(&engine, k, cursor.0);
                            window.request_redraw();
                            return;
                        }
                        if engine.borrow().edit_menu.is_some() {
                            let hit = { let e = engine.borrow(); hit_test_ordered(&e.doc, &e.screen_rects, &e.screen_order, cursor.0, cursor.1) };
                            if host::edit_menu_hover(&engine, hit) {
                                window.request_redraw();
                            }
                        }
                        if buttons_down & 1 != 0 {
                            if let Some(t) = text_drag {
                                let styles = host::cached_computed_styles(&engine);
                                let rect = engine.borrow().screen_rects.get(&t).copied();
                                if let Some(r) = rect {
                                    let is_ta = {
                                        let e = engine.borrow();
                                        e.doc.arena.get(t).map_or(false, |n| n.tag == "textarea")
                                    };
                                    let pad = if is_ta { 5.0 } else { host::input_pad_left(styles.get(&t)) };
                                    let (y, wrap, avail_h) = if is_ta {
                                        (
                                            cursor.1 - r.1 - pad,
                                            Some((r.2 - 2.0 * pad).max(8.0)),
                                            Some(((r.3 - 2.0 * pad).max(1.0)) as f64),
                                        )
                                    } else {
                                        (2.0, None, None)
                                    };
                                    let scroll = if is_ta {
                                        0.0
                                    } else {
                                        host::input_scroll_x(
                                            &engine,
                                            &mut text_system.borrow_mut(),
                                            t,
                                            r.2,
                                            &styles,
                                        )
                                    };
                                    host::caret_click(
                                        &engine,
                                        &mut text_system.borrow_mut(),
                                        t,
                                        cursor.0 - r.0 - pad + scroll,
                                        y,
                                        wrap,
                                        avail_h,
                                        host::CaretClick::Drag,
                                        &styles,
                                    );
                                    window.request_redraw();
                                }
                                return;
                            }
                        }
                        // A caption press becomes an OS window-move only once the
                        // pointer actually moves past a small threshold. drag_window()
                        // is deferred to here (not fired on press) because winit's
                        // internal `dragging` flag only clears on WM_EXITSIZEMOVE; a
                        // press that never enters a move loop (a plain click or the
                        // clicks of a double-click-to-maximize) would leave it stuck
                        // true and permanently disable dragging.
                        if let Some((px, py)) = caption_pressed {
                            let (dx, dy) = (cursor.0 - px, cursor.1 - py);
                            if dx * dx + dy * dy > 9.0 {
                                caption_pressed = None;
                                // Dragging a maximized window restores it first
                                // (native titlebar behaviour), keeping the cursor
                                // proportionally over the restored caption.
                                if window.is_maximized() {
                                    let sf = window.scale_factor();
                                    let max_w = window.inner_size().width.max(1) as f64;
                                    let origin = window
                                        .outer_position()
                                        .unwrap_or(winit::dpi::PhysicalPosition::new(0, 0));
                                    let cursor_screen_x = origin.x as f64 + cursor.0 as f64 * sf;
                                    let cursor_screen_y = origin.y as f64 + cursor.1 as f64 * sf;
                                    let frac = (cursor.0 as f64 * sf / max_w).clamp(0.0, 1.0);
                                    window.set_maximized(false);
                                    host::set_current_window_state(1);
                                    let rest_w = window.outer_size().width as f64;
                                    window.set_outer_position(winit::dpi::PhysicalPosition::new(
                                        cursor_screen_x - rest_w * frac,
                                        (cursor_screen_y - cursor.1 as f64 * sf).max(0.0),
                                    ));
                                }
                                window.drag_window().ok();
                                return;
                            }
                        }
                        let hit = { let e = engine.borrow(); hit_test_ordered(&e.doc, &e.screen_rects, &e.screen_order, cursor.0, cursor.1) };
                        host::dispatch_mouse_subscriptions(
                            &mut interp, &engine, 0x1003, hit, cursor.0, cursor.1,
                        );
                        // Custom pointer image: hide the OS cursor while over an
                        // element with style.cursor(img) and repaint the overlay
                        // at every move. The software renderer can't afford a
                        // full-scene repaint per mouse move (a remote video pegs a
                        // slow CPU and the pointer stutters), so there we leave the
                        // OS cursor drawn natively and skip the sprite entirely.
                        {
                            pointer_inside = true;
                            let mut e = engine.borrow_mut();
                            e.pointer = cursor;
                            #[cfg(not(windows))]
                            {
                                let custom = !super::paint_backend::backend_is_cpu()
                                    && !e.cursor_images.is_empty()
                                    && cursor_image_at(&e, hit).is_some();
                                drop(e);
                                if custom != os_cursor_hidden {
                                    os_cursor_hidden = custom;
                                    window.set_cursor_visible(!custom);
                                }
                                if custom {
                                    window.request_redraw();
                                }
                            }
                        }
                        let om_target = host::on_mouse_target(&engine, hit);
                        if om_target != onmouse_over {
                            if let Some(old) = onmouse_over {
                                host::dispatch_on_mouse(&mut interp, &engine, old,
                                    &raw_mouse(0x1005, cursor, buttons_down, (0.0, 0.0), key_mods));
                            }
                            if let Some(new) = om_target {
                                host::dispatch_on_mouse(&mut interp, &engine, new,
                                    &raw_mouse(0x1004, cursor, buttons_down, (0.0, 0.0), key_mods));
                            }
                            onmouse_over = om_target;
                        }
                        if let Some(t) = om_target {
                            host::dispatch_on_mouse(&mut interp, &engine, t,
                                &raw_mouse(0x1003, cursor, buttons_down, (0.0, 0.0), key_mods));
                            host::drain_events(&mut interp, &engine);
                        }
                        let over_control = hit.map_or(false, |t| {
                            let e = engine.borrow();
                            hit_is_interactive(&e.doc, t)
                        });
                        let edge = if extended_frame && !over_control && has_resize_edges(&window) {
                            let ls = window.inner_size().to_logical::<f32>(window.scale_factor());
                            resize_direction(cursor.0, cursor.1, ls.width, ls.height)
                        } else {
                            None
                        };
                        let over_divider = hit.map_or(false, |t| {
                            let e = engine.borrow();
                            e.doc.closest("thead", t).is_some()
                        }) && host::column_resize_hit(&engine, cursor.0, cursor.1).is_some();
                        let icon = if over_divider {
                            winit::window::CursorIcon::ColResize
                        } else {
                            match edge {
                                Some(d) => resize_cursor_icon(d),
                                None => host::cursor_at(&engine, hit, cursor.0),
                            }
                        };
                        if icon != current_cursor {
                            current_cursor = icon;
                            window.set_cursor_icon(icon);
                        }
                        #[cfg(windows)]
                        {
                            let img = cursor_image_at(&engine.borrow(), hit);
                            apply_native_cursor(&mut native_cursor, img, scale);
                        }
                        if set_hover_chain(&engine, hit) {
                            window.request_redraw();
                        }
                        // Tooltip: (re)start the hover timer when the title element
                        // under the cursor changes; hide any shown tooltip on move.
                        let title_el = { let e = engine.borrow(); host::element_title(&e, hit).map(|(k, _)| k) };
                        let same = matches!((&hover_title, title_el), (Some((k, _)), Some(nk)) if *k == nk);
                        if !same {
                            hover_title = title_el.map(|k| (k, Instant::now()));
                            let mut e = engine.borrow_mut();
                            if e.tooltip.take().is_some() {
                                drop(e);
                                window.request_redraw();
                            }
                        }
                    }
                    WindowEvent::MouseWheel { delta, .. } => {
                        // wheelDeltas are in wheel "lines" (positive = up); a pixel
                        // delta (precision touchpad) maps to fractional lines, which
                        // is exactly what remote.tis's touchpad detection keys on.
                        let (wx, wy) = match delta {
                            winit::event::MouseScrollDelta::LineDelta(x, y) => (x as f64, y as f64),
                            winit::event::MouseScrollDelta::PixelDelta(p) => {
                                (p.x / (40.0 * scale as f64), p.y / (40.0 * scale as f64))
                            }
                        };
                        let hit = { let e = engine.borrow(); hit_test_ordered(&e.doc, &e.screen_rects, &e.screen_order, cursor.0, cursor.1) };
                        if let Some(t) = host::on_mouse_target(&engine, hit) {
                            let consumed = host::dispatch_on_mouse(&mut interp, &engine, t,
                                &raw_mouse(0x1007, cursor, buttons_down, (wx, wy), key_mods));
                            host::drain_events(&mut interp, &engine);
                            if consumed {
                                window.request_redraw();
                                return;
                            }
                        }
                        // DOM mousewheel (the terminal's scrollback); consumed
                        // suppresses the engine's own scroll.
                        if let Some(t) = hit {
                            use crate::script::interp::{sv_array, SV};
                            let extra: Vec<(String, SV)> = vec![(
                                "wheelDeltas".into(),
                                sv_array(vec![SV::Float(wx), SV::Float(wy)]),
                            )];
                            if host::dispatch_dom_event_with(&mut interp, &engine, "mousewheel", t, &extra)
                                .unwrap_or(false)
                            {
                                host::drain_events(&mut interp, &engine);
                                window.request_redraw();
                                return;
                            }
                        }
                        let dy = match delta {
                            winit::event::MouseScrollDelta::LineDelta(_, y) => -y * host::scroll_step(),
                            winit::event::MouseScrollDelta::PixelDelta(p) => -(p.y as f32 / scale),
                        };
                        let styles = host::cached_computed_styles(&engine);
                        let screen = engine.borrow().screen_rects.clone();
                        if host::scroll_at(&engine, &screen, cursor.0, cursor.1, dy, &styles) {
                            window.request_redraw();
                        }
                    }
                    WindowEvent::MouseInput {
                        state: ElementState::Pressed,
                        button: MouseButton::Left,
                        ..
                    } => {
                        left_press_hit = None;
                        // While a context menu is open, a press is consumed here; the
                        // menu acts (item click) or dismisses on the matching release.
                        if engine.borrow().active_context_menu.is_some() {
                            return;
                        }
                        // The right-click edit menu (Cut/Copy/Paste/Select All):
                        // a press runs the clicked item and dismisses the menu.
                        if engine.borrow().edit_menu.is_some() {
                            let hit = { let e = engine.borrow(); hit_test_ordered(&e.doc, &e.screen_rects, &e.screen_order, cursor.0, cursor.1) };
                            host::edit_menu_click(&engine, hit);
                            window.request_redraw();
                            return;
                        }
                        // A click dismisses any tooltip (re-arms on the next hover).
                        hover_title = None;
                        if engine.borrow_mut().tooltip.take().is_some() {
                            window.request_redraw();
                        }
                        let hit = { let e = engine.borrow(); hit_test_ordered(&e.doc, &e.screen_rects, &e.screen_order, cursor.0, cursor.1) };
                        if host::dispatch_mouse_subscriptions(
                            &mut interp, &engine, 0x1001, hit, cursor.0, cursor.1,
                        ) {
                            window.request_redraw();
                            return;
                        }
                        // Native <select> dropdown: an open list consumes the press
                        // (pick an option or dismiss); otherwise clicking a select
                        // opens its list.
                        if engine.borrow().open_select.is_some() {
                            host::select_dropdown_click(&mut interp, &engine, hit);
                            window.request_redraw();
                            return;
                        }
                        if let Some(t) = hit {
                            let sel = { let e = engine.borrow(); e.doc.closest("select", t) };
                            if let Some(sel) = sel {
                                if host::is_native_select(&engine, sel) {
                                    host::open_select_dropdown(&engine, sel);
                                    window.request_redraw();
                                    return;
                                }
                                // An EDITABLE select (the file-transfer path box)
                                // opens its list only from the chevron zone at the
                                // right edge; the text area stays caret-editable.
                                let chevron = {
                                    let e = engine.borrow();
                                    e.screen_rects
                                        .get(&sel)
                                        .map_or(false, |r| cursor.0 >= r.0 + r.2 - host::SELECT_CHEVRON_HIT)
                                };
                                if chevron {
                                    host::open_select_dropdown(&engine, sel);
                                    window.request_redraw();
                                    return;
                                }
                            }
                        }
                        if let Some(d) = host::scrollbar_hit(&engine, cursor.0, cursor.1) {
                            host::scrollbar_drag_to(&engine, &d, cursor.1);
                            scrollbar_drag = Some(d);
                            buttons_down |= 1;
                            window.request_redraw();
                            return;
                        }
                        // Column-resizer: a press on a header divider starts the
                        // drag and owns the whole gesture (no DOM press, and no
                        // sort-click on release).
                        if hit.map_or(false, |t| {
                            let e = engine.borrow();
                            e.doc.closest("thead", t).is_some()
                        }) {
                            if let Some(d) = host::column_resize_hit(&engine, cursor.0, cursor.1) {
                                col_drag = Some(d);
                                buttons_down |= 1;
                                if current_cursor != winit::window::CursorIcon::ColResize {
                                    current_cursor = winit::window::CursorIcon::ColResize;
                                    window.set_cursor_icon(current_cursor);
                                }
                                return;
                            }
                        }
                        // Clicking the caption's app icon opens the native window
                        // system menu (Restore/Move/Size/Minimize/Maximize/Close)
                        // and double-clicking it closes the window, like Sciter's
                        // role="window-icon". A custom-chrome window has no OS title
                        // bar to summon it, so do it here. The menu is modal and
                        // owns the gesture, so return before the caption-drag /
                        // DOM-press paths. On platforms without a system menu this
                        // falls through to the normal drag.
                        if extended_frame {
                            let icon = hit.and_then(|t| {
                                let e = engine.borrow();
                                window_icon_at(&e.doc, t)
                                    .and_then(|k| e.screen_rects.get(&k).copied())
                            });
                            if let Some(rect) = icon {
                                if show_window_system_menu(&window, rect, scale) {
                                    return;
                                }
                            }
                        }
                        // Custom-chrome: edge -> OS resize, caption -> OS move.
                        // Either takes over the gesture, so skip the DOM press.
                        // But an interactive control ALWAYS clicks -- the min/max/
                        // close buttons sit in the top-right corner inside the
                        // resize zone, so without this they'd resize instead.
                        let interactive = hit.map_or(false, |t| {
                            let e = engine.borrow();
                            hit_is_interactive(&e.doc, t)
                        });
                        let mut chrome = false;
                        if extended_frame && !interactive {
                            let ls = window.inner_size().to_logical::<f32>(window.scale_factor());
                            let edge = if has_resize_edges(&window) {
                                resize_direction(cursor.0, cursor.1, ls.width, ls.height)
                            } else {
                                None
                            };
                            if let Some(dir) = edge {
                                window.drag_resize_window(dir).ok();
                                chrome = true;
                            } else if {
                                // Drag only when the TOPMOST element at the point is
                                // part of the titlebar (ancestor walk). The region
                                // fallback applies only when nothing was hit at all --
                                // an overlay painted over the header (a popup menu's
                                // full-window backdrop) must receive its mousedown
                                // (that's how menus dismiss), not start a window drag.
                                let e = engine.borrow();
                                match hit {
                                    Some(t) => {
                                        caption_drag_target(&e.doc, t)
                                            || (hit_is_msgbox_overlay(&e.doc, t)
                                                && point_in_caption_region(&e, cursor.0, cursor.1))
                                    }
                                    None => point_in_caption_region(&e, cursor.0, cursor.1),
                                }
                            } {
                                // Double-clicking the caption toggles maximize (native
                                // titlebar behaviour). drag_window() below enters the OS
                                // move loop and eats the mouse-up, so the gesture must be
                                // detected here at press time, not on release.
                                let now = Instant::now();
                                if last_caption_press
                                    .map_or(false, |t| now.duration_since(t).as_millis() < 400)
                                {
                                    window.set_maximized(!window.is_maximized());
                                    last_caption_press = None;
                                    caption_pressed = None;
                                } else {
                                    last_caption_press = Some(now);
                                    caption_pressed = Some(cursor);
                                }
                                chrome = true;
                            }
                        }
                        buttons_down |= 1;
                        left_press_hit = hit;
                        if !chrome {
                            if let Some(t) = host::on_mouse_target(&engine, hit) {
                                let dclick = last_onmouse_down
                                    .map_or(false, |i| i.elapsed().as_millis() < 400);
                                let etype = if dclick { 0x1006 } else { 0x1001 };
                                last_onmouse_down = if dclick { None } else { Some(Instant::now()) };
                                let consumed = host::dispatch_on_mouse(&mut interp, &engine, t,
                                    &raw_mouse(etype, cursor, 1, (0.0, 0.0), key_mods));
                                host::drain_events(&mut interp, &engine);
                                if consumed {
                                    window.request_redraw();
                                    return;
                                }
                            } else {
                                last_onmouse_down = None;
                            }
                            host::set_active(&engine, hit);
                            host::set_focus(&engine, hit);
                            if let Some(t) = hit {
                                let hslider = {
                                    let e = engine.borrow();
                                    e.doc.arena.get(t).map_or(false, host::is_hslider)
                                };
                                if hslider {
                                    host::slider_drag_to(&engine, t, cursor.0);
                                    slider_drag = Some(t);
                                }
                            }
                            // Clicking inside a text control places the caret at the
                            // clicked character (Sciter behaviour; previously typing
                            // could only append at the end).
                            if let Some(t) = hit {
                                let editable = {
                                    let e = engine.borrow();
                                    e.doc
                                        .arena
                                        .get(t)
                                        .map_or(false, host::is_text_editable)
                                };
                                if editable {
                                    let styles = host::cached_computed_styles(&engine);
                                    let rect = engine.borrow().screen_rects.get(&t).copied();
                                    if let Some(r) = rect {
                                        let is_ta = {
                                            let e = engine.borrow();
                                            e.doc.arena.get(t).map_or(false, |n| n.tag == "textarea")
                                        };
                                        let pad = if is_ta { 5.0 } else { host::input_pad_left(styles.get(&t)) };
                                        let (y, wrap, avail_h) = if is_ta {
                                            (
                                                cursor.1 - r.1 - pad,
                                                Some((r.2 - 2.0 * pad).max(8.0)),
                                                Some(((r.3 - 2.0 * pad).max(1.0)) as f64),
                                            )
                                        } else {
                                            (2.0, None, None)
                                        };
                                        let clicks = last_edit_click
                                            .filter(|(i, k, _, at)| {
                                                *k == t
                                                    && i.elapsed().as_millis() < 400
                                                    && (cursor.0 - at.0).abs() <= 4.0
                                                    && (cursor.1 - at.1).abs() <= 4.0
                                            })
                                            .map_or(1, |(_, _, c, _)| c % 3 + 1);
                                        last_edit_click = Some((Instant::now(), t, clicks, cursor));
                                        let mode = match clicks {
                                            2 => host::CaretClick::Word,
                                            3 => host::CaretClick::Line,
                                            _ => host::CaretClick::Place { extend: key_mods.2 },
                                        };
                                        let scroll = if is_ta {
                                            0.0
                                        } else {
                                            host::input_scroll_x(
                                                &engine,
                                                &mut text_system.borrow_mut(),
                                                t,
                                                r.2,
                                                &styles,
                                            )
                                        };
                                        host::caret_click(
                                            &engine,
                                            &mut text_system.borrow_mut(),
                                            t,
                                            cursor.0 - r.0 - pad + scroll,
                                            y,
                                            wrap,
                                            avail_h,
                                            mode,
                                            &styles,
                                        );
                                        text_drag = if clicks > 1 { None } else { Some(t) };
                                    }
                                } else {
                                    last_edit_click = None;
                                }
                            }
                            if let Some(target) = hit {
                                if let Err(e) =
                                    host::dispatch_dom_event(&mut interp, &engine, "mousedown", target)
                                {
                                    eprintln!("mousedown handler error: {}", e.0);
                                }
                            }
                            window.request_redraw();
                        }
                    }
                    WindowEvent::MouseInput {
                        state: ElementState::Released,
                        button: MouseButton::Left,
                        ..
                    } => {
                        let press_hit = left_press_hit.take();
                        // A caption press that released without moving was a click,
                        // not a drag -- drop the deferred move.
                        caption_pressed = None;
                        if let Some(k) = text_drag.take() {
                            host::finish_text_drag(&engine, k);
                            window.request_redraw();
                        }
                        if slider_drag.take().is_some() {
                            host::set_active(&engine, None);
                            buttons_down &= !1;
                            window.request_redraw();
                            return;
                        }
                        if col_drag.take().is_some() || scrollbar_drag.take().is_some() {
                            buttons_down &= !1;
                            return;
                        }
                        // Context menu open: a release on a menu item fires its click;
                        // a release anywhere then dismisses the menu.
                        // NOTE: copy the Option OUT of the borrow -- an
                        // `if let Some(m) = engine.borrow().x` scrutinee keeps the
                        // Ref alive for the whole body, and close_context_menu's
                        // borrow_mut() inside then panics (BorrowMutError, the
                        // peer-card click crash).
                        let active_menu = engine.borrow().active_context_menu;
                        if let Some(m) = active_menu {
                            let hit = { let e = engine.borrow(); hit_test_ordered(&e.doc, &e.screen_rects, &e.screen_order, cursor.0, cursor.1) };
                            let inside = hit.map_or(false, |h| {
                                let e = engine.borrow();
                                host::node_in_subtree(&e.doc, m, h)
                            });
                            if inside {
                                if let Some(target) = hit {
                                    if let Err(e) = host::dispatch_dom_event(&mut interp, &engine, "click", target) {
                                        eprintln!("context-menu click error: {}", e.0);
                                    }
                                }
                            }
                            host::close_context_menu(&engine);
                            window.request_redraw();
                            return;
                        }
                        host::set_active(&engine, None);
                        let hit = { let e = engine.borrow(); hit_test_ordered(&e.doc, &e.screen_rects, &e.screen_order, cursor.0, cursor.1) };
                        buttons_down &= !1;
                        host::dispatch_mouse_subscriptions(
                            &mut interp, &engine, 0x1002, hit, cursor.0, cursor.1,
                        );
                        if let Some(t) = host::on_mouse_target(&engine, hit) {
                            let consumed = host::dispatch_on_mouse(&mut interp, &engine, t,
                                &raw_mouse(0x1002, cursor, 1, (0.0, 0.0), key_mods));
                            host::drain_events(&mut interp, &engine);
                            if consumed {
                                window.request_redraw();
                                return;
                            }
                        }
                        if let Some(target) = hit {
                            use crate::script::interp::SV;
                            host::dispatch_dom_event_with(
                                &mut interp, &engine, "mouseup", target,
                                &[("propButton".into(), SV::Bool(false)),
                                  ("mainButton".into(), SV::Bool(true))],
                            )
                            .ok();
                        }
                        let click_target = match (press_hit, hit) {
                            (Some(p), Some(r)) => {
                                let e = engine.borrow();
                                click_target_for(&e.doc, p, r)
                            }
                            _ => None,
                        };
                        if let Some(target) = click_target {
                            if let Err(e) =
                                host::dispatch_dom_event(&mut interp, &engine, "click", target)
                            {
                                eprintln!("click handler error: {}", e.0);
                            }
                            // A second click on the same element within the double-
                            // click window fires dblclick (double-clicking a device
                            // card starts a connection).
                            let now = Instant::now();
                            if last_click
                                .map_or(false, |(t, k)| k == target && now.duration_since(t).as_millis() < 400)
                            {
                                if let Err(e) =
                                    host::dispatch_dom_event(&mut interp, &engine, "dblclick", target)
                                {
                                    eprintln!("dblclick handler error: {}", e.0);
                                }
                                last_click = None;
                            } else {
                                last_click = Some((now, target));
                            }
                            // Window-control roles are the behaviour themselves in
                            // Sciter (no script handler on min/max/close).
                            let role = if press_hit == hit {
                                let e = engine.borrow();
                                window_control_role(&e.doc, target)
                                    .or_else(|| window_control_at_point(&e, cursor.0, cursor.1))
                            } else {
                                None
                            };
                            match role.as_deref() {
                                Some("window-close") => host::request_view_close(),
                                Some("window-minimize") => window.set_minimized(true),
                                Some("window-maximize") => window.set_maximized(!window.is_maximized()),
                                _ => {}
                            }
                        }
                        window.request_redraw();
                    }
                    WindowEvent::MouseInput {
                        state: ElementState::Pressed,
                        button: MouseButton::Right,
                        ..
                    } => {
                        buttons_down |= 2;
                        let hit = { let e = engine.borrow(); hit_test_ordered(&e.doc, &e.screen_rects, &e.screen_order, cursor.0, cursor.1) };
                        if let Some(t) = host::on_mouse_target(&engine, hit) {
                            host::dispatch_on_mouse(&mut interp, &engine, t,
                                &raw_mouse(0x1001, cursor, 2, (0.0, 0.0), key_mods));
                            host::drain_events(&mut interp, &engine);
                        }
                    }
                    WindowEvent::MouseInput {
                        state: ElementState::Released,
                        button: MouseButton::Right,
                        ..
                    } => {
                        buttons_down &= !2;
                        // Right-click opens the target's <menu.context> at the cursor
                        // (folder-view Send/Receive/Rename/Delete); an empty target
                        // just dismisses any open menu.
                        let hit = { let e = engine.borrow(); hit_test_ordered(&e.doc, &e.screen_rects, &e.screen_order, cursor.0, cursor.1) };
                        // A right-click on a text input shows the native edit menu
                        // (Cut/Copy/Paste/Select All), like Sciter.
                        let editable = hit.map_or(false, |t| {
                            let e = engine.borrow();
                            e.doc.arena.get(t).map_or(false, host::is_text_editable)
                        });
                        if editable {
                            if let Some(t) = hit {
                                host::open_edit_menu(&engine, t, cursor.0, cursor.1);
                                window.request_redraw();
                                return;
                            }
                        }
                        // Right-click off an input dismisses an open edit menu.
                        if host::close_edit_menu(&engine) {
                            window.request_redraw();
                            return;
                        }
                        if let Some(t) = host::on_mouse_target(&engine, hit) {
                            let consumed = host::dispatch_on_mouse(&mut interp, &engine, t,
                                &raw_mouse(0x1002, cursor, 2, (0.0, 0.0), key_mods));
                            host::drain_events(&mut interp, &engine);
                            if consumed {
                                window.request_redraw();
                                return;
                            }
                        }
                        // DOM mouseup with propButton (the address-book tag menu is
                        // a right-mouseup handler) and contextmenu (the terminal's
                        // right-click paste); either consumed suppresses the
                        // built-in <menu.context> handling.
                        if let Some(t) = hit {
                            use crate::script::interp::SV;
                            if host::dispatch_dom_event_with(
                                &mut interp, &engine, "mouseup", t,
                                &[("propButton".into(), SV::Bool(true)),
                                  ("mainButton".into(), SV::Bool(false))],
                            )
                            .unwrap_or(false)
                            {
                                host::drain_events(&mut interp, &engine);
                                window.request_redraw();
                                return;
                            }
                            if host::dispatch_dom_event(&mut interp, &engine, "contextmenu", t)
                                .unwrap_or(false)
                            {
                                host::drain_events(&mut interp, &engine);
                                window.request_redraw();
                                return;
                            }
                        }
                        let opened = hit.map_or(false, |h| host::open_context_menu(&engine, h, cursor.0, cursor.1));
                        if !opened {
                            host::close_context_menu(&engine);
                        }
                        window.request_redraw();
                    }
                    WindowEvent::MouseInput {
                        state,
                        button: MouseButton::Middle,
                        ..
                    } => {
                        let (etype, add) = match state {
                            ElementState::Pressed => (0x1001, true),
                            ElementState::Released => (0x1002, false),
                        };
                        if add { buttons_down |= 4; } else { buttons_down &= !4; }
                        let hit = { let e = engine.borrow(); hit_test_ordered(&e.doc, &e.screen_rects, &e.screen_order, cursor.0, cursor.1) };
                        if let Some(t) = host::on_mouse_target(&engine, hit) {
                            host::dispatch_on_mouse(&mut interp, &engine, t,
                                &raw_mouse(etype, cursor, 4, (0.0, 0.0), key_mods));
                            host::drain_events(&mut interp, &engine);
                        }
                    }
                    WindowEvent::MouseInput {
                        state,
                        button: button @ (MouseButton::Back | MouseButton::Forward),
                        ..
                    } => {
                        let bit: i64 = if button == MouseButton::Back { 8 } else { 16 };
                        let (etype, add) = match state {
                            ElementState::Pressed => (0x1001, true),
                            ElementState::Released => (0x1002, false),
                        };
                        if add { buttons_down |= bit; } else { buttons_down &= !bit; }
                        let hit = { let e = engine.borrow(); hit_test_ordered(&e.doc, &e.screen_rects, &e.screen_order, cursor.0, cursor.1) };
                        if let Some(t) = host::on_mouse_target(&engine, hit) {
                            host::dispatch_on_mouse(&mut interp, &engine, t,
                                &raw_mouse(etype, cursor, bit, (0.0, 0.0), key_mods));
                            host::drain_events(&mut interp, &engine);
                        }
                    }
                    WindowEvent::CursorLeft { .. } => {
                        pointer_inside = false;
                        if let Some(old) = onmouse_over.take() {
                            host::dispatch_on_mouse(&mut interp, &engine, old,
                                &raw_mouse(0x1005, cursor, buttons_down, (0.0, 0.0), key_mods));
                            host::drain_events(&mut interp, &engine);
                        }
                    }
                    WindowEvent::ModifiersChanged(m) => {
                        let s = m.state();
                        key_mods = (s.alt_key(), s.control_key(), s.shift_key(), s.super_key());
                        host::set_key_mods(key_mods);
                    }
                    WindowEvent::Ime(ime) => {
                        use winit::event::Ime;
                        match ime {
                            Ime::Enabled | Ime::Disabled => {
                                if host::clear_preedit(&engine) {
                                    window.request_redraw();
                                }
                            }
                            Ime::Preedit(text, cursor) => {
                                if host::set_preedit(&engine, &text, cursor.map(|(s, _)| s)) {
                                    window.request_redraw();
                                }
                            }
                            Ime::Commit(text) => {
                                host::commit_ime_text(&mut interp, &engine, &text, key_mods);
                                window.request_redraw();
                            }
                        }
                    }
                    WindowEvent::KeyboardInput { event: kev, is_synthetic: true, .. }
                        if kev.state == ElementState::Pressed => {}
                    WindowEvent::KeyboardInput { event: kev, .. } => {
                        {
                            let etype = if kev.state == ElementState::Pressed { 0x2001 } else { 0x2002 };
                            let code = vk_keycode(&kev);
                            let mut consumed = code != 0
                                && host::dispatch_on_key(&mut interp, &engine, etype, code, key_mods);
                            // Deliver KEY_CHAR after KEY_DOWN so self.onKey can
                            // forward typed characters (remote.tis suppresses the
                            // char on KEY_DOWN and sends it on KEY_CHAR). Only
                            // matters when the raw-input grab is not capturing;
                            // when it is, self.onKey never runs for these keys.
                            if kev.state == ElementState::Pressed {
                                if let Some(txt) = &kev.text {
                                    for ch in txt.chars().filter(|c| !c.is_control()) {
                                        if host::dispatch_on_key(
                                            &mut interp, &engine, 0x2003, ch as i64, key_mods,
                                        ) {
                                            consumed = true;
                                        }
                                    }
                                }
                            }
                            if consumed {
                                host::drain_events(&mut interp, &engine);
                                window.request_redraw();
                                return;
                            }
                        }
                        // Sciter also delivers keydown/keypress/keyup as DOM events
                        // that bubble from the focused element (the terminal's whole
                        // input path). A consumed keydown suppresses keypress and
                        // local typing, matching Sciter's consume semantics.
                        {
                            use crate::script::interp::SV;
                            let focus_target = {
                                let e = engine.borrow();
                                host::key_event_target(&e)
                            };
                            let key_extra = |kc: i64| -> Vec<(String, SV)> {
                                vec![
                                    ("keyCode".into(), SV::Int(kc)),
                                    ("altKey".into(), SV::Bool(key_mods.0)),
                                    ("ctrlKey".into(), SV::Bool(key_mods.1)),
                                    ("shiftKey".into(), SV::Bool(key_mods.2)),
                                    ("commandKey".into(), SV::Bool(key_mods.3)),
                                    ("metaKey".into(), SV::Bool(key_mods.3)),
                                ]
                            };
                            let code = vk_keycode(&kev);
                            if kev.state == ElementState::Pressed {
                                let mut consumed = false;
                                if code != 0 {
                                    consumed = host::dispatch_dom_event_with(
                                        &mut interp, &engine, "keydown", focus_target, &key_extra(code),
                                    )
                                    .unwrap_or(false);
                                }
                                if !consumed
                                    && !modifiers_suppress_text(
                                        key_mods.0,
                                        key_mods.1,
                                        cfg!(target_os = "macos"),
                                    )
                                {
                                    if let Some(txt) = &kev.text {
                                        for ch in txt.chars().filter(|c| !c.is_control()) {
                                            if host::dispatch_dom_event_with(
                                                &mut interp, &engine, "keypress", focus_target,
                                                &key_extra(ch as i64),
                                            )
                                            .unwrap_or(false)
                                            {
                                                consumed = true;
                                            }
                                        }
                                    }
                                }
                                if consumed {
                                    host::drain_events(&mut interp, &engine);
                                    window.request_redraw();
                                    return;
                                }
                            } else if code != 0 {
                                host::dispatch_dom_event_with(
                                    &mut interp, &engine, "keyup", focus_target, &key_extra(code),
                                )
                                .ok();
                            }
                        }
                        if kev.state == ElementState::Pressed
                            && matches!(
                                kev.logical_key,
                                Key::Named(NamedKey::Enter) | Key::Named(NamedKey::Space)
                            )
                        {
                            let target = {
                                let e = engine.borrow();
                                host::activatable_focus(&e)
                            };
                            if let Some(t) = target {
                                host::dispatch_dom_event(&mut interp, &engine, "click", t).ok();
                                host::drain_events(&mut interp, &engine);
                                window.request_redraw();
                                return;
                            }
                        }
                        if kev.state == ElementState::Pressed
                            && matches!(kev.logical_key, Key::Named(NamedKey::Tab))
                            && !key_mods.1
                            && !key_mods.3
                        {
                            if host::focus_move(&engine, key_mods.2) {
                                host::drain_events(&mut interp, &engine);
                                window.request_redraw();
                                return;
                            }
                        }
                        if kev.state == ElementState::Pressed
                            && matches!(
                                kev.logical_key,
                                Key::Named(NamedKey::ArrowUp) | Key::Named(NamedKey::ArrowDown)
                            )
                        {
                            let down = matches!(kev.logical_key, Key::Named(NamedKey::ArrowDown));
                            if let Some(row) = host::grid_arrow_target(&engine, down) {
                                host::dispatch_dom_event(&mut interp, &engine, "click", row).ok();
                                host::scroll_into_view(&engine, row);
                                host::drain_events(&mut interp, &engine);
                                window.request_redraw();
                                return;
                            }
                        }
                        if kev.state == ElementState::Pressed
                            && !is_shortcut_chord(
                                key_mods.0,
                                key_mods.1,
                                key_mods.3,
                                cfg!(target_os = "macos"),
                            )
                        {
                            let typed = kev
                                .text
                                .as_ref()
                                .and_then(|t| t.chars().find(|c| !c.is_control()));
                            if let Some(ch) = typed {
                                if let Some(row) = host::grid_typeahead_target(&engine, ch) {
                                    host::dispatch_dom_event(&mut interp, &engine, "click", row)
                                        .ok();
                                    host::scroll_into_view(&engine, row);
                                    host::drain_events(&mut interp, &engine);
                                    window.request_redraw();
                                    return;
                                }
                            }
                        }
                        if kev.state == ElementState::Pressed {
                            let mut dirty = false;
                            let shift = key_mods.2;
                            let shortcut = is_shortcut_chord(
                                key_mods.0,
                                key_mods.1,
                                key_mods.3,
                                cfg!(target_os = "macos"),
                            );
                            match &kev.logical_key {
                                Key::Named(NamedKey::Backspace) => {
                                    dirty = host::backspace(&engine);
                                }
                                Key::Named(NamedKey::Delete) => {
                                    dirty = host::delete_forward(&engine);
                                }
                                Key::Named(NamedKey::ArrowLeft) => {
                                    if host::move_caret(&engine, -1, false, false, shift) {
                                        window.request_redraw();
                                    }
                                }
                                Key::Named(NamedKey::ArrowRight) => {
                                    if host::move_caret(&engine, 1, false, false, shift) {
                                        window.request_redraw();
                                    }
                                }
                                Key::Named(NamedKey::ArrowUp) | Key::Named(NamedKey::ArrowDown) => {
                                    let down =
                                        matches!(kev.logical_key, Key::Named(NamedKey::ArrowDown));
                                    let styles = host::cached_computed_styles(&engine);
                                    if host::move_caret_line(
                                        &engine,
                                        &mut text_system.borrow_mut(),
                                        down,
                                        shift,
                                        &styles,
                                    ) {
                                        window.request_redraw();
                                    }
                                }
                                Key::Named(NamedKey::Home) => {
                                    if host::move_caret(&engine, 0, true, false, shift) {
                                        window.request_redraw();
                                    }
                                }
                                Key::Named(NamedKey::End) => {
                                    if host::move_caret(&engine, 0, false, true, shift) {
                                        window.request_redraw();
                                    }
                                }
                                Key::Named(NamedKey::Space) => {
                                    dirty = host::type_char(&engine, ' ');
                                }
                                Key::Named(NamedKey::Enter) => {
                                    dirty = host::insert_newline(&engine);
                                }
                                Key::Character(s) if shortcut && s.eq_ignore_ascii_case("a") => {
                                    if host::select_all(&engine) {
                                        window.request_redraw();
                                    }
                                }
                                Key::Character(s) if shortcut && s.eq_ignore_ascii_case("c") => {
                                    let sel = { let e = engine.borrow(); host::selected_text(&e) };
                                    if let Some(text) = sel {
                                        let _ = arboard::Clipboard::new()
                                            .and_then(|mut c| c.set_text(text));
                                    }
                                }
                                Key::Character(s) if shortcut && s.eq_ignore_ascii_case("x") => {
                                    let sel = { let e = engine.borrow(); host::selected_text(&e) };
                                    if let Some(text) = sel {
                                        let _ = arboard::Clipboard::new()
                                            .and_then(|mut c| c.set_text(text));
                                        dirty = host::delete_forward(&engine);
                                    }
                                }
                                Key::Character(s) if shortcut && s.eq_ignore_ascii_case("v") => {
                                    if let Ok(text) =
                                        arboard::Clipboard::new().and_then(|mut c| c.get_text())
                                    {
                                        let clean = host::clean_pasted_text(
                                            &text,
                                            host::focused_input_is_multiline(&engine),
                                        );
                                        if !clean.is_empty() {
                                            dirty = host::insert_text(&engine, &clean);
                                        }
                                    }
                                }
                                _ if shortcut => {}
                                _ => {
                                    if let Some(txt) = &kev.text {
                                        for ch in txt.chars() {
                                            if host::type_char(&engine, ch) {
                                                dirty = true;
                                            }
                                        }
                                    }
                                }
                            }
                            if dirty {
                                // Sciter fires `change` on a text input for every
                                // edit; handlers like the address-book search filter
                                // (event change $(input)) depend on it.
                                let focused = { let e = engine.borrow(); host::focused_input(&e) };
                                if let Some(fk) = focused {
                                    host::dispatch_dom_event(&mut interp, &engine, "change", fk).ok();
                                    host::drain_events(&mut interp, &engine);
                                }
                                window.request_redraw();
                            }
                        }
                    }
                    WindowEvent::RedrawRequested => {
                        #[cfg(any(target_os = "windows", target_os = "macos"))]
                        {
                            let pending = {
                                let e = engine.borrow();
                                super::a11y::take_pending(&e.doc)
                            };
                            for (action, target) in pending {
                                match action {
                                    super::a11y::PendingAction::Click => {
                                        host::dispatch_dom_event(
                                            &mut interp, &engine, "click", target,
                                        )
                                        .ok();
                                    }
                                    super::a11y::PendingAction::Focus => {
                                        host::set_focus(&engine, Some(target));
                                    }
                                }
                                host::drain_events(&mut interp, &engine);
                            }
                        }
                        host::record_fg_overlays(&mut interp, &engine);
                        repaint_main(
                            &mut gpu.borrow_mut(),
                            &engine,
                            &mut text_system.borrow_mut(),
                            &mut last_rects.borrow_mut(),
                            &mut layout_cache.borrow_mut(),
                            scale,
                        );
                        if let Some(r) = host::caret_device_rect() {
                            main_caret_rect = Some(r);
                        }
                        if pending_statechange > 0 {
                            let is = window.inner_size();
                            let matched = {
                                let g = gpu.borrow();
                                g.width() == is.width && g.height() == is.height
                            };
                            pending_statechange -= 1;
                            // Fire once a repaint has landed at the new size, or
                            // after the countdown -- waiting forever on a size
                            // match left the header hidden and its buttons
                            // disabled when the matching Resized never came
                            // (exit-fullscreen), which read as "cannot restore /
                            // cannot drag the remote window".
                            if matched || pending_statechange == 0 {
                                pending_statechange = 0;
                                host::fire_view_event(&mut interp, &engine, "statechange");
                            }
                            window.request_redraw();
                        }
                    }
                    WindowEvent::HoveredFile(path) => {
                        os_drop_files.push(path);
                        if let Some((x, y, _)) = os_mouse_state(&window) {
                            if let Some(t) = host::file_drop_target_at(&engine, x, y) {
                                host::dispatch_os_file_drag(
                                    &mut interp, &engine, "dragenter", t, x, y, &os_drop_files,
                                );
                                os_drop_hovering = true;
                                window.request_redraw();
                            }
                        }
                    }
                    WindowEvent::HoveredFileCancelled => {
                        if os_drop_hovering {
                            if let Some((x, y, _)) = os_mouse_state(&window) {
                                if let Some(t) = host::file_drop_target_at(&engine, x, y) {
                                    host::dispatch_os_file_drag(
                                        &mut interp, &engine, "dragleave", t, x, y, &[],
                                    );
                                }
                            }
                            window.request_redraw();
                        }
                        os_drop_files.clear();
                        os_drop_hovering = false;
                    }
                    WindowEvent::DroppedFile(path) => {
                        // Fired one-per-file with no batch end; collect and flush
                        // in AboutToWait as a single drop event.
                        if !os_drop_files.contains(&path) {
                            os_drop_files.push(path);
                        }
                        os_drop_ready = true;
                    }
                    _ => {}
                    }
                }
                Event::AboutToWait => {
                    let focused_edit = host::focused_input(&engine.borrow());
                    let want_ime = host::ime_force() || focused_edit.is_some();
                    if want_ime != ime_on {
                        ime_on = want_ime;
                        window.set_ime_allowed(want_ime);
                        if !want_ime {
                            ime_area = None;
                            main_caret_rect = None;
                            if host::clear_preedit(&engine) {
                                window.request_redraw();
                            }
                        }
                    }
                    let want_pw = want_ime
                        && focused_edit.map_or(false, |k| {
                            engine
                                .borrow()
                                .doc
                                .arena
                                .get(k)
                                .map_or(false, |n| n.attr("type") == Some("password"))
                        });
                    if want_pw != ime_pw {
                        ime_pw = want_pw;
                        window.set_ime_purpose(if want_pw {
                            winit::window::ImePurpose::Password
                        } else {
                            winit::window::ImePurpose::Normal
                        });
                    }
                    if want_ime && window.has_focus() {
                        if let Some(r) = main_caret_rect {
                            if ime_area != Some(r) {
                                ime_area = Some(r);
                                window.set_ime_cursor_area(
                                    winit::dpi::PhysicalPosition::new(r.0, r.1),
                                    winit::dpi::PhysicalSize::new(
                                        (r.2 - r.0).max(1.0),
                                        (r.3 - r.1).max(1.0),
                                    ),
                                );
                            }
                        }
                    }
                    if uncloak_pending {
                        uncloak_pending = false;
                        gpu.borrow().flush();
                        set_window_cloak(&window, false);
                        apply_window_icon(&window);
                    }
                    #[cfg(windows)]
                    {
                        let dropped = take_dropped_files();
                        if !dropped.is_empty() {
                            os_drop_files = dropped;
                            os_drop_ready = true;
                        }
                    }
                    if os_drop_ready {
                        os_drop_ready = false;
                        if let Some((x, y, _)) = os_mouse_state(&window) {
                            if let Some(t) = host::file_drop_target_at(&engine, x, y) {
                                host::dispatch_os_file_drag(
                                    &mut interp, &engine, "drop", t, x, y, &os_drop_files,
                                );
                            }
                        }
                        os_drop_files.clear();
                        os_drop_hovering = false;
                        window.request_redraw();
                    }
                    // Keep view.windowState / view.screenBox reads in sync with the
                    // real OS window + monitor.
                    let st = if window.fullscreen().is_some() {
                        5
                    } else if window.is_minimized() == Some(true) {
                        2
                    } else if window.is_maximized() {
                        3
                    } else {
                        1
                    };
                    host::set_current_window_state(st);
                    raise::when_mapped(&window);
                    if let Some(cw) = child.as_ref() {
                        raise::when_mapped(&cw.window);
                    }
                    // Sciter fires `statechange` on the view when the OS window
                    // state flips; header.tis relies on it to hide the toolbar on
                    // entering fullscreen and to toggle the button states. The fire
                    // is DEFERRED until a repaint at the new window size has landed
                    // (except minimize, which never repaints): stateChanged hides
                    // the header, and the toolbar re-center must be able to measure
                    // the header at its new width from the cached layout rects.
                    if st != last_window_state {
                        last_window_state = st;
                        if st == 2 {
                            host::fire_view_event(&mut interp, &engine, "statechange");
                        } else {
                            pending_statechange = 30;
                        }
                        window.request_redraw();
                    }
                    if let Some(m) = window.current_monitor() {
                        let monitors: Vec<_> = window.available_monitors().collect();
                        let current = monitors.iter().position(|x| *x == m).unwrap_or(0);
                        host::set_screens(monitors.len().max(1) as i64, current as i64);
                        let sf = window.scale_factor();
                        let p = m.position();
                        let s = m.size();
                        host::set_monitor_rect((
                            p.x as f64 / sf,
                            p.y as f64 / sf,
                            s.width as f64 / sf,
                            s.height as f64 / sf,
                        ));
                        if let Some((wx, wy, ww, wh)) = os_workarea_physical() {
                            host::set_workarea_rect((wx / sf, wy / sf, ww / sf, wh / sf));
                        }
                    }
                    {
                        let pos = window.outer_position().unwrap_or_default();
                        let sf = window.scale_factor();
                        let sz = window.inner_size();
                        host::set_window_rect((
                            pos.x as f64 / sf,
                            pos.y as f64 / sf,
                            sz.width as f64 / sf,
                            sz.height as f64 / sf,
                        ));
                    }
                    let now = start.elapsed().as_millis() as f64;
                    // Tick the chat child (its own timers) and repaint it when
                    // its timers fired, a parent-driven chatbox.refresh() asked, or
                    // any document changed since the last tick.
                    if let Some(cw) = child.as_ref() {
                        host::set_current_engine(&cw.engine);
                        cw.engine.borrow_mut().now_ms = now;
                        let child_due =
                            cw.engine.borrow().timers.iter().any(|(t, _, _)| *t <= now);
                        if child_due {
                            host::pump_timers(&mut interp, &cw.engine, now).ok();
                        }
                        let child_refresh =
                            std::mem::take(&mut cw.engine.borrow_mut().repaint_requested);
                        let epoch_now = super::dom::layout_epoch();
                        let child_scrolling = host::animate_scroll(&cw.engine);
                        if child_due || child_refresh || child_scrolling || epoch_now != child_seen_epoch {
                            cw.window.request_redraw();
                        }
                        child_seen_epoch = epoch_now;
                        host::set_current_engine(&engine);
                    }
                    let due = {
                        let e = engine.borrow();
                        e.timers.iter().any(|(t, _, _)| *t <= now)
                    };
                    if due {
                        let epoch_before = super::dom::layout_epoch();
                        if let Err(e) = host::pump_timers(&mut interp, &engine, now) {
                            eprintln!("timer error: {}", e.0);
                        }
                        if super::dom::layout_epoch() != epoch_before
                            || engine.borrow().repaint_requested
                        {
                            window.request_redraw();
                        }
                    }
                    let cursor_epoch = engine.borrow().cursor_image_epoch;
                    if cursor_epoch != cursor_epoch_seen {
                        cursor_epoch_seen = cursor_epoch;
                        if pointer_inside {
                            let img = {
                                let e = engine.borrow();
                                let hit = hit_test_ordered(
                                    &e.doc,
                                    &e.screen_rects,
                                    &e.screen_order,
                                    e.pointer.0,
                                    e.pointer.1,
                                );
                                cursor_image_at(&e, hit)
                            };
                            #[cfg(windows)]
                            apply_native_cursor(&mut native_cursor, img, scale);
                            #[cfg(not(windows))]
                            {
                                let custom =
                                    img.is_some() && !super::paint_backend::backend_is_cpu();
                                if custom != os_cursor_hidden {
                                    os_cursor_hidden = custom;
                                    window.set_cursor_visible(!custom);
                                    window.request_redraw();
                                } else if custom {
                                    window.request_redraw();
                                }
                            }
                        }
                    }
                    // Script stdout is buffered in interp.output; the render bin
                    // prints it but the live host never drained it, so println
                    // debugging in a real window silently vanished.
                    for line in interp.output.drain(..) {
                        println!("[script] {}", line);
                    }
                    host::pump_video_events(&engine);
                    // Keep the clock advancing and repaint at ~60fps while an
                    // indeterminate progress bar is on screen so it animates.
                    let (progress, caret) = {
                        let mut e = engine.borrow_mut();
                        e.now_ms = now;
                        (host::has_active_progress(&e), host::focused_input(&e).is_some())
                    };
                    // Smooth wheel scrolling: ease scroll_top toward its target.
                    let scrolling = host::animate_scroll(&engine);
                    if scrolling {
                        window.request_redraw();
                    }
                    // Show a tooltip once the cursor has rested on a title element.
                    let mut tip_pending = false;
                    if let Some((el, since)) = hover_title {
                        let shown = engine.borrow().tooltip.is_some();
                        if !shown {
                            if since.elapsed().as_millis() >= 600 {
                                let title = {
                                    let e = engine.borrow();
                                    e.doc.arena.get(el).and_then(|n| n.attr("title")).map(|s| s.to_string())
                                };
                                if let Some(t) = title {
                                    engine.borrow_mut().tooltip = Some((t, cursor.0, cursor.1));
                                    window.request_redraw();
                                }
                            } else {
                                tip_pending = true;
                            }
                        }
                    }
                    if progress || scrolling {
                        // Progress sweeps and scroll easing need ~60fps.
                        window.request_redraw();
                        elwt.set_control_flow(ControlFlow::WaitUntil(
                            Instant::now() + std::time::Duration::from_millis(16),
                        ));
                    } else {
                        // Idle pacing: wake at the earliest of the tooltip rest
                        // tick, the caret's next 530ms blink boundary, or the next
                        // script timer. The caret repaints only when its phase
                        // actually flips; a 120ms poll of the 530ms square wave
                        // pinned a core on the CPU raster backend.
                        let mut wake_ms = f64::INFINITY;
                        if tip_pending {
                            window.request_redraw();
                            wake_ms = wake_ms.min(120.0);
                        }
                        if caret {
                            let solid_now = last_input.elapsed().as_secs() >= 5;
                            if solid_now != caret_solid {
                                caret_solid = solid_now;
                                super::paint::CARET_SOLID
                                    .store(solid_now, std::sync::atomic::Ordering::Relaxed);
                                window.request_redraw();
                            }
                            if !solid_now {
                                let phase = (now / 530.0) as i64 % 2;
                                if phase != caret_blink_phase {
                                    caret_blink_phase = phase;
                                    window.request_redraw();
                                }
                                wake_ms = wake_ms.min((530.0 - (now % 530.0)).max(10.0));
                            }
                        }
                        let next = {
                            let e = engine.borrow();
                            e.timers.iter().map(|(t, _, _)| *t).fold(f64::INFINITY, f64::min)
                        };
                        if next.is_finite() {
                            wake_ms = wake_ms.min((next - now).max(0.0));
                        }
                        if wake_ms.is_finite() {
                            elwt.set_control_flow(ControlFlow::WaitUntil(
                                Instant::now() + std::time::Duration::from_millis(wake_ms as u64),
                            ));
                        }
                    }
                }
                _ => {}
            }
        })
        .map_err(|e| format!("run: {}", e))
}

#[cfg(test)]
mod tests {
    use super::intel_broadwell_on_dx12;

    fn adapter(vendor: u32, device: u32, backend: wgpu::Backend) -> wgpu::AdapterInfo {
        wgpu::AdapterInfo {
            name: String::new(),
            vendor,
            device,
            device_type: wgpu::DeviceType::IntegratedGpu,
            driver: String::new(),
            driver_info: String::new(),
            backend,
        }
    }

    #[test]
    fn only_broadwell_on_dx12_is_matched() {
        assert!(intel_broadwell_on_dx12(&adapter(0x8086, 0x1616, wgpu::Backend::Dx12)));
        assert!(!intel_broadwell_on_dx12(&adapter(0x8086, 0x0416, wgpu::Backend::Dx12)));
        assert!(!intel_broadwell_on_dx12(&adapter(0x8086, 0x1916, wgpu::Backend::Dx12)));
        assert!(!intel_broadwell_on_dx12(&adapter(0x8086, 0x1616, wgpu::Backend::Vulkan)));
        assert!(!intel_broadwell_on_dx12(&adapter(0x10DE, 0x1616, wgpu::Backend::Dx12)));
    }
}
