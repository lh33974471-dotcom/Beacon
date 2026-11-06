#![cfg(windows)]

use hbb_common::{
    anyhow::{anyhow, bail},
    log, ResultType,
};
use std::mem::ManuallyDrop;
use std::sync::{Once, OnceLock};
use windows::core::{Interface, GUID};
use windows::Win32::Media::MediaFoundation::{
    IMFActivate, IMFAttributes, IMFMediaEvent, IMFMediaEventGenerator, IMFMediaType, IMFSample,
    IMFTransform, MFCreateMediaType, MFCreateMemoryBuffer, MFCreateSample, MFStartup,
    MFTEnumEx, MFSTARTUP_FULL, MFT_CATEGORY_VIDEO_DECODER, MFT_CATEGORY_VIDEO_ENCODER,
    MFT_ENUM_FLAG_HARDWARE, MFT_ENUM_FLAG_SORTANDFILTER, MFT_ENUM_FLAG_SYNCMFT,
    MFT_FRIENDLY_NAME_Attribute, MFT_MESSAGE_COMMAND_FLUSH, MFT_MESSAGE_NOTIFY_BEGIN_STREAMING,
    MFT_MESSAGE_NOTIFY_END_OF_STREAM, MFT_MESSAGE_NOTIFY_START_OF_STREAM,
    MFT_OUTPUT_DATA_BUFFER, MFT_OUTPUT_STREAM_PROVIDES_SAMPLES, MFT_REGISTER_TYPE_INFO,
    MFMediaType_Video, MFVideoFormat_H264, MFVideoFormat_NV12,
    MF_E_TRANSFORM_NEED_MORE_INPUT, MF_E_TRANSFORM_STREAM_CHANGE, MF_LOW_LATENCY,
    MF_MT_AVG_BITRATE, MF_MT_DEFAULT_STRIDE, MF_MT_FRAME_RATE, MF_MT_FRAME_SIZE,
    MF_MT_INTERLACE_MODE, MF_MT_MAJOR_TYPE, MF_MT_MPEG2_PROFILE, MF_MT_PIXEL_ASPECT_RATIO,
    MF_MT_SUBTYPE, MF_TRANSFORM_ASYNC_UNLOCK, MFSampleExtension_CleanPoint,
    MFVideoInterlace_Progressive, eAVEncH264VProfile_Main,
};
use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, IDXGIDevice, IDXGIFactory1};
use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};

const ME_TRANSFORM_NEED_INPUT: i32 = 601;
const ME_TRANSFORM_HAVE_OUTPUT: i32 = 602;
const ME_ERROR: i32 = 1;
const SAMPLE_TIME_UNITS_PER_MS: i64 = 10_000;
const DEFAULT_FRAME_DURATION_100NS: i64 = 333_333;
const EVENT_DRAIN_BUDGET: usize = 16;

static MF_INIT: Once = Once::new();

fn media_foundation_installed() -> bool {
    static INSTALLED: OnceLock<bool> = OnceLock::new();
    *INSTALLED.get_or_init(|| {
        let system_root =
            std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".to_string());
        let present = std::path::Path::new(&system_root)
            .join("System32")
            .join("mfplat.dll")
            .exists();
        if !present {
            log::info!("mfplat.dll is absent, hardware H264 is unavailable");
        }
        present
    })
}

fn ensure_mf() -> bool {
    if !media_foundation_installed() {
        return false;
    }
    MF_INIT.call_once(|| unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        if let Err(e) = MFStartup(windows::Win32::Media::MediaFoundation::MF_VERSION, MFSTARTUP_FULL) {
            log::error!("MFStartup failed: {e:?}");
        }
    });
    true
}

fn com_init_this_thread() {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    }
}

fn frame_size_u64(width: u32, height: u32) -> u64 {
    ((width as u64) << 32) | height as u64
}

fn ratio_u64(num: u32, den: u32) -> u64 {
    ((num as u64) << 32) | den as u64
}

fn friendly_name(activate: &IMFActivate) -> String {
    unsafe {
        let mut ptr = windows::core::PWSTR::null();
        let mut len = 0u32;
        if activate
            .GetAllocatedString(&MFT_FRIENDLY_NAME_Attribute, &mut ptr, &mut len)
            .is_ok()
            && !ptr.is_null()
        {
            let s = ptr.to_string().unwrap_or_default();
            windows::Win32::System::Com::CoTaskMemFree(Some(ptr.as_ptr() as _));
            s
        } else {
            String::new()
        }
    }
}

