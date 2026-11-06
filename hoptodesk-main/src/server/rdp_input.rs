use super::input_service::{
    restore_clipboard_after_paste, save_clipboard_text_for_restore, set_clipboard_for_paste_sync,
};
use crate::uinput::service::{can_input_via_keysym, char_to_keysym, map_key};
use dbus::{blocking::SyncConnection, Path};
use enigo::{Key, KeyboardControllable, MouseButton, MouseControllable};
use hbb_common::{log, ResultType};
use scrap::wayland::pipewire::{get_portal, PwStreamInfo};
use scrap::wayland::remote_desktop_portal::OrgFreedesktopPortalRemoteDesktop as remote_desktop_portal;
use std::collections::HashMap;
use std::sync::Arc;

pub mod client {
    use hbb_common::platform::linux::{DISPLAY_DESKTOP_KDE, XDG_CURRENT_DESKTOP};

    use super::*;

    const EVDEV_MOUSE_LEFT: i32 = 272;
    const EVDEV_MOUSE_RIGHT: i32 = 273;
    const EVDEV_MOUSE_MIDDLE: i32 = 274;

    const PRESSED_DOWN_STATE: u32 = 1;
    const PRESSED_UP_STATE: u32 = 0;

    #[derive(Default)]
    struct ModifierState {
        shift_left: bool,
        shift_right: bool,
        ctrl_left: bool,
        ctrl_right: bool,
        alt_left: bool,
        alt_right: bool,
        meta_left: bool,
        meta_right: bool,
    }

    impl ModifierState {
        fn update(&mut self, key: &Key, down: bool) {
            match key {
                Key::Shift => self.shift_left = down,
                Key::RightShift => self.shift_right = down,
                Key::Control => self.ctrl_left = down,
                Key::RightControl => self.ctrl_right = down,
                Key::Alt => self.alt_left = down,
                Key::RightAlt => self.alt_right = down,
                Key::Meta | Key::Super | Key::Windows | Key::Command => self.meta_left = down,
                Key::RWin => self.meta_right = down,
                Key::Raw(code) => {
                    const EVDEV_OFFSET: u16 = 8;
                    const KEY_LEFTSHIFT: u16 = evdev::Key::KEY_LEFTSHIFT.code() + EVDEV_OFFSET;
                    const KEY_RIGHTSHIFT: u16 = evdev::Key::KEY_RIGHTSHIFT.code() + EVDEV_OFFSET;
                    const KEY_LEFTCTRL: u16 = evdev::Key::KEY_LEFTCTRL.code() + EVDEV_OFFSET;
                    const KEY_RIGHTCTRL: u16 = evdev::Key::KEY_RIGHTCTRL.code() + EVDEV_OFFSET;
                    const KEY_LEFTALT: u16 = evdev::Key::KEY_LEFTALT.code() + EVDEV_OFFSET;
                    const KEY_RIGHTALT: u16 = evdev::Key::KEY_RIGHTALT.code() + EVDEV_OFFSET;
                    const KEY_LEFTMETA: u16 = evdev::Key::KEY_LEFTMETA.code() + EVDEV_OFFSET;
                    const KEY_RIGHTMETA: u16 = evdev::Key::KEY_RIGHTMETA.code() + EVDEV_OFFSET;
                    match *code {
                        KEY_LEFTSHIFT => self.shift_left = down,
                        KEY_RIGHTSHIFT => self.shift_right = down,
                        KEY_LEFTCTRL => self.ctrl_left = down,
                        KEY_RIGHTCTRL => self.ctrl_right = down,
                        KEY_LEFTALT => self.alt_left = down,
                        KEY_RIGHTALT => self.alt_right = down,
                        KEY_LEFTMETA => self.meta_left = down,
                        KEY_RIGHTMETA => self.meta_right = down,
                        _ => {}
                    }
                }
                _ => {}
            }
        }
    }

    pub struct RdpInputKeyboard {
        conn: Arc<SyncConnection>,
        session: Path<'static>,
        modifier_state: ModifierState,
    }

