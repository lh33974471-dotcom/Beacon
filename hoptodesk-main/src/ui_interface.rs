#[cfg(any(target_os = "android", target_os = "ios"))]
use hbb_common::{password_security, bytes::Bytes};
#[cfg(all(feature = "flutter", not(any(target_os = "android", target_os = "ios"))))]
use hbb_common::bytes::Bytes;
use hbb_common::{
    allow_err,
    config::{self, keys::*, Config, LocalConfig, PeerConfig},
    directories_next, 
    log, 
    tokio,
};
#[cfg(not(any(target_os = "android", target_os = "ios")))]
use hbb_common::{
    anyhow::anyhow,
    message_proto::{message, InviteRequest, Message},
    protobuf::Message as _,
    ResultType,
};
#[cfg(not(any(target_os = "android", target_os = "ios")))]
use std::time::{Duration, Instant};
#[cfg(not(any(target_os = "android", target_os = "ios")))]
use hbb_common::{
    sleep,
    tokio::{sync::mpsc, time},
};
use serde_derive::Serialize;
use std::{
    collections::HashMap,
    process::Child,
    sync::{Arc, Mutex},
};

use crate::{common::SOFTWARE_UPDATE_URL};
#[cfg(not(any(target_os = "ios")))]
use crate::ipc;

pub type Children = Arc<Mutex<(bool, HashMap<(String, String), Child>)>>;

