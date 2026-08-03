//! The contract every part of Scour agrees on.
//!
//! This crate holds two things and nothing else:
//!
//! * [`types`] — the vocabulary. What an entry is, what a change is, what a
//!   query means, what a search asks for and answers with.
//! * [`traits`] — the interfaces. What it takes to be a source of entries, an
//!   index over them, an extractor of their content, a folder of their text.
//!
//! It performs no I/O, opens no files, knows no search engine and names no
//! platform. That restraint is the point: it is what lets the search engine,
//! the filesystem walker, the configuration format and the user interface each
//! be replaced without any of the others noticing.
//!
//! The rule, stated once so it can be pointed at: **if an implementation crate
//! needs another implementation crate, the abstraction it actually needs is
//! missing from here.** Add the trait; do not add the dependency.

pub mod text;
pub mod traits;
pub mod types;

pub use text::{Catalog, DefaultFolder, Folder};
pub use traits::{ChangeSink, ContentSink, EntrySink, Extractor, Flow, Index, Source, WatchHandle};
pub use types::{
    ApplyReport, Ast, Caps, Change, Cmp, Completion, CompletionKind, Entry, EntryId, Error, Facet,
    FacetBy, FacetRequest, FacetResponse, Group, Hit, IndexStats, Key, Kind, MaintReport,
    Maintenance, Match, Meta, Page, Result, Role, ScanOptions, ScanReport, SearchRequest,
    SearchResponse, SortKey, SourceId, SourceInfo, SourceKind, Span, Status, TimeField, TreeNode,
    ext_of, ext_str, kind_of, mode_string,
};
