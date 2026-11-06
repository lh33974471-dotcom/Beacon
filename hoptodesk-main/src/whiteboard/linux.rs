use super::{
    server::{Ripple, EVENT_PROXY},
    win_linux::{create_font_face, draw_text},
    Cursor, CustomEvent,
};
use hbb_common::{bail, log, tokio::sync::mpsc::unbounded_channel, ResultType};
use softbuffer::{Context, Surface};
use std::{
    collections::HashMap,
    ffi::{c_char, c_int, c_short, c_ulong, c_ushort, CString},
    num::NonZeroU32,
    sync::Arc,
    time::Instant,
};
use tiny_skia::{Color, FillRule, Paint, PathBuilder, PixmapMut, Stroke, Transform};
use ttf_parser::Face;
use winit::raw_window_handle::{
    DisplayHandle, HasDisplayHandle, HasWindowHandle, RawDisplayHandle, RawWindowHandle,
};
use winit::{
    application::ApplicationHandler,
    dpi::{PhysicalPosition, PhysicalSize},
    event::WindowEvent,
    event_loop::{ActiveEventLoop, EventLoop},
    platform::x11::{WindowAttributesExtX11, WindowType},
    window::{Window, WindowId, WindowLevel},
};

enum _XDisplay {}
type Display = _XDisplay;

type XID = c_ulong;
type XserverRegion = XID;
type Atom = c_ulong;

#[derive(Debug, Clone, Copy, PartialEq)]
#[repr(C)]
pub struct XRectangle {
    pub x: c_short,
    pub y: c_short,
    pub width: c_ushort,
    pub height: c_ushort,
}

#[link(name = "Xfixes")]
extern "C" {
    fn XFixesCreateRegion(
        dpy: *mut Display,
        rectangles: *mut XRectangle,
        nrectangles: c_int,
    ) -> XserverRegion;
    fn XFixesDestroyRegion(dpy: *mut Display, region: XserverRegion) -> ();
    fn XFixesSetWindowShapeRegion(
        dpy: *mut Display,
        win: XID,
        shape_kind: c_int,
        x_off: c_int,
        y_off: c_int,
        region: XserverRegion,
    ) -> ();
}

#[link(name = "X11")]
extern "C" {
    fn XInternAtom(dpy: *mut Display, name: *const c_char, only_if_exists: c_int) -> Atom;
    fn XGetSelectionOwner(dpy: *mut Display, selection: Atom) -> XID;
}

const SHAPE_BOUNDING: c_int = 0;
const SHAPE_INPUT: std::ffi::c_int = 2;
const VISIBLE_ALPHA: u32 = 0x80;

struct BoundingShape {
    display: *mut Display,
    window: XID,
    compositor_selection: Option<Atom>,
    applied: Option<Vec<XRectangle>>,
}

impl BoundingShape {
    fn has_compositor(&self) -> bool {
        match self.compositor_selection {
            Some(selection) => unsafe { XGetSelectionOwner(self.display, selection) != 0 },
            None => true,
        }
    }

    fn update(&mut self, pixels: &mut [u32], width: usize) {
        let mut rects = if self.has_compositor() {
            drawn_rects(pixels, width, has_alpha)
        } else {
            drawn_rects(pixels, width, make_opaque)
        };
        if self.applied.as_ref() == Some(&rects) {
            return;
        }
        unsafe {
            let region = XFixesCreateRegion(self.display, rects.as_mut_ptr(), rects.len() as c_int);
            XFixesSetWindowShapeRegion(self.display, self.window, SHAPE_BOUNDING, 0, 0, region);
            XFixesDestroyRegion(self.display, region);
        }
        self.applied = Some(rects);
    }
}

