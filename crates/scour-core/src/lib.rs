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
    AGE_BANDS, ApplyReport, Ast, Caps, Change, Cmp, Completion, CompletionKind, DirUsage, Entry,
    EntryId, Error, Facet, FacetBy, FacetGroup, FacetRequest, FacetResponse, Group, Hit,
    IndexStats, Key, Kind, MaintReport, Maintenance, Match, Meta, NumField, Page, PrefixSet,
    Result, Role, ScanOptions, ScanReport, ScanRequest, SearchRequest, SearchResponse, SortKey,
    SourceId, SourceInfo, SourceKind, Span, Status, Subtree, TimeField, TreeNode, UsageRequest,
    UsageResponse, ext_of, ext_str, kind_of, mode_string, path_digest, runs_when_opened, under,
};

/// Say something, and carry on if nobody is listening.
///
/// **`eprintln!` panics when the write fails**, and a long-lived service has a
/// stderr that can go away at any moment: a pipe whose reader exits, a terminal
/// that closes, a shell that moves on. Reproduced exactly that way — the daemon
/// was started with its output piped through `head`, the reader left, and the
/// next line the worker printed killed the worker thread. Nothing announced it.
/// The service went on answering searches from a frozen index at **no CPU at
/// all**, which reads as the best result anyone had measured all evening and
/// was a corpse.
///
/// Rust already ignores `SIGPIPE`, so the write returns `EPIPE` rather than
/// ending the process; what was left to remove is the panic. A thread that
/// cannot say what it is doing has to keep doing it — the line is a courtesy
/// and the work is the job.
#[macro_export]
macro_rules! note {
    ($($arg:tt)*) => {{
        use std::io::Write as _;
        let _ = writeln!(std::io::stderr(), $($arg)*);
    }};
}
