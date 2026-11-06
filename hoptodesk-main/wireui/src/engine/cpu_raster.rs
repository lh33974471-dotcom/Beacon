// CPU rasterizer for the recorded display list (tiny-skia). Consumes the
// PaintScene Cpu arm on machines without a usable GPU adapter.
//
// Matches the GPU path's model: white base, src-over compositing, coordinates
// already in device pixels. Anti-aliasing differs from vello by design.

use super::paint_backend::{Cmd, CpuBrush, DisplayList};
use skrifa::outline::{DrawSettings, OutlinePen};
use skrifa::MetadataProvider;
use std::collections::HashMap;
use tiny_skia::{FillRule, Mask, Paint, Pixmap, PixmapPaint, PixmapRef, Shader, Transform};
use vello::kurbo::{self, Affine};
use vello::peniko::{Blob, Color, Fill, Font};

pub fn rasterize(list: &DisplayList, width: u32, height: u32) -> Pixmap {
    let t0 = std::time::Instant::now();
    let w = width.clamp(1, 16384);
    let h = height.clamp(1, 16384);
    let mut pm = Pixmap::new(w, h).expect("pixmap");
    pm.fill(tiny_skia::Color::WHITE);
    let mut st = Raster {
        w,
        h,
        frames: Vec::new(),
    };
    st.run(list, Affine::IDENTITY, &mut pm);
    let ms = t0.elapsed().as_millis();
    if ms > 30 && std::env::var_os("WIREUI_PERF").is_some() {
        eprintln!("wireui-cpu: slow frame {}x{} {}ms", w, h, ms);
    }
    pm
}

enum Frame {
    Layer(Layer),
    Noop,
}

struct Layer {
    target: Pixmap,
    mask: Option<Mask>,
    alpha: f32,
    ox: i32,
    oy: i32,
}

struct Raster {
    w: u32,
    h: u32,
    frames: Vec<Frame>,
}

pub(crate) fn ts_transform(a: Affine) -> Transform {
    let c = a.as_coeffs();
    Transform::from_row(
        c[0] as f32,
        c[1] as f32,
        c[2] as f32,
        c[3] as f32,
        c[4] as f32,
        c[5] as f32,
    )
}

fn ts_color(c: Color) -> tiny_skia::Color {
    tiny_skia::Color::from_rgba8(c.r, c.g, c.b, c.a)
}

fn brush_shader(brush: &CpuBrush) -> Option<Shader<'static>> {
    match brush {
        CpuBrush::Solid(c) => Some(Shader::SolidColor(ts_color(*c))),
        CpuBrush::LinearGradient { start, end, stops } => tiny_skia::LinearGradient::new(
            tiny_skia::Point::from_xy(start.x as f32, start.y as f32),
            tiny_skia::Point::from_xy(end.x as f32, end.y as f32),
            stops
                .iter()
                .map(|(o, c)| tiny_skia::GradientStop::new(*o, ts_color(*c)))
                .collect(),
            tiny_skia::SpreadMode::Pad,
            Transform::identity(),
        ),
    }
}

fn ts_stroke(s: &kurbo::Stroke) -> tiny_skia::Stroke {
    let cap = |c: kurbo::Cap| match c {
        kurbo::Cap::Butt => tiny_skia::LineCap::Butt,
        kurbo::Cap::Square => tiny_skia::LineCap::Square,
        kurbo::Cap::Round => tiny_skia::LineCap::Round,
    };
    tiny_skia::Stroke {
        width: s.width as f32,
        miter_limit: s.miter_limit as f32,
        line_cap: cap(s.start_cap),
        line_join: match s.join {
            kurbo::Join::Bevel => tiny_skia::LineJoin::Bevel,
            kurbo::Join::Miter => tiny_skia::LineJoin::Miter,
            kurbo::Join::Round => tiny_skia::LineJoin::Round,
        },
        ..Default::default()
    }
}