#[derive(Clone, Debug, Serialize)]
pub struct UiStatus {
    pub status_num: i32,
    #[cfg(not(feature = "flutter"))]
    pub key_confirmed: bool,
    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    pub mouse_time: i64,
    #[cfg(not(feature = "flutter"))]
    pub id: String,
    #[cfg(feature = "flutter")]
    pub video_conn_count: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct LoginDeviceInfo {
    pub os: String,
    pub r#type: String,
    pub name: String,
}

lazy_static::lazy_static! {
    static ref CHILDREN : Children = Default::default();
    static ref UI_STATUS : Arc<Mutex<UiStatus>> = Arc::new(Mutex::new(UiStatus{
        status_num: 0,
        #[cfg(not(feature = "flutter"))]
        key_confirmed: false,
        #[cfg(not(any(target_os = "android", target_os = "ios")))]
        mouse_time: 0,
        #[cfg(not(feature = "flutter"))]
        id: "".to_owned(),
        #[cfg(feature = "flutter")]
        video_conn_count: 0,
    }));
    static ref ASYNC_JOB_STATUS : Arc<Mutex<String>> = Default::default();
    static ref TEMPORARY_PASSWD : Arc<Mutex<String>> = Arc::new(Mutex::new("".to_owned()));
}

#[cfg(not(any(target_os = "android", target_os = "ios")))]
lazy_static::lazy_static! {
    static ref OPTION_SYNCED: Arc<Mutex<bool>> = Default::default();
    static ref OPTIONS : Arc<Mutex<HashMap<String, String>>> = Arc::new(Mutex::new(Config::get_options()));
    pub static ref SENDER : Mutex<mpsc::UnboundedSender<ipc::Data>> = Mutex::new(check_connect_status(true));
}
#[cfg(any(target_os = "android", target_os = "ios", feature = "flutter"))]
const INIT_ASYNC_JOB_STATUS: &str = " ";

#[cfg(any(target_os = "android", target_os = "ios", feature = "flutter"))]
#[inline]
pub fn get_id() -> String {
    #[cfg(any(target_os = "android", target_os = "ios"))]
    return Config::get_id();
    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    return ipc::get_id();
}

#[inline]
#[cfg(not(any(target_os = "android", target_os = "ios")))]
pub fn get_remote_id() -> String {
    LocalConfig::get_remote_id()
}

#[inline]
#[cfg(not(any(target_os = "android", target_os = "ios")))]
pub fn set_remote_id(id: String) {
    LocalConfig::set_remote_id(&id);
}

#[inline]
pub fn goto_install() {
    allow_err!(crate::run_me(vec!["--install"]));
    std::process::exit(0);
}

#[inline]
pub fn install_me(_options: String, _path: String, _silent: bool, _debug: bool, _nostartup: bool) {
    #[cfg(windows)]
    std::thread::spawn(move || {
		allow_err!(crate::platform::windows::install_me(
            &_options, _path, _silent, _debug, _nostartup
        ));
		std::process::exit(0);
    });
}

#[inline]
pub fn run_without_install() {
    crate::run_me(vec!["--noinstall"]).ok();
    std::process::exit(0);
}

#[inline]
pub fn show_run_without_install() -> bool {
    let mut it = std::env::args();
    if let Some(tmp) = it.next() {
        if crate::is_setup(&tmp) {
            return it.next() == None;
        }
    }
    false
}

#[inline]
pub fn refresh_options() {
    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    {
        *OPTIONS.lock().unwrap() = Config::get_options();
    }
}

#[inline]
pub fn get_option<T: AsRef<str>>(key: T) -> String {
    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    {
        let map = OPTIONS.lock().unwrap();
        if let Some(v) = map.get(key.as_ref()) {
            v.to_owned()
        } else {
            "".to_owned()
        }
    }
    #[cfg(any(target_os = "android", target_os = "ios"))]
    {
        Config::get_option(key.as_ref())
    }
}

#[inline]
pub fn use_texture_render() -> bool {
    #[cfg(target_os = "android")]
    return false;
    #[cfg(target_os = "ios")]
    return false;

    #[cfg(target_os = "macos")]
    return cfg!(feature = "flutter")
        && LocalConfig::get_option(config::keys::OPTION_TEXTURE_RENDER) == "Y";

    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    return cfg!(feature = "flutter")
        && LocalConfig::get_option(config::keys::OPTION_TEXTURE_RENDER) != "N";

    #[cfg(target_os = "windows")]
    {
        if !cfg!(feature = "flutter") {
            return false;
        }
        // https://learn.microsoft.com/en-us/windows/win32/sysinfo/targeting-your-application-at-windows-8-1
        #[cfg(debug_assertions)]
        let default_texture = true;
        #[cfg(not(debug_assertions))]
        // Beacon LAN-only: the GPU texture renderer produced wrong colors on
        // fleet machines. Default to the software renderer (stable); users can
        // still opt back in via Settings > Display > Use texture rendering.
        let default_texture = false;
        if default_texture {
            LocalConfig::get_option(config::keys::OPTION_TEXTURE_RENDER) != "N"
        } else {
            return LocalConfig::get_option(config::keys::OPTION_TEXTURE_RENDER) == "Y";
        }
    }
}

#[inline]
pub fn get_local_option(key: String) -> String {
    // The autostart checkbox reflects the real registry state, not a cached flag.
    #[cfg(target_os = "windows")]
    if key == config::keys::OPTION_ENABLE_START_ON_BOOT {
        return if crate::platform::windows::get_run_at_startup() {
            "Y"
        } else {
            "N"
        }
        .to_owned();
    }
    LocalConfig::get_option(&key)
}

#[inline]
#[cfg(feature = "flutter")]
pub fn get_hard_option(key: String) -> String {
    config::HARD_SETTINGS
        .read()
        .unwrap()
        .get(&key)
        .cloned()
        .unwrap_or_default()
}

#[inline]
pub fn get_builtin_option(key: &str) -> String {
    config::BUILTIN_SETTINGS
        .read()
        .unwrap()
        .get(key)
        .cloned()
        .unwrap_or_default()
}

#[inline]
pub fn set_local_option(key: String, value: String) {
    #[cfg(target_os = "windows")]
    if key == config::keys::OPTION_ENABLE_START_ON_BOOT {
        // enable-* options treat "" as "yes"; "N" is the only explicit "no".
        crate::platform::windows::set_run_at_startup(value != "N");
    }
    LocalConfig::set_option(key, value);
}

#[cfg(not(any(target_os = "android", target_os = "ios")))]
pub async fn migrate_auto_record_outgoing() {
    let key = hbb_common::config::keys::OPTION_ALLOW_AUTO_RECORD_OUTGOING;
    if get_option(key) != "Y" {
        return;
    }
    if LocalConfig::get_option(key).is_empty() {
        LocalConfig::set_option(key.to_owned(), "Y".to_owned());
        log::info!("moved {key} from the app config to the local config");
    }
    let options = {
        let mut options = OPTIONS.lock().unwrap();
        options.remove(key);
        options.clone()
    };
    ipc::set_options_async(options).await.ok();
}

#[cfg(any(target_os = "android", target_os = "ios", feature = "flutter"))]
#[inline]
pub fn get_local_flutter_option(key: String) -> String {
    LocalConfig::get_flutter_option(&key)
}

#[cfg(any(target_os = "android", target_os = "ios", feature = "flutter"))]
#[inline]
pub fn set_local_flutter_option(key: String, value: String) {
    LocalConfig::set_flutter_option(key, value);
}

#[cfg(feature = "flutter")]
#[inline]
pub fn get_kb_layout_type() -> String {
    LocalConfig::get_kb_layout_type()
}

#[cfg(feature = "flutter")]
#[inline]
pub fn set_kb_layout_type(kb_layout_type: String) {
    LocalConfig::set_kb_layout_type(kb_layout_type);
}

#[inline]
pub fn peer_has_password(id: String) -> bool {
    !PeerConfig::load(&id).password.is_empty()
}

#[inline]
pub fn forget_password(id: String) {
    let mut c = PeerConfig::load(&id);
    c.password.clear();
    c.store(&id);
}

#[inline]
pub fn get_peer_option(id: String, name: String) -> String {
    let c = PeerConfig::load(&id);
    c.options.get(&name).unwrap_or(&"".to_owned()).to_owned()
}

#[inline]
#[cfg(feature = "flutter")]
pub fn get_peer_flutter_option(id: String, name: String) -> String {
    let c = PeerConfig::load(&id);
    c.ui_flutter.get(&name).unwrap_or(&"".to_owned()).to_owned()
}

#[inline]
#[cfg(feature = "flutter")]
pub fn set_peer_flutter_option(id: String, name: String, value: String) {
    let mut c = PeerConfig::load(&id);
    if value.is_empty() {
        c.ui_flutter.remove(&name);
    } else {
        c.ui_flutter.insert(name, value);
    }
    c.store(&id);
}

#[inline]
pub fn set_peer_option(id: String, name: String, value: String) {
    let mut c = PeerConfig::load(&id);
    if value.is_empty() {
        c.options.remove(&name);
    } else {
        c.options.insert(name, value);
    }
    c.store(&id);
}

#[inline]
pub fn get_options() -> String {
    let options = {
        #[cfg(not(any(target_os = "android", target_os = "ios")))]
        {
            OPTIONS.lock().unwrap()
        }
        #[cfg(any(target_os = "android", target_os = "ios"))]
        {
            Config::get_options()
        }
    };
    let mut m = serde_json::Map::new();
    for (k, v) in options.iter() {
        m.insert(k.into(), v.to_owned().into());
    }
    serde_json::to_string(&m).unwrap_or_default()
}

#[inline]
pub fn test_if_valid_server(host: String) -> String {
    hbb_common::socket_client::test_if_valid_server(&host)
}

#[inline]
#[cfg(feature = "flutter")]
#[cfg(not(any(target_os = "android", target_os = "ios")))]
pub fn get_sound_inputs() -> Vec<String> {
    let mut a = Vec::new();
    #[cfg(not(target_os = "linux"))]
    {
        fn get_sound_inputs_() -> Vec<String> {
            let mut out = Vec::new();
            use cpal::traits::{DeviceTrait, HostTrait};
            let host = cpal::default_host();
            if let Ok(devices) = host.devices() {
                for device in devices {
                    if device.default_input_config().is_err() {
                        continue;
                    }
                    if let Ok(name) = device.name() {
                        out.push(name);
                    }
                }
            }
            out
        }

        let inputs = Arc::new(Mutex::new(Vec::new()));
        let cloned = inputs.clone();
        // can not call below in UI thread, because it conflicts with sound COM initialization
        std::thread::spawn(move || *cloned.lock().unwrap() = get_sound_inputs_())
            .join()
            .ok();
        for name in inputs.lock().unwrap().drain(..) {
            a.push(name);
        }
    }
    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    {
        let inputs: Vec<String> = crate::platform::linux::get_pa_sources()
            .drain(..)
            .map(|x| x.1)
            .collect();

        for name in inputs {
            a.push(name);
        }
    }
    a
}

#[inline]
pub fn set_options(m: HashMap<String, String>) {
    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    {
        *OPTIONS.lock().unwrap() = m.clone();
        ipc::set_options(m).ok();
    }
    #[cfg(any(target_os = "android", target_os = "ios"))]
    Config::set_options(m);
}

#[inline]
pub fn set_option(key: String, value: String) {
    if &key == "stop-service" {
        #[cfg(target_os = "macos")]
        {
            let is_stop = value == "Y";
            if is_stop && crate::platform::uninstall_service(true, false) {
                return;
            }
        }
        #[cfg(target_os = "windows")]
        if value != "Y"
            && crate::platform::is_installed()
            && !crate::platform::is_self_service_running()
        {
            crate::platform::install_service();
            return;
        }
    } else if &key == "audio-input" {
        #[cfg(not(target_os = "ios"))]
        crate::audio_service::restart();
    } else if &key == "enable-remote-printing" {
        #[cfg(target_os = "windows")]
        {
            if value == "Y" {
                if !crate::platform::is_remote_printing_available() {
                    log::warn!("Remote printing requires the PDF printer driver (Windows 10 or newer)");
                    return;
                }
                // Printer will be installed when an incoming remote session starts
                // If there's already an active session, install now
                let active = crate::server::Connection::alive_conns();
                if !active.is_empty() {
                    if let Err(e) = crate::platform::install_virtual_printer() {
                        log::error!("Failed to install virtual printer: {}", e);
                    }
                    crate::server::printer_service::start_remote_printing();
                }
            } else {
                // Stop print service and uninstall printer
                crate::server::printer_service::stop_remote_printing();
                if let Err(e) = crate::platform::uninstall_virtual_printer() {
                    log::error!("Failed to uninstall virtual printer: {}", e);
                }
            }
        }
        #[cfg(target_os = "macos")]
        {
            if value == "Y" {
                if let Err(e) = crate::platform::install_virtual_printer() {
                    log::error!("Failed to install virtual printer: {}", e);
                }
                // If there's already an active session, start the print service
                let active = crate::server::Connection::alive_conns();
                if !active.is_empty() {
                    crate::server::printer_service::start_remote_printing();
                }
            } else {
                crate::server::printer_service::stop_remote_printing();
                if let Err(e) = crate::platform::uninstall_virtual_printer() {
                    log::error!("Failed to uninstall virtual printer: {}", e);
                }
            }
        }
        #[cfg(any(target_os = "linux", target_os = "freebsd"))]
        {
            if value != "Y" {
                crate::server::printer_service::stop_remote_printing();
                if let Err(e) = crate::platform::uninstall_virtual_printer() {
                    log::error!("Failed to uninstall virtual printer: {}", e);
                }
            }
        }
    }
    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    if key == "invite_code" && !value.is_empty() {
        // Trigger dashboard enrollment when invite code is set from Settings UI
        std::thread::spawn(|| crate::dashboard::start());
    }
    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    {
        let mut options = OPTIONS.lock().unwrap();
        if value.is_empty() {
            options.remove(&key);
        } else {
            options.insert(key.clone(), value.clone());
        }
        ipc::set_options(options.clone()).ok();
    }
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    if key == "invite_code" && !value.is_empty() {
        std::thread::spawn(wake_server_dashboard);
    }
    #[cfg(any(target_os = "android", target_os = "ios"))]
    Config::set_option(key, value);
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[tokio::main(flavor = "current_thread")]
async fn wake_server_dashboard() {
    let _ = ipc::notify_dashboard_relinked().await;
}

#[inline]
pub fn install_path() -> String {
    #[cfg(windows)]
    return crate::platform::windows::get_install_info().1;
    #[cfg(not(windows))]
    return "".to_owned();
}

#[inline]
pub fn install_options() -> String {
    #[cfg(windows)]
    return crate::platform::windows::get_install_options();
    #[cfg(not(windows))]
    return "{}".to_owned();
}

#[inline]
pub fn get_socks() -> Vec<String> {
    #[cfg(any(target_os = "android", target_os = "ios"))]
    return Vec::new();
    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    {
        let s = ipc::get_socks();
        match s {
            None => Vec::new(),
            Some(s) => {
                let mut v = Vec::new();
                v.push(s.proxy);
                v.push(s.username);
                v.push(s.password);
                v.push(s.proxy_type.as_str().to_string());
                v
            }
        }
    }
}

#[inline]
#[cfg(not(any(target_os = "android", target_os = "ios")))]
pub fn set_socks(proxy: String, username: String, password: String, proxy_type: String) {
    ipc::set_socks(config::Socks5Server {
        proxy,
        username,
        password,
        proxy_type: config::ProxyType::from_str(&proxy_type),
    })
    .ok();
}

#[cfg(any(target_os = "android", target_os = "ios"))]
pub fn set_socks(_: String, _: String, _: String, _: String) {}

#[cfg(not(any(target_os = "android", target_os = "ios")))]
#[inline]
pub fn is_installed() -> bool {
    crate::platform::is_installed()
}

#[cfg(any(target_os = "android", target_os = "ios"))]
#[inline]
pub fn is_installed() -> bool {
    false
}

#[inline]
pub fn is_share_rdp() -> bool {
    #[cfg(windows)]
    return crate::platform::windows::is_share_rdp();
    #[cfg(not(windows))]
    return false;
}

#[inline]
pub fn set_share_rdp(_enable: bool) {
    #[cfg(windows)]
    crate::platform::windows::set_share_rdp(_enable);
}

#[inline]
pub fn is_installed_lower_version() -> bool {
    #[cfg(not(windows))]
    return false;
    #[cfg(windows)]
    {
        let installed_version = crate::platform::windows::get_installed_version();
        let a = hbb_common::get_version_number(crate::VERSION);
        let b = hbb_common::get_version_number(&installed_version);
        return a > b;
    }
}

#[inline]
#[cfg(not(any(target_os = "android", target_os = "ios")))]
pub fn closing(x: i32, y: i32, w: i32, h: i32) {
    crate::server::graceful_shutdown_server();
    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    crate::server::input_service::fix_key_down_timeout_at_exit();
    LocalConfig::set_size(x, y, w, h);
	#[cfg(target_os = "windows")]
	{
		let (_, _, _, exe) = crate::platform::get_install_info();
	    if crate::platform::is_installed() {
			if !crate::check_process("--tray", false) {
				if !crate::check_process("", false) {
					std::process::Command::new(&exe).arg("--tray").spawn().ok();
				}
			}
		}
	}
}

#[inline]
#[cfg(not(any(target_os = "android", target_os = "ios")))]
pub fn get_mouse_time() -> f64 {
    UI_STATUS.lock().unwrap().mouse_time as f64
}

#[inline]
pub fn check_mouse_time() {
    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    {
        let sender = SENDER.lock().unwrap();
        allow_err!(sender.send(ipc::Data::MouseMoveTime(0)));
    }
}

#[inline]
#[cfg(not(any(target_os = "android", target_os = "ios")))]
pub fn get_connect_status() -> UiStatus {
    UI_STATUS.lock().unwrap().clone()
}

#[inline]
pub fn temporary_password() -> String {
    #[cfg(any(target_os = "android", target_os = "ios"))]
    return password_security::temporary_password();
    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    return TEMPORARY_PASSWD.lock().unwrap().clone();
}

#[inline]
pub fn update_temporary_password() {
	#[cfg(any(target_os = "android", target_os = "ios"))]
    password_security::update_temporary_password();
    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    allow_err!(ipc::update_temporary_password());
}

#[inline]
pub fn permanent_password() -> String {
    #[cfg(any(target_os = "android", target_os = "ios"))]
    return Config::get_permanent_password();
    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    return ipc::get_permanent_password();
}

#[inline]
pub fn set_permanent_password(password: String) {
    #[cfg(any(target_os = "android", target_os = "ios"))]
    Config::set_permanent_password(&password);
    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    allow_err!(ipc::set_permanent_password(password));
}

#[inline]
#[cfg(not(any(target_os = "android", target_os = "ios", feature = "cli", feature = "flutter")))]
pub fn guest_password() -> String {
    #[cfg(any(target_os = "android", target_os = "ios"))]
    return Config::get_guest_password();
    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    return ipc::get_guest_password();
}

#[inline]
#[cfg(not(any(target_os = "android", target_os = "ios", feature = "cli", feature = "flutter")))]
pub fn set_guest_password(password: String) {
    #[cfg(any(target_os = "android", target_os = "ios"))]
    Config::set_guest_password(&password);
    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    allow_err!(ipc::set_guest_password(password));
}

#[inline]
pub fn get_peer(id: String) -> PeerConfig {
    PeerConfig::load(&id)
}

#[inline]
pub fn get_fav() -> Vec<String> {
    LocalConfig::get_fav()
}

#[inline]
pub fn store_fav(fav: Vec<String>) {
    LocalConfig::set_fav(fav);
}

#[cfg(not(target_os = "ios"))]
pub fn close_remote(id: &str) {
    CHILDREN.lock().unwrap().1.retain(|k, child| {
        if k.0 == id {
            match child.kill() {
                Ok(_) => return false,
                Err(e) => log::error!("Failed to kill remote {id}: {e}")
            }
        }
        true
    });
}

#[inline]
pub fn is_process_trusted(_prompt: bool) -> bool {
    #[cfg(target_os = "macos")]
    return crate::platform::macos::is_process_trusted(_prompt);
    #[cfg(not(target_os = "macos"))]
    return true;
}

#[inline]
pub fn is_can_screen_recording(_prompt: bool) -> bool {
    #[cfg(target_os = "macos")]
    return crate::platform::macos::is_can_screen_recording(_prompt);
    #[cfg(not(target_os = "macos"))]
    return true;
}

#[inline]
pub fn is_installed_daemon(_prompt: bool) -> bool {
    #[cfg(target_os = "macos")]
    return crate::platform::macos::is_installed_daemon(_prompt);
    #[cfg(not(target_os = "macos"))]
    return true;
}

#[inline]
#[cfg(feature = "flutter")]
pub fn is_can_input_monitoring(_prompt: bool) -> bool {
    #[cfg(target_os = "macos")]
    return crate::platform::macos::is_can_input_monitoring(_prompt);
    #[cfg(not(target_os = "macos"))]
    return true;
}

#[inline]
pub fn get_error() -> String {
    #[cfg(not(any(feature = "cli")))]
    #[cfg(target_os = "linux")]
    {
        let dtype = crate::platform::linux::get_display_server();
        if crate::platform::linux::DISPLAY_SERVER_WAYLAND == dtype {
            return crate::server::wayland::common_get_error();
        }
        if dtype != crate::platform::linux::DISPLAY_SERVER_X11 {
            return format!(
                "{} {}, {}",
                crate::client::translate("Unsupported display server".to_owned()),
                dtype,
                crate::client::translate("x11 expected".to_owned()),
            );
        }
    }
    return "".to_owned();
}

#[inline]
pub fn is_login_wayland() -> bool {
    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    return crate::platform::linux::is_login_wayland();
    #[cfg(not(any(target_os = "linux", target_os = "freebsd")))]
    return false;
}

#[inline]
pub fn current_is_wayland() -> bool {
    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    return crate::platform::linux::current_is_wayland();
    #[cfg(not(any(target_os = "linux", target_os = "freebsd")))]
    return false;
}

#[inline]
pub fn get_new_version() -> String {
    (*SOFTWARE_UPDATE_URL
        .lock()
        .unwrap()
        .rsplit('/')
        .next()
        .unwrap_or(""))
    .to_string()
}

#[inline]
pub fn get_version() -> String {
    crate::VERSION.to_owned()
}

#[cfg(any(target_os = "android", target_os = "ios", feature = "flutter"))]
#[inline]
pub fn get_app_name() -> String {
    crate::get_app_name()
}

#[cfg(windows)]
#[inline]
pub fn create_shortcut(_id: String) {
    crate::platform::windows::create_shortcut(&_id).ok();
}

#[cfg(any(target_os = "android", target_os = "ios", feature = "flutter"))]
#[inline]
pub fn discover() {
    std::thread::spawn(move || {
        allow_err!(crate::lan::discover());
    });
}

#[cfg(feature = "flutter")]
pub fn peer_to_map(id: String, p: PeerConfig) -> HashMap<&'static str, String> {
    use hbb_common::sodiumoxide::base64;
    HashMap::<&str, String>::from_iter([
        ("id", id),
        ("username", p.info.username.clone()),
        ("hostname", p.info.hostname.clone()),
        ("platform", p.info.platform.clone()),
        (
            "alias",
            p.options.get("alias").unwrap_or(&"".to_owned()).to_owned(),
        ),
        (
            "hash",
            base64::encode(p.password, base64::Variant::Original),
        ),
    ])
}

#[cfg(feature = "flutter")]
pub fn peer_exists(id: &str) -> bool {
    PeerConfig::exists(id)
}

#[inline]
pub fn get_lan_peers() -> Vec<HashMap<&'static str, String>> {
    config::LanPeers::load()
        .peers
        .iter()
        .map(|peer| {
            HashMap::<&str, String>::from_iter([
                ("id", peer.id.clone()),
                ("username", peer.username.clone()),
                ("hostname", peer.hostname.clone()),
                ("platform", peer.platform.clone()),
                (
                    "alias",
                    PeerConfig::load(&peer.id)
                        .options
                        .get("alias")
                        .cloned()
                        .unwrap_or_default(),
                ),
            ])
        })
        .collect()
}

