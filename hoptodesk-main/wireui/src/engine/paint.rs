use super::dom::{Document, NodeKey};
use super::layout::LayoutResult;
use super::style::{Background, Computed, Rgba};
use std::collections::HashMap;
use super::paint_backend::PaintScene as Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Shape, Vec2};
use vello::peniko::{self, Color, Fill};

fn to_color(c: Rgba) -> Color {
    Color::rgba(c.r as f64, c.g as f64, c.b as f64, c.a as f64)
}

const SCROLLBAR_TRACK_ALPHA: f64 = 0.10;
const SCROLLBAR_THUMB_ALPHA: f64 = 0.45;

pub fn paint_document(
    doc: &Document,
    styles: &HashMap<NodeKey, Computed>,
    layout: &LayoutResult,
    scale: f32,
) -> Scene {
    paint_document_with_video(doc, styles, layout, scale, &HashMap::new())
}

pub fn paint_document_with_video(
    doc: &Document,
    styles: &HashMap<NodeKey, Computed>,
    layout: &LayoutResult,
    scale: f32,
    video_sinks: &HashMap<NodeKey, crate::video::FrameSink>,
) -> Scene {
    paint_document_full(doc, styles, layout, scale, video_sinks, &HashMap::new(), 0.0)
}

pub fn paint_document_full(
    doc: &Document,
    styles: &HashMap<NodeKey, Computed>,
    layout: &LayoutResult,
    scale: f32,
    video_sinks: &HashMap<NodeKey, crate::video::FrameSink>,
    pseudo: &HashMap<NodeKey, Vec<super::style::PseudoBox>>,
    now_ms: f64,
) -> Scene {
    paint_document_overlaid(
        doc, styles, layout, scale, video_sinks, pseudo, now_ms, &HashMap::new(),
    )
}

#[allow(clippy::too_many_arguments)]
pub fn paint_document_overlaid(
    doc: &Document,
    styles: &HashMap<NodeKey, Computed>,
    layout: &LayoutResult,
    scale: f32,
    video_sinks: &HashMap<NodeKey, crate::video::FrameSink>,
    pseudo: &HashMap<NodeKey, Vec<super::style::PseudoBox>>,
    now_ms: f64,
    overlays: &HashMap<NodeKey, Vec<super::host::DrawCmd>>,
) -> Scene {
    if let Ok(mut slot) = CARET_DEVICE_RECT.lock() {
        *slot = None;
    }
    let mut scene = Scene::new();
    let ctx = PaintCtx {
        video_sinks,
        pseudo,
        now_ms,
        overlays,
        unboxed: subtrees_escaping_their_box(doc, styles),
    };
    // Positioned subtrees with a z-index are deferred out of the normal flow
    // and painted afterwards, lowest z first, mirroring
    // host::compute_screen_geometry so hit-testing matches the pixels.
    let mut deferred: Vec<Deferred> = Vec::new();
    let mut seq = 0usize;
    paint_node(
        &mut scene, doc, styles, layout, doc.root, scale, &ctx, 0.0, 0.0, &mut deferred, &mut seq,
        None,
    );
    while !deferred.is_empty() {
        let mut idx = 0;
        for (i, d) in deferred.iter().enumerate() {
            if (d.z, d.seq) < (deferred[idx].z, deferred[idx].seq) {
                idx = i;
            }
        }
        let d = deferred.remove(idx);
        paint_node(
            &mut scene, doc, styles, layout, d.key, scale, &ctx, d.offset_x, d.offset_y,
            &mut deferred, &mut seq, None,
        );
    }
    scene
}

// Caret blink pauses (drawn solid) after a few seconds without user input so
// an idle focused input stops forcing periodic full repaints; the window loop
// owns the timing and flips this before painting.
pub static CARET_SOLID: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub static CARET_DEVICE_RECT: std::sync::Mutex<Option<(f64, f64, f64, f64)>> =
    std::sync::Mutex::new(None);

struct PaintCtx<'a> {
    video_sinks: &'a HashMap<NodeKey, crate::video::FrameSink>,
    pseudo: &'a HashMap<NodeKey, Vec<super::style::PseudoBox>>,
    now_ms: f64,
    overlays: &'a HashMap<NodeKey, Vec<super::host::DrawCmd>>,
    unboxed: std::collections::HashSet<NodeKey>,
}

fn subtrees_escaping_their_box(
    doc: &Document,
    styles: &HashMap<NodeKey, Computed>,
) -> std::collections::HashSet<NodeKey> {
    let mut out = std::collections::HashSet::new();
    for (key, style) in styles.iter() {
        if matches!(style.position, super::style::Position::Static) {
            continue;
        }
        let mut cur = doc.arena.get(*key).and_then(|n| n.parent);
        while let Some(p) = cur {
            if !out.insert(p) {
                break;
            }
            cur = doc.arena.get(p).and_then(|n| n.parent);
        }
    }
    out
}

struct Deferred {
    z: i32,
    seq: usize,
    key: NodeKey,
    offset_x: f32,
    offset_y: f32,
}

