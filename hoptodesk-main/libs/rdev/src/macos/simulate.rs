use crate::keycodes::macos::{code_from_key, virtual_keycodes::*};
use crate::macos::common::CGEventSourceKeyState;
use crate::rdev::{Button, EventType, RawKey, SimulateError};
use core_graphics::{
    event::{
        CGEvent, CGEventFlags, CGEventTapLocation, CGEventType, CGKeyCode, CGMouseButton,
        EventField, ScrollEventUnit,
    },
    event_source::{CGEventSource, CGEventSourceStateID},
    geometry::CGPoint,
};
use std::convert::TryInto;
use std::sync::atomic::{AtomicU64, Ordering};

static mut MOUSE_EXTRA_INFO: i64 = 0;
static mut KEYBOARD_EXTRA_INFO: i64 = 0;

pub fn set_mouse_extra_info(extra: i64) {
    unsafe { MOUSE_EXTRA_INFO = extra }
}

pub fn set_keyboard_extra_info(extra: i64) {
    unsafe { KEYBOARD_EXTRA_INFO = extra }
}

#[allow(non_upper_case_globals)]
fn has_intrinsic_fn(keycode: CGKeyCode) -> bool {
    matches!(
        keycode,
        kVK_F1 | kVK_F2 | kVK_F3 | kVK_F4 | kVK_F5 | kVK_F6 | kVK_F7 | kVK_F8 | kVK_F9
            | kVK_F10 | kVK_F11 | kVK_F12 | kVK_F13 | kVK_F14 | kVK_F15 | kVK_F16 | kVK_F17
            | kVK_F18 | kVK_F19 | kVK_ANSI_KeypadClear | kVK_ForwardDelete | kVK_Home
            | kVK_End | kVK_PageDown | kVK_PageUp
            | 129
            | 130
            | 131
            | 144
            | 145
            | kVK_UpArrow | kVK_DownArrow | kVK_LeftArrow | kVK_RightArrow | kVK_Help
    )
}

#[allow(non_upper_case_globals)]
fn workaround_fn(event: CGEvent, keycode: CGKeyCode) -> CGEvent {
    match keycode {
        // https://github.com/rustdesk/rustdesk/issues/10126
        // https://stackoverflow.com/questions/74938870/sticky-fn-after-home-is-simulated-programmatically-macos
        // `kVK_F20` does not stick `CGEventFlags::CGEventFlagSecondaryFn`
        kVK_F1 | kVK_F2 | kVK_F3 | kVK_F4 | kVK_F5 | kVK_F6 | kVK_F7 | kVK_F8 | kVK_F9
        | kVK_F10 | kVK_F11 | kVK_F12 | kVK_F13 | kVK_F14 | kVK_F15 | kVK_F16 | kVK_F17
        | kVK_F18 | kVK_F19 | kVK_ANSI_KeypadClear | kVK_ForwardDelete | kVK_Home
        | kVK_End | kVK_PageDown | kVK_PageUp
        | 129 // Spotlight Search
        | 130 // Application
        | 131 // Launchpad
        | 144 // Brightness Up
        | 145 // Brightness Down
        => {
            let flags = event.get_flags();
            event.set_flags(flags & (!(CGEventFlags::CGEventFlagSecondaryFn)));
        }
        kVK_UpArrow | kVK_DownArrow | kVK_LeftArrow | kVK_RightArrow => {
            let flags = event.get_flags();
            event.set_flags(
                flags
                    & (!(CGEventFlags::CGEventFlagSecondaryFn
                        | CGEventFlags::CGEventFlagNumericPad)),
            );
        }
        kVK_Help => {
            let flags = event.get_flags();
            event.set_flags(
                flags
                    & (!(CGEventFlags::CGEventFlagSecondaryFn
                        | CGEventFlags::CGEventFlagHelp)),
            );
        }
        _ => {}
    }
    event
}

