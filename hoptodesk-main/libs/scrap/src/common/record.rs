use crate::CodecFormat;
use hbb_common::{
    bail, chrono,
    config::Config,
    log,
    message_proto::{message, video_frame, EncodedVideoFrame, Message, RecordingFinished},
    ResultType,
};
use std::{
    fs::{File, OpenOptions},
    io,
    ops::{Deref, DerefMut},
    path::PathBuf,
    sync::{
        atomic::{AtomicUsize, Ordering},
        mpsc::Sender,
        Mutex,
    },
    time::Instant,
};
use webm::mux::{self, Segment, Track, VideoTrack, Writer};

const MIN_SECS: u64 = 1;

static OPEN_RECORDERS: AtomicUsize = AtomicUsize::new(0);
static FINISHED_OUTGOING: Mutex<Vec<(String, RecordingFinished)>> = Mutex::new(Vec::new());

pub fn open_recorders() -> usize {
    OPEN_RECORDERS.load(Ordering::SeqCst)
}

pub fn take_finished_outgoing(peer_id: &str) -> Vec<RecordingFinished> {
    let mut finished = FINISHED_OUTGOING.lock().unwrap();
    let (mine, rest): (Vec<_>, Vec<_>) = finished.drain(..).partition(|(id, _)| id == peer_id);
    *finished = rest;
    mine.into_iter().map(|(_, r)| r).collect()
}

#[derive(Debug, Clone)]
pub struct RecorderContext {
    pub server: bool,
    pub id: String,
    pub dir: String,
    pub display_idx: usize,
    pub camera: bool,
    pub label: String,
    pub technician_id: String,
    pub technician_name: String,
    pub tx: Option<Sender<RecordState>>,
}

#[derive(Debug, Clone)]
pub struct RecorderContext2 {
    pub filename: String,
    pub width: usize,
    pub height: usize,
    pub format: CodecFormat,
}

impl RecorderContext2 {
    pub fn set_filename(&mut self, ctx: &RecorderContext) -> ResultType<()> {
        if !PathBuf::from(&ctx.dir).exists() {
            std::fs::create_dir_all(&ctx.dir)?;
        }
        let part = |s: &str| -> String {
            let s: String = s
                .chars()
                .map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '-' })
                .take(32)
                .collect();
            if s.is_empty() { s } else { format!("_{s}") }
        };
        let file = if ctx.server { "incoming" } else { "outgoing" }.to_string()
            + "_"
            + &ctx.id.clone()
            + &part(&ctx.label)
            + &part(&ctx.technician_id)
            + &part(&ctx.technician_name)
            + &chrono::Local::now().format("_%Y%m%d%H%M%S%3f_").to_string()
            + &format!(
                "{}{}_",
                if ctx.camera { "camera" } else { "display" },
                ctx.display_idx
            )
            + &self.format.to_string().to_lowercase()
            + if self.format == CodecFormat::VP9
                || self.format == CodecFormat::VP8
                //|| self.format == CodecFormat::AV1
            {
                ".webm"
            } else {
                ".mp4"
            };
        self.filename = PathBuf::from(&ctx.dir)
            .join(file)
            .to_string_lossy()
            .to_string();
        Ok(())
    }
}

unsafe impl Send for Recorder {}
unsafe impl Sync for Recorder {}

pub trait RecorderApi {
    fn new(ctx: RecorderContext, ctx2: RecorderContext2) -> ResultType<Self>
    where
        Self: Sized;
    fn write_video(&mut self, frame: &EncodedVideoFrame, pts: i64) -> bool;
}

#[derive(Debug)]
pub enum RecordState {
    NewFile(String),
    NewFrame,
    WriteTail,
    RemoveFile,
}

pub struct Recorder {
    pub inner: Option<Box<dyn RecorderApi>>,
    ctx: RecorderContext,
    ctx2: Option<RecorderContext2>,
    pts: Option<i64>,
    pts_offset: i64,
    failed_format: Option<CodecFormat>,
}

impl Deref for Recorder {
    type Target = Option<Box<dyn RecorderApi>>;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl DerefMut for Recorder {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.inner
    }
}

