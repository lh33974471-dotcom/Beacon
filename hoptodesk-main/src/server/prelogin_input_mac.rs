#[repr(C)]
#[derive(Clone, Copy)]
struct CGPoint {
    x: f64,
    y: f64,
}

type CGError = i32;

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGPostMouseEvent(
        mouse_cursor_position: CGPoint,
        update_mouse_cursor_position: i32,
        button_count: u32,
        button1_down: i32,
        ...
    ) -> CGError;
    fn CGPostKeyboardEvent(key_char: u16, virtual_key: u16, key_down: i32) -> CGError;
    fn CGWarpMouseCursorPosition(new_cursor_position: CGPoint) -> CGError;
}

fn b(v: bool) -> i32 {
    if v {
        1
    } else {
        0
    }
}

pub fn mouse(x: i32, y: i32, left_down: bool, right_down: bool, is_move: bool) {
    let pos = CGPoint {
        x: x as f64,
        y: y as f64,
    };
    unsafe {
        if is_move {
            CGWarpMouseCursorPosition(pos);
        }
        CGPostMouseEvent(pos, 1, 2, b(left_down), b(right_down));
    }
}

pub fn key(virtual_key: u16, down: bool) {
    unsafe {
        CGPostKeyboardEvent(0, virtual_key, b(down));
    }
}
