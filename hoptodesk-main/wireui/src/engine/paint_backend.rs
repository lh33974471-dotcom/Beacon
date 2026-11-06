// Paint-backend seam. The engine paints through PaintScene, which is either a
// real vello::Scene (GPU, the default -- every argument forwards verbatim so
// GPU output is byte-identical to painting vello directly) or a recorded
// display list rasterized by cpu_raster (tiny-skia) on machines without a
// usable GPU adapter (Windows 7/8, GPU-less VMs, containers).
//
// The backend is decided once per process, BEFORE the first PaintScene is
// built: the window loop's Gpu::new decides from the real surface init
// (set_backend_cpu), and headless paths (tests, wireui-render) decide lazily
// via an adapter probe or WIREUI_FORCE_CPU=1.

use std::sync::atomic::{AtomicU8, Ordering};
use vello::kurbo::{self, Affine, PathEl, Rect, Shape};
use vello::peniko::{self, BlendMode, BrushRef, Color, Fill, Font};

const MODE_UNSET: u8 = 0;
const MODE_GPU: u8 = 1;
const MODE_CPU: u8 = 2;
static CPU_MODE: AtomicU8 = AtomicU8::new(MODE_UNSET);

thread_local! {
    static CPU_SCENE_OVERRIDE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

pub fn with_cpu_scene<R>(f: impl FnOnce() -> R) -> R {
    let previous = CPU_SCENE_OVERRIDE.with(|c| c.replace(true));
    let out = f();
    CPU_SCENE_OVERRIDE.with(|c| c.set(previous));
    out
}

fn cpu_scene_forced() -> bool {
    CPU_SCENE_OVERRIDE.with(|c| c.get())
}

const CONTENT_BLOCK: usize = 16;
const CONTENT_CONTRAST: u8 = 48;
const BLANK_CONTRAST: u8 = 24;

fn block_contrast(rgba: &[u8], width: u32, height: u32) -> Vec<u8> {
    let (w, h) = (width as usize, height as usize);
    if w == 0 || h == 0 || rgba.len() < w * h * 4 {
        return Vec::new();
    }
    let cols = (w + CONTENT_BLOCK - 1) / CONTENT_BLOCK;
    let rows = (h + CONTENT_BLOCK - 1) / CONTENT_BLOCK;
    let mut lo = vec![u8::MAX; cols * rows];
    let mut hi = vec![u8::MIN; cols * rows];
    for y in 0..h {
        let row = y / CONTENT_BLOCK * cols;
        let line = &rgba[y * w * 4..(y + 1) * w * 4];
        for (x, px) in line.chunks_exact(4).enumerate() {
            let b = row + x / CONTENT_BLOCK;
            for &c in &px[..3] {
                lo[b] = lo[b].min(c);
                hi[b] = hi[b].max(c);
            }
        }
    }
    lo.iter().zip(&hi).map(|(l, h)| h.saturating_sub(*l)).collect()
}

pub fn frame_has_content(rgba: &[u8], width: u32, height: u32) -> bool {
    block_contrast(rgba, width, height)
        .iter()
        .any(|&c| c >= CONTENT_CONTRAST)
}

pub struct ContentLoss {
    pub content: usize,
    pub missing: usize,
}

impl ContentLoss {
    pub fn is_broken(&self) -> bool {
        self.content > 0 && self.missing * 2 > self.content
    }
}

pub fn content_loss(reference: &[u8], candidate: &[u8], width: u32, height: u32) -> ContentLoss {
    let expected = block_contrast(reference, width, height);
    let actual = block_contrast(candidate, width, height);
    let content = expected.iter().filter(|&&c| c >= CONTENT_CONTRAST).count();
    let missing = expected
        .iter()
        .zip(&actual)
        .filter(|(&e, &a)| e >= CONTENT_CONTRAST && a < BLANK_CONTRAST)
        .count();
    ContentLoss { content, missing }
}

// Authoritative: the window loop calls this from Gpu init before any painting.
pub fn set_backend_cpu(cpu: bool) {
    let mode = if cpu { MODE_CPU } else { MODE_GPU };
    CPU_MODE.store(mode, Ordering::Relaxed);
}

pub fn backend_is_cpu() -> bool {
    match CPU_MODE.load(Ordering::Relaxed) {
        MODE_GPU => false,
        MODE_CPU => true,
        _ => {
            let cpu = decide_backend_headless();
            let mode = if cpu { MODE_CPU } else { MODE_GPU };
            let _ = CPU_MODE.compare_exchange(
                MODE_UNSET,
                mode,
                Ordering::Relaxed,
                Ordering::Relaxed,
            );
            CPU_MODE.load(Ordering::Relaxed) == MODE_CPU
        }
    }
}

fn decide_backend_headless() -> bool {
    if crate::cpu_backend_forced()
        || std::env::var("WIREUI_FORCE_CPU").map_or(false, |v| v == "1")
    {
        return true;
    }
    pollster::block_on(async {
        let instance = wgpu::Instance::default();
        let hp = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: None,
                force_fallback_adapter: false,
            })
            .await;
        if hp.is_some() {
            return false;
        }
        instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::LowPower,
                compatible_surface: None,
                force_fallback_adapter: true,
            })
            .await
            .is_none()
    })
}

