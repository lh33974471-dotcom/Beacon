#![no_std]
#![allow(non_snake_case, non_camel_case_types)]

use core::{
    ffi::c_void,
    ptr,
    sync::atomic::{AtomicU32, Ordering},
};
use windows_sys::{
    core::{IID_IUnknown, IUnknown_Vtbl, BOOL, GUID, HRESULT, PCWSTR, PWSTR},
    Win32::{
        Foundation::{
            CloseHandle, CLASS_E_NOAGGREGATION, ERROR_SUCCESS, E_FAIL, E_NOINTERFACE, E_NOTIMPL,
            E_OUTOFMEMORY, E_POINTER, HINSTANCE, S_FALSE, S_OK,
        },
        System::{
            Com::{CoTaskMemAlloc, CoTaskMemFree},
            Memory::{GetProcessHeap, HeapAlloc, HeapFree},
            Registry::{RegGetValueW, HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ},
            Threading::{CreateProcessW, ExitProcess, PROCESS_INFORMATION, STARTUPINFOW},
        },
        UI::Shell::{ECF_DEFAULT, ECS_ENABLED, SIGDN_FILESYSPATH},
    },
};

const IID_ICLASSFACTORY: GUID = GUID::from_u128(0x00000001_0000_0000_c000_000000000046);
const IID_IEXPLORERCOMMAND: GUID = GUID::from_u128(0xa08ce4d0_fa25_44ab_b57c_c7b1c323e0b9);

macro_rules! wide {
    ($text:literal) => {{
        const BYTES: &[u8] = $text.as_bytes();
        const LEN: usize = BYTES.len();
        const OUT: [u16; LEN] = utf16(BYTES);
        &OUT
    }};
}

const fn utf16<const N: usize>(bytes: &[u8]) -> [u16; N] {
    let mut out = [0u16; N];
    let mut i = 0;
    while i < N {
        out[i] = bytes[i] as u16;
        i += 1;
    }
    out
}

const KEY_PREFIX: &[u16] = wide!("SOFTWARE\\SendMenu\\");
const VALUE_EXE: &[u16] = wide!("Exe\0");
const VALUE_TITLE: &[u16] = wide!("Title\0");
const ARG: &[u16] = wide!("\" --send-file \"");
const ICON_SUFFIX: &[u16] = wide!(",0");

static OBJECTS: AtomicU32 = AtomicU32::new(0);

#[repr(C)]
struct IShellItemArray_Vtbl {
    base: IUnknown_Vtbl,
    BindToHandler: usize,
    GetPropertyStore: usize,
    GetPropertyDescriptionList: usize,
    GetAttributes: usize,
    GetCount: unsafe extern "system" fn(*mut c_void, *mut u32) -> HRESULT,
    GetItemAt: unsafe extern "system" fn(*mut c_void, u32, *mut *mut c_void) -> HRESULT,
}

#[repr(C)]
struct IShellItem_Vtbl {
    base: IUnknown_Vtbl,
    BindToHandler: usize,
    GetParent: usize,
    GetDisplayName: unsafe extern "system" fn(*mut c_void, i32, *mut PWSTR) -> HRESULT,
}

#[repr(C)]
struct IExplorerCommand_Vtbl {
    base: IUnknown_Vtbl,
    GetTitle: unsafe extern "system" fn(*mut c_void, *mut c_void, *mut PWSTR) -> HRESULT,
    GetIcon: unsafe extern "system" fn(*mut c_void, *mut c_void, *mut PWSTR) -> HRESULT,
    GetToolTip: unsafe extern "system" fn(*mut c_void, *mut c_void, *mut PWSTR) -> HRESULT,
    GetCanonicalName: unsafe extern "system" fn(*mut c_void, *mut GUID) -> HRESULT,
    GetState: unsafe extern "system" fn(*mut c_void, *mut c_void, BOOL, *mut u32) -> HRESULT,
    Invoke: unsafe extern "system" fn(*mut c_void, *mut c_void, *mut c_void) -> HRESULT,
    GetFlags: unsafe extern "system" fn(*mut c_void, *mut u32) -> HRESULT,
    EnumSubCommands: unsafe extern "system" fn(*mut c_void, *mut *mut c_void) -> HRESULT,
}

