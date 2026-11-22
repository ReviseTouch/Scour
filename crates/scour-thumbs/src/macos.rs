//! macOS Quick Look pictures of files: PDFs, videos, HEIC, Office, Keynote,
//! RAW, PSD, and anything a Quick Look generator handles. Asked directly from
//! `QuickLook.framework` into memory — no subprocess, no temp files.

use crate::shell::Picture;

/// Quick Look's thumbnail of `path`, at most `px` on its longer side.
#[cfg(target_os = "macos")]
pub fn picture(path: &std::path::Path, px: u32) -> Option<Picture> {
    use std::ffi::c_void;
    use std::os::unix::ffi::OsStrExt;

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct CGPoint {
        x: f64,
        y: f64,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct CGSize {
        width: f64,
        height: f64,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct CGRect {
        origin: CGPoint,
        size: CGSize,
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFURLCreateFromFileSystemRepresentation(
            allocator: *const c_void,
            buffer: *const u8,
            buf_len: isize,
            is_directory: u8,
        ) -> *const c_void;
        fn CFRelease(cf: *const c_void);
    }

    #[link(name = "QuickLook", kind = "framework")]
    unsafe extern "C" {
        fn QLThumbnailImageCreate(
            allocator: *const c_void,
            url: *const c_void,
            max_thumbnail_size: CGSize,
            options: *const c_void,
        ) -> *const c_void;
    }

    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        fn CGImageGetWidth(image: *const c_void) -> usize;
        fn CGImageGetHeight(image: *const c_void) -> usize;
        fn CGImageRelease(image: *const c_void);
        fn CGColorSpaceCreateDeviceRGB() -> *const c_void;
        fn CGColorSpaceRelease(space: *const c_void);
        fn CGBitmapContextCreate(
            data: *mut c_void,
            width: usize,
            height: usize,
            bits_per_component: usize,
            bytes_per_row: usize,
            space: *const c_void,
            bitmap_info: u32,
        ) -> *const c_void;
        fn CGContextDrawImage(ctx: *const c_void, rect: CGRect, image: *const c_void);
        fn CGContextRelease(ctx: *const c_void);
    }

    // kCGImageAlphaPremultipliedLast (1) | kCGBitmapByteOrder32Big (4 << 12).
    const RGBA_PREMUL: u32 = 1 | (4 << 12);

    let bytes = path.as_os_str().as_bytes();
    if bytes.is_empty() {
        return None;
    }
    let side = px.clamp(16, 1024) as f64;

    // SAFETY: every CoreFoundation / CoreGraphics object created here is
    // checked for null and released before returning.
    unsafe {
        let url = CFURLCreateFromFileSystemRepresentation(
            std::ptr::null(),
            bytes.as_ptr(),
            bytes.len() as isize,
            0,
        );
        if url.is_null() {
            return None;
        }
        let image = QLThumbnailImageCreate(
            std::ptr::null(),
            url,
            CGSize {
                width: side,
                height: side,
            },
            std::ptr::null(),
        );
        CFRelease(url);
        if image.is_null() {
            return None;
        }

        let w = CGImageGetWidth(image);
        let h = CGImageGetHeight(image);
        let out = if w == 0 || h == 0 || w > 4096 || h > 4096 {
            None
        } else {
            let space = CGColorSpaceCreateDeviceRGB();
            if space.is_null() {
                None
            } else {
                let mut pixels = vec![0u8; w * h * 4];
                let ctx = CGBitmapContextCreate(
                    pixels.as_mut_ptr().cast(),
                    w,
                    h,
                    8,
                    w * 4,
                    space,
                    RGBA_PREMUL,
                );
                CGColorSpaceRelease(space);
                if ctx.is_null() {
                    None
                } else {
                    let rect = CGRect {
                        origin: CGPoint { x: 0.0, y: 0.0 },
                        size: CGSize {
                            width: w as f64,
                            height: h as f64,
                        },
                    };
                    CGContextDrawImage(ctx, rect, image);
                    CGContextRelease(ctx);
                    crate::shell::unpremultiply_rgba(&mut pixels);
                    Some(Picture {
                        width: w as u32,
                        height: h as u32,
                        rgba: pixels,
                    })
                }
            }
        };
        CGImageRelease(image);
        out
    }
}

#[cfg(not(target_os = "macos"))]
#[allow(dead_code)]
pub fn picture(_: &std::path::Path, _: u32) -> Option<Picture> {
    None
}
