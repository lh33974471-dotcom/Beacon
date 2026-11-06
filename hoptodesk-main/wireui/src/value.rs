use std::fmt;
use std::sync::Arc;

#[derive(Debug, Clone, PartialEq)]
#[allow(dead_code)]
pub(crate) enum VData {
    Undefined,
    Null,
    Bool(bool),
    Int(i32),
    Float(f64),
    String(String),
    Symbol(String),
    Error(String),
    Bytes(Vec<u8>),
    Array(Vec<Value>),
    Map(Vec<(Value, Value)>),
    FunctionRef { heap_id: u64, window_id: u32 },
}

#[derive(Clone)]
pub struct Value {
    pub(crate) data: Arc<VData>,
}

impl Value {
    pub fn new() -> Value {
        Value {
            data: Arc::new(VData::Undefined),
        }
    }

    pub fn null() -> Value {
        Value {
            data: Arc::new(VData::Null),
        }
    }

    pub fn symbol(val: &str) -> Value {
        Value {
            data: Arc::new(VData::Symbol(val.to_owned())),
        }
    }

    pub fn error(val: &str) -> Value {
        Value {
            data: Arc::new(VData::Error(val.to_owned())),
        }
    }

    pub fn map() -> Value {
        Value {
            data: Arc::new(VData::Map(Vec::new())),
        }
    }

    pub fn array(length: usize) -> Value {
        Value {
            data: Arc::new(VData::Array(vec![Value::new(); length])),
        }
    }

    pub(crate) fn from_data(data: VData) -> Value {
        Value {
            data: Arc::new(data),
        }
    }

    pub fn is_undefined(&self) -> bool {
        matches!(*self.data, VData::Undefined)
    }

    pub fn is_null(&self) -> bool {
        matches!(*self.data, VData::Null)
    }

    pub fn is_bool(&self) -> bool {
        matches!(*self.data, VData::Bool(_))
    }

    pub fn is_int(&self) -> bool {
        matches!(*self.data, VData::Int(_))
    }

    pub fn is_float(&self) -> bool {
        matches!(*self.data, VData::Float(_))
    }

    pub fn is_string(&self) -> bool {
        matches!(*self.data, VData::String(_))
    }

    pub fn is_symbol(&self) -> bool {
        matches!(*self.data, VData::Symbol(_))
    }

    pub fn is_error_string(&self) -> bool {
        matches!(*self.data, VData::Error(_))
    }

    pub fn is_bytes(&self) -> bool {
        matches!(*self.data, VData::Bytes(_))
    }

    pub fn is_array(&self) -> bool {
        matches!(*self.data, VData::Array(_))
    }

    pub fn is_map(&self) -> bool {
        matches!(*self.data, VData::Map(_))
    }

    pub fn is_function(&self) -> bool {
        matches!(*self.data, VData::FunctionRef { .. })
    }

    pub fn is_empty(&self) -> bool {
        match &*self.data {
            VData::Undefined | VData::Null => true,
            VData::String(s) | VData::Symbol(s) | VData::Error(s) => s.is_empty(),
            VData::Bytes(b) => b.is_empty(),
            VData::Array(a) => a.is_empty(),
            VData::Map(m) => m.is_empty(),
            _ => false,
        }
    }

    pub fn len(&self) -> usize {
        match &*self.data {
            VData::Array(a) => a.len(),
            VData::Map(m) => m.len(),
            VData::Bytes(b) => b.len(),
            VData::String(s) | VData::Symbol(s) | VData::Error(s) => s.chars().count(),
            _ => 0,
        }
    }

    pub fn to_bool(&self) -> Option<bool> {
        match &*self.data {
            VData::Bool(b) => Some(*b),
            _ => None,
        }
    }

    pub fn to_int(&self) -> Option<i32> {
        match &*self.data {
            VData::Int(i) => Some(*i),
            _ => None,
        }
    }

    pub fn to_float(&self) -> Option<f64> {
        match &*self.data {
            VData::Float(f) => Some(*f),
            VData::Int(i) => Some(*i as f64),
            _ => None,
        }
    }

    pub fn as_string(&self) -> Option<String> {
        match &*self.data {
            VData::String(s) | VData::Symbol(s) | VData::Error(s) => Some(s.clone()),
            _ => None,
        }
    }

    pub fn as_bytes(&self) -> Option<&[u8]> {
        match &*self.data {
            VData::Bytes(b) => Some(b.as_slice()),
            _ => None,
        }
    }

    pub fn into_string(self) -> String {
        match &*self.data {
            VData::String(s) | VData::Symbol(s) | VData::Error(s) => s.clone(),
            other => format!("{}", Value { data: Arc::new(other.clone()) }),
        }
    }

