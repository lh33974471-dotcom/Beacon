use super::interp::{
    loose_eq, sv_array, sv_object, to_display, Gc, Interp, NativeFnVal, ObjectData, SResult,
    SV,
};
use std::cell::RefCell;

// Open a URL/file with the OS handler (Sciter.launch). Dependency-free: shell
// out to the platform opener and never block the UI on it.
fn open_external(target: &str) {
    let mut cmd = if cfg!(target_os = "macos") {
        let mut c = std::process::Command::new("open");
        c.arg(target);
        c
    } else if cfg!(target_os = "windows") {
        let mut c = std::process::Command::new("cmd");
        c.args(["/C", "start", "", target]);
        c
    } else {
        let mut c = std::process::Command::new("xdg-open");
        c.arg(target);
        c
    };
    let _ = cmd.spawn();
}

pub fn native_fn(
    name: &str,
    f: impl Fn(&mut Interp, &SV, &[SV]) -> SResult<SV> + 'static,
) -> SV {
    SV::NativeFn(Gc::new(NativeFnVal {
        name: name.to_string(),
        f: Box::new(f),
    }))
}

pub fn new_object(props: Vec<(String, SV)>) -> SV {
    sv_object(ObjectData {
        class: RefCell::new(None),
        props: RefCell::new(props),
        native: None,
    })
}

fn arg(argv: &[SV], i: usize) -> SV {
    argv.get(i).cloned().unwrap_or(SV::Undefined)
}

fn as_str(v: &SV) -> String {
    to_display(v)
}

fn as_int(v: &SV) -> i64 {
    match v {
        SV::Int(i) => *i,
        SV::Float(x) => *x as i64,
        SV::Unit(x, _) => *x as i64,
        SV::Bool(b) => *b as i64,
        SV::Str(s) => s.trim().parse().unwrap_or(0),
        _ => 0,
    }
}

fn char_to_byte(text: &str, chars: usize) -> usize {
    text.char_indices().nth(chars).map_or(text.len(), |(b, _)| b)
}

fn as_float(v: &SV) -> f64 {
    match v {
        SV::Int(i) => *i as f64,
        SV::Float(x) => *x,
        SV::Unit(x, _) => *x,
        SV::Str(s) => s.trim().parse().unwrap_or(0.0),
        _ => 0.0,
    }
}

pub fn builtin_member(_interp: &mut Interp, base: &SV, name: &str) -> SV {
    match (base, name) {
        (SV::Str(s), "length") => SV::Int(s.chars().count() as i64),
        (SV::Array(a), "length") => SV::Int(a.borrow().len() as i64),
        (SV::Regex(r), "source") => SV::Str(r.source.as_str().into()),
        _ => SV::Undefined,
    }
}

pub fn call_builtin_method(interp: &mut Interp, base: &SV, name: &str, argv: &[SV]) -> SResult<SV> {
    match base {
        SV::Str(s) => string_method(interp, s, name, argv),
        SV::Array(a) => array_method(interp, a, name, argv),
        SV::Int(_) | SV::Float(_) | SV::Unit(..) => number_method(interp, base, name, argv),
        SV::Symbol(s) => match name {
            "toString" => Ok(SV::Str(format!("#{}", s).into())),
            _ => interp.throw(format!("no method '{}' on symbol", name)),
        },
        SV::Regex(r) => match name {
            "test" => {
                let text = as_str(&arg(argv, 0));
                let re = compile_regex(interp, &r.source, &r.flags)?;
                Ok(SV::Bool(re.find(&text).is_some()))
            }
            "exec" => {
                let text = as_str(&arg(argv, 0));
                let re = compile_regex(interp, &r.source, &r.flags)?;
                Ok(match re.find(&text) {
                    Some(m) => sv_array(vec![SV::Str(text[m.range()].into())]),
                    None => SV::Null,
                })
            }
            _ => interp.throw(format!("no method '{}' on regexp", name)),
        },
        SV::Bool(_) => match name {
            "toString" => Ok(SV::Str(to_display(base).into())),
            _ => interp.throw(format!("no method '{}' on boolean", name)),
        },
        SV::Function(_) | SV::NativeFn(_) => match name {
            "call" => {
                let this = arg(argv, 0);
                interp.call_value(base, &this, argv.get(1..).unwrap_or(&[]))
            }
            "apply" => {
                let this = arg(argv, 0);
                let args = match arg(argv, 1) {
                    SV::Array(a) => a.borrow().clone(),
                    _ => Vec::new(),
                };
                interp.call_value(base, &this, &args)
            }
            _ => interp.throw(format!("no method '{}' on function", name)),
        },
        _ => interp.throw(format!(
            "no method '{}' on {}",
            name,
            super::interp::type_symbol(base)
        )),
    }
}

