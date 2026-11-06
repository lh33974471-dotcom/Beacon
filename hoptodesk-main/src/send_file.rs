use hbb_common::{
    log,
    tokio::{
        self,
        io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
        net::windows::named_pipe::ServerOptions,
    },
};
use std::{
    ffi::OsStr,
    io::{Read, Write},
    os::windows::{ffi::OsStrExt, io::AsRawHandle, process::CommandExt},
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};
use windows::{
    core::HSTRING,
    ApplicationModel::Package,
    Foundation::Uri,
    Management::Deployment::{
        AddPackageOptions, PackageManager, RemovalOptions, StagePackageOptions,
    },
};
use winapi::{
    shared::{
        minwindef::{BOOL, LPARAM, TRUE},
        windef::HWND,
    },
    um::{
        winbase::GetNamedPipeServerProcessId,
        winuser::{
            EnumWindows, GetWindowThreadProcessId, IsWindowVisible, MessageBoxW,
            MB_ICONINFORMATION, MB_OK, MB_SETFOREGROUND, MB_TOPMOST,
        },
    },
};

const ERROR_PIPE_BUSY: i32 = 231;
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const MENU_DLL: &str = "send-menu.dll";
const MENU_PACKAGE: &str = "send-menu.msix";
const MENU_KEY: &str = r"SOFTWARE\SendMenu";
const MENU_CLSID: &str = env!("SEND_MENU_CLSID");

#[cfg(feature = "send-menu")]
const MENU_FILES: Option<(&[u8], &[u8], &str)> = Some((
    include_bytes!(concat!(env!("SEND_MENU_DIR"), "\\send-menu.dll")),
    include_bytes!(concat!(env!("SEND_MENU_DIR"), "\\send-menu.msix")),
    include_str!(concat!(env!("SEND_MENU_DIR"), "\\package-full-name.txt")),
));
#[cfg(not(feature = "send-menu"))]
const MENU_FILES: Option<(&[u8], &[u8], &str)> = None;

#[derive(PartialEq)]
pub enum Delivery {
    Sent,
    NoTarget,
    ViewerTooOld,
}

impl Delivery {
    fn reply(&self) -> &'static [u8; 3] {
        match self {
            Delivery::Sent => b"ok\n",
            Delivery::NoTarget => b"no\n",
            Delivery::ViewerTooOld => b"up\n",
        }
    }
}

fn pipe_name(pid: u32) -> String {
    format!(
        r"\\.\pipe\{}-send-file-{}",
        crate::get_app_name().replace(' ', "-"),
        pid
    )
}

fn is_windows_11() -> bool {
    hbb_common::platform::windows::is_windows_version_or_greater(10, 0, 22000, 0, 0)
}

