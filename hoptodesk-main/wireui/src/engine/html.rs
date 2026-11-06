use super::dom::{Document, NodeKey};

pub struct ParsedPage {
    pub styles: Vec<String>,
    pub scripts: Vec<String>,
}

const VOID_TAGS: &[&str] = &[
    "br", "hr", "img", "input", "meta", "link", "area", "base", "col", "embed", "source",
    "track", "wbr", "progress",
];

pub fn parse_into(doc: &mut Document, parent: NodeKey, source: &str) -> ParsedPage {
    let mut page = ParsedPage {
        styles: Vec::new(),
        scripts: Vec::new(),
    };
    let chars: Vec<char> = source.chars().collect();
    let mut pos = 0usize;
    let mut stack: Vec<NodeKey> = vec![parent];
    let preserve_ws = doc.arena.get(parent).map_or(false, |n| {
        n.inline_style.iter().any(|(k, v)| {
            k.eq_ignore_ascii_case("white-space")
                && matches!(v.trim().to_ascii_lowercase().as_str(), "pre" | "pre-wrap")
        })
    });
    while pos < chars.len() {
        if chars[pos] == '<' {
            if chars[pos + 1..].starts_with(&['!', '-', '-']) {
                if let Some(end) = find_sub(&chars, pos, "-->") {
                    pos = end + 3;
                    continue;
                }
                break;
            }
            if chars.get(pos + 1) == Some(&'!') {
                while pos < chars.len() && chars[pos] != '>' {
                    pos += 1;
                }
                pos += 1;
                continue;
            }
            if chars.get(pos + 1) == Some(&'/') {
                pos += 2;
                let mut name = String::new();
                while pos < chars.len() && chars[pos] != '>' {
                    if !chars[pos].is_whitespace() {
                        name.push(chars[pos]);
                    }
                    pos += 1;
                }
                pos += 1;
                let name = name.to_lowercase();
                if let Some(found) = stack.iter().rposition(|&k| {
                    doc.arena.get(k).map_or(false, |n| n.tag == name)
                }) {
                    if found > 0 {
                        stack.truncate(found);
                    }
                }
                continue;
            }
            pos += 1;
            let mut tag = String::new();
            while pos < chars.len()
                && (chars[pos].is_alphanumeric() || chars[pos] == '_' || chars[pos] == '-')
            {
                tag.push(chars[pos]);
                pos += 1;
            }
            if tag.is_empty() {
                continue;
            }
            let tag_lower = tag.to_lowercase();
            let mut type_attr: Option<String> = None;
            let mut name_attr: Option<String> = None;
            if chars.get(pos) == Some(&'|') {
                pos += 1;
                let mut t = String::new();
                while pos < chars.len() && (chars[pos].is_alphanumeric() || chars[pos] == '-') {
                    t.push(chars[pos]);
                    pos += 1;
                }
                type_attr = Some(t);
                // Sciter shorthand <input|text(field_name)>: the parenthesized
                // token is the control's name attribute.
                if chars.get(pos) == Some(&'(') {
                    pos += 1;
                    let mut n = String::new();
                    while pos < chars.len() && chars[pos] != ')' {
                        n.push(chars[pos]);
                        pos += 1;
                    }
                    if chars.get(pos) == Some(&')') {
                        pos += 1;
                    }
                    if !n.is_empty() {
                        name_attr = Some(n);
                    }
                }
            }
            let key = doc.create_element(&tag_lower);
            if let Some(t) = type_attr {
                if let Some(node) = doc.arena.get_mut(key) {
                    node.set_attr("type", &t);
                }
            }
            if let Some(n) = name_attr {
                if let Some(node) = doc.arena.get_mut(key) {
                    node.set_attr("name", &n);
                }
            }
            let mut self_closing = false;
            loop {
                while pos < chars.len() && chars[pos].is_whitespace() {
                    pos += 1;
                }
                match chars.get(pos) {
                    None => break,
                    Some('>') => {
                        pos += 1;
                        break;
                    }
                    Some('/') => {
                        pos += 1;
                        if chars.get(pos) == Some(&'>') {
                            pos += 1;
                            self_closing = true;
                            break;
                        }
                    }
                    Some('#') => {
                        pos += 1;
                        let mut id = String::new();
                        while pos < chars.len()
                            && (chars[pos].is_alphanumeric()
                                || chars[pos] == '_'
                                || chars[pos] == '-')
                        {
                            id.push(chars[pos]);
                            pos += 1;
                        }
                        if let Some(node) = doc.arena.get_mut(key) {
                            node.set_attr("id", &id);
                        }
                    }
                    Some('.') => {
                        pos += 1;
                        let mut class = String::new();
                        while pos < chars.len()
                            && (chars[pos].is_alphanumeric()
                                || chars[pos] == '_'
                                || chars[pos] == '-')
                        {
                            class.push(chars[pos]);
                            pos += 1;
                        }
                        if let Some(node) = doc.arena.get_mut(key) {
                            let existing = node.attr("class").unwrap_or("").to_string();
                            let merged = if existing.is_empty() {
                                class
                            } else {
                                format!("{} {}", existing, class)
                            };
                            node.set_attr("class", &merged);
                        }
                    }
                    Some(_) => {
                        let mut name = String::new();
                        while pos < chars.len()
                            && (chars[pos].is_alphanumeric()
                                || chars[pos] == '_'
                                || chars[pos] == '-'
                                || chars[pos] == ':')
                        {
                            name.push(chars[pos]);
                            pos += 1;
                        }
                        if name.is_empty() {
                            pos += 1;
                            continue;
                        }
                        while pos < chars.len() && chars[pos].is_whitespace() {
                            pos += 1;
                        }
                        let mut value = String::new();
                        let mut has_value = false;
                        if chars.get(pos) == Some(&'=') {
                            has_value = true;
                            pos += 1;
                            while pos < chars.len() && chars[pos].is_whitespace() {
                                pos += 1;
                            }
                            match chars.get(pos) {
                                Some(&q) if q == '"' || q == '\'' => {
                                    pos += 1;
                                    while pos < chars.len() && chars[pos] != q {
                                        value.push(chars[pos]);
                                        pos += 1;
                                    }
                                    pos += 1;
                                }
                                _ => {
                                    while pos < chars.len()
                                        && !chars[pos].is_whitespace()
                                        && chars[pos] != '>'
                                        && chars[pos] != '/'
                                    {
                                        value.push(chars[pos]);
                                        pos += 1;
                                    }
                                }
                            }
                        }
                        if let Some(node) = doc.arena.get_mut(key) {
                            if has_value {
                                node.set_attr(&name, &decode_entities(&value));
                            } else {
                                node.set_attr(&name, "");
                            }
                        }
                    }
                }
            }
            if let Some(node) = doc.arena.get_mut(key) {
                let checkable = tag_lower == "input"
                    || (tag_lower == "button" && node.attr("type") == Some("checkbox"));
                if checkable && node.attr("checked").is_some() {
                    node.states.checked = true;
                }
            }
            let current = *stack.last().unwrap();
            let merged_into_root = tag_lower == "html"
                && current == doc.root
                && doc.arena.get(current).map_or(false, |n| n.tag == "html");
            if merged_into_root {
                let attrs = doc.arena.get(key).map(|n| n.attrs.clone()).unwrap_or_default();
                for (name, value) in attrs {
                    if let Some(root) = doc.arena.get_mut(current) {
                        if name == "class" {
                            let existing = root.attr("class").unwrap_or("").to_string();
                            let merged = if existing.is_empty() {
                                value
                            } else {
                                format!("{} {}", existing, value)
                            };
                            root.set_attr("class", &merged);
                        } else {
                            root.set_attr(&name, &value);
                        }
                    }
                }
                doc.arena.remove(key);
            } else {
                doc.append_child(current, key);
            }
            if tag_lower == "style" || tag_lower == "script" {
                let close = format!("</{}", tag_lower);
                let end = find_sub_ci(&chars, pos, &close).unwrap_or(chars.len());
                let content: String = chars[pos..end].iter().collect();
                if tag_lower == "style" {
                    page.styles.push(content);
                } else {
                    page.scripts.push(content);
                }
                pos = end;
                if let Some(gt) = chars[pos..].iter().position(|&c| c == '>') {
                    pos += gt + 1;
                }
                continue;
            }
            if !self_closing && !VOID_TAGS.contains(&tag_lower.as_str()) {
                stack.push(if merged_into_root { current } else { key });
            }
        } else {
            let start = pos;
            while pos < chars.len() && chars[pos] != '<' {
                pos += 1;
            }
            let text: String = chars[start..pos].iter().collect();
            let processed = if preserve_ws {
                decode_entities(&text)
            } else {
                collapse_ws(&text)
            };
            if !processed.is_empty() {
                let tkey = doc.create_text(&processed);
                let current = *stack.last().unwrap();
                doc.append_child(current, tkey);
            }
        }
    }
    page
}