#[allow(clippy::too_many_arguments)]
fn paint_node(
    scene: &mut Scene,
    doc: &Document,
    styles: &HashMap<NodeKey, Computed>,
    layout: &LayoutResult,
    key: NodeKey,
    scale: f32,
    ctx: &PaintCtx,
    offset_x: f32,
    offset_y: f32,
    deferred: &mut Vec<Deferred>,
    seq: &mut usize,
    cull: Option<(f32, f32, f32, f32)>,
) {
    let node = match doc.arena.get(key) {
        Some(n) => n,
        None => return,
    };
    let style = styles.get(&key).cloned().unwrap_or_default();
    if !style.visible || style.display == super::style::DisplayKind::None {
        return;
    }
    let raw = match layout.rects.get(&key) {
        Some(r) => *r,
        None => return,
    };
    // Apply any accumulated ancestor scroll offset to this node's position.
    let rect = (raw.0 - offset_x, raw.1 - offset_y, raw.2, raw.3);
    let s = scale as f64;
    let device_rect = Rect::new(
        rect.0 as f64 * s,
        rect.1 as f64 * s,
        (rect.0 + rect.2) as f64 * s,
        (rect.1 + rect.3) as f64 * s,
    );
    let radius = if style.border_radius_pct > 0.0 {
        (rect.2.min(rect.3) as f64) * (style.border_radius_pct as f64 / 100.0) * s
    } else {
        style.border_radius as f64 * s
    };

    let pushed_layer = if style.opacity < 1.0 {
        scene.push_layer(
            peniko::Mix::Normal,
            style.opacity,
            Affine::IDENTITY,
            &device_rect,
        );
        true
    } else {
        false
    };

    if let Some(shadow) = &style.box_shadow {
        let shadow_rect = device_rect
            + Vec2::new(shadow.dx as f64 * s, shadow.dy as f64 * s);
        scene.draw_blurred_rounded_rect(
            Affine::IDENTITY,
            shadow_rect,
            to_color(shadow.color),
            radius,
            shadow.blur as f64 * s / 2.0,
        );
    }

    let rounded = RoundedRect::from_rect(device_rect, radius);
    match &style.background {
        Background::None => {}
        Background::Color(c) => {
            if c.a > 0.0 {
                scene.fill(Fill::NonZero, Affine::IDENTITY, to_color(*c), None, &rounded);
            }
        }
        Background::LinearGradient { angle_deg, stops } => {
            if stops.len() >= 2 {
                let rad = (*angle_deg as f64 - 90.0).to_radians();
                let (cx, cy) = (device_rect.center().x, device_rect.center().y);
                let half = (device_rect.width().hypot(device_rect.height())) / 2.0;
                let dir = Vec2::new(rad.cos(), rad.sin());
                let start = Point::new(cx - dir.x * half, cy - dir.y * half);
                let end = Point::new(cx + dir.x * half, cy + dir.y * half);
                let color_stops: Vec<peniko::ColorStop> = stops
                    .iter()
                    .enumerate()
                    .map(|(i, c)| peniko::ColorStop {
                        offset: i as f32 / (stops.len() - 1) as f32,
                        color: to_color(*c),
                    })
                    .collect();
                let gradient = peniko::Gradient::new_linear(start, end)
                    .with_stops(color_stops.as_slice());
                scene.fill(
                    Fill::NonZero,
                    Affine::IDENTITY,
                    &peniko::Brush::Gradient(gradient),
                    None,
                    &rounded,
                );
            }
        }
    }

    if let Some(src) = &style.bg_image {
        if let Some((iw, ih, blob)) = decode_bg_image(src) {
            paint_bg_image(scene, device_rect, radius, iw, ih, blob, &style, s, src);
        }
    }

    // <img src="data:..."> stretches the image over its styled box (the 2FA QR
    // code). Reuses the bg-image pipeline with an explicit box-filling size.
    if node.tag == "img" && style.bg_image.is_none() {
        if let Some(src) = node.attr("src") {
            if let Some((iw, ih, blob)) = decode_bg_image(src) {
                let mut img_style = style.clone();
                img_style.bg_size = super::style::BgSize::Length(
                    Some((device_rect.width() / s) as f32),
                    Some((device_rect.height() / s) as f32),
                );
                img_style.bg_pos = (super::style::BgAxis::Start, super::style::BgAxis::Start);
                paint_bg_image(scene, device_rect, radius, iw, ih, blob, &img_style, s, src);
            }
        }
    }

    if style.border_width.iter().any(|w| *w > 0.0) && style.border_color.a > 0.0 {
        let bw = style.border_width; // [top, right, bottom, left]
        let uniform = bw[0] == bw[1] && bw[1] == bw[2] && bw[2] == bw[3];
        let col = to_color(style.border_color);
        if uniform {
            let w = bw[0] as f64 * s;
            let inset = device_rect.inset(-w / 2.0);
            let border_shape = RoundedRect::from_rect(inset, (radius - w / 2.0).max(0.0));
            let stroke = vello::kurbo::Stroke::new(w);
            scene.stroke(&stroke, Affine::IDENTITY, col, None, &border_shape);
        } else {
            // Per-side borders (e.g. the minimize glyph's border-bottom): draw
            // each present edge as a filled strip.
            let (x0, y0, x1, y1) = (device_rect.x0, device_rect.y0, device_rect.x1, device_rect.y1);
            let f = |scene: &mut Scene, r: Rect| {
                scene.fill(Fill::NonZero, Affine::IDENTITY, col, None, &r);
            };
            if bw[0] > 0.0 {
                f(scene, Rect::new(x0, y0, x1, y0 + bw[0] as f64 * s));
            }
            if bw[2] > 0.0 {
                f(scene, Rect::new(x0, y1 - bw[2] as f64 * s, x1, y1));
            }
            if bw[3] > 0.0 {
                f(scene, Rect::new(x0, y0, x0 + bw[3] as f64 * s, y1));
            }
            if bw[1] > 0.0 {
                f(scene, Rect::new(x1 - bw[1] as f64 * s, y0, x1, y1));
            }
        }
    }

    if style.outline_width > 0.0 && style.outline_color.a > 0.0 {
        let w = style.outline_width as f64 * s;
        let ring = RoundedRect::from_rect(device_rect.inset(w / 2.0), radius);
        let stroke = vello::kurbo::Stroke::new(w);
        scene.stroke(
            &stroke,
            Affine::IDENTITY,
            to_color(style.outline_color),
            None,
            &ring,
        );
    }

    if style.checkbox {
        let box_sz = 14.0 * s;
        let stroke_w = 1.5 * s;
        let cx = device_rect.x0;
        let cy = device_rect.y0 + (device_rect.height() - box_sz) / 2.0;
        let bx = Rect::new(cx, cy, cx + box_sz, cy + box_sz).inset(-stroke_w / 2.0);
        let br = RoundedRect::from_rect(bx, 3.0 * s - stroke_w / 2.0);
        let tick = style.color;
        let tick_is_light =
            0.2126 * tick.r + 0.7152 * tick.g + 0.0722 * tick.b > 0.5 && tick.a > 0.0;
        let fill = if tick_is_light {
            Color::rgb8(0x0f, 0x17, 0x2a)
        } else {
            Color::WHITE
        };
        scene.fill(Fill::NonZero, Affine::IDENTITY, fill, None, &br);
        let border = Color::rgb8(0x94, 0xa3, 0xb8);
        scene.stroke(
            &vello::kurbo::Stroke::new(stroke_w),
            Affine::IDENTITY,
            border,
            None,
            &br,
        );
        if style.checked {
            let mut check = vello::kurbo::BezPath::new();
            check.move_to((cx + box_sz * 0.24, cy + box_sz * 0.52));
            check.line_to((cx + box_sz * 0.44, cy + box_sz * 0.72));
            check.line_to((cx + box_sz * 0.78, cy + box_sz * 0.30));
            scene.stroke(
                &vello::kurbo::Stroke::new(2.0 * s),
                Affine::IDENTITY,
                to_color(style.color),
                None,
                &check,
            );
        }
    }

    if node.tag == "input" && node.attr("type") == Some("hslider") {
        let attr_f = |name: &str, fallback: f32| {
            node.attr(name)
                .and_then(|v| v.trim().parse::<f32>().ok())
                .unwrap_or(fallback)
        };
        let min = attr_f("min", 0.0);
        let max = attr_f("max", 100.0).max(min + 1.0);
        let val = attr_f("value", min).clamp(min, max);
        let frac = ((val - min) / (max - min)) as f64;
        let knob_r = 8.0 * s;
        let track_h = 4.0 * s;
        let tx0 = device_rect.x0 + knob_r;
        let tx1 = (device_rect.x1 - knob_r).max(tx0 + 1.0);
        let cy = (device_rect.y0 + device_rect.y1) / 2.0;
        let text_is_light = 0.2126 * style.color.r + 0.7152 * style.color.g
            + 0.0722 * style.color.b
            > 0.5
            && style.color.a > 0.0;
        let (track, knob_fill) = if text_is_light {
            (Color::rgb8(0x47, 0x55, 0x69), Color::rgb8(0xe2, 0xe8, 0xf0))
        } else {
            (Color::rgb8(0xd7, 0xdb, 0xe0), Color::WHITE)
        };
        let tr = RoundedRect::new(tx0, cy - track_h / 2.0, tx1, cy + track_h / 2.0, track_h / 2.0);
        scene.fill(Fill::NonZero, Affine::IDENTITY, track, None, &tr);
        let kx = tx0 + (tx1 - tx0) * frac;
        let knob = vello::kurbo::Circle::new((kx, cy), knob_r);
        scene.fill(Fill::NonZero, Affine::IDENTITY, knob_fill, None, &knob);
        scene.stroke(
            &vello::kurbo::Stroke::new(1.0 * s),
            Affine::IDENTITY,
            Color::rgb8(0x94, 0xa3, 0xb8),
            None,
            &knob,
        );
    }

    let clip = radius > 0.0
        && node.tag != "html"
        && rect.2 > 0.0
        && rect.3 > 0.0
        && !matches!(style.width, super::style::Length::Star(_));
    if clip {
        scene.push_layer(peniko::Mix::Normal, 1.0, Affine::IDENTITY, &rounded.to_path(0.1));
    }

    // A scroll container clips its content to its own box and shifts children up
    // by its scroll_top; an overflow:hidden box (e.g. a folder-view <td>) clips its
    // content too so a long file name doesn't spill into the next column.
    let scroll_clip = (style.scroll_y || style.overflow_hidden) && rect.2 > 0.0 && rect.3 > 0.0;
    if scroll_clip {
        scene.push_layer(peniko::Mix::Normal, 1.0, Affine::IDENTITY, &device_rect);
    }
    let child_cull = if scroll_clip {
        let margin_x = rect.2.max(64.0);
        let margin_y = rect.3.max(64.0);
        let own = (
            rect.0 - margin_x,
            rect.1 - margin_y,
            rect.0 + rect.2 + margin_x,
            rect.1 + rect.3 + margin_y,
        );
        Some(match cull {
            Some(c) => (c.0.max(own.0), c.1.max(own.1), c.2.min(own.2), c.3.min(own.3)),
            None => own,
        })
    } else {
        cull
    };
    let scroll_carrier = style.scroll_y || style.overflow_hidden;
    let child_offset = offset_y + if scroll_carrier { node.scroll_top } else { 0.0 };
    // A scroll container also pans horizontally when script scrolls it (scrollTo);
    // additive, so content that never scrolls is unaffected (offset stays 0).
    let child_offset_x = offset_x + if scroll_carrier { node.scroll_left } else { 0.0 };

    if let Some(boxes) = ctx.pseudo.get(&key) {
        for b in boxes {
            let bx = device_rect.x0 + b.left as f64 * s;
            let by = device_rect.y0 + b.top as f64 * s;
            let brect = Rect::new(bx, by, bx + b.width as f64 * s, by + b.height as f64 * s);
            let br = if b.border_radius_pct > 0.0 {
                (b.width.min(b.height) as f64) * (b.border_radius_pct as f64 / 100.0) * s
            } else {
                b.border_radius as f64 * s
            };
            if let Some(c) = b.background {
                let col = Color::rgba(
                    c.r as f64,
                    c.g as f64,
                    c.b as f64,
                    (c.a * b.opacity) as f64,
                );
                scene.fill(
                    Fill::NonZero,
                    Affine::IDENTITY,
                    col,
                    None,
                    &RoundedRect::from_rect(brect, br),
                );
            }
        }
    }

    if node.tag == "video" {
        paint_video(scene, ctx, key, device_rect);
    }
    if node.tag == "svg" {
        // The graphic draws in the CONTENT box (inset by padding), not the padded
        // box -- otherwise a padded icon like the card kebab (size:1em; padding:.3em)
        // scales its glyph up to fill the whole box.
        let px = |l: super::style::Length| match l {
            super::style::Length::Px(v) => v,
            _ => 0.0,
        };
        let (pt, pr, pb, pl) = (
            px(style.padding[0]),
            px(style.padding[1]),
            px(style.padding[2]),
            px(style.padding[3]),
        );
        let content = (rect.0 + pl, rect.1 + pt, rect.2 - pl - pr, rect.3 - pt - pb);
        super::svg::paint_svg(scene, doc, key, content, style.color, s);
    } else if node.tag == "progress" {
        let radius = device_rect.height() / 2.0;
        let color = to_color(style.color);
        if node.attr("value").is_some() {
            // Determinate: fill proportional to value/max.
            let val: f64 = node.attr("value").and_then(|v| v.parse().ok()).unwrap_or(0.0);
            let max: f64 = node.attr("max").and_then(|v| v.parse().ok()).unwrap_or(1.0);
            let frac = if max > 0.0 { (val / max).clamp(0.0, 1.0) } else { 0.0 };
            let mut fill = device_rect;
            fill.x1 = device_rect.x0 + device_rect.width() * frac;
            scene.fill(Fill::NonZero, Affine::IDENTITY, color, None, &RoundedRect::from_rect(fill, radius));
        } else {
            // Indeterminate: a short segment sweeps across the track, matching the
            // Sciter "connecting" animation. Driven by now_ms so it advances each frame.
            let track_w = device_rect.width();
            let seg_w = (track_w * 0.35).max(1.0);
            let period = 1400.0;
            let phase = ((ctx.now_ms % period) / period) as f64;
            let travel = track_w + seg_w;
            let left = device_rect.x0 + phase * travel - seg_w;
            let x0 = left.max(device_rect.x0);
            let x1 = (left + seg_w).min(device_rect.x1);
            if x1 > x0 {
                let seg = Rect::new(x0, device_rect.y0, x1, device_rect.y1);
                scene.fill(Fill::NonZero, Affine::IDENTITY, color, None, &RoundedRect::from_rect(seg, radius));
            }
        }
    } else if style.behavior.as_deref() == Some("shell-icon") {
        let type_n: i64 = node.attr("type").and_then(|v| v.parse().ok()).unwrap_or(5);
        let is_folder = type_n <= 3;
        let target = (16.0 * s).round().max(1.0) as u32;
        let icon = node
            .attr("filename")
            .filter(|f| !f.is_empty())
            .and_then(|f| super::shell_icon::icon_image(f, is_folder, target));
        match icon {
            Some(img) => {
                let x0 = (device_rect.center().x - img.width as f64 / 2.0).round();
                let y0 = (device_rect.center().y - img.height as f64 / 2.0).round();
                scene.draw_image(&img, Affine::translate((x0, y0)));
            }
            None => paint_shell_icon(scene, device_rect, is_folder, s),
        }
    } else if node.tag == "textarea" {
        // Multi-line value/placeholder, top-left anchored inside the padding.
        let pad = super::host::TEXTAREA_PAD as f64 * s;
        let (display, _) = super::layout::input_display_text(node, Some(&style));
        let has_value =
            node.attr("value").map_or(false, |v| !v.is_empty()) || !node.preedit.is_empty();
        let n_chars = display.chars().count();
        let preedit_chars = node.preedit.chars().count();
        let caret_ci = if preedit_chars > 0 {
            (node.caret + node.preedit_caret.min(preedit_chars)).min(n_chars)
        } else {
            node.caret.min(n_chars)
        };
        let editing = node.states.focus && has_value;
        let geometry = if editing {
            layout
                .text_layouts
                .get(&key)
                .and_then(|run| super::host::caret_geometry(&run.layout, &display, caret_ci))
        } else {
            None
        };
        let scroll_y = super::host::textarea_scroll_y(node.scroll_top);
        let ox = device_rect.x0 + pad;
        let oy = device_rect.y0 + pad - scroll_y * s;
        if let Some(run) = layout.text_layouts.get(&key) {
            draw_text(
                scene,
                run,
                (ox, oy),
                s,
                style.underline,
                None,
                Some([style.color.r, style.color.g, style.color.b, style.color.a]),
            );
        }
        if editing {
            if let (Some(anchor), Some(run)) = (node.sel_anchor, layout.text_layouts.get(&key)) {
                let a = anchor.min(n_chars);
                super::host::selection_rects(&run.layout, &display, a, caret_ci, |x0, y0, x1, y1| {
                    let sel = Rect::new(
                        ox + x0 as f64 * s,
                        oy + y0 as f64 * s,
                        ox + x1 as f64 * s,
                        oy + y1 as f64 * s,
                    );
                    scene.fill(
                        Fill::NonZero,
                        Affine::IDENTITY,
                        peniko::Color::rgba(0.0, 0.48, 1.0, 0.3),
                        None,
                        &sel,
                    );
                });
            }
        }
        if node.states.focus {
            let fallback_h = style.font_size as f64 * 1.2 * s;
            let (cx, cy, ch) = match geometry {
                Some((x, y, h)) => (ox + x as f64 * s, oy + y as f64 * s, h as f64 * s),
                None => (ox, device_rect.y0 + pad, fallback_h),
            };
            let caret = Rect::new(cx, cy, cx + (1.0 * s).max(1.0), cy + ch);
            if let Ok(mut slot) = CARET_DEVICE_RECT.lock() {
                *slot = Some((caret.x0, caret.y0, caret.x1, caret.y1));
            }
            let blink_on = CARET_SOLID.load(std::sync::atomic::Ordering::Relaxed)
                || (ctx.now_ms / 530.0) as i64 % 2 == 0;
            if blink_on {
                scene.fill(Fill::NonZero, Affine::IDENTITY, to_color(style.color), None, &caret);
            }
        }
    } else if node.tag == "input" || node.tag == "select" {
        let pad_left = super::host::input_pad_left(Some(&style)) as f64 * s;
        // A long value must not paint past the control (the file-transfer path
        // box was drawing over the folder/trash icons to the window edge):
        // truncate with an ellipsis inside the box, leaving the select's
        // chevron area clear.
        let right_reserve = if node.tag == "select" {
            super::layout::SELECT_CHEVRON_WIDTH as f64
        } else {
            6.0
        };
        let avail_w = ((device_rect.width() - pad_left - right_reserve * s) / s).max(8.0) as f32;
        let focused_edit = node.states.focus
            && (node.tag == "input"
                || (node.tag == "select" && node.attr("editable").is_some()));
        let mut scroll_x = 0.0f64;
        if focused_edit {
            if let Some(run) = layout.text_layouts.get(&key) {
                let (display, _) = super::layout::input_display_text(node, Some(&style));
                let has_value =
                    node.attr("value").map_or(false, |v| !v.is_empty()) || !node.preedit.is_empty();
                if has_value {
                    let n_chars = display.chars().count();
                    let preedit_chars = node.preedit.chars().count();
                    let caret_ci = if preedit_chars > 0 {
                        (node.caret + node.preedit_caret.min(preedit_chars)).min(n_chars)
                    } else {
                        node.caret.min(n_chars)
                    };
                    let byte = display
                        .char_indices()
                        .nth(caret_ci)
                        .map(|(b, _)| b)
                        .unwrap_or(display.len());
                    let caret_x = super::host::boundary_x(&run.layout, &display, byte) as f64;
                    scroll_x = (caret_x - (avail_w as f64 - 2.0)).max(0.0);
                }
            }
        }
        if focused_edit {
            let clip = Rect::new(
                device_rect.x0 + (pad_left - 1.0 * s).max(0.0),
                device_rect.y0,
                device_rect.x1 - right_reserve * s,
                device_rect.y1,
            );
            scene.push_layer(peniko::Mix::Normal, 1.0, Affine::IDENTITY, &clip);
        }
        let mut text_w = 0.0;
        if let Some(run) = layout.text_layouts.get(&key) {
            let text_h = run.layout.height() as f64 * s;
            let ty = match run.layout.lines().next().map(|l| *l.metrics()) {
                Some(m) if m.ascent > 0.0 => {
                    let ink_h = (m.ascent + m.descent) as f64 * s;
                    let ink_top = (m.baseline - m.ascent) as f64 * s;
                    device_rect.y0 + (device_rect.height() - ink_h) / 2.0 - ink_top
                }
                _ => device_rect.y0 + (device_rect.height() - text_h) / 2.0,
            };
            draw_text(
                scene,
                run,
                (device_rect.x0 + pad_left - scroll_x * s, ty.max(device_rect.y0)),
                s,
                style.underline,
                if focused_edit { None } else { Some(avail_w) },
                Some([style.color.r, style.color.g, style.color.b, style.color.a]),
            );
            text_w = (run.layout.width() as f64 * s).min(avail_w as f64 * s);
        }
        // Caret at its character position + selection highlight for a focused
        // input (or editable select -- the file-transfer path box); falls back
        // to the text end when there is no run.
        if focused_edit {
            let (display, _) = super::layout::input_display_text(node, Some(&style));
            let has_value =
                node.attr("value").map_or(false, |v| !v.is_empty()) || !node.preedit.is_empty();
            let x_of = |ci: usize| -> f64 {
                if !has_value {
                    return 0.0;
                }
                if let Some(run) = layout.text_layouts.get(&key) {
                    let byte = display
                        .char_indices()
                        .nth(ci)
                        .map(|(b, _)| b)
                        .unwrap_or(display.len());
                    (super::host::boundary_x(&run.layout, &display, byte) as f64 - scroll_x) * s
                } else {
                    text_w
                }
            };
            let n_chars = display.chars().count();
            let preedit_chars = node.preedit.chars().count();
            let caret_ci = if preedit_chars > 0 {
                (node.caret + node.preedit_caret.min(preedit_chars)).min(n_chars)
            } else {
                node.caret.min(n_chars)
            };
            if preedit_chars > 0 {
                let (x0, x1) = (
                    x_of(node.caret.min(n_chars)),
                    x_of((node.caret + preedit_chars).min(n_chars)),
                );
                let right = device_rect.x1 - 2.0 * s;
                let lx0 = (device_rect.x0 + pad_left + x0).min(right);
                let lx1 = (device_rect.x0 + pad_left + x1).min(right);
                if lx1 > lx0 {
                    let cy = device_rect.center().y + style.font_size as f64 * 0.6 * s;
                    let line = Rect::new(lx0, cy, lx1, cy + (1.0 * s).max(1.0));
                    scene.fill(Fill::NonZero, Affine::IDENTITY, to_color(style.color), None, &line);
                }
            }
            if let Some(anchor) = node.sel_anchor {
                let a = anchor.min(n_chars);
                if a != caret_ci {
                    let (s0, s1) = (a.min(caret_ci), a.max(caret_ci));
                    let (x0, x1) = (x_of(s0), x_of(s1));
                    let ch = style.font_size as f64 * 1.3 * s;
                    let cy = device_rect.center().y;
                    let sel = Rect::new(
                        device_rect.x0 + pad_left + x0,
                        cy - ch / 2.0,
                        (device_rect.x0 + pad_left + x1).min(device_rect.x1 - 2.0 * s),
                        cy + ch / 2.0,
                    );
                    scene.fill(
                        Fill::NonZero,
                        Affine::IDENTITY,
                        peniko::Color::rgba(0.0, 0.48, 1.0, 0.3),
                        None,
                        &sel,
                    );
                }
            }
            let cx = (device_rect.x0 + pad_left + x_of(caret_ci))
                .min(device_rect.x1 - 2.0 * s);
            let ch = style.font_size as f64 * 1.2 * s;
            let cy = device_rect.center().y;
            let cw = (1.0 * s).max(1.0);
            let caret = Rect::new(cx, cy - ch / 2.0, cx + cw, cy + ch / 2.0);
            if let Ok(mut slot) = CARET_DEVICE_RECT.lock() {
                *slot = Some((caret.x0, caret.y0, caret.x1, caret.y1));
            }
            let blink_on = CARET_SOLID.load(std::sync::atomic::Ordering::Relaxed)
                || (ctx.now_ms / 530.0) as i64 % 2 == 0;
            if blink_on {
                scene.fill(Fill::NonZero, Affine::IDENTITY, to_color(style.color), None, &caret);
            }
            scene.pop_layer();
        }
        if node.tag == "select" {
            // Down chevron at the right edge.
            let cx = device_rect.x1 - 14.0 * s;
            let cy = device_rect.center().y;
            let w = 4.0 * s;
            let mut chev = vello::kurbo::BezPath::new();
            chev.move_to((cx - w, cy - w * 0.5));
            chev.line_to((cx, cy + w * 0.5));
            chev.line_to((cx + w, cy - w * 0.5));
            scene.stroke(
                &vello::kurbo::Stroke::new(1.5 * s),
                Affine::IDENTITY,
                Color::rgb8(0x6c, 0x75, 0x7d),
                None,
                &chev,
            );
        }
    } else if let Some(run) = layout.text_layouts.get(&key) {
        // text-overflow:ellipsis (the @ELLIPSIS mixin) truncates a nowrap line that
        // overflows the content box and paints a trailing ellipsis.
        let mut text_x = rect.0;
        let clip = if style.text_ellipsis {
            let px = |l: super::style::Length| match l {
                super::style::Length::Px(v) => v,
                _ => 0.0,
            };
            // taffy sizes the text run to its natural width, so its own rect can
            // never truncate it. The element holding it is what clips, so measure
            // the room from that box's content edge.
            let own_right = rect.0 + rect.2 - px(style.padding[1]);
            let parent_box = doc.arena.get(key).and_then(|n| n.parent).and_then(|p| {
                let pr = layout.rects.get(&p)?;
                let ps = styles.get(&p);
                let pad_l = ps.map_or(0.0, |ps| px(ps.padding[3]));
                let pad_r = ps.map_or(0.0, |ps| px(ps.padding[1]));
                Some((pr.0 + pad_l, pr.0 + pr.2 - pad_r))
            });
            let box_right = parent_box.map(|(_, r)| r).unwrap_or(own_right);
            if let Some((box_left, _)) = parent_box {
                if text_x < box_left - 0.5 {
                    text_x = box_left;
                }
            }
            Some((box_right.min(own_right) - text_x).max(0.0))
        } else {
            None
        };
        draw_text(
            scene,
            run,
            (text_x as f64 * s, rect.1 as f64 * s),
            s,
            style.underline,
            clip,
            Some([style.color.r, style.color.g, style.color.b, style.color.a]),
        );
    } else {
        for pass in 0..2 {
        for &child in &node.children {
            let child_abs = styles
                .get(&child)
                .map_or(false, |st| st.position == super::style::Position::Absolute);
            if (pass == 0) == child_abs {
                continue;
            }
            if let Some(z) = super::host::deferred_z(styles.get(&child)) {
                *seq += 1;
                deferred.push(Deferred {
                    z,
                    seq: *seq,
                    key: child,
                    offset_x: child_offset_x,
                    offset_y: child_offset,
                });
                continue;
            }
            // A CSS transform paints the child's subtree through an Affine
            // (translate/scale/rotate) rather than at its laid-out position.
            let xf = styles.get(&child).map(|s| s.transform.clone()).unwrap_or_default();
            if xf.is_empty() && !ctx.unboxed.contains(&child) {
                if let (Some(bounds), Some(craw)) = (child_cull, layout.rects.get(&child)) {
                    let cx = craw.0 - child_offset_x;
                    let cy = craw.1 - child_offset;
                    if cx + craw.2 < bounds.0
                        || cx > bounds.2
                        || cy + craw.3 < bounds.1
                        || cy > bounds.3
                    {
                        continue;
                    }
                }
            }
            if !xf.is_empty() {
                if let Some(craw) = layout.rects.get(&child) {
                    let crect = (craw.0 - child_offset_x, craw.1 - child_offset, craw.2, craw.3);
                    let cdev = Rect::new(
                        crect.0 as f64 * s,
                        crect.1 as f64 * s,
                        (crect.0 + crect.2) as f64 * s,
                        (crect.1 + crect.3) as f64 * s,
                    );
                    let affine = build_transform(&xf, cdev, crect.2, crect.3, s);
                    let mut sub = Scene::new();
                    paint_node(
                        &mut sub, doc, styles, layout, child, scale, ctx, child_offset_x,
                        child_offset, deferred, seq, None,
                    );
                    scene.append(&sub, Some(affine));
                    continue;
                }
            }
            paint_node(
                scene, doc, styles, layout, child, scale, ctx, child_offset_x, child_offset,
                deferred, seq, child_cull,
            );
        }
        }
    }

    if let Some(cmds) = ctx.overlays.get(&key) {
        paint_draw_cmds(scene, cmds, device_rect.x0, device_rect.y0, s);
    }

    if style.scroll_y {
        if let Some(bar) = super::host::scrollbar_geometry(
            doc,
            styles,
            &layout.rects,
            key,
            rect,
            node.scroll_top,
            layout.textarea_content_h.get(&key).copied(),
        ) {
            let device = |r: (f32, f32, f32, f32)| {
                Rect::new(
                    r.0 as f64 * s,
                    r.1 as f64 * s,
                    (r.0 + r.2) as f64 * s,
                    (r.1 + r.3) as f64 * s,
                )
            };
            let shade = |a: f64| {
                Color::rgba(style.color.r as f64, style.color.g as f64, style.color.b as f64, a)
            };
            scene.fill(
                Fill::NonZero,
                Affine::IDENTITY,
                shade(SCROLLBAR_TRACK_ALPHA),
                None,
                &device(bar.track),
            );
            let thumb = device(bar.thumb);
            scene.fill(
                Fill::NonZero,
                Affine::IDENTITY,
                shade(SCROLLBAR_THUMB_ALPHA),
                None,
                &RoundedRect::from_rect(thumb, thumb.width().min(thumb.height()) / 2.0),
            );
        }
    }

    if scroll_clip {
        scene.pop_layer();
    }
    if clip {
        scene.pop_layer();
    }
    if pushed_layer {
        scene.pop_layer();
    }
}