fn ts_rule(f: Fill) -> FillRule {
    match f {
        Fill::NonZero => FillRule::Winding,
        Fill::EvenOdd => FillRule::EvenOdd,
    }
}

// Images arrive as straight-alpha RGBA (peniko Format::Rgba8); tiny-skia wants
// premultiplied. Fully-opaque data is valid premultiplied data as-is (zero
// copy); anything with real alpha is converted once and cached by blob id.
enum ImgEntry {
    Opaque,
    Premul(std::sync::Arc<Vec<u8>>),
}

thread_local! {
    static IMG_CACHE: std::cell::RefCell<HashMap<u64, ImgEntry>> =
        std::cell::RefCell::new(HashMap::new());
}

fn premul_entry(data: &Blob<u8>, w: u32, h: u32) -> Option<ImgEntry> {
    let bytes = data.data();
    if bytes.len() < (w * h * 4) as usize {
        return None;
    }
    let bytes = &bytes[..(w * h * 4) as usize];
    if bytes.chunks_exact(4).all(|p| p[3] == 255) {
        return Some(ImgEntry::Opaque);
    }
    let mut out = Vec::with_capacity(bytes.len());
    for p in bytes.chunks_exact(4) {
        let a = p[3] as u32;
        out.push(((p[0] as u32 * a + 127) / 255) as u8);
        out.push(((p[1] as u32 * a + 127) / 255) as u8);
        out.push(((p[2] as u32 * a + 127) / 255) as u8);
        out.push(p[3]);
    }
    Some(ImgEntry::Premul(std::sync::Arc::new(out)))
}

impl Raster {
    fn run(&mut self, list: &DisplayList, ctm: Affine, root: &mut Pixmap) {
        for cmd in &list.cmds {
            match cmd {
                Cmd::Fill {
                    rule,
                    transform,
                    brush,
                    path,
                } => {
                    let shader = match brush_shader(brush) {
                        Some(s) => s,
                        None => continue,
                    };
                    let paint = Paint {
                        shader,
                        anti_alias: true,
                        ..Default::default()
                    };
                    let (mut target, ox, oy) = split(&mut self.frames, root);
                    let t = shifted(ox, oy, ctm * *transform);
                    target.fill_path(path, &paint, ts_rule(*rule), t, None);
                }
                Cmd::Stroke {
                    stroke,
                    transform,
                    brush,
                    path,
                } => {
                    let shader = match brush_shader(brush) {
                        Some(s) => s,
                        None => continue,
                    };
                    let paint = Paint {
                        shader,
                        anti_alias: true,
                        ..Default::default()
                    };
                    let (mut target, ox, oy) = split(&mut self.frames, root);
                    let t = shifted(ox, oy, ctm * *transform);
                    target.stroke_path(path, &paint, &ts_stroke(stroke), t, None);
                }
                Cmd::PushLayer {
                    alpha,
                    transform,
                    clip,
                } => {
                    self.push_layer(*alpha, ctm * *transform, clip.as_ref());
                }
                Cmd::PopLayer => match self.frames.pop() {
                    Some(Frame::Layer(mut l)) => {
                        if let Some(m) = &l.mask {
                            l.target.apply_mask(m);
                        }
                        let paint = PixmapPaint {
                            opacity: l.alpha,
                            ..Default::default()
                        };
                        let (mut target, pox, poy) = split(&mut self.frames, root);
                        target.draw_pixmap(
                            l.ox - pox,
                            l.oy - poy,
                            l.target.as_ref(),
                            &paint,
                            Transform::identity(),
                            None,
                        );
                    }
                    _ => {}
                },
                Cmd::Image {
                    data,
                    width,
                    height,
                    transform,
                } => {
                    self.draw_image(root, data, *width, *height, ctm * *transform);
                }
                Cmd::GlyphRun {
                    font,
                    font_size,
                    transform,
                    color,
                    glyphs,
                } => {
                    self.glyph_run(root, ctm * *transform, font, *font_size, *color, glyphs);
                }
                Cmd::BlurredRect {
                    transform,
                    rect,
                    color,
                    radius,
                    std_dev,
                } => {
                    self.blurred_rect(root, ctm * *transform, *rect, *color, *radius, *std_dev);
                }
                Cmd::Append { list, transform } => {
                    self.run(list, ctm * *transform, root);
                }
            }
        }
    }

