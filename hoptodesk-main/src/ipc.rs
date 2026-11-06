use crate::privacy_mode::PrivacyModeState;
#[cfg(not(target_os = "ios"))]
use crate::ui_interface::{get_local_option, set_local_option};
use bytes::Bytes;
#[cfg(not(target_os = "ios"))]
use parity_tokio_ipc::{
    Connection as Conn, ConnectionClient as ConnClient, Endpoint, Incoming, SecurityAttributes,
};
use serde_derive::{Deserialize, Serialize};
use std::collections::HashMap;
#[cfg(not(target_os = "ios"))]
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(not(windows))]
#[cfg(not(target_os = "ios"))]
use std::{fs::File, io::prelude::*};

#[cfg(all(feature = "flutter", feature = "plugin_framework"))]
#[cfg(not(any(target_os = "android", target_os = "ios")))]
use crate::plugin::ipc::Plugin;
#[cfg(not(any(target_os = "android", target_os = "ios")))]
pub use clipboard::ClipboardFile;
#[cfg(not(target_os = "ios"))]
use hbb_common::{
    api,
    allow_err, bail, bytes,
    bytes_codec::BytesCodec,
    config::{self, Config, Config2},
    futures::StreamExt as _,
    futures_util::sink::SinkExt,
    log, password_security as password, timeout,
    tokio::{
        self,
        io::{AsyncRead, AsyncWrite},
    },
    tokio_util::codec::Framed,
    ResultType,
};
#[cfg(target_os = "ios")]
use hbb_common::{
    bytes,
    config::{self, Config, Config2},
};

#[cfg(not(target_os = "ios"))]
use crate::{common::is_server, privacy_mode, rendezvous_mediator::RendezvousMediator};

#[cfg(any(target_os = "windows", target_os = "linux", target_os = "macos"))]
#[path = "ipc/auth.rs"]
mod ipc_auth;

#[cfg(all(target_os = "linux", feature = "drm"))]
#[path = "ipc/drm.rs"]
mod ipc_drm;

#[cfg(all(target_os = "linux", feature = "drm"))]
pub use ipc_drm::{start_drm, DmabufDesc, DrmDisplayInfo};

#[cfg(all(target_os = "linux", feature = "drm"))]
pub(crate) use ipc_drm::connect_drm;

#[cfg(all(target_os = "linux", feature = "drm"))]
pub(crate) use ipc_auth::authorize_service_scoped_ipc_connection;

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) use ipc_auth::peer_uid_from_fd;

// IPC actions here.
#[cfg(not(target_os = "ios"))]
pub const IPC_ACTION_CLOSE: &str = "close";
#[cfg(not(target_os = "ios"))]
pub static EXIT_RECV_CLOSE: AtomicBool = AtomicBool::new(true);

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(tag = "t", content = "c")]
pub enum FS {
    ReadEmptyDirs {
        dir: String,
        include_hidden: bool,
    },
    ReadDir {
        dir: String,
        include_hidden: bool,
    },
    SearchDir {
        id: i32,
        dir: String,
        pattern: String,
        include_hidden: bool,
    },
    RemoveDir {
        path: String,
        id: i32,
        recursive: bool,
    },
    RemoveFile {
        path: String,
        id: i32,
        file_num: i32,
    },
    CreateDir {
        path: String,
        id: i32,
    },
    NewWrite {
        path: String,
        id: i32,
        file_num: i32,
        files: Vec<(String, u64)>,
        overwrite_detection: bool,
        total_size: u64,
        conn_id: i32,
        is_bundle: bool,
    },
    CancelWrite {
        id: i32,
    },
    WriteBlock {
        id: i32,
        file_num: i32,
        data: Bytes,
        compressed: bool,
    },
    WriteDone {
        id: i32,
        file_num: i32,
    },
    WriteError {
        id: i32,
        file_num: i32,
        err: String,
    },
    WriteOffset {
        id: i32,
        file_num: i32,
        offset_blk: u32,
    },
    CheckDigest {
        id: i32,
        file_num: i32,
        file_size: u64,
        last_modified: u64,
        is_upload: bool,
        is_resume: bool,
    },
    SendConfirm(Vec<u8>),
    Rename {
        id: i32,
        path: String,
        new_name: String,
    },
}

#[cfg(target_os = "windows")]
#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(tag = "t")]
pub struct ClipboardNonFile {
    pub compress: bool,
    pub content: bytes::Bytes,
    pub content_len: usize,
    pub next_raw: bool,
    pub width: i32,
    pub height: i32,
    // message.proto: ClipboardFormat
    pub format: i32,
    pub special_name: String,
}

#[cfg(not(any(target_os = "android", target_os = "ios")))]
#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(tag = "t", content = "c")]
pub enum DataKeyboard {
    Sequence(String),
    KeyDown(enigo::Key),
    KeyUp(enigo::Key),
    KeyClick(enigo::Key),
    GetKeyState(enigo::Key),
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(tag = "t", content = "c")]
pub enum DataKeyboardResponse {
    GetKeyState(bool),
}

#[cfg(not(any(target_os = "android", target_os = "ios")))]
#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(tag = "t", content = "c")]
pub enum DataMouse {
    MoveTo(i32, i32),
    MoveRelative(i32, i32),
    Down(enigo::MouseButton),
    Up(enigo::MouseButton),
    Click(enigo::MouseButton),
    ScrollX(i32),
    ScrollY(i32),
    Refresh,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(tag = "t", content = "c")]
