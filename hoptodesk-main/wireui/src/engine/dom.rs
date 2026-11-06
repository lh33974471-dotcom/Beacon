use slotmap::{new_key_type, Key, KeyData, SlotMap};
use std::collections::HashMap;

new_key_type! { pub struct NodeKey; }

// Layout invalidation epoch: bumped by every DOM mutation that can change
// geometry (attrs/class/inline-style, tree edits). The window loop caches the
// laid-out document and reuses it while the epoch is unchanged, so hover
// highlights and scrolling repaint WITHOUT re-running taffy layout + parley
// text shaping over the whole document (the Sciter-parity responsiveness
// model: hover/scroll are paint-only). Global on purpose: cross-engine (chat
// window) over-invalidation is harmless.
static LAYOUT_EPOCH: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

pub fn bump_layout_epoch() {
    LAYOUT_EPOCH.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
}

pub fn layout_epoch() -> u64 {
    LAYOUT_EPOCH.load(std::sync::atomic::Ordering::Relaxed)
}

/// Pack a slotmap NodeKey (index + generation) into a HELEMENT pointer value.
/// The consumer never dereferences it; a stale handle decodes to a key whose
/// slot is empty, so lookups return None (call_method -> Err). A live slotmap
/// key always has generation >= 1, so its ffi value is never 0 and never
/// collides with a null HELEMENT.
pub fn key_to_helement(key: NodeKey) -> crate::capi::scdom::HELEMENT {
    key.data().as_ffi() as usize as crate::capi::scdom::HELEMENT
}

pub fn helement_to_key(h: crate::capi::scdom::HELEMENT) -> Option<NodeKey> {
    let ffi = h as usize as u64;
    if ffi == 0 {
        return None;
    }
    Some(NodeKey::from(KeyData::from_ffi(ffi)))
}

#[derive(Debug, Clone, PartialEq)]
pub enum NodeKind {
    Element,
    Text(String),
}

#[derive(Debug, Default, Clone)]
pub struct NodeStates {
    pub hover: bool,
    pub active: bool,
    pub focus: bool,
    pub checked: bool,
    pub disabled: bool,
    pub current: bool,
}

#[derive(Debug)]
pub struct Node {
    pub kind: NodeKind,
    pub tag: String,
    pub attrs: Vec<(String, String)>,
    pub parent: Option<NodeKey>,
    pub children: Vec<NodeKey>,
    pub states: NodeStates,
    pub inline_style: Vec<(String, String)>,
    pub scroll_top: f32,
    pub scroll_target: f32,
    pub scroll_left: f32,
    // Text-edit caret as a CHAR index into the value (char, not byte: the
    // painted text of a password field is bullets, one per value char, so char
    // indices stay valid across the mask). Selection = caret..sel_anchor.
    pub caret: usize,
    pub caret_moved: bool,
    pub sel_anchor: Option<usize>,
    pub preedit: String,
    pub preedit_caret: usize,
}

impl Node {
    pub fn element(tag: &str) -> Node {
        Node {
            kind: NodeKind::Element,
            tag: tag.to_lowercase(),
            attrs: Vec::new(),
            parent: None,
            children: Vec::new(),
            states: NodeStates::default(),
            inline_style: Vec::new(),
            scroll_top: 0.0,
            scroll_target: 0.0,
            scroll_left: 0.0,
            caret: 0,
            caret_moved: false,
            sel_anchor: None,
            preedit: String::new(),
            preedit_caret: 0,
        }
    }

    pub fn text(content: &str) -> Node {
        Node {
            kind: NodeKind::Text(content.to_string()),
            tag: String::new(),
            attrs: Vec::new(),
            parent: None,
            children: Vec::new(),
            states: NodeStates::default(),
            inline_style: Vec::new(),
            scroll_top: 0.0,
            scroll_target: 0.0,
            scroll_left: 0.0,
            caret: 0,
            caret_moved: false,
            sel_anchor: None,
            preedit: String::new(),
            preedit_caret: 0,
        }
    }

