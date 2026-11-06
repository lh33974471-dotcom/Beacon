use super::{collect_wayland_displays, get_values_of_seat0_with_gdm_wayland, WaylandDisplayInfo};
use crate::{bail, ResultType};
use sctk::reexports::client::Connection;
use std::path::{Path, PathBuf};

const RUNTIME_DIR_PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

pub const WAYLAND_DISPLAY_PROBE_ARG: &str = "--wayland-display-probe";

const WAYLAND_PROBE_MAGIC: &str = "wayland-display-probe-v1";

static PROBE_UNSUPPORTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

static RUNTIME_DIR_PROBE_BUSY: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

struct ProbeBusyGuard;

impl Drop for ProbeBusyGuard {
    fn drop(&mut self) {
        RUNTIME_DIR_PROBE_BUSY.store(false, std::sync::atomic::Ordering::Release);
    }
}

pub fn wayland_display_probe_child_main() -> ! {
    use std::io::Write;
    println!("{WAYLAND_PROBE_MAGIC}");
    let _ = std::io::stdout().flush();
    let code = match seat0_runtime_dir()
        .and_then(|dir| {
            drop_to_dir_owner(&dir)?;
            probe_runtime_dir(&dir)
        })
        .and_then(|displays| serde_json::to_string(&displays).map_err(anyhow::Error::from))
    {
        Ok(json) => {
            println!("{json}");
            0
        }
        Err(err) => {
            eprintln!("{err:#}");
            1
        }
    };
    let _ = std::io::stdout().flush();
    std::process::exit(code)
}

static ENDPOINT_WAS_NAMED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

pub(super) fn env_names_wayland_endpoint() -> bool {
    use std::sync::atomic::Ordering;
    let named = ["WAYLAND_DISPLAY", "WAYLAND_SOCKET"]
        .iter()
        .any(|key| std::env::var_os(key).is_some_and(|value| !value.is_empty()));
    if named {
        ENDPOINT_WAS_NAMED.store(true, Ordering::Release);
    }
    ENDPOINT_WAS_NAMED.load(Ordering::Acquire)
}

fn drop_to_dir_owner(dir: &Path) -> ResultType<()> {
    if unsafe { libc::geteuid() } != 0 {
        return Ok(());
    }
    use std::os::unix::fs::MetadataExt;
    let meta = std::fs::metadata(dir)?;
    let (uid, gid) = (meta.uid(), meta.gid());
    if uid == 0 {
        return Ok(());
    }
    unsafe {
        if libc::setgroups(0, std::ptr::null()) != 0
            || libc::setgid(gid) != 0
            || libc::setuid(uid) != 0
            || libc::setuid(0) == 0
        {
            bail!("could not drop privileges for the socket probe");
        }
    }
    Ok(())
}

fn seat0_runtime_dir() -> ResultType<PathBuf> {
    let uid = get_values_of_seat0_with_gdm_wayland(&[1]).remove(0);
    if uid.is_empty() || !uid.bytes().all(|b| b.is_ascii_digit()) {
        bail!("no active seat0 session to take a runtime directory from");
    }
    Ok(PathBuf::from(format!("/run/user/{uid}")))
}

fn wayland_sockets_in(dir: &Path) -> Vec<PathBuf> {
    use std::os::unix::fs::FileTypeExt;
    let mut paths: Vec<PathBuf> = match std::fs::read_dir(dir) {
        Ok(entries) => entries
            .flatten()
            .filter(|entry| {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                name.starts_with("wayland-")
                    && !name.ends_with(".lock")
                    && entry.file_type().map(|t| t.is_socket()).unwrap_or(false)
            })
            .map(|entry| entry.path())
            .collect(),
        Err(_) => Vec::new(),
    };
    paths.sort_by_key(|path| {
        path.file_name()
            .and_then(|name| name.to_str())
            .and_then(|name| name.strip_prefix("wayland-"))
            .and_then(|number| number.parse::<u32>().ok())
            .unwrap_or(u32::MAX)
    });
    paths
}

