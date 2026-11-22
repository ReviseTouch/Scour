//! Native OS shell file icons for Windows (`SHGetFileInfoW`) and macOS
//! (`NSWorkspace`). Neither touches the indexed files on disk.

#[cfg(any(windows, target_os = "macos"))]
use crate::shell::Picture;

#[cfg(windows)]
pub(crate) fn resolve(key: &str, px: u32) -> Option<Picture> {
    use std::os::windows::ffi::OsStrExt;
    use windows::Win32::Graphics::Gdi::{
        BI_RGB, BITMAP, BITMAPINFO, BITMAPINFOHEADER, CreateCompatibleDC, DIB_RGB_COLORS, DeleteDC,
        DeleteObject, GetDIBits, GetObjectW,
    };
    use windows::Win32::Storage::FileSystem::{
        FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_NORMAL, FILE_FLAGS_AND_ATTRIBUTES,
    };
    use windows::Win32::System::Com::CoTaskMemFree;
    use windows::Win32::UI::Shell::{
        FOLDERID_Desktop, FOLDERID_Documents, FOLDERID_Downloads, FOLDERID_Music,
        FOLDERID_Pictures, FOLDERID_Profile, FOLDERID_Public, FOLDERID_Templates, FOLDERID_Videos,
        KF_FLAG_DEFAULT, SHFILEINFOW, SHGFI_ICON, SHGFI_LARGEICON, SHGFI_PIDL, SHGFI_SMALLICON,
        SHGFI_USEFILEATTRIBUTES, SHGetFileInfoW, SHGetKnownFolderIDList,
    };
    use windows::Win32::UI::WindowsAndMessaging::{DestroyIcon, GetIconInfo, HICON, ICONINFO};
    use windows::core::{GUID, PCWSTR};

    let size_flag = if px <= 24 {
        SHGFI_SMALLICON
    } else {
        SHGFI_LARGEICON
    };

    let known_folder: Option<&GUID> = match key.strip_prefix("place:") {
        Some("home") => Some(&FOLDERID_Profile),
        Some("desktop") => Some(&FOLDERID_Desktop),
        Some("documents") => Some(&FOLDERID_Documents),
        Some("downloads") => Some(&FOLDERID_Downloads),
        Some("music") => Some(&FOLDERID_Music),
        Some("pictures") => Some(&FOLDERID_Pictures),
        Some("videos") => Some(&FOLDERID_Videos),
        Some("public") => Some(&FOLDERID_Public),
        Some("templates") => Some(&FOLDERID_Templates),
        _ => None,
    };

    // SAFETY: Win32 Shell and GDI handles (`pidl`, `hIcon`, `hbmColor`, `hbmMask`, `dc`)
    // are checked and freed on every exit path.
    unsafe {
        let mut shfi = SHFILEINFOW::default();
        let mut got = 0usize;
        if let Some(fid) = known_folder
            && let Ok(pidl) = SHGetKnownFolderIDList(fid, KF_FLAG_DEFAULT.0 as u32, None)
        {
            got = SHGetFileInfoW(
                PCWSTR(pidl.cast()),
                FILE_FLAGS_AND_ATTRIBUTES(0),
                Some(&mut shfi),
                std::mem::size_of::<SHFILEINFOW>() as u32,
                SHGFI_PIDL | SHGFI_ICON | size_flag,
            );
            CoTaskMemFree(Some(pidl.cast()));
        }
        if got == 0 {
            let (dummy, attrs) = if key == "folder" || key.starts_with("place:") {
                ("folder".to_owned(), FILE_ATTRIBUTE_DIRECTORY)
            } else if let Some(ext) = key.strip_prefix("ext:") {
                (format!("item.{ext}"), FILE_ATTRIBUTE_NORMAL)
            } else if key == "kind:exec" {
                ("item.exe".to_owned(), FILE_ATTRIBUTE_NORMAL)
            } else {
                ("item".to_owned(), FILE_ATTRIBUTE_NORMAL)
            };
            let wide: Vec<u16> = std::ffi::OsStr::new(&dummy)
                .encode_wide()
                .chain([0])
                .collect();
            got = SHGetFileInfoW(
                PCWSTR(wide.as_ptr()),
                attrs,
                Some(&mut shfi),
                std::mem::size_of::<SHFILEINFOW>() as u32,
                SHGFI_USEFILEATTRIBUTES | SHGFI_ICON | size_flag,
            );
        }
        if got == 0 || shfi.hIcon.is_invalid() {
            return None;
        }
        let pic = extract_hicon(shfi.hIcon);
        let _ = DestroyIcon(shfi.hIcon);
        return pic;
    }

    unsafe fn extract_hicon(hicon: HICON) -> Option<Picture> {
        let mut ii = ICONINFO::default();
        if unsafe { GetIconInfo(hicon, &mut ii) }.is_err() {
            return None;
        }
        let color_bmp = ii.hbmColor;
        let mask_bmp = ii.hbmMask;
        let out = (|| unsafe {
            if color_bmp.is_invalid() {
                return None;
            }
            let mut bm = BITMAP::default();
            let read = GetObjectW(
                color_bmp.into(),
                std::mem::size_of::<BITMAP>() as i32,
                Some(&mut bm as *mut BITMAP as *mut _),
            );
            let (w, h) = (bm.bmWidth, bm.bmHeight.abs());
            if read == 0 || w <= 0 || h <= 0 || w > 512 || h > 512 {
                return None;
            }
            let mut header = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: w,
                    biHeight: -h,
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut rgba = vec![0u8; (w * h * 4) as usize];
            let dc = CreateCompatibleDC(None);
            let lines = GetDIBits(
                dc,
                color_bmp,
                0,
                h as u32,
                Some(rgba.as_mut_ptr().cast()),
                &mut header,
                DIB_RGB_COLORS,
            );
            let has_alpha = rgba.as_chunks::<4>().0.iter().any(|p| p[3] != 0);
            if lines == h && !has_alpha && !mask_bmp.is_invalid() {
                let mut mask = vec![0u8; (w * h * 4) as usize];
                if GetDIBits(
                    dc,
                    mask_bmp,
                    0,
                    h as u32,
                    Some(mask.as_mut_ptr().cast()),
                    &mut header,
                    DIB_RGB_COLORS,
                ) == h
                {
                    for (px, m) in rgba
                        .as_chunks_mut::<4>()
                        .0
                        .iter_mut()
                        .zip(mask.as_chunks::<4>().0)
                    {
                        px[3] = if m[0] == 0 { 255 } else { 0 };
                    }
                }
            }
            let _ = DeleteDC(dc);
            if lines != h {
                return None;
            }
            for p in rgba.as_chunks_mut::<4>().0 {
                p.swap(0, 2);
            }
            Some(Picture {
                width: w as u32,
                height: h as u32,
                rgba,
            })
        })();
        unsafe {
            if !color_bmp.is_invalid() {
                let _ = DeleteObject(color_bmp.into());
            }
            if !mask_bmp.is_invalid() {
                let _ = DeleteObject(mask_bmp.into());
            }
        }
        out
    }
}

