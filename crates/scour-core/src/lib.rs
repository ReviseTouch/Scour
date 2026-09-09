//! The contract every part of Scour agrees on: [`types`] is the vocabulary —
//! entry, change, query, search — and [`traits`] the interfaces over them.
//!
//! No I/O, no search engine, no platform named. If an implementation crate
//! needs another implementation crate, the abstraction it needs belongs here.

pub mod text;
pub mod traits;
pub mod types;

pub use text::{Catalog, DefaultFolder, Folder};
pub use traits::{ChangeSink, ContentSink, EntrySink, Extractor, Flow, Index, Source, WatchHandle};
pub use types::{
    AGE_BANDS, ApplyReport, Ast, Caps, Change, Cmp, Completion, CompletionKind, DirUsage, Entry,
    EntryId, Error, Facet, FacetBy, FacetGroup, FacetRequest, FacetResponse, Group, Hit,
    IndexStats, Key, Kind, MaintReport, Maintenance, Match, Meta, NumField, Owner, PAGE_ROWS, Page,
    PrefixSet, Result, Role, ScanOptions, ScanReport, ScanRequest, SearchRequest, SearchResponse,
    SortKey, SourceId, SourceInfo, SourceKind, Span, Status, Subtree, TimeField, TreeNode,
    UsageRequest, UsageResponse, ext_of, ext_str, kind_of, mode_string, owner_name, path_digest,
    runs_when_opened, under,
};

/// Write a line to stderr, carrying on if the write fails.
///
/// `eprintln!` panics on a failed write, and a long-lived service's stderr can
/// vanish at any moment; a thread that cannot report must still do its work.
#[macro_export]
macro_rules! note {
    ($($arg:tt)*) => {{
        use std::io::Write as _;
        let _ = writeln!(std::io::stderr(), $($arg)*);
    }};
}
