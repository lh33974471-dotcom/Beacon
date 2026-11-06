use super::css::{CssParser, Stylesheet};
use super::dom::{Document, NodeKey};
use crate::script::interp::{
    sv_array, sv_object, to_display, ClassVal, Gc, Interp, NativeObj, ObjectData, SResult, SV,
};
use crate::script::runtime::{native_fn, new_object};
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;

pub struct EventBinding {
    pub name: String,
    pub selector: Option<String>,
    pub func: SV,
    // Registration receiver: a component instance scopes the binding to its
    // rendered subtree (Sciter semantics); self/view means document-wide.
    pub scope: SV,
}

pub struct Engine {
    pub doc: Document,
    pub sheets: Vec<Stylesheet>,
    // (due ms, callback, interval ms). A callback that returns true is re-armed
    // at its interval, which is how the UI expresses a repeating timer.
    pub timers: Vec<(f64, SV, f64)>,
    pub now_ms: f64,
    pub scale: f32,
    pub base_dirs: Vec<PathBuf>,
    pub instance_roots: Vec<NodeKey>,
    pub platform: String,
    pub next_css_order: u32,
    pub events: Vec<EventBinding>,
    pub last_rects: HashMap<NodeKey, (f32, f32, f32, f32)>,
    // On-screen rects with ancestor scroll offsets applied and clipped to
    // scroll containers. Pointer hit-testing must use these, not last_rects,
    // or clicks/hover land on the wrong element inside a scrolled panel.
    pub screen_rects: HashMap<NodeKey, (f32, f32, f32, f32)>,
    pub screen_order: Vec<NodeKey>,
    pub hover_cursors: HashMap<NodeKey, crate::engine::style::Cursor>,
    pub textarea_content_h: HashMap<NodeKey, f32>,
    pub behavior_factories: Vec<(String, Rc<dyn Fn() -> crate::bridge::SharedHandler>)>,
    pub behavior_instances: HashMap<NodeKey, crate::bridge::SharedHandler>,
    pub video_sinks: HashMap<NodeKey, crate::video::FrameSink>,
    pub video_streaming: HashMap<NodeKey, bool>,
    pub video_wake: Option<crate::video::FrameWaker>,
    pub archives: Vec<crate::engine::archive::Archive>,
    pub element_objects: HashMap<NodeKey, SV>,
    pub element_handlers: Vec<(NodeKey, String, Option<String>, SV)>,
    // Active tooltip: (text, x, y) in logical px, painted as a top-layer overlay.
    pub tooltip: Option<(String, f32, f32)>,
    // A right-click-opened <menu.context> currently shown as a fixed overlay.
    pub active_context_menu: Option<NodeKey>,
    // An open native <select> dropdown: (owning select, synthesized popup node).
    pub open_select: Option<(NodeKey, NodeKey)>,
    // The right-click edit menu (Cut/Copy/Paste/Select All) over a text input:
    // (target input, synthesized popup node). Sciter provides this natively.
    pub edit_menu: Option<(NodeKey, NodeKey)>,
    // self.bindImage(url, img) registry: "in-memory:cursor" -> rgba image the
    // <img #cursor> paints. Rebindable, so paint must not cache by url.
    pub image_bindings: HashMap<String, (u32, u32, std::sync::Arc<Vec<u8>>)>,
    // element.style.cursor(img, hotx, hoty): while the pointer is over the
    // element, the OS cursor hides and the image paints at the pointer.
    pub cursor_images: HashMap<NodeKey, (u32, u32, std::sync::Arc<Vec<u8>>, f32, f32)>,
    // Last pointer position in logical px (for the custom-cursor overlay).
    pub pointer: (f32, f32),
    pub cursor_image_epoch: u64,
    // view.on(name, fn) registrations (e.g. "size" for on-resize reflow).
    pub view_handlers: Vec<(String, SV)>,
    // element.subscribe(fn, Event.MOUSE)-style raw subscriptions: (owner, mask,
    // handler). Each raw event of a subscribed class is delivered twice --
    // sinking (type | 0x8000) then bubbling -- like Sciter.
    pub subscriptions: Vec<(NodeKey, i64, SV)>,
    // Computed styles memoized per (layout epoch, node-states fingerprint) for
    // both script reads AND paint. The fingerprint folds :hover/:active/:focus
    // etc. into the key, so state flips invalidate without bumping the layout
    // epoch and an unchanged frame skips selector matching entirely.
    pub computed_cache: Option<((u64, u64), std::rc::Rc<HashMap<NodeKey, crate::engine::style::Computed>>)>,
    pub computed_states: HashMap<NodeKey, u8>,
    // ::before/::after boxes memoized under the same key (second full selector
    // pass otherwise re-ran every paint).
    pub pseudo_cache: Option<((u64, u64), std::rc::Rc<HashMap<NodeKey, Vec<crate::engine::style::PseudoBox>>>)>,
    // Ancestor chain of the currently hovered node; lets mousemove diff the
    // hover set instead of walking every node in the document.
    pub hover_chain: Vec<NodeKey>,
    // element.paintForeground = fn(gfx) recordings: (img w, img h, rgba, x, y,
    // draw w, draw h, opacity) in view px, painted as a top layer each frame.
    pub fg_overlays: Vec<(u32, u32, std::sync::Arc<Vec<u8>>, f32, f32, f32, f32, f32)>,
    // element.paintContent = fn(gfx) recordings, element-local logical px,
    // stroked over the element's content each frame (the draw-on-screen
    // annotation overlay).
    pub content_overlays: HashMap<NodeKey, Vec<DrawCmd>>,
    // element.capture(#strict): all raw mouse routes to this element's onMouse
    // until capture(false).
    pub mouse_capture: Option<NodeKey>,
    // element.refresh()/update() from script; window loop polls and redraws.
    pub repaint_requested: bool,
    // (layout epoch, answer) for has_active_progress, which the window loop asks
    // once per iteration and which otherwise walks the whole document.
    progress_scan: std::cell::Cell<(u64, bool)>,
}

#[derive(Clone, Copy)]
pub enum DrawCmd {
    Line { x1: f32, y1: f32, x2: f32, y2: f32, color: u32, width: f32 },
    Rect { l: f32, t: f32, r: f32, b: f32, color: u32, width: f32 },
    Ellipse { cx: f32, cy: f32, rx: f32, ry: f32, color: u32, width: f32 },
}

pub type EngineRef = Rc<RefCell<Engine>>;

impl Engine {
    pub fn new(platform: &str) -> EngineRef {
        Rc::new(RefCell::new(Engine {
            doc: Document::new(),
            sheets: Vec::new(),
            timers: Vec::new(),
            now_ms: 0.0,
            scale: 1.0,
            base_dirs: Vec::new(),
            instance_roots: Vec::new(),
            platform: platform.to_string(),
            next_css_order: 0,
            events: Vec::new(),
            last_rects: HashMap::new(),
            screen_rects: HashMap::new(),
            screen_order: Vec::new(),
            hover_cursors: HashMap::new(),
            textarea_content_h: HashMap::new(),
            behavior_factories: Vec::new(),
            behavior_instances: HashMap::new(),
            video_sinks: HashMap::new(),
            video_streaming: HashMap::new(),
            video_wake: None,
            archives: Vec::new(),
            element_objects: HashMap::new(),
            element_handlers: Vec::new(),
            tooltip: None,
            active_context_menu: None,
            open_select: None,
            edit_menu: None,
            image_bindings: HashMap::new(),
            cursor_images: HashMap::new(),
            pointer: (0.0, 0.0),
            cursor_image_epoch: 0,
            view_handlers: Vec::new(),
            subscriptions: Vec::new(),
            computed_cache: None,
            computed_states: HashMap::new(),
            pseudo_cache: None,
            hover_chain: Vec::new(),
            fg_overlays: Vec::new(),
            content_overlays: HashMap::new(),
            mouse_capture: None,
            repaint_requested: false,
            progress_scan: std::cell::Cell::new((0, false)),
        }))
    }

    pub fn resolve_file(&self, name: &str) -> Option<String> {
        for archive in &self.archives {
            if let Some(content) = archive.get_str(name) {
                return Some(content);
            }
        }
        for dir in &self.base_dirs {
            let path = dir.join(name);
            if let Ok(content) = std::fs::read_to_string(&path) {
                return Some(content);
            }
        }
        None
    }
}

pub fn ingest_css(engine: &EngineRef, css: &str) {
    let (platform, order) = {
        let e = engine.borrow();
        (e.platform.clone(), e.next_css_order)
    };
    let resolver = {
        let engine = engine.clone();
        move |name: &str| engine.borrow().resolve_file(name)
    };
    let sheet = CssParser::parse(css, &platform, Some(&resolver), order);
    let mut e = engine.borrow_mut();
    e.next_css_order = order + sheet.rules.len() as u32 + 10;
    e.sheets.push(sheet);
}

fn ingest_new_styles(engine: &EngineRef, styles: Vec<String>) {
    for css in styles {
        ingest_css(engine, &css);
    }
}

fn ms_of(v: &SV) -> f64 {
    match v {
        SV::Int(i) => *i as f64,
        SV::Float(f) => *f,
        SV::Unit(x, u) => match &**u {
            "s" => x * 1000.0,
            _ => *x,
        },
        _ => 0.0,
    }
}

struct ElementNative {
    engine: EngineRef,
    key: NodeKey,
}

fn element_sv(engine: &EngineRef, key: NodeKey) -> SV {
    if let Ok(e) = engine.try_borrow() {
        if let Some(sv) = e.element_objects.get(&key) {
            return sv.clone();
        }
    }
    let sv = sv_object(ObjectData {
        class: RefCell::new(None),
        props: RefCell::new(Vec::new()),
        native: Some(Gc::new(ElementNative {
            engine: engine.clone(),
            key,
        })),
    });
    if let Ok(mut e) = engine.try_borrow_mut() {
        e.element_objects.insert(key, sv.clone());
    }
    sv
}

fn focused_node(engine: &EngineRef) -> Option<NodeKey> {
    let e = engine.try_borrow().ok()?;
    e.doc
        .descendants(e.doc.root)
        .into_iter()
        .find(|&k| e.doc.arena.get(k).map_or(false, |n| n.states.focus))
}

fn node_from_sv(engine: &EngineRef, val: &SV) -> Option<NodeKey> {
    if !matches!(val, SV::Object(_)) {
        return None;
    }
    let e = engine.try_borrow().ok()?;
    e.element_objects
        .iter()
        .find(|(_, sv)| crate::script::interp::loose_eq(sv, val))
        .map(|(k, _)| *k)
}

/// The child-index path (relative to `root`) and current value of the first
/// focused `<input>` in the subtree, if any. Used to carry a focused text field
/// across a Reactor re-render, which rebuilds the whole subtree from scratch.
fn focused_input_path(e: &Engine, root: NodeKey) -> Option<(Vec<usize>, String, String)> {
    fn rec(e: &Engine, node: NodeKey, path: &mut Vec<usize>) -> Option<(Vec<usize>, String, String)> {
        let n = e.doc.arena.get(node)?;
        if is_text_editable(n) && n.states.focus {
            return Some((path.clone(), n.attr("value").unwrap_or("").to_string(), n.tag.clone()));
        }
        let children = n.children.clone();
        for (i, c) in children.into_iter().enumerate() {
            path.push(i);
            if let Some(r) = rec(e, c, path) {
                return Some(r);
            }
            path.pop();
        }
        None
    }
    rec(e, root, &mut Vec::new())
}

fn node_at_path(e: &Engine, root: NodeKey, path: &[usize]) -> Option<NodeKey> {
    let mut cur = root;
    for &i in path {
        cur = *e.doc.arena.get(cur)?.children.get(i)?;
    }
    Some(cur)
}

// Element state that must survive a Reactor re-render: selection (:current /
// :checked) and scroll offsets. Sciter's Reactor PATCHES the existing DOM, so
// these persist there; our update() rebuilds the subtree, which wiped a folder
// row's selection whenever an io-thread event re-rendered the pane (the live FT
// "selection never shows" bug). Snapshot by child-index path and restore onto
// same-path nodes of the rebuilt tree.
struct NodeStateSnap {
    path: Vec<usize>,
    current: bool,
    checked: bool,
    scroll_top: f32,
    scroll_target: f32,
    scroll_left: f32,
    caret: usize,
    sel_anchor: Option<usize>,
}

// Script-set function expandos (element.onRowDoubleClick = fn, .onMouse, ...)
// keyed by element id, so an update()-driven subtree rebuild does not drop them.
// Sciter patches the DOM in place; wireui rebuilds it, so a handler assigned
// once in attached() (e.g. FolderView's onRowDoubleClick on its <table>) would
// vanish when an async refresh re-renders the component (the remote file-transfer
// pane: its table is created empty, then rebuilt when the listing arrives).
fn snapshot_expandos(e: &Engine, root: NodeKey) -> Vec<(String, Vec<(String, SV)>)> {
    let mut out = Vec::new();
    for node in e.doc.descendants(root) {
        let Some(n) = e.doc.arena.get(node) else { continue };
        let Some(id) = n.attr("id") else { continue };
        let id = id.to_string();
        if let Some(SV::Object(o)) = e.element_objects.get(&node) {
            let fns: Vec<(String, SV)> = o
                .props
                .borrow()
                .iter()
                .filter(|(_, v)| matches!(v, SV::Function(_) | SV::NativeFn(_)))
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            if !fns.is_empty() {
                out.push((id, fns));
            }
        }
    }
    out
}

fn restore_expandos(engine: &EngineRef, root: NodeKey, snap: &[(String, Vec<(String, SV)>)]) {
    if snap.is_empty() {
        return;
    }
    let by_id: Vec<(String, NodeKey)> = {
        let e = engine.borrow();
        e.doc
            .descendants(root)
            .into_iter()
            .filter_map(|k| {
                e.doc
                    .arena
                    .get(k)
                    .and_then(|n| n.attr("id"))
                    .map(|id| (id.to_string(), k))
            })
            .collect()
    };
    for (id, fns) in snap {
        let Some((_, key)) = by_id.iter().find(|(i, _)| i == id) else { continue };
        let sv = element_sv(engine, *key);
        if let SV::Object(o) = &sv {
            let mut props = o.props.borrow_mut();
            for (name, f) in fns {
                if !props.iter().any(|(k, _)| k == name) {
                    props.push((name.clone(), f.clone()));
                }
            }
        }
    }
}

fn subtree_state_snapshot(e: &Engine, root: NodeKey) -> Vec<NodeStateSnap> {
    fn rec(e: &Engine, node: NodeKey, path: &mut Vec<usize>, out: &mut Vec<NodeStateSnap>) {
        let Some(n) = e.doc.arena.get(node) else { return };
        if n.states.current
            || n.states.checked
            || n.scroll_top != 0.0
            || n.scroll_left != 0.0
            || n.caret != 0
            || n.sel_anchor.is_some()
        {
            out.push(NodeStateSnap {
                path: path.clone(),
                current: n.states.current,
                checked: n.states.checked,
                scroll_top: n.scroll_top,
                scroll_target: n.scroll_target,
                scroll_left: n.scroll_left,
                caret: n.caret,
                sel_anchor: n.sel_anchor,
            });
        }
        let children = n.children.clone();
        for (i, c) in children.into_iter().enumerate() {
            path.push(i);
            rec(e, c, path, out);
            path.pop();
        }
    }
    let mut out = Vec::new();
    rec(e, root, &mut Vec::new(), &mut out);
    out
}

fn restore_subtree_state(e: &mut Engine, root: NodeKey, snap: &[NodeStateSnap]) {
    for s in snap {
        let Some(k) = node_at_path(e, root, &s.path) else { continue };
        if let Some(n) = e.doc.arena.get_mut(k) {
            n.states.current = s.current;
            n.states.checked = s.checked;
            n.scroll_top = s.scroll_top;
            n.scroll_target = s.scroll_target;
            n.scroll_left = s.scroll_left;
            n.caret = s.caret;
            n.sel_anchor = s.sel_anchor;
        }
    }
}

fn first_input(e: &Engine, root: NodeKey) -> Option<NodeKey> {
    e.doc
        .descendants(root)
        .into_iter()
        .find(|k| e.doc.arena.get(*k).map_or(false, |n| n.tag == "input"))
}

/// The nearest ancestor (from `hit`) carrying a non-empty `title` attribute, for
/// hover tooltips.
pub fn element_title(engine: &Engine, hit: Option<NodeKey>) -> Option<(NodeKey, String)> {
    let mut cur = hit;
    while let Some(k) = cur {
        let n = engine.doc.arena.get(k)?;
        if let Some(t) = n.attr("title").filter(|t| !t.is_empty()) {
            return Some((k, t.to_string()));
        }
        cur = n.parent;
    }
    None
}

/// True when the document has a visible indeterminate `<progress>` (no `value`),
/// which needs a continuously-animated sweep (the "Connecting..." bar).
pub fn has_active_progress(engine: &Engine) -> bool {
    let epoch = super::dom::layout_epoch();
    let cached = engine.progress_scan.get();
    if cached.0 == epoch {
        return cached.1;
    }
    let found = engine.doc.find_first(engine.doc.root, |n| {
        n.tag == "progress"
            && n.attr("value").is_none()
            && !n
                .inline_style
                .iter()
                .any(|(p, v)| p == "display" && v.trim() == "none")
    });
    let answer = found.is_some();
    engine.progress_scan.set((epoch, answer));
    answer
}

struct AttrsNative {
    engine: EngineRef,
    key: NodeKey,
}

struct StyleNative {
    engine: EngineRef,
    key: NodeKey,
}

impl NativeObj for AttrsNative {
    fn type_name(&self) -> &'static str {
        "attributes"
    }

    fn get(&self, _interp: &mut Interp, name: &str) -> Option<SV> {
        let engine = self.engine.borrow();
        engine
            .doc
            .arena
            .get(self.key)
            .and_then(|n| n.attr(name))
            .map(|v| SV::Str(v.into()))
    }

    fn set(&self, _interp: &mut Interp, name: &str, value: SV) -> bool {
        let mut engine = self.engine.borrow_mut();
        if let Some(node) = engine.doc.arena.get_mut(self.key) {
            // Assigning undefined/null removes the attribute (Sciter), so a later
            // `attributes[x] || default` reads the default, not the string "undefined".
            if matches!(value, SV::Undefined | SV::Null) {
                node.remove_attr(name);
            } else {
                node.set_attr(name, &to_display(&value));
            }
        }
        true
    }

    fn call_method(&self, _interp: &mut Interp, name: &str, argv: &[SV]) -> Option<SResult<SV>> {
        let mut engine = self.engine.borrow_mut();
        let node = engine.doc.arena.get_mut(self.key)?;
        match name {
            "hasClass" => {
                let class = to_display(argv.first().unwrap_or(&SV::Undefined));
                Some(Ok(SV::Bool(node.has_class(&class))))
            }
            "addClass" | "removeClass" | "toggleClass" => {
                let class = to_display(argv.first().unwrap_or(&SV::Undefined));
                let mut classes: Vec<String> =
                    node.classes().map(|c| c.to_string()).collect();
                let has = classes.iter().any(|c| *c == class);
                let want = match name {
                    "addClass" => true,
                    "removeClass" => false,
                    _ => match argv.get(1) {
                        Some(v) => crate::script::interp::truthy(v),
                        None => !has,
                    },
                };
                if want && !has {
                    classes.push(class);
                } else if !want && has {
                    classes.retain(|c| *c != class);
                }
                let joined = classes.join(" ");
                node.set_attr("class", &joined);
                Some(Ok(SV::Undefined))
            }
            _ => Some(Ok(SV::Undefined)),
        }
    }
}

impl NativeObj for StyleNative {
    fn type_name(&self) -> &'static str {
        "style"
    }

    fn get(&self, _interp: &mut Interp, name: &str) -> Option<SV> {
        {
            let engine = self.engine.borrow();
            let node = engine.doc.arena.get(self.key)?;
            if let Some(v) = node
                .inline_style
                .iter()
                .find(|(p, _)| p == name)
                .map(|(_, v)| v.clone())
            {
                return Some(SV::Str(v.into()));
            }
        }
        // Fall back to the CSS-file computed value for the properties scripts
        // probe on elements styled from stylesheets rather than inline.
        match name {
            "overflow" | "overflow-y" | "overflow-x" => {
                let styles = cached_computed_styles(&self.engine);
                let scroll = styles.get(&self.key).map_or(false, |s| s.scroll_y);
                Some(SV::Str(if scroll { "auto" } else { "visible" }.into()))
            }
            "display" => {
                let styles = cached_computed_styles(&self.engine);
                let d = styles.get(&self.key).map(|s| s.display);
                use super::style::DisplayKind as D;
                Some(SV::Str(
                    match d {
                        Some(D::None) => "none",
                        Some(D::Inline) => "inline",
                        Some(D::InlineBlock) => "inline-block",
                        _ => "block",
                    }
                    .into(),
                ))
            }
            _ => None,
        }
    }

    fn set(&self, _interp: &mut Interp, name: &str, value: SV) -> bool {
        let mut engine = self.engine.borrow_mut();
        let prop = name.to_lowercase();
        // style#cursor = undefined restores the system cursor (remote.tis
        // updateCursor(true)); a string value is a plain inline property.
        if prop == "cursor" && matches!(value, SV::Undefined | SV::Null) {
            if engine.cursor_images.remove(&self.key).is_some() {
                engine.cursor_image_epoch += 1;
            }
        }
        if let Some(node) = engine.doc.arena.get_mut(self.key) {
            super::dom::bump_layout_epoch();
            // Assigning undefined/null clears the inline property (Sciter:
            // style.set { left: undefined } removes the override).
            if matches!(value, SV::Undefined | SV::Null) {
                node.inline_style.retain(|(p, _)| *p != prop);
                return true;
            }
            let val = to_display(&value);
            if let Some(slot) = node.inline_style.iter_mut().find(|(p, _)| *p == prop) {
                slot.1 = val;
            } else {
                node.inline_style.push((prop, val));
            }
        }
        true
    }

    fn call_method(&self, interp: &mut Interp, name: &str, argv: &[SV]) -> Option<SResult<SV>> {
        match name {
            "set" => {
                if let Some(SV::Object(o)) = argv.first() {
                    let entries = o.props.borrow().clone();
                    for (prop, value) in entries {
                        self.set(interp, &prop, value);
                    }
                }
                Some(Ok(SV::Undefined))
            }
            // style.cursor(img, hotx, hoty): show the image as the pointer
            // while it is over this element (the remote host's cursor shape).
            "cursor" => {
                let as_f = |v: Option<&SV>| match v {
                    Some(SV::Int(n)) => *n as f32,
                    Some(SV::Float(f)) => *f as f32,
                    _ => 0.0,
                };
                if let Some((w, h, rgba)) = argv.first().and_then(sv_as_image) {
                    let hotx = as_f(argv.get(1));
                    let hoty = as_f(argv.get(2));
                    let mut engine = self.engine.borrow_mut();
                    let changed = engine.cursor_images.get(&self.key).map_or(true, |prev| {
                        prev.0 != w
                            || prev.1 != h
                            || !std::sync::Arc::ptr_eq(&prev.2, &rgba)
                            || prev.3 != hotx
                            || prev.4 != hoty
                    });
                    engine
                        .cursor_images
                        .insert(self.key, (w, h, rgba, hotx, hoty));
                    if changed {
                        engine.cursor_image_epoch += 1;
                    }
                }
                Some(Ok(SV::Undefined))
            }
            _ => Some(Ok(SV::Undefined)),
        }
    }
}

// Opaque byte blob crossing the native bridge (the cursor PNG); scripts only
// pass it along (Image.fromBytes), never inspect it.
struct BytesNative {
    data: Vec<u8>,
}

impl NativeObj for BytesNative {
    fn type_name(&self) -> &'static str {
        "bytes"
    }
    fn as_bytes(&self) -> Option<&[u8]> {
        Some(&self.data)
    }
}

pub fn bytes_sv(data: Vec<u8>) -> SV {
    sv_object(ObjectData {
        class: RefCell::new(None),
        props: RefCell::new(Vec::new()),
        native: Some(Gc::new(BytesNative { data })),
    })
}

// A decoded image (Image.fromBytes / new Image): width/height readable from
// script, rgba extractable engine-side (bindImage, style.cursor).
struct ImageNative {
    w: u32,
    h: u32,
    rgba: std::sync::Arc<Vec<u8>>,
}

impl NativeObj for ImageNative {
    fn type_name(&self) -> &'static str {
        "image"
    }
    fn get(&self, _interp: &mut Interp, name: &str) -> Option<SV> {
        match name {
            "width" => Some(SV::Int(self.w as i64)),
            "height" => Some(SV::Int(self.h as i64)),
            _ => None,
        }
    }
    fn as_image(&self) -> Option<(u32, u32, std::sync::Arc<Vec<u8>>)> {
        Some((self.w, self.h, self.rgba.clone()))
    }
}

pub fn image_sv(w: u32, h: u32, rgba: std::sync::Arc<Vec<u8>>) -> SV {
    sv_object(ObjectData {
        class: RefCell::new(None),
        props: RefCell::new(Vec::new()),
        native: Some(Gc::new(ImageNative { w, h, rgba })),
    })
}

pub fn sv_as_image(v: &SV) -> Option<(u32, u32, std::sync::Arc<Vec<u8>>)> {
    match v {
        SV::Object(o) => o.native.as_ref().and_then(|n| n.as_image()),
        _ => None,
    }
}

// The `Image` global: fromBytes decodes an encoded image (the cursor PNG);
// `new Image(w, h, paintFn)` -- the cursor-scaling idiom -- calls paintFn with
// a one-method gfx whose drawImage captures the source, which we then resize.
struct ImageGlobal;

impl NativeObj for ImageGlobal {
    fn type_name(&self) -> &'static str {
        "Image"
    }
    fn call_method(&self, interp: &mut Interp, name: &str, argv: &[SV]) -> Option<SResult<SV>> {
        match name {
            "fromBytes" => {
                let bytes: Option<Vec<u8>> = match argv.first() {
                    Some(SV::Object(o)) => o
                        .native
                        .as_ref()
                        .and_then(|n| n.as_bytes())
                        .map(|b| b.to_vec()),
                    Some(SV::Array(a)) => Some(
                        a.borrow()
                            .iter()
                            .filter_map(|v| match v {
                                SV::Int(n) => Some(*n as u8),
                                _ => None,
                            })
                            .collect(),
                    ),
                    _ => None,
                };
                let Some(bytes) = bytes else {
                    return Some(Ok(SV::Null));
                };
                match image::load_from_memory(&bytes) {
                    Ok(img) => {
                        let rgba = img.to_rgba8();
                        let (w, h) = rgba.dimensions();
                        Some(Ok(image_sv(w, h, std::sync::Arc::new(rgba.into_raw()))))
                    }
                    Err(_) => Some(Ok(SV::Null)),
                }
            }
            "new" => {
                let w = match argv.first() {
                    Some(SV::Int(n)) => (*n).max(1) as u32,
                    Some(SV::Float(f)) => f.max(1.0) as u32,
                    _ => return Some(Ok(SV::Null)),
                };
                let h = match argv.get(1) {
                    Some(SV::Int(n)) => (*n).max(1) as u32,
                    Some(SV::Float(f)) => f.max(1.0) as u32,
                    _ => return Some(Ok(SV::Null)),
                };
                let paint = argv.get(2).cloned();
                // new Image(w, h, element): snapshot the element's rendered
                // subtree (the file-transfer drag ghost). Headless there is no
                // snapshot hook and this returns null -- the caller's try/catch
                // path (drag without a ghost) still works.
                if let Some(arg) = paint.as_ref() {
                    if !matches!(arg, SV::Function(_) | SV::NativeFn(_)) {
                        if let Some(engine) = current_engine() {
                            if let Some(key) = node_from_sv(&engine, arg) {
                                return Some(Ok(
                                    match snapshot_element(&engine, key, w, h) {
                                        Some((sw, sh, rgba)) => image_sv(sw, sh, rgba),
                                        None => SV::Null,
                                    },
                                ));
                            }
                        }
                    }
                }
                let captured: Gc<RefCell<Option<(u32, u32, std::sync::Arc<Vec<u8>>)>>> =
                    Gc::new(RefCell::new(None));
                if let Some(f @ (SV::Function(_) | SV::NativeFn(_))) = paint {
                    struct GfxCapture {
                        slot: Gc<RefCell<Option<(u32, u32, std::sync::Arc<Vec<u8>>)>>>,
                    }
                    impl NativeObj for GfxCapture {
                        fn type_name(&self) -> &'static str {
                            "graphics"
                        }
                        fn call_method(
                            &self,
                            _interp: &mut Interp,
                            name: &str,
                            argv: &[SV],
                        ) -> Option<SResult<SV>> {
                            if name == "drawImage" {
                                if let Some(img) = argv.first().and_then(sv_as_image) {
                                    *self.slot.borrow_mut() = Some(img);
                                }
                            }
                            Some(Ok(SV::Undefined))
                        }
                    }
                    let gfx = sv_object(ObjectData {
                        class: RefCell::new(None),
                        props: RefCell::new(Vec::new()),
                        native: Some(Gc::new(GfxCapture {
                            slot: captured.clone(),
                        })),
                    });
                    let _ = interp.call_value(&f, &SV::Undefined, &[gfx]);
                }
                let src = captured.borrow_mut().take();
                match src {
                    Some((sw, sh, rgba)) => {
                        let source: Option<image::RgbaImage> =
                            image::ImageBuffer::from_raw(sw, sh, rgba.as_ref().clone());
                        match source {
                            Some(buf) => {
                                let resized = image::imageops::resize(
                                    &buf,
                                    w,
                                    h,
                                    image::imageops::FilterType::Lanczos3,
                                );
                                Some(Ok(image_sv(w, h, std::sync::Arc::new(resized.into_raw()))))
                            }
                            None => Some(Ok(SV::Null)),
                        }
                    }
                    None => Some(Ok(SV::Null)),
                }
            }
            _ => None,
        }
    }
}

pub fn image_global_sv() -> SV {
    sv_object(ObjectData {
        class: RefCell::new(None),
        props: RefCell::new(Vec::new()),
        native: Some(Gc::new(ImageGlobal)),
    })
}

struct StateNative {
    engine: EngineRef,
    key: NodeKey,
}

impl NativeObj for StateNative {
    fn type_name(&self) -> &'static str {
        "element-state"
    }

    fn get(&self, _interp: &mut Interp, name: &str) -> Option<SV> {
        let engine = self.engine.borrow();
        let v = engine.doc.arena.get(self.key).map_or(false, |n| match name {
            "focus" => n.states.focus,
            "disabled" => n.states.disabled,
            "checked" => n.states.checked,
            "current" => n.states.current,
            "hover" => n.states.hover,
            "active" => n.states.active,
            _ => false,
        });
        Some(SV::Bool(v))
    }

    fn set(&self, _interp: &mut Interp, name: &str, value: SV) -> bool {
        let want = crate::script::interp::truthy(&value);
        let mut engine = self.engine.borrow_mut();
        if name == "focus" {
            // Focus is exclusive: setting it true clears every other element's
            // focus; setting it false clears only this one.
            if want {
                let all = engine.doc.descendants(engine.doc.root);
                for k in all {
                    if let Some(node) = engine.doc.arena.get_mut(k) {
                        node.states.focus = k == self.key;
                    }
                }
            } else if let Some(node) = engine.doc.arena.get_mut(self.key) {
                node.states.focus = false;
            }
            return true;
        }
        if let Some(node) = engine.doc.arena.get_mut(self.key) {
            match name {
                "disabled" => node.states.disabled = want,
                "checked" => node.states.checked = want,
                "current" => node.states.current = want,
                "hover" => node.states.hover = want,
                "active" => node.states.active = want,
                _ => return false,
            }
        }
        true
    }
}

