use std::{
    sync::{Arc, Mutex, RwLock},
    task::Poll,
};

use serde_json::{json, Map, Value};

#[cfg(not(target_os = "ios"))]
use hbb_common::whoami;
use hbb_common::{
    base64,
    bytes::Bytes,
    config::{self, Config, RENDEZVOUS_TIMEOUT},
    futures_util::future::poll_fn,
    log,
    message_proto::*,
    protobuf::Enum,
    socket_client, 
    tokio::{
        self,
        time::{Duration, Instant, Interval},
    },
    ResultType
};
use hbb_common::{config::RENDEZVOUS_PORT, futures::future::join_all};

#[derive(Debug, Eq, PartialEq)]
pub enum GrabState {
    Ready,
    Run,
    Wait,
    Exit,
}

// the executable name of the portable version

pub const PLATFORM_WINDOWS: &str = "Windows";
pub const PLATFORM_ANDROID: &str = "Android";

pub const TIMER_OUT: Duration = Duration::from_secs(1);

const MIN_VER_MULTI_UI_SESSION: &str = "1.41.5";

pub mod input {
    pub const MOUSE_TYPE_MOVE: i32 = 0;
    pub const MOUSE_TYPE_DOWN: i32 = 1;
    pub const MOUSE_TYPE_UP: i32 = 2;
    pub const MOUSE_TYPE_WHEEL: i32 = 3;
    pub const MOUSE_TYPE_TRACKPAD: i32 = 4;
    pub const MOUSE_TYPE_MOVE_RELATIVE: i32 = 5;

    pub const MOUSE_BUTTON_LEFT: i32 = 0x01;
    pub const MOUSE_BUTTON_RIGHT: i32 = 0x02;
    pub const MOUSE_BUTTON_WHEEL: i32 = 0x04;
    pub const MOUSE_BUTTON_BACK: i32 = 0x08;
    pub const MOUSE_BUTTON_FORWARD: i32 = 0x10;
}

lazy_static::lazy_static! {
    pub static ref SOFTWARE_UPDATE_URL: Arc<Mutex<String>> = Default::default();
    pub static ref DEVICE_ID: Arc<Mutex<String>> = Default::default();
    pub static ref DEVICE_NAME: Arc<Mutex<String>> = Default::default();
}

lazy_static::lazy_static! {
    // Is server process, with "--server" args
    static ref IS_SERVER: bool = std::env::args().nth(1) == Some("--server".to_owned());
    // Is server logic running. The server code can invoked to run by the main process if --server is not running.
    static ref SERVER_RUNNING: Arc<RwLock<bool>> = Default::default();
    static ref IS_MAIN: bool = std::env::args().nth(1).map_or(true, |arg| !arg.starts_with("--"));
    static ref IS_CM: bool = std::env::args().nth(1) == Some("--cm".to_owned()) || std::env::args().nth(1) == Some("--cm-no-ui".to_owned());
}

pub struct SimpleCallOnReturn {
    pub b: bool,
    pub f: Box<dyn Fn() + Send + 'static>,
}

impl Drop for SimpleCallOnReturn {
    fn drop(&mut self) {
        if self.b {
            (self.f)();
        }
    }
}

pub fn global_init() -> bool {
    #[cfg(all(target_os = "linux", feature = "drm"))]
    crate::platform::linux::dispatch_wayland_display_probe();
    #[cfg(target_os = "linux")]
    {
        if !crate::platform::linux::is_x11() {
            crate::server::wayland::init();
        }
    }
    true
}

pub fn global_clean() {}

#[inline]
pub fn set_server_running(b: bool) {
    *SERVER_RUNNING.write().unwrap() = b;
}

#[inline]
pub fn is_support_multi_ui_session(ver: &str) -> bool {
    is_support_multi_ui_session_num(hbb_common::get_version_number(ver))
}

#[inline]
pub fn is_support_multi_ui_session_num(ver: i64) -> bool {
    ver >= hbb_common::get_version_number(MIN_VER_MULTI_UI_SESSION)
}

#[inline]
#[cfg(feature = "unix-file-copy-paste")]
pub fn is_support_file_copy_paste(ver: &str) -> bool {
    is_support_file_copy_paste_num(hbb_common::get_version_number(ver))
}

#[inline]
#[cfg(feature = "unix-file-copy-paste")]
pub fn is_support_file_copy_paste_num(ver: i64) -> bool {
    ver >= hbb_common::get_version_number("1.3.8")
}

pub fn is_support_file_paste_if_macos(ver: &str) -> bool {
    hbb_common::get_version_number(ver) >= hbb_common::get_version_number("1.3.9")
}

#[inline]
pub fn is_support_screenshot_num(ver: i64) -> bool {
    ver >= hbb_common::get_version_number("1.44.2")
}

#[inline]
pub fn is_support_file_transfer_resume_num(ver: i64) -> bool {
    ver >= hbb_common::get_version_number("1.4.2")
}

#[inline]
pub fn is_support_switchsides_num(ver: i64) -> bool {
    ver >= hbb_common::get_version_number("1.44.2")
}

#[inline]
pub fn is_support_annotation_num(ver: i64) -> bool {
    ver >= hbb_common::get_version_number("1.46.6")
}

// is server process, with "--server" args
#[inline]
pub fn is_server() -> bool {
    *IS_SERVER
}

#[inline]
pub fn is_main() -> bool {
    *IS_MAIN
}

#[inline]
pub fn is_cm() -> bool {
    *IS_CM
}

// Is server logic running.
#[inline]
pub fn is_server_running() -> bool {
    *SERVER_RUNNING.read().unwrap()
}

#[cfg(target_os = "android")]
pub fn is_peer_path_allowed(path: &str, allow_empty: bool) -> bool {
    use std::path::{Component, Path, PathBuf};

    fn resolve(path: &Path) -> Option<PathBuf> {
        let mut tail = Vec::new();
        let mut base = path.to_path_buf();
        loop {
            if let Ok(mut resolved) = base.canonicalize() {
                while let Some(name) = tail.pop() {
                    resolved.push(name);
                }
                return Some(resolved);
            }
            tail.push(base.file_name()?.to_os_string());
            if !base.pop() {
                return None;
            }
        }
    }

    if path.is_empty() {
        return allow_empty;
    }
    let path = Path::new(path);
    if !path.is_absolute() || path.components().any(|c| c == Component::ParentDir) {
        return false;
    }
    let Some(target) = resolve(path) else {
        return false;
    };
    if target.starts_with("/data") {
        return false;
    }
    let app_dir = PathBuf::from(hbb_common::config::APP_DIR.read().unwrap().clone());
    match app_dir.parent() {
        Some(app_data) if app_data.components().count() > 2 => {
            let app_data = app_data
                .canonicalize()
                .unwrap_or_else(|_| app_data.to_path_buf());
            !target.starts_with(app_data)
        }
        _ => true,
    }
}

#[cfg(not(target_os = "android"))]
#[inline]
pub fn is_peer_path_allowed(_path: &str, _allow_empty: bool) -> bool {
    true
}

/// Get system's default sound input device name.
#[inline]
#[cfg(not(any(target_os = "android", target_os = "ios")))]
pub fn get_default_sound_input() -> Option<String> {
    #[cfg(not(any(target_os = "linux", target_os = "freebsd")))]
    {
        use cpal::traits::{DeviceTrait, HostTrait};
        let host = cpal::default_host();
        let dev = host.default_input_device();
        return if let Some(dev) = dev {
            match dev.name() {
                Ok(name) => Some(name),
                Err(_) => None,
            }
        } else {
            None
        };
    }
    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    {
        let input = crate::platform::linux::get_default_pa_source();
        return if let Some(input) = input {
            Some(input.1)
        } else {
            None
        };
    }
}

