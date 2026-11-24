//! A query answered: its page, its similar spellings, its folders weighed.

use super::*;
use scour_core::{Completion, Page, Result, SearchRequest, SearchResponse, SortKey, Span};
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

/// One reading of a query: what it means, what its pieces are, and what could
/// come next.
#[derive(Debug, Clone, Default)]
pub struct Explained {
    pub description: String,
    pub needs_content: bool,
    pub spans: Vec<Span>,
    pub completions: Vec<Completion>,
}

/// The most near-matches a search that found nothing counts before saying
/// "at least": the number is an offer, not an answer.
const SIMILAR_CAP: u32 = 1_000;

/// How long the count of near-matches for one query stands: a window asks
/// again each time the index moves, and the count is a walk.
const SIMILAR_STANDS: Duration = Duration::from_secs(10);

/// The parts of a query the parser could not read as written. Answered here, by
/// what read it: 0.5 µs for five terms, against 160 µs for the fastest search.
pub(super) fn misread(query: &str) -> Vec<scour_core::Span> {
    scour_query::spans(query)
        .into_iter()
        .filter(|s| s.role.is_warning())
        .collect()
}

impl Engine {
    /// One page of one query. Served from [`Prepared`] when the query's order is
    /// known, which is what makes row nineteen thousand cost what row zero does.
    pub fn search(
        &self,
        query: &str,
        sort: SortKey,
        descending: bool,
        page: Page,
    ) -> Result<SearchResponse> {
        self.shared.reading();
        let started = Instant::now();
        let mut res = self.page_of(query, sort, descending, page)?;
        // On every path, cached included: a warning that comes and goes teaches nothing.
        res.misread = misread(query);
        // A first page with nothing on it; a count asks for no page at all.
        if res.total == 0 && page.offset == 0 && page.limit > 0 {
            res.similar = self.similar(query);
        }
        self.weigh_folders(&mut res);
        res.took_us = started.elapsed().as_micros() as u64;
        Ok(res)
    }

    /// What a query that found nothing nearly found, when that is to be offered.
    /// A count and no rows; kept a while, since a waiting window asks again
    /// every time the index moves.
    fn similar(&self, query: &str) -> Option<scour_core::Similar> {
        if self.shared.read_as().typos != Typos::Suggest {
            return None;
        }
        if let Some((asked, at, found)) = &*self.shared.similar.lock()
            && asked == query
            && at.elapsed() < SIMILAR_STANDS
        {
            return found.clone();
        }
        let forgiven = self.shared.ast(query).forgiving()?;
        let text = scour_query::forgiving(query)?;
        let counted = self
            .shared
            .index
            .search(&SearchRequest {
                query: forgiven,
                sort: SortKey::Modified,
                descending: true,
                page: Page {
                    offset: 0,
                    limit: 0,
                    count_cap: SIMILAR_CAP,
                },
            })
            .ok()?;
        let found = (counted.total > 0).then_some(scour_core::Similar {
            total: counted.total,
            capped: counted.capped,
            query: text,
        });
        *self.shared.similar.lock() = Some((query.to_owned(), Instant::now(), found.clone()));
        found
    }

    /// Fill in what each folder on this page holds, in one batched call outside the
    /// row loop. A failure leaves `None`, where a zero would read as an empty folder.
    fn weigh_folders(&self, res: &mut SearchResponse) {
        let where_dirs: Vec<usize> = res
            .hits
            .iter()
            .enumerate()
            .filter(|(_, h)| h.is_dir)
            .map(|(i, _)| i)
            .collect();
        if where_dirs.is_empty() {
            return;
        }
        let paths: Vec<String> = where_dirs
            .iter()
            .map(|&i| res.hits[i].path.clone())
            .collect();
        let Ok(sizes) = self.shared.index.subtree_sizes(&paths) else {
            return;
        };
        for (&i, size) in where_dirs.iter().zip(sizes) {
            res.hits[i].under = size.map(|(disk, files)| scour_core::Subtree { disk, files });
        }
    }

