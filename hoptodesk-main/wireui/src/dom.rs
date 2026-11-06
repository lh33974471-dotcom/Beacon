use crate::capi::scdom::SCDOM_RESULT;
use crate::value::Value;
use std::fmt;

pub use crate::capi::scdom::{ELEMENT_AREAS, HELEMENT};

pub type Result<T> = std::result::Result<T, SCDOM_RESULT>;

pub struct Element {
    he: HELEMENT,
}

unsafe impl Send for Element {}
unsafe impl Sync for Element {}

impl Element {
    pub fn as_ptr(&self) -> HELEMENT {
        self.he
    }

    pub fn call_method(&self, name: &str, args: &[Value]) -> Result<Value> {
        // Called from the client's io thread as well as the UI thread. Off the
        // UI thread the script interpreter is unavailable, so queue the call to
        // run on the UI thread (fire-and-forget) and return null immediately.
        if !crate::engine::host::on_ui_thread() {
            crate::engine::host::queue_offthread_call(self.he, name, args.to_vec());
            return Ok(Value::null());
        }
        let sv_args: Vec<crate::script::interp::SV> =
            args.iter().map(crate::bridge::value_to_sv).collect();
        match crate::engine::host::element_call_method(self.he, name, &sv_args) {
            Ok(sv) => Ok(crate::bridge::sv_to_value(&sv)),
            Err(()) => Err(SCDOM_RESULT::PASSIVE_HANDLE),
        }
    }

    pub fn call_method_queued(&self, name: &str, args: &[Value]) {
        crate::engine::host::queue_offthread_call(self.he, name, args.to_vec());
    }

    pub fn call_function(&self, name: &str, args: &[Value]) -> Result<Value> {
        self.call_method(name, args)
    }

    pub fn get_location(&self, kind: u32) -> Result<crate::capi::sctypes::RECT> {
        let _ = kind;
        crate::engine::host::element_location(self.he).ok_or(SCDOM_RESULT::PASSIVE_HANDLE)
    }
}

impl From<HELEMENT> for Element {
    fn from(he: HELEMENT) -> Element {
        Element { he }
    }
}

impl Clone for Element {
    fn clone(&self) -> Element {
        Element { he: self.he }
    }
}

impl fmt::Display for Element {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match crate::engine::host::element_display(self.he) {
            Some(s) => write!(f, "{}", s),
            None => write!(f, "element({:p})", self.he),
        }
    }
}

impl fmt::Debug for Element {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{{{}}}", self)
    }
}

pub mod event {
    use crate::capi::sctypes::{HGFX, LPVOID, RECT};
    use crate::value::Value;

    pub use crate::capi::scbehavior::{
        BEHAVIOR_EVENTS, CLICK_REASON, DRAW_EVENTS, EDIT_CHANGED_REASON, EVENT_GROUPS, PHASE_MASK,
    };
    pub use crate::capi::scdom::HELEMENT;

    pub fn default_events() -> EVENT_GROUPS {
        EVENT_GROUPS::HANDLE_BEHAVIOR_EVENT
            | EVENT_GROUPS::HANDLE_SCRIPTING_METHOD_CALL
            | EVENT_GROUPS::HANDLE_METHOD_CALL
    }

    #[derive(Debug)]
    pub enum EventReason {
        General(CLICK_REASON),
        EditChanged(EDIT_CHANGED_REASON),
        VideoBind(LPVOID),
    }

    #[derive(Debug)]
    pub enum MethodParams<'a> {
        Click,
        IsEmpty(&'a mut bool),
        GetValue(&'a mut Value),
        SetValue(Value),
        Custom(u32, LPVOID),
    }

    #[allow(unused_variables)]
    pub trait EventHandler {
        fn get_subscription(&mut self) -> Option<EVENT_GROUPS> {
            Some(default_events())
        }

        fn attached(&mut self, root: HELEMENT) {}

        fn detached(&mut self, root: HELEMENT) {}

        fn document_complete(&mut self, root: HELEMENT, target: HELEMENT) {}

        fn document_close(&mut self, root: HELEMENT, target: HELEMENT) {}

        fn on_method_call(&mut self, root: HELEMENT, params: MethodParams) -> bool {
            false
        }

        fn on_script_call(&mut self, root: HELEMENT, name: &str, args: &[Value]) -> Option<Value> {
            self.dispatch_script_call(root, name, args)
        }

        #[doc(hidden)]
        fn dispatch_script_call(
            &mut self,
            root: HELEMENT,
            name: &str,
            args: &[Value],
        ) -> Option<Value> {
            None
        }

        fn on_event(
            &mut self,
            root: HELEMENT,
            source: HELEMENT,
            target: HELEMENT,
            code: BEHAVIOR_EVENTS,
            phase: PHASE_MASK,
            reason: EventReason,
        ) -> bool {
            false
        }

        fn on_timer(&mut self, root: HELEMENT, timer_id: u64) -> bool {
            false
        }

        fn on_draw(&mut self, root: HELEMENT, gfx: HGFX, area: &RECT, layer: DRAW_EVENTS) -> bool {
            false
        }

        fn on_size(&mut self, root: HELEMENT) {}
    }
}