impl Recorder {
    pub fn new(ctx: RecorderContext) -> ResultType<Self> {
        Ok(Self {
            inner: None,
            ctx,
            ctx2: None,
            pts: None,
            pts_offset: 0,
            failed_format: None,
        })
    }

    fn check(&mut self, w: usize, h: usize, format: CodecFormat) -> ResultType<()> {
        match self.ctx2 {
            Some(ref ctx2) => {
                if ctx2.width != w || ctx2.height != h || ctx2.format != format {
                    let mut ctx2 = RecorderContext2 {
                        width: w,
                        height: h,
                        format,
                        filename: Default::default(),
                    };
                    ctx2.set_filename(&self.ctx)?;
                    self.ctx2 = Some(ctx2);
                    self.inner = None;
                }
            }
            None => {
                let mut ctx2 = RecorderContext2 {
                    width: w,
                    height: h,
                    format,
                    filename: Default::default(),
                };
                ctx2.set_filename(&self.ctx)?;
                self.ctx2 = Some(ctx2);
                self.inner = None;
            }
        }
        let Some(ctx2) = &self.ctx2 else {
            bail!("ctx2 is None");
        };
        if self.inner.is_none() {
            self.inner = match format {
                CodecFormat::VP8 | CodecFormat::VP9 => Some(Box::new(
                    WebmRecorder::new(self.ctx.clone(), (*ctx2).clone())?,
                )),
                _ => bail!("unsupported codec type"),
            };
            self.pts = None;
            self.pts_offset = 0;
            self.send_state(RecordState::NewFile(ctx2.filename.clone()));
        }
        Ok(())
    }

    pub fn write_message(&mut self, msg: &Message, w: usize, h: usize) {
        if let Some(message::Union::VideoFrame(vf)) = &msg.union {
            if let Some(frame) = &vf.union {
                self.write_frame(frame, w, h).ok();
            }
        }
    }

    pub fn write_frame(
        &mut self,
        frame: &video_frame::Union,
        w: usize,
        h: usize,
    ) -> ResultType<()> {
        let format = CodecFormat::from(frame);
        if format == CodecFormat::Unknown {
            bail!("unsupported frame type");
        }
        if self.failed_format == Some(format) {
            bail!("check failed");
        }
        self.failed_format = None;
        let res = self.check(w, h, format);
        if res.is_err() {
            self.failed_format = Some(format);
            log::error!("check failed: {:?}", res);
            res?;
        }
        match frame {
            video_frame::Union::Vp8s(vp8s) => {
                for f in vp8s.frames.iter() {
                    let pts = self.check_pts(f.pts, f.key)?;
                    self.as_mut().map(|x| x.write_video(f, pts));
                }
            }
            video_frame::Union::Vp9s(vp9s) => {
                for f in vp9s.frames.iter() {
                    let pts = self.check_pts(f.pts, f.key)?;
                    self.as_mut().map(|x| x.write_video(f, pts));
                }
            }
            video_frame::Union::Av1s(av1s) => {
                for f in av1s.frames.iter() {
                    let pts = self.check_pts(f.pts, f.key)?;
                    self.as_mut().map(|x| x.write_video(f, pts));
                }
            }
            #[cfg(feature = "nativecodec")]
            video_frame::Union::H264s(h264s) => {
                for f in h264s.frames.iter() {
                    let pts = self.check_pts(f.pts, f.key)?;
                    self.as_mut().map(|x| x.write_video(f, pts));
                }
            }
            #[cfg(feature = "nativecodec")]
            video_frame::Union::H265s(h265s) => {
                for f in h265s.frames.iter() {
                    let pts = self.check_pts(f.pts, f.key)?;
                    self.as_mut().map(|x| x.write_video(f, pts));
                }
            }
            _ => bail!("unsupported frame type"),
        }
        self.send_state(RecordState::NewFrame);
        Ok(())
    }

    fn check_pts(&mut self, raw_pts: i64, key: bool) -> ResultType<i64> {
        // https://stackoverflow.com/questions/76379101/how-to-create-one-playable-webm-file-from-two-different-video-tracks-with-same-c
        if self.pts.is_none() && !key {
            bail!("first frame is not key frame");
        }
        let mut pts = raw_pts + self.pts_offset;
        if let Some(last) = self.pts {
            if pts <= last {
                self.pts_offset = last + 1 - raw_pts;
                pts = last + 1;
                log::info!("pts went backwards ({raw_pts} after {last}), remapped to keep one file");
            }
        }
        self.pts = Some(pts);
        Ok(pts)
    }

