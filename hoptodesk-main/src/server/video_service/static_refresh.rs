use super::{GenericService, VideoFrameController};
use hbb_common::{bail, log, message_proto::Message, ResultType};
use scrap::{
    codec::{Encoder, EncoderCfg, BR_BALANCED, BR_BEST, BR_SPEED},
    record::Recorder,
    CodecFormat, EncodeInput,
};
use std::{
    collections::HashSet,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

const INITIAL_REPEAT_INTERVAL: Duration = Duration::from_millis(100);
const MAX_REPEAT_FPS: u32 = 30;
const MAX_REPEAT_FAILURES: usize = 3;
const MAX_REPEAT_NO_OUTPUTS: usize = 100;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HardwareBackend {
    #[cfg(any(feature = "nativecodec", test))]
    VideoToolbox,
    Other,
}

fn max_repeat_frames(codec: CodecFormat, quality: f32, backend: HardwareBackend) -> usize {
    let (low, low_to_balanced, balanced_to_best, best) = match (codec, backend) {
        (CodecFormat::VP8, _) => (20, 20, 20, 30),
        (CodecFormat::VP9, _) => (40, 20, 20, 20),
        #[cfg(any(feature = "nativecodec", test))]
        (CodecFormat::H265, HardwareBackend::VideoToolbox) => (90, 90, 50, 90),
        (CodecFormat::H264, _) => (90, 60, 60, 30),
        (CodecFormat::H265, _) => (90, 60, 60, 30),
        (CodecFormat::Unknown, _) => return 0,
    };
    if quality >= BR_BEST {
        best
    } else if quality >= BR_BALANCED {
        balanced_to_best
    } else if quality >= BR_SPEED {
        low_to_balanced
    } else {
        low
    }
}

struct RefreshPlan {
    codec_format: CodecFormat,
    hardware_backend: HardwareBackend,
    is_monitor: bool,
    repeat_output_counter: usize,
    repeat_no_outputs: usize,
    repeat_failures: usize,
    source_ready: bool,
}

impl RefreshPlan {
    fn new(codec_format: CodecFormat, hardware_backend: HardwareBackend, is_monitor: bool) -> Self {
        Self {
            codec_format,
            hardware_backend,
            is_monitor,
            repeat_output_counter: 0,
            repeat_no_outputs: 0,
            repeat_failures: 0,
            source_ready: false,
        }
    }

    fn is_vpx(&self) -> bool {
        matches!(self.codec_format, CodecFormat::VP8 | CodecFormat::VP9)
    }

    fn on_frame(&mut self) {
        self.repeat_output_counter = 0;
        self.repeat_no_outputs = 0;
        self.source_ready = false;
    }

    fn on_encoded(&mut self, success: bool) {
        if success {
            self.source_ready = true;
            self.repeat_failures = 0;
        }
    }

    fn note_output(&mut self) {
        self.repeat_output_counter += 1;
        self.repeat_failures = 0;
    }

    fn note_no_output(&mut self) {
        self.repeat_no_outputs += 1;
    }

    fn note_failure(&mut self) {
        self.repeat_no_outputs += 1;
        self.repeat_failures += 1;
    }

    fn interval(&self, spf: Duration) -> Duration {
        if self.is_vpx() || (self.repeat_output_counter == 0 && self.repeat_no_outputs == 0) {
            INITIAL_REPEAT_INTERVAL.max(spf)
        } else {
            spf
        }
    }

    fn due(&self, spf: Duration, quality: f32, elapsed: Duration) -> bool {
        self.is_monitor
            && self.source_ready
            && (self.repeat_failures < MAX_REPEAT_FAILURES || self.is_vpx())
            && self.repeat_no_outputs < MAX_REPEAT_NO_OUTPUTS
            && self.repeat_output_counter
                < max_repeat_frames(self.codec_format, quality, self.hardware_backend)
            && elapsed >= self.interval(spf)
    }
}

#[derive(Debug)]
pub(super) struct FrameEncodeResult {
    pub(super) send_conn_ids: HashSet<i32>,
    pub(super) vpx_no_output: bool,
}

pub(super) struct StaticRefresh<'a> {
    plan: RefreshPlan,
    sp: &'a GenericService,
    recorder: &'a Arc<Mutex<Option<Recorder>>>,
    display_idx: usize,
    width: usize,
    height: usize,
    last_encode: Instant,
}

impl<'a> StaticRefresh<'a> {
    pub(super) fn new(
        is_monitor: bool,
        codec_format: CodecFormat,
        encoder_config: &EncoderCfg,
        sp: &'a GenericService,
        recorder: &'a Arc<Mutex<Option<Recorder>>>,
        display_idx: usize,
        width: usize,
        height: usize,
    ) -> Self {
        let hardware_backend = match encoder_config {
            #[cfg(feature = "nativecodec")]
            EncoderCfg::HWRAM(_) if cfg!(target_os = "macos") => HardwareBackend::VideoToolbox,
            _ => HardwareBackend::Other,
        };
        Self {
            plan: RefreshPlan::new(codec_format, hardware_backend, is_monitor),
            sp,
            recorder,
            display_idx,
            width,
            height,
            last_encode: Instant::now(),
        }
    }

    pub(super) fn on_frame(&mut self) {
        self.plan.on_frame();
    }

    pub(super) fn on_frame_encoded(&mut self, result: &FrameEncodeResult) {
        let vpx_dropped_without_error = self.plan.is_vpx() && result.vpx_no_output;
        self.on_encoded(!result.send_conn_ids.is_empty() || vpx_dropped_without_error);
    }

    pub(super) fn on_encoded(&mut self, success: bool) {
        self.last_encode = Instant::now();
        self.plan.on_encoded(success);
    }