    pub fn attr(&self, name: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    pub fn set_attr(&mut self, name: &str, value: &str) {
        if name.eq_ignore_ascii_case("style") {
            let parsed = parse_inline_style(value);
            if parsed != self.inline_style {
                self.inline_style = parsed;
                bump_layout_epoch();
            }
            return;
        }
        if let Some(slot) = self
            .attrs
            .iter_mut()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
        {
            if slot.1 != value {
                slot.1 = value.to_string();
                bump_layout_epoch();
            }
        } else {
            self.attrs.push((name.to_string(), value.to_string()));
            bump_layout_epoch();
        }
    }

    pub fn remove_attr(&mut self, name: &str) {
        let before = self.attrs.len();
        self.attrs.retain(|(k, _)| !k.eq_ignore_ascii_case(name));
        if self.attrs.len() != before {
            bump_layout_epoch();
        }
    }

    pub fn id(&self) -> Option<&str> {
        self.attr("id")
    }

    pub fn classes(&self) -> impl Iterator<Item = &str> {
        self.attr("class").unwrap_or("").split_whitespace()
    }

    pub fn has_class(&self, class: &str) -> bool {
        self.classes().any(|c| c == class)
    }

    pub fn is_text(&self) -> bool {
        matches!(self.kind, NodeKind::Text(_))
    }

    pub fn text_content(&self) -> Option<&str> {
        match &self.kind {
            NodeKind::Text(t) => Some(t),
            _ => None,
        }
    }
}

pub fn parse_inline_style(style: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for decl in split_declarations(style) {
        if let Some(colon) = decl.find(':') {
            let prop = decl[..colon].trim().to_lowercase();
            let value = decl[colon + 1..].trim().to_string();
            if !prop.is_empty() && !value.is_empty() {
                out.push((prop, value));
            }
        }
    }
    out
}

fn split_declarations(style: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut depth = 0usize;
    let mut cur = String::new();
    for c in style.chars() {
        match c {
            '(' => {
                depth += 1;
                cur.push(c);
            }
            ')' => {
                depth = depth.saturating_sub(1);
                cur.push(c);
            }
            ';' if depth == 0 => {
                out.push(std::mem::take(&mut cur));
            }
            _ => cur.push(c),
        }
    }
    if !cur.trim().is_empty() {
        out.push(cur);
    }
    out
}

pub struct Document {
    pub arena: SlotMap<NodeKey, Node>,
    pub root: NodeKey,
    pub var_cache: HashMap<NodeKey, HashMap<String, String>>,
}

impl Document {
    pub fn new() -> Document {
        let mut arena = SlotMap::with_key();
        let root = arena.insert(Node::element("html"));
        Document {
            arena,
            root,
            var_cache: HashMap::new(),
        }
    }

    pub fn create_element(&mut self, tag: &str) -> NodeKey {
        self.arena.insert(Node::element(tag))
    }

    pub fn create_text(&mut self, content: &str) -> NodeKey {
        self.arena.insert(Node::text(content))
    }

    pub fn append_child(&mut self, parent: NodeKey, child: NodeKey) {
        bump_layout_epoch();
        if let Some(node) = self.arena.get_mut(child) {
            node.parent = Some(parent);
        }
        if let Some(p) = self.arena.get_mut(parent) {
            p.children.push(child);
        }
    }

    pub fn clear_children(&mut self, parent: NodeKey) {
        bump_layout_epoch();
        let children = match self.arena.get_mut(parent) {
            Some(p) => std::mem::take(&mut p.children),
            None => return,
        };
        for child in children {
            self.remove_subtree(child);
        }
    }

    pub fn remove_subtree(&mut self, key: NodeKey) {
        bump_layout_epoch();
        let children = match self.arena.get(key) {
            Some(n) => n.children.clone(),
            None => return,
        };
        for child in children {
            self.remove_subtree(child);
        }
        self.arena.remove(key);
    }

    pub fn descendants(&self, from: NodeKey) -> Vec<NodeKey> {
        let mut out = Vec::new();
        let mut stack = vec![from];
        while let Some(key) = stack.pop() {
            out.push(key);
            if let Some(node) = self.arena.get(key) {
                for &child in node.children.iter().rev() {
                    stack.push(child);
                }
            }
        }
        out
    }