fn drawn_rects(
    pixels: &mut [u32],
    width: usize,
    visible: impl Fn(&mut u32) -> bool,
) -> Vec<XRectangle> {
    let mut rects = Vec::new();
    for (y, row) in pixels.chunks_exact_mut(width).enumerate() {
        let mut run_start = None;
        for x in 0..=width {
            let shown = x < width && visible(&mut row[x]);
            match (shown, run_start) {
                (true, None) => run_start = Some(x),
                (false, Some(start)) => {
                    rects.push(XRectangle {
                        x: start as c_short,
                        y: y as c_short,
                        width: (x - start) as c_ushort,
                        height: 1,
                    });
                    run_start = None;
                }
                _ => {}
            }
        }
    }
    rects
}

fn has_alpha(pixel: &mut u32) -> bool {
    *pixel >> 24 != 0
}

fn make_opaque(pixel: &mut u32) -> bool {
    let (argb, alpha) = (*pixel, *pixel >> 24);
    if alpha < VISIBLE_ALPHA {
        return false;
    }
    let channel = |shift: u32| ((argb >> shift & 0xFF) * 0xFF / alpha).min(0xFF) << shift;
    *pixel = 0xFF00_0000 | channel(16) | channel(8) | channel(0);
    true
}

fn get_display_from_xwayland() -> Option<String> {
    if let Ok(output) = crate::platform::run_cmds("pgrep -a Xwayland") {
        // 1410 /usr/bin/Xwayland :1 -auth /run/user/1000/xauth_RoDZey -listenfd 8 -listenfd 9 -displayfd 76 -wm 78 -rootless -enable-ei-portal
        if output.contains("Xwayland") {
            if let Some(display) = output.split_whitespace().nth(2) {
                if display.starts_with(':') {
                    return Some(display.to_string());
                }
            }
        }
    }
    None
}

fn preset_env() -> bool {
    if crate::platform::is_x11() {
        return true;
    }
    if let Some(display) = get_display_from_xwayland() {
        // https://github.com/rust-windowing/winit/blob/f6893a4390dfe6118ce4b33458d458fd3efd3025/src/event_loop.rs#L99
        // It is acceptable to modify global environment variables here because this process is an isolated,
        // dedicated "whiteboard" process.
        std::env::set_var("DISPLAY", display);
        std::env::remove_var("WAYLAND_DISPLAY");
        return true;
    }
    false
}

pub fn is_supported() -> bool {
    crate::platform::is_x11() || get_display_from_xwayland().is_some()
}

pub fn run() {
    if !preset_env() {
        return;
    }

    let event_loop = match EventLoop::<(String, CustomEvent)>::with_user_event().build() {
        Ok(el) => el,
        Err(e) => {
            log::error!("Failed to create event loop: {}", e);
            return;
        }
    };

    let event_loop_proxy = event_loop.create_proxy();
    EVENT_PROXY.write().unwrap().replace(event_loop_proxy);

    let (tx_exit, rx_exit) = unbounded_channel();
    std::thread::spawn(move || {
        super::server::start_ipc(rx_exit);
    });

    let mut app = match WhiteboardApplication::new(&event_loop) {
        Ok(app) => app,
        Err(e) => {
            log::error!("Failed to create whiteboard application: {}", e);
            tx_exit.send(()).ok();
            return;
        }
    };

    if let Err(e) = event_loop.run_app(&mut app) {
        log::error!("Failed to run app: {}", e);
        tx_exit.send(()).ok();
        return;
    }
}

struct WindowState {
    window: Arc<Window>,
    // NOTE: This surface must be dropped before the `Window`.
    surface: Surface<DisplayHandle<'static>, Arc<Window>>,
    ripples: Vec<Ripple>,
    last_cursors: HashMap<String, Cursor>,
    shapes: HashMap<String, Vec<super::Shape>>,
    origin: (f32, f32),
    bounding: BoundingShape,
}

struct WhiteboardApplication {
    windows: Vec<WindowState>,
    // Drawing context.
    //
    // With OpenGL it could be EGLDisplay.
    context: Option<Context<DisplayHandle<'static>>>,
    face: Option<Face<'static>>,
    close_requested: bool,
}

