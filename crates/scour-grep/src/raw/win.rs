//! The Windows half: the volume opened, its MFT, `$SDS` and change journal
//! read, the asker's access checked, the clusters read many at once. See the
//! module above for the rules.

use std::alloc::{Layout, alloc_zeroed, dealloc};
use std::collections::{HashMap, HashSet, VecDeque};
use std::ffi::c_void;
use std::path::Path;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_IO_PENDING, GENERIC_READ, GetLastError, HANDLE, INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Security::{
    AccessCheck, DuplicateToken, GENERIC_MAPPING, IsValidSecurityDescriptor,
    SecurityIdentification, TOKEN_DUPLICATE, TOKEN_QUERY,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_ALL_ACCESS, FILE_FLAG_NO_BUFFERING, FILE_FLAG_OVERLAPPED,
    FILE_GENERIC_EXECUTE, FILE_GENERIC_READ, FILE_GENERIC_WRITE, FILE_READ_DATA, FILE_SHARE_READ,
    FILE_SHARE_WRITE, OPEN_EXISTING, ReadFile,
};
use windows_sys::Win32::System::IO::{DeviceIoControl, GetOverlappedResult, OVERLAPPED};
use windows_sys::Win32::System::Ioctl::{
    FSCTL_GET_NTFS_VOLUME_DATA, FSCTL_QUERY_USN_JOURNAL, FSCTL_READ_USN_JOURNAL,
    NTFS_VOLUME_DATA_BUFFER, READ_USN_JOURNAL_DATA_V0, USN_JOURNAL_DATA_V0,
};
use windows_sys::Win32::System::Threading::{
    CreateEventW, GetCurrentProcess, GetCurrentThread, OpenProcessToken, OpenThreadToken,
};

use super::{
    Catalog, PIECE, Place, SPAN, TICKS, journal, key_of, mft, pieces_of, place_of, recent_secs,
    sds, spans_of,
};

/// Requests kept in flight: the disk orders what it is given by where the
/// head is. 32 read 1.6–2.1× faster than one at a time.
const DEPTH: usize = 32;
/// The MFT and `$SDS` are read this much at a time.
const STRIDE: usize = 1 << 20;
/// FILETIME at the Unix epoch.
const EPOCH: i64 = 116_444_736_000_000_000;

struct Owned(HANDLE);

impl Drop for Owned {
    fn drop(&mut self) {
        // SAFETY: a handle this struct alone holds.
        unsafe { CloseHandle(self.0) };
    }
}

/// A buffer aligned for unbuffered reads.
struct Aligned {
    ptr: *mut u8,
    len: usize,
}

impl Aligned {
    fn new(len: usize) -> Aligned {
        let layout = Layout::from_size_align(len.max(4096), 4096).expect("a layout");
        // SAFETY: a non-zero size.
        let ptr = unsafe { alloc_zeroed(layout) };
        assert!(!ptr.is_null(), "out of memory");
        Aligned {
            ptr,
            len: len.max(4096),
        }
    }

    fn bytes(&mut self) -> &mut [u8] {
        // SAFETY: `len` bytes allocated above, owned by this struct.
        unsafe { std::slice::from_raw_parts_mut(self.ptr, self.len) }
    }
}

