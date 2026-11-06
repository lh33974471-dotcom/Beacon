#![cfg(any(windows, target_os = "macos"))]

use crate::codec::{EncoderApi, EncoderCfg};
#[cfg(windows)]
use crate::mediafoundation::{list_hardware_h264_encoders, MfEncoder as NatBenchEncoder};
#[cfg(target_os = "macos")]
use crate::videotoolbox::{list_hardware_h264_encoders, VtEncoder as NatBenchEncoder};
use crate::vpxcodec::{VpxEncoder, VpxEncoderConfig, VpxVideoCodecId};
use std::time::Instant;
#[cfg(windows)]
use windows::Win32::Foundation::FILETIME;
#[cfg(windows)]
use windows::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};

const BENCH_FRAMES: usize = 240;

#[cfg(windows)]
fn process_cpu_secs() -> f64 {
    unsafe {
        let mut creation = FILETIME::default();
        let mut exit = FILETIME::default();
        let mut kernel = FILETIME::default();
        let mut user = FILETIME::default();
        if GetProcessTimes(
            GetCurrentProcess(),
            &mut creation,
            &mut exit,
            &mut kernel,
            &mut user,
        )
        .is_ok()
        {
            let to_secs = |ft: FILETIME| {
                (((ft.dwHighDateTime as u64) << 32) | ft.dwLowDateTime as u64) as f64 / 1e7
            };
            to_secs(kernel) + to_secs(user)
        } else {
            0.0
        }
    }
}

#[cfg(target_os = "macos")]
fn process_cpu_secs() -> f64 {
    #[repr(C)]
    struct Timeval {
        tv_sec: i64,
        tv_usec: i32,
    }
    #[repr(C)]
    struct Rusage {
        ru_utime: Timeval,
        ru_stime: Timeval,
        pad: [i64; 14],
    }
    extern "C" {
        fn getrusage(who: i32, usage: *mut Rusage) -> i32;
    }
    unsafe {
        let mut ru = std::mem::zeroed::<Rusage>();
        if getrusage(0, &mut ru) == 0 {
            ru.ru_utime.tv_sec as f64
                + ru.ru_utime.tv_usec as f64 / 1e6
                + ru.ru_stime.tv_sec as f64
                + ru.ru_stime.tv_usec as f64 / 1e6
        } else {
            0.0
        }
    }
}

fn make_i420(w: usize, h: usize) -> Vec<u8> {
    vec![16u8; w * h * 3 / 2]
}

fn make_nv12(w: usize, h: usize) -> Vec<u8> {
    vec![16u8; w * h * 3 / 2]
}

fn stir(buf: &mut [u8], frame: usize) {
    let n = buf.len();
    let stride = 719usize;
    let mut i = (frame * 37) % stride;
    while i < n {
        buf[i] = (frame as u8).wrapping_mul(31).wrapping_add(buf[i]);
        i += stride;
    }
}

struct Result {
    label: String,
    ok: bool,
    frames: usize,
    wall_secs: f64,
    cpu_secs: f64,
    detail: String,
}

impl Result {
    fn line(&self) -> String {
        if !self.ok {
            return format!("{:<28} FAILED: {}", self.label, self.detail);
        }
        let fps = self.frames as f64 / self.wall_secs.max(0.0001);
        let cpu_cores = self.cpu_secs / self.wall_secs.max(0.0001);
        format!(
            "{:<28} {:>3} frames  {:>6.1} fps  cpu {:>6.2}s  ({:>4.0}% of one core)",
            self.label, self.frames, fps, self.cpu_secs, cpu_cores * 100.0
        )
    }
}

fn bench_vp9(w: usize, h: usize) -> Result {
    let label = "VP9 software".to_owned();
    let cfg = EncoderCfg::VPX(VpxEncoderConfig {
        width: w as _,
        height: h as _,
        quality: 1.0,
        codec: VpxVideoCodecId::VP9,
        keyframe_interval: None,
    });
    let mut enc = match VpxEncoder::new(cfg, false) {
        Ok(e) => e,
        Err(e) => {
            return Result {
                label,
                ok: false,
                frames: 0,
                wall_secs: 0.0,
                cpu_secs: 0.0,
                detail: format!("{e:?}"),
            }
        }
    };
    let mut buf = make_i420(w, h);
    let cpu0 = process_cpu_secs();
    let t0 = Instant::now();
    for i in 0..BENCH_FRAMES {
        stir(&mut buf, i);
        if let Ok(frames) = enc.encode(i as i64, &buf, 1) {
            for _ in frames {}
        }
    }
    let wall = t0.elapsed().as_secs_f64();
    let cpu = process_cpu_secs() - cpu0;
    Result {
        label,
        ok: true,
        frames: BENCH_FRAMES,
        wall_secs: wall,
        cpu_secs: cpu,
        detail: String::new(),
    }
}

fn bench_mf(name: &str, w: usize, h: usize) -> Result {
    let label = format!("H264 hw [{name}]");
    let bitrate = HwRamEncoderBitrate::for_size(w, h);
    let mut enc = match NatBenchEncoder::new(name, w, h, bitrate, 30) {
        Ok(e) => e,
        Err(e) => {
            return Result {
                label,
                ok: false,
                frames: 0,
                wall_secs: 0.0,
                cpu_secs: 0.0,
                detail: format!("{e:?}"),
            }
        }
    };
    let mut buf = make_nv12(w, h);
    let mut produced = 0usize;
    let cpu0 = process_cpu_secs();
    let t0 = Instant::now();
    for i in 0..BENCH_FRAMES {
        stir(&mut buf, i);
        match enc.encode_nv12(&buf, (i as i64) * 33) {
            Ok(frames) => produced += frames.len(),
            Err(_) => {}
        }
    }
    let wall = t0.elapsed().as_secs_f64();
    let cpu = process_cpu_secs() - cpu0;
    Result {
        label,
        ok: true,
        frames: BENCH_FRAMES,
        wall_secs: wall,
        cpu_secs: cpu,
        detail: format!("{produced} packets"),
    }
}

struct HwRamEncoderBitrate;
impl HwRamEncoderBitrate {
    fn for_size(w: usize, h: usize) -> u32 {
        crate::hwcodec::HwRamEncoder::calc_bitrate(w, h, 1.0, true)
    }
}

pub fn run(width: usize, height: usize) -> String {
    let (w, h) = if width == 0 || height == 0 {
        (3840, 2160)
    } else {
        (width, height)
    };
    let mut out = String::new();
    out.push_str(&format!(
        "codec benchmark: {w}x{h}, {BENCH_FRAMES} frames per encoder\n",
    ));
    out.push_str(&format!("{}\n", bench_vp9(w, h).line()));
    let names = list_hardware_h264_encoders();
    if names.is_empty() {
        out.push_str("H264 hw                      no hardware H264 encoder found\n");
    }
    for name in names {
        out.push_str(&format!("{}\n", bench_mf(&name, w, h).line()));
    }
    out
}