unsafe fn convert_native_with_source(
    event_type: &EventType,
    source: CGEventSource,
) -> Option<CGEvent> {
    match event_type {
        EventType::KeyPress(key) => match key {
            crate::Key::RawKey(rawkey) => {
                if let RawKey::MacVirtualKeycode(keycode) = rawkey {
                    CGEvent::new_keyboard_event(source, *keycode as _, true)
                        // Don't use `workaround_fn()` for `KeyPress`, or `F11` will not work.
                        // .and_then(|event| Ok(workaround_fn(event, *keycode)))
                        .ok()
                } else {
                    None
                }
            }
            _ => {
                let code = code_from_key(*key)?;
                CGEvent::new_keyboard_event(source, code as _, true)
                    // Don't use `workaround_fn()` for `KeyPress`, or `F11` will not work.
                    // .and_then(|event| Ok(workaround_fn(event, code as _)))
                    .ok()
            }
        },
        EventType::KeyRelease(key) => match key {
            crate::Key::RawKey(rawkey) => {
                if let RawKey::MacVirtualKeycode(keycode) = rawkey {
                    CGEvent::new_keyboard_event(source, *keycode as _, false)
                        .and_then(|event| Ok(workaround_fn(event, *keycode)))
                        .ok()
                } else {
                    None
                }
            }
            _ => {
                let code = code_from_key(*key)?;
                CGEvent::new_keyboard_event(source, code as _, false)
                    .and_then(|event| Ok(workaround_fn(event, code as _)))
                    .ok()
            }
        },
        EventType::ButtonPress(button) => {
            let point = get_current_mouse_location()?;
            let event = match button {
                Button::Left => CGEventType::LeftMouseDown,
                Button::Right => CGEventType::RightMouseDown,
                _ => return None,
            };
            CGEvent::new_mouse_event(
                source,
                event,
                point,
                CGMouseButton::Left, // ignored because we don't use OtherMouse EventType
            )
            .ok()
        }
        EventType::ButtonRelease(button) => {
            let point = get_current_mouse_location()?;
            let event = match button {
                Button::Left => CGEventType::LeftMouseUp,
                Button::Right => CGEventType::RightMouseUp,
                _ => return None,
            };
            CGEvent::new_mouse_event(
                source,
                event,
                point,
                CGMouseButton::Left, // ignored because we don't use OtherMouse EventType
            )
            .ok()
        }
        EventType::MouseMove { x, y } => {
            let point = CGPoint { x: (*x), y: (*y) };
            CGEvent::new_mouse_event(source, CGEventType::MouseMoved, point, CGMouseButton::Left)
                .ok()
        }
        EventType::Wheel { delta_x, delta_y } => {
            let wheel_count = 2;
            CGEvent::new_scroll_event(
                source,
                ScrollEventUnit::PIXEL,
                wheel_count,
                (*delta_y).try_into().ok()?,
                (*delta_x).try_into().ok()?,
                0,
            )
            .ok()
        }
    }
}

unsafe fn convert_native(event_type: &EventType) -> Option<CGEvent> {
    // https://developer.apple.com/documentation/coregraphics/cgeventsourcestateid#:~:text=kCGEventSourceStatePrivate
    let source = CGEventSource::new(CGEventSourceStateID::HIDSystemState).ok()?;
    convert_native_with_source(event_type, source)
}

unsafe fn get_current_mouse_location() -> Option<CGPoint> {
    let source = CGEventSource::new(CGEventSourceStateID::HIDSystemState).ok()?;
    let event = CGEvent::new(source).ok()?;
    Some(event.location())
}

pub fn simulate(event_type: &EventType) -> Result<(), SimulateError> {
    unsafe {
        if let Some(cg_event) = convert_native(event_type) {
            cg_event.set_integer_value_field(EventField::EVENT_SOURCE_USER_DATA, MOUSE_EXTRA_INFO);
            cg_event.post(CGEventTapLocation::HID);
            Ok(())
        } else {
            Err(SimulateError)
        }
    }
}

