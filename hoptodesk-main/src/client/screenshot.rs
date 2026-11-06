#[cfg(not(any(target_os = "android", target_os = "ios")))]
use crate::clipboard::{update_clipboard, ClipboardSide};
#[cfg(not(any(target_os = "android", target_os = "ios")))]
use hbb_common::message_proto::*;
use hbb_common::ResultType;
#[cfg(not(any(target_os = "android", target_os = "ios")))]
use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

lazy_static::lazy_static! {
    static ref SCREENSHOT: Mutex<Screenshot> = Default::default();
}

#[cfg(not(any(target_os = "android", target_os = "ios")))]
lazy_static::lazy_static! {
    static ref COMPOSITE: Mutex<Option<Composite>> = Default::default();
}

const COMPOSITE_SID_PREFIX: &str = "multi:";

#[cfg(not(any(target_os = "android", target_os = "ios")))]
struct Composite {
    bbox_w: usize,
    bbox_h: usize,
    origins: HashMap<usize, (usize, usize)>,
    pending: HashSet<usize>,
    canvas: Vec<u8>,
}

#[cfg(not(any(target_os = "android", target_os = "ios")))]
impl Composite {
    fn paste(&mut self, display: usize, png: &[u8]) -> ResultType<()> {
        self.pending.remove(&display);
        let Some(&(x0, y0)) = self.origins.get(&display) else {
            return Ok(());
        };
        let image = image::load_from_memory(png)?.to_rgba8();
        let (w, h) = (image.width() as usize, image.height() as usize);
        let raw = image.into_raw();
        let cols = w.min(self.bbox_w.saturating_sub(x0));
        let rows = h.min(self.bbox_h.saturating_sub(y0));
        for row in 0..rows {
            let src = row * w * 4;
            let dst = ((y0 + row) * self.bbox_w + x0) * 4;
            self.canvas[dst..dst + cols * 4].copy_from_slice(&raw[src..src + cols * 4]);
        }
        Ok(())
    }

    fn encode(&self) -> ResultType<Vec<u8>> {
        let mut png = Vec::new();
        repng::encode(&mut png, self.bbox_w as u32, self.bbox_h as u32, &self.canvas)?;
        Ok(png)
    }
}

#[cfg(not(any(target_os = "android", target_os = "ios", feature = "cli", feature = "flutter")))]
pub fn composite_sid(display: usize) -> String {
    format!("{COMPOSITE_SID_PREFIX}{display}")
}

#[cfg(not(any(target_os = "android", target_os = "ios")))]
pub fn begin_composite(bbox_w: usize, bbox_h: usize, slots: Vec<(usize, usize, usize)>) {
    let mut canvas = vec![0u8; bbox_w * bbox_h * 4];
    for px in canvas.chunks_exact_mut(4) {
        px[3] = 255;
    }
    *COMPOSITE.lock().unwrap() = Some(Composite {
        bbox_w,
        bbox_h,
        origins: slots.iter().map(|&(d, x, y)| (d, (x, y))).collect(),
        pending: slots.iter().map(|&(d, _, _)| d).collect(),
        canvas,
    });
}

#[cfg(not(any(target_os = "android", target_os = "ios")))]
fn set_composite_piece(display: usize, data: bytes::Bytes, msg: String) -> Option<String> {
    let mut composite_lock = COMPOSITE.lock().unwrap();
    let outcome = match composite_lock.as_mut() {
        None => return None,
        Some(_) if !msg.is_empty() => Err(msg),
        Some(composite) => match composite.paste(display, &data) {
            Err(e) => Err(e.to_string()),
            Ok(()) if !composite.pending.is_empty() => return None,
            Ok(()) => composite.encode().map_err(|e| e.to_string()),
        },
    };
    *composite_lock = None;
    match outcome {
        Ok(png) => {
            SCREENSHOT.lock().unwrap().set_screenshot(png.into());
            Some(String::new())
        }
        Err(e) => Some(e),
    }
}

