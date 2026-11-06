use super::ast::*;
use std::cell::RefCell;
use std::collections::HashMap;
use std::fmt;
use std::rc::Rc;

pub type Gc<T> = Rc<T>;

#[derive(Clone)]
pub enum SV {
    Undefined,
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Unit(f64, Gc<str>),
    Str(Gc<str>),
    Symbol(Gc<str>),
    Regex(Gc<RegexVal>),
    Array(Gc<RefCell<Vec<SV>>>),
    Object(Gc<ObjectData>),
    Class(Gc<ClassVal>),
    Function(Gc<FuncVal>),
    NativeFn(Gc<NativeFnVal>),
}

pub struct RegexVal {
    pub source: String,
    pub flags: String,
}

pub struct ObjectData {
    pub class: RefCell<Option<Gc<ClassVal>>>,
    pub props: RefCell<Vec<(String, SV)>>,
    pub native: Option<Gc<dyn NativeObj>>,
}

pub struct ClassVal {
    pub name: String,
    pub base: RefCell<Option<Gc<ClassVal>>>,
    pub methods: RefCell<HashMap<String, SV>>,
    pub class_props: RefCell<Vec<(String, SV)>>,
    pub events: RefCell<Vec<EventDecl>>,
    pub class_env: RefCell<Option<EnvRef>>,
}

pub struct FuncVal {
    pub name: String,
    pub params: Vec<Param>,
    pub body: Gc<Vec<Stmt>>,
    pub env: EnvRef,
    pub this_capture: Option<Box<SV>>,
}

pub struct NativeFnVal {
    pub name: String,
    #[allow(clippy::type_complexity)]
    pub f: Box<dyn Fn(&mut Interp, &SV, &[SV]) -> SResult<SV>>,
}

pub trait NativeObj {
    fn type_name(&self) -> &'static str;
    fn get(&self, _interp: &mut Interp, _name: &str) -> Option<SV> {
        None
    }
    fn set(&self, _interp: &mut Interp, _name: &str, _value: SV) -> bool {
        false
    }
    fn call_method(&self, _interp: &mut Interp, _name: &str, _args: &[SV]) -> Option<SResult<SV>> {
        None
    }
    fn as_bytes(&self) -> Option<&[u8]> {
        None
    }
    fn as_image(&self) -> Option<(u32, u32, std::sync::Arc<Vec<u8>>)> {
        None
    }
}

pub type EnvRef = Gc<Env>;

pub struct Env {
    pub vars: RefCell<HashMap<String, SV>>,
    pub parent: Option<EnvRef>,
}

impl Env {
    pub fn new(parent: Option<EnvRef>) -> EnvRef {
        let e = Gc::new(Env {
            vars: RefCell::new(HashMap::new()),
            parent,
        });
        super::gc::track_env(&e);
        e
    }

    pub fn define(&self, name: &str, value: SV) {
        self.vars.borrow_mut().insert(name.to_string(), value);
    }

    pub fn lookup(&self, name: &str) -> Option<SV> {
        if let Some(v) = self.vars.borrow().get(name) {
            return Some(v.clone());
        }
        match &self.parent {
            Some(p) => p.lookup(name),
            None => None,
        }
    }

    pub fn assign(&self, name: &str, value: SV) -> bool {
        if self.vars.borrow().contains_key(name) {
            self.vars.borrow_mut().insert(name.to_string(), value);
            return true;
        }
        match &self.parent {
            Some(p) => p.assign(name, value),
            None => false,
        }
    }
}

pub fn sv_object(o: ObjectData) -> SV {
    let rc = Gc::new(o);
    super::gc::track_object(&rc);
    SV::Object(rc)
}

pub fn sv_array(items: Vec<SV>) -> SV {
    let rc = Gc::new(RefCell::new(items));
    super::gc::track_array(&rc);
    SV::Array(rc)
}

pub fn sv_func(f: FuncVal) -> SV {
    let rc = Gc::new(f);
    super::gc::track_func(&rc);
    SV::Function(rc)
}

#[derive(Debug)]
pub struct Thrown(pub String);

pub type SResult<T> = std::result::Result<T, Thrown>;

pub enum Completion {
    Normal,
    Return(SV),
    Break,
    Continue,
}

impl fmt::Debug for SV {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}", to_display(self))
    }
}

pub fn to_display(v: &SV) -> String {
    match v {
        SV::Undefined => "undefined".into(),
        SV::Null => "null".into(),
        SV::Bool(b) => b.to_string(),
        SV::Int(i) => i.to_string(),
        SV::Float(x) => {
            if x.fract() == 0.0 && x.abs() < 1e15 {
                format!("{:.1}", x)
            } else {
                x.to_string()
            }
        }
        SV::Unit(x, u) => format!("{}{}", x, u),
        SV::Str(s) => s.to_string(),
        SV::Symbol(s) => format!("#{}", s),
        SV::Regex(r) => format!("/{}/{}", r.source, r.flags),
        SV::Array(a) => {
            let items: Vec<String> = a.borrow().iter().map(to_display).collect();
            format!("[{}]", items.join(","))
        }
        SV::Object(o) => {
            if let Some(n) = &o.native {
                format!("[native {}]", n.type_name())
            } else {
                let props = o.props.borrow();
                let items: Vec<String> = props
                    .iter()
                    .map(|(k, v)| format!("{}:{}", k, to_display(v)))
                    .collect();
                format!("{{{}}}", items.join(","))
            }
        }
        SV::Class(c) => format!("[class {}]", c.name),
        SV::Function(f) => format!("[function {}]", f.name),
        SV::NativeFn(f) => format!("[native function {}]", f.name),
    }
}

pub fn truthy(v: &SV) -> bool {
    match v {
        SV::Undefined | SV::Null => false,
        SV::Bool(b) => *b,
        SV::Int(i) => *i != 0,
        SV::Float(x) => *x != 0.0,
        SV::Unit(x, _) => *x != 0.0,
        SV::Str(s) => !s.is_empty(),
        _ => true,
    }
}

pub fn type_symbol(v: &SV) -> &'static str {
    match v {
        SV::Undefined => "undefined",
        SV::Null => "object",
        SV::Bool(_) => "boolean",
        SV::Int(_) => "integer",
        SV::Float(_) | SV::Unit(..) => "float",
        SV::Str(_) => "string",
        SV::Symbol(_) => "symbol",
        SV::Regex(_) => "regexp",
        SV::Array(_) => "array",
        SV::Object(o) => {
            if o.class.borrow().is_some() {
                "object"
            } else if o.native.is_some() {
                "object"
            } else {
                "map"
            }
        }
        SV::Class(_) => "class",
        SV::Function(_) | SV::NativeFn(_) => "function",
    }
}

fn num2(v: &SV) -> Option<f64> {
    match v {
        SV::Int(i) => Some(*i as f64),
        SV::Float(x) => Some(*x),
        SV::Unit(x, _) => Some(*x),
        SV::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
        _ => None,
    }
}

fn num_coerce(v: &SV) -> Option<f64> {
    match v {
        SV::Str(s) => {
            let t = s.trim();
            if t.is_empty() {
                None
            } else {
                t.parse::<f64>().ok()
            }
        }
        _ => num2(v),
    }
}

