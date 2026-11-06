use super::{PrivacyMode, PrivacyModeState};
#[cfg(target_os = "linux")]
use hbb_common::tokio;
use hbb_common::{bail, ResultType};
use std::process::{Child, Command, Stdio};
#[cfg(target_os = "linux")]
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};

pub const PRIVACY_MODE_IMPL: &str = "privacy_mode_impl_linux";

const VIRTUAL_GPU_DRIVERS: &[&str] = &[
    "vboxvideo",
    "vmwgfx",
    "virtio_gpu",
    "qxl",
    "bochs",
    "bochs-drm",
    "cirrus",
    "hyperv_drm",
    "hyperv_fb",
    "simpledrm",
    "simple-framebuffer",
];

pub fn is_supported() -> bool {
    if crate::platform::is_x11() {
        return has_gamma_capable_gpu() && has_cmd("xrandr") && has_cmd("xinput");
    }
    let supported;
    #[cfg(target_os = "linux")]
    {
        supported = !super::linux_backlight::backlight_devices().is_empty();
    }
    #[cfg(not(target_os = "linux"))]
    {
        supported = false;
    }
    supported
}

#[cfg(target_os = "linux")]
struct BacklightSession {
    stop_tx: Sender<()>,
    thread: std::thread::JoinHandle<()>,
}

#[cfg(target_os = "linux")]
#[tokio::main(flavor = "current_thread")]
async fn run_backlight_session(ready_tx: Sender<bool>, stop_rx: Receiver<()>) {
    let mut stream = match crate::ipc::connect(1000, crate::POSTFIX_SERVICE).await {
        Ok(stream) => stream,
        Err(_) => {
            let _ = ready_tx.send(false);
            return;
        }
    };
    if stream
        .send(&crate::ipc::Data::PrivacyBacklight {
            blank: true,
            block_input: true,
        })
        .await
        .is_err()
    {
        let _ = ready_tx.send(false);
        return;
    }
    let on = matches!(
        stream.next_timeout(2_000).await,
        Ok(Some(crate::ipc::Data::PrivacyBacklight { blank: true, .. }))
    );
    let _ = ready_tx.send(on);
    if !on {
        return;
    }
    while let Err(RecvTimeoutError::Timeout) =
        stop_rx.recv_timeout(std::time::Duration::from_millis(300))
    {}
    let _ = stream
        .send(&crate::ipc::Data::PrivacyBacklight {
            blank: false,
            block_input: false,
        })
        .await;
    let _ = stream.next_timeout(1_000).await;
}

#[cfg(target_os = "linux")]
fn start_backlight_session() -> ResultType<BacklightSession> {
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let (stop_tx, stop_rx) = std::sync::mpsc::channel();
    let thread = std::thread::spawn(move || run_backlight_session(ready_tx, stop_rx));
    match ready_rx.recv_timeout(std::time::Duration::from_secs(5)) {
        Ok(true) => Ok(BacklightSession { stop_tx, thread }),
        _ => {
            let _ = stop_tx.send(());
            let _ = thread.join();
            bail!("Failed to turn on privacy mode");
        }
    }
}

#[cfg(target_os = "linux")]
fn stop_backlight_session(session: BacklightSession) {
    let _ = session.stop_tx.send(());
    let _ = session.thread.join();
}

#[cfg(target_os = "linux")]
#[tokio::main(flavor = "current_thread")]
async fn request_backlight_off() -> ResultType<()> {
    let mut stream = crate::ipc::connect(1000, crate::POSTFIX_SERVICE).await?;
    stream
        .send(&crate::ipc::Data::PrivacyBacklight {
            blank: false,
            block_input: false,
        })
        .await?;
    let _ = stream.next_timeout(1_000).await;
    Ok(())
}

fn has_gamma_capable_gpu() -> bool {
    let entries = match std::fs::read_dir("/sys/class/drm") {
        Ok(entries) => entries,
        Err(_) => return true,
    };
    let mut found_driver = false;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !name.starts_with("card") || name.contains('-') {
            continue;
        }
        if let Ok(link) = std::fs::read_link(entry.path().join("device/driver")) {
            if let Some(driver) = link.file_name() {
                found_driver = true;
                if !VIRTUAL_GPU_DRIVERS.contains(&driver.to_string_lossy().as_ref()) {
                    return true;
                }
            }
        }
    }
    !found_driver
}

