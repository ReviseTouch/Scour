//! Anything that can store entries and answer questions about them.

use std::fmt::Debug;

use crate::types::{
    ApplyReport, Change, FacetRequest, FacetResponse, IndexStats, MaintReport, Maintenance, Result,
    SearchRequest, SearchResponse,
};

/// An index over entries.
///
/// Everything an implementation is asked for arrives as a typed request built
/// from an [`Ast`], never as a string: lowering that tree into whatever the
/// engine actually speaks is the implementation's job, and it is the only
/// place that knows the engine exists.
///
/// Implementations must be usable from several threads at once. In practice
/// searches are concurrent and writes are not, which is why `apply` takes
/// `&self` and is expected to serialise internally rather than force every
/// caller to hold a lock. On Windows this is not optional: tantivy's
/// `commit()` races with itself and fails with `PermissionDenied`.
///
/// [`Ast`]: crate::types::Ast
pub trait Index: Send + Sync + Debug {
    /// Apply a stream of changes. Not durable until [`Index::commit`].
    ///
    /// Removals are expected to take effect for searches *immediately*, before
    /// the commit that erases them, so that deleting a directory does not leave
    /// its contents visible for the second or two until the next flush.
    fn apply(&self, changes: &mut dyn Iterator<Item = Change>) -> Result<ApplyReport>;

    /// Make everything applied so far durable and visible to new readers.
    ///
    /// Expensive — tens of milliseconds — which is why the engine batches
    /// rather than committing per change.
    fn commit(&self) -> Result<()>;

    fn search(&self, req: &SearchRequest) -> Result<SearchResponse>;

    fn facets(&self, req: &FacetRequest) -> Result<FacetResponse>;

    fn stats(&self) -> Result<IndexStats>;

    fn maintain(&self, level: Maintenance) -> Result<MaintReport>;
}
