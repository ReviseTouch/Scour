//! A Mac: the key is registered by a process that stays — Scour's service,
//! which runs in the person's own session — through Carbon's hot keys, the one
//! global key a program needs no permission for. [`listen`] is that process's
//! main thread from then on: Carbon delivers a hot key to the main event queue.

use std::ffi::c_void;
use std::path::Path;

use crate::key::Key;

#[repr(C)]
#[derive(Clone, Copy)]
struct HotKeyId {
    signature: u32,
    id: u32,
}

#[repr(C)]
struct EventType {
    class: u32,
    kind: u32,
}

type Handler = extern "C" fn(call: *mut c_void, event: *mut c_void, data: *mut c_void) -> i32;

#[link(name = "Carbon", kind = "framework")]
unsafe extern "C" {
    fn GetEventDispatcherTarget() -> *mut c_void;
    fn GetApplicationEventTarget() -> *mut c_void;
    fn InstallEventHandler(
        target: *mut c_void,
        handler: Handler,
        types: usize,
        list: *const EventType,
        data: *mut c_void,
        out: *mut *mut c_void,
    ) -> i32;
    fn RegisterEventHotKey(
        code: u32,
        modifiers: u32,
        id: HotKeyId,
        target: *mut c_void,
        options: u32,
        out: *mut *mut c_void,
    ) -> i32;
    fn UnregisterEventHotKey(hot_key: *mut c_void) -> i32;
    fn ReceiveNextEvent(
        types: usize,
        list: *const EventType,
        timeout: f64,
        pull: u8,
        out: *mut *mut c_void,
    ) -> i32;
    fn SendEventToEventTarget(event: *mut c_void, target: *mut c_void) -> i32;
    fn ReleaseEvent(event: *mut c_void);
}

/// `'keyb'` and `kEventHotKeyPressed`.
const KEYBOARD: u32 = 0x6b65_7962;
const HOT_KEY_PRESSED: u32 = 5;
/// `eventLoopTimedOutErr`: a second passed and nothing happened.
const TIMED_OUT: i32 = -9875;
/// `'Scou'`.
const SIGNATURE: u32 = 0x5363_6f75;

static PRESSED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

extern "C" fn pressed(_call: *mut c_void, _event: *mut c_void, _data: *mut c_void) -> i32 {
    PRESSED.store(true, std::sync::atomic::Ordering::Relaxed);
    0
}

/// Hold the key `file` names and call `open` each time it is pressed; the file
/// is read again every second, so a key chosen in a face is the key at once.
/// Returns only if this process has no event queue to wait on — started
/// outside a session — and then at once.
pub fn listen(file: &Path, mut open: impl FnMut()) {
    let on = EventType {
        class: KEYBOARD,
        kind: HOT_KEY_PRESSED,
    };
    // SAFETY: Carbon's event calls, on the thread that goes on to wait on its
    // queue; every pointer handed over outlives the call it is handed to, and
    // the handler is a function, which lives as long as the process.
    unsafe {
        let installed = InstallEventHandler(
            GetApplicationEventTarget(),
            pressed,
            1,
            &on,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        );
        if installed != 0 {
            return;
        }
        let mut held: Option<(Key, *mut c_void)> = None;
        loop {
            let wanted = crate::kept::current(file).ok().flatten();
            if wanted.as_ref() != held.as_ref().map(|(key, _)| key) {
                if let Some((_, registered)) = held.take() {
                    UnregisterEventHotKey(registered);
                }
                if let Some((key, (code, modifiers))) = wanted.and_then(|k| k.mac().map(|m| (k, m)))
                {
                    let mut registered = std::ptr::null_mut();
                    let id = HotKeyId {
                        signature: SIGNATURE,
                        id: 1,
                    };
                    let target = GetApplicationEventTarget();
                    if RegisterEventHotKey(code, modifiers, id, target, 0, &mut registered) == 0 {
                        held = Some((key, registered));
                    }
                }
            }
            let mut event = std::ptr::null_mut();
            match ReceiveNextEvent(0, std::ptr::null(), 1.0, 1, &mut event) {
                0 => {
                    SendEventToEventTarget(event, GetEventDispatcherTarget());
                    ReleaseEvent(event);
                }
                TIMED_OUT => {}
                // No queue to wait on: not a second's wait, so not a loop.
                _ => return,
            }
            if PRESSED.swap(false, std::sync::atomic::Ordering::Relaxed) {
                open();
            }
        }
    }
}