impl Drop for Aligned {
    fn drop(&mut self) {
        let layout = Layout::from_size_align(self.len, 4096).expect("a layout");
        // SAFETY: allocated with this layout in `new`.
        unsafe { dealloc(self.ptr, layout) };
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn open_volume(letter: char, flags: u32) -> Option<Owned> {
    let name = wide(&format!(r"\\.\{letter}:"));
    // SAFETY: a null-terminated name; read only, shared with everyone.
    let h = unsafe {
        CreateFileW(
            name.as_ptr(),
            GENERIC_READ,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            std::ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_NO_BUFFERING | flags,
            std::ptr::null_mut(),
        )
    };
    (h != INVALID_HANDLE_VALUE && !h.is_null()).then_some(Owned(h))
}

fn control<T>(volume: HANDLE, code: u32, input: Option<&[u8]>, out: &mut T) -> Option<u32> {
    control_raw(
        volume,
        code,
        input,
        out as *mut T as *mut c_void,
        size_of::<T>(),
    )
}

fn control_raw(
    volume: HANDLE,
    code: u32,
    input: Option<&[u8]>,
    out: *mut c_void,
    len: usize,
) -> Option<u32> {
    let (ip, il) = input.map_or((std::ptr::null(), 0), |i| {
        (i.as_ptr() as *const c_void, i.len())
    });
    let mut got = 0u32;
    // SAFETY: buffers of the sizes given; synchronous on this handle.
    let ok = unsafe {
        DeviceIoControl(
            volume,
            code,
            ip,
            il as u32,
            out,
            len as u32,
            &mut got,
            std::ptr::null_mut(),
        )
    };
    (ok != 0).then_some(got)
}

/// Read at a byte offset of the volume, synchronously; false on failure.
fn read_at(volume: HANDLE, offset: u64, buf: &mut [u8]) -> bool {
    // SAFETY: a zeroed OVERLAPPED only carries the offset on a handle opened
    // without FILE_FLAG_OVERLAPPED.
    let mut over: OVERLAPPED = unsafe { std::mem::zeroed() };
    over.Anonymous.Anonymous.Offset = offset as u32;
    over.Anonymous.Anonymous.OffsetHigh = (offset >> 32) as u32;
    let mut got = 0u32;
    // SAFETY: a buffer of the length given.
    let ok = unsafe {
        ReadFile(
            volume,
            buf.as_mut_ptr(),
            buf.len() as u32,
            &mut got,
            &mut over,
        )
    };
    ok != 0 && got as usize == buf.len()
}

/// The bytes of these runs, `len` of them, from start to end, handed out a
/// stride at a time with their offset in the stream: a few strides in
/// flight, so the disk is never idle between two.
fn read_runs(
    letter: char,
    runs: &[(u64, u64, u64)],
    cluster: u64,
    len: u64,
    each: &mut dyn FnMut(u64, &mut [u8]),
) -> bool {
    let per = (STRIDE as u64 / cluster).max(1);
    let mut strides: Vec<(u64, u64, u64)> = Vec::new();
    for &(vcn, lcn, n) in runs {
        let mut done = 0;
        while done < n && (vcn + done) * cluster < len {
            let take = (n - done).min(per);
            strides.push(((vcn + done) * cluster, lcn + done, take));
            done += take;
        }
    }
    let spans: Vec<Span> = strides
        .iter()
        .enumerate()
        .map(|(k, &(_, lcn, n))| (lcn, lcn + n, k, k))
        .collect();
    let Some(queue) = Queue::open(letter, 4, STRIDE) else {
        return false;
    };
    let mut ok = true;
    queue.run(&spans, cluster, &mut |data, span| {
        let Some(data) = data else {
            ok = false;
            return false;
        };
        let at = strides[span.2].0;
        let keep = (len - at).min(data.len() as u64) as usize;
        let mut chunk = data[..keep].to_vec();
        each(at, &mut chunk);
        true
    });
    ok
}

fn now_filetime() -> i64 {
    let since = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    EPOCH + since.as_secs() as i64 * TICKS + since.subsec_nanos() as i64 / 100
}

/// The change journal's records from `from` to `until` (None: to now);
/// None when there is none, or it was cut short under us.
fn journal_from(
    volume: HANDLE,
    id: u64,
    from: i64,
    until: Option<i64>,
) -> Option<Vec<journal::Change>> {
    let mut out = Vec::new();
    let mut buf = vec![0u64; 8192];
    let mut start = from;
    loop {
        let ask = READ_USN_JOURNAL_DATA_V0 {
            StartUsn: start,
            ReasonMask: u32::MAX,
            ReturnOnlyOnClose: 0,
            Timeout: 0,
            BytesToWaitFor: 0,
            UsnJournalID: id,
        };
        // SAFETY: the struct read as its bytes.
        let input = unsafe {
            std::slice::from_raw_parts(
                &ask as *const _ as *const u8,
                size_of::<READ_USN_JOURNAL_DATA_V0>(),
            )
        };
        let got = control_raw(
            volume,
            FSCTL_READ_USN_JOURNAL,
            Some(input),
            buf.as_mut_ptr() as *mut c_void,
            buf.len() * 8,
        )?;
        // SAFETY: `got` bytes the system wrote.
        let bytes = unsafe { std::slice::from_raw_parts(buf.as_ptr() as *const u8, got as usize) };
        let (next, changes) = journal::parse(bytes)?;
        let empty = changes.is_empty();
        out.extend(
            changes
                .into_iter()
                .filter(|c| until.is_none_or(|u| c.usn < u)),
        );
        if empty || next <= start || until.is_some_and(|u| next >= u) {
            return Some(out);
        }
        start = next;
    }
}

/// The records of files changed since `cut`, read back from the journal's
/// end a widening stretch at a time — a record never crosses a 4 KiB page,
/// so a page's start is a place to read from. None when the journal does
/// not reach back that far.
fn changed_since(
    volume: HANDLE,
    j: &USN_JOURNAL_DATA_V0,
    cut: i64,
) -> Option<(HashSet<u64>, usize)> {
    let (mut end, mut back, mut set, mut seen) = (j.NextUsn, 1i64 << 20, HashSet::new(), 0);
    loop {
        let start = ((end - back) & !0xFFF).max(j.FirstUsn);
        let changes = journal_from(volume, j.UsnJournalID, start, Some(end))?;
        seen += changes.len();
        set.extend(changes.iter().filter(|c| c.time >= cut).map(|c| c.record));
        let oldest = changes.first().map(|c| c.time);
        if oldest.is_some_and(|t| t < cut) {
            return Some((set, seen));
        }
        if start <= j.FirstUsn {
            return None;
        }
        end = start;
        back *= 2;
    }
}

fn query_journal(volume: HANDLE) -> Option<USN_JOURNAL_DATA_V0> {
    // SAFETY: plain data, filled by the call.
    let mut data: USN_JOURNAL_DATA_V0 = unsafe { std::mem::zeroed() };
    control(volume, FSCTL_QUERY_USN_JOURNAL, None, &mut data)?;
    Some(data)
}

/// A file to be read raw.
struct Item {
    index: usize,
    record: u64,
    sid: u32,
    size: u64,
    place: Place,
}

pub(crate) struct Plan {
    volume: Owned,
    letter: char,
    cluster: u64,
    journal: u64,
    began: i64,
    items: Vec<Item>,
    /// Security descriptors by id, kept aligned.
    descriptors: HashMap<u32, Vec<u64>>,
    left: Vec<usize>,
    read: Vec<(usize, u64)>,
    note: Note,
}

/// What a reading cost, said on stderr when `SCOUR_GREP_RAW_LOG` is set.
#[derive(Default)]
struct Note {
    mft_mib: f64,
    mft_s: f64,
    journal: usize,
    found: usize,
    asked: usize,
    spans: usize,
    read_mib: f64,
    read_s: f64,
    journal_s: f64,
    recent: usize,
    not_placed: usize,
    descriptors: usize,
    refused: usize,
    check_error: u32,
}

#[derive(Default)]
pub(crate) struct Settled {
    pub read: Vec<usize>,
    pub left: Vec<usize>,
}

impl Plan {
    /// As the process itself — the service, an administrator — before
    /// becoming the asker: the volume, the journal, the MFT, `$SDS`. None
    /// when the volume does not open, is not NTFS or keeps no journal.
    pub(crate) fn prepare(letter: char, paths: &[(usize, &Path)], max_size: u64) -> Option<Plan> {
        match Plan::try_prepare(letter, paths, max_size) {
            Ok(plan) => Some(plan),
            Err(why) => {
                if std::env::var_os("SCOUR_GREP_RAW_LOG").is_some() {
                    eprintln!("raw {letter}: not read raw: {why}");
                }
                None
            }
        }
    }

    fn try_prepare(
        letter: char,
        paths: &[(usize, &Path)],
        max_size: u64,
    ) -> Result<Plan, &'static str> {
        let volume = open_volume(letter, 0).ok_or("the volume does not open")?;
        let h = volume.0;
        // SAFETY: plain data, filled by the call.
        let mut data: NTFS_VOLUME_DATA_BUFFER = unsafe { std::mem::zeroed() };
        control(h, FSCTL_GET_NTFS_VOLUME_DATA, None, &mut data).ok_or("not NTFS")?;
        let cluster = data.BytesPerCluster as u64;
        let record = data.BytesPerFileRecordSegment as usize;
        if cluster == 0 || record == 0 || !cluster.is_multiple_of(512) {
            return Err("an odd cluster or record size");
        }
        let jrnl = query_journal(h).ok_or("no change journal")?;
        let mut note = Note::default();

        // What changed lately is not trusted on the disk. A journal whose
        // oldest record is newer than that cannot say, and nothing is.
        let t = Instant::now();
        let began = jrnl.NextUsn;
        let cut = now_filetime() - recent_secs() as i64 * TICKS;
        let (recent, seen) =
            changed_since(h, &jrnl, cut).ok_or("the journal does not reach back far enough")?;
        note.journal = seen;
        note.journal_s = t.elapsed().as_secs_f64();
        let t = Instant::now();

        // The MFT's own record says where the rest of it is.
        let first = (record as u64).div_ceil(cluster) * cluster;
        let mut head = Aligned::new(first as usize);
        if !read_at(
            h,
            data.MftStartLcn as u64 * cluster,
            &mut head.bytes()[..first as usize],
        ) {
            return Err("record 0 of the MFT would not read");
        }
        let mut zero = head.bytes()[..record].to_vec();
        // A record the MFT's own record names, found by the runs known so far.
        let mut read_record = |n: u64, known: &[(u64, u64, u64)]| -> Option<Vec<u8>> {
            if (record as u64) > cluster {
                return None;
            }
            let at = n * record as u64;
            let vcn = at / cluster;
            let &(start, lcn, _) = known
                .iter()
                .find(|&&(v, _, len)| v <= vcn && vcn < v + len)?;
            let mut buf = Aligned::new(cluster as usize);
            let bytes = &mut buf.bytes()[..cluster as usize];
            if !read_at(h, (lcn + vcn - start) * cluster, bytes) {
                return None;
            }
            let within = (at % cluster) as usize;
            Some(bytes[within..within + record].to_vec())
        };
        let mft_runs = mft::mft_runs(&mut zero, &mut read_record)
            .ok_or("the places of the MFT could not be followed")?;
        let strs: Vec<(usize, &str)> = paths
            .iter()
            .filter_map(|&(i, p)| Some((i, p.to_str()?)))
            .collect();
        let wanted: HashMap<String, usize> = strs.iter().map(|&(i, p)| (key_of(p), i)).collect();
        let mut catalog = Catalog::new(strs.iter().map(|&(_, p)| p));
        let mut secure: Option<Vec<u8>> = None;
        let valid = data.MftValidDataLength as u64;
        let ok = read_runs(letter, &mft_runs, cluster, valid, &mut |at, bytes| {
            for (k, chunk) in bytes.chunks_exact_mut(record).enumerate() {
                let number = at / record as u64 + k as u64;
                if number == mft::SECURE {
                    secure = Some(chunk.to_vec());
                }
                if let Some(rec) = mft::parse(chunk) {
                    catalog.take(number, rec);
                }
            }
        });
        if !ok {
            return Err("the MFT would not read");
        }
        note.mft_mib = valid as f64 / 1048576.0;
        note.mft_s = t.elapsed().as_secs_f64();

        let mut left = Vec::new();
        let mut items = Vec::new();
        let mut seen = HashSet::new();
        for (index, number, rec) in catalog.resolve(letter, &wanted) {
            seen.insert(index);
            let fresh = recent.contains(&number);
            note.recent += usize::from(fresh);
            let place = (!fresh)
                .then(|| place_of(&rec, cluster, max_size, cut))
                .flatten();
            note.not_placed += usize::from(!fresh && place.is_none());
            match (place, rec.security_id) {
                (Some(place), Some(sid)) => items.push(Item {
                    index,
                    record: number,
                    sid,
                    size: rec.size,
                    place,
                }),
                _ => left.push(index),
            }
        }
        left.extend(paths.iter().map(|&(i, _)| i).filter(|i| !seen.contains(i)));
        note.found = seen.len();
        note.asked = paths.len();

        // The descriptors the files name, from `$Secure:$SDS`.
        let (sds_runs, sds_len) = mft::named_runs(&mut secure.ok_or("no $Secure record")?, "$SDS")
            .ok_or("no $SDS stream to follow")?;
        let mut stream = vec![0u8; sds_len as usize];
        let ok = read_runs(letter, &sds_runs, cluster, sds_len, &mut |at, bytes| {
            stream[at as usize..at as usize + bytes.len()].copy_from_slice(bytes);
        });
        if !ok {
            return Err("$SDS would not read");
        }
        let ids: HashSet<u32> = items.iter().map(|it| it.sid).collect();
        let descriptors: HashMap<u32, Vec<u64>> = sds::wanted(&stream, &ids)
            .into_iter()
            .map(|(id, sd)| {
                let mut words = vec![0u64; sd.len().div_ceil(8)];
                // SAFETY: `words` holds at least `sd.len()` bytes.
                unsafe {
                    std::ptr::copy_nonoverlapping(
                        sd.as_ptr(),
                        words.as_mut_ptr() as *mut u8,
                        sd.len(),
                    )
                };
                (id, words)
            })
            .collect();
        note.descriptors = descriptors.len();
        Ok(Plan {
            volume,
            letter,
            cluster,
            journal: jrnl.UsnJournalID,
            began,
            items,
            descriptors,
            left,
            read: Vec::new(),
            note,
        })
    }

    /// On the thread that reads, now as the asker: the files whose data the
    /// asker's token may not read go the normal way, which answers for them.
    /// False when the token cannot be had: nothing is read raw.
    pub(crate) fn allow(&mut self) -> bool {
        let Some(token) = token() else { return false };
        let mapping = GENERIC_MAPPING {
            GenericRead: FILE_GENERIC_READ,
            GenericWrite: FILE_GENERIC_WRITE,
            GenericExecute: FILE_GENERIC_EXECUTE,
            GenericAll: FILE_ALL_ACCESS,
        };
        let mut verdict: HashMap<u32, bool> = HashMap::new();
        for (&id, sd) in &self.descriptors {
            let p = sd.as_ptr() as *mut c_void;
            let mut privileges = [0u64; 32];
            let mut plen = (privileges.len() * 8) as u32;
            let (mut granted, mut status) = (0u32, 0);
            // SAFETY: a self-relative descriptor checked valid first; buffers
            // of the sizes given; a token for identification.
            let allowed = unsafe {
                IsValidSecurityDescriptor(p) != 0
                    && AccessCheck(
                        p,
                        token.0,
                        FILE_READ_DATA,
                        &mapping,
                        privileges.as_mut_ptr() as *mut _,
                        &mut plen,
                        &mut granted,
                        &mut status,
                    ) != 0
                    && status != 0
            };
            if !allowed && self.note.check_error == 0 {
                // SAFETY: no call in between.
                self.note.check_error = unsafe { GetLastError() }.max(1);
            }
            verdict.insert(id, allowed);
        }
        let (keep, refused): (Vec<Item>, Vec<Item>) = std::mem::take(&mut self.items)
            .into_iter()
            .partition(|it| verdict.get(&it.sid).copied().unwrap_or(false));
        self.note.refused = refused.len();
        self.left.extend(refused.iter().map(|it| it.index));
        self.items = keep;
        true
    }

    /// Every file's bytes whole, in the order they lie, to `deliver`; when
    /// it says false nothing more is started.
    pub(crate) fn read(&mut self, deliver: &mut dyn FnMut(usize, &[u8]) -> bool) {
        let t = Instant::now();
        let items = std::mem::take(&mut self.items);
        let mut going = true;
        let mut pieces = Vec::new();
        for (k, it) in items.iter().enumerate() {
            match &it.place {
                Place::Here(bytes) if going => {
                    going = deliver(it.index, bytes);
                    self.read.push((it.index, it.record));
                }
                Place::Here(_) => self.left.push(it.index),
                Place::Runs(runs) => pieces_of(k, runs, self.cluster, it.size, &mut pieces),
            }
        }
        pieces.sort_by_key(|p| p.lcn);
        let mut pending = vec![0usize; items.len()];
        for p in &pieces {
            pending[p.file] += 1;
        }
        let mut whole: Vec<Option<Vec<u8>>> = vec![None; items.len()];
        let mut failed = vec![false; items.len()];
        let mut taken = vec![false; items.len()];
        let spans = spans_of(&pieces, self.cluster);
        self.note.spans = spans.len();
        let cluster = self.cluster;
        let mut bytes = 0u64;
        let queue = going
            .then(|| Queue::open(self.letter, DEPTH, (SPAN.max(PIECE)) as usize))
            .flatten();
        if let Some(queue) = queue {
            queue.run(&spans, cluster, &mut |data, (start, _, i, j)| {
                bytes += data.map_or(0, |d| d.len() as u64);
                for p in &pieces[i..j] {
                    let Some(data) = data else {
                        failed[p.file] = true;
                        continue;
                    };
                    let it = &items[p.file];
                    let buf = whole[p.file].get_or_insert_with(|| vec![0u8; it.size as usize]);
                    let from = ((p.lcn - start) * cluster) as usize;
                    let at = p.offset as usize;
                    let n = ((p.clusters * cluster) as usize).min(buf.len() - at);
                    buf[at..at + n].copy_from_slice(&data[from..from + n]);
                    pending[p.file] -= 1;
                    if pending[p.file] == 0 && !failed[p.file] && going {
                        let buf = whole[p.file].take().unwrap_or_default();
                        going = deliver(it.index, &buf);
                        taken[p.file] = true;
                    }
                    if pending[p.file] == 0 {
                        whole[p.file] = None;
                    }
                }
                going
            });
        }
        for (k, it) in items.iter().enumerate() {
            if !matches!(it.place, Place::Runs(_)) {
                continue;
            }
            if taken[k] {
                self.read.push((it.index, it.record));
            } else {
                self.left.push(it.index);
            }
        }
        self.note.read_mib = bytes as f64 / 1048576.0;
        self.note.read_s = t.elapsed().as_secs_f64();
    }

    /// After reading: a file the journal says changed meanwhile goes the
    /// normal way, and so does everything if the journal cannot say.
    pub(crate) fn settle(mut self) -> Settled {
        let changed: Option<HashSet<u64>> =
            journal_from(self.volume.0, self.journal, self.began, None)
                .filter(|_| {
                    query_journal(self.volume.0).is_some_and(|j| j.UsnJournalID == self.journal)
                })
                .map(|c| c.into_iter().map(|c| c.record).collect());
        let mut out = Settled {
            left: std::mem::take(&mut self.left),
            ..Settled::default()
        };
        for (index, record) in std::mem::take(&mut self.read) {
            match &changed {
                Some(set) if !set.contains(&record) => out.read.push(index),
                _ => out.left.push(index),
            }
        }
        if std::env::var_os("SCOUR_GREP_RAW_LOG").is_some() {
            let n = &self.note;
            eprintln!(
                "raw {}: asked {} found {} read {} left {} journal {} {:.3} s recent {} not_placed {} descriptors {} refused {} check_error {} mft {:.1} MiB {:.3} s spans {} read {:.1} MiB {:.3} s",
                self.letter,
                n.asked,
                n.found,
                out.read.len(),
                out.left.len(),
                n.journal,
                n.journal_s,
                n.recent,
                n.not_placed,
                n.descriptors,
                n.refused,
                n.check_error,
                n.mft_mib,
                n.mft_s,
                n.spans,
                n.read_mib,
                n.read_s
            );
        }
        out
    }
}

/// The token to check access with: the thread's while it is someone else,
/// else the process's own, made fit to be checked against.
fn token() -> Option<Owned> {
    let mut t: HANDLE = std::ptr::null_mut();
    // SAFETY: out-pointers to locals; OpenAsSelf, so SYSTEM opens it.
    unsafe {
        if OpenThreadToken(GetCurrentThread(), TOKEN_QUERY, 1, &mut t) != 0 {
            return Some(Owned(t));
        }
        let mut p: HANDLE = std::ptr::null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_DUPLICATE | TOKEN_QUERY, &mut p) == 0 {
            return None;
        }
        let p = Owned(p);
        (DuplicateToken(p.0, SecurityIdentification, &mut t) != 0).then_some(Owned(t))
    }
}

