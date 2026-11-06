use super::interp::{ClassVal, Env, FuncVal, ObjectData, SV};
use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::{Rc, Weak};

#[derive(Default)]
struct Registry {
    objects: Vec<Weak<ObjectData>>,
    arrays: Vec<Weak<RefCell<Vec<SV>>>>,
    funcs: Vec<Weak<FuncVal>>,
    envs: Vec<Weak<Env>>,
    allocs_since_gc: usize,
}

thread_local! {
    static REGISTRY: RefCell<Registry> = RefCell::new(Registry::default());
}

pub fn track_object(o: &Rc<ObjectData>) {
    REGISTRY.with(|r| {
        let mut r = r.borrow_mut();
        r.objects.push(Rc::downgrade(o));
        r.allocs_since_gc += 1;
    });
}

pub fn track_array(a: &Rc<RefCell<Vec<SV>>>) {
    REGISTRY.with(|r| {
        let mut r = r.borrow_mut();
        r.arrays.push(Rc::downgrade(a));
        r.allocs_since_gc += 1;
    });
}

pub fn track_func(f: &Rc<FuncVal>) {
    REGISTRY.with(|r| {
        let mut r = r.borrow_mut();
        r.funcs.push(Rc::downgrade(f));
        r.allocs_since_gc += 1;
    });
}

pub fn track_env(e: &Rc<Env>) {
    REGISTRY.with(|r| {
        let mut r = r.borrow_mut();
        r.envs.push(Rc::downgrade(e));
        r.allocs_since_gc += 1;
    });
}

pub fn allocs_since_gc() -> usize {
    REGISTRY.with(|r| r.borrow().allocs_since_gc)
}

#[derive(Debug, Default, Clone, Copy)]
pub struct GcStats {
    pub objects_swept: usize,
    pub arrays_swept: usize,
    pub funcs_swept: usize,
    pub envs_swept: usize,
    pub tracked_after: usize,
}

#[derive(Default)]
struct Marks {
    objects: HashSet<*const ObjectData>,
    arrays: HashSet<*const RefCell<Vec<SV>>>,
    funcs: HashSet<*const FuncVal>,
    envs: HashSet<*const Env>,
    classes: HashSet<*const ClassVal>,
}

fn mark_sv(v: &SV, m: &mut Marks) {
    match v {
        SV::Array(a) => {
            if m.arrays.insert(Rc::as_ptr(a)) {
                let items = a.borrow();
                for item in items.iter() {
                    mark_sv(item, m);
                }
            }
        }
        SV::Object(o) => {
            if m.objects.insert(Rc::as_ptr(o)) {
                let props = o.props.borrow();
                for (_, pv) in props.iter() {
                    mark_sv(pv, m);
                }
                drop(props);
                if let Some(c) = o.class.borrow().as_ref() {
                    mark_class(c, m);
                }
            }
        }
        SV::Class(c) => mark_class(c, m),
        SV::Function(f) => {
            if m.funcs.insert(Rc::as_ptr(f)) {
                if let Some(t) = &f.this_capture {
                    mark_sv(t, m);
                }
                mark_env(&f.env, m);
            }
        }
        _ => {}
    }
}

fn mark_env(e: &Rc<Env>, m: &mut Marks) {
    if m.envs.insert(Rc::as_ptr(e)) {
        let vars = e.vars.borrow();
        for v in vars.values() {
            mark_sv(v, m);
        }
        drop(vars);
        if let Some(p) = &e.parent {
            mark_env(p, m);
        }
    }
}

fn mark_class(c: &Rc<ClassVal>, m: &mut Marks) {
    if m.classes.insert(Rc::as_ptr(c)) {
        let methods = c.methods.borrow();
        for v in methods.values() {
            mark_sv(v, m);
        }
        drop(methods);
        let props = c.class_props.borrow();
        for (_, v) in props.iter() {
            mark_sv(v, m);
        }
        drop(props);
        if let Some(e) = c.class_env.borrow().as_ref() {
            mark_env(e, m);
        }
        if let Some(b) = c.base.borrow().as_ref() {
            mark_class(b, m);
        }
    }
}

pub fn collect(roots: &[SV], root_envs: &[Rc<Env>]) -> GcStats {
    let mut marks = Marks::default();
    for r in roots {
        mark_sv(r, &mut marks);
    }
    for e in root_envs {
        mark_env(e, &mut marks);
    }

    let mut stats = GcStats::default();
    let mut clear_objects: Vec<Rc<ObjectData>> = Vec::new();
    let mut clear_arrays: Vec<Rc<RefCell<Vec<SV>>>> = Vec::new();
    let mut clear_envs: Vec<Rc<Env>> = Vec::new();

    REGISTRY.with(|reg| {
        let mut reg = reg.borrow_mut();
        reg.objects.retain(|w| match w.upgrade() {
            None => false,
            Some(o) => {
                if marks.objects.contains(&Rc::as_ptr(&o)) {
                    true
                } else {
                    clear_objects.push(o);
                    false
                }
            }
        });
        reg.arrays.retain(|w| match w.upgrade() {
            None => false,
            Some(a) => {
                if marks.arrays.contains(&Rc::as_ptr(&a)) {
                    true
                } else {
                    clear_arrays.push(a);
                    false
                }
            }
        });
        reg.funcs.retain(|w| match w.upgrade() {
            None => false,
            Some(f) => {
                if marks.funcs.contains(&Rc::as_ptr(&f)) {
                    true
                } else {
                    stats.funcs_swept += 1;
                    false
                }
            }
        });
        reg.envs.retain(|w| match w.upgrade() {
            None => false,
            Some(e) => {
                if marks.envs.contains(&Rc::as_ptr(&e)) {
                    true
                } else {
                    clear_envs.push(e);
                    false
                }
            }
        });
        stats.tracked_after =
            reg.objects.len() + reg.arrays.len() + reg.funcs.len() + reg.envs.len();
        reg.allocs_since_gc = 0;
    });

    stats.objects_swept = clear_objects.len();
    stats.arrays_swept = clear_arrays.len();
    stats.envs_swept = clear_envs.len();

    for o in &clear_objects {
        o.props.borrow_mut().clear();
        *o.class.borrow_mut() = None;
    }
    for a in &clear_arrays {
        a.borrow_mut().clear();
    }
    for e in &clear_envs {
        e.vars.borrow_mut().clear();
    }

    stats
}
