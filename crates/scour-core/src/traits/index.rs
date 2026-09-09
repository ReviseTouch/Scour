//! Anything that can store entries and answer questions about them.

use std::fmt::Debug;

use crate::types::{
    ApplyReport, Change, Error, FacetRequest, FacetResponse, Hit, IndexStats, MaintReport,
    Maintenance, Result, ScanRequest, SearchRequest, SearchResponse, SourceId, UsageRequest,
    UsageResponse,
};

/// An index over entries. Requests arrive typed, built from an
/// [`Ast`](crate::types::Ast), never as a string. `&self` throughout: searches run
/// concurrently, and overlapping commits corrupt an index, so writes serialise.
pub trait Index: Send + Sync + Debug {
    /// Apply a stream of changes. Not durable until [`Index::commit`].
    ///
    /// Removals take effect for searches immediately, before that commit.
    fn apply(&self, changes: &mut dyn Iterator<Item = Change>) -> Result<ApplyReport>;

    /// Start a reconciliation pass and return its number. Every entry upserted
    /// afterwards is stamped with it; sweeping what is not stamped is how files
    /// deleted while nothing was watching are noticed.
    fn begin_generation(&self) -> Result<u64>;

    /// Remove everything **this source** has under `under` that is not stamped with
    /// `generation`; returns how many went. One call per finished scan with every root
    /// it vouched for — the first consumes the pass's notes — and rows under `spare`,
    /// the subtrees that walk could not enter, are left alone.
    fn sweep(
        &self,
        source: SourceId,
        under: &[String],
        generation: u64,
        spare: &crate::types::PrefixSet,
    ) -> Result<u64>;

    /// End a pass that will not be swept and throw away what it noted. A walk that
    /// could not look must not sweep, but must still end: notes left behind pin
    /// their segments against compaction for good.
    fn abandon_generation(&self, _generation: u64) -> Result<()> {
        Ok(())
    }

    /// Make everything applied so far durable and visible to new readers.
    /// Tens of milliseconds, so callers batch rather than commit per change.
    fn commit(&self) -> Result<()>;

    /// Remove everything one source ever put here. Returns how many rows went.
    /// For a source no longer configured: a sweep needs a walk, and a source that
    /// is gone will never walk again. Defaults to nothing removed.
    fn forget(&self, _source: SourceId) -> Result<u64> {
        Ok(0)
    }

    fn search(&self, req: &SearchRequest) -> Result<SearchResponse>;

    /// Every matching row, one at a time, for a caller that will not keep them.
    /// Unbounded, and not assembled from pages: a page costs a walk to its offset, so
    /// paging the set is quadratic. `f` returns false to stop; the order is arbitrary.
    fn scan(&self, _req: &ScanRequest, _f: &mut dyn FnMut(&Hit) -> bool) -> Result<u64> {
        Err(Error::unsupported("streaming the whole matching set"))
    }

    fn facets(&self, req: &FacetRequest) -> Result<FacetResponse>;

    /// What each of these folders weighs: bytes on disk, and how many files.
    /// Batched because one page holds many folders. Hard links count once, so this
    /// agrees with [`Index::usage`]. `None` is *not known*, which is not zero.
    fn subtree_sizes(&self, paths: &[String]) -> Result<Vec<Option<(u64, u64)>>> {
        Ok(vec![None; paths.len()])
    }

    fn stats(&self) -> Result<IndexStats>;

    fn maintain(&self, level: Maintenance) -> Result<MaintReport>;

    /// What a subtree weighs, and what is inside it. Defaults to a refusal rather
    /// than a walk; [`Caps`](crate::types::Caps) says whether an index answers it.
    fn usage(&self, _req: &UsageRequest) -> Result<UsageResponse> {
        Err(Error::unsupported("disk usage"))
    }
}