fn enum_transforms(
    category: GUID,
    flags: i32,
    input: Option<&MFT_REGISTER_TYPE_INFO>,
    output: Option<&MFT_REGISTER_TYPE_INFO>,
) -> Vec<(String, IMFActivate)> {
    if !ensure_mf() {
        return Vec::new();
    }
    let mut result = Vec::new();
    unsafe {
        let mut activates: *mut Option<IMFActivate> = std::ptr::null_mut();
        let mut count = 0u32;
        let hr = MFTEnumEx(
            category,
            windows::Win32::Media::MediaFoundation::MFT_ENUM_FLAG(flags),
            input.map(|r| r as *const _),
            output.map(|r| r as *const _),
            &mut activates,
            &mut count,
        );
        if hr.is_err() || activates.is_null() {
            return result;
        }
        for i in 0..count as usize {
            if let Some(activate) = (*activates.add(i)).clone() {
                let name = friendly_name(&activate);
                result.push((name, activate));
            }
            std::ptr::drop_in_place(activates.add(i));
        }
        windows::Win32::System::Com::CoTaskMemFree(Some(activates as _));
    }
    result
}

fn h264_type_info() -> MFT_REGISTER_TYPE_INFO {
    MFT_REGISTER_TYPE_INFO {
        guidMajorType: MFMediaType_Video,
        guidSubtype: MFVideoFormat_H264,
    }
}

fn nv12_type_info() -> MFT_REGISTER_TYPE_INFO {
    MFT_REGISTER_TYPE_INFO {
        guidMajorType: MFMediaType_Video,
        guidSubtype: MFVideoFormat_NV12,
    }
}

pub fn list_hardware_h264_encoders() -> Vec<String> {
    enum_transforms(
        MFT_CATEGORY_VIDEO_ENCODER,
        MFT_ENUM_FLAG_HARDWARE.0 | MFT_ENUM_FLAG_SORTANDFILTER.0,
        Some(&nv12_type_info()),
        Some(&h264_type_info()),
    )
    .into_iter()
    .map(|(name, _)| name)
    .fold(Vec::new(), |mut acc, name| {
        if !acc.contains(&name) {
            acc.push(name);
        }
        acc
    })
}

pub struct MfEncodedFrame {
    pub data: Vec<u8>,
    pub pts_ms: i64,
    pub key: bool,
}

pub struct MfEncoder {
    transform: IMFTransform,
    events: Option<IMFMediaEventGenerator>,
    provides_samples: bool,
    need_input_credits: usize,
    width: u32,
    height: u32,
    pub bitrate_kbs: u32,
    pub name: String,
}

impl MfEncoder {
    pub fn new(
        selector: &str,
        width: usize,
        height: usize,
        bitrate_kbs: u32,
        fps: u32,
    ) -> ResultType<Self> {
        com_init_this_thread();
        if !ensure_mf() {
            bail!("Media Foundation is not installed");
        }
        let candidates = enum_transforms(
            MFT_CATEGORY_VIDEO_ENCODER,
            MFT_ENUM_FLAG_HARDWARE.0 | MFT_ENUM_FLAG_SORTANDFILTER.0,
            Some(&nv12_type_info()),
            Some(&h264_type_info()),
        );
        if candidates.is_empty() {
            bail!("no hardware H264 encoder MFT found");
        }
        let (name, activate) = candidates
            .iter()
            .find(|(n, _)| selector.is_empty() || n == selector || selector.ends_with(n.as_str()))
            .unwrap_or(&candidates[0])
            .clone();
        let transform: IMFTransform = unsafe { activate.ActivateObject()? };

        let mut events = None;
        if let Ok(attrs) = unsafe { transform.GetAttributes() } {
            let is_async = attr_u32(&attrs, &windows::Win32::Media::MediaFoundation::MF_TRANSFORM_ASYNC) == 1;
            if is_async {
                unsafe { attrs.SetUINT32(&MF_TRANSFORM_ASYNC_UNLOCK, 1)? };
                events = transform.cast::<IMFMediaEventGenerator>().ok();
            }
            unsafe {
                let _ = attrs.SetUINT32(&MF_LOW_LATENCY, 1);
            }
        }

        let (w, h) = (width as u32, height as u32);
        unsafe {
            let out_type: IMFMediaType = MFCreateMediaType()?;
            out_type.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
            out_type.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_H264)?;
            out_type.SetUINT32(&MF_MT_AVG_BITRATE, bitrate_kbs.saturating_mul(1000))?;
            out_type.SetUINT64(&MF_MT_FRAME_SIZE, frame_size_u64(w, h))?;
            out_type.SetUINT64(&MF_MT_FRAME_RATE, ratio_u64(fps, 1))?;
            out_type.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, ratio_u64(1, 1))?;
            out_type.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
            out_type.SetUINT32(&MF_MT_MPEG2_PROFILE, eAVEncH264VProfile_Main.0 as u32)?;
            transform.SetOutputType(0, &out_type, 0)?;