#[inline]
pub fn remove_discovered(id: String) {
    let mut peers = config::LanPeers::load().peers;
    peers.retain(|x| x.id != id);
    config::LanPeers::store(&peers);
}

#[inline]
pub fn get_uuid() -> String {
    crate::encode64(hbb_common::get_uuid())
}

#[cfg(any(target_os = "android", target_os = "ios", feature = "flutter"))]
#[inline]
pub fn get_init_async_job_status() -> String {
    INIT_ASYNC_JOB_STATUS.to_string()
}

#[cfg(any(target_os = "android", target_os = "ios", feature = "flutter"))]
#[inline]
pub fn reset_async_job_status() {
    *ASYNC_JOB_STATUS.lock().unwrap() = get_init_async_job_status();
}

#[cfg(any(target_os = "android", target_os = "ios", feature = "flutter"))]
#[inline]
pub fn change_id(id: String) {
    reset_async_job_status();
    let old_id = get_id();
    std::thread::spawn(move || {
        change_id_shared(id, old_id);
    });
}

#[inline]
pub fn post_request(url: String, body: String, header: String) {
    *ASYNC_JOB_STATUS.lock().unwrap() = " ".to_owned();
    std::thread::spawn(move || {
        *ASYNC_JOB_STATUS.lock().unwrap() = match crate::post_request_sync(url, body, &header) {
            Err(err) => err.to_string(),
            Ok(text) => text,
        };
    });
}