    pub(super) fn try_encode(
        &mut self,
        yuv: &[u8],
        spf: Duration,
        quality: f32,
        now: Instant,
        ms: i64,
        encoder: &mut Encoder,
        frame_controller: &mut VideoFrameController,
    ) -> ResultType<()> {
        let spf = spf.max(Duration::from_secs(1) / MAX_REPEAT_FPS);
        if yuv.is_empty() || !self.plan.due(spf, quality, self.last_encode.elapsed()) {
            return Ok(());
        }
        self.sp.snapshot(|sps| {
            if sps.has_subscribes() {
                bail!("SWITCH");
            }
            Ok(())
        })?;
        let result = encoder.encode_to_message(EncodeInput::YUV(yuv), ms);
        self.last_encode = if self.plan.is_vpx() {
            Instant::now()
        } else {
            now
        };
        let mut vf = match result {
            Ok(vf) if vf.union.is_none() => {
                self.plan.note_no_output();
                return Ok(());
            }
            Ok(vf) => vf,
            Err(error) => {
                self.plan.note_failure();
                if !error.is::<scrap::VpxNoOutput>() {
                    log::debug!(
                        "static refresh failed ({}): {error:?}",
                        self.plan.repeat_failures
                    );
                }
                return Ok(());
            }
        };
        self.plan.note_output();
        vf.display = self.display_idx as _;
        let mut msg = Message::new();
        msg.set_video_frame(vf);
        self.recorder
            .lock()
            .unwrap()
            .as_mut()
            .map(|r| r.write_message(&msg, self.width, self.height));
        let send_conn_ids = self.sp.send_video_frame(msg);
        frame_controller.set_send(now, send_conn_ids);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SPF: Duration = Duration::from_millis(33);
    const LONG: Duration = Duration::from_secs(1);

    fn ready_plan(codec: CodecFormat) -> RefreshPlan {
        let mut plan = RefreshPlan::new(codec, HardwareBackend::Other, true);
        plan.on_frame();
        plan.on_encoded(true);
        plan
    }

    #[test]
    fn budget_stops_repeats_and_a_new_frame_restarts_it() {
        let mut plan = ready_plan(CodecFormat::VP9);
        let limit = max_repeat_frames(CodecFormat::VP9, BR_BALANCED, HardwareBackend::Other);
        assert_eq!(limit, 20);
        for _ in 0..limit {
            assert!(plan.due(SPF, BR_BALANCED, LONG));
            plan.note_output();
        }
        assert!(!plan.due(SPF, BR_BALANCED, LONG));
        plan.on_frame();
        assert!(!plan.due(SPF, BR_BALANCED, LONG));
        plan.on_encoded(true);
        assert!(plan.due(SPF, BR_BALANCED, LONG));
    }

    #[test]
    fn first_repeat_waits_100ms_then_hardware_follows_spf_and_vpx_keeps_100ms() {
        let mut h264 = ready_plan(CodecFormat::H264);
        assert!(!h264.due(SPF, BR_BALANCED, Duration::from_millis(99)));
        assert!(h264.due(SPF, BR_BALANCED, Duration::from_millis(100)));
        h264.note_output();
        assert!(h264.due(SPF, BR_BALANCED, SPF));
        let mut vp9 = ready_plan(CodecFormat::VP9);
        vp9.note_output();
        assert!(!vp9.due(SPF, BR_BALANCED, SPF));
        assert!(vp9.due(SPF, BR_BALANCED, Duration::from_millis(100)));
    }

    #[test]
    fn genuine_errors_pause_hardware_encoders_but_not_vpx() {
        let mut h264 = ready_plan(CodecFormat::H264);
        let mut vp8 = ready_plan(CodecFormat::VP8);
        for _ in 0..MAX_REPEAT_FAILURES {
            h264.note_failure();
            vp8.note_failure();
        }
        assert!(!h264.due(SPF, BR_BALANCED, LONG));
        assert!(vp8.due(SPF, BR_BALANCED, LONG));
        h264.on_encoded(true);
        assert!(h264.due(SPF, BR_BALANCED, LONG));
    }

    #[test]
    fn no_output_runs_end_after_the_cap() {
        let mut vp9 = ready_plan(CodecFormat::VP9);
        for _ in 0..MAX_REPEAT_NO_OUTPUTS {
            vp9.note_no_output();
        }
        assert!(!vp9.due(SPF, BR_BALANCED, LONG));
    }

    #[test]
    fn cameras_unknown_codecs_and_unready_sources_never_refresh() {
        let mut camera = RefreshPlan::new(CodecFormat::VP9, HardwareBackend::Other, false);
        camera.on_encoded(true);
        assert!(!camera.due(SPF, BR_BALANCED, LONG));
        assert!(!ready_plan(CodecFormat::Unknown).due(SPF, BR_BALANCED, LONG));
        let mut fresh = RefreshPlan::new(CodecFormat::VP9, HardwareBackend::Other, true);
        assert!(!fresh.due(SPF, BR_BALANCED, LONG));
        fresh.on_encoded(false);
        assert!(!fresh.due(SPF, BR_BALANCED, LONG));
    }

    #[test]
    fn budgets_follow_the_quality_band() {
        assert_eq!(max_repeat_frames(CodecFormat::VP8, 0.1, HardwareBackend::Other), 20);
        assert_eq!(max_repeat_frames(CodecFormat::VP8, BR_BEST, HardwareBackend::Other), 30);
        assert_eq!(max_repeat_frames(CodecFormat::VP9, 0.1, HardwareBackend::Other), 40);
        assert_eq!(max_repeat_frames(CodecFormat::H264, BR_SPEED, HardwareBackend::Other), 60);
        assert_eq!(
            max_repeat_frames(CodecFormat::H265, BR_BEST, HardwareBackend::VideoToolbox),
            90
        );
    }
}
