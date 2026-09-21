//! Bars out of block characters, for a terminal.

pub const FULL: char = '█';
/// The partial cell, in eighths: index 0 is empty, 7 is seven eighths.
pub const EIGHTHS: [char; 8] = [' ', '▏', '▎', '▍', '▌', '▋', '▊', '▉'];

/// `fraction` of `width` cells, full blocks then one partial to the nearest
/// eighth; anything above zero shows at least one eighth. Padded with spaces
/// to `width`, so bars in a column line up.
pub fn bar(fraction: f64, width: usize) -> String {
    let fraction = fraction.clamp(0.0, 1.0);
    let eighths = (fraction * width as f64 * 8.0).round() as usize;
    let eighths = if fraction > 0.0 { eighths.max(1) } else { 0 };
    let full = (eighths / 8).min(width);
    let part = eighths % 8;
    let mut out = String::with_capacity(width * 3);
    out.extend(std::iter::repeat_n(FULL, full));
    if full < width && part > 0 {
        out.push(EIGHTHS[part]);
    }
    let drawn = full + usize::from(full < width && part > 0);
    out.extend(std::iter::repeat_n(' ', width - drawn));
    out
}

/// A whole bar of `width` cells cut into segments: cell counts that sum to
/// `width`, the largest remainders taking the cells left over. A share too small
/// for a cell gets none — the legend carries its number.
pub fn segment_cells(values: &[u64], width: usize) -> Vec<usize> {
    let total: u128 = values.iter().map(|&v| v as u128).sum();
    if total == 0 || width == 0 {
        return vec![0; values.len()];
    }
    let w = width as u128;
    let mut cells: Vec<usize> = values
        .iter()
        .map(|&v| (v as u128 * w / total) as usize)
        .collect();
    let mut remainders: Vec<(u128, usize)> = values
        .iter()
        .enumerate()
        .map(|(i, &v)| ((v as u128 * w) % total, i))
        .collect();
    remainders.sort_by_key(|&(r, i)| (std::cmp::Reverse(r), i));
    let short = width - cells.iter().sum::<usize>();
    for (_, i) in remainders.iter().take(short) {
        cells[*i] += 1;
    }
    cells
}