    pub fn get(&self, index: usize) -> Value {
        match &*self.data {
            VData::Array(a) => a.get(index).cloned().unwrap_or_else(Value::new),
            VData::Map(m) => m.get(index).map(|kv| kv.1.clone()).unwrap_or_else(Value::new),
            _ => Value::new(),
        }
    }

    pub fn get_item<T: Into<Value>>(&self, key: T) -> Value {
        let key = key.into();
        match &*self.data {
            VData::Map(m) => m
                .iter()
                .find(|(k, _)| *k == key)
                .map(|(_, v)| v.clone())
                .unwrap_or_else(Value::new),
            _ => Value::new(),
        }
    }

    pub fn key_at(&self, index: usize) -> Value {
        match &*self.data {
            VData::Map(m) => m.get(index).map(|kv| kv.0.clone()).unwrap_or_else(Value::new),
            _ => Value::new(),
        }
    }

    pub fn set(&mut self, index: usize, value: impl Into<Value>) {
        let value = value.into();
        if let VData::Array(a) = Arc::make_mut(&mut self.data) {
            if index >= a.len() {
                a.resize(index + 1, Value::new());
            }
            a[index] = value;
        }
    }

    pub fn set_item<TKey: Into<Value>, TValue: Into<Value>>(&mut self, key: TKey, value: TValue) {
        let key = key.into();
        let value = value.into();
        match Arc::make_mut(&mut self.data) {
            VData::Map(m) => {
                if let Some(slot) = m.iter_mut().find(|(k, _)| *k == key) {
                    slot.1 = value;
                } else {
                    m.push((key, value));
                }
            }
            other => {
                *other = VData::Map(vec![(key, value)]);
            }
        }
    }

    pub fn push<T: Into<Value>>(&mut self, src: T) {
        let value = src.into();
        match Arc::make_mut(&mut self.data) {
            VData::Array(a) => a.push(value),
            other => {
                *other = VData::Array(vec![value]);
            }
        }
    }

    pub fn items(&self) -> Vec<(Value, Value)> {
        match &*self.data {
            VData::Map(m) => m.clone(),
            _ => Vec::new(),
        }
    }

    pub fn keys(&self) -> std::vec::IntoIter<Value> {
        match &*self.data {
            VData::Map(m) => m.iter().map(|(k, _)| k.clone()).collect::<Vec<_>>().into_iter(),
            _ => Vec::new().into_iter(),
        }
    }

    pub fn values(&self) -> std::vec::IntoIter<Value> {
        match &*self.data {
            VData::Map(m) => m.iter().map(|(_, v)| v.clone()).collect::<Vec<_>>().into_iter(),
            VData::Array(a) => a.clone().into_iter(),
            _ => Vec::new().into_iter(),
        }
    }

    pub fn isolate(&mut self) {}

    pub fn clear(&mut self) -> &mut Value {
        self.data = Arc::new(VData::Undefined);
        self
    }
}

impl Default for Value {
    fn default() -> Self {
        Value::new()
    }
}

impl PartialEq for Value {
    fn eq(&self, other: &Value) -> bool {
        self.data == other.data
    }
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match &*self.data {
            VData::Undefined => write!(f, "undefined"),
            VData::Null => write!(f, "null"),
            VData::Bool(b) => write!(f, "{}", b),
            VData::Int(i) => write!(f, "{}", i),
            VData::Float(v) => write!(f, "{}", v),
            VData::String(s) => write!(f, "{}", s),
            VData::Symbol(s) => write!(f, "#{}", s),
            VData::Error(s) => write!(f, "error: {}", s),
            VData::Bytes(b) => write!(f, "[{} bytes]", b.len()),
            VData::Array(a) => {
                write!(f, "[")?;
                for (i, v) in a.iter().enumerate() {
                    if i > 0 {
                        write!(f, ",")?;
                    }
                    write!(f, "{}", v)?;
                }
                write!(f, "]")
            }
            VData::Map(m) => {
                write!(f, "{{")?;
                for (i, (k, v)) in m.iter().enumerate() {
                    if i > 0 {
                        write!(f, ",")?;
                    }
                    write!(f, "{}:{}", k, v)?;
                }
                write!(f, "}}")
            }
            VData::FunctionRef { heap_id, .. } => write!(f, "function({})", heap_id),
        }
    }
}