impl NativeObj for ElementNative {
    fn type_name(&self) -> &'static str {
        "element"
    }

    fn get(&self, _interp: &mut Interp, name: &str) -> Option<SV> {
        match name {
            "attributes" => Some(sv_object(ObjectData {
                class: RefCell::new(None),
                props: RefCell::new(Vec::new()),
                native: Some(Gc::new(AttrsNative {
                    engine: self.engine.clone(),
                    key: self.key,
                })),
            })),
            "style" => Some(sv_object(ObjectData {
                class: RefCell::new(None),
                props: RefCell::new(Vec::new()),
                native: Some(Gc::new(StyleNative {
                    engine: self.engine.clone(),
                    key: self.key,
                })),
            })),
            "state" => Some(sv_object(ObjectData {
                class: RefCell::new(None),
                props: RefCell::new(Vec::new()),
                native: Some(Gc::new(StateNative {
                    engine: self.engine.clone(),
                    key: self.key,
                })),
            })),
            "sortBy" => {
                let engine = self.engine.borrow();
                let col = grid_sort_column(&engine.doc, self.key);
                drop(engine);
                Some(match col {
                    Some(k) => element_sv(&self.engine, k),
                    None => SV::Undefined,
                })
            }
            "parent" => {
                let engine = self.engine.borrow();
                let parent = engine.doc.arena.get(self.key)?.parent?;
                drop(engine);
                Some(element_sv(&self.engine, parent))
            }
            "index" => {
                let engine = self.engine.borrow();
                Some(SV::Int(engine.doc.element_index(self.key).map_or(-1, |i| i as i64)))
            }
            // element[n] = nth element child (grid comparators do row[col].text).
            _ if name.bytes().all(|b| b.is_ascii_digit()) && !name.is_empty() => {
                let i: usize = name.parse().ok()?;
                let child = {
                    let engine = self.engine.borrow();
                    engine.doc.element_children(self.key).get(i).copied()
                };
                child.map(|c| element_sv(&self.engine, c))
            }
            "first" | "last" => {
                let engine = self.engine.borrow();
                let kids = engine.doc.element_children(self.key);
                let pick = if name == "first" { kids.first() } else { kids.last() }.copied();
                drop(engine);
                Some(match pick {
                    Some(k) => element_sv(&self.engine, k),
                    None => SV::Null,
                })
            }
            "next" | "prior" | "previous" => {
                let engine = self.engine.borrow();
                let sib = engine.doc.element_sibling(self.key, name == "next");
                drop(engine);
                Some(match sib {
                    Some(k) => element_sv(&self.engine, k),
                    None => SV::Null,
                })
            }
            _ if name.parse::<usize>().is_ok() => {
                let i = name.parse::<usize>().unwrap();
                let engine = self.engine.borrow();
                let child = engine.doc.element_children(self.key).get(i).copied();
                drop(engine);
                child.map(|k| element_sv(&self.engine, k))
            }
            "id" => {
                let engine = self.engine.borrow();
                let id = engine
                    .doc
                    .arena
                    .get(self.key)
                    .and_then(|n| n.id())
                    .unwrap_or("")
                    .to_string();
                Some(SV::Str(id.into()))
            }
            "tag" => {
                let engine = self.engine.borrow();
                Some(SV::Str(
                    engine
                        .doc
                        .arena
                        .get(self.key)
                        .map(|n| n.tag.clone())
                        .unwrap_or_default()
                        .into(),
                ))
            }
            "text" => {
                let engine = self.engine.borrow();
                Some(SV::Str(engine.doc.collect_text(self.key).into()))
            }
            "html" => {
                let engine = self.engine.borrow();
                Some(SV::Str(engine.doc.inner_html(self.key).into()))
            }
            "value" => {
                let engine = self.engine.borrow();
                if engine.doc.arena.get(self.key).map_or(false, is_hslider) {
                    drop(engine);
                    return Some(SV::Int(slider_value(&self.engine, self.key)));
                }
                let is_checkbox = engine.doc.arena.get(self.key).map_or(false, |n| {
                    (n.tag == "input" || n.tag == "button")
                        && matches!(n.attr("type"), Some("checkbox") | Some("radio"))
                });
                if is_checkbox {
                    let checked = engine
                        .doc
                        .arena
                        .get(self.key)
                        .map_or(false, |n| n.states.checked);
                    return Some(SV::Bool(checked));
                }
                let is_select = engine
                    .doc
                    .arena
                    .get(self.key)
                    .map_or(false, |n| n.tag == "select");
                if is_select {
                    // An editable <select> holds a free-text value (assigned by
                    // script, e.g. the file-transfer path box) rather than an option.
                    if let Some(n) = engine.doc.arena.get(self.key) {
                        if n.attr("editable").is_some() {
                            if let Some(v) = n.attr("value").filter(|v| !v.is_empty()) {
                                return Some(SV::Str(v.into()));
                            }
                        }
                    }
                    // The value of a <select> is its selected <option>'s value
                    // (or the first option's).
                    let node = engine.doc.arena.get(self.key);
                    let mut first = None;
                    let mut selected = None;
                    if let Some(node) = node {
                        for &c in &node.children {
                            if let Some(opt) = engine.doc.arena.get(c) {
                                if opt.tag == "option" {
                                    if first.is_none() {
                                        first = opt.attr("value").map(|s| s.to_string());
                                    }
                                    if opt.attr("selected").is_some() {
                                        selected = opt.attr("value").map(|s| s.to_string());
                                    }
                                }
                            }
                        }
                    }
                    return Some(SV::Str(selected.or(first).unwrap_or_default().into()));
                }
                let v = engine
                    .doc
                    .arena
                    .get(self.key)
                    .and_then(|n| n.attr("value"))
                    .unwrap_or("")
                    .to_string();
                Some(SV::Str(v.into()))
            }
            "length" => {
                let engine = self.engine.borrow();
                Some(SV::Int(
                    engine
                        .doc
                        .arena
                        .get(self.key)
                        .map(|n| n.children.len() as i64)
                        .unwrap_or(0),
                ))
            }
            _ => None,
        }
    }

    fn set(&self, interp: &mut Interp, name: &str, value: SV) -> bool {
        match name {
            "html" => {
                let html = to_display(&value);
                let mut engine = self.engine.borrow_mut();
                engine.doc.clear_children(self.key);
                let key = self.key;
                super::html::parse_into(&mut engine.doc, key, &html);
                true
            }
            "text" => {
                let text = to_display(&value);
                let mut engine = self.engine.borrow_mut();
                let unchanged = engine.doc.arena.get(self.key).map_or(false, |n| {
                    n.children.len() == 1
                        && engine
                            .doc
                            .arena
                            .get(n.children[0])
                            .and_then(|c| c.text_content())
                            == Some(text.as_str())
                });
                if unchanged {
                    return true;
                }
                engine.doc.clear_children(self.key);
                let tkey = engine.doc.create_text(&text);
                let key = self.key;
                engine.doc.append_child(key, tkey);
                true
            }
            "value" => {
                let v = to_display(&value);
                let slider = {
                    let engine = self.engine.borrow();
                    engine.doc.arena.get(self.key).map_or(false, is_hslider)
                };
                if slider {
                    if let Ok(f) = v.trim().parse::<f32>() {
                        slider_set_value(&self.engine, self.key, f);
                    }
                    return true;
                }
                let mut engine = self.engine.borrow_mut();
                if let Some(node) = engine.doc.arena.get_mut(self.key) {
                    if node.attr("value").unwrap_or("") != v {
                        node.set_attr("value", &v);
                    }
                }
                true
            }
            "id" => {
                let v = to_display(&value);
                let mut engine = self.engine.borrow_mut();
                if let Some(node) = engine.doc.arena.get_mut(self.key) {
                    node.set_attr("id", &v);
                }
                true
            }
            _ => {
                let _ = interp;
                false
            }
        }
    }

    fn call_method(&self, interp: &mut Interp, name: &str, argv: &[SV]) -> Option<SResult<SV>> {
        match name {
            "content" => {
                {
                    let mut engine = self.engine.borrow_mut();
                    engine.doc.clear_children(self.key);
                }
                let v = argv.first().cloned().unwrap_or(SV::Undefined);
                let queued = interp.pending_events.len();
                let built = build_vnode(interp, &self.engine, self.key, &v);
                if current_engine()
                    .map_or(true, |cur| engine_id(&cur) != engine_id(&self.engine))
                {
                    claim_pending_events(interp, &self.engine, queued);
                }
                Some(built.map(|_| SV::Undefined))
            }
            // Grid prototype methods on a folder-view <table>. A "row" is an array
            // of the <tr>'s <td> cells; current rows are those the click marked.
            "getCurrentRows" | "getCurrentRow" => {
                let rows: Vec<NodeKey> = {
                    let e = self.engine.borrow();
                    e.doc
                        .descendants(self.key)
                        .into_iter()
                        .filter(|k| {
                            e.doc
                                .arena
                                .get(*k)
                                .map_or(false, |n| n.tag == "tr" && n.states.current)
                        })
                        .collect()
                };
                // Sciter's Grid returns the <tr> ELEMENT (row[1] works via
                // numeric child indexing, and the drag code calls row.box()).
                let row_sv = |tr: NodeKey| -> SV { element_sv(&self.engine, tr) };
                if name == "getCurrentRow" {
                    Some(Ok(rows.first().map(|tr| row_sv(*tr)).unwrap_or(SV::Null)))
                } else {
                    Some(Ok(sv_array(rows.into_iter().map(row_sv).collect())))
                }
            }
            "resetCurrent" => {
                let mut e = self.engine.borrow_mut();
                let trs: Vec<NodeKey> = e
                    .doc
                    .descendants(self.key)
                    .into_iter()
                    .filter(|k| e.doc.arena.get(*k).map_or(false, |n| n.tag == "tr"))
                    .collect();
                for tr in trs {
                    if let Some(n) = e.doc.arena.get_mut(tr) {
                        n.states.current = false;
                    }
                }
                Some(Ok(SV::Undefined))
            }
            "sortRows" => {
                let clicked = matches!(argv.first(), Some(SV::Bool(true)));
                grid_sort_rows(&self.engine, self.key, None, clicked);
                Some(Ok(SV::Undefined))
            }
            "sortColumn" | "getRow" => Some(Ok(SV::Undefined)),
            "$" | "select" => {
                let sel = to_display(argv.first().unwrap_or(&SV::Undefined));
                let engine = self.engine.borrow();
                let found = engine.doc.select_first(&sel, self.key);
                drop(engine);
                Some(Ok(match found {
                    Some(k) => element_sv(&self.engine, k),
                    None => SV::Null,
                }))
            }
            "$$" | "selectAll" => {
                let sel = to_display(argv.first().unwrap_or(&SV::Undefined));
                let engine = self.engine.borrow();
                let found = engine.doc.select_all(&sel, self.key);
                drop(engine);
                let items: Vec<SV> = found
                    .into_iter()
                    .map(|k| element_sv(&self.engine, k))
                    .collect();
                Some(Ok(sv_array(items)))
            }
            "$p" | "closest" => {
                let sel = to_display(argv.first().unwrap_or(&SV::Undefined));
                let engine = self.engine.borrow();
                let found = engine.doc.closest(&sel, self.key);
                drop(engine);
                Some(Ok(match found {
                    Some(k) => element_sv(&self.engine, k),
                    None => SV::Null,
                }))
            }
            "$is" | "is" => {
                let sel = to_display(argv.first().unwrap_or(&SV::Undefined));
                let engine = self.engine.borrow();
                let ok = engine.doc.matches_any(&sel, self.key);
                Some(Ok(SV::Bool(ok)))
            }
            "sendEvent" => {
                // Synchronously synthesize + dispatch the named event so the
                // element's handler runs (Enter->connect, msgbox submit/cancel).
                let ev = to_display(argv.first().unwrap_or(&SV::Undefined));
                let r = dispatch_dom_event(interp, &self.engine, &ev, self.key);
                Some(r.map(|_| SV::Undefined))
            }
            "$append" => {
                let html = to_display(argv.first().unwrap_or(&SV::Undefined));
                let page = {
                    let mut engine = self.engine.borrow_mut();
                    let key = self.key;
                    super::html::parse_into(&mut engine.doc, key, &html)
                };
                ingest_new_styles(&self.engine, page.styles);
                Some(Ok(SV::Undefined))
            }
            "timer" => {
                let due = ms_of(argv.first().unwrap_or(&SV::Int(0)));
                let f = argv.get(1).cloned().unwrap_or(SV::Undefined);
                schedule_timer(&self.engine, due, f);
                Some(Ok(SV::Bool(true)))
            }
            "toPixels" => {
                // Convert a length (dip/in/px/pt/em/mm/cm) to device pixels at
                // the current DPI scale. Script derives scaleFactor from
                // toPixels(10000dip)/10000 and pixelsPerInch from toPixels(1in),
                // so both the unit factor and the scale must be applied.
                let scale = self.engine.borrow().scale as f64;
                let px = match argv.first() {
                    Some(SV::Unit(x, unit)) => {
                        let per_dip = match unit.as_ref() {
                            "in" => 96.0,
                            "cm" => 96.0 / 2.54,
                            "mm" => 96.0 / 25.4,
                            "pt" => 96.0 / 72.0,
                            "pc" => 16.0,
                            _ => 1.0, // dip, px, dpx
                        };
                        *x * per_dip * scale
                    }
                    Some(SV::Int(i)) => *i as f64 * scale,
                    Some(SV::Float(x)) => *x * scale,
                    _ => 0.0,
                };
                Some(Ok(SV::Int(px.round() as i64)))
            }
            "url" => Some(Ok(argv.first().cloned().unwrap_or(SV::Str("".into())))),
            "box" => {
                ensure_layout(&self.engine);
                let rect = {
                    let e = self.engine.borrow();
                    e.last_rects.get(&self.key).copied().unwrap_or((0.0, 0.0, 0.0, 0.0))
                };
                let (mut x, mut y, mut w, mut h) = rect;
                if argv.iter().skip(1).any(|a| matches!(a, SV::Symbol(s) if s.as_ref() == "content")) {
                    let styles = cached_computed_styles(&self.engine);
                    let e = self.engine.borrow();
                    w += max_scroll_x_of(&e, &styles, &e.last_rects, self.key);
                    h += max_scroll_of(&e, &styles, &e.last_rects, self.key);
                }
                let what = match argv.first() {
                    Some(SV::Symbol(s)) => s.to_string(),
                    _ => "rectw".to_string(),
                };
                // Third arg = coordinate space: #view (default, layout coords),
                // #parent (relative to the parent box), #screen (desktop coords).
                let coords = argv
                    .iter()
                    .skip(1)
                    .find_map(|a| match a {
                        SV::Symbol(s) if matches!(s.as_ref(), "view" | "parent" | "screen" | "self") => {
                            Some(s.to_string())
                        }
                        _ => None,
                    })
                    .unwrap_or_else(|| "view".to_string());
                match coords.as_str() {
                    "parent" => {
                        let e = self.engine.borrow();
                        if let Some(p) = e.doc.arena.get(self.key).and_then(|n| n.parent) {
                            if let Some(&(px, py, _, _)) = e.last_rects.get(&p) {
                                x -= px;
                                y -= py;
                            }
                        }
                    }
                    "screen" => {
                        if let Some((wx, wy, _, _)) = window_rect() {
                            x += wx as f32;
                            y += wy as f32;
                        }
                    }
                    "self" => {
                        x = 0.0;
                        y = 0.0;
                    }
                    _ => {}
                }
                let mk = |v: f32| SV::Int(v.round() as i64);
                Some(Ok(match what.as_str() {
                    "width" => mk(w),
                    "height" => mk(h),
                    "left" => mk(x),
                    "top" => mk(y),
                    "right" => mk(x + w),
                    "bottom" => mk(y + h),
                    "position" => sv_array(vec![mk(x), mk(y)]),
                    "dimension" => sv_array(vec![mk(w), mk(h)]),
                    // Sciter: #rect = (left, top, right, bottom); #rectw = (x, y, w, h).
                    "rect" => sv_array(vec![mk(x), mk(y), mk(x + w), mk(y + h)]),
                    "rectw" => sv_array(vec![mk(x), mk(y), mk(w), mk(h)]),
                    _ => mk(w),
                }))
            }
            "on" | "subscribe" => {
                // .subscribe(fn, Event.MOUSE) - raw event-class subscription
                // (the file-transfer drag tracker). Function-first + int mask.
                if let (Some(f @ (SV::Function(_) | SV::NativeFn(_))), Some(SV::Int(mask))) =
                    (argv.first(), argv.get(1))
                {
                    let mut engine = self.engine.borrow_mut();
                    engine.subscriptions.push((self.key, *mask, f.clone()));
                    return Some(Ok(SV::Undefined));
                }
                // .on(eventName, [selector,] handler) - register a per-element
                // handler; a selector arg makes it delegated (fires for matching
                // descendants). The event name may carry a namespace
                // ("size.foo") which we strip.
                let name_arg = match argv.first() {
                    Some(SV::Str(s)) => Some(s.to_string()),
                    _ => None,
                };
                let selector = match argv.get(1) {
                    Some(SV::Str(s)) => Some(s.to_string()),
                    _ => None,
                };
                let func = argv
                    .iter()
                    .rev()
                    .find(|a| matches!(a, SV::Function(_) | SV::NativeFn(_)));
                if let (Some(ev), Some(f)) = (name_arg, func) {
                    let ev = ev.split('.').next().unwrap_or(&ev).to_string();
                    let mut engine = self.engine.borrow_mut();
                    engine.element_handlers.push((self.key, ev, selector, f.clone()));
                }
                Some(Ok(SV::Undefined))
            }
            "unsubscribe" => {
                if let Some(f) = argv.first() {
                    let mut engine = self.engine.borrow_mut();
                    engine
                        .subscriptions
                        .retain(|(_, _, h)| !crate::script::interp::loose_eq(h, f));
                }
                Some(Ok(SV::Undefined))
            }
            "off" => {
                let ev = match argv.first() {
                    Some(SV::Str(s)) => Some(s.to_string()),
                    _ => None,
                };
                let mut engine = self.engine.borrow_mut();
                match ev {
                    Some(e) => {
                        let e = e.split('.').next().unwrap_or(&e).to_string();
                        engine.element_handlers.retain(|(k, n, _, _)| !(*k == self.key && *n == e));
                    }
                    None => engine.element_handlers.retain(|(k, _, _, _)| *k != self.key),
                }
                Some(Ok(SV::Undefined))
            }
            "detach" | "remove" => {
                let mut engine = self.engine.borrow_mut();
                let key = self.key;
                engine.element_handlers.retain(|(k, _, _, _)| *k != key);
                engine.doc.remove_subtree(key);
                Some(Ok(SV::Undefined))
            }
            "scrollTo" => {
                let n = |i: usize| match argv.get(i) {
                    Some(SV::Int(v)) => *v as f32,
                    Some(SV::Float(v)) => *v as f32,
                    Some(SV::Unit(v, _)) => *v as f32,
                    _ => 0.0,
                };
                let mut engine = self.engine.borrow_mut();
                let mut moved = false;
                if let Some(node) = engine.doc.arena.get_mut(self.key) {
                    let (nx, ny) = (n(0).max(0.0), n(1).max(0.0));
                    moved = node.scroll_left != nx || node.scroll_top != ny;
                    node.scroll_left = nx;
                    node.scroll_top = ny;
                    node.scroll_target = node.scroll_top;
                }
                if moved {
                    engine.repaint_requested = true;
                }
                Some(Ok(SV::Undefined))
            }
            "scroll" => {
                let which = match argv.first() {
                    Some(SV::Symbol(s)) => s.to_string(),
                    _ => "top".to_string(),
                };
                let engine = self.engine.borrow();
                let node = engine.doc.arena.get(self.key);
                let val = match which.as_str() {
                    "left" => node.map_or(0.0, |n| n.scroll_left),
                    "top" => node.map_or(0.0, |n| n.scroll_top),
                    "width" | "height" => match engine.last_rects.get(&self.key).copied() {
                        Some((nx, ny, _, _)) => {
                            let mut ext = 0.0f32;
                            for d in engine.doc.descendants(self.key) {
                                if let Some((cx, cy, cw, ch)) = engine.last_rects.get(&d) {
                                    let e = if which == "height" {
                                        (cy + ch) - ny
                                    } else {
                                        (cx + cw) - nx
                                    };
                                    if e > ext {
                                        ext = e;
                                    }
                                }
                            }
                            ext
                        }
                        None => 0.0,
                    },
                    _ => 0.0,
                };
                Some(Ok(SV::Int(val as i64)))
            }
            "post" | "focus" | "postEvent" => {
                Some(Ok(SV::Undefined))
            }
            "capture" => {
                let on = !matches!(
                    argv.first(),
                    None | Some(SV::Bool(false)) | Some(SV::Null) | Some(SV::Undefined)
                );
                if let Ok(mut e) = self.engine.try_borrow_mut() {
                    if on {
                        e.mouse_capture = Some(self.key);
                    } else if e.mouse_capture == Some(self.key) {
                        e.mouse_capture = None;
                    }
                }
                Some(Ok(SV::Bool(true)))
            }
            "update" | "refresh" => {
                if let Ok(mut e) = self.engine.try_borrow_mut() {
                    e.repaint_requested = true;
                }
                Some(Ok(SV::Undefined))
            }
            // self.bindImage(url, img): register an in-memory image so an
            // <img src=url> paints it (the in-window remote-cursor sprite).
            "bindImage" => {
                let url = to_display(argv.first().unwrap_or(&SV::Undefined));
                if let Some((w, h, rgba)) = argv.get(1).and_then(sv_as_image) {
                    super::paint::bind_image(&url, w, h, rgba);
                }
                Some(Ok(SV::Undefined))
            }
            // input.xcall(#selectionStart/#selectionEnd/#setSelection) -- the
            // caret/selection API (password eye-toggle restore, select-all).
            "xcall" => {
                let sub = match argv.first() {
                    Some(SV::Symbol(s)) => s.to_string(),
                    Some(SV::Str(s)) => s.to_string(),
                    _ => return Some(Ok(SV::Undefined)),
                };
                let as_i = |v: Option<&SV>| -> usize {
                    match v {
                        Some(SV::Int(n)) => (*n).max(0) as usize,
                        Some(SV::Float(f)) => f.max(0.0) as usize,
                        _ => 0,
                    }
                };
                match sub.as_str() {
                    "selectionStart" | "selectionEnd" => {
                        let e = self.engine.borrow();
                        let node = e.doc.arena.get(self.key);
                        let (caret, anchor, n) = node
                            .map(|n| {
                                (
                                    n.caret,
                                    n.sel_anchor,
                                    n.attr("value").unwrap_or("").chars().count(),
                                )
                            })
                            .unwrap_or((0, None, 0));
                        let caret = caret.min(n);
                        let val = match (sub.as_str(), anchor) {
                            ("selectionStart", Some(a)) => a.min(n).min(caret),
                            ("selectionEnd", Some(a)) => a.min(n).max(caret),
                            _ => caret,
                        };
                        Some(Ok(SV::Int(val as i64)))
                    }
                    "setSelection" => {
                        let start = as_i(argv.get(1));
                        let end = as_i(argv.get(2));
                        set_selection(&self.engine, self.key, start, end);
                        Some(Ok(SV::Undefined))
                    }
                    _ => Some(Ok(SV::Undefined)),
                }
            }
            // Reorder this element's children by a script comparator (Sciter
            // tbody.sort -- the grid's column-header sort). Comparator gets two
            // child elements, negative return = first sorts earlier.
            "sort" => {
                let cmp = match argv.first() {
                    Some(f @ (SV::Function(_) | SV::NativeFn(_))) => f.clone(),
                    _ => return Some(Ok(SV::Undefined)),
                };
                let (rows, dropped): (Vec<NodeKey>, Vec<NodeKey>) = {
                    let e = self.engine.borrow();
                    let all = e
                        .doc
                        .arena
                        .get(self.key)
                        .map(|n| n.children.clone())
                        .unwrap_or_default();
                    all.into_iter().partition(|c| {
                        e.doc.arena.get(*c).map_or(false, |n| !n.is_text())
                    })
                };
                let mut items: Vec<(NodeKey, SV)> = rows
                    .iter()
                    .map(|k| (*k, element_sv(&self.engine, *k)))
                    .collect();
                items.sort_by(|a, b| {
                    let r = interp.call_value(&cmp, &SV::Undefined, &[a.1.clone(), b.1.clone()]);
                    let n = match r {
                        Ok(SV::Int(n)) => n as f64,
                        Ok(SV::Float(f)) => f,
                        _ => 0.0,
                    };
                    if n < 0.0 {
                        std::cmp::Ordering::Less
                    } else if n > 0.0 {
                        std::cmp::Ordering::Greater
                    } else {
                        std::cmp::Ordering::Equal
                    }
                });
                let sorted: Vec<NodeKey> = items.iter().map(|(k, _)| *k).collect();
                let reordered = sorted != rows;
                {
                    let mut e = self.engine.borrow_mut();
                    // Whitespace text children of a table body aren't rendered;
                    // remove them rather than leaking arena nodes.
                    let had_text = !dropped.is_empty();
                    for t in dropped {
                        e.doc.arena.remove(t);
                    }
                    if reordered || had_text {
                        if let Some(node) = e.doc.arena.get_mut(self.key) {
                            node.children = sorted;
                        }
                    }
                }
                if reordered {
                    super::dom::bump_layout_epoch();
                }
                Some(Ok(SV::Undefined))
            }
            // Scroll the nearest scrollable ancestor so this element is visible
            // (grid keyboard navigation).
            "scrollToView" => {
                scroll_into_view(&self.engine, self.key);
                Some(Ok(SV::Undefined))
            }
            "clear" => {
                let mut engine = self.engine.borrow_mut();
                let key = self.key;
                engine.doc.clear_children(key);
                Some(Ok(SV::Undefined))
            }
            _ => {
                let behavior = self.engine.borrow().behavior_instances.get(&self.key).cloned();
                if let Some(h) = behavior {
                    let values: Vec<crate::value::Value> =
                        argv.iter().map(crate::bridge::sv_to_value).collect();
                    let result =
                        h.borrow_mut().on_script_call(std::ptr::null_mut(), name, &values);
                    if let Some(v) = result {
                        return Some(Ok(crate::bridge::value_to_sv(&v)));
                    }
                }
                if name == "t" {
                    Some(Ok(argv.first().cloned().unwrap_or(SV::Str("".into()))))
                } else if name.starts_with("get_") {
                    Some(Ok(SV::Str("".into())))
                } else if name.starts_with("is_")
                    || name.starts_with("has_")
                    || name.starts_with("can_")
                {
                    Some(Ok(SV::Bool(false)))
                } else {
                    Some(Ok(SV::Undefined))
                }
            }
        }
    }
}

pub fn build_vnode(
    interp: &mut Interp,
    engine: &EngineRef,
    parent: NodeKey,
    v: &SV,
) -> SResult<()> {
    match v {
        SV::Array(a) => {
            let items = a.borrow().clone();
            let is_vnode = items.len() == 3
                && matches!(items[0], SV::Str(_) | SV::Class(_))
                && matches!(items[1], SV::Object(_))
                && matches!(items[2], SV::Array(_));
            if !is_vnode {
                for item in items {
                    build_vnode(interp, engine, parent, &item)?;
                }
                return Ok(());
            }
            let tag = &items[0];
            let attrs = &items[1];
            let children = &items[2];
            match tag {
                SV::Class(_) => {
                    let inst = interp.construct(tag, &[attrs.clone()])?;
                    apply_ref(interp, attrs, inst.clone())?;
                    let idx = if let SV::Object(o) = &inst {
                        let idx = {
                            let mut e = engine.borrow_mut();
                            e.instance_roots.push(parent);
                            e.instance_roots.len() - 1
                        };
                        o.props
                            .borrow_mut()
                            .push(("__mount".into(), SV::Int(idx as i64)));
                        Some(idx)
                    } else {
                        None
                    };
                    let before = {
                        let e = engine.borrow();
                        e.doc.arena.get(parent).map(|n| n.children.len()).unwrap_or(0)
                    };
                    let rendered = interp.call_method(&inst, "render", &[])?;
                    build_vnode(interp, engine, parent, &rendered)?;
                    // Anchor the instance to its own render-root element, NOT
                    // the parent: update() replaces only the component's
                    // subtree. Anchoring to the parent made update() clear the
                    // whole parent, wiping siblings mounted next to the
                    // component (the page-level #msgbox died on the first
                    // app.update(), killing every dialog afterwards).
                    if let Some(idx) = idx {
                        let mut e = engine.borrow_mut();
                        let kids: Vec<NodeKey> = e
                            .doc
                            .arena
                            .get(parent)
                            .map(|n| n.children[before.min(n.children.len())..].to_vec())
                            .unwrap_or_default();
                        let root = kids
                            .into_iter()
                            .find(|k| e.doc.arena.get(*k).map_or(false, |c| !c.is_text()));
                        if let Some(root) = root {
                            e.instance_roots[idx] = root;
                        }
                    }
                    // Fire the component's attached() lifecycle now that its subtree
                    // (and @{this.x} refs) are mounted. FolderView.attached() wires
                    // the table row-click handlers and sets the local path box; the
                    // Reactor base attached() is a harmless no-op.
                    if let Err(e) = interp.call_method(&inst, "attached", &[]) {
                        eprintln!("attached: {}", e.0);
                    }
                }
                SV::Str(tag_name) => {
                    let key = {
                        let mut e = engine.borrow_mut();
                        let key = e.doc.create_element(tag_name);
                        e.doc.append_child(parent, key);
                        key
                    };
                    if let SV::Object(ao) = attrs {
                        let entries = ao.props.borrow().clone();
                        for (name, value) in entries {
                            if name == "@ref" {
                                let el = element_sv(engine, key);
                                apply_ref_binding(interp, &value, el)?;
                                continue;
                            }
                            let mut e = engine.borrow_mut();
                            if let Some(node) = e.doc.arena.get_mut(key) {
                                match &value {
                                    SV::Bool(true) => node.set_attr(&name, ""),
                                    SV::Bool(false) | SV::Undefined | SV::Null => {}
                                    other => node.set_attr(&name, &to_display(other)),
                                }
                                // checked/disabled attrs seed the element state
                                // (the single source of truth for :checked etc.),
                                // so JSX <input checked/> shows on and toggles.
                                if name == "checked" && !matches!(value, SV::Bool(false) | SV::Undefined | SV::Null) {
                                    node.states.checked = true;
                                } else if name == "disabled" && !matches!(value, SV::Bool(false) | SV::Undefined | SV::Null) {
                                    node.states.disabled = true;
                                }
                            }
                        }
                    }
                    if let SV::Array(ch) = children {
                        let ch = ch.borrow().clone();
                        for c in ch {
                            match &c {
                                SV::Array(_) => build_vnode(interp, engine, key, &c)?,
                                SV::Str(s) => {
                                    let mut e = engine.borrow_mut();
                                    let raw: &str = s;
                                    if raw.contains('<') {
                                        super::html::parse_into(&mut e.doc, key, raw);
                                    } else if !raw.is_empty() {
                                        let t = e.doc.create_text(raw);
                                        e.doc.append_child(key, t);
                                    }
                                }
                                SV::Undefined | SV::Null | SV::Bool(false) => {}
                                other => {
                                    let text = to_display(other);
                                    if !text.is_empty() {
                                        let mut e = engine.borrow_mut();
                                        let t = e.doc.create_text(&text);
                                        e.doc.append_child(key, t);
                                    }
                                }
                            }
                        }
                    }
                }
                _ => {}
            }
            Ok(())
        }
        SV::Str(s) => {
            let mut e = engine.borrow_mut();
            let raw: &str = s;
            if raw.contains('<') {
                super::html::parse_into(&mut e.doc, parent, raw);
            } else if !raw.is_empty() {
                let t = e.doc.create_text(raw);
                e.doc.append_child(parent, t);
            }
            Ok(())
        }
        SV::Undefined | SV::Null | SV::Bool(false) => Ok(()),
        other => {
            let text = to_display(other);
            if !text.is_empty() {
                let mut e = engine.borrow_mut();
                let t = e.doc.create_text(&text);
                e.doc.append_child(parent, t);
            }
            Ok(())
        }
    }
}