fn paint_draw_cmds(
    scene: &mut Scene,
    cmds: &[super::host::DrawCmd],
    ox: f64,
    oy: f64,
    s: f64,
) {
    use super::host::DrawCmd;
    let unpack = |c: u32| {
        Color::rgba8(
            (c & 0xFF) as u8,
            ((c >> 8) & 0xFF) as u8,
            ((c >> 16) & 0xFF) as u8,
            ((c >> 24) & 0xFF) as u8,
        )
    };
    for cmd in cmds {
        match *cmd {
            DrawCmd::Line { x1, y1, x2, y2, color, width } => {
                let mut p = vello::kurbo::BezPath::new();
                p.move_to((ox + x1 as f64 * s, oy + y1 as f64 * s));
                p.line_to((ox + x2 as f64 * s, oy + y2 as f64 * s));
                scene.stroke(
                    &vello::kurbo::Stroke::new(width as f64 * s),
                    Affine::IDENTITY,
                    unpack(color),
                    None,
                    &p,
                );
            }
            DrawCmd::Rect { l, t, r, b, color, width } => {
                let rect = Rect::new(
                    ox + l as f64 * s,
                    oy + t as f64 * s,
                    ox + r as f64 * s,
                    oy + b as f64 * s,
                );
                scene.stroke(
                    &vello::kurbo::Stroke::new(width as f64 * s),
                    Affine::IDENTITY,
                    unpack(color),
                    None,
                    &rect,
                );
            }
            DrawCmd::Ellipse { cx, cy, rx, ry, color, width } => {
                let el = vello::kurbo::Ellipse::new(
                    (ox + cx as f64 * s, oy + cy as f64 * s),
                    (rx as f64 * s, ry as f64 * s),
                    0.0,
                );
                scene.stroke(
                    &vello::kurbo::Stroke::new(width as f64 * s),
                    Affine::IDENTITY,
                    unpack(color),
                    None,
                    &el,
                );
            }
        }
    }
}