pub fn loose_eq(a: &SV, b: &SV) -> bool {
    match (a, b) {
        (SV::Undefined | SV::Null, SV::Undefined | SV::Null) => true,
        (SV::Str(x), SV::Str(y)) => x == y,
        (SV::Symbol(x), SV::Symbol(y)) => x == y,
        (SV::Str(x), SV::Symbol(y)) | (SV::Symbol(x), SV::Str(y)) => x == y,
        (SV::Bool(x), SV::Bool(y)) => x == y,
        (SV::Array(x), SV::Array(y)) => Gc::ptr_eq(x, y),
        (SV::Object(x), SV::Object(y)) => Gc::ptr_eq(x, y),
        (SV::Class(x), SV::Class(y)) => Gc::ptr_eq(x, y),
        (SV::Function(x), SV::Function(y)) => Gc::ptr_eq(x, y),
        (SV::NativeFn(x), SV::NativeFn(y)) => Gc::ptr_eq(x, y),
        _ => match (num_coerce(a), num_coerce(b)) {
            (Some(x), Some(y)) => x == y,
            _ => false,
        },
    }
}

fn strict_eq(a: &SV, b: &SV) -> bool {
    match (a, b) {
        (SV::Undefined, SV::Undefined) => true,
        (SV::Null, SV::Null) => true,
        (SV::Int(x), SV::Int(y)) => x == y,
        (SV::Float(x), SV::Float(y)) => x == y,
        (SV::Int(_), SV::Float(_)) | (SV::Float(_), SV::Int(_)) => false,
        (SV::Undefined | SV::Null, _) | (_, SV::Undefined | SV::Null) => false,
        (SV::Str(_), SV::Bool(_) | SV::Int(_) | SV::Float(_) | SV::Unit(..))
        | (SV::Bool(_) | SV::Int(_) | SV::Float(_) | SV::Unit(..), SV::Str(_)) => false,
        _ => loose_eq(a, b),
    }
}

pub type IncludeResolver = Rc<dyn Fn(&mut Interp, &str) -> Option<SResult<()>>>;

pub struct PendingEvent {
    pub name: String,
    pub selector: Option<String>,
    pub func: SV,
    pub this: SV,
    pub target: SV,
}

pub struct Interp {
    pub global: EnvRef,
    pub base_dir: std::path::PathBuf,
    pub include_hook: Option<IncludeResolver>,
    pub self_object: SV,
    pub view_object: SV,
    pub output: Vec<String>,
    pub warnings: Vec<String>,
    pub lenient_idents: bool,
    pub depth: usize,
    pub pending_events: Vec<PendingEvent>,
}

impl Interp {
    pub fn new() -> Interp {
        Interp {
            global: Env::new(None),
            base_dir: std::path::PathBuf::from("."),
            include_hook: None,
            self_object: SV::Undefined,
            view_object: SV::Undefined,
            output: Vec::new(),
            warnings: Vec::new(),
            lenient_idents: true,
            depth: 0,
            pending_events: Vec::new(),
        }
    }

    pub fn throw<T>(&self, msg: impl Into<String>) -> SResult<T> {
        Err(Thrown(msg.into()))
    }

    pub fn gc(&mut self, extra_roots: &[SV]) -> super::gc::GcStats {
        assert_eq!(self.depth, 0, "gc must only run at idle, not mid-eval");
        let mut roots: Vec<SV> = vec![self.self_object.clone(), self.view_object.clone()];
        roots.extend_from_slice(extra_roots);
        for pe in &self.pending_events {
            roots.push(pe.func.clone());
            roots.push(pe.this.clone());
            roots.push(pe.target.clone());
        }
        super::gc::collect(&roots, &[self.global.clone()])
    }

    pub fn run_source(&mut self, source: &str) -> SResult<()> {
        let program = super::parser::parse(source).map_err(|e| Thrown(format!("parse: {}", e)))?;
        let env = self.global.clone();
        self.hoist_functions(&program, &env)?;
        for (i, stmt) in program.iter().enumerate() {
            match self
                .exec_stmt(stmt, &env)
                .map_err(|e| Thrown(format!("{} [top-level statement #{}]", e.0, i + 1)))?
            {
                Completion::Normal => {}
                _ => return self.throw("illegal top-level control flow"),
            }
        }
        Ok(())
    }

    pub fn run_file(&mut self, path: &std::path::Path) -> SResult<()> {
        let source = std::fs::read_to_string(path)
            .map_err(|e| Thrown(format!("read {}: {}", path.display(), e)))?;
        let old_base = self.base_dir.clone();
        if let Some(parent) = path.parent() {
            self.base_dir = parent.to_path_buf();
        }
        let r = self.run_source(&source);
        self.base_dir = old_base;
        r
    }

    fn do_include(&mut self, spec: &str) -> SResult<()> {
        if let Some(hook) = self.include_hook.clone() {
            if let Some(r) = hook(self, spec) {
                return r;
            }
        }
        let path = self.base_dir.join(spec);
        self.run_file(&path)
    }

    fn hoist_functions(&mut self, stmts: &[Stmt], env: &EnvRef) -> SResult<()> {
        for stmt in stmts {
            if let Stmt::Function(f) = stmt {
                if f.path.len() == 1 {
                    let fv = self.make_function(f, env);
                    env.define(&f.path[0], fv);
                }
            }
        }
        Ok(())
    }

    fn make_function(&self, f: &FunctionDecl, env: &EnvRef) -> SV {
        sv_func(FuncVal {
            name: f.path.join("."),
            params: f.params.clone(),
            body: Gc::new(f.body.clone()),
            env: env.clone(),
            this_capture: None,
        })
    }

    pub fn exec_block(&mut self, stmts: &[Stmt], env: &EnvRef) -> SResult<Completion> {
        self.hoist_functions(stmts, env)?;
        for stmt in stmts {
            match self.exec_stmt(stmt, env)? {
                Completion::Normal => {}
                other => return Ok(other),
            }
        }
        Ok(Completion::Normal)
    }