fn string_method(interp: &mut Interp, s: &Gc<str>, name: &str, argv: &[SV]) -> SResult<SV> {
    let text: &str = s;
    match name {
        "toString" => Ok(SV::Str(s.clone())),
        "trim" => Ok(SV::Str(text.trim().into())),
        "toLowerCase" => Ok(SV::Str(text.to_lowercase().into())),
        "toUpperCase" => Ok(SV::Str(text.to_uppercase().into())),
        // Sciter string comparison: <0 / 0 / >0. Used by the file-transfer
        // folder sort; without it the sort threw and the panel stayed blank.
        "lexicalCompare" | "localeCompare" => {
            let other = to_display(argv.first().unwrap_or(&SV::Undefined));
            let ord = match text.cmp(other.as_str()) {
                std::cmp::Ordering::Less => -1,
                std::cmp::Ordering::Equal => 0,
                std::cmp::Ordering::Greater => 1,
            };
            Ok(SV::Int(ord))
        }
        "toInteger" => {
            let t = text.trim();
            match t.parse::<i64>() {
                Ok(v) => Ok(SV::Int(v)),
                Err(_) => match t.parse::<f64>() {
                    Ok(v) => Ok(SV::Int(v as i64)),
                    Err(_) => Ok(arg(argv, 0)),
                },
            }
        }
        "toFloat" => match text.trim().parse::<f64>() {
            Ok(v) => Ok(SV::Float(v)),
            Err(_) => Ok(arg(argv, 0)),
        },
        "toNumber" => {
            let t = text.trim();
            if let Ok(v) = t.parse::<i64>() {
                Ok(SV::Int(v))
            } else if let Ok(v) = t.parse::<f64>() {
                Ok(SV::Float(v))
            } else {
                Ok(arg(argv, 0))
            }
        }
        "indexOf" => {
            let needle = as_str(&arg(argv, 0));
            let len = text.chars().count() as i64;
            let from = if argv.len() > 1 { as_int(&argv[1]).clamp(0, len) } else { 0 };
            let start = char_to_byte(&text, from as usize);
            Ok(match text[start..].find(&needle) {
                Some(byte_idx) => SV::Int(text[..start + byte_idx].chars().count() as i64),
                None => SV::Int(-1),
            })
        }
        "lastIndexOf" => {
            let needle = as_str(&arg(argv, 0));
            let len = text.chars().count() as i64;
            let from = if argv.len() > 1 { as_int(&argv[1]).clamp(0, len) } else { len };
            let end_char = (from + needle.chars().count() as i64).min(len) as usize;
            let end = char_to_byte(&text, end_char);
            Ok(match text[..end].rfind(&needle) {
                Some(byte_idx) => SV::Int(text[..byte_idx].chars().count() as i64),
                None => SV::Int(-1),
            })
        }
        "charAt" => {
            let i = as_int(&arg(argv, 0));
            Ok(SV::Str(
                text.chars()
                    .nth(i.max(0) as usize)
                    .map(|c| c.to_string())
                    .unwrap_or_default()
                    .into(),
            ))
        }
        "charCodeAt" => {
            let i = as_int(&arg(argv, 0));
            Ok(text
                .chars()
                .nth(i.max(0) as usize)
                .map(|c| SV::Int(c as i64))
                .unwrap_or(SV::Undefined))
        }
        "substr" => {
            let chars: Vec<char> = text.chars().collect();
            let start = normalize_index(as_int(&arg(argv, 0)), chars.len());
            let len = match argv.get(1) {
                Some(v) => as_int(v).max(0) as usize,
                None => chars.len().saturating_sub(start),
            };
            let out: String = chars.iter().skip(start).take(len).collect();
            Ok(SV::Str(out.into()))
        }
        "substring" => {
            let chars: Vec<char> = text.chars().collect();
            let mut a = normalize_index(as_int(&arg(argv, 0)), chars.len());
            let mut b = match argv.get(1) {
                Some(v) => normalize_index(as_int(v), chars.len()),
                None => chars.len(),
            };
            if a > b {
                std::mem::swap(&mut a, &mut b);
            }
            let out: String = chars[a..b.min(chars.len())].iter().collect();
            Ok(SV::Str(out.into()))
        }
        "slice" => {
            let chars: Vec<char> = text.chars().collect();
            let a = normalize_index(as_int(&arg(argv, 0)), chars.len());
            let b = match argv.get(1) {
                Some(v) => normalize_index(as_int(v), chars.len()),
                None => chars.len(),
            };
            let out: String = if a < b { chars[a..b].iter().collect() } else { String::new() };
            Ok(SV::Str(out.into()))
        }
        "replace" => {
            let with = as_str(&arg(argv, 1));
            match &arg(argv, 0) {
                SV::Regex(r) => {
                    let re = compile_regex(interp, &r.source, &r.flags)?;
                    let global = r.flags.contains('g');
                    let mut out = String::new();
                    let mut last = 0usize;
                    for m in re.find_iter(text) {
                        let range = m.range();
                        out.push_str(&text[last..range.start]);
                        out.push_str(&with);
                        last = range.end;
                        if !global {
                            break;
                        }
                        if range.is_empty() {
                            break;
                        }
                    }
                    out.push_str(&text[last..]);
                    Ok(SV::Str(out.into()))
                }
                pat => {
                    let pat = as_str(pat);
                    Ok(SV::Str(text.replacen(&pat, &with, 1).into()))
                }
            }
        }
        "split" => {
            let parts: Vec<SV> = match &arg(argv, 0) {
                SV::Regex(r) => {
                    let re = compile_regex(interp, &r.source, &r.flags)?;
                    let mut out = Vec::new();
                    let mut last = 0usize;
                    for m in re.find_iter(text) {
                        let range = m.range();
                        if range.is_empty() {
                            continue;
                        }
                        out.push(SV::Str(text[last..range.start].into()));
                        last = range.end;
                    }
                    out.push(SV::Str(text[last..].into()));
                    out
                }
                sep => {
                    let sep = as_str(sep);
                    if sep.is_empty() {
                        text.chars().map(|c| SV::Str(c.to_string().into())).collect()
                    } else {
                        text.split(&sep as &str).map(|p| SV::Str(p.into())).collect()
                    }
                }
            };
            Ok(sv_array(parts))
        }
        "match" => match &arg(argv, 0) {
            SV::Regex(r) => {
                let re = compile_regex(interp, &r.source, &r.flags)?;
                let global = r.flags.contains('g');
                let mut out = Vec::new();
                for m in re.find_iter(text) {
                    out.push(SV::Str(text[m.range()].into()));
                    if !global {
                        break;
                    }
                }
                if out.is_empty() {
                    Ok(SV::Null)
                } else {
                    Ok(sv_array(out))
                }
            }
            _ => Ok(SV::Null),
        },
        "startsWith" => Ok(SV::Bool(text.starts_with(&as_str(&arg(argv, 0))))),
        "endsWith" => Ok(SV::Bool(text.ends_with(&as_str(&arg(argv, 0))))),
        "includes" => Ok(SV::Bool(text.contains(&as_str(&arg(argv, 0))))),
        "urlUnescape" => Ok(SV::Str(url_unescape(text).into())),
        "urlEscape" => Ok(SV::Str(url_escape(text).into())),
        "htmlEscape" => Ok(SV::Str(html_escape(text).into())),
        "repeat" => {
            let n = as_int(&arg(argv, 0)).max(0) as usize;
            Ok(SV::Str(text.repeat(n).into()))
        }
        "padStart" => {
            let n = as_int(&arg(argv, 0)).max(0) as usize;
            let pad = match argv.get(1) {
                Some(v) => as_str(v),
                None => " ".into(),
            };
            let mut out = String::new();
            while out.chars().count() + text.chars().count() < n {
                out.push_str(&pad);
            }
            let deficit = n.saturating_sub(text.chars().count());
            let out: String = out.chars().take(deficit).collect();
            Ok(SV::Str(format!("{}{}", out, text).into()))
        }
        _ => interp.throw(format!("no method '{}' on string", name)),
    }
}

