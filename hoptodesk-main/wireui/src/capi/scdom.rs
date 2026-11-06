#![allow(non_camel_case_types)]

#[repr(C)]
pub struct _HELEMENT {
    _unused: usize,
}
pub type HELEMENT = *mut _HELEMENT;

#[repr(C)]
#[derive(Debug, Copy, Clone, PartialOrd, PartialEq)]
pub enum SCDOM_RESULT {
    OK = 0,
    INVALID_HWND = 1,
    INVALID_HANDLE = 2,
    PASSIVE_HANDLE = 3,
    INVALID_PARAMETER = 4,
    OPERATION_FAILED = 5,
    OK_NOT_HANDLED = -1,
}

impl std::error::Error for SCDOM_RESULT {}

impl std::fmt::Display for SCDOM_RESULT {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "{:?}", self)
    }
}

#[repr(C)]
#[derive(Debug, Copy, Clone, PartialOrd, PartialEq)]
pub enum ELEMENT_AREAS {
    CONTENT_BOX = 0x00,
    ROOT_RELATIVE = 0x01,
    SELF_RELATIVE = 0x02,
    CONTAINER_RELATIVE = 0x03,
    VIEW_RELATIVE = 0x04,
    PADDING_BOX = 0x10,
    BORDER_BOX = 0x20,
    MARGIN_BOX = 0x30,
    BACK_IMAGE_AREA = 0x40,
    FORE_IMAGE_AREA = 0x50,
    SCROLLABLE_AREA = 0x60,
}