    fn send_state(&self, state: RecordState) {
        self.ctx.tx.as_ref().map(|tx| tx.send(state));
    }
}

struct WebmRecorder {
    vt: VideoTrack,
    webm: Option<Segment<Writer<File>>>,
    ctx: RecorderContext,
    ctx2: RecorderContext2,
    key: bool,
    written: bool,
    start: Instant,
}

impl RecorderApi for WebmRecorder {
    fn new(ctx: RecorderContext, ctx2: RecorderContext2) -> ResultType<Self> {
        let out = match {
            OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&ctx2.filename)
        } {
            Ok(file) => file,
            Err(ref e) if e.kind() == io::ErrorKind::AlreadyExists => File::create(&ctx2.filename)?,
            Err(e) => return Err(e.into()),
        };
        let mut webm = match mux::Segment::new(mux::Writer::new(out)) {
            Some(v) => v,
            None => bail!("Failed to create webm mux"),
        };
        let vt = webm.add_video_track(
            ctx2.width as _,
            ctx2.height as _,
            None,
            if ctx2.format == CodecFormat::VP9 {
                mux::VideoCodecId::VP9
            } else if ctx2.format == CodecFormat::VP8 {
                mux::VideoCodecId::VP8
            } else {
                mux::VideoCodecId::VP9
            },
        );
        /*if ctx2.format == CodecFormat::AV1 {
            // [129, 8, 12, 0] in 3.6.0, but zero works
            let codec_private = vec![0, 0, 0, 0];
            if !webm.set_codec_private(vt.track_number(), &codec_private) {
                bail!("Failed to set codec private");
            }
        }*/
        OPEN_RECORDERS.fetch_add(1, Ordering::SeqCst);
        Ok(WebmRecorder {
            vt,
            webm: Some(webm),
            ctx,
            ctx2,
            key: false,
            written: false,
            start: Instant::now(),
        })
    }

    fn write_video(&mut self, frame: &EncodedVideoFrame, pts: i64) -> bool {
        if frame.key {
            self.key = true;
        }
        if self.key {
            let ok = self
                .vt
                .add_frame(&frame.data, pts as u64 * 1_000_000, frame.key);
            if ok {
                self.written = true;
            }
            ok
        } else {
            false
        }
    }
}

impl Drop for WebmRecorder {
    fn drop(&mut self) {
        let elapsed = self.start.elapsed().as_secs();
        let _ = std::mem::replace(&mut self.webm, None).map_or(false, |webm| webm.finalize(None));
        let mut state = RecordState::WriteTail;
        if !self.written || elapsed < MIN_SECS {
            std::fs::remove_file(&self.ctx2.filename).ok();
            state = RecordState::RemoveFile;
        } else {
            queue_recording_metadata(&self.ctx, &self.ctx2, elapsed);
        }
        self.ctx.tx.as_ref().map(|tx| tx.send(state));
        OPEN_RECORDERS.fetch_sub(1, Ordering::SeqCst);
    }
}

pub fn recording_outbox_path() -> PathBuf {
    Config::path("recordings-outbox.jsonl")
}

fn json_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