fn build_transform(
    ops: &[super::style::TransformOp],
    dev: Rect,
    w: f32,
    h: f32,
    s: f64,
) -> Affine {
    use super::style::{Length, TransformOp};
    let center = dev.center();
    let mut translate = Affine::IDENTITY;
    let mut centered = Affine::IDENTITY;
    for op in ops {
        match op {
            TransformOp::Translate(tx, ty) => {
                let px = |l: &Length, base: f32| match l {
                    Length::Px(v) => *v,
                    Length::Percent(p) => base * p / 100.0,
                    _ => 0.0,
                };
                translate *= Affine::translate((px(tx, w) as f64 * s, px(ty, h) as f64 * s));
            }
            TransformOp::Scale(sx, sy) => {
                centered *= Affine::scale_non_uniform(*sx as f64, *sy as f64);
            }
            TransformOp::Rotate(deg) => {
                centered *= Affine::rotate((*deg as f64).to_radians());
            }
        }
    }
    // Translate is absolute; scale/rotate pivot on the box center. Translate is
    // the outermost (leftmost in the CSS list), applied after the centered ops.
    translate
        * Affine::translate((center.x, center.y))
        * centered
        * Affine::translate((-center.x, -center.y))
}

// Per-sink scratch for the CPU backend: while the frame version is unchanged
// the SAME Blob is reused (stable id -> the rasterizer's opaque/premul cache
// hits, zero copies); a new frame is copied into the scratch Arc in place
// (Arc::get_mut succeeds once the previous frame's display list is dropped),
// so steady-state playback allocates nothing.
struct VideoScratch {
    version: u64,
    w: u32,
    h: u32,
    arc: std::sync::Arc<Vec<u8>>,
    blob: peniko::Blob<u8>,
}