fn native_tool(relative: &str) -> PathBuf {
    let root = PathBuf::from(std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".to_owned()));
    let sysnative = root.join("Sysnative").join(relative);
    if sysnative.exists() {
        sysnative
    } else {
        root.join("System32").join(relative)
    }
}

pub fn stop_menu_host() {
    Command::new(native_tool("taskkill.exe"))
        .args(["/F", "/IM", "dllhost.exe", "/FI"])
        .arg(format!("MODULES eq {}", MENU_DLL))
        .creation_flags(CREATE_NO_WINDOW)
        .status()
        .ok();
}

pub fn top_menu_registered(exe: &str) -> bool {
    let (Some((_, _, full_name)), Some(dir)) = (MENU_FILES, Path::new(exe).parent()) else {
        return false;
    };
    is_windows_11()
        && PackageManager::new().map_or(false, |manager| {
            staged_in(&manager, &HSTRING::from(full_name.trim()), dir)
        })
}

pub fn install_top_menu(exe: &str, title: &str) -> bool {
    let Some((dll, package, full_name)) = MENU_FILES else {
        return false;
    };
    if !is_windows_11() || std::env::var("PROCESSOR_ARCHITEW6432").as_deref() == Ok("ARM64") {
        return false;
    }
    match register_top_menu(exe, title, dll, package, full_name.trim()) {
        Ok(()) => true,
        Err(err) => {
            log::error!("top menu: {}", err);
            false
        }
    }
}

fn register_top_menu(
    exe: &str,
    title: &str,
    dll: &[u8],
    package: &[u8],
    full_name: &str,
) -> hbb_common::ResultType<()> {
    stop_menu_host();
    let dir = Path::new(exe)
        .parent()
        .ok_or_else(|| hbb_common::anyhow::anyhow!("no install folder for {}", exe))?;
    let (key, _) = winreg::RegKey::predef(winreg::enums::HKEY_LOCAL_MACHINE).create_subkey_with_flags(
        menu_key_path(),
        winreg::enums::KEY_WRITE | winreg::enums::KEY_WOW64_64KEY,
    )?;
    key.set_value("Exe", &exe)?;
    key.set_value("Title", &title)?;
    std::fs::remove_file(dir.join("send-menu.txt")).ok();
    std::fs::write(dir.join(MENU_DLL), dll)?;
    let msix = dir.join(MENU_PACKAGE);
    std::fs::write(&msix, package)?;
    let registered = provision_package(&msix, dir, full_name);
    std::fs::remove_file(&msix).ok();
    registered
}

fn file_uri(path: &Path) -> windows::core::Result<Uri> {
    let path = path.to_string_lossy().replace('\\', "/");
    Uri::CreateUri(&HSTRING::from(format!("file:///{}", path)))
}

fn stage_package(manager: &PackageManager, package: &Uri, location: &Uri) -> windows::core::Result<()> {
    let options = StagePackageOptions::new()?;
    options.SetExternalLocationUri(location)?;
    manager
        .StagePackageByUriAsync(package, &options)?
        .get()?
        .ExtendedErrorCode()?
        .ok()
}

fn staged_in(manager: &PackageManager, full_name: &HSTRING, dir: &Path) -> bool {
    manager
        .FindPackageByPackageFullName(full_name)
        .and_then(|package| package.MachineExternalPath())
        .map_or(false, |path| {
            path.to_string()
                .trim_end_matches('\\')
                .eq_ignore_ascii_case(&dir.to_string_lossy())
        })
}

fn provision_package(msix: &Path, dir: &Path, full_name: &str) -> hbb_common::ResultType<()> {
    let name = full_name.split('_').next().unwrap_or_default();
    let publisher = full_name.rsplit('_').next().unwrap_or_default();
    let family = HSTRING::from(format!("{}_{}", name, publisher));
    let full_name = HSTRING::from(full_name);
    let manager = PackageManager::new()?;
    let location = file_uri(&dir.join(""))?;
    let package = file_uri(msix)?;
    stage_package(&manager, &package, &location)?;
    if !staged_in(&manager, &full_name, dir) {
        hbb_common::bail!("the package is staged for another folder");
    }
    manager
        .ProvisionPackageForAllUsersAsync(&family)?
        .get()?
        .ExtendedErrorCode()?
        .ok()?;
    if crate::platform::is_root() {
        return Ok(());
    }
    let add = AddPackageOptions::new()?;
    add.SetExternalLocationUri(&location)?;
    manager
        .AddPackageByUriAsync(&package, &add)?
        .get()?
        .ExtendedErrorCode()?
        .ok()?;
    Ok(())
}

fn is_named(package: &Package, name: &str) -> bool {
    package
        .Id()
        .and_then(|id| id.Name())
        .map_or(false, |found| found == name)
}

fn deprovision_packages(manager: &PackageManager, name: &str) -> windows::core::Result<()> {
    for package in manager.FindProvisionedPackages()? {
        if is_named(&package, name) {
            manager
                .DeprovisionPackageForAllUsersAsync(&package.Id()?.FamilyName()?)?
                .get()?
                .ExtendedErrorCode()?
                .ok()?;
        }
    }
    Ok(())
}

fn remove_packages(manager: &PackageManager, name: &str) -> windows::core::Result<()> {
    for package in manager.FindPackages()? {
        if is_named(&package, name) {
            manager
                .RemovePackageWithOptionsAsync(
                    &package.Id()?.FullName()?,
                    RemovalOptions::RemoveForAllUsers,
                )?
                .get()?
                .ExtendedErrorCode()?
                .ok()?;
        }
    }
    Ok(())
}

fn menu_key_path() -> String {
    format!("{}\\{}", MENU_KEY, MENU_CLSID)
}

pub fn remove_top_menu() -> hbb_common::ResultType<()> {
    let Some((_, _, full_name)) = MENU_FILES else {
        return Ok(());
    };
    let name = full_name.trim().split('_').next().unwrap_or_default();
    let manager = PackageManager::new()?;
    let deprovisioned = deprovision_packages(&manager, name);
    let removed = remove_packages(&manager, name);
    stop_menu_host();
    let hklm = winreg::RegKey::predef(winreg::enums::HKEY_LOCAL_MACHINE);
    hklm.delete_subkey_with_flags(menu_key_path(), winreg::enums::KEY_WOW64_64KEY)
        .ok();
    hklm.delete_subkey_with_flags(MENU_KEY, winreg::enums::KEY_WOW64_64KEY)
        .ok();
    deprovisioned.and(removed)?;
    Ok(())
}

pub fn top_menu_remove_cmds() -> String {
    if MENU_FILES.is_none() || !is_windows_11() {
        return String::new();
    }
    std::env::current_exe().map_or(String::new(), |exe| {
        format!("\"{}\" --uninstall-send-menu", exe.display())
    })
}

pub fn listen(on_path: impl Fn(String) -> Delivery + Send + 'static) {
    std::thread::spawn(move || {
        match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(rt) => rt.block_on(serve(on_path)),
            Err(err) => log::error!("send-file listener: {}", err),
        }
    });
}

