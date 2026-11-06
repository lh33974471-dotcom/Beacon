#![cfg(target_os = "macos")]

use cidre::{arc, cf, cm, cv, os, vt};
use hbb_common::{anyhow::bail, log, ResultType};
use std::ffi::c_void;
use std::sync::Mutex;

pub struct VtEncodedFrame {
    pub data: Vec<u8>,
    pub pts_ms: i64,
    pub key: bool,
}

pub struct VtDecodedFrame {
    pub nv12: Vec<u8>,
    pub stride_y: i32,
    pub width: i32,
    pub height: i32,
}

#[link(name = "CoreVideo", kind = "framework")]
extern "C" {
    fn CVPixelBufferGetBaseAddressOfPlane(pb: *const c_void, plane: usize) -> *mut u8;
    fn CVPixelBufferGetBytesPerRowOfPlane(pb: *const c_void, plane: usize) -> usize;
}

struct EncState {
    frames: Mutex<Vec<VtEncodedFrame>>,
}

extern "C" fn enc_output(
    ref_con: *mut EncState,
    _src_ref_con: *mut c_void,
    status: os::Status,
    _flags: vt::EncodeInfoFlags,
    sample: Option<&cm::SampleBuf>,
) {
    if ref_con.is_null() || status.is_err() {
        return;
    }
    let state = unsafe { &*ref_con };
    let Some(sample) = sample else {
        return;
    };
    let Some(block) = sample.data_buf() else {
        return;
    };
    let avcc = match read_block(block) {
        Some(b) => b,
        None => return,
    };
    let key = sample.is_key_frame();
    let mut nal_len = 4usize;
    let mut headers: Vec<u8> = Vec::new();
    if let Some(desc) = sample.format_desc() {
        let vdesc: &cm::VideoFormatDesc = unsafe { std::mem::transmute(desc) };
        if let Ok((count, hdr_len)) = vdesc.h264_params_count_and_header_len() {
            nal_len = hdr_len.max(1) as usize;
            if key {
                for i in 0..count {
                    if let Ok(ps) = vdesc.h264_param_set_at(i) {
                        headers.extend_from_slice(&[0, 0, 0, 1]);
                        headers.extend_from_slice(ps);
                    }
                }
            }
        }
    }
    let mut annexb = headers;
    let mut off = 0usize;
    while off + nal_len <= avcc.len() {
        let mut sz = 0usize;
        for i in 0..nal_len {
            sz = (sz << 8) | avcc[off + i] as usize;
        }
        off += nal_len;
        if sz == 0 || off + sz > avcc.len() {
            break;
        }
        annexb.extend_from_slice(&[0, 0, 0, 1]);
        annexb.extend_from_slice(&avcc[off..off + sz]);
        off += sz;
    }
    if annexb.is_empty() {
        return;
    }
    let pts = sample.pts();
    let pts_ms = if pts.is_valid() && pts.scale != 0 {
        pts.value.saturating_mul(1000) / pts.scale as i64
    } else {
        0
    };
    state.frames.lock().unwrap().push(VtEncodedFrame {
        data: annexb,
        pts_ms,
        key,
    });
}

fn read_block(block: &cm::BlockBuf) -> Option<Vec<u8>> {
    if let Ok(slice) = block.as_slice() {
        return Some(slice.to_vec());
    }
    let len = block.data_len();
    let mut out = Vec::with_capacity(len);
    let mut off = 0usize;
    while off < len {
        let (chunk, _remaining) = block.data_ptr_at(off).ok()?;
        if chunk.is_empty() {
            return None;
        }
        out.extend_from_slice(chunk);
        off += chunk.len();
    }
    Some(out)
}

pub struct VtEncoder {
    session: arc::R<vt::compression::Session>,
    state: Box<EncState>,
    width: usize,
    height: usize,
    fps: u32,
}

unsafe impl Send for VtEncoder {}

