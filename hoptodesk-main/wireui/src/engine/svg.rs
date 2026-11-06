use super::dom::{Document, NodeKey};
use super::style::{parse_color, Rgba};
use super::paint_backend::PaintScene as Scene;
use vello::kurbo::{Affine, BezPath, Circle, Ellipse, Rect, Shape};
use vello::peniko::{Color, Fill};

fn to_color(c: Rgba) -> Color {
    Color::rgba(c.r as f64, c.g as f64, c.b as f64, c.a as f64)
}

pub fn paint_svg(
    scene: &mut Scene,
    doc: &Document,
    svg_key: NodeKey,
    rect: (f32, f32, f32, f32),
    inherit_fill: Rgba,
    scale: f64,
) {
    let node = match doc.arena.get(svg_key) {
        Some(n) => n,
        None => return,
    };
    let view_box = node
        .attr("viewBox")
        .or_else(|| node.attr("viewbox"))
        .map(|vb| {
            let parts: Vec<f64> = vb
                .split_whitespace()
                .filter_map(|p| p.parse().ok())
                .collect();
            if parts.len() == 4 {
                (parts[0], parts[1], parts[2], parts[3])
            } else {
                (0.0, 0.0, rect.2 as f64, rect.3 as f64)
            }
        })
        .unwrap_or((0.0, 0.0, rect.2 as f64, rect.3 as f64));
    if view_box.2 <= 0.0 || view_box.3 <= 0.0 || rect.2 <= 0.0 || rect.3 <= 0.0 {
        return;
    }
    let sx = rect.2 as f64 / view_box.2;
    let sy = rect.3 as f64 / view_box.3;
    let s = sx.min(sy);
    let offset_x = rect.0 as f64 + (rect.2 as f64 - view_box.2 * s) / 2.0;
    let offset_y = rect.1 as f64 + (rect.3 as f64 - view_box.3 * s) / 2.0;
    let transform = Affine::scale(scale)
        * Affine::translate((offset_x, offset_y))
        * Affine::scale(s)
        * Affine::translate((-view_box.0, -view_box.1));
    // fill/stroke/stroke-width on the root svg are inherited by children.
    // Line icons set fill="none" stroke="currentColor" on the <svg> only, with
    // bare <path>/<rect>/<circle> children -- without inheritance the children
    // would default to a solid black fill and no stroke (a black silhouette, or
    // with the fill fix, nothing at all).
    let svg_fill = resolve_paint(node.attr("fill"), inherit_fill).unwrap_or(inherit_fill);
    let svg_stroke = resolve_paint(node.attr("stroke"), inherit_fill);
    let svg_stroke_w = node
        .attr("stroke-width")
        .and_then(|w| w.parse().ok())
        .unwrap_or(1.0);
    for &child in doc.arena.get(svg_key).map(|n| &n.children).unwrap_or(&Vec::new()) {
        paint_svg_node(scene, doc, child, transform, svg_fill, svg_stroke, svg_stroke_w);
    }
}

// Resolve an SVG paint attr: none -> transparent, currentColor -> the inherited
// colour, a colour -> parsed, absent -> None (meaning "inherit").
fn resolve_paint(attr: Option<&str>, inherited: Rgba) -> Option<Rgba> {
    match attr {
        Some("none") => Some(Rgba::TRANSPARENT),
        Some("currentColor") => Some(inherited),
        Some(other) => parse_color(other),
        None => None,
    }
}