async fn serve(on_path: impl Fn(String) -> Delivery) {
    let name = pipe_name(std::process::id());
    let mut server = match ServerOptions::new().first_pipe_instance(true).create(&name) {
        Ok(server) => server,
        Err(err) => {
            log::error!("send-file listener: {}", err);
            return;
        }
    };
    loop {
        if let Err(err) = server.connect().await {
            log::error!("send-file listener: {}", err);
            return;
        }
        let next = match ServerOptions::new().create(&name) {
            Ok(next) => next,
            Err(err) => {
                log::error!("send-file listener: {}", err);
                return;
            }
        };
        let mut reader = BufReader::new(std::mem::replace(&mut server, next)).take(4096);
        let mut line = String::new();
        let read = tokio::time::timeout(Duration::from_secs(2), reader.read_line(&mut line)).await;
        let path = line.trim_end_matches(['\r', '\n']);
        let valid = matches!(read, Ok(Ok(n)) if n > 0)
            && Path::new(path).is_absolute()
            && Path::new(path).exists();
        let delivery = if valid {
            on_path(path.to_owned())
        } else {
            Delivery::NoTarget
        };
        let mut client = reader.into_inner().into_inner();
        client.write_all(delivery.reply()).await.ok();
    }
}

pub fn send(paths: Vec<String>) {
    let paths: Vec<String> = paths.into_iter().filter(|p| !p.is_empty()).collect();
    if paths.is_empty() {
        return;
    }
    let me = std::process::id();
    for pid in window_pids().into_iter().filter(|pid| *pid != me) {
        let first = send_to(pid, &paths[0]);
        if first == Delivery::NoTarget {
            continue;
        }
        for path in &paths[1..] {
            if send_to(pid, path) != Delivery::Sent {
                log::error!("send-file: {} was not sent", path);
            }
        }
        if first == Delivery::ViewerTooOld {
            message_box(&crate::lang::translate(
                "The connected app does not support this. Update it.".to_owned(),
            ));
        }
        return;
    }
    message_box(&crate::lang::translate(
        "No connected device. Start a remote session, then try again.".to_owned(),
    ));
}

fn send_to(pid: u32, path: &str) -> Delivery {
    for _ in 0..40 {
        match std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(pipe_name(pid))
        {
            Ok(mut pipe) => {
                let mut server_pid = 0;
                let ok = unsafe {
                    GetNamedPipeServerProcessId(pipe.as_raw_handle() as _, &mut server_pid)
                };
                if ok == 0 || server_pid != pid {
                    return Delivery::NoTarget;
                }
                let mut reply = [0u8; 3];
                if writeln!(pipe, "{}", path).is_err() || pipe.read_exact(&mut reply).is_err() {
                    return Delivery::NoTarget;
                }
                return match &reply {
                    b"ok\n" => Delivery::Sent,
                    b"up\n" => Delivery::ViewerTooOld,
                    _ => Delivery::NoTarget,
                };
            }
            Err(err) if err.raw_os_error() == Some(ERROR_PIPE_BUSY) => {
                std::thread::sleep(Duration::from_millis(50))
            }
            Err(_) => return Delivery::NoTarget,
        }
    }
    Delivery::NoTarget
}

fn window_pids() -> Vec<u32> {
    let mut windows: Vec<(bool, u32)> = Vec::new();
    unsafe { EnumWindows(Some(collect_window), &mut windows as *mut Vec<(bool, u32)> as LPARAM) };
    windows.sort_by_key(|(visible, _)| !*visible);
    let mut pids: Vec<u32> = Vec::new();
    for (_, pid) in windows {
        if !pids.contains(&pid) {
            pids.push(pid);
        }
    }
    pids
}

unsafe extern "system" fn collect_window(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let windows = &mut *(lparam as *mut Vec<(bool, u32)>);
    let mut pid = 0;
    GetWindowThreadProcessId(hwnd, &mut pid);
    if pid != 0 {
        windows.push((IsWindowVisible(hwnd) != 0, pid));
    }
    TRUE
}

fn message_box(text: &str) {
    let wide = |s: &str| OsStr::new(s).encode_wide().chain(Some(0)).collect::<Vec<u16>>();
    unsafe {
        MessageBoxW(
            std::ptr::null_mut(),
            wide(text).as_ptr(),
            wide(&crate::get_app_name()).as_ptr(),
            MB_OK | MB_ICONINFORMATION | MB_SETFOREGROUND | MB_TOPMOST,
        )
    };
}