#[inline]
#[cfg(any(target_os = "android", target_os = "ios"))]
pub fn get_default_sound_input() -> Option<String> {
    None
}

pub fn get_voice_call_microphone(chosen: String) -> Option<String> {
    Some(chosen).filter(|c| !c.is_empty()).or_else(get_default_sound_input)
}

const RESAMPLE_TAPS: usize = 32;
const RESAMPLE_PHASES: usize = 256;

pub struct AudioResampler {
    step: f64,
    channels: usize,
    position: f64,
    frames: Vec<f32>,
    table: Vec<f32>,
}

impl AudioResampler {
    pub fn new(from_hz: u32, to_hz: u32, channels: u16) -> Self {
        let channels = channels.max(1) as usize;
        let half = (RESAMPLE_TAPS / 2) as f64;
        let cutoff = 0.92 * (to_hz as f64 / from_hz as f64).min(1.0);
        let mut table = Vec::with_capacity((RESAMPLE_PHASES + 1) * RESAMPLE_TAPS);
        for phase in 0..=RESAMPLE_PHASES {
            let frac = phase as f64 / RESAMPLE_PHASES as f64;
            let row: Vec<f64> = (0..RESAMPLE_TAPS)
                .map(|k| {
                    let x = k as f64 - (half - 1.0) - frac;
                    let sinc = if x == 0.0 {
                        1.0
                    } else {
                        let a = std::f64::consts::PI * cutoff * x;
                        a.sin() / a
                    };
                    let w = std::f64::consts::PI * x / half;
                    let window = if x.abs() >= half {
                        0.0
                    } else {
                        0.42 + 0.5 * w.cos() + 0.08 * (2.0 * w).cos()
                    };
                    sinc * window
                })
                .collect();
            let sum: f64 = row.iter().sum();
            table.extend(row.iter().map(|v| (v / sum) as f32));
        }
        Self {
            step: from_hz as f64 / to_hz as f64,
            channels,
            position: half - 1.0,
            frames: vec![0.0; channels * RESAMPLE_TAPS],
            table,
        }
    }

    pub fn process(&mut self, input: &[f32]) -> Vec<f32> {
        let ch = self.channels;
        let half = RESAMPLE_TAPS / 2;
        self.frames
            .extend_from_slice(&input[..input.len() / ch * ch]);
        let available = self.frames.len() / ch;
        let mut output =
            Vec::with_capacity(((input.len() / ch) as f64 / self.step) as usize * ch + ch);
        while (self.position as usize) + half < available {
            let i = self.position as usize;
            let phase = (self.position - i as f64) * RESAMPLE_PHASES as f64;
            let p = (phase as usize).min(RESAMPLE_PHASES - 1);
            let w = (phase - p as f64) as f32;
            let row0 = &self.table[p * RESAMPLE_TAPS..(p + 1) * RESAMPLE_TAPS];
            let row1 = &self.table[(p + 1) * RESAMPLE_TAPS..(p + 2) * RESAMPLE_TAPS];
            let first = i + 1 - half;
            for c in 0..ch {
                let mut acc = 0.0f32;
                for k in 0..RESAMPLE_TAPS {
                    let tap = row0[k] + (row1[k] - row0[k]) * w;
                    acc += self.frames[(first + k) * ch + c] * tap;
                }
                output.push(acc);
            }
            self.position += self.step;
        }
        let keep_from = ((self.position as usize) + 1)
            .saturating_sub(half)
            .min(available);
        self.frames.drain(..keep_from * ch);
        self.position -= keep_from as f64;
        output
    }
}

fn downmix_surround(input: &[f32], in_chan: u16, output_chan: u16) -> Vec<f32> {
    const C: f32 = std::f32::consts::FRAC_1_SQRT_2;
    let gains: &[(f32, f32)] = match in_chan {
        3 => &[(1.0, 0.0), (0.0, 1.0), (C, C)],
        4 => &[(1.0, 0.0), (0.0, 1.0), (C, 0.0), (0.0, C)],
        5 => &[(1.0, 0.0), (0.0, 1.0), (C, C), (C, 0.0), (0.0, C)],
        6 => &[(1.0, 0.0), (0.0, 1.0), (C, C), (0.0, 0.0), (C, 0.0), (0.0, C)],
        7 => &[(1.0, 0.0), (0.0, 1.0), (C, C), (0.0, 0.0), (C, C), (C, 0.0), (0.0, C)],
        _ => &[(1.0, 0.0), (0.0, 1.0), (C, C), (0.0, 0.0), (C, 0.0), (0.0, C), (C, 0.0), (0.0, C)],
    };
    let mut output = Vec::with_capacity(input.len() / in_chan as usize * output_chan as usize);
    for frame in input.chunks_exact(in_chan as usize) {
        let (mut left, mut right) = (0.0f32, 0.0f32);
        for (sample, (gl, gr)) in frame.iter().zip(gains) {
            left += sample * gl;
            right += sample * gr;
        }
        if output_chan == 1 {
            output.push((left + right) * 0.5);
        } else {
            output.push(left);
            output.push(right);
        }
    }
    output
}

const LIMITER_RELEASE_SECS: f32 = 0.2;

pub struct PeakLimiter {
    gain: f32,
    release: f32,
}

impl PeakLimiter {
    pub fn new(sample_rate: u32, channels: u16) -> Self {
        Self {
            gain: 1.0,
            release: 1.0 / (LIMITER_RELEASE_SECS * sample_rate as f32 * channels.max(1) as f32),
        }
    }

    pub fn process(&mut self, samples: &mut [f32]) {
        for sample in samples {
            let level = sample.abs();
            if level * self.gain > 1.0 {
                self.gain = 1.0 / level;
            }
            *sample *= self.gain;
            self.gain = (self.gain + self.release).min(1.0);
        }
    }
}

