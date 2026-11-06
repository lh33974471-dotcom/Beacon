use crate::dom::event::EventHandler;
use crate::script::interp::{sv_array, sv_object, Gc, Interp, NativeObj, ObjectData, SResult, SV};
use crate::value::Value;
use std::cell::RefCell;
use std::rc::Rc;

pub fn sv_to_value(v: &SV) -> Value {
    match v {
        SV::Undefined => Value::new(),
        SV::Null => Value::null(),
        SV::Bool(b) => Value::from(*b),
        SV::Int(i) => Value::from(*i as i32),
        SV::Float(f) => Value::from(*f),
        SV::Unit(x, _) => Value::from(*x),
        SV::Str(s) => Value::from(s.as_ref()),
        SV::Symbol(s) => Value::symbol(s),
        SV::Regex(r) => Value::from(r.source.as_str()),
        SV::Array(a) => {
            let mut out = Value::array(0);
            for item in a.borrow().iter() {
                out.push(sv_to_value(item));
            }
            out
        }
        SV::Object(o) => {
            if o.native.is_some() {
                Value::null()
            } else {
                let mut out = Value::map();
                for (k, val) in o.props.borrow().iter() {
                    out.set_item(k.as_str(), sv_to_value(val));
                }
                out
            }
        }
        SV::Class(_) | SV::Function(_) | SV::NativeFn(_) => Value::null(),
    }
}

pub fn value_to_sv(v: &Value) -> SV {
    if v.is_undefined() {
        SV::Undefined
    } else if v.is_null() {
        SV::Null
    } else if let Some(b) = v.to_bool() {
        SV::Bool(b)
    } else if let Some(i) = v.to_int() {
        SV::Int(i as i64)
    } else if v.is_float() {
        SV::Float(v.to_float().unwrap_or(0.0))
    } else if v.is_array() {
        let mut items = Vec::with_capacity(v.len());
        for i in 0..v.len() {
            items.push(value_to_sv(&v.get(i)));
        }
        sv_array(items)
    } else if v.is_map() {
        let mut props = Vec::new();
        for (k, val) in v.items() {
            let key = k.as_string().unwrap_or_else(|| k.to_string());
            props.push((key, value_to_sv(&val)));
        }
        sv_object(ObjectData {
            class: RefCell::new(None),
            props: RefCell::new(props),
            native: None,
        })
    } else if let Some(s) = v.as_string() {
        SV::Str(s.as_str().into())
    } else if let Some(b) = v.as_bytes() {
        crate::engine::host::bytes_sv(b.to_vec())
    } else {
        SV::Undefined
    }
}

pub type SharedHandler = Rc<RefCell<Box<dyn EventHandler>>>;

pub struct HandlerBridge {
    handler: SharedHandler,
    root: crate::capi::scdom::HELEMENT,
}

impl HandlerBridge {
    pub fn new(handler: SharedHandler) -> HandlerBridge {
        HandlerBridge {
            handler,
            root: std::ptr::null_mut(),
        }
    }

    pub fn into_object(self) -> SV {
        sv_object(ObjectData {
            class: RefCell::new(None),
            props: RefCell::new(Vec::new()),
            native: Some(Gc::new(self)),
        })
    }
}

impl NativeObj for HandlerBridge {
    fn type_name(&self) -> &'static str {
        "handler"
    }

    fn call_method(&self, _interp: &mut Interp, name: &str, argv: &[SV]) -> Option<SResult<SV>> {
        let values: Vec<Value> = argv.iter().map(sv_to_value).collect();
        let result = {
            let mut h = self.handler.borrow_mut();
            h.on_script_call(self.root, name, &values)
        };
        match result {
            Some(v) => Some(Ok(value_to_sv(&v))),
            None => {
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
