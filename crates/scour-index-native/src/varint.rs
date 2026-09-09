//! Variable-length integers, and frame-of-reference bit packing.

/// Append `v` as a LEB128-style varint.
pub fn put(out: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        out.push((v as u8) | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

/// Read a varint, returning it and how many bytes it took.
pub fn get(bytes: &[u8]) -> Option<(u64, usize)> {
    let mut v = 0u64;
    let mut shift = 0u32;
    for (i, &b) in bytes.iter().enumerate() {
        // Ten bytes is the most a u64 can need; past that the shift is undefined.
        if shift > 63 {
            return None;
        }
        v |= u64::from(b & 0x7f) << shift;
        if b < 0x80 {
            return Some((v, i + 1));
        }
        shift += 7;
    }
    None
}

/// How many bits hold every value in `vals` once the smallest is subtracted.
/// A block of timestamps spanning an hour needs sixteen bits, not sixty-four.
pub fn width_for(vals: &[i64]) -> (i64, u32) {
    let Some(&min) = vals.iter().min() else {
        return (0, 0);
    };
    let max_offset = vals
        .iter()
        .map(|v| v.wrapping_sub(min) as u64)
        .max()
        .unwrap_or(0);
    (min, 64 - max_offset.leading_zeros())
}

/// Pack `vals` at `bits` each, least significant bit first.
pub fn pack(out: &mut Vec<u8>, vals: &[i64], min: i64, bits: u32) {
    if bits == 0 {
        return;
    }
    let mut acc: u128 = 0;
    let mut held = 0u32;
    for &v in vals {
        acc |= u128::from(v.wrapping_sub(min) as u64) << held;
        held += bits;
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

/// The `i`-th value of a packed block. Random access rather than bulk decode: a
/// filter usually rejects a row on the first column it reads.
pub fn unpack_one(bytes: &[u8], min: i64, bits: u32, i: usize) -> i64 {
    if bits == 0 {
        return min;
    }
    let start_bit = i * bits as usize;
    let start_byte = start_bit / 8;
    let offset = (start_bit % 8) as u32;
    let mut acc: u128 = 0;
    // `bits` is at most 64, so with the offset this spans at most nine bytes.
    for (n, &b) in bytes[start_byte..].iter().take(9).enumerate() {
        acc |= u128::from(b) << (n * 8);
    }
    let mask = if bits >= 64 {
        u64::MAX
    } else {
        (1u64 << bits) - 1
    };
    let raw = ((acc >> offset) as u64) & mask;
    min.wrapping_add(raw as i64)
}

/// Bytes a packed block of `n` values at `bits` each occupies. Not on any read
/// path — a reader finds a block by its offset; the round-trip tests use it.
#[allow(dead_code)]
pub fn packed_len(n: usize, bits: u32) -> usize {
    (n * bits as usize).div_ceil(8)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn varints_round_trip() {
        let mut buf = Vec::new();
        let values = [
            0u64,
            1,
            127,
            128,
            300,
            16_383,
            16_384,
            u32::MAX as u64,
            u64::MAX,
        ];
        for v in values {
            put(&mut buf, v);
        }
        let mut at = 0;
        for want in values {
            let (got, used) = get(&buf[at..]).expect("decode");
            assert_eq!(got, want);
            at += used;
        }
        assert_eq!(at, buf.len());
    }

    #[test]
    fn a_truncated_or_absurd_varint_is_refused_rather_than_wrapping() {
        assert_eq!(
            get(&[0x80]),
            None,
            "a continuation byte with nothing after it"
        );
        assert_eq!(get(&[]), None);
        // Eleven continuation bytes cannot describe a u64.
        assert_eq!(get(&[0xff; 11]), None);
    }

    #[test]
    fn packing_round_trips_at_every_width() {
        // Widths matter one at a time: 0 (all equal), 1, 7 (crosses no byte),
        // 8 (aligned), 13 (crosses), 64 (no headroom left).
        for vals in [
            vec![5i64; 16],
            vec![0, 1, 0, 1, 1, 0, 1, 0],
            (0..40).map(|i| 1_000_000 + i).collect(),
            (0..40).map(|i| i * 7919).collect(),
            vec![i64::MIN, 0, i64::MAX],
        ] {
            let (min, bits) = width_for(&vals);
            let mut buf = Vec::new();
            pack(&mut buf, &vals, min, bits);
            assert_eq!(
                buf.len(),
                packed_len(vals.len(), bits),
                "length disagrees at {bits} bits"
            );
            for (i, &want) in vals.iter().enumerate() {
                assert_eq!(
                    unpack_one(&buf, min, bits, i),
                    want,
                    "value {i} at {bits} bits"
                );
            }
        }
    }

    #[test]
    fn a_block_of_one_repeated_value_costs_nothing() {
        // What makes uid and gid free on a single-user machine.
        let vals = vec![1000i64; 128];
        let (min, bits) = width_for(&vals);
        assert_eq!((min, bits), (1000, 0));
        assert_eq!(packed_len(vals.len(), bits), 0);
        assert_eq!(unpack_one(&[], min, bits, 57), 1000);
    }

    #[test]
    fn sorted_values_pack_tightly() {
        // Why `mtime` costs 0.29 bytes an entry: rows are already in its order.
        let base = 1_785_000_000i64;
        let vals: Vec<i64> = (0..128).map(|i| base - i * 3).collect();
        let (_, bits) = width_for(&vals);
        assert!(
            bits <= 9,
            "a block of ordered timestamps should need ~9 bits, got {bits}"
        );
    }
}