pub fn audio_rechannel(
    input: Vec<f32>,
    in_hz: u32,
    out_hz: u32,
    in_chan: u16,
    output_chan: u16,
) -> Vec<f32> {
    if in_chan == output_chan {
        return input;
    }
    let mut input = input;
    input.truncate(input.len() / in_chan as usize * in_chan as usize);
    if (3..=8).contains(&in_chan) && output_chan <= 2 {
        return downmix_surround(&input, in_chan, output_chan);
    }
    match (in_chan, output_chan) {
        (1, 2) => audio_rechannel_1_2(&input, in_hz, out_hz),
        (1, 3) => audio_rechannel_1_3(&input, in_hz, out_hz),
        (1, 4) => audio_rechannel_1_4(&input, in_hz, out_hz),
        (1, 5) => audio_rechannel_1_5(&input, in_hz, out_hz),
        (1, 6) => audio_rechannel_1_6(&input, in_hz, out_hz),
        (1, 7) => audio_rechannel_1_7(&input, in_hz, out_hz),
        (1, 8) => audio_rechannel_1_8(&input, in_hz, out_hz),
        (2, 1) => audio_rechannel_2_1(&input, in_hz, out_hz),
        (2, 3) => audio_rechannel_2_3(&input, in_hz, out_hz),
        (2, 4) => audio_rechannel_2_4(&input, in_hz, out_hz),
        (2, 5) => audio_rechannel_2_5(&input, in_hz, out_hz),
        (2, 6) => audio_rechannel_2_6(&input, in_hz, out_hz),
        (2, 7) => audio_rechannel_2_7(&input, in_hz, out_hz),
        (2, 8) => audio_rechannel_2_8(&input, in_hz, out_hz),
        (3, 4) => audio_rechannel_3_4(&input, in_hz, out_hz),
        (3, 5) => audio_rechannel_3_5(&input, in_hz, out_hz),
        (3, 6) => audio_rechannel_3_6(&input, in_hz, out_hz),
        (3, 7) => audio_rechannel_3_7(&input, in_hz, out_hz),
        (3, 8) => audio_rechannel_3_8(&input, in_hz, out_hz),
        (4, 3) => audio_rechannel_4_3(&input, in_hz, out_hz),
        (4, 5) => audio_rechannel_4_5(&input, in_hz, out_hz),
        (4, 6) => audio_rechannel_4_6(&input, in_hz, out_hz),
        (4, 7) => audio_rechannel_4_7(&input, in_hz, out_hz),
        (4, 8) => audio_rechannel_4_8(&input, in_hz, out_hz),
        (5, 3) => audio_rechannel_5_3(&input, in_hz, out_hz),
        (5, 4) => audio_rechannel_5_4(&input, in_hz, out_hz),
        (5, 6) => audio_rechannel_5_6(&input, in_hz, out_hz),
        (5, 7) => audio_rechannel_5_7(&input, in_hz, out_hz),
        (5, 8) => audio_rechannel_5_8(&input, in_hz, out_hz),
        (6, 3) => audio_rechannel_6_3(&input, in_hz, out_hz),
        (6, 4) => audio_rechannel_6_4(&input, in_hz, out_hz),
        (6, 5) => audio_rechannel_6_5(&input, in_hz, out_hz),
        (6, 7) => audio_rechannel_6_7(&input, in_hz, out_hz),
        (6, 8) => audio_rechannel_6_8(&input, in_hz, out_hz),
        (7, 3) => audio_rechannel_7_3(&input, in_hz, out_hz),
        (7, 4) => audio_rechannel_7_4(&input, in_hz, out_hz),
        (7, 5) => audio_rechannel_7_5(&input, in_hz, out_hz),
        (7, 6) => audio_rechannel_7_6(&input, in_hz, out_hz),
        (7, 8) => audio_rechannel_7_8(&input, in_hz, out_hz),
        (8, 3) => audio_rechannel_8_3(&input, in_hz, out_hz),
        (8, 4) => audio_rechannel_8_4(&input, in_hz, out_hz),
        (8, 5) => audio_rechannel_8_5(&input, in_hz, out_hz),
        (8, 6) => audio_rechannel_8_6(&input, in_hz, out_hz),
        (8, 7) => audio_rechannel_8_7(&input, in_hz, out_hz),
        _ => input,
    }
}

macro_rules! audio_rechannel {
    ($name:ident, $in_channels:expr, $out_channels:expr) => {
        fn $name(input: &[f32], in_hz: u32, out_hz: u32) -> Vec<f32> {
            use fon::{chan::Ch32, Audio, Frame};
            let mut in_audio =
                Audio::<Ch32, $in_channels>::with_silence(in_hz, input.len() / $in_channels);
            for (x, y) in input.chunks_exact($in_channels).zip(in_audio.iter_mut()) {
                let mut f = Frame::<Ch32, $in_channels>::default();
                let mut i = 0;
                for c in f.channels_mut() {
                    *c = x[i].into();
                    i += 1;
                }
                *y = f;
            }
            Audio::<Ch32, $out_channels>::with_audio(out_hz, &in_audio)
                .as_f32_slice()
                .to_owned()
        }
    };
}

audio_rechannel!(audio_rechannel_1_2, 1, 2);
audio_rechannel!(audio_rechannel_1_3, 1, 3);
audio_rechannel!(audio_rechannel_1_4, 1, 4);
audio_rechannel!(audio_rechannel_1_5, 1, 5);
audio_rechannel!(audio_rechannel_1_6, 1, 6);
audio_rechannel!(audio_rechannel_1_7, 1, 7);
audio_rechannel!(audio_rechannel_1_8, 1, 8);
audio_rechannel!(audio_rechannel_2_1, 2, 1);
audio_rechannel!(audio_rechannel_2_3, 2, 3);
audio_rechannel!(audio_rechannel_2_4, 2, 4);
audio_rechannel!(audio_rechannel_2_5, 2, 5);
audio_rechannel!(audio_rechannel_2_6, 2, 6);
audio_rechannel!(audio_rechannel_2_7, 2, 7);
audio_rechannel!(audio_rechannel_2_8, 2, 8);
audio_rechannel!(audio_rechannel_3_4, 3, 4);
audio_rechannel!(audio_rechannel_3_5, 3, 5);
audio_rechannel!(audio_rechannel_3_6, 3, 6);
audio_rechannel!(audio_rechannel_3_7, 3, 7);
audio_rechannel!(audio_rechannel_3_8, 3, 8);
audio_rechannel!(audio_rechannel_4_3, 4, 3);
audio_rechannel!(audio_rechannel_4_5, 4, 5);
audio_rechannel!(audio_rechannel_4_6, 4, 6);
audio_rechannel!(audio_rechannel_4_7, 4, 7);
audio_rechannel!(audio_rechannel_4_8, 4, 8);
audio_rechannel!(audio_rechannel_5_3, 5, 3);
audio_rechannel!(audio_rechannel_5_4, 5, 4);
audio_rechannel!(audio_rechannel_5_6, 5, 6);
audio_rechannel!(audio_rechannel_5_7, 5, 7);
audio_rechannel!(audio_rechannel_5_8, 5, 8);
audio_rechannel!(audio_rechannel_6_3, 6, 3);
audio_rechannel!(audio_rechannel_6_4, 6, 4);
audio_rechannel!(audio_rechannel_6_5, 6, 5);
audio_rechannel!(audio_rechannel_6_7, 6, 7);
audio_rechannel!(audio_rechannel_6_8, 6, 8);
audio_rechannel!(audio_rechannel_7_3, 7, 3);
audio_rechannel!(audio_rechannel_7_4, 7, 4);
audio_rechannel!(audio_rechannel_7_5, 7, 5);
audio_rechannel!(audio_rechannel_7_6, 7, 6);
audio_rechannel!(audio_rechannel_7_8, 7, 8);
audio_rechannel!(audio_rechannel_8_3, 8, 3);
audio_rechannel!(audio_rechannel_8_4, 8, 4);
audio_rechannel!(audio_rechannel_8_5, 8, 5);
audio_rechannel!(audio_rechannel_8_6, 8, 6);
audio_rechannel!(audio_rechannel_8_7, 8, 7);

pub fn test_nat_type() {
    let mut i = 0;
    std::thread::spawn(move || loop {
        if Config::get_nat_type() != 0 {
            break;
        }
        i = i * 2 + 1;
        if i > 300 {
            i = 300;
        }
        std::thread::sleep(std::time::Duration::from_secs(i));
    });
}



#[cfg(any(target_os = "android", target_os = "ios"))]
pub async fn get_rendezvous_server(_ms_timeout: u64) -> Option<String> {
    Config::get_rendezvous_server().await
}