#[repr(C)]
struct IClassFactory_Vtbl {
    base: IUnknown_Vtbl,
    CreateInstance: unsafe extern "system" fn(
        *mut c_void,
        *mut c_void,
        *const GUID,
        *mut *mut c_void,
    ) -> HRESULT,
    LockServer: unsafe extern "system" fn(*mut c_void, BOOL) -> HRESULT,
}

#[repr(C)]
struct Object<V: 'static> {
    vtbl: &'static V,
    refs: AtomicU32,
    clsid: GUID,
}

unsafe fn vtbl<'a, V>(this: *mut c_void) -> &'a V {
    &**(this as *mut *mut V)
}

unsafe fn heap_alloc(bytes: usize) -> *mut c_void {
    HeapAlloc(GetProcessHeap(), 0, bytes)
}

unsafe fn heap_free(block: *const c_void) {
    HeapFree(GetProcessHeap(), 0, block);
}

unsafe fn new_object<V>(
    table: &'static V,
    clsid: GUID,
    iid: *const GUID,
    own_iid: &GUID,
    out: *mut *mut c_void,
) -> HRESULT {
    if out.is_null() {
        return E_POINTER;
    }
    *out = ptr::null_mut();
    if !guid_eq(iid, &IID_IUnknown) && !guid_eq(iid, own_iid) {
        return E_NOINTERFACE;
    }
    let object = heap_alloc(core::mem::size_of::<Object<V>>()) as *mut Object<V>;
    if object.is_null() {
        return E_OUTOFMEMORY;
    }
    ptr::write(
        object,
        Object {
            vtbl: table,
            refs: AtomicU32::new(1),
            clsid,
        },
    );
    OBJECTS.fetch_add(1, Ordering::Relaxed);
    *out = object as *mut c_void;
    S_OK
}

unsafe fn guid_eq(a: *const GUID, b: &GUID) -> bool {
    let a = &*a;
    a.data1 == b.data1 && a.data2 == b.data2 && a.data3 == b.data3 && a.data4 == b.data4
}

unsafe fn query<V>(
    this: *mut c_void,
    own_iid: &GUID,
    iid: *const GUID,
    out: *mut *mut c_void,
) -> HRESULT {
    if out.is_null() {
        return E_POINTER;
    }
    if guid_eq(iid, &IID_IUnknown) || guid_eq(iid, own_iid) {
        add_ref::<V>(this);
        *out = this;
        S_OK
    } else {
        *out = ptr::null_mut();
        E_NOINTERFACE
    }
}

unsafe extern "system" fn add_ref<V>(this: *mut c_void) -> u32 {
    (*(this as *mut Object<V>))
        .refs
        .fetch_add(1, Ordering::Relaxed)
        + 1
}

unsafe extern "system" fn release<V>(this: *mut c_void) -> u32 {
    let left = (*(this as *mut Object<V>))
        .refs
        .fetch_sub(1, Ordering::Release)
        - 1;
    if left == 0 {
        heap_free(this);
        OBJECTS.fetch_sub(1, Ordering::Relaxed);
    }
    left
}

unsafe fn clsid_of<V>(this: *mut c_void) -> &'static GUID {
    &(*(this as *mut Object<V>)).clsid
}

struct Key([u16; 57]);