pub enum DataControl {
    Resolution {
        minx: i32,
        maxx: i32,
        miny: i32,
        maxy: i32,
    },
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(tag = "t", content = "c")]
pub enum DataPortableService {
    Ping,
    Pong,
    ConnCount(Option<usize>),
    Mouse((Vec<u8>, i32, String, u32, bool, bool)),
    Pointer((Vec<u8>, i32)),
    Key(Vec<u8>),
    RequestStart,
    WillClose,
    CmShowElevation(bool),
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(tag = "t", content = "c")]
pub enum Data {
    Login {
        id: i32,
        is_file_transfer: bool,
        is_view_camera: bool,
        is_terminal: bool,
        peer_id: String,
        name: String,
        authorized: bool,
        port_forward: String,
        keyboard: bool,
        clipboard: bool,
        audio: bool,
        file: bool,
        file_transfer_enabled: bool,
        restart: bool,
        recording: bool,
        block_input: bool,
        from_switch: bool,        
        is_invite: bool,
        password_to_connect_to_inviter: String,
        security_numbers: String,
        secured: bool,
        avatar_image: String,
        guest_timeout: u32,
        guest_auto_accept: bool,
    },
    ChatMessage {
        text: String,
    },
    FileOffer {
        path: String,
        name: String,
        size: u64,
    },
    FileOfferReceived,
    TechIdentity {
        id: i32,
        verified: bool,
        display_name: String,
        org_name: String,
        png: String,
    },
    SwitchPermission {
        name: String,
        enabled: bool,
    },
    SystemInfo(Option<String>),
    ClickTime(i64),
    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    MouseMoveTime(i64),
    Authorize,
    SessionIssue(String),
    Close,
    CloseWithReason(String),
    CloseIncoming {
        id: String,
    },
    #[cfg(not(any(target_os = "android", target_os = "ios")))]
	ListSessions {
        id: String,
    },
    #[cfg(windows)]
    SAS,
    UserSid(Option<u32>),
    OnlineStatus(Option<(i64, bool)>),
    Config((String, Option<String>)),
    Options(Option<HashMap<String, String>>),
    NatType(Option<i32>),
    RawMessage(Vec<u8>),
    Socks(Option<config::Socks5Server>),
    FS(FS),
    Test,
    SyncConfig(Option<(Config, Config2)>),
    #[cfg(windows)]
    ClipboardFile(ClipboardFile),
    ClipboardFileEnabled(bool),
    #[cfg(target_os = "windows")]
    ClipboardNonFile(Option<(String, Vec<ClipboardNonFile>)>),
    PrivacyModeState((i32, PrivacyModeState, String)),
    TestRendezvousServer,
    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    Keyboard(DataKeyboard),
    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    KeyboardResponse(DataKeyboardResponse),
    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    Mouse(DataMouse),
    Control(DataControl),
    Theme(String),
    Language(String),
    Empty,
    Disconnected,
    DataPortableService(DataPortableService),
    SwitchSidesRequest(String),
    ShowMyScreenRequest(String),
    SwitchSidesReturn(String),
    SwitchSidesAuth(String),
    SwitchSidesBack,
    UrlLink(String),
    VoiceCallIncoming,
    StartVoiceCall,
    VoiceCallResponse(bool),
    CloseVoiceCall(String),
    LinkDashboardIncoming {
        account_name: String,
        existing_account_name: String,
    },
    LinkDashboardResponse(bool),
    LinkDashboardTimeout,
    DashboardRelinked,
    #[cfg(all(feature = "flutter", feature = "plugin_framework"))]
    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    Plugin(Plugin),
    #[cfg(windows)]
    SyncWinCpuUsage(Option<f64>),
    FileTransferLog((String, String)),
    #[cfg(windows)]
    ControlledSessionCount(usize),
    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    TicketReplyCounter(u64),
    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    AiActivity(String),
    CmErr(String),
    CheckHwcodec,
    #[cfg(feature = "flutter")]
    VideoConnCount(Option<usize>),
    // Although the key is not neccessary, it is used to avoid hardcoding the key.
    WaylandScreencastRestoreToken((String, String)),
    HwCodecConfig(Option<String>),
    RemoveTrustedDevices(Vec<Bytes>),
    ClearTrustedDevices,
    IncomingInvite {
        from_id: String,
        from_name: String,
        password_to_connect: String,
        original_connection_id: i32,
    },
    InviteResponse {
        id: i32,
        accepted: bool,
    },
    Invite(String, String, String),
    RespondToClientInvite {
        original_connection_id: i32,
        accepted: bool,
        reason: Option<String>,
    },
    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    Whiteboard((String, crate::whiteboard::CustomEvent)),
    #[cfg(any(target_os = "windows", target_os = "macos", target_os = "linux", target_os = "freebsd"))]
    PrinterData(Vec<u8>),
    #[cfg(target_os = "linux")]
    Reboot { done: bool, error: Option<String> },
    #[cfg(target_os = "linux")]
    PrivacyBacklight { blank: bool, block_input: bool },
    #[cfg(target_os = "linux")]
    RestartService { name: String, output: Option<String>, error: Option<String> },
    #[cfg(all(target_os = "linux", feature = "drm"))]
    DrmHello { wake: bool },
    #[cfg(all(target_os = "linux", feature = "drm"))]
    DrmStart { display: i32, need_cpu: bool },
    #[cfg(all(target_os = "linux", feature = "drm"))]
    DrmDisplayList(Vec<DrmDisplayInfo>),
    #[cfg(all(target_os = "linux", feature = "drm"))]
    DrmDisplaysChanged(Vec<DrmDisplayInfo>),
    #[cfg(all(target_os = "linux", feature = "drm"))]
    DrmFrame { width: u32, height: u32 },
    #[cfg(all(target_os = "linux", feature = "drm"))]
    DrmFrameDmabuf(DmabufDesc),
    #[cfg(all(target_os = "linux", feature = "drm"))]
    DrmCursor {
        id: u64,
        width: u32,
        height: u32,
        hotx: i32,
        hoty: i32,
    },
    #[cfg(windows)]
    DashboardRelinkInvite(String),
    #[cfg(windows)]
    TamperAttempt(String),
    #[cfg(not(any(target_os = "android", target_os = "ios", feature = "cli", feature = "flutter")))]
    KvmLinkToken(String),
    #[cfg(not(any(target_os = "android", target_os = "ios", feature = "cli", feature = "flutter")))]
    KvmLinkOwned(Option<bool>),
}

// ── IPC transport layer ── (not available on iOS) ──────────────────────
#[cfg(not(target_os = "ios"))]
#[tokio::main(flavor = "current_thread")]
pub async fn start(postfix: &str) -> ResultType<()> {
    let mut incoming = new_listener(postfix).await?;
    loop {
        if let Some(result) = incoming.next().await {
            match result {
                Ok(stream) => {
                    let mut stream = Connection::new(stream);
                    #[cfg(any(
                        target_os = "windows",
                        target_os = "linux",
                        target_os = "macos"
                    ))]
                    if ipc_auth::peer_is_another_executable(
                        stream.peer_pid(),
                        stream.peer_uid(),
                        postfix,
                    ) {
                        continue;
                    }
                    #[cfg(any(
                        target_os = "windows",
                        target_os = "linux",
                        target_os = "macos"
                    ))]
                    let trusted = stream.peer_is_trusted();
                    #[cfg(not(any(
                        target_os = "windows",
                        target_os = "linux",
                        target_os = "macos"
                    )))]
                    let trusted = true;
                    let postfix = postfix.to_owned();
                    tokio::spawn(async move {
                        #[cfg(target_os = "linux")]
                        let mut owns_backlight_blank = false;
                        loop {
                            match stream.next().await {
                                Err(err) => {
                                    log::trace!("ipc '{}' connection closed: {}", postfix, err);
                                    #[cfg(target_os = "linux")]
                                    if owns_backlight_blank
                                        && crate::privacy_mode::linux_backlight::is_blanked()
                                    {
                                        log::info!(
                                            "privacy: restoring the backlight because the ipc '{}' peer that requested the blank went away",
                                            postfix
                                        );
                                        crate::privacy_mode::linux_backlight::restore();
                                    }
                                    break;
                                }
                                Ok(Some(data)) => {
                                    #[cfg(target_os = "linux")]
                                    let is_blank_request = matches!(
                                        &data,
                                        Data::PrivacyBacklight { blank: true, .. }
                                    );
                                    handle(data, &mut stream, trusted).await;
                                    #[cfg(target_os = "linux")]
                                    if is_blank_request {
                                        owns_backlight_blank =
                                            crate::privacy_mode::linux_backlight::is_blanked();
                                    }
                                }
                                _ => {}
                            }
                        }
                    });
                }
                Err(err) => {
                    log::error!("Couldn't get client: {:?}", err);
                }
            }
        }
    }
}

#[cfg(not(target_os = "ios"))]
pub async fn new_listener(postfix: &str) -> ResultType<Incoming> {
    let path = Config::ipc_path(postfix);
    #[cfg(not(any(windows, target_os = "android", target_os = "ios")))]
    check_pid(postfix).await;
    let mut endpoint = Endpoint::new(path.clone());
    match SecurityAttributes::allow_everyone_create() {
        Ok(attr) => endpoint.set_security_attributes(attr),
        Err(err) => log::error!("Failed to set ipc{} security: {}", postfix, err),
    };
    match endpoint.incoming() {
        Ok(incoming) => {
            log::info!("Started ipc{} server at path: {}", postfix, &path);
            #[cfg(not(windows))]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o0777)).ok();
                write_pid(postfix);
            }
            Ok(incoming)
        }
        Err(err) => {
            log::error!(
                "Failed to start ipc{} server at path {}: {}",
                postfix,
                path,
                err
            );
            Err(err.into())
        }
    }
}