/// Spans in flight at once on a volume opened for overlapped reads, each
/// with its own buffer and event; taken back in the order handed out.
struct Queue {
    volume: Owned,
    slots: Vec<Slot>,
}

struct Slot {
    buf: Aligned,
    over: Box<OVERLAPPED>,
    event: Owned,
}

type Span = (u64, u64, usize, usize);

impl Queue {
    fn open(letter: char, depth: usize, cap: usize) -> Option<Queue> {
        let volume = open_volume(letter, FILE_FLAG_OVERLAPPED)?;
        let mut slots = Vec::with_capacity(depth);
        for _ in 0..depth {
            // SAFETY: a manual-reset event, unnamed.
            let event = unsafe { CreateEventW(std::ptr::null(), 1, 0, std::ptr::null()) };
            if event.is_null() {
                return None;
            }
            slots.push(Slot {
                buf: Aligned::new(cap),
                // SAFETY: OVERLAPPED is plain data.
                over: Box::new(unsafe { std::mem::zeroed() }),
                event: Owned(event),
            });
        }
        Some(Queue { volume, slots })
    }

    /// Every span read and handed to `take` — None for one that failed —
    /// until `take` says false.
    fn run(
        mut self,
        spans: &[Span],
        cluster: u64,
        take: &mut dyn FnMut(Option<&[u8]>, Span) -> bool,
    ) {
        let mut free: Vec<usize> = (0..self.slots.len()).collect();
        let mut flying: VecDeque<(usize, Span, usize, bool)> = VecDeque::new();
        let (mut next, mut going) = (0usize, true);
        while (going && next < spans.len()) || !flying.is_empty() {
            while going && next < spans.len() {
                let Some(k) = free.pop() else { break };
                let span = spans[next];
                let len = ((span.1 - span.0) * cluster) as usize;
                let issued = self.issue(k, span.0 * cluster, len);
                flying.push_back((k, span, len, issued));
                next += 1;
            }
            let Some((k, span, len, issued)) = flying.pop_front() else {
                break;
            };
            let slot = &mut self.slots[k];
            let mut got = 0u32;
            // SAFETY: the OVERLAPPED of a read issued on this handle, waited for.
            let ok = issued
                && unsafe { GetOverlappedResult(self.volume.0, &*slot.over, &mut got, 1) } != 0;
            let data = (ok && got as usize == len).then(|| &slot.buf.bytes()[..len]);
            going = take(data, span) && going;
            free.push(k);
        }
    }