thread_local! {
    static VIDEO_SCRATCH: std::cell::RefCell<HashMap<usize, VideoScratch>> =
        std::cell::RefCell::new(HashMap::new());
}

fn video_frame_cpu(sink: &crate::video::FrameSink) -> Option<(u32, u32, peniko::Blob<u8>)> {
    let sink_key = std::sync::Arc::as_ptr(sink) as usize;
    let f = sink.lock().unwrap();
    if f.version == 0 || f.width <= 0 || f.height <= 0 {
        return None;
    }
    let expected = (f.width * f.height * 4) as usize;
    if f.rgba.len() < expected {
        return None;
    }
    let (w, h) = (f.width as u32, f.height as u32);
    VIDEO_SCRATCH.with(|c| {
        let mut c = c.borrow_mut();
        if c.len() > 8 {
            c.clear();
        }
        if let Some(s) = c.get_mut(&sink_key) {
            if s.version == f.version && s.w == w && s.h == h {
                return Some((w, h, s.blob.clone()));
            }
            // Drop the old Blob handle first so the scratch Arc is unique
            // once the previous frame's display list is gone.
            s.blob = peniko::Blob::new(std::sync::Arc::new(Vec::new()));
            let arc = match std::sync::Arc::get_mut(&mut s.arc) {
                Some(buf) if buf.len() == expected => {
                    buf.copy_from_slice(&f.rgba[..expected]);
                    s.arc.clone()
                }
                _ => {
                    let arc = std::sync::Arc::new(f.rgba[..expected].to_vec());
                    s.arc = arc.clone();
                    arc
                }
            };
            s.version = f.version;
            s.w = w;
            s.h = h;
            s.blob = peniko::Blob::new(arc);
            return Some((w, h, s.blob.clone()));
        }
        let arc = std::sync::Arc::new(f.rgba[..expected].to_vec());
        let blob = peniko::Blob::new(arc.clone());
        c.insert(
            sink_key,
            VideoScratch {
                version: f.version,
                w,
                h,
                arc,
                blob: blob.clone(),
            },
        );
        Some((w, h, blob))
    })
}

