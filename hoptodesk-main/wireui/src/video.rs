#![allow(non_camel_case_types)]

use std::ops::{Deref, DerefMut};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

#[repr(C)]
#[derive(Debug, Copy, Clone, PartialOrd, PartialEq)]
pub enum COLOR_SPACE {
    Unknown = 0,
    Yv12,
    Iyuv,
    Nv12,
    Yuy2,
    Rgb24,
    Rgb555,
    Rgb565,
    Rgb32,
}

pub type FrameWaker = Arc<dyn Fn() + Send + Sync>;

#[derive(Default)]
pub struct FrameData {
    pub width: i32,
    pub height: i32,
    pub rgba: Vec<u8>,
    pub version: u64,
    pub streaming: bool,
    pub wake: Option<FrameWaker>,
    pub cached_image: Option<(u64, vello::peniko::Image)>,
}

pub type FrameSink = Arc<Mutex<FrameData>>;

pub fn new_frame_sink() -> FrameSink {
    Arc::new(Mutex::new(FrameData::default()))
}

pub struct video_source {
    _priv: (),
}

pub struct video_destination {
    refcount: AtomicUsize,
    sink: FrameSink,
}

impl video_destination {
    pub fn boxed(sink: FrameSink) -> *mut video_destination {
        Box::into_raw(Box::new(video_destination {
            refcount: AtomicUsize::new(0),
            sink,
        }))
    }

    pub fn is_alive(&self) -> bool {
        true
    }

    pub fn start_streaming(
        &mut self,
        frame_size: (i32, i32),
        _color_space: COLOR_SPACE,
        src: Option<&video_source>,
    ) -> Result<(), ()> {
        let _ = src;
        let wake = {
            let mut f = self.sink.lock().unwrap();
            f.width = frame_size.0;
            f.height = frame_size.1;
            f.streaming = true;
            f.wake.clone()
        };
        if let Some(w) = wake {
            w();
        }
        Ok(())
    }

    pub fn stop_streaming(&mut self) -> Result<(), ()> {
        let wake = {
            let mut f = self.sink.lock().unwrap();
            f.streaming = false;
            f.wake.clone()
        };
        if let Some(w) = wake {
            w();
        }
        Ok(())
    }

    pub fn render_frame(&mut self, data: &[u8]) -> Result<(), ()> {
        let wake = {
            let mut f = self.sink.lock().unwrap();
            f.rgba.clear();
            f.rgba.extend_from_slice(data);
            f.version = f.version.wrapping_add(1);
            f.wake.clone()
        };
        if let Some(w) = wake {
            w();
        }
        Ok(())
    }

    pub fn render_frame_with_stride(&mut self, data: &[u8], stride: u32) -> Result<(), ()> {
        let _ = stride;
        self.render_frame(data)
    }
}

pub struct fragmented_video_destination {
    _priv: (),
}

pub trait Asset {
    /// # Safety
    /// `ptr` must be a valid pointer to this asset type or null.
    unsafe fn add_ref_ptr(ptr: *mut Self);
    /// # Safety
    /// `ptr` must be a valid pointer to this asset type or null.
    unsafe fn release_ptr(ptr: *mut Self);
}

impl Asset for video_destination {
    unsafe fn add_ref_ptr(ptr: *mut Self) {
        if !ptr.is_null() {
            (*ptr).refcount.fetch_add(1, Ordering::SeqCst);
        }
    }
    unsafe fn release_ptr(ptr: *mut Self) {
        if ptr.is_null() {
            return;
        }
        if (*ptr).refcount.fetch_sub(1, Ordering::SeqCst) == 1 {
            drop(Box::from_raw(ptr));
        }
    }
}

impl Asset for fragmented_video_destination {
    unsafe fn add_ref_ptr(_ptr: *mut Self) {}
    unsafe fn release_ptr(_ptr: *mut Self) {}
}

pub struct AssetPtr<T: Asset> {
    ptr: *mut T,
}

unsafe impl<T: Asset> Send for AssetPtr<T> {}

impl<T: Asset> AssetPtr<T> {
    pub fn adopt(lp: *mut T) -> Self {
        unsafe { T::add_ref_ptr(lp) };
        AssetPtr { ptr: lp }
    }
}

impl<T: Asset> From<*mut T> for AssetPtr<T> {
    fn from(lp: *mut T) -> Self {
        AssetPtr::adopt(lp)
    }
}

impl Deref for AssetPtr<video_destination> {
    type Target = video_destination;
    fn deref(&self) -> &video_destination {
        unsafe { &*self.ptr }
    }
}

impl DerefMut for AssetPtr<video_destination> {
    fn deref_mut(&mut self) -> &mut video_destination {
        unsafe { &mut *self.ptr }
    }
}

impl<T: Asset> Drop for AssetPtr<T> {
    fn drop(&mut self) {
        unsafe { T::release_ptr(self.ptr) };
    }
}