fn normalize_index(i: i64, len: usize) -> usize {
    if i < 0 {
        len.saturating_sub((-i) as usize)
    } else {
        (i as usize).min(len)
    }
}

fn array_method(
    interp: &mut Interp,
    a: &Gc<RefCell<Vec<SV>>>,
    name: &str,
    argv: &[SV],
) -> SResult<SV> {
    match name {
        "push" => {
            for v in argv {
                a.borrow_mut().push(v.clone());
            }
            Ok(SV::Int(a.borrow().len() as i64))
        }
        "pop" => Ok(a.borrow_mut().pop().unwrap_or(SV::Undefined)),
        "shift" => {
            let mut b = a.borrow_mut();
            if b.is_empty() {
                Ok(SV::Undefined)
            } else {
                Ok(b.remove(0))
            }
        }
        "unshift" => {
            for (i, v) in argv.iter().enumerate() {
                a.borrow_mut().insert(i, v.clone());
            }
            Ok(SV::Int(a.borrow().len() as i64))
        }
        "indexOf" => {
            let needle = arg(argv, 0);
            Ok(a.borrow()
                .iter()
                .position(|v| loose_eq(v, &needle))
                .map(|i| SV::Int(i as i64))
                .unwrap_or(SV::Int(-1)))
        }
        "join" => {
            let sep = match argv.first() {
                Some(v) => as_str(v),
                None => ",".into(),
            };
            let items: Vec<String> = a.borrow().iter().map(to_display).collect();
            Ok(SV::Str(items.join(&sep).into()))
        }
        "concat" => {
            let mut out = a.borrow().clone();
            for v in argv {
                match v {
                    SV::Array(other) => out.extend(other.borrow().iter().cloned()),
                    other => out.push(other.clone()),
                }
            }
            Ok(sv_array(out))
        }
        "slice" => {
            let items = a.borrow();
            let start = normalize_index(as_int(&arg(argv, 0)), items.len());
            let end = match argv.get(1) {
                Some(v) => normalize_index(as_int(v), items.len()),
                None => items.len(),
            };
            let out: Vec<SV> = if start < end {
                items[start..end].to_vec()
            } else {
                Vec::new()
            };
            Ok(sv_array(out))
        }
        "splice" => {
            let mut items = a.borrow_mut();
            let start = normalize_index(as_int(&arg(argv, 0)), items.len());
            let count = match argv.get(1) {
                Some(v) => as_int(v).max(0) as usize,
                None => items.len() - start,
            };
            let end = (start + count).min(items.len());
            let removed: Vec<SV> = items.drain(start..end).collect();
            for (i, v) in argv.iter().skip(2).enumerate() {
                items.insert(start + i, v.clone());
            }
            Ok(sv_array(removed))
        }
        "map" => {
            let f = arg(argv, 0);
            let items = a.borrow().clone();
            let mut out = Vec::with_capacity(items.len());
            for (i, v) in items.iter().enumerate() {
                out.push(interp.call_value(&f, &SV::Undefined, &[v.clone(), SV::Int(i as i64)])?);
            }
            Ok(sv_array(out))
        }
        "filter" => {
            let f = arg(argv, 0);
            let items = a.borrow().clone();
            let mut out = Vec::new();
            for (i, v) in items.iter().enumerate() {
                let keep =
                    interp.call_value(&f, &SV::Undefined, &[v.clone(), SV::Int(i as i64)])?;
                if super::interp::truthy(&keep) {
                    out.push(v.clone());
                }
            }
            Ok(sv_array(out))
        }
        "sort" => {
            let f = argv.first().cloned();
            let mut items = a.borrow().clone();
            let mut err = None;
            items.sort_by(|x, y| {
                if err.is_some() {
                    return std::cmp::Ordering::Equal;
                }
                match &f {
                    Some(f) if !matches!(f, SV::Undefined) => {
                        match interp.call_value(f, &SV::Undefined, &[x.clone(), y.clone()]) {
                            Ok(r) => {
                                let n = as_float(&r);
                                n.partial_cmp(&0.0).unwrap_or(std::cmp::Ordering::Equal)
                            }
                            Err(e) => {
                                err = Some(e);
                                std::cmp::Ordering::Equal
                            }
                        }
                    }
                    _ => to_display(x).cmp(&to_display(y)),
                }
            });
            if let Some(e) = err {
                return Err(e);
            }
            *a.borrow_mut() = items;
            Ok(SV::Array(a.clone()))
        }
        "reverse" => {
            a.borrow_mut().reverse();
            Ok(SV::Array(a.clone()))
        }
        "clear" => {
            a.borrow_mut().clear();
            Ok(SV::Undefined)
        }
        "remove" => {
            let i = as_int(&arg(argv, 0));
            let mut items = a.borrow_mut();
            if i >= 0 && (i as usize) < items.len() {
                Ok(items.remove(i as usize))
            } else {
                Ok(SV::Undefined)
            }
        }
        _ => interp.throw(format!("no method '{}' on array", name)),
    }
}

