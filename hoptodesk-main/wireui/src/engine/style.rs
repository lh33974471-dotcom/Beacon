use super::css::{compute_matched, Stylesheet};
use super::dom::{Document, NodeKey};
use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rgba {
    pub r: f32,
    pub g: f32,
    pub b: f32,
    pub a: f32,
}

impl Rgba {
    pub const TRANSPARENT: Rgba = Rgba {
        r: 0.0,
        g: 0.0,
        b: 0.0,
        a: 0.0,
    };
    pub const BLACK: Rgba = Rgba {
        r: 0.0,
        g: 0.0,
        b: 0.0,
        a: 1.0,
    };
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum Length {
    #[default]
    Auto,
    Px(f32),
    Percent(f32),
    Star(f32),
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum Flow {
    #[default]
    Default,
    Horizontal,
    Vertical,
    HorizontalWrap,
    Text,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum VAlign {
    #[default]
    Default,
    Middle,
    Top,
    Bottom,
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum Position {
    #[default]
    Static,
    Relative,
    Absolute,
    Fixed,
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum DisplayKind {
    #[default]
    Block,
    InlineBlock,
    Inline,
    None,
    Flex,
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum TextAlign {
    #[default]
    Left,
    Center,
    Right,
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum TextTransform {
    #[default]
    None,
    Uppercase,
    Lowercase,
    Capitalize,
}

impl TextTransform {
    pub fn apply(&self, text: &str) -> String {
        match self {
            TextTransform::None => text.to_string(),
            TextTransform::Uppercase => text.to_uppercase(),
            TextTransform::Lowercase => text.to_lowercase(),
            TextTransform::Capitalize => text
                .split(' ')
                .map(|w| {
                    let mut ch = w.chars();
                    match ch.next() {
                        Some(f) => f.to_uppercase().collect::<String>() + ch.as_str(),
                        None => String::new(),
                    }
                })
                .collect::<Vec<_>>()
                .join(" "),
        }
    }
}

#[derive(Debug, Clone)]
pub enum Background {
    None,
    Color(Rgba),
    LinearGradient { angle_deg: f32, stops: Vec<Rgba> },
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum BgSize {
    #[default]
    Auto,
    Cover,
    Contain,
    // explicit width/height in CSS px; None = auto (proportional)
    Length(Option<f32>, Option<f32>),
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum BgAxis {
    #[default]
    Start,
    Center,
    End,
    Px(f32),
    Percent(f32),
}

#[derive(Debug, Clone)]
pub struct BoxShadow {
    pub dx: f32,
    pub dy: f32,
    pub blur: f32,
    pub color: Rgba,
}

#[derive(Debug, Clone)]
pub struct Computed {
    pub display: DisplayKind,
    pub position: Position,
    pub flow: Flow,
    pub width: Length,
    pub height: Length,
    pub min_width: Length,
    pub min_height: Length,
    pub max_width: Length,
    pub max_height: Length,
    pub margin: [Length; 4],
    pub padding: [Length; 4],
    pub border_width: [f32; 4],
    pub border_color: Rgba,
    pub border_radius: f32,
    pub border_radius_pct: f32,
    pub background: Background,
    pub color: Rgba,
    pub font_size: f32,
    pub font_weight: u16,
    pub font_family: Vec<String>,
    pub text_align: TextAlign,
    pub text_transform: TextTransform,
    pub line_height: Option<f32>,
    pub letter_spacing: f32,
    pub white_space_nowrap: bool,
    pub opacity: f32,
    pub box_shadow: Option<BoxShadow>,
    pub left: Length,
    pub top: Length,
    pub right: Length,
    pub bottom: Length,
    pub vars: HashMap<String, String>,
    pub visible: bool,
    pub overflow_hidden: bool,
    pub scroll_y: bool,
    pub outline_width: f32,
    pub outline_color: Rgba,
    pub checkbox: bool,
    pub checked: bool,
    pub gap: [f32; 2],
    pub behavior: Option<String>,
    pub z_index: Option<i32>,
    pub cursor: Cursor,
    pub bg_image: Option<String>,
    pub bg_size: BgSize,
    pub bg_pos: (BgAxis, BgAxis),
    pub bg_repeat: bool,
    pub underline: bool,
    pub valign: VAlign,
    pub halign_center: bool,
    pub transform: Vec<TransformOp>,
    pub text_ellipsis: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum TransformOp {
    // Translate distances (px or percent of the element's own box).
    Translate(Length, Length),
    Scale(f32, f32),
    Rotate(f32),
}

pub fn parse_transform(value: &str) -> Vec<TransformOp> {
    let mut ops = Vec::new();
    let mut rest = value.trim();
    while let Some(open) = rest.find('(') {
        let name = rest[..open].trim().to_lowercase();
        let close = match rest[open..].find(')') {
            Some(c) => open + c,
            None => break,
        };
        let args: Vec<&str> = rest[open + 1..close].split(',').map(|s| s.trim()).collect();
        let ang = |s: &str| s.trim_end_matches("deg").trim().parse::<f32>().ok();
        match name.as_str() {
            "translate" => {
                let x = parse_length(args.first().copied().unwrap_or("0"), 0.0);
                let y = args
                    .get(1)
                    .map(|s| parse_length(s, 0.0))
                    .unwrap_or(Length::Px(0.0));
                ops.push(TransformOp::Translate(x, y));
            }
            "translatex" => {
                ops.push(TransformOp::Translate(
                    parse_length(args.first().copied().unwrap_or("0"), 0.0),
                    Length::Px(0.0),
                ));
            }
            "translatey" => {
                ops.push(TransformOp::Translate(
                    Length::Px(0.0),
                    parse_length(args.first().copied().unwrap_or("0"), 0.0),
                ));
            }
            "scale" => {
                let sx = args.first().and_then(|s| s.parse::<f32>().ok()).unwrap_or(1.0);
                let sy = args.get(1).and_then(|s| s.parse::<f32>().ok()).unwrap_or(sx);
                ops.push(TransformOp::Scale(sx, sy));
            }
            "scalex" => ops.push(TransformOp::Scale(
                args.first().and_then(|s| s.parse().ok()).unwrap_or(1.0),
                1.0,
            )),
            "scaley" => ops.push(TransformOp::Scale(
                1.0,
                args.first().and_then(|s| s.parse().ok()).unwrap_or(1.0),
            )),
            "rotate" => {
                if let Some(d) = args.first().and_then(|s| ang(s)) {
                    ops.push(TransformOp::Rotate(d));
                }
            }
            _ => {}
        }
        rest = rest[close + 1..].trim_start();
    }
    ops
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Cursor {
    #[default]
    Inherit,
    Default,
    Pointer,
    Text,
    Move,
    Wait,
    NotAllowed,
    ColResize,
    RowResize,
}

impl Default for Computed {
    fn default() -> Computed {
        Computed {
            display: DisplayKind::Block,
            position: Position::Static,
            flow: Flow::Default,
            width: Length::Auto,
            height: Length::Auto,
            min_width: Length::Auto,
            min_height: Length::Auto,
            max_width: Length::Auto,
            max_height: Length::Auto,
            margin: [Length::Px(0.0); 4],
            padding: [Length::Px(0.0); 4],
            border_width: [0.0; 4],
            border_color: Rgba::TRANSPARENT,
            border_radius: 0.0,
            border_radius_pct: 0.0,
            background: Background::None,
            color: Rgba::BLACK,
            font_size: 13.0,
            font_weight: 400,
            font_family: vec!["system-ui".to_string()],
            text_align: TextAlign::Left,
            text_transform: TextTransform::None,
            line_height: None,
            letter_spacing: 0.0,
            white_space_nowrap: false,
            opacity: 1.0,
            box_shadow: None,
            left: Length::Auto,
            top: Length::Auto,
            right: Length::Auto,
            bottom: Length::Auto,
            vars: HashMap::new(),
            visible: true,
            overflow_hidden: false,
            scroll_y: false,
            outline_width: 0.0,
            outline_color: Rgba::TRANSPARENT,
            checkbox: false,
            checked: false,
            gap: [0.0, 0.0],
            behavior: None,
            z_index: None,
            cursor: Cursor::Inherit,
            bg_image: None,
            bg_size: BgSize::Auto,
            // Sciter renders background images un-tiled by default (every UI use
            // is a single glyph/graphic); default no-repeat matches the client.
            bg_pos: (BgAxis::Start, BgAxis::Start),
            bg_repeat: false,
            underline: false,
            valign: VAlign::Default,
            halign_center: false,
            transform: Vec::new(),
            text_ellipsis: false,
        }
    }
}

pub fn compute_styles(
    doc: &Document,
    sheets: &[Stylesheet],
) -> HashMap<NodeKey, Computed> {
    let mut out: HashMap<NodeKey, Computed> = HashMap::new();
    compute_recursive(doc, sheets, doc.root, None, &mut out);
    apply_table_columns(doc, &mut out);
    out
}

pub fn recompute_subtree(
    doc: &Document,
    sheets: &[Stylesheet],
    key: NodeKey,
    out: &mut HashMap<NodeKey, Computed>,
) {
    let parent = doc
        .arena
        .get(key)
        .and_then(|n| n.parent)
        .and_then(|p| out.get(&p).cloned());
    compute_recursive(doc, sheets, key, parent.as_ref(), out);
}

pub fn apply_table_columns_public(doc: &Document, out: &mut HashMap<NodeKey, Computed>) {
    apply_table_columns(doc, out);
}

// table-fixed: the CSS gives per-column widths only on the header cells
// (th:nth-child). Propagate that template onto every row's cells by column index
// so body columns align with the header and the star column stays bounded.
fn apply_table_columns(doc: &Document, out: &mut HashMap<NodeKey, Computed>) {
    let is_cell = |k: NodeKey| {
        doc.arena
            .get(k)
            .map_or(false, |n| n.tag == "td" || n.tag == "th")
    };
    let row_cells = |row: NodeKey| -> Vec<NodeKey> {
        doc.arena
            .get(row)
            .map(|n| n.children.iter().copied().filter(|c| is_cell(*c)).collect())
            .unwrap_or_default()
    };
    let tables: Vec<NodeKey> = doc
        .descendants(doc.root)
        .into_iter()
        .filter(|k| doc.arena.get(*k).map_or(false, |n| n.tag == "table"))
        .collect();
    for table in tables {
        let rows: Vec<NodeKey> = doc
            .descendants(table)
            .into_iter()
            .filter(|k| doc.arena.get(*k).map_or(false, |n| n.tag == "tr"))
            .collect();
        let header = rows
            .iter()
            .find(|r| {
                doc.arena.get(**r).map_or(false, |n| {
                    n.children
                        .iter()
                        .any(|c| doc.arena.get(*c).map_or(false, |cn| cn.tag == "th"))
                })
            })
            .copied()
            .or_else(|| rows.first().copied());
        let Some(header) = header else { continue };
        let template: Vec<Length> = row_cells(header)
            .iter()
            .map(|c| out.get(c).map(|s| s.width).unwrap_or(Length::Auto))
            .collect();
        if template.is_empty() {
            continue;
        }
        for row in &rows {
            for (i, cell) in row_cells(*row).iter().enumerate() {
                if let (Some(w), Some(cs)) = (template.get(i), out.get_mut(cell)) {
                    cs.width = *w;
                    // A flex item's default min-width is its content size, so a long
                    // file name would grow the Name cell and shove the other columns
                    // right. min-width:0 lets the star column shrink and clip instead.
                    cs.min_width = Length::Px(0.0);
                }
            }
        }
    }
}

fn compute_recursive(
    doc: &Document,
    sheets: &[Stylesheet],
    key: NodeKey,
    parent: Option<&Computed>,
    out: &mut HashMap<NodeKey, Computed>,
) {
    let node = match doc.arena.get(key) {
        Some(n) => n,
        None => return,
    };
    let computed = if node.is_text() {
        let mut c = Computed::default();
        if let Some(p) = parent {
            inherit(&mut c, p);
        }
        c
    } else {
        let matched = compute_matched(doc, sheets, key);
        let mut c = Computed::default();
        if let Some(p) = parent {
            inherit(&mut c, p);
            c.vars = p.vars.clone();
        }
        for (name, value) in matched.vars {
            c.vars.insert(name, value);
        }
        let defaults = element_defaults(&node.tag);
        let decls = matched.declarations.clone();
        // font-size resolves the `em` unit for every other length, so it must be
        // applied before them regardless of source order (CSS resolves it first).
        for (prop, value) in &defaults {
            if *prop == "font-size" {
                apply_declaration(&mut c, prop, value);
            }
        }
        for (prop, value) in &decls {
            if prop == "font-size" {
                let resolved = resolve_value_refs(value, &c.vars);
                apply_declaration(&mut c, prop, &resolved);
            }
        }
        for (prop, value) in &defaults {
            if *prop != "font-size" {
                apply_declaration(&mut c, prop, value);
            }
        }
        for (prop, value) in &decls {
            if prop != "font-size" {
                let resolved = resolve_value_refs(value, &c.vars);
                apply_declaration(&mut c, prop, &resolved);
            }
        }
        if node.tag == "button" && node.attr("type") == Some("checkbox") {
            c.checkbox = true;
            c.checked = node.states.checked;
            c.padding[3] = Length::Px(18.0);
            c.text_align = TextAlign::Left;
        }
        if node.tag == "input" && node.attr("type") == Some("hslider") {
            c.background = Background::None;
            c.border_width = [0.0; 4];
            c.padding = [Length::Px(0.0); 4];
        }
        c
    };
    let children = node.children.clone();
    out.insert(key, computed);
    let parent_ref = out.get(&key).cloned();
    for child in children {
        compute_recursive(doc, sheets, child, parent_ref.as_ref(), out);
    }
}

fn inherit(c: &mut Computed, parent: &Computed) {
    c.color = parent.color;
    c.font_size = parent.font_size;
    c.font_weight = parent.font_weight;
    c.font_family = parent.font_family.clone();
    c.text_align = parent.text_align;
    c.text_transform = parent.text_transform;
    c.line_height = parent.line_height;
    c.letter_spacing = parent.letter_spacing;
    c.white_space_nowrap = parent.white_space_nowrap;
    // the box declares text-overflow; the run that gets clipped is its text child
    c.text_ellipsis = parent.text_ellipsis;
    // text-decoration is not a CSS inherited property, but the underline visually
    // spans descendant text. Link labels are rendered as a child text node
    // ({translate(...)} JSX), which would otherwise never be underlined; an inner
    // element with `text-decoration: none` still overrides this back to false.
    c.underline = parent.underline;
}

fn element_defaults(tag: &str) -> Vec<(&'static str, &'static str)> {
    match tag {
        // Sciter buttons are transparent and unbordered by default (the CSS
        // supplies any chrome, e.g. .button/.outline); a default grey fill made
        // the bare icon buttons in the remote toolbar look like grey chips.
        "button" => vec![
            ("display", "inline-block"),
            ("padding", "3px 10px"),
            ("border-radius", "4px"),
            ("text-align", "center"),
        ],
        "input" => vec![
            ("display", "inline-block"),
            ("background", "white"),
            ("border", "1px solid #b0b0b0"),
            ("padding", "5px 8px"),
            ("min-height", "2em"),
        ],
        "textarea" => vec![
            ("display", "inline-block"),
            ("background", "white"),
            ("border", "1px solid #b0b0b0"),
            ("padding", "3px 5px"),
        ],
        "span" | "b" | "i" | "a" | "em" | "strong" | "code" => vec![("display", "inline")],
        "progress" => vec![("display", "inline-block"), ("width", "80px"), ("height", "8px")],
        // A bare inline <svg> with no CSS size defaults to 1em (Sciter's inline
        // intrinsic), so glyph-only icons like the file-transfer Send arrow show;
        // any explicit width/height/size in CSS overrides this default.
        "svg" => vec![
            ("display", "inline-block"),
            ("width", "1em"),
            ("height", "1em"),
        ],
        // The window-caption shows the title centred in the titlebar (Sciter's
        // built-in <caption> centres it); wireui left-aligned it during loading.
        "caption" => vec![("display", "block"), ("text-align", "center")],
        "img" => vec![("display", "inline-block")],
        // <center> centres inline children via text-align (block children are
        // handled by the center-tag align_items mapping in layout).
        "center" => vec![("display", "block"), ("text-align", "center")],
        "popup" => vec![("display", "none")],
        // A <select> renders as a collapsed control (the engine paints the
        // selected option's label + a chevron); its <option> children are not
        // laid out inline, so they don't stack as a visible list.
        "select" => vec![
            ("display", "inline-block"),
            ("background", "white"),
            ("border", "1px solid #b0b0b0"),
            ("border-radius", "6px"),
            ("padding", "5px 8px"),
            ("min-height", "2em"),
        ],
        "option" => vec![("display", "none")],
        // Table family: rows stack vertically, cells sit side by side. Without
        // these the folder-view <td>s stacked into one column (looked word-wrapped).
        "table" | "thead" | "tbody" => vec![("flow", "vertical")],
        "tr" => vec![("flow", "horizontal")],
        "th" | "td" => vec![("display", "block"), ("overflow", "hidden")],
        _ => Vec::new(),
    }
}

pub fn resolve_value_refs(value: &str, vars: &HashMap<String, String>) -> String {
    let mut out = value.to_string();
    for _ in 0..4 {
        let mut changed = false;
        for func in ["color(", "var("] {
            while let Some(start) = out.find(func) {
                let after = &out[start + func.len()..];
                let end = match after.find(')') {
                    Some(e) => e,
                    None => break,
                };
                let inner = after[..end].trim();
                let (name, default) = match inner.find(',') {
                    Some(c) => (inner[..c].trim(), Some(inner[c + 1..].trim())),
                    None => (inner, None),
                };
                let replacement = vars
                    .get(name)
                    .map(|s| s.as_str())
                    .or(default)
                    .unwrap_or("transparent")
                    .to_string();
                out = format!(
                    "{}{}{}",
                    &out[..start],
                    replacement,
                    &out[start + func.len() + end + 1..]
                );
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    out
}

fn apply_declaration(c: &mut Computed, prop: &str, value: &str) {
    let value = value.trim();
    match prop {
        "display" => {
            c.display = match value {
                "none" => DisplayKind::None,
                "inline-block" => DisplayKind::InlineBlock,
                "inline" => DisplayKind::Inline,
                "flex" => DisplayKind::Flex,
                _ => DisplayKind::Block,
            }
        }
        "visibility" => c.visible = value != "hidden" && value != "collapse",
        "behavior" => {
            let first = value.split_whitespace().next().unwrap_or("");
            if !first.is_empty() && first != "none" {
                c.behavior = Some(first.to_string());
            }
        }
        "overflow" | "overflow-x" | "overflow-y" => {
            if value == "hidden" || value == "clip" {
                c.overflow_hidden = true;
            }
            // Sciter's auto/scroll/scroll-indicator make the box scrollable.
            if matches!(prop, "overflow" | "overflow-y")
                && matches!(value, "auto" | "scroll" | "scroll-indicator")
            {
                c.scroll_y = true;
            }
        }
        "z-index" => {
            if let Ok(z) = value.trim().parse::<i32>() {
                c.z_index = Some(z);
            }
        }
        "cursor" => {
            c.cursor = match value.trim() {
                "pointer" => Cursor::Pointer,
                "text" => Cursor::Text,
                "move" | "all-scroll" | "grab" | "grabbing" => Cursor::Move,
                "wait" | "progress" => Cursor::Wait,
                "not-allowed" | "no-drop" => Cursor::NotAllowed,
                "col-resize" | "ew-resize" => Cursor::ColResize,
                "row-resize" | "ns-resize" => Cursor::RowResize,
                _ => Cursor::Default,
            };
        }
        "position" => {
            c.position = match value {
                "absolute" => Position::Absolute,
                "fixed" => Position::Fixed,
                "relative" => Position::Relative,
                _ => Position::Static,
            }
        }
        "flow" => {
            c.flow = match value {
                "horizontal" => Flow::Horizontal,
                // Sciter's horizontal-flow wraps children to the next row when
                // they don't fit (inline-block-like), same as horizontal-wrap.
                "horizontal-wrap" | "horizontal-flow" => Flow::HorizontalWrap,
                "vertical" => Flow::Vertical,
                "text" => Flow::Text,
                // table/table-fixed: rows are the vertical run; each <tr> is
                // horizontal via element_defaults.
                "table" | "table-fixed" => Flow::Vertical,
                _ => Flow::Default,
            }
        }
        "size" => {
            let parts: Vec<&str> = value.split_whitespace().collect();
            if let Some(w) = parts.first() {
                c.width = parse_length(w, c.font_size);
            }
            match parts.get(1) {
                Some(h) => c.height = parse_length(h, c.font_size),
                None => c.height = c.width,
            }
        }
        "width" => c.width = parse_length(value, c.font_size),
        "height" => c.height = parse_length(value, c.font_size),
        "min-width" => c.min_width = parse_length(value, c.font_size),
        "min-height" => c.min_height = parse_length(value, c.font_size),
        "max-width" => c.max_width = parse_length(value, c.font_size),
        "max-height" => c.max_height = parse_length(value, c.font_size),
        "margin" => {
            let sides = parse_sides(value, c.font_size);
            c.margin = sides;
        }
        "margin-left" => c.margin[3] = parse_length(value, c.font_size),
        "margin-right" => c.margin[1] = parse_length(value, c.font_size),
        "margin-top" => c.margin[0] = parse_length(value, c.font_size),
        "margin-bottom" => c.margin[2] = parse_length(value, c.font_size),
        "padding" => c.padding = parse_sides(value, c.font_size),
        "padding-left" => c.padding[3] = parse_length(value, c.font_size),
        "padding-right" => c.padding[1] = parse_length(value, c.font_size),
        "padding-top" => c.padding[0] = parse_length(value, c.font_size),
        "padding-bottom" => c.padding[2] = parse_length(value, c.font_size),
        "border" => {
            if value == "none" || value == "0" {
                c.border_width = [0.0; 4];
                c.border_color = Rgba::TRANSPARENT;
            } else {
                let mut width = 1.0f32;
                let mut color = c.border_color;
                for part in split_whitespace_outside_parens(value) {
                    if let Length::Px(w) = parse_length(part, c.font_size) {
                        width = w;
                    } else if let Some(col) = parse_color(part) {
                        color = col;
                    }
                }
                c.border_width = [width; 4];
                c.border_color = color;
            }
        }
        "border-top" | "border-right" | "border-bottom" | "border-left" => {
            let idx = match prop {
                "border-top" => 0,
                "border-right" => 1,
                "border-bottom" => 2,
                _ => 3,
            };
            if value == "none" || value == "0" {
                c.border_width[idx] = 0.0;
            } else {
                for part in split_whitespace_outside_parens(value) {
                    if let Length::Px(w) = parse_length(part, c.font_size) {
                        c.border_width[idx] = w;
                    } else if let Some(col) = parse_color(part) {
                        c.border_color = col;
                    }
                }
            }
        }
        "border-color" => {
            if let Some(col) = parse_color(value) {
                c.border_color = col;
            }
        }
        "border-width" => {
            if let Length::Px(w) = parse_length(value, c.font_size) {
                c.border_width = [w; 4];
            }
        }
        "border-radius" => {
            match parse_length(
                value.split_whitespace().next().unwrap_or("0"),
                c.font_size,
            ) {
                Length::Px(r) => {
                    c.border_radius = r;
                    c.border_radius_pct = 0.0;
                }
                // Percent border-radius (e.g. 50% for a circular toggle knob)
                // resolves against the element size at paint time.
                Length::Percent(p) => {
                    c.border_radius_pct = p;
                }
                _ => {}
            }
        }
        "background" | "background-color" => {
            c.background = parse_background(value);
            // A url() in the background shorthand (cm.css uses `background:
            // url(data:...)`) supplies the image too.
            if let Some(url) = extract_bg_url(value) {
                c.bg_image = Some(url);
            }
        }
        "background-image" => {
            if value.trim() == "none" {
                c.bg_image = None;
            } else if let Some(url) = extract_bg_url(value) {
                c.bg_image = Some(url);
            }
        }
        "background-size" => {
            c.bg_size = parse_bg_size(value, c.font_size);
        }
        "background-position" => {
            c.bg_pos = parse_bg_position(value, c.font_size);
        }
        "background-repeat" => {
            c.bg_repeat = !value.trim().starts_with("no-repeat");
        }
        "color" => {
            if let Some(col) = parse_color(value) {
                c.color = col;
            }
        }
        "font-size" => {
            if let Length::Px(px) = parse_length(value, c.font_size) {
                c.font_size = px;
            }
        }
        "font-family" => {
            c.font_family = value
                .split(',')
                .map(|f| f.trim().trim_matches(['\'', '"']).to_string())
                .filter(|f| !f.is_empty())
                .collect();
            if c.font_family.is_empty() {
                c.font_family = vec!["system-ui".to_string()];
            }
        }
        "font-weight" => {
            c.font_weight = match value {
                "bold" => 700,
                "normal" => 400,
                other => other.parse().unwrap_or(c.font_weight),
            };
        }
        "text-align" => {
            c.text_align = match value {
                "center" => TextAlign::Center,
                "right" => TextAlign::Right,
                _ => TextAlign::Left,
            }
        }
        "text-transform" => {
            c.text_transform = match value {
                "uppercase" => TextTransform::Uppercase,
                "lowercase" => TextTransform::Lowercase,
                "capitalize" => TextTransform::Capitalize,
                _ => TextTransform::None,
            }
        }
        "text-decoration" | "text-decoration-line" => {
            c.underline = value.contains("underline");
        }
        "vertical-align" => {
            c.valign = match value {
                "middle" | "center" => VAlign::Middle,
                "top" => VAlign::Top,
                "bottom" => VAlign::Bottom,
                _ => VAlign::Default,
            };
        }
        "horizontal-align" => {
            c.halign_center = value == "center" || value == "middle";
        }
        "transform" => {
            c.transform = if value == "none" {
                Vec::new()
            } else {
                parse_transform(value)
            };
        }
        "text-overflow" => {
            c.text_ellipsis = value == "ellipsis";
        }
        "line-height" => {
            let v = value.trim();
            c.line_height = if let Ok(mult) = v.parse::<f32>() {
                Some(c.font_size * mult)
            } else {
                match parse_length(v, c.font_size) {
                    Length::Px(px) => Some(px),
                    Length::Percent(p) => Some(c.font_size * p / 100.0),
                    _ => None,
                }
            };
        }
        "letter-spacing" => {
            c.letter_spacing = match parse_length(value, c.font_size) {
                Length::Px(px) => px,
                _ => 0.0,
            };
        }
        "white-space" => {
            c.white_space_nowrap = value == "nowrap" || value == "pre";
        }
        "opacity" => {
            c.opacity = value.parse().unwrap_or(1.0);
        }
        "border-spacing" => {
            let parts: Vec<f32> = value
                .split_whitespace()
                .filter_map(|p| match parse_length(p, c.font_size) {
                    Length::Px(v) => Some(v),
                    _ => None,
                })
                .collect();
            match parts.len() {
                1 => c.gap = [parts[0], parts[0]],
                n if n >= 2 => c.gap = [parts[0], parts[1]],
                _ => {}
            }
        }
        "gap" => {
            let parts: Vec<f32> = value
                .split_whitespace()
                .filter_map(|p| match parse_length(p, c.font_size) {
                    Length::Px(v) => Some(v),
                    _ => None,
                })
                .collect();
            match parts.len() {
                1 => c.gap = [parts[0], parts[0]],
                n if n >= 2 => c.gap = [parts[0], parts[1]],
                _ => {}
            }
        }
        "outline" => {
            if value == "none" || value == "0" {
                c.outline_width = 0.0;
            } else {
                for part in split_whitespace_outside_parens(value) {
                    if let Length::Px(w) = parse_length(part, c.font_size) {
                        c.outline_width = w;
                    } else if let Some(col) = parse_color(part) {
                        c.outline_color = col;
                    }
                }
                if c.outline_width == 0.0 {
                    c.outline_width = 1.0;
                }
            }
        }
        "box-shadow" => {
            c.box_shadow = parse_box_shadow(value, c.font_size);
        }
        "left" => c.left = parse_length(value, c.font_size),
        "top" => c.top = parse_length(value, c.font_size),
        "right" => c.right = parse_length(value, c.font_size),
        "bottom" => c.bottom = parse_length(value, c.font_size),
        _ => {}
    }
}

fn parse_sides(value: &str, em: f32) -> [Length; 4] {
    let parts: Vec<Length> = value
        .split_whitespace()
        .map(|p| parse_length(p, em))
        .collect();
    match parts.len() {
        1 => [parts[0]; 4],
        2 => [parts[0], parts[1], parts[0], parts[1]],
        3 => [parts[0], parts[1], parts[2], parts[1]],
        4 => [parts[0], parts[1], parts[2], parts[3]],
        _ => [Length::Px(0.0); 4],
    }
}

pub fn parse_length(value: &str, em: f32) -> Length {
    let v = value.trim();
    if v == "auto" {
        return Length::Auto;
    }
    if v == "*" {
        return Length::Star(1.0);
    }
    if let Some(stripped) = v.strip_suffix('*') {
        let f: f32 = stripped.trim().parse().unwrap_or(1.0);
        return Length::Star(f);
    }
    if let Some(stripped) = v.strip_suffix('%') {
        if let Ok(p) = stripped.trim().parse::<f32>() {
            return Length::Percent(p);
        }
    }
    // Viewport units approximated as parent-percent; the UI only uses them on
    // body-level overlays (popup backdrop 100vw/100vh) where they coincide.
    for suffix in ["vw", "vh"] {
        if let Some(stripped) = v.strip_suffix(suffix) {
            if let Ok(p) = stripped.trim().parse::<f32>() {
                return Length::Percent(p);
            }
        }
    }
    for (suffix, mult) in [
        ("px", 1.0f32),
        ("dip", 1.0),
        ("pt", 96.0 / 72.0),
        ("em", em),
    ] {
        if let Some(stripped) = v.strip_suffix(suffix) {
            if let Ok(n) = stripped.trim().parse::<f32>() {
                return Length::Px(n * mult);
            }
        }
    }
    if let Ok(n) = v.parse::<f32>() {
        return Length::Px(n);
    }
    Length::Auto
}

pub fn parse_color(value: &str) -> Option<Rgba> {
    let v = value.trim();
    if let Some(hex) = v.strip_prefix('#') {
        let hex: String = hex.chars().filter(|c| c.is_ascii_hexdigit()).collect();
        let parse2 = |s: &str| u8::from_str_radix(s, 16).unwrap_or(0) as f32 / 255.0;
        return match hex.len() {
            3 => {
                let r = parse2(&hex[0..1].repeat(2));
                let g = parse2(&hex[1..2].repeat(2));
                let b = parse2(&hex[2..3].repeat(2));
                Some(Rgba { r, g, b, a: 1.0 })
            }
            6 => Some(Rgba {
                r: parse2(&hex[0..2]),
                g: parse2(&hex[2..4]),
                b: parse2(&hex[4..6]),
                a: 1.0,
            }),
            8 => Some(Rgba {
                r: parse2(&hex[0..2]),
                g: parse2(&hex[2..4]),
                b: parse2(&hex[4..6]),
                a: parse2(&hex[6..8]),
            }),
            _ => None,
        };
    }
    if let Some(inner) = v.strip_prefix("rgba(").and_then(|s| s.strip_suffix(')')) {
        let parts: Vec<f32> = inner
            .split(',')
            .map(|p| p.trim().parse().unwrap_or(0.0))
            .collect();
        if parts.len() == 4 {
            return Some(Rgba {
                r: parts[0] / 255.0,
                g: parts[1] / 255.0,
                b: parts[2] / 255.0,
                a: parts[3],
            });
        }
    }
    if let Some(inner) = v.strip_prefix("rgb(").and_then(|s| s.strip_suffix(')')) {
        let parts: Vec<f32> = inner
            .split(',')
            .map(|p| p.trim().parse().unwrap_or(0.0))
            .collect();
        if parts.len() == 3 {
            return Some(Rgba {
                r: parts[0] / 255.0,
                g: parts[1] / 255.0,
                b: parts[2] / 255.0,
                a: 1.0,
            });
        }
    }
    named_color(v)
}

fn named_color(name: &str) -> Option<Rgba> {
    let (r, g, b, a) = match name.to_lowercase().as_str() {
        "white" => (255, 255, 255, 255),
        "black" => (0, 0, 0, 255),
        "red" => (255, 0, 0, 255),
        "green" => (0, 128, 0, 255),
        "blue" => (0, 0, 255, 255),
        "gray" | "grey" => (128, 128, 128, 255),
        "lightgray" | "lightgrey" => (211, 211, 211, 255),
        "darkgray" | "darkgrey" => (169, 169, 169, 255),
        "orange" => (255, 165, 0, 255),
        "yellow" => (255, 255, 0, 255),
        "transparent" | "none" => (0, 0, 0, 0),
        "currentcolor" => return None,
        _ => return None,
    };
    Some(Rgba {
        r: r as f32 / 255.0,
        g: g as f32 / 255.0,
        b: b as f32 / 255.0,
        a: a as f32 / 255.0,
    })
}

// Extract the content of a url(...) inside a background value (data URI or a
// resolvable path), stripping quotes. Data-URI base64 never contains ')'.
fn extract_bg_url(value: &str) -> Option<String> {
    let start = value.find("url(")? + 4;
    let rest = &value[start..];
    let end = rest.find(')')?;
    let inner = rest[..end].trim().trim_matches(['"', '\'']).trim();
    if inner.is_empty() {
        None
    } else {
        Some(inner.to_string())
    }
}

fn parse_bg_size(value: &str, em: f32) -> BgSize {
    let v = value.trim();
    match v {
        "cover" => BgSize::Cover,
        "contain" => BgSize::Contain,
        "auto" => BgSize::Auto,
        _ => {
            let mut parts = v.split_whitespace();
            let w = parts.next().map(|p| bg_len_px(p, em));
            let h = parts.next().map(|p| bg_len_px(p, em));
            match (w, h) {
                (Some(w), Some(h)) => BgSize::Length(w, h),
                (Some(w), None) => BgSize::Length(w, None),
                _ => BgSize::Auto,
            }
        }
    }
}

// A single background length token -> px, or None for `auto`.
fn bg_len_px(tok: &str, em: f32) -> Option<f32> {
    if tok == "auto" {
        return None;
    }
    match parse_length(tok, em) {
        Length::Px(v) => Some(v),
        _ => None,
    }
}

fn parse_bg_position(value: &str, em: f32) -> (BgAxis, BgAxis) {
    let axis = |tok: &str| match tok {
        "left" | "top" => BgAxis::Start,
        "right" | "bottom" => BgAxis::End,
        "center" => BgAxis::Center,
        _ => match parse_length(tok, em) {
            Length::Px(v) => BgAxis::Px(v),
            Length::Percent(p) => BgAxis::Percent(p),
            _ => BgAxis::Start,
        },
    };
    let mut parts = value.split_whitespace();
    let x = parts.next().map(axis).unwrap_or(BgAxis::Start);
    let y = parts.next().map(axis).unwrap_or(BgAxis::Center);
    (x, y)
}

fn parse_background(value: &str) -> Background {
    let v = value.trim();
    if v == "none" || v == "transparent" {
        return Background::None;
    }
    if let Some(inner) = v
        .strip_prefix("linear-gradient(")
        .and_then(|s| s.strip_suffix(')'))
    {
        let parts = split_commas_outside_parens(inner);
        let mut angle = 180.0f32;
        let mut stops = Vec::new();
        for (i, part) in parts.iter().enumerate() {
            let p = part.trim();
            if i == 0 && p.ends_with("deg") {
                angle = p.trim_end_matches("deg").trim().parse().unwrap_or(180.0);
                continue;
            }
            let color_part = p.split_whitespace().next().unwrap_or(p);
            if let Some(c) = parse_color(color_part) {
                stops.push(c);
            } else if let Some(c) = parse_color(p) {
                stops.push(c);
            }
        }
        if stops.len() >= 2 {
            return Background::LinearGradient {
                angle_deg: angle,
                stops,
            };
        }
    }
    match parse_color(first_value_token(v)) {
        Some(c) => Background::Color(c),
        None => Background::None,
    }
}

// The color is the leading token of the `background` shorthand, but rgba()/rgb()
// carry internal spaces (`rgba(15, 23, 42, 0.85)`), so a plain split_whitespace()
// would truncate the function call at its first comma-space. Split on the first
// whitespace that sits OUTSIDE any parentheses instead.
fn first_value_token(v: &str) -> &str {
    let mut depth = 0i32;
    for (i, b) in v.bytes().enumerate() {
        match b {
            b'(' => depth += 1,
            b')' => depth = (depth - 1).max(0),
            b' ' | b'\t' if depth == 0 => return &v[..i],
            _ => {}
        }
    }
    v
}

fn split_commas_outside_parens(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut depth = 0usize;
    for c in text.chars() {
        match c {
            '(' => {
                depth += 1;
                cur.push(c);
            }
            ')' => {
                depth = depth.saturating_sub(1);
                cur.push(c);
            }
            ',' if depth == 0 => out.push(std::mem::take(&mut cur)),
            _ => cur.push(c),
        }
    }
    if !cur.trim().is_empty() {
        out.push(cur);
    }
    out
}

fn split_whitespace_outside_parens(v: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut start: Option<usize> = None;
    for (i, b) in v.bytes().enumerate() {
        match b {
            b' ' | b'\t' | b'\n' | b'\r' if depth == 0 => {
                if let Some(s) = start.take() {
                    out.push(&v[s..i]);
                }
            }
            _ => {
                if start.is_none() {
                    start = Some(i);
                }
                match b {
                    b'(' => depth += 1,
                    b')' => depth = (depth - 1).max(0),
                    _ => {}
                }
            }
        }
    }
    if let Some(s) = start {
        out.push(&v[s..]);
    }
    out
}

fn parse_box_shadow(value: &str, em: f32) -> Option<BoxShadow> {
    let mut nums: Vec<f32> = Vec::new();
    let mut color = Rgba {
        r: 0.0,
        g: 0.0,
        b: 0.0,
        a: 0.3,
    };
    for part in split_whitespace_outside_parens(value.trim()) {
        if let Length::Px(n) = parse_length(part, em) {
            nums.push(n);
        } else if let Some(c) = parse_color(part) {
            color = c;
        }
    }
    if nums.len() >= 2 {
        Some(BoxShadow {
            dx: nums[0],
            dy: nums[1],
            blur: nums.get(2).copied().unwrap_or(0.0),
            color,
        })
    } else {
        None
    }
}

// Paint-only pseudo-element boxes (::before/::after). The UI uses them solely
// for decorative absolute boxes (the toggle-slider knob, custom checkbox
// glyphs), so they render as overlay boxes on the host element - no DOM nodes,
// no layout or hit-test participation.
#[derive(Debug, Clone, Default)]
pub struct PseudoBox {
    pub left: f32,
    pub top: f32,
    pub width: f32,
    pub height: f32,
    pub background: Option<Rgba>,
    pub border_radius: f32,
    pub border_radius_pct: f32,
    pub opacity: f32,
}

pub fn compute_pseudo_boxes(
    doc: &Document,
    sheets: &[Stylesheet],
    styles: &HashMap<NodeKey, Computed>,
) -> HashMap<NodeKey, Vec<PseudoBox>> {
    let mut per_node: HashMap<(NodeKey, String), Vec<(u32, u32, Vec<(String, String)>)>> =
        HashMap::new();
    let mut any = false;
    for sheet in sheets {
        for rule in &sheet.rules {
            for sel in &rule.selectors {
                if sel.pseudo_element.is_none() {
                    continue;
                }
                any = true;
                let kind = sel.pseudo_element.clone().unwrap();
                for key in doc.descendants(doc.root) {
                    let visible = styles
                        .get(&key)
                        .map_or(true, |s| s.visible && s.display != DisplayKind::None);
                    if !visible {
                        continue;
                    }
                    if doc.arena.get(key).map_or(true, |n| n.is_text()) {
                        continue;
                    }
                    if super::css::matches(doc, key, sel) {
                        per_node
                            .entry((key, kind.clone()))
                            .or_default()
                            .push((sel.specificity, rule.order, rule.declarations.clone()));
                    }
                }
            }
        }
        if !any {
            continue;
        }
    }
    let mut out: HashMap<NodeKey, Vec<PseudoBox>> = HashMap::new();
    for ((key, _kind), mut rules) in per_node {
        rules.sort_by_key(|(spec, order, _)| (*spec, *order));
        let mut b = PseudoBox {
            opacity: 1.0,
            ..Default::default()
        };
        let mut has_content = false;
        for (_, _, decls) in rules {
            for (prop, value) in decls {
                match prop.as_str() {
                    "content" => has_content = true,
                    "left" => {
                        if let Length::Px(v) = parse_length(&value, 13.0) {
                            b.left = v;
                        }
                    }
                    "top" => {
                        if let Length::Px(v) = parse_length(&value, 13.0) {
                            b.top = v;
                        }
                    }
                    "width" => {
                        if let Length::Px(v) = parse_length(&value, 13.0) {
                            b.width = v;
                        }
                    }
                    "height" => {
                        if let Length::Px(v) = parse_length(&value, 13.0) {
                            b.height = v;
                        }
                    }
                    "background" | "background-color" => {
                        if let Some(c) = parse_color(&value) {
                            b.background = Some(c);
                        }
                    }
                    "border-radius" => {
                        let v = value.trim();
                        if let Some(p) = v.strip_suffix('%') {
                            b.border_radius_pct = p.trim().parse().unwrap_or(0.0);
                        } else if let Length::Px(px) = parse_length(v, 13.0) {
                            b.border_radius = px;
                        }
                    }
                    "opacity" => {
                        if let Ok(o) = value.trim().parse::<f32>() {
                            b.opacity = o;
                        }
                    }
                    _ => {}
                }
            }
        }
        if has_content && b.width > 0.0 && b.height > 0.0 && b.opacity > 0.0 {
            out.entry(key).or_default().push(b);
        }
    }
    out
}