fn key_path(clsid: &GUID) -> Key {
    let mut key = [0u16; 57];
    key[..KEY_PREFIX.len()].copy_from_slice(KEY_PREFIX);
    let mut bytes = [0u8; 16];
    bytes[..4].copy_from_slice(&clsid.data1.to_be_bytes());
    bytes[4..6].copy_from_slice(&clsid.data2.to_be_bytes());
    bytes[6..8].copy_from_slice(&clsid.data3.to_be_bytes());
    bytes[8..].copy_from_slice(&clsid.data4);
    let mut pos = KEY_PREFIX.len();
    key[pos] = b'{' as u16;
    pos += 1;
    for (index, byte) in bytes.iter().enumerate() {
        if matches!(index, 4 | 6 | 8 | 10) {
            key[pos] = b'-' as u16;
            pos += 1;
        }
        key[pos] = HEX[(byte >> 4) as usize] as u16;
        key[pos + 1] = HEX[(byte & 15) as usize] as u16;
        pos += 2;
    }
    key[pos] = b'}' as u16;
    Key(key)
}

const HEX: &[u8; 16] = b"0123456789ABCDEF";

struct Value {
    data: *mut u16,
    len: usize,
}

impl Value {
    unsafe fn read(clsid: &GUID, name: &[u16]) -> Option<Value> {
        let key = key_path(clsid);
        let mut bytes = 0u32;
        if RegGetValueW(
            HKEY_LOCAL_MACHINE,
            key.0.as_ptr(),
            name.as_ptr(),
            RRF_RT_REG_SZ,
            ptr::null_mut(),
            ptr::null_mut(),
            &mut bytes,
        ) != ERROR_SUCCESS
        {
            return None;
        }
        let data = heap_alloc(bytes as usize) as *mut u16;
        if data.is_null() {
            return None;
        }
        if RegGetValueW(
            HKEY_LOCAL_MACHINE,
            key.0.as_ptr(),
            name.as_ptr(),
            RRF_RT_REG_SZ,
            ptr::null_mut(),
            data as *mut c_void,
            &mut bytes,
        ) != ERROR_SUCCESS
        {
            heap_free(data as *const c_void);
            return None;
        }
        let mut len = bytes as usize / 2;
        while len > 0 && *data.add(len - 1) == 0 {
            len -= 1;
        }
        Some(Value { data, len })
    }

    fn chars(&self) -> &[u16] {
        unsafe { core::slice::from_raw_parts(self.data, self.len) }
    }
}

impl Drop for Value {
    fn drop(&mut self) {
        unsafe { heap_free(self.data as *const c_void) }
    }
}

unsafe fn join(parts: &[&[u16]], alloc: unsafe fn(usize) -> *mut c_void) -> *mut u16 {
    let len: usize = parts.iter().map(|part| part.len()).sum::<usize>() + 1;
    let out = alloc(len * 2) as *mut u16;
    if out.is_null() {
        return out;
    }
    let mut pos = 0;
    for part in parts {
        ptr::copy_nonoverlapping(part.as_ptr(), out.add(pos), part.len());
        pos += part.len();
    }
    *out.add(pos) = 0;
    out
}

unsafe fn co_alloc(bytes: usize) -> *mut c_void {
    CoTaskMemAlloc(bytes)
}

unsafe fn shell_string(parts: &[&[u16]], out: *mut PWSTR) -> HRESULT {
    if out.is_null() {
        return E_POINTER;
    }
    *out = join(parts, co_alloc);
    if (*out).is_null() {
        E_OUTOFMEMORY
    } else {
        S_OK
    }
}

unsafe fn wide_len(text: PCWSTR) -> usize {
    let mut len = 0;
    while *text.add(len) != 0 {
        len += 1;
    }
    len
}

unsafe fn run(exe: &[u16], path: &[u16]) {
    let quote: &[u16] = &[b'"' as u16];
    let escape: &[u16] = if path.last() == Some(&(b'\\' as u16)) {
        &[b'\\' as u16]
    } else {
        &[]
    };
    let command = join(&[quote, exe, ARG, path, escape, quote], heap_alloc);
    if command.is_null() {
        return;
    }
    let mut startup: STARTUPINFOW = core::mem::zeroed();
    startup.cb = core::mem::size_of::<STARTUPINFOW>() as u32;
    let mut process: PROCESS_INFORMATION = core::mem::zeroed();
    if CreateProcessW(
        ptr::null(),
        command,
        ptr::null(),
        ptr::null(),
        0,
        0,
        ptr::null(),
        ptr::null(),
        &startup,
        &mut process,
    ) != 0
    {
        CloseHandle(process.hThread);
        CloseHandle(process.hProcess);
    }
    heap_free(command as *const c_void);
}