fn number_method(interp: &mut Interp, base: &SV, name: &str, argv: &[SV]) -> SResult<SV> {
    let x = as_float(base);
    match name {
        "toString" => Ok(SV::Str(to_display(base).into())),
        "toInteger" => Ok(SV::Int(x as i64)),
        "toFloat" => Ok(SV::Float(x)),
        "toFixed" => {
            let digits = as_int(&arg(argv, 0)).max(0) as usize;
            Ok(SV::Str(format!("{:.*}", digits, x).into()))
        }
        "limit" => {
            let lo = as_float(&arg(argv, 0));
            let hi = as_float(&arg(argv, 1));
            let v = x.max(lo).min(hi);
            match base {
                SV::Int(_) => Ok(SV::Int(v as i64)),
                _ => Ok(SV::Float(v)),
            }
        }
        "min" => {
            let mut m = x;
            for v in argv {
                m = m.min(as_float(v));
            }
            Ok(SV::Float(m))
        }
        "max" => {
            let mut m = x;
            for v in argv {
                m = m.max(as_float(v));
            }
            Ok(SV::Float(m))
        }
        _ => interp.throw(format!("no method '{}' on number", name)),
    }
}

pub fn install_globals(interp: &mut Interp) {
    let g = interp.global.clone();

    g.define(
        "stdout",
        new_object(vec![
            (
                "println".into(),
                native_fn("println", |interp, _this, argv| {
                    let line: Vec<String> = argv.iter().map(to_display).collect();
                    interp.output.push(line.join(" "));
                    Ok(SV::Undefined)
                }),
            ),
            (
                "printf".into(),
                native_fn("printf", |interp, _this, argv| {
                    let line: Vec<String> = argv.iter().map(to_display).collect();
                    interp.output.push(line.join(" "));
                    Ok(SV::Undefined)
                }),
            ),
        ]),
    );

    g.define(
        "Math",
        new_object(vec![
            (
                "abs".into(),
                native_fn("abs", |_i, _t, a| Ok(SV::Float(as_float(&arg(a, 0)).abs()))),
            ),
            (
                "sqrt".into(),
                native_fn("sqrt", |_i, _t, a| {
                    Ok(SV::Float(as_float(&arg(a, 0)).sqrt()))
                }),
            ),
            (
                "round".into(),
                native_fn("round", |_i, _t, a| {
                    Ok(SV::Int(as_float(&arg(a, 0)).round() as i64))
                }),
            ),
            (
                "floor".into(),
                native_fn("floor", |_i, _t, a| {
                    Ok(SV::Int(as_float(&arg(a, 0)).floor() as i64))
                }),
            ),
            (
                "ceil".into(),
                native_fn("ceil", |_i, _t, a| {
                    Ok(SV::Int(as_float(&arg(a, 0)).ceil() as i64))
                }),
            ),
            (
                "min".into(),
                native_fn("min", |_i, _t, a| {
                    let mut m = f64::INFINITY;
                    for v in a {
                        m = m.min(as_float(v));
                    }
                    Ok(SV::Float(m))
                }),
            ),
            (
                "max".into(),
                native_fn("max", |_i, _t, a| {
                    let mut m = f64::NEG_INFINITY;
                    for v in a {
                        m = m.max(as_float(v));
                    }
                    Ok(SV::Float(m))
                }),
            ),
            (
                "random".into(),
                native_fn("random", |_i, _t, _a| Ok(SV::Float(0.5))),
            ),
            (
                "sin".into(),
                native_fn("sin", |_i, _t, a| Ok(SV::Float(as_float(&arg(a, 0)).sin()))),
            ),
            (
                "cos".into(),
                native_fn("cos", |_i, _t, a| Ok(SV::Float(as_float(&arg(a, 0)).cos()))),
            ),
            (
                "tan".into(),
                native_fn("tan", |_i, _t, a| Ok(SV::Float(as_float(&arg(a, 0)).tan()))),
            ),
            (
                "atan2".into(),
                native_fn("atan2", |_i, _t, a| {
                    Ok(SV::Float(as_float(&arg(a, 0)).atan2(as_float(&arg(a, 1)))))
                }),
            ),
            ("PI".into(), SV::Float(std::f64::consts::PI)),
            ("E".into(), SV::Float(std::f64::consts::E)),
        ]),
    );

    g.define(
        "JSON",
        new_object(vec![
            (
                "parse".into(),
                native_fn("parse", |interp, _t, a| {
                    let text = as_str(&arg(a, 0));
                    json_parse(interp, &text)
                }),
            ),
            (
                "stringify".into(),
                native_fn("stringify", |_i, _t, a| {
                    Ok(SV::Str(json_stringify(&arg(a, 0)).into()))
                }),
            ),
        ]),
    );

    g.define("Date", make_date_class());

    g.define(
        "Array",
        native_fn("Array", |_i, _t, a| {
            if a.len() == 1 {
                if let SV::Int(n) = a[0] {
                    if n >= 0 {
                        return Ok(sv_array(vec![SV::Undefined; n as usize]));
                    }
                }
            }
            Ok(sv_array(a.to_vec()))
        }),
    );

    g.define(
        "Wireui",
        new_object(vec![(
            "launch".into(),
            native_fn("launch", |_i, _t, a| {
                let url = as_str(&arg(a, 0));
                if !url.is_empty() {
                    open_external(&url);
                }
                Ok(SV::Bool(true))
            }),
        )]),
    );

    g.define(
        "System",
        new_object(vec![(
            "path".into(),
            native_fn("path", |_i, _t, a| {
                let folder = match a.first() {
                    Some(SV::Symbol(s)) => s.to_string(),
                    _ => String::new(),
                };
                let name = as_str(&arg(a, 1));
                let home = std::env::var("HOME")
                    .or_else(|_| std::env::var("USERPROFILE"))
                    .unwrap_or_default();
                let base = match folder.as_str() {
                    "USER_DOCUMENTS" => format!("{}/Documents", home),
                    "USER_APPDATA" => std::env::var("APPDATA").unwrap_or_else(|_| home.clone()),
                    _ => home.clone(),
                };
                let path = if name.is_empty() {
                    base
                } else {
                    format!("{}/{}", base, name)
                };
                Ok(SV::Str(path.into()))
            }),
        )]),
    );

    g.define(
        "URL",
        new_object(vec![
            (
                "toPath".into(),
                native_fn("toPath", |_i, _t, a| {
                    Ok(SV::Str(
                        crate::engine::host::strip_file_url(&as_str(&arg(a, 0))).into(),
                    ))
                }),
            ),
            (
                "fromPath".into(),
                native_fn("fromPath", |_i, _t, a| {
                    Ok(SV::Str(
                        crate::engine::host::path_to_file_url(&as_str(&arg(a, 0))).into(),
                    ))
                }),
            ),
        ]),
    );

    g.define(
        "String",
        new_object(vec![
            (
                "printf".into(),
                native_fn("printf", |_i, _t, a| Ok(SV::Str(sprintf(a).into()))),
            ),
            (
                "fromCharCode".into(),
                native_fn("fromCharCode", |_i, _t, a| {
                    let s: String = a
                        .iter()
                        .filter_map(|v| match v {
                            SV::Int(n) => u32::try_from(*n).ok().and_then(char::from_u32),
                            SV::Float(f) => u32::try_from(*f as i64).ok().and_then(char::from_u32),
                            _ => None,
                        })
                        .collect();
                    Ok(SV::Str(s.into()))
                }),
            ),
        ]),
    );

    g.define(
        "Color",
        native_fn("Color", |_i, _t, _a| Ok(SV::Int(0))),
    );

    g.define("Image", crate::engine::host::image_global_sv());

    g.define(
        "color",
        native_fn("color", |_i, _t, a| {
            let chan = |i: usize| (as_float(&arg(a, i)).clamp(0.0, 255.0)) as u32;
            let alpha = match a.get(3) {
                None => 255u32,
                Some(v) => {
                    let f = as_float(v);
                    if f <= 1.0 { (f * 255.0) as u32 } else { f.clamp(0.0, 255.0) as u32 }
                }
            };
            Ok(SV::Int(
                ((alpha << 24) | (chan(2) << 16) | (chan(1) << 8) | chan(0)) as i64,
            ))
        }),
    );

    let event_consts: Vec<(String, SV)> = [
        ("MOUSE", 0x1),
        ("KEY", 0x2),
        ("FOCUS", 0x4),
        ("SCROLL", 0x8),
        ("TIMER", 0x10),
        ("SIZE", 0x20),
        ("DRAW", 0x40),
        ("MOUSE_DOWN", 0x1001),
        ("MOUSE_UP", 0x1002),
        ("MOUSE_MOVE", 0x1003),
        ("MOUSE_ENTER", 0x1004),
        ("MOUSE_LEAVE", 0x1005),
        ("MOUSE_WHEEL", 0x1007),
        ("MOUSE_DCLICK", 0x1006),
        ("KEY_DOWN", 0x2001),
        ("KEY_UP", 0x2002),
        ("KEY_CHAR", 0x2003),
        ("GOT_FOCUS", 0x4001),
        ("LOST_FOCUS", 0x4002),
        ("SINKING", 0x8000),
        ("HANDLED", 0x10000),
        ("VK_ENTER", 13),
        ("VK_RETURN", 13),
        ("VK_ESCAPE", 27),
        ("VK_TAB", 9),
        ("VK_SPACE", 32),
        ("VK_BACK", 8),
        ("VK_DELETE", 46),
        ("VK_INSERT", 45),
        ("VK_HOME", 36),
        ("VK_END", 35),
        ("VK_PRIOR", 33),
        ("VK_NEXT", 34),
        ("VK_UP", 38),
        ("VK_DOWN", 40),
        ("VK_LEFT", 37),
        ("VK_RIGHT", 39),
        ("VK_F1", 0x70),
        ("VK_F2", 0x71),
        ("VK_F3", 0x72),
        ("VK_F4", 0x73),
        ("VK_F5", 0x74),
        ("VK_F6", 0x75),
        ("VK_F7", 0x76),
        ("VK_F8", 0x77),
        ("VK_F9", 0x78),
        ("VK_F10", 0x79),
        ("VK_F11", 0x7A),
        ("VK_F12", 0x7B),
        ("VK_SNAPSHOT", 0x2C),
        ("VK_PAUSE", 0x13),
        // Full VK_ letter/digit set so remote.tis's vk_keymap (built by
        // iterating Event) names every key the client's KEY_MAP expects
        // (VK_A..VK_Z, VK_0..VK_9 -- Windows VK codes, matching vk_keycode()).
        ("VK_SHIFT", 16),
        ("VK_CONTROL", 17),
        ("VK_MENU", 18),
        ("VK_RMENU", 165),
        ("VK_LWIN", 91),
        ("VK_APPS", 93),
        ("VK_SEMICOLON", 0xBA),
        ("VK_PLUS", 0xBB),
        ("VK_COMMA", 0xBC),
        ("VK_MINUS", 0xBD),
        ("VK_PERIOD", 0xBE),
        ("VK_SLASH", 0xBF),
        ("VK_LBRACKET", 0xDB),
        ("VK_BACKSLASH", 0xDC),
        ("VK_RBRACKET", 0xDD),
        ("VK_QUOTE", 0xDE),
        ("VK_NUMPAD0", 0x60),
        ("VK_NUMPAD1", 0x61),
        ("VK_NUMPAD2", 0x62),
        ("VK_NUMPAD3", 0x63),
        ("VK_NUMPAD4", 0x64),
        ("VK_NUMPAD5", 0x65),
        ("VK_NUMPAD6", 0x66),
        ("VK_NUMPAD7", 0x67),
        ("VK_NUMPAD8", 0x68),
        ("VK_NUMPAD9", 0x69),
        ("VK_MULTIPLY", 0x6A),
        ("VK_ADD", 0x6B),
        ("VK_SUBTRACT", 0x6D),
        ("VK_DECIMAL", 0x6E),
        ("VK_DIVIDE", 0x6F),
        ("VK_A", 65),
        ("VK_B", 66),
        ("VK_C", 67),
        ("VK_D", 68),
        ("VK_E", 69),
        ("VK_F", 70),
        ("VK_G", 71),
        ("VK_H", 72),
        ("VK_I", 73),
        ("VK_J", 74),
        ("VK_K", 75),
        ("VK_L", 76),
        ("VK_M", 77),
        ("VK_N", 78),
        ("VK_O", 79),
        ("VK_P", 80),
        ("VK_Q", 81),
        ("VK_R", 82),
        ("VK_S", 83),
        ("VK_T", 84),
        ("VK_U", 85),
        ("VK_V", 86),
        ("VK_W", 87),
        ("VK_Y", 89),
        ("VK_X", 88),
        ("VK_Z", 90),
        ("VK_0", 48),
        ("VK_1", 49),
        ("VK_2", 50),
        ("VK_3", 51),
        ("VK_4", 52),
        ("VK_5", 53),
        ("VK_6", 54),
        ("VK_7", 55),
        ("VK_8", 56),
        ("VK_9", 57),
    ]
    .iter()
    .map(|(k, v)| (k.to_string(), SV::Int(*v)))
    .collect();
    g.define("Event", new_object(event_consts));

    let view_consts: Vec<(String, SV)> = [
        ("WINDOW_SHOWN", 1),
        ("WINDOW_MINIMIZED", 2),
        ("WINDOW_MAXIMIZED", 3),
        ("WINDOW_HIDDEN", 4),
        ("WINDOW_FULL_SCREEN", 5),
        ("FRAME_WINDOW", 0),
        ("TOOL_WINDOW", 2),
        ("POPUP_WINDOW", 3),
        ("DIALOG_WINDOW", 4),
        ("FRAME", 0),
    ]
    .iter()
    .map(|(k, v)| (k.to_string(), SV::Int(*v)))
    .collect();
    g.define("View", new_object(view_consts));
}