    pub fn exec_stmt(&mut self, stmt: &Stmt, env: &EnvRef) -> SResult<Completion> {
        match stmt {
            Stmt::Empty => Ok(Completion::Normal),
            Stmt::Include(spec) => {
                self.do_include(spec)?;
                Ok(Completion::Normal)
            }
            Stmt::VarDecl(decls) | Stmt::ConstDecl(decls) => {
                for (name, init) in decls {
                    let v = match init {
                        Some(e) => self.eval(e, env)?,
                        None => SV::Undefined,
                    };
                    env.define(name, v);
                }
                Ok(Completion::Normal)
            }
            Stmt::VarDestructure(names, init) => {
                let v = self.eval(init, env)?;
                for (i, name) in names.iter().enumerate() {
                    let item = self.index_get(&v, &SV::Int(i as i64))?;
                    env.define(name, item);
                }
                Ok(Completion::Normal)
            }
            Stmt::VarObjectDestructure(names, init) => {
                let v = self.eval(init, env)?;
                for name in names {
                    let item = self.member_get(&v, name)?;
                    env.define(name, item);
                }
                Ok(Completion::Normal)
            }
            Stmt::Function(f) => {
                if f.path.len() == 1 {
                    Ok(Completion::Normal)
                } else {
                    let target_path = &f.path[..f.path.len() - 1];
                    let mut target = self.eval(&Expr::Ident(target_path[0].clone()), env)?;
                    for part in &target_path[1..] {
                        target = self.member_get(&target, part)?;
                    }
                    let fv = self.make_function(f, env);
                    self.member_set(&target, &f.path[f.path.len() - 1], fv)?;
                    Ok(Completion::Normal)
                }
            }
            Stmt::Class(decl) => {
                let cv = self.instantiate_class(decl, env)?;
                env.define(&decl.name, cv);
                Ok(Completion::Normal)
            }
            Stmt::Event(decl) => {
                let target = self.self_object.clone();
                self.register_event(&target, decl, env)?;
                Ok(Completion::Normal)
            }
            Stmt::If(cond, then, alt) => {
                let c = self.eval(cond, env)?;
                if truthy(&c) {
                    self.exec_stmt(then, env)
                } else if let Some(alt) = alt {
                    self.exec_stmt(alt, env)
                } else {
                    Ok(Completion::Normal)
                }
            }
            Stmt::While(cond, body) => {
                loop {
                    let c = self.eval(cond, env)?;
                    if !truthy(&c) {
                        break;
                    }
                    match self.exec_stmt(body, env)? {
                        Completion::Break => break,
                        Completion::Continue | Completion::Normal => {}
                        r @ Completion::Return(_) => return Ok(r),
                    }
                }
                Ok(Completion::Normal)
            }
            Stmt::DoWhile(body, cond) => {
                loop {
                    match self.exec_stmt(body, env)? {
                        Completion::Break => break,
                        Completion::Continue | Completion::Normal => {}
                        r @ Completion::Return(_) => return Ok(r),
                    }
                    let c = self.eval(cond, env)?;
                    if !truthy(&c) {
                        break;
                    }
                }
                Ok(Completion::Normal)
            }
            Stmt::For(init, cond, update, body) => {
                let scope = Env::new(Some(env.clone()));
                if let Some(init) = init {
                    self.exec_stmt(init, &scope)?;
                }
                loop {
                    if let Some(cond) = cond {
                        let c = self.eval(cond, &scope)?;
                        if !truthy(&c) {
                            break;
                        }
                    }
                    match self.exec_stmt(body, &scope)? {
                        Completion::Break => break,
                        Completion::Continue | Completion::Normal => {}
                        r @ Completion::Return(_) => return Ok(r),
                    }
                    if let Some(update) = update {
                        self.eval(update, &scope)?;
                    }
                }
                Ok(Completion::Normal)
            }
            Stmt::ForIn(head, coll, body) => {
                let coll = self.eval(coll, env)?;
                let items = self.enumerate(&coll)?;
                for (key, value) in items {
                    let scope = Env::new(Some(env.clone()));
                    match head {
                        ForInHead::One(name) => scope.define(name, value.clone()),
                        ForInHead::Pair(k, v) => {
                            scope.define(k, key.clone());
                            scope.define(v, value.clone());
                        }
                        ForInHead::Triple(k, v, extra) => {
                            scope.define(k, key.clone());
                            scope.define(v, value.clone());
                            scope.define(extra, SV::Undefined);
                        }
                    }
                    match self.exec_stmt(body, &scope)? {
                        Completion::Break => break,
                        Completion::Continue | Completion::Normal => {}
                        r @ Completion::Return(_) => return Ok(r),
                    }
                }
                Ok(Completion::Normal)
            }
            Stmt::Switch(disc, cases) => {
                let d = self.eval(disc, env)?;
                let mut matched = false;
                for case in cases {
                    if !matched {
                        match &case.test {
                            Some(test) => {
                                let t = self.eval(test, env)?;
                                if loose_eq(&d, &t) {
                                    matched = true;
                                }
                            }
                            None => matched = true,
                        }
                    }
                    if matched {
                        for stmt in &case.body {
                            match self.exec_stmt(stmt, env)? {
                                Completion::Break => return Ok(Completion::Normal),
                                Completion::Normal => {}
                                other => return Ok(other),
                            }
                        }
                    }
                }
                Ok(Completion::Normal)
            }
            Stmt::Try(body, catch, finally) => {
                let scope = Env::new(Some(env.clone()));
                let mut result = self.exec_block(body, &scope);
                if let Err(Thrown(msg)) = &result {
                    if let Some((var, cbody)) = catch {
                        let cscope = Env::new(Some(env.clone()));
                        if !var.is_empty() {
                            cscope.define(var, SV::Str(msg.as_str().into()));
                        }
                        result = self.exec_block(cbody, &cscope);
                    } else {
                        result = Ok(Completion::Normal);
                    }
                }
                if let Some(fbody) = finally {
                    let fscope = Env::new(Some(env.clone()));
                    match self.exec_block(fbody, &fscope)? {
                        Completion::Normal => {}
                        other => return Ok(other),
                    }
                }
                result
            }
            Stmt::Throw(e) => {
                let v = self.eval(e, env)?;
                Err(Thrown(to_display(&v)))
            }
            Stmt::Assert(cond, msg) => {
                let c = self.eval(cond, env)?;
                if !truthy(&c) {
                    let m = match msg {
                        Some(m) => to_display(&self.eval(m, env)?),
                        None => "assertion failed".into(),
                    };
                    return Err(Thrown(m));
                }
                Ok(Completion::Normal)
            }
            Stmt::Return(e) => {
                let v = match e {
                    Some(e) => self.eval(e, env)?,
                    None => SV::Undefined,
                };
                Ok(Completion::Return(v))
            }
            Stmt::Break => Ok(Completion::Break),
            Stmt::Continue => Ok(Completion::Continue),
            Stmt::Block(body) => {
                let scope = Env::new(Some(env.clone()));
                self.exec_block(body, &scope)
            }
            Stmt::Expr(e) => {
                self.eval(e, env)?;
                Ok(Completion::Normal)
            }
        }
    }

    fn enumerate(&mut self, coll: &SV) -> SResult<Vec<(SV, SV)>> {
        match coll {
            SV::Array(a) => Ok(a
                .borrow()
                .iter()
                .enumerate()
                .map(|(i, v)| (SV::Int(i as i64), v.clone()))
                .collect()),
            SV::Object(o) => Ok(o
                .props
                .borrow()
                .iter()
                .map(|(k, v)| (SV::Str(k.as_str().into()), v.clone()))
                .collect()),
            SV::Str(s) => Ok(s
                .chars()
                .enumerate()
                .map(|(i, c)| (SV::Int(i as i64), SV::Str(c.to_string().into())))
                .collect()),
            SV::Undefined | SV::Null => Ok(Vec::new()),
            other => self.throw(format!("cannot iterate over {}", type_symbol(other))),
        }
    }

