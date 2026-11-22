//! macOS native PDF page inspection and rasterization via `CoreGraphics`.

use std::path::Path;

use crate::doc::DocInfo;
use crate::ppm::PageImage;

#[cfg(target_os = "macos")]
pub fn inspect(pdf: &Path) -> Option<DocInfo> {
    use std::os::unix::ffi::OsStrExt;

    let bytes = pdf.as_os_str().as_bytes();
    if bytes.is_empty() {
        return None;
    }
    // SAFETY: CoreFoundation / CoreGraphics pointers checked for null and released.
    unsafe {
        let doc = open_doc(bytes)?;
        let pages = ffi::CGPDFDocumentGetNumberOfPages(doc);
        let aspect = if pages > 0 {
            let page = ffi::CGPDFDocumentGetPage(doc, 1);
            if page.is_null() {
                1.414
            } else {
                let box_rect = ffi::CGPDFPageGetBoxRect(page, 0);
                if box_rect.size.width > 1.0 && box_rect.size.height > 1.0 {
                    (box_rect.size.height / box_rect.size.width) as f32
                } else {
                    1.414
                }
            }
        } else {
            1.414
        };
        ffi::CGPDFDocumentRelease(doc);
        (pages > 0).then_some(DocInfo {
            pages: pages as u32,
            aspect: aspect.clamp(0.25, 4.0),
        })
    }
}

#[cfg(target_os = "macos")]
pub fn render_page(pdf: &Path, page_num: u32, width_px: u32) -> Option<PageImage> {
    use std::os::unix::ffi::OsStrExt;

    let bytes = pdf.as_os_str().as_bytes();
    if bytes.is_empty() || page_num == 0 {
        return None;
    }
    // kCGImageAlphaNoneSkipLast (5) | kCGBitmapByteOrder32Big (4 << 12).
    const RGBX_OPAQUE: u32 = 5 | (4 << 12);

    // SAFETY: every CoreGraphics object created here is checked and released.
    unsafe {
        let doc = open_doc(bytes)?;
        let out = (|| {
            let page = ffi::CGPDFDocumentGetPage(doc, page_num as usize);
            if page.is_null() {
                return None;
            }
            let rect = ffi::CGPDFPageGetBoxRect(page, 0);
            if rect.size.width <= 1.0 || rect.size.height <= 1.0 {
                return None;
            }
            let w = width_px.clamp(64, 2048) as usize;
            let scale = w as f64 / rect.size.width;
            let h = ((rect.size.height * scale).round() as usize).clamp(1, 4096);
            let space = ffi::CGColorSpaceCreateDeviceRGB();
            if space.is_null() {
                return None;
            }
            let mut rgba = vec![255u8; w * h * 4];
            let ctx = ffi::CGBitmapContextCreate(
                rgba.as_mut_ptr().cast(),
                w,
                h,
                8,
                w * 4,
                space,
                RGBX_OPAQUE,
            );
            ffi::CGColorSpaceRelease(space);
            if ctx.is_null() {
                return None;
            }
            ffi::CGContextSetRGBFillColor(ctx, 1.0, 1.0, 1.0, 1.0);
            ffi::CGContextFillRect(
                ctx,
                ffi::CGRect {
                    origin: ffi::CGPoint { x: 0.0, y: 0.0 },
                    size: ffi::CGSize {
                        width: w as f64,
                        height: h as f64,
                    },
                },
            );
            ffi::CGContextTranslateCTM(ctx, -rect.origin.x * scale, -rect.origin.y * scale);
            ffi::CGContextScaleCTM(ctx, scale, scale);
            ffi::CGContextDrawPDFPage(ctx, page);
            ffi::CGContextRelease(ctx);
            for px in rgba.as_chunks_mut::<4>().0 {
                px[3] = 255;
            }
            Some(PageImage {
                width: w as u32,
                height: h as u32,
                rgba,
            })
        })();
        ffi::CGPDFDocumentRelease(doc);
        out
    }
}

#[cfg(target_os = "macos")]
unsafe fn open_doc(bytes: &[u8]) -> Option<*const std::ffi::c_void> {
    unsafe {
        let url = ffi::CFURLCreateFromFileSystemRepresentation(
            std::ptr::null(),
            bytes.as_ptr(),
            bytes.len() as isize,
            0,
        );
        if url.is_null() {
            return None;
        }
        let doc = ffi::CGPDFDocumentCreateWithURL(url);
        ffi::CFRelease(url);
        (!doc.is_null()).then_some(doc)
    }
}

#[cfg(target_os = "macos")]
mod ffi {
    use std::ffi::c_void;

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct CGPoint {
        pub x: f64,
        pub y: f64,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct CGSize {
        pub width: f64,
        pub height: f64,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct CGRect {
        pub origin: CGPoint,
        pub size: CGSize,
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        pub fn CFURLCreateFromFileSystemRepresentation(
            allocator: *const c_void,
            buffer: *const u8,
            buf_len: isize,
            is_directory: u8,
        ) -> *const c_void;
        pub fn CFRelease(cf: *const c_void);
    }

    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        pub fn CGPDFDocumentCreateWithURL(url: *const c_void) -> *const c_void;
        pub fn CGPDFDocumentRelease(doc: *const c_void);
        pub fn CGPDFDocumentGetNumberOfPages(doc: *const c_void) -> usize;
        pub fn CGPDFDocumentGetPage(doc: *const c_void, page_number: usize) -> *const c_void;
        pub fn CGPDFPageGetBoxRect(page: *const c_void, box_kind: i32) -> CGRect;
        pub fn CGColorSpaceCreateDeviceRGB() -> *const c_void;
        pub fn CGColorSpaceRelease(space: *const c_void);
        pub fn CGBitmapContextCreate(
            data: *mut c_void,
            width: usize,
            height: usize,
            bits_per_component: usize,
            bytes_per_row: usize,
            space: *const c_void,
            bitmap_info: u32,
        ) -> *const c_void;
        pub fn CGContextSetRGBFillColor(c: *const c_void, r: f64, g: f64, b: f64, a: f64);
        pub fn CGContextFillRect(c: *const c_void, rect: CGRect);
        pub fn CGContextTranslateCTM(c: *const c_void, tx: f64, ty: f64);
        pub fn CGContextScaleCTM(c: *const c_void, sx: f64, sy: f64);
        pub fn CGContextDrawPDFPage(c: *const c_void, page: *const c_void);
        pub fn CGContextRelease(c: *const c_void);
    }
}

#[cfg(not(target_os = "macos"))]
#[allow(dead_code)]
pub fn inspect(_: &Path) -> Option<DocInfo> {
    None
}

#[cfg(not(target_os = "macos"))]
#[allow(dead_code)]
pub fn render_page(_: &Path, _: u32, _: u32) -> Option<PageImage> {
    None
}