impl WhiteboardApplication {
    fn new<T>(event_loop: &EventLoop<T>) -> ResultType<Self> {
        // https://github.com/rust-windowing/winit/blob/f6893a4390dfe6118ce4b33458d458fd3efd3025/examples/window.rs#L91
        // SAFETY: we drop the context right before the event loop is stopped, thus making it safe.
        let context = match Context::new(unsafe {
            std::mem::transmute::<DisplayHandle<'_>, DisplayHandle<'static>>(
                event_loop.display_handle()?,
            )
        }) {
            Ok(ctx) => Some(ctx),
            Err(e) => {
                bail!("Failed to create context: {}", e);
            }
        };
        let face = match create_font_face() {
            Ok(face) => Some(face),
            Err(err) => {
                log::error!("Failed to create font face: {}", err);
                None
            }
        };
        Ok(Self {
            windows: Vec::new(),
            context,
            face,
            close_requested: false,
        })
    }
}

impl ApplicationHandler<(String, CustomEvent)> for WhiteboardApplication {
    fn user_event(&mut self, _event_loop: &ActiveEventLoop, (k, evt): (String, CustomEvent)) {
        match evt {
            CustomEvent::Cursor(mut cursor) => {
                if let Some(state) = self.windows.first_mut() {
                    cursor.x -= state.origin.0;
                    cursor.y -= state.origin.1;
                    if cursor.btns != 0 {
                        state.ripples.push(Ripple {
                            x: cursor.x,
                            y: cursor.y,
                            start_time: Instant::now(),
                        });
                    }
                    state.last_cursors.insert(k, cursor);
                    state.window.request_redraw();
                }
            }
            CustomEvent::Shape(mut shape) => {
                if let Some(state) = self.windows.first_mut() {
                    shape.x1 -= state.origin.0;
                    shape.y1 -= state.origin.1;
                    shape.x2 -= state.origin.0;
                    shape.y2 -= state.origin.1;
                    for (i, p) in shape.points.iter_mut().enumerate() {
                        *p -= if i % 2 == 0 {
                            state.origin.0
                        } else {
                            state.origin.1
                        };
                    }
                    let v = state.shapes.entry(k).or_default();
                    match (shape.id != 0)
                        .then(|| v.iter_mut().find(|s| s.id == shape.id))
                        .flatten()
                    {
                        Some(existing) => *existing = shape,
                        None => v.push(shape),
                    }
                    state.window.request_redraw();
                }
            }
            CustomEvent::Undo => {
                if let Some(state) = self.windows.first_mut() {
                    state.shapes.get_mut(&k).map(|v| v.pop());
                    state.window.request_redraw();
                }
            }
            CustomEvent::Clear => {
                if let Some(state) = self.windows.first_mut() {
                    state.shapes.remove(&k);
                    state.last_cursors.remove(&k);
                    state.window.request_redraw();
                }
            }
            CustomEvent::Exit => {
                self.close_requested = true;
            }
        }
    }

    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        let (x, y, w, h) = match super::server::get_displays_rect() {
            Ok(r) => r,
            Err(err) => {
                log::error!("Failed to get displays rect: {}", err);
                self.close_requested = true;
                return;
            }
        };

        let window_attributes = Window::default_attributes()
            .with_title("RustDesk whiteboard")
            .with_inner_size(PhysicalSize::new(w, h))
            .with_position(PhysicalPosition::new(x, y))
            .with_decorations(false)
            .with_transparent(true)
            .with_window_level(WindowLevel::AlwaysOnTop)
            .with_x11_window_type(vec![WindowType::Dock])
            .with_override_redirect(true);

        let window = match event_loop.create_window(window_attributes) {
            Ok(window) => Arc::new(window),
            Err(e) => {
                log::error!("Failed to create window: {}", e);
                self.close_requested = true;
                return;
            }
        };