#[inline]
#[cfg(not(any(target_os = "android", target_os = "ios")))]
pub fn get_request(url: String, header: String) {
    *ASYNC_JOB_STATUS.lock().unwrap() = " ".to_owned();
    std::thread::spawn(move || {
        *ASYNC_JOB_STATUS.lock().unwrap() = match crate::get_request_sync(url, &header) {
            Err(err) => err.to_string(),
            Ok(text) => text,
        };
    });
}

#[inline]
pub fn get_async_job_status() -> String {
    ASYNC_JOB_STATUS.lock().unwrap().clone()
}

#[inline]
pub fn get_langs() -> String {
    crate::lang::LANGS.to_string()
}

#[cfg(any(windows, test))]
fn latest_recording_mtime(dir: &str) -> Option<std::time::SystemTime> {
    std::fs::read_dir(dir)
        .ok()?
        .filter_map(|e| e.ok())
        .filter_map(|e| e.metadata().ok())
        .filter(|m| m.is_file())
        .filter_map(|m| m.modified().ok())
        .max()
}

#[cfg(any(windows, test))]
pub fn choose_recordings_dir(root_dir: &str, user_dir: &str) -> String {
    match (
        latest_recording_mtime(root_dir),
        latest_recording_mtime(user_dir),
    ) {
        (Some(r), Some(u)) if r > u => root_dir.to_owned(),
        (Some(_), None) => root_dir.to_owned(),
        _ => user_dir.to_owned(),
    }
}