#[inline]
#[cfg(not(any(target_os = "android", target_os = "ios")))]
pub async fn get_rendezvous_server(ms_timeout: u64) -> Option<String> {
    crate::ipc::get_rendezvous_server(ms_timeout).await
}

#[inline]
#[cfg(any(target_os = "android", target_os = "ios"))]
pub async fn get_nat_type(_ms_timeout: u64) -> i32 {
    Config::get_nat_type()
}

#[inline]
#[cfg(not(any(target_os = "android", target_os = "ios")))]
pub async fn get_nat_type(ms_timeout: u64) -> i32 {
    crate::ipc::get_nat_type(ms_timeout).await
}

#[tokio::main(flavor = "current_thread")]
async fn test_rendezvous_server_() {
    if let Some(servers) = Config::get_rendezvous_servers().await {
	    if servers.len() <= 1 {
	        return;
	    }
        let mut futs = Vec::new();
        for host in servers {
            futs.push(tokio::spawn(async move {
                let tm = std::time::Instant::now();
                if socket_client::connect_tcp(
                    crate::check_port(&host, RENDEZVOUS_PORT),
                    RENDEZVOUS_TIMEOUT,
                )
                .await
                .is_ok()
                {
                    let elapsed = tm.elapsed().as_micros();
                    Config::update_latency(&host, elapsed as _);
                } else {
                    Config::update_latency(&host, -1);
                }
            }));
        }
        join_all(futs).await;
    }
}


pub fn test_rendezvous_server() {
    std::thread::spawn(test_rendezvous_server_);
}

pub const ENV_PEER_PASSWORD: &str = "HOPTODESK_PEER_PASSWORD";
pub const ENV_PEER_TOKEN: &str = "HOPTODESK_PEER_TOKEN";
pub const ENV_PEER_CONN_TOKEN: &str = "HOPTODESK_PEER_CONN_TOKEN";
pub const ENV_INVITE_PASSWORD: &str = "HOPTODESK_INVITE_PASSWORD";

pub fn take_env_secret(key: &str) -> String {
    let value = std::env::var(key).unwrap_or_default();
    if !value.is_empty() {
        std::env::remove_var(key);
    }
    value
}

pub fn run_me<T: AsRef<std::ffi::OsStr>>(args: Vec<T>) -> std::io::Result<std::process::Child> {
    run_me_with_secrets(args, Vec::new())
}

pub fn run_me_with_secrets<T: AsRef<std::ffi::OsStr>>(
    args: Vec<T>,
    secrets: Vec<(&str, String)>,
) -> std::io::Result<std::process::Child> {
    let mut cmd = me_command()?;
    cmd.envs(secrets.iter().map(|(k, v)| (*k, v.as_str())));
    #[cfg(windows)]
    let mut force_foreground = false;
    #[cfg(windows)]
    {
        let arg_strs = args
            .iter()
            .map(|x| x.as_ref().to_string_lossy())
            .collect::<Vec<_>>();
        if arg_strs == vec!["--install"] || arg_strs == &["--noinstall"] {
            cmd.env(crate::platform::SET_FOREGROUND_WINDOW, "1");
            force_foreground = true;
        }
    }
    let result = cmd.args(&args).spawn();
    match result.as_ref() {
        Ok(_child) =>
        {
            #[cfg(windows)]
            if force_foreground {
                unsafe { winapi::um::winuser::AllowSetForegroundWindow(_child.id() as u32) };
            }
        }
        Err(err) => log::error!("run_me: {err:?}"),
    }
    result
}

pub fn run_me_piped<T: AsRef<std::ffi::OsStr>>(
    args: Vec<T>,
    secrets: Vec<(&str, String)>,
) -> std::io::Result<std::process::Child> {
    me_command()?
        .envs(secrets.iter().map(|(k, v)| (*k, v.as_str())))
        .args(&args)
        .stdin(std::process::Stdio::piped())
        .spawn()
}

fn me_command() -> std::io::Result<std::process::Command> {
    #[cfg(target_os = "linux")]
    if let Ok(appdir) = std::env::var("APPDIR") {
        let appimage_cmd = std::path::Path::new(&appdir).join("AppRun");
        if appimage_cmd.exists() {
            return Ok(std::process::Command::new(appimage_cmd));
        }
    }
    Ok(std::process::Command::new(std::env::current_exe()?))
}

#[inline]
pub fn username() -> String {
    // fix bug of whoami
    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    return whoami::username().trim_end_matches('\0').to_owned();
    #[cfg(any(target_os = "android", target_os = "ios"))]
    return DEVICE_NAME.lock().unwrap().clone();
}

#[inline(always)]
#[cfg(not(target_os = "ios"))]
pub fn whoami_hostname() -> String {
    let mut hostname = whoami::fallible::hostname().unwrap_or_else(|_| "localhost".to_string());
    hostname.make_ascii_lowercase();
    hostname
}

#[inline]
pub fn hostname() -> String {
    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    {
        #[allow(unused_mut)]
        let mut name = whoami_hostname();
        // some time, there is .local, some time not, so remove it for osx
        #[cfg(target_os = "macos")]
        if name.ends_with(".local") {
            name = name.trim_end_matches(".local").to_owned();
        }
        name
    }
    #[cfg(any(target_os = "android", target_os = "ios"))]
    return DEVICE_NAME.lock().unwrap().clone();
}

#[inline]
pub fn check_port<T: std::string::ToString>(host: T, port: i32) -> String {
    hbb_common::socket_client::check_port(host, port)
}

#[inline]
pub fn increase_port<T: std::string::ToString>(host: T, offset: i32) -> String {
    hbb_common::socket_client::increase_port(host, offset)
}

pub const POSTFIX_SERVICE: &'static str = "_service";

#[inline]
pub fn is_control_key(evt: &KeyEvent, key: &ControlKey) -> bool {
    if let Some(key_event::Union::ControlKey(ck)) = evt.union {
        ck.value() == key.value()
    } else {
        false
    }
}

#[inline]
pub fn is_modifier(evt: &KeyEvent) -> bool {
    if let Some(key_event::Union::ControlKey(ck)) = evt.union {
        let v = ck.value();
        v == ControlKey::Alt.value()
            || v == ControlKey::Shift.value()
            || v == ControlKey::Control.value()
            || v == ControlKey::Meta.value()
            || v == ControlKey::RAlt.value()
            || v == ControlKey::RShift.value()
            || v == ControlKey::RControl.value()
            || v == ControlKey::RWin.value()
    } else {
        false
    }
}

/*
pub fn check_software_update() {
    std::thread::spawn(move || allow_err!(check_software_update_()));
}

#[tokio::main(flavor = "current_thread")]
async fn check_software_update_() -> hbb_common::ResultType<()> {
    let latest_release_response = create_http_client_async().get(url).send().await?;
    let latest_release_version = latest_release_response
        .url()
        .path()
        .rsplit('/')
        .next()
        .unwrap_or_default();

    let response_url = latest_release_response.url().to_string();

    if get_version_number(&latest_release_version) > get_version_number(crate::VERSION) {
        #[cfg(feature = "flutter")]
        {
            let mut m = HashMap::new();
            m.insert("name", "check_software_update_finish");
            m.insert("url", &response_url);
            if let Ok(data) = serde_json::to_string(&m) {
                let _ = crate::flutter::push_global_event(crate::flutter::APP_TYPE_MAIN, data);
            }
        }
        *SOFTWARE_UPDATE_URL.lock().unwrap() = response_url;
    }
    Ok(())
}
*/

