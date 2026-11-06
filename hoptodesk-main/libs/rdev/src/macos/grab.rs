#![allow(improper_ctypes_definitions)]
use crate::macos::common::*;
use crate::rdev::{Event, GrabError};
use cocoa::base::nil;
use cocoa::foundation::NSAutoreleasePool;
use core_graphics::event::{CGEventTapLocation, CGEventType};
use std::os::raw::c_void;

static mut GLOBAL_CALLBACK: Option<Box<dyn FnMut(Event) -> Option<Event>>> = None;

unsafe extern "C" fn raw_callback(
    _proxy: CGEventTapProxy,
    _type: CGEventType,
    cg_event: CGEventRef,
    _user_info: *mut c_void,
) -> CGEventRef {
    // println!("Event ref {:?}", cg_event_ptr);
    // let cg_event: CGEvent = transmute_copy::<*mut c_void, CGEvent>(&cg_event_ptr);
    if let Ok(mut state) = KEYBOARD_STATE.lock() {
        if let Some(keyboard) = state.as_mut() {
            if let Some(event) = convert(_type, &cg_event, keyboard) {
                if let Some(callback) = (*(&raw mut GLOBAL_CALLBACK)).as_mut() {
                    if callback(event).is_none() {
                        cg_event.set_type(CGEventType::Null);
                    }
                }
            }
        }
    }
    cg_event
}

static mut CUR_LOOP: CFRunLoopSourceRef = std::ptr::null_mut();
static mut TAP: CFMachPortRef = std::ptr::null_mut();
static mut SOURCE: CFRunLoopSourceRef = std::ptr::null_mut();

unsafe fn create_tap() -> Option<(CFMachPortRef, CFRunLoopSourceRef)> {
    let tap = CGEventTapCreate(
        CGEventTapLocation::Session,
        kCGHeadInsertEventTap,
        CGEventTapOption::Default,
        kCGEventMaskForAllEvents,
        raw_callback,
        nil,
    );
    if tap.is_null() {
        return None;
    }
    let source = CFMachPortCreateRunLoopSource(nil, tap, 0);
    if source.is_null() {
        CFMachPortInvalidate(tap);
        return None;
    }
    Some((tap, source))
}

pub fn rehook_keyboard() {
    unsafe {
        if CUR_LOOP.is_null() || TAP.is_null() {
            return;
        }
        let Some((tap, source)) = create_tap() else {
            return;
        };
        CFRunLoopAddSource(CUR_LOOP as _, source, kCFRunLoopCommonModes);
        CGEventTapEnable(tap, true);
        CFRunLoopWakeUp(CUR_LOOP as _);
        CGEventTapEnable(TAP, false);
        CFRunLoopRemoveSource(CUR_LOOP as _, SOURCE, kCFRunLoopCommonModes);
        CFMachPortInvalidate(TAP);
        core_foundation_sys::base::CFRelease(SOURCE as _);
        core_foundation_sys::base::CFRelease(TAP as _);
        TAP = tap;
        SOURCE = source;
    }
}

#[inline]
pub fn is_grabbed() -> bool {
    unsafe {
        !CUR_LOOP.is_null()
    }
}

pub fn grab<T>(callback: T) -> Result<(), GrabError>
where
    T: FnMut(Event) -> Option<Event> + 'static,
{
    if is_grabbed() {
        return Ok(());
    }

    unsafe {
        GLOBAL_CALLBACK = Some(Box::new(callback));
        let _pool = NSAutoreleasePool::new(nil);
        let Some((tap, _loop)) = create_tap() else {
            return Err(GrabError::EventTapError);
        };

        CUR_LOOP = CFRunLoopGetCurrent() as _;
        CFRunLoopAddSource(CUR_LOOP, _loop, kCFRunLoopCommonModes);

        CGEventTapEnable(tap, true);
        TAP = tap;
        SOURCE = _loop;
        CFRunLoopRun();
    }
    Ok(())
}

pub fn exit_grab() -> Result<(), GrabError> {
    unsafe {
        if !CUR_LOOP.is_null() {
            CFRunLoopStop(CUR_LOOP);
            CUR_LOOP = std::ptr::null_mut();
        }
    }
    Ok(())
}
