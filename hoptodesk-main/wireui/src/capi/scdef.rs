#![allow(non_camel_case_types, non_snake_case)]

use super::sctypes::{HWINDOW, LPCBYTE, LPCWSTR, LPVOID, UINT};

#[repr(C)]
#[derive(Debug, Copy, Clone, PartialEq)]
pub enum GFX_LAYER {
    AUTO = 0xFFFF,
    CPU = 1,
    WARP = 2,
    D2D = 3,
    SKIA_CPU = 4,
    SKIA_OPENGL = 5,
}

#[repr(C)]
#[derive(Debug, Copy, Clone, PartialEq)]
pub enum SCRIPT_RUNTIME_FEATURES {
    ALLOW_FILE_IO = 0x1,
    ALLOW_SOCKET_IO = 0x2,
    ALLOW_EVAL = 0x4,
    ALLOW_SYSINFO = 0x8,
}

#[repr(C)]
#[derive(Debug, Copy, Clone, PartialOrd, PartialEq)]
pub enum LOAD_RESULT {
    LOAD_DEFAULT = 0,
    LOAD_DISCARD = 1,
    LOAD_DELAYED = 2,
    LOAD_MYSELF = 3,
}

#[repr(C)]
#[derive(Debug, Copy, Clone, PartialOrd, PartialEq)]
pub enum OUTPUT_SUBSYTEMS {
    DOM = 0,
    CSSS,
    CSS,
    TIS,
}

#[repr(C)]
#[derive(Debug, Copy, Clone, PartialOrd, PartialEq)]
pub enum OUTPUT_SEVERITY {
    INFO = 0,
    WARNING,
    ERROR,
}

#[repr(C)]
pub struct SCN_LOAD_DATA {
    pub code: UINT,
    pub hwnd: HWINDOW,
    pub uri: LPCWSTR,
    pub outData: LPCBYTE,
    pub outDataSize: UINT,
    pub dataType: UINT,
    pub requestId: LPVOID,
    pub principal: super::scdom::HELEMENT,
    pub initiator: super::scdom::HELEMENT,
}

#[repr(C)]
pub struct SCN_DATA_LOADED {
    pub code: UINT,
    pub hwnd: HWINDOW,
    pub uri: LPCWSTR,
    pub data: LPCBYTE,
    pub dataSize: UINT,
    pub dataType: UINT,
    pub status: UINT,
}

#[repr(C)]
pub struct SCN_ATTACH_BEHAVIOR {
    pub code: UINT,
    pub hwnd: HWINDOW,
    pub element: super::scdom::HELEMENT,
    pub name: super::sctypes::LPCSTR,
    pub elementProc: LPVOID,
    pub elementTag: LPVOID,
}

#[repr(C)]
pub struct SCN_INVALIDATE_RECT {
    pub code: UINT,
    pub hwnd: HWINDOW,
    pub invalidRect: super::sctypes::RECT,
}