#[cfg(not(target_os = "ios"))]
pub struct CheckIfRestart(String, Option<Vec<String>>, String, String);

#[cfg(not(target_os = "ios"))]
impl CheckIfRestart {
    pub async fn new() -> Self {
        Self(
            Config::get_option("stop-service"),
            Config::get_rendezvous_servers().await,
            Config::get_option("audio-input"),
            Config::get_option("voice-call-input"),
        )
    }

    pub async fn check(&self) {
        if self.0 != Config::get_option("stop-service")
            || self.1 != Config::get_rendezvous_servers().await
        {
            RendezvousMediator::restart();
        }
        if self.2 != Config::get_option("audio-input") {
            crate::audio_service::restart();
        }
        if self.3 != Config::get_option("voice-call-input") {
            crate::audio_service::set_voice_call_input_device(
                Some(Config::get_option("voice-call-input")),
                true,
            )
        }
    }
}

#[cfg(not(target_os = "ios"))]
pub async fn notify_dashboard_relinked() -> ResultType<()> {
    let mut conn = connect(1000, "").await?;
    conn.send(&Data::DashboardRelinked).await?;
    Ok(())
}

#[cfg(windows)]
pub async fn notify_dashboard_relink_invite(invite_code: &str) -> ResultType<()> {
    let mut conn = connect(1000, "").await?;
    conn.send(&Data::DashboardRelinkInvite(invite_code.to_owned()))
        .await?;
    Ok(())
}

#[cfg(not(target_os = "ios"))]
const PUBLIC_CONFIG: [&str; 7] = [
    "id",
    "salt",
    "rendezvous_server",
    "rendezvous_servers",
    "hide_cm",
    "voice-call-input",
    "custom-api-url",
];

#[cfg(not(target_os = "ios"))]
const SECRET_OPTIONS: [&str; 11] = [
    "dashboard-device-key",
    "dashboard-log-token",
    "dashboard_user_id",
    "avatar-mint-secret",
    "2fa",
    "bot",
    "tamper-code-hash",
    "tamper-code-salt",
    "pending_quick_connect_token",
    "invite_code",
    "last_enrolled_invite_code",
];

#[cfg(not(target_os = "ios"))]
fn open_to_any_peer(data: &Data) -> bool {
    match data {
        Data::Config((name, None)) => PUBLIC_CONFIG.contains(&name.as_str()),
        Data::Options(None)
        | Data::Socks(None)
        | Data::OnlineStatus(_)
        | Data::NatType(_)
        | Data::SystemInfo(_)
        | Data::ClickTime(_)
        | Data::Close => true,
        #[cfg(not(any(target_os = "android", target_os = "ios")))]
        Data::MouseMoveTime(_) => true,
        #[cfg(windows)]
        Data::SyncWinCpuUsage(None) => true,
        #[cfg(feature = "nativecodec")]
        Data::HwCodecConfig(None) => true,
        _ => false,
    }
}

#[cfg(not(target_os = "ios"))]
async fn refuse(data: Data, stream: &mut Connection) {
    if let Data::Config((name, None)) = data {
        allow_err!(stream.send(&Data::Config((name, None))).await);
        return;
    }
    let request = format!("{:?}", data);
    log::warn!(
        "IPC: refused {} from a peer outside this session",
        request.split(|c: char| !c.is_alphanumeric()).next().unwrap_or_default()
    );
    match data {
        Data::Options(Some(_)) => allow_err!(stream.send(&Data::Options(None)).await),
        Data::SyncConfig(_) => allow_err!(stream.send(&Data::SyncConfig(None)).await),
        _ => {}
    }
}