    fn issue(&mut self, k: usize, offset: u64, len: usize) -> bool {
        let slot = &mut self.slots[k];
        // SAFETY: plain data, reset for this read.
        *slot.over = unsafe { std::mem::zeroed() };
        slot.over.Anonymous.Anonymous.Offset = offset as u32;
        slot.over.Anonymous.Anonymous.OffsetHigh = (offset >> 32) as u32;
        slot.over.hEvent = slot.event.0;
        let buf = slot.buf.bytes();
        // SAFETY: the buffer and OVERLAPPED stay put until the read is waited for.
        let ok = unsafe {
            ReadFile(
                self.volume.0,
                buf.as_mut_ptr(),
                len as u32,
                std::ptr::null_mut(),
                &mut *slot.over,
            )
        };
        // SAFETY: no call in between.
        ok != 0 || unsafe { GetLastError() } == ERROR_IO_PENDING
    }
}

impl Drop for Queue {
    fn drop(&mut self) {
        // A read still in flight writes into a buffer about to be freed:
        // each one is waited for first.
        for slot in &self.slots {
            let mut got = 0u32;
            // SAFETY: waits on a request of this handle, or returns at once.
            unsafe { GetOverlappedResult(self.volume.0, &*slot.over, &mut got, 1) };
        }
    }
}
