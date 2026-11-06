use super::dom::{Document, NodeKey};
use std::collections::HashMap;

#[derive(Debug, Clone)]
pub struct Stylesheet {
    pub rules: Vec<Rule>,
    pub font_faces: Vec<(String, Vec<u8>)>,
}

#[derive(Debug, Clone)]
pub struct Rule {
    pub selectors: Vec<Selector>,
    pub declarations: Vec<(String, String)>,
    pub order: u32,
}

#[derive(Debug, Clone)]
pub struct Selector {
    pub parts: Vec<SelectorPart>,
    pub specificity: u32,
    pub pseudo_element: Option<String>,
}

#[derive(Debug, Clone)]
pub struct SelectorPart {
    pub tag: Option<String>,
    pub type_attr: Option<String>,
    pub id: Option<String>,
    pub classes: Vec<String>,
    pub attrs: Vec<(String, Option<String>)>,
    pub pseudo: Vec<String>,
    pub combinator: Combinator,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Combinator {
    None,
    Descendant,
    Child,
    Adjacent,
}

pub fn parse_selector_list(text: &str) -> Vec<Selector> {
    text.split(',')
        .map(|s| parse_selector(s.trim()))
        .filter(|s| !s.parts.is_empty())
        .collect()
}

fn parse_selector(text: &str) -> Selector {
    let mut parts: Vec<SelectorPart> = Vec::new();
    let mut specificity = 0u32;
    let mut chars = text.chars().peekable();
    let mut pending_combinator = Combinator::None;
    loop {
        while let Some(&c) = chars.peek() {
            if c.is_whitespace() {
                chars.next();
                if !parts.is_empty() && pending_combinator == Combinator::None {
                    pending_combinator = Combinator::Descendant;
                }
            } else if c == '>' {
                chars.next();
                pending_combinator = Combinator::Child;
            } else if c == '+' {
                chars.next();
                pending_combinator = Combinator::Adjacent;
            } else {
                break;
            }
        }
        if chars.peek().is_none() {
            break;
        }
        let mut part = SelectorPart {
            tag: None,
            type_attr: None,
            id: None,
            classes: Vec::new(),
            attrs: Vec::new(),
            pseudo: Vec::new(),
            combinator: pending_combinator,
        };
        pending_combinator = Combinator::None;
        let mut any = false;
        while let Some(&c) = chars.peek() {
            match c {
                '*' => {
                    chars.next();
                    any = true;
                }
                '#' => {
                    chars.next();
                    part.id = Some(read_name(&mut chars));
                    specificity += 100;
                    any = true;
                }
                '.' => {
                    chars.next();
                    part.classes.push(read_name(&mut chars));
                    specificity += 10;
                    any = true;
                }
                ':' => {
                    chars.next();
                    if chars.peek() == Some(&':') {
                        chars.next();
                    }
                    let mut name = read_name(&mut chars);
                    if chars.peek() == Some(&'(') {
                        let mut depth = 0;
                        for c in chars.by_ref() {
                            name.push(c);
                            if c == '(' {
                                depth += 1;
                            }
                            if c == ')' {
                                depth -= 1;
                                if depth == 0 {
                                    break;
                                }
                            }
                        }
                    }
                    part.pseudo.push(name);
                    specificity += 10;
                    any = true;
                }
                '[' => {
                    chars.next();
                    let mut inner = String::new();
                    for c in chars.by_ref() {
                        if c == ']' {
                            break;
                        }
                        inner.push(c);
                    }
                    if let Some(eq) = inner.find('=') {
                        let name = inner[..eq].trim().to_lowercase();
                        let value = inner[eq + 1..].trim().trim_matches(['"', '\'']).to_string();
                        part.attrs.push((name, Some(value)));
                    } else {
                        part.attrs.push((inner.trim().to_lowercase(), None));
                    }
                    specificity += 10;
                    any = true;
                }
                '|' => {
                    chars.next();
                    part.type_attr = Some(read_name(&mut chars));
                    specificity += 10;
                    any = true;
                }
                c if c.is_alphanumeric() || c == '_' || c == '-' => {
                    part.tag = Some(read_name(&mut chars).to_lowercase());
                    specificity += 1;
                    any = true;
                }
                _ => break,
            }
            if chars
                .peek()
                .map_or(true, |&c| c.is_whitespace() || c == '>' || c == '+' || c == ',')
            {
                break;
            }
        }
        if !any {
            break;
        }
        parts.push(part);
    }
    let mut pseudo_element = None;
    if let Some(last) = parts.last_mut() {
        if let Some(i) = last.pseudo.iter().position(|p| p == "before" || p == "after") {
            pseudo_element = Some(last.pseudo.remove(i));
        }
    }
    Selector { parts, specificity, pseudo_element }
}

fn read_name(chars: &mut std::iter::Peekable<std::str::Chars>) -> String {
    let mut s = String::new();
    while let Some(&c) = chars.peek() {
        if c.is_alphanumeric() || c == '_' || c == '-' {
            s.push(c);
            chars.next();
        } else {
            break;
        }
    }
    s
}

fn part_matches(doc: &Document, key: NodeKey, part: &SelectorPart) -> bool {
    let node = match doc.arena.get(key) {
        Some(n) if !n.is_text() => n,
        _ => return false,
    };
    if let Some(tag) = &part.tag {
        if node.tag != *tag {
            return false;
        }
    }
    if let Some(t) = &part.type_attr {
        if node.attr("type").map_or(true, |v| v != t) {
            return false;
        }
    }
    if let Some(id) = &part.id {
        if node.id().map_or(true, |v| v != id) {
            return false;
        }
    }
    for class in &part.classes {
        if !node.has_class(class) {
            return false;
        }
    }
    for (name, value) in &part.attrs {
        match (node.attr(name), value) {
            (None, _) => return false,
            (Some(_), None) => {}
            (Some(actual), Some(expected)) => {
                if actual != expected {
                    return false;
                }
            }
        }
    }
    for pseudo in &part.pseudo {
        // :not(sel) - excludes elements matching the inner compound selector.
        // header button:not(.window) is what greys the toolbar icons.
        if let Some(inner) = pseudo.strip_prefix("not(").and_then(|s| s.strip_suffix(')')) {
            let sel = parse_selector(inner.trim());
            if let Some(p) = sel.parts.last() {
                if part_matches(doc, key, p) {
                    return false;
                }
            }
            continue;
        }
        let ok = match pseudo.as_str() {
            "root" => doc.root == key,
            "hover" => node.states.hover,
            "active" => node.states.active,
            "focus" => node.states.focus,
            "checked" => node.states.checked,
            "disabled" => node.states.disabled,
            "current" => node.states.current,
            // Sciter :empty on a form control means empty VALUE (controls are
            // void elements with no children, so children-based :empty would
            // always match and paint every input with the placeholder colour).
            "empty" => match node.tag.as_str() {
                "input" | "textarea" | "select" => {
                    node.attr("value").map_or(true, |v| v.is_empty())
                }
                _ => node.children.is_empty(),
            },
            "first-child" => is_nth_child(doc, key, 0),
            "last-child" => is_last_child(doc, key),
            p if p.starts_with("nth-child(") && p.ends_with(')') => {
                let arg = p[10..p.len() - 1].trim();
                match nth_child_index(doc, key) {
                    Some(pos) => match arg {
                        "odd" => pos % 2 == 1,
                        "even" => pos % 2 == 0,
                        n => n.parse::<usize>().map_or(false, |n| n == pos),
                    },
                    None => false,
                }
            }
            _ => false,
        };
        if !ok {
            return false;
        }
    }
    true
}

// 1-based position of `key` among its element siblings (for :nth-child(odd/even/N)).
fn nth_child_index(doc: &Document, key: NodeKey) -> Option<usize> {
    let parent = doc.arena.get(key).and_then(|n| n.parent)?;
    doc.arena
        .get(parent)?
        .children
        .iter()
        .copied()
        .filter(|k| doc.arena.get(*k).map_or(false, |n| !n.is_text()))
        .position(|k| k == key)
        .map(|i| i + 1)
}

fn is_nth_child(doc: &Document, key: NodeKey, n: usize) -> bool {
    let parent = doc.arena.get(key).and_then(|n| n.parent);
    match parent.and_then(|p| doc.arena.get(p)) {
        Some(p) => {
            let elems: Vec<NodeKey> = p
                .children
                .iter()
                .copied()
                .filter(|k| doc.arena.get(*k).map_or(false, |n| !n.is_text()))
                .collect();
            elems.get(n) == Some(&key)
        }
        None => false,
    }
}

fn is_last_child(doc: &Document, key: NodeKey) -> bool {
    let parent = doc.arena.get(key).and_then(|n| n.parent);
    match parent.and_then(|p| doc.arena.get(p)) {
        Some(p) => p
            .children
            .iter()
            .copied()
            .filter(|k| doc.arena.get(*k).map_or(false, |n| !n.is_text()))
            .last()
            == Some(key),
        None => false,
    }
}

pub fn matches(doc: &Document, key: NodeKey, selector: &Selector) -> bool {
    let parts = &selector.parts;
    if parts.is_empty() {
        return false;
    }
    fn match_from(doc: &Document, key: NodeKey, parts: &[SelectorPart], idx: usize) -> bool {
        if !part_matches(doc, key, &parts[idx]) {
            return false;
        }
        if idx == 0 {
            return true;
        }
        let combinator = parts[idx].combinator;
        let mut ancestor = doc.arena.get(key).and_then(|n| n.parent);
        match combinator {
            Combinator::Child => {
                if let Some(p) = ancestor {
                    return match_from(doc, p, parts, idx - 1);
                }
                false
            }
            Combinator::Descendant | Combinator::None => {
                while let Some(p) = ancestor {
                    if match_from(doc, p, parts, idx - 1) {
                        return true;
                    }
                    ancestor = doc.arena.get(p).and_then(|n| n.parent);
                }
                false
            }
            Combinator::Adjacent => {
                let parent = doc.arena.get(key).and_then(|n| n.parent);
                if let Some(p) = parent.and_then(|p| doc.arena.get(p)) {
                    let elems: Vec<NodeKey> = p
                        .children
                        .iter()
                        .copied()
                        .filter(|k| doc.arena.get(*k).map_or(false, |n| !n.is_text()))
                        .collect();
                    if let Some(pos) = elems.iter().position(|&k| k == key) {
                        if pos > 0 {
                            return match_from(doc, elems[pos - 1], parts, idx - 1);
                        }
                    }
                }
                false
            }
        }
    }
    match_from(doc, key, parts, parts.len() - 1)
}

thread_local! {
    // `@mixin NAME { ... }` bodies, keyed by name. Accumulated across every
    // stylesheet/@import parsed this session (Sciter mixins are defined in
    // common.css but used in later files), then spliced in wherever `@NAME;`
    // appears inside a rule.
    static MIXINS: std::cell::RefCell<std::collections::HashMap<String, String>> =
        std::cell::RefCell::new(std::collections::HashMap::new());
}

fn expand_mixins(body: &str) -> String {
    if !body.contains('@') {
        return body.to_string();
    }
    let mixins = MIXINS.with(|m| m.borrow().clone());
    if mixins.is_empty() {
        return body.to_string();
    }
    let chars: Vec<char> = body.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '@' {
            let mut j = i + 1;
            let mut name = String::new();
            while j < chars.len() && (chars[j].is_alphanumeric() || chars[j] == '-' || chars[j] == '_') {
                name.push(chars[j]);
                j += 1;
            }
            if let Some(def) = mixins.get(&name) {
                let mut k = j;
                while k < chars.len() && chars[k].is_whitespace() {
                    k += 1;
                }
                if k < chars.len() && chars[k] == ';' {
                    k += 1;
                }
                out.push_str(def.trim());
                if !def.trim_end().ends_with(';') {
                    out.push(';');
                }
                i = k;
                continue;
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

pub struct CssParser<'a> {
    chars: Vec<char>,
    pos: usize,
    pub order: u32,
    resolver: Option<&'a dyn Fn(&str) -> Option<String>>,
    platform: &'a str,
}

impl<'a> CssParser<'a> {
    pub fn parse(
        source: &str,
        platform: &'a str,
        resolver: Option<&'a dyn Fn(&str) -> Option<String>>,
        order_start: u32,
    ) -> Stylesheet {
        let mut p = CssParser {
            chars: source.chars().collect(),
            pos: 0,
            order: order_start,
            resolver,
            platform,
        };
        let mut sheet = Stylesheet {
            rules: Vec::new(),
            font_faces: Vec::new(),
        };
        p.parse_block_contents(&mut sheet);
        sheet
    }

    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    fn skip_ws_comments(&mut self) {
        loop {
            while self.peek().map_or(false, |c| c.is_whitespace()) {
                self.pos += 1;
            }
            if self.peek() == Some('/') && self.chars.get(self.pos + 1) == Some(&'*') {
                self.pos += 2;
                while self.pos < self.chars.len() {
                    if self.chars[self.pos] == '*' && self.chars.get(self.pos + 1) == Some(&'/') {
                        self.pos += 2;
                        break;
                    }
                    self.pos += 1;
                }
            } else if self.peek() == Some('/') && self.chars.get(self.pos + 1) == Some(&'/') {
                while self.pos < self.chars.len() && self.chars[self.pos] != '\n' {
                    self.pos += 1;
                }
            } else {
                break;
            }
        }
    }

    fn read_until(&mut self, stops: &[char]) -> String {
        let mut out = String::new();
        let mut depth = 0usize;
        while let Some(c) = self.peek() {
            if depth == 0 && stops.contains(&c) {
                break;
            }
            match c {
                '(' => depth += 1,
                ')' => depth = depth.saturating_sub(1),
                _ => {}
            }
            out.push(c);
            self.pos += 1;
        }
        out
    }

    fn skip_block(&mut self) {
        let mut depth = 0usize;
        while let Some(c) = self.peek() {
            self.pos += 1;
            match c {
                '{' => depth += 1,
                '}' => {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        return;
                    }
                }
                _ => {}
            }
        }
    }

    fn parse_block_contents(&mut self, sheet: &mut Stylesheet) {
        loop {
            self.skip_ws_comments();
            match self.peek() {
                None | Some('}') => return,
                Some('@') => self.parse_at_rule(sheet),
                _ => self.parse_rule(sheet),
            }
        }
    }

    fn parse_at_rule(&mut self, sheet: &mut Stylesheet) {
        self.pos += 1;
        let name = {
            let mut s = String::new();
            while let Some(c) = self.peek() {
                if c.is_alphanumeric() || c == '-' {
                    s.push(c);
                    self.pos += 1;
                } else {
                    break;
                }
            }
            s
        };
        match name.as_str() {
            "import" => {
                let spec = self.read_until(&[';']);
                if self.peek() == Some(';') {
                    self.pos += 1;
                }
                let url = spec
                    .trim()
                    .trim_start_matches("url(")
                    .trim_end_matches(')')
                    .trim_matches(['"', '\'', ' '])
                    .to_string();
                if let Some(resolver) = self.resolver {
                    if let Some(content) = resolver(&url) {
                        let inner = CssParser::parse(
                            &content,
                            self.platform,
                            self.resolver,
                            self.order,
                        );
                        self.order += inner.rules.len() as u32 + 1;
                        sheet.rules.extend(inner.rules);
                        sheet.font_faces.extend(inner.font_faces);
                    }
                }
            }
            "font-face" => {
                let _ = self.read_until(&['{']);
                if self.peek() == Some('{') {
                    self.pos += 1;
                    let mut body = String::new();
                    let mut depth = 1usize;
                    while let Some(c) = self.peek() {
                        self.pos += 1;
                        match c {
                            '{' => {
                                depth += 1;
                                body.push(c);
                            }
                            '}' => {
                                depth -= 1;
                                if depth == 0 {
                                    break;
                                }
                                body.push(c);
                            }
                            _ => body.push(c),
                        }
                    }
                    let decls = super::dom::parse_inline_style(&body);
                    let family = decls
                        .iter()
                        .find(|(p, _)| p == "font-family")
                        .map(|(_, v)| v.trim_matches(['\'', '"', ' ']).to_string());
                    let data = decls.iter().find(|(p, _)| p == "src").and_then(|(_, v)| {
                        let v = v.trim();
                        let inner = v
                            .strip_prefix("url(")?
                            .trim_end_matches(')')
                            .trim_matches(['\'', '"']);
                        let b64 = inner.split("base64,").nth(1)?;
                        decode_base64(b64)
                    });
                    if let (Some(family), Some(data)) = (family, data) {
                        sheet.font_faces.push((family, data));
                    }
                }
            }
            "media" => {
                let cond = self.read_until(&['{']);
                let active = eval_media_condition(&cond, self.platform);
                if active {
                    if self.peek() == Some('{') {
                        self.pos += 1;
                    }
                    self.parse_block_contents(sheet);
                    if self.peek() == Some('}') {
                        self.pos += 1;
                    }
                } else {
                    self.skip_block();
                }
            }
            "mixin" => {
                self.skip_ws_comments();
                let mut mname = String::new();
                while let Some(c) = self.peek() {
                    if c.is_alphanumeric() || c == '-' || c == '_' {
                        mname.push(c);
                        self.pos += 1;
                    } else {
                        break;
                    }
                }
                let _ = self.read_until(&['{']);
                if self.peek() == Some('{') {
                    self.pos += 1;
                    let mut body = String::new();
                    let mut depth = 1usize;
                    while let Some(c) = self.peek() {
                        self.pos += 1;
                        match c {
                            '{' => {
                                depth += 1;
                                body.push(c);
                            }
                            '}' => {
                                depth -= 1;
                                if depth == 0 {
                                    break;
                                }
                                body.push(c);
                            }
                            _ => body.push(c),
                        }
                    }
                    if !mname.is_empty() {
                        MIXINS.with(|m| m.borrow_mut().insert(mname, body));
                    }
                }
            }
            _ => {
                let _prelude = self.read_until(&['{', ';']);
                match self.peek() {
                    Some('{') => self.skip_block(),
                    Some(';') => self.pos += 1,
                    _ => {}
                }
            }
        }
    }

    fn parse_rule(&mut self, sheet: &mut Stylesheet) {
        let selector_text = self.read_until(&['{']);
        if self.peek() != Some('{') {
            return;
        }
        self.pos += 1;
        let body = {
            let mut out = String::new();
            let mut depth = 1usize;
            while let Some(c) = self.peek() {
                self.pos += 1;
                match c {
                    '{' => {
                        depth += 1;
                        out.push(c);
                    }
                    '}' => {
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                        out.push(c);
                    }
                    _ => out.push(c),
                }
            }
            out
        };
        let selectors = parse_selector_list(&selector_text);
        if selectors.is_empty() {
            return;
        }
        let body = expand_mixins(&body);
        let declarations = super::dom::parse_inline_style(&body);
        let order = self.order;
        self.order += 1;
        sheet.rules.push(Rule {
            selectors,
            declarations,
            order,
        });
    }
}

pub fn decode_base64(input: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(input.len() * 3 / 4);
    let mut acc: u32 = 0;
    let mut bits = 0u32;
    for c in input.chars() {
        let v = match c {
            'A'..='Z' => c as u32 - 'A' as u32,
            'a'..='z' => c as u32 - 'a' as u32 + 26,
            '0'..='9' => c as u32 - '0' as u32 + 52,
            '+' => 62,
            '/' => 63,
            '=' => break,
            c if c.is_whitespace() => continue,
            _ => return None,
        };
        acc = (acc << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Some(out)
}

fn eval_media_condition(cond: &str, platform: &str) -> bool {
    let cond = cond.trim();
    if let Some(rest) = cond.strip_prefix("platform") {
        let rest = rest.trim();
        if let Some(value) = rest.strip_prefix("==") {
            let value = value.trim().trim_matches(['"', '\'']);
            return value == platform;
        }
        if let Some(value) = rest.strip_prefix("!=") {
            let value = value.trim().trim_matches(['"', '\'']);
            return value != platform;
        }
    }
    false
}

pub struct MatchedStyle {
    pub declarations: Vec<(String, String)>,
    pub vars: HashMap<String, String>,
}

pub fn compute_matched(
    doc: &Document,
    sheets: &[Stylesheet],
    key: NodeKey,
) -> MatchedStyle {
    let mut applicable: Vec<(u32, u32, &Vec<(String, String)>)> = Vec::new();
    for sheet in sheets {
        for rule in &sheet.rules {
            let best = rule
                .selectors
                .iter()
                .filter(|s| s.pseudo_element.is_none() && matches(doc, key, s))
                .map(|s| s.specificity)
                .max();
            if let Some(spec) = best {
                applicable.push((spec, rule.order, &rule.declarations));
            }
        }
    }
    applicable.sort_by_key(|(spec, order, _)| (*spec, *order));
    let mut declarations: Vec<(String, String)> = Vec::new();
    let mut vars: HashMap<String, String> = HashMap::new();
    // Re-declaring a property moves its slot to the END so application order
    // follows the cascade. An in-place update froze the slot at its first
    // position, letting an earlier rule's `background-color` stomp a later,
    // winning rule's `background` shorthand (the FT Send button hover painted
    // gray instead of blue -- shorthand and longhand are separate slots that
    // write the same computed field, so apply order must be cascade order).
    let upsert = |declarations: &mut Vec<(String, String)>, prop: &str, value: &str| {
        if let Some(idx) = declarations.iter().position(|(p, _)| p == prop) {
            declarations.remove(idx);
        }
        declarations.push((prop.to_string(), value.to_string()));
    };
    for (_, _, decls) in applicable {
        for (prop, value) in decls {
            if let Some(name) = prop.strip_prefix("var(") {
                let name = name.trim_end_matches(')').to_string();
                vars.insert(name, value.clone());
                continue;
            }
            upsert(&mut declarations, prop, value);
        }
    }
    if let Some(node) = doc.arena.get(key) {
        for (prop, value) in &node.inline_style {
            if let Some(name) = prop.strip_prefix("var(") {
                let name = name.trim_end_matches(')').to_string();
                vars.insert(name, value.clone());
                continue;
            }
            upsert(&mut declarations, prop, value);
        }
    }
    MatchedStyle { declarations, vars }
}