        let display = match window.display_handle() {
            Ok(d) => d,
            Err(e) => {
                log::error!("Failed to get display handle: {}", e);
                self.close_requested = true;
                return;
            }
        };
        let rwh = match window.window_handle() {
            Ok(w) => w,
            Err(e) => {
                log::error!("Failed to get window handle: {}", e);
                self.close_requested = true;
                return;
            }
        };

        // Both the following block and `window.set_cursor_hittest(false)` in `draw()` are necessary to ensure cursor events are properly passed through the window.
        // These issues may be related to winit X11 handling.
        // https://github.com/rust-windowing/winit/issues/3509
        // https://github.com/rust-windowing/winit/issues/4120
        // If either block is removed, cursor events may not be passed through as expected.
        // If you update winit, please revisit this workaround.
        let mut bounding = None;
        match (rwh.as_raw(), display.as_raw()) {
            (RawWindowHandle::Xlib(xlib_window), RawDisplayHandle::Xlib(xlib_display)) => {
                unsafe {
                    let xwindow = xlib_window.window;
                    if let Some(display_ptr) = xlib_display.display {
                        let xdisplay = display_ptr.as_ptr() as *mut Display;
                        let compositor_selection = crate::platform::is_x11().then(|| {
                            let name = CString::new(format!("_NET_WM_CM_S{}", xlib_display.screen))
                                .unwrap_or_default();
                            XInternAtom(xdisplay, name.as_ptr(), 0)
                        });
                        bounding = Some(BoundingShape {
                            display: xdisplay,
                            window: xwindow,
                            compositor_selection,
                            applied: None,
                        });
                        // Mouse event passthrough
                        let empty_region = XFixesCreateRegion(xdisplay, std::ptr::null_mut(), 0);
                        if empty_region == 0 {
                            log::error!("XFixesCreateRegion failed: returned null region");
                        } else {
                            XFixesSetWindowShapeRegion(
                                xdisplay,
                                xwindow,
                                SHAPE_INPUT,
                                0,
                                0,
                                empty_region,
                            );
                            XFixesDestroyRegion(xdisplay, empty_region);
                        }
                    }
                }
            }
            _ => {
                log::error!("Unsupported windowing system for shape extension");
                self.close_requested = true;
                return;
            }
        }
        let Some(bounding) = bounding else {
            log::error!("No X11 display for the whiteboard window");
            self.close_requested = true;
            return;
        };

        let Some(ctx) = self.context.as_ref() else {
            // unreachable
            self.close_requested = true;
            return;
        };

        let surface = match Surface::new(ctx, window.clone()) {
            Ok(s) => s,
            Err(e) => {
                log::error!("Failed to create surface: {}", e);
                self.close_requested = true;
                return;
            }
        };

        let state = WindowState {
            window,
            surface,
            ripples: Vec::new(),
            last_cursors: HashMap::new(),
            shapes: HashMap::new(),
            origin: (x as f32, y as f32),
            bounding,
        };

        self.windows.push(state);
    }

    fn window_event(
        &mut self,
        _event_loop: &ActiveEventLoop,
        window_id: WindowId,
        event: WindowEvent,
    ) {
        match event {
            WindowEvent::CloseRequested => {
                self.close_requested = true;
            }
            WindowEvent::RedrawRequested => {
                let Some(state) = self.windows.iter_mut().find(|w| w.window.id() == window_id)
                else {
                    log::error!("No window found for id: {:?}", window_id);
                    return;
                };
                if let Err(err) = state.draw(&self.face) {
                    log::error!("Failed to draw window: {}", err);
                }
            }
            _ => (),
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if !self.close_requested {
            for state in self.windows.iter().filter(|s| !s.ripples.is_empty()) {
                state.window.request_redraw();
            }
        } else {
            event_loop.exit();
        }
    }

    fn exiting(&mut self, _event_loop: &ActiveEventLoop) {
        // We must drop the context here.
        self.context = None;
    }
}