            let in_type: IMFMediaType = MFCreateMediaType()?;
            in_type.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
            in_type.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_NV12)?;
            in_type.SetUINT64(&MF_MT_FRAME_SIZE, frame_size_u64(w, h))?;
            in_type.SetUINT64(&MF_MT_FRAME_RATE, ratio_u64(fps, 1))?;
            in_type.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
            in_type.SetUINT32(&MF_MT_DEFAULT_STRIDE, w)?;
            transform.SetInputType(0, &in_type, 0)?;
        }

        let provides_samples = unsafe {
            transform
                .GetOutputStreamInfo(0)
                .map(|info| info.dwFlags & MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 as u32 != 0)
                .unwrap_or(false)
        };

        unsafe {
            transform.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)?;
            transform.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)?;
        }

        Ok(MfEncoder {
            transform,
            events,
            provides_samples,
            need_input_credits: 0,
            width: w,
            height: h,
            bitrate_kbs,
            name,
        })
    }

    pub fn encode_nv12(&mut self, nv12: &[u8], ms: i64) -> ResultType<Vec<MfEncodedFrame>> {
        let expected = (self.width as usize * self.height as usize * 3) / 2;
        if nv12.len() < expected {
            bail!("nv12 buffer too small: {} < {expected}", nv12.len());
        }
        let sample = self.make_input_sample(&nv12[..expected], ms)?;
        let mut outputs = Vec::new();
        if self.events.is_some() {
            self.wait_need_input(&mut outputs)?;
            unsafe { self.transform.ProcessInput(0, &sample, 0)? };
            self.need_input_credits = self.need_input_credits.saturating_sub(1);
            self.pump_events(&mut outputs)?;
        } else {
            unsafe { self.transform.ProcessInput(0, &sample, 0)? };
            self.drain_sync(&mut outputs)?;
        }
        Ok(outputs)
    }

    fn wait_need_input(&mut self, outputs: &mut Vec<MfEncodedFrame>) -> ResultType<()> {
        let events = self.events.clone().ok_or_else(|| anyhow!("no event generator"))?;
        let mut budget = EVENT_DRAIN_BUDGET;
        while self.need_input_credits == 0 && budget > 0 {
            budget -= 1;
            let event: IMFMediaEvent = unsafe { events.GetEvent(windows::Win32::Media::MediaFoundation::MEDIA_EVENT_GENERATOR_GET_EVENT_FLAGS(0))? };
            self.handle_event(&event, outputs)?;
        }
        if self.need_input_credits == 0 {
            bail!("encoder never signalled NeedInput");
        }
        Ok(())
    }

    fn pump_events(&mut self, outputs: &mut Vec<MfEncodedFrame>) -> ResultType<()> {
        let events = match self.events.clone() {
            Some(e) => e,
            None => return Ok(()),
        };
        let mut budget = EVENT_DRAIN_BUDGET;
        while budget > 0 {
            budget -= 1;
            match unsafe {
                events.GetEvent(windows::Win32::Media::MediaFoundation::MEDIA_EVENT_GENERATOR_GET_EVENT_FLAGS(1))
            } {
                Ok(event) => self.handle_event(&event, outputs)?,
                Err(_) => break,
            }
            if self.need_input_credits > 0 && !outputs.is_empty() {
                break;
            }
        }
        Ok(())
    }

    fn handle_event(
        &mut self,
        event: &IMFMediaEvent,
        outputs: &mut Vec<MfEncodedFrame>,
    ) -> ResultType<()> {
        let event_type = unsafe { event.GetType()? } as i32;
        match event_type {
            ME_TRANSFORM_NEED_INPUT => {
                self.need_input_credits += 1;
            }
            ME_TRANSFORM_HAVE_OUTPUT => {
                if let Some(frame) = self.process_output()? {
                    outputs.push(frame);
                }
            }
            ME_ERROR => {
                bail!("encoder MFT reported MEError");
            }
            _ => {}
        }
        let _ = event_type;
        Ok(())
    }

    fn drain_sync(&mut self, outputs: &mut Vec<MfEncodedFrame>) -> ResultType<()> {
        loop {
            match self.process_output() {
                Ok(Some(frame)) => outputs.push(frame),
                Ok(None) => break,
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }

    fn make_input_sample(&self, nv12: &[u8], ms: i64) -> ResultType<IMFSample> {
        unsafe {
            let buffer = MFCreateMemoryBuffer(nv12.len() as u32)?;
            let mut ptr = std::ptr::null_mut();
            buffer.Lock(&mut ptr, None, None)?;
            std::ptr::copy_nonoverlapping(nv12.as_ptr(), ptr, nv12.len());
            buffer.Unlock()?;
            buffer.SetCurrentLength(nv12.len() as u32)?;
            let sample = MFCreateSample()?;
            sample.AddBuffer(&buffer)?;
            sample.SetSampleTime(ms * SAMPLE_TIME_UNITS_PER_MS)?;
            sample.SetSampleDuration(DEFAULT_FRAME_DURATION_100NS)?;
            Ok(sample)
        }
    }

    fn process_output(&mut self) -> ResultType<Option<MfEncodedFrame>> {
        unsafe {
            let provided_sample = if self.provides_samples {
                None
            } else {
                let info = self.transform.GetOutputStreamInfo(0)?;
                let size = info.cbSize.max(1024 * 1024);
                let buffer = MFCreateMemoryBuffer(size)?;
                let sample = MFCreateSample()?;
                sample.AddBuffer(&buffer)?;
                Some(sample)
            };
            let mut out = MFT_OUTPUT_DATA_BUFFER {
                dwStreamID: 0,
                pSample: ManuallyDrop::new(provided_sample),
                dwStatus: 0,
                pEvents: ManuallyDrop::new(None),
            };
            let mut status = 0u32;
            let hr = self
                .transform
                .ProcessOutput(0, std::slice::from_mut(&mut out), &mut status);
            let sample = ManuallyDrop::take(&mut out.pSample);
            let _ = ManuallyDrop::take(&mut out.pEvents);
            match hr {
                Ok(()) => {
                    let Some(sample) = sample else {
                        return Ok(None);
                    };
                    let frame = self.copy_sample(&sample)?;
                    Ok(Some(frame))
                }
                Err(e) if e.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => Ok(None),
                Err(e) if e.code() == MF_E_TRANSFORM_STREAM_CHANGE => Ok(None),
                Err(e) => Err(anyhow!("ProcessOutput failed: {e:?}")),
            }
        }
    }

    fn copy_sample(&self, sample: &IMFSample) -> ResultType<MfEncodedFrame> {
        unsafe {
            let buffer = sample.ConvertToContiguousBuffer()?;
            let mut ptr = std::ptr::null_mut();
            let mut len = 0u32;
            buffer.Lock(&mut ptr, None, Some(&mut len))?;
            let data = std::slice::from_raw_parts(ptr, len as usize).to_vec();
            buffer.Unlock()?;
            let pts_ms = sample.GetSampleTime().unwrap_or(0) / SAMPLE_TIME_UNITS_PER_MS;
            let key = sample_u32(sample, &MFSampleExtension_CleanPoint) == 1;
            Ok(MfEncodedFrame { data, pts_ms, key })
        }
    }
}

impl Drop for MfEncoder {
    fn drop(&mut self) {
        unsafe {
            let _ = self
                .transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_END_OF_STREAM, 0);
            let _ = self.transform.ProcessMessage(MFT_MESSAGE_COMMAND_FLUSH, 0);
        }
    }
}