#[inline]
pub fn get_app_name() -> String {
    hbb_common::config::APP_NAME.read().unwrap().clone()
}

#[inline]
pub fn get_app_name_lower() -> String {
    get_app_name().to_lowercase().replace(' ', "")
}

#[inline]
pub fn get_uri_prefix() -> String {
    format!("{}://", get_app_name_lower())
}

#[cfg(target_os = "macos")]
pub fn get_full_name() -> String {
    format!(
        "{}.{}",
        hbb_common::config::ORG.read().unwrap(),
        hbb_common::config::APP_NAME.read().unwrap(),
    )
}


pub fn is_setup(name: &str) -> bool {
    name.to_lowercase().ends_with("install.exe")
}

/*
pub fn get_custom_rendezvous_server(custom: String) -> String {
    #[cfg(windows)]
    if let Ok(lic) = crate::platform::windows::get_license_from_exe_name() {
        if !lic.host.is_empty() {
            return lic.host.clone();
        }
    }
    if !custom.is_empty() {
        return custom;
    }
    if !config::PROD_RENDEZVOUS_SERVER.read().unwrap().is_empty() {
        return config::PROD_RENDEZVOUS_SERVER.read().unwrap().clone();
    }
    "".to_owned()
}

pub fn get_api_server(api: String, custom: String) -> String {
    #[cfg(windows)]
    if let Ok(lic) = crate::platform::windows::get_license_from_exe_name() {
        if !lic.api.is_empty() {
            return lic.api.clone();
        }
    }
    if !api.is_empty() {
        return api.to_owned();
    }
    let api = option_env!("API_SERVER").unwrap_or_default();
    if !api.is_empty() {
        return api.into();
    }
    let s0 = get_custom_rendezvous_server(custom);
    if !s0.is_empty() {
        let s = crate::increase_port(&s0, -2);
        if s == s0 {
            let s = if hbb_common::is_ipv6_str(&s) && !s.starts_with('[') {
                format!("[{s}]")
            } else {
                s
            };
            return format!("http://{}:{}", s, config::RENDEZVOUS_PORT - 2);
        } else {
            return format!("http://{}", s);
        }
    }
    "https://admin.none.com".to_owned()
}

pub fn get_audit_server(api: String, custom: String, typ: String) -> String {
    let url = get_api_server(api, custom);
    if url.is_empty() || url.contains("rustdesk.com") {
        return "".to_owned();
    }
    format!("{}/api/audit/{}", url, typ)
}
*/

/// Build an HTTP client that routes through the proxy when configured.
pub fn make_http_client() -> reqwest::Client {
    let mut builder =
        reqwest::Client::builder().connect_timeout(std::time::Duration::from_secs(10));
    if let Some(conf) = config::Config::get_socks() {
        let proxy_url = hbb_common::proxy::proxy_url(&conf);
        if let Ok(proxy) = reqwest::Proxy::all(&proxy_url) {
            builder = builder.proxy(proxy);
        }
    }
    builder.build().unwrap_or_else(|_| reqwest::Client::new())
}

pub async fn post_request(url: String, body: String, header: &str) -> ResultType<String> {
    #[cfg(not(any(target_os = "linux", target_os = "freebsd")))]
    {
        let mut req = make_http_client().post(url);
        if !header.is_empty() {
            let tmp: Vec<&str> = header.split(": ").collect();
            if tmp.len() == 2 {
                req = req.header(tmp[0], tmp[1]);
            }
        }
        req = req.header("Content-Type", "application/json");
        let to = std::time::Duration::from_secs(12);
        Ok(req.body(body).timeout(to).send().await?.text().await?)
    }
    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    {
        let mut data = vec![
            "-sS",
            "-X",
            "POST",
            &url,
            "-H",
            "Content-Type: application/json",
            "-d",
            &body,
            "--connect-timeout",
            "12",
        ];
        if !header.is_empty() {
            data.push("-H");
            data.push(header);
        }
        // Add proxy to curl if configured
        let socks_arg;
        if let Some(conf) = config::Config::get_socks() {
            socks_arg = hbb_common::proxy::proxy_url(&conf);
            data.push("--proxy");
            data.push(&socks_arg);
        }
        let output = async_process::Command::new("curl")
            .args(&data)
            .output()
            .await?;
        let res = String::from_utf8_lossy(&output.stdout).to_string();
        if !res.is_empty() {
            return Ok(res);
        }
        hbb_common::bail!(String::from_utf8_lossy(&output.stderr).to_string());
    }
}

#[tokio::main(flavor = "current_thread")]
pub async fn post_request_sync(url: String, body: String, header: &str) -> ResultType<String> {
    post_request(url, body, header).await
}

pub async fn get_request(url: String, header: &str) -> ResultType<String> {
	let mut req = make_http_client().get(url);
	if !header.is_empty() {
		let tmp: Vec<&str> = header.split(": ").collect();
		if tmp.len() == 2 {
			req = req.header(tmp[0], tmp[1]);
		}
	}
	let to = std::time::Duration::from_secs(12);
	Ok(req.timeout(to).send().await?.text().await?)
}

#[tokio::main(flavor = "current_thread")]
pub async fn get_request_sync(url: String, header: &str) -> ResultType<String> {
    get_request(url, header).await
}

#[inline]
pub fn make_privacy_mode_msg_with_details(
    state: back_notification::PrivacyModeState,
    details: String,
    impl_key: String,
) -> Message {
    let mut misc = Misc::new();
    let mut back_notification = BackNotification {
        details,
        impl_key,
        ..Default::default()
    };
    back_notification.set_privacy_mode_state(state);
    misc.set_back_notification(back_notification);
    let mut msg_out = Message::new();
    msg_out.set_misc(misc);
    msg_out
}

#[inline]
pub fn make_privacy_mode_msg(
    state: back_notification::PrivacyModeState,
    impl_key: String,
) -> Message {
    make_privacy_mode_msg_with_details(state, "".to_owned(), impl_key)
}

pub fn is_keyboard_mode_supported(
    keyboard_mode: &KeyboardMode,
    version_number: i64,
    peer_platform: &str,
) -> bool {
    match keyboard_mode {
        KeyboardMode::Legacy => true,
        KeyboardMode::Map => {
            if peer_platform.to_lowercase() == crate::PLATFORM_ANDROID.to_lowercase() {
                false
            } else {
                version_number >= hbb_common::get_version_number("1.2.0")
            }
        }
        KeyboardMode::Translate => version_number >= hbb_common::get_version_number("1.2.0"),
        KeyboardMode::Auto => version_number >= hbb_common::get_version_number("1.2.0"),
    }
}

pub fn get_supported_keyboard_modes(version: i64, peer_platform: &str) -> Vec<KeyboardMode> {
    KeyboardMode::iter()
        .filter(|&mode| is_keyboard_mode_supported(mode, version, peer_platform))
        .map(|&mode| mode)
        .collect::<Vec<_>>()
}

pub fn make_fd_to_json(id: i32, path: String, entries: &Vec<FileEntry>) -> String {
    let fd_json = _make_fd_to_json(id, path, entries);
    serde_json::to_string(&fd_json).unwrap_or("".into())
}