pub struct PaintScene {
    pub(crate) inner: Inner,
}

pub(crate) enum Inner {
    Vello(vello::Scene),
    Cpu(DisplayList),
}

#[derive(Clone, Default)]
pub struct DisplayList {
    pub(crate) cmds: Vec<Cmd>,
}

#[derive(Clone)]
pub(crate) enum Cmd {
    Fill {
        rule: Fill,
        transform: Affine,
        brush: CpuBrush,
        path: tiny_skia::Path,
    },
    Stroke {
        stroke: kurbo::Stroke,
        transform: Affine,
        brush: CpuBrush,
        path: tiny_skia::Path,
    },
    // alpha 1.0 = pure clip; < 1.0 = an opacity group. clip None = degenerate
    // clip shape (everything inside is clipped out).
    PushLayer {
        alpha: f32,
        transform: Affine,
        clip: Option<tiny_skia::Path>,
    },
    PopLayer,
    Image {
        data: peniko::Blob<u8>,
        width: u32,
        height: u32,
        transform: Affine,
    },
    GlyphRun {
        font: Font,
        font_size: f32,
        transform: Affine,
        color: Color,
        glyphs: Vec<vello::Glyph>,
    },
    BlurredRect {
        transform: Affine,
        rect: Rect,
        color: Color,
        radius: f64,
        std_dev: f64,
    },
    Append {
        list: DisplayList,
        transform: Affine,
    },
}

#[derive(Clone)]
pub(crate) enum CpuBrush {
    Solid(Color),
    LinearGradient {
        start: kurbo::Point,
        end: kurbo::Point,
        stops: Vec<(f32, Color)>,
    },
}

fn to_cpu_brush(b: BrushRef) -> CpuBrush {
    match b {
        BrushRef::Solid(c) => CpuBrush::Solid(c),
        BrushRef::Gradient(g) => match g.kind {
            peniko::GradientKind::Linear { start, end } => CpuBrush::LinearGradient {
                start,
                end,
                stops: g.stops.iter().map(|s| (s.offset, s.color)).collect(),
            },
            // Radial/sweep are unused by the engine; first stop keeps it visible.
            _ => CpuBrush::Solid(
                g.stops.first().map(|s| s.color).unwrap_or(Color::BLACK),
            ),
        },
        BrushRef::Image(_) => CpuBrush::Solid(Color::TRANSPARENT),
    }
}

// kurbo shape -> tiny-skia path, converted once at record time. 0.1 tolerance
// matches the .to_path(0.1) calls the vello path uses for the same shapes.
pub(crate) fn shape_to_path(shape: &impl Shape) -> Option<tiny_skia::Path> {
    let mut pb = tiny_skia::PathBuilder::new();
    for el in shape.path_elements(0.1) {
        match el {
            PathEl::MoveTo(p) => pb.move_to(p.x as f32, p.y as f32),
            PathEl::LineTo(p) => pb.line_to(p.x as f32, p.y as f32),
            PathEl::QuadTo(p1, p2) => {
                pb.quad_to(p1.x as f32, p1.y as f32, p2.x as f32, p2.y as f32)
            }
            PathEl::CurveTo(p1, p2, p3) => pb.cubic_to(
                p1.x as f32,
                p1.y as f32,
                p2.x as f32,
                p2.y as f32,
                p3.x as f32,
                p3.y as f32,
            ),
            PathEl::ClosePath => pb.close(),
        }
    }
    pb.finish()
}

impl Default for PaintScene {
    fn default() -> Self {
        Self::new()
    }
}

impl PaintScene {
    pub fn new() -> Self {
        let inner = if cpu_scene_forced() || backend_is_cpu() {
            Inner::Cpu(DisplayList::default())
        } else {
            Inner::Vello(vello::Scene::new())
        };
        PaintScene { inner }
    }

    pub fn as_vello(&self) -> Option<&vello::Scene> {
        match &self.inner {
            Inner::Vello(s) => Some(s),
            Inner::Cpu(_) => None,
        }
    }

