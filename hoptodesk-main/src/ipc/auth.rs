use hbb_common::{anyhow, log, ResultType};
#[cfg(any(windows, target_os = "macos", all(target_os = "linux", feature = "drm")))]
use hbb_common::bail;
use std::fs;
use std::path::{Path, PathBuf};

#[cfg(any(target_os = "linux", target_os = "macos"))]
use hbb_common::libc;
#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::os::unix::io::RawFd;

#[cfg(all(target_os = "linux", feature = "drm"))]
#[inline]
pub(crate) fn is_allowed_service_peer_uid(peer_uid: u32, active_uid: Option<u32>) -> bool {
    peer_uid == 0 || active_uid.is_some_and(|uid| uid == peer_uid)
}

#[cfg(all(target_os = "linux", feature = "drm"))]
#[inline]
fn active_uid_strict() -> Option<u32> {
    let reported_uid_raw = crate::platform::linux::get_active_userid();
    let trimmed = reported_uid_raw.trim();
    if let Ok(uid) = trimmed.parse::<u32>() {
        return Some(uid);
    }
    if trimmed.is_empty() {
        log::debug!("Failed to resolve active user uid on linux: active uid is empty");
    } else {
        log::warn!("Failed to parse active user uid on linux: '{}'", trimmed);
    }
    None
}

#[cfg(all(target_os = "linux", feature = "drm"))]
#[inline]
pub(crate) fn active_uid() -> Option<u32> {
    active_uid_strict()
}

#[cfg(all(target_os = "linux", feature = "drm"))]
#[inline]
pub(crate) fn active_uid_cached() -> Option<u32> {
    crate::platform::linux::get_active_userid_cached()
}

#[cfg(target_os = "linux")]
#[inline]
pub(crate) fn peer_uid_from_fd(fd: RawFd) -> Option<u32> {
    peer_cred_from_fd(fd).map(|cred| cred.uid as u32)
}

#[cfg(target_os = "macos")]
#[inline]
pub(crate) fn peer_uid_from_fd(fd: RawFd) -> Option<u32> {
    let mut uid: libc::uid_t = 0;
    let mut gid: libc::gid_t = 0;
    let rc = unsafe { libc::getpeereid(fd, &mut uid, &mut gid) };
    (rc == 0).then_some(uid as u32)
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[inline]
fn peer_user_is_trusted(peer_uid: Option<u32>) -> bool {
    let own_uid = unsafe { libc::geteuid() } as u32;
    peer_uid.is_some_and(|uid| uid == 0 || uid == own_uid)
}

#[cfg(windows)]
#[inline]
fn peer_user_is_trusted(_peer_uid: Option<u32>) -> bool {
    false
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) fn peer_is_trusted(peer_uid: Option<u32>) -> bool {
    peer_user_is_trusted(peer_uid)
        || (unsafe { libc::geteuid() } == 0
            && peer_uid.is_some()
            && crate::platform::get_active_userid().trim().parse::<u32>().ok() == peer_uid)
}

#[cfg(windows)]
pub(crate) fn peer_shares_session(handle: std::os::windows::io::RawHandle) -> bool {
    use winapi::um::winbase::GetNamedPipeClientSessionId;
    let mut session = 0;
    let known = unsafe { GetNamedPipeClientSessionId(handle as _, &mut session) } != 0;
    known && crate::platform::windows::get_current_process_session_id() == Some(session)
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[inline]
pub(crate) fn ipc_server_user_is_trusted(server_uid: Option<u32>) -> bool {
    let own_uid = unsafe { libc::geteuid() };
    own_uid == 0 || peer_user_is_trusted(server_uid)
}

#[cfg(target_os = "linux")]
#[inline]
pub(crate) fn peer_pid_from_fd(fd: RawFd) -> Option<u32> {
    peer_cred_from_fd(fd).and_then(|cred| (cred.pid > 0).then_some(cred.pid as u32))
}

#[cfg(target_os = "linux")]
#[inline]
fn peer_cred_from_fd(fd: RawFd) -> Option<libc::ucred> {
    let mut cred: libc::ucred = unsafe { std::mem::zeroed() };
    let mut len = std::mem::size_of::<libc::ucred>() as _;
    let rc = unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            &mut cred as *mut _ as *mut libc::c_void,
            &mut len,
        )
    };
    if rc == 0 {
        Some(cred)
    } else {
        None
    }
}