    fn page_of(
        &self,
        query: &str,
        sort: SortKey,
        descending: bool,
        page: Page,
    ) -> Result<SearchResponse> {
        let page = Page {
            limit: page.limit.min(self.shared.opts.result_limit),
            ..page
        };
        let started = Instant::now();
        let ast = self.shared.ast(query);
        if let Some(plan) = crate::content::plan(&ast) {
            let lent = self.lent();
            let contents = &self.shared.contents;
            return contents.page(lent, ast, plan, sort, descending, page);
        }
        if let Some(res) = self.sliced(query, sort, descending, &page, started) {
            return Ok(res);
        }
        // Ask, and answer this page the long way. Only for a deep page, and never
        // under a scan, which cost 27-28% of a core on orderings nothing redeemed.
        let floor = Duration::from_micros(self.shared.prepare_floor.load(Ordering::Relaxed));
        if page.offset == 0
            || page.limit == 0
            || page.offset.saturating_add(page.limit) > PREPARE
            || self.shared.scanning.load(Ordering::Acquire)
            || self.shared.prepared_at.lock().elapsed() < floor
        {
            return self.shared.index.search(&SearchRequest {
                query: self.shared.ast(query),
                sort,
                descending,
                page,
            });
        }
        let res = self.shared.index.search(&SearchRequest {
            query: self.shared.ast(query),
            sort,
            descending,
            page,
        })?;
        if started.elapsed() < PREPARE_MIN_COST {
            return Ok(res);
        }
        let mut prepared_at = self.shared.prepared_at.lock();
        // A simultaneous expensive page may already have requested it.
        if prepared_at.elapsed() < floor || self.shared.scanning.load(Ordering::Acquire) {
            return Ok(res);
        }
        *prepared_at = Instant::now();
        let _ = self.shared.prepare.try_send(Prepare {
            query: query.to_string(),
            sort,
            descending,
            count_cap: page.count_cap,
        });
        Ok(res)
    }

    /// The page, if the order it belongs to is already known. Refuses on anything it
    /// cannot answer exactly, including a page reaching past what was prepared.
    fn sliced(
        &self,
        query: &str,
        sort: SortKey,
        descending: bool,
        page: &Page,
        started: Instant,
    ) -> Option<SearchResponse> {
        let held = self.shared.prepared.read();
        let ready = held.as_ref()?;
        if ready.query != query
            || ready.sort != sort
            || ready.descending != descending
            || ready.count_cap != page.count_cap
            || ready.revision != self.shared.revision.load(Ordering::Acquire)
            // Relative time windows change even while the index is quiet.
            || ready.parsed != self.shared.ast(query)
        {
            return None;
        }
        let offset = page.offset as usize;
        // Short of the ceiling, the walk reached the end: past it is empty, not unknown.
        let complete = ready.hits.len() < PREPARE as usize;
        if offset > ready.hits.len() && !complete {
            return None;
        }
        let end = (offset + page.limit as usize).min(ready.hits.len());
        if end < offset + page.limit as usize && !complete {
            return None;
        }
        Some(SearchResponse {
            hits: ready.hits[offset.min(end)..end].to_vec(),
            total: ready.total,
            capped: ready.capped,
            took_us: started.elapsed().as_micros() as u64,
            fast_path: true,
            rows_visited: 0,
            rows_built: 0,
            // Stamped by `search` for every path alike.
            misread: Vec::new(),
            similar: None,
            reading: None,
        })
    }

    /// Read a query back without running it: what it means, what its pieces are, and
    /// what could follow the caret — one call, so the three answers agree.
    pub fn explain(&self, query: &str, cursor: Option<u32>) -> Explained {
        let ast = self.shared.ast(query);
        Explained {
            description: scour_query::describe(&ast),
            needs_content: ast.needs_content(),
            spans: scour_query::spans(query),
            completions: match cursor {
                Some(at) => scour_query::complete(query, at as usize),
                None => Vec::new(),
            },
        }
    }
}