const MODIFIER_KEYS: [(CGKeyCode, u64, u64); 9] = [
    (kVK_Shift, CGEventFlags::CGEventFlagShift.bits(), 0x0000_0002),
    (kVK_RightShift, CGEventFlags::CGEventFlagShift.bits(), 0x0000_0004),
    (kVK_Control, CGEventFlags::CGEventFlagControl.bits(), 0x0000_0001),
    (kVK_RightControl, CGEventFlags::CGEventFlagControl.bits(), 0x0000_2000),
    (kVK_Option, CGEventFlags::CGEventFlagAlternate.bits(), 0x0000_0020),
    (kVK_RightOption, CGEventFlags::CGEventFlagAlternate.bits(), 0x0000_0040),
    (kVK_Command, CGEventFlags::CGEventFlagCommand.bits(), 0x0000_0008),
    (kVK_RightCommand, CGEventFlags::CGEventFlagCommand.bits(), 0x0000_0010),
    (
        kVK_Function,
        CGEventFlags::CGEventFlagSecondaryFn.bits(),
        CGEventFlags::CGEventFlagSecondaryFn.bits(),
    ),
];

fn modifier_mask() -> u64 {
    MODIFIER_KEYS
        .iter()
        .fold(0, |mask, (_, flag, bit)| mask | flag | bit)
}

fn key_event_keycode(event_type: &EventType) -> Option<(CGKeyCode, bool)> {
    let (key, down) = match event_type {
        EventType::KeyPress(key) => (key, true),
        EventType::KeyRelease(key) => (key, false),
        _ => return None,
    };
    let keycode = match key {
        crate::Key::RawKey(RawKey::MacVirtualKeycode(code)) => *code as CGKeyCode,
        crate::Key::RawKey(_) => return None,
        _ => code_from_key(*key)?,
    };
    Some((keycode, down))
}

pub struct VirtualInput {
    source: CGEventSource,
    tap_loc: CGEventTapLocation,
    held_modifiers: AtomicU64,
}

impl VirtualInput {
    pub fn new(state_id: CGEventSourceStateID, tap_loc: CGEventTapLocation) -> Result<Self, ()> {
        Ok(Self {
            source: CGEventSource::new(state_id)?,
            tap_loc,
            held_modifiers: AtomicU64::new(0),
        })
    }

    fn stamp_modifier_flags(&self, event_type: &EventType, event: &CGEvent) {
        let Some((keycode, down)) = key_event_keycode(event_type) else {
            return;
        };
        let mut held = self.held_modifiers.load(Ordering::Relaxed);
        if let Some(&(_, _, bit)) = MODIFIER_KEYS.iter().find(|(code, _, _)| *code == keycode) {
            held = if down { held | bit } else { held & !bit };
            self.held_modifiers.store(held, Ordering::Relaxed);
        }
        let wanted = MODIFIER_KEYS
            .iter()
            .filter(|(_, _, bit)| held & bit != 0)
            .fold(0, |wanted, (_, flag, bit)| wanted | flag | bit);
        let mut mask = modifier_mask();
        if down && has_intrinsic_fn(keycode) {
            mask &= !CGEventFlags::CGEventFlagSecondaryFn.bits();
        }
        let flags = event.get_flags().bits();
        let stamped = (flags & !mask) | wanted;
        if stamped != flags {
            event.set_flags(unsafe { CGEventFlags::from_bits_unchecked(stamped) });
        }
    }

    pub fn simulate(&self, event_type: &EventType) -> Result<(), SimulateError> {
        unsafe {
            if let Some(cg_event) = convert_native_with_source(event_type, self.source.clone()) {
                self.stamp_modifier_flags(event_type, &cg_event);
                cg_event.post(self.tap_loc);
                Ok(())
            } else {
                Err(SimulateError)
            }
        }
    }

    // keycode is defined in rdev::macos::virtual_keycodes
    pub fn get_key_state(state_id: CGEventSourceStateID, keycode: CGKeyCode) -> bool {
        unsafe { CGEventSourceKeyState(state_id, keycode) }
    }
}