#[cfg(target_os = "macos")]
#[inline]
pub(crate) fn peer_pid_from_fd(fd: RawFd) -> Option<u32> {
    let mut pid: libc::pid_t = 0;
    let mut len = std::mem::size_of::<libc::pid_t>() as libc::socklen_t;
    let rc = unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_LOCAL,
            libc::LOCAL_PEERPID,
            &mut pid as *mut _ as *mut libc::c_void,
            &mut len,
        )
    };
    if rc == 0 && pid > 0 {
        Some(pid as u32)
    } else {
        None
    }
}

#[cfg(windows)]
#[inline]
pub(crate) fn peer_pid_from_handle(handle: std::os::windows::io::RawHandle) -> Option<u32> {
    use winapi::{shared::minwindef::DWORD, um::winbase::GetNamedPipeClientProcessId};
    let mut pid: DWORD = 0;
    let ok = unsafe { GetNamedPipeClientProcessId(handle as _, &mut pid) };
    if ok != 0 && pid > 0 {
        Some(pid as u32)
    } else {
        None
    }
}

#[cfg(target_os = "linux")]
#[inline]
fn without_deleted_suffix(path: PathBuf) -> PathBuf {
    match path.to_str().and_then(|text| text.strip_suffix(" (deleted)")) {
        Some(text) => PathBuf::from(text),
        None => path,
    }
}

#[inline]
fn current_exe_canonical_path() -> ResultType<PathBuf> {
    let current = std::env::current_exe()
        .map_err(|err| anyhow::anyhow!("Failed to resolve current executable path: {}", err))?;
    #[cfg(target_os = "linux")]
    let current = without_deleted_suffix(current);
    fs::canonicalize(&current).map_err(|err| {
        anyhow::anyhow!(
            "Failed to canonicalize current executable path '{}': {}",
            current.display(),
            err
        )
        .into()
    })
}

#[cfg(target_os = "linux")]
#[inline]
fn peer_exe_canonical_path_by_pid(peer_pid: u32) -> ResultType<PathBuf> {
    let proc_exe = PathBuf::from(format!("/proc/{peer_pid}/exe"));
    let peer_exe = fs::read_link(&proc_exe).map_err(|err| {
        anyhow::anyhow!(
            "Failed to read peer executable link '{}': {}",
            proc_exe.display(),
            err
        )
    })?;
    let peer_exe = without_deleted_suffix(peer_exe);
    fs::canonicalize(&peer_exe).map_err(|err| {
        anyhow::anyhow!(
            "Failed to canonicalize peer executable path '{}': {}",
            peer_exe.display(),
            err
        )
        .into()
    })
}

#[cfg(target_os = "macos")]
#[inline]
fn peer_exe_canonical_path_by_pid(peer_pid: u32) -> ResultType<PathBuf> {
    const PATH_BUF_SIZE: usize = 4096;
    let mut buffer = vec![0u8; PATH_BUF_SIZE];
    let length = unsafe {
        libc::proc_pidpath(
            peer_pid as _,
            buffer.as_mut_ptr() as _,
            PATH_BUF_SIZE as _,
        )
    };
    if length <= 0 {
        bail!(
            "Failed to query the executable path of peer process {}",
            peer_pid
        );
    }
    buffer.truncate(length as _);
    let path = PathBuf::from(String::from_utf8_lossy(&buffer).to_string());
    fs::canonicalize(&path).map_err(|err| {
        anyhow::anyhow!(
            "Failed to canonicalize peer executable path '{}': {}",
            path.display(),
            err
        )
        .into()
    })
}