pub enum ScreenshotAction {
    SaveAs(String),
    CopyToClipboard,
    Discard,
}

impl Default for ScreenshotAction {
    fn default() -> Self {
        Self::Discard
    }
}

impl From<&str> for ScreenshotAction {
    fn from(value: &str) -> Self {
        match value.chars().next() {
            Some('0') => {
                if let Some((pos, _)) = value.char_indices().nth(2) {
                    let substring = &value[pos..];
                    Self::SaveAs(substring.to_string())
                } else {
                    Self::default()
                }
            }
            Some('1') => Self::CopyToClipboard,
            Some('2') => Self::default(),
            _ => Self::default(),
        }
    }
}

impl Into<String> for ScreenshotAction {
    fn into(self) -> String {
        match self {
            Self::SaveAs(p) => format!("0:{p}"),
            Self::CopyToClipboard => "1".to_owned(),
            Self::Discard => "2".to_owned(),
        }
    }
}

#[derive(Default)]
pub struct Screenshot {
    data: Option<bytes::Bytes>,
}

impl Screenshot {
    fn set_screenshot(&mut self, data: bytes::Bytes) {
        self.data.replace(data);
    }

    fn handle_screenshot(&mut self, action: String) -> String {
        let Some(data) = self.data.take() else {
            return "No cached screenshot".to_owned();
        };
        match Self::handle_screenshot_(data, action) {
            Ok(()) => "".to_owned(),
            Err(e) => e.to_string(),
        }
    }

    fn handle_screenshot_(data: bytes::Bytes, action: String) -> ResultType<()> {
        match ScreenshotAction::from(&action as &str) {
            ScreenshotAction::SaveAs(p) => {
                std::fs::write(p, data)?;
            }
            ScreenshotAction::CopyToClipboard => {
                #[cfg(not(any(target_os = "android", target_os = "ios")))]
                {
                    // Decode PNG to RGBA so arboard sets both "image/png" and
                    // CF_DIBV5 on Windows.  Without CF_DIBV5 standard apps like
                    // Paint cannot paste the screenshot.
                    let (rgba_data, width, height) = match image::load_from_memory(&data) {
                        Ok(img) => {
                            let rgba = img.to_rgba8();
                            let w = rgba.width();
                            let h = rgba.height();
                            (rgba.into_raw().into(), w, h)
                        }
                        Err(_) => {
                            // Fallback: send raw PNG (clipboard-to-clipboard copy
                            // still works, just no CF_DIBV5 on Windows).
                            let clips = vec![Clipboard {
                                compress: false,
                                content: data,
                                format: ClipboardFormat::ImagePng.into(),
                                ..Default::default()
                            }];
                            update_clipboard(clips, ClipboardSide::Client);
                            return Ok(());
                        }
                    };
                    let clips = vec![Clipboard {
                        compress: false,
                        content: rgba_data,
                        width: width as _,
                        height: height as _,
                        format: ClipboardFormat::ImageRgba.into(),
                        ..Default::default()
                    }];
                    update_clipboard(clips, ClipboardSide::Client);
                }
            }
            ScreenshotAction::Discard => {}
        }
        Ok(())
    }
}

pub fn set_screenshot(sid: &str, data: bytes::Bytes, msg: String) -> Option<String> {
    let display = sid
        .strip_prefix(COMPOSITE_SID_PREFIX)
        .and_then(|d| d.parse::<usize>().ok());
    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    if let Some(display) = display {
        return set_composite_piece(display, data, msg);
    }
    let _ = display;
    SCREENSHOT.lock().unwrap().set_screenshot(data);
    Some(msg)
}

#[cfg(not(any(target_os = "android", target_os = "ios", feature = "cli", feature = "flutter")))]
pub fn take_png() -> Option<bytes::Bytes> {
    SCREENSHOT.lock().unwrap().data.take()
}

pub fn handle_screenshot(action: String) -> String {
    SCREENSHOT.lock().unwrap().handle_screenshot(action)
}