fn paint_video(scene: &mut Scene, ctx: &PaintCtx, key: NodeKey, dest: Rect) {
    let sink = match ctx.video_sinks.get(&key) {
        Some(s) => s,
        None => return,
    };
    if super::paint_backend::backend_is_cpu() {
        if let Some((w, h, blob)) = video_frame_cpu(sink) {
            let image = peniko::Image::new(blob, peniko::Format::Rgba8, w, h);
            let (dw, dh) = (dest.width(), dest.height());
            let scale = (dw / w as f64).min(dh / h as f64);
            let (draw_w, draw_h) = (w as f64 * scale, h as f64 * scale);
            let ox = dest.x0 + (dw - draw_w) / 2.0;
            let oy = dest.y0 + (dh - draw_h) / 2.0;
            scene.draw_image(&image, Affine::translate((ox, oy)) * Affine::scale(scale));
        }
        return;
    }
    let (w, h, image) = {
        let mut f = sink.lock().unwrap();
        if f.version == 0 || f.width <= 0 || f.height <= 0 {
            return;
        }
        let expected = (f.width * f.height * 4) as usize;
        if f.rgba.len() < expected {
            return;
        }
        let (w, h) = (f.width as u32, f.height as u32);
        let cached = match &f.cached_image {
            Some((v, img)) if *v == f.version => Some(img.clone()),
            _ => None,
        };
        let image = match cached {
            Some(img) => img,
            None => {
                let blob = peniko::Blob::new(std::sync::Arc::new(f.rgba[..expected].to_vec()));
                let img = peniko::Image::new(blob, peniko::Format::Rgba8, w, h);
                f.cached_image = Some((f.version, img.clone()));
                img
            }
        };
        (w, h, image)
    };
    // letterbox: scale to fit the destination box preserving aspect
    let (dw, dh) = (dest.width(), dest.height());
    let scale = (dw / w as f64).min(dh / h as f64);
    let draw_w = w as f64 * scale;
    let draw_h = h as f64 * scale;
    let ox = dest.x0 + (dw - draw_w) / 2.0;
    let oy = dest.y0 + (dh - draw_h) / 2.0;
    let transform = Affine::translate((ox, oy)) * Affine::scale(scale);
    scene.draw_image(&image, transform);
}

thread_local! {
    // src string -> (width, height, rgba). None = failed/unsupported (don't retry).
    static BG_IMAGE_CACHE: std::cell::RefCell<
        HashMap<String, Option<(u32, u32, std::sync::Arc<Vec<u8>>)>>,
    > = std::cell::RefCell::new(HashMap::new());
    // self.bindImage(url, img) registry ("in-memory:cursor" -> live rgba). NOT
    // cached like data: URIs -- the binding is replaced whenever the remote
    // cursor shape changes.
    static IMAGE_BINDINGS: std::cell::RefCell<
        HashMap<String, (u32, u32, std::sync::Arc<Vec<u8>>)>,
    > = std::cell::RefCell::new(HashMap::new());
    // (src, target_w, target_h) -> pre-scaled rgba at that exact device size.
    static SCALED_IMAGE_CACHE: std::cell::RefCell<
        HashMap<(String, u32, u32), std::sync::Arc<Vec<u8>>>,
    > = std::cell::RefCell::new(HashMap::new());
}

// Downscaling a big source (e.g. a multi-res .ico decoded at 256px) to a small
// icon box with the GPU's single-tap bilinear sampler aliases badly. Pre-resize
// the source to the exact device target with a quality filter (Lanczos3) once,
// cache it, and draw 1:1 -- crisp icons without a per-frame cost.
fn scaled_bg_image(
    src: &str,
    iw: u32,
    ih: u32,
    blob: &std::sync::Arc<Vec<u8>>,
    dw: u32,
    dh: u32,
) -> Option<std::sync::Arc<Vec<u8>>> {
    if dw == 0 || dh == 0 || (dw == iw && dh == ih) {
        return None;
    }
    // Bound urls (in-memory:) are live-replaced; never serve a stale scale.
    if src.starts_with("in-memory:") {
        let source: image::RgbaImage =
            image::ImageBuffer::from_raw(iw, ih, blob.as_ref().clone())?;
        let resized =
            image::imageops::resize(&source, dw, dh, image::imageops::FilterType::Lanczos3);
        return Some(std::sync::Arc::new(resized.into_raw()));
    }
    let key = (src.to_string(), dw, dh);
    if let Some(hit) = SCALED_IMAGE_CACHE.with(|c| c.borrow().get(&key).cloned()) {
        return Some(hit);
    }
    let source: image::RgbaImage =
        image::ImageBuffer::from_raw(iw, ih, blob.as_ref().clone())?;
    let resized =
        image::imageops::resize(&source, dw, dh, image::imageops::FilterType::Lanczos3);
    let out = std::sync::Arc::new(resized.into_raw());
    SCALED_IMAGE_CACHE.with(|c| c.borrow_mut().insert(key, out.clone()));
    Some(out)
}

// Decode a CSS background-image url() to RGBA, cached. Only data: URIs are
// supported (every UI background image is a data:image/png;base64 PNG); other
// schemes (paths, this://app) return None and are skipped.
pub fn bind_image(url: &str, w: u32, h: u32, rgba: std::sync::Arc<Vec<u8>>) {
    IMAGE_BINDINGS.with(|b| b.borrow_mut().insert(url.to_string(), (w, h, rgba)));
}

