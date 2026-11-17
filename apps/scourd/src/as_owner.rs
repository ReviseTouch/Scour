//! Reading a network share as the person who added it. The service runs as
//! LocalSystem, which a share sees as the machine's account and mostly turns
//! away; `\\server\share` that opened fine in the person's own session was
//! walked to nothing (PC2, 17 November). So each walker thread of a share's
//! walk borrows the token of the owner's logged-on session for as long as it
//! walks, and gives it back when it ends. Only SYSTEM may ask for that token.

use std::sync::{Arc, OnceLock};

use scour_source_fs::WalkAs;

/// The SID the service was started for, `--owner`.
static OWNER: OnceLock<String> = OnceLock::new();

pub(crate) fn set_owner(sid: &str) {
    let _ = OWNER.set(sid.to_owned());
}

/// A UNC root: what `\\server\share\…` and `//server/share/…` look like.
pub(crate) fn is_share(root: &std::path::Path) -> bool {
    let s = root.to_string_lossy();
    s.starts_with(r"\\") || s.starts_with("//")
}

/// The hook for a source with these roots, when it needs one: only in the
/// service, and only for a share.
pub(crate) fn walk_as(roots: &[std::path::PathBuf]) -> Option<WalkAs> {
    let owner = OWNER.get()?.clone();
    if !cfg!(windows) || !roots.iter().any(|r| is_share(r)) {
        return None;
    }
    Some(WalkAs(Arc::new(move || imp::become_owner(&owner))))
}

#[cfg(windows)]
mod imp {
    use std::any::Any;
    use std::ffi::c_void;

    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, LocalFree};
    use windows_sys::Win32::Security::Authorization::ConvertSidToStringSidW;
    use windows_sys::Win32::Security::{
        GetTokenInformation, ImpersonateLoggedOnUser, RevertToSelf, TOKEN_USER, TokenUser,
    };
    use windows_sys::Win32::System::RemoteDesktop::{
        WTS_CURRENT_SERVER_HANDLE, WTS_SESSION_INFOW, WTSEnumerateSessionsW, WTSFreeMemory,
        WTSQueryUserToken,
    };

    /// Gives the thread its own identity back, and the token to the system.
    struct Borrowed(usize);

    impl Drop for Borrowed {
        fn drop(&mut self) {
            // SAFETY: the thread that impersonated is the one dropping this:
            // the walker keeps the guard in its visitor, which ends on it.
            unsafe {
                RevertToSelf();
                CloseHandle(self.0 as HANDLE);
            }
        }
    }

    pub(super) fn become_owner(owner: &str) -> Option<Box<dyn Any + Send>> {
        let token = session_token(owner)?;
        // SAFETY: a token handle this process holds; failing leaves the
        // thread as it was, and the walk reads what SYSTEM may.
        if unsafe { ImpersonateLoggedOnUser(token) } == 0 {
            scour_core::note!(
                "scourd: could not read the share as its owner: {}",
                std::io::Error::last_os_error()
            );
            unsafe { CloseHandle(token) };
            return None;
        }
        Some(Box::new(Borrowed(token as usize)))
    }

    /// The token of a logged-on session whose user is `owner`. None when the
    /// person is not logged on: then nobody's credentials reach the share.
    fn session_token(owner: &str) -> Option<HANDLE> {
        let mut list: *mut WTS_SESSION_INFOW = std::ptr::null_mut();
        let mut count = 0u32;
        // SAFETY: out-pointers to locals; the list is freed below.
        if unsafe { WTSEnumerateSessionsW(WTS_CURRENT_SERVER_HANDLE, 0, 1, &mut list, &mut count) }
            == 0
        {
            return None;
        }
        // SAFETY: the system wrote `count` entries at `list`.
        let sessions = unsafe { std::slice::from_raw_parts(list, count as usize) };
        let mut found = None;
        for s in sessions {
            let mut token: HANDLE = std::ptr::null_mut();
            // SAFETY: an out-pointer; fails for sessions nobody is logged on to.
            if unsafe { WTSQueryUserToken(s.SessionId, &mut token) } == 0 {
                continue;
            }
            if sid_of(token).is_some_and(|sid| sid.eq_ignore_ascii_case(owner)) {
                found = Some(token);
                break;
            }
            unsafe { CloseHandle(token) };
        }
        unsafe { WTSFreeMemory(list as *mut c_void) };
        found
    }

    /// The token's user as `S-1-5-…`.
    fn sid_of(token: HANDLE) -> Option<String> {
        let mut buf = vec![0u64; 64];
        let mut len = 0u32;
        // SAFETY: a buffer of the size given, aligned for TOKEN_USER.
        let ok = unsafe {
            GetTokenInformation(
                token,
                TokenUser,
                buf.as_mut_ptr() as *mut c_void,
                (buf.len() * 8) as u32,
                &mut len,
            )
        };
        if ok == 0 {
            return None;
        }
        // SAFETY: filled by the call above.
        let user = unsafe { &*(buf.as_ptr() as *const TOKEN_USER) };
        let mut text: *mut u16 = std::ptr::null_mut();
        // SAFETY: a SID inside `buf`; the string is freed with LocalFree.
        if unsafe { ConvertSidToStringSidW(user.User.Sid, &mut text) } == 0 {
            return None;
        }
        let mut n = 0;
        // SAFETY: a null-terminated string the system allocated.
        let sid = unsafe {
            while *text.add(n) != 0 {
                n += 1;
            }
            String::from_utf16_lossy(std::slice::from_raw_parts(text, n))
        };
        unsafe { LocalFree(text as *mut c_void) };
        Some(sid)
    }
}

#[cfg(not(windows))]
mod imp {
    pub(super) fn become_owner(_: &str) -> Option<Box<dyn std::any::Any + Send>> {
        None
    }
}
