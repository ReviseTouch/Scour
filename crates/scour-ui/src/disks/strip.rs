//! The strip over the tiles: one segment per disk with something indexed on
//! it, as wide as its share of the files, a small one still visible.

use super::Tile;

/// The narrowest a segment is drawn, as a share of the strip.
pub const STRIP_FLOOR: f64 = 0.02;

/// One segment of the strip.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Segment {
    /// The tile it stands for, by index.
    pub at: usize,
    /// Its width: shares sum to 1.
    pub share: f64,
}

/// The strip of `tiles`, in their order: by file count, none under
/// [`STRIP_FLOOR`]. Equal shares while nothing has been counted.
pub fn strip(tiles: &[Tile]) -> Vec<Segment> {
    let on: Vec<(usize, f64)> = tiles
        .iter()
        .enumerate()
        .filter(|(_, t)| t.choosable())
        .map(|(i, t)| (i, t.files.unwrap_or(0) as f64))
        .collect();
    let n = on.len() as f64;
    let total: f64 = on.iter().map(|(_, f)| f).sum();
    if total <= 0.0 || n * STRIP_FLOOR >= 1.0 {
        return on
            .iter()
            .map(|&(at, _)| Segment { at, share: 1.0 / n })
            .collect();
    }
    // Lift the smallest to the floor and share what is left among the rest,
    // until no segment of the rest falls below it.
    let mut floored = vec![false; on.len()];
    loop {
        let rest: f64 = on
            .iter()
            .zip(&floored)
            .filter(|(_, f)| !**f)
            .map(|((_, c), _)| c)
            .sum();
        let left = 1.0 - STRIP_FLOOR * floored.iter().filter(|f| **f).count() as f64;
        let mut moved = false;
        for (k, (_, c)) in on.iter().enumerate() {
            if !floored[k] && (rest <= 0.0 || c / rest * left < STRIP_FLOOR) {
                floored[k] = true;
                moved = true;
            }
        }
        if !moved {
            return on
                .iter()
                .zip(&floored)
                .map(|(&(at, c), &f)| Segment {
                    at,
                    share: if f { STRIP_FLOOR } else { c / rest * left },
                })
                .collect();
        }
    }
}