impl VtEncoder {
    pub fn new(
        _selector: &str,
        width: usize,
        height: usize,
        bitrate_kbs: u32,
        fps: u32,
    ) -> ResultType<Self> {
        let state = Box::new(EncState {
            frames: Mutex::new(Vec::new()),
        });
        let spec = cf::DictionaryOf::with_keys_values(
            &[cf::String::from_str("EnableHardwareAcceleratedVideoEncoder").as_ref()],
            &[cf::Boolean::value_true().as_type_ref()],
        );
        let mut session = match vt::compression::Session::new::<EncState>(
            width as u32,
            height as u32,
            cm::VideoCodec::H264,
            Some(&spec),
            None,
            None,
            Some(enc_output),
            &*state as *const EncState as *mut EncState,
        ) {
            Ok(s) => s,
            Err(e) => bail!("VTCompressionSessionCreate failed: {e:?}"),
        };
        use vt::compression::properties::{keys, profile_level};
        let set_bool = |s: &mut vt::compression::Session, k: &cf::String, v: bool| {
            let _ = s.set_prop(
                k,
                Some(if v {
                    cf::Boolean::value_true().as_type_ref()
                } else {
                    cf::Boolean::value_false().as_type_ref()
                }),
            );
        };
        set_bool(&mut session, keys::real_time(), true);
        set_bool(&mut session, keys::allow_frame_reordering(), false);
        let _ = session.set_prop(
            keys::profile_lvl(),
            Some(profile_level::h264::main_auto_lvl().as_type_ref()),
        );
        let bits = cf::Number::from_i32(bitrate_kbs.saturating_mul(1000).min(i32::MAX as u32) as i32);
        let _ = session.set_prop(keys::avarage_bit_rate(), Some(bits.as_type_ref()));
        let fps_n = cf::Number::from_i32(fps.max(1) as i32);
        let _ = session.set_prop(keys::expected_frame_rate(), Some(fps_n.as_type_ref()));
        let gop = cf::Number::from_i32(i32::MAX);
        let _ = session.set_prop(keys::max_key_frame_interval(), Some(gop.as_type_ref()));
        if let Err(e) = session.prepare() {
            bail!("VTCompressionSessionPrepareToEncodeFrames failed: {e:?}");
        }
        Ok(VtEncoder {
            session,
            state,
            width,
            height,
            fps,
        })
    }

    pub fn encode_nv12(&mut self, nv12: &[u8], ms: i64) -> ResultType<Vec<VtEncodedFrame>> {
        let (w, h) = (self.width, self.height);
        if nv12.len() < w * h * 3 / 2 {
            bail!("nv12 buffer too small: {} < {}", nv12.len(), w * h * 3 / 2);
        }
        let pooled = self
            .session
            .pixel_buf_pool()
            .and_then(|p| p.pixel_buf().ok())
            .filter(|b| b.plane_count() == 2 && b.width() == w && b.height() == h);
        let pb = match pooled {
            Some(p) => p,
            None => match cv::PixelBuf::new(w, h, cv::PixelFormat::_420V, None) {
                Ok(p) => p,
                Err(e) => bail!("CVPixelBufferCreate failed: {e:?}"),
            },
        };
        {
            let _guard = pb
                .base_address_lock(cv::pixel_buffer::LockFlags::DEFAULT)
                .map_err(|e| hbb_common::anyhow::anyhow!("CVPixelBufferLockBaseAddress failed: {e:?}"))?;
            let pbp = &*pb as *const cv::PixelBuf as *const c_void;
            unsafe {
                let y_dst = CVPixelBufferGetBaseAddressOfPlane(pbp, 0);
                let y_stride = CVPixelBufferGetBytesPerRowOfPlane(pbp, 0);
                let uv_dst = CVPixelBufferGetBaseAddressOfPlane(pbp, 1);
                let uv_stride = CVPixelBufferGetBytesPerRowOfPlane(pbp, 1);
                if y_dst.is_null() || uv_dst.is_null() {
                    bail!("CVPixelBuffer plane base address is null");
                }
                for row in 0..h {
                    std::ptr::copy_nonoverlapping(
                        nv12.as_ptr().add(row * w),
                        y_dst.add(row * y_stride),
                        w,
                    );
                }
                let uv_src = nv12.as_ptr().add(w * h);
                for row in 0..h / 2 {
                    std::ptr::copy_nonoverlapping(
                        uv_src.add(row * w),
                        uv_dst.add(row * uv_stride),
                        w,
                    );
                }
            }
        }
        let pts = cm::Time::new(ms, 1000);
        let duration = cm::Time::new(1000 / self.fps.max(1) as i64, 1000);
        if let Err(e) = self.session.encode_frame(
            &pb,
            pts,
            duration,
            None,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        ) {
            bail!("VTCompressionSessionEncodeFrame failed: {e:?}");
        }
        let mut out = self.state.frames.lock().unwrap();
        Ok(std::mem::take(&mut *out))
    }
}