    fn push_layer(&mut self, alpha: f32, t: Affine, clip: Option<&tiny_skia::Path>) {
        let (px0, py0, pw, ph) = self.surface_bounds();
        let path = match clip {
            Some(p) => p,
            None => {
                self.frames.push(dead_layer(alpha));
                return;
            }
        };
        let b = path.bounds();
        let dev = t.transform_rect_bbox(kurbo::Rect::new(
            b.left() as f64,
            b.top() as f64,
            b.right() as f64,
            b.bottom() as f64,
        ));
        let sharp = axis_aligned(t) && rect_path(path) && near_integer(&dev);
        let (x0, y0, x1, y1) = if sharp {
            (
                dev.x0.round() as i64,
                dev.y0.round() as i64,
                dev.x1.round() as i64,
                dev.y1.round() as i64,
            )
        } else {
            (
                dev.x0.floor() as i64,
                dev.y0.floor() as i64,
                dev.x1.ceil() as i64,
                dev.y1.ceil() as i64,
            )
        };
        let x0 = x0.max(px0 as i64);
        let y0 = y0.max(py0 as i64);
        let x1 = x1.min(px0 as i64 + pw as i64);
        let y1 = y1.min(py0 as i64 + ph as i64);
        if x1 <= x0 || y1 <= y0 {
            self.frames.push(dead_layer(alpha));
            return;
        }
        let (bw, bh) = ((x1 - x0) as u32, (y1 - y0) as u32);
        if sharp && alpha >= 1.0 && x0 == px0 as i64 && y0 == py0 as i64 && bw == pw && bh == ph {
            self.frames.push(Frame::Noop);
            return;
        }
        let mask = if sharp {
            None
        } else {
            let mut m = Mask::new(bw, bh).expect("mask");
            m.fill_path(
                path,
                FillRule::Winding,
                true,
                shifted(x0 as i32, y0 as i32, t),
            );
            Some(m)
        };
        self.frames.push(Frame::Layer(Layer {
            target: Pixmap::new(bw, bh).expect("layer"),
            mask,
            alpha,
            ox: x0 as i32,
            oy: y0 as i32,
        }));
    }

    fn surface_bounds(&self) -> (i32, i32, u32, u32) {
        for f in self.frames.iter().rev() {
            if let Frame::Layer(l) = f {
                return (l.ox, l.oy, l.target.width(), l.target.height());
            }
        }
        (0, 0, self.w, self.h)
    }

    fn draw_image(&mut self, root: &mut Pixmap, data: &Blob<u8>, w: u32, h: u32, t: Affine) {
        if w == 0 || h == 0 {
            return;
        }
        // Ok(None) = draw straight from the blob (opaque); Ok(Some) = use the
        // cached premultiplied copy; Err = undersized/invalid blob.
        let premul: Result<Option<std::sync::Arc<Vec<u8>>>, ()> = IMG_CACHE.with(|c| {
            let mut c = c.borrow_mut();
            if c.len() > 64 {
                c.clear();
            }
            if !c.contains_key(&data.id()) {
                match premul_entry(data, w, h) {
                    Some(e) => {
                        c.insert(data.id(), e);
                    }
                    None => return Err(()),
                }
            }
            match c.get(&data.id()) {
                Some(ImgEntry::Premul(p)) => Ok(Some(p.clone())),
                _ => Ok(None),
            }
        });
        let premul = match premul {
            Ok(p) => p,
            Err(()) => return,
        };
        let bytes: &[u8] = match &premul {
            Some(p) => &p[..],
            None => &data.data()[..(w * h * 4) as usize],
        };
        let src = match PixmapRef::from_bytes(bytes, w, h) {
            Some(s) => s,
            None => return,
        };
        let paint = PixmapPaint {
            quality: tiny_skia::FilterQuality::Bilinear,
            ..Default::default()
        };
        let (mut target, ox, oy) = split(&mut self.frames, root);
        target.draw_pixmap(0, 0, src, &paint, shifted(ox, oy, t), None);
    }