#[cfg(not(any(target_os = "android", target_os = "ios", feature = "cli", feature = "flutter")))]
pub fn recordings_directory() -> String {
    let user_dir = video_save_directory(false);
    #[cfg(windows)]
    {
        let root_dir = video_save_directory(true);
        if !root_dir.is_empty() {
            return choose_recordings_dir(&root_dir, &user_dir);
        }
    }
    user_dir
}

#[inline]
pub fn video_save_directory(root: bool) -> String {
    let appname = crate::get_app_name();

    if root {
        // Currently, only installed windows run as root
        #[cfg(windows)]
        {
            if let Some(locked) = config::OVERWRITE_LOCAL_SETTINGS
                .read()
                .unwrap()
                .get(OPTION_VIDEO_SAVE_DIRECTORY)
                .filter(|dir| !dir.is_empty())
            {
                return locked.clone();
            }
            let drive = std::env::var("SystemDrive").unwrap_or("C:".to_owned());
            let dir =
                std::path::PathBuf::from(format!("{drive}\\ProgramData\\HopToDesk\\recording",));
            return dir.to_string_lossy().to_string();
        }
    }
    #[cfg(any(target_os = "linux", target_os = "freebsd", target_os = "macos"))]
    let dir = LocalConfig::get_option_from_file(OPTION_VIDEO_SAVE_DIRECTORY);
    #[cfg(not(any(target_os = "linux", target_os = "freebsd", target_os = "macos")))]
    let dir = LocalConfig::get_option(OPTION_VIDEO_SAVE_DIRECTORY);
    if !dir.is_empty() {
        return dir;
    }
    #[cfg(any(target_os = "android", target_os = "ios"))]
    if let Ok(home) = config::APP_HOME_DIR.read() {
        let mut path = home.to_owned();
        path.push_str("/HopToDesk/ScreenRecord");
        return path;
    }

    if let Some(user) = directories_next::UserDirs::new() {
        if let Some(video_dir) = user.video_dir() {
            return video_dir.join(&appname).to_string_lossy().to_string();
        }
        if let Some(desktop_dir) = user.desktop_dir() {
            if desktop_dir.exists() {
                return desktop_dir.to_string_lossy().to_string();
            }
        }
        let home = user.home_dir();
        if home.exists() {
            return home.to_string_lossy().to_string();
        }
    }

    // same order as above
    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    if let Some(home) = crate::platform::get_active_user_home() {
        let name = if cfg!(target_os = "macos") {
            "Movies"
        } else {
            "Videos"
        };
        return home.join(name).join(&appname).to_string_lossy().to_string();
    }

    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            return parent.join("videos").to_string_lossy().to_string();
        }
    }
    Default::default()
}

