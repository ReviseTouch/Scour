//! An entry inflated whole, up to a cap: stored entries copied, deflated ones
//! through one reusable inflater.

use flate2::{Decompress, FlushDecompress, Status};

/// `comp` inflated into `out` by `method` (0 stored, 8 deflate), at most `cap`
/// bytes; `uncomp_hint` is the size the directory claims, used only to size
/// the buffer. False for another method, or a stream that does not inflate.
pub fn inflate_whole(
    inflate: &mut Decompress,
    method: u16,
    comp: &[u8],
    uncomp_hint: u64,
    out: &mut Vec<u8>,
    cap: usize,
) -> bool {
    out.clear();
    match method {
        0 => {
            if comp.len() > cap {
                return false;
            }
            out.extend_from_slice(comp);
            true
        }
        8 => {
            inflate.reset(false);
            let want = usize::try_from(uncomp_hint)
                .ok()
                .filter(|&n| n > 0 && n <= cap);
            if let Some(exact) = want {
                // One call, `Finish`: `zlib-rs`'s fast path, no window copies.
                let need = (exact + 64).min(cap + 64);
                if let Ok(Status::StreamEnd) = into_window(inflate, comp, out, need, true) {
                    out.truncate(cap);
                    return true;
                }
                out.clear();
                inflate.reset(false);
            }
            let mut in_pos = 0usize;
            loop {
                if out.len() >= cap {
                    out.truncate(cap);
                    return true;
                }
                let room = out.len().max(64 << 10).min(cap - out.len());
                let (before_in, before_len) = (inflate.total_in(), out.len());
                let status = into_window(inflate, &comp[in_pos..], out, room, false);
                let consumed = (inflate.total_in() - before_in) as usize;
                in_pos += consumed;
                match status {
                    Ok(Status::StreamEnd) => return true,
                    // Neither in nor out moved: the stream is cut short.
                    Ok(_) if consumed == 0 && out.len() == before_len => return !out.is_empty(),
                    Ok(_) => {}
                    Err(_) => return false,
                }
            }
        }
        _ => false,
    }
}

/// Inflate into at most `room` more bytes of `out`. The inflater is handed
/// exactly that window: given a buffer's whole spare capacity it costs time
/// in proportion to it, and a buffer kept between entries grows to the
/// largest entry seen — a 1 KiB part read after a 30 MiB sheet paid for 30.
fn into_window(
    inflate: &mut Decompress,
    comp: &[u8],
    out: &mut Vec<u8>,
    room: usize,
    finish: bool,
) -> Result<Status, flate2::DecompressError> {
    let start = out.len();
    out.resize(start + room, 0);
    let before = inflate.total_out();
    let flush = if finish {
        FlushDecompress::Finish
    } else {
        FlushDecompress::None
    };
    let status = inflate.decompress(comp, &mut out[start..], flush);
    out.truncate(start + (inflate.total_out() - before) as usize);
    status
}