    // Box-shadow: rasterize the rounded rect into a small scratch pixmap and
    // approximate the Gaussian with three box-blur passes.
    fn blurred_rect(
        &mut self,
        root: &mut Pixmap,
        t: Affine,
        rect: kurbo::Rect,
        color: Color,
        radius: f64,
        std_dev: f64,
    ) {
        let shape = kurbo::RoundedRect::from_rect(rect, radius);
        if std_dev <= 0.1 {
            let path = match super::paint_backend::shape_to_path(&shape) {
                Some(p) => p,
                None => return,
            };
            let paint = Paint {
                shader: Shader::SolidColor(ts_color(color)),
                anti_alias: true,
                ..Default::default()
            };
            let (mut target, ox, oy) = split(&mut self.frames, root);
            target.fill_path(&path, &paint, FillRule::Winding, shifted(ox, oy, t), None);
            return;
        }
        let scale = (t.as_coeffs()[0].abs() + t.as_coeffs()[3].abs()) / 2.0;
        let sigma = (std_dev * scale.max(0.01)).min(100.0);
        let pad = (sigma * 3.0).ceil();
        let dev = t.transform_rect_bbox(rect);
        let x0 = (dev.x0 - pad).floor().max(-(self.w as f64)) as i32;
        let y0 = (dev.y0 - pad).floor().max(-(self.h as f64)) as i32;
        let x1 = (dev.x1 + pad).ceil().min(2.0 * self.w as f64) as i32;
        let y1 = (dev.y1 + pad).ceil().min(2.0 * self.h as f64) as i32;
        let (tw, th) = ((x1 - x0).max(1) as u32, (y1 - y0).max(1) as u32);
        let mut tmp = match Pixmap::new(tw, th) {
            Some(p) => p,
            None => return,
        };
        let path = match super::paint_backend::shape_to_path(&shape) {
            Some(p) => p,
            None => return,
        };
        let paint = Paint {
            shader: Shader::SolidColor(ts_color(color)),
            anti_alias: true,
            ..Default::default()
        };
        let local = Affine::translate((-(x0 as f64), -(y0 as f64))) * t;
        tmp.as_mut()
            .fill_path(&path, &paint, FillRule::Winding, ts_transform(local), None);
        box_blur_3(tmp.data_mut(), tw as usize, th as usize, sigma);
        let (mut target, ox, oy) = split(&mut self.frames, root);
        target.draw_pixmap(
            x0 - ox,
            y0 - oy,
            tmp.as_ref(),
            &PixmapPaint::default(),
            Transform::identity(),
            None,
        );
    }
}

// -------- glyphs (skrifa outlines -> cached alpha masks) --------

#[derive(Hash, PartialEq, Eq, Clone)]
struct GlyphKey {
    blob_id: u64,
    index: u32,
    glyph: u32,
    ppem_bits: u32,
    subpix: u8, // quarter-pixel x bin 0..4
}

struct GlyphMask {
    w: u32,
    h: u32,
    left: i32, // placement relative to the floored device glyph origin
    top: i32,
    alpha: Vec<u8>,
}

thread_local! {
    static GLYPH_CACHE: std::cell::RefCell<HashMap<GlyphKey, Option<GlyphMask>>> =
        std::cell::RefCell::new(HashMap::new());
}