#[cfg(not(target_os = "ios"))]
async fn handle(data: Data, stream: &mut Connection, trusted: bool) {
    if !trusted && !open_to_any_peer(&data) {
        refuse(data, stream).await;
        return;
    }
    match data {
        #[cfg(not(any(target_os = "android", target_os = "ios", feature = "cli", feature = "flutter")))]
        Data::KvmLinkToken(target) => {
            let token = if crate::is_server() { crate::kvm::link::token_for(&target, stream.peer_pid()) } else { String::new() };
            allow_err!(stream.send(&Data::KvmLinkToken(token)).await);
        }
        #[cfg(not(any(target_os = "android", target_os = "ios", feature = "cli", feature = "flutter")))]
        Data::KvmLinkOwned(_) => {
            allow_err!(stream.send(&Data::KvmLinkOwned(crate::kvm::link::owns(stream.peer_pid()))).await);
        }
        Data::DashboardRelinked => {
            #[cfg(not(any(target_os = "android", target_os = "ios")))]
            {
                log::info!("IPC: DashboardRelinked received, starting dashboard");
                std::thread::spawn(|| crate::dashboard::start());
            }
        }
        #[cfg(windows)]
        Data::DashboardRelinkInvite(code) => {
            let code = code.trim().to_owned();
            if crate::is_server() && !code.is_empty() && !crate::dashboard::is_linked() {
                let path = Config::shared_path("InviteCode.toml");
                if !path.exists() {
                    if let Some(parent) = path.parent() {
                        let _ = std::fs::create_dir_all(parent);
                    }
                    if let Err(e) = std::fs::write(&path, &code) {
                        log::warn!("IPC: DashboardRelinkInvite write failed: {}", e);
                    }
                }
            } else if crate::dashboard::is_linked() {
                log::info!("IPC: DashboardRelinkInvite ignored, service already linked");
            }
            std::thread::spawn(|| crate::dashboard::start());
        }
        #[cfg(windows)]
        Data::TamperAttempt(reason) => {
            if crate::is_server() {
                crate::dashboard::queue_tamper_attempt(&reason);
            }
        }
        Data::SystemInfo(_) => {
            let info = format!(
                "log_path: {}, config: {}, username: {}",
                Config::log_path().to_str().unwrap_or(""),
                Config::file().to_str().unwrap_or(""),
                crate::username(),
            );
            allow_err!(stream.send(&Data::SystemInfo(Some(info))).await);
        }
        Data::ClickTime(_) => {
            let t = crate::server::CLICK_TIME.load(Ordering::SeqCst);
            allow_err!(stream.send(&Data::ClickTime(t)).await);
        }
        #[cfg(not(any(target_os = "android", target_os = "ios")))]
        Data::MouseMoveTime(_) => {
            let t = crate::server::MOUSE_MOVE_TIME.load(Ordering::SeqCst);
            allow_err!(stream.send(&Data::MouseMoveTime(t)).await);
        }
        Data::Close => {
            log::info!("Receive close message");
            if EXIT_RECV_CLOSE.load(Ordering::SeqCst) {
                #[cfg(not(target_os = "android"))]
                crate::server::input_service::fix_key_down_timeout_at_exit();
                if is_server() {
                    let _ = privacy_mode::turn_off_privacy(0, Some(PrivacyModeState::OffByPeer));
                }
                #[cfg(any(target_os = "macos", target_os = "linux", target_os = "freebsd"))]
                if crate::is_main() {
                    // below part is for main windows can be reopen during rustdesk installation and installing service from UI
                    // this make new ipc server (domain socket) can be created.
                    std::fs::remove_file(&Config::ipc_path("")).ok();
                    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
                    {
                        hbb_common::sleep((crate::platform::SERVICE_INTERVAL * 2) as f32 / 1000.0)
                            .await;
                        crate::run_me::<&str>(vec![]).ok();
                    }
                    #[cfg(target_os = "macos")]
                    {
                        // our launchagent interval is 1 second
                        hbb_common::sleep(1.5).await;
                        std::process::Command::new("open")
                            .arg("-n")
                            .arg(&format!("/Applications/{}.app", crate::get_app_name()))
                            .spawn()
                            .ok();
                    }
                    // leave above open a little time
                    hbb_common::sleep(0.3).await;
                    // in case below exit failed
                    crate::platform::quit_gui();
                }
                std::process::exit(-1); // to make sure --server luauchagent process can restart because SuccessfulExit used
            }
        }
        Data::OnlineStatus(_) => {
            let x = config::get_online_state();
            let confirmed = Config::get_key_confirmed();
            allow_err!(stream.send(&Data::OnlineStatus(Some((x, confirmed)))).await);
        }
        Data::Socks(s) => match s {
            None => {
                allow_err!(stream.send(&Data::Socks(Config::get_socks())).await);
            }
            Some(data) => {
                if data.proxy.is_empty() {
                    Config::set_socks(None);
                } else {
                    Config::set_socks(Some(data));
                }
                RendezvousMediator::restart();
                log::info!("socks updated");
            }
        },
        #[cfg(feature = "flutter")]
        Data::VideoConnCount(None) => {
            let n = crate::server::AUTHED_CONNS
                .lock()
                .unwrap()
                .iter()
                .filter(|x| x.1 == crate::server::AuthConnType::Remote)
                .count();
            allow_err!(stream.send(&Data::VideoConnCount(Some(n))).await);
        }
        Data::Config((name, value)) => match value {
            None => {
                let value;
                if name == "id" {
                    value = Some(Config::get_id());
                } else if name == "temporary-password" {
					value = Some(password::temporary_password());
                } else if name == "permanent-password" {
                    value = Some(Config::get_permanent_password());
                } else if name == "guest-password" {
                    value = Some(Config::get_guest_password());
                } else if name == "salt" {
                    value = Some(Config::get_salt());
                } else if name == "password-file-transfer" {
                    let id_password = password::file_transfer_password();
                    value = Some(id_password.clone());
                } else if name == "rendezvous_server" {
                    value = Config::get_rendezvous_server().await;
                } else if name == "rendezvous_servers" {
                    value = Config::get_rendezvous_servers()
                        .await
                        .and_then(|v| Some(v.join(",")));
                } else if name == "hide_cm" {
                    value = None;
                } else if name == "voice-call-input" {
                    value = crate::audio_service::get_voice_call_input_device();
                } else if name == "custom-api-url" {
                    value = Some(get_option_async(&name).await)
                } else if name == "unlock-pin" {
                    value = Some(Config::get_unlock_pin());
                } else if name == "trusted-devices" {
                    value = Some(Config::get_trusted_devices_json());
                } else {
                    value = None;
                }
                allow_err!(stream.send(&Data::Config((name, value))).await);
            }
            Some(value) => {
                if name == "id" {
                    Config::set_key_confirmed(false);
                    Config::set_id(&value);
                } else if name == "temporary-password" {
					password::update_temporary_password();
                } else if name == "permanent-password" {
                    Config::set_permanent_password(&value);
                } else if name == "guest-password" {
                    Config::set_guest_password(&value);
                } else if name == "salt" {
                    Config::set_salt(&value);
                } else if name == "voice-call-input" {
                    crate::audio_service::set_voice_call_input_device(Some(value), true);
                } else if name == "unlock-pin" {
                    Config::set_unlock_pin(&value);
                } else if name == "password-file-transfer" {
                    password::update_file_transfer_password(value.clone());
                } else if name == "custom-api-url" {
                    set_option_async(&name, &value).await;
                    api::erase_api().await;
                    RendezvousMediator::restart()
                } else {
                    return;
                }
                log::info!("{} updated", name);
            }
        },
        Data::Options(value) => match value {
            None => {
                let mut v: HashMap<String, String> = Config::get_options();
                if !trusted {
                    v.retain(|key, _| !SECRET_OPTIONS.contains(&key.as_str()));
                }
                allow_err!(stream.send(&Data::Options(Some(v))).await);
            }
            Some(value) => {
                let chk = CheckIfRestart::new().await;
                if let Some(v) = value.get("privacy-mode-impl-key") {
                    crate::privacy_mode::switch(v);
                }
                let pre_opts = Config::get_options();
                #[cfg(windows)]
                let value = {
                    let mut value = value;
                    crate::dashboard::keep_service_owned_options(&mut value, &pre_opts);
                    value
                };
                let clears_dashboard_link = pre_opts
                    .get("dashboard_user_id")
                    .map(|v| !v.is_empty())
                    .unwrap_or(false)
                    && value
                        .get("dashboard_user_id")
                        .map(|v| v.is_empty())
                        .unwrap_or(true);
                Config::set_options(value);
                if clears_dashboard_link {
                    crate::dashboard::remove_enrollment_artifacts();
                }
                chk.check().await;
                allow_err!(stream.send(&Data::Options(None)).await);
            }
        },
        Data::NatType(_) => {
            let t = Config::get_nat_type();
            allow_err!(stream.send(&Data::NatType(Some(t))).await);
        }
        #[cfg(target_os = "linux")]
        Data::Reboot { done: false, .. } => {
            let error = if !reboot_request_allowed(stream.peer_uid()) {
                Some("Not allowed".to_owned())
            } else {
                match tokio::task::spawn_blocking(crate::platform::reboot_now).await {
                    Ok(Ok(())) => None,
                    Ok(Err(err)) => Some(match err.to_string() {
                        text if text.is_empty() => "Reboot failed".to_owned(),
                        text => text,
                    }),
                    Err(_) => Some("Reboot request failed".to_owned()),
                }
            };
            allow_err!(stream.send(&Data::Reboot { done: true, error }).await);
        }
        #[cfg(target_os = "linux")]
        Data::PrivacyBacklight { blank, block_input } => {
            let ok = if blank {
                match crate::privacy_mode::linux_backlight::blank(block_input) {
                    Ok(()) => true,
                    Err(err) => {
                        log::warn!("privacy backlight request failed: {}", err);
                        crate::privacy_mode::linux_backlight::restore();
                        false
                    }
                }
            } else {
                crate::privacy_mode::linux_backlight::restore();
                false
            };
            allow_err!(
                stream
                    .send(&Data::PrivacyBacklight {
                        blank: ok,
                        block_input,
                    })
                    .await
            );
        }
        #[cfg(target_os = "linux")]
        Data::RestartService { name, .. } => {
            // LAN-only build: remote service management is stripped.
            let (output, error) = (None, Some("Service request failed".to_string()));
            allow_err!(stream.send(&Data::RestartService { name, output, error }).await);
        }
        #[cfg(windows)]
        Data::SyncConfig(Some(_)) => {
            log::warn!("IPC: refused a full config replace");
            allow_err!(stream.send(&Data::SyncConfig(None)).await);
        }
        #[cfg(not(windows))]
        Data::SyncConfig(Some((config, config2))) => {
            let _chk = CheckIfRestart::new();
            let unattended_before = Config::get_option("unattended-access");
            Config::set(config);
            Config2::set(config2);
            Config::resave_if_unattended_changed(&unattended_before);
            allow_err!(stream.send(&Data::SyncConfig(None)).await);
        }
        Data::SyncConfig(None) => {
            allow_err!(
                stream
                    .send(&Data::SyncConfig(Some(
                        (Config::get(), Config2::get()).into()
                    )))
                    .await
            );
        }
        #[cfg(windows)]
        Data::SyncWinCpuUsage(None) => {
            allow_err!(
                stream
                    .send(&Data::SyncWinCpuUsage(
                        hbb_common::platform::windows::cpu_uage_one_minute()
                    ))
                    .await
            );
        }
        Data::TestRendezvousServer => {
            crate::test_rendezvous_server();
        }
        Data::SwitchSidesRequest(id) => {
            let uuid = uuid::Uuid::new_v4();
            crate::server::insert_switch_sides_uuid(id, uuid.clone(), crate::server::SwitchKind::Control);
            allow_err!(
                stream
                    .send(&Data::SwitchSidesRequest(uuid.to_string()))
                    .await
            );
        }
        Data::ShowMyScreenRequest(id) => {
            let uuid = uuid::Uuid::new_v4();
            crate::server::insert_switch_sides_uuid(id, uuid.clone(), crate::server::SwitchKind::ViewOnly);
            allow_err!(
                stream
                    .send(&Data::ShowMyScreenRequest(uuid.to_string()))
                    .await
            );
        }
        Data::SwitchSidesReturn(id) => {
            let uuid = crate::server::issue_switch_return_uuid(&id)
                .map(|uuid| uuid.to_string())
                .unwrap_or_default();
            allow_err!(stream.send(&Data::SwitchSidesReturn(uuid)).await);
        }
        Data::SwitchSidesAuth(peer_id) => {
            let granted = crate::server::outgoing_switch_allowed(&peer_id);
            allow_err!(
                stream
                    .send(&Data::SwitchSidesAuth(if granted {
                        "1".to_owned()
                    } else {
                        String::new()
                    }))
                    .await
            );
        }
        #[cfg(all(feature = "flutter", feature = "plugin_framework"))]
        #[cfg(not(any(target_os = "android", target_os = "ios")))]
        Data::Plugin(plugin) => crate::plugin::ipc::handle_plugin(plugin, stream).await,
        #[cfg(windows)]
        Data::ControlledSessionCount(_) => {
            allow_err!(
                stream
                    .send(&Data::ControlledSessionCount(
                        crate::Connection::alive_conns().len()
                    ))
                    .await
            );
        }
        #[cfg(not(any(target_os = "android", target_os = "ios")))]
        Data::TicketReplyCounter(_) => {
            allow_err!(
                stream
                    .send(&Data::TicketReplyCounter(
                        crate::dashboard::get_ticket_reply_counter()
                    ))
                    .await
            );
        }
        #[cfg(not(any(target_os = "android", target_os = "ios")))]
        Data::AiActivity(_) => {
            allow_err!(
                stream
                    .send(&Data::AiActivity(crate::dashboard::get_ai_activity()))
                    .await
            );
        }
        #[cfg(feature = "nativecodec")]
        #[cfg(not(any(target_os = "android", target_os = "ios")))]
        Data::CheckHwcodec => {
            scrap::hwcodec::start_check_process();
        }
        #[cfg(feature = "nativecodec")]
        #[cfg(not(any(target_os = "android", target_os = "ios")))]
        Data::HwCodecConfig(c) => {
            match c {
                None => {
                    let v = match scrap::hwcodec::HwCodecConfig::get_set_value() {
                        Some(v) => Some(serde_json::to_string(&v).unwrap_or_default()),
                        None => None,
                    };
                    allow_err!(stream.send(&Data::HwCodecConfig(v)).await);
                }
                Some(v) => {
                    // --server and portable
                    scrap::hwcodec::HwCodecConfig::set(v);
                }
            }
        }
        Data::WaylandScreencastRestoreToken((key, value)) => {
            let v = if value == "get" {
                let opt = get_local_option(key.clone());
                #[cfg(not(target_os = "linux"))]
                {
                    Some(opt)
                }
                #[cfg(target_os = "linux")]
                {
                    let v = if opt.is_empty() {
                        if scrap::wayland::pipewire::is_rdp_session_hold() {
                            "fake token".to_string()
                        } else {
                            "".to_owned()
                        }
                    } else {
                        opt
                    };
                    Some(v)
                }
            } else if value == "clear" {
                set_local_option(key.clone(), "".to_owned());
                #[cfg(target_os = "linux")]
                allow_err!(
                    hbb_common::tokio::task::spawn_blocking(
                        scrap::wayland::pipewire::close_session
                    )
                    .await
                );
                Some("".to_owned())
            } else {
                None
            };
            if let Some(v) = v {
                allow_err!(
                    stream
                        .send(&Data::WaylandScreencastRestoreToken((key, v)))
                        .await
                );
            }
        }
        Data::RemoveTrustedDevices(v) => {
            Config::remove_trusted_devices(&v);
        }
        Data::ClearTrustedDevices => {
            Config::clear_trusted_devices();
        }
        #[cfg(not(any(target_os = "android", target_os = "ios")))]
		Data::Invite(remote_id, self_id, password) => {
            tokio::spawn(async move {
                if let Err(err) =
                    crate::ui_interface::process_invite_request(remote_id, self_id, password).await
                {
                    log::error!("Failed to process invite request: {}", err);
                }
            });
        }
        _ => {}
    }
}