fn now_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(windows)]
fn os_locale_datetime(ms: i64) -> Option<String> {
    use chrono::{Datelike, TimeZone, Timelike};
    use winapi::um::datetimeapi::{GetDateFormatW, GetTimeFormatW};
    use winapi::um::minwinbase::SYSTEMTIME;
    const LOCALE_USER_DEFAULT: u32 = 0x0400;
    const DATE_SHORTDATE: u32 = 0x0000_0001;
    const TIME_NOSECONDS: u32 = 0x0000_0002;
    let dt = chrono::Local.timestamp_millis_opt(ms).single()?;
    let st = SYSTEMTIME {
        wYear: dt.year().try_into().ok()?,
        wMonth: dt.month() as u16,
        wDayOfWeek: dt.weekday().num_days_from_sunday() as u16,
        wDay: dt.day() as u16,
        wHour: dt.hour() as u16,
        wMinute: dt.minute() as u16,
        wSecond: dt.second() as u16,
        wMilliseconds: 0,
    };
    let mut date_buf = [0u16; 128];
    let mut time_buf = [0u16; 128];
    unsafe {
        let dn = GetDateFormatW(
            LOCALE_USER_DEFAULT,
            DATE_SHORTDATE,
            &st,
            std::ptr::null(),
            date_buf.as_mut_ptr(),
            date_buf.len() as i32,
        );
        let tn = GetTimeFormatW(
            LOCALE_USER_DEFAULT,
            TIME_NOSECONDS,
            &st,
            std::ptr::null(),
            time_buf.as_mut_ptr(),
            time_buf.len() as i32,
        );
        if dn <= 0 || tn <= 0 {
            return None;
        }
        let date = String::from_utf16_lossy(&date_buf[..(dn - 1) as usize]);
        let time = String::from_utf16_lossy(&time_buf[..(tn - 1) as usize]);
        Some(format!("{} {}", date, time))
    }
}