// Accumulates a glyph outline as a tiny-skia path in y-down pixel space
// (skrifa outlines are y-up; vello applies the same flip on the GPU path).
struct TsPen {
    pb: tiny_skia::PathBuilder,
}

impl OutlinePen for TsPen {
    fn move_to(&mut self, x: f32, y: f32) {
        self.pb.move_to(x, -y);
    }
    fn line_to(&mut self, x: f32, y: f32) {
        self.pb.line_to(x, -y);
    }
    fn quad_to(&mut self, cx0: f32, cy0: f32, x: f32, y: f32) {
        self.pb.quad_to(cx0, -cy0, x, -y);
    }
    fn curve_to(&mut self, cx0: f32, cy0: f32, cx1: f32, cy1: f32, x: f32, y: f32) {
        self.pb.cubic_to(cx0, -cy0, cx1, -cy1, x, -y);
    }
    fn close(&mut self) {
        self.pb.close();
    }
}

fn glyph_path(font: &Font, glyph: u32, ppem: f32) -> Option<tiny_skia::Path> {
    let fr = skrifa::FontRef::from_index(font.data.as_ref(), font.index).ok()?;
    let og = fr.outline_glyphs().get(skrifa::GlyphId::new(glyph))?;
    let mut pen = TsPen {
        pb: tiny_skia::PathBuilder::new(),
    };
    og.draw(
        DrawSettings::unhinted(
            skrifa::instance::Size::new(ppem),
            skrifa::instance::LocationRef::default(),
        ),
        &mut pen,
    )
    .ok()?;
    pen.pb.finish()
}

fn build_glyph_mask(font: &Font, glyph: u32, ppem: f32, subpix: u8) -> Option<GlyphMask> {
    let path = glyph_path(font, glyph, ppem)?;
    let b = path.bounds();
    let fx = subpix as f32 / 4.0;
    let left = (b.left() + fx).floor() as i32;
    let top = b.top().floor() as i32;
    let w = (((b.right() + fx).ceil() as i32 - left).max(1) as u32) + 1;
    let h = ((b.bottom().ceil() as i32 - top).max(1) as u32) + 1;
    if w > 1024 || h > 1024 {
        return None;
    }
    let mut mask = Mask::new(w, h)?;
    mask.fill_path(
        &path,
        FillRule::Winding,
        true,
        Transform::from_translate(fx - left as f32, -(top as f32)),
    );
    Some(GlyphMask {
        w,
        h,
        left,
        top,
        alpha: mask.data().to_vec(),
    })
}