fn queue_recording_metadata(ctx: &RecorderContext, ctx2: &RecorderContext2, duration_secs: u64) {
    let path = PathBuf::from(&ctx2.filename);
    let file_name = path
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    let file_size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
    let session_end = chrono::Local::now();
    let session_start = (session_end - chrono::Duration::seconds(duration_secs as i64)).to_rfc3339();
    let session_end = session_end.to_rfc3339();
    let line = format!(
        "{{\"direction\":\"{}\",\"file_name\":\"{}\",\"file_path\":\"{}\",\"file_size\":{},\"duration_seconds\":{},\"peer_id\":\"{}\",\"peer_name\":\"{}\",\"technician\":\"{}\",\"session_start\":\"{}\",\"session_end\":\"{}\"}}\n",
        if ctx.server { "incoming" } else { "outgoing" },
        json_escape(&file_name),
        json_escape(&ctx2.filename),
        file_size,
        duration_secs,
        json_escape(if ctx.server && !ctx.technician_id.is_empty() { &ctx.technician_id } else { &ctx.id }),
        json_escape(&ctx.label),
        json_escape(&ctx.technician_name),
        session_start,
        session_end,
    );
    match OpenOptions::new()
        .create(true)
        .append(true)
        .open(recording_outbox_path())
    {
        Ok(mut f) => {
            use std::io::Write;
            if let Err(e) = f.write_all(line.as_bytes()) {
                log::warn!("failed to queue recording metadata: {}", e);
            }
        }
        Err(e) => log::warn!("failed to open recording outbox: {}", e),
    }
    if !ctx.server {
        FINISHED_OUTGOING.lock().unwrap().push((
            ctx.id.clone(),
            RecordingFinished {
                file_name,
                file_size,
                duration_seconds: duration_secs,
                session_start,
                session_end,
                ..Default::default()
            },
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hbb_common::message_proto::EncodedVideoFrames;

    fn recorder(dir: &std::path::Path) -> Recorder {
        Recorder::new(RecorderContext {
            server: false,
            id: "100000001".to_owned(),
            dir: dir.to_string_lossy().to_string(),
            display_idx: 0,
            camera: false,
            label: String::new(),
            technician_id: String::new(),
            technician_name: String::new(),
            tx: None,
        })
        .unwrap()
    }

    fn vp9(pts: i64, key: bool) -> video_frame::Union {
        video_frame::Union::Vp9s(EncodedVideoFrames {
            frames: vec![EncodedVideoFrame {
                data: vec![0u8; 16].into(),
                key,
                pts,
                ..Default::default()
            }]
            .into(),
            ..Default::default()
        })
    }

    fn webm_count(dir: &std::path::Path) -> usize {
        std::fs::read_dir(dir)
            .map(|d| d.filter_map(|e| e.ok()).count())
            .unwrap_or(0)
    }

    #[test]
    fn a_pts_regression_stays_in_one_file_with_monotonic_pts() {
        let dir = std::env::temp_dir().join(format!("htd-record-pts-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        let mut r = recorder(&dir);

        r.write_frame(&vp9(100, true), 1920, 1080).unwrap();
        r.write_frame(&vp9(845, false), 1920, 1080).unwrap();
        let name_before = r.ctx2.as_ref().unwrap().filename.clone();

        r.write_frame(&vp9(395, false), 1920, 1080).unwrap();
        assert_eq!(r.pts, Some(846));
        assert_eq!(
            r.ctx2.as_ref().unwrap().filename,
            name_before,
            "a pts regression must not rotate to a new file"
        );
        assert_eq!(webm_count(&dir), 1);

        r.write_frame(&vp9(428, false), 1920, 1080).unwrap();
        assert_eq!(r.pts, Some(879));

        drop(r);
        std::fs::remove_dir_all(&dir).ok();
    }

    fn av1(pts: i64, key: bool) -> video_frame::Union {
        video_frame::Union::Av1s(EncodedVideoFrames {
            frames: vec![EncodedVideoFrame {
                data: vec![0u8; 16].into(),
                key,
                pts,
                ..Default::default()
            }]
            .into(),
            ..Default::default()
        })
    }

    #[test]
    fn an_unsupported_codec_no_longer_latches_the_recorder_forever() {
        let dir = std::env::temp_dir().join(format!("htd-record-latch-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        let mut r = recorder(&dir);

        assert!(r.write_frame(&av1(100, true), 1920, 1080).is_err());
        assert!(
            r.write_frame(&av1(133, true), 1920, 1080).is_err(),
            "repeated frames of the failed format stay rejected"
        );

        r.write_frame(&vp9(200, true), 1920, 1080)
            .expect("a recordable format after a failed one must start recording");
        assert_eq!(webm_count(&dir), 1);

        drop(r);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_first_frame_must_still_be_a_key_frame() {
        let dir = std::env::temp_dir().join(format!("htd-record-key-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        let mut r = recorder(&dir);
        assert!(r.write_frame(&vp9(100, false), 1920, 1080).is_err());
        drop(r);
        std::fs::remove_dir_all(&dir).ok();
    }
}