pub(super) fn wayland_displays_from_runtime_dir(
    named_endpoint: bool,
) -> ResultType<Vec<WaylandDisplayInfo>> {
    use std::sync::atomic::Ordering;
    if named_endpoint {
        bail!("an explicit wayland endpoint is set and did not connect");
    }
    if PROBE_UNSUPPORTED.load(Ordering::Acquire) {
        bail!("this binary does not dispatch {WAYLAND_DISPLAY_PROBE_ARG}");
    }
    if RUNTIME_DIR_PROBE_BUSY.swap(true, Ordering::AcqRel) {
        bail!("an earlier probe has not returned");
    }
    let _busy = ProbeBusyGuard;
    let exe = std::env::current_exe()?;
    use std::os::unix::process::CommandExt;
    let mut child = std::process::Command::new(exe)
        .arg(WAYLAND_DISPLAY_PROBE_ARG)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .process_group(0)
        .spawn()?;
    let probe_pgid = child.id() as libc::pid_t;
    let kill_probe_group = || unsafe {
        let _ = libc::kill(-probe_pgid, libc::SIGKILL);
    };
    let deadline = std::time::Instant::now() + RUNTIME_DIR_PROBE_TIMEOUT;
    let status = loop {
        match child.try_wait()? {
            Some(status) => {
                kill_probe_group();
                break status;
            }
            None if std::time::Instant::now() >= deadline => {
                kill_probe_group();
                let _ = child.kill();
                let _ = child.wait();
                match first_buffered_line(child.stdout.take()) {
                    None => {
                        bail!("the wayland socket probe timed out and its output was uninspectable")
                    }
                    Some(head) if head.as_deref() == Some(WAYLAND_PROBE_MAGIC) => {
                        bail!("the wayland socket probe did not answer and was killed");
                    }
                    Some(_) => {
                        PROBE_UNSUPPORTED.store(true, Ordering::Release);
                        bail!("the wayland socket probe timed out without the handshake; probe disabled");
                    }
                }
            }
            None => std::thread::sleep(std::time::Duration::from_millis(25)),
        }
    };
    let stdout = drain_nonblocking(child.stdout.take()).unwrap_or_default();
    let stderr = drain_nonblocking(child.stderr.take()).unwrap_or_default();
    let mut lines = stdout.lines();
    if lines.next() != Some(WAYLAND_PROBE_MAGIC) {
        PROBE_UNSUPPORTED.store(true, Ordering::Release);
        bail!("this binary does not dispatch {WAYLAND_DISPLAY_PROBE_ARG}; probe disabled");
    }
    if !status.success() {
        let detail = stderr.trim();
        if detail.is_empty() {
            bail!("wayland socket probe failed: {status}");
        }
        bail!("wayland socket probe failed ({status}): {detail}");
    }
    let displays: Vec<WaylandDisplayInfo> =
        match serde_json::from_str(lines.next().unwrap_or_default()) {
            Ok(displays) => displays,
            Err(err) => bail!("wayland socket probe answered a malformed list: {err}"),
        };
    if displays.is_empty() {
        bail!("wayland socket probe returned no outputs");
    }
    log::debug!(
        "wayland: {} output(s) via the probe subprocess",
        displays.len()
    );
    Ok(displays)
}

fn drain_nonblocking<R: std::io::Read + std::os::fd::AsRawFd>(pipe: Option<R>) -> Option<String> {
    let mut pipe = pipe?;
    let fd = pipe.as_raw_fd();
    unsafe {
        let flags = libc::fcntl(fd, libc::F_GETFL);
        if flags < 0 || libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) < 0 {
            return None;
        }
    }
    const CAP: usize = 64 * 1024;
    let mut out = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        match pipe.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                out.extend_from_slice(&buf[..n]);
                if out.len() >= CAP {
                    break;
                }
            }
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        }
    }
    Some(String::from_utf8_lossy(&out).into_owned())
}

fn first_buffered_line(pipe: Option<std::process::ChildStdout>) -> Option<Option<String>> {
    drain_nonblocking(pipe).map(|s| s.lines().next().map(str::to_owned))
}

fn probe_runtime_dir(dir: &Path) -> ResultType<Vec<WaylandDisplayInfo>> {
    use std::os::unix::net::UnixStream;
    let mut errs = Vec::new();
    for path in wayland_sockets_in(dir) {
        match UnixStream::connect(&path)
            .map_err(anyhow::Error::from)
            .and_then(|s| Connection::from_socket(s).map_err(anyhow::Error::from))
            .and_then(|conn| collect_wayland_displays(&conn))
        {
            Ok(displays) if displays.is_empty() => {
                errs.push(format!("{}: no outputs yet", path.display()))
            }
            Ok(displays) => {
                log::debug!(
                    "wayland: {} output(s) from {}, found by scanning",
                    displays.len(),
                    path.display()
                );
                return Ok(displays);
            }
            Err(err) => errs.push(format!("{}: {err}", path.display())),
        }
    }
    bail!(
        "no usable wayland socket in {} ({})",
        dir.display(),
        if errs.is_empty() {
            "none present".to_owned()
        } else {
            errs.join("; ")
        }
    )
}