fn apply_ref(interp: &mut Interp, attrs: &SV, instance: SV) -> SResult<()> {
    if let SV::Object(ao) = attrs {
        let binding = ao
            .props
            .borrow()
            .iter()
            .find(|(k, _)| k == "@ref")
            .map(|(_, v)| v.clone());
        if let Some(b) = binding {
            apply_ref_binding(interp, &b, instance)?;
        }
    }
    Ok(())
}

fn apply_ref_binding(interp: &mut Interp, binding: &SV, value: SV) -> SResult<()> {
    if let SV::Object(b) = binding {
        let target = b
            .props
            .borrow()
            .iter()
            .find(|(k, _)| k == "target")
            .map(|(_, v)| v.clone());
        let prop = b
            .props
            .borrow()
            .iter()
            .find(|(k, _)| k == "prop")
            .map(|(_, v)| to_display(v));
        if let (Some(target), Some(prop)) = (target, prop) {
            interp.member_set(&target, &prop, value)?;
        }
    }
    Ok(())
}

pub fn install_host(interp: &mut Interp, engine: &EngineRef) {
    install_host_bridged(interp, engine, None)
}

pub fn install_host_bridged(interp: &mut Interp, engine: &EngineRef, handler: Option<crate::bridge::SharedHandler>) {
    // Pure builtins (Math/JSON/Array/... and the language classes) go into a
    // shared BASE env; the per-window bindings below (self/view/$/$$, the include
    // hook, Reactor + component classes) go into a child env that becomes
    // interp.global. Each window gets its OWN child env over the same base, so a
    // second window (the chat window) shares the heap + builtins yet keeps its own
    // top-level vars (`var handler = ...`) and its own view/self/DOM selectors.
    crate::script::runtime::install_globals(interp);
    let window_env = crate::script::interp::Env::new(Some(interp.global.clone()));
    interp.global = window_env;
    install_window_bindings(interp, engine, handler, None);
}

// The per-window interpreter bindings (self/view/$/$$ + include hook), installed
// into interp.global (which must already be this window's env). Both the initial
// page load and a child window created via view.window use this; `params`
// becomes the window's view.parameters (None for the main window).
pub fn install_window_bindings(
    interp: &mut Interp,
    engine: &EngineRef,
    handler: Option<crate::bridge::SharedHandler>,
    params: Option<SV>,
) {
    let root = engine.borrow().doc.root;
    let self_sv = element_sv(engine, root);
    interp.global.define("self", self_sv.clone());
    interp.self_object = self_sv.clone();

    let view = make_view_mock(engine, handler);
    if let (Some(p), SV::Object(o)) = (&params, &view) {
        o.props.borrow_mut().push(("parameters".to_string(), p.clone()));
    }
    interp.global.define("view", view.clone());
    interp.view_object = view;

    let g_engine = engine.clone();
    interp.global.define(
        "$",
        native_fn("$", move |_i, _t, argv| {
            let sel = to_display(argv.first().unwrap_or(&SV::Undefined));
            let e = g_engine.borrow();
            let found = e.doc.select_first(sel.trim(), e.doc.root);
            drop(e);
            Ok(match found {
                Some(k) => element_sv(&g_engine, k),
                None => SV::Null,
            })
        }),
    );
    let g_engine2 = engine.clone();
    interp.global.define(
        "$$",
        native_fn("$$", move |_i, _t, argv| {
            let sel = to_display(argv.first().unwrap_or(&SV::Undefined));
            let e = g_engine2.borrow();
            let found = e.doc.select_all(sel.trim(), e.doc.root);
            drop(e);
            let items: Vec<SV> = found
                .into_iter()
                .map(|k| element_sv(&g_engine2, k))
                .collect();
            Ok(sv_array(items))
        }),
    );

    let hook_engine = engine.clone();
    interp.include_hook = Some(std::rc::Rc::new(move |interp: &mut Interp, spec: &str| {
        if spec == "wireui:reactor.tis" {
            install_reactor(interp, &hook_engine);
            return Some(Ok(()));
        }
        if spec.starts_with("wireui:") {
            return Some(Ok(()));
        }
        let content = hook_engine.borrow().resolve_file(spec);
        content.map(|source| interp.run_source(&source))
    }));
}

// URL.toPath: a file:// URL -> a filesystem path (percent-decoded). Sciter's
// pickers return file:// URLs; our rfd pickers already return plain paths, so a
// bare path passes through unchanged -- handling both keeps callers working.
pub fn strip_file_url(u: &str) -> String {
    let s = u.trim();
    let rest = s
        .strip_prefix("file://localhost/")
        .map(|r| format!("/{}", r))
        .or_else(|| s.strip_prefix("file:///").map(|r| {
            // Windows drive URL (file:///C:/..) has no leading slash on the path.
            if r.chars().nth(1) == Some(':') { r.to_string() } else { format!("/{}", r) }
        }))
        .or_else(|| s.strip_prefix("file://").map(|r| r.to_string()));
    match rest {
        Some(r) => percent_decode(&r),
        None => s.to_string(),
    }
}

