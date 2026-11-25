//! What every Scour window agrees on: data, and nothing else. Each face turns
//! it into what its toolkit wants — CSS variables, Slint globals, escape codes
//! — and this crate has no dependencies. What is shared is the vocabulary,
//! never the layout: which colour means "a field name", how tall a row is.

mod bands;
mod columns;
mod palette;
mod said;
pub use bands::*;
pub use columns::*;
pub use palette::*;
pub use said::*;

pub mod calendar;
pub mod disks;
pub mod faces;
pub mod format;
pub mod path;
pub mod preview;

pub mod frame;
pub mod lens;
pub mod menu;
pub mod pattern;
pub mod query;
pub mod wrap;
