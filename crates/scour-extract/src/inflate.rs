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
                // Single-call SIMD fast-path (`inflate_fast` with `Finish`): avoids
                // 32 KiB sliding-window copies inside `zlib-rs`.
                let need = (exact + 64).max(64 << 10).min(cap + 64);
                if out.capacity() < need {
                    out.reserve(need - out.len());
                }
                if let Ok(Status::StreamEnd) =
                    inflate.decompress_vec(comp, out, FlushDecompress::Finish)
                {
                    if out.len() > cap {
                        out.truncate(cap);
                    }
                    return true;
                }
                out.clear();
                inflate.reset(false);
            }
            // Not `clamp`: near the end of an archive's budget the cap is under
            // 64 KiB, and a clamp whose floor passes its ceiling panics.
            let init = comp
                .len()
                .saturating_mul(3)
                .max(64 << 10)
                .min(cap.min(1 << 20));
            if out.capacity() < init {
                out.reserve(init - out.len());
            }
            let mut in_pos = 0usize;
            loop {
                if out.len() >= cap {
                    return true;
                }
                if out.capacity() == out.len() {
                    let add = out.len().max(32 << 10).min(cap - out.len());
                    out.reserve(add);
                }
                let before_in = inflate.total_in();
                let status = inflate.decompress_vec(&comp[in_pos..], out, FlushDecompress::None);
                let consumed = (inflate.total_in() - before_in) as usize;
                in_pos += consumed;
                match status {
                    Ok(Status::StreamEnd) => return true,
                    Ok(_) if consumed == 0 && out.len() == out.capacity() => continue,
                    Ok(_) if consumed == 0 => return !out.is_empty(),
                    Ok(_) => {}
                    Err(_) => return false,
                }
            }
        }
        _ => false,
    }
}