pub fn path_to_file_url(p: &str) -> String {
    let norm = p.replace('\\', "/");
    if norm.starts_with('/') {
        format!("file://{}", norm)
    } else {
        format!("file:///{}", norm)
    }
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = |b: u8| (b as char).to_digit(16);
            if let (Some(h), Some(l)) = (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                out.push((h * 16 + l) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn make_view_mock(engine: &EngineRef, handler: Option<crate::bridge::SharedHandler>) -> SV {
    let mut overrides: Vec<(String, SV)> = Vec::new();
    let platform = engine.borrow().platform.clone();
    overrides.push((
        "mediaVar".into(),
        native_fn("mediaVar", move |_i, _t, argv| {
            let name = to_display(argv.first().unwrap_or(&SV::Undefined));
            Ok(match name.as_str() {
                "platform" => SV::Str(platform.as_str().into()),
                _ => SV::Undefined,
            })
        }),
    ));
    overrides.push((
        "get_connect_status".into(),
        native_fn("get_connect_status", |_i, _t, _a| {
            Ok(sv_array(vec![
                SV::Int(1),
                SV::Bool(true),
                SV::Str("123456789".into()),
            ]))
        }),
    ));
    overrides.push((
        "get_option".into(),
        native_fn("get_option", |_i, _t, _a| Ok(SV::Str("".into()))),
    ));
    overrides.push((
        "get_local_option".into(),
        native_fn("get_local_option", |_i, _t, _a| Ok(SV::Str("".into()))),
    ));
    overrides.push((
        "get_langs".into(),
        native_fn("get_langs", |_i, _t, _a| {
            Ok(SV::Str(
                "[[\"default\",\"Default\"],[\"en\",\"English\"]]".into(),
            ))
        }),
    ));
    if std::env::var("WIREUI_MOCK_LAN").is_ok() {
        const PEERS: &str = "[[\"111111111\",\"alice\",\"iPhone10\",\"iOS\",\"iPhone10\"],[\"222222222\",\"bob\",\"g-SM-G991\",\"Android\",\"\"],[\"333333333\",\"carol\",\"desktop-x\",\"Windows\",\"\"],[\"444444444\",\"dan\",\"macbook\",\"Mac\",\"\"],[\"555555555\",\"eve\",\"rator-pc\",\"Windows\",\"\"],[\"666666666\",\"fay\",\"tablet-9\",\"Android\",\"\"]]";
        overrides.push((
            "get_lan_peers".into(),
            native_fn("get_lan_peers", |_i, _t, _a| Ok(SV::Str(PEERS.into()))),
        ));
        overrides.push((
            "get_recent_sessions".into(),
            native_fn("get_recent_sessions", |_i, _t, _a| Ok(SV::Str(PEERS.into()))),
        ));
        overrides.push((
            "get_local_option".into(),
            native_fn("get_local_option", |_i, _t, argv| {
                let key = match argv.first() {
                    Some(SV::Str(s)) => s.to_string(),
                    _ => String::new(),
                };
                if key == "show-sessions-type" {
                    Ok(SV::Str("lan".into()))
                } else {
                    Ok(SV::Str("".into()))
                }
            }),
        ));
    }
    overrides.push((
        "t".into(),
        native_fn("t", |_i, _t, argv| {
            Ok(argv.first().cloned().unwrap_or(SV::Str("".into())))
        }),
    ));
    overrides.push((
        "get_id".into(),
        native_fn("get_id", |_i, _t, _a| Ok(SV::Str("123456789".into()))),
    ));
    overrides.push((
        "get_default_pi".into(),
        native_fn("get_default_pi", |_i, _t, _a| {
            Ok(new_object(vec![
                ("hostname".into(), SV::Str("mockhost".into())),
                ("username".into(), SV::Str("mockuser".into())),
                ("platform".into(), SV::Str("OSX".into())),
            ]))
        }),
    ));
    overrides.push((
        "screenBox".into(),
        native_fn("screenBox", |_i, _t, argv| {
            let workarea = argv
                .iter()
                .any(|a| matches!(a, SV::Symbol(s) if s.as_ref() == "workarea"));
            let (x, y, w, h) = if workarea {
                workarea_rect()
                    .or_else(monitor_rect)
                    .unwrap_or((0.0, 0.0, 1440.0, 900.0))
            } else {
                monitor_rect().unwrap_or((0.0, 0.0, 1440.0, 900.0))
            };
            // The second arg is the coordinate flavour: #dimension -> [w,h],
            // #position -> [x,y], #rectw/#rect (or none) -> [x,y,w,h]. Returning
            // the 4-tuple regardless placed a `var (sw,sh) = screenBox(#dimension)`
            // reader's (sw,sh) = (x,y) = (0,0), moving the CM window off-screen.
            let has = |name: &str| {
                argv.iter().any(|a| matches!(a, SV::Symbol(s) if s.as_ref() == name))
            };
            let mk = |v: f64| SV::Int(v as i64);
            if has("dimension") {
                Ok(sv_array(vec![mk(w), mk(h)]))
            } else if has("position") {
                Ok(sv_array(vec![mk(x), mk(y)]))
            } else {
                Ok(sv_array(vec![mk(x), mk(y), mk(w), mk(h)]))
            }
        }),
    ));
    overrides.push((
        "box".into(),
        native_fn("box", |_i, _t, argv| {
            let (x, y, w, h) = window_rect().unwrap_or((0.0, 0.0, 800.0, 600.0));
            // box(...#screen) = the window's outer rect (position + size); otherwise
            // the client box at the origin.
            let screen = argv
                .iter()
                .any(|a| matches!(a, SV::Symbol(s) if s.as_ref() == "screen"));
            let (ox, oy) = if screen { (x, y) } else { (0.0, 0.0) };
            Ok(sv_array(vec![
                SV::Int(ox as i64),
                SV::Int(oy as i64),
                SV::Int(w as i64),
                SV::Int(h as i64),
            ]))
        }),
    ));
    overrides.push((
        "move".into(),
        native_fn("move", |_i, _t, argv| {
            let n = |i: usize| match argv.get(i) {
                Some(SV::Int(v)) => *v as f64,
                Some(SV::Float(v)) => *v,
                Some(SV::Unit(v, _)) => *v,
                _ => 0.0,
            };
            if argv.len() >= 4 {
                request_view_move(n(0), n(1), n(2), n(3));
            }
            Ok(SV::Undefined)
        }),
    ));
    overrides.push((
        "focus".into(),
        native_fn("focus", |_i, _t, _a| Ok(SV::Undefined)),
    ));
    overrides.push((
        "close".into(),
        native_fn("close", |_i, _t, _a| {
            request_view_close();
            Ok(SV::Undefined)
        }),
    ));
    overrides.push((
        "nativeTheme".into(),
        native_fn("nativeTheme", |_i, _t, argv| {
            request_native_theme(matches!(argv.get(0), Some(SV::Bool(true))));
            Ok(SV::Undefined)
        }),
    ));
    overrides.push((
        "selectFile".into(),
        native_fn("selectFile", |_i, _t, argv| {
            let mode = match argv.first() {
                Some(SV::Symbol(s)) => s.to_string(),
                _ => "open".to_string(),
            };
            let mut dlg = rfd::FileDialog::new();
            if let Some(filter) = argv.get(1).map(to_display).filter(|s| !s.is_empty()) {
                let parts: Vec<&str> = filter.split('|').collect();
                let mut i = 0;
                while i + 1 < parts.len() {
                    let name = parts[i].trim();
                    let exts: Vec<String> = parts[i + 1]
                        .split(';')
                        .map(|p| p.trim().trim_start_matches("*.").to_string())
                        .filter(|p| !p.is_empty() && p != "*")
                        .collect();
                    if !exts.is_empty() {
                        let refs: Vec<&str> = exts.iter().map(|s| s.as_str()).collect();
                        dlg = dlg.add_filter(name, &refs);
                    }
                    i += 2;
                }
            }
            // selectFile(mode, filter, defaultExt, initialPath, caption): preset
            // the save dialog's folder + filename (the screenshot "Save as" opens
            // at ~/Documents with "screenshot.png"), and its title.
            let default_ext = argv.get(2).map(to_display).filter(|s| !s.is_empty());
            if let Some(init) = argv.get(3).map(to_display).filter(|s| !s.is_empty()) {
                let mut p = std::path::PathBuf::from(strip_file_url(&init));
                if p.extension().is_none() {
                    if let Some(ext) = &default_ext {
                        p.set_extension(ext);
                    }
                }
                if let Some(dir) = p.parent().filter(|d| !d.as_os_str().is_empty()) {
                    dlg = dlg.set_directory(dir);
                }
                if let Some(name) = p.file_name().and_then(|n| n.to_str()) {
                    dlg = dlg.set_file_name(name);
                }
            }
            if let Some(cap) = argv.get(4).map(to_display).filter(|s| !s.is_empty()) {
                dlg = dlg.set_title(cap);
            }
            let picked = if mode == "save" {
                dlg.save_file()
            } else {
                dlg.pick_file()
            };
            match picked {
                Some(mut p) => {
                    // Enforce the default extension on save if the user didn't type one.
                    if mode == "save" && p.extension().is_none() {
                        if let Some(ext) = &default_ext {
                            p.set_extension(ext);
                        }
                    }
                    Ok(SV::Str(p.to_string_lossy().into_owned().into()))
                }
                None => Ok(SV::Null),
            }
        }),
    ));
    overrides.push((
        "selectFolder".into(),
        native_fn("selectFolder", |_i, _t, argv| {
            let mut dlg = rfd::FileDialog::new();
            if let Some(cap) = argv.first().map(to_display).filter(|s| !s.is_empty()) {
                dlg = dlg.set_title(cap);
            }
            if let Some(dir) = argv.get(1).map(to_display).filter(|s| !s.is_empty()) {
                dlg = dlg.set_directory(dir);
            }
            match dlg.pick_folder() {
                Some(p) => Ok(SV::Str(p.to_string_lossy().into_owned().into())),
                None => Ok(SV::Null),
            }
        }),
    ));
    overrides.push((
        "clipboard".into(),
        native_fn("clipboard", |_i, _t, argv| {
            // view.clipboard(#get, #text) -> String; view.clipboard(#put, value).
            let sym = |i: usize, name: &str| {
                matches!(argv.get(i), Some(SV::Symbol(s)) if s.as_ref() == name)
            };
            if sym(0, "put") {
                if let Some(v) = argv.get(1) {
                    let text = to_display(v);
                    let _ = arboard::Clipboard::new().and_then(|mut c| c.set_text(text));
                }
                Ok(SV::Undefined)
            } else {
                // #get (default): return the clipboard text ("" on any failure).
                let text = arboard::Clipboard::new()
                    .and_then(|mut c| c.get_text())
                    .unwrap_or_default();
                Ok(SV::Str(text.into()))
            }
        }),
    ));
    let on_engine = engine.clone();
    overrides.push((
        "on".into(),
        native_fn("on", move |_i, _t, argv| {
            // view.on(name, fn) — register a window event handler (e.g. "size").
            if let (Some(name), Some(f)) = (argv.first(), argv.get(1)) {
                if !matches!(f, SV::Undefined | SV::Null) {
                    on_engine
                        .borrow_mut()
                        .view_handlers
                        .push((to_display(name), f.clone()));
                }
            }
            Ok(SV::Undefined)
        }),
    ));
    let win_engine = engine.clone();
    let win_handler = handler.clone();
    overrides.push((
        "window".into(),
        native_fn("window", move |interp, _t, argv| {
            let params = argv.first().cloned().unwrap_or(SV::Undefined);
            let get = |k: &str| match &params {
                SV::Object(od) => od
                    .props
                    .borrow()
                    .iter()
                    .find(|(kk, _)| kk == k)
                    .map(|(_, v)| v.clone()),
                _ => None,
            };
            let as_u32 = |v: Option<SV>, d: u32| match v {
                Some(SV::Int(n)) => n.max(1) as u32,
                Some(SV::Float(f)) => (f as u32).max(1),
                Some(SV::Unit(f, _)) => (f as u32).max(1),
                _ => d,
            };
            let child_params = get("parameters").unwrap_or(SV::Undefined);
            let width = as_u32(get("width"), 300);
            let height = as_u32(get("height"), 400);
            let title = get("caption").map(|v| to_display(&v)).unwrap_or_default();
            let source = match get("html") {
                Some(SV::Str(s)) if !s.is_empty() => s.to_string(),
                _ => {
                    let url = get("url").map(|v| to_display(&v)).unwrap_or_default();
                    let name = url.rsplit('/').next().unwrap_or(&url).to_string();
                    match win_engine.borrow().resolve_file(&name) {
                        Some(c) => c,
                        None => return Ok(SV::Null),
                    }
                }
            };
            let platform = win_engine.borrow().platform.clone();
            let base_dirs = win_engine.borrow().base_dirs.clone();
            let archives = win_engine.borrow().archives.clone();
            match open_child_window(
                interp,
                &source,
                child_params,
                win_handler.clone(),
                &platform,
                base_dirs,
                archives,
            ) {
                Ok((child_engine, child_view)) => {
                    request_child_window(PendingChildWindow {
                        engine: child_engine,
                        view: child_view.clone(),
                        width,
                        height,
                        title,
                    });
                    Ok(child_view)
                }
                Err(e) => {
                    eprintln!("view.window: {}", e);
                    Ok(SV::Null)
                }
            }
        }),
    ));

    struct ViewNative {
        engine: EngineRef,
        overrides: HashMap<String, SV>,
        handler: Option<crate::bridge::SharedHandler>,
    }
    impl NativeObj for ViewNative {
        fn type_name(&self) -> &'static str {
            "view"
        }
        fn get(&self, _interp: &mut Interp, name: &str) -> Option<SV> {
            if name == "focus" {
                return Some(match focused_node(&self.engine) {
                    Some(k) => element_sv(&self.engine, k),
                    None => SV::Null,
                });
            }
            if name == "windowState" {
                return Some(SV::Int(current_window_state()));
            }
            if name == "screens" {
                return Some(SV::Int(screens().0));
            }
            if name == "screen" {
                return Some(SV::Int(screens().1));
            }
            self.overrides.get(name).cloned()
        }
        fn set(&self, _interp: &mut Interp, name: &str, value: SV) -> bool {
            if name == "focus" {
                let target = node_from_sv(&self.engine, &value);
                set_focus(&self.engine, target);
                return true;
            }
            let as_int = |v: &SV| match v {
                SV::Int(n) => Some(*n),
                SV::Float(f) => Some(*f as i64),
                _ => None,
            };
            let id = engine_id(&self.engine);
            match name {
                "windowState" => {
                    if let Some(n) = as_int(&value) {
                        push_window_command(id, WindowCommand::State(n));
                    }
                    return true;
                }
                "windowMonitor" => {
                    if let Some(n) = as_int(&value) {
                        push_window_command(id, WindowCommand::Monitor(n));
                    }
                    return true;
                }
                "windowActivate" => {
                    if crate::script::interp::truthy(&value) {
                        push_window_command(id, WindowCommand::Activate);
                    }
                    return true;
                }
                "windowTopmost" => {
                    push_window_command(
                        id,
                        WindowCommand::Topmost(crate::script::interp::truthy(&value)),
                    );
                    return true;
                }
                "windowMinSize" | "windowMaxSize" => {
                    if let SV::Array(a) = &value {
                        let items = a.borrow();
                        let as_f = |v: &SV| match v {
                            SV::Int(n) => Some(*n as f64),
                            SV::Float(f) => Some(*f),
                            _ => None,
                        };
                        if let (Some(w), Some(h)) = (
                            items.first().and_then(as_f),
                            items.get(1).and_then(as_f),
                        ) {
                            let cmd = if name == "windowMinSize" {
                                WindowCommand::MinSize(w, h)
                            } else {
                                WindowCommand::MaxSize(w, h)
                            };
                            push_window_command(id, cmd);
                        }
                    }
                    return true;
                }
                "windowResizable" => {
                    push_window_command(
                        id,
                        WindowCommand::Resizable(crate::script::interp::truthy(&value)),
                    );
                    return true;
                }
                "windowMaximizable" => {
                    push_window_command(
                        id,
                        WindowCommand::Maximizable(crate::script::interp::truthy(&value)),
                    );
                    return true;
                }
                "windowCaption" => {
                    if let SV::Str(s) = &value {
                        push_window_command(id, WindowCommand::Caption(s.to_string()));
                    }
                    return true;
                }
                "windowIcon" => {
                    if let SV::Str(s) = &value {
                        if let Some((w, h, rgba)) = crate::engine::paint::decode_data_uri_image(s)
                        {
                            push_window_command(id, WindowCommand::Icon(w, h, rgba));
                        }
                    }
                    return true;
                }
                "windowBlurbehind" | "windowFrame" => {
                    return true;
                }
                _ => {}
            }
            false
        }
        fn call_method(
            &self,
            interp: &mut Interp,
            name: &str,
            argv: &[SV],
        ) -> Option<SResult<SV>> {
            const ENGINE_METHODS: &[&str] = &[
                "screenBox",
                "box",
                "move",
                "focus",
                "close",
                "selectFile",
                "selectFolder",
                "clipboard",
                "on",
                "mediaVar",
                "window",
            ];
            if name == "doEvent" {
                if matches!(argv.first(), Some(SV::Symbol(s)) if s.as_ref() == "untilMouseUp") {
                    run_modal_until_mouse_up(interp, &self.engine);
                }
                return Some(Ok(SV::Undefined));
            }
            let is_engine = ENGINE_METHODS.contains(&name);
            if is_engine {
                if let Some(v) = self.overrides.get(name) {
                    let v = v.clone();
                    return Some(interp.call_value(&v, &SV::Undefined, argv));
                }
            }
            if let Some(h) = &self.handler {
                let values: Vec<crate::value::Value> =
                    argv.iter().map(crate::bridge::sv_to_value).collect();
                let result = h.borrow_mut().on_script_call(std::ptr::null_mut(), name, &values);
                if let Some(v) = result {
                    return Some(Ok(crate::bridge::value_to_sv(&v)));
                }
            }
            if let Some(v) = self.overrides.get(name) {
                let v = v.clone();
                return Some(interp.call_value(&v, &SV::Undefined, argv));
            }
            if name.starts_with("get_") {
                Some(Ok(SV::Str("".into())))
            } else if name.starts_with("is_") || name.starts_with("has_") || name.starts_with("can_")
            {
                Some(Ok(SV::Bool(false)))
            } else {
                Some(Ok(SV::Undefined))
            }
        }
    }

    sv_object(ObjectData {
        class: RefCell::new(None),
        props: RefCell::new(Vec::new()),
        native: Some(Gc::new(ViewNative {
            engine: engine.clone(),
            overrides: overrides.into_iter().collect(),
            handler,
        })),
    })
}

fn install_reactor(interp: &mut Interp, engine: &EngineRef) {
    if interp.global.lookup("Reactor").is_some() {
        return;
    }
    let component = ClassVal {
        name: "Component".into(),
        base: RefCell::new(None),
        methods: RefCell::new(HashMap::new()),
        class_props: RefCell::new(Vec::new()),
        events: RefCell::new(Vec::new()),
        class_env: RefCell::new(None),
    };
    {
        let mut m = component.methods.borrow_mut();
        let e_update = engine.clone();
        m.insert(
            "update".into(),
            native_fn("update", move |interp, this, argv| {
                // Sciter's Component.update(delta) merges delta into the component
                // before re-rendering; the folder-view refresh passes {fd:...} this
                // way, so without the merge the remote pane never got its file list.
                if let Some(SV::Object(o)) = argv.first() {
                    if o.native.is_none() {
                        let props: Vec<(String, SV)> =
                            o.props.borrow().iter().cloned().collect();
                        for (k, v) in props {
                            interp.member_set(this, &k, v)?;
                        }
                    }
                }
                let idx = match interp.member_get(this, "__mount")? {
                    SV::Int(i) => i as usize,
                    _ => return Ok(this.clone()),
                };
                let root = {
                    let e = e_update.borrow();
                    e.instance_roots.get(idx).copied()
                };
                let Some(root) = root else { return Ok(this.clone()) };
                // Replace the component's OWN root element in place (same
                // parent, same position); a stale root (its subtree was
                // re-rendered away by an ancestor) makes update a no-op.
                let ctx = {
                    let e = e_update.borrow();
                    e.doc.arena.get(root).and_then(|n| n.parent).and_then(|p| {
                        let pos = e
                            .doc
                            .arena
                            .get(p)?
                            .children
                            .iter()
                            .position(|&k| k == root)?;
                        Some((p, pos))
                    })
                };
                let Some((parent, pos)) = ctx else { return Ok(this.clone()) };
                // A focused text field inside this component must survive the
                // rebuild (e.g. the address-book search box re-renders on every
                // device update); record it so we can refocus the new one.
                let saved_focus = {
                    let e = e_update.borrow();
                    focused_input_path(&e, root)
                };
                // Selection / checked / scroll must survive too (Sciter patches
                // the DOM in place, so they persist there).
                let saved_state = {
                    let e = e_update.borrow();
                    subtree_state_snapshot(&e, root)
                };
                // Function expandos (onRowClick/onRowDoubleClick/onMouse set in
                // attached()) live on the element objects, not the DOM, so they
                // must be carried across the rebuild by id.
                let saved_expandos = {
                    let e = e_update.borrow();
                    snapshot_expandos(&e, root)
                };
                {
                    let mut e = e_update.borrow_mut();
                    e.doc.remove_subtree(root);
                }
                let before = {
                    let e = e_update.borrow();
                    e.doc.arena.get(parent).map(|n| n.children.len()).unwrap_or(0)
                };
                let rendered = interp.call_method(this, "render", &[])?;
                build_vnode(interp, &e_update, parent, &rendered)?;
                {
                    let mut e = e_update.borrow_mut();
                    let mut new_root = None;
                    if let Some(node) = e.doc.arena.get_mut(parent) {
                        if before <= node.children.len() {
                            let fresh: Vec<NodeKey> = node.children.split_off(before);
                            for (i, k) in fresh.iter().enumerate() {
                                node.children.insert(pos + i, *k);
                            }
                            new_root = Some(fresh);
                        }
                    }
                    if let Some(fresh) = new_root {
                        if let Some(r) = fresh
                            .into_iter()
                            .find(|k| e.doc.arena.get(*k).map_or(false, |c| !c.is_text()))
                        {
                            e.instance_roots[idx] = r;
                            restore_subtree_state(&mut e, r, &saved_state);
                            if let Some((path, val, tag)) = &saved_focus {
                                // Prefer the same positional path; if the rebuild
                                // shifted it (e.g. a filtered list changed the sibling
                                // count), fall back to the first input in the subtree.
                                let target = node_at_path(&e, r, path)
                                    .filter(|k| {
                                        e.doc.arena.get(*k).map_or(false, |n| n.tag == *tag)
                                    })
                                    .or_else(|| (tag == "input").then(|| first_input(&e, r)).flatten());
                                if let Some(input) = target {
                                    if let Some(node) = e.doc.arena.get_mut(input) {
                                        node.states.focus = true;
                                        node.set_attr("value", val);
                                    }
                                }
                            }
                        }
                    }
                }
                if let Some(nr) = e_update.borrow().instance_roots.get(idx).copied() {
                    restore_expandos(&e_update, nr, &saved_expandos);
                }
                Ok(this.clone())
            }),
        );
        let e_select = engine.clone();
        m.insert(
            "select".into(),
            native_fn("select", move |interp, this, argv| {
                let sel = to_display(argv.first().unwrap_or(&SV::Undefined));
                let idx = match interp.member_get(this, "__mount")? {
                    SV::Int(i) => i as usize,
                    _ => return Ok(SV::Null),
                };
                let found = {
                    let e = e_select.borrow();
                    e.instance_roots
                        .get(idx)
                        .copied()
                        .and_then(|root| e.doc.select_first(sel.trim(), root))
                };
                Ok(match found {
                    Some(k) => element_sv(&e_select, k),
                    None => SV::Null,
                })
            }),
        );
        let e_dollar = engine.clone();
        m.insert(
            "$".into(),
            native_fn("$", move |interp, this, argv| {
                let sel = to_display(argv.first().unwrap_or(&SV::Undefined));
                let idx = match interp.member_get(this, "__mount")? {
                    SV::Int(i) => i as usize,
                    _ => return Ok(SV::Null),
                };
                let found = {
                    let e = e_dollar.borrow();
                    e.instance_roots
                        .get(idx)
                        .copied()
                        .and_then(|root| e.doc.select_first(sel.trim(), root))
                };
                Ok(match found {
                    Some(k) => element_sv(&e_dollar, k),
                    None => SV::Null,
                })
            }),
        );
        for name in ["$$", "selectAll"] {
            let e_all = engine.clone();
            m.insert(
                name.into(),
                native_fn(name, move |interp, this, argv| {
                    let sel = to_display(argv.first().unwrap_or(&SV::Undefined));
                    let idx = match interp.member_get(this, "__mount")? {
                        SV::Int(i) => i as usize,
                        _ => return Ok(sv_array(Vec::new())),
                    };
                    let found: Vec<NodeKey> = {
                        let e = e_all.borrow();
                        e.instance_roots
                            .get(idx)
                            .copied()
                            .map(|root| e.doc.select_all(sel.trim(), root))
                            .unwrap_or_default()
                    };
                    let items: Vec<SV> =
                        found.into_iter().map(|k| element_sv(&e_all, k)).collect();
                    Ok(sv_array(items))
                }),
            );
        }
        let e_timer = engine.clone();
        m.insert(
            "timer".into(),
            native_fn("timer", move |_i, _t, argv| {
                let due = ms_of(argv.first().unwrap_or(&SV::Int(0)));
                let f = argv.get(1).cloned().unwrap_or(SV::Undefined);
                schedule_timer(&e_timer, due, f);
                Ok(SV::Bool(true))
            }),
        );
        m.insert(
            "render".into(),
            native_fn("render", |_i, _t, _a| Ok(SV::Null)),
        );
        m.insert(
            "attached".into(),
            native_fn("attached", |_i, _t, _a| Ok(SV::Undefined)),
        );
        m.insert(
            "post".into(),
            native_fn("post", |_i, _t, _a| Ok(SV::Undefined)),
        );
        m.insert(
            "content".into(),
            native_fn("content", |_i, _t, _a| Ok(SV::Undefined)),
        );
    }
    let reactor = new_object(vec![("Component".into(), SV::Class(Gc::new(component)))]);
    interp.global.define("Reactor", reactor);

    for name in ["Behavior", "Element"] {
        let class = ClassVal {
            name: name.into(),
            base: RefCell::new(None),
            methods: RefCell::new(HashMap::new()),
            class_props: RefCell::new(Vec::new()),
            events: RefCell::new(Vec::new()),
        class_env: RefCell::new(None),
        };
        interp.global.define(name, SV::Class(Gc::new(class)));
    }
}

pub fn attach_behaviors(engine: &EngineRef) {
    let factories: Vec<(String, Rc<dyn Fn() -> crate::bridge::SharedHandler>)> = {
        let e = engine.borrow();
        if e.behavior_factories.is_empty() {
            return;
        }
        e.behavior_factories.clone()
    };
    let styles = cached_computed_styles(engine);
    let mut to_attach: Vec<(NodeKey, String)> = Vec::new();
    {
        let e = engine.borrow();
        for (key, computed) in styles.iter() {
            if e.behavior_instances.contains_key(key) {
                continue;
            }
            if let Some(name) = &computed.behavior {
                if factories.iter().any(|(fname, _)| fname == name) {
                    to_attach.push((*key, name.clone()));
                }
            }
        }
    }
    for (key, name) in to_attach {
        if let Some((_, factory)) = factories.iter().find(|(fname, _)| *fname == name) {
            let handler = factory();
            handler
                .borrow_mut()
                .attached(key_as_helement(key));
            engine.borrow_mut().behavior_instances.insert(key, handler);
        }
    }
}

pub fn detach_behaviors(engine: &EngineRef) {
    let instances: Vec<_> = engine.borrow_mut().behavior_instances.drain().collect();
    for (key, handler) in instances {
        handler.borrow_mut().detached(key_as_helement(key));
    }
}

fn key_as_helement(key: NodeKey) -> crate::capi::scdom::HELEMENT {
    super::dom::key_to_helement(key)
}

thread_local! {
    static CURRENT_ENGINE: RefCell<Option<EngineRef>> = const { RefCell::new(None) };
    static CURRENT_INTERP: std::cell::Cell<*mut Interp> =
        const { std::cell::Cell::new(std::ptr::null_mut()) };
}

pub fn set_current_engine(engine: &EngineRef) {
    CURRENT_ENGINE.with(|c| *c.borrow_mut() = Some(engine.clone()));
}

pub fn current_engine() -> Option<EngineRef> {
    CURRENT_ENGINE.with(|c| c.borrow().clone())
}

pub fn set_current_interp(interp: *mut Interp) {
    CURRENT_INTERP.with(|c| c.set(interp));
}

pub struct InterpGuard;
impl Drop for InterpGuard {
    fn drop(&mut self) {
        set_current_interp(std::ptr::null_mut());
    }
}

thread_local! {
    static VIEW_CLOSE_REQUESTED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

static QUIT_REQUESTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// view.close() from script sets this; the window loop exits when it sees it set.
pub fn request_view_close() {
    VIEW_CLOSE_REQUESTED.with(|c| c.set(true));
}

/// Thread-safe teardown request usable from any thread (the client's quit_gui
/// runs off the UI thread). Wakes the UI loop so it notices even while idle.
pub fn request_quit() {
    QUIT_REQUESTED.store(true, std::sync::atomic::Ordering::SeqCst);
    let waker = OFFTHREAD_WAKER.lock().unwrap().clone();
    if let Some(w) = waker {
        w();
    }
}

pub fn take_view_close() -> bool {
    let tl = VIEW_CLOSE_REQUESTED.with(|c| c.replace(false));
    tl || QUIT_REQUESTED.swap(false, std::sync::atomic::Ordering::SeqCst)
}

thread_local! {
    // view.move(x,y,w,h) from script, applied by the window loop (the CM window
    // sizes itself to its content this way).
    static VIEW_MOVE: std::cell::Cell<Option<(f64, f64, f64, f64)>> = const { std::cell::Cell::new(None) };
}

pub fn request_view_move(x: f64, y: f64, w: f64, h: f64) {
    VIEW_MOVE.with(|c| c.set(Some((x, y, w, h))));
}

pub fn take_view_move() -> Option<(f64, f64, f64, f64)> {
    VIEW_MOVE.with(|c| c.replace(None))
}

thread_local! {
    static NATIVE_THEME: std::cell::Cell<Option<bool>> = const { std::cell::Cell::new(None) };
}

pub fn request_native_theme(dark: bool) {
    NATIVE_THEME.with(|c| c.set(Some(dark)));
}

pub fn take_native_theme() -> Option<bool> {
    NATIVE_THEME.with(|c| c.replace(None))
}

// A child window (the chat window) that view.window loaded into a new engine and
// wants the window loop to give a native OS window.
pub struct PendingChildWindow {
    pub engine: EngineRef,
    pub view: SV,
    pub width: u32,
    pub height: u32,
    pub title: String,
}

thread_local! {
    static VIEW_WINDOW: RefCell<Vec<PendingChildWindow>> = const { RefCell::new(Vec::new()) };
}

pub fn request_child_window(w: PendingChildWindow) {
    VIEW_WINDOW.with(|c| c.borrow_mut().push(w));
}
pub fn take_child_windows() -> Vec<PendingChildWindow> {
    VIEW_WINDOW.with(|c| std::mem::take(&mut *c.borrow_mut()))
}

#[derive(Clone)]
pub enum WindowCommand {
    State(i64),
    Topmost(bool),
    MinSize(f64, f64),
    MaxSize(f64, f64),
    Resizable(bool),
    Maximizable(bool),
    Caption(String),
    Icon(u32, u32, std::sync::Arc<Vec<u8>>),
    Activate,
    Monitor(i64),
}

// Identify which window (engine) a command targets, so a chat-window command
// doesn't drive the main window. Rc identity of the engine.
pub fn engine_id(e: &EngineRef) -> usize {
    std::rc::Rc::as_ptr(e) as *const () as usize
}

thread_local! {
    static WINDOW_COMMANDS: std::cell::RefCell<Vec<(usize, WindowCommand)>> =
        const { std::cell::RefCell::new(Vec::new()) };
    // Current OS window state (View.WINDOW_* code); the window loop keeps this in
    // sync each frame so `view.windowState` reads are accurate.
    static CURRENT_WINDOW_STATE: std::cell::Cell<i64> = const { std::cell::Cell::new(1) };
    // Work-area of the window's monitor in logical px (x,y,w,h) for view.screenBox.
    static MONITOR_RECT: std::cell::Cell<Option<(f64, f64, f64, f64)>> =
        const { std::cell::Cell::new(None) };
}

pub fn push_window_command(target: usize, c: WindowCommand) {
    WINDOW_COMMANDS.with(|q| q.borrow_mut().push((target, c)));
}
pub fn pending_window_state(target: usize) -> Option<i64> {
    WINDOW_COMMANDS.with(|q| {
        q.borrow().iter().rev().find_map(|(t, c)| match c {
            WindowCommand::State(n) if *t == target => Some(*n),
            _ => None,
        })
    })
}
pub fn take_window_commands() -> Vec<(usize, WindowCommand)> {
    WINDOW_COMMANDS.with(|q| std::mem::take(&mut *q.borrow_mut()))
}
pub fn set_current_window_state(s: i64) {
    CURRENT_WINDOW_STATE.with(|c| c.set(s));
}
pub fn current_window_state() -> i64 {
    CURRENT_WINDOW_STATE.with(|c| c.get())
}
pub fn set_monitor_rect(r: (f64, f64, f64, f64)) {
    MONITOR_RECT.with(|c| c.set(Some(r)));
}
pub fn monitor_rect() -> Option<(f64, f64, f64, f64)> {
    MONITOR_RECT.with(|c| c.get())
}

thread_local! {
    static SCREENS: std::cell::Cell<(i64, i64)> = const { std::cell::Cell::new((1, 0)) };
}
pub fn set_screens(count: i64, current: i64) {
    SCREENS.with(|c| c.set((count, current)));
}
pub fn screens() -> (i64, i64) {
    SCREENS.with(|c| c.get())
}

thread_local! {
    // The monitor workarea (minus taskbar/dock) in logical px. screenBox's
    // #workarea flavour must be smaller than #frame on Windows or the
    // "saved size >= workarea -> restore maximized" comparison in
    // index.tis/remote.tis can never pass.
    static WORKAREA_RECT: std::cell::Cell<Option<(f64, f64, f64, f64)>> =
        const { std::cell::Cell::new(None) };
}
pub fn set_workarea_rect(r: (f64, f64, f64, f64)) {
    WORKAREA_RECT.with(|c| c.set(Some(r)));
}
pub fn workarea_rect() -> Option<(f64, f64, f64, f64)> {
    WORKAREA_RECT.with(|c| c.get())
}

thread_local! {
    static SELECT_KEY_MODS: std::cell::Cell<(bool, bool, bool, bool)> =
        const { std::cell::Cell::new((false, false, false, false)) };
    static SELECT_ANCHOR: std::cell::Cell<Option<NodeKey>> = const { std::cell::Cell::new(None) };
    static SELECT_CURSOR: std::cell::Cell<Option<NodeKey>> = const { std::cell::Cell::new(None) };
}

pub fn grid_arrow_target(engine: &EngineRef, down: bool) -> Option<NodeKey> {
    let e = engine.try_borrow().ok()?;
    if let Some(f) = focused_element(&e) {
        if e.doc.arena.get(f).map_or(false, is_text_editable) {
            return None;
        }
    }
    let cursor = SELECT_CURSOR.with(|c| c.get())?;
    let tbody = e.doc.arena.get(cursor).and_then(|n| n.parent)?;
    let rows = match e.doc.arena.get(tbody) {
        Some(n) if n.tag == "tbody" => n.children.clone(),
        _ => return None,
    };
    let at = rows.iter().position(|&r| r == cursor)?;
    let next = if down { at.checked_add(1)? } else { at.checked_sub(1)? };
    rows.get(next).copied()
}
pub fn set_key_mods(m: (bool, bool, bool, bool)) {
    SELECT_KEY_MODS.with(|c| c.set(m));
}

const TYPE_AHEAD_MS: f64 = 1000.0;

struct TypeAhead {
    prefix: String,
    last_ms: f64,
}

thread_local! {
    static TYPE_AHEAD: std::cell::RefCell<TypeAhead> =
        std::cell::RefCell::new(TypeAhead { prefix: String::new(), last_ms: 0.0 });
}

fn monotonic_ms() -> f64 {
    static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    START.get_or_init(std::time::Instant::now).elapsed().as_secs_f64() * 1000.0
}

pub fn selected_grid(e: &Engine) -> Option<NodeKey> {
    let cursor = SELECT_CURSOR.with(|c| c.get())?;
    let mut table = None;
    let mut cur = Some(cursor);
    while let Some(k) = cur {
        let n = e.doc.arena.get(k)?;
        if n.tag == "table" && table.is_none() {
            table = Some(k);
        }
        if k == e.doc.root {
            return table;
        }
        cur = n.parent;
    }
    None
}

pub fn key_event_target(e: &Engine) -> NodeKey {
    focused_element(e)
        .or_else(|| selected_grid(e))
        .unwrap_or(e.doc.root)
}

pub fn grid_typeahead_target(engine: &EngineRef, ch: char) -> Option<NodeKey> {
    grid_typeahead_target_at(engine, ch, monotonic_ms())
}

pub fn grid_typeahead_target_at(engine: &EngineRef, ch: char, now_ms: f64) -> Option<NodeKey> {
    let ch = ch.to_lowercase().next()?;
    if ch.is_control() {
        return None;
    }
    let e = engine.try_borrow().ok()?;
    if let Some(f) = focused_element(&e) {
        if e.doc.arena.get(f).map_or(false, is_text_editable) {
            return None;
        }
    }
    let cursor = SELECT_CURSOR.with(|c| c.get())?;
    let tbody = e.doc.arena.get(cursor).and_then(|n| n.parent)?;
    let rows: Vec<NodeKey> = match e.doc.arena.get(tbody) {
        Some(n) if n.tag == "tbody" => n
            .children
            .iter()
            .copied()
            .filter(|&r| e.doc.arena.get(r).map_or(false, |x| x.tag == "tr"))
            .collect(),
        _ => return None,
    };
    if rows.is_empty() {
        return None;
    }
    let (prefix, cycle) = TYPE_AHEAD.with(|t| {
        let mut t = t.borrow_mut();
        let restart = t.prefix.is_empty() || now_ms - t.last_ms > TYPE_AHEAD_MS;
        t.last_ms = now_ms;
        let repeat = t.prefix.chars().count() == 1 && t.prefix.starts_with(ch);
        if restart {
            t.prefix = ch.to_string();
        } else if !repeat {
            t.prefix.push(ch);
        }
        (t.prefix.clone(), !restart && t.prefix.chars().count() == 1)
    });
    if prefix == " " {
        return None;
    }
    let at = rows.iter().position(|&r| r == cursor).unwrap_or(0);
    let start = if cycle { at + 1 } else { at };
    for n in 0..rows.len() {
        let row = rows[(start + n) % rows.len()];
        let name = row_name(&e.doc, row).trim().to_lowercase();
        if name.is_empty() || name == ".." {
            continue;
        }
        if name.starts_with(&prefix) {
            return Some(row);
        }
    }
    None
}

thread_local! {
    // The main window's (outer_x, outer_y, inner_w, inner_h) in logical px, for
    // view.box(#screen) so self.closing() can persist the window geometry.
    static WINDOW_RECT: std::cell::Cell<Option<(f64, f64, f64, f64)>> =
        const { std::cell::Cell::new(None) };
}
pub fn set_window_rect(r: (f64, f64, f64, f64)) {
    WINDOW_RECT.with(|c| c.set(Some(r)));
}
pub fn window_rect() -> Option<(f64, f64, f64, f64)> {
    WINDOW_RECT.with(|c| c.get())
}

pub fn node_in_subtree(doc: &Document, ancestor: NodeKey, mut key: NodeKey) -> bool {
    loop {
        if key == ancestor {
            return true;
        }
        match doc.arena.get(key).and_then(|n| n.parent) {
            Some(p) => key = p,
            None => return false,
        }
    }
}

// The <menu.context> a right-click on `from` should open: walk up the ancestors
// and return the first context menu found among an ancestor's direct children
// (the folder-view <table> holds its menu as `table > popup > menu.context`, or
// `table > menu.context` on OSX).
fn thead_headers(doc: &Document, table: NodeKey) -> Vec<NodeKey> {
    let mut out = Vec::new();
    for &sect in doc.arena.get(table).map(|n| &n.children).into_iter().flatten() {
        if doc.arena.get(sect).map_or(false, |n| n.tag == "thead") {
            for &row in doc.arena.get(sect).map(|n| &n.children).into_iter().flatten() {
                if doc.arena.get(row).map_or(false, |n| n.tag == "tr") {
                    for &cell in doc.arena.get(row).map(|n| &n.children).into_iter().flatten() {
                        if doc.arena.get(cell).map_or(false, |n| n.tag == "th") {
                            out.push(cell);
                        }
                    }
                }
            }
        }
    }
    out
}

fn tbody_of(doc: &Document, table: NodeKey) -> Option<NodeKey> {
    doc.arena
        .get(table)?
        .children
        .iter()
        .copied()
        .find(|&c| doc.arena.get(c).map_or(false, |n| n.tag == "tbody"))
}

pub fn grid_sort_column(doc: &Document, table: NodeKey) -> Option<NodeKey> {
    thead_headers(doc, table)
        .into_iter()
        .find(|&th| doc.arena.get(th).map_or(false, |n| n.attr("sort").is_some()))
}

fn row_cells(doc: &Document, row: NodeKey) -> Vec<NodeKey> {
    doc.arena
        .get(row)
        .map(|n| {
            n.children
                .iter()
                .copied()
                .filter(|&c| doc.arena.get(c).map_or(false, |x| x.tag == "td"))
                .collect()
        })
        .unwrap_or_default()
}

fn row_name(doc: &Document, row: NodeKey) -> String {
    row_cells(doc, row)
        .get(1)
        .map(|&c| doc.collect_text(c))
        .unwrap_or_default()
}

fn row_sort_key(doc: &Document, row: NodeKey, col: usize) -> (bool, bool, f64, String) {
    let cells = row_cells(doc, row);
    let name = row_name(doc, row);
    let is_parent = name.trim() == "..";
    let is_file = cells
        .first()
        .and_then(|&c| doc.arena.get(c))
        .and_then(|n| n.attr("type"))
        .and_then(|t| t.trim().parse::<i64>().ok())
        .map_or(false, |t| t > 3);
    let cell = match cells.get(col) {
        Some(&c) => c,
        None => return (is_parent, is_file, 0.0, String::new()),
    };
    match doc.arena.get(cell).and_then(|n| n.attr("value")) {
        Some(v) => (is_parent, is_file, v.trim().parse::<f64>().unwrap_or(0.0), String::new()),
        None => (is_parent, is_file, 0.0, doc.collect_text(cell).trim().to_lowercase()),
    }
}

pub fn grid_sort_rows(engine: &EngineRef, table: NodeKey, th: Option<NodeKey>, clicked: bool) -> bool {
    let mut e = engine.borrow_mut();
    let headers = thead_headers(&e.doc, table);
    let col_key = match th.or_else(|| grid_sort_column(&e.doc, table)) {
        Some(k) => k,
        None => return false,
    };
    let col = match headers.iter().position(|&k| k == col_key) {
        Some(i) => i,
        None => return false,
    };
    let current = e
        .doc
        .arena
        .get(col_key)
        .and_then(|n| n.attr("sort"))
        .map(|s| s.to_string());
    let desc_first = e.doc.arena.get(col_key).map_or(false, |n| n.attr("desc-first").is_some());
    let asc = match (clicked, current.as_deref()) {
        (true, None) => !desc_first,
        (true, Some(cur)) => cur != "asc",
        (false, cur) => cur == Some("asc"),
    };
    for &h in &headers {
        if let Some(n) = e.doc.arena.get_mut(h) {
            n.remove_attr("sort");
            n.states.current = false;
        }
    }
    if let Some(n) = e.doc.arena.get_mut(col_key) {
        n.set_attr("sort", if asc { "asc" } else { "desc" });
        n.states.current = true;
    }
    let body = match tbody_of(&e.doc, table) {
        Some(b) => b,
        None => return false,
    };
    let mut rows: Vec<NodeKey> = e
        .doc
        .arena
        .get(body)
        .map(|n| {
            n.children
                .iter()
                .copied()
                .filter(|&c| e.doc.arena.get(c).map_or(false, |x| x.tag == "tr"))
                .collect()
        })
        .unwrap_or_default();
    if rows.is_empty() {
        return false;
    }
    let mut keyed: Vec<(NodeKey, (bool, bool, f64, String))> = rows
        .drain(..)
        .map(|r| {
            let k = row_sort_key(&e.doc, r, col);
            (r, k)
        })
        .collect();
    keyed.sort_by(|a, b| {
        if a.1 .0 != b.1 .0 {
            return b.1 .0.cmp(&a.1 .0);
        }
        let ord = a.1 .1.cmp(&b.1 .1).then_with(|| {
            if a.1 .3.is_empty() && b.1 .3.is_empty() {
                a.1 .2.partial_cmp(&b.1 .2).unwrap_or(std::cmp::Ordering::Equal)
            } else {
                a.1 .3.cmp(&b.1 .3)
            }
        });
        if asc { ord } else { ord.reverse() }
    });
    let ordered: Vec<NodeKey> = keyed.into_iter().map(|(r, _)| r).collect();
    if let Some(n) = e.doc.arena.get_mut(body) {
        let others: Vec<NodeKey> = n
            .children
            .iter()
            .copied()
            .filter(|c| !ordered.contains(c))
            .collect();
        n.children = ordered.into_iter().chain(others).collect();
    }
    drop(e);
    super::dom::bump_layout_epoch();
    true
}

fn find_context_menu(doc: &Document, from: NodeKey) -> Option<NodeKey> {
    let is_ctx =
        |k: NodeKey| doc.arena.get(k).map_or(false, |n| n.tag == "menu" && n.has_class("context"));
    let mut cur = Some(from);
    while let Some(k) = cur {
        if let Some(n) = doc.arena.get(k) {
            for &child in &n.children {
                if is_ctx(child) {
                    return Some(child);
                }
                if doc.arena.get(child).map_or(false, |cn| cn.tag == "popup") {
                    if let Some(pn) = doc.arena.get(child) {
                        for &gc in &pn.children {
                            if is_ctx(gc) {
                                return Some(gc);
                            }
                        }
                    }
                }
            }
        }
        cur = doc.arena.get(k).and_then(|n| n.parent);
    }
    None
}

fn mark_row_current(engine: &mut Engine, from: NodeKey) {
    let mut tr = None;
    let mut cur = Some(from);
    while let Some(k) = cur {
        let (tag, parent, children) = match engine.doc.arena.get(k) {
            Some(n) => (n.tag.clone(), n.parent, n.children.clone()),
            None => return,
        };
        if tag == "tr" {
            tr = Some(k);
        }
        if tag == "tbody" {
            if let Some(row) = tr {
                for r in children {
                    if let Some(rn) = engine.doc.arena.get_mut(r) {
                        rn.states.current = r == row;
                    }
                }
            }
            return;
        }
        cur = parent;
    }
}

// Right-click opens the element's context menu (Sciter's built-in behavior). We
// reuse the `osx-popup-active` overlay CSS: unhide the <popup> wrapper, tag the
// menu, and pin it at the cursor. The right-clicked folder row becomes current
// so ctx-send/rename/delete act on it.
pub fn open_context_menu(engine: &EngineRef, from: NodeKey, cx: f32, cy: f32) -> bool {
    let menu = {
        let e = engine.borrow();
        match find_context_menu(&e.doc, from) {
            Some(m) => m,
            None => return false,
        }
    };
    close_context_menu(engine);
    let mut e = engine.borrow_mut();
    mark_row_current(&mut e, from);
    let popup_parent = e
        .doc
        .arena
        .get(menu)
        .and_then(|n| n.parent)
        .filter(|&p| e.doc.arena.get(p).map_or(false, |pn| pn.tag == "popup"));
    if let Some(p) = popup_parent {
        if let Some(pn) = e.doc.arena.get_mut(p) {
            pn.inline_style.retain(|(k, _)| k != "display");
            pn.inline_style.push(("display".into(), "block".into()));
        }
    }
    let viewport_h = e
        .screen_rects
        .get(&e.doc.root)
        .map(|r| r.3)
        .filter(|h| *h > 0.0)
        .unwrap_or(f32::MAX);
    let item_count = e
        .doc
        .arena
        .get(menu)
        .map(|n| {
            n.children
                .iter()
                .filter(|&&c| {
                    e.doc.arena.get(c).map_or(false, |cn| {
                        cn.tag == "li"
                            && !cn
                                .inline_style
                                .iter()
                                .any(|(k, v)| k == "display" && v == "none")
                    })
                })
                .count()
        })
        .unwrap_or(0);
    let est_h = item_count as f32 * 28.0 + 8.0;
    let top = if cy + est_h > viewport_h {
        (cy - est_h).max(0.0)
    } else {
        cy
    };
    if let Some(n) = e.doc.arena.get_mut(menu) {
        let mut classes: Vec<String> = n.classes().map(|c| c.to_string()).collect();
        if !classes.iter().any(|c| c == "osx-popup-active") {
            classes.push("osx-popup-active".into());
        }
        n.set_attr("class", &classes.join(" "));
        n.inline_style.retain(|(k, _)| k != "left" && k != "top");
        n.inline_style.push(("left".into(), format!("{}px", cx as i32)));
        n.inline_style.push(("top".into(), format!("{}px", top as i32)));
    }
    e.active_context_menu = Some(menu);
    true
}

pub fn close_context_menu(engine: &EngineRef) -> bool {
    let menu = match engine.borrow().active_context_menu {
        Some(m) => m,
        None => return false,
    };
    let mut e = engine.borrow_mut();
    if let Some(n) = e.doc.arena.get_mut(menu) {
        let classes: Vec<String> = n
            .classes()
            .map(|c| c.to_string())
            .filter(|c| c != "osx-popup-active")
            .collect();
        n.set_attr("class", &classes.join(" "));
        n.inline_style.retain(|(k, _)| k != "left" && k != "top");
    }
    let popup_parent = e
        .doc
        .arena
        .get(menu)
        .and_then(|n| n.parent)
        .filter(|&p| e.doc.arena.get(p).map_or(false, |pn| pn.tag == "popup"));
    if let Some(p) = popup_parent {
        if let Some(pn) = e.doc.arena.get_mut(p) {
            pn.inline_style.retain(|(k, _)| k != "display");
        }
    }
    e.active_context_menu = None;
    true
}

// True if `key` is a non-editable <select> (editable selects are free-text boxes
// like the file-transfer path, handled by normal input editing).
pub fn is_native_select(engine: &EngineRef, key: NodeKey) -> bool {
    let e = engine.borrow();
    e.doc
        .arena
        .get(key)
        .map_or(false, |n| n.tag == "select" && n.attr("editable").is_none())
}

// Open a native <select> as a synthesized fixed-position dropdown: a <div> list
// of the options placed under the control. Reuses the normal layout/paint/hit-
// test path (no custom overlay code), matching how the context menu works.
pub fn open_select_dropdown(engine: &EngineRef, select: NodeKey) -> bool {
    close_select_dropdown(engine);
    let (sx, sy, sw, sh, viewport_h, options) = {
        let e = engine.borrow();
        let (sx, sy, sw, sh) = match e.screen_rects.get(&select).copied() {
            Some(r) => r,
            None => return false,
        };
        let viewport_h = e
            .screen_rects
            .get(&e.doc.root)
            .map(|r| r.3)
            .filter(|h| *h > 0.0)
            .unwrap_or(f32::MAX);
        let node = match e.doc.arena.get(select) {
            Some(n) => n,
            None => return false,
        };
        let selected_val = current_select_value(&e.doc, select);
        let mut opts = Vec::new();
        for &c in &node.children {
            if let Some(opt) = e.doc.arena.get(c) {
                if opt.tag == "option" {
                    let label = e.doc.collect_text(c).trim().to_string();
                    let val = opt
                        .attr("value")
                        .map(|s| s.to_string())
                        .or_else(|| Some(label.clone()));
                    let is_sel = val.as_deref() == selected_val.as_deref();
                    opts.push((val, label, is_sel));
                }
            }
        }
        (sx, sy, sw, sh, viewport_h, opts)
    };
    if options.is_empty() {
        return false;
    }
    let est_h = options.len() as f32 * sh.min(32.0) + 8.0;
    let below = sy + sh;
    let top = if below + est_h > viewport_h && sy - est_h >= 0.0 {
        sy - est_h
    } else if below + est_h > viewport_h {
        (viewport_h - est_h).max(0.0)
    } else {
        below
    };
    let mut e = engine.borrow_mut();
    let popup = e.doc.create_element("div");
    if let Some(n) = e.doc.arena.get_mut(popup) {
        n.set_attr("class", "wireui-select-popup");
        n.inline_style = super::dom::parse_inline_style(&format!(
            "position: fixed; left: {}px; top: {}px; width: {}px; \
             background: white; border: 1px solid #b0b0b0; border-radius: 4px; \
             box-shadow: 0 4px 14px rgba(0,0,0,0.18); z-index: 10050; \
             padding: 3px 0; flow: vertical;",
            sx as i32,
            top as i32,
            sw.max(60.0) as i32,
        ));
    }
    for (val, label, is_sel) in options {
        let item = e.doc.create_element("div");
        if let Some(n) = e.doc.arena.get_mut(item) {
            n.set_attr("class", "wireui-select-opt");
            if let Some(v) = &val {
                n.set_attr("data-value", v);
            }
            let bg = if is_sel { "#e3f2fd" } else { "transparent" };
            n.inline_style = super::dom::parse_inline_style(&format!(
                "padding: 5px 10px; background: {}; white-space: nowrap; \
                 overflow: hidden; text-overflow: ellipsis;",
                bg
            ));
        }
        let text = e.doc.create_text(&label);
        e.doc.append_child(item, text);
        e.doc.append_child(popup, item);
    }
    let root = e.doc.root;
    e.doc.append_child(root, popup);
    e.open_select = Some((select, popup));
    true
}

// The value of the select right now: the assigned `value` attr if present, else
// the selected/first option's value.
fn current_select_value(doc: &Document, select: NodeKey) -> Option<String> {
    let node = doc.arena.get(select)?;
    if let Some(v) = node.attr("value").filter(|v| !v.is_empty()) {
        return Some(v.to_string());
    }
    let mut first = None;
    for &c in &node.children {
        if let Some(opt) = doc.arena.get(c) {
            if opt.tag == "option" {
                let val = opt.attr("value").map(|s| s.to_string());
                if first.is_none() {
                    first = val.clone();
                }
                if opt.attr("selected").is_some() {
                    return val;
                }
            }
        }
    }
    first
}

pub fn close_select_dropdown(engine: &EngineRef) -> bool {
    let popup = match engine.borrow().open_select {
        Some((_, p)) => p,
        None => return false,
    };
    let mut e = engine.borrow_mut();
    e.doc.remove_subtree(popup);
    e.open_select = None;
    true
}

// Right-click edit menu (Cut/Copy/Paste/Select All) over a text input, the
// native Sciter behaviour. Builds a synthesized fixed-position popup like the
// <select> dropdown; the actions reuse the caret/selection/clipboard machinery.
pub fn open_edit_menu(engine: &EngineRef, input: NodeKey, cx: f32, cy: f32) -> bool {
    close_edit_menu(engine);
    // Focus the input so selection/clipboard ops target it; keep any existing
    // selection (right-clicking selected text must not clear it).
    set_focus(engine, Some(input));
    // A real (non-collapsed) selection: selection_range reports a bare caret as a
    // zero-width range, so use selected_text which filters those out.
    let has_selection = { selected_text(&engine.borrow()).is_some() };
    // Paste is always offered (its action no-ops on an empty clipboard). Probing
    // the clipboard just to gray it out would open the OS clipboard on every
    // right-click -- contention we deliberately avoid (see the VM clipboard fix).
    let value_len = {
        let e = engine.borrow();
        e.doc
            .arena
            .get(input)
            .and_then(|n| n.attr("value"))
            .map(|v| v.chars().count())
            .unwrap_or(0)
    };
    // (action, label, enabled)
    let items = [
        ("cut", "Cut", has_selection),
        ("copy", "Copy", has_selection),
        ("paste", "Paste", true),
        ("selectall", "Select All", value_len > 0),
    ];
    let mut e = engine.borrow_mut();
    let popup = e.doc.create_element("div");
    if let Some(n) = e.doc.arena.get_mut(popup) {
        n.set_attr("class", "wireui-edit-menu");
        n.inline_style = super::dom::parse_inline_style(&format!(
            "position: fixed; left: {}px; top: {}px; min-width: 140px; \
             background: white; border: 1px solid #b0b0b0; border-radius: 6px; \
             box-shadow: 0 4px 14px rgba(0,0,0,0.18); z-index: 10060; \
             padding: 4px 0; flow: vertical;",
            cx as i32, cy as i32,
        ));
    }
    for (action, label, enabled) in items {
        let item = e.doc.create_element("div");
        if let Some(n) = e.doc.arena.get_mut(item) {
            n.set_attr("class", "wireui-edit-menu-item");
            n.set_attr("data-action", action);
            let color = if enabled { "#212529" } else { "#adb5bd" };
            if !enabled {
                n.set_attr("data-disabled", "1");
            }
            n.inline_style = super::dom::parse_inline_style(&format!(
                "padding: 5px 16px; color: {}; white-space: nowrap; font-size: 13px;",
                color
            ));
        }
        let text = e.doc.create_text(label);
        e.doc.append_child(item, text);
        e.doc.append_child(popup, item);
    }
    let root = e.doc.root;
    e.doc.append_child(root, popup);
    e.edit_menu = Some((input, popup));
    true
}

pub fn close_edit_menu(engine: &EngineRef) -> bool {
    let popup = match engine.borrow().edit_menu {
        Some((_, p)) => p,
        None => return false,
    };
    let mut e = engine.borrow_mut();
    e.doc.remove_subtree(popup);
    e.edit_menu = None;
    true
}

// Highlight the hovered enabled item; clear the rest. Returns true on a change.
pub fn edit_menu_hover(engine: &EngineRef, hit: Option<NodeKey>) -> bool {
    let popup = match engine.borrow().edit_menu {
        Some((_, p)) => p,
        None => return false,
    };
    let hovered = hit.and_then(|h| {
        let e = engine.borrow();
        let mut cur = Some(h);
        while let Some(k) = cur {
            let n = e.doc.arena.get(k)?;
            if n.has_class("wireui-edit-menu-item") {
                return if n.attr("data-disabled").is_some() { None } else { Some(k) };
            }
            cur = n.parent;
        }
        None
    });
    let mut e = engine.borrow_mut();
    let items: Vec<NodeKey> = e.doc.arena.get(popup).map(|n| n.children.clone()).unwrap_or_default();
    let mut changed = false;
    for item in items {
        let want = if Some(item) == hovered { "#e8f0fe" } else { "transparent" };
        if let Some(n) = e.doc.arena.get_mut(item) {
            let cur = n
                .inline_style
                .iter()
                .find(|(k, _)| k == "background")
                .map(|(_, v)| v.clone())
                .unwrap_or_default();
            if cur != want {
                n.inline_style.retain(|(k, _)| k != "background");
                n.inline_style.push(("background".into(), want.into()));
                changed = true;
            }
        }
    }
    changed
}

// Apply a click while the edit menu is open. Returns true when handled.
pub fn edit_menu_click(engine: &EngineRef, hit: Option<NodeKey>) -> bool {
    let input = match engine.borrow().edit_menu {
        Some((i, _)) => i,
        None => return false,
    };
    let action = hit.and_then(|h| {
        let e = engine.borrow();
        let mut cur = Some(h);
        while let Some(k) = cur {
            let n = e.doc.arena.get(k)?;
            if n.has_class("wireui-edit-menu-item") {
                if n.attr("data-disabled").is_some() {
                    return None;
                }
                return n.attr("data-action").map(|s| s.to_string());
            }
            cur = n.parent;
        }
        None
    });
    close_edit_menu(engine);
    set_focus(engine, Some(input));
    match action.as_deref() {
        Some("cut") => {
            let sel = { selected_text(&engine.borrow()) };
            if let Some(t) = sel {
                let _ = arboard::Clipboard::new().and_then(|mut c| c.set_text(t));
                insert_text(engine, "");
            }
        }
        Some("copy") => {
            let sel = { selected_text(&engine.borrow()) };
            if let Some(t) = sel {
                let _ = arboard::Clipboard::new().and_then(|mut c| c.set_text(t));
            }
        }
        Some("paste") => {
            if let Ok(t) = arboard::Clipboard::new().and_then(|mut c| c.get_text()) {
                let clean: String = t.chars().filter(|c| !c.is_control()).collect();
                if !clean.is_empty() {
                    insert_text(engine, &clean);
                }
            }
        }
        Some("selectall") => {
            select_all(engine);
        }
        _ => {}
    }
    true
}

// Apply a click at (x,y) while a select dropdown is open. Returns true if it was
// handled (option chosen or dismissed) so the caller consumes the click.
pub fn select_dropdown_click(
    interp: &mut Interp,
    engine: &EngineRef,
    hit: Option<NodeKey>,
) -> bool {
    let (select, _popup) = match engine.borrow().open_select {
        Some(v) => v,
        None => return false,
    };
    // Did the click land on one of our option rows (or its text child)?
    let chosen = hit.and_then(|h| {
        let e = engine.borrow();
        let mut cur = Some(h);
        while let Some(k) = cur {
            let n = e.doc.arena.get(k)?;
            if n.has_class("wireui-select-opt") {
                return Some((k, n.attr("data-value").map(|s| s.to_string())));
            }
            cur = n.parent;
        }
        None
    });
    close_select_dropdown(engine);
    if let Some((_item, value)) = chosen {
        let changed = {
            let mut e = engine.borrow_mut();
            let cur = current_select_value(&e.doc, select);
            if cur == value {
                false
            } else {
                // Move the `selected` marker to the chosen option and store the
                // value on the select so .value reads it immediately.
                let opt_children: Vec<NodeKey> = e
                    .doc
                    .arena
                    .get(select)
                    .map(|n| n.children.clone())
                    .unwrap_or_default();
                for c in opt_children {
                    if let Some(opt) = e.doc.arena.get_mut(c) {
                        if opt.tag == "option" {
                            let matches = opt.attr("value").map(|s| s.to_string()) == value;
                            if matches {
                                opt.set_attr("selected", "");
                            } else {
                                opt.remove_attr("selected");
                            }
                        }
                    }
                }
                if let Some(sn) = e.doc.arena.get_mut(select) {
                    sn.set_attr("value", value.as_deref().unwrap_or(""));
                }
                true
            }
        };
        if changed {
            dispatch_dom_event(interp, engine, "change", select).ok();
            drain_events(interp, engine);
        }
    }
    true
}

pub fn element_location(h: crate::capi::scdom::HELEMENT) -> Option<crate::capi::sctypes::RECT> {
    let key = super::dom::helement_to_key(h)?;
    CURRENT_ENGINE.with(|c| {
        let e = c.borrow();
        let e = e.as_ref()?;
        let e = e.borrow();
        e.last_rects
            .get(&key)
            .map(|(x, y, w, h)| crate::capi::sctypes::RECT {
                left: *x as i32,
                top: *y as i32,
                right: (*x + *w) as i32,
                bottom: (*y + *h) as i32,
            })
    })
}

pub fn element_display(h: crate::capi::scdom::HELEMENT) -> Option<String> {
    let key = super::dom::helement_to_key(h)?;
    CURRENT_ENGINE.with(|c| {
        let e = c.borrow();
        let e = e.as_ref()?;
        let e = e.borrow();
        let node = e.doc.arena.get(key)?;
        let mut out = node.tag.clone();
        if let Some(id) = node.id() {
            out.push('#');
            out.push_str(id);
        }
        for class in node.classes() {
            out.push('.');
            out.push_str(class);
        }
        Some(out)
    })
}

// Off-thread Element::call_method marshaling. The client's io thread calls
// element.call_method(...) (setDisplay, cancel_msgbox, updateToolbar, ...) to
// drive the UI; the script interpreter is single-threaded and lives on the UI
// thread, so those calls are queued here (Value is Send) and run on the UI
// thread when it next wakes. Fire-and-forget: the caller gets null immediately
// (every off-thread call site discards the result).
type OffthreadCall = (usize, String, Vec<crate::value::Value>);
static OFFTHREAD_CALLS: std::sync::Mutex<Vec<OffthreadCall>> = std::sync::Mutex::new(Vec::new());
static OFFTHREAD_WAKER: std::sync::Mutex<Option<std::sync::Arc<dyn Fn() + Send + Sync>>> =
    std::sync::Mutex::new(None);

pub fn on_ui_thread() -> bool {
    !CURRENT_INTERP.with(|c| c.get()).is_null()
}

pub fn set_offthread_waker(w: std::sync::Arc<dyn Fn() + Send + Sync>) {
    *OFFTHREAD_WAKER.lock().unwrap() = Some(w.clone());
    if !OFFTHREAD_CALLS.lock().unwrap().is_empty() {
        w();
    }
}

pub fn queue_offthread_call(h: crate::capi::scdom::HELEMENT, name: &str, args: Vec<crate::value::Value>) {
    OFFTHREAD_CALLS
        .lock()
        .unwrap()
        .push((h as usize, name.to_string(), args));
    let waker = OFFTHREAD_WAKER.lock().unwrap().clone();
    if let Some(w) = waker {
        w();
    }
}

pub fn drain_offthread_calls(_interp: &mut Interp, engine: &EngineRef) {
    let calls = std::mem::take(&mut *OFFTHREAD_CALLS.lock().unwrap());
    for (he, name, args) in calls {
        let sv_args: Vec<SV> = args.iter().map(crate::bridge::value_to_sv).collect();
        set_current_engine(engine);
        let _ = element_call_method(he as crate::capi::scdom::HELEMENT, &name, &sv_args);
    }
}

/// The OS cursor for the element under the pointer: walk up from the hit node to
/// the first element with an explicit `cursor` (Sciter/CSS effectively inherits
/// it down), mapping to a winit icon. None hit or all-inherit -> arrow.
pub fn cursor_at(engine: &EngineRef, hit: Option<NodeKey>, x: f32) -> winit::window::CursorIcon {
    use crate::engine::style::Cursor;
    use winit::window::CursorIcon;
    let Some(hit) = hit else { return CursorIcon::Default };
    let e = engine.borrow();
    let mut cur = Some(hit);
    while let Some(k) = cur {
        let explicit = e.hover_cursors.get(&k).map_or(false, |c| *c != Cursor::Inherit);
        if !explicit && e.doc.arena.get(k).map_or(false, takes_typed_text) {
            let on_chevron = e.doc.arena.get(k).map_or(false, |n| n.tag == "select")
                && e.screen_rects.get(&k).map_or(false, |r| x >= r.0 + r.2 - SELECT_CHEVRON_HIT);
            return if on_chevron { CursorIcon::Default } else { CursorIcon::Text };
        }
        if let Some(c) = e.hover_cursors.get(&k) {
            if *c != Cursor::Inherit {
                return match c {
                    Cursor::Pointer => CursorIcon::Pointer,
                    Cursor::Text => CursorIcon::Text,
                    Cursor::Move => CursorIcon::Move,
                    Cursor::Wait => CursorIcon::Wait,
                    Cursor::NotAllowed => CursorIcon::NotAllowed,
                    Cursor::ColResize => CursorIcon::ColResize,
                    Cursor::RowResize => CursorIcon::RowResize,
                    Cursor::Default | Cursor::Inherit => CursorIcon::Default,
                };
            }
        }
        cur = e.doc.arena.get(k).and_then(|n| n.parent);
    }
    CursorIcon::Default
}

/// A mounted Reactor component IS its root element in Sciter, so `comp.value` /
/// `comp.text` etc. read/write the root DOM element while `comp.update()` stays
/// a component method. Resolve the root element object for a component instance
/// (an object carrying `__mount`), or None for a plain object.
pub fn component_root_element(instance: &SV) -> Option<SV> {
    let idx = match instance {
        SV::Object(o) => o
            .props
            .borrow()
            .iter()
            .find(|(k, _)| k == "__mount")
            .and_then(|(_, v)| match v {
                SV::Int(i) => Some(*i as usize),
                _ => None,
            })?,
        _ => return None,
    };
    let engine = CURRENT_ENGINE.with(|c| c.borrow().clone())?;
    let root = engine.borrow().instance_roots.get(idx).copied()?;
    if !engine.borrow().doc.arena.contains_key(root) {
        return None;
    }
    Some(element_sv(&engine, root))
}

pub fn element_call_method(
    h: crate::capi::scdom::HELEMENT,
    name: &str,
    args: &[SV],
) -> Result<SV, ()> {
    let key = super::dom::helement_to_key(h).ok_or(())?;
    let engine = CURRENT_ENGINE.with(|c| c.borrow().clone()).ok_or(())?;
    if !engine.borrow().doc.arena.contains_key(key) {
        return Err(());
    }
    let interp_ptr = CURRENT_INTERP.with(|c| c.get());
    if interp_ptr.is_null() {
        return Err(());
    }
    let el = element_sv(&engine, key);
    // SAFETY: single UI thread; the interp is alive for as long as the guard
    // that published this pointer is on the stack.
    let interp = unsafe { &mut *interp_ptr };
    interp.call_method(&el, name, args).map_err(|_| ())
}

pub fn bind_video(engine: &EngineRef) {
    let video_keys: Vec<NodeKey> = {
        let e = engine.borrow();
        e.doc
            .descendants(e.doc.root)
            .into_iter()
            .filter(|k| {
                e.doc.arena.get(*k).map_or(false, |n| n.tag == "video")
                    && !e.video_sinks.contains_key(k)
            })
            .collect()
    };
    for key in video_keys {
        let handler = engine.borrow().behavior_instances.get(&key).cloned();
        let handler = match handler {
            Some(h) => h,
            None => continue,
        };
        let sink = crate::video::new_frame_sink();
        if let Some(wake) = engine.borrow().video_wake.clone() {
            sink.lock().unwrap().wake = Some(wake);
        }
        engine.borrow_mut().video_sinks.insert(key, sink.clone());
        let ptr = crate::video::video_destination::boxed(sink);
        let root = key_as_helement(key);
        handler.borrow_mut().on_event(
            root,
            root,
            root,
            crate::capi::scbehavior::BEHAVIOR_EVENTS::VIDEO_BIND_RQ,
            crate::capi::scbehavior::PHASE_MASK::BUBBLING,
            crate::dom::event::EventReason::VideoBind(ptr as *mut std::ffi::c_void),
        );
        handler.borrow_mut().on_event(
            root,
            root,
            root,
            crate::capi::scbehavior::BEHAVIOR_EVENTS::VIDEO_INITIALIZED,
            crate::capi::scbehavior::PHASE_MASK::BUBBLING,
            crate::dom::event::EventReason::General(
                crate::capi::scbehavior::CLICK_REASON::SYNTHESIZED,
            ),
        );
    }
}

pub fn install_video_wake(engine: &EngineRef, waker: crate::video::FrameWaker) {
    let sinks: Vec<crate::video::FrameSink> = {
        let mut e = engine.borrow_mut();
        e.video_wake = Some(waker.clone());
        e.video_sinks.values().cloned().collect()
    };
    for sink in sinks {
        sink.lock().unwrap().wake = Some(waker.clone());
    }
}

pub fn pump_video_events(engine: &EngineRef) {
    let transitions: Vec<(NodeKey, bool)> = {
        let e = engine.borrow();
        e.video_sinks
            .iter()
            .filter_map(|(key, sink)| {
                let now = sink.lock().ok()?.streaming;
                let last = e.video_streaming.get(key).copied().unwrap_or(false);
                if now != last {
                    Some((*key, now))
                } else {
                    None
                }
            })
            .collect()
    };
    for (key, streaming) in transitions {
        engine.borrow_mut().video_streaming.insert(key, streaming);
        let handler = engine.borrow().behavior_instances.get(&key).cloned();
        let handler = match handler {
            Some(h) => h,
            None => continue,
        };
        let root = key_as_helement(key);
        let code = if streaming {
            crate::capi::scbehavior::BEHAVIOR_EVENTS::VIDEO_STARTED
        } else {
            crate::capi::scbehavior::BEHAVIOR_EVENTS::VIDEO_STOPPED
        };
        handler.borrow_mut().on_event(
            root,
            root,
            root,
            code,
            crate::capi::scbehavior::PHASE_MASK::BUBBLING,
            crate::dom::event::EventReason::General(
                crate::capi::scbehavior::CLICK_REASON::SYNTHESIZED,
            ),
        );
    }
}

pub fn drain_events(interp: &mut Interp, engine: &EngineRef) {
    if interp.pending_events.is_empty() {
        return;
    }
    let drained = std::mem::take(&mut interp.pending_events);
    let mut e = engine.borrow_mut();
    for pe in drained {
        e.events.push(EventBinding {
            name: pe.name,
            selector: pe.selector,
            func: pe.func,
            scope: pe.this,
        });
    }
}

fn claim_pending_events(interp: &mut Interp, engine: &EngineRef, from: usize) {
    if interp.pending_events.len() <= from {
        return;
    }
    let claimed: Vec<_> = interp.pending_events.drain(from..).collect();
    let mut e = engine.borrow_mut();
    for pe in claimed {
        e.events.push(EventBinding {
            name: pe.name,
            selector: pe.selector,
            func: pe.func,
            scope: pe.this,
        });
    }
}

fn node_matches_selector(engine: &Engine, key: NodeKey, selector: &str) -> bool {
    let sels = super::css::parse_selector_list(selector);
    sels.iter()
        .any(|s| super::css::matches(&engine.doc, key, s))
}

pub fn dispatch_dom_event(
    interp: &mut Interp,
    engine: &EngineRef,
    event_name: &str,
    target: NodeKey,
) -> SResult<bool> {
    dispatch_dom_event_with(interp, engine, event_name, target, &[])
}

// Dispatch with extra evt properties (keyCode/wheelDeltas/modifiers) -- the
// terminal's keydown/keypress/mousewheel handlers read them off the event.
pub fn dispatch_dom_event_with(
    interp: &mut Interp,
    engine: &EngineRef,
    event_name: &str,
    target: NodeKey,
    extra: &[(String, SV)],
) -> SResult<bool> {
    // Label activation: clicking anywhere in a <label> that wraps a checkbox
    // toggles the (usually hidden) input and retargets the click to it, so
    // handlers keyed on the input fire and :checked styling updates.
    let mut target = target;
    if event_name == "click" {
        let checkbox = {
            let e = engine.borrow();
            let mut label = None;
            let mut cur = Some(target);
            while let Some(k) = cur {
                let n = e.doc.arena.get(k);
                if n.map_or(false, |n| n.tag == "label") {
                    label = Some(k);
                    break;
                }
                cur = n.and_then(|n| n.parent);
            }
            label.and_then(|l| {
                e.doc.descendants(l).into_iter().find(|&d| {
                    e.doc.arena.get(d).map_or(false, |n| {
                        (n.tag == "input" || n.tag == "button")
                            && n.attr("type") == Some("checkbox")
                    })
                })
            })
        };
        if let Some(cb) = checkbox {
            let mut e = engine.borrow_mut();
            if let Some(node) = e.doc.arena.get_mut(cb) {
                node.states.checked = !node.states.checked;
            }
            drop(e);
            target = cb;
        } else {
            let is_checkbox = {
                let e = engine.borrow();
                e.doc.arena.get(target).map_or(false, |n| {
                    (n.tag == "input" || n.tag == "button")
                        && n.attr("type") == Some("checkbox")
                })
            };
            if is_checkbox {
                let mut e = engine.borrow_mut();
                if let Some(node) = e.doc.arena.get_mut(target) {
                    node.states.checked = !node.states.checked;
                }
            }
        }
    }
    let target = target;
    let mut ancestry: Vec<NodeKey> = Vec::new();
    {
        let e = engine.borrow();
        let mut cur = Some(target);
        while let Some(k) = cur {
            ancestry.push(k);
            cur = e.doc.arena.get(k).and_then(|n| n.parent);
        }
    }
    // Resolve each matching binding's scope to a live subtree root. A component
    // instance scopes its class-level handlers to where it rendered; a stale
    // instance (its mount parent re-rendered away) never fires. self/view (or
    // anything without __mount) is document-wide.
    let bindings: Vec<(usize, SV, Option<NodeKey>)> = {
        let raw: Vec<(usize, SV, SV)> = {
            let e = engine.borrow();
            e.events
                .iter()
                .enumerate()
                .filter(|(_, b)| b.name == event_name)
                .map(|(i, b)| (i, b.func.clone(), b.scope.clone()))
                .collect()
        };
        let mut out = Vec::with_capacity(raw.len());
        for (i, func, scope) in raw {
            let mount = match &scope {
                SV::Object(_) => match interp.member_get(&scope, "__mount") {
                    Ok(SV::Int(idx)) => {
                        let e = engine.borrow();
                        match e.instance_roots.get(idx as usize).copied() {
                            Some(root) if e.doc.arena.get(root).is_some() => Some(Some(root)),
                            _ => None,
                        }
                    }
                    _ => Some(None),
                },
                _ => Some(None),
            };
            if let Some(scope_root) = mount {
                if scope_root.map_or(true, |r| ancestry.contains(&r)) {
                    out.push((i, func, scope_root));
                }
            }
        }
        out
    };
    let elem_handlers: Vec<(NodeKey, Option<String>, SV)> = {
        let e = engine.borrow();
        e.element_handlers
            .iter()
            .filter(|(_, n, _, _)| n == event_name)
            .map(|(k, _, sel, f)| (*k, sel.clone(), f.clone()))
            .collect()
    };
    let mut handled = false;
    let mut first_err: Option<crate::script::interp::Thrown> = None;
    let fire = |interp: &mut Interp,
                    func: &SV,
                    this: &SV,
                    node: NodeKey,
                    handled: &mut bool,
                    first_err: &mut Option<crate::script::interp::Thrown>| {
        let evt = make_event_object_with(engine, target, event_name, extra);
        let el = element_sv(engine, node);
        match interp.call_value(func, this, &[evt, el]) {
            Ok(r) => {
                if crate::script::interp::truthy(&r) {
                    *handled = true;
                }
            }
            Err(err) => {
                // One faulty handler must not kill the rest of the chain
                // (Sciter logs script errors and keeps dispatching).
                eprintln!("{} handler error: {}", event_name, err.0);
                if first_err.is_none() {
                    *first_err = Some(err);
                }
            }
        }
    };
    for &node in &ancestry {
        for (i, func, _scope) in &bindings {
            let matches = {
                let e = engine.borrow();
                match &e.events[*i].selector {
                    None => node == e.doc.root,
                    Some(sel) => node_matches_selector(&e, node, sel),
                }
            };
            if !matches {
                continue;
            }
            fire(interp, func, &SV::Undefined, node, &mut handled, &mut first_err);
        }
        for (hkey, sel, func) in &elem_handlers {
            match sel {
                // Plain .on: fires when bubbling reaches the element itself.
                None => {
                    if *hkey != node {
                        continue;
                    }
                    let el = element_sv(engine, node);
                    fire(interp, func, &el, node, &mut handled, &mut first_err);
                }
                // Delegated .on(event, selector, fn): registered on an ancestor,
                // fires when a node between target and that ancestor matches.
                Some(sel) => {
                    if !ancestry.contains(hkey) {
                        continue;
                    }
                    let is_match = {
                        let e = engine.borrow();
                        node_matches_selector(&e, node, sel)
                    };
                    if !is_match {
                        continue;
                    }
                    let el = element_sv(engine, node);
                    fire(interp, func, &el, node, &mut handled, &mut first_err);
                }
            }
        }
        if handled {
            break;
        }
    }
    // Grid prototype: a folder-view <table> assigns onRowClick/onRowDoubleClick
    // (in FolderView.attached); Sciter's Grid invokes them when a body row is
    // clicked. Reproduce that so files/folders respond to click and double-click.
    if event_name == "click" {
        let hit = {
            let e = engine.borrow();
            let mut th = None;
            let mut tbl = None;
            let mut cur = Some(target);
            while let Some(k) = cur {
                let n = match e.doc.arena.get(k) {
                    Some(n) => n,
                    None => break,
                };
                if n.tag == "th" && n.has_class("sortable") {
                    th = Some(k);
                }
                if n.tag == "table" {
                    tbl = Some(k);
                    break;
                }
                cur = n.parent;
            }
            th.zip(tbl)
        };
        if let Some((th, table)) = hit {
            if grid_sort_rows(engine, table, Some(th), true) {
                return Ok(true);
            }
        }
    }
    if event_name == "click" || event_name == "dblclick" {
        let (tr, table) = {
            let e = engine.borrow();
            let (mut tr, mut table) = (None, None);
            let mut cur = Some(target);
            while let Some(k) = cur {
                let n = match e.doc.arena.get(k) {
                    Some(n) => n,
                    None => break,
                };
                if n.tag == "tr" && tr.is_none() {
                    tr = Some(k);
                }
                if n.tag == "table" {
                    table = Some(k);
                    break;
                }
                cur = n.parent;
            }
            (tr, table)
        };
        if let (Some(tr), Some(table)) = (tr, table) {
            let (in_body, tbody) = {
                let e = engine.borrow();
                let parent = e.doc.arena.get(tr).and_then(|n| n.parent);
                let in_body = parent
                    .and_then(|p| e.doc.arena.get(p))
                    .map_or(false, |p| p.tag == "tbody");
                (in_body, parent)
            };
            if in_body {
                if let Some(tbody) = tbody {
                    let mut e = engine.borrow_mut();
                    let rows: Vec<NodeKey> = e
                        .doc
                        .arena
                        .get(tbody)
                        .map(|n| n.children.clone())
                        .unwrap_or_default();
                    let mods = SELECT_KEY_MODS.with(|c| c.get());
                    let toggle = if cfg!(target_os = "macos") { mods.3 } else { mods.1 };
                    let range = mods.2;
                    let anchor = SELECT_ANCHOR.with(|c| c.get());
                    let anchor_idx = anchor.and_then(|a| rows.iter().position(|&r| r == a));
                    if range && anchor_idx.is_some() {
                        let ai = anchor_idx.unwrap();
                        let ti = rows.iter().position(|&r| r == tr).unwrap_or(ai);
                        let (lo, hi) = if ai <= ti { (ai, ti) } else { (ti, ai) };
                        for (i, &r) in rows.iter().enumerate() {
                            if let Some(rn) = e.doc.arena.get_mut(r) {
                                rn.states.current = i >= lo && i <= hi;
                            }
                        }
                    } else if toggle {
                        let was = e.doc.arena.get(tr).map_or(false, |n| n.states.current);
                        if let Some(rn) = e.doc.arena.get_mut(tr) {
                            rn.states.current = !was;
                        }
                        SELECT_ANCHOR.with(|c| c.set(Some(tr)));
                    } else {
                        for &r in &rows {
                            if let Some(rn) = e.doc.arena.get_mut(r) {
                                rn.states.current = r == tr;
                            }
                        }
                        SELECT_ANCHOR.with(|c| c.set(Some(tr)));
                    }
                    SELECT_CURSOR.with(|c| c.set(Some(tr)));
                }
                let handler_name = if event_name == "dblclick" {
                    "onRowDoubleClick"
                } else {
                    "onRowClick"
                };
                let handler = match element_sv(engine, table) {
                    SV::Object(o) => o
                        .props
                        .borrow()
                        .iter()
                        .find(|(k, _)| k == handler_name)
                        .map(|(_, v)| v.clone()),
                    _ => None,
                };
                if let Some(h) = handler {
                    let row = element_sv(engine, tr);
                    let args = if event_name == "dblclick" {
                        vec![row]
                    } else {
                        vec![row, SV::Int(0)]
                    };
                    if let Err(err) = interp.call_value(&h, &SV::Undefined, &args) {
                        eprintln!("{}: {}", handler_name, err.0);
                    }
                }
            }
        }
    }
    drain_events(interp, engine);
    Ok(handled)
}

fn make_event_object_with(
    engine: &EngineRef,
    target: NodeKey,
    name: &str,
    extra: &[(String, SV)],
) -> SV {
    let mut props = vec![
        ("type".into(), SV::Str(name.into())),
        ("target".into(), element_sv(engine, target)),
        ("reason".into(), SV::Int(0)),
    ];
    for (k, v) in extra {
        props.push((k.clone(), v.clone()));
    }
    new_object(props)
}

// element.subscribe(fn, Event.MOUSE) delivery: every raw mouse event goes to
// each subscribed handler twice -- sinking phase (type | 0x8000) first, then
// bubbling -- so the file-transfer drag tracker sees rows' mousedowns before
// tbody's select behavior. Truthy return consumes.
pub fn dispatch_mouse_subscriptions(
    interp: &mut Interp,
    engine: &EngineRef,
    etype: i64,
    hit: Option<NodeKey>,
    x: f32,
    y: f32,
) -> bool {
    let subs: Vec<SV> = engine
        .borrow()
        .subscriptions
        .iter()
        .filter(|(_, mask, _)| mask & 0x1 != 0)
        .map(|(_, _, f)| f.clone())
        .collect();
    if subs.is_empty() {
        return false;
    }
    let target = hit.unwrap_or_else(|| engine.borrow().doc.root);
    let mut consumed = false;
    for phase in [0x8000i64, 0] {
        for f in &subs {
            let evt = new_object(vec![
                ("type".into(), SV::Int(etype | phase)),
                ("target".into(), element_sv(engine, target)),
                ("xView".into(), SV::Int(x as i64)),
                ("yView".into(), SV::Int(y as i64)),
                ("reason".into(), SV::Int(0)),
            ]);
            match interp.call_value(f, &SV::Undefined, &[evt]) {
                Ok(v) => {
                    if crate::script::interp::truthy(&v) {
                        consumed = true;
                    }
                }
                Err(e) => eprintln!("subscription handler error: {}", e.0),
            }
        }
        if consumed {
            break;
        }
    }
    consumed
}

// Whole-document computed styles, memoized per layout epoch. For script-side
// reads only: showPopupMenu probes style#overflow on a dozen ancestors per
// open, and a fresh full compute per read made the peer-card menus take
// seconds on populated home pages.
// Wheel-feel knobs, overridable per machine for tuning without a rebuild:
// WIREUI_SCROLL_STEP = px per wheel notch (absolute; wins over the OS setting)
// WIREUI_SCROLL_EASE = fraction of the remaining distance applied per frame,
//   0.1..=1.0; 1.0 snaps instantly with no animation (default 0.5)
pub fn scroll_step() -> f32 {
    static STEP: std::sync::OnceLock<f32> = std::sync::OnceLock::new();
    *STEP.get_or_init(|| {
        std::env::var("WIREUI_SCROLL_STEP")
            .ok()
            .and_then(|v| v.parse::<f32>().ok())
            .filter(|v| (8.0..=400.0).contains(v))
            .unwrap_or_else(os_default_scroll_step)
    })
}

#[cfg(target_os = "windows")]
fn os_default_scroll_step() -> f32 {
    #[link(name = "user32")]
    extern "system" {
        fn SystemParametersInfoW(
            action: u32,
            param: u32,
            pv: *mut std::ffi::c_void,
            win_ini: u32,
        ) -> i32;
    }
    const SPI_GETWHEELSCROLLLINES: u32 = 0x0068;
    const WHEEL_PAGESCROLL: u32 = u32::MAX;
    const PX_PER_WHEEL_LINE: f32 = 40.0;
    let mut lines: u32 = 3;
    let ok = unsafe {
        SystemParametersInfoW(SPI_GETWHEELSCROLLLINES, 0, &mut lines as *mut u32 as *mut _, 0)
    };
    if ok == 0 || lines == WHEEL_PAGESCROLL {
        return 3.0 * PX_PER_WHEEL_LINE;
    }
    lines.min(100) as f32 * PX_PER_WHEEL_LINE
}

#[cfg(not(target_os = "windows"))]
fn os_default_scroll_step() -> f32 {
    60.0
}

static SCROLL_EASE_DEFAULT: std::sync::atomic::AtomicU32 =
    std::sync::atomic::AtomicU32::new(0x3F000000);

// The CPU raster backend repaints too slowly for a multi-frame ease to read as
// smooth (each step is a visible chunk), so the window setup lowers the ease
// to instant there. Must run before the first wheel event; the env var wins.
pub fn set_scroll_ease_default(v: f32) {
    SCROLL_EASE_DEFAULT.store(v.to_bits(), std::sync::atomic::Ordering::Relaxed);
}

pub fn scroll_ease() -> f32 {
    static EASE: std::sync::OnceLock<f32> = std::sync::OnceLock::new();
    *EASE.get_or_init(|| {
        std::env::var("WIREUI_SCROLL_EASE")
            .ok()
            .and_then(|v| v.parse::<f32>().ok())
            .filter(|v| (0.1..=1.0).contains(v))
            .unwrap_or_else(|| {
                f32::from_bits(SCROLL_EASE_DEFAULT.load(std::sync::atomic::Ordering::Relaxed))
            })
    })
}

fn stateful_nodes(doc: &super::dom::Document) -> (u64, HashMap<NodeKey, u8>) {
    use slotmap::Key;
    let mut h: u64 = 0xcbf29ce484222325;
    let mut states = HashMap::new();
    for (key, node) in doc.arena.iter() {
        let s = &node.states;
        let bits = (s.hover as u8)
            | ((s.active as u8) << 1)
            | ((s.focus as u8) << 2)
            | ((s.checked as u8) << 3)
            | ((s.disabled as u8) << 4)
            | ((s.current as u8) << 5);
        if bits != 0 {
            h ^= key.data().as_ffi();
            h = h.wrapping_mul(0x100000001b3);
            h ^= bits as u64;
            h = h.wrapping_mul(0x100000001b3);
            states.insert(key, bits);
        }
    }
    (h, states)
}

fn states_fingerprint(doc: &super::dom::Document) -> u64 {
    stateful_nodes(doc).0
}

fn style_cache_key(engine: &EngineRef) -> (u64, u64) {
    let fp = states_fingerprint(&engine.borrow().doc);
    (super::dom::layout_epoch(), fp)
}

pub fn cached_computed_styles(
    engine: &EngineRef,
) -> std::rc::Rc<HashMap<NodeKey, crate::engine::style::Computed>> {
    let (fingerprint, states) = stateful_nodes(&engine.borrow().doc);
    let key = (super::dom::layout_epoch(), fingerprint);
    {
        let e = engine.borrow();
        if let Some((k, m)) = &e.computed_cache {
            if *k == key {
                return m.clone();
            }
        }
    }
    let previous = engine.borrow_mut().computed_cache.take();
    if let Some((prev_key, prev)) = previous {
        if prev_key.0 == key.0 {
            if let Some(m) = restyle_state_change(engine, prev, &states) {
                let mut e = engine.borrow_mut();
                e.computed_cache = Some((key, m.clone()));
                e.computed_states = states;
                return m;
            }
        }
    }
    let m = {
        let e = engine.borrow();
        std::rc::Rc::new(super::style::compute_styles(&e.doc, &e.sheets))
    };
    engine.borrow_mut().computed_cache = Some((key, m.clone()));
    engine.borrow_mut().computed_states = states;
    m
}

const MAX_INCREMENTAL_RESTYLE_NODES: usize = 64;

fn restyle_state_change(
    engine: &EngineRef,
    prev: std::rc::Rc<HashMap<NodeKey, crate::engine::style::Computed>>,
    states: &HashMap<NodeKey, u8>,
) -> Option<std::rc::Rc<HashMap<NodeKey, crate::engine::style::Computed>>> {
    let e = engine.borrow();
    let mut changed: Vec<NodeKey> = Vec::new();
    for (k, bits) in states {
        if e.computed_states.get(k) != Some(bits) {
            changed.push(*k);
        }
    }
    for k in e.computed_states.keys() {
        if !states.contains_key(k) {
            changed.push(*k);
        }
    }
    if changed.is_empty() || changed.len() > MAX_INCREMENTAL_RESTYLE_NODES {
        return None;
    }
    let mut roots: Vec<NodeKey> = Vec::new();
    for k in changed {
        if e.doc.arena.get(k).is_none() || !prev.contains_key(&k) {
            return None;
        }
        roots.push(k);
        if let Some(next) = next_sibling(&e.doc, k) {
            roots.push(next);
        }
    }
    roots.sort_unstable();
    roots.dedup();
    let covered: std::collections::HashSet<NodeKey> = roots.iter().copied().collect();
    roots.retain(|k| {
        let mut cur = e.doc.arena.get(*k).and_then(|n| n.parent);
        while let Some(p) = cur {
            if covered.contains(&p) {
                return false;
            }
            cur = e.doc.arena.get(p).and_then(|n| n.parent);
        }
        true
    });
    let mut rc = prev;
    let out = std::rc::Rc::make_mut(&mut rc);
    for root in roots {
        super::style::recompute_subtree(&e.doc, &e.sheets, root, out);
    }
    super::style::apply_table_columns_public(&e.doc, out);
    Some(rc)
}

fn next_sibling(doc: &super::dom::Document, key: NodeKey) -> Option<NodeKey> {
    let parent = doc.arena.get(key)?.parent?;
    let siblings = &doc.arena.get(parent)?.children;
    let idx = siblings.iter().position(|c| *c == key)?;
    siblings.get(idx + 1).copied()
}

pub fn cached_pseudo_boxes(
    engine: &EngineRef,
    styles: &HashMap<NodeKey, crate::engine::style::Computed>,
) -> std::rc::Rc<HashMap<NodeKey, Vec<crate::engine::style::PseudoBox>>> {
    let key = style_cache_key(engine);
    if let Some((k, m)) = engine.borrow().pseudo_cache.clone() {
        if k == key {
            return m;
        }
    }
    let m = {
        let e = engine.borrow();
        std::rc::Rc::new(super::style::compute_pseudo_boxes(&e.doc, &e.sheets, styles))
    };
    engine.borrow_mut().pseudo_cache = Some((key, m.clone()));
    m
}

pub type FgOverlay = (u32, u32, std::sync::Arc<Vec<u8>>, f32, f32, f32, f32, f32);

// element.paintForeground = fn(gfx): evaluate every element's hook with a
// recording gfx whose blendImage/drawImage calls become fg_overlays, painted
// as a top layer by the window repaint. Called once per frame before painting
// (the file-transfer drag ghost).
pub fn record_fg_overlays(interp: &mut Interp, engine: &EngineRef) {
    record_content_overlays(interp, engine);
    let hooks: Vec<(SV, SV)> = {
        let e = engine.borrow();
        e.element_objects
            .iter()
            .filter(|(k, _)| e.doc.arena.get(**k).is_some())
            .filter_map(|(_, obj)| {
                if let SV::Object(o) = obj {
                    o.props
                        .borrow()
                        .iter()
                        .find(|(n, v)| {
                            n == "paintForeground"
                                && matches!(v, SV::Function(_) | SV::NativeFn(_))
                        })
                        .map(|(_, f)| (obj.clone(), f.clone()))
                } else {
                    None
                }
            })
            .collect()
    };
    if hooks.is_empty() {
        let mut e = engine.borrow_mut();
        if !e.fg_overlays.is_empty() {
            e.fg_overlays.clear();
        }
        return;
    }
    let collected: Gc<RefCell<Vec<FgOverlay>>> = Gc::new(RefCell::new(Vec::new()));
    struct GfxRecorder {
        out: Gc<RefCell<Vec<FgOverlay>>>,
    }
    impl NativeObj for GfxRecorder {
        fn type_name(&self) -> &'static str {
            "graphics"
        }
        fn call_method(&self, _interp: &mut Interp, name: &str, argv: &[SV]) -> Option<SResult<SV>> {
            if name == "blendImage" || name == "drawImage" {
                if let Some((iw, ih, rgba)) = argv.first().and_then(sv_as_image) {
                    let num = |i: usize, d: f32| match argv.get(i) {
                        Some(SV::Int(n)) => *n as f32,
                        Some(SV::Float(f)) => *f as f32,
                        _ => d,
                    };
                    let x = num(1, 0.0);
                    let y = num(2, 0.0);
                    let w = num(3, iw as f32);
                    let h = num(4, ih as f32);
                    let op = num(5, 1.0).clamp(0.0, 1.0);
                    self.out.borrow_mut().push((iw, ih, rgba, x, y, w, h, op));
                }
            }
            Some(Ok(SV::Undefined))
        }
    }
    for (obj, f) in hooks {
        let gfx = sv_object(ObjectData {
            class: RefCell::new(None),
            props: RefCell::new(Vec::new()),
            native: Some(Gc::new(GfxRecorder {
                out: collected.clone(),
            })),
        });
        if let Err(e) = interp.call_value(&f, &obj, &[gfx]) {
            eprintln!("paintForeground error: {}", e.0);
        }
    }
    engine.borrow_mut().fg_overlays = collected.borrow().clone();
}

// element.paintContent = fn(gfx): evaluate per-element hooks with a recording
// gfx whose stroke calls become element-anchored DrawCmds (the draw-on-screen
// annotation preview). Coordinates are element-local logical px.
pub fn record_content_overlays(interp: &mut Interp, engine: &EngineRef) {
    let hooks: Vec<(NodeKey, SV, SV)> = {
        let e = engine.borrow();
        e.element_objects
            .iter()
            .filter(|(k, _)| e.doc.arena.get(**k).is_some())
            .filter_map(|(k, obj)| {
                if let SV::Object(o) = obj {
                    o.props
                        .borrow()
                        .iter()
                        .find(|(n, v)| {
                            n == "paintContent"
                                && matches!(v, SV::Function(_) | SV::NativeFn(_))
                        })
                        .map(|(_, f)| (*k, obj.clone(), f.clone()))
                } else {
                    None
                }
            })
            .collect()
    };
    if hooks.is_empty() {
        let mut e = engine.borrow_mut();
        if !e.content_overlays.is_empty() {
            e.content_overlays.clear();
        }
        return;
    }
    struct GfxVec {
        out: Gc<RefCell<Vec<DrawCmd>>>,
        stroke: RefCell<(u32, f32)>,
    }
    impl NativeObj for GfxVec {
        fn type_name(&self) -> &'static str {
            "graphics"
        }
        fn call_method(&self, _interp: &mut Interp, name: &str, argv: &[SV]) -> Option<SResult<SV>> {
            let num = |i: usize| match argv.get(i) {
                Some(SV::Int(n)) => *n as f32,
                Some(SV::Float(f)) => *f as f32,
                _ => 0.0,
            };
            match name {
                "strokeColor" => {
                    self.stroke.borrow_mut().0 = match argv.first() {
                        Some(SV::Int(n)) => *n as u32,
                        Some(SV::Float(f)) => *f as u32,
                        _ => 0xFF000000,
                    };
                }
                "strokeWidth" => self.stroke.borrow_mut().1 = num(0).max(0.5),
                "line" => {
                    let (color, width) = *self.stroke.borrow();
                    self.out.borrow_mut().push(DrawCmd::Line {
                        x1: num(0), y1: num(1), x2: num(2), y2: num(3), color, width,
                    });
                }
                "rectangle" => {
                    let (color, width) = *self.stroke.borrow();
                    self.out.borrow_mut().push(DrawCmd::Rect {
                        l: num(0), t: num(1), r: num(2), b: num(3), color, width,
                    });
                }
                "ellipse" => {
                    let (color, width) = *self.stroke.borrow();
                    self.out.borrow_mut().push(DrawCmd::Ellipse {
                        cx: num(0), cy: num(1), rx: num(2), ry: num(3), color, width,
                    });
                }
                _ => {}
            }
            Some(Ok(SV::Undefined))
        }
    }
    let mut map: HashMap<NodeKey, Vec<DrawCmd>> = HashMap::new();
    for (key, obj, f) in hooks {
        let collected: Gc<RefCell<Vec<DrawCmd>>> = Gc::new(RefCell::new(Vec::new()));
        let gfx = sv_object(ObjectData {
            class: RefCell::new(None),
            props: RefCell::new(Vec::new()),
            native: Some(Gc::new(GfxVec {
                out: collected.clone(),
                stroke: RefCell::new((0xFF000000, 1.0)),
            })),
        });
        if let Err(e) = interp.call_value(&f, &obj, &[gfx]) {
            eprintln!("paintContent error: {}", e.0);
        }
        let cmds = collected.borrow().clone();
        if !cmds.is_empty() {
            map.insert(key, cmds);
        }
    }
    engine.borrow_mut().content_overlays = map;
}

// view.doEvent(#untilMouseUp): the window loop registers the real modal pump
// (poll OS mouse, deliver MOUSE_MOVE subscriptions, repaint, return on button
// release). Headless there is no hook and the call returns immediately; tests
// install their own to feed synthetic moves.
thread_local! {
    static MODAL_HOOK: RefCell<Option<Box<dyn FnMut(&mut Interp, &EngineRef)>>> =
        RefCell::new(None);
    static SNAPSHOT_HOOK: RefCell<
        Option<Box<dyn FnMut(&EngineRef, NodeKey, u32, u32) -> Option<(u32, u32, std::sync::Arc<Vec<u8>>)>>>,
    > = RefCell::new(None);
    // Script geometry reads must see the CURRENT tree, not the last painted
    // frame: right after a Reactor update() the rebuilt nodes have no painted
    // rect yet, so box() would report 0 until a paint lands.
    static LAYOUT_HOOK: RefCell<Option<Box<dyn FnMut(&EngineRef)>>> = RefCell::new(None);
}

pub fn set_modal_hook(f: Box<dyn FnMut(&mut Interp, &EngineRef)>) {
    MODAL_HOOK.with(|h| *h.borrow_mut() = Some(f));
}

// The hooks capture the Window/GPU behind Rc; thread_locals outlive the event
// loop, so an exiting window must drop them or the OS window is never
// destroyed (it lingers on screen, unpumped -- "Not Responding" forever).
pub fn clear_window_hooks() {
    MODAL_HOOK.with(|h| h.borrow_mut().take());
    SNAPSHOT_HOOK.with(|h| h.borrow_mut().take());
    LAYOUT_HOOK.with(|h| h.borrow_mut().take());
}

pub fn run_modal_until_mouse_up(interp: &mut Interp, engine: &EngineRef) {
    let hook = MODAL_HOOK.with(|h| h.borrow_mut().take());
    if let Some(mut f) = hook {
        f(interp, engine);
        MODAL_HOOK.with(|h| {
            let mut slot = h.borrow_mut();
            if slot.is_none() {
                *slot = Some(f);
            }
        });
    }
}

pub fn set_layout_hook(f: Box<dyn FnMut(&EngineRef)>) {
    LAYOUT_HOOK.with(|h| *h.borrow_mut() = Some(f));
}

// Bring last_rects up to date with the current document. Cheap when nothing
// changed: the window's layout cache is keyed by the layout epoch.
pub fn ensure_layout(engine: &EngineRef) {
    let hook = LAYOUT_HOOK.with(|h| h.borrow_mut().take());
    if let Some(mut f) = hook {
        f(engine);
        LAYOUT_HOOK.with(|h| {
            let mut slot = h.borrow_mut();
            if slot.is_none() {
                *slot = Some(f);
            }
        });
    }
}

pub fn set_snapshot_hook(
    f: Box<dyn FnMut(&EngineRef, NodeKey, u32, u32) -> Option<(u32, u32, std::sync::Arc<Vec<u8>>)>>,
) {
    SNAPSHOT_HOOK.with(|h| *h.borrow_mut() = Some(f));
}

pub fn snapshot_element(
    engine: &EngineRef,
    key: NodeKey,
    w: u32,
    h: u32,
) -> Option<(u32, u32, std::sync::Arc<Vec<u8>>)> {
    let hook = SNAPSHOT_HOOK.with(|h| h.borrow_mut().take());
    let mut result = None;
    if let Some(mut f) = hook {
        result = f(engine, key, w, h);
        SNAPSHOT_HOOK.with(|h| {
            let mut slot = h.borrow_mut();
            if slot.is_none() {
                *slot = Some(f);
            }
        });
    }
    result
}

// Sciter's raw-event convention: a script can define onMouse/onKey directly on
// an element object (`function handler.onMouse(evt)`, remote.tis's entire input
// forwarding) and the engine delivers every raw event of that class to it. The
// expando lives in the element object's props; deliver to the nearest
// ancestor-or-self that defines it. Truthy return = consumed.
pub struct RawMouse {
    pub etype: i64,
    pub x: f32,
    pub y: f32,
    pub buttons: i64,
    pub wheel: (f64, f64),
    pub alt: bool,
    pub ctrl: bool,
    pub shift: bool,
    pub meta: bool,
}

fn element_expando(engine: &EngineRef, key: NodeKey, name: &str) -> Option<SV> {
    let e = engine.try_borrow().ok()?;
    let sv = e.element_objects.get(&key)?;
    if let SV::Object(o) = sv {
        return o
            .props
            .borrow()
            .iter()
            .find(|(n, v)| n == name && matches!(v, SV::Function(_) | SV::NativeFn(_)))
            .map(|(_, v)| v.clone());
    }
    None
}

pub fn on_mouse_target(engine: &EngineRef, hit: Option<NodeKey>) -> Option<NodeKey> {
    if let Some(cap) = engine.try_borrow().ok().and_then(|e| {
        e.mouse_capture.filter(|k| e.doc.arena.get(*k).is_some())
    }) {
        if element_expando(engine, cap, "onMouse").is_some() {
            return Some(cap);
        }
    }
    let mut cur = hit;
    while let Some(k) = cur {
        if element_expando(engine, k, "onMouse").is_some() {
            return Some(k);
        }
        cur = {
            let e = engine.try_borrow().ok()?;
            e.doc.arena.get(k).and_then(|n| n.parent)
        };
    }
    None
}

pub fn dispatch_on_mouse(
    interp: &mut Interp,
    engine: &EngineRef,
    target: NodeKey,
    ev: &RawMouse,
) -> bool {
    let func = match element_expando(engine, target, "onMouse") {
        Some(f) => f,
        None => return false,
    };
    let (this_sv, ex, ey, deep_hit) = {
        let e = match engine.try_borrow() {
            Ok(e) => e,
            Err(_) => return false,
        };
        let sv = match e.element_objects.get(&target) {
            Some(s) => s.clone(),
            None => return false,
        };
        let (rx, ry) = e
            .screen_rects
            .get(&target)
            .map(|&(x, y, _, _)| (x, y))
            .unwrap_or((0.0, 0.0));
        let deep = if e.mouse_capture.is_some() {
            None
        } else {
            super::window::hit_test_ordered(&e.doc, &e.screen_rects, &e.screen_order, ev.x, ev.y)
        };
        (sv, (ev.x - rx) as i64, (ev.y - ry) as i64, deep)
    };
    let target_sv = deep_hit
        .filter(|k| *k != target)
        .map(|k| element_sv(engine, k))
        .unwrap_or_else(|| this_sv.clone());
    let shortcut = if cfg!(target_os = "macos") { ev.meta } else { ev.ctrl };
    let evt = new_object(vec![
        ("type".into(), SV::Int(ev.etype)),
        ("x".into(), SV::Int(ex)),
        ("y".into(), SV::Int(ey)),
        ("xView".into(), SV::Int(ev.x as i64)),
        ("yView".into(), SV::Int(ev.y as i64)),
        ("buttons".into(), SV::Int(ev.buttons)),
        ("mainButton".into(), SV::Bool(ev.buttons & 1 != 0)),
        ("propButton".into(), SV::Bool(ev.buttons & 2 != 0)),
        (
            "wheelDeltas".into(),
            crate::script::interp::sv_array(vec![SV::Float(ev.wheel.0), SV::Float(ev.wheel.1)]),
        ),
        ("altKey".into(), SV::Bool(ev.alt)),
        ("ctrlKey".into(), SV::Bool(ev.ctrl)),
        ("shiftKey".into(), SV::Bool(ev.shift)),
        ("commandKey".into(), SV::Bool(ev.meta)),
        ("metaKey".into(), SV::Bool(ev.meta)),
        ("shortcutKey".into(), SV::Bool(shortcut)),
        ("dragging".into(), SV::Bool(false)),
        ("target".into(), target_sv),
    ]);
    match interp.call_value(&func, &this_sv, &[evt]) {
        Ok(v) => crate::script::interp::truthy(&v),
        Err(err) => {
            eprintln!("onMouse error: {}", err.0);
            false
        }
    }
}

pub fn dispatch_on_key(
    interp: &mut Interp,
    engine: &EngineRef,
    etype: i64,
    key_code: i64,
    mods: (bool, bool, bool, bool),
) -> bool {
    let start = focused_node(engine).or_else(|| Some(engine.borrow().doc.root));
    let mut cur = start;
    let target = loop {
        match cur {
            Some(k) => {
                if element_expando(engine, k, "onKey").is_some() {
                    break Some(k);
                }
                cur = {
                    let e = match engine.try_borrow() {
                        Ok(e) => e,
                        Err(_) => return false,
                    };
                    e.doc.arena.get(k).and_then(|n| n.parent)
                };
            }
            None => break None,
        }
    };
    let target = match target {
        Some(t) => t,
        None => return false,
    };
    let func = match element_expando(engine, target, "onKey") {
        Some(f) => f,
        None => return false,
    };
    let this_sv = match engine.try_borrow().ok().and_then(|e| e.element_objects.get(&target).cloned()) {
        Some(s) => s,
        None => return false,
    };
    let (alt, ctrl, shift, meta) = mods;
    let shortcut = if cfg!(target_os = "macos") { meta } else { ctrl };
    let evt = new_object(vec![
        ("type".into(), SV::Int(etype)),
        ("keyCode".into(), SV::Int(key_code)),
        ("altKey".into(), SV::Bool(alt)),
        ("ctrlKey".into(), SV::Bool(ctrl)),
        ("shiftKey".into(), SV::Bool(shift)),
        ("commandKey".into(), SV::Bool(meta)),
        ("metaKey".into(), SV::Bool(meta)),
        ("shortcutKey".into(), SV::Bool(shortcut)),
    ]);
    match interp.call_value(&func, &this_sv, &[evt]) {
        Ok(v) => crate::script::interp::truthy(&v),
        Err(err) => {
            eprintln!("onKey error: {}", err.0);
            false
        }
    }
}

// Any focused element (a tabindex div like the terminal viewport counts, not
// just text inputs) -- the keydown/keypress DOM dispatch target.
pub fn focused_element(engine: &Engine) -> Option<NodeKey> {
    engine
        .doc
        .descendants(engine.doc.root)
        .into_iter()
        .find(|k| engine.doc.arena.get(*k).map_or(false, |n| n.states.focus))
}

pub fn is_focusable(n: &crate::engine::dom::Node) -> bool {
    if n.is_text() || n.attr("disabled").is_some() {
        return false;
    }
    if let Some(t) = n.attr("tabindex") {
        return !t.trim().starts_with('-');
    }
    matches!(n.tag.as_str(), "input" | "textarea" | "button" | "select" | "a")
}

pub fn activatable_focus(e: &Engine) -> Option<NodeKey> {
    let k = focused_element(e)?;
    let n = e.doc.arena.get(k)?;
    if is_text_editable(n) || !is_focusable(n) {
        return None;
    }
    Some(k)
}

pub fn focus_move(engine: &EngineRef, backwards: bool) -> bool {
    let mut e = engine.borrow_mut();
    let root = e.doc.root;
    let order: Vec<NodeKey> = e
        .doc
        .descendants(root)
        .into_iter()
        .filter(|k| e.doc.arena.get(*k).map_or(false, is_focusable))
        .filter(|k| {
            e.last_rects
                .get(k)
                .map_or(false, |r| r.2 > 0.0 && r.3 > 0.0)
        })
        .collect();
    if order.is_empty() {
        return false;
    }
    let current = order
        .iter()
        .position(|k| e.doc.arena.get(*k).map_or(false, |n| n.states.focus));
    let next = match current {
        Some(i) => {
            if backwards {
                (i + order.len() - 1) % order.len()
            } else {
                (i + 1) % order.len()
            }
        }
        None => {
            if backwards {
                order.len() - 1
            } else {
                0
            }
        }
    };
    let target = order[next];
    for k in e.doc.descendants(root) {
        if let Some(n) = e.doc.arena.get_mut(k) {
            if n.states.focus && k != target {
                n.states.focus = false;
            }
        }
    }
    if let Some(n) = e.doc.arena.get_mut(target) {
        n.states.focus = true;
    }
    for k in e.doc.descendants(root) {
        if let Some(n) = e.doc.arena.get_mut(k) {
            if k == target {
                n.set_attr("keyfocus", "");
            } else if n.attr("keyfocus").is_some() {
                n.remove_attr("keyfocus");
            }
        }
    }
    true
}

// input, textarea, and `<select editable>` (the file-transfer path box) are
// text-editable: caret from click, typing, caret painting.
pub const SELECT_CHEVRON_HIT: f32 = 22.0;

fn takes_typed_text(n: &crate::engine::dom::Node) -> bool {
    if n.attr("disabled").is_some() {
        return false;
    }
    match n.tag.as_str() {
        "textarea" => true,
        "select" => n.attr("editable").is_some(),
        "input" => matches!(
            n.attr("type").unwrap_or("text"),
            "text" | "password" | "number" | "search" | "email" | "url" | "tel"
        ),
        _ => false,
    }
}

pub fn is_text_editable(n: &crate::engine::dom::Node) -> bool {
    (n.tag == "input" && !is_hslider(n))
        || n.tag == "textarea"
        || (n.tag == "select" && n.attr("editable").is_some())
}

pub fn is_hslider(n: &crate::engine::dom::Node) -> bool {
    n.tag == "input" && n.attr("type") == Some("hslider")
}

fn slider_bounds(n: &crate::engine::dom::Node) -> (f32, f32) {
    let attr_f = |name: &str, fallback: f32| {
        n.attr(name)
            .and_then(|v| v.trim().parse::<f32>().ok())
            .unwrap_or(fallback)
    };
    let min = attr_f("min", 0.0);
    (min, attr_f("max", 100.0).max(min + 1.0))
}

pub fn slider_value(engine: &EngineRef, key: NodeKey) -> i64 {
    let e = engine.borrow();
    e.doc.arena.get(key).map_or(0, |n| {
        let (min, max) = slider_bounds(n);
        n.attr("value")
            .and_then(|v| v.trim().parse::<f32>().ok())
            .unwrap_or(min)
            .clamp(min, max)
            .round() as i64
    })
}

pub fn slider_set_value(engine: &EngineRef, key: NodeKey, value: f32) {
    let updated = {
        let mut e = engine.borrow_mut();
        match e.doc.arena.get_mut(key) {
            Some(n) if is_hslider(n) => {
                let (min, max) = slider_bounds(n);
                let text = format!("{}", value.clamp(min, max).round() as i64);
                n.set_attr("value", &text);
                n.attr("buddy").map(|b| (b.to_string(), text))
            }
            _ => None,
        }
    };
    if let Some((buddy_id, text)) = updated {
        let buddy_key = {
            let e = engine.borrow();
            e.doc.select_first(&format!("#{}", buddy_id), e.doc.root)
        };
        if let Some(bk) = buddy_key {
            let mut e = engine.borrow_mut();
            e.doc.clear_children(bk);
            let t = e.doc.create_text(&text);
            e.doc.append_child(bk, t);
        }
    }
}

pub fn slider_drag_to(engine: &EngineRef, key: NodeKey, screen_x: f32) {
    let value = {
        let e = engine.borrow();
        let Some(n) = e.doc.arena.get(key).filter(|n| is_hslider(n)) else {
            return;
        };
        let Some(&(x, _, w, _)) = e.screen_rects.get(&key) else {
            return;
        };
        let (min, max) = slider_bounds(n);
        let x0 = x + 8.0;
        let x1 = (x + w - 8.0).max(x0 + 1.0);
        min + ((screen_x - x0) / (x1 - x0)).clamp(0.0, 1.0) * (max - min)
    };
    slider_set_value(engine, key, value);
}

pub fn focused_input(engine: &Engine) -> Option<NodeKey> {
    engine
        .doc
        .find_first(engine.doc.root, |n| is_text_editable(n) && n.states.focus)
}

pub fn focused_input_is_password(engine: &Engine) -> bool {
    focused_input(engine)
        .and_then(|k| engine.doc.arena.get(k))
        .map_or(false, |n| n.attr("type") == Some("password"))
}

pub fn focused_input_is_multiline(engine: &EngineRef) -> bool {
    let e = engine.borrow();
    focused_input(&e)
        .and_then(|k| e.doc.arena.get(k))
        .map_or(false, |n| n.tag == "textarea")
}

pub fn insert_newline(engine: &EngineRef) -> bool {
    focused_input_is_multiline(engine) && insert_text(engine, "\n")
}

pub fn clean_pasted_text(text: &str, multiline: bool) -> String {
    text.replace("\r\n", "\n")
        .chars()
        .filter(|c| !c.is_control() || (multiline && *c == '\n'))
        .collect()
}

// --- Caret-aware text editing (single caret + anchor selection, char indices).

fn byte_of_char(s: &str, ci: usize) -> usize {
    s.char_indices().nth(ci).map(|(b, _)| b).unwrap_or(s.len())
}

// The focused editable's (key, value, caret clamped, selection range) or None.
fn edit_ctx(e: &Engine) -> Option<(NodeKey, String, usize, Option<(usize, usize)>)> {
    let key = focused_input(e)?;
    let node = e.doc.arena.get(key)?;
    let value = node.attr("value").unwrap_or("").to_string();
    let n = value.chars().count();
    let caret = node.caret.min(n);
    let sel = node.sel_anchor.map(|a| {
        let a = a.min(n);
        (a.min(caret), a.max(caret))
    });
    Some((key, value, caret, sel.filter(|(s, en)| s != en)))
}

pub fn selection_range(e: &Engine) -> Option<(NodeKey, usize, usize)> {
    let (key, _, caret, sel) = edit_ctx(e)?;
    match sel {
        Some((s, en)) => Some((key, s, en)),
        None => Some((key, caret, caret)),
    }
}

pub fn set_selection(engine: &EngineRef, key: NodeKey, start: usize, end: usize) {
    let mut e = engine.borrow_mut();
    if let Some(node) = e.doc.arena.get_mut(key) {
        let n = node.attr("value").unwrap_or("").chars().count();
        node.sel_anchor = if start == end { None } else { Some(start.min(n)) };
        node.caret = end.min(n);
        node.caret_moved = true;
    }
}

pub fn selected_text(e: &Engine) -> Option<String> {
    let (_, value, _, sel) = edit_ctx(e)?;
    let (s, en) = sel?;
    Some(
        value
            .chars()
            .skip(s)
            .take(en - s)
            .collect(),
    )
}

pub fn type_char(engine: &EngineRef, ch: char) -> bool {
    if ch.is_control() {
        return false;
    }
    insert_text(engine, &ch.to_string())
}

pub fn insert_text(engine: &EngineRef, text: &str) -> bool {
    let mut e = engine.borrow_mut();
    let (key, mut value, mut caret, sel) = match edit_ctx(&e) {
        Some(c) => c,
        None => return false,
    };
    if let Some((s, en)) = sel {
        let bs = byte_of_char(&value, s);
        let be = byte_of_char(&value, en);
        value.replace_range(bs..be, "");
        caret = s;
    }
    let b = byte_of_char(&value, caret);
    value.insert_str(b, text);
    caret += text.chars().count();
    if let Some(node) = e.doc.arena.get_mut(key) {
        node.set_attr("value", &value);
        node.caret = caret;
        node.caret_moved = true;
        node.sel_anchor = None;
        return true;
    }
    false
}

thread_local! {
    static IME_FORCE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

pub fn set_ime_force(on: bool) {
    IME_FORCE.with(|c| c.set(on));
}

pub fn ime_force() -> bool {
    IME_FORCE.with(|c| c.get())
}

pub fn set_preedit(engine: &EngineRef, text: &str, cursor_byte: Option<usize>) -> bool {
    let caret = match cursor_byte {
        Some(b) if b <= text.len() && text.is_char_boundary(b) => text[..b].chars().count(),
        _ => text.chars().count(),
    };
    let key = { focused_input(&engine.borrow()) };
    let mut changed = clear_preedit_except(engine, key);
    let Some(key) = key else { return changed };
    let mut e = engine.borrow_mut();
    if let Some(node) = e.doc.arena.get_mut(key) {
        if node.preedit != text || node.preedit_caret != caret {
            node.preedit = text.to_string();
            node.preedit_caret = caret;
            changed = true;
        }
    }
    if changed {
        super::dom::bump_layout_epoch();
    }
    changed
}

pub fn clear_preedit(engine: &EngineRef) -> bool {
    clear_preedit_except(engine, None)
}

fn clear_preedit_except(engine: &EngineRef, keep: Option<NodeKey>) -> bool {
    let stale: Vec<NodeKey> = {
        let e = engine.borrow();
        e.doc
            .descendants(e.doc.root)
            .into_iter()
            .filter(|k| Some(*k) != keep)
            .filter(|k| e.doc.arena.get(*k).map_or(false, |n| !n.preedit.is_empty()))
            .collect()
    };
    if stale.is_empty() {
        return false;
    }
    let mut e = engine.borrow_mut();
    for k in stale {
        if let Some(node) = e.doc.arena.get_mut(k) {
            node.preedit.clear();
            node.preedit_caret = 0;
        }
    }
    super::dom::bump_layout_epoch();
    true
}

pub fn caret_device_rect() -> Option<(f64, f64, f64, f64)> {
    super::paint::CARET_DEVICE_RECT.lock().ok().and_then(|r| *r)
}

pub fn commit_ime_text(
    interp: &mut crate::script::interp::Interp,
    engine: &EngineRef,
    text: &str,
    mods: (bool, bool, bool, bool),
) -> bool {
    clear_preedit(engine);
    let clean: String = text.chars().filter(|c| !c.is_control()).collect();
    if clean.is_empty() {
        return false;
    }
    let mut consumed = false;
    for ch in clean.chars() {
        if dispatch_on_key(interp, engine, 0x2003, ch as i64, mods) {
            consumed = true;
        }
    }
    drain_events(interp, engine);
    if consumed {
        return true;
    }
    let focus_target = {
        let e = engine.borrow();
        focused_element(&e).unwrap_or(e.doc.root)
    };
    for ch in clean.chars() {
        let extra = vec![
            ("keyCode".to_string(), SV::Int(ch as i64)),
            ("altKey".to_string(), SV::Bool(false)),
            ("ctrlKey".to_string(), SV::Bool(false)),
            ("shiftKey".to_string(), SV::Bool(false)),
            ("commandKey".to_string(), SV::Bool(false)),
            ("metaKey".to_string(), SV::Bool(false)),
        ];
        if dispatch_dom_event_with(interp, engine, "keypress", focus_target, &extra)
            .unwrap_or(false)
        {
            consumed = true;
        }
    }
    if !consumed && insert_text(engine, &clean) {
        let focused = { focused_input(&engine.borrow()) };
        if let Some(fk) = focused {
            dispatch_dom_event(interp, engine, "change", fk).ok();
        }
    }
    drain_events(interp, engine);
    true
}

pub fn backspace(engine: &EngineRef) -> bool {
    delete_at_caret(engine, false)
}

pub fn delete_forward(engine: &EngineRef) -> bool {
    delete_at_caret(engine, true)
}

fn delete_at_caret(engine: &EngineRef, forward: bool) -> bool {
    let mut e = engine.borrow_mut();
    let (key, mut value, mut caret, sel) = match edit_ctx(&e) {
        Some(c) => c,
        None => return false,
    };
    let (s, en) = match sel {
        Some(r) => r,
        None => {
            if forward {
                if caret >= value.chars().count() {
                    return false;
                }
                (caret, caret + 1)
            } else {
                if caret == 0 {
                    return false;
                }
                (caret - 1, caret)
            }
        }
    };
    let bs = byte_of_char(&value, s);
    let be = byte_of_char(&value, en);
    value.replace_range(bs..be, "");
    caret = s;
    if let Some(node) = e.doc.arena.get_mut(key) {
        node.set_attr("value", &value);
        node.caret = caret;
        node.caret_moved = true;
        node.sel_anchor = None;
        return true;
    }
    false
}

// Arrow/Home/End caret movement; select extends from an anchor (Shift held).
pub fn move_caret(engine: &EngineRef, delta: i64, home: bool, end: bool, select: bool) -> bool {
    let mut e = engine.borrow_mut();
    let (key, value, caret, sel) = match edit_ctx(&e) {
        Some(c) => c,
        None => return false,
    };
    let n = value.chars().count();
    let new = if home {
        0
    } else if end {
        n
    } else if delta < 0 {
        // A plain left/right with an active selection collapses to its edge.
        if !select {
            if let Some((s, _)) = sel {
                s
            } else {
                caret.saturating_sub(1)
            }
        } else {
            caret.saturating_sub(1)
        }
    } else if !select {
        if let Some((_, en)) = sel {
            en
        } else {
            (caret + 1).min(n)
        }
    } else {
        (caret + 1).min(n)
    };
    if let Some(node) = e.doc.arena.get_mut(key) {
        if select {
            if node.sel_anchor.is_none() {
                node.sel_anchor = Some(caret);
            }
        } else {
            node.sel_anchor = None;
        }
        let changed = node.caret != new || (!select && sel.is_some());
        node.caret = new;
        node.caret_moved = true;
        return changed;
    }
    false
}

pub const TEXTAREA_PAD: f32 = 5.0;

pub fn textarea_max_scroll(content_h: f32, avail_h: f64) -> f64 {
    (content_h as f64 - avail_h).max(0.0)
}

pub fn textarea_scroll_y(scroll_top: f32) -> f64 {
    scroll_top.max(0.0) as f64
}

pub fn sync_textarea_scroll(engine: &EngineRef, layout: &crate::engine::layout::LayoutResult) {
    let styles = cached_computed_styles(engine);
    let mut e = engine.borrow_mut();
    e.textarea_content_h.clone_from(&layout.textarea_content_h);
    for (&key, &content_h) in layout.textarea_content_h.iter() {
        let box_h = match layout.rects.get(&key) {
            Some(r) => r.3,
            None => continue,
        };
        let avail = (box_h - TEXTAREA_PAD * 2.0).max(1.0) as f64;
        let max_scroll = textarea_max_scroll(content_h, avail) as f32;
        let caret = {
            let node = match e.doc.arena.get(key) {
                Some(n) => n,
                None => continue,
            };
            if node.caret_moved {
                let style = styles.get(&key).cloned().unwrap_or_default();
                let (display, _) =
                    crate::engine::layout::input_display_text(node, Some(&style));
                let editable = node.attr("value").map_or(false, |v| !v.is_empty())
                    || !node.preedit.is_empty();
                if editable {
                    let ci = node.caret.min(display.chars().count());
                    layout
                        .text_layouts
                        .get(&key)
                        .and_then(|run| caret_geometry(&run.layout, &display, ci))
                } else {
                    None
                }
            } else {
                None
            }
        };
        if let Some(node) = e.doc.arena.get_mut(key) {
            node.caret_moved = false;
            node.scroll_top = node.scroll_top.clamp(0.0, max_scroll);
            node.scroll_target = node.scroll_target.clamp(0.0, max_scroll);
            if let Some((_, cy, ch)) = caret {
                let base = node.scroll_target;
                let mut top = base;
                if cy < top {
                    top = cy;
                } else if (cy + ch) as f64 > top as f64 + avail {
                    top = (cy + ch) - avail as f32;
                }
                let reach = (textarea_max_scroll(cy + ch, avail) as f32).max(max_scroll);
                let top = top.clamp(0.0, reach);
                if top != base {
                    node.scroll_top = top;
                    node.scroll_target = top;
                }
            }
        }
    }
}

// Up/Down in a textarea: re-place the caret at the same x one line away.
pub fn move_caret_line(
    engine: &EngineRef,
    ts: &mut crate::engine::layout::TextSystem,
    down: bool,
    select: bool,
    styles: &HashMap<NodeKey, crate::engine::style::Computed>,
) -> bool {
    let (key, display, style, caret, wrap_w, had_sel) = {
        let e = engine.borrow();
        let key = match focused_input(&e) {
            Some(k) => k,
            None => return false,
        };
        let node = match e.doc.arena.get(key) {
            Some(n) if n.tag == "textarea" => n,
            _ => return false,
        };
        let style = styles.get(&key).cloned().unwrap_or_default();
        let (display, _) = crate::engine::layout::input_display_text(node, Some(&style));
        if display.is_empty() {
            return false;
        }
        let caret = node.caret.min(display.chars().count());
        let had_sel = node.sel_anchor.is_some();
        let wrap_w = e.screen_rects.get(&key).map(|r| (r.2 - 10.0).max(8.0));
        (key, display, style, caret, wrap_w, had_sel)
    };
    let layout = ts.build_layout(
        &display,
        style.font_size,
        style.font_weight,
        &style.font_family,
        [0.0; 4],
        wrap_w,
        parley::layout::Alignment::Start,
        1.0,
        style.line_height,
        style.letter_spacing,
        wrap_w.is_none(),
    );
    let (x, y, h) = match caret_geometry(&layout, &display, caret) {
        Some(g) => g,
        None => return false,
    };
    let target_y = if down { y + h * 1.5 } else { y - h * 0.5 };
    if target_y < 0.0 || target_y > layout.height() {
        return false;
    }
    let byte = parley::layout::Cursor::from_point(&layout, x, target_y)
        .text_range()
        .start
        .min(display.len());
    let new = display[..byte].chars().count();
    if let Some(node) = engine.borrow_mut().doc.arena.get_mut(key) {
        if select {
            if node.sel_anchor.is_none() {
                node.sel_anchor = Some(caret);
            }
        } else {
            node.sel_anchor = None;
        }
        let changed = node.caret != new || (!select && had_sel);
        node.caret = new;
        node.caret_moved = true;
        return changed;
    }
    false
}

pub fn select_all(engine: &EngineRef) -> bool {
    let mut e = engine.borrow_mut();
    let key = match focused_input(&e) {
        Some(k) => k,
        None => return false,
    };
    if let Some(node) = e.doc.arena.get_mut(key) {
        let n = node.attr("value").unwrap_or("").chars().count();
        if n == 0 {
            return false;
        }
        node.sel_anchor = Some(0);
        node.caret = n;
        node.caret_moved = true;
        return true;
    }
    false
}

// The x of caret boundary `byte` in a shaped layout. Downstream = the leading
// edge of the cluster at `byte`; the end-of-text boundary has no cluster to
// lead, so it is the LAST cluster's trailing edge (Upstream at the last char's
// start). parley wraps from_index(len) back to cluster 0 -- never pass len.
pub fn boundary_x(
    layout: &parley::Layout<crate::engine::layout::ColorBrush>,
    text: &str,
    byte: usize,
) -> f32 {
    if text.is_empty() {
        return 0.0;
    }
    if byte < text.len() {
        parley::layout::Cursor::from_index(layout, byte, parley::layout::Affinity::Downstream)
            .visual_offset() as f32
    } else {
        let last = text
            .char_indices()
            .next_back()
            .map(|(b, _)| b)
            .unwrap_or(0);
        parley::layout::Cursor::from_index(layout, last, parley::layout::Affinity::Upstream)
            .visual_offset() as f32
    }
}

pub fn caret_geometry(
    layout: &parley::Layout<crate::engine::layout::ColorBrush>,
    text: &str,
    caret_ci: usize,
) -> Option<(f32, f32, f32)> {
    if text.is_empty() {
        return None;
    }
    let at_end = caret_ci >= text.chars().count();
    let (index, affinity) = match text.char_indices().nth(caret_ci) {
        Some((b, _)) => (b, parley::layout::Affinity::Downstream),
        None => (
            text.char_indices().next_back().map(|(b, _)| b)?,
            parley::layout::Affinity::Upstream,
        ),
    };
    let rect = parley::layout::Cursor::from_index(layout, index, affinity)
        .strong_geometry(layout, 1.0)?;
    if at_end && text.ends_with('\n') {
        return Some((0.0, layout.height(), rect.height() as f32));
    }
    Some((rect.x0 as f32, rect.y0 as f32, rect.height() as f32))
}

pub fn selection_rects(
    layout: &parley::Layout<crate::engine::layout::ColorBrush>,
    text: &str,
    from_ci: usize,
    to_ci: usize,
    mut f: impl FnMut(f32, f32, f32, f32),
) {
    if text.is_empty() || from_ci == to_ci {
        return;
    }
    let (s0, s1) = (from_ci.min(to_ci), from_ci.max(to_ci));
    let (Some((sx, sy, sh)), Some((ex, ey, eh))) = (
        caret_geometry(layout, text, s0),
        caret_geometry(layout, text, s1),
    ) else {
        return;
    };
    parley::layout::Selection::from_point(layout, sx, sy + sh * 0.5)
        .extend_to_point(layout, ex, ey + eh * 0.5)
        .geometry_with(layout, |r| {
            f(r.x0 as f32, r.y0 as f32, r.x1 as f32, r.y1 as f32)
        });
}

#[derive(Clone, Copy, PartialEq)]
pub enum CaretClick {
    Place { extend: bool },
    Drag,
    Word,
    Line,
}

pub fn input_scroll_x(
    engine: &EngineRef,
    ts: &mut crate::engine::layout::TextSystem,
    key: NodeKey,
    box_w: f32,
    styles: &HashMap<NodeKey, crate::engine::style::Computed>,
) -> f32 {
    let e = engine.borrow();
    let node = match e.doc.arena.get(key) {
        Some(n) => n,
        None => return 0.0,
    };
    if node.tag == "textarea" || !node.states.focus {
        return 0.0;
    }
    let style = styles.get(&key).cloned().unwrap_or_default();
    let (display, _) = crate::engine::layout::input_display_text(node, Some(&style));
    let has_value =
        node.attr("value").map_or(false, |v| !v.is_empty()) || !node.preedit.is_empty();
    if !has_value {
        return 0.0;
    }
    let pad_left = input_pad_left(Some(&style));
    let right_reserve = if node.tag == "select" {
        crate::engine::layout::SELECT_CHEVRON_WIDTH as f32
    } else {
        6.0
    };
    let avail_w = (box_w - pad_left - right_reserve).max(8.0);
    let n_chars = display.chars().count();
    let preedit_chars = node.preedit.chars().count();
    let caret_ci = if preedit_chars > 0 {
        (node.caret + node.preedit_caret.min(preedit_chars)).min(n_chars)
    } else {
        node.caret.min(n_chars)
    };
    let layout = ts.build_layout(
        &display,
        style.font_size,
        style.font_weight,
        &style.font_family,
        [0.0; 4],
        None,
        parley::layout::Alignment::Start,
        1.0,
        style.line_height,
        style.letter_spacing,
        true,
    );
    let byte = display
        .char_indices()
        .nth(caret_ci)
        .map(|(b, _)| b)
        .unwrap_or(display.len());
    (boundary_x(&layout, &display, byte) - (avail_w - 2.0)).max(0.0)
}

// Map a click at (x, y) (px, relative to the text origin) to a caret position
// using the same shaping the control paints with. Place anchors a pending
// drag-selection at the caret; Drag moves the caret keeping the anchor; Word
// selects the character class run around the click (double-click).
pub fn caret_click(
    engine: &EngineRef,
    ts: &mut crate::engine::layout::TextSystem,
    key: NodeKey,
    x: f32,
    y: f32,
    wrap_w: Option<f32>,
    avail_h: Option<f64>,
    mode: CaretClick,
    styles: &HashMap<NodeKey, crate::engine::style::Computed>,
) {
    let (display, style, has_value) = {
        let e = engine.borrow();
        let node = match e.doc.arena.get(key) {
            Some(n) => n,
            None => return,
        };
        let style = styles.get(&key).cloned().unwrap_or_default();
        let (display, _) = crate::engine::layout::input_display_text(node, Some(&style));
        let has_value = node.attr("value").map_or(false, |v| !v.is_empty());
        (display, style, has_value)
    };
    if !has_value {
        if let Some(n) = engine.borrow_mut().doc.arena.get_mut(key) {
            n.caret = 0;
            n.sel_anchor = None;
        }
        return;
    }
    let layout = ts.build_layout(
        &display,
        style.font_size,
        style.font_weight,
        &style.font_family,
        [0.0; 4],
        wrap_w,
        parley::layout::Alignment::Start,
        1.0,
        style.line_height,
        style.letter_spacing,
        wrap_w.is_none(),
    );
    let scroll_y = avail_h.map_or(0.0, |_| {
        textarea_scroll_y(engine.borrow().doc.arena.get(key).map_or(0.0, |n| n.scroll_top))
    });
    let cur = parley::layout::Cursor::from_point(
        &layout,
        x.max(0.0),
        (y + scroll_y as f32).max(0.0),
    );
    let byte = cur.text_range().start.min(display.len());
    let ci = display[..byte].chars().count();
    let n = display.chars().count();
    if let Some(node) = engine.borrow_mut().doc.arena.get_mut(key) {
        match mode {
            CaretClick::Place { extend: false } => {
                node.caret = ci;
                node.sel_anchor = Some(ci);
            }
            CaretClick::Place { extend: true } => {
                if node.sel_anchor.is_none() {
                    node.sel_anchor = Some(node.caret.min(n));
                }
                node.caret = ci;
            }
            CaretClick::Drag => {
                node.caret = ci;
                if node.sel_anchor.is_none() {
                    node.sel_anchor = Some(ci);
                }
            }
            CaretClick::Word => {
                let chars: Vec<char> = display.chars().collect();
                let at = ci.min(n.saturating_sub(1));
                let class = |c: char| -> u8 {
                    if c.is_alphanumeric() || c == '_' {
                        0
                    } else if c.is_whitespace() {
                        1
                    } else {
                        2
                    }
                };
                let cl = class(chars[at]);
                let mut start = at;
                while start > 0 && class(chars[start - 1]) == cl {
                    start -= 1;
                }
                let mut end = at + 1;
                while end < n && class(chars[end]) == cl {
                    end += 1;
                }
                node.sel_anchor = Some(start);
                node.caret = end;
            }
            CaretClick::Line => {
                let chars: Vec<char> = display.chars().collect();
                let at = ci.min(n);
                let start = chars[..at].iter().rposition(|&c| c == '\n').map_or(0, |i| i + 1);
                let end = chars[at..].iter().position(|&c| c == '\n').map_or(n, |i| at + i);
                node.sel_anchor = Some(start);
                node.caret = end;
            }
        }
    }
}

// A press-drag that never moved collapses the pending anchor to a plain caret.
pub fn finish_text_drag(engine: &EngineRef, key: NodeKey) {
    if let Some(node) = engine.borrow_mut().doc.arena.get_mut(key) {
        if node.sel_anchor == Some(node.caret) {
            node.sel_anchor = None;
        }
    }
}

pub fn set_focus(engine: &EngineRef, target: Option<NodeKey>) -> bool {
    let mut e = engine.borrow_mut();
    let all = e.doc.descendants(e.doc.root);
    for k in all.iter() {
        if let Some(n) = e.doc.arena.get_mut(*k) {
            if n.attr("keyfocus").is_some() {
                n.remove_attr("keyfocus");
            }
        }
    }
    let mut changed = false;
    for k in all {
        let want = Some(k) == target
            && e.doc.arena.get(k).map_or(false, |n| {
                matches!(n.tag.as_str(), "input" | "textarea" | "button" | "select")
            });
        if let Some(node) = e.doc.arena.get_mut(k) {
            if node.states.focus != want {
                node.states.focus = want;
                changed = true;
                // A freshly-focused text control starts with the caret at the
                // end (typing appends); a click then repositions it.
                if want && is_text_editable(node) {
                    node.caret = node.attr("value").unwrap_or("").chars().count();
                    node.caret_moved = true;
                    node.sel_anchor = None;
                }
            }
        }
    }
    changed
}

/// True when paint defers this node's subtree to the top-layer overlay pass:
/// only `position: fixed` escapes to the viewport top layer (popup menus and
/// their backdrops). `position: absolute` overlays (settings panel, msgbox)
/// stay in normal document order and land on top because they render last -- so
/// a low z-index absolute element (a card's fav-heart) does NOT float above a
/// later modal. Shared by paint and hit-order so they cannot diverge.
pub fn input_pad_left(style: Option<&crate::engine::style::Computed>) -> f32 {
    match style.map(|s| s.padding[3]) {
        Some(crate::engine::style::Length::Px(v)) if v > 0.0 => v,
        _ => 10.0,
    }
}

pub fn deferred_z(style: Option<&crate::engine::style::Computed>) -> Option<i32> {
    match style {
        // fixed with no z-index still escapes; treat as z 0.
        Some(s) if s.position == crate::engine::style::Position::Fixed => Some(s.z_index.unwrap_or(0)),
        _ => None,
    }
}

/// Compute on-screen geometry: rects shifted by ancestor scroll offsets and
/// clipped to scroll-container boxes, PLUS the paint order (document order with
/// z-indexed positioned subtrees deferred and processed lowest-z first).
/// Pointer hit-testing must take the LAST containing node in this order - the
/// topmost pixel - which plain document order gets wrong whenever an overlay
/// relies on z-index to sit above later content or its own backdrop.
pub fn compute_screen_geometry(
    engine: &EngineRef,
    layout_rects: &HashMap<NodeKey, (f32, f32, f32, f32)>,
    styles: &HashMap<NodeKey, crate::engine::style::Computed>,
) -> (HashMap<NodeKey, (f32, f32, f32, f32)>, Vec<NodeKey>) {
    let e = engine.borrow();
    let mut out = HashMap::new();
    let mut order = Vec::new();
    fn intersect(
        a: (f32, f32, f32, f32),
        b: (f32, f32, f32, f32),
    ) -> Option<(f32, f32, f32, f32)> {
        let x0 = a.0.max(b.0);
        let y0 = a.1.max(b.1);
        let x1 = (a.0 + a.2).min(b.0 + b.2);
        let y1 = (a.1 + a.3).min(b.1 + b.3);
        if x1 > x0 && y1 > y0 {
            Some((x0, y0, x1 - x0, y1 - y0))
        } else {
            None
        }
    }
    struct Deferred {
        z: i32,
        seq: usize,
        key: NodeKey,
        offset_x: f32,
        offset_y: f32,
        clip: Option<(f32, f32, f32, f32)>,
    }
    #[allow(clippy::too_many_arguments)]
    fn recurse(
        e: &Engine,
        styles: &HashMap<NodeKey, crate::engine::style::Computed>,
        layout_rects: &HashMap<NodeKey, (f32, f32, f32, f32)>,
        key: NodeKey,
        offset_x: f32,
        offset_y: f32,
        clip: Option<(f32, f32, f32, f32)>,
        out: &mut HashMap<NodeKey, (f32, f32, f32, f32)>,
        order: &mut Vec<NodeKey>,
        deferred: &mut Vec<Deferred>,
        seq: &mut usize,
    ) {
        let node = match e.doc.arena.get(key) {
            Some(n) => n,
            None => return,
        };
        let style = styles.get(&key);
        if style.map_or(false, |s| !s.visible || s.display == crate::engine::style::DisplayKind::None) {
            return;
        }
        let mut screen = None;
        if let Some(&(x, y, w, h)) = layout_rects.get(&key) {
            let sr = (x - offset_x, y - offset_y, w, h);
            screen = match clip {
                Some(c) => intersect(sr, c),
                None => Some(sr),
            };
            if let Some(vis) = screen {
                out.insert(key, vis);
                order.push(key);
            }
        }
        let scroll_carrier = style.map_or(false, |s| s.scroll_y || s.overflow_hidden);
        let child_offset = offset_y + if scroll_carrier { node.scroll_top } else { 0.0 };
        let child_offset_x = offset_x + if scroll_carrier { node.scroll_left } else { 0.0 };
        let child_clip = if style.map_or(false, |s| s.scroll_y) {
            match (screen, clip) {
                (Some(s), _) => Some(s),
                (None, c) => c,
            }
        } else {
            clip
        };
        for pass in 0..2 {
        for &child in &node.children {
            let child_abs = styles
                .get(&child)
                .map_or(false, |st| st.position == crate::engine::style::Position::Absolute);
            if (pass == 0) == child_abs {
                continue;
            }
            if let Some(z) = deferred_z(styles.get(&child)) {
                *seq += 1;
                // Deferred overlays paint after ancestor clip layers pop, so
                // their hit rects must not be clipped by them either.
                deferred.push(Deferred {
                    z,
                    seq: *seq,
                    key: child,
                    offset_x: child_offset_x,
                    offset_y: child_offset,
                    clip: None,
                });
            } else {
                recurse(
                    e, styles, layout_rects, child, child_offset_x, child_offset, child_clip,
                    out, order, deferred, seq,
                );
            }
        }
        }
    }
    let mut deferred: Vec<Deferred> = Vec::new();
    let mut seq = 0usize;
    recurse(
        &e,
        styles,
        layout_rects,
        e.doc.root,
        0.0,
        0.0,
        None,
        &mut out,
        &mut order,
        &mut deferred,
        &mut seq,
    );
    while !deferred.is_empty() {
        let mut idx = 0;
        for (i, d) in deferred.iter().enumerate() {
            if (d.z, d.seq) < (deferred[idx].z, deferred[idx].seq) {
                idx = i;
            }
        }
        let d = deferred.remove(idx);
        recurse(
            &e,
            styles,
            layout_rects,
            d.key,
            d.offset_x,
            d.offset_y,
            d.clip,
            &mut out,
            &mut order,
            &mut deferred,
            &mut seq,
        );
    }
    (out, order)
}

/// Back-compat shim over compute_screen_geometry for callers that only need
/// the rects.
pub fn compute_screen_rects(
    engine: &EngineRef,
    layout_rects: &HashMap<NodeKey, (f32, f32, f32, f32)>,
    styles: &HashMap<NodeKey, crate::engine::style::Computed>,
) -> HashMap<NodeKey, (f32, f32, f32, f32)> {
    compute_screen_geometry(engine, layout_rects, styles).0
}

/// Scroll the nearest scrollable ancestor of the node under (x,y) by dy layout
/// pixels (positive = content moves up). Returns true if anything moved, so the
/// window can request a redraw. Uses the cached rects for hit-testing and to
/// clamp against the container's content height.
pub fn scroll_at(
    engine: &EngineRef,
    rects: &HashMap<NodeKey, (f32, f32, f32, f32)>,
    x: f32,
    y: f32,
    dy: f32,
    styles: &HashMap<NodeKey, crate::engine::style::Computed>,
) -> bool {
    let mut e = engine.borrow_mut();
    // Topmost node under the cursor = last in PAINT order (z-aware; see
    // compute_screen_geometry). Fall back to document order if no order cached.
    let order: Vec<NodeKey> = if e.screen_order.is_empty() {
        e.doc.descendants(e.doc.root)
    } else {
        e.screen_order.clone()
    };
    let mut hit: Option<NodeKey> = None;
    for key in order {
        if e.doc.arena.get(key).map_or(true, |n| n.is_text()) {
            continue;
        }
        if let Some(&(rx, ry, rw, rh)) = rects.get(&key) {
            if x >= rx && x < rx + rw && y >= ry && y < ry + rh {
                hit = Some(key);
            }
        }
    }
    // Walk up to the nearest scrollable container. Content height must be
    // measured from the LAYOUT rects (unscrolled), not the on-screen rects.
    let layout = e.last_rects.clone();
    // The window itself clips: a scroll container whose box grew past the
    // viewport (Sciter bounds these; taffy content-sizes them) is scrollable by
    // the amount the WINDOW cuts off, not just its own box overflow. The home
    // page's .main-content is exactly this: content-sized to 1000+px in a
    // 600px window -- the wheel must scroll it or nothing on the page scrolls.
    let max_scroll_of = |e: &Engine, k: NodeKey| -> f32 { max_scroll_of(e, styles, &layout, k) };
    let apply = |e: &mut Engine, k: NodeKey, max_scroll: f32| -> bool {
        if let Some(node) = e.doc.arena.get_mut(k) {
            // Accumulate into a target; animate_scroll eases scroll_top toward
            // it for smooth (Sciter-like) wheel scrolling.
            let old = node.scroll_target;
            let base = node.scroll_target.clamp(0.0, max_scroll);
            node.scroll_target = (base + dy).clamp(0.0, max_scroll);
            return node.scroll_target != old;
        }
        false
    };
    let mut cur = hit;
    while let Some(k) = cur {
        if styles.get(&k).map_or(false, |s| s.scroll_y) {
            let max_scroll = max_scroll_of(&e, k);
            // A scroller WITH overflow consumes the wheel even when clamped
            // at an edge (Sciter never chains past it). Chaining here let the
            // file-transfer list ratchet the whole page: wheel-down at the
            // list bottom fell through to the page scroller, but wheel-up was
            // eaten by the list, so the page could never scroll back.
            // Sub-pixel/rounding overflow doesn't count -- a container that
            // "overflows" by a hair must not eat the whole gesture.
            if max_scroll > 2.0 {
                return apply(&mut e, k, max_scroll);
            }
            // A scroller with NO overflow is transparent: keep walking so an
            // outer scroller (or the fallback) can take the wheel.
        }
        cur = e.doc.arena.get(k).and_then(|n| n.parent);
    }
    // Nothing scrollable under the cursor: scroll the page's main scrollable
    // container (the one with the most overflow) so the wheel works anywhere
    // over the window, matching how the real client feels.
    let candidates: Vec<NodeKey> = e.doc.descendants(e.doc.root);
    let mut best: Option<(NodeKey, f32)> = None;
    for k in candidates {
        if !styles.get(&k).map_or(false, |s| s.scroll_y) {
            continue;
        }
        let ms = max_scroll_of(&e, k);
        if ms > 0.0 && best.map_or(true, |(_, b)| ms > b) {
            best = Some((k, ms));
        }
    }
    if let Some((k, ms)) = best {
        return apply(&mut e, k, ms);
    }
    false
}

pub fn scroll_into_view(engine: &EngineRef, key: NodeKey) {
    let styles = cached_computed_styles(engine);
    let mut e = engine.borrow_mut();
    let row = match e.last_rects.get(&key).copied() {
        Some(r) => r,
        None => return,
    };
    let mut cur = e.doc.arena.get(key).and_then(|n| n.parent);
    while let Some(k) = cur {
        if styles.get(&k).map_or(false, |s| s.scroll_y) {
            let (_, cy, _, ch) = e.last_rects.get(&k).copied().unwrap_or((0.0, 0.0, 0.0, 0.0));
            let content_bottom = scroll_content_bottom(&e.doc, &styles, &e.last_rects, k, cy);
            let max_scroll = (content_bottom - (cy + ch)).max(0.0);
            if let Some(node) = e.doc.arena.get_mut(k) {
                let cur_scroll = node.scroll_target.clamp(0.0, max_scroll);
                let view_top = cy + cur_scroll;
                let view_bottom = view_top + ch;
                let new_scroll = if row.1 < view_top {
                    row.1 - cy
                } else if row.1 + row.3 > view_bottom {
                    row.1 + row.3 - ch - cy
                } else {
                    cur_scroll
                }
                .clamp(0.0, max_scroll);
                if new_scroll != cur_scroll {
                    node.scroll_target = new_scroll;
                    node.scroll_top = new_scroll;
                }
            }
            break;
        }
        cur = e.doc.arena.get(k).and_then(|n| n.parent);
    }
}

/// Ease every scroll container's scroll_top toward its scroll_target. Returns true
/// while any is still in motion (the event loop keeps ticking + repainting).
pub fn animate_scroll(engine: &EngineRef) -> bool {
    let mut e = engine.borrow_mut();
    let mut moving = false;
    // Flat pass over the arena rather than a tree walk: the window loop calls
    // this every iteration, and building a key list for a document the size of
    // a file-transfer listing cost more than the easing itself. Detached nodes
    // are reached too; their offsets converge and are then left alone.
    for node in e.doc.arena.values_mut() {
        let diff = node.scroll_target - node.scroll_top;
        if diff.abs() < 0.5 {
            if node.scroll_top != node.scroll_target {
                node.scroll_top = node.scroll_target;
            }
        } else {
            node.scroll_top += diff * scroll_ease();
            moving = true;
        }
    }
    moving
}

pub fn set_active(engine: &EngineRef, target: Option<NodeKey>) -> bool {
    let mut e = engine.borrow_mut();
    let all = e.doc.descendants(e.doc.root);
    let mut changed = false;
    for k in all {
        let want = Some(k) == target;
        if let Some(node) = e.doc.arena.get_mut(k) {
            if node.states.active != want {
                node.states.active = want;
                changed = true;
            }
        }
    }
    changed
}

pub fn focus_first_input(engine: &EngineRef) {
    let mut e = engine.borrow_mut();
    let root = e.doc.root;
    if let Some(first_input) = e
        .doc
        .descendants(root)
        .into_iter()
        .find(|k| e.doc.arena.get(*k).map_or(false, |n| n.tag == "input" && n.attr("noautofocus").is_none()))
    {
        if let Some(node) = e.doc.arena.get_mut(first_input) {
            node.states.focus = true;
        }
    }
}

fn timer_fn_eq(a: &SV, b: &SV) -> bool {
    match (a, b) {
        (SV::Function(x), SV::Function(y)) => Gc::ptr_eq(x, y),
        (SV::NativeFn(x), SV::NativeFn(y)) => Gc::ptr_eq(x, y),
        _ => false,
    }
}

// Sciter timer semantics: element.timer(ms, fn) STARTS OR RESTARTS the timer for
// that function (same fn replaces its pending entry), and ms == 0 REMOVES it.
// The UI relies on the cancel idiom -- msgbox.render does `self.timer(0,
// msgboxTimerFunc)` to cancel the previous render's closure, and the reconnect
// path cancels retryConnect the same way. Plain push semantics instead FIRED the
// stale closure ("cannot set property 'html' of null" on every msgbox re-render)
// and would trigger spurious reconnects.
pub fn schedule_timer(engine: &EngineRef, due: f64, f: SV) {
    let mut e = engine.borrow_mut();
    e.timers.retain(|(_, existing, _)| !timer_fn_eq(existing, &f));
    if due > 0.0 {
        let now = e.now_ms;
        e.timers.push((now + due, f, due));
    }
}

pub fn pump_timers(interp: &mut Interp, engine: &EngineRef, until_ms: f64) -> SResult<()> {
    loop {
        let next = {
            let e = engine.borrow();
            e.timers
                .iter()
                .enumerate()
                .filter(|(_, (due, _, _))| *due <= until_ms)
                .min_by(|a, b| a.1 .0.partial_cmp(&b.1 .0).unwrap())
                .map(|(i, (due, _, _))| (i, *due))
        };
        let (idx, due) = match next {
            Some(v) => v,
            None => break,
        };
        let (f, interval) = {
            let mut e = engine.borrow_mut();
            e.now_ms = due;
            let t = e.timers.remove(idx);
            (t.1, t.2)
        };
        if matches!(f, SV::Function(_) | SV::NativeFn(_)) {
            // A throwing timer must not abort the rest of the batch - the
            // remaining timers are already dequeued and would be lost.
            match interp.call_value(&f, &SV::Undefined, &[]) {
                Err(err) => eprintln!("timer error: {}", err.0),
                // Returning true means "call me again", the repeating-timer
                // contract the UI is written against (auto-pan, the printer
                // poll, the session clocks). Re-arm from the end of this pump
                // rather than from `due`, so a loop that stalled does not fire
                // a burst of catch-up ticks.
                Ok(SV::Bool(true)) if interval > 0.0 => {
                    let mut e = engine.borrow_mut();
                    if !e.timers.iter().any(|(_, g, _)| timer_fn_eq(g, &f)) {
                        e.timers.push((until_ms + interval, f, interval));
                    }
                }
                Ok(_) => {}
            }
        }
    }
    engine.borrow_mut().now_ms = until_ms;
    drain_events(interp, engine);
    if crate::script::gc::allocs_since_gc() > 256 {
        // Roots: timers PLUS the registered event bindings, per-element .on
        // handlers, element identity objects (script expandos live there), and
        // view.on handlers -- gathered from EVERY live engine (main + the chat
        // child window), not just the engine whose pump triggered the GC.
        // Rooting only the current engine let the sweeper clear the chat
        // window's ChatBox instance (its class emptied -> "no method 'send'").
        let mut extra: Vec<SV> = Vec::new();
        for eng in live_engines(engine) {
            let e = eng.borrow();
            extra.extend(e.timers.iter().map(|(_, f, _)| f.clone()));
            extra.extend(
                e.events
                    .iter()
                    .flat_map(|b| [b.func.clone(), b.scope.clone()]),
            );
            extra.extend(e.element_handlers.iter().map(|(_, _, _, f)| f.clone()));
            extra.extend(e.element_objects.values().cloned());
            extra.extend(e.view_handlers.iter().map(|(_, f)| f.clone()));
            extra.extend(e.subscriptions.iter().map(|(_, _, f)| f.clone()));
        }
        interp.gc(&extra);
    }
    Ok(())
}

thread_local! {
    // Every engine that currently backs a window (main + view.window children),
    // so the GC can root all of their handler containers.
    static LIVE_ENGINES: RefCell<Vec<std::rc::Weak<RefCell<Engine>>>> =
        const { RefCell::new(Vec::new()) };
}

pub fn register_engine(engine: &EngineRef) {
    LIVE_ENGINES.with(|l| {
        let mut l = l.borrow_mut();
        l.retain(|w| w.strong_count() > 0);
        if !l.iter().any(|w| w.as_ptr() == std::rc::Rc::as_ptr(engine)) {
            l.push(std::rc::Rc::downgrade(engine));
        }
    });
}

fn live_engines(current: &EngineRef) -> Vec<EngineRef> {
    let mut out = vec![current.clone()];
    LIVE_ENGINES.with(|l| {
        for w in l.borrow().iter() {
            if let Some(e) = w.upgrade() {
                if std::rc::Rc::as_ptr(&e) != std::rc::Rc::as_ptr(current) {
                    out.push(e);
                }
            }
        }
    });
    out
}

pub struct LoadedPage {
    pub engine: EngineRef,
    pub interp: Interp,
    pub body: NodeKey,
}

pub fn load_page(
    page_path: &std::path::Path,
    extra_base: Option<PathBuf>,
    platform: &str,
) -> Result<LoadedPage, String> {
    load_page_bridged(page_path, extra_base, platform, None)
}

pub fn load_page_bridged(
    page_path: &std::path::Path,
    extra_base: Option<PathBuf>,
    platform: &str,
    handler: Option<crate::bridge::SharedHandler>,
) -> Result<LoadedPage, String> {
    load_page_bridged_with_behaviors(page_path, extra_base, platform, handler, Vec::new())
}

pub fn load_page_bridged_with_behaviors(
    page_path: &std::path::Path,
    extra_base: Option<PathBuf>,
    platform: &str,
    handler: Option<crate::bridge::SharedHandler>,
    behaviors: Vec<(String, crate::engine::window::BehaviorFactory)>,
) -> Result<LoadedPage, String> {
    let source =
        std::fs::read_to_string(page_path).map_err(|e| format!("read page: {}", e))?;
    let engine = Engine::new(platform);
    {
        let mut e = engine.borrow_mut();
        if let Some(dir) = page_path.parent() {
            e.base_dirs.push(dir.to_path_buf());
        }
        if let Some(extra) = extra_base {
            e.base_dirs.push(extra);
        }
    }
    load_page_into(engine, &source, handler, behaviors)
}

pub fn load_page_archive(
    archive: crate::engine::archive::Archive,
    page: &str,
    platform: &str,
    handler: Option<crate::bridge::SharedHandler>,
) -> Result<LoadedPage, String> {
    load_page_archive_with_behaviors(archive, page, platform, handler, Vec::new())
}

pub fn load_page_archive_with_behaviors(
    archive: crate::engine::archive::Archive,
    page: &str,
    platform: &str,
    handler: Option<crate::bridge::SharedHandler>,
    behaviors: Vec<(String, crate::engine::window::BehaviorFactory)>,
) -> Result<LoadedPage, String> {
    let source = archive
        .get_str(page)
        .ok_or_else(|| format!("page {} not in archive", page))?;
    let engine = Engine::new(platform);
    engine.borrow_mut().archives.push(archive);
    load_page_into(engine, &source, handler, behaviors)
}

fn load_page_into(
    engine: EngineRef,
    source: &str,
    handler: Option<crate::bridge::SharedHandler>,
    behaviors: Vec<(String, crate::engine::window::BehaviorFactory)>,
) -> Result<LoadedPage, String> {
    let root = engine.borrow().doc.root;
    let page = {
        let mut e = engine.borrow_mut();
        super::html::parse_into(&mut e.doc, root, source)
    };
    for style in &page.styles {
        ingest_css(&engine, style);
    }

    set_current_engine(&engine);
    register_engine(&engine);
    let mut interp = Interp::new();
    install_host_bridged(&mut interp, &engine, handler);
    {
        let base = engine.borrow().base_dirs.clone();
        if let Some(first) = base.last() {
            interp.base_dir = first.clone();
        }
    }
    // Behaviors (e.g. native-remote on <video #handler>) must be live BEFORE the
    // page scripts run: remote.tis reads `handler.is_file_transfer()` at top level
    // and calls initializeFileTransfer() from self.ready(), both of which dispatch
    // to the handler element's behavior. Attaching after load left those calls
    // hitting the unbound fallback, so the file-transfer pane was never built.
    if !behaviors.is_empty() {
        engine.borrow_mut().behavior_factories = behaviors;
        attach_behaviors(&engine);
        bind_video(&engine);
    }
    let script_count = page.scripts.len();
    for (i, script) in page.scripts.iter().enumerate() {
        interp
            .run_source(script)
            .map_err(|e| format!("script {}/{}: {}", i + 1, script_count, e.0))?;
    }
    let ready = {
        let self_obj = interp.self_object.clone();
        match &self_obj {
            SV::Object(o) => o
                .props
                .borrow()
                .iter()
                .find(|(k, _)| k == "ready")
                .map(|(_, v)| v.clone()),
            _ => None,
        }
    };
    drain_events(&mut interp, &engine);
    if let Some(f) = ready {
        let this = interp.self_object.clone();
        if let Err(e) = interp.call_value(&f, &this, &[]) {
            eprintln!("wireui: self.ready error: {}", e.0);
        }
    }
    drain_events(&mut interp, &engine);
    let body = {
        let e = engine.borrow();
        e.doc.select_first("body", e.doc.root)
    }
    .unwrap_or(root);
    Ok(LoadedPage {
        engine,
        interp,
        body,
    })
}

// view.window(params): load a child page (the chat window) into a NEW engine but
// the SAME shared interp, under its own window env over the shared builtins base.
// Cross-window closures work because $/$$/view/self are per-window env bindings
// that capture their engine, so a parent closure called from the child still
// resolves the parent's DOM, and vice-versa. Returns (child engine, child view).
pub fn open_child_window(
    interp: &mut Interp,
    source: &str,
    params: SV,
    handler: Option<crate::bridge::SharedHandler>,
    platform: &str,
    base_dirs: Vec<std::path::PathBuf>,
    archives: Vec<crate::engine::archive::Archive>,
) -> Result<(EngineRef, SV), String> {
    let engine = Engine::new(platform);
    // The child resolves its includes (common.tis) the same way the parent does:
    // packaged builds ship the UI in the archive, dev builds read from base_dirs.
    {
        let mut e = engine.borrow_mut();
        e.base_dirs = base_dirs;
        e.archives = archives;
    }
    register_engine(&engine);
    let root = engine.borrow().doc.root;
    let page = {
        let mut e = engine.borrow_mut();
        super::html::parse_into(&mut e.doc, root, source)
    };
    for style in &page.styles {
        ingest_css(&engine, style);
    }

    // Save the caller (parent) window context.
    let base_env = interp.global.parent.clone();
    let saved_global = interp.global.clone();
    let saved_self = interp.self_object.clone();
    let saved_view = interp.view_object.clone();
    let saved_hook = interp.include_hook.clone();
    let saved_engine = current_engine();

    // Switch to the child window context (fresh env over the shared builtins).
    interp.global = crate::script::interp::Env::new(base_env);
    set_current_engine(&engine);
    install_window_bindings(interp, &engine, handler, Some(params));
    let child_view = interp.view_object.clone();

    let run = (|| -> Result<(), String> {
        for script in &page.scripts {
            interp
                .run_source(script)
                .map_err(|e| format!("child script: {}", e.0))?;
        }
        let ready = match &interp.self_object {
            SV::Object(o) => o
                .props
                .borrow()
                .iter()
                .find(|(k, _)| k == "ready")
                .map(|(_, v)| v.clone()),
            _ => None,
        };
        drain_events(interp, &engine);
        if let Some(f) = ready {
            let this = interp.self_object.clone();
            interp
                .call_value(&f, &this, &[])
                .map_err(|e| format!("child self.ready: {}", e.0))?;
        }
        drain_events(interp, &engine);
        Ok(())
    })();

    // Restore the parent window context before returning to the caller's script.
    interp.global = saved_global;
    interp.self_object = saved_self;
    interp.view_object = saved_view;
    interp.include_hook = saved_hook;
    if let Some(e) = saved_engine {
        set_current_engine(&e);
    }
    run?;
    Ok((engine, child_view))
}

// Fire the view.on(name, fn) handlers registered for this window (e.g. "size"
// on resize so the header/terminal re-lay out).
// element.box() must keep answering for display:none elements with their last
// laid-out geometry (Sciter keeps the cached box): merge fresh layout rects
// over the old ones, dropping only nodes gone from the arena. The fullscreen
// toolbar re-center (updateWindowToolbarPosition) measures the header while
// stateChanged has it hidden -- a wholesale replace would return 0-width and
// pin the toolbar to the left edge.
// How far a scroll container can travel. The box itself may be content-sized
// past the window, so the travel is measured against what the WINDOW shows.
pub(crate) fn scroll_content_bottom(
    doc: &Document,
    styles: &HashMap<NodeKey, crate::engine::style::Computed>,
    rects: &std::collections::HashMap<NodeKey, (f32, f32, f32, f32)>,
    key: NodeKey,
    top: f32,
) -> f32 {
    let mut bottom = top;
    let mut stack = vec![key];
    while let Some(k) = stack.pop() {
        if let Some(r) = rects.get(&k) {
            bottom = bottom.max(r.1 + r.3);
        }
        if k != key && styles.get(&k).map_or(false, |s| s.scroll_y) {
            continue;
        }
        if let Some(n) = doc.arena.get(k) {
            stack.extend(n.children.iter().copied());
        }
    }
    bottom
}

pub(crate) fn scroll_content_right(
    doc: &Document,
    styles: &HashMap<NodeKey, crate::engine::style::Computed>,
    rects: &std::collections::HashMap<NodeKey, (f32, f32, f32, f32)>,
    key: NodeKey,
    left: f32,
) -> f32 {
    let mut right = left;
    let mut stack = vec![key];
    while let Some(k) = stack.pop() {
        if let Some(r) = rects.get(&k) {
            right = right.max(r.0 + r.2);
        }
        if k != key && styles.get(&k).map_or(false, |s| s.scroll_y) {
            continue;
        }
        if let Some(n) = doc.arena.get(k) {
            stack.extend(n.children.iter().copied());
        }
    }
    right
}

pub(crate) fn max_scroll_x_of(
    e: &Engine,
    styles: &HashMap<NodeKey, crate::engine::style::Computed>,
    rects: &std::collections::HashMap<NodeKey, (f32, f32, f32, f32)>,
    k: NodeKey,
) -> f32 {
    let viewport_w = rects
        .get(&e.doc.root)
        .map(|r| r.2)
        .filter(|w| *w > 0.0)
        .unwrap_or(f32::MAX);
    let (cx, _, cw, _) = *rects.get(&k).unwrap_or(&(0.0, 0.0, 0.0, 0.0));
    let content_right = scroll_content_right(&e.doc, styles, rects, k, cx);
    (content_right - (cx + cw).min(viewport_w)).max(0.0)
}

pub(crate) fn max_scroll_of(
    e: &Engine,
    styles: &HashMap<NodeKey, crate::engine::style::Computed>,
    rects: &std::collections::HashMap<NodeKey, (f32, f32, f32, f32)>,
    k: NodeKey,
) -> f32 {
    let viewport_h = rects
        .get(&e.doc.root)
        .map(|r| r.3)
        .filter(|h| *h > 0.0)
        .unwrap_or(f32::MAX);
    let (_, cy, _, ch) = *rects.get(&k).unwrap_or(&(0.0, 0.0, 0.0, 0.0));
    let mut content_bottom = scroll_content_bottom(&e.doc, styles, rects, k, cy);
    if let Some(h) = e.textarea_content_h.get(&k) {
        content_bottom = content_bottom.max(cy + h + TEXTAREA_PAD * 2.0);
    }
    (content_bottom - (cy + ch).min(viewport_h)).max(0.0)
}

pub const SCROLLBAR_W: f32 = 10.0;
const SCROLLBAR_THUMB_INSET: f32 = 2.0;
const SCROLLBAR_MIN_THUMB: f32 = 24.0;
const SCROLLBAR_MIN_OVERFLOW: f32 = 2.0;

pub struct ScrollbarGeom {
    pub track: (f32, f32, f32, f32),
    pub thumb: (f32, f32, f32, f32),
    pub max_scroll: f32,
}

pub fn scrollbar_geometry(
    doc: &Document,
    styles: &HashMap<NodeKey, crate::engine::style::Computed>,
    rects: &std::collections::HashMap<NodeKey, (f32, f32, f32, f32)>,
    key: NodeKey,
    box_rect: (f32, f32, f32, f32),
    scroll_top: f32,
    textarea_content_h: Option<f32>,
) -> Option<ScrollbarGeom> {
    let (_, ly, _, lh) = rects.get(&key).copied()?;
    if lh <= 0.0 || box_rect.2 <= SCROLLBAR_W || box_rect.3 <= 0.0 {
        return None;
    }
    let mut content_bottom = scroll_content_bottom(doc, styles, rects, key, ly);
    if let Some(h) = textarea_content_h {
        content_bottom = content_bottom.max(ly + h + TEXTAREA_PAD * 2.0);
    }
    let content_h = content_bottom - ly;
    let max_scroll = (content_h - lh).max(0.0);
    if max_scroll <= SCROLLBAR_MIN_OVERFLOW {
        return None;
    }
    let track = (
        box_rect.0 + box_rect.2 - SCROLLBAR_W,
        box_rect.1,
        SCROLLBAR_W,
        box_rect.3,
    );
    let thumb_h = (track.3 * lh / content_h).clamp(SCROLLBAR_MIN_THUMB.min(track.3), track.3);
    let pos = (scroll_top.clamp(0.0, max_scroll) / max_scroll) * (track.3 - thumb_h);
    Some(ScrollbarGeom {
        track,
        thumb: (
            track.0 + SCROLLBAR_THUMB_INSET,
            track.1 + pos,
            SCROLLBAR_W - SCROLLBAR_THUMB_INSET * 2.0,
            thumb_h,
        ),
        max_scroll,
    })
}

pub struct ScrollDrag {
    pub key: NodeKey,
    pub grab: f32,
    pub track_y: f32,
    pub track_h: f32,
    pub thumb_h: f32,
    pub max_scroll: f32,
}

pub fn scrollbar_hit(engine: &EngineRef, x: f32, y: f32) -> Option<ScrollDrag> {
    let styles = cached_computed_styles(engine);
    let e = engine.borrow();
    let order: Vec<NodeKey> = if e.screen_order.is_empty() {
        e.doc.descendants(e.doc.root)
    } else {
        e.screen_order.clone()
    };
    let mut topmost: Option<NodeKey> = None;
    for key in order {
        if e.doc.arena.get(key).map_or(true, |n| n.is_text()) {
            continue;
        }
        if let Some(&(rx, ry, rw, rh)) = e.screen_rects.get(&key) {
            if x >= rx && x < rx + rw && y >= ry && y < ry + rh {
                topmost = Some(key);
            }
        }
    }
    let mut cur = topmost;
    while let Some(key) = cur {
        if styles.get(&key).map_or(false, |s| s.scroll_y) {
            if let Some(&box_rect) = e.screen_rects.get(&key) {
                let scroll_top = e.doc.arena.get(key).map_or(0.0, |n| n.scroll_top);
                if let Some(g) = scrollbar_geometry(
                    &e.doc,
                    &styles,
                    &e.last_rects,
                    key,
                    box_rect,
                    scroll_top,
                    e.textarea_content_h.get(&key).copied(),
                ) {
                    let (tx, ty, tw, th) = g.track;
                    if x >= tx && x < tx + tw && y >= ty && y < ty + th {
                        let on_thumb = y >= g.thumb.1 && y < g.thumb.1 + g.thumb.3;
                        return Some(ScrollDrag {
                            key,
                            grab: if on_thumb { y - g.thumb.1 } else { g.thumb.3 / 2.0 },
                            track_y: ty,
                            track_h: th,
                            thumb_h: g.thumb.3,
                            max_scroll: g.max_scroll,
                        });
                    }
                }
            }
        }
        cur = e.doc.arena.get(key).and_then(|n| n.parent);
    }
    None
}

pub fn scrollbar_drag_to(engine: &EngineRef, d: &ScrollDrag, y: f32) -> bool {
    let span = (d.track_h - d.thumb_h).max(1.0);
    let target = ((y - d.grab - d.track_y) / span).clamp(0.0, 1.0) * d.max_scroll;
    let mut e = engine.borrow_mut();
    match e.doc.arena.get_mut(d.key) {
        Some(n) if n.scroll_top != target || n.scroll_target != target => {
            n.scroll_top = target;
            n.scroll_target = target;
            true
        }
        _ => false,
    }
}

pub fn update_layout_rects(
    engine: &EngineRef,
    fresh: &std::collections::HashMap<NodeKey, (f32, f32, f32, f32)>,
) {
    let styles = cached_computed_styles(engine);
    let mut e = engine.borrow_mut();
    let mut merged = std::mem::take(&mut e.last_rects);
    merged.retain(|k, _| e.doc.arena.get(*k).is_some());
    for (k, v) in fresh {
        merged.insert(*k, *v);
    }
    e.last_rects = merged;
    // A scroll offset carried across a rebuild (restore_subtree_state) can point
    // past the end of the replacement content: entering a small directory in the
    // file transfer opened the listing scrolled, with its first rows above the
    // viewport. Clamp every scrolled box to what its content actually allows.
    let scrolled: Vec<NodeKey> = e
        .doc
        .descendants(e.doc.root)
        .into_iter()
        .filter(|k| {
            e.doc
                .arena
                .get(*k)
                .map_or(false, |n| n.scroll_top > 0.0 || n.scroll_left > 0.0)
        })
        .collect();
    for k in scrolled {
        let max_scroll = max_scroll_of(&e, &styles, &e.last_rects, k);
        let max_scroll_x = max_scroll_x_of(&e, &styles, &e.last_rects, k);
        if let Some(n) = e.doc.arena.get_mut(k) {
            if n.scroll_top > max_scroll {
                n.scroll_top = max_scroll;
                n.scroll_target = max_scroll;
            }
            if n.scroll_left > max_scroll_x {
                n.scroll_left = max_scroll_x;
            }
        }
    }
}

// The column-resizer behavior (file_transfer.css `table > thead`): dragging the
// divider between two header cells writes explicit px widths onto both cells;
// apply_table_columns then propagates the header template to every row.
pub struct ColumnDrag {
    pub left: NodeKey,
    pub right: NodeKey,
    pub start_x: f32,
    pub left_w: f32,
    pub right_w: f32,
    // width is content-box; screen rects are border-box. Horizontal
    // padding+border per cell, captured at grab time.
    pub left_extra: f32,
    pub right_extra: f32,
}

const COLUMN_GRIP: f32 = 4.0;
const COLUMN_MIN_W: f32 = 16.0;

pub fn file_drop_target_at(engine: &EngineRef, x: f32, y: f32) -> Option<NodeKey> {
    let e = engine.borrow();
    let hit = crate::engine::window::hit_test_ordered(
        &e.doc,
        &e.screen_rects,
        &e.screen_order,
        x,
        y,
    )?;
    let mut cur = Some(hit);
    while let Some(k) = cur {
        let n = e.doc.arena.get(k)?;
        if (n.tag == "table" && n.has_class("folder-view")) || n.attr("filedrop").is_some() {
            return Some(k);
        }
        cur = n.parent;
    }
    None
}

pub fn dispatch_os_file_drag(
    interp: &mut Interp,
    engine: &EngineRef,
    event_name: &str,
    target: NodeKey,
    x: f32,
    y: f32,
    paths: &[std::path::PathBuf],
) -> bool {
    let dragging = sv_array(
        paths
            .iter()
            .map(|p| SV::Str(p.to_string_lossy().into_owned().into()))
            .collect(),
    );
    let (rx, ry) = engine
        .borrow()
        .screen_rects
        .get(&target)
        .map(|&(rx, ry, _, _)| (rx, ry))
        .unwrap_or((0.0, 0.0));
    let extra = [
        ("draggingDataType".to_string(), SV::Symbol("file".into())),
        ("dragging".to_string(), dragging),
        ("x".to_string(), SV::Int((x - rx) as i64)),
        ("y".to_string(), SV::Int((y - ry) as i64)),
    ];
    dispatch_dom_event_with(interp, engine, event_name, target, &extra).unwrap_or(false)
}

pub fn column_resize_hit(engine: &EngineRef, x: f32, y: f32) -> Option<ColumnDrag> {
    let styles = cached_computed_styles(engine);
    let e = engine.borrow();
    for thead in e.doc.descendants(e.doc.root) {
        let is_thead = e.doc.arena.get(thead).map_or(false, |n| n.tag == "thead");
        if !is_thead
            || styles.get(&thead).and_then(|s| s.behavior.as_deref()) != Some("column-resizer")
        {
            continue;
        }
        let Some(&(hx, hy, hw, hh)) = e.screen_rects.get(&thead) else { continue };
        if y < hy || y > hy + hh || x < hx || x > hx + hw {
            continue;
        }
        let cells: Vec<NodeKey> = e
            .doc
            .descendants(thead)
            .into_iter()
            .filter(|k| e.doc.arena.get(*k).map_or(false, |n| n.tag == "th"))
            .collect();
        for pair in cells.windows(2) {
            let (Some(&lr), Some(&rr)) = (
                e.screen_rects.get(&pair[0]),
                e.screen_rects.get(&pair[1]),
            ) else {
                continue;
            };
            let divider = lr.0 + lr.2;
            if (x - divider).abs() <= COLUMN_GRIP {
                let h_extra = |k: NodeKey| {
                    styles.get(&k).map_or(0.0, |s| {
                        let px = |l: &super::style::Length| match l {
                            super::style::Length::Px(v) => *v,
                            _ => 0.0,
                        };
                        px(&s.padding[1])
                            + px(&s.padding[3])
                            + s.border_width[1]
                            + s.border_width[3]
                    })
                };
                return Some(ColumnDrag {
                    left: pair[0],
                    right: pair[1],
                    start_x: x,
                    left_w: lr.2,
                    right_w: rr.2,
                    left_extra: h_extra(pair[0]),
                    right_extra: h_extra(pair[1]),
                });
            }
        }
    }
    None
}

pub fn column_resize_apply(engine: &EngineRef, drag: &ColumnDrag, x: f32) {
    let dx = (x - drag.start_x)
        .max(COLUMN_MIN_W - drag.left_w)
        .min(drag.right_w - COLUMN_MIN_W);
    let mut e = engine.borrow_mut();
    for (key, w) in [
        (drag.left, drag.left_w + dx - drag.left_extra),
        (drag.right, drag.right_w - dx - drag.right_extra),
    ] {
        if let Some(node) = e.doc.arena.get_mut(key) {
            let val = format!("{}px", w.round().max(0.0));
            if let Some(slot) = node.inline_style.iter_mut().find(|(p, _)| p == "width") {
                slot.1 = val;
            } else {
                node.inline_style.push(("width".to_string(), val));
            }
        }
    }
    super::dom::bump_layout_epoch();
}

pub fn fire_view_event(interp: &mut Interp, engine: &EngineRef, name: &str) {
    let mut handlers: Vec<SV> = engine
        .borrow()
        .view_handlers
        .iter()
        .filter(|(n, _)| n == name)
        .map(|(_, f)| f.clone())
        .collect();
    // The `view << event statechange { .. }` form registers an EventBinding
    // scoped to the view object (header.tis's fullscreen/maximize watcher);
    // deliver those too.
    {
        let e = engine.borrow();
        for b in e.events.iter() {
            if b.name == name
                && b.selector.is_none()
                && crate::script::interp::loose_eq(&b.scope, &interp.view_object)
            {
                handlers.push(b.func.clone());
            }
        }
    }
    if handlers.is_empty() {
        return;
    }
    let view = interp.view_object.clone();
    for f in handlers {
        if let Err(e) = interp.call_value(&f, &view, &[]) {
            eprintln!("view.on({}) handler: {}", name, e.0);
        }
    }
    drain_events(interp, engine);
}

pub fn run_window_closing(interp: &mut Interp, engine: &EngineRef) -> bool {
    set_current_engine(engine);
    let root = engine.borrow().doc.root;
    let self_sv = element_sv(engine, root);
    let closing = match &self_sv {
        SV::Object(o) => o
            .props
            .borrow()
            .iter()
            .find(|(k, _)| k == "closing")
            .map(|(_, v)| v.clone()),
        _ => None,
    };
    let mut allow = true;
    if let Some(f) = closing {
        if let Ok(v) = interp.call_value(&f, &self_sv, &[]) {
            if matches!(v, SV::Bool(false)) {
                allow = false;
            }
        }
    }
    drain_events(interp, engine);
    allow
}
