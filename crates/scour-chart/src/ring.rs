//! A ring cut into arcs, for a canvas that draws arcs and for one that draws
//! dashes along a circle.

/// Degrees, clockwise from twelve o'clock.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Arc {
    pub start: f64,
    pub sweep: f64,
}

/// One value per arc, in the order given; the sweeps add up to 360.
pub fn arcs(values: &[u64]) -> Vec<Arc> {
    crate::segments(values)
        .into_iter()
        .map(|s| Arc {
            start: s.start * 360.0,
            sweep: s.width * 360.0,
        })
        .collect()
}

/// A slice drawn as a dash of a stroked circle: `stroke-dasharray="{length} C"`
/// with `stroke-dashoffset="{offset}"`, where `C` is the circumference and the
/// circle is rotated so the dash starts at twelve o'clock.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Dash {
    pub length: f64,
    /// Negative and cumulative: how far along the circle this dash begins.
    pub offset: f64,
}

pub fn dashes(values: &[u64], circumference: f64) -> Vec<Dash> {
    crate::segments(values)
        .into_iter()
        .map(|s| Dash {
            length: s.width * circumference,
            offset: -(s.start * circumference),
        })
        .collect()
}
