//! One still picture of a capture target, through `SCScreenshotManager`.
//!
//! A stream's frames go to the encoder as they come; a prompt to an agent wants the display as
//! it is *now*, at native resolution, as plain bytes. ScreenCaptureKit takes such a picture
//! from the same content filter a stream would use, on its own queue, and hands back a
//! `CGImage` in BGRA (SDR) whose bytes are copied out here.

use std::ptr::NonNull;

use block2::RcBlock;
use objc2_core_foundation::CFRetained;
use objc2_core_graphics::{CGDataProvider, CGImage, CGImageByteOrderInfo};
use objc2_core_video::kCVPixelFormatType_32BGRA;
use objc2_foundation::NSError;
use objc2_screen_capture_kit::{SCScreenshotManager, SCStreamConfiguration};
use parking_lot::Mutex;

use crate::CaptureError;
use crate::stream::Target;

/// The order of the four bytes of a pixel in a [`Picture`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PixelOrder {
    /// Blue, green, red, alpha (little-endian `ARGB`, what ScreenCaptureKit gives for SDR).
    Bgra,
    /// Alpha, red, green, blue (big-endian `ARGB`).
    Argb,
}

/// A still picture, 32 bits per pixel, rows possibly padded.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Picture {
    /// Pixels across.
    pub width: u32,
    /// Pixels down.
    pub height: u32,
    /// Bytes from one row to the next (at least `width * 4`).
    pub bytes_per_row: usize,
    /// The byte order within a pixel.
    pub order: PixelOrder,
    /// `height * bytes_per_row` bytes.
    pub data: Vec<u8>,
}

impl Target {
    /// Take one picture of the target at its native pixel size, without the cursor; `done`
    /// runs on ScreenCaptureKit's queue with the picture or the error.
    pub fn snapshot(&self, done: impl FnOnce(Result<Picture, CaptureError>) + Send + 'static) {
        crate::ensure_core_graphics();
        let (width, height) = self.pixel_size();
        // SAFETY: plain constructor.
        let config = unsafe { SCStreamConfiguration::new() };
        // SAFETY: plain property write on the fresh configuration object.
        unsafe {
            config.setWidth(usize::try_from(width).unwrap_or(usize::MAX));
        }
        // SAFETY: plain property write on the fresh configuration object.
        unsafe {
            config.setHeight(usize::try_from(height).unwrap_or(usize::MAX));
        }
        // SAFETY: plain property write on the fresh configuration object.
        unsafe {
            config.setPixelFormat(kCVPixelFormatType_32BGRA);
        }
        // SAFETY: plain property write on the fresh configuration object.
        unsafe {
            config.setShowsCursor(false);
        }
        // SAFETY: plain property write on the fresh configuration object.
        unsafe {
            config.setScalesToFit(true);
        }
        // SAFETY: plain property write on the fresh configuration object.
        unsafe {
            config.setPreservesAspectRatio(true);
        }
        let done = Mutex::new(Some(done));
        let block = RcBlock::new(move |image: *mut CGImage, error: *mut NSError| {
            let Some(done) = done.lock().take() else {
                return;
            };
            let result = match (NonNull::new(image), NonNull::new(error)) {
                (Some(image), _) => {
                    // SAFETY: ScreenCaptureKit passes a live image it keeps for the callback's
                    // duration; retaining gives this code its own reference to read from.
                    let image: CFRetained<CGImage> = unsafe { CFRetained::retain(image) };
                    copy_out(&image)
                }
                // SAFETY: a live error object for the callback's duration.
                (None, Some(error)) => Err(CaptureError::from_ns(unsafe { error.as_ref() })),
                (None, None) => Err(CaptureError::Stopped("no picture, no error".to_owned())),
            };
            done(result);
        });
        // SAFETY: the block is copied by the framework before this returns and captures only
        // `Send` data; the filter and configuration are live objects for the call.
        unsafe {
            SCScreenshotManager::captureImageWithFilter_configuration_completionHandler(
                self.filter(),
                &config,
                Some(&block),
            );
        }
    }
}

/// The image's bytes and layout. ScreenCaptureKit promises 32 bits per pixel here; anything
/// else is reported rather than misread.
fn copy_out(image: &CGImage) -> Result<Picture, CaptureError> {
    let some = Some(image);
    let bits = CGImage::bits_per_pixel(some);
    if bits != 32 {
        return Err(CaptureError::Stopped(format!(
            "picture has {bits} bits per pixel"
        )));
    }
    let width = u32::try_from(CGImage::width(some)).unwrap_or(u32::MAX);
    let height = u32::try_from(CGImage::height(some)).unwrap_or(u32::MAX);
    let bytes_per_row = CGImage::bytes_per_row(some);
    let order = if CGImage::byte_order_info(some) == CGImageByteOrderInfo::Order32Little {
        PixelOrder::Bgra
    } else {
        PixelOrder::Argb
    };
    let provider = CGImage::data_provider(some)
        .ok_or_else(|| CaptureError::Stopped("picture has no data provider".to_owned()))?;
    let data = CGDataProvider::data(Some(&provider))
        .ok_or_else(|| CaptureError::Stopped("picture data could not be copied".to_owned()))?
        .to_vec();
    let expected = bytes_per_row.saturating_mul(height as usize);
    if data.len() < expected {
        return Err(CaptureError::Stopped(format!(
            "picture holds {} bytes for {expected}",
            data.len()
        )));
    }
    Ok(Picture {
        width,
        height,
        bytes_per_row,
        order,
        data,
    })
}