    impl RdpInputKeyboard {
        pub fn new(conn: Arc<SyncConnection>, session: Path<'static>) -> ResultType<Self> {
            Ok(Self {
                conn,
                session,
                modifier_state: ModifierState::default(),
            })
        }
    }

    impl KeyboardControllable for RdpInputKeyboard {
        fn as_any(&self) -> &dyn std::any::Any {
            self
        }

        fn as_mut_any(&mut self) -> &mut dyn std::any::Any {
            self
        }

        fn get_key_state(&mut self, key: Key) -> bool {
            match key {
                Key::Shift => self.modifier_state.shift_left,
                Key::RightShift => self.modifier_state.shift_right,
                Key::Control => self.modifier_state.ctrl_left,
                Key::RightControl => self.modifier_state.ctrl_right,
                Key::Alt => self.modifier_state.alt_left,
                Key::RightAlt => self.modifier_state.alt_right,
                Key::Meta | Key::Super | Key::Windows | Key::Command => {
                    self.modifier_state.meta_left
                }
                Key::RWin => self.modifier_state.meta_right,
                _ => false,
            }
        }

        fn key_sequence(&mut self, s: &str) {
            if s.is_empty() {
                return;
            }

            let ascii_only = s.chars().all(|c| {
                let keysym = char_to_keysym(c);
                can_input_via_keysym(c, keysym)
            });
            if !ascii_only {
                input_text_via_clipboard(s, self.conn.clone(), &self.session);
                return;
            }

            for c in s.chars() {
                let keysym = char_to_keysym(c);
                if can_input_via_keysym(c, keysym) {
                    if let Err(e) = send_keysym(keysym, true, self.conn.clone(), &self.session) {
                        log::error!("Failed to send keysym down: {:?}", e);
                    }
                    if let Err(e) = send_keysym(keysym, false, self.conn.clone(), &self.session) {
                        log::error!("Failed to send keysym up: {:?}", e);
                    }
                }
            }
        }

        fn key_down(&mut self, key: Key) -> enigo::ResultType {
            if let Key::Layout(chr) = key {
                let keysym = char_to_keysym(chr);
                if can_input_via_keysym(chr, keysym) {
                    send_keysym(keysym, true, self.conn.clone(), &self.session)?;
                } else {
                    input_text_via_clipboard(&chr.to_string(), self.conn.clone(), &self.session);
                }
            } else {
                handle_key(true, key.clone(), self.conn.clone(), &self.session)?;
                self.modifier_state.update(&key, true);
            }
            Ok(())
        }

        fn key_up(&mut self, key: Key) {
            self.modifier_state.update(&key, false);

            if let Key::Layout(chr) = key {
                let keysym = char_to_keysym(chr);
                if can_input_via_keysym(chr, keysym) {
                    if let Err(e) = send_keysym(keysym, false, self.conn.clone(), &self.session) {
                        log::error!("Failed to send keysym up: {:?}", e);
                    }
                }
            } else {
                if let Err(e) = handle_key(false, key, self.conn.clone(), &self.session) {
                    log::error!("Failed to handle key up: {:?}", e);
                }
            }
        }

        fn key_click(&mut self, key: Key) {
            if let Key::Layout(chr) = key {
                let keysym = char_to_keysym(chr);
                if can_input_via_keysym(chr, keysym) {
                    if let Err(e) = send_keysym(keysym, true, self.conn.clone(), &self.session) {
                        log::error!("Failed to send keysym down: {:?}", e);
                    }
                    if let Err(e) = send_keysym(keysym, false, self.conn.clone(), &self.session) {
                        log::error!("Failed to send keysym up: {:?}", e);
                    }
                } else {
                    input_text_via_clipboard(&chr.to_string(), self.conn.clone(), &self.session);
                }
            } else {
                if let Err(e) = handle_key(true, key.clone(), self.conn.clone(), &self.session) {
                    log::error!("Failed to handle key down: {:?}", e);
                } else {
                    self.modifier_state.update(&key, true);
                }
                self.modifier_state.update(&key, false);
                if let Err(e) = handle_key(false, key, self.conn.clone(), &self.session) {
                    log::error!("Failed to handle key up: {:?}", e);
                }
            }
        }
    }