fn decode_bg_image(src: &str) -> Option<(u32, u32, std::sync::Arc<Vec<u8>>)> {
    if let Some(bound) = IMAGE_BINDINGS.with(|b| b.borrow().get(src).cloned()) {
        return Some(bound);
    }
    if let Some(hit) = BG_IMAGE_CACHE.with(|c| c.borrow().get(src).cloned()) {
        return hit;
    }
    let decoded = decode_data_uri_image(src);
    BG_IMAGE_CACHE.with(|c| c.borrow_mut().insert(src.to_string(), decoded.clone()));
    decoded
}

pub fn decode_data_uri_image(src: &str) -> Option<(u32, u32, std::sync::Arc<Vec<u8>>)> {
    let s = src.trim();
    if !s.starts_with("data:") {
        return None;
    }
    // Tolerate the malformed `data: image/png;base64, iVBOR...` (stray spaces).
    let b64: String = s.split("base64,").nth(1)?.split_whitespace().collect();
    let bytes = super::css::decode_base64(&b64)?;
    let img = image::load_from_memory(&bytes).ok()?.to_rgba8();
    let (w, h) = img.dimensions();
    Some((w, h, std::sync::Arc::new(img.into_raw())))
}

#[allow(clippy::too_many_arguments)]
fn paint_bg_image(
    scene: &mut Scene,
    box_rect: Rect,
    radius: f64,
    iw: u32,
    ih: u32,
    blob: std::sync::Arc<Vec<u8>>,
    style: &Computed,
    s: f64,
    src: &str,
) {
    use super::style::{BgAxis, BgSize};
    if iw == 0 || ih == 0 {
        return;
    }
    let (bw, bh) = (box_rect.width(), box_rect.height());
    // Natural device size = image CSS px (its pixel size) times the DPI scale.
    let (nat_w, nat_h) = (iw as f64 * s, ih as f64 * s);
    let (draw_w, draw_h) = match style.bg_size {
        BgSize::Auto => (nat_w, nat_h),
        BgSize::Cover => {
            let f = (bw / nat_w).max(bh / nat_h);
            (nat_w * f, nat_h * f)
        }
        BgSize::Contain => {
            let f = (bw / nat_w).min(bh / nat_h);
            (nat_w * f, nat_h * f)
        }
        BgSize::Length(w, h) => {
            let dw = w.map(|v| v as f64 * s);
            let dh = h.map(|v| v as f64 * s);
            match (dw, dh) {
                (Some(w), Some(h)) => (w, h),
                (Some(w), None) => (w, w * ih as f64 / iw as f64),
                (None, Some(h)) => (h * iw as f64 / ih as f64, h),
                (None, None) => (nat_w, nat_h),
            }
        }
    };
    let off = |axis: BgAxis, free: f64| -> f64 {
        match axis {
            BgAxis::Start => 0.0,
            BgAxis::Center => free / 2.0,
            BgAxis::End => free,
            BgAxis::Px(p) => p as f64 * s,
            BgAxis::Percent(p) => free * p as f64 / 100.0,
        }
    };
    let ox = box_rect.x0 + off(style.bg_pos.0, bw - draw_w);
    let oy = box_rect.y0 + off(style.bg_pos.1, bh - draw_h);
    let clip = RoundedRect::from_rect(box_rect, radius);
    scene.push_layer(peniko::Mix::Normal, 1.0, Affine::IDENTITY, &clip.to_path(0.1));
    let (dw, dh) = (draw_w.round().max(1.0) as u32, draw_h.round().max(1.0) as u32);
    let (image, sw, sh) = match scaled_bg_image(src, iw, ih, &blob, dw, dh) {
        Some(scaled) => (
            peniko::Image::new(peniko::Blob::new(scaled), peniko::Format::Rgba8, dw, dh),
            dw,
            dh,
        ),
        None => (
            peniko::Image::new(peniko::Blob::new(blob), peniko::Format::Rgba8, iw, ih),
            iw,
            ih,
        ),
    };
    let transform =
        Affine::translate((ox, oy)) * Affine::scale_non_uniform(draw_w / sw as f64, draw_h / sh as f64);
    scene.draw_image(&image, transform);
    scene.pop_layer();
}

pub fn paint_tooltip(
    scene: &mut Scene,
    ts: &mut super::layout::TextSystem,
    text: &str,
    x: f32,
    y: f32,
    viewport: (f32, f32),
    scale: f32,
) {
    let s = scale as f64;
    // Prefer the embedded UI font: on Win7/8 the OS font scan is off, so a bare
    // "system-ui" resolves to nothing and the tooltip shapes to zero width --
    // leaving only its tiny dark background box (the "black dot" near the cursor).
    let fam = ["opensans-semb".to_string(), "system-ui".to_string()];
    let layout = ts.build_layout(
        text,
        12.0,
        400,
        &fam,
        [0.96, 0.97, 0.98, 1.0],
        None,
        parley::layout::Alignment::Start,
        1.0,
        None,
        0.0,
        true,
    );
    let tw = layout.width() as f64 * s;
    let th = layout.height() as f64 * s;
    let pad = 6.0 * s;
    let box_w = tw + pad * 2.0;
    let box_h = th + pad * 2.0;
    // Below-right of the cursor, clamped to stay on screen.
    let mut bx = (x as f64 + 12.0) * s;
    let mut by = (y as f64 + 20.0) * s;
    let vw = viewport.0 as f64 * s;
    let vh = viewport.1 as f64 * s;
    if bx + box_w > vw {
        bx = (vw - box_w).max(0.0);
    }
    if by + box_h > vh {
        by = ((y as f64 - 8.0) * s - box_h).max(0.0);
    }
    let rect = Rect::new(bx, by, bx + box_w, by + box_h);
    scene.fill(
        Fill::NonZero,
        Affine::IDENTITY,
        Color::rgba8(0x2b, 0x30, 0x36, 0xF5),
        None,
        &RoundedRect::from_rect(rect, 4.0 * s),
    );
    let run = super::layout::TextRun { layout };
    draw_text(scene, &run, (bx + pad, by + pad), s, false, None, None);
}

fn paint_shell_icon(scene: &mut Scene, device_rect: Rect, is_folder: bool, s: f64) {
    let sz = 16.0 * s;
    let x0 = device_rect.center().x - sz / 2.0;
    let y0 = device_rect.center().y - sz / 2.0;
    if is_folder {
        let amber = Color::rgb8(0xF5, 0xB9, 0x40);
        scene.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            amber,
            None,
            &RoundedRect::new(x0, y0 + 2.0 * s, x0 + 8.0 * s, y0 + 7.0 * s, 1.0 * s),
        );
        scene.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            amber,
            None,
            &RoundedRect::new(x0, y0 + 4.0 * s, x0 + sz, y0 + sz, 2.0 * s),
        );
    } else {
        let paper = Color::rgb8(0xEC, 0xEF, 0xF3);
        let edge = Color::rgb8(0x9A, 0xA3, 0xAF);
        let body = RoundedRect::new(x0 + 2.0 * s, y0, x0 + sz - 2.0 * s, y0 + sz, 1.5 * s);
        scene.fill(Fill::NonZero, Affine::IDENTITY, paper, None, &body);
        scene.stroke(
            &vello::kurbo::Stroke::new(1.0 * s),
            Affine::IDENTITY,
            edge,
            None,
            &body,
        );
        let mut corner = vello::kurbo::BezPath::new();
        let rx = x0 + sz - 2.0 * s;
        corner.move_to((rx - 4.0 * s, y0));
        corner.line_to((rx, y0 + 4.0 * s));
        corner.line_to((rx - 4.0 * s, y0 + 4.0 * s));
        corner.close_path();
        scene.fill(Fill::NonZero, Affine::IDENTITY, edge, None, &corner);
    }
}