    fn instantiate_class(&mut self, decl: &ClassDecl, env: &EnvRef) -> SResult<SV> {
        let base = match &decl.base {
            Some(path) => {
                let mut v = match env.lookup(&path[0]) {
                    Some(v) => v,
                    None => return self.throw(format!("unknown base class {}", path.join("."))),
                };
                for part in &path[1..] {
                    v = self.member_get(&v, part)?;
                }
                match v {
                    SV::Class(c) => Some(c),
                    _ => return self.throw(format!("{} is not a class", path.join("."))),
                }
            }
            None => None,
        };
        let cv = Gc::new(ClassVal {
            name: decl.name.clone(),
            base: RefCell::new(base),
            methods: RefCell::new(HashMap::new()),
            class_props: RefCell::new(Vec::new()),
            events: RefCell::new(Vec::new()),
        class_env: RefCell::new(None),
        });
        let class_env = Env::new(Some(env.clone()));
        *cv.class_env.borrow_mut() = Some(class_env.clone());
        for member in &decl.members {
            match member {
                ClassMember::Method(f) => {
                    let fv = self.make_function(f, &class_env);
                    cv.methods.borrow_mut().insert(f.path.join("."), fv);
                }
                ClassMember::Var(decls) | ClassMember::Const(decls) => {
                    for (name, init) in decls {
                        let v = match init {
                            Some(e) => self.eval(e, env)?,
                            None => SV::Undefined,
                        };
                        cv.class_props.borrow_mut().push((name.clone(), v));
                    }
                }
                ClassMember::Event(decl) => {
                    cv.events.borrow_mut().push(decl.clone());
                }
                ClassMember::Class(inner) => {
                    let iv = self.instantiate_class(inner, env)?;
                    cv.class_props.borrow_mut().push((inner.name.clone(), iv));
                }
            }
        }
        Ok(SV::Class(cv))
    }

    pub fn event_to_function(&mut self, decl: &EventDecl, env: &EnvRef, this: Option<SV>) -> SResult<(String, Option<String>, SV)> {
        let selector = match &decl.selector {
            Some(parts) => {
                let mut out = String::new();
                for part in parts {
                    match part {
                        TextPart::Text(t) => out.push_str(t),
                        TextPart::Expr(e) => {
                            let v = self.eval(e, env)?;
                            out.push_str(&to_display(&v));
                        }
                    }
                }
                Some(out.trim().to_string())
            }
            None => None,
        };
        let params: Vec<Param> = decl
            .params
            .iter()
            .map(|p| Param {
                name: p.clone(),
                default: None,
                rest: false,
            })
            .collect();
        let func = sv_func(FuncVal {
            name: format!("event {}", decl.name),
            params,
            body: Gc::new(decl.body.clone()),
            env: env.clone(),
            this_capture: this.map(Box::new),
        });
        Ok((decl.name.clone(), selector, func))
    }

    fn register_event(&mut self, target: &SV, decl: &EventDecl, env: &EnvRef) -> SResult<()> {
        let (name, selector, func) = self.event_to_function(decl, env, None)?;
        let this = target.clone();
        self.pending_events.push(PendingEvent {
            name,
            selector,
            func,
            this: this.clone(),
            target: this,
        });
        Ok(())
    }

    pub fn eval(&mut self, e: &Expr, env: &EnvRef) -> SResult<SV> {
        self.depth += 1;
        if self.depth > 512 {
            self.depth -= 1;
            return self.throw("script recursion limit exceeded");
        }
        let r = self.eval_inner(e, env);
        self.depth -= 1;
        r
    }

