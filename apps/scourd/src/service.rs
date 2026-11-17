//! Running under the Windows Service Control Manager, for the service mode in
//! docs/WINDOWS-SERVICE.md. The manager calls `service_main` on a thread of its
//! own; that reports running, runs the same [`crate::run`] a console start does,
//! and a stop from the manager goes through the same hook as Ctrl-C.

use std::ffi::c_void;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicUsize, Ordering};

use windows_sys::Win32::Foundation::{ERROR_CALL_NOT_IMPLEMENTED, NO_ERROR};
use windows_sys::Win32::System::Services::{
    RegisterServiceCtrlHandlerExW, SERVICE_ACCEPT_SHUTDOWN, SERVICE_ACCEPT_STOP,
    SERVICE_CONTROL_INTERROGATE, SERVICE_CONTROL_SHUTDOWN, SERVICE_CONTROL_STOP, SERVICE_RUNNING,
    SERVICE_START_PENDING, SERVICE_STATUS, SERVICE_STATUS_CURRENT_STATE, SERVICE_STOP_PENDING,
    SERVICE_STOPPED, SERVICE_TABLE_ENTRYW, SERVICE_WIN32_OWN_PROCESS, SetServiceStatus,
    StartServiceCtrlDispatcherW,
};

/// The name the service is registered under.
pub(crate) const NAME: &str = "Scour";

/// What the manager's thread runs with: set before the dispatcher starts.
static ARGS: OnceLock<crate::Args> = OnceLock::new();
/// The status handle, as an integer: a raw pointer is not `Sync`.
static HANDLE: AtomicUsize = AtomicUsize::new(0);

/// Hand the process to the Service Control Manager; returns when the service
/// has stopped. Fails at once when not started by the manager.
pub(crate) fn dispatch(args: crate::Args) -> anyhow::Result<()> {
    let _ = ARGS.set(args);
    let mut name = wide(NAME);
    let table = [
        SERVICE_TABLE_ENTRYW {
            lpServiceName: name.as_mut_ptr(),
            lpServiceProc: Some(service_main),
        },
        SERVICE_TABLE_ENTRYW {
            lpServiceName: std::ptr::null_mut(),
            lpServiceProc: None,
        },
    ];
    // SAFETY: the table is null-terminated and outlives the call, which blocks
    // until every service in it has stopped.
    if unsafe { StartServiceCtrlDispatcherW(table.as_ptr()) } == 0 {
        anyhow::bail!(
            "--service is for the Service Control Manager: {}",
            std::io::Error::last_os_error()
        );
    }
    Ok(())
}

unsafe extern "system" fn service_main(_argc: u32, _argv: *mut *mut u16) {
    let name = wide(NAME);
    // SAFETY: a null-terminated name, and a handler that lives for the process.
    let handle =
        unsafe { RegisterServiceCtrlHandlerExW(name.as_ptr(), Some(control), std::ptr::null()) };
    if handle.is_null() {
        return;
    }
    HANDLE.store(handle as usize, Ordering::Release);
    report(SERVICE_START_PENDING, 0);
    // Running before the walk and the bind: the manager waits thirty seconds
    // for this, and opening a large index can take longer.
    report(SERVICE_RUNNING, 0);
    let code = match ARGS.get().cloned().map(crate::run) {
        Some(Ok(())) => 0,
        Some(Err(e)) => {
            scour_core::note!("scourd: the service stopped: {e:#}");
            1
        }
        None => 1,
    };
    report(SERVICE_STOPPED, code);
}

unsafe extern "system" fn control(
    control: u32,
    _event: u32,
    _data: *mut c_void,
    _context: *mut c_void,
) -> u32 {
    match control {
        SERVICE_CONTROL_STOP | SERVICE_CONTROL_SHUTDOWN => {
            report(SERVICE_STOP_PENDING, 0);
            // On its own thread: the manager wants this call back quickly, and
            // stopping commits the index.
            std::thread::spawn(crate::stop_now);
            NO_ERROR
        }
        SERVICE_CONTROL_INTERROGATE => NO_ERROR,
        _ => ERROR_CALL_NOT_IMPLEMENTED,
    }
}

/// Tell the manager where the service is. `code` is the service's own exit code.
fn report(state: SERVICE_STATUS_CURRENT_STATE, code: u32) {
    let handle = HANDLE.load(Ordering::Acquire);
    if handle == 0 {
        return;
    }
    let pending = state == SERVICE_START_PENDING || state == SERVICE_STOP_PENDING;
    let status = SERVICE_STATUS {
        dwServiceType: SERVICE_WIN32_OWN_PROCESS,
        dwCurrentState: state,
        dwControlsAccepted: if state == SERVICE_RUNNING {
            SERVICE_ACCEPT_STOP | SERVICE_ACCEPT_SHUTDOWN
        } else {
            0
        },
        // 1066, ERROR_SERVICE_SPECIFIC_ERROR, carries `code` to the event log.
        dwWin32ExitCode: if code == 0 { NO_ERROR } else { 1066 },
        dwServiceSpecificExitCode: code,
        dwCheckPoint: 0,
        dwWaitHint: if pending { 30_000 } else { 0 },
    };
    // SAFETY: a handle the manager gave, and a status that lives for the call.
    unsafe {
        SetServiceStatus(handle as *mut c_void, &status);
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}
