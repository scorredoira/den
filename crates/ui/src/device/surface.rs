//! A frame of a device program: an IOSurface of this Mac, opened by its
//! number, as the pixel buffer GPUI's `surface` element paints (YCbCr 4:2:0,
//! which is the one format it takes).

use std::ffi::c_void;

use anyhow::{Result, bail};
use core_foundation::base::{CFRelease, TCFType};
use core_video::pixel_buffer::{CVPixelBuffer, CVPixelBufferRef, kCVPixelFormatType_420YpCbCr8BiPlanarFullRange};

#[link(name = "IOSurface", kind = "framework")]
unsafe extern "C" {
    fn IOSurfaceLookup(id: u32) -> *const c_void;
}

#[link(name = "CoreVideo", kind = "framework")]
unsafe extern "C" {
    fn CVPixelBufferCreateWithIOSurface(
        allocator: *const c_void,
        surface: *const c_void,
        attributes: *const c_void,
        out: *mut CVPixelBufferRef,
    ) -> i32;
}

/// The surface with number `id`, which the program made global.
pub fn open(id: u32) -> Result<CVPixelBuffer> {
    // SAFETY: IOSurfaceLookup returns a surface we own, or null; the buffer
    // made from it holds its own reference, so ours is released either way.
    unsafe {
        let surface = IOSurfaceLookup(id);
        if surface.is_null() {
            bail!("no surface {id}");
        }
        let mut buffer: CVPixelBufferRef = std::ptr::null_mut();
        let status = CVPixelBufferCreateWithIOSurface(std::ptr::null(), surface, std::ptr::null(), &mut buffer);
        CFRelease(surface);
        if status != 0 || buffer.is_null() {
            bail!("surface {id} isn't a pixel buffer: {status}");
        }
        let buffer = CVPixelBuffer::wrap_under_create_rule(buffer);
        if buffer.get_pixel_format() != kCVPixelFormatType_420YpCbCr8BiPlanarFullRange {
            bail!("surface {id} isn't YCbCr 4:2:0");
        }
        Ok(buffer)
    }
}