impl Raster {
    fn glyph_run(
        &mut self,
        root: &mut Pixmap,
        dev: Affine,
        font: &Font,
        font_size: f32,
        color: Color,
        glyphs: &[vello::Glyph],
    ) {
        let c = dev.as_coeffs();
        let axis_aligned =
            c[1].abs() < 1e-6 && c[2].abs() < 1e-6 && (c[0] - c[3]).abs() < 1e-6 && c[0] > 0.0;
        if !axis_aligned {
            // Rare (CSS-transformed text): fill each outline directly with the
            // full transform, uncached. The recorded path is y-down at
            // font_size ppem, so the y-flip is already folded in.
            let paint = Paint {
                shader: Shader::SolidColor(ts_color(color)),
                anti_alias: true,
                ..Default::default()
            };
            for g in glyphs {
                if let Some(path) = glyph_path(font, g.id, font_size) {
                    let t = dev * Affine::translate((g.x as f64, g.y as f64));
                    let (mut target, ox, oy) = split(&mut self.frames, root);
                    target.fill_path(&path, &paint, FillRule::Winding, shifted(ox, oy, t), None);
                }
            }
            return;
        }
        let s = c[0];
        let ppem = font_size * s as f32;
        if ppem <= 0.1 {
            return;
        }
        let ca = color.a as u32;
        let pr = (color.r as u32 * ca + 127) / 255;
        let pg = (color.g as u32 * ca + 127) / 255;
        let pb = (color.b as u32 * ca + 127) / 255;
        GLYPH_CACHE.with(|cache| {
            let mut cache = cache.borrow_mut();
            if cache.len() > 4096 {
                cache.clear();
            }
            for g in glyphs {
                let p = dev * kurbo::Point::new(g.x as f64, g.y as f64);
                let mut ix = p.x.floor() as i64;
                let mut bin = ((p.x - p.x.floor()) * 4.0).round() as u8;
                if bin == 4 {
                    bin = 0;
                    ix += 1;
                }
                let iy = p.y.round() as i64;
                let key = GlyphKey {
                    blob_id: font.data.id(),
                    index: font.index,
                    glyph: g.id,
                    ppem_bits: ppem.to_bits(),
                    subpix: bin,
                };
                let gm = cache
                    .entry(key)
                    .or_insert_with(|| build_glyph_mask(font, g.id, ppem, bin));
                if let Some(gm) = gm {
                    let (mut target, ox, oy) = split(&mut self.frames, root);
                    composite_mask(
                        &mut target,
                        gm,
                        ix + gm.left as i64 - ox as i64,
                        iy + gm.top as i64 - oy as i64,
                        pr,
                        pg,
                        pb,
                        ca,
                    );
                }
            }
        });
    }
}

#[allow(clippy::too_many_arguments)]
fn composite_mask(
    target: &mut tiny_skia::PixmapMut,
    gm: &GlyphMask,
    ox: i64,
    oy: i64,
    pr: u32,
    pg: u32,
    pb: u32,
    ca: u32,
) {
    let tw = target.width() as i64;
    let th = target.height() as i64;
    let pixels = target.pixels_mut();
    for my in 0..gm.h as i64 {
        let dy = oy + my;
        if dy < 0 || dy >= th {
            continue;
        }
        for mx in 0..gm.w as i64 {
            let dx = ox + mx;
            if dx < 0 || dx >= tw {
                continue;
            }
            let cov = gm.alpha[(my as u32 * gm.w + mx as u32) as usize] as u32;
            if cov == 0 {
                continue;
            }
            let di = (dy * tw + dx) as usize;
            let ea = (ca * cov + 127) / 255;
            let d = pixels[di];
            let inv = 255 - ea;
            let r = ((pr * cov + 127) / 255 + (d.red() as u32 * inv + 127) / 255).min(255) as u8;
            let g = ((pg * cov + 127) / 255 + (d.green() as u32 * inv + 127) / 255).min(255) as u8;
            let b = ((pb * cov + 127) / 255 + (d.blue() as u32 * inv + 127) / 255).min(255) as u8;
            let a = (ea + (d.alpha() as u32 * inv + 127) / 255).min(255) as u8;
            if let Some(px) =
                tiny_skia::PremultipliedColorU8::from_rgba(r.min(a), g.min(a), b.min(a), a)
            {
                pixels[di] = px;
            }
        }
    }
}

fn split<'a>(
    frames: &'a mut [Frame],
    root: &'a mut Pixmap,
) -> (tiny_skia::PixmapMut<'a>, i32, i32) {
    for f in frames.iter_mut().rev() {
        if let Frame::Layer(l) = f {
            let (ox, oy) = (l.ox, l.oy);
            return (l.target.as_mut(), ox, oy);
        }
    }
    (root.as_mut(), 0, 0)
}

fn shifted(ox: i32, oy: i32, t: Affine) -> Transform {
    ts_transform(Affine::translate((-(ox as f64), -(oy as f64))) * t)
}

fn dead_layer(alpha: f32) -> Frame {
    Frame::Layer(Layer {
        target: Pixmap::new(1, 1).expect("layer"),
        mask: Some(Mask::new(1, 1).expect("mask")),
        alpha,
        ox: 0,
        oy: 0,
    })
}

