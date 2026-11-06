use hbb_common::{libc, log, ResultType};
use std::{
    io::Read,
    os::unix::{
        fs::PermissionsExt,
        net::{UnixListener, UnixStream},
    },
    sync::mpsc::{channel, Sender},
    time::Duration,
};

pub const PRINTER_SOCKET_PATH: &str = "/tmp/hoptodesk-printer.sock";
const MAX_PRINT_JOB_BYTES: u64 = 1 << 30;
#[cfg(target_os = "macos")]
const FIRST_LOGIN_UID: u32 = 500;
#[cfg(not(target_os = "macos"))]
const FIRST_LOGIN_UID: u32 = 1000;

pub fn peer_uid(stream: &UnixStream) -> Option<u32> {
    use std::os::unix::io::AsRawFd;
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    return crate::ipc::peer_uid_from_fd(stream.as_raw_fd());
    #[cfg(target_os = "freebsd")]
    {
        let mut uid: libc::uid_t = 0;
        let mut gid: libc::gid_t = 0;
        let rc = unsafe { libc::getpeereid(stream.as_raw_fd(), &mut uid, &mut gid) };
        (rc == 0).then_some(uid as u32)
    }
}

fn may_send_print_job(uid: Option<u32>) -> bool {
    let Some(uid) = uid else {
        return false;
    };
    uid < FIRST_LOGIN_UID || uid == unsafe { libc::geteuid() } as u32
}

fn read_print_job(mut stream: UnixStream) -> Option<Vec<u8>> {
    if !may_send_print_job(peer_uid(&stream)) {
        log::warn!("Print job refused: the sender is not the print system");
        return None;
    }
    let _ = stream.set_nonblocking(false);
    let mut data = Vec::new();
    let read = (&mut stream)
        .take(MAX_PRINT_JOB_BYTES + 1)
        .read_to_end(&mut data);
    match read {
        Ok(_) if data.is_empty() => log::warn!("Empty print job received"),
        Ok(_) if data.len() as u64 > MAX_PRINT_JOB_BYTES => log::warn!("Print job refused: too large"),
        Ok(_) => return Some(data),
        Err(e) => log::error!("Error reading print data: {}", e),
    }
    None
}

pub fn start_print_pipe_server<F: Fn(Vec<u8>) + Send + 'static>(
    callback: F,
) -> ResultType<Sender<()>> {
    let _ = std::fs::remove_file(PRINTER_SOCKET_PATH);

    let listener = UnixListener::bind(PRINTER_SOCKET_PATH)?;
    std::fs::set_permissions(PRINTER_SOCKET_PATH, std::fs::Permissions::from_mode(0o777))?;
    listener.set_nonblocking(true)?;

    let (stop_tx, stop_rx) = channel::<()>();
    std::thread::spawn(move || {
        log::info!("Print socket server listening on {}", PRINTER_SOCKET_PATH);
        loop {
            if stop_rx.try_recv().is_ok() {
                break;
            }
            match listener.accept() {
                Ok((stream, _)) => {
                    if let Some(data) = read_print_job(stream) {
                        log::info!("Received print job: {} bytes", data.len());
                        callback(data);
                    }
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_secs(1));
                }
                Err(e) => {
                    log::error!("Accept error: {}", e);
                    std::thread::sleep(Duration::from_secs(1));
                }
            }
        }
        let _ = std::fs::remove_file(PRINTER_SOCKET_PATH);
        log::info!("Print socket server stopped");
    });

    Ok(stop_tx)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_print_system_may_send() {
        assert!(may_send_print_job(Some(0)));
        assert!(may_send_print_job(Some(unsafe { libc::geteuid() } as u32)));
        assert!(!may_send_print_job(Some(54321)));
        assert!(!may_send_print_job(None));
    }
}