pub fn _make_fd_to_json(id: i32, path: String, entries: &Vec<FileEntry>) -> Map<String, Value> {
    let mut fd_json = serde_json::Map::new();
    fd_json.insert("id".into(), json!(id));
    fd_json.insert("path".into(), json!(path));

    let mut entries_out = vec![];
    for entry in entries {
        let mut entry_map = serde_json::Map::new();
        entry_map.insert("entry_type".into(), json!(entry.entry_type.value()));
        entry_map.insert("name".into(), json!(entry.name));
        entry_map.insert("size".into(), json!(entry.size));
        entry_map.insert("modified_time".into(), json!(entry.modified_time));
        entries_out.push(entry_map);
    }
    fd_json.insert("entries".into(), json!(entries_out));
    fd_json
}

pub fn make_vec_fd_to_json(fds: &[FileDirectory]) -> String {
    let mut fd_jsons = vec![];

    for fd in fds.iter() {
        let fd_json = _make_fd_to_json(fd.id, fd.path.clone(), &fd.entries);
        fd_jsons.push(fd_json);
    }

    serde_json::to_string(&fd_jsons).unwrap_or("".into())
}

pub fn make_empty_dirs_response_to_json(res: &ReadEmptyDirsResponse) -> String {
    let mut map: Map<String, Value> = serde_json::Map::new();
    map.insert("path".into(), json!(res.path));

    let mut fd_jsons = vec![];

    for fd in res.empty_dirs.iter() {
        let fd_json = _make_fd_to_json(fd.id, fd.path.clone(), &fd.entries);
        fd_jsons.push(fd_json);
    }
    map.insert("empty_dirs".into(), fd_jsons.into());

    serde_json::to_string(&map).unwrap_or("".into())
}

/// The function to handle the url scheme sent by the system.
///
/// 1. Try to send the url scheme from ipc.
/// 2. If failed to send the url scheme, we open a new main window to handle this url scheme.
pub fn handle_url_scheme(url: String) {
    #[cfg(not(target_os = "ios"))]
    if let Err(err) = crate::ipc::send_url_scheme(url.clone()) {
        log::debug!("No main window took the url, {}. Opening a new one to handle it.", err);
        let _ = if is_session_link(&url) {
            crate::run_me(vec!["--connect".to_owned(), url])
        } else {
            crate::run_me(vec![url])
        };
    }
}

#[cfg(not(target_os = "ios"))]
fn is_session_link(url: &str) -> bool {
    let link_path = url.split_once("://").map_or("", |(_, path)| path);
    ["sso-login/", "file-transfer/", "terminal/"]
        .iter()
        .any(|kind| link_path.starts_with(kind))
}

#[inline]
pub fn encode64<T: AsRef<[u8]>>(input: T) -> String {
    #[allow(deprecated)]
    base64::encode(input)
}

#[inline]
pub fn decode64<T: AsRef<[u8]>>(input: T) -> Result<Vec<u8>, base64::DecodeError> {
    #[allow(deprecated)]
    base64::decode(input)
}


#[allow(unused_mut)]
#[cfg(not(any(target_os = "android", target_os = "ios")))]
pub fn check_process(arg: &str, mut same_uid: bool) -> bool {
    #[cfg(target_os = "macos")]
    if !crate::platform::is_root() && !same_uid {
        log::warn!("Can not get other process's command line arguments on macos without root");
        same_uid = true;
    }
    use hbb_common::sysinfo::System;
    let mut sys = System::new();
    sys.refresh_processes();
    let mut path = std::env::current_exe().unwrap_or_default();
    if let Ok(linked) = path.read_link() {
        path = linked;
    }
    let path = path.to_string_lossy().to_lowercase();
    let my_uid = sys
        .process((std::process::id() as usize).into())
        .map(|x| x.user_id())
        .unwrap_or_default();
    for (_, p) in sys.processes().iter() {
        let mut cur_path = p.exe().to_path_buf();
        if let Ok(linked) = cur_path.read_link() {
            cur_path = linked;
        }
        if cur_path.to_string_lossy().to_lowercase() != path {
            continue;
        }
        if p.pid().to_string() == std::process::id().to_string() {
            continue;
        }
        if same_uid && p.user_id() != my_uid {
            continue;
        }
        let parg = if p.cmd().len() <= 1 { "" } else { &p.cmd()[1] };
        if arg.is_empty() {
            if !parg.starts_with("--") {
                return true;
            }
        } else if arg == parg {
            return true;
        }
    }
    false
}


pub struct ThrottledInterval {
    interval: Interval,
    next_tick: Instant,
    min_interval: Duration,
}

impl ThrottledInterval {
    pub fn new(i: Interval) -> ThrottledInterval {
        let period = i.period();
        ThrottledInterval {
            interval: i,
            next_tick: Instant::now(),
            min_interval: Duration::from_secs_f64(period.as_secs_f64() * 0.9),
        }
    }

    pub async fn tick(&mut self) -> Instant {
        let instant = poll_fn(|cx| self.poll_tick(cx));
        instant.await
    }

