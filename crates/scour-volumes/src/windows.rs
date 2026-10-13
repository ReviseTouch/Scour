//! Asking Windows: the drive letters, what each one is, and its bus.

use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Storage::FileSystem::{
    BusTypeMmc, BusTypeSd, BusTypeUsb, CreateFileW, FILE_SHARE_READ, FILE_SHARE_WRITE,
    GetDiskFreeSpaceExW, GetDriveTypeW, GetLogicalDrives, GetVolumeInformationW, OPEN_EXISTING,
};
use windows_sys::Win32::System::IO::DeviceIoControl;
use windows_sys::Win32::System::Ioctl::{
    IOCTL_STORAGE_QUERY_PROPERTY, PropertyStandardQuery, STORAGE_DEVICE_DESCRIPTOR,
    STORAGE_PROPERTY_QUERY, StorageDeviceProperty,
};

use crate::volume::{Kind, Volume};

const DRIVE_REMOVABLE: u32 = 2;
const DRIVE_FIXED: u32 = 3;
const DRIVE_REMOTE: u32 = 4;
const DRIVE_CDROM: u32 = 5;

/// Every drive letter with a volume behind it now.
pub fn look() -> Vec<Volume> {
    let mask = letters();
    let system = std::env::var("SystemDrive").unwrap_or_else(|_| "C:".into());
    (0..26u8)
        .filter(|i| mask & (1 << i) != 0)
        .filter_map(|i| drive(char::from(b'A' + i), &system))
        .collect()
}

/// A bit per drive letter in use: what changes when a disk comes or goes.
pub fn letters() -> u32 {
    // SAFETY: no arguments.
    unsafe { GetLogicalDrives() }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn drive(letter: char, system: &str) -> Option<Volume> {
    let root = format!("{letter}:\\");
    let path = wide(&root);
    // SAFETY: a NUL-terminated path.
    let kind = match unsafe { GetDriveTypeW(path.as_ptr()) } {
        DRIVE_FIXED if format!("{letter}:").eq_ignore_ascii_case(system) => Kind::System,
        DRIVE_FIXED if plugged_in(letter) => Kind::Removable,
        DRIVE_FIXED => Kind::Fixed,
        DRIVE_REMOVABLE | DRIVE_CDROM => Kind::Removable,
        DRIVE_REMOTE => Kind::Network,
        _ => return None,
    };
    // An empty card reader or disc drive has a letter and no volume.
    let (serial, label, fs) = information(&path)?;
    let id = match kind {
        Kind::Network => format!("net:{letter}:"),
        _ => format!("serial:{serial:08X}"),
    };
    let size = if kind == Kind::Network {
        0
    } else {
        total(&path)
    };
    Some(Volume {
        id,
        roots: vec![root],
        label,
        kind,
        fs,
        size,
    })
}

/// The volume's serial number, label and filesystem name.
fn information(path: &[u16]) -> Option<(u32, String, String)> {
    let mut label = [0u16; 261];
    let mut fs = [0u16; 261];
    let mut serial = 0u32;
    // SAFETY: buffers and their lengths as passed; the two unused outputs may be null.
    let ok = unsafe {
        GetVolumeInformationW(
            path.as_ptr(),
            label.as_mut_ptr(),
            label.len() as u32,
            &mut serial,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            fs.as_mut_ptr(),
            fs.len() as u32,
        )
    };
    let text = |b: &[u16]| {
        String::from_utf16_lossy(&b[..b.iter().position(|c| *c == 0).unwrap_or(b.len())])
    };
    (ok != 0).then(|| (serial, text(&label), text(&fs)))
}

fn total(path: &[u16]) -> u64 {
    let mut bytes = 0u64;
    // SAFETY: one output; the other two may be null.
    let ok = unsafe {
        GetDiskFreeSpaceExW(
            path.as_ptr(),
            std::ptr::null_mut(),
            &mut bytes,
            std::ptr::null_mut(),
        )
    };
    if ok != 0 { bytes } else { 0 }
}

/// A disk Windows calls fixed that is on a USB or card bus: an external drive.
/// Asked of the volume's device with no access rights, which needs no administrator.
fn plugged_in(letter: char) -> bool {
    let device = wide(&format!("\\\\.\\{letter}:"));
    // SAFETY: a NUL-terminated name; the handle is closed below.
    let handle = unsafe {
        CreateFileW(
            device.as_ptr(),
            0,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            std::ptr::null(),
            OPEN_EXISTING,
            0,
            std::ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return false;
    }
    let query = STORAGE_PROPERTY_QUERY {
        PropertyId: StorageDeviceProperty,
        QueryType: PropertyStandardQuery,
        AdditionalParameters: [0],
    };
    let mut out = STORAGE_DEVICE_DESCRIPTOR::default();
    let mut got = 0u32;
    // SAFETY: the query and the descriptor are sized as passed.
    let ok = unsafe {
        let ok = DeviceIoControl(
            handle,
            IOCTL_STORAGE_QUERY_PROPERTY,
            (&raw const query).cast(),
            std::mem::size_of::<STORAGE_PROPERTY_QUERY>() as u32,
            (&raw mut out).cast(),
            std::mem::size_of::<STORAGE_DEVICE_DESCRIPTOR>() as u32,
            &mut got,
            std::ptr::null_mut(),
        );
        CloseHandle(handle);
        ok
    };
    ok != 0 && [BusTypeUsb, BusTypeSd, BusTypeMmc].contains(&out.BusType)
}
