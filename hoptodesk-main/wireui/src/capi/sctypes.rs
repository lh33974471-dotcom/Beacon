#![allow(non_camel_case_types, non_snake_case)]

use std::os::raw::c_void;

#[repr(C)]
pub struct _HWINDOW {
    _unused: usize,
}
pub type HWINDOW = *mut _HWINDOW;

#[repr(C)]
pub struct _HGFX {
    _unused: usize,
}
pub type HGFX = *mut _HGFX;

#[repr(C)]
pub struct _HREQUEST {
    _unused: usize,
}
pub type HREQUEST = *mut _HREQUEST;

pub type VOID = c_void;
pub type LPVOID = *mut c_void;
pub type LPCVOID = *const c_void;
pub type UINT = u32;
pub type INT = i32;
pub type LONG = i32;
pub type UINT64 = u64;
pub type BOOL = i32;
pub type BYTE = u8;
pub type LPCBYTE = *const u8;
pub type LPCSTR = *const std::os::raw::c_char;
pub type LPCWSTR = *const u16;

#[repr(C)]
#[derive(Debug, Default, Copy, Clone, PartialEq)]
pub struct RECT {
    pub left: LONG,
    pub top: LONG,
    pub right: LONG,
    pub bottom: LONG,
}

impl RECT {
    pub fn size(&self) -> SIZE {
        SIZE {
            cx: self.right - self.left,
            cy: self.bottom - self.top,
        }
    }

    pub fn width(&self) -> LONG {
        self.right - self.left
    }

    pub fn height(&self) -> LONG {
        self.bottom - self.top
    }

    pub fn topleft(&self) -> POINT {
        POINT {
            x: self.left,
            y: self.top,
        }
    }
}

#[repr(C)]
#[derive(Debug, Default, Copy, Clone, PartialEq)]
pub struct SIZE {
    pub cx: LONG,
    pub cy: LONG,
}

#[repr(C)]
#[derive(Debug, Default, Copy, Clone, PartialEq)]
pub struct POINT {
    pub x: LONG,
    pub y: LONG,
}
