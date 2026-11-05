//! IOSurface-backed `CVPixelBuffer` pools (macOS).
//!
//! Buffers are full-range NV12 (`420f`) and Metal-compatible: the format GPUI's
//! `surface()` draws directly and VideoToolbox encodes without a copy.

use std::ptr::{self, NonNull};

use objc2_core_foundation::{CFBoolean, CFDictionary, CFNumber, CFRetained, CFString, CFType};
use objc2_core_video::{
    CVPixelBuffer, CVPixelBufferGetBaseAddressOfPlane, CVPixelBufferGetBytesPerRowOfPlane,
    CVPixelBufferGetHeight, CVPixelBufferGetWidth, CVPixelBufferLockBaseAddress,
    CVPixelBufferLockFlags, CVPixelBufferPool, CVPixelBufferUnlockBaseAddress,
    kCVPixelBufferHeightKey, kCVPixelBufferIOSurfacePropertiesKey,
    kCVPixelBufferMetalCompatibilityKey, kCVPixelBufferPixelFormatTypeKey, kCVPixelBufferWidthKey,
    kCVPixelFormatType_420YpCbCr8BiPlanarFullRange, kCVReturnSuccess,
};

pub struct PixelBufferPool {
    pool: CFRetained<CVPixelBufferPool>,
    pub width: u32,
    pub height: u32,
}

// SAFETY: CoreVideo pools may be used from any thread.
unsafe impl Send for PixelBufferPool {}

impl PixelBufferPool {
    pub fn new(width: u32, height: u32) -> Option<Self> {
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

    /// A buffer from the pool; it returns to the pool when the last reference drops.
    pub fn take(&self) -> Option<CFRetained<CVPixelBuffer>> {
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

/// Test pattern for the synthetic camera and screen: horizontal bands that
/// scroll every frame (`n`), so the encoder has real motion, tinted around the
/// `[cb, cr]` chroma pair. Painted with row fills, so generating costs next to
/// nothing and measurements show the pipeline, not the pattern.
pub(crate) fn paint_test_pattern(pixel_buffer: &CVPixelBuffer, n: usize, chroma: [u8; 2]) {
    let (w, h) = (
        CVPixelBufferGetWidth(pixel_buffer),
        CVPixelBufferGetHeight(pixel_buffer),
    );
    // SAFETY: the buffer comes from a 420f pool (see `PixelBufferPool`): plane
    // 0 has `h` rows of at least `w` bytes, plane 1 has `h / 2` rows of at
    // least `w` bytes (`w / 2` CbCr pairs).
    unsafe {
        if CVPixelBufferLockBaseAddress(pixel_buffer, CVPixelBufferLockFlags(0)) != kCVReturnSuccess
        {
            return;
        }
        let y = CVPixelBufferGetBaseAddressOfPlane(pixel_buffer, 0).cast::<u8>();
        let y_stride = CVPixelBufferGetBytesPerRowOfPlane(pixel_buffer, 0);
        let uv = CVPixelBufferGetBaseAddressOfPlane(pixel_buffer, 1).cast::<u8>();
        let uv_stride = CVPixelBufferGetBytesPerRowOfPlane(pixel_buffer, 1);
        for row in 0..h {
            let luma = ((row + n * 4) / 3) as u8;
            std::ptr::write_bytes(y.add(row * y_stride), luma, w);
        }
        let [cb, cr] = chroma;
        for row in 0..h / 2 {
            let line = std::slice::from_raw_parts_mut(uv.add(row * uv_stride), w & !1);
            let (pairs, _) = line.as_chunks_mut::<2>();
            let tint = ((row / 45 + n / 30) % 4) as u8 * 24;
            pairs.fill([cb.saturating_add(tint), cr.saturating_sub(tint)]);
        }
        CVPixelBufferUnlockBaseAddress(pixel_buffer, CVPixelBufferLockFlags(0));
    }
}
