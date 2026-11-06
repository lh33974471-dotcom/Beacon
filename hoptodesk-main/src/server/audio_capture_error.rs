use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

#[derive(Clone, Default)]
pub(super) struct CaptureErrorHandler {
    interrupted: Arc<AtomicBool>,
}

impl CaptureErrorHandler {
    pub(super) fn handle(&self, error: cpal::StreamError) {
        if matches!(
            error,
            cpal::StreamError::StreamInterrupted { .. } | cpal::StreamError::DeviceNotAvailable
        ) {
            hbb_common::log::error!("Audio capture stream interrupted: {}", error);
            self.interrupted.store(true, Ordering::Relaxed);
        } else {
            hbb_common::log::trace!("an error occurred on stream: {}", error);
        }
    }

    pub(super) fn needs_restart(&self) -> bool {
        self.interrupted.load(Ordering::Relaxed)
    }
}