#[cfg(target_os = "macos")]
fn os_locale_datetime(ms: i64) -> Option<String> {
    use objc::runtime::Object;
    use objc::{class, msg_send, sel, sel_impl};
    objc::rc::autoreleasepool(|| {
        let date: *mut Object = unsafe {
            msg_send![class!(NSDate), dateWithTimeIntervalSince1970: ms as f64 / 1000.0]
        };
        if date.is_null() {
            return None;
        }
        let part = |template: &'static [u8]| -> Option<String> {
            unsafe {
                let fmt: *mut Object = msg_send![class!(NSDateFormatter), alloc];
                let fmt: *mut Object = msg_send![fmt, init];
                if fmt.is_null() {
                    return None;
                }
                let tpl: *mut Object =
                    msg_send![class!(NSString), stringWithUTF8String: template.as_ptr()];
                let _: () = msg_send![fmt, setLocalizedDateFormatFromTemplate: tpl];
                let s: *mut Object = msg_send![fmt, stringFromDate: date];
                let out = if s.is_null() {
                    None
                } else {
                    let utf8: *const std::os::raw::c_char = msg_send![s, UTF8String];
                    if utf8.is_null() {
                        None
                    } else {
                        Some(std::ffi::CStr::from_ptr(utf8).to_string_lossy().into_owned())
                    }
                };
                let _: () = msg_send![fmt, release];
                out
            }
        };
        let date_part = part(b"yMd\0")?;
        let time_part = part(b"jmm\0")?;
        Some(format!("{} {}", date_part, time_part))
    })
}