#[cfg(windows)]
#[inline]
fn peer_exe_canonical_path_by_pid(peer_pid: u32) -> ResultType<PathBuf> {
    use hbb_common::platform::windows::RAIIHandle;
    use winapi::{
        shared::minwindef::{DWORD, FALSE},
        um::{
            processthreadsapi::OpenProcess, winbase::QueryFullProcessImageNameW,
            winnt::PROCESS_QUERY_LIMITED_INFORMATION,
        },
    };
    const PATH_BUF_LEN: usize = 4096;
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, FALSE, peer_pid as DWORD);
        if handle.is_null() {
            bail!(
                "Failed to open peer process {}: {}",
                peer_pid,
                std::io::Error::last_os_error()
            );
        }
        let _handle = RAIIHandle(handle);
        let mut buffer = vec![0u16; PATH_BUF_LEN];
        let mut size = buffer.len() as DWORD;
        if QueryFullProcessImageNameW(handle, 0, buffer.as_mut_ptr(), &mut size) == FALSE {
            bail!(
                "Failed to query the executable path of peer process {}: {}",
                peer_pid,
                std::io::Error::last_os_error()
            );
        }
        let path = PathBuf::from(String::from_utf16_lossy(&buffer[..size as usize]));
        fs::canonicalize(&path).map_err(|err| {
            anyhow::anyhow!(
                "Failed to canonicalize peer executable path '{}': {}",
                path.display(),
                err
            )
            .into()
        })
    }
}

#[inline]
pub(crate) fn executable_paths_match(left: &Path, right: &Path) -> bool {
    #[cfg(windows)]
    {
        fn normalize(path: &Path) -> String {
            let text = path.to_string_lossy().replace('/', "\\");
            text.strip_prefix(r"\\?\")
                .unwrap_or(&text)
                .to_ascii_lowercase()
        }
        return normalize(left) == normalize(right);
    }
    #[cfg(not(windows))]
    {
        left == right
    }
}

#[cfg(all(target_os = "linux", feature = "drm"))]
#[inline]
fn ensure_peer_executable_matches_current_by_pid(peer_pid: u32, postfix: &str) -> ResultType<()> {
    let peer_exe = peer_exe_canonical_path_by_pid(peer_pid)?;
    let current_exe = current_exe_canonical_path()?;
    if executable_paths_match(&peer_exe, &current_exe) {
        return Ok(());
    }
    bail!(
        "Peer executable path mismatch on ipc channel '{}': peer_pid={}, peer_exe='{}', current_exe='{}'",
        postfix,
        peer_pid,
        peer_exe.display(),
        current_exe.display()
    );
}

#[cfg(all(target_os = "linux", feature = "drm"))]
#[inline]
pub(crate) fn ensure_peer_executable_matches_current_by_pid_opt(
    peer_pid: Option<u32>,
    postfix: &str,
) -> ResultType<()> {
    let peer_pid = peer_pid
        .ok_or_else(|| anyhow::anyhow!("Failed to resolve peer pid on ipc channel '{}'", postfix))?;
    ensure_peer_executable_matches_current_by_pid(peer_pid, postfix)
}

#[cfg(target_os = "linux")]
#[inline]
fn running_from_appimage() -> bool {
    std::env::var("APPDIR")
        .map_or(false, |dir| Path::new(&dir).join("AppRun").exists())
}

fn file_sha256(path: &Path) -> Option<Vec<u8>> {
    use sha2::{Digest, Sha256};
    use std::io::Read;
    let mut file = fs::File::open(path).ok()?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        match file.read(&mut buf).ok()? {
            0 => break,
            n => hasher.update(&buf[..n]),
        }
    }
    Some(hasher.finalize().to_vec())
}

type PeerVerdict = (PathBuf, u64, Option<std::time::SystemTime>, bool);

