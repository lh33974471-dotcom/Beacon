use super::interp::{
    sv_array, sv_object, ClassVal, Env, Gc, Interp, NativeObj, ObjectData, SResult, SV,
};
use super::runtime::{native_fn, new_object};
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

pub struct CallLog {
    pub calls: RefCell<Vec<String>>,
}

impl CallLog {
    pub fn new() -> Rc<CallLog> {
        Rc::new(CallLog {
            calls: RefCell::new(Vec::new()),
        })
    }
}

struct PermissiveNative {
    kind: &'static str,
    log: Rc<CallLog>,
    overrides: HashMap<String, SV>,
}

impl NativeObj for PermissiveNative {
    fn type_name(&self) -> &'static str {
        self.kind
    }

    fn get(&self, _interp: &mut Interp, name: &str) -> Option<SV> {
        if let Some(v) = self.overrides.get(name) {
            return Some(v.clone());
        }
        match name {
            "style" | "attributes" | "state" => {
                Some(permissive_object("element-part", &self.log, Vec::new()))
            }
            "parent" => Some(permissive_object("element", &self.log, Vec::new())),
            _ => None,
        }
    }

    fn set(&self, _interp: &mut Interp, _name: &str, _value: SV) -> bool {
        false
    }

    fn call_method(&self, interp: &mut Interp, name: &str, argv: &[SV]) -> Option<SResult<SV>> {
        self.log
            .calls
            .borrow_mut()
            .push(format!("{}.{}", self.kind, name));
        if let Some(v) = self.overrides.get(name) {
            let v = v.clone();
            return Some(interp.call_value(&v, &SV::Undefined, argv));
        }
        match name {
            "content" => {
                let v = argv.first().cloned().unwrap_or(SV::Undefined);
                Some(mount_vnode(interp, &v, &self.log).map(|_| SV::Undefined))
            }
            "$" | "select" => Some(Ok(make_element_mock(&self.log, &format!("{} arg", name)))),
            "$$" | "selectAll" => Some(Ok(sv_array(Vec::new()))),
            _ => {
                if name.starts_with("get_") {
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

fn permissive_object(kind: &'static str, log: &Rc<CallLog>, overrides: Vec<(String, SV)>) -> SV {
    sv_object(ObjectData {
        class: RefCell::new(None),
        props: RefCell::new(Vec::new()),
        native: Some(Gc::new(PermissiveNative {
            kind,
            log: log.clone(),
            overrides: overrides.into_iter().collect(),
        })),
    })
}

pub fn make_element_mock(log: &Rc<CallLog>, _hint: &str) -> SV {
    permissive_object("element", log, Vec::new())
}

fn set_ref(interp: &mut Interp, attrs: &SV, instance: SV) -> SResult<()> {
    if let SV::Object(ao) = attrs {
        let binding = ao
            .props
            .borrow()
            .iter()
            .find(|(k, _)| k == "@ref")
            .map(|(_, v)| v.clone());
        if let Some(SV::Object(b)) = binding {
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
                .map(|(_, v)| super::interp::to_display(v));
            if let (Some(target), Some(prop)) = (target, prop) {
                interp.member_set(&target, &prop, instance)?;
            }
        }
    }
    Ok(())
}

pub fn mount_vnode(interp: &mut Interp, v: &SV, log: &Rc<CallLog>) -> SResult<()> {
    match v {
        SV::Array(a) => {
            let items = a.borrow().clone();
            let is_vnode = items.len() == 3
                && matches!(items[0], SV::Str(_) | SV::Class(_))
                && matches!(items[1], SV::Object(_))
                && matches!(items[2], SV::Array(_));
            if is_vnode {
                let tag = &items[0];
                let attrs = &items[1];
                let children = &items[2];
                match tag {
                    SV::Class(_) => {
                        let inst = interp.construct(tag, &[attrs.clone()])?;
                        set_ref(interp, attrs, inst.clone())?;
                        let rendered = interp.call_method(&inst, "render", &[])?;
                        mount_vnode(interp, &rendered, log)?;
                    }
                    _ => {
                        set_ref(interp, attrs, make_element_mock(log, "mounted"))?;
                        if let SV::Array(ch) = children {
                            let ch = ch.borrow().clone();
                            for c in ch {
                                mount_vnode(interp, &c, log)?;
                            }
                        }
                    }
                }
                return Ok(());
            }
            for item in items {
                mount_vnode(interp, &item, log)?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

pub fn install_test_host(interp: &mut Interp) -> Rc<CallLog> {
    let log = CallLog::new();

    let view = permissive_object(
        "view",
        &log,
        vec![
            (
                "get_default_pi".into(),
                native_fn("get_default_pi", |_i, _t, _a| {
                    Ok(new_object(vec![
                        ("hostname".into(), SV::Str("mockhost".into())),
                        ("username".into(), SV::Str("mockuser".into())),
                        ("platform".into(), SV::Str("OSX".into())),
                        ("displays".into(), sv_array(Vec::new())),
                        ("current_display".into(), SV::Int(0)),
                        ("sas_enabled".into(), SV::Bool(false)),
                    ]))
                }),
            ),
            (
                "get_connect_status".into(),
                native_fn("get_connect_status", |_i, _t, _a| {
                    Ok(sv_array(vec![
                        SV::Int(1),
                        SV::Bool(true),
                        SV::Str("123456789".into()),
                    ]))
                }),
            ),
            (
                "get_option".into(),
                native_fn("get_option", |_i, _t, _a| Ok(SV::Str("".into()))),
            ),
            (
                "get_local_option".into(),
                native_fn("get_local_option", |_i, _t, _a| Ok(SV::Str("".into()))),
            ),
            (
                "t".into(),
                native_fn("t", |_i, _t, argv| {
                    Ok(argv.first().cloned().unwrap_or(SV::Str("".into())))
                }),
            ),
            (
                "get_icon".into(),
                native_fn("get_icon", |_i, _t, _a| {
                    Ok(SV::Str("<svg viewBox=\"0 0 1 1\"/>".into()))
                }),
            ),
            (
                "get_id".into(),
                native_fn("get_id", |_i, _t, _a| Ok(SV::Str("123456789".into()))),
            ),
            (
                "get_version".into(),
                native_fn("get_version", |_i, _t, _a| Ok(SV::Str("1.46.5".into()))),
            ),
            (
                "get_app_name".into(),
                native_fn("get_app_name", |_i, _t, _a| Ok(SV::Str("HopToDesk".into()))),
            ),
            (
                "get_recent_sessions".into(),
                native_fn("get_recent_sessions", |_i, _t, _a| {
                    Ok(sv_array(Vec::new()))
                }),
            ),
            (
                "get_fav".into(),
                native_fn("get_fav", |_i, _t, _a| {
                    Ok(sv_array(Vec::new()))
                }),
            ),
            (
                "temporary_password".into(),
                native_fn("temporary_password", |_i, _t, _a| Ok(SV::Str("mockpass".into()))),
            ),
            (
                "mediaVar".into(),
                native_fn("mediaVar", |_i, _t, argv| {
                    let name = match argv.first() {
                        Some(SV::Str(s)) => s.to_string(),
                        _ => String::new(),
                    };
                    Ok(match name.as_str() {
                        "platform" => SV::Str("OSX".into()),
                        _ => SV::Undefined,
                    })
                }),
            ),
            (
                "screenBox".into(),
                native_fn("screenBox", |_i, _t, _a| {
                    Ok(sv_array(vec![
                        SV::Int(0),
                        SV::Int(0),
                        SV::Int(1920),
                        SV::Int(1080),
                    ]))
                }),
            ),
            (
                "box".into(),
                native_fn("box", |_i, _t, _a| {
                    Ok(sv_array(vec![
                        SV::Int(0),
                        SV::Int(0),
                        SV::Int(800),
                        SV::Int(600),
                    ]))
                }),
            ),
        ],
    );

    let self_log = log.clone();
    let self_obj = permissive_object(
        "self",
        &log,
        vec![
            (
                "toPixels".into(),
                native_fn("toPixels", |_i, _t, argv| {
                    Ok(match argv.first() {
                        Some(SV::Unit(x, _)) => SV::Int(*x as i64),
                        Some(SV::Int(i)) => SV::Int(*i),
                        Some(SV::Float(x)) => SV::Int(*x as i64),
                        _ => SV::Int(0),
                    })
                }),
            ),
            (
                "url".into(),
                native_fn("url", |_i, _t, argv| {
                    Ok(argv.first().cloned().unwrap_or(SV::Str("".into())))
                }),
            ),
            (
                "$".into(),
                native_fn("$", move |_i, _t, argv| {
                    let sel = match argv.first() {
                        Some(SV::Str(s)) => s.to_string(),
                        _ => String::new(),
                    };
                    if sel.trim() == "#handler" {
                        Ok(SV::Null)
                    } else {
                        Ok(make_element_mock(&self_log, &sel))
                    }
                }),
            ),
            (
                "timer".into(),
                native_fn("timer", |_i, _t, _a| Ok(SV::Bool(true))),
            ),
        ],
    );

    let global_log = log.clone();
    interp.global.define(
        "$",
        native_fn("$", move |_i, _t, argv| {
            let sel = match argv.first() {
                Some(SV::Str(s)) => s.to_string(),
                _ => String::new(),
            };
            let sel = sel.trim();
            if sel == "#handler" {
                Ok(SV::Null)
            } else {
                Ok(make_element_mock(&global_log, sel))
            }
        }),
    );
    interp.global.define(
        "$$",
        native_fn("$$", |_i, _t, _argv| {
            Ok(sv_array(Vec::new()))
        }),
    );

    interp.global.define("view", view.clone());
    interp.global.define("self", self_obj.clone());
    interp.view_object = view;
    interp.self_object = self_obj;

    let hook_log = log.clone();
    interp.include_hook = Some(std::rc::Rc::new(move |interp: &mut Interp, spec: &str| {
        if spec == "wireui:reactor.tis" {
            install_reactor(interp, &hook_log);
            return Some(Ok(()));
        }
        if let Some(rest) = spec.strip_prefix("wireui:") {
            let _ = rest;
            return Some(Ok(()));
        }
        None
    }));

    super::runtime::install_globals(interp);
    log
}

fn install_reactor(interp: &mut Interp, _log: &Rc<CallLog>) {
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
        m.insert(
            "update".into(),
            native_fn("update", |_i, this, _a| Ok(this.clone())),
        );
        m.insert(
            "select".into(),
            native_fn("select", |_i, _t, _a| Ok(SV::Null)),
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
            "content".into(),
            native_fn("content", |_i, _t, _a| Ok(SV::Undefined)),
        );
        m.insert(
            "timer".into(),
            native_fn("timer", |_i, _t, _a| Ok(SV::Bool(true))),
        );
        m.insert(
            "post".into(),
            native_fn("post", |_i, _t, _a| Ok(SV::Undefined)),
        );
    }
    let reactor = new_object(vec![(
        "Component".into(),
        SV::Class(Gc::new(component)),
    )]);
    interp.global.define("Reactor", reactor);

    let behavior = ClassVal {
        name: "Behavior".into(),
        base: RefCell::new(None),
        methods: RefCell::new(HashMap::new()),
        class_props: RefCell::new(Vec::new()),
        events: RefCell::new(Vec::new()),
        class_env: RefCell::new(None),
    };
    interp
        .global
        .define("Behavior", SV::Class(Gc::new(behavior)));

    let element = ClassVal {
        name: "Element".into(),
        base: RefCell::new(None),
        methods: RefCell::new(HashMap::new()),
        class_props: RefCell::new(Vec::new()),
        events: RefCell::new(Vec::new()),
        class_env: RefCell::new(None),
    };
    interp
        .global
        .define("Element", SV::Class(Gc::new(element)));
    let _ = Env::new(None);
}
