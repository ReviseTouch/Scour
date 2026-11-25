//! Spans in flight at once: the volume opened for overlapped reads, `depth`
//! requests outstanding, each with its own buffer and event, so the disk's
//! own queue orders neighbours by where the platter is. The spans are
//! handed out in place order; they are taken back in that order too.

use std::collections::VecDeque;

use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_IO_PENDING, GENERIC_READ, GetLastError, HANDLE, INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAG_NO_BUFFERING, FILE_FLAG_OVERLAPPED, FILE_SHARE_READ, FILE_SHARE_WRITE,
    OPEN_EXISTING, ReadFile,
};
use windows_sys::Win32::System::IO::{GetOverlappedResult, OVERLAPPED};
use windows_sys::Win32::System::Threading::CreateEventW;

use crate::win::{aligned, wide};

pub struct Queue {
    volume: HANDLE,
    slots: Vec<Slot>,
}

struct Slot {
    buf: &'static mut [u8],
    over: Box<OVERLAPPED>,
    event: HANDLE,
}

type Span = (u64, u64, usize, usize);

impl Queue {
    pub fn open(letter: &str, depth: usize, cap: usize) -> Queue {
        let name = wide(&format!("\\\\.\\{letter}"));
        let volume = unsafe {
            CreateFileW(
                name.as_ptr(),
                GENERIC_READ,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                std::ptr::null(),
                OPEN_EXISTING,
                FILE_FLAG_NO_BUFFERING | FILE_FLAG_OVERLAPPED,
                std::ptr::null_mut(),
            )
        };
        assert!(volume != INVALID_HANDLE_VALUE, "the volume would not open for overlapped reads");
        let slots = (0..depth)
            .map(|_| Slot {
                buf: aligned(cap),
                over: Box::new(unsafe { std::mem::zeroed() }),
                event: unsafe { CreateEventW(std::ptr::null(), 1, 0, std::ptr::null()) },
            })
            .collect();
        Queue { volume, slots }
    }

    /// Every span read, each handed to `take` with its bytes; the bytes read.
    pub fn run(mut self, spans: &[Span], cluster: u64, base: u64, take: &mut dyn FnMut(&[u8], Span)) -> u64 {
        let mut free: Vec<usize> = (0..self.slots.len()).collect();
        let mut flying: VecDeque<(usize, Span, usize)> = VecDeque::new();
        let mut next = 0usize;
        let mut total = 0u64;
        while next < spans.len() || !flying.is_empty() {
            while next < spans.len()
                && let Some(k) = free.pop()
            {
                let span = spans[next];
                let len = ((span.1 - span.0) * cluster) as usize;
                self.issue(k, base + span.0 * cluster, len);
                flying.push_back((k, span, len));
                next += 1;
            }
            let Some((k, span, len)) = flying.pop_front() else { break };
            let slot = &mut self.slots[k];
            let mut got = 0u32;
            let ok = unsafe { GetOverlappedResult(self.volume, &*slot.over, &mut got, 1) };
            if ok == 0 || got as usize != len {
                eprintln!("an overlapped read failed (error {})", unsafe { GetLastError() });
            }
            total += len as u64;
            take(&slot.buf[..len], span);
            free.push(k);
        }
        total
    }

    fn issue(&mut self, k: usize, offset: u64, len: usize) {
        let slot = &mut self.slots[k];
        *slot.over = unsafe { std::mem::zeroed() };
        slot.over.Anonymous.Anonymous.Offset = offset as u32;
        slot.over.Anonymous.Anonymous.OffsetHigh = (offset >> 32) as u32;
        slot.over.hEvent = slot.event;
        let ok = unsafe {
            ReadFile(
                self.volume,
                slot.buf.as_mut_ptr(),
                len as u32,
                std::ptr::null_mut(),
                &mut *slot.over,
            )
        };
        if ok == 0 && unsafe { GetLastError() } != ERROR_IO_PENDING {
            eprintln!("an overlapped read was refused (error {})", unsafe { GetLastError() });
        }
    }
}

impl Drop for Queue {
    fn drop(&mut self) {
        for slot in &self.slots {
            unsafe { CloseHandle(slot.event) };
        }
        unsafe { CloseHandle(self.volume) };
    }
}