    pub fn as_cpu(&self) -> Option<&DisplayList> {
        match &self.inner {
            Inner::Cpu(l) => Some(l),
            Inner::Vello(_) => None,
        }
    }

    pub fn fill<'b>(
        &mut self,
        style: Fill,
        transform: Affine,
        brush: impl Into<BrushRef<'b>>,
        brush_transform: Option<Affine>,
        shape: &impl Shape,
    ) {
        let brush = brush.into();
        match &mut self.inner {
            Inner::Vello(s) => s.fill(style, transform, brush, brush_transform, shape),
            Inner::Cpu(l) => {
                debug_assert!(brush_transform.is_none());
                if let Some(path) = shape_to_path(shape) {
                    l.cmds.push(Cmd::Fill {
                        rule: style,
                        transform,
                        brush: to_cpu_brush(brush),
                        path,
                    });
                }
            }
        }
    }

    pub fn stroke<'b>(
        &mut self,
        stroke: &kurbo::Stroke,
        transform: Affine,
        brush: impl Into<BrushRef<'b>>,
        brush_transform: Option<Affine>,
        shape: &impl Shape,
    ) {
        let brush = brush.into();
        match &mut self.inner {
            Inner::Vello(s) => s.stroke(stroke, transform, brush, brush_transform, shape),
            Inner::Cpu(l) => {
                debug_assert!(brush_transform.is_none());
                if let Some(path) = shape_to_path(shape) {
                    l.cmds.push(Cmd::Stroke {
                        stroke: stroke.clone(),
                        transform,
                        brush: to_cpu_brush(brush),
                        path,
                    });
                }
            }
        }
    }

    pub fn push_layer(
        &mut self,
        blend: impl Into<BlendMode>,
        alpha: f32,
        transform: Affine,
        clip: &impl Shape,
    ) {
        let blend = blend.into();
        match &mut self.inner {
            Inner::Vello(s) => s.push_layer(blend, alpha, transform, clip),
            Inner::Cpu(l) => l.cmds.push(Cmd::PushLayer {
                alpha,
                transform,
                clip: shape_to_path(clip),
            }),
        }
    }

    pub fn pop_layer(&mut self) {
        match &mut self.inner {
            Inner::Vello(s) => s.pop_layer(),
            Inner::Cpu(l) => l.cmds.push(Cmd::PopLayer),
        }
    }

    pub fn draw_image(&mut self, image: &peniko::Image, transform: Affine) {
        match &mut self.inner {
            Inner::Vello(s) => s.draw_image(image, transform),
            Inner::Cpu(l) => l.cmds.push(Cmd::Image {
                data: image.data.clone(),
                width: image.width,
                height: image.height,
                transform,
            }),
        }
    }

    pub fn draw_blurred_rounded_rect(
        &mut self,
        transform: Affine,
        rect: Rect,
        color: Color,
        radius: f64,
        std_dev: f64,
    ) {
        match &mut self.inner {
            Inner::Vello(s) => s.draw_blurred_rounded_rect(transform, rect, color, radius, std_dev),
            Inner::Cpu(l) => l.cmds.push(Cmd::BlurredRect {
                transform,
                rect,
                color,
                radius,
                std_dev,
            }),
        }
    }

    pub fn append(&mut self, other: &PaintScene, transform: Option<Affine>) {
        match (&mut self.inner, &other.inner) {
            (Inner::Vello(s), Inner::Vello(o)) => s.append(o, transform),
            (Inner::Cpu(l), Inner::Cpu(o)) => l.cmds.push(Cmd::Append {
                list: o.clone(),
                transform: transform.unwrap_or(Affine::IDENTITY),
            }),
            // Impossible per-process (the backend is fixed before any scene is
            // built); losing a subtree beats corrupting the scene in release.
            _ => debug_assert!(false, "mixed paint backends in append"),
        }
    }

    pub fn draw_glyphs(&mut self, font: &Font) -> DrawGlyphsBuilder<'_> {
        DrawGlyphsBuilder {
            scene: self,
            font: font.clone(),
            font_size: 16.0,
            transform: Affine::IDENTITY,
            brush: Color::BLACK,
        }
    }
}

pub struct DrawGlyphsBuilder<'a> {
    scene: &'a mut PaintScene,
    font: Font,
    font_size: f32,
    transform: Affine,
    brush: Color,
}

impl<'a> DrawGlyphsBuilder<'a> {
    pub fn font_size(mut self, size: f32) -> Self {
        self.font_size = size;
        self
    }

    pub fn transform(mut self, transform: Affine) -> Self {
        self.transform = transform;
        self
    }

