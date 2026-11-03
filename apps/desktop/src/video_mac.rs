//! Remote video → GPUI `surface()` on macOS.
//!
//! GPUI's Metal renderer samples `CVPixelBuffer`s directly, but only full-range
//! NV12 (`420f`). Decoded WebRTC frames are limited-range I420, so each frame is
//! converted into a pooled, IOSurface-backed buffer: Y gets a range-expansion
//! lookup, U/V get interleaved and expanded. One pass, no RGB step.
//!
//! TODO(stage 1): hardware-decoded frames already arrive as `CVPixelBuffer`s;
//! convert those with VTPixelTransferSession on the GPU instead.

use std::ptr::{self, NonNull};
use std::sync::atomic::{AtomicU64, Ordering};

use gpui::SharedString;
use objc2_core_foundation::{CFBoolean, CFDictionary, CFNumber, CFRetained, CFString, CFType};
use objc2_core_video::{
    CVPixelBuffer, CVPixelBufferGetBaseAddressOfPlane, CVPixelBufferGetBytesPerRowOfPlane,
    CVPixelBufferLockBaseAddress, CVPixelBufferLockFlags, CVPixelBufferPool,
    CVPixelBufferUnlockBaseAddress, kCVPixelBufferHeightKey, kCVPixelBufferIOSurfacePropertiesKey,
    kCVPixelBufferMetalCompatibilityKey, kCVPixelBufferPixelFormatTypeKey, kCVPixelBufferWidthKey,
    kCVPixelFormatType_420YpCbCr8BiPlanarFullRange, kCVReturnSuccess,
};
use parking_lot::Mutex;
use quipcall_core::FrameSink;
use quipcall_core::livekit::webrtc::video_frame::{BoxVideoBuffer, VideoBuffer, VideoFrame};

/// A frame ready to hand to `gpui::surface`.
#[derive(Clone)]
pub struct Frame(pub CFRetained<CVPixelBuffer>);

// SAFETY: CVPixelBuffers are reference-counted CF objects that may be retained
// and released from any thread. We never write to one after publishing it.
unsafe impl Send for Frame {}
unsafe impl Sync for Frame {}

pub struct SurfaceSink {
    label: SharedString,
    latest: Mutex<Option<Frame>>,
    pool: Mutex<Option<Pool>>,
    /// Coalescing "please repaint" signal (bounded(1); extra sends are dropped).
    redraw: async_channel::Sender<()>,
    frames: AtomicU64,
}

impl SurfaceSink {
    pub fn new(label: SharedString, redraw: async_channel::Sender<()>) -> Self {
        Self {
            label,
            latest: Mutex::new(None),
            pool: Mutex::new(None),
            redraw,
            frames: AtomicU64::new(0),
        }
    }

    pub fn latest(&self) -> Option<Frame> {
        self.latest.lock().clone()
    }
}

impl FrameSink for SurfaceSink {
    fn on_frame(&self, frame: &VideoFrame<BoxVideoBuffer>) {
        let i420 = frame.buffer.to_i420();
        let (width, height) = (i420.width(), i420.height());
        if width == 0 || height == 0 {
            return;
        }

        let pixel_buffer = {
            let mut pool = self.pool.lock();
            if pool
                .as_ref()
                .is_none_or(|p| p.width != width || p.height != height)
            {
                tracing::debug!(track = %self.label, width, height, "remote video size");
                *pool = Pool::new(width, height);
            }
            let Some(buffer) = pool.as_ref().and_then(Pool::take) else {
                return;
            };
            buffer
        };

        let (sy, su, sv) = i420.strides();
        let (y, u, v) = i420.data();
        // SAFETY: the buffer is freshly taken from a pool configured for
        // `width`x`height` 420f, so plane 0 is width x height bytes and plane 1
        // is (width/2 rounded up) x (height/2 rounded up) UV pairs.
        unsafe {
            if CVPixelBufferLockBaseAddress(&pixel_buffer, CVPixelBufferLockFlags(0))
                != kCVReturnSuccess
            {
                return;
            }
            let dst_y = CVPixelBufferGetBaseAddressOfPlane(&pixel_buffer, 0) as *mut u8;
            let dst_y_stride = CVPixelBufferGetBytesPerRowOfPlane(&pixel_buffer, 0);
            let dst_uv = CVPixelBufferGetBaseAddressOfPlane(&pixel_buffer, 1) as *mut u8;
            let dst_uv_stride = CVPixelBufferGetBytesPerRowOfPlane(&pixel_buffer, 1);

            let (w, h) = (width as usize, height as usize);
            for row in 0..h {
                let src = &y[row * sy as usize..][..w];
                let dst = std::slice::from_raw_parts_mut(dst_y.add(row * dst_y_stride), w);
                for (d, s) in dst.iter_mut().zip(src) {
                    *d = LUMA[*s as usize];
                }
            }
            let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
            for row in 0..ch {
                let su_row = &u[row * su as usize..][..cw];
                let sv_row = &v[row * sv as usize..][..cw];
                let dst = std::slice::from_raw_parts_mut(dst_uv.add(row * dst_uv_stride), cw * 2);
                let (pairs, _) = dst.as_chunks_mut::<2>();
                for ((pair, &cb), &cr) in pairs.iter_mut().zip(su_row).zip(sv_row) {
                    *pair = [CHROMA[cb as usize], CHROMA[cr as usize]];
                }
            }
            CVPixelBufferUnlockBaseAddress(&pixel_buffer, CVPixelBufferLockFlags(0));
        }

        *self.latest.lock() = Some(Frame(pixel_buffer));
        if self.frames.fetch_add(1, Ordering::Relaxed) == 0 {
            tracing::info!(track = %self.label, width, height, "first remote frame");
        }
        let _ = self.redraw.try_send(());
    }
}