impl Drop for VtEncoder {
    fn drop(&mut self) {
        let _ = self.session.complete_all();
        self.session.invalidate();
    }
}

struct DecState {
    frames: Mutex<Vec<VtDecodedFrame>>,
}

extern "C" fn dec_output(
    ref_con: *mut DecState,
    _src_ref_con: *mut c_void,
    status: os::Status,
    _flags: vt::DecodeInfoFlags,
    image: Option<&cv::ImageBuf>,
    _pts: cm::Time,
    _duration: cm::Time,
) {
    if ref_con.is_null() || status.is_err() {
        return;
    }
    let state = unsafe { &*ref_con };
    let Some(image) = image else {
        return;
    };
    let width = image.width();
    let height = image.height();
    if width == 0 || height == 0 {
        return;
    }
    let Ok(_guard) = image.base_address_lock(cv::pixel_buffer::LockFlags::READ_ONLY) else {
        return;
    };
    let pbp = image as *const cv::ImageBuf as *const c_void;
    unsafe {
        let y_src = CVPixelBufferGetBaseAddressOfPlane(pbp, 0);
        let y_stride = CVPixelBufferGetBytesPerRowOfPlane(pbp, 0);
        let uv_src = CVPixelBufferGetBaseAddressOfPlane(pbp, 1);
        let uv_stride = CVPixelBufferGetBytesPerRowOfPlane(pbp, 1);
        if y_src.is_null() || uv_src.is_null() || y_stride == 0 {
            return;
        }
        let stride = y_stride;
        let mut nv12 = vec![0u8; stride * height * 3 / 2];
        for row in 0..height {
            std::ptr::copy_nonoverlapping(
                y_src.add(row * y_stride),
                nv12.as_mut_ptr().add(row * stride),
                stride.min(y_stride),
            );
        }
        let uv_out = nv12.as_mut_ptr().add(stride * height);
        for row in 0..height / 2 {
            std::ptr::copy_nonoverlapping(
                uv_src.add(row * uv_stride),
                uv_out.add(row * stride),
                stride.min(uv_stride),
            );
        }
        state.frames.lock().unwrap().push(VtDecodedFrame {
            nv12,
            stride_y: stride as i32,
            width: width as i32,
            height: height as i32,
        });
    }
}

pub struct VtDecoder {
    session: Option<arc::R<vt::decompression::Session>>,
    desc: Option<arc::R<cm::VideoFormatDesc>>,
    record: Option<vt::decompression::session::OutputCbRecord<DecState, c_void>>,
    sps: Vec<u8>,
    pps: Vec<u8>,
}

unsafe impl Send for VtDecoder {}

impl VtDecoder {
    pub fn new() -> ResultType<Self> {
        Ok(VtDecoder {
            session: None,
            desc: None,
            record: None,
            sps: Vec::new(),
            pps: Vec::new(),
        })
    }