#[cfg(any(target_os = "linux", target_os = "freebsd"))]
fn os_locale_datetime(ms: i64) -> Option<String> {
    static LC_TIME_INIT: std::sync::Once = std::sync::Once::new();
    LC_TIME_INIT.call_once(|| unsafe {
        libc::setlocale(libc::LC_TIME, b"\0".as_ptr() as *const libc::c_char);
    });
    let secs = ms.div_euclid(1000) as libc::time_t;
    unsafe {
        let mut tm: libc::tm = std::mem::zeroed();
        if libc::localtime_r(&secs, &mut tm).is_null() {
            return None;
        }
        let mut buf = [0u8; 160];
        let n = libc::strftime(
            buf.as_mut_ptr() as *mut libc::c_char,
            buf.len(),
            b"%x %R\0".as_ptr() as *const libc::c_char,
            &tm,
        );
        if n == 0 {
            return None;
        }
        Some(String::from_utf8_lossy(&buf[..n]).into_owned())
    }
}

#[cfg(not(any(
    windows,
    target_os = "macos",
    target_os = "linux",
    target_os = "freebsd"
)))]
fn os_locale_datetime(_ms: i64) -> Option<String> {
    None
}

fn make_date_class() -> SV {
    use super::interp::ClassVal;
    let class = ClassVal {
        name: "Date".into(),
        base: RefCell::new(None),
        methods: RefCell::new(std::collections::HashMap::new()),
        class_props: RefCell::new(Vec::new()),
        events: RefCell::new(Vec::new()),
        class_env: RefCell::new(None),
    };
    {
        let mut m = class.methods.borrow_mut();
        m.insert(
            "this".into(),
            native_fn("this", |interp, this, argv| {
                use chrono::{TimeZone, Timelike};
                let ms = match argv.first() {
                    Some(v) => as_int(v),
                    None => now_millis(),
                };
                interp.member_set(this, "_ms", SV::Int(ms))?;
                let (hour, minute, second) = chrono::Local
                    .timestamp_millis_opt(ms)
                    .single()
                    .map(|dt| (dt.hour() as i64, dt.minute() as i64, dt.second() as i64))
                    .unwrap_or((0, 0, 0));
                interp.member_set(this, "hour", SV::Int(hour))?;
                interp.member_set(this, "minute", SV::Int(minute))?;
                interp.member_set(this, "second", SV::Int(second))?;
                Ok(SV::Undefined)
            }),
        );
        m.insert(
            "valueOf".into(),
            native_fn("valueOf", |interp, this, _| interp.member_get(this, "_ms")),
        );
        m.insert(
            "toLocaleString".into(),
            native_fn("toLocaleString", |interp, this, _| {
                use chrono::TimeZone;
                let ms = as_int(&interp.member_get(this, "_ms")?);
                let s = os_locale_datetime(ms)
                    .map(|s| s.replace(['\u{202f}', '\u{00a0}'], " "))
                    .or_else(|| {
                        chrono::Local
                            .timestamp_millis_opt(ms)
                            .single()
                            .map(|dt| dt.format("%Y-%m-%d %H:%M").to_string())
                    })
                    .unwrap_or_default();
                Ok(SV::Str(s.into()))
            }),
        );
    }
    {
        let mut p = class.class_props.borrow_mut();
        p.push((
            "now".into(),
            native_fn("now", |_i, _t, _a| Ok(SV::Int(now_millis()))),
        ));
        p.push((
            "diff".into(),
            native_fn("diff", |interp, _t, argv| {
                let a = as_int(&interp.call_method(&arg(argv, 0), "valueOf", &[])?);
                let b = as_int(&interp.call_method(&arg(argv, 1), "valueOf", &[])?);
                let delta = b - a;
                let unit = match &arg(argv, 2) {
                    SV::Symbol(s) => s.to_string(),
                    other => to_display(other),
                };
                Ok(match unit.as_str() {
                    "seconds" => SV::Int(delta / 1000),
                    "minutes" => SV::Int(delta / 60_000),
                    "hours" => SV::Int(delta / 3_600_000),
                    "days" => SV::Int(delta / 86_400_000),
                    _ => SV::Int(delta),
                })
            }),
        ));
    }
    SV::Class(Gc::new(class))
}

fn sprintf(argv: &[SV]) -> String {
    let fmt = as_str(&arg(argv, 0));
    let mut out = String::new();
    let mut arg_i = 1;
    let mut chars = fmt.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        let mut spec = String::new();
        while let Some(&n) = chars.peek() {
            spec.push(n);
            chars.next();
            if n.is_ascii_alphabetic() || n == '%' {
                break;
            }
        }
        if spec == "%" {
            out.push('%');
            continue;
        }
        let conv = spec.chars().last().unwrap_or('s');
        let flags = &spec[..spec.len().saturating_sub(1)];
        let zero_pad = flags.starts_with('0');
        let width: usize = flags
            .trim_start_matches(['0', '-', '+', ' '])
            .split('.')
            .next()
            .and_then(|w| w.parse().ok())
            .unwrap_or(0);
        let v = arg(argv, arg_i);
        arg_i += 1;
        let body = match conv {
            'd' | 'i' => as_int(&v).to_string(),
            'f' => {
                let prec: usize = flags
                    .split('.')
                    .nth(1)
                    .and_then(|p| p.parse().ok())
                    .unwrap_or(6);
                format!("{:.*}", prec, as_float(&v))
            }
            'x' => format!("{:x}", as_int(&v)),
            'X' => format!("{:X}", as_int(&v)),
            's' => to_display(&v),
            _ => to_display(&v),
        };
        if body.chars().count() < width {
            let pad = width - body.chars().count();
            let fill = if zero_pad && matches!(conv, 'd' | 'i' | 'x' | 'X' | 'f') {
                '0'
            } else {
                ' '
            };
            out.extend(std::iter::repeat(fill).take(pad));
        }
        out.push_str(&body);
    }
    out
}