    fn input_text_via_clipboard(text: &str, conn: Arc<SyncConnection>, session: &Path<'static>) {
        if text.is_empty() {
            return;
        }
        let previous = save_clipboard_text_for_restore();
        if !set_clipboard_for_paste_sync(text) {
            return;
        }

        let portal = get_portal(&conn);
        let shift_keycode = evdev::Key::KEY_LEFTSHIFT.code() as i32;
        let insert_keycode = evdev::Key::KEY_INSERT.code() as i32;

        if let Err(e) = remote_desktop_portal::notify_keyboard_keycode(
            &portal,
            session,
            HashMap::new(),
            shift_keycode,
            PRESSED_DOWN_STATE,
        ) {
            log::error!("input_text_via_clipboard: failed to press Shift: {:?}", e);
            restore_clipboard_after_paste(previous, text);
            return;
        }

        if let Err(e) = remote_desktop_portal::notify_keyboard_keycode(
            &portal,
            session,
            HashMap::new(),
            insert_keycode,
            PRESSED_DOWN_STATE,
        ) {
            log::error!("input_text_via_clipboard: failed to press Insert: {:?}", e);
            let _ = remote_desktop_portal::notify_keyboard_keycode(
                &portal,
                session,
                HashMap::new(),
                shift_keycode,
                PRESSED_UP_STATE,
            );
            restore_clipboard_after_paste(previous, text);
            return;
        }

        if let Err(e) = remote_desktop_portal::notify_keyboard_keycode(
            &portal,
            session,
            HashMap::new(),
            insert_keycode,
            PRESSED_UP_STATE,
        ) {
            log::error!(
                "input_text_via_clipboard: failed to release Insert: {:?}",
                e
            );
        }

        if let Err(e) = remote_desktop_portal::notify_keyboard_keycode(
            &portal,
            session,
            HashMap::new(),
            shift_keycode,
            PRESSED_UP_STATE,
        ) {
            log::error!("input_text_via_clipboard: failed to release Shift: {:?}", e);
        }
        restore_clipboard_after_paste(previous, text);
    }

    fn desktop_is_niri(desktop: &str) -> bool {
        desktop
            .split(':')
            .any(|name| name.eq_ignore_ascii_case("niri"))
    }

    lazy_static::lazy_static! {
        static ref SHOULD_SCALE_POINTER_COORDINATES: bool =
            std::env::var(XDG_CURRENT_DESKTOP)
                .map(|desktop| desktop == DISPLAY_DESKTOP_KDE || desktop_is_niri(&desktop))
                .unwrap_or(false);
        static ref PORTAL_INVERTS_VERTICAL_SCROLL: bool =
            std::env::var(XDG_CURRENT_DESKTOP)
                .map(|desktop| desktop == DISPLAY_DESKTOP_KDE)
                .unwrap_or(false);
    }

    pub struct RdpInputMouse {
        conn: Arc<SyncConnection>,
        session: Path<'static>,
        stream: PwStreamInfo,
        scale: Option<f64>,
        position: (f64, f64),
    }

    impl RdpInputMouse {
        pub fn new(
            conn: Arc<SyncConnection>,
            session: Path<'static>,
            stream: PwStreamInfo,
            resolution: (usize, usize),
        ) -> ResultType<Self> {
            let scale = if *SHOULD_SCALE_POINTER_COORDINATES {
                if resolution.0 == 0 || stream.get_size().0 == 0 {
                    Some(1.0f64)
                } else {
                    Some(resolution.0 as f64 / stream.get_size().0 as f64)
                }
            } else {
                None
            };
            let pos = stream.get_position();
            Ok(Self {
                conn,
                session,
                stream,
                scale,
                position: (pos.0 as f64, pos.1 as f64),
            })
        }
    }

