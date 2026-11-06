use hbb_common::{platform::windows::is_windows_version_or_greater, ResultType};

pub const HTD_IDD_DEVICE_STRING: &'static str = "HTD Virtual Display\0";

const IDD_IMPL: &str = "htd_idd";

#[derive(Debug, Clone, Copy)]
pub struct MonitorMode {
    pub width: u32,
    pub height: u32,
    pub sync: u32,
}

pub fn get_cur_device_string() -> &'static str {
    HTD_IDD_DEVICE_STRING
}

pub fn is_virtual_display_supported() -> bool {
    #[cfg(target_os = "windows")]
    {
        is_windows_version_or_greater(10, 0, 17763, 0, 0)
    }
    #[cfg(not(target_os = "windows"))]
    {
        false
    }
}

pub fn plug_in_headless() -> ResultType<()> {
    htd_idd::plug_in_headless()
}

pub fn get_platform_additions() -> serde_json::Map<String, serde_json::Value> {
    let mut map = serde_json::Map::new();
    if !crate::platform::windows::is_self_service_running() {
        return map;
    }
    map.insert("idd_impl".into(), serde_json::json!(IDD_IMPL));
    let count = htd_idd::get_monitor_count();
    if count > 0 {
        map.insert("htd_virtual_displays".into(), serde_json::json!(count));
    }
    map
}

#[inline]
pub fn plug_in_monitor(_idx: u32, _modes: Vec<MonitorMode>) -> ResultType<()> {
    htd_idd::plug_in_monitor()
}

pub fn plug_out_monitor(_index: i32, _force_all: bool, _force_one: bool) -> ResultType<()> {
    htd_idd::plug_out_monitor()
}

pub fn plug_in_peer_request(_modes: Vec<Vec<MonitorMode>>) -> ResultType<Vec<u32>> {
    htd_idd::plug_in_monitor()?;
    Ok(vec![0])
}

pub fn plug_out_monitor_indices(
    _indices: &[u32],
    _force_all: bool,
    _force_one: bool,
) -> ResultType<()> {
    htd_idd::plug_out_monitor()
}

pub fn reset_all() -> ResultType<()> {
    htd_idd::reset_all()
}