#[inline]
pub fn has_hwcodec() -> bool {
    // Has real hardware codec using gpu
    (cfg!(feature = "nativecodec") && (cfg!(windows) || cfg!(target_os = "linux")))
        || cfg!(feature = "mediacodec")
}

#[inline]
pub fn has_vram() -> bool {
    cfg!(feature = "vram")
}

#[cfg(feature = "flutter")]
#[inline]
pub fn supported_hwdecodings() -> (bool, bool) {
    let decoding =
        scrap::codec::Decoder::supported_decodings(None, use_texture_render(), None, &vec![]);
    #[allow(unused_mut)]
    let (mut h264, mut h265) = (decoding.ability_h264 > 0, decoding.ability_h265 > 0);
    #[cfg(feature = "vram")]
    {
        // supported_decodings check runtime luid
        let vram = scrap::vram::VRamDecoder::possible_available_without_check();
        if vram.0 {
            h264 = true;
        }
        if vram.1 {
            h265 = true;
        }
    }
    (h264, h265)
}

#[cfg(not(any(target_os = "android", target_os = "ios")))]
#[inline]
pub fn is_root() -> bool {
    crate::platform::is_root()
}

#[cfg(any(target_os = "android", target_os = "ios"))]
#[inline]
pub fn is_root() -> bool {
    false
}

#[cfg(any(target_os = "android", target_os = "ios", feature = "flutter"))]
#[inline]
pub fn check_super_user_permission() -> bool {
    #[cfg(any(windows, target_os = "linux", target_os = "freebsd", target_os = "macos"))]
    return crate::platform::check_super_user_permission().unwrap_or(false);
    #[cfg(not(any(windows, target_os = "linux", target_os = "freebsd", target_os = "macos")))]
    return true;
}

#[cfg(not(any(target_os = "android", target_os = "ios", feature = "flutter")))]
pub fn check_zombie() {
    let mut deads = Vec::new();
    loop {
        let mut lock = CHILDREN.lock().unwrap();
        let mut n = 0;
        for (id, c) in lock.1.iter_mut() {
            if let Ok(Some(_)) = c.try_wait() {
                deads.push(id.clone());
                n += 1;
            }
        }
        for ref id in deads.drain(..) {
            lock.1.remove(id);
        }
        if n > 0 {
            lock.0 = true;
        }
        drop(lock);
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}
// Make sure `SENDER` is inited here.
#[inline]
#[cfg(not(any(target_os = "android", target_os = "ios")))]
pub fn start_option_status_sync() {
    let _sender = SENDER.lock().unwrap();
}

// not call directly
#[cfg(not(any(target_os = "android", target_os = "ios")))]
fn check_connect_status(reconnect: bool) -> mpsc::UnboundedSender<ipc::Data> {
    let (tx, rx) = mpsc::unbounded_channel::<ipc::Data>();
    std::thread::spawn(move || check_connect_status_(reconnect, rx));
    tx
}

#[cfg(feature = "flutter")]
pub fn set_user_default_option(key: String, value: String) {
    use hbb_common::config::UserDefaultConfig;
    UserDefaultConfig::load().set(key, value);
}

#[cfg(feature = "flutter")]
pub fn get_user_default_option(key: String) -> String {
    use hbb_common::config::UserDefaultConfig;
    UserDefaultConfig::load().get(&key)
}

pub fn get_fingerprint() -> String {
    #[cfg(any(target_os = "android", target_os = "ios"))]
    if Config::get_key_confirmed() {
        return "".to_owned();
    } else {
        return "".to_owned();
    }
    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    return ipc::get_fingerprint();
}

#[inline]
pub fn get_login_device_info() -> LoginDeviceInfo {
    LoginDeviceInfo {
        // std::env::consts::OS is better than whoami::platform() here.
        os: std::env::consts::OS.to_owned(),
        r#type: "client".to_owned(),
        name: crate::common::hostname(),
    }
}

#[inline]
pub fn get_login_device_info_json() -> String {
    serde_json::to_string(&get_login_device_info()).unwrap_or("{}".to_string())
}

// notice: avoiding create ipc connection repeatedly,
// because windows named pipe has serious memory leak issue.
#[cfg(not(any(target_os = "android", target_os = "ios")))]
#[tokio::main(flavor = "current_thread")]
async fn check_connect_status_(reconnect: bool, rx: mpsc::UnboundedReceiver<ipc::Data>) {
	#[cfg(not(feature = "flutter"))]
    let mut key_confirmed = false;
    let mut rx = rx;
    let mut mouse_time = 0;
    #[cfg(feature = "flutter")]
    let mut video_conn_count = 0;
    #[cfg(not(feature = "flutter"))]
    let mut id = "".to_owned();
    #[cfg(target_os = "windows")]
    let mut enable_file_transfer = "".to_owned();
    let is_cm = crate::common::is_cm();

    loop {
        if let Ok(mut c) = ipc::connect(1000, "").await {
            let mut timer = crate::rustdesk_interval(time::interval(time::Duration::from_secs(1)));
            loop {
                tokio::select! {
                    res = c.next() => {
                        match res {
                            Err(err) => {
                                log::error!("ipc connection closed: {}", err);
                                if is_cm {
                                    crate::ui_cm_interface::quit_cm();
                                }
                                break;
                            }
                            #[cfg(not(any(target_os = "android", target_os = "ios")))]
                            Ok(Some(ipc::Data::MouseMoveTime(v))) => {
                                mouse_time = v;
                                UI_STATUS.lock().unwrap().mouse_time = v;
                            }
                            Ok(Some(ipc::Data::Options(Some(v)))) => {
                                *OPTIONS.lock().unwrap() = v;
                                *OPTION_SYNCED.lock().unwrap() = true;
                                migrate_auto_record_outgoing().await;

                                #[cfg(target_os = "windows")]
                                {
                                    let b = OPTIONS.lock().unwrap().get(OPTION_ENABLE_FILE_TRANSFER).map(|x| x.to_string()).unwrap_or_default();
                                    if b != enable_file_transfer {
                                        clipboard::ContextSend::enable(hbb_common::config::option2bool(OPTION_ENABLE_FILE_TRANSFER, &b));
                                        enable_file_transfer = b;
                                    }
                                }
                            }
                            Ok(Some(ipc::Data::Config((name, Some(value))))) => {
                                if name == "id" {
                                    #[cfg(not(feature = "flutter"))]
                                    {
                                        id = value;
                                    }
                                } else if name == "temporary-password" {
                                    *TEMPORARY_PASSWD.lock().unwrap() = value;
                                }
                            }
                            #[cfg(feature = "flutter")]
                            Ok(Some(ipc::Data::VideoConnCount(Some(n)))) => {
                                video_conn_count = n;
                            }
                            Ok(Some(ipc::Data::OnlineStatus(Some((mut x, _c))))) => {
								if x > 0 {
                                    x = 1
                                }
                                #[cfg(not(feature = "flutter"))]
                                {
                                    key_confirmed = _c;
                                }
                                *UI_STATUS.lock().unwrap() = UiStatus {
                                    status_num: x as _,
                                    #[cfg(not(feature = "flutter"))]
                                    key_confirmed: _c,
                                    #[cfg(not(any(target_os = "android", target_os = "ios")))]
                                    mouse_time,
                                    #[cfg(not(feature = "flutter"))]
                                    id: id.clone(),
                                    #[cfg(feature = "flutter")]
                                    video_conn_count,
                                };
                            }
                            _ => {}
                        }
                    }
                    Some(data) = rx.recv() => {
                        allow_err!(c.send(&data).await);
                    }
                    _ = timer.tick() => {
                        c.send(&ipc::Data::OnlineStatus(None)).await.ok();
                        c.send(&ipc::Data::Options(None)).await.ok();
                        c.send(&ipc::Data::Config(("id".to_owned(), None))).await.ok();
                        c.send(&ipc::Data::Config(("temporary-password".to_owned(), None))).await.ok();
                        #[cfg(feature = "flutter")]
                        c.send(&ipc::Data::VideoConnCount(None)).await.ok();
                    }
                }
            }
        }
        if !reconnect {
            OPTIONS
                .lock()
                .unwrap()
                .insert("ipc-closed".to_owned(), "Y".to_owned());
            break;
        }
        *UI_STATUS.lock().unwrap() = UiStatus {
            status_num: -1,
            #[cfg(not(feature = "flutter"))]
            key_confirmed,
            #[cfg(not(any(target_os = "android", target_os = "ios")))]
            mouse_time,
            #[cfg(not(feature = "flutter"))]
            id: id.clone(),
            #[cfg(feature = "flutter")]
            video_conn_count,
        };
        sleep(1.).await;
    }
}

#[allow(dead_code)]
pub fn option_synced() -> bool {
    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    {
        OPTION_SYNCED.lock().unwrap().clone()
    }
    #[cfg(any(target_os = "android", target_os = "ios"))]
    {
        true
    }
}

#[cfg(any(target_os = "android", feature = "flutter"))]
#[cfg(not(any(target_os = "ios")))]
#[tokio::main(flavor = "current_thread")]
pub(crate) async fn send_to_cm(data: &ipc::Data) {
    if let Ok(mut c) = ipc::connect(1000, "_cm").await {
        c.send(data).await.ok();
    }
}


#[inline]
#[cfg(any(target_os = "android", target_os = "ios", feature = "flutter"))]
#[tokio::main(flavor = "current_thread")]
pub async fn change_id_shared(id: String, old_id: String) -> String {
    let res = change_id_shared_(id, old_id).await.to_owned();
    *ASYNC_JOB_STATUS.lock().unwrap() = res.clone();
    res
}

#[cfg(any(target_os = "android", target_os = "ios", feature = "flutter"))]
pub async fn change_id_shared_(id: String, _old_id: String) -> &'static str {
    hbb_common::api::erase_api().await;
    Config::set_id(&id);
    let err: &'static str = "";
    err    
}
// if it's relay id, return id processed, otherwise return original id
pub fn handle_relay_id(id: &str) -> &str {
    if id.ends_with(r"\r") || id.ends_with(r"/r") {
        &id[0..id.len() - 2]
    } else {
        id
    }
}