    fn ensure_session(&mut self, sps: &[u8], pps: &[u8]) -> ResultType<()> {
        if self.session.is_some() && self.sps == sps && self.pps == pps {
            return Ok(());
        }
        if let Some(mut s) = self.session.take() {
            s.invalidate();
        }
        if let Some(r) = self.record.take() {
            unsafe {
                drop(Box::from_raw(r.output_ref_con));
            }
        }
        let pointers = [sps.as_ptr(), pps.as_ptr()];
        let sizes = [sps.len(), pps.len()];
        let desc = match cm::VideoFormatDesc::with_h264_param_sets(&pointers, &sizes, 4) {
            Ok(d) => d,
            Err(e) => bail!("CMVideoFormatDescriptionCreateFromH264ParameterSets failed: {e:?}"),
        };
        let fmt = cf::Number::from_i32(cv::PixelFormat::_420V.0 as i32);
        let attrs = cf::DictionaryOf::with_keys_values(
            &[cv::pixel_buffer::keys::pixel_format()],
            &[fmt.as_type_ref()],
        );
        let record = vt::decompression::session::OutputCbRecord::new(
            DecState {
                frames: Mutex::new(Vec::new()),
            },
            dec_output,
        );
        let session = match vt::decompression::Session::new::<DecState, c_void>(
            &desc,
            None,
            Some(&attrs),
            Some(&record),
        ) {
            Ok(s) => s,
            Err(e) => bail!("VTDecompressionSessionCreate failed: {e:?}"),
        };
        self.session = Some(session);
        self.desc = Some(desc);
        self.record = Some(record);
        self.sps = sps.to_vec();
        self.pps = pps.to_vec();
        Ok(())
    }

    pub fn decode(&mut self, data: &[u8], _ms: i64) -> ResultType<Vec<VtDecodedFrame>> {
        let nals = split_annexb(data);
        if nals.is_empty() {
            return Ok(Vec::new());
        }
        let mut sps: Option<&[u8]> = None;
        let mut pps: Option<&[u8]> = None;
        let mut avcc: Vec<u8> = Vec::with_capacity(data.len() + 16);
        for nal in &nals {
            if nal.is_empty() {
                continue;
            }
            match nal[0] & 0x1f {
                7 => sps = Some(nal),
                8 => pps = Some(nal),
                6 | 9 => {}
                _ => {
                    avcc.extend_from_slice(&(nal.len() as u32).to_be_bytes());
                    avcc.extend_from_slice(nal);
                }
            }
        }
        if let (Some(s), Some(p)) = (sps, pps) {
            self.ensure_session(s, p)?;
        }
        let Some(session) = self.session.as_ref() else {
            bail!("no SPS/PPS seen yet");
        };
        if avcc.is_empty() {
            return Ok(Vec::new());
        }
        let mut block = match cm::BlockBuf::with_mem_block(avcc.len(), None) {
            Ok(b) => b,
            Err(e) => bail!("CMBlockBufferCreateWithMemoryBlock failed: {e:?}"),
        };
        {
            let dst = block
                .as_mut_slice()
                .map_err(|e| hbb_common::anyhow::anyhow!("CMBlockBufferGetDataPointer failed: {e:?}"))?;
            dst.copy_from_slice(&avcc);
        }
        let desc = self.desc.as_ref().map(|d| d.as_ref());
        let fdesc: Option<&cm::FormatDesc> = desc.map(|d| unsafe { std::mem::transmute(d) });
        let timing = cm::SampleTimingInfo {
            duration: cm::Time::new(33, 1000),
            pts: cm::Time::new(_ms.max(0), 1000),
            dts: cm::Time::invalid(),
        };
        let sizes = [avcc.len()];
        let mut sample_out: Option<arc::R<cm::SampleBuf>> = None;
        let create = unsafe {
            cm::SampleBuf::create_in(
                None,
                Some(&block),
                true,
                None,
                std::ptr::null(),
                fdesc,
                1,
                1,
                &timing,
                1,
                sizes.as_ptr(),
                &mut sample_out,
            )
        };
        if create.is_err() {
            bail!("CMSampleBufferCreate failed: {create:?}");
        }
        let Some(sample) = sample_out else {
            bail!("CMSampleBufferCreate returned no sample");
        };
        if let Err(e) = session.decode(&sample, vt::DecodeFrameFlags(0)) {
            bail!("VTDecompressionSessionDecodeFrame failed: {e:?}");
        }
        let record = self.record.as_ref().unwrap();
        let state = unsafe { &*record.output_ref_con };
        let mut out = state.frames.lock().unwrap();
        Ok(std::mem::take(&mut *out))
    }
}