pub mod htd_idd {
    use super::windows;
    use hbb_common::{bail, lazy_static, log, tokio::time::Instant, ResultType};
    use std::{
        os::windows::process::CommandExt,
        path::PathBuf,
        sync::{
            atomic::{AtomicBool, Ordering},
            Arc, Mutex,
        },
        time::Duration,
    };
    use winapi::um::wincrypt::{
        CertAddEncodedCertificateToStore, CertCloseStore, CertCreateCertificateContext,
        CertDeleteCertificateFromStore, CertFindCertificateInStore, CertFreeCertificateContext,
        CertOpenStore, HCERTSTORE, CERT_STORE_ADD_REPLACE_EXISTING, CERT_STORE_PROV_SYSTEM_W,
        CERT_SYSTEM_STORE_LOCAL_MACHINE, X509_ASN_ENCODING,
    };
    use ::windows::{
        core::PCWSTR,
        Win32::{
            Devices::Enumeration::Pnp::{
                SwDeviceClose, SwDeviceCreate, HSWDEVICE, SWDeviceCapabilitiesDriverRequired,
                SWDeviceCapabilitiesRemovable, SWDeviceCapabilitiesSilentInstall,
                SW_DEVICE_CREATE_INFO,
            },
            Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0},
            System::Threading::{CreateEventW, SetEvent, WaitForSingleObject},
        },
    };

    const HARDWARE_ID: &str = "HTDVDD";
    const ENUMERATOR_NAME: &str = "HTDVDD";
    const PARENT_DEVICE: &str = "HTREE\\ROOT\\0";
    const CONFIG_DIR: &str = "C:\\ProgramData\\HTDVDD";
    const INF_NAME: &str = "HtdVDD.inf";
    const CERT_FIND_EXISTING: u32 = 0x000D0000;
    const CREATE_NO_WINDOW: u32 = 0x08000000;
    const CREATE_TIMEOUT_MS: u32 = 10_000;
    const REAL_DISPLAY_POLL_INTERVAL: Duration = Duration::from_secs(3);

    static WATCHING_FOR_REAL_DISPLAY: AtomicBool = AtomicBool::new(false);
    static INF_BYTES: &[u8] = include_bytes!("../res/vdd/HtdVDD.inf");
    static DLL_BYTES: &[u8] = include_bytes!("../res/vdd/HtdVDD.dll");
    static CAT_BYTES: &[u8] = include_bytes!("../res/vdd/HtdVDD.cat");
    static SETTINGS_BYTES: &[u8] = include_bytes!("../res/vdd/vdd_settings.xml");
    static PUBLISHER_CER_BYTES: &[u8] = include_bytes!("../res/vdd/HtdVDD-publisher.cer");

    struct SwDevice(HSWDEVICE);
    unsafe impl Send for SwDevice {}

    lazy_static::lazy_static! {
        static ref LOCK: Arc<Mutex<()>> = Default::default();
        static ref LAST_PLUG_IN_HEADLESS_TIME: Arc<Mutex<Option<Instant>>> = Arc::new(Mutex::new(None));
        static ref SW_DEVICE: Arc<Mutex<Option<SwDevice>>> = Default::default();
    }

    fn wide_z(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn wide_zz(s: &str) -> Vec<u16> {
        s.encode_utf16().chain([0, 0]).collect()
    }

    fn stage_package() -> ResultType<PathBuf> {
        // Beacon LAN build: the proprietary HtdVDD driver binaries are not
        // bundled; virtual display (headless monitor) is unavailable.
        if crate::lan_only::LAN_ONLY {
            hbb_common::bail!("Virtual display driver not bundled in LAN build");
        }
        let dir = PathBuf::from(CONFIG_DIR);
        std::fs::create_dir_all(&dir)?;
        for (name, bytes) in [
            (INF_NAME, INF_BYTES),
            ("HtdVDD.dll", DLL_BYTES),
            ("HtdVDD.cat", CAT_BYTES),
        ] {
            std::fs::write(dir.join(name), bytes)?;
        }
        let settings = dir.join("vdd_settings.xml");
        if !settings.exists() {
            std::fs::write(&settings, SETTINGS_BYTES)?;
        }
        Ok(dir.join(INF_NAME))
    }

    fn pnputil_path() -> PathBuf {
        let root = std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".to_string());
        let sysnative = PathBuf::from(&root).join("Sysnative\\pnputil.exe");
        if sysnative.exists() {
            sysnative
        } else {
            PathBuf::from(&root).join("System32\\pnputil.exe")
        }
    }

    fn is_driver_present() -> bool {
        windows::get_display_drivers()
            .iter()
            .any(|(s, c)| s == super::HTD_IDD_DEVICE_STRING && *c == 0)
    }

    fn open_trusted_publisher_store() -> Option<HCERTSTORE> {
        let name = wide_z("TrustedPublisher");
        let store = unsafe {
            CertOpenStore(
                CERT_STORE_PROV_SYSTEM_W,
                0,
                0,
                CERT_SYSTEM_STORE_LOCAL_MACHINE,
                name.as_ptr() as _,
            )
        };
        if store.is_null() {
            log::error!(
                "Failed to open the TrustedPublisher store: {}",
                std::io::Error::last_os_error()
            );
            None
        } else {
            Some(store)
        }
    }

    fn trust_publisher() {
        let Some(store) = open_trusted_publisher_store() else {
            return;
        };
        let added = unsafe {
            CertAddEncodedCertificateToStore(
                store,
                X509_ASN_ENCODING,
                PUBLISHER_CER_BYTES.as_ptr(),
                PUBLISHER_CER_BYTES.len() as _,
                CERT_STORE_ADD_REPLACE_EXISTING,
                std::ptr::null_mut(),
            )
        };
        if added == 0 {
            log::error!(
                "Failed to trust the driver publisher: {}",
                std::io::Error::last_os_error()
            );
        }
        unsafe { CertCloseStore(store, 0) };
    }

    fn check_install_driver() -> ResultType<()> {
        let _l = LOCK.lock().unwrap();
        if is_driver_present() {
            return Ok(());
        }
        let inf_path = stage_package()?;
        trust_publisher();
        let output = std::process::Command::new(pnputil_path())
            .arg("/add-driver")
            .arg(&inf_path)
            .arg("/install")
            .creation_flags(CREATE_NO_WINDOW)
            .output()?;
        match output.status.code() {
            Some(0) | Some(3010) => Ok(()),
            code => {
                log::error!(
                    "pnputil failed ({:?}): {}",
                    code,
                    String::from_utf8_lossy(&output.stdout)
                );
                bail!("Failed to install the virtual display driver package.")
            }
        }
    }

    unsafe extern "system" fn create_callback(
        _device: HSWDEVICE,
        _result: ::windows::core::HRESULT,
        context: *const core::ffi::c_void,
        _instance_id: PCWSTR,
    ) {
        if !context.is_null() {
            let _ = SetEvent(HANDLE(context as _));
        }
    }

    fn create_device() -> ResultType<()> {
        if !super::is_virtual_display_supported() {
            bail!("The virtual display requires Windows 10 1809 or later.");
        }
        let mut device = SW_DEVICE.lock().unwrap();
        if device.is_some() {
            return Ok(());
        }

        let event = unsafe { CreateEventW(None, true, false, PCWSTR::null())? };
        let instance_id = wide_z(HARDWARE_ID);
        let hardware_ids = wide_zz(HARDWARE_ID);
        let description = wide_z(super::HTD_IDD_DEVICE_STRING.trim_end_matches('\0'));
        let enumerator = wide_z(ENUMERATOR_NAME);
        let parent = wide_z(PARENT_DEVICE);

        let create_info = SW_DEVICE_CREATE_INFO {
            cbSize: std::mem::size_of::<SW_DEVICE_CREATE_INFO>() as u32,
            pszInstanceId: PCWSTR(instance_id.as_ptr()),
            pszzHardwareIds: PCWSTR(hardware_ids.as_ptr()),
            pszzCompatibleIds: PCWSTR(hardware_ids.as_ptr()),
            pszDeviceDescription: PCWSTR(description.as_ptr()),
            CapabilityFlags: (SWDeviceCapabilitiesRemovable.0
                | SWDeviceCapabilitiesSilentInstall.0
                | SWDeviceCapabilitiesDriverRequired.0) as u32,
            ..Default::default()
        };

        let handle = unsafe {
            SwDeviceCreate(
                PCWSTR(enumerator.as_ptr()),
                PCWSTR(parent.as_ptr()),
                &create_info,
                None,
                Some(create_callback),
                Some(event.0 as _),
                )
        };
        let handle = match handle {
            Ok(h) => h,
            Err(e) => {
                unsafe { let _ = CloseHandle(event); }
                bail!("Failed to create the virtual display device: {e}");
            }
        };

        let waited = unsafe { WaitForSingleObject(event, CREATE_TIMEOUT_MS) };
        unsafe {
            let _ = CloseHandle(event);
        }
        if waited != WAIT_OBJECT_0 {
            unsafe { SwDeviceClose(handle) };
            bail!("Timed out creating the virtual display device.");
        }

        *device = Some(SwDevice(handle));
        Ok(())
    }

    fn close_device() {
        if let Some(device) = SW_DEVICE.lock().unwrap().take() {
            unsafe { SwDeviceClose(device.0) };
        }
    }

    fn wait_for_monitor(timeout: Duration) {
        let now = Instant::now();
        while now.elapsed() < timeout {
            if get_monitor_count() > 0 {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    pub fn plug_in_headless() -> ResultType<()> {
        let mut tm = LAST_PLUG_IN_HEADLESS_TIME.lock().unwrap();
        if let Some(tm) = &mut *tm {
            if tm.elapsed() < Duration::from_secs(3) {
                bail!("Plugging in too frequently.");
            }
        }
        *tm = Some(Instant::now());
        drop(tm);

        plug_in_monitor()
    }

    pub fn plug_in_monitor() -> ResultType<()> {
        if let Err(e) = check_install_driver() {
            log::error!("Failed to install the virtual display driver: {e}");
            bail!("Failed to install the virtual display driver.");
        }
        create_device()?;
        wait_for_monitor(Duration::from_millis(5_000));
        Ok(())
    }

    pub fn plug_out_monitor() -> ResultType<()> {
        close_device();
        Ok(())
    }

    pub fn reset_all() -> ResultType<()> {
        if !crate::privacy_mode::is_in_privacy_mode() && is_only_display() {
            close_when_real_display_appears();
        } else {
            let _ = crate::privacy_mode::turn_off_privacy(0, None);
            close_device();
        }
        *LAST_PLUG_IN_HEADLESS_TIME.lock().unwrap() = None;
        Ok(())
    }

    fn is_only_display() -> bool {
        let ours = get_monitor_count();
        ours > 0 && ours == windows::get_device_names(None).len()
    }

    fn has_real_display() -> bool {
        windows::get_device_names(None).len() > get_monitor_count()
    }

    fn close_when_real_display_appears() {
        if WATCHING_FOR_REAL_DISPLAY.swap(true, Ordering::SeqCst) {
            return;
        }
        std::thread::spawn(|| loop {
            std::thread::sleep(REAL_DISPLAY_POLL_INTERVAL);
            let mut device = SW_DEVICE.lock().unwrap();
            if device.is_some() && !has_real_display() {
                continue;
            }
            if let Some(device) = device.take() {
                unsafe { SwDeviceClose(device.0) };
            }
            WATCHING_FOR_REAL_DISPLAY.store(false, Ordering::SeqCst);
            break;
        });
    }

    #[inline]
    pub fn get_monitor_count() -> usize {
        windows::get_device_names(Some(super::HTD_IDD_DEVICE_STRING)).len()
    }

    #[inline]
    pub fn is_my_display(name: &str) -> bool {
        windows::get_device_names(Some(super::HTD_IDD_DEVICE_STRING))
            .iter()
            .any(|s| windows::is_device_name(s, name))
    }

    pub fn uninstall_driver() -> ResultType<()> {
        close_device();
        let published = find_published_inf();
        if let Some(oem_inf) = published {
            let _ = std::process::Command::new(pnputil_path())
                .arg("/delete-driver")
                .arg(&oem_inf)
                .arg("/uninstall")
                .arg("/force")
                .creation_flags(CREATE_NO_WINDOW)
                .output();
        }
        untrust_publisher();
        let _ = std::fs::remove_dir_all(CONFIG_DIR);
        Ok(())
    }

    fn untrust_publisher() {
        let Some(store) = open_trusted_publisher_store() else {
            return;
        };
        let ours = unsafe {
            CertCreateCertificateContext(
                X509_ASN_ENCODING,
                PUBLISHER_CER_BYTES.as_ptr(),
                PUBLISHER_CER_BYTES.len() as _,
            )
        };
        if !ours.is_null() {
            let found = unsafe {
                CertFindCertificateInStore(
                    store,
                    X509_ASN_ENCODING,
                    0,
                    CERT_FIND_EXISTING,
                    ours as _,
                    std::ptr::null(),
                )
            };
            if !found.is_null() {
                unsafe { CertDeleteCertificateFromStore(found) };
            }
            unsafe { CertFreeCertificateContext(ours) };
        }
        unsafe { CertCloseStore(store, 0) };
    }

    fn find_published_inf() -> Option<String> {
        let root = std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".to_string());
        let inf_dir = PathBuf::from(root).join("INF");
        let entries = std::fs::read_dir(inf_dir).ok()?;
        for entry in entries.flatten() {
            let path = entry.path();
            let is_oem = path
                .file_name()
                .and_then(|n| n.to_str())
                .map_or(false, |n| {
                    let n = n.to_ascii_lowercase();
                    n.starts_with("oem") && n.ends_with(".inf")
                });
            if !is_oem {
                continue;
            }
            let Ok(bytes) = std::fs::read(&path) else {
                continue;
            };
            let text = if bytes.starts_with(&[0xff, 0xfe]) {
                let units: Vec<u16> = bytes[2..]
                    .chunks_exact(2)
                    .map(|c| u16::from_le_bytes([c[0], c[1]]))
                    .collect();
                String::from_utf16_lossy(&units)
            } else {
                String::from_utf8_lossy(&bytes).into_owned()
            };
            if text.to_ascii_uppercase().contains("ROOT\\HTDVDD") {
                return path.file_name()?.to_str().map(|s| s.to_string());
            }
        }
        None
    }
}

mod windows {
    use std::ptr::null_mut;
    use winapi::{
        shared::{
            devguid::GUID_DEVCLASS_DISPLAY,
            minwindef::{DWORD, FALSE},
            ntdef::ULONG,
        },
        um::{
            cfgmgr32::{CM_Get_DevNode_Status, CR_SUCCESS},
            cguid::GUID_NULL,
            setupapi::{
                SetupDiEnumDeviceInfo, SetupDiGetClassDevsW, SetupDiGetDeviceRegistryPropertyW,
                SP_DEVINFO_DATA,
            },
            wingdi::{
                DEVMODEW, DISPLAY_DEVICEW, DISPLAY_DEVICE_ACTIVE, DISPLAY_DEVICE_MIRRORING_DRIVER,
            },
            winnt::HANDLE,
            winuser::{EnumDisplayDevicesW, EnumDisplaySettingsExW, ENUM_CURRENT_SETTINGS},
        },
    };

    const DIGCF_PRESENT: DWORD = 0x00000002;
    const SPDRP_DEVICEDESC: DWORD = 0x00000000;
    const INVALID_HANDLE_VALUE: HANDLE = -1isize as HANDLE;

    #[inline]
    pub(super) fn is_device_name(device_name: &str, name: &str) -> bool {
        if name.len() == device_name.len() {
            name == device_name
        } else if name.len() > device_name.len() {
            false
        } else {
            device_name.starts_with(name) && device_name.as_bytes()[name.len()] == 0
        }
    }

    pub(super) fn get_device_names(device_string: Option<&str>) -> Vec<String> {
        let mut device_names = Vec::new();
        let mut dd: DISPLAY_DEVICEW = unsafe { std::mem::zeroed() };
        dd.cb = std::mem::size_of::<DISPLAY_DEVICEW>() as DWORD;
        let mut i_dev_num = 0;
        loop {
            let result = unsafe { EnumDisplayDevicesW(null_mut(), i_dev_num, &mut dd, 0) };
            if result == 0 {
                break;
            }
            i_dev_num += 1;

            if 0 == (dd.StateFlags & DISPLAY_DEVICE_ACTIVE)
                || (dd.StateFlags & DISPLAY_DEVICE_MIRRORING_DRIVER) > 0
            {
                continue;
            }

            let mut dm: DEVMODEW = unsafe { std::mem::zeroed() };
            dm.dmSize = std::mem::size_of::<DEVMODEW>() as _;
            dm.dmDriverExtra = 0;
            let ok = unsafe {
                EnumDisplaySettingsExW(
                    dd.DeviceName.as_ptr(),
                    ENUM_CURRENT_SETTINGS,
                    &mut dm as _,
                    0,
                )
            };
            if ok == FALSE {
                continue;
            }
            if dm.dmPelsHeight == 0 || dm.dmPelsWidth == 0 {
                continue;
            }

            if let (Ok(device_name), Ok(ds)) = (
                String::from_utf16(&dd.DeviceName),
                String::from_utf16(&dd.DeviceString),
            ) {
                if let Some(s) = device_string {
                    if ds.starts_with(s) {
                        device_names.push(device_name);
                    }
                } else {
                    device_names.push(device_name);
                }
            }
        }
        device_names
    }

    pub(super) fn get_display_drivers() -> Vec<(String, u32)> {
        let mut display_drivers: Vec<(String, u32)> = Vec::new();

        let device_info_set = unsafe {
            SetupDiGetClassDevsW(
                &GUID_DEVCLASS_DISPLAY,
                null_mut(),
                null_mut(),
                DIGCF_PRESENT,
            )
        };

        if device_info_set == INVALID_HANDLE_VALUE {
            println!(
                "Failed to get device information set. Error: {}",
                std::io::Error::last_os_error()
            );
            return display_drivers;
        }

        let mut device_info_data = SP_DEVINFO_DATA {
            cbSize: std::mem::size_of::<SP_DEVINFO_DATA>() as u32,
            ClassGuid: GUID_NULL,
            DevInst: 0,
            Reserved: 0,
        };

        let mut device_index = 0;
        loop {
            let result = unsafe {
                SetupDiEnumDeviceInfo(device_info_set, device_index, &mut device_info_data)
            };
            if result == 0 {
                break;
            }

            let mut data_type: DWORD = 0;
            let mut required_size: DWORD = 0;

            // Get the required buffer size for the driver description
            let mut buffer;
            unsafe {
                SetupDiGetDeviceRegistryPropertyW(
                    device_info_set,
                    &mut device_info_data,
                    SPDRP_DEVICEDESC,
                    &mut data_type,
                    null_mut(),
                    0,
                    &mut required_size,
                );

                buffer = vec![0; required_size as usize / 2];
                SetupDiGetDeviceRegistryPropertyW(
                    device_info_set,
                    &mut device_info_data,
                    SPDRP_DEVICEDESC,
                    &mut data_type,
                    buffer.as_mut_ptr() as *mut u8,
                    required_size,
                    null_mut(),
                );
            }

            let Ok(driver_description) = String::from_utf16(&buffer) else {
                println!("Failed to convert driver description to string");
                device_index += 1;
                continue;
            };

            let mut status: ULONG = 0;
            let mut problem_number: ULONG = 0;
            // Get the device status and problem number
            let config_ret = unsafe {
                CM_Get_DevNode_Status(
                    &mut status,
                    &mut problem_number,
                    device_info_data.DevInst,
                    0,
                )
            };
            if config_ret != CR_SUCCESS {
                println!(
                    "Failed to get device status. Error: {}",
                    std::io::Error::last_os_error()
                );
                device_index += 1;
                continue;
            }
            display_drivers.push((driver_description, problem_number));
            device_index += 1;
        }

        display_drivers
    }
}
