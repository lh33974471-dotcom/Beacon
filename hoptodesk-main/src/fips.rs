use hbb_common::{config::Config, log};

pub fn require_approved_mode() {
    #[cfg(windows)]
    if let Err(reason) = load_module() {
        refuse(&reason);
    }
    match aws_lc_rs::try_fips_mode() {
        Ok(()) => log::info!("FIPS mode: on ({})", module_version()),
        Err(e) => refuse(&format!("{} is not in approved mode: {:?}", module_version(), e)),
    }
    match hop_fips::self_check() {
        Ok(lines) => lines.iter().for_each(|line| log::info!("FIPS mode: {}", line)),
        Err(e) => refuse(&format!("session self-check failed: {}", e)),
    }
    if Config::get_key_pair().0.is_empty() {
        match hop_fips::new_identity() {
            Ok(key_pair) => {
                if Config::set_fips_key_pair(key_pair) {
                    log::info!("FIPS mode: made a new identity key in the module");
                }
            }
            Err(e) => refuse(&format!("cannot make an identity key: {}", e)),
        }
    }
}

fn module_version() -> String {
    unsafe { std::ffi::CStr::from_ptr(aws_lc_fips_sys::awslc_version_string()) }
        .to_string_lossy()
        .into_owned()
}

fn refuse(reason: &str) -> ! {
    log::error!("FIPS mode: refusing to run, {}", reason);
    eprintln!("FIPS mode: refusing to run, {}", reason);
    std::process::exit(1);
}

#[cfg(windows)]
const MODULE_NAME: &str = "aws_lc_fips_0_13_11_crypto.dll";

#[cfg(windows)]
static MODULE: &[u8] = include_bytes!(env!("HOP_FIPS_MODULE"));

#[cfg(windows)]
#[link(name = "kernel32")]
extern "system" {
    fn LoadLibraryW(name: *const u16) -> *mut std::ffi::c_void;
}

#[cfg(windows)]
fn load_module() -> Result<(), String> {
    use sha2::{Digest, Sha256};
    use std::os::windows::ffi::OsStrExt;
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let digest = Sha256::digest(MODULE);
    let tag: String = digest[..6].iter().map(|b| format!("{:02x}", b)).collect();
    let dir = exe
        .parent()
        .ok_or("the app folder is unknown")?
        .join(format!("fips-{}", tag));
    let path = dir.join(MODULE_NAME);
    let current = || std::fs::read(&path).map_or(false, |bytes| bytes == MODULE);
    if !current() {
        let tmp = dir.join(format!("{}.{}", MODULE_NAME, std::process::id()));
        let written = std::fs::create_dir_all(&dir)
            .and_then(|_| std::fs::write(&tmp, MODULE))
            .and_then(|_| std::fs::rename(&tmp, &path));
        std::fs::remove_file(&tmp).ok();
        if written.is_err() && !(0..20).any(|_| {
            std::thread::sleep(std::time::Duration::from_millis(500));
            current()
        }) {
            return Err(format!("cannot place {} in {}", MODULE_NAME, dir.display()));
        }
    }
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    if unsafe { LoadLibraryW(wide.as_ptr()) }.is_null() {
        return Err(format!("cannot load {}: {}", path.display(), std::io::Error::last_os_error()));
    }
    Ok(())
}