fn collapse_ws(text: &str) -> String {
    let collapsed: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    decode_entities(&collapsed)
}

pub fn decode_entities(text: &str) -> String {
    if !text.contains('&') {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    let mut chars = text.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        if c != '&' {
            out.push(c);
            continue;
        }
        let rest = &text[i + 1..];
        let end = rest.find(';');
        match end {
            Some(semi) if semi <= 8 => {
                let entity = &rest[..semi];
                let replacement = match entity {
                    "lt" => Some('<'),
                    "gt" => Some('>'),
                    "amp" => Some('&'),
                    "quot" => Some('"'),
                    "apos" => Some('\''),
                    "nbsp" => Some('\u{a0}'),
                    "copy" => Some('\u{a9}'),
                    "reg" => Some('\u{ae}'),
                    "times" => Some('\u{d7}'),
                    "mdash" => Some('\u{2014}'),
                    "ndash" => Some('\u{2013}'),
                    "hellip" => Some('\u{2026}'),
                    "rarr" => Some('\u{2192}'),
                    "larr" => Some('\u{2190}'),
                    "raquo" => Some('\u{bb}'),
                    "laquo" => Some('\u{ab}'),
                    _ if entity.starts_with("#x") || entity.starts_with("#X") => {
                        u32::from_str_radix(&entity[2..], 16).ok().and_then(char::from_u32)
                    }
                    _ if entity.starts_with('#') => {
                        entity[1..].parse::<u32>().ok().and_then(char::from_u32)
                    }
                    _ => None,
                };
                match replacement {
                    Some(r) => {
                        out.push(r);
                        for _ in 0..=semi {
                            chars.next();
                        }
                    }
                    None => out.push('&'),
                }
            }
            _ => out.push('&'),
        }
    }
    out
}

fn find_sub(chars: &[char], from: usize, needle: &str) -> Option<usize> {
    let needle: Vec<char> = needle.chars().collect();
    let mut i = from;
    while i + needle.len() <= chars.len() {
        if chars[i..i + needle.len()] == needle[..] {
            return Some(i);
        }
        i += 1;
    }
    None
}

fn find_sub_ci(chars: &[char], from: usize, needle: &str) -> Option<usize> {
    let needle: Vec<char> = needle.chars().flat_map(|c| c.to_lowercase()).collect();
    let mut i = from;
    while i + needle.len() <= chars.len() {
        let window: String = chars[i..i + needle.len()].iter().collect::<String>().to_lowercase();
        let target: String = needle.iter().collect();
        if window == target {
            return Some(i);
        }
        i += 1;
    }
    None
}