fn attr_u32(attrs: &IMFAttributes, key: &GUID) -> u32 {
    unsafe { attrs.GetUINT32(key).unwrap_or(0) }
}

fn sample_u32(sample: &IMFSample, key: &GUID) -> u32 {
    unsafe { sample.GetUINT32(key).unwrap_or(0) }
}

pub struct MfDecodedFrame {
    pub nv12: Vec<u8>,
    pub stride_y: i32,
    pub width: i32,
    pub height: i32,
}

pub struct MfDecoder {
    transform: IMFTransform,
    width: u32,
    height: u32,
    stride: u32,
}

impl MfDecoder {
    pub fn new() -> ResultType<Self> {
        com_init_this_thread();
        if !ensure_mf() {
            bail!("Media Foundation is not installed");
        }
        let candidates = enum_transforms(
            MFT_CATEGORY_VIDEO_DECODER,
            MFT_ENUM_FLAG_SYNCMFT.0 | MFT_ENUM_FLAG_SORTANDFILTER.0,
            Some(&h264_type_info()),
            Some(&nv12_type_info()),
        );
        let (_, activate) = candidates
            .into_iter()
            .next()
            .ok_or_else(|| anyhow!("no H264 decoder MFT found"))?;
        let transform: IMFTransform = unsafe { activate.ActivateObject()? };
        if let Ok(attrs) = unsafe { transform.GetAttributes() } {
            unsafe {
                let _ = attrs.SetUINT32(&MF_LOW_LATENCY, 1);
            }
        }
        unsafe {
            let in_type: IMFMediaType = MFCreateMediaType()?;
            in_type.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
            in_type.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_H264)?;
            transform.SetInputType(0, &in_type, 0)?;
            Self::negotiate_output(&transform)?;
            transform.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)?;
            transform.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)?;
        }
        let mut decoder = MfDecoder {
            transform,
            width: 0,
            height: 0,
            stride: 0,
        };
        decoder.refresh_output_geometry();
        Ok(decoder)
    }

    fn negotiate_output(transform: &IMFTransform) -> ResultType<()> {
        unsafe {
            let mut index = 0u32;
            loop {
                let t = match transform.GetOutputAvailableType(0, index) {
                    Ok(t) => t,
                    Err(_) => bail!("decoder offers no NV12 output type"),
                };
                if t.GetGUID(&MF_MT_SUBTYPE).map(|g| g == MFVideoFormat_NV12) == Ok(true) {
                    transform.SetOutputType(0, &t, 0)?;
                    return Ok(());
                }
                index += 1;
            }
        }
    }

    fn refresh_output_geometry(&mut self) {
        unsafe {
            if let Ok(t) = self.transform.GetOutputCurrentType(0) {
                if let Ok(size) = t.GetUINT64(&MF_MT_FRAME_SIZE) {
                    self.width = (size >> 32) as u32;
                    self.height = (size & 0xffff_ffff) as u32;
                }
                self.stride = t.GetUINT32(&MF_MT_DEFAULT_STRIDE).unwrap_or(self.width);
                if self.stride == 0 {
                    self.stride = self.width;
                }
            }
        }
    }

    pub fn decode(&mut self, h264: &[u8], ms: i64) -> ResultType<Vec<MfDecodedFrame>> {
        unsafe {
            let buffer = MFCreateMemoryBuffer(h264.len() as u32)?;
            let mut ptr = std::ptr::null_mut();
            buffer.Lock(&mut ptr, None, None)?;
            std::ptr::copy_nonoverlapping(h264.as_ptr(), ptr, h264.len());
            buffer.Unlock()?;
            buffer.SetCurrentLength(h264.len() as u32)?;
            let sample = MFCreateSample()?;
            sample.AddBuffer(&buffer)?;
            sample.SetSampleTime(ms * SAMPLE_TIME_UNITS_PER_MS)?;
            self.transform.ProcessInput(0, &sample, 0)?;
        }
        let mut frames = Vec::new();
        loop {
            match self.process_output()? {
                Some(frame) => frames.push(frame),
                None => break,
            }
        }
        Ok(frames)
    }

    fn process_output(&mut self) -> ResultType<Option<MfDecodedFrame>> {
        unsafe {
            let info = self.transform.GetOutputStreamInfo(0)?;
            let provides = info.dwFlags & MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 as u32 != 0;
            let provided_sample = if provides {
                None
            } else {
                let min_size = ((self.stride.max(self.width) * self.height * 3) / 2).max(info.cbSize);
                let buffer = MFCreateMemoryBuffer(min_size)?;
                let sample = MFCreateSample()?;
                sample.AddBuffer(&buffer)?;
                Some(sample)
            };
            let mut out = MFT_OUTPUT_DATA_BUFFER {
                dwStreamID: 0,
                pSample: ManuallyDrop::new(provided_sample),
                dwStatus: 0,
                pEvents: ManuallyDrop::new(None),
            };
            let mut status = 0u32;
            let hr = self
                .transform
                .ProcessOutput(0, std::slice::from_mut(&mut out), &mut status);
            let sample = ManuallyDrop::take(&mut out.pSample);
            let _ = ManuallyDrop::take(&mut out.pEvents);
            match hr {
                Ok(()) => {
                    let Some(sample) = sample else {
                        return Ok(None);
                    };
                    let buffer = sample.ConvertToContiguousBuffer()?;
                    let mut ptr = std::ptr::null_mut();
                    let mut len = 0u32;
                    buffer.Lock(&mut ptr, None, Some(&mut len))?;
                    let nv12 = std::slice::from_raw_parts(ptr, len as usize).to_vec();
                    buffer.Unlock()?;
                    Ok(Some(MfDecodedFrame {
                        nv12,
                        stride_y: self.stride as i32,
                        width: self.width as i32,
                        height: self.height as i32,
                    }))
                }
                Err(e) if e.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => Ok(None),
                Err(e) if e.code() == MF_E_TRANSFORM_STREAM_CHANGE => {
                    Self::negotiate_output(&self.transform)?;
                    self.refresh_output_geometry();
                    self.process_output()
                }
                Err(e) => Err(anyhow!("decoder ProcessOutput failed: {e:?}")),
            }
        }
    }
}

