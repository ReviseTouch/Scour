//! Everything else a face may ask of the index: facets, a folder's tree and
//! weight, duplicates, an export.

use super::*;
use scour_core::{
    Entry, Error, FacetRequest, FacetResponse, Page, Result, SearchRequest, SortKey, TreeNode,
};

/// How much CSV goes into one frame of an export. A frame is a line of JSON, so a
/// row per frame pays an envelope and a write each; 128 KB is about two thousand.
const EXPORT_CHUNK: usize = 128 * 1024;

impl Engine {
    /// The whole matching set, as CSV, in pieces; returns how many rows were written.
    /// `out` returns false to stop, which abandons the walk. Nothing grows with it.
    pub fn export(
        &self,
        query: &str,
        columns: &[String],
        mut out: impl FnMut(String) -> bool,
    ) -> Result<u64> {
        self.shared.reading();
        let sheet = if columns.is_empty() {
            scour_export::Sheet::new(scour_export::Sheet::default_columns())
        } else {
            scour_export::Sheet::new(columns.to_vec())
        };
        let mut buf: Vec<u8> = Vec::with_capacity(EXPORT_CHUNK + 4096);
        sheet.header(&mut buf);

        // A caller that went away is not a query that ran out of rows.
        let mut stopped = false;
        let mut flush = |buf: &mut Vec<u8>, stopped: &mut bool| {
            if buf.is_empty() {
                return true;
            }
            // UTF-8 by construction: rows are built from `String` paths.
            let text = String::from_utf8_lossy(buf).into_owned();
            buf.clear();
            if out(text) {
                true
            } else {
                *stopped = true;
                false
            }
        };

        let mut wrote = 0u64;
        let scanned = self.shared.index.scan(
            &scour_core::ScanRequest {
                query: self.shared.ast(query),
            },
            &mut |hit| {
                sheet.row(hit, &mut buf);
                wrote += 1;
                if buf.len() >= EXPORT_CHUNK {
                    return flush(&mut buf, &mut stopped);
                }
                true
            },
        )?;
        debug_assert_eq!(scanned, wrote);
        if !stopped {
            // The tail, and the header alone when nothing matched.
            flush(&mut buf, &mut stopped);
        }
        Ok(wrote)
    }

    /// Every facet question about one query, answered from one walk.
    pub fn facets(&self, query: &str, by: Vec<scour_core::FacetBy>) -> Result<FacetResponse> {
        self.shared.reading();
        let ast = self.shared.ast(query);
        let reads = crate::content::plan(&ast).is_some();
        // Of the candidates, when the query reads contents: what kinds hold
        // the text is not known until every one of them is read.
        let mut res = self.shared.index.facets(&FacetRequest {
            query: crate::content::without(ast.clone()),
            by,
        })?;
        // But the total is the list's length in every face, so it is the
        // reading's: the files found, not the files there are to read.
        if reads {
            (res.total, res.capped) = self.shared.contents.found(&ast);
        }
        res.misread = misread(query);
        Ok(res)
    }

    /// One entry, from the index if it is there and from the source if not: a file
    /// created a moment ago is on disk before it is indexed.
    pub fn stat(&self, path: &str) -> Result<Entry> {
        // Only an owning source may answer: `stat` is a bare `symlink_metadata`, so
        // any fallback would disclose every path on the machine.
        let idx = self.owner_of(path).ok_or_else(|| Error::NotFound {
            path: path.to_owned(),
        })?;
        self.shared
            .sources
            .get(idx)
            .ok_or_else(|| Error::NotFound {
                path: path.to_owned(),
            })?
            .stat(path)
    }

    pub fn tree(&self, path: &str, depth: u32, limit: u32) -> Result<TreeNode> {
        self.shared.reading();
        crate::tree::build(
            self.shared.index.as_ref(),
            path,
            depth,
            limit.min(self.shared.opts.result_limit),
        )
    }

    /// What a subtree weighs — all of it, or only the part a query names; an empty
    /// query is the `du` question. Parsed here, so a frontend sends only typed text.
    pub fn usage(&self, path: &str, top: u32, query: &str) -> Result<scour_core::UsageResponse> {
        self.shared.reading();
        self.shared.index.usage(&scour_core::UsageRequest {
            path: path.to_owned(),
            top,
            query: self.shared.ast(query),
        })
    }

    /// The same file, several times over. The query is built here, not taken, so
    /// nobody can ask for duplicate directories; `result_limit` would clamp it.
    pub fn duplicates(
        &self,
        under: &str,
        opts: &scour_dupes::Options,
    ) -> Result<scour_dupes::Report> {
        self.shared.reading();
        let mut query = format!("file: size:>={}", opts.min_size);
        if !under.is_empty() {
            // Quoted: an unquoted path with a space becomes two terms.
            query.push_str(&format!(" under:\"{under}\""));
        }
        let found = self.shared.index.search(&SearchRequest {
            query: scour_query::parse(&query),
            sort: SortKey::Size,
            descending: true,
            page: Page {
                offset: 0,
                limit: CANDIDATES,
                count_cap: CANDIDATES,
            },
        })?;
        Ok(scour_dupes::find_named(
            found.hits.into_iter().map(|h| {
                let (size, links) = (h.meta.size.max(0) as u64, h.meta.links.max(1) as u64);
                (h.path, size, links)
            }),
            opts,
        ))
    }
}