#[cfg(not(target_os = "ios"))]
pub async fn connect(ms_timeout: u64, postfix: &str) -> ResultType<ConnectionTmpl<ConnClient>> {
    let path = Config::ipc_path(postfix);
    let client = timeout(ms_timeout, Endpoint::connect(&path)).await??;
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        use std::os::unix::io::AsRawFd;
        let server_uid = ipc_auth::peer_uid_from_fd(client.as_raw_fd());
        if !ipc_auth::ipc_server_user_is_trusted(server_uid) {
            bail!("ipc{} is served by another user: uid={:?}", postfix, server_uid);
        }
    }
    Ok(ConnectionTmpl::new(client))
}

#[cfg(not(target_os = "ios"))]
pub async fn pull_config(conn: &mut ConnectionTmpl<ConnClient>) {
    if conn.send(&Data::SyncConfig(None)).await.is_err() {
        return;
    }
    if let Ok(Some(Data::SyncConfig(Some((config, config2))))) = conn.next_timeout(1000).await {
        if Config::set(config) {
            log::info!("config synced");
        }
        if Config2::set(config2) {
            log::info!("config2 synced");
        }
    }
}

#[cfg(target_os = "linux")]
#[tokio::main(flavor = "current_thread")]
pub async fn start_pa() {
    use crate::audio_service::AUDIO_DATA_SIZE_U8;

    match new_listener("_pa").await {
        Ok(mut incoming) => {
            loop {
                if let Some(result) = incoming.next().await {
                    match result {
                        Ok(stream) => {
                            let mut stream = Connection::new(stream);
                            let mut device: String = "".to_owned();
                            if let Some(Ok(Some(Data::Config((_, Some(x)))))) =
                                stream.next_timeout2(1000).await
                            {
                                device = x;
                            }
                            if !device.is_empty() {
                                device = crate::platform::linux::get_pa_source_name(&device);
                            }
                            if device.is_empty() {
                                device = crate::platform::linux::get_pa_monitor();
                            }
                            if device.is_empty() {
                                continue;
                            }
                            let spec = pulse::sample::Spec {
                                format: pulse::sample::Format::F32le,
                                channels: 2,
                                rate: crate::platform::PA_SAMPLE_RATE,
                            };
                            log::info!("pa monitor: {:?}", device);
                            // systemctl --user status pulseaudio.service
                            let mut buf: Vec<u8> = vec![0; AUDIO_DATA_SIZE_U8];
                            match psimple::Simple::new(
                                None,                             // Use the default server
                                &crate::get_app_name(),           // Our application's name
                                pulse::stream::Direction::Record, // We want a record stream
                                Some(&device),                    // Use the default device
                                "record",                         // Description of our stream
                                &spec,                            // Our sample format
                                None,                             // Use default channel map
                                None, // Use default buffering attributes
                            ) {
                                Ok(s) => loop {
                                    if let Err(err) = s.read(&mut buf) {
                                        log::error!("Failed to read audio data:{}", err);
                                        break;
                                    }
                                    let out = if buf.iter().filter(|x| **x != 0).next().is_none() {
                                        vec![]
                                    } else {
                                        buf.clone()
                                    };
                                    if let Err(err) = stream.send_raw(out.into()).await {
                                        log::error!("Failed to send audio data:{}", err);
                                        break;
                                    }
                                },
                                Err(err) => {
                                    log::error!("Could not create simple pulse: {}", err);
                                }
                            }
                        }
                        Err(err) => {
                            log::error!("Couldn't get pa client: {:?}", err);
                        }
                    }
                }
            }
        }
        Err(err) => {
            log::error!("Failed to start pa ipc server: {}", err);
        }
    }
}