fn draw_text(
    scene: &mut Scene,
    run: &super::layout::TextRun,
    origin: (f64, f64),
    scale: f64,
    underline: bool,
    clip_width: Option<f32>,
    // The CURRENT computed color: the cached parley layout bakes the color from
    // layout time, but hover/selection recolor text without a re-layout (layout
    // is cached across paint-only frames), so paint must use the live color.
    color_now: Option<[f32; 4]>,
) {
    // When the line overflows an ellipsis box, stop at `limit` (leaving room for
    // the dots) and remember where to paint them. Sub-pixel overflow from layout
    // rounding must not trigger the dots: replacing trailing characters with an
    // ellipsis over a fraction of a pixel is how the remote-window device name
    // flipped to "Mac M..." after a minimize/restore reshuffled fractional
    // box positions.
    let truncate = clip_width.map_or(false, |cw| run.layout.width() > cw + 1.0);
    let limit = clip_width.map(|cw| cw - run.layout.height() * 0.75);
    let xf = Affine::translate((origin.0, origin.1)).pre_scale(scale);
    let mut dots: Option<(f64, f64, f64, Color)> = None; // (x, baseline, font_size, color)

    for line in run.layout.lines() {
        for item in line.items() {
            let glyph_run = match item {
                parley::layout::PositionedLayoutItem::GlyphRun(g) => g,
                _ => continue,
            };
            let style = glyph_run.style();
            let color = color_now.unwrap_or(style.brush.0);
            let font = glyph_run.run().font();
            let font_size = glyph_run.run().font_size();
            let start_x = glyph_run.offset();
            let mut x = start_x;
            let y = glyph_run.baseline();
            let brush = Color::rgba(
                color[0] as f64,
                color[1] as f64,
                color[2] as f64,
                color[3] as f64,
            );
            let mut glyphs: Vec<vello::Glyph> = Vec::new();
            let mut cut = false;
            for g in glyph_run.glyphs() {
                if truncate {
                    if let Some(lim) = limit {
                        if x + g.advance > lim {
                            dots = Some((x as f64, y as f64, font_size as f64, brush));
                            cut = true;
                            break;
                        }
                    }
                }
                let gx = x + g.x;
                x += g.advance;
                glyphs.push(vello::Glyph {
                    id: g.id as u32,
                    x: gx,
                    y: y - g.y,
                });
            }
            scene
                .draw_glyphs(font)
                .font_size(font_size)
                .transform(xf)
                .brush(brush)
                .draw(Fill::NonZero, glyphs.into_iter());
            if underline {
                let uy = (y + font_size * 0.12) as f64;
                let thickness = (font_size as f64 * 0.07).max(0.8);
                let mut ul = vello::kurbo::BezPath::new();
                ul.move_to((start_x as f64, uy));
                ul.line_to((x as f64, uy));
                scene.stroke(&vello::kurbo::Stroke::new(thickness), xf, brush, None, &ul);
            }
            if cut {
                break;
            }
        }
        if dots.is_some() {
            break;
        }
    }

    if let Some((dx, baseline, fs, color)) = dots {
        let r = fs * 0.085;
        let gap = fs * 0.32;
        let by = baseline - fs * 0.08;
        for i in 0..3 {
            let cx = dx + fs * 0.22 + i as f64 * gap;
            scene.fill(
                Fill::NonZero,
                xf,
                color,
                None,
                &vello::kurbo::Circle::new((cx, by), r),
            );
        }
    }
}

pub fn render_to_png(
    scene: &Scene,
    width: u32,
    height: u32,
    out_path: &std::path::Path,
) -> Result<(), String> {
    pollster::block_on(render_async(scene, width, height, out_path))
}

async fn render_async(
    scene: &Scene,
    width: u32,
    height: u32,
    out_path: &std::path::Path,
) -> Result<(), String> {
    if let Some(list) = scene.as_cpu() {
        let pm = super::cpu_raster::rasterize(list, width, height);
        // Alpha is 255 everywhere (opaque white base + src-over), so the
        // premultiplied pixmap bytes are valid straight RGBA.
        let img: image::RgbaImage =
            image::ImageBuffer::from_raw(pm.width(), pm.height(), pm.take())
                .ok_or("bad image buffer")?;
        img.save(out_path).map_err(|e| format!("save: {}", e))?;
        return Ok(());
    }
    let scene = scene.as_vello().ok_or("no scene backend")?;
    let instance = wgpu::Instance::default();
    let adapter = instance
        .request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: None,
            force_fallback_adapter: false,
        })
        .await
        .ok_or("no gpu adapter")?;
    let (device, queue) = adapter
        .request_device(
            &wgpu::DeviceDescriptor {
                label: Some("wireui-render"),
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

    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("target"),
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

    renderer
        .render_to_texture(
            &device,
            &queue,
            scene,
            &view,
            &vello::RenderParams {
                base_color: Color::WHITE,
                width,
                height,
                antialiasing_method: vello::AaConfig::Area,
            },
        )
        .map_err(|e| format!("render: {}", e))?;

    let bytes_per_row = (width * 4 + 255) / 256 * 256;
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("readback"),
        size: (bytes_per_row * height) as u64,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("copy"),
    });
    encoder.copy_texture_to_buffer(
        wgpu::ImageCopyTexture {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::ImageCopyBuffer {
            buffer: &buffer,
            layout: wgpu::ImageDataLayout {
                offset: 0,
                bytes_per_row: Some(bytes_per_row),
                rows_per_image: None,
            },
        },
        wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
    );
    queue.submit([encoder.finish()]);

    let slice = buffer.slice(..);
    let (tx, rx) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |r| {
        tx.send(r).ok();
    });
    device.poll(wgpu::Maintain::Wait);
    rx.recv()
        .map_err(|_| "map channel closed".to_string())?
        .map_err(|e| format!("map: {:?}", e))?;
    let data = slice.get_mapped_range();
    let mut pixels = Vec::with_capacity((width * height * 4) as usize);
    for row in 0..height {
        let start = (row * bytes_per_row) as usize;
        pixels.extend_from_slice(&data[start..start + (width * 4) as usize]);
    }
    drop(data);
    buffer.unmap();

    let img: image::RgbaImage =
        image::ImageBuffer::from_raw(width, height, pixels).ok_or("bad image buffer")?;
    img.save(out_path).map_err(|e| format!("save: {}", e))?;
    Ok(())
}

// The custom pointer image (element.style.cursor): painted last, at the
// pointer position, unclipped.
pub fn paint_pointer_image(
    scene: &mut Scene,
    w: u32,
    h: u32,
    rgba: std::sync::Arc<Vec<u8>>,
    x_dev: f64,
    y_dev: f64,
    s: f64,
) {
    if w == 0 || h == 0 {
        return;
    }
    let image = peniko::Image::new(peniko::Blob::new(rgba), peniko::Format::Rgba8, w, h);
    let transform = Affine::translate((x_dev, y_dev)) * Affine::scale(s);
    scene.draw_image(&image, transform);
}

pub fn bound_image_size(url: &str) -> Option<(u32, u32)> {
    IMAGE_BINDINGS.with(|b| b.borrow().get(url).map(|(w, h, _)| (*w, *h)))
}

// paintForeground overlay (the file-transfer drag ghost): an image drawn at
// view coords with an opacity, on top of the whole scene.
pub fn paint_overlay_image(
    scene: &mut Scene,
    iw: u32,
    ih: u32,
    rgba: std::sync::Arc<Vec<u8>>,
    x: f64,
    y: f64,
    dw: f64,
    dh: f64,
    opacity: f32,
    s: f64,
) {
    if iw == 0 || ih == 0 || dw <= 0.0 || dh <= 0.0 {
        return;
    }
    let image = peniko::Image::new(peniko::Blob::new(rgba), peniko::Format::Rgba8, iw, ih);
    let rect = Rect::new(x * s, y * s, (x + dw) * s, (y + dh) * s);
    scene.push_layer(peniko::Mix::Normal, opacity, Affine::IDENTITY, &rect);
    let transform = Affine::translate((x * s, y * s))
        * Affine::scale_non_uniform(dw * s / iw as f64, dh * s / ih as f64);
    scene.draw_image(&image, transform);
    scene.pop_layer();
}