    pub fn brush<'b>(mut self, brush: impl Into<BrushRef<'b>>) -> Self {
        if let BrushRef::Solid(c) = brush.into() {
            self.brush = c;
        }
        self
    }

    pub fn draw(self, style: Fill, glyphs: impl Iterator<Item = vello::Glyph>) {
        match &mut self.scene.inner {
            Inner::Vello(s) => {
                s.draw_glyphs(&self.font)
                    .font_size(self.font_size)
                    .transform(self.transform)
                    .brush(self.brush)
                    .draw(style, glyphs);
            }
            Inner::Cpu(l) => {
                let glyphs: Vec<vello::Glyph> = glyphs.collect();
                if !glyphs.is_empty() {
                    l.cmds.push(Cmd::GlyphRun {
                        font: self.font,
                        font_size: self.font_size,
                        transform: self.transform,
                        color: self.brush,
                        glyphs,
                    });
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{content_loss, frame_has_content};

    const W: u32 = 320;
    const H: u32 = 320;
    const INK: [u8; 3] = [30, 41, 59];
    const PAPER: [u8; 3] = [255, 255, 255];

    fn solid(w: u32, h: u32, rgb: [u8; 3]) -> Vec<u8> {
        let mut v = Vec::with_capacity((w * h * 4) as usize);
        for _ in 0..w * h {
            v.extend_from_slice(&[rgb[0], rgb[1], rgb[2], 255]);
        }
        v
    }

    fn fill(px: &mut [u8], x0: u32, y0: u32, x1: u32, y1: u32, rgb: [u8; 3]) {
        for y in y0..y1 {
            for x in x0..x1 {
                let i = ((y * W + x) * 4) as usize;
                px[i..i + 3].copy_from_slice(&rgb);
            }
        }
    }

    fn text_band(px: &mut [u8], y0: u32, y1: u32, shift: u32) {
        for y in (y0..y1).step_by(4) {
            fill(px, 8 + shift, y, 300 + shift, y + 2, INK);
        }
    }

    fn page(shift: u32) -> Vec<u8> {
        let mut px = solid(W, H, PAPER);
        text_band(&mut px, 8, 24, shift);
        text_band(&mut px, 96, 224, shift);
        text_band(&mut px, 296, 312, shift);
        px
    }

    #[test]
    fn solid_frame_has_no_content() {
        assert!(!frame_has_content(&solid(64, 64, [0, 0, 0]), 64, 64));
        assert!(!frame_has_content(&solid(64, 64, PAPER), 64, 64));
    }

    #[test]
    fn faint_noise_is_not_content() {
        let mut px = solid(64, 64, PAPER);
        for (i, v) in px.iter_mut().enumerate() {
            if i % 8 == 0 {
                *v = 250;
            }
        }
        assert!(!frame_has_content(&px, 64, 64));
    }

    #[test]
    fn a_page_has_content() {
        assert!(frame_has_content(&page(0), W, H));
    }

    #[test]
    fn the_same_page_loses_nothing() {
        let loss = content_loss(&page(0), &page(0), W, H);
        assert!(loss.content > 0);
        assert_eq!(loss.missing, 0);
        assert!(!loss.is_broken());
    }

    #[test]
    fn jittered_edges_lose_nothing() {
        let loss = content_loss(&page(0), &page(1), W, H);
        assert_eq!(loss.missing, 0);
        assert!(!loss.is_broken());
    }

    #[test]
    fn a_missing_middle_is_broken() {
        let mut gpu = page(0);
        fill(&mut gpu, 0, 96, W, 224, PAPER);
        let loss = content_loss(&page(0), &gpu, W, H);
        assert!(loss.missing * 2 > loss.content, "{}/{}", loss.missing, loss.content);
        assert!(loss.is_broken());
    }

    #[test]
    fn a_flat_frame_is_broken() {
        let loss = content_loss(&page(0), &solid(W, H, PAPER), W, H);
        assert_eq!(loss.missing, loss.content);
        assert!(loss.is_broken());
    }

    #[test]
    fn a_small_loss_is_not_broken() {
        let mut gpu = page(0);
        fill(&mut gpu, 0, 296, W, 312, PAPER);
        let loss = content_loss(&page(0), &gpu, W, H);
        assert!(loss.missing > 0);
        assert!(!loss.is_broken());
    }

    #[test]
    fn a_short_buffer_is_neither_content_nor_loss() {
        assert!(!frame_has_content(&[0u8; 8], 4, 4));
        let loss = content_loss(&page(0), &[0u8; 8], W, H);
        assert!(loss.content > 0);
        assert_eq!(loss.missing, 0);
        assert!(!loss.is_broken());
    }
}
