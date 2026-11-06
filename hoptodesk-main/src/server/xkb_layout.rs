use hbb_common::{lazy_static, log};
use std::{collections::HashMap, ffi::CString, fs, ptr};
use xkbcommon_dl::{
    xkb_context_flags, xkb_keymap_compile_flags, xkb_keysym_t, xkb_rule_names, xkbcommon_option,
};

const EVDEV_OFFSET: u32 = 8;
const MAX_LEVELS: u32 = 4;

#[derive(Clone, Copy)]
pub struct LayoutKey {
    pub key: evdev::Key,
    pub shift: bool,
    pub altgr: bool,
}

lazy_static::lazy_static! {
    pub static ref LAYOUT_MAP: Option<HashMap<char, LayoutKey>> = build_layout_map();
}

fn parse_env_value(content: &str, wanted: &str) -> Option<String> {
    for line in content.lines() {
        let line = line.trim();
        if line.starts_with('#') {
            continue;
        }
        if let Some((key, value)) = line.split_once('=') {
            if key.trim() == wanted {
                let value = value.trim().trim_matches('"').trim_matches('\'');
                if !value.is_empty() {
                    return Some(value.to_owned());
                }
            }
        }
    }
    None
}

fn parse_xorg_option(content: &str, wanted: &str) -> Option<String> {
    for line in content.lines() {
        let parts: Vec<&str> = line.split('"').collect();
        if parts.len() >= 4
            && parts[0].trim().eq_ignore_ascii_case("Option")
            && parts[1] == wanted
            && !parts[3].is_empty()
        {
            return Some(parts[3].to_owned());
        }
    }
    None
}

fn system_layout() -> (Option<String>, Option<String>) {
    for path in ["/etc/default/keyboard", "/etc/vconsole.conf"] {
        if let Ok(content) = fs::read_to_string(path) {
            if let Some(layout) = parse_env_value(&content, "XKBLAYOUT") {
                return (Some(layout), parse_env_value(&content, "XKBVARIANT"));
            }
        }
    }
    for path in [
        "/etc/X11/xorg.conf.d/00-keyboard.conf",
        "/usr/share/X11/xorg.conf.d/00-keyboard.conf",
    ] {
        if let Ok(content) = fs::read_to_string(path) {
            if let Some(layout) = parse_xorg_option(&content, "XkbLayout") {
                return (Some(layout), parse_xorg_option(&content, "XkbVariant"));
            }
        }
    }
    (None, None)
}

fn build_layout_map() -> Option<HashMap<char, LayoutKey>> {
    let xkb = xkbcommon_option()?;
    let (layout, variant) = system_layout();
    log::info!(
        "xkb_layout: building layout map, layout={:?} variant={:?}",
        layout,
        variant
    );
    let layout_c = layout.and_then(|s| CString::new(s).ok());
    let variant_c = variant.and_then(|s| CString::new(s).ok());
    let names = xkb_rule_names {
        rules: ptr::null(),
        model: ptr::null(),
        layout: layout_c.as_ref().map_or(ptr::null(), |s| s.as_ptr()),
        variant: variant_c.as_ref().map_or(ptr::null(), |s| s.as_ptr()),
        options: ptr::null(),
    };
    unsafe {
        let context = (xkb.xkb_context_new)(xkb_context_flags::XKB_CONTEXT_NO_FLAGS);
        if context.is_null() {
            return None;
        }
        let keymap = (xkb.xkb_keymap_new_from_names)(
            context,
            &names,
            xkb_keymap_compile_flags::XKB_KEYMAP_COMPILE_NO_FLAGS,
        );
        if keymap.is_null() {
            (xkb.xkb_context_unref)(context);
            return None;
        }
        let mut map: HashMap<char, LayoutKey> = HashMap::new();
        let min_kc = (xkb.xkb_keymap_min_keycode)(keymap).max(EVDEV_OFFSET);
        let max_kc = (xkb.xkb_keymap_max_keycode)(keymap)
            .min(EVDEV_OFFSET + evdev::Key::BTN_TRIGGER_HAPPY40.code() as u32);
        for level in 0..MAX_LEVELS {
            for kc in min_kc..=max_kc {
                if level >= (xkb.xkb_keymap_num_levels_for_key)(keymap, kc, 0) {
                    continue;
                }
                let mut syms: *const xkb_keysym_t = ptr::null();
                if (xkb.xkb_keymap_key_get_syms_by_level)(keymap, kc, 0, level, &mut syms) != 1 {
                    continue;
                }
                let utf32 = (xkb.xkb_keysym_to_utf32)(*syms);
                if utf32 < 0x20 || utf32 == 0x7f {
                    continue;
                }
                let c = match char::from_u32(utf32) {
                    Some(c) => c,
                    None => continue,
                };
                let key = evdev::Key::new((kc - EVDEV_OFFSET) as u16);
                let key_name = format!("{:?}", &key);
                if key_name.contains("unknown key") || key_name.starts_with("KEY_KP") {
                    continue;
                }
                map.entry(c).or_insert(LayoutKey {
                    key,
                    shift: level & 1 != 0,
                    altgr: level & 2 != 0,
                });
            }
        }
        (xkb.xkb_keymap_unref)(keymap);
        (xkb.xkb_context_unref)(context);
        if map.is_empty() {
            log::warn!("xkb_layout: layout map is empty, falling back to static table");
            None
        } else {
            log::info!("xkb_layout: layout map built with {} chars", map.len());
            Some(map)
        }
    }
}