#[cfg(target_os = "macos")]
pub(crate) fn resolve(key: &str, px: u32) -> Option<Picture> {
    use std::ffi::c_void;

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
        fn CFStringCreateWithBytes(
            alloc: *const c_void,
            bytes: *const u8,
            num_bytes: isize,
            encoding: u32,
            is_external: u8,
        ) -> *const c_void;
        fn CFRelease(cf: *const c_void);
    }

    #[link(name = "AppKit", kind = "framework")]
    unsafe extern "C" {}

    #[link(name = "objc")]
    unsafe extern "C" {
        fn objc_autoreleasePoolPush() -> *mut c_void;
        fn objc_autoreleasePoolPop(pool: *mut c_void);
        fn objc_getClass(name: *const std::ffi::c_char) -> *const c_void;
        fn sel_registerName(name: *const std::ffi::c_char) -> *const c_void;
        fn objc_msgSend();
    }

    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
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

    const UTF8_ENCODING: u32 = 0x0800_0100;
    const RGBA_PREMUL: u32 = 1 | (4 << 12);

    let side = px.clamp(16, 128) as usize;
    let special_path = key.strip_prefix("place:").and_then(|role| {
        let home = std::env::var("HOME").ok()?;
        let sub = match role {
            "home" => return Some(home),
            "desktop" => "Desktop",
            "documents" => "Documents",
            "downloads" => "Downloads",
            "music" => "Music",
            "pictures" => "Pictures",
            "videos" => "Movies",
            "public" => "Public",
            _ => return None,
        };
        let p = format!("{home}/{sub}");
        std::path::Path::new(&p).is_dir().then_some(p)
    });
    let file_type = if key == "folder" || key.starts_with("place:") {
        "public.folder"
    } else if let Some(ext) = key.strip_prefix("ext:") {
        ext
    } else if key == "kind:exec" {
        "public.unix-executable"
    } else {
        "public.data"
    };

    // SAFETY: Objective-C messages use exact C ABI function pointer casts with
    // pointer-only arguments inside an autorelease pool; CF/CG handles are freed.
    unsafe {
        let pool = objc_autoreleasePoolPush();
        let out = (|| {
            let ws_cls = objc_getClass(c"NSWorkspace".as_ptr());
            if ws_cls.is_null() {
                return None;
            }
            let msg0: unsafe extern "C" fn(*const c_void, *const c_void) -> *const c_void =
                std::mem::transmute(objc_msgSend as *const c_void);
            let msg1: unsafe extern "C" fn(
                *const c_void,
                *const c_void,
                *const c_void,
            ) -> *const c_void = std::mem::transmute(objc_msgSend as *const c_void);
            let msg_cg: unsafe extern "C" fn(
                *const c_void,
                *const c_void,
                *mut CGRect,
                *const c_void,
                *const c_void,
            ) -> *const c_void = std::mem::transmute(objc_msgSend as *const c_void);

            let ws = msg0(ws_cls, sel_registerName(c"sharedWorkspace".as_ptr()));
            if ws.is_null() {
                return None;
            }
            let (sel, arg_str) = match &special_path {
                Some(path) => (c"iconForFile:", path.as_str()),
                None => (c"iconForFileType:", file_type),
            };
            let cf_arg = CFStringCreateWithBytes(
                std::ptr::null(),
                arg_str.as_ptr(),
                arg_str.len() as isize,
                UTF8_ENCODING,
                0,
            );
            if cf_arg.is_null() {
                return None;
            }
            let ns_img = msg1(ws, sel_registerName(sel.as_ptr()), cf_arg);
            CFRelease(cf_arg);
            if ns_img.is_null() {
                return None;
            }
            let mut proposed = CGRect {
                origin: CGPoint { x: 0.0, y: 0.0 },
                size: CGSize {
                    width: side as f64,
                    height: side as f64,
                },
            };
            let cg_img = msg_cg(
                ns_img,
                sel_registerName(c"CGImageForProposedRect:context:hints:".as_ptr()),
                &mut proposed,
                std::ptr::null(),
                std::ptr::null(),
            );
            if cg_img.is_null() {
                return None;
            }
            let space = CGColorSpaceCreateDeviceRGB();
            if space.is_null() {
                return None;
            }
            let mut rgba = vec![0u8; side * side * 4];
            let ctx = CGBitmapContextCreate(
                rgba.as_mut_ptr().cast(),
                side,
                side,
                8,
                side * 4,
                space,
                RGBA_PREMUL,
            );
            CGColorSpaceRelease(space);
            if ctx.is_null() {
                return None;
            }
            let draw_rect = CGRect {
                origin: CGPoint { x: 0.0, y: 0.0 },
                size: CGSize {
                    width: side as f64,
                    height: side as f64,
                },
            };
            CGContextDrawImage(ctx, draw_rect, cg_img);
            CGContextRelease(ctx);
            crate::shell::unpremultiply_rgba(&mut rgba);
            Some(Picture {
                width: side as u32,
                height: side as u32,
                rgba,
            })
        })();
        objc_autoreleasePoolPop(pool);
        out
    }
}