pub fn support_remove_wallpaper() -> bool {
    #[cfg(any(target_os = "windows", target_os = "linux", target_os = "freebsd"))]
    return crate::platform::WallPaperRemover::support();
    #[cfg(not(any(target_os = "windows", target_os = "linux", target_os = "freebsd")))]
    return false;
}

pub fn has_valid_2fa() -> bool {
    let raw = get_option("2fa");
    crate::auth_2fa::get_2fa(Some(raw)).is_some()
}

pub fn generate2fa() -> String {
    crate::auth_2fa::generate2fa()
}

pub fn verify2fa(code: String) -> bool {
    let res = crate::auth_2fa::verify2fa(code);
    if res {
        refresh_options();
    }
    res
}

#[cfg(feature = "flutter")]
pub fn has_valid_bot() -> bool {
    crate::auth_2fa::TelegramBot::get().map_or(false, |bot| bot.is_some())
}

#[cfg(feature = "flutter")]
pub fn verify_bot(token: String) -> String {
    match crate::auth_2fa::get_chatid_telegram(&token) {
        Err(err) => err.to_string(),
        Ok(None) => {
            "To activate the bot, simply send a message beginning with a forward slash (\"/\") like \"/hello\" to its chat.".to_owned()
        }
        _ => "".to_owned(),
    }
}

pub fn check_hwcodec() {
    #[cfg(feature = "nativecodec")]
    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    {
        use std::sync::Once;
        static ONCE: Once = Once::new();

        ONCE.call_once(|| {
            if crate::platform::is_installed() {
                ipc::notify_server_to_check_hwcodec().ok();
                ipc::client_get_hwcodec_config_thread(3);
            } else {
                scrap::hwcodec::start_check_process();
            }
        })
    }
}


#[cfg(feature = "flutter")]
pub fn get_unlock_pin() -> String {
    #[cfg(any(target_os = "android", target_os = "ios"))]
    return String::default();
    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    return ipc::get_unlock_pin();
}