    /// First node in `descendants` order matching `pred`, without materialising
    /// the key list. The window loop asks this per iteration (focused input,
    /// animated progress bar), where a Vec of every node was the dominant cost
    /// on a large document such as a file-transfer listing.
    pub fn find_first(&self, from: NodeKey, mut pred: impl FnMut(&Node) -> bool) -> Option<NodeKey> {
        let mut stack = vec![from];
        while let Some(key) = stack.pop() {
            if let Some(node) = self.arena.get(key) {
                if pred(node) {
                    return Some(key);
                }
                stack.extend(node.children.iter().rev().copied());
            }
        }
        None
    }

    pub fn select_first(&self, selector: &str, scope: NodeKey) -> Option<NodeKey> {
        let sel = super::css::parse_selector_list(selector);
        self.descendants(scope)
            .into_iter()
            .filter(|k| self.arena.get(*k).map_or(false, |n| !n.is_text()))
            .find(|k| sel.iter().any(|s| super::css::matches(self, *k, s)))
    }

    pub fn element_children(&self, key: NodeKey) -> Vec<NodeKey> {
        self.arena
            .get(key)
            .map(|n| {
                n.children
                    .iter()
                    .copied()
                    .filter(|c| self.arena.get(*c).map_or(false, |cn| !cn.is_text()))
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn element_index(&self, key: NodeKey) -> Option<usize> {
        let parent = self.arena.get(key)?.parent?;
        self.element_children(parent).iter().position(|&c| c == key)
    }

    pub fn element_sibling(&self, key: NodeKey, forward: bool) -> Option<NodeKey> {
        let parent = self.arena.get(key)?.parent?;
        let sibs = self.element_children(parent);
        let i = sibs.iter().position(|&c| c == key)?;
        if forward {
            sibs.get(i + 1).copied()
        } else {
            i.checked_sub(1).and_then(|j| sibs.get(j).copied())
        }
    }

    // Nearest ancestor-or-self matching the selector (Sciter element.$p).
    pub fn closest(&self, selector: &str, from: NodeKey) -> Option<NodeKey> {
        let sel = super::css::parse_selector_list(selector);
        let mut cur = Some(from);
        while let Some(k) = cur {
            if self.arena.get(k).map_or(false, |n| !n.is_text())
                && sel.iter().any(|s| super::css::matches(self, k, s))
            {
                return Some(k);
            }
            cur = self.arena.get(k).and_then(|n| n.parent);
        }
        None
    }

    pub fn matches_any(&self, selector: &str, key: NodeKey) -> bool {
        let sel = super::css::parse_selector_list(selector);
        sel.iter().any(|s| super::css::matches(self, key, s))
    }

    // Serialize an element's children back to HTML (element.html getter). Round-
    // trips through the .html setter (parse_into) so scripts can save + restore
    // markup (e.g. the copy-button pill).
    pub fn inner_html(&self, key: NodeKey) -> String {
        let mut out = String::new();
        if let Some(n) = self.arena.get(key) {
            for &c in &n.children {
                self.write_outer(c, &mut out);
            }
        }
        out
    }

    fn write_outer(&self, key: NodeKey, out: &mut String) {
        let n = match self.arena.get(key) {
            Some(n) => n,
            None => return,
        };
        if let Some(t) = n.text_content() {
            out.push_str(t);
            return;
        }
        out.push('<');
        out.push_str(&n.tag);
        for (k, v) in &n.attrs {
            out.push(' ');
            out.push_str(k);
            out.push_str("=\"");
            out.push_str(v);
            out.push('"');
        }
        out.push('>');
        for &c in &n.children {
            self.write_outer(c, out);
        }
        out.push_str("</");
        out.push_str(&n.tag);
        out.push('>');
    }

    pub fn select_all(&self, selector: &str, scope: NodeKey) -> Vec<NodeKey> {
        let sel = super::css::parse_selector_list(selector);
        self.descendants(scope)
            .into_iter()
            .filter(|k| self.arena.get(*k).map_or(false, |n| !n.is_text()))
            .filter(|k| sel.iter().any(|s| super::css::matches(self, *k, s)))
            .collect()
    }

    pub fn collect_text(&self, key: NodeKey) -> String {
        let mut out = String::new();
        for k in self.descendants(key) {
            if let Some(t) = self.arena.get(k).and_then(|n| n.text_content()) {
                if !out.is_empty() {
                    out.push(' ');
                }
                out.push_str(t);
            }
        }
        out
    }
}