    #[cfg(test)]
    mod tests {
        use super::desktop_is_niri;

        #[test]
        fn detects_niri_in_desktop_list() {
            assert!(desktop_is_niri("niri"));
            assert!(desktop_is_niri("NIRI"));
            assert!(desktop_is_niri("GNOME:niri"));
            assert!(!desktop_is_niri("GNOME"));
        }
    }

    impl MouseControllable for RdpInputMouse {
        fn as_any(&self) -> &dyn std::any::Any {
            self
        }

        fn as_mut_any(&mut self) -> &mut dyn std::any::Any {
            self
        }

        fn mouse_move_to(&mut self, x: i32, y: i32) {
            let x = if let Some(s) = self.scale {
                x as f64 / s
            } else {
                x as f64
            };
            let y = if let Some(s) = self.scale {
                y as f64 / s
            } else {
                y as f64
            };
            let x = x - self.position.0;
            let y = y - self.position.1;
            let portal = get_portal(&self.conn);
            let _ = remote_desktop_portal::notify_pointer_motion_absolute(
                &portal,
                &self.session,
                HashMap::new(),
                self.stream.path as u32,
                x,
                y,
            );
        }
        fn mouse_move_relative(&mut self, x: i32, y: i32) {
            let x = if let Some(s) = self.scale {
                x as f64 / s
            } else {
                x as f64
            };
            let y = if let Some(s) = self.scale {
                y as f64 / s
            } else {
                y as f64
            };
            let portal = get_portal(&self.conn);
            let _ = remote_desktop_portal::notify_pointer_motion(
                &portal,
                &self.session,
                HashMap::new(),
                x,
                y,
            );
        }
        fn mouse_down(&mut self, button: MouseButton) -> enigo::ResultType {
            handle_mouse(true, button, self.conn.clone(), &self.session);
            Ok(())
        }
        fn mouse_up(&mut self, button: MouseButton) {
            handle_mouse(false, button, self.conn.clone(), &self.session);
        }
        fn mouse_click(&mut self, button: MouseButton) {
            handle_mouse(true, button, self.conn.clone(), &self.session);
            handle_mouse(false, button, self.conn.clone(), &self.session);
        }
        fn mouse_scroll_x(&mut self, length: i32) {
            let portal = get_portal(&self.conn);
            let _ = remote_desktop_portal::notify_pointer_axis(
                &portal,
                &self.session,
                HashMap::new(),
                length as f64,
                0 as f64,
            );
        }
        fn mouse_scroll_y(&mut self, length: i32) {
            let length = if *PORTAL_INVERTS_VERTICAL_SCROLL {
                -length
            } else {
                length
            };
            let portal = get_portal(&self.conn);
            let _ = remote_desktop_portal::notify_pointer_axis(
                &portal,
                &self.session,
                HashMap::new(),
                0 as f64,
                length as f64,
            );
        }
    }

    fn send_keysym(
        keysym: i32,
        down: bool,
        conn: Arc<SyncConnection>,
        session: &Path<'static>,
    ) -> ResultType<()> {
        let state: u32 = if down {
            PRESSED_DOWN_STATE
        } else {
            PRESSED_UP_STATE
        };
        let portal = get_portal(&conn);
        log::trace!(
            "send_keysym: calling notify_keyboard_keysym, keysym={:#x}, state={}",
            keysym,
            state
        );
        match remote_desktop_portal::notify_keyboard_keysym(
            &portal,
            session,
            HashMap::new(),
            keysym,
            state,
        ) {
            Ok(_) => {
                log::trace!("send_keysym: notify_keyboard_keysym succeeded");
                Ok(())
            }
            Err(e) => Err(e.into()),
        }
    }

    fn get_raw_evdev_keycode(key: u16) -> i32 {
        let mut key = key as i32 - 8;
        if key == 126 {
            key = 125;
        }
        key
    }

