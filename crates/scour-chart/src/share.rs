//! Shares of a whole: percentages that add up, and where each part starts.

/// Percentages to `decimals` places that sum to exactly 100 — the largest
/// remainders take the units rounding left over, so a legend never says 99.9.
/// Every value zero gives every share zero.
pub fn shares(values: &[u64], decimals: u8) -> Vec<f64> {
    let total: u128 = values.iter().map(|&v| v as u128).sum();
    if total == 0 {
        return vec![0.0; values.len()];
    }
    let scale = 10u128.pow(decimals as u32);
    let whole = 100 * scale;
    // Units of one hundredth-of-a-percent (at two decimals), floored.
    let mut units: Vec<u128> = values.iter().map(|&v| v as u128 * whole / total).collect();
    let mut remainders: Vec<(u128, usize)> = values
        .iter()
        .enumerate()
        .map(|(i, &v)| ((v as u128 * whole) % total, i))
        .collect();
    let short = whole - units.iter().sum::<u128>();
    // The largest remainder first; on a tie the earlier value, which is the larger
    // one when the caller has sorted, so the biggest slice is the one that grows.
    remainders.sort_by_key(|&(r, i)| (std::cmp::Reverse(r), i));
    for (_, i) in remainders.iter().take(short as usize) {
        units[*i] += 1;
    }
    units.into_iter().map(|u| u as f64 / scale as f64).collect()
}

/// One piece of a bar, as fractions of its whole length.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Segment {
    pub start: f64,
    pub width: f64,
}

/// A bar cut in the order given; exact fractions, not rounded, so widths meet.
pub fn segments(values: &[u64]) -> Vec<Segment> {
    let total: u128 = values.iter().map(|&v| v as u128).sum();
    let mut at = 0.0;
    values
        .iter()
        .map(|&v| {
            let width = if total == 0 {
                0.0
            } else {
                v as f64 / total as f64
            };
            let seg = Segment { start: at, width };
            at += width;
            seg
        })
        .collect()
}