fn executable_contents_match(peer_exe: &Path, current_exe: &Path) -> bool {
    static CURRENT_SHA256: std::sync::OnceLock<Option<Vec<u8>>> = std::sync::OnceLock::new();
    static LAST_VERDICT: std::sync::Mutex<Option<PeerVerdict>> = std::sync::Mutex::new(None);
    let (Ok(peer), Ok(current)) = (fs::metadata(peer_exe), fs::metadata(current_exe)) else {
        return false;
    };
    if peer.len() != current.len() {
        return false;
    }
    let modified = peer.modified().ok();
    let mut last = LAST_VERDICT.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((path, len, time, same)) = last.as_ref() {
        if path == peer_exe && *len == peer.len() && *time == modified {
            return *same;
        }
    }
    let same = CURRENT_SHA256
        .get_or_init(|| file_sha256(current_exe))
        .as_ref()
        .map_or(false, |own| file_sha256(peer_exe).as_ref() == Some(own));
    *last = Some((peer_exe.to_path_buf(), peer.len(), modified, same));
    same
}

fn unidentified_peer_is_rejected(peer_uid: Option<u32>, postfix: &str, reason: String) -> bool {
    if peer_user_is_trusted(peer_uid) {
        log::debug!(
            "Unidentified peer on ipc channel '{}' allowed for its user: peer_uid={:?}, {}",
            postfix,
            peer_uid,
            reason
        );
        return false;
    }
    log::warn!(
        "Rejected unidentified peer on ipc channel '{}': peer_uid={:?}, {}",
        postfix,
        peer_uid,
        reason
    );
    true
}

pub(crate) fn peer_is_another_executable(
    peer_pid: Option<u32>,
    peer_uid: Option<u32>,
    postfix: &str,
) -> bool {
    #[cfg(target_os = "linux")]
    if running_from_appimage() {
        return unidentified_peer_is_rejected(peer_uid, postfix, "running from an AppImage".into());
    }
    let Some(peer_pid) = peer_pid else {
        return unidentified_peer_is_rejected(peer_uid, postfix, "no peer pid".into());
    };
    let peer_exe = match peer_exe_canonical_path_by_pid(peer_pid) {
        Ok(path) => path,
        Err(err) => {
            return unidentified_peer_is_rejected(
                peer_uid,
                postfix,
                format!("peer_pid={}, {}", peer_pid, err),
            );
        }
    };
    let current_exe = match current_exe_canonical_path() {
        Ok(path) => path,
        Err(err) => {
            return unidentified_peer_is_rejected(
                peer_uid,
                postfix,
                format!("peer_pid={}, own executable unknown, {}", peer_pid, err),
            );
        }
    };
    if executable_paths_match(&peer_exe, &current_exe)
        || executable_contents_match(&peer_exe, &current_exe)
    {
        return false;
    }
    log::warn!(
        "Rejected ipc connection from another executable on channel '{}': peer_pid={}, peer_exe='{}', current_exe='{}'",
        postfix,
        peer_pid,
        peer_exe.display(),
        current_exe.display()
    );
    true
}

#[cfg(all(target_os = "linux", feature = "drm"))]
pub(crate) fn authorize_service_scoped_ipc_connection(fd: RawFd, postfix: &str) -> bool {
    let peer_uid = peer_uid_from_fd(fd);
    let active_uid = active_uid();
    if !peer_uid.is_some_and(|uid| is_allowed_service_peer_uid(uid, active_uid)) {
        log::debug!(
            "Rejected unauthorized connection on service-scoped IPC channel '{}': peer_uid={:?}, active_uid={:?}",
            postfix,
            peer_uid,
            active_uid
        );
        return false;
    }
    let peer_pid = peer_pid_from_fd(fd);
    if let Err(err) = ensure_peer_executable_matches_current_by_pid_opt(peer_pid, postfix) {
        log::warn!(
            "Rejected unauthorized connection on protected service-scoped IPC channel due to executable mismatch: postfix={}, peer_pid={:?}, err={}",
            postfix,
            peer_pid,
            err
        );
        return false;
    }
    true
}
