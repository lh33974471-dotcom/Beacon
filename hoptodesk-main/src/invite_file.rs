#[cfg(any(target_os = "macos", target_os = "linux", target_os = "freebsd"))]
use hbb_common::config::Config;
#[cfg(any(target_os = "macos", target_os = "linux", target_os = "freebsd"))]
use std::path::{Path, PathBuf};

#[cfg(target_os = "macos")]
const PACKAGE_SUFFIXES: &[&str] = &[".dmg"];
#[cfg(any(target_os = "linux", target_os = "freebsd"))]
const PACKAGE_SUFFIXES: &[&str] = &[".deb", ".rpm", ".appimage", ".pkg"];

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "freebsd"))]
const DECLINED_OPTION: &str = "invite-file-declined";

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "freebsd"))]
static INVITE_FILE_OFFER: std::sync::Mutex<String> = std::sync::Mutex::new(String::new());

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "freebsd"))]
struct InviteFile {
    #[cfg(target_os = "macos")]
    path: PathBuf,
    code: String,
    modified: std::time::SystemTime,
}

fn validate_invite_code(s: &str) -> Option<String> {
    if s.len() != 16 {
        return None;
    }
    let mut has_uppercase = false;
    for c in s.chars() {
        if c.is_ascii_uppercase() {
            has_uppercase = true;
        } else if !(c.is_ascii_lowercase() || c.is_ascii_digit()) {
            return None;
        }
    }
    has_uppercase.then(|| s.to_string())
}

pub fn invite_code_from_exe_name(exe_file_name: &str) -> Option<String> {
    let stem = exe_file_name
        .rsplit_once('.')
        .map(|(head, _)| head)
        .unwrap_or(exe_file_name);
    stem.split('-').rev().find_map(validate_invite_code)
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "freebsd"))]
fn invite_file_prefixes() -> Vec<String> {
    let mut prefixes = vec!["hoptodeskpro-".to_owned()];
    let branded: String = crate::common::get_app_name()
        .to_lowercase()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect();
    if !branded.is_empty() && branded != "hoptodeskpro" {
        prefixes.push(format!("{}-", branded));
    }
    prefixes
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "freebsd"))]
fn invite_code_from_package_name(name: &str) -> Option<String> {
    if !name.is_ascii() {
        return None;
    }
    let lower = name.to_lowercase();
    let suffix = PACKAGE_SUFFIXES.iter().find(|s| lower.ends_with(*s))?;
    let prefix_len = invite_file_prefixes()
        .iter()
        .find(|p| lower.starts_with(p.as_str()))
        .map(|p| p.len())?;
    validate_invite_code(&name[prefix_len..name.len() - suffix.len()])
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "freebsd"))]
fn invite_file_at(path: PathBuf) -> Option<InviteFile> {
    let code = invite_code_from_package_name(&path.file_name()?.to_string_lossy())?;
    let meta = std::fs::symlink_metadata(&path).ok()?;
    if !meta.is_file() {
        return None;
    }
    let modified = meta.modified().ok()?;
    Some(InviteFile {
        #[cfg(target_os = "macos")]
        path,
        code,
        modified,
    })
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "freebsd"))]
fn invite_files_in(dir: &Path) -> Vec<InviteFile> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|entry| invite_file_at(entry.path()))
        .collect()
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "freebsd"))]
fn newest_first(mut files: Vec<InviteFile>) -> Vec<InviteFile> {
    files.sort_by(|a, b| b.modified.cmp(&a.modified));
    files
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "freebsd"))]
fn own_invite_files() -> Vec<InviteFile> {
    let Some(home) = std::env::var_os("HOME") else {
        return Vec::new();
    };
    let home = Path::new(&home);
    newest_first(
        ["Downloads", "Desktop", "Documents"]
            .iter()
            .flat_map(|dir| invite_files_in(&home.join(dir)))
            .collect(),
    )
}

#[cfg(target_os = "macos")]
fn spotlight_invite_files() -> Vec<InviteFile> {
    let mut files = Vec::new();
    for prefix in invite_file_prefixes() {
        let Ok(output) = std::process::Command::new("mdfind")
            .arg(format!("kMDItemFSName == '{}*.dmg'c", prefix))
            .output()
        else {
            continue;
        };
        if output.status.success() {
            files.extend(
                String::from_utf8_lossy(&output.stdout)
                    .lines()
                    .filter_map(|line| invite_file_at(PathBuf::from(line.trim()))),
            );
        }
    }
    files
}

#[cfg(target_os = "macos")]
fn quarantine_stamp(path: &Path) -> Option<String> {
    use std::os::unix::ffi::OsStrExt;
    let path = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut value = [0u8; 256];
    let size = unsafe {
        hbb_common::libc::getxattr(
            path.as_ptr(),
            b"com.apple.quarantine\0".as_ptr() as *const _,
            value.as_mut_ptr() as *mut _,
            value.len(),
            0,
            hbb_common::libc::XATTR_NOFOLLOW,
        )
    };
    (size > 0).then(|| String::from_utf8_lossy(&value[..size as usize]).into_owned())
}

#[cfg(target_os = "macos")]
fn quarantine_event_id(stamp: &str) -> Option<&str> {
    stamp.split(';').nth(3).filter(|id| !id.is_empty())
}

#[cfg(target_os = "macos")]
fn sole_link_owner(path: &Path) -> Option<u32> {
    use std::os::unix::fs::MetadataExt;
    let meta = std::fs::symlink_metadata(path).ok()?;
    (meta.nlink() == 1).then(|| meta.uid())
}