#[cfg(not(target_os = "ios"))]
#[inline]
#[cfg(not(windows))]
fn get_pid_file(postfix: &str) -> String {
    let path = Config::ipc_path(postfix);
    format!("{}.pid", path)
}

#[cfg(not(any(windows, target_os = "android", target_os = "ios")))]
async fn check_pid(postfix: &str) {
    let pid_file = get_pid_file(postfix);
    if let Ok(mut file) = File::open(&pid_file) {
        let mut content = String::new();
        file.read_to_string(&mut content).ok();
        let pid = content.parse::<usize>().unwrap_or(0);
        if pid > 0 {
            use hbb_common::sysinfo::System;
            let mut sys = System::new();
            sys.refresh_processes();
            if let Some(p) = sys.process(pid.into()) {
                if let Some(current) = sys.process((std::process::id() as usize).into()) {
                    if current.name() == p.name() {
                        // double check with connect
                        if connect(1000, postfix).await.is_ok() {
                            return;
                        }
                    }
                }
            }
        }
    }
    // if not remove old ipc file, the new ipc creation will fail
    // if we remove a ipc file, but the old ipc process is still running,
    // new connection to the ipc will connect to new ipc, old connection to old ipc still keep alive
    std::fs::remove_file(&Config::ipc_path(postfix)).ok();
}

#[cfg(not(target_os = "ios"))]
#[inline]
#[cfg(not(windows))]
fn write_pid(postfix: &str) {
    let path = get_pid_file(postfix);
    if let Ok(mut file) = File::create(&path) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o0777)).ok();
        file.write_all(&std::process::id().to_string().into_bytes())
            .ok();
    }
}

#[cfg(not(target_os = "ios"))]
pub struct ConnectionTmpl<T> {
    inner: Framed<T, BytesCodec>,
}

#[cfg(not(target_os = "ios"))]
pub type Connection = ConnectionTmpl<Conn>;

#[cfg(any(target_os = "windows", target_os = "linux", target_os = "macos"))]
impl Connection {
    fn peer_pid(&self) -> Option<u32> {
        #[cfg(windows)]
        {
            use std::os::windows::io::AsRawHandle;
            ipc_auth::peer_pid_from_handle(self.inner.get_ref().as_raw_handle())
        }
        #[cfg(not(windows))]
        {
            use std::os::unix::io::AsRawFd;
            ipc_auth::peer_pid_from_fd(self.inner.get_ref().as_raw_fd())
        }
    }

    fn peer_uid(&self) -> Option<u32> {
        #[cfg(windows)]
        {
            None
        }
        #[cfg(not(windows))]
        {
            use std::os::unix::io::AsRawFd;
            ipc_auth::peer_uid_from_fd(self.inner.get_ref().as_raw_fd())
        }
    }

    fn peer_is_trusted(&self) -> bool {
        #[cfg(windows)]
        {
            use std::os::windows::io::AsRawHandle;
            ipc_auth::peer_shares_session(self.inner.get_ref().as_raw_handle())
                || self.peer_is_elevated()
        }
        #[cfg(not(windows))]
        {
            ipc_auth::peer_is_trusted(self.peer_uid())
        }
    }

    #[cfg(windows)]
    pub(crate) fn peer_is_elevated(&self) -> bool {
        self.peer_pid().map_or(false, |pid| {
            crate::platform::windows::is_elevated(Some(pid)).unwrap_or(false)
        })
    }
}

#[cfg(target_os = "freebsd")]
impl Connection {
    fn peer_pid(&self) -> Option<u32> {
        None
    }
}

#[cfg(not(target_os = "ios"))]
impl<T> ConnectionTmpl<T>
where
    T: AsyncRead + AsyncWrite + std::marker::Unpin,
{
    pub fn new(conn: T) -> Self {
        Self {
            inner: Framed::new(conn, BytesCodec::new()),
        }
    }

    pub async fn send(&mut self, data: &Data) -> ResultType<()> {
        let v = serde_json::to_vec(data)?;
        self.inner.send(bytes::Bytes::from(v)).await?;
        Ok(())
    }

    async fn send_config(&mut self, name: &str, value: String) -> ResultType<()> {
        self.send(&Data::Config((name.to_owned(), Some(value))))
            .await
    }

    async fn wait_until_handled(&mut self) {
        if self.send(&Data::ClickTime(0)).await.is_ok() {
            self.next_timeout(1000).await.ok();
        }
    }

    pub async fn next_timeout(&mut self, ms_timeout: u64) -> ResultType<Option<Data>> {
        Ok(timeout(ms_timeout, self.next()).await??)
    }

    pub async fn next_timeout2(&mut self, ms_timeout: u64) -> Option<ResultType<Option<Data>>> {
        if let Ok(x) = timeout(ms_timeout, self.next()).await {
            Some(x)
        } else {
            None
        }
    }

    pub async fn next(&mut self) -> ResultType<Option<Data>> {
        match self.inner.next().await {
            Some(res) => {
                let bytes = res?;
                if let Ok(s) = std::str::from_utf8(&bytes) {
                    if let Ok(data) = serde_json::from_str::<Data>(s) {
                        return Ok(Some(data));
                    }
                }
                return Ok(None);
            }
            _ => {
                bail!("Connection lost");
            }
        }
    }

    pub async fn send_raw(&mut self, data: Bytes) -> ResultType<()> {
        self.inner.send(data).await?;
        Ok(())
    }

    pub async fn next_raw(&mut self) -> ResultType<bytes::BytesMut> {
        match self.inner.next().await {
            Some(Ok(res)) => Ok(res),
            _ => {
                bail!("Connection lost");
            }
        }
    }
}

#[cfg(not(target_os = "ios"))]
#[tokio::main(flavor = "current_thread")]
pub async fn get_config(name: &str) -> ResultType<Option<String>> {
    get_config_async(name, 1_000).await
}

#[cfg(not(target_os = "ios"))]
async fn get_config_async(name: &str, ms_timeout: u64) -> ResultType<Option<String>> {
    let mut c = connect(ms_timeout, "").await?;
    c.send(&Data::Config((name.to_owned(), None))).await?;
    if let Some(Data::Config((name2, value))) = c.next_timeout(ms_timeout).await? {
        if name == name2 {
            return Ok(value);
        }
    }
    return Ok(None);
}

