//! IOSurface-backed `CVPixelBuffer` pools (macOS).
//!
//! Buffers are full-range NV12 (`420f`) and Metal-compatible: the format GPUI's
//! `surface()` draws directly and VideoToolbox encodes without a copy.

use std::ptr::{self, NonNull};

use objc2_core_foundation::{CFBoolean, CFDictionary, CFNumber, CFRetained, CFString, CFType};
use objc2_core_video::{
    CVPixelBuffer, CVPixelBufferPool, kCVPixelBufferHeightKey,
    kCVPixelBufferIOSurfacePropertiesKey, kCVPixelBufferMetalCompatibilityKey,
    kCVPixelBufferPixelFormatTypeKey, kCVPixelBufferWidthKey,
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