#[cfg(target_os = "macos")]
fn current_uid() -> u32 {
    unsafe { hbb_common::libc::geteuid() }
}

#[cfg(target_os = "macos")]
fn came_from_same_download(file: &InviteFile, event_id: &str) -> bool {
    sole_link_owner(&file.path) == Some(current_uid())
        && quarantine_stamp(&file.path)
            .as_deref()
            .and_then(quarantine_event_id)
            == Some(event_id)
}

#[cfg(target_os = "macos")]
fn script_placed_in_tmp(file: &InviteFile) -> bool {
    matches!(sole_link_owner(&file.path), Some(owner) if owner == 0 || owner == current_uid())
        && quarantine_stamp(&file.path).is_none()
}

#[cfg(target_os = "macos")]
pub fn proven_invite_code() -> Option<String> {
    let exe = std::env::current_exe().ok()?;
    let app = exe
        .ancestors()
        .find(|p| p.extension().map_or(false, |ext| ext == "app"))?;
    match quarantine_stamp(app) {
        Some(stamp) => {
            let id = quarantine_event_id(&stamp)?;
            let mut files = own_invite_files();
            files.extend(spotlight_invite_files());
            newest_first(files)
                .into_iter()
                .find(|file| came_from_same_download(file, id))
                .map(|file| file.code)
        }
        None => newest_first(invite_files_in(Path::new("/tmp")))
            .into_iter()
            .find(script_placed_in_tmp)
            .map(|file| file.code),
    }
}

#[cfg(any(target_os = "linux", target_os = "freebsd"))]
pub fn proven_invite_code() -> Option<String> {
    let image = std::env::var_os("APPIMAGE")?;
    invite_code_from_package_name(&Path::new(&image).file_name()?.to_string_lossy())
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "freebsd"))]
fn invite_file_offer() -> Option<String> {
    if crate::dashboard::is_linked()
        || !crate::dashboard::get_invite_code().is_empty()
        || !crate::dashboard::get_prefilled_enrollment_token().is_empty()
    {
        return None;
    }
    let enrolled = Config::get_option("last_enrolled_invite_code");
    let declined = hbb_common::config::LocalConfig::get_option(DECLINED_OPTION);
    own_invite_files()
        .into_iter()
        .map(|file| file.code)
        .find(|code| *code != enrolled && *code != declined)
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "freebsd"))]
pub fn start_invite_file_lookup() {
    std::thread::spawn(|| {
        let Some(code) = invite_file_offer() else {
            return;
        };
        if let Ok(account) = crate::dashboard::invite_account_name(&code) {
            *INVITE_FILE_OFFER.lock().unwrap() =
                serde_json::json!({ "code": code, "account": account }).to_string();
        }
    });
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "freebsd"))]
pub fn take_invite_file_offer() -> String {
    std::mem::take(&mut *INVITE_FILE_OFFER.lock().unwrap())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exe_name_code_needs_sixteen_chars_and_an_uppercase() {
        assert_eq!(
            invite_code_from_exe_name("HopToDeskPro-ABCDabcd12345678.exe").as_deref(),
            Some("ABCDabcd12345678")
        );
        assert_eq!(invite_code_from_exe_name("HopToDeskPro-abcdabcd12345678.exe"), None);
        assert_eq!(invite_code_from_exe_name("HopToDesk.exe"), None);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn only_a_plain_own_file_counts_as_script_placed() {
        let dir = std::env::temp_dir().join(format!("htd-invite-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let plain = dir.join("hoptodeskpro-ABCDabcd12345678.dmg");
        std::fs::write(&plain, b"x").unwrap();
        let file = invite_file_at(plain.clone()).unwrap();
        assert!(script_placed_in_tmp(&file));

        let linked = dir.join("hoptodeskpro-BCDEabcd12345678.dmg");
        std::os::unix::fs::symlink("/etc/hosts", &linked).unwrap();
        assert!(invite_file_at(linked).is_none());

        let hard = dir.join("hoptodeskpro-CDEFabcd12345678.dmg");
        std::fs::hard_link(&plain, &hard).unwrap();
        assert!(!script_placed_in_tmp(&invite_file_at(hard.clone()).unwrap()));
        std::fs::remove_file(&hard).unwrap();

        let stamped = InviteFile {
            path: plain.clone(),
            code: String::new(),
            modified: std::time::SystemTime::now(),
        };
        std::process::Command::new("xattr")
            .args(["-w", "com.apple.quarantine", "0083;1;Safari;SOME-ID"])
            .arg(&plain)
            .status()
            .unwrap();
        assert!(!script_placed_in_tmp(&stamped));
        assert!(came_from_same_download(&stamped, "SOME-ID"));
        assert!(!came_from_same_download(&stamped, "OTHER-ID"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn quarantine_event_id_is_the_fourth_field() {
        assert_eq!(
            quarantine_event_id("0083;6ae59113;Safari;CC0B3B8C-8AC4"),
            Some("CC0B3B8C-8AC4")
        );
        assert_eq!(quarantine_event_id("0083;00000000;;CC0B3B8C-8AC4"), Some("CC0B3B8C-8AC4"));
        assert_eq!(quarantine_event_id("0083;6ae59113;Safari;"), None);
        assert_eq!(quarantine_event_id("0083;6ae59113;Safari"), None);
    }
}