#[cfg(not(target_os = "ios"))]
pub async fn set_config_async(name: &str, value: String) -> ResultType<()> {
    let mut c = connect(1000, "").await?;
    c.send_config(name, value).await?;
    c.wait_until_handled().await;
    Ok(())
}

#[cfg(not(target_os = "ios"))]
#[tokio::main(flavor = "current_thread")]
pub async fn set_data(data: &Data) -> ResultType<()> {
    set_data_async(data).await
}

#[cfg(not(target_os = "ios"))]
async fn set_data_async(data: &Data) -> ResultType<()> {
    let mut c = connect(1000, "").await?;
    c.send(data).await?;
    Ok(())
}

// New public async function

#[cfg(not(target_os = "ios"))]
#[tokio::main(flavor = "current_thread")]
pub async fn set_config(name: &str, value: String) -> ResultType<()> {
    set_config_async(name, value).await
}

#[cfg(not(target_os = "ios"))]
pub fn update_temporary_password() -> ResultType<()> {
    set_config("temporary-password", "".to_owned())
}

#[cfg(not(target_os = "ios"))]
pub fn get_permanent_password() -> String {
    if let Ok(Some(v)) = get_config("permanent-password") {
        Config::set_permanent_password(&v);
        v
    } else {
        Config::get_permanent_password()
    }
}

#[cfg(not(target_os = "ios"))]
pub fn get_fingerprint() -> String {
    get_config("fingerprint")
        .unwrap_or_default()
        .unwrap_or_default()
}

#[cfg(not(target_os = "ios"))]
pub fn set_permanent_password(v: String) -> ResultType<()> {
    Config::set_permanent_password(&v);
    set_config("permanent-password", v)
}

#[cfg(not(target_os = "ios"))]
pub fn get_guest_password() -> String {
    if let Ok(Some(v)) = get_config("guest-password") {
        Config::set_guest_password(&v);
        v
    } else {
        Config::get_guest_password()
    }
}

#[cfg(not(target_os = "ios"))]
pub fn set_guest_password(v: String) -> ResultType<()> {
    Config::set_guest_password(&v);
    set_config("guest-password", v)
}

#[cfg(feature = "flutter")]
#[cfg(not(any(target_os = "android", target_os = "ios")))]
pub fn set_unlock_pin(v: String, translate: bool) -> ResultType<()> {
    let v = v.trim().to_owned();
    let min_len = 4;
    let max_len = crate::ui_interface::max_encrypt_len();
    let len = v.chars().count();
    if !v.is_empty() {
        if len < min_len {
            let err = if translate {
                crate::lang::translate("Requires at least {} characters".to_owned())
                    .replacen("{}", &min_len.to_string(), 1)
            } else {
                // Sometimes, translated can't show normally in command line
                format!("Requires at least {} characters", min_len)
            };
            bail!(err);
        }
        if len > max_len {
            bail!("No more than {max_len} characters");
        }
    }
    Config::set_unlock_pin(&v);
    set_config("unlock-pin", v)
}

#[cfg(feature = "flutter")]
#[cfg(not(any(target_os = "android", target_os = "ios")))]
pub fn get_unlock_pin() -> String {
    if let Ok(Some(v)) = get_config("unlock-pin") {
        Config::set_unlock_pin(&v);
        v
    } else {
        Config::get_unlock_pin()
    }
}

#[cfg(feature = "flutter")]
#[cfg(not(any(target_os = "android", target_os = "ios")))]
pub fn get_trusted_devices() -> String {
    if let Ok(Some(v)) = get_config("trusted-devices") {
        v
    } else {
        Config::get_trusted_devices_json()
    }
}

#[cfg(feature = "flutter")]
#[cfg(not(any(target_os = "android", target_os = "ios")))]
pub fn remove_trusted_devices(hwids: Vec<Bytes>) {
    Config::remove_trusted_devices(&hwids);
    allow_err!(set_data(&Data::RemoveTrustedDevices(hwids)));
}

#[cfg(feature = "flutter")]
#[cfg(not(any(target_os = "android", target_os = "ios")))]
pub fn clear_trusted_devices() {
    Config::clear_trusted_devices();
    allow_err!(set_data(&Data::ClearTrustedDevices));
}

#[cfg(not(target_os = "ios"))]
pub fn get_id() -> String {
    if let Ok(Some(v)) = get_config("id") {
        // update salt also, so that next time reinstallation not causing first-time auto-login failure
        if let Ok(Some(v2)) = get_config("salt") {
            Config::set_salt(&v2);
        }
        if v != Config::get_id() {
            Config::set_key_confirmed(false);
            Config::set_id(&v);
        }
        v
    } else {
        Config::get_id()
    }
}

#[cfg(not(target_os = "ios"))]
pub async fn get_rendezvous_server(ms_timeout: u64) -> Option<String> {
    if let Ok(Some(v)) = get_config_async("rendezvous_server", ms_timeout).await {
        Some(v)
    } else {
        Config::get_rendezvous_server().await
    }
}

#[cfg(not(target_os = "ios"))]
async fn get_options_(ms_timeout: u64) -> ResultType<HashMap<String, String>> {
    let mut c = connect(ms_timeout, "").await?;
    c.send(&Data::Options(None)).await?;
    if let Some(Data::Options(Some(value))) = c.next_timeout(ms_timeout).await? {
        Config::set_options(value.clone());
        Ok(value)
    } else {
        Ok(Config::get_options())
    }
}

#[cfg(not(target_os = "ios"))]
pub async fn get_options_async() -> HashMap<String, String> {
    get_options_(1000).await.unwrap_or(Config::get_options())
}

#[cfg(not(target_os = "ios"))]
#[tokio::main(flavor = "current_thread")]
pub async fn get_options() -> HashMap<String, String> {
    get_options_async().await
}

#[cfg(not(target_os = "ios"))]
pub async fn get_option_async(key: &str) -> String {
    if let Some(v) = get_options_async().await.get(key) {
        v.clone()
    } else {
        "".to_owned()
    }
}

#[cfg(not(target_os = "ios"))]
pub fn set_option(key: &str, value: &str) {
    let mut options = get_options();
    if value.is_empty() {
        options.remove(key);
    } else {
        options.insert(key.to_owned(), value.to_owned());
    }
    set_options(options).ok();
}

#[cfg(not(target_os = "ios"))]
#[tokio::main(flavor = "current_thread")]
pub async fn set_options(value: HashMap<String, String>) -> ResultType<()> {
    set_options_async(value).await
}

#[cfg(not(target_os = "ios"))]
pub async fn set_options_async(value: HashMap<String, String>) -> ResultType<()> {
    if let Ok(mut c) = connect(1000, "").await {
        c.send(&Data::Options(Some(value.clone()))).await?;
        // do not put below before connect, because we need to check should_exit
        c.next_timeout(1000).await.ok();
    }
    Config::set_options(value);
    Ok(())
}

#[cfg(not(target_os = "ios"))]
pub async fn set_option_async(key: &str, value: &str) {
    let mut options = get_options_async().await;
    if value.is_empty() {
        options.remove(key);
    } else {
        options.insert(key.to_owned(), value.to_owned());
    }
    set_options_async(options).await.ok();
}

#[cfg(not(target_os = "ios"))]
#[inline]
async fn get_nat_type_(ms_timeout: u64) -> ResultType<i32> {
    let mut c = connect(ms_timeout, "").await?;
    c.send(&Data::NatType(None)).await?;
    if let Some(Data::NatType(Some(value))) = c.next_timeout(ms_timeout).await? {
        Config::set_nat_type(value);
        Ok(value)
    } else {
        Ok(Config::get_nat_type())
    }
}