    fn handle_key(
        down: bool,
        key: Key,
        conn: Arc<SyncConnection>,
        session: &Path<'static>,
    ) -> ResultType<()> {
        let state: u32 = if down {
            PRESSED_DOWN_STATE
        } else {
            PRESSED_UP_STATE
        };
        let portal = get_portal(&conn);
        match key {
            Key::Raw(key) => {
                let key = get_raw_evdev_keycode(key);
                remote_desktop_portal::notify_keyboard_keycode(
                    &portal,
                    &session,
                    HashMap::new(),
                    key,
                    state,
                )?;
            }
            _ => {
                if let Ok((key, is_shift, is_altgr)) = map_key(&key) {
                    let mut modifier_keycodes = Vec::new();
                    if is_shift {
                        modifier_keycodes.push(evdev::Key::KEY_LEFTSHIFT.code() as i32);
                    }
                    if is_altgr {
                        modifier_keycodes.push(evdev::Key::KEY_RIGHTALT.code() as i32);
                    }
                    let release_modifiers = |pressed: &[i32]| {
                        for keycode in pressed.iter().rev() {
                            if let Err(e) = remote_desktop_portal::notify_keyboard_keycode(
                                &portal,
                                &session,
                                HashMap::new(),
                                *keycode,
                                PRESSED_UP_STATE,
                            ) {
                                log::warn!(
                                    "handle_key: best-effort modifier release failed: {:?}",
                                    e
                                );
                            }
                        }
                    };
                    if down {
                        for (i, keycode) in modifier_keycodes.iter().enumerate() {
                            if let Err(e) = remote_desktop_portal::notify_keyboard_keycode(
                                &portal,
                                &session,
                                HashMap::new(),
                                *keycode,
                                state,
                            ) {
                                log::error!("handle_key: failed to press modifier: {:?}", e);
                                release_modifiers(&modifier_keycodes[..i]);
                                return Err(e.into());
                            }
                        }
                        if let Err(e) = remote_desktop_portal::notify_keyboard_keycode(
                            &portal,
                            &session,
                            HashMap::new(),
                            key.code() as i32,
                            state,
                        ) {
                            log::error!("handle_key: failed to press key: {:?}", e);
                            release_modifiers(&modifier_keycodes);
                            return Err(e.into());
                        }
                    } else {
                        if let Err(e) = remote_desktop_portal::notify_keyboard_keycode(
                            &portal,
                            &session,
                            HashMap::new(),
                            key.code() as i32,
                            PRESSED_UP_STATE,
                        ) {
                            log::error!("handle_key: failed to release key: {:?}", e);
                            release_modifiers(&modifier_keycodes);
                            return Err(e.into());
                        }
                        for keycode in modifier_keycodes.iter().rev() {
                            if let Err(e) = remote_desktop_portal::notify_keyboard_keycode(
                                &portal,
                                &session,
                                HashMap::new(),
                                *keycode,
                                PRESSED_UP_STATE,
                            ) {
                                log::error!("handle_key: failed to release modifier: {:?}", e);
                                return Err(e.into());
                            }
                        }
                    }
                }
            }
        }
        Ok(())
    }

    fn handle_mouse(
        down: bool,
        button: MouseButton,
        conn: Arc<SyncConnection>,
        session: &Path<'static>,
    ) {
        let portal = get_portal(&conn);
        let but_key = match button {
            MouseButton::Left => EVDEV_MOUSE_LEFT,
            MouseButton::Right => EVDEV_MOUSE_RIGHT,
            MouseButton::Middle => EVDEV_MOUSE_MIDDLE,
            _ => {
                return;
            }
        };
        let state: u32 = if down {
            PRESSED_DOWN_STATE
        } else {
            PRESSED_UP_STATE
        };
        let _ = remote_desktop_portal::notify_pointer_button(
            &portal,
            &session,
            HashMap::new(),
            but_key,
            state,
        );
    }
}
