//! Numbers of one width, packed bit to bit.
//!
//! A row number in a segment of five million rows is 23 bits, not 32. Reading
//! one is an unaligned eight-byte load, a shift and a mask: a width is at most
//! [`MAX_WIDTH`], so a value and its seven bits of slack fit the load.

/// The widest value: 64 bits less the seven a value may begin after.
pub(crate) const MAX_WIDTH: u32 = 57;

/// The bits it takes to tell `count` things apart, and never none.
pub(crate) fn width_for(count: usize) -> u32 {
    (usize::BITS - count.saturating_sub(1).leading_zeros()).max(1)
}

/// The bytes `count` values of `width` bits occupy.
pub(crate) fn len(count: usize, width: u32) -> Option<usize> {
    Some(count.checked_mul(width as usize)?.div_ceil(8))
}

/// Append `values`, each `width` bits, low bits first. A value wider than
/// that loses its high bits; the caller chose the width for its values.
pub(crate) fn pack(values: impl IntoIterator<Item = u64>, width: u32, out: &mut Vec<u8>) {
    debug_assert!((1..=MAX_WIDTH).contains(&width));
    let mask = (1u64 << width) - 1;
    // Fewer than eight bits are held between values, so the next always fits.
    let (mut acc, mut held) = (0u64, 0u32);
    for v in values {
        acc |= (v & mask) << held;
        held += width;
        while held >= 8 {
            out.push(acc as u8);
            acc >>= 8;
            held -= 8;
        }
    }
    if held > 0 {
        out.push(acc as u8);
    }
}

/// The `i`-th value, or nothing where the bytes end before it does.
#[inline(always)]
pub(crate) fn get(bytes: &[u8], width: u32, i: usize) -> Option<u64> {
    // Sixty-four bits of bit offset, whatever a pointer is: no overflow to ask.
    let bit = i as u64 * u64::from(width);
    let (at, shift) = ((bit / 8) as usize, (bit % 8) as u32);
    let mask = (1u64 << width) - 1;
    if let Some(word) = bytes.get(at..at + 8) {
        let word = u64::from_le_bytes([
            word[0], word[1], word[2], word[3], word[4], word[5], word[6], word[7],
        ]);
        return Some(word >> shift & mask);
    }
    // The last few values: the file ends with the value, not with padding.
    let tail = bytes.get(at..)?;
    if tail.len() * 8 < (shift + width) as usize {
        return None;
    }
    let mut word = [0u8; 8];
    word[..tail.len()].copy_from_slice(tail);
    Some(u64::from_le_bytes(word) >> shift & mask)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_width_tells_that_many_things_apart() {
        for (count, want) in [
            (0, 1),
            (1, 1),
            (2, 1),
            (3, 2),
            (4, 2),
            (5, 3),
            (256, 8),
            (257, 9),
        ] {
            assert_eq!(width_for(count), want, "{count}");
        }
        assert_eq!(width_for(5_056_158), 23);
    }

    #[test]
    fn every_value_comes_back_at_every_width() {
        let mut state: u64 = 0x243f_6a88_85a3_08d3;
        for width in 1..=MAX_WIDTH {
            let mask = (1u64 << width) - 1;
            let values: Vec<u64> = (0..301)
                .map(|i| {
                    state = state
                        .wrapping_mul(6_364_136_223_846_793_005)
                        .wrapping_add(1);
                    match i % 7 {
                        0 => mask,
                        1 => 0,
                        _ => (state >> 5) & mask,
                    }
                })
                .collect();
            let mut bytes = Vec::new();
            pack(values.iter().copied(), width, &mut bytes);
            assert_eq!(Some(bytes.len()), len(values.len(), width), "width {width}");
            for (i, want) in values.iter().enumerate() {
                assert_eq!(get(&bytes, width, i), Some(*want), "width {width} at {i}");
            }
            assert_eq!(get(&bytes, width, values.len() + 8), None);
            // Cut anywhere: a value is whole or it is not there.
            for cut in 0..bytes.len() {
                for (i, want) in values.iter().enumerate() {
                    let got = get(&bytes[..cut], width, i);
                    let whole = (i + 1) * width as usize <= cut * 8;
                    assert_eq!(
                        got,
                        whole.then_some(*want),
                        "width {width} cut {cut} at {i}"
                    );
                }
            }
        }
    }
}