impl Drop for MfDecoder {
    fn drop(&mut self) {
        unsafe {
            let _ = self
                .transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_END_OF_STREAM, 0);
            let _ = self.transform.ProcessMessage(MFT_MESSAGE_COMMAND_FLUSH, 0);
        }
    }
}

pub fn probe_hardware_h264() -> (Vec<String>, bool) {
    com_init_this_thread();
    let encoders = list_hardware_h264_encoders();
    let mut working = Vec::new();
    for name in encoders {
        match MfEncoder::new(&name, 1280, 720, 1000, 30) {
            Ok(mut enc) => {
                let nv12 = vec![0u8; 1280 * 720 * 3 / 2];
                let mut produced = false;
                for i in 0..8 {
                    match enc.encode_nv12(&nv12, i * 33) {
                        Ok(frames) if !frames.is_empty() => {
                            produced = true;
                            break;
                        }
                        Ok(_) => {}
                        Err(e) => {
                            log::info!("probe encode failed for {name}: {e:?}");
                            break;
                        }
                    }
                }
                if produced {
                    working.push(name);
                }
            }
            Err(e) => {
                log::info!("probe create failed for {name}: {e:?}");
            }
        }
    }
    let decoder_ok = MfDecoder::new().is_ok();
    (working, decoder_ok)
}

pub fn gpu_signature() -> u64 {
    let mut signature: u64 = 0;
    unsafe {
        let Ok(factory) = CreateDXGIFactory1::<IDXGIFactory1>() else {
            return 0;
        };
        let mut i = 0;
        while let Ok(adapter) = factory.EnumAdapters1(i) {
            i += 1;
            let Ok(desc) = adapter.GetDesc1() else {
                continue;
            };
            let is_gpu_vendor = matches!(desc.VendorId, 0x10de | 0x1002 | 0x8086);
            if !is_gpu_vendor {
                continue;
            }
            signature = signature
                .wrapping_add(desc.VendorId as u64)
                .wrapping_add(desc.DeviceId as u64)
                .wrapping_add(desc.SubSysId as u64)
                .wrapping_add(desc.Revision as u64);
            if let Ok(umd_version) = adapter.CheckInterfaceSupport(&IDXGIDevice::IID) {
                signature = signature.wrapping_add(umd_version as u64);
            }
        }
    }
    signature.wrapping_add(media_foundation_installed() as u64)
}
