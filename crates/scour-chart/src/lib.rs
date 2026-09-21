//! The numbers behind a picture, shared by every face: shares that add up to a
//! hundred, a bar cut into segments, a ring cut into arcs, block-character bars
//! for a terminal. No colours and no drawing — what each face paints is its own,
//! but the share it paints is decided here, once.

mod blocks;
mod fold;
mod ring;
mod share;

pub use blocks::{EIGHTHS, FULL, bar, segment_cells};
pub use fold::{Folded, Rest, fold};
pub use ring::{Arc, Dash, arcs, dashes};
pub use share::{Segment, segments, shares};
