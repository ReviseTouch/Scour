//! Answering a query by walking rows.
//!
//! No posting list: rows are stored newest-first, so the default view stops after
//! forty matches; a numeric order stops once the page's worst row beats any
//! unopened block ([`zone_order`]); text orders read stored row lists.

mod needle;
mod plan;
mod relevance;
mod sort;
mod test;
mod view;
mod walk;
pub use needle::*;
pub use plan::*;
use relevance::*;
pub use sort::*;
pub(crate) use test::*;
pub use view::*;
pub use walk::*;

#[cfg(test)]
mod tests;