#[cfg(feature = "flutter")]
pub fn set_unlock_pin(_pin: String) -> String {
    #[cfg(any(target_os = "android", target_os = "ios"))]
    return String::default();
    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    match ipc::set_unlock_pin(_pin, true) {
        Ok(_) => String::default(),
        Err(err) => err.to_string(),
    }
}

#[cfg(feature = "flutter")]
pub fn get_trusted_devices() -> String {
    #[cfg(any(target_os = "android", target_os = "ios"))]
    return Config::get_trusted_devices_json();
    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    return ipc::get_trusted_devices();
}

#[cfg(feature = "flutter")]
pub fn remove_trusted_devices(json: &str) {
    let hwids = serde_json::from_str::<Vec<Bytes>>(json).unwrap_or_default();
    #[cfg(any(target_os = "android", target_os = "ios"))]
    Config::remove_trusted_devices(&hwids);
    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    ipc::remove_trusted_devices(hwids);
}

#[cfg(feature = "flutter")]
pub fn clear_trusted_devices() {
    #[cfg(any(target_os = "android", target_os = "ios"))]
    Config::clear_trusted_devices();
    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    ipc::clear_trusted_devices();
}

#[cfg(feature = "flutter")]
pub fn max_encrypt_len() -> usize {
    hbb_common::config::ENCRYPT_MAX_LEN
}


#[allow(clippy::too_many_arguments)]
#[cfg(not(any(target_os = "android", target_os = "ios")))]
pub async fn process_invite_request(target_peer_id: String, self_id: String, invite_password: String) -> ResultType<()> {
            log::info!(
        "process_invite_request: Target Peer ID: {}, Self ID: {}",target_peer_id,self_id);

    let (mut secured_stream, _peer_pk) = 
        match crate::client::Client::establish_secure_stream_for_invite(&target_peer_id).await {
            Ok((stream, pk)) => {
                log::info!("Successfully established secure stream to {}", target_peer_id);
                (stream, pk)
            }
            Err(e) => {
                log::error!("Failed to establish secure stream to {}: {:?}", target_peer_id, e);
                return Err(e.context(format!("Failed to establish secure stream to {}", target_peer_id)));
            }
        };

    match time::timeout(Duration::from_secs(5), secured_stream.next()).await {
                                Ok(Some(Ok(bytes))) => {
            match Message::parse_from_bytes(&bytes) {
                Ok(_initial_msg) => {
                                        }
                                        Err(_e) => {
                                        }
                                    }
                                }
                                Ok(Some(Err(_e))) => {

                                }
                                Ok(None) => {
            return Err(anyhow!("Stream closed by peer before InviteRequest could be processed"));
        }
        Err(_) => {
            log::warn!("Timeout waiting for initial message from server");
        }
    }

    let self_name = Config::get_option("name");
    let final_self_name = if self_name.is_empty() { 
        log::warn!("Inviter name is not set in config, using 'Unknown User'");
		let username = crate::username();
		if username.is_empty() {
			"Unknown User".to_string()
		} else {
			username
		}
    } else {
        self_name 
    };
    
    let mut invite_req_proto = InviteRequest::new();
    invite_req_proto.from_id = self_id.clone();
    invite_req_proto.from_name = final_self_name;
    invite_req_proto.password_to_connect = invite_password.clone();

    let mut msg_out = Message::new();
    msg_out.union = Some(message::Union::InviteRequest(invite_req_proto));

    if let Err(e) = secured_stream.send(&msg_out).await { 
        return Err(anyhow!("Failed to send InviteRequest: {}", e));
    }

    let deadline = Instant::now() + Duration::from_secs(120);
    while Instant::now() < deadline {
        match secured_stream.next_timeout(15000).await {
            Some(Ok(bytes)) => match Message::parse_from_bytes(&bytes) {
                Ok(msg) => match msg.union {
                    Some(message::Union::InviteResponse(ir)) => {
                        log::info!("Invite to {} answered, accepted: {}", target_peer_id, ir.accepted);
                        return Ok(());
                    }
                    Some(message::Union::TestDelay(t)) => {
                        let mut msg_out = Message::new();
                        msg_out.set_test_delay(t);
                        allow_err!(secured_stream.send(&msg_out).await);
                    }
                    _ => {}
                },
                Err(_) => {}
            },
            Some(Err(e)) => return Err(e.into()),
            None => break,
        }
    }
    Err(anyhow!("Timed out waiting for response"))
}

#[cfg(test)]
mod recordings_dir_tests {
    use super::choose_recordings_dir;
    use std::io::Write;

    fn scratch(tag: &str) -> std::path::PathBuf {
        let n = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let p = std::env::temp_dir().join(format!("htd-rec-{}-{}-{}", tag, std::process::id(), n));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    fn touch(dir: &std::path::Path, name: &str) {
        std::fs::create_dir_all(dir).unwrap();
        let mut f = std::fs::File::create(dir.join(name)).unwrap();
        f.write_all(b"x").unwrap();
        f.sync_all().unwrap();
    }

    #[test]
    fn prefers_the_root_dir_when_only_it_holds_recordings() {
        let t = scratch("only-root");
        let (root, user) = (t.join("root"), t.join("user"));
        std::fs::create_dir_all(&user).unwrap();
        touch(&root, "a.mp4");
        assert_eq!(
            choose_recordings_dir(root.to_str().unwrap(), user.to_str().unwrap()),
            root.to_str().unwrap()
        );
        std::fs::remove_dir_all(&t).ok();
    }

    #[test]
    fn keeps_the_user_dir_when_it_holds_the_newer_recording() {
        let t = scratch("newer-user");
        let (root, user) = (t.join("root"), t.join("user"));
        touch(&root, "old.mp4");
        std::thread::sleep(std::time::Duration::from_millis(1100));
        touch(&user, "new.mp4");
        assert_eq!(
            choose_recordings_dir(root.to_str().unwrap(), user.to_str().unwrap()),
            user.to_str().unwrap()
        );
        std::fs::remove_dir_all(&t).ok();
    }

    #[test]
    fn falls_back_to_the_user_dir_when_nothing_is_recorded() {
        let t = scratch("empty");
        let (root, user) = (t.join("root"), t.join("user"));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&user).unwrap();
        assert_eq!(
            choose_recordings_dir(root.to_str().unwrap(), user.to_str().unwrap()),
            user.to_str().unwrap()
        );
        std::fs::remove_dir_all(&t).ok();
    }
}
