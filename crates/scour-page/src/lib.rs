//! Which page of a result to hold, and which one to ask for next: a window onto
//! a list of millions that no face actually has.
//!
//! Policy only, no drawing. [`Pages`] is generic over whatever a row is to its
//! owner, and says what changed rather than telling anybody to redraw.

mod pages;
pub use pages::*;

#[cfg(test)]
mod tests;
