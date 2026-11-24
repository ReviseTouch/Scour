use std::os::windows::ffi::OsStrExt;
use std::path::Path;

use windows_sys::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx, CoUninitialize};
use windows_sys::Win32::UI::Shell::Common::ITEMIDLIST;
use windows_sys::Win32::UI::Shell::{
    ILFree, SHOpenFolderAndSelectItems, SHParseDisplayName, ShellExecuteW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

fn wide(s: &std::ffi::OsStr) -> Vec<u16> {
    s.encode_wide().chain(std::iter::once(0)).collect()
}

/// COM for the length of `f`, on this thread; left alone where the thread
/// had it already.
fn with_com<T>(f: impl FnOnce() -> T) -> T {
    // SAFETY: paired with the uninitialise below when it succeeded.
    let ours = unsafe { CoInitializeEx(std::ptr::null(), COINIT_APARTMENTTHREADED as u32) } >= 0;
    let out = f();
    if ours {
        unsafe { CoUninitialize() };
    }
    out
}

/// What a double-click would do.
pub fn open(target: &std::ffi::OsStr) -> std::io::Result<()> {
    let file = wide(target);
    let verb: Vec<u16> = "open\0".encode_utf16().collect();
    // SAFETY: null-terminated strings that outlive the call.
    let r = with_com(|| unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            verb.as_ptr(),
            file.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            SW_SHOWNORMAL,
        )
    });
    // Above 32 is success; below, an error code in the old style.
    if r as isize > 32 {
        Ok(())
    } else {
        Err(std::io::Error::other(format!(
            "Windows would not open it (code {})",
            r as isize
        )))
    }
}

/// Each path selected in an Explorer window of its folder.
pub fn reveal(paths: &[&Path]) {
    let owned: Vec<Vec<u16>> = paths.iter().map(|p| wide(p.as_os_str())).collect();
    // A thread of its own: the shell may start Explorer, which takes a second.
    std::thread::spawn(move || {
        with_com(|| {
            for path in &owned {
                let mut item: *mut ITEMIDLIST = std::ptr::null_mut();
                // SAFETY: a null-terminated path; the list is freed below.
                let hr = unsafe {
                    SHParseDisplayName(
                        path.as_ptr(),
                        std::ptr::null_mut(),
                        &mut item,
                        0,
                        std::ptr::null_mut(),
                    )
                };
                if hr < 0 || item.is_null() {
                    continue;
                }
                // SAFETY: the item the shell just gave, selected in its parent.
                unsafe {
                    SHOpenFolderAndSelectItems(item, 0, std::ptr::null(), 0);
                    ILFree(item);
                }
            }
        });
    });
}