impl fmt::Debug for Value {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match &*self.data {
            VData::Undefined => write!(f, "undefined"),
            VData::Null => write!(f, "null"),
            VData::Bool(_) => write!(f, "bool:{}", self),
            VData::Int(_) => write!(f, "int:{}", self),
            VData::Float(_) => write!(f, "float:{}", self),
            VData::String(_) => write!(f, "string:\"{}\"", self),
            VData::Symbol(_) => write!(f, "symbol:{}", self),
            VData::Error(_) => write!(f, "{}", self),
            VData::Bytes(_) => write!(f, "bytes:{}", self),
            VData::Array(_) => write!(f, "array:{}", self),
            VData::Map(_) => write!(f, "map:{}", self),
            VData::FunctionRef { .. } => write!(f, "{}", self),
        }
    }
}

impl From<()> for Value {
    fn from(_: ()) -> Value {
        Value::new()
    }
}

impl From<bool> for Value {
    fn from(val: bool) -> Value {
        Value::from_data(VData::Bool(val))
    }
}

impl From<&bool> for Value {
    fn from(val: &bool) -> Value {
        Value::from_data(VData::Bool(*val))
    }
}

impl From<i32> for Value {
    fn from(val: i32) -> Value {
        Value::from_data(VData::Int(val))
    }
}

impl From<&i32> for Value {
    fn from(val: &i32) -> Value {
        Value::from_data(VData::Int(*val))
    }
}

impl From<f64> for Value {
    fn from(val: f64) -> Value {
        Value::from_data(VData::Float(val))
    }
}

impl From<&f64> for Value {
    fn from(val: &f64) -> Value {
        Value::from_data(VData::Float(*val))
    }
}

impl From<&str> for Value {
    fn from(val: &str) -> Value {
        Value::from_data(VData::String(val.to_owned()))
    }
}

impl From<String> for Value {
    fn from(val: String) -> Value {
        Value::from_data(VData::String(val))
    }
}

impl From<&String> for Value {
    fn from(val: &String) -> Value {
        Value::from_data(VData::String(val.clone()))
    }
}

impl From<&[u8]> for Value {
    fn from(val: &[u8]) -> Value {
        Value::from_data(VData::Bytes(val.to_vec()))
    }
}

impl From<Vec<u8>> for Value {
    fn from(val: Vec<u8>) -> Value {
        Value::from_data(VData::Bytes(val))
    }
}

impl From<std::time::SystemTime> for Value {
    fn from(val: std::time::SystemTime) -> Value {
        let secs = val
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as f64)
            .unwrap_or(0.0);
        Value::from_data(VData::Float(secs))
    }
}

impl<T: Into<Value>, E: Into<Value>> From<Result<T, E>> for Value {
    fn from(val: Result<T, E>) -> Value {
        match val {
            Ok(v) => v.into(),
            Err(e) => {
                let v: Value = e.into();
                Value::error(&v.into_string())
            }
        }
    }
}

impl<T: Into<Value>> FromIterator<T> for Value {
    fn from_iter<I: IntoIterator<Item = T>>(iter: I) -> Value {
        let items: Vec<Value> = iter.into_iter().map(|v| v.into()).collect();
        Value::from_data(VData::Array(items))
    }
}

pub struct SeqIterator {
    items: std::vec::IntoIter<Value>,
}

impl Iterator for SeqIterator {
    type Item = Value;
    fn next(&mut self) -> Option<Value> {
        self.items.next()
    }
}

impl IntoIterator for &Value {
    type Item = Value;
    type IntoIter = SeqIterator;
    fn into_iter(self) -> SeqIterator {
        SeqIterator {
            items: self.values(),
        }
    }
}

pub trait FromValue {
    fn from_value(v: &Value) -> Option<Self>
    where
        Self: Sized;
}

impl FromValue for Value {
    fn from_value(v: &Value) -> Option<Value> {
        Some(v.clone())
    }
}

impl FromValue for bool {
    fn from_value(v: &Value) -> Option<bool> {
        v.to_bool()
    }
}

impl FromValue for i32 {
    fn from_value(v: &Value) -> Option<i32> {
        v.to_int()
    }
}

impl FromValue for f64 {
    fn from_value(v: &Value) -> Option<f64> {
        v.to_float()
    }
}

impl FromValue for String {
    fn from_value(v: &Value) -> Option<String> {
        v.as_string()
    }
}

impl FromValue for Vec<u8> {
    fn from_value(v: &Value) -> Option<Vec<u8>> {
        v.as_bytes().map(|b| b.to_vec())
    }
}

impl FromValue for Vec<Value> {
    fn from_value(v: &Value) -> Option<Vec<Value>> {
        match &*v.data {
            VData::Array(a) => Some(a.clone()),
            _ => None,
        }
    }
}
