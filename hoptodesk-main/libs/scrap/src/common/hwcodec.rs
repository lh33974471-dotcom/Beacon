use crate::{
    codec::{base_bitrate, enable_hwcodec_option, EncoderApi, EncoderCfg},
    CodecFormat, EncodeInput, Pixfmt,
};
#[cfg(windows)]
use crate::{
    convert::*,
    mediafoundation::gpu_signature as nat_gpu_signature,
    mediafoundation::probe_hardware_h264 as nat_probe_hardware_h264,
    mediafoundation::MfDecodedFrame as NatDecodedFrame,
    mediafoundation::MfDecoder as NatDecoder,
    mediafoundation::MfEncoder as NatEncoder,
    ImageFormat,
};
#[cfg(target_os = "macos")]
use crate::{
    convert::*,
    videotoolbox::gpu_signature as nat_gpu_signature,
    videotoolbox::probe_hardware_h264 as nat_probe_hardware_h264,
    videotoolbox::VtDecodedFrame as NatDecodedFrame,
    videotoolbox::VtDecoder as NatDecoder,
    videotoolbox::VtEncoder as NatEncoder,
    ImageFormat,
};
use hbb_common::{
    anyhow::{anyhow, bail},
    bytes::Bytes,
    log,
    message_proto::{EncodedVideoFrame, EncodedVideoFrames, VideoFrame},
    serde_derive::{Deserialize, Serialize},
    serde_json, ResultType,
};

pub const DEFAULT_FPS: i32 = 30;

#[cfg(any(windows, target_os = "macos"))]
crate::generate_call_macro!(call_yuv, false);