fn url_unescape(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("");
            if let Ok(v) = u8::from_str_radix(hex, 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        if bytes[i] == b'+' {
            out.push(b' ');
        } else {
            out.push(bytes[i]);
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn url_escape(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{:02X}", b)),
        }
    }
    out
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn json_stringify(v: &SV) -> String {
    match v {
        SV::Undefined | SV::Null => "null".into(),
        SV::Bool(b) => b.to_string(),
        SV::Int(i) => i.to_string(),
        SV::Float(x) => x.to_string(),
        SV::Unit(x, _) => x.to_string(),
        SV::Str(s) | SV::Symbol(s) => json_quote(s),
        SV::Array(a) => {
            let items: Vec<String> = a.borrow().iter().map(json_stringify).collect();
            format!("[{}]", items.join(","))
        }
        SV::Object(o) => {
            let props = o.props.borrow();
            let items: Vec<String> = props
                .iter()
                .map(|(k, v)| format!("{}:{}", json_quote(k), json_stringify(v)))
                .collect();
            format!("{{{}}}", items.join(","))
        }
        _ => "null".into(),
    }
}

fn json_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

pub fn json_parse(interp: &mut Interp, text: &str) -> SResult<SV> {
    let mut p = JsonParser {
        chars: text.chars().collect(),
        pos: 0,
    };
    p.skip_ws();
    let v = p.value(interp)?;
    Ok(v)
}

struct JsonParser {
    chars: Vec<char>,
    pos: usize,
}

impl JsonParser {
    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    fn bump(&mut self) -> Option<char> {
        let c = self.peek();
        if c.is_some() {
            self.pos += 1;
        }
        c
    }

    fn skip_ws(&mut self) {
        while self.peek().map_or(false, |c| c.is_whitespace()) {
            self.pos += 1;
        }
    }

    fn value(&mut self, interp: &mut Interp) -> SResult<SV> {
        self.skip_ws();
        match self.peek() {
            None => interp.throw("json: unexpected end"),
            Some('{') => {
                self.bump();
                let obj = new_object(Vec::new());
                loop {
                    self.skip_ws();
                    if self.peek() == Some('}') {
                        self.bump();
                        break;
                    }
                    let key = match self.value(interp)? {
                        SV::Str(s) => s.to_string(),
                        other => to_display(&other),
                    };
                    self.skip_ws();
                    if self.bump() != Some(':') {
                        return interp.throw("json: expected ':'");
                    }
                    let v = self.value(interp)?;
                    interp.member_set(&obj, &key, v)?;
                    self.skip_ws();
                    if self.peek() == Some(',') {
                        self.bump();
                    }
                }
                Ok(obj)
            }
            Some('[') => {
                self.bump();
                let mut items = Vec::new();
                loop {
                    self.skip_ws();
                    if self.peek() == Some(']') {
                        self.bump();
                        break;
                    }
                    items.push(self.value(interp)?);
                    self.skip_ws();
                    if self.peek() == Some(',') {
                        self.bump();
                    }
                }
                Ok(sv_array(items))
            }
            Some('"') => {
                self.bump();
                let mut s = String::new();
                loop {
                    match self.bump() {
                        None => return interp.throw("json: unterminated string"),
                        Some('"') => break,
                        Some('\\') => match self.bump() {
                            Some('n') => s.push('\n'),
                            Some('t') => s.push('\t'),
                            Some('r') => s.push('\r'),
                            Some('u') => {
                                let mut code = 0u32;
                                for _ in 0..4 {
                                    match self.bump().and_then(|c| c.to_digit(16)) {
                                        Some(d) => code = code * 16 + d,
                                        None => return interp.throw("json: bad \\u"),
                                    }
                                }
                                s.push(char::from_u32(code).unwrap_or('\u{fffd}'));
                            }
                            Some(c) => s.push(c),
                            None => return interp.throw("json: bad escape"),
                        },
                        Some(c) => s.push(c),
                    }
                }
                Ok(SV::Str(s.into()))
            }
            Some(c) if c == '-' || c.is_ascii_digit() => {
                let start = self.pos;
                self.bump();
                while self
                    .peek()
                    .map_or(false, |c| c.is_ascii_digit() || "+-.eE".contains(c))
                {
                    self.bump();
                }
                let text: String = self.chars[start..self.pos].iter().collect();
                if text.contains('.') || text.contains('e') || text.contains('E') {
                    Ok(SV::Float(text.parse().unwrap_or(0.0)))
                } else {
                    Ok(SV::Int(text.parse().unwrap_or(0)))
                }
            }
            Some('t') => {
                self.pos += 4;
                Ok(SV::Bool(true))
            }
            Some('f') => {
                self.pos += 5;
                Ok(SV::Bool(false))
            }
            Some('n') => {
                self.pos += 4;
                Ok(SV::Null)
            }
            Some(c) => interp.throw(format!("json: unexpected '{}'", c)),
        }
    }
}

fn compile_regex(interp: &mut Interp, source: &str, flags: &str) -> SResult<regress::Regex> {
    let flags: String = flags.chars().filter(|c| "imsu".contains(*c)).collect();
    regress::Regex::with_flags(source, flags.as_str())
        .map_err(|e| match interp.throw::<()>(format!("bad regex /{}/: {}", source, e)) {
            Err(t) => t,
            Ok(_) => unreachable!(),
        })
}