#[cfg(not(target_os = "ios"))]
pub async fn get_nat_type(ms_timeout: u64) -> i32 {
    get_nat_type_(ms_timeout)
        .await
        .unwrap_or(Config::get_nat_type())
}

#[cfg(not(target_os = "ios"))]
pub async fn get_rendezvous_servers(ms_timeout: u64) -> Vec<String> {
    if let Ok(Some(v)) = get_config_async("rendezvous_servers", ms_timeout).await {
        return v.split(',').map(|x| x.to_owned()).collect();
    }
    return Config::get_rendezvous_servers().await.unwrap_or_default();
}

#[cfg(not(target_os = "ios"))]
#[inline]
async fn get_socks_(ms_timeout: u64) -> ResultType<Option<config::Socks5Server>> {
    let mut c = connect(ms_timeout, "").await?;
    c.send(&Data::Socks(None)).await?;
    if let Some(Data::Socks(value)) = c.next_timeout(ms_timeout).await? {
        Config::set_socks(value.clone());
        Ok(value)
    } else {
        Ok(Config::get_socks())
    }
}

#[cfg(not(target_os = "ios"))]
pub async fn get_socks_async(ms_timeout: u64) -> Option<config::Socks5Server> {
    get_socks_(ms_timeout).await.unwrap_or(Config::get_socks())
}

#[cfg(not(target_os = "ios"))]
#[tokio::main(flavor = "current_thread")]
pub async fn get_socks() -> Option<config::Socks5Server> {
    get_socks_async(1_000).await
}

#[cfg(not(target_os = "ios"))]
#[tokio::main(flavor = "current_thread")]
pub async fn set_socks(value: config::Socks5Server) -> ResultType<()> {
    Config::set_socks(if value.proxy.is_empty() {
        None
    } else {
        Some(value.clone())
    });
    connect(1_000, "")
        .await?
        .send(&Data::Socks(Some(value)))
        .await?;
    Ok(())
}

#[cfg(not(target_os = "ios"))]
#[tokio::main(flavor = "current_thread")]
pub async fn send_url_scheme(url: String) -> ResultType<()> {
    connect(1_000, "_url")
        .await?
        .send(&Data::UrlLink(url))
        .await?;
    Ok(())
}

// Emit `close` events to ipc.
#[cfg(not(target_os = "ios"))]
pub fn close_all_instances() -> ResultType<bool> {
    match crate::ipc::send_url_scheme(IPC_ACTION_CLOSE.to_owned()) {
        Ok(_) => Ok(true),
        Err(err) => Err(err),
    }
}

#[cfg(not(target_os = "ios"))]
#[tokio::main(flavor = "current_thread")]
pub async fn connect_to_user_session(usid: Option<u32>) -> ResultType<()> {
    let mut stream = crate::ipc::connect(1000, crate::POSTFIX_SERVICE).await?;
    timeout(1000, stream.send(&crate::ipc::Data::UserSid(usid))).await??;
    Ok(())
}

#[cfg(not(target_os = "ios"))]
#[tokio::main(flavor = "current_thread")]
pub async fn notify_server_to_check_hwcodec() -> ResultType<()> {
    connect(1_000, "").await?.send(&&Data::CheckHwcodec).await?;
    Ok(())
}

#[cfg(feature = "nativecodec")]
#[cfg(not(any(target_os = "android", target_os = "ios")))]
#[tokio::main(flavor = "current_thread")]
pub async fn get_hwcodec_config_from_server() -> ResultType<()> {
    if !scrap::codec::enable_hwcodec_option() || scrap::hwcodec::HwCodecConfig::already_set() {
        return Ok(());
    }
    let mut c = connect(50, "").await?;
    c.send(&Data::HwCodecConfig(None)).await?;
    if let Some(Data::HwCodecConfig(v)) = c.next_timeout(50).await? {
        match v {
            Some(v) => {
                scrap::hwcodec::HwCodecConfig::set(v);
                return Ok(());
            }
            None => {
                bail!("hwcodec config is none");
            }
        }
    }
    bail!("failed to get hwcodec config");
}

#[cfg(feature = "nativecodec")]
#[cfg(not(any(target_os = "android", target_os = "ios")))]
pub fn client_get_hwcodec_config_thread(wait_sec: u64) {
    static ONCE: std::sync::Once = std::sync::Once::new();
    if !crate::platform::is_installed()
        || !scrap::codec::enable_hwcodec_option()
        || scrap::hwcodec::HwCodecConfig::already_set()
    {
        return;
    }
    ONCE.call_once(move || {
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_secs(1));
            let mut intervals: Vec<u64> = vec![wait_sec, 3, 3, 6, 9];
            for i in intervals.drain(..) {
                if i > 0 {
                    std::thread::sleep(std::time::Duration::from_secs(i));
                }
                if get_hwcodec_config_from_server().is_ok() {
                    break;
                }
            }
        });
    });
}

#[cfg(feature = "nativecodec")]
#[cfg(not(target_os = "ios"))]
#[tokio::main(flavor = "current_thread")]
pub async fn hwcodec_process() {
    if scrap::hwcodec::HwCodecConfig::has_valid_cache() {
        return;
    }
    let s = scrap::hwcodec::check_available_hwcodec();
    scrap::hwcodec::HwCodecConfig::set(s.clone());
    for _ in 0..5 {
        match crate::ipc::connect(1000, "").await {
            Ok(mut conn) => {
                match conn
                    .send(&crate::ipc::Data::HwCodecConfig(Some(s.clone())))
                    .await
                {
                    Ok(()) => {
                        log::info!("send ok");
                        break;
                    }
                    Err(e) => {
                        log::error!("send failed: {e:?}");
                    }
                }
            }
            Err(e) => {
                log::error!("connect failed: {e:?}");
            }
        }
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
}

#[cfg(not(target_os = "ios"))]
#[tokio::main(flavor = "current_thread")]
pub async fn get_wayland_screencast_restore_token(key: String) -> ResultType<String> {
    let v = handle_wayland_screencast_restore_token(key, "get".to_owned()).await?;
    Ok(v.unwrap_or_default())
}

#[cfg(not(target_os = "ios"))]
#[tokio::main(flavor = "current_thread")]
pub async fn clear_wayland_screencast_restore_token(key: String) -> ResultType<bool> {
    if let Some(v) = handle_wayland_screencast_restore_token(key, "clear".to_owned()).await? {
        return Ok(v.is_empty());
    }
    return Ok(false);
}

#[cfg(not(target_os = "ios"))]
async fn handle_wayland_screencast_restore_token(
    key: String,
    value: String,
) -> ResultType<Option<String>> {
    let ms_timeout = 1_000;
    let mut c = connect(ms_timeout, "").await?;
    c.send(&Data::WaylandScreencastRestoreToken((key, value)))
        .await?;
    if let Some(Data::WaylandScreencastRestoreToken((_key, v))) = c.next_timeout(ms_timeout).await?
    {
        return Ok(Some(v));
    }
    return Ok(None);
}

#[cfg(test)]
mod test {
    use super::*;
    #[test]
    fn verify_ffi_enum_data_size() {
        println!("{}", std::mem::size_of::<Data>());
        assert!(std::mem::size_of::<Data>() <= 96);
    }
}

#[cfg(target_os = "linux")]
fn reboot_request_allowed(peer_uid: Option<u32>) -> bool {
    if !crate::platform::is_root() {
        return false;
    }
    match peer_uid {
        Some(0) => true,
        Some(uid) => crate::platform::linux::get_active_userid().parse::<u32>().ok() == Some(uid),
        None => false,
    }
}