lazy_static::lazy_static! {
    static ref CONFIG: std::sync::Arc<std::sync::Mutex<Option<HwCodecConfig>>> = Default::default();
    static ref CONFIG_SET_BY_IPC: std::sync::Arc<std::sync::Mutex<bool>> = Default::default();
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DataFormat {
    VP8,
    VP9,
    AV1,
    H264,
    H265,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodecInfo {
    pub name: String,
    #[serde(default)]
    pub mc_name: Option<String>,
    pub format: DataFormat,
    #[serde(default)]
    pub priority: i32,
}

#[derive(Debug, Default, Clone)]
pub struct CodecInfos {
    pub h264: Option<CodecInfo>,
    pub h265: Option<CodecInfo>,
}

impl CodecInfo {
    pub fn prioritized(infos: Vec<CodecInfo>) -> CodecInfos {
        let mut best = CodecInfos::default();
        for info in infos {
            match info.format {
                DataFormat::H264 => {
                    if best
                        .h264
                        .as_ref()
                        .map_or(true, |c| info.priority < c.priority)
                    {
                        best.h264 = Some(info);
                    }
                }
                DataFormat::H265 => {
                    if best
                        .h265
                        .as_ref()
                        .map_or(true, |c| info.priority < c.priority)
                    {
                        best.h265 = Some(info);
                    }
                }
                _ => {}
            }
        }
        best
    }

    pub fn soft() -> CodecInfos {
        #[cfg(windows)]
        {
            CodecInfos {
                h264: Some(CodecInfo {
                    name: "h264_mf_dec".to_owned(),
                    mc_name: None,
                    format: DataFormat::H264,
                    priority: 0,
                }),
                h265: None,
            }
        }
        #[cfg(target_os = "macos")]
        {
            CodecInfos {
                h264: Some(CodecInfo {
                    name: "h264_vt_dec".to_owned(),
                    mc_name: None,
                    format: DataFormat::H264,
                    priority: 0,
                }),
                h265: None,
            }
        }
        #[cfg(not(any(windows, target_os = "macos")))]
        {
            CodecInfos::default()
        }
    }
}

#[derive(Debug, Clone)]
pub struct HwRamEncoderConfig {
    pub name: String,
    pub mc_name: Option<String>,
    pub width: usize,
    pub height: usize,
    pub quality: f32,
    pub keyframe_interval: Option<usize>,
}

pub struct EncodeFrame {
    pub data: Vec<u8>,
    pub pts: i64,
    pub key: i32,
}

pub struct HwRamEncoder {
    #[cfg(any(windows, target_os = "macos"))]
    encoder: NatEncoder,
    pub format: DataFormat,
    bitrate: u32,
    config: HwRamEncoderConfig,
}

impl EncoderApi for HwRamEncoder {
    fn new(cfg: EncoderCfg, _i444: bool) -> ResultType<Self>
    where
        Self: Sized,
    {
        match cfg {
            #[cfg(any(windows, target_os = "macos"))]
            EncoderCfg::HWRAM(config) => {
                let mut bitrate =
                    Self::bitrate(&config.name, config.width, config.height, config.quality);
                bitrate = Self::check_bitrate_range(&config, bitrate);
                let encoder = NatEncoder::new(
                    &config.name,
                    config.width,
                    config.height,
                    bitrate,
                    DEFAULT_FPS as u32,
                )
                .map_err(|e| anyhow!("failed to create hardware encoder: {e:?}"))?;
                Ok(HwRamEncoder {
                    encoder,
                    format: DataFormat::H264,
                    bitrate,
                    config,
                })
            }
            #[cfg(not(any(windows, target_os = "macos")))]
            EncoderCfg::HWRAM(_) => bail!("hardware encoder not supported on this platform"),
            _ => Err(anyhow!("encoder type mismatch")),
        }
    }

    fn encode_to_message(&mut self, input: EncodeInput, ms: i64) -> ResultType<VideoFrame> {
        let mut vf = VideoFrame::new();
        let mut frames = Vec::new();
        for frame in self.encode(input.yuv()?, ms)? {
            frames.push(EncodedVideoFrame {
                data: Bytes::from(frame.data),
                pts: frame.pts,
                key: frame.key == 1,
                ..Default::default()
            });
        }
        if frames.len() > 0 {
            let frames = EncodedVideoFrames {
                frames: frames.into(),
                ..Default::default()
            };
            match self.format {
                DataFormat::H264 => vf.set_h264s(frames),
                DataFormat::H265 => vf.set_h265s(frames),
                _ => bail!("unsupported format: {:?}", self.format),
            }
            Ok(vf)
        } else {
            Err(anyhow!("no valid frame"))
        }
    }

    fn yuvfmt(&self) -> crate::EncodeYuvFormat {
        let w = self.config.width;
        let h = self.config.height;
        crate::EncodeYuvFormat {
            pixfmt: Pixfmt::NV12,
            w,
            h,
            stride: vec![w, w],
            u: w * h,
            v: 0,
        }
    }

    #[cfg(feature = "vram")]
    fn input_texture(&self) -> bool {
        false
    }

    fn set_quality(&mut self, ratio: f32) -> ResultType<()> {
        self.config.quality = ratio;
        Ok(())
    }

    fn bitrate(&self) -> u32 {
        self.bitrate
    }

    fn support_changing_quality(&self) -> bool {
        false
    }

    fn latency_free(&self) -> bool {
        true
    }

    fn is_hardware(&self) -> bool {
        true
    }

    fn disable(&self) {
        HwCodecConfig::clear(false, true);
    }
}

impl HwRamEncoder {
    pub fn try_get(format: CodecFormat) -> Option<CodecInfo> {
        if !enable_hwcodec_option() {
            return None;
        }
        let best = CodecInfo::prioritized(HwCodecConfig::get().ram_encode);
        match format {
            CodecFormat::H264 => best.h264,
            CodecFormat::H265 => best.h265,
            _ => None,
        }
    }

    #[cfg(any(windows, target_os = "macos"))]
    pub fn encode(&mut self, yuv: &[u8], ms: i64) -> ResultType<Vec<EncodeFrame>> {
        let frames = self
            .encoder
            .encode_nv12(yuv, ms)
            .map_err(|e| anyhow!("hardware encode failed: {e:?}"))?;
        Ok(frames
            .into_iter()
            .map(|f| EncodeFrame {
                data: f.data,
                pts: f.pts_ms,
                key: if f.key { 1 } else { 0 },
            })
            .collect())
    }

    #[cfg(not(any(windows, target_os = "macos")))]
    pub fn encode(&mut self, _yuv: &[u8], _ms: i64) -> ResultType<Vec<EncodeFrame>> {
        bail!("hardware encoder not supported on this platform")
    }

    pub fn bitrate(name: &str, width: usize, height: usize, ratio: f32) -> u32 {
        Self::calc_bitrate(width, height, ratio, name.contains("h264"))
    }

    pub fn calc_bitrate(width: usize, height: usize, ratio: f32, h264: bool) -> u32 {
        let base = base_bitrate(width as _, height as _) as f32 * ratio;
        let threshold = 2000.0;
        let decay_rate = 0.001;
        let factor: f32 = if h264 {
            if base > threshold {
                1.0 + 1.0 / (1.0 + (base - threshold) * decay_rate)
            } else {
                2.0
            }
        } else {
            if base > threshold {
                1.0 + 0.5 / (1.0 + (base - threshold) * decay_rate)
            } else {
                1.5
            }
        };
        (base * factor) as u32
    }

    pub fn check_bitrate_range(_config: &HwRamEncoderConfig, bitrate: u32) -> u32 {
        bitrate
    }
}

pub struct HwRamDecoder {
    #[cfg(any(windows, target_os = "macos"))]
    decoder: NatDecoder,
    pub info: CodecInfo,
}

impl HwRamDecoder {
    pub fn try_get(format: CodecFormat) -> Option<CodecInfo> {
        let soft = CodecInfo::soft();
        match format {
            CodecFormat::H264 => soft.h264,
            CodecFormat::H265 => soft.h265,
            _ => None,
        }
    }

    #[cfg(any(windows, target_os = "macos"))]
    pub fn new(format: CodecFormat) -> ResultType<Self> {
        let info = HwRamDecoder::try_get(format);
        log::info!("try create {info:?} hardware decoder");
        let Some(info) = info else {
            bail!("unsupported format: {:?}", format);
        };
        match NatDecoder::new() {
            Ok(decoder) => Ok(HwRamDecoder { decoder, info }),
            Err(e) => {
                HwCodecConfig::clear(false, false);
                Err(anyhow!("failed to create hardware decoder: {e:?}"))
            }
        }
    }

    #[cfg(not(any(windows, target_os = "macos")))]
    pub fn new(format: CodecFormat) -> ResultType<Self> {
        bail!("hardware decoder not supported on this platform: {:?}", format)
    }

    #[cfg(any(windows, target_os = "macos"))]
    pub fn decode(&mut self, data: &[u8]) -> ResultType<Vec<HwRamDecoderImage>> {
        let frames = self
            .decoder
            .decode(data, 0)
            .map_err(|e| anyhow!("hardware decode failed: {e:?}"))?;
        Ok(frames
            .into_iter()
            .map(|frame| HwRamDecoderImage { frame })
            .collect())
    }

    #[cfg(not(any(windows, target_os = "macos")))]
    pub fn decode(&mut self, _data: &[u8]) -> ResultType<Vec<HwRamDecoderImage>> {
        bail!("hardware decoder not supported on this platform")
    }
}

#[cfg(any(windows, target_os = "macos"))]
pub struct HwRamDecoderImage {
    frame: NatDecodedFrame,
}

#[cfg(not(any(windows, target_os = "macos")))]
pub struct HwRamDecoderImage {}

#[cfg(any(windows, target_os = "macos"))]
impl HwRamDecoderImage {
    pub fn to_fmt(&self, rgb: &mut crate::ImageRgb, i420: &mut Vec<u8>) -> ResultType<()> {
        let frame = &self.frame;
        let width = frame.width;
        let height = frame.height;
        let stride_y = frame.stride_y as usize;
        let expected = stride_y * height as usize * 3 / 2;
        if frame.nv12.len() < expected {
            bail!(
                "nv12 buffer too small: {} < {expected} ({}x{} stride {stride_y})",
                frame.nv12.len(),
                width,
                height
            );
        }
        rgb.w = width as _;
        rgb.h = height as _;
        let dst_align = rgb.align();
        let bytes_per_row = (rgb.w * 4 + dst_align - 1) & !(dst_align - 1);
        rgb.raw.resize(rgb.h * bytes_per_row, 0);
        let y_ptr = frame.nv12.as_ptr();
        let uv_ptr = unsafe { y_ptr.add(stride_y * height as usize) };
        let stride_i420_y = width as usize;
        let stride_i420_c = (width as usize + 1) / 2;
        let i420_len =
            stride_i420_y * height as usize + 2 * stride_i420_c * ((height as usize + 1) / 2);
        i420.resize(i420_len, 0);
        let i420_offset_y = i420.as_mut_ptr();
        let i420_offset_u = unsafe { i420_offset_y.add(stride_i420_y * height as usize) };
        let i420_offset_v =
            unsafe { i420_offset_u.add(stride_i420_c * ((height as usize + 1) / 2)) };
        call_yuv!(NV12ToI420(
            y_ptr,
            stride_y as _,
            uv_ptr,
            stride_y as _,
            i420_offset_y,
            stride_i420_y as _,
            i420_offset_u,
            stride_i420_c as _,
            i420_offset_v,
            stride_i420_c as _,
            width,
            height,
        ));
        let f = match rgb.fmt() {
            ImageFormat::ARGB => I420ToARGB,
            ImageFormat::ABGR => I420ToABGR,
            _ => bail!("unsupported image format: {:?}", rgb.fmt()),
        };
        call_yuv!(f(
            i420_offset_y,
            stride_i420_y as _,
            i420_offset_u,
            stride_i420_c as _,
            i420_offset_v,
            stride_i420_c as _,
            rgb.raw.as_mut_ptr(),
            bytes_per_row as _,
            width,
            height,
        ));
        Ok(())
    }
}

#[cfg(not(any(windows, target_os = "macos")))]
impl HwRamDecoderImage {
    pub fn to_fmt(&self, _rgb: &mut crate::ImageRgb, _i420: &mut Vec<u8>) -> ResultType<()> {
        bail!("unsupported platform")
    }
}

#[derive(Debug, Default, Serialize, Deserialize, Clone)]
pub struct HwCodecConfig {
    #[serde(default)]
    pub signature: u64,
    #[serde(default)]
    pub ram_encode: Vec<CodecInfo>,
    #[serde(default)]
    pub ram_decode: Vec<CodecInfo>,
}

#[derive(Debug, Default, Serialize, Deserialize, Clone)]
struct HwCodecConfig2 {
    #[serde(default)]
    pub config: String,
}

impl HwCodecConfig {
    pub fn set(config: String) {
        let config = serde_json::from_str(&config).unwrap_or_default();
        log::info!("set hwcodec config");
        log::debug!("{config:?}");
        #[cfg(any(windows, target_os = "macos"))]
        hbb_common::config::common_store(
            &HwCodecConfig2 {
                config: serde_json::to_string_pretty(&config).unwrap_or_default(),
            },
            "_hwcodec",
        );
        *CONFIG.lock().unwrap() = Some(config);
        *CONFIG_SET_BY_IPC.lock().unwrap() = true;
    }

    pub fn get() -> HwCodecConfig {
        #[cfg(any(windows, target_os = "macos"))]
        {
            let config = CONFIG.lock().unwrap().clone();
            match config {
                Some(c) => c,
                None => {
                    log::info!("try load cached hwcodec config");
                    let c = hbb_common::config::common_load::<HwCodecConfig2>("_hwcodec");
                    let c: HwCodecConfig = serde_json::from_str(&c.config).unwrap_or_default();
                    let new_signature = nat_gpu_signature();
                    if c.signature == new_signature {
                        log::debug!("load cached hwcodec config: {c:?}");
                        *CONFIG.lock().unwrap() = Some(c.clone());
                        c
                    } else {
                        log::info!(
                            "gpu signature changed, {} -> {}",
                            c.signature,
                            new_signature
                        );
                        HwCodecConfig::default()
                    }
                }
            }
        }
        #[cfg(not(any(windows, target_os = "macos")))]
        {
            CONFIG.lock().unwrap().clone().unwrap_or_default()
        }
    }

    pub fn get_set_value() -> Option<HwCodecConfig> {
        let set = CONFIG_SET_BY_IPC.lock().unwrap().clone();
        if set {
            CONFIG.lock().unwrap().clone()
        } else {
            None
        }
    }

    pub fn already_set() -> bool {
        CONFIG_SET_BY_IPC.lock().unwrap().clone()
    }

    #[cfg(any(windows, target_os = "macos"))]
    pub fn has_valid_cache() -> bool {
        if CONFIG.lock().unwrap().is_some() {
            return true;
        }
        let stored = hbb_common::config::common_load::<HwCodecConfig2>("_hwcodec");
        if stored.config.is_empty() {
            return false;
        }
        match serde_json::from_str::<HwCodecConfig>(&stored.config) {
            Ok(c) => c.signature == nat_gpu_signature(),
            Err(_) => false,
        }
    }

    #[cfg(not(any(windows, target_os = "macos")))]
    pub fn has_valid_cache() -> bool {
        false
    }

    pub fn clear(_vram: bool, encode: bool) {
        log::info!("clear hwcodec config, encode: {encode}");
        {
            let mut c = CONFIG.lock().unwrap();
            if let Some(c) = c.as_mut() {
                if encode {
                    c.ram_encode = vec![];
                } else {
                    c.ram_decode = vec![];
                }
            }
        }
        crate::codec::Encoder::update(crate::codec::EncodingUpdate::Check);
    }
}

pub fn check_available_hwcodec() -> String {
    #[cfg(any(windows, target_os = "macos"))]
    {
        #[cfg(windows)]
        let prefix = "h264_mf";
        #[cfg(target_os = "macos")]
        let prefix = "h264_vt";
        let (encoders, decoder_ok) = nat_probe_hardware_h264();
        let ram_encode = encoders
            .into_iter()
            .enumerate()
            .map(|(i, name)| CodecInfo {
                name: format!("{prefix}:{name}"),
                mc_name: None,
                format: DataFormat::H264,
                priority: i as i32,
            })
            .collect();
        let ram_decode = if decoder_ok {
            CodecInfo::soft().h264.into_iter().collect()
        } else {
            vec![]
        };
        let c = HwCodecConfig {
            ram_encode,
            ram_decode,
            signature: nat_gpu_signature(),
        };
        log::debug!("{c:?}");
        serde_json::to_string(&c).unwrap_or_default()
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        serde_json::to_string(&HwCodecConfig::default()).unwrap_or_default()
    }
}

#[cfg(windows)]
fn kill_child_when_parent_exits(child: &std::process::Child) {
    use std::os::windows::io::AsRawHandle;
    use winapi::um::jobapi2::{AssignProcessToJobObject, CreateJobObjectW, SetInformationJobObject};
    use winapi::um::winnt::{
        JobObjectExtendedLimitInformation, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };
    unsafe {
        let job = CreateJobObjectW(std::ptr::null_mut(), std::ptr::null());
        if job.is_null() {
            return;
        }
        let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        SetInformationJobObject(
            job,
            JobObjectExtendedLimitInformation,
            &mut info as *mut _ as *mut _,
            std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        );
        AssignProcessToJobObject(job, child.as_raw_handle() as _);
    }
}

pub fn start_check_process() {
    if !enable_hwcodec_option() || HwCodecConfig::already_set() || HwCodecConfig::has_valid_cache() {
        return;
    }
    use hbb_common::allow_err;
    use std::sync::Once;
    let f = || {
        if let Ok(exe) = std::env::current_exe() {
            if let Some(_) = exe.file_name().to_owned() {
                let arg = "--check-hwcodec-config";
                if let Ok(mut child) = std::process::Command::new(exe).arg(arg).spawn() {
                    #[cfg(windows)]
                    kill_child_when_parent_exits(&child);
                    for _ in 0..30 {
                        std::thread::sleep(std::time::Duration::from_secs(1));
                        if let Ok(Some(_)) = child.try_wait() {
                            break;
                        }
                    }
                    allow_err!(child.kill());
                    std::thread::sleep(std::time::Duration::from_millis(30));
                    if let Ok(None) = child.try_wait() {
                        let _ = child.wait();
                    }
                }
            }
        };
    };
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        std::thread::spawn(f);
    });
}