unsafe extern "system" fn command_query(
    this: *mut c_void,
    iid: *const GUID,
    out: *mut *mut c_void,
) -> HRESULT {
    query::<IExplorerCommand_Vtbl>(this, &IID_IEXPLORERCOMMAND, iid, out)
}

unsafe extern "system" fn get_title(
    this: *mut c_void,
    _items: *mut c_void,
    out: *mut PWSTR,
) -> HRESULT {
    match Value::read(clsid_of::<IExplorerCommand_Vtbl>(this), VALUE_TITLE) {
        Some(title) => shell_string(&[title.chars()], out),
        None => E_FAIL,
    }
}

unsafe extern "system" fn get_icon(
    this: *mut c_void,
    _items: *mut c_void,
    out: *mut PWSTR,
) -> HRESULT {
    match Value::read(clsid_of::<IExplorerCommand_Vtbl>(this), VALUE_EXE) {
        Some(exe) => shell_string(&[exe.chars(), ICON_SUFFIX], out),
        None => E_FAIL,
    }
}

unsafe extern "system" fn not_implemented_string(
    _this: *mut c_void,
    _items: *mut c_void,
    out: *mut PWSTR,
) -> HRESULT {
    if !out.is_null() {
        *out = ptr::null_mut();
    }
    E_NOTIMPL
}

unsafe extern "system" fn get_canonical_name(_this: *mut c_void, _out: *mut GUID) -> HRESULT {
    E_NOTIMPL
}

unsafe extern "system" fn get_state(
    _this: *mut c_void,
    _items: *mut c_void,
    _slow: BOOL,
    out: *mut u32,
) -> HRESULT {
    if out.is_null() {
        return E_POINTER;
    }
    *out = ECS_ENABLED as u32;
    S_OK
}

unsafe extern "system" fn invoke(
    this: *mut c_void,
    items: *mut c_void,
    _context: *mut c_void,
) -> HRESULT {
    if items.is_null() {
        return E_POINTER;
    }
    let Some(exe) = Value::read(clsid_of::<IExplorerCommand_Vtbl>(this), VALUE_EXE) else {
        return E_FAIL;
    };
    let array: &IShellItemArray_Vtbl = vtbl(items);
    let mut count = 0u32;
    if (array.GetCount)(items, &mut count) < 0 {
        return E_FAIL;
    }
    for index in 0..count {
        let mut item: *mut c_void = ptr::null_mut();
        if (array.GetItemAt)(items, index, &mut item) < 0 || item.is_null() {
            continue;
        }
        let shell_item: &IShellItem_Vtbl = vtbl(item);
        let mut name: PWSTR = ptr::null_mut();
        if (shell_item.GetDisplayName)(item, SIGDN_FILESYSPATH, &mut name) >= 0 && !name.is_null() {
            run(
                exe.chars(),
                core::slice::from_raw_parts(name, wide_len(name)),
            );
            CoTaskMemFree(name as *const c_void);
        }
        (shell_item.base.Release)(item);
    }
    S_OK
}

unsafe extern "system" fn get_flags(_this: *mut c_void, out: *mut u32) -> HRESULT {
    if out.is_null() {
        return E_POINTER;
    }
    *out = ECF_DEFAULT as u32;
    S_OK
}

unsafe extern "system" fn enum_sub_commands(_this: *mut c_void, out: *mut *mut c_void) -> HRESULT {
    if !out.is_null() {
        *out = ptr::null_mut();
    }
    E_NOTIMPL
}