    pub fn poll_tick(&mut self, cx: &mut std::task::Context<'_>) -> Poll<Instant> {
        match self.interval.poll_tick(cx) {
            Poll::Ready(instant) => {
                let now = Instant::now();
                if self.next_tick <= now {
                    self.next_tick = now + self.min_interval;
                    Poll::Ready(instant)
                } else {
                    // This call is required since tokio 1.27
                    cx.waker().wake_by_ref();
                    Poll::Pending
                }
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

pub type RustDeskInterval = ThrottledInterval;

#[inline]
pub fn rustdesk_interval(i: Interval) -> ThrottledInterval {
    ThrottledInterval::new(i)
}


#[inline]
#[cfg(target_os = "macos")]
pub fn get_dst_align_rgba() -> usize {
    // https://developer.apple.com/forums/thread/712709
    // Memory alignment should be multiple of 64.
    if crate::ui_interface::use_texture_render() {
        64
    } else {
        1
    }
}

#[inline]
#[cfg(not(target_os = "macos"))]
pub fn get_dst_align_rgba() -> usize {
    1
}
#[cfg(any(windows, target_os = "macos"))]
pub fn apply_managed_enrollment_config() {
    if !config::Config::get_option("dashboard_user_id").is_empty() {
        return;
    }
    if !crate::dashboard::get_prefilled_enrollment_token().is_empty() {
        return;
    }
    if let Some(token) = read_managed_enrollment_token() {
        crate::dashboard::set_prefilled_enrollment_token(&token);
    }
}

#[cfg(not(any(windows, target_os = "macos")))]
pub fn apply_managed_enrollment_config() {}

#[cfg(windows)]
fn read_managed_enrollment_token() -> Option<String> {
    use winreg::enums::*;
    use winreg::RegKey;
    let hklm = RegKey::predef(HKEY_LOCAL_MACHINE);
    for path in ["SOFTWARE\\Policies\\Beacon", "SOFTWARE\\Beacon"] {
        for perms in [KEY_READ | KEY_WOW64_64KEY, KEY_READ] {
            if let Ok(key) = hklm.open_subkey_with_flags(path, perms) {
                if let Ok(val) = key.get_value::<String, _>("EnrollmentToken") {
                    if !val.trim().is_empty() {
                        return Some(val);
                    }
                }
            }
        }
    }
    None
}

#[cfg(target_os = "macos")]
fn read_managed_enrollment_token() -> Option<String> {
    let bundle = "com.hoptodesk.hoptodesk";
    let mut paths = vec![format!("/Library/Managed Preferences/{}.plist", bundle)];
    if let Ok(out) = std::process::Command::new("/usr/bin/stat")
        .args(["-f", "%Su", "/dev/console"])
        .output()
    {
        let user = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if !user.is_empty() {
            paths.push(format!("/Library/Managed Preferences/{}/{}.plist", user, bundle));
        }
    }
    for path in &paths {
        if !std::path::Path::new(path).exists() {
            continue;
        }
        if let Ok(out) = std::process::Command::new("/usr/bin/plutil")
            .args(["-extract", "enrollment_token", "raw", "-o", "-", path])
            .output()
        {
            if out.status.success() {
                let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
                if !s.is_empty() {
                    return Some(s);
                }
            }
        }
    }
    None
}

const BUILTIN_CUSTOM_CLIENT: &str = include_str!("../custom-client.json");

pub fn builtin_network() -> Option<serde_json::Value> {
    let data = serde_json::from_str::<serde_json::Value>(BUILTIN_CUSTOM_CLIENT.trim()).ok()?;
    let network = &data["network"];
    (network["network_type"] == "custom" && network["api_json"].is_object())
        .then(|| network["api_json"].clone())
}

pub fn apply_builtin_custom_client() {
    let trimmed = BUILTIN_CUSTOM_CLIENT.trim();
    if trimmed.is_empty() || trimmed == "{}" {
        return;
    }
    let Ok(mut data) =
        serde_json::from_str::<std::collections::HashMap<String, serde_json::Value>>(trimmed)
    else {
        log::error!("Failed to parse builtin custom client config");
        return;
    };
    if let Some(serde_json::Value::Object(over)) = data.remove("override-settings") {
        let local: std::collections::HashSet<String> = config::keys::KEYS_LOCAL_SETTINGS
            .iter()
            .map(|s| s.replace('_', "-"))
            .collect();
        let display: std::collections::HashSet<String> = config::keys::KEYS_DISPLAY_SETTINGS
            .iter()
            .map(|s| s.replace('_', "-"))
            .collect();
        for (k, v) in over {
            if let Some(s) = v.as_str() {
                let value = s.to_owned();
                if local.contains(&k) {
                    config::OVERWRITE_LOCAL_SETTINGS.write().unwrap().insert(k, value);
                } else if display.contains(&k) {
                    config::OVERWRITE_DISPLAY_SETTINGS.write().unwrap().insert(k, value);
                } else {
                    config::OVERWRITE_SETTINGS.write().unwrap().insert(k, value);
                }
            }
        }
    }
    if let Some(serde_json::Value::String(token)) = data.remove("enrollment-token") {
        #[cfg(not(target_os = "ios"))]
        if config::Config::get_option("dashboard_user_id").is_empty()
            && crate::dashboard::get_prefilled_enrollment_token().is_empty()
        {
            crate::dashboard::set_prefilled_enrollment_token(&token);
        }
        #[cfg(target_os = "ios")]
        let _ = token;
    }
    if let Some(network) = data.remove("network") {
        #[cfg(not(target_os = "ios"))]
        crate::dashboard::seed_builtin_network(&network);
        #[cfg(target_os = "ios")]
        let _ = network;
    }
    for (k, v) in data {
        if let Some(s) = v.as_str() {
            config::HARD_SETTINGS
                .write()
                .unwrap()
                .insert(k, s.to_owned());
        }
    }
}

#[inline]
pub fn is_empty_uni_link(arg: &str) -> bool {
    let prefix = crate::get_uri_prefix();
    if !arg.starts_with(&prefix) {
        return false;
    }
    arg[prefix.len()..].chars().all(|c| c == '/')
}

pub fn get_hwid() -> Bytes {
    use sha2::{Digest, Sha256};

    let uuid = hbb_common::get_uuid();
    let mut hasher = Sha256::new();
    hasher.update(&uuid);
    Bytes::from(hasher.finalize().to_vec())
}

// The color is the same to `str2color()` in flutter.
pub fn str2color(s: &str, alpha: u8) -> u32 {
    let bytes = s.as_bytes();
    // dart code `160 << 16 + 114 << 8 + 91` results `0`.
    let mut hash: u32 = 0;
    for &byte in bytes {
        let code = byte as u32;
        hash = code.wrapping_add((hash << 5).wrapping_sub(hash));
    }

    hash = hash % 16777216;
    let rgb = hash & 0xFF7FFF;

    (alpha as u32) << 24 | rgb
}

const MAX_UNTRUSTED_PEER_ID_LEN: usize = 253;
const UNTRUSTED_PEER_ID_FORBIDDEN_CHARS: &[char] = &['"', '<', '>', '/', '\\', '|', '?', '*'];

pub fn is_valid_untrusted_peer_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_UNTRUSTED_PEER_ID_LEN
        && !id.chars().any(|ch| {
            ch.is_control() || ch.is_whitespace() || UNTRUSTED_PEER_ID_FORBIDDEN_CHARS.contains(&ch)
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use hbb_common::tokio::{
        self,
        time::{interval, interval_at, sleep, Duration, Instant, Interval},
    };
    use std::collections::HashSet;

    fn tone(rate: u32, channels: usize, active: &[usize], freq: f64, seconds: f64) -> Vec<f32> {
        let frames = (rate as f64 * seconds) as usize;
        let mut out = vec![0.0f32; frames * channels];
        for i in 0..frames {
            let v = (0.25 * (2.0 * std::f64::consts::PI * freq * i as f64 / rate as f64).sin()) as f32;
            for &c in active {
                out[i * channels + c] = v;
            }
        }
        out
    }

    fn noise_db(samples: &[f32], channels: usize, rate: f64, freq: f64) -> f64 {
        let left: Vec<f64> = samples.iter().step_by(channels).map(|v| *v as f64).collect();
        let body = &left[left.len() / 10..left.len() * 9 / 10];
        let offset = left.len() / 10;
        let angle = |i: usize| 2.0 * std::f64::consts::PI * freq * (i + offset) as f64 / rate;
        let (s, c) = body.iter().enumerate().fold((0.0, 0.0), |(s, c), (i, v)| {
            (s + v * angle(i).sin(), c + v * angle(i).cos())
        });
        let (a, b) = (2.0 * s / body.len() as f64, 2.0 * c / body.len() as f64);
        let (signal, residual) = body.iter().enumerate().fold((0.0, 0.0), |(sg, r), (i, v)| {
            let fit = a * angle(i).sin() + b * angle(i).cos();
            (sg + fit * fit, r + (v - fit).powi(2))
        });
        10.0 * (residual / signal).log10()
    }

    #[test]
    fn resampler_stays_clean_across_10ms_chunks() {
        for (from, to) in [(44100, 48000), (48000, 44100), (96000, 48000)] {
            for freq in [1000.0, 5000.0, 15000.0] {
                let input = tone(from, 2, &[0, 1], freq, 0.5);
                let mut resampler = AudioResampler::new(from, to, 2);
                let output: Vec<f32> = input
                    .chunks(from as usize / 100 * 2)
                    .flat_map(|chunk| resampler.process(chunk))
                    .collect();
                let db = noise_db(&output, 2, to as f64, freq);
                assert!(db < -70.0, "{from} to {to} Hz at {freq} Hz: noise {db:.1} dB");
            }
        }
    }

    #[test]
    fn surround_downmix_keeps_front_stereo_at_full_level() {
        for channels in [6usize, 8] {
            let input = tone(48000, channels, &[0, 1], 1000.0, 0.2);
            let output = audio_rechannel(input, 48000, 48000, channels as u16, 2);
            let peak = output.iter().fold(0.0f32, |m, v| m.max(*v));
            assert!((peak - 0.25).abs() < 0.001, "{channels} channels: peak {peak}");
            let loud = tone(48000, channels, &(0..channels).collect::<Vec<_>>(), 1000.0, 0.2)
                .iter()
                .map(|v| v * 3.2)
                .collect();
            let mut mixed = audio_rechannel(loud, 48000, 48000, channels as u16, 2);
            PeakLimiter::new(48000, 2).process(&mut mixed);
            assert!(mixed.iter().all(|v| v.abs() <= 1.0), "{channels} channels clipped");
        }
    }

    #[test]
    fn peak_limiter_leaves_normal_levels_alone_and_recovers_smoothly() {
        let quiet = tone(48000, 2, &[0, 1], 1000.0, 0.2);
        let mut untouched = quiet.clone();
        PeakLimiter::new(48000, 2).process(&mut untouched);
        assert_eq!(untouched, quiet);

        let loud: Vec<f32> = quiet.iter().map(|v| v * 10.0).collect();
        let signal = [loud, quiet.clone(), quiet.clone()].concat();
        let mut whole = signal.clone();
        PeakLimiter::new(48000, 2).process(&mut whole);
        let mut split = signal.clone();
        let mut limiter = PeakLimiter::new(48000, 2);
        for chunk in split.chunks_mut(960) {
            limiter.process(chunk);
        }
        assert_eq!(whole, split);
        assert!(whole.iter().all(|v| v.abs() <= 1.0));
        let steps = whole.windows(2).map(|w| (w[1] - w[0]).abs()).fold(0.0f32, f32::max);
        let source_steps = signal.windows(2).map(|w| (w[1] - w[0]).abs()).fold(0.0f32, f32::max);
        assert!(steps <= source_steps);
        let tail = &whole[whole.len() - quiet.len()..];
        assert_eq!(tail, &quiet[..]);
    }

    #[test]
    fn untrusted_peer_id_validation() {
        let cases = [
            ("123456789", true),
            ("m\u{00FC}nchen-pc", true),
            ("192.168.1.10:21118", true),
            ("9123456234@public", true),
            (
                r#"1" & oWS.Run("cmd.exe /k whoami /priv",1,False) & ""#,
                false,
            ),
            ("", false),
            ("peer id", false),
            ("peer\nid", false),
            ("peer/id", false),
            ("peer?id", false),
        ];

        for (id, expected) in cases {
            assert_eq!(is_valid_untrusted_peer_id(id), expected, "{id:?}");
        }
    }

    #[inline]
    fn get_timestamp_secs() -> u128 {
        (std::time::SystemTime::UNIX_EPOCH
            .elapsed()
            .unwrap()
            .as_millis()
            + 500)
            / 1000
    }

    fn interval_maker() -> Interval {
        interval(Duration::from_secs(1))
    }

    fn interval_at_maker() -> Interval {
        interval_at(
            Instant::now() + Duration::from_secs(1),
            Duration::from_secs(1),
        )
    }

    // ThrottledInterval tick at the same time as tokio interval, if no sleeps
    #[allow(non_snake_case)]
    #[tokio::test]
    async fn test_RustDesk_interval() {
        let base_intervals = [interval_maker, interval_at_maker];
        for maker in base_intervals.into_iter() {
            let mut tokio_timer = maker();
            let mut tokio_times = Vec::new();
            let mut timer = rustdesk_interval(maker());
            let mut times = Vec::new();
            loop {
                tokio::select! {
                    _ = timer.tick() => {
                        if tokio_times.len() >= 10 && times.len() >= 10 {
                            break;
                        }
                        times.push(get_timestamp_secs());
                    }
                    _ = tokio_timer.tick() => {
                        if tokio_times.len() >= 10 && times.len() >= 10 {
                            break;
                        }
                        tokio_times.push(get_timestamp_secs());
                    }
                }
            }
            assert_eq!(times, tokio_times);
        }
    }

    #[tokio::test]
    async fn test_tokio_time_interval_sleep() {
        let mut timer = interval_maker();
        let mut times = Vec::new();
        sleep(Duration::from_secs(3)).await;
        loop {
            tokio::select! {
                _ = timer.tick() => {
                    times.push(get_timestamp_secs());
                    if times.len() == 5 {
                        break;
                    }
                }
            }
        }
        let times2: HashSet<u128> = HashSet::from_iter(times.clone());
        assert_eq!(times.len(), times2.len() + 3);
    }

    // ThrottledInterval tick less times than tokio interval, if there're sleeps
    #[allow(non_snake_case)]
    #[tokio::test]
    async fn test_RustDesk_interval_sleep() {
        let base_intervals = [interval_maker, interval_at_maker];
        for (i, maker) in base_intervals.into_iter().enumerate() {
            let mut timer = rustdesk_interval(maker());
            let mut times = Vec::new();
            sleep(Duration::from_secs(3)).await;
            loop {
                tokio::select! {
                    _ = timer.tick() => {
                        times.push(get_timestamp_secs());
                        if times.len() == 5 {
                            break;
                        }
                    }
                }
            }
            // No mutliple ticks in the `interval` time.
            // Values in "times" are unique and are less than normal tokio interval.
            // See previous test (test_tokio_time_interval_sleep) for comparison.
            let times2: HashSet<u128> = HashSet::from_iter(times.clone());
            assert_eq!(times.len(), times2.len(), "test: {}", i);
        }
    }

    #[test]
    fn test_duration_multiplication() {
        let dur = Duration::from_secs(1);

        assert_eq!(dur * 2, Duration::from_secs(2));
        assert_eq!(
            Duration::from_secs_f64(dur.as_secs_f64() * 0.9),
            Duration::from_millis(900)
        );
        assert_eq!(
            Duration::from_secs_f64(dur.as_secs_f64() * 0.923),
            Duration::from_millis(923)
        );
        assert_eq!(
            Duration::from_secs_f64(dur.as_secs_f64() * 0.923 * 1e-3),
            Duration::from_micros(923)
        );
        assert_eq!(
            Duration::from_secs_f64(dur.as_secs_f64() * 0.923 * 1e-6),
            Duration::from_nanos(923)
        );
        assert_eq!(
            Duration::from_secs_f64(dur.as_secs_f64() * 0.923 * 1e-9),
            Duration::from_nanos(1)
        );
        assert_eq!(
            Duration::from_secs_f64(dur.as_secs_f64() * 0.5 * 1e-9),
            Duration::from_nanos(1)
        );
        assert_eq!(
            Duration::from_secs_f64(dur.as_secs_f64() * 0.499 * 1e-9),
            Duration::from_nanos(0)
        );
    }
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

pub fn configured_display_name() -> String {
    let mut name = get_builtin_option(config::keys::OPTION_DISPLAY_NAME);
    if name.is_empty() {
        name = config::LocalConfig::get_option(config::keys::OPTION_DISPLAY_NAME);
    }
    if name.is_empty() {
        name = serde_json::from_str::<serde_json::Value>(&config::LocalConfig::get_option(
            "user_info",
        ))
        .map(|x| {
            x.get("name")
                .and_then(|n| n.as_str())
                .unwrap_or_default()
                .to_owned()
        })
        .unwrap_or_default();
    }
    name
}