impl WindowState {
    fn draw(&mut self, face: &Option<Face<'static>>) -> ResultType<()> {
        let (width, height) = {
            let size = self.window.inner_size();
            (size.width, size.height)
        };

        let (Some(width), Some(height)) = (NonZeroU32::new(width), NonZeroU32::new(height)) else {
            bail!("Invalid window size, {width}x{height}")
        };
        if let Err(e) = self.surface.resize(width, height) {
            bail!("Failed to resize surface: {}", e);
        }

        let mut buffer = match self.surface.buffer_mut() {
            Ok(buf) => buf,
            Err(e) => {
                bail!("Failed to get buffer: {}", e);
            }
        };

        let Some(mut pixmap) = PixmapMut::from_bytes(
            bytemuck::cast_slice_mut(&mut buffer),
            width.get(),
            height.get(),
        ) else {
            bail!("Failed to create pixmap from buffer");
        };
        pixmap.fill(Color::TRANSPARENT);

        Ripple::retain_active(&mut self.ripples);
        for ripple in &self.ripples {
            let (radius, alpha) = ripple.get_radius_alpha();

            let mut ripple_paint = Paint::default();
            // Note: The real color is bgra here.
            ripple_paint.set_color_rgba8(64, 64, 255, (alpha * 128.0) as u8);
            ripple_paint.anti_alias = true;

            let mut ripple_pb = PathBuilder::new();
            ripple_pb.push_circle(ripple.x, ripple.y, radius);
            if let Some(path) = ripple_pb.finish() {
                pixmap.fill_path(
                    &path,
                    &ripple_paint,
                    FillRule::Winding,
                    Transform::identity(),
                    None,
                );
            }
        }

        super::win_linux::draw_shapes(&mut pixmap, &self.shapes);

        for cursor in self.last_cursors.values() {
            let (x, y) = (cursor.x, cursor.y);
            let size = 1.5f32;

            let mut pb = PathBuilder::new();
            pb.move_to(x, y);
            pb.line_to(x, y + 16.0 * size);
            pb.line_to(x + 4.0 * size, y + 13.0 * size);
            pb.line_to(x + 7.0 * size, y + 20.0 * size);
            pb.line_to(x + 9.0 * size, y + 19.0 * size);
            pb.line_to(x + 6.0 * size, y + 12.0 * size);
            pb.line_to(x + 11.0 * size, y + 12.0 * size);
            pb.close();

            if let Some(path) = pb.finish() {
                let mut arrow_paint = Paint::default();
                let rgba = super::argb_to_rgba(cursor.argb);
                arrow_paint.set_color_rgba8(rgba.2, rgba.1, rgba.0, rgba.3);
                arrow_paint.anti_alias = true;
                pixmap.fill_path(
                    &path,
                    &arrow_paint,
                    FillRule::Winding,
                    Transform::identity(),
                    None,
                );

                let mut black_paint = Paint::default();
                black_paint.set_color_rgba8(0, 0, 0, 255);
                black_paint.anti_alias = true;
                let mut stroke = Stroke::default();
                stroke.width = 1.0f32;
                pixmap.stroke_path(&path, &black_paint, &stroke, Transform::identity(), None);

                face.as_ref().map(|face| {
                    draw_text(
                        &mut pixmap,
                        face,
                        &cursor.text,
                        x + 24.0 * size,
                        y + 24.0 * size,
                        &arrow_paint,
                        14.0f32,
                    );
                });
            }
        }

        self.bounding.update(&mut buffer, width.get() as usize);
        self.window.pre_present_notify();

        if let Err(e) = buffer.present() {
            log::error!("Failed to present buffer: {}", e);
        }

        self.window.set_cursor_hittest(false).ok();

        Ok(())
    }
}