static COMMAND_VTBL: IExplorerCommand_Vtbl = IExplorerCommand_Vtbl {
    base: IUnknown_Vtbl {
        QueryInterface: command_query,
        AddRef: add_ref::<IExplorerCommand_Vtbl>,
        Release: release::<IExplorerCommand_Vtbl>,
    },
    GetTitle: get_title,
    GetIcon: get_icon,
    GetToolTip: not_implemented_string,
    GetCanonicalName: get_canonical_name,
    GetState: get_state,
    Invoke: invoke,
    GetFlags: get_flags,
    EnumSubCommands: enum_sub_commands,
};

unsafe extern "system" fn factory_query(
    this: *mut c_void,
    iid: *const GUID,
    out: *mut *mut c_void,
) -> HRESULT {
    query::<IClassFactory_Vtbl>(this, &IID_ICLASSFACTORY, iid, out)
}

unsafe extern "system" fn create_instance(
    this: *mut c_void,
    outer: *mut c_void,
    iid: *const GUID,
    out: *mut *mut c_void,
) -> HRESULT {
    if !outer.is_null() {
        return CLASS_E_NOAGGREGATION;
    }
    new_object(
        &COMMAND_VTBL,
        *clsid_of::<IClassFactory_Vtbl>(this),
        iid,
        &IID_IEXPLORERCOMMAND,
        out,
    )
}

unsafe extern "system" fn lock_server(_this: *mut c_void, _lock: BOOL) -> HRESULT {
    S_OK
}

static FACTORY_VTBL: IClassFactory_Vtbl = IClassFactory_Vtbl {
    base: IUnknown_Vtbl {
        QueryInterface: factory_query,
        AddRef: add_ref::<IClassFactory_Vtbl>,
        Release: release::<IClassFactory_Vtbl>,
    },
    CreateInstance: create_instance,
    LockServer: lock_server,
};

#[no_mangle]
unsafe extern "system" fn DllGetClassObject(
    clsid: *const GUID,
    iid: *const GUID,
    out: *mut *mut c_void,
) -> HRESULT {
    if clsid.is_null() {
        return E_POINTER;
    }
    new_object(&FACTORY_VTBL, *clsid, iid, &IID_ICLASSFACTORY, out)
}

#[no_mangle]
extern "system" fn DllCanUnloadNow() -> HRESULT {
    if OBJECTS.load(Ordering::Acquire) == 0 {
        S_OK
    } else {
        S_FALSE
    }
}

#[no_mangle]
extern "system" fn DllMain(_module: HINSTANCE, _reason: u32, _reserved: *mut c_void) -> BOOL {
    1
}

#[no_mangle]
unsafe extern "C" fn memcpy(dst: *mut u8, src: *const u8, len: usize) -> *mut u8 {
    for i in 0..len {
        ptr::write_volatile(dst.add(i), ptr::read_volatile(src.add(i)));
    }
    dst
}

#[no_mangle]
unsafe extern "C" fn memmove(dst: *mut u8, src: *const u8, len: usize) -> *mut u8 {
    if (dst as usize) <= (src as usize) {
        return memcpy(dst, src, len);
    }
    for i in (0..len).rev() {
        ptr::write_volatile(dst.add(i), ptr::read_volatile(src.add(i)));
    }
    dst
}

#[no_mangle]
unsafe extern "C" fn memset(dst: *mut u8, value: i32, len: usize) -> *mut u8 {
    for i in 0..len {
        ptr::write_volatile(dst.add(i), value as u8);
    }
    dst
}

#[no_mangle]
unsafe extern "C" fn memcmp(a: *const u8, b: *const u8, len: usize) -> i32 {
    for i in 0..len {
        let (x, y) = (ptr::read_volatile(a.add(i)), ptr::read_volatile(b.add(i)));
        if x != y {
            return x as i32 - y as i32;
        }
    }
    0
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    unsafe { ExitProcess(3) }
}