impl Drop for VtDecoder {
    fn drop(&mut self) {
        if let Some(mut s) = self.session.take() {
            s.invalidate();
        }
        if let Some(r) = self.record.take() {
            unsafe {
                drop(Box::from_raw(r.output_ref_con));
            }
        }
    }
}

fn split_annexb(data: &[u8]) -> Vec<&[u8]> {
    let mut out = Vec::new();
    let mut i = 0usize;
    let n = data.len();
    let mut start: Option<usize> = None;
    while i + 2 < n {
        if data[i] == 0 && data[i + 1] == 0 && (data[i + 2] == 1 || (i + 3 < n && data[i + 2] == 0 && data[i + 3] == 1)) {
            let sc_len = if data[i + 2] == 1 { 3 } else { 4 };
            if let Some(s) = start {
                out.push(&data[s..i]);
            }
            start = Some(i + sc_len);
            i += sc_len;
        } else {
            i += 1;
        }
    }
    if let Some(s) = start {
        if s < n {
            out.push(&data[s..n]);
        }
    }
    out
}

pub fn list_hardware_h264_encoders() -> Vec<String> {
    vec!["videotoolbox".to_owned()]
}

pub fn probe_hardware_h264() -> (Vec<String>, bool) {
    let mut working = Vec::new();
    match VtEncoder::new("videotoolbox", 1280, 720, 1000, 30) {
        Ok(mut enc) => {
            let nv12 = vec![16u8; 1280 * 720 * 3 / 2];
            let mut produced = false;
            for i in 0..8 {
                match enc.encode_nv12(&nv12, i * 33) {
                    Ok(frames) if !frames.is_empty() => {
                        produced = true;
                        break;
                    }
                    _ => {}
                }
            }
            if produced {
                working.push("videotoolbox".to_owned());
            } else {
                log::info!("videotoolbox encoder produced no frames in probe");
            }
        }
        Err(e) => {
            log::info!("videotoolbox encoder probe failed: {e:?}");
        }
    }
    let decoder_ok = vt::decompression::session::is_hardware_decode_supported(cm::VideoCodec::H264)
        || !working.is_empty();
    (working, decoder_ok)
}

pub fn gpu_signature() -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in b"videotoolbox-v1" {
        hash ^= *b as u64;
        hash = hash.wrapping_mul(0x1000_0000_01b3);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_decode_roundtrip() {
        let mut enc = VtEncoder::new("videotoolbox", 640, 360, 800, 30).unwrap();
        let mut dec = VtDecoder::new().unwrap();
        let mut nv12 = vec![100u8; 640 * 360 * 3 / 2];
        let mut decoded = 0usize;
        for i in 0..30i64 {
            nv12[(i as usize * 977) % (640 * 360)] = (i * 7) as u8;
            for f in enc.encode_nv12(&nv12, i * 33).unwrap() {
                for d in dec.decode(&f.data, 0).unwrap() {
                    assert_eq!(d.width, 640);
                    assert_eq!(d.height, 360);
                    assert!(d.nv12.len() >= d.stride_y as usize * 360 * 3 / 2);
                    decoded += 1;
                }
            }
        }
        assert!(decoded > 0, "no frames decoded");
    }
}