#[allow(clippy::too_many_arguments)]
fn paint_svg_node(
    scene: &mut Scene,
    doc: &Document,
    key: NodeKey,
    transform: Affine,
    inherited_fill: Rgba,
    inherited_stroke: Option<Rgba>,
    inherited_stroke_w: f64,
) {
    let node = match doc.arena.get(key) {
        Some(n) if !n.is_text() => n,
        _ => return,
    };
    // Each paint inherits from the parent unless overridden on this node.
    let fill = resolve_paint(node.attr("fill"), inherited_fill).unwrap_or(inherited_fill);
    let stroke_color = match node.attr("stroke") {
        Some(_) => resolve_paint(node.attr("stroke"), inherited_fill),
        None => inherited_stroke,
    };
    let stroke_width: f64 = node
        .attr("stroke-width")
        .and_then(|w| w.parse().ok())
        .unwrap_or(inherited_stroke_w);
    let mut transform = transform;
    if let Some(t) = node.attr("transform") {
        transform *= parse_svg_transform(t);
    }
    let fill_rule = match node.attr("fill-rule") {
        Some("evenodd") => Fill::EvenOdd,
        _ => Fill::NonZero,
    };

    let shape: Option<BezPath> = match node.tag.as_str() {
        "path" => node
            .attr("d")
            .and_then(|d| BezPath::from_svg(d).ok()),
        "circle" => {
            let cx: f64 = node.attr("cx").and_then(|v| v.parse().ok()).unwrap_or(0.0);
            let cy: f64 = node.attr("cy").and_then(|v| v.parse().ok()).unwrap_or(0.0);
            let r: f64 = node.attr("r").and_then(|v| v.parse().ok()).unwrap_or(0.0);
            Some(Circle::new((cx, cy), r).to_path(0.1))
        }
        "ellipse" => {
            let cx: f64 = node.attr("cx").and_then(|v| v.parse().ok()).unwrap_or(0.0);
            let cy: f64 = node.attr("cy").and_then(|v| v.parse().ok()).unwrap_or(0.0);
            let rx: f64 = node.attr("rx").and_then(|v| v.parse().ok()).unwrap_or(0.0);
            let ry: f64 = node.attr("ry").and_then(|v| v.parse().ok()).unwrap_or(0.0);
            Some(Ellipse::new((cx, cy), (rx, ry), 0.0).to_path(0.1))
        }
        "rect" => {
            let x: f64 = node.attr("x").and_then(|v| v.parse().ok()).unwrap_or(0.0);
            let y: f64 = node.attr("y").and_then(|v| v.parse().ok()).unwrap_or(0.0);
            let w: f64 = node
                .attr("width")
                .and_then(|v| v.parse().ok())
                .unwrap_or(0.0);
            let h: f64 = node
                .attr("height")
                .and_then(|v| v.parse().ok())
                .unwrap_or(0.0);
            Some(Rect::new(x, y, x + w, y + h).to_path(0.1))
        }
        "polygon" | "polyline" => node.attr("points").and_then(|pts| {
            let nums: Vec<f64> = pts
                .split(|c: char| c.is_whitespace() || c == ',')
                .filter(|t| !t.is_empty())
                .filter_map(|t| t.parse().ok())
                .collect();
            if nums.len() < 4 {
                return None;
            }
            let mut p = BezPath::new();
            p.move_to((nums[0], nums[1]));
            for pair in nums[2..].chunks(2) {
                if pair.len() == 2 {
                    p.line_to((pair[0], pair[1]));
                }
            }
            if node.tag == "polygon" {
                p.close_path();
            }
            Some(p)
        }),
        "line" => {
            let v = |n: &str| node.attr(n).and_then(|v| v.parse::<f64>().ok()).unwrap_or(0.0);
            let mut p = BezPath::new();
            p.move_to((v("x1"), v("y1")));
            p.line_to((v("x2"), v("y2")));
            Some(p)
        }
        "g" | "svg" => None,
        _ => None,
    };

    if let Some(path) = shape {
        if fill.a > 0.0 {
            scene.fill(fill_rule, transform, to_color(fill), None, &path);
        }
        if let Some(sc) = stroke_color {
            if sc.a > 0.0 {
                let stroke = vello::kurbo::Stroke::new(stroke_width);
                scene.stroke(&stroke, transform, to_color(sc), None, &path);
            }
        }
    }

    for &child in &node.children {
        paint_svg_node(scene, doc, child, transform, fill, stroke_color, stroke_width);
    }
}

fn parse_svg_transform(text: &str) -> Affine {
    let mut out = Affine::IDENTITY;
    let mut rest = text;
    while let Some(open) = rest.find('(') {
        let name = rest[..open].trim().trim_start_matches(',').trim();
        let close = match rest[open..].find(')') {
            Some(c) => open + c,
            None => break,
        };
        let args: Vec<f64> = rest[open + 1..close]
            .split([',', ' '])
            .filter(|s| !s.trim().is_empty())
            .filter_map(|s| s.trim().parse().ok())
            .collect();
        match name {
            "scale" => {
                let sx = args.first().copied().unwrap_or(1.0);
                let sy = args.get(1).copied().unwrap_or(sx);
                out *= Affine::scale_non_uniform(sx, sy);
            }
            "translate" => {
                let tx = args.first().copied().unwrap_or(0.0);
                let ty = args.get(1).copied().unwrap_or(0.0);
                out *= Affine::translate((tx, ty));
            }
            "matrix" => {
                if args.len() == 6 {
                    out *= Affine::new([args[0], args[1], args[2], args[3], args[4], args[5]]);
                }
            }
            "rotate" => {
                let deg = args.first().copied().unwrap_or(0.0);
                match (args.get(1), args.get(2)) {
                    (Some(&cx), Some(&cy)) => {
                        out *= Affine::translate((cx, cy))
                            * Affine::rotate(deg.to_radians())
                            * Affine::translate((-cx, -cy));
                    }
                    _ => out *= Affine::rotate(deg.to_radians()),
                }
            }
            _ => {}
        }
        rest = &rest[close + 1..];
    }
    out
}