    fn eval_inner(&mut self, e: &Expr, env: &EnvRef) -> SResult<SV> {
        match e {
            Expr::Undefined => Ok(SV::Undefined),
            Expr::Null => Ok(SV::Null),
            Expr::Bool(b) => Ok(SV::Bool(*b)),
            Expr::Int(v) => Ok(SV::Int(*v)),
            Expr::Float(v) => Ok(SV::Float(*v)),
            Expr::Unit(v, u) => Ok(SV::Unit(*v, u.as_str().into())),
            Expr::Str(s) => Ok(SV::Str(s.as_str().into())),
            Expr::Symbol(s) => Ok(SV::Symbol(s.as_str().into())),
            Expr::Regex(src, flags) => Ok(SV::Regex(Gc::new(RegexVal {
                source: src.clone(),
                flags: flags.clone(),
            }))),
            Expr::This => env
                .lookup("this")
                .map(Ok)
                .unwrap_or(Ok(SV::Undefined)),
            Expr::Super => self.throw("super is not supported"),
            Expr::Ident(name) => match env.lookup(name) {
                Some(v) => Ok(v),
                None => {
                    if self.lenient_idents {
                        self.warnings.push(format!("undefined variable '{}'", name));
                        Ok(SV::Undefined)
                    } else {
                        self.throw(format!("undefined variable '{}'", name))
                    }
                }
            },
            Expr::Array(items) => {
                let mut out = Vec::with_capacity(items.len());
                for item in items {
                    out.push(self.eval(item, env)?);
                }
                Ok(sv_array(out))
            }
            Expr::Map(entries) => {
                let obj = ObjectData {
                    class: RefCell::new(None),
                    props: RefCell::new(Vec::new()),
                    native: None,
                };
                for (key, value) in entries {
                    let k = match key {
                        MapKey::Ident(n) => n.clone(),
                        MapKey::Str(s) => s.clone(),
                        MapKey::Symbol(s) => s.clone(),
                        MapKey::Int(v) => v.to_string(),
                    };
                    let v = self.eval(value, env)?;
                    obj.props.borrow_mut().push((k, v));
                }
                Ok(sv_object(obj))
            }
            Expr::Function(params, body) => Ok(sv_func(FuncVal {
                name: "anonymous".into(),
                params: params.clone(),
                body: Gc::new(body.clone()),
                env: env.clone(),
                this_capture: None,
            })),
            Expr::Arrow(params, body) => {
                let this = env.lookup("this");
                let body_stmts = match &**body {
                    ArrowBody::Block(stmts) => stmts.clone(),
                    ArrowBody::Expr(e) => vec![Stmt::Return(Some(e.clone()))],
                };
                Ok(sv_func(FuncVal {
                    name: "lambda".into(),
                    params: params.clone(),
                    body: Gc::new(body_stmts),
                    env: env.clone(),
                    this_capture: this.map(Box::new),
                }))
            }
            Expr::Jsx(node) => self.eval_jsx(node, env),
            Expr::Stringizer(target, name, parts) => {
                let text = self.stringizer_text(parts, env)?;
                match target {
                    None => {
                        // A bare $()/$$() resolves against the window the code was
                        // WRITTEN in: the lexical `self` binding (each window env
                        // defines its own), not whichever window is current at call
                        // time. A parent-invoked chat-window closure (chatbox.refresh)
                        // must query the chat DOM, not the main window's.
                        let root = env
                            .lookup("self")
                            .filter(|v| !matches!(v, SV::Undefined | SV::Null))
                            .unwrap_or_else(|| self.self_object.clone());
                        self.call_native_stringizer(&root, name, &text)
                    }
                    Some(t) => {
                        let base = self.eval(t, env)?;
                        self.call_native_stringizer(&base, name, &text)
                    }
                }
            }
            Expr::Member(base, name) => {
                let b = self.eval(base, env)?;
                self.member_get(&b, name)
            }
            Expr::Index(base, idx) => {
                let b = self.eval(base, env)?;
                let i = self.eval(idx, env)?;
                self.index_get(&b, &i)
            }
            Expr::Call(callee, args) => self.eval_call(callee, args, env),
            Expr::New(callee, args) => {
                let c = self.eval(callee, env)?;
                let mut argv = Vec::with_capacity(args.len());
                for a in args {
                    argv.push(self.eval(a, env)?);
                }
                self.construct(&c, &argv)
            }
            Expr::Unary(op, e) => {
                if *op == "++" || *op == "--" {
                    let old = self.eval(e, env)?;
                    let n = num_coerce(&old).unwrap_or(0.0);
                    let delta = if *op == "++" { 1.0 } else { -1.0 };
                    let new = match old {
                        SV::Float(_) => SV::Float(n + delta),
                        _ => SV::Int((n + delta) as i64),
                    };
                    self.assign_to(e, new.clone(), env)?;
                    return Ok(new);
                }
                let v = self.eval(e, env)?;
                match *op {
                    "!" => Ok(SV::Bool(!truthy(&v))),
                    "-" => match v {
                        SV::Int(i) => Ok(SV::Int(-i)),
                        SV::Float(x) => Ok(SV::Float(-x)),
                        SV::Unit(x, u) => Ok(SV::Unit(-x, u)),
                        _ => self.throw("cannot negate value"),
                    },
                    "+" => Ok(v),
                    "~" => match v {
                        SV::Int(i) => Ok(SV::Int(!i)),
                        _ => self.throw("cannot bitwise-negate value"),
                    },
                    "typeof" => Ok(SV::Symbol(type_symbol(&v).into())),
                    "void" => Ok(SV::Undefined),
                    _ => self.throw(format!("unsupported unary operator {}", op)),
                }
            }
            Expr::Postfix(op, e) => {
                let old = self.eval(e, env)?;
                let n = num_coerce(&old).unwrap_or(0.0);
                let delta = if *op == "++" { 1.0 } else { -1.0 };
                let new = match old {
                    SV::Float(_) => SV::Float(n + delta),
                    _ => SV::Int((n + delta) as i64),
                };
                self.assign_to(e, new, env)?;
                Ok(old)
            }
            Expr::Delete(e) => {
                match &**e {
                    Expr::Member(base, name) => {
                        let b = self.eval(base, env)?;
                        if let SV::Object(o) = &b {
                            o.props.borrow_mut().retain(|(k, _)| k != name);
                        }
                    }
                    Expr::Index(base, idx) => {
                        let b = self.eval(base, env)?;
                        let i = self.eval(idx, env)?;
                        match &b {
                            SV::Object(o) => {
                                let key = to_display(&i);
                                o.props.borrow_mut().retain(|(k, _)| *k != key);
                            }
                            SV::Array(a) => {
                                if let SV::Int(n) = i {
                                    let mut a = a.borrow_mut();
                                    let n = n as usize;
                                    if n < a.len() {
                                        a.remove(n);
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                    _ => {}
                }
                Ok(SV::Bool(true))
            }
            Expr::Binary(op, a, b) => {
                match *op {
                    "&&" => {
                        let x = self.eval(a, env)?;
                        if !truthy(&x) {
                            return Ok(x);
                        }
                        return self.eval(b, env);
                    }
                    "||" => {
                        let x = self.eval(a, env)?;
                        if truthy(&x) {
                            return Ok(x);
                        }
                        return self.eval(b, env);
                    }
                    "??" => {
                        let x = self.eval(a, env)?;
                        if !matches!(x, SV::Undefined | SV::Null) {
                            return Ok(x);
                        }
                        return self.eval(b, env);
                    }
                    _ => {}
                }
                let x = self.eval(a, env)?;
                let y = self.eval(b, env)?;
                self.binary_op(op, &x, &y)
            }
            Expr::Assign(op, target, value) => {
                let v = if *op == "=" {
                    self.eval(value, env)?
                } else {
                    let cur = self.eval(target, env)?;
                    let rhs = self.eval(value, env)?;
                    let base_op = &op[..op.len() - 1];
                    self.binary_op(base_op, &cur, &rhs)?
                };
                self.assign_to(target, v.clone(), env)?;
                Ok(v)
            }
            Expr::Ternary(c, t, f) => {
                let cv = self.eval(c, env)?;
                if truthy(&cv) {
                    self.eval(t, env)
                } else {
                    self.eval(f, env)
                }
            }
            Expr::Comma(a, b) => {
                self.eval(a, env)?;
                self.eval(b, env)
            }
            Expr::EventAttach(target, decl) => {
                let t = self.eval(target, env)?;
                self.register_event(&t, decl, env)?;
                Ok(t)
            }
            Expr::Let(name, init) => {
                let v = self.eval(init, env)?;
                env.define(name, v.clone());
                Ok(v)
            }
        }
    }

    fn binary_op(&mut self, op: &str, x: &SV, y: &SV) -> SResult<SV> {
        match op {
            "+" => {
                if let (SV::Int(a), SV::Int(b)) = (x, y) {
                    return Ok(SV::Int(a.wrapping_add(*b)));
                }
                if matches!(x, SV::Str(_)) || matches!(y, SV::Str(_)) {
                    return Ok(SV::Str(format!("{}{}", to_display(x), to_display(y)).into()));
                }
                match (num2(x), num2(y)) {
                    (Some(a), Some(b)) => Ok(SV::Float(a + b)),
                    _ => Ok(SV::Str(format!("{}{}", to_display(x), to_display(y)).into())),
                }
            }
            "-" | "*" | "/" | "%" => {
                if let (SV::Int(a), SV::Int(b)) = (x, y) {
                    let (a, b) = (*a, *b);
                    return match op {
                        "-" => Ok(SV::Int(a.wrapping_sub(b))),
                        "*" => Ok(SV::Int(a.wrapping_mul(b))),
                        "/" => {
                            if b == 0 {
                                self.throw("integer division by zero")
                            } else if a % b == 0 {
                                Ok(SV::Int(a / b))
                            } else {
                                Ok(SV::Float(a as f64 / b as f64))
                            }
                        }
                        "%" => {
                            if b == 0 {
                                self.throw("integer modulo by zero")
                            } else {
                                Ok(SV::Int(a % b))
                            }
                        }
                        _ => unreachable!(),
                    };
                }
                let (a, b) = match (num_coerce(x), num_coerce(y)) {
                    (Some(a), Some(b)) => (a, b),
                    _ => return self.throw(format!("cannot apply '{}' to non-numbers", op)),
                };
                Ok(SV::Float(match op {
                    "-" => a - b,
                    "*" => a * b,
                    "/" => a / b,
                    "%" => a % b,
                    _ => unreachable!(),
                }))
            }
            "==" => Ok(SV::Bool(loose_eq(x, y))),
            "!=" => Ok(SV::Bool(!loose_eq(x, y))),
            "===" => Ok(SV::Bool(strict_eq(x, y))),
            "!==" => Ok(SV::Bool(!strict_eq(x, y))),
            "<" | ">" | "<=" | ">=" => {
                if let (SV::Str(a), SV::Str(b)) = (x, y) {
                    let r = match op {
                        "<" => a < b,
                        ">" => a > b,
                        "<=" => a <= b,
                        ">=" => a >= b,
                        _ => unreachable!(),
                    };
                    return Ok(SV::Bool(r));
                }
                let (a, b) = match (num_coerce(x), num_coerce(y)) {
                    (Some(a), Some(b)) => (a, b),
                    _ => return Ok(SV::Bool(false)),
                };
                Ok(SV::Bool(match op {
                    "<" => a < b,
                    ">" => a > b,
                    "<=" => a <= b,
                    ">=" => a >= b,
                    _ => unreachable!(),
                }))
            }
            "&" | "|" | "^" | "<<" | ">>" | ">>>" => {
                let (a, b) = match (num_coerce(x), num_coerce(y)) {
                    (Some(a), Some(b)) => (a as i64, b as i64),
                    _ => {
                        return self.throw(&format!(
                            "bitwise {} on non-numbers ({} {} {})",
                            op,
                            type_symbol(x),
                            op,
                            type_symbol(y)
                        ))
                    }
                };
                Ok(SV::Int(match op {
                    "&" => a & b,
                    "|" => a | b,
                    "^" => a ^ b,
                    "<<" => a << (b & 63),
                    ">>" => a >> (b & 63),
                    ">>>" => ((a as u64) >> (b as u64 & 63)) as i64,
                    _ => unreachable!(),
                }))
            }
            "in" => match y {
                SV::Object(o) => {
                    let key = to_display(x);
                    Ok(SV::Bool(o.props.borrow().iter().any(|(k, _)| *k == key)))
                }
                SV::Array(a) => Ok(SV::Bool(a.borrow().iter().any(|v| loose_eq(v, x)))),
                _ => Ok(SV::Bool(false)),
            },
            "instanceof" => {
                let class = match y {
                    SV::Class(c) => c,
                    _ => return Ok(SV::Bool(false)),
                };
                match x {
                    SV::Object(o) => {
                        let mut cur = o.class.borrow().clone();
                        while let Some(c) = cur {
                            if Gc::ptr_eq(&c, class) {
                                return Ok(SV::Bool(true));
                            }
                            cur = c.base.borrow().clone();
                        }
                        Ok(SV::Bool(false))
                    }
                    _ => Ok(SV::Bool(false)),
                }
            }
            "like" => Ok(SV::Bool(false)),
            _ => self.throw(format!("unsupported binary operator {}", op)),
        }
    }

    fn assign_to(&mut self, target: &Expr, value: SV, env: &EnvRef) -> SResult<()> {
        match target {
            Expr::Ident(name) => {
                if !env.assign(name, value.clone()) {
                    self.global.define(name, value);
                }
                Ok(())
            }
            Expr::Member(base, name) => {
                let b = self.eval(base, env)?;
                self.member_set(&b, name, value)
            }
            Expr::Index(base, idx) => {
                let b = self.eval(base, env)?;
                let i = self.eval(idx, env)?;
                self.index_set(&b, &i, value)
            }
            _ => self.throw("invalid assignment target"),
        }
    }

    pub fn member_get(&mut self, base: &SV, name: &str) -> SResult<SV> {
        if name == "length" {
            match base {
                SV::Str(s) => return Ok(SV::Int(s.chars().count() as i64)),
                SV::Array(a) => return Ok(SV::Int(a.borrow().len() as i64)),
                SV::Object(o) if o.native.is_none() && o.class.borrow().is_none() => {
                    return Ok(SV::Int(o.props.borrow().len() as i64))
                }
                _ => {}
            }
        }
        match base {
            SV::Object(o) => {
                if let Some(v) = o.props.borrow().iter().find(|(k, _)| k == name) {
                    return Ok(v.1.clone());
                }
                if let Some(native) = &o.native {
                    if let Some(v) = native.clone().get(self, name) {
                        return Ok(v);
                    }
                }
                let mut cur = o.class.borrow().clone();
                while let Some(c) = cur {
                    if let Some(m) = c.methods.borrow().get(name) {
                        return Ok(m.clone());
                    }
                    if let Some(v) = c.class_props.borrow().iter().find(|(k, _)| k == name) {
                        return Ok(v.1.clone());
                    }
                    cur = c.base.borrow().clone();
                }
                // A mounted Reactor component IS its root element: value/text/html
                // read the element when the component has no such field of its own.
                if matches!(name, "value" | "text" | "html")
                    && o.props.borrow().iter().any(|(k, _)| k == "__mount")
                {
                    if let Some(el) = crate::engine::host::component_root_element(base) {
                        return self.member_get(&el, name);
                    }
                }
                Ok(SV::Undefined)
            }
            SV::Class(c) => {
                if let Some(v) = c.class_props.borrow().iter().find(|(k, _)| k == name) {
                    return Ok(v.1.clone());
                }
                if let Some(m) = c.methods.borrow().get(name) {
                    return Ok(m.clone());
                }
                Ok(SV::Undefined)
            }
            SV::Undefined | SV::Null => {
                self.throw(format!("cannot read property '{}' of {}", name, to_display(base)))
            }
            _ => Ok(super::runtime::builtin_member(self, base, name)),
        }
    }

    pub fn member_set(&mut self, base: &SV, name: &str, value: SV) -> SResult<()> {
        match base {
            SV::Object(o) => {
                if let Some(native) = &o.native {
                    if native.clone().set(self, name, value.clone()) {
                        return Ok(());
                    }
                }
                if matches!(name, "value" | "text" | "html") {
                    let (has_prop, has_mount) = {
                        let p = o.props.borrow();
                        (
                            p.iter().any(|(k, _)| k == name),
                            p.iter().any(|(k, _)| k == "__mount"),
                        )
                    };
                    if !has_prop && has_mount {
                        if let Some(el) = crate::engine::host::component_root_element(base) {
                            self.member_set(&el, name, value)?;
                            return Ok(());
                        }
                    }
                }
                let mut props = o.props.borrow_mut();
                if let Some(slot) = props.iter_mut().find(|(k, _)| k == name) {
                    slot.1 = value;
                } else {
                    props.push((name.to_string(), value));
                }
                Ok(())
            }
            SV::Class(c) => {
                let mut props = c.class_props.borrow_mut();
                if let Some(slot) = props.iter_mut().find(|(k, _)| k == name) {
                    slot.1 = value;
                } else {
                    props.push((name.to_string(), value));
                }
                Ok(())
            }
            SV::Undefined | SV::Null => {
                self.throw(format!("cannot set property '{}' of {}", name, to_display(base)))
            }
            _ => Ok(()),
        }
    }

    pub fn index_get(&mut self, base: &SV, idx: &SV) -> SResult<SV> {
        // A numeric index may arrive as a Float (Math.abs/floor return floats);
        // Sciter coerces it to an integer for array/string access.
        let int_idx = match idx {
            SV::Int(i) => Some(*i),
            SV::Float(f) => Some(*f as i64),
            _ => None,
        };
        match (base, idx) {
            (SV::Array(a), _) if int_idx.is_some() => {
                let a = a.borrow();
                let i = int_idx.unwrap();
                if i < 0 || i as usize >= a.len() {
                    Ok(SV::Undefined)
                } else {
                    Ok(a[i as usize].clone())
                }
            }
            (SV::Str(s), _) if int_idx.is_some() => Ok(s
                .chars()
                .nth(int_idx.unwrap().max(0) as usize)
                .map(|c| SV::Str(c.to_string().into()))
                .unwrap_or(SV::Undefined)),
            (SV::Object(_), _) => {
                let key = match idx {
                    SV::Str(s) => s.to_string(),
                    SV::Symbol(s) => s.to_string(),
                    other => to_display(other),
                };
                self.member_get(base, &key)
            }
            (SV::Undefined | SV::Null, _) => {
                self.throw(format!("cannot index {}", to_display(base)))
            }
            _ => Ok(SV::Undefined),
        }
    }

    pub fn index_set(&mut self, base: &SV, idx: &SV, value: SV) -> SResult<()> {
        match (base, idx) {
            (SV::Array(a), SV::Int(i)) => {
                let mut a = a.borrow_mut();
                let i = *i as usize;
                if i >= a.len() {
                    a.resize(i + 1, SV::Undefined);
                }
                a[i] = value;
                Ok(())
            }
            (SV::Object(_), _) => {
                let key = match idx {
                    SV::Str(s) => s.to_string(),
                    SV::Symbol(s) => s.to_string(),
                    other => to_display(other),
                };
                self.member_set(base, &key, value)
            }
            _ => Ok(()),
        }
    }

    fn eval_call(&mut self, callee: &Expr, args: &[Expr], env: &EnvRef) -> SResult<SV> {
        let mut argv = Vec::with_capacity(args.len());
        match callee {
            Expr::Member(base, name) => {
                let b = self.eval(base, env)?;
                for a in args {
                    argv.push(self.eval(a, env)?);
                }
                self.call_method(&b, name, &argv)
            }
            Expr::Stringizer(target, name, parts) => {
                let text = self.stringizer_text(parts, env)?;
                let base = match target {
                    None => self.self_object.clone(),
                    Some(t) => self.eval(t, env)?,
                };
                let receiver = self.call_native_stringizer(&base, name, &text)?;
                for a in args {
                    argv.push(self.eval(a, env)?);
                }
                self.call_value(&receiver, &SV::Undefined, &argv)
            }
            Expr::Ident(name) if env.lookup(name).is_none() => {
                // Bare call to a name that is not a variable: resolve it as a
                // method on `this` (implicit-this, like a class method calling
                // a sibling method bare). Falls back to the normal path if
                // there is no object `this` to dispatch on.
                let this = env.lookup("this");
                if let Some(this @ SV::Object(_)) = this {
                    for a in args {
                        argv.push(self.eval(a, env)?);
                    }
                    return self.call_method(&this, name, &argv);
                }
                let f = self.eval(callee, env)?;
                for a in args {
                    argv.push(self.eval(a, env)?);
                }
                self.call_value(&f, &SV::Undefined, &argv)
            }
            _ => {
                let f = self.eval(callee, env)?;
                for a in args {
                    argv.push(self.eval(a, env)?);
                }
                self.call_value(&f, &SV::Undefined, &argv)
            }
        }
    }

    pub fn call_method(&mut self, base: &SV, name: &str, argv: &[SV]) -> SResult<SV> {
        if let SV::Object(o) = base {
            if let Some(v) = o.props.borrow().iter().find(|(k, _)| k == name) {
                let f = v.1.clone();
                return self.call_value(&f, base, argv);
            }
            if let Some(native) = &o.native {
                if let Some(r) = native.clone().call_method(self, name, argv) {
                    return r;
                }
            }
            let mut cur = o.class.borrow().clone();
            while let Some(c) = cur {
                let m = c.methods.borrow().get(name).cloned();
                if let Some(m) = m {
                    return self.call_value(&m, base, argv);
                }
                cur = c.base.borrow().clone();
            }
            let class_name = o
                .class
                .borrow()
                .as_ref()
                .map(|c| format!(" (class {})", c.name))
                .unwrap_or_default();
            return self.throw(format!("no method '{}' on object{}", name, class_name));
        }
        if let SV::Class(c) = base {
            let m = c
                .class_props
                .borrow()
                .iter()
                .find(|(k, _)| k == name)
                .map(|(_, v)| v.clone())
                .or_else(|| c.methods.borrow().get(name).cloned());
            if let Some(m) = m {
                return self.call_value(&m, base, argv);
            }
            return self.throw(format!("no static '{}' on class {}", name, c.name));
        }
        super::runtime::call_builtin_method(self, base, name, argv)
    }

    pub fn call_value(&mut self, f: &SV, this: &SV, argv: &[SV]) -> SResult<SV> {
        match f {
            SV::Function(func) => self.call_function(func.clone(), this, argv),
            SV::NativeFn(nf) => (nf.f)(self, this, argv),
            SV::Class(_) => self.construct(f, argv),
            SV::Undefined | SV::Null => self.throw("cannot call undefined"),
            other => self.throw(format!("cannot call {}", type_symbol(other))),
        }
    }

    pub fn call_function(&mut self, func: Gc<FuncVal>, this: &SV, argv: &[SV]) -> SResult<SV> {
        let scope = Env::new(Some(func.env.clone()));
        let this_value = match &func.this_capture {
            Some(t) => (**t).clone(),
            None => this.clone(),
        };
        scope.define("this", this_value);
        for (i, param) in func.params.iter().enumerate() {
            let v = if param.rest {
                let rest: Vec<SV> = argv.iter().skip(i).cloned().collect();
                sv_array(rest)
            } else {
                match argv.get(i) {
                    Some(v) => v.clone(),
                    None => match &param.default {
                        Some(d) => self.eval(d, &scope)?,
                        None => SV::Undefined,
                    },
                }
            };
            scope.define(&param.name, v);
        }
        scope.define(
            "arguments",
            sv_array(argv.to_vec()),
        );
        match self.exec_block(&func.body, &scope)? {
            Completion::Return(v) => Ok(v),
            _ => Ok(SV::Undefined),
        }
    }

    pub fn construct(&mut self, class: &SV, argv: &[SV]) -> SResult<SV> {
        match class {
            SV::Class(c) => {
                let obj = sv_object(ObjectData {
                    class: RefCell::new(Some(c.clone())),
                    props: RefCell::new(Vec::new()),
                    native: None,
                });
                let mut cur = Some(c.clone());
                let mut ctor = None;
                while let Some(cl) = cur {
                    if let Some(m) = cl.methods.borrow().get("this") {
                        ctor = Some(m.clone());
                        break;
                    }
                    cur = cl.base.borrow().clone();
                }
                if let Some(ctor) = ctor {
                    self.call_value(&ctor, &obj, argv)?;
                }
                self.register_class_events(c, &obj)?;
                Ok(obj)
            }
            SV::NativeFn(nf) => (nf.f)(self, &SV::Undefined, argv),
            SV::Object(o) => {
                if let Some(native) = &o.native {
                    if let Some(r) = native.clone().call_method(self, "new", argv) {
                        return r;
                    }
                }
                self.throw("new on non-class")
            }
            _ => self.throw("new on non-class"),
        }
    }

    fn register_class_events(&mut self, class: &Gc<ClassVal>, instance: &SV) -> SResult<()> {
        let mut chain: Vec<Gc<ClassVal>> = Vec::new();
        let mut cur = Some(class.clone());
        while let Some(c) = cur {
            cur = c.base.borrow().clone();
            chain.push(c);
        }
        for c in chain.into_iter().rev() {
            let env = match c.class_env.borrow().clone() {
                Some(e) => e,
                None => self.global.clone(),
            };
            let decls = c.events.borrow().clone();
            for decl in &decls {
                let (name, selector, func) =
                    self.event_to_function(decl, &env, Some(instance.clone()))?;
                self.pending_events.push(PendingEvent {
                    name,
                    selector,
                    func,
                    this: instance.clone(),
                    target: instance.clone(),
                });
            }
        }
        Ok(())
    }

    fn stringizer_text(&mut self, parts: &[TextPart], env: &EnvRef) -> SResult<String> {
        let mut out = String::new();
        for part in parts {
            match part {
                TextPart::Text(t) => out.push_str(t),
                TextPart::Expr(e) => {
                    let v = self.eval(e, env)?;
                    out.push_str(&to_display(&v));
                }
            }
        }
        Ok(out)
    }

    fn call_native_stringizer(&mut self, base: &SV, name: &str, text: &str) -> SResult<SV> {
        let argv = [SV::Str(text.into())];
        match base {
            SV::Object(o) => {
                if let Some(v) = o.props.borrow().iter().find(|(k, _)| k == name) {
                    let f = v.1.clone();
                    return self.call_value(&f, base, &argv);
                }
                if let Some(native) = &o.native {
                    if let Some(r) = native.clone().call_method(self, name, &argv) {
                        return r;
                    }
                }
                let mut cur = o.class.borrow().clone();
                while let Some(c) = cur {
                    let m = c.methods.borrow().get(name).cloned();
                    if let Some(m) = m {
                        return self.call_value(&m, base, &argv);
                    }
                    cur = c.base.borrow().clone();
                }
                Ok(SV::Null)
            }
            SV::Undefined | SV::Null => {
                self.throw(format!("cannot call stringizer '{}' on {}", name, to_display(base)))
            }
            _ => Ok(SV::Null),
        }
    }

    fn eval_jsx(&mut self, node: &JsxNode, env: &EnvRef) -> SResult<SV> {
        let tag_value = if node
            .tag
            .chars()
            .next()
            .map_or(false, |c| c.is_ascii_uppercase())
        {
            match env.lookup(&node.tag) {
                Some(v) => v,
                None => SV::Str(node.tag.as_str().into()),
            }
        } else {
            SV::Str(node.tag.as_str().into())
        };
        let attrs = ObjectData {
            class: RefCell::new(None),
            props: RefCell::new(Vec::new()),
            native: None,
        };
        if let Some(t) = &node.type_suffix {
            attrs
                .props
                .borrow_mut()
                .push(("type".into(), SV::Str(t.as_str().into())));
        }
        if let Some(n) = &node.name_binding {
            attrs
                .props
                .borrow_mut()
                .push(("name".into(), SV::Str(n.as_str().into())));
        }
        for attr in &node.attrs {
            match attr {
                JsxAttr::Id(parts) => {
                    let mut id = String::new();
                    for p in parts {
                        match p {
                            TextPart::Text(t) => id.push_str(t),
                            TextPart::Expr(e) => {
                                let v = self.eval(e, env)?;
                                id.push_str(&to_display(&v));
                            }
                        }
                    }
                    attrs
                        .props
                        .borrow_mut()
                        .push(("id".into(), SV::Str(id.into())));
                }
                JsxAttr::Class(c) => {
                    let mut props = attrs.props.borrow_mut();
                    if let Some(slot) = props.iter_mut().find(|(k, _)| k == "class") {
                        let merged = format!("{} {}", to_display(&slot.1), c);
                        slot.1 = SV::Str(merged.into());
                    } else {
                        props.push(("class".into(), SV::Str(c.as_str().into())));
                    }
                }
                JsxAttr::Ref(e) => {
                    if let Expr::Member(base, name) = e {
                        let b = self.eval(base, env)?;
                        let binding = ObjectData {
                            class: RefCell::new(None),
                            props: RefCell::new(vec![
                                ("target".into(), b),
                                ("prop".into(), SV::Str(name.as_str().into())),
                            ]),
                            native: None,
                        };
                        attrs
                            .props
                            .borrow_mut()
                            .push(("@ref".into(), sv_object(binding)));
                    }
                }
                JsxAttr::Splat(e) => {
                    let v = self.eval(e, env)?;
                    if let SV::Object(o) = &v {
                        let entries = o.props.borrow().clone();
                        let mut props = attrs.props.borrow_mut();
                        for (k, val) in entries {
                            if let Some(slot) = props.iter_mut().find(|(pk, _)| *pk == k) {
                                slot.1 = val;
                            } else {
                                props.push((k, val));
                            }
                        }
                    }
                }
                JsxAttr::Named(name, value) => {
                    let v = match value {
                        None => SV::Bool(true),
                        Some(JsxAttrValue::Str(s)) => SV::Str(s.as_str().into()),
                        Some(JsxAttrValue::Int(i)) => SV::Int(*i),
                        Some(JsxAttrValue::Float(f)) => SV::Float(*f),
                        Some(JsxAttrValue::Unit(x, u)) => SV::Unit(*x, u.as_str().into()),
                        Some(JsxAttrValue::Ident(s)) => SV::Str(s.as_str().into()),
                        Some(JsxAttrValue::Expr(e)) => self.eval(e, env)?,
                    };
                    let mut props = attrs.props.borrow_mut();
                    if name == "class" {
                        if let Some(slot) = props.iter_mut().find(|(k, _)| k == "class") {
                            let base = to_display(&slot.1);
                            let add = to_display(&v);
                            let merged = if base.trim().is_empty() {
                                add
                            } else if add.trim().is_empty() {
                                base
                            } else {
                                format!("{} {}", base, add)
                            };
                            slot.1 = SV::Str(merged.into());
                            continue;
                        }
                    }
                    props.push((name.clone(), v));
                }
            }
        }
        let mut children = Vec::new();
        for child in &node.children {
            match child {
                JsxChild::Text(t) => {
                    let trimmed = t.trim();
                    if !trimmed.is_empty() {
                        let decoded = crate::engine::html::decode_entities(trimmed);
                        children.push(SV::Str(decoded.into()));
                    }
                }
                JsxChild::Expr(e) => {
                    let v = self.eval(e, env)?;
                    children.push(v);
                }
                JsxChild::Element(el) => {
                    children.push(self.eval_jsx(el, env)?);
                }
            }
        }
        let vnode = vec![
            tag_value,
            sv_object(attrs),
            sv_array(children),
        ];
        Ok(sv_array(vnode))
    }
}
