//! Windows' own pictures of files: what Explorer draws for a photograph, a PDF,
//! a video or anything a program installed a thumbnail handler for. Asked of
//! the shell through `IShellItemImageFactory`, which reads its cache or runs the
//! handler. Slow for a file it has not seen — a video is a decode — so a caller
//! asks off the drawing thread.

/// A picture as straight RGBA, rows top to bottom.
#[derive(Debug)]
pub struct Picture {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// The shell's thumbnail of `path`, at most `px` on its longer side. None when
/// the shell has only an icon for it: an icon says less than the text preview.
#[cfg(windows)]
pub fn picture(path: &std::path::Path, px: u32) -> Option<Picture> {
    use std::os::windows::ffi::OsStrExt;
    use windows::Win32::Foundation::SIZE;
    use windows::Win32::Graphics::Gdi::{
        BI_RGB, BITMAP, BITMAPINFO, BITMAPINFOHEADER, CreateCompatibleDC, DIB_RGB_COLORS, DeleteDC,
        DeleteObject, GetDIBits, GetObjectW,
    };
    use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx, CoUninitialize};
    use windows::Win32::UI::Shell::{
        IShellItemImageFactory, SHCreateItemFromParsingName, SIIGBF_RESIZETOFIT,
        SIIGBF_THUMBNAILONLY,
    };
    use windows::core::PCWSTR;

    let wide: Vec<u16> = path.as_os_str().encode_wide().chain([0]).collect();
    let px = px.clamp(16, 1024) as i32;
    // SAFETY: COM on this thread for the length of the call, a null-terminated
    // path, and every handle released before returning.
    unsafe {
        let com = CoInitializeEx(None, COINIT_APARTMENTTHREADED).is_ok();
        let out = (|| {
            let factory: IShellItemImageFactory =
                SHCreateItemFromParsingName(PCWSTR(wide.as_ptr()), None).ok()?;
            let bitmap = factory
                .GetImage(
                    SIZE { cx: px, cy: px },
                    SIIGBF_RESIZETOFIT | SIIGBF_THUMBNAILONLY,
                )
                .ok()?;
            let mut info = BITMAP::default();
            let read = GetObjectW(
                bitmap.into(),
                std::mem::size_of::<BITMAP>() as i32,
                Some(&mut info as *mut BITMAP as *mut _),
            );
            let (w, h) = (info.bmWidth, info.bmHeight.abs());
            let mut pixels = vec![0u8; (w.max(0) * h.max(0) * 4) as usize];
            let mut header = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: w,
                    // Negative: rows top to bottom.
                    biHeight: -h,
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            let dc = CreateCompatibleDC(None);
            let lines = if read == 0 || w <= 0 || h <= 0 {
                0
            } else {
                GetDIBits(
                    dc,
                    bitmap,
                    0,
                    h as u32,
                    Some(pixels.as_mut_ptr() as *mut _),
                    &mut header,
                    DIB_RGB_COLORS,
                )
            };
            let _ = DeleteDC(dc);
            let _ = DeleteObject(bitmap.into());
            if lines != h {
                return None;
            }
            straighten(&mut pixels);
            Some(Picture {
                width: w as u32,
                height: h as u32,
                rgba: pixels,
            })
        })();
        if com {
            CoUninitialize();
        }
        out
    }
}

#[cfg(not(windows))]
pub fn picture(_: &std::path::Path, _: u32) -> Option<Picture> {
    None
}

/// BGRA with premultiplied alpha, as the shell hands it, to straight RGBA.
/// A bitmap whose alpha is zero throughout has none: it is opaque.
#[cfg_attr(not(windows), allow(dead_code))]
fn straighten(px: &mut [u8]) {
    let opaque = px.as_chunks::<4>().0.iter().all(|p| p[3] == 0);
    for p in px.as_chunks_mut::<4>().0 {
        p.swap(0, 2);
        if opaque {
            p[3] = 255;
        } else if p[3] != 0 && p[3] != 255 {
            let a = p[3] as u32;
            for c in &mut p[..3] {
                *c = ((*c as u32 * 255 + a / 2) / a).min(255) as u8;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_shells_pixels_become_straight_rgba() {
        // No alpha anywhere: opaque, channels swapped.
        let mut px = vec![10, 20, 30, 0, 40, 50, 60, 0];
        super::straighten(&mut px);
        assert_eq!(px, [30, 20, 10, 255, 60, 50, 40, 255]);
        // Half-transparent red, premultiplied, comes back full red.
        let mut px = vec![0, 0, 128, 128, 0, 0, 0, 255];
        super::straighten(&mut px);
        assert_eq!(&px[..4], &[255, 0, 0, 128]);
    }
}
