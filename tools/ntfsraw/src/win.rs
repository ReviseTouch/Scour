use std::collections::HashMap;
use std::ffi::c_void;
use std::io::Read as _;
use std::os::windows::ffi::OsStrExt as _;
use std::os::windows::io::AsRawHandle as _;
use std::time::Instant;

use windows_sys::Win32::Foundation::{
    CloseHandle, GENERIC_READ, GetLastError, HANDLE, INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Storage::FileSystem::{
    BY_HANDLE_FILE_INFORMATION, CreateFileW, FILE_ATTRIBUTE_COMPRESSED, FILE_ATTRIBUTE_ENCRYPTED,
    FILE_ATTRIBUTE_SPARSE_FILE, FILE_BEGIN, FILE_FLAG_NO_BUFFERING, FILE_READ_ATTRIBUTES,
    FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, GetFileInformationByHandle,
    OPEN_EXISTING, ReadFile, SetFilePointerEx,
};
use windows_sys::Win32::System::IO::DeviceIoControl;
use windows_sys::Win32::System::Ioctl::{
    FSCTL_GET_NTFS_FILE_RECORD, FSCTL_GET_NTFS_VOLUME_DATA, FSCTL_GET_RETRIEVAL_POINTERS,
    NTFS_VOLUME_DATA_BUFFER, STARTING_VCN_INPUT_BUFFER,
};
use windows_sys::Win32::System::Memory::{MEM_COMMIT, MEM_RESERVE, PAGE_READWRITE, VirtualAlloc};

const BLOCK: usize = 4096;
/// One read of the volume at most.
const SPAN: u64 = 4 << 20;
/// Pieces nearer than this are read in one span, the gap read and dropped.
const GAP: u64 = 256 << 10;
/// An extent is cut into pieces of at most this.
const PIECE: u64 = 1 << 20;

pub fn main() {
    let args: Vec<String> = std::env::args().collect();
    let (mode, list) = (args[1].as_str(), &args[2]);
    let text = std::fs::read_to_string(list).expect("list");
    let paths: Vec<String> = text
        .lines()
        .map(|l| l.trim_start_matches('\u{feff}').trim().to_string())
        .filter(|l| !l.is_empty())
        .collect();
    match mode {
        "normal" => {
            let t = Instant::now();
            let (mut bytes, mut fails) = (0u64, 0);
            for p in &paths {
                match normal(p) {
                    Some((_, n)) => bytes += n,
                    None => fails += 1,
                }
            }
            report("normal", paths.len(), bytes, t.elapsed().as_secs_f64(), 0.0, fails, &[]);
        }
        "raw" => {
            let (_, stats) = raw(&paths);
            stats.print();
        }
        "verify" => {
            let (hashes, stats) = raw(&paths);
            stats.print();
            let (mut same, mut differ, mut missing) = (0, 0, 0);
            for (i, p) in paths.iter().enumerate() {
                match (normal(p), hashes.get(&i)) {
                    (Some((h, _)), Some(r)) if h == *r => same += 1,
                    (Some(_), Some(_)) => {
                        differ += 1;
                        if differ <= 5 {
                            println!("DIFFERS: file #{i}");
                        }
                    }
                    _ => missing += 1,
                }
            }
            println!("verify same {same} differ {differ} unread {missing}");
        }
        _ => eprintln!("mode: normal | raw | verify"),
    }
}

fn report(mode: &str, files: usize, bytes: u64, total: f64, gather: f64, fails: usize, extra: &[(&str, String)]) {
    print!(
        "{mode} files {files} seconds {total:.3} gather {gather:.3} mib {:.1} files_per_s {:.0} failed {fails}",
        bytes as f64 / 1048576.0,
        files as f64 / total
    );
    for (k, v) in extra {
        print!(" {k} {v}");
    }
    println!();
}

/// A file's content as an order-free sum of its 4 KiB blocks, each mixed with
/// its index: the same however the blocks arrive.
fn block_hash(data: &[u8], index: u64) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in data {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h ^ index.wrapping_mul(0x9E37_79B9_7F4A_7C15)
}

fn hash_range(sum: &mut u64, data: &[u8], file_offset: u64, size: u64) {
    let mut at = 0usize;
    while at < data.len() {
        let off = file_offset + at as u64;
        if off >= size {
            break;
        }
        let len = BLOCK.min(data.len() - at).min((size - off) as usize);
        *sum = sum.wrapping_add(block_hash(&data[at..at + len], off / BLOCK as u64));
        at += BLOCK;
    }
}

/// Opened and read the ordinary way; the block sum and the bytes read.
fn normal(path: &str) -> Option<(u64, u64)> {
    let mut f = std::fs::File::open(path).ok()?;
    let size = f.metadata().ok()?.len();
    let mut buf = vec![0u8; BLOCK * 256];
    let (mut sum, mut off) = (0u64, 0u64);
    loop {
        let mut filled = 0;
        while filled < buf.len() {
            match f.read(&mut buf[filled..]) {
                Ok(0) => break,
                Ok(n) => filled += n,
                Err(_) => return None,
            }
        }
        if filled == 0 {
            break;
        }
        hash_range(&mut sum, &buf[..filled], off, size);
        off += filled as u64;
        if filled < buf.len() {
            break;
        }
    }
    Some((sum, off))
}

fn wide(s: &str) -> Vec<u16> {
    std::ffi::OsStr::new(s).encode_wide().chain(Some(0)).collect()
}

struct Stats {
    files: usize,
    nonresident: usize,
    resident: usize,
    fallback: usize,
    empty: usize,
    failed: usize,
    spans: usize,
    gather: f64,
    read: f64,
    total: f64,
    bytes_raw: u64,
    bytes_files: u64,
}

impl Stats {
    fn print(&self) {
        report(
            "raw",
            self.files,
            self.bytes_raw,
            self.total,
            self.gather,
            self.failed,
            &[
                ("read_s", format!("{:.3}", self.read)),
                ("spans", self.spans.to_string()),
                ("extents_files", self.nonresident.to_string()),
                ("in_mft", self.resident.to_string()),
                ("normal_way", self.fallback.to_string()),
                ("empty", self.empty.to_string()),
                ("file_mib", format!("{:.1}", self.bytes_files as f64 / 1048576.0)),
            ],
        );
    }
}

/// One piece of one file: where it is on the volume and in the file.
#[derive(Clone, Copy)]
struct Piece {
    file: usize,
    lcn: u64,
    clusters: u64,
    file_offset: u64,
}

fn raw(paths: &[String]) -> (HashMap<usize, u64>, Stats) {
    let began = Instant::now();
    let letter: String = paths[0].chars().take(2).collect();
    let volume = open_volume(&letter);
    let data = volume_data(volume);
    let cluster = data.BytesPerCluster as u64;
    let record_size = data.BytesPerFileRecordSegment as usize;
    assert!(cluster as usize % BLOCK == 0, "cluster {cluster} under 4 KiB");
    let mut stats = Stats {
        files: paths.len(),
        nonresident: 0,
        resident: 0,
        fallback: 0,
        empty: 0,
        failed: 0,
        spans: 0,
        gather: 0.0,
        read: 0.0,
        total: 0.0,
        bytes_raw: 0,
        bytes_files: 0,
    };
    let mut sums: HashMap<usize, u64> = HashMap::new();
    let mut sizes: Vec<u64> = vec![0; paths.len()];
    let mut pieces: Vec<Piece> = Vec::new();
    let mut normal_way: Vec<usize> = Vec::new();
    for (i, p) in paths.iter().enumerate() {
        let Some((info, size, handle)) = attributes(p) else {
            stats.failed += 1;
            continue;
        };
        sizes[i] = size;
        stats.bytes_files += size;
        let flags = info.dwFileAttributes;
        if flags & (FILE_ATTRIBUTE_COMPRESSED | FILE_ATTRIBUTE_ENCRYPTED | FILE_ATTRIBUTE_SPARSE_FILE) != 0 {
            normal_way.push(i);
            unsafe { CloseHandle(handle) };
            continue;
        }
        if size == 0 {
            stats.empty += 1;
            sums.insert(i, 0);
            unsafe { CloseHandle(handle) };
            continue;
        }
        match extents(handle) {
            Some(list) if !list.is_empty() => {
                stats.nonresident += 1;
                let mut covered = 0u64;
                for (vcn, lcn, n) in list {
                    let mut done = 0u64;
                    while done < n {
                        let take = (n - done).min(PIECE / cluster);
                        pieces.push(Piece {
                            file: i,
                            lcn: lcn + done,
                            clusters: take,
                            file_offset: (vcn + done) * cluster,
                        });
                        done += take;
                    }
                    covered += n * cluster;
                }
                if covered < size {
                    // Not all of it is placed: the normal way.
                    pieces.retain(|pc| pc.file != i);
                    normal_way.push(i);
                    stats.nonresident -= 1;
                }
            }
            _ => {
                let record = ((info.nFileIndexHigh as u64) << 32 | info.nFileIndexLow as u64) & 0x0000_FFFF_FFFF_FFFF;
                match resident_data(volume, record, record_size) {
                    Some(bytes) if bytes.len() as u64 == size => {
                        stats.resident += 1;
                        let mut sum = 0;
                        hash_range(&mut sum, &bytes, 0, size);
                        sums.insert(i, sum);
                    }
                    _ => normal_way.push(i),
                }
            }
        }
        unsafe { CloseHandle(handle) };
    }
    stats.gather = began.elapsed().as_secs_f64();

    let t = Instant::now();
    pieces.sort_by_key(|p| p.lcn);
    let buf = aligned(SPAN as usize + GAP as usize + PIECE as usize);
    let mut i = 0;
    while i < pieces.len() {
        let start = pieces[i].lcn;
        let mut end = pieces[i].lcn + pieces[i].clusters;
        let mut j = i + 1;
        while j < pieces.len() {
            let next = &pieces[j];
            let gap = next.lcn.saturating_sub(end) * cluster;
            let new_end = end.max(next.lcn + next.clusters);
            if gap > GAP || (new_end - start) * cluster > SPAN {
                break;
            }
            end = new_end;
            j += 1;
        }
        let len = ((end - start) * cluster) as usize;
        read_at(volume, start * cluster, &mut buf[..len]);
        stats.spans += 1;
        stats.bytes_raw += len as u64;
        for p in &pieces[i..j] {
            let from = ((p.lcn - start) * cluster) as usize;
            let to = from + (p.clusters * cluster) as usize;
            let sum = sums.entry(p.file).or_insert(0);
            hash_range(sum, &buf[from..to], p.file_offset, sizes[p.file]);
        }
        i = j;
    }
    for &f in &normal_way {
        stats.fallback += 1;
        if let Some((sum, n)) = normal(&paths[f]) {
            sums.insert(f, sum);
            stats.bytes_raw += n;
        } else {
            stats.failed += 1;
        }
    }
    stats.read = t.elapsed().as_secs_f64();
    stats.total = began.elapsed().as_secs_f64();
    unsafe { CloseHandle(volume) };
    (sums, stats)
}

fn open_volume(letter: &str) -> HANDLE {
    let name = wide(&format!("\\\\.\\{letter}"));
    let h = unsafe {
        CreateFileW(
            name.as_ptr(),
            GENERIC_READ,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            std::ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_NO_BUFFERING,
            std::ptr::null_mut(),
        )
    };
    if h == INVALID_HANDLE_VALUE {
        eprintln!("the volume {letter} would not open (error {}): run elevated", unsafe { GetLastError() });
        std::process::exit(2);
    }
    h
}

fn volume_data(volume: HANDLE) -> NTFS_VOLUME_DATA_BUFFER {
    let mut data: NTFS_VOLUME_DATA_BUFFER = unsafe { std::mem::zeroed() };
    let mut got = 0u32;
    let ok = unsafe {
        DeviceIoControl(
            volume,
            FSCTL_GET_NTFS_VOLUME_DATA,
            std::ptr::null(),
            0,
            &mut data as *mut _ as *mut c_void,
            size_of::<NTFS_VOLUME_DATA_BUFFER>() as u32,
            &mut got,
            std::ptr::null_mut(),
        )
    };
    assert!(ok != 0, "not NTFS, or not allowed");
    data
}

/// Opened for its attributes only: no data is asked for, so nothing scans it.
fn attributes(path: &str) -> Option<(BY_HANDLE_FILE_INFORMATION, u64, HANDLE)> {
    let name = wide(path);
    let h = unsafe {
        CreateFileW(
            name.as_ptr(),
            FILE_READ_ATTRIBUTES,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            std::ptr::null(),
            OPEN_EXISTING,
            0,
            std::ptr::null_mut(),
        )
    };
    if h == INVALID_HANDLE_VALUE {
        return None;
    }
    let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
    if unsafe { GetFileInformationByHandle(h, &mut info) } == 0 {
        unsafe { CloseHandle(h) };
        return None;
    }
    let size = (info.nFileSizeHigh as u64) << 32 | info.nFileSizeLow as u64;
    Some((info, size, h))
}

/// (VCN, LCN, clusters) of every placed run; None when the file has no runs
/// of its own — it lives in its MFT record.
fn extents(handle: HANDLE) -> Option<Vec<(u64, u64, u64)>> {
    let mut out = Vec::new();
    let mut vcn: i64 = 0;
    let mut buf = vec![0u8; 64 << 10];
    loop {
        let input = STARTING_VCN_INPUT_BUFFER { StartingVcn: vcn };
        let mut got = 0u32;
        let ok = unsafe {
            DeviceIoControl(
                handle,
                FSCTL_GET_RETRIEVAL_POINTERS,
                &input as *const _ as *const c_void,
                size_of::<STARTING_VCN_INPUT_BUFFER>() as u32,
                buf.as_mut_ptr() as *mut c_void,
                buf.len() as u32,
                &mut got,
                std::ptr::null_mut(),
            )
        };
        let more = ok == 0 && unsafe { GetLastError() } == 234;
        if ok == 0 && !more {
            return if out.is_empty() { None } else { Some(out) };
        }
        let count = u32::from_le_bytes(buf[0..4].try_into().ok()?) as usize;
        let mut prev = i64::from_le_bytes(buf[8..16].try_into().ok()?);
        for k in 0..count {
            let at = 16 + k * 16;
            let next = i64::from_le_bytes(buf[at..at + 8].try_into().ok()?);
            let lcn = i64::from_le_bytes(buf[at + 8..at + 16].try_into().ok()?);
            if lcn >= 0 {
                out.push((prev as u64, lcn as u64, (next - prev) as u64));
            }
            prev = next;
        }
        if !more {
            return Some(out);
        }
        vcn = prev;
    }
}

/// The unnamed $DATA attribute of a record, when it is resident.
fn resident_data(volume: HANDLE, record: u64, record_size: usize) -> Option<Vec<u8>> {
    let input = record as i64;
    let mut out = vec![0u8; 12 + record_size];
    let mut got = 0u32;
    let ok = unsafe {
        DeviceIoControl(
            volume,
            FSCTL_GET_NTFS_FILE_RECORD,
            &input as *const i64 as *const c_void,
            8,
            out.as_mut_ptr() as *mut c_void,
            out.len() as u32,
            &mut got,
            std::ptr::null_mut(),
        )
    };
    if ok == 0 {
        return None;
    }
    let returned = i64::from_le_bytes(out[0..8].try_into().ok()?) as u64 & 0x0000_FFFF_FFFF_FFFF;
    if returned != record {
        return None;
    }
    let len = u32::from_le_bytes(out[8..12].try_into().ok()?) as usize;
    let rec = &mut out[12..12 + len.min(record_size)];
    if &rec[0..4] != b"FILE" {
        return None;
    }
    fixup(rec);
    let mut at = u16::from_le_bytes(rec[0x14..0x16].try_into().ok()?) as usize;
    while at + 16 <= rec.len() {
        let kind = u32::from_le_bytes(rec[at..at + 4].try_into().ok()?);
        if kind == 0xFFFF_FFFF {
            break;
        }
        let alen = u32::from_le_bytes(rec[at + 4..at + 8].try_into().ok()?) as usize;
        if alen == 0 || at + alen > rec.len() {
            break;
        }
        let nonresident = rec[at + 8];
        let name_len = rec[at + 9];
        if kind == 0x80 && name_len == 0 {
            if nonresident != 0 {
                return None;
            }
            let vlen = u32::from_le_bytes(rec[at + 0x10..at + 0x14].try_into().ok()?) as usize;
            let voff = u16::from_le_bytes(rec[at + 0x14..at + 0x16].try_into().ok()?) as usize;
            return rec.get(at + voff..at + voff + vlen).map(<[u8]>::to_vec);
        }
        at += alen;
    }
    None
}

/// The update sequence: each sector's last two bytes stand in the array.
fn fixup(rec: &mut [u8]) {
    let off = u16::from_le_bytes([rec[4], rec[5]]) as usize;
    let count = u16::from_le_bytes([rec[6], rec[7]]) as usize;
    if count < 2 || off + count * 2 > rec.len() {
        return;
    }
    let usn = [rec[off], rec[off + 1]];
    for s in 1..count {
        let end = s * 512;
        if end > rec.len() {
            break;
        }
        if rec[end - 2..end] == usn {
            rec[end - 2] = rec[off + s * 2];
            rec[end - 1] = rec[off + s * 2 + 1];
        }
    }
}

fn aligned(len: usize) -> &'static mut [u8] {
    let p = unsafe { VirtualAlloc(std::ptr::null(), len, MEM_COMMIT | MEM_RESERVE, PAGE_READWRITE) };
    assert!(!p.is_null());
    unsafe { std::slice::from_raw_parts_mut(p as *mut u8, len) }
}

fn read_at(volume: HANDLE, offset: u64, buf: &mut [u8]) {
    unsafe { SetFilePointerEx(volume, offset as i64, std::ptr::null_mut(), FILE_BEGIN) };
    let mut done = 0usize;
    while done < buf.len() {
        let mut got = 0u32;
        let ok = unsafe {
            ReadFile(
                volume,
                buf[done..].as_mut_ptr(),
                (buf.len() - done) as u32,
                &mut got,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 || got == 0 {
            eprintln!("volume read failed at {offset} (error {})", unsafe { GetLastError() });
            return;
        }
        done += got as usize;
    }
}

#[allow(dead_code)]
fn handle_of(f: &std::fs::File) -> HANDLE {
    f.as_raw_handle() as HANDLE
}