struct Pool {
    pool: CFRetained<CVPixelBufferPool>,
    width: u32,
    height: u32,
}

// SAFETY: CVPixelBufferPool is thread-safe; access is serialized by a Mutex anyway.
unsafe impl Send for Pool {}

impl Pool {
    fn new(width: u32, height: u32) -> Option<Self> {
        // SAFETY: CoreVideo constant keys are valid static CFStrings.
        let (k_format, k_width, k_height, k_iosurface, k_metal) = unsafe {
            (
                kCVPixelBufferPixelFormatTypeKey,
                kCVPixelBufferWidthKey,
                kCVPixelBufferHeightKey,
                kCVPixelBufferIOSurfacePropertiesKey,
                kCVPixelBufferMetalCompatibilityKey,
            )
        };
        let format = CFNumber::new_i32(kCVPixelFormatType_420YpCbCr8BiPlanarFullRange as i32);
        let w = CFNumber::new_i32(width as i32);
        let h = CFNumber::new_i32(height as i32);
        let iosurface = CFDictionary::<CFString, CFType>::empty();
        let keys: [&CFString; 5] = [k_format, k_width, k_height, k_iosurface, k_metal];
        let values: [&CFType; 5] = [
            format.as_ref(),
            w.as_ref(),
            h.as_ref(),
            iosurface.as_ref(),
            CFBoolean::new(true).as_ref(),
        ];
        let attrs = CFDictionary::<CFString, CFType>::from_slices(&keys, &values);

        let mut out: *mut CVPixelBufferPool = ptr::null_mut();
        // SAFETY: valid attribute dictionary and out-pointer.
        let status = unsafe {
            CVPixelBufferPool::create(None, None, Some(attrs.as_opaque()), NonNull::from(&mut out))
        };
        if status != kCVReturnSuccess || out.is_null() {
            tracing::error!(status, "CVPixelBufferPoolCreate failed");
            return None;
        }
        // SAFETY: a successful Create call transfers ownership of a non-null pool.
        let pool = unsafe { CFRetained::from_raw(NonNull::new_unchecked(out)) };
        Some(Self {
            pool,
            width,
            height,
        })
    }

    fn take(&self) -> Option<CFRetained<CVPixelBuffer>> {
        let mut out: *mut CVPixelBuffer = ptr::null_mut();
        // SAFETY: valid pool and out-pointer.
        let status = unsafe {
            CVPixelBufferPool::create_pixel_buffer(None, &self.pool, NonNull::from(&mut out))
        };
        if status != kCVReturnSuccess || out.is_null() {
            return None;
        }
        // SAFETY: a successful Create call transfers ownership.
        Some(unsafe { CFRetained::from_raw(NonNull::new_unchecked(out)) })
    }
}

/// Limited-range (16..235) → full-range (0..255) luma.
static LUMA: [u8; 256] = range_lut(16, 219);
/// Limited-range (16..240, centered at 128) → full-range chroma.
static CHROMA: [u8; 256] = chroma_lut();

const fn range_lut(offset: i32, span: i32) -> [u8; 256] {
    let mut lut = [0u8; 256];
    let mut i = 0;
    while i < 256 {
        let v = ((i as i32 - offset) * 255 + span / 2) / span;
        lut[i] = if v < 0 {
            0
        } else if v > 255 {
            255
        } else {
            v as u8
        };
        i += 1;
    }
    lut
}

const fn chroma_lut() -> [u8; 256] {
    let mut lut = [0u8; 256];
    let mut i = 0;
    while i < 256 {
        let v = 128 + ((i as i32 - 128) * 255 + 112) / 224;
        lut[i] = if v < 0 {
            0
        } else if v > 255 {
            255
        } else {
            v as u8
        };
        i += 1;
    }
    lut
}