fn has_cmd(cmd: &str) -> bool {
    Command::new("which")
        .arg(cmd)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn connected_outputs_with_brightness() -> ResultType<Vec<(String, String)>> {
    let output = Command::new("xrandr").arg("--verbose").output()?;
    if !output.status.success() {
        bail!("Failed to query displays");
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let mut outputs = Vec::new();
    let mut cur: Option<String> = None;
    for line in text.lines() {
        if !line.starts_with(' ') && !line.starts_with('\t') {
            let mut parts = line.split_whitespace();
            let name = parts.next().unwrap_or_default().to_owned();
            cur = match parts.next() {
                Some("connected") => Some(name),
                _ => None,
            };
        } else if let Some(name) = &cur {
            if let Some(value) = line.trim().strip_prefix("Brightness:") {
                outputs.push((name.clone(), value.trim().to_owned()));
                cur = None;
            }
        }
    }
    Ok(outputs)
}

fn enabled_physical_inputs() -> ResultType<Vec<String>> {
    let output = Command::new("xinput").args(["list", "--short"]).output()?;
    if !output.status.success() {
        bail!("Failed to query input devices");
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let mut ids = Vec::new();
    for line in text.lines() {
        if !line.contains("slave") || line.contains("XTEST") {
            continue;
        }
        if !line.contains("pointer") && !line.contains("keyboard") {
            continue;
        }
        if let Some(pos) = line.find("id=") {
            let id: String = line[pos + 3..]
                .chars()
                .take_while(|c| c.is_ascii_digit())
                .collect();
            if !id.is_empty() && is_input_enabled(&id) {
                ids.push(id);
            }
        }
    }
    Ok(ids)
}

fn is_input_enabled(id: &str) -> bool {
    Command::new("xinput")
        .args(["list-props", id])
        .output()
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .any(|l| l.contains("Device Enabled") && l.trim_end().ends_with('1'))
        })
        .unwrap_or(false)
}

fn spawn_watchdog(outputs: &[(String, String)], inputs: &[String]) -> ResultType<Child> {
    let mut restore = String::new();
    for (output, brightness) in outputs {
        restore.push_str(&format!(
            "xrandr --output {} --brightness {}; ",
            output, brightness
        ));
    }
    for id in inputs {
        restore.push_str(&format!("xinput enable {}; ", id));
    }
    let script = format!(
        "while kill -0 {} 2>/dev/null; do sleep 1; done; {}",
        std::process::id(),
        restore
    );
    Ok(Command::new("sh")
        .args(["-c", &script])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?)
}

pub struct PrivacyModeImpl {
    impl_key: String,
    conn_id: i32,
    saved_brightness: Vec<(String, String)>,
    disabled_inputs: Vec<String>,
    watchdog: Option<Child>,
    #[cfg(target_os = "linux")]
    backlight_session: Option<BacklightSession>,
}

impl PrivacyModeImpl {
    pub fn new(impl_key: &str) -> Self {
        Self {
            impl_key: impl_key.to_owned(),
            conn_id: 0,
            saved_brightness: Vec::new(),
            disabled_inputs: Vec::new(),
            watchdog: None,
            #[cfg(target_os = "linux")]
            backlight_session: None,
        }
    }

    fn restore(&mut self) {
        if !crate::platform::is_x11() {
            #[cfg(target_os = "linux")]
            match self.backlight_session.take() {
                Some(session) => stop_backlight_session(session),
                None => {
                    let _ = request_backlight_off();
                }
            }
            return;
        }
        for (output, brightness) in &self.saved_brightness {
            let _ = Command::new("xrandr")
                .args(["--output", output, "--brightness", brightness])
                .status();
        }
        for id in &self.disabled_inputs {
            let _ = Command::new("xinput").args(["enable", id]).status();
        }
        self.saved_brightness.clear();
        self.disabled_inputs.clear();
        if let Some(mut child) = self.watchdog.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

impl PrivacyMode for PrivacyModeImpl {
    fn is_async_privacy_mode(&self) -> bool {
        false
    }

    fn init(&self) -> ResultType<()> {
        Ok(())
    }

    fn clear(&mut self) {
        self.restore();
        self.conn_id = 0;
    }

    fn turn_on_privacy(&mut self, conn_id: i32) -> ResultType<bool> {
        if self.check_on_conn_id(conn_id)? {
            return Ok(true);
        }
        if !crate::platform::is_x11() {
            #[cfg(target_os = "linux")]
            {
                self.backlight_session = Some(start_backlight_session()?);
                self.conn_id = conn_id;
                return Ok(true);
            }
            #[cfg(not(target_os = "linux"))]
            bail!("Privacy mode is not supported here");
        }
        let outputs = connected_outputs_with_brightness()?;
        if outputs.is_empty() {
            bail!("No physical displays");
        }
        let inputs = enabled_physical_inputs()?;
        self.watchdog = Some(spawn_watchdog(&outputs, &inputs)?);
        self.saved_brightness = outputs;
        self.disabled_inputs = inputs;
        let mut ok = true;
        for (output, _) in &self.saved_brightness {
            ok &= Command::new("xrandr")
                .args(["--output", output, "--brightness", "0"])
                .status()
                .map(|s| s.success())
                .unwrap_or(false);
        }
        if ok {
            for id in &self.disabled_inputs {
                ok &= Command::new("xinput")
                    .args(["disable", id])
                    .status()
                    .map(|s| s.success())
                    .unwrap_or(false);
            }
        }
        if !ok {
            self.restore();
            bail!("Failed to turn on privacy mode");
        }
        self.conn_id = conn_id;
        Ok(true)
    }

    fn turn_off_privacy(
        &mut self,
        conn_id: i32,
        _state: Option<PrivacyModeState>,
    ) -> ResultType<()> {
        self.check_off_conn_id(conn_id)?;
        self.restore();
        self.conn_id = 0;
        Ok(())
    }

    fn pre_conn_id(&self) -> i32 {
        self.conn_id
    }

    fn get_impl_key(&self) -> &str {
        &self.impl_key
    }
}

impl Drop for PrivacyModeImpl {
    fn drop(&mut self) {
        self.clear();
    }
}