fn axis_aligned(t: Affine) -> bool {
    let c = t.as_coeffs();
    c[1].abs() < 1e-6 && c[2].abs() < 1e-6
}

fn near_integer(r: &kurbo::Rect) -> bool {
    [r.x0, r.y0, r.x1, r.y1]
        .iter()
        .all(|v| (v - v.round()).abs() < 1e-3)
}

fn rect_path(path: &tiny_skia::Path) -> bool {
    let mut pts = [(0.0f32, 0.0f32); 5];
    let mut n = 0;
    for seg in path.segments() {
        match seg {
            tiny_skia::PathSegment::MoveTo(p) => {
                if n != 0 {
                    return false;
                }
                pts[0] = (p.x, p.y);
                n = 1;
            }
            tiny_skia::PathSegment::LineTo(p) => {
                if n == 0 || n >= 5 {
                    return false;
                }
                pts[n] = (p.x, p.y);
                n += 1;
            }
            tiny_skia::PathSegment::Close => {}
            _ => return false,
        }
    }
    if n == 5 {
        if (pts[4].0 - pts[0].0).abs() > 1e-4 || (pts[4].1 - pts[0].1).abs() > 1e-4 {
            return false;
        }
        n = 4;
    }
    if n != 4 {
        return false;
    }
    for i in 0..4 {
        let a = pts[i];
        let b = pts[(i + 1) % 4];
        let same_x = (a.0 - b.0).abs() < 1e-4;
        let same_y = (a.1 - b.1).abs() < 1e-4;
        if same_x == same_y {
            return false;
        }
    }
    true
}

// Three box blurs approximate a Gaussian (Kovesi). Operates on premultiplied
// RGBA in place, blurring all four channels.
fn box_blur_3(data: &mut [u8], w: usize, h: usize, sigma: f64) {
    let n = 3.0f64;
    let wi = (12.0 * sigma * sigma / n + 1.0).sqrt().floor();
    let wl = if wi as i64 % 2 == 0 { wi - 1.0 } else { wi };
    let wl = wl.max(1.0);
    let wu = wl + 2.0;
    let m = ((12.0 * sigma * sigma - n * wl * wl - 4.0 * n * wl - 3.0 * n)
        / (-4.0 * wl - 4.0))
        .round();
    let mut buf = vec![0u8; data.len()];
    for i in 0..3 {
        let box_w = if (i as f64) < m { wl as usize } else { wu as usize };
        let r = box_w / 2;
        box_pass(data, &mut buf, w, h, r, true);
        box_pass(&buf, data, w, h, r, false);
    }
}

fn box_pass(src: &[u8], dst: &mut [u8], w: usize, h: usize, r: usize, horizontal: bool) {
    let (len, lines) = if horizontal { (w, h) } else { (h, w) };
    let idx = |line: usize, i: usize| -> usize {
        if horizontal {
            (line * w + i) * 4
        } else {
            (i * w + line) * 4
        }
    };
    let norm = (2 * r + 1) as u32;
    for line in 0..lines {
        let mut sum = [0u32; 4];
        for i in 0..=(r.min(len - 1)) {
            let p = idx(line, i);
            for c in 0..4 {
                sum[c] += src[p + c] as u32;
            }
        }
        // Edge policy: outside = 0 (transparent), correct for a shadow fading out.
        for i in 0..len {
            let p = idx(line, i);
            for c in 0..4 {
                dst[p + c] = (sum[c] / norm).min(255) as u8;
            }
            if i + r + 1 < len {
                let a = idx(line, i + r + 1);
                for c in 0..4 {
                    sum[c] += src[a + c] as u32;
                }
            }
            if i >= r {
                let s = idx(line, i - r);
                for c in 0..4 {
                    sum[c] -= src[s + c] as u32;
                }
            }
        }
    }
}
