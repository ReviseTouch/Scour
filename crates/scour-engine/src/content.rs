//! `content:` in a query. The index holds no contents, so the rest of the
//! query names the candidates and their files are read, in the order asked
//! for, on a thread of their own; a page is what has been found so far, and
//! the revision moves when there is more — a waiting face asks again.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use parking_lot::{Condvar, Mutex};
use scour_core::{
    Ast, Error, Hit, Index, Match, Page, Result, SearchRequest, SearchResponse, SortKey,
};
use scour_grep::{Budget, Enter, Ground, Needle, Options, Outcome, Pattern, Scanner};

/// How long a page waits for its rows before answering with what there is.
const WAIT: Duration = Duration::from_millis(300);
/// How long the reading goes on for rows nobody has asked to see yet. Asking
/// for a later page, or the query again after another, allows as long again.
const GRANT: Duration = Duration::from_secs(15);
/// Rows wanted by a query that has only been counted.
const AT_FIRST: usize = 200;
/// Kept at most: a hundred bytes a row, and nobody scrolls this far.
const KEPT: usize = 100_000;
/// A reading older than this is begun again once the index has moved.
const STALE: Duration = Duration::from_secs(300);
/// The revision moves at most this often while rows arrive.
const TOLD_EVERY: Duration = Duration::from_millis(250);
const FIRST: u32 = 64;
const WIDEST: u32 = 65_536;
const BATCH: usize = 2048;

/// The texts files must hold, and the query that is left without them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Plan {
    needles: Vec<String>,
    rest: Ast,
}

/// None unless the query has a `content:` term and each one stands alone:
/// `!content:x` and `a | content:x` would mean reading every file there is.
pub(crate) fn plan(ast: &Ast) -> Option<Plan> {
    let mut plan = Plan {
        needles: Vec::new(),
        rest: Ast {
            groups: Vec::new(),
            exact_accents: ast.exact_accents,
        },
    };
    for group in &ast.groups {
        match group.alts.as_slice() {
            [(false, Match::ContentContains(text))] => plan.needles.push(text.clone()),
            alts if alts
                .iter()
                .any(|(_, m)| matches!(m, Match::ContentContains(_))) =>
            {
                return None;
            }
            _ => plan.rest.groups.push(group.clone()),
        }
    }
    (!plan.needles.is_empty()).then_some(plan)
}

/// The query without its `content:` terms, for a question the reading does
/// not answer — what kinds the candidates are — rather than an error.
pub(crate) fn without(ast: Ast) -> Ast {
    plan(&ast).map_or(ast, |plan| plan.rest)
}

/// What the engine lends a reading.
pub(crate) struct Lent {
    pub index: Arc<dyn Index>,
    pub roots: Vec<String>,
    pub enter: Option<Enter>,
    /// Says a repeated search would now answer differently.
    pub touched: Arc<dyn Fn() + Send + Sync>,
    /// The engine's revision, as it stands.
    pub revision: u64,
}

/// The one reading kept: the query asked for last.
#[derive(Default)]
pub(crate) struct Contents {
    current: Mutex<Option<Arc<Reading>>>,
    /// How often a reading moved the revision, which is not the index moving.
    told: AtomicU64,
}

impl std::fmt::Debug for Contents {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Contents")
    }
}

struct Reading {
    ast: Ast,
    plan: Plan,
    sort: SortKey,
    descending: bool,
    began: Instant,
    /// The index's own revision when this began.
    index_at: u64,
    budget: Budget,
    state: Mutex<State>,
    grew: Condvar,
}

#[derive(Default)]
struct State {
    hits: Vec<Hit>,
    /// Candidates already read.
    at: u32,
    scanned: u64,
    /// Every candidate was read.
    done: bool,
    failed: Option<Error>,
    /// Rows somebody asked for, and until when to go on reading for them.
    want: usize,
    until: Option<Instant>,
    running: bool,
}

impl Contents {
    /// One page of a query with `content:` in it: what the reading has found
    /// of it so far, after waiting a moment for the reading to get there.
    pub(crate) fn page(
        self: &Arc<Self>,
        lent: Lent,
        ast: Ast,
        plan: Plan,
        sort: SortKey,
        descending: bool,
        page: Page,
    ) -> Result<SearchResponse> {
        let index_now = lent
            .revision
            .wrapping_sub(self.told.load(Ordering::Relaxed));
        let reading = {
            let mut current = self.current.lock();
            let kept = current.as_ref().filter(|r| {
                let stale =
                    r.index_at != index_now && r.began.elapsed() > STALE && !r.state.lock().running;
                r.ast == ast && r.sort == sort && r.descending == descending && !stale
            });
            match kept {
                Some(reading) => Arc::clone(reading),
                None => {
                    if let Some(old) = current.take() {
                        old.budget.stop();
                    }
                    let reading = Arc::new(Reading {
                        ast,
                        plan,
                        sort,
                        descending,
                        began: Instant::now(),
                        index_at: index_now,
                        budget: Budget::unlimited(),
                        state: Mutex::new(State::default()),
                        grew: Condvar::new(),
                    });
                    *current = Some(Arc::clone(&reading));
                    reading
                }
            }
        };
        let need = page.offset as usize + page.limit as usize;
        // A page ahead of the one shown; a count asks for none and gets a first one.
        let want = (need + page.limit as usize).clamp(AT_FIRST, KEPT);
        let mut state = reading.state.lock();
        if want > state.want {
            (state.want, state.until) = (want, Some(Instant::now() + GRANT));
        }
        let open = state.until.is_some_and(|until| Instant::now() < until);
        let idle = !state.running && !state.done && state.failed.is_none();
        if idle && open && state.hits.len() < state.want {
            state.running = true;
            let (this, reading) = (Arc::clone(self), Arc::clone(&reading));
            std::thread::Builder::new()
                .name("scour-content".into())
                .spawn(move || this.read(&reading, &lent))
                .map_err(|e| Error::unsupported(format!("reading files: {e}")))?;
        }
        let deadline = Instant::now() + WAIT;
        while state.hits.len() < need && state.running {
            if reading.grew.wait_until(&mut state, deadline).timed_out() {
                break;
            }
        }
        if let Some(error) = &state.failed {
            return Err(error.clone());
        }
        let from = (page.offset as usize).min(state.hits.len());
        let to = need.min(state.hits.len());
        Ok(SearchResponse {
            hits: state.hits[from..to].to_vec(),
            total: state.hits.len() as u64,
            // A floor until every candidate is read.
            capped: !state.done,
            rows_visited: state.scanned,
            rows_built: (to - from) as u64,
            ..SearchResponse::default()
        })
    }

    /// The reading thread: candidates a page at a time, in the order asked
    /// for, until enough is found, the time allowed is up, or another query
    /// takes its place.
    fn read(&self, reading: &Reading, lent: &Lent) {
        let ended = self.read_on(reading, lent);
        let mut state = reading.state.lock();
        state.running = false;
        match ended {
            Ok(done) => state.done = done,
            Err(error) => state.failed = Some(error),
        }
        drop(state);
        reading.grew.notify_all();
        // Told once more at the end: whether it is a floor has changed.
        self.told.fetch_add(1, Ordering::Relaxed);
        (lent.touched)();
    }

    /// `Ok(true)` when every candidate was read.
    fn read_on(&self, reading: &Reading, lent: &Lent) -> Result<bool> {
        let ground = Ground::probe(lent.roots.iter().map(|r| scour_core::path::to_path(r)));
        let scanners = reading
            .plan
            .needles
            .iter()
            .map(|text| {
                let pattern = Pattern::new(&Needle::Text(text.clone()), false).map_err(|e| {
                    Error::QuerySyntax {
                        at: 0,
                        expected: e.to_string(),
                    }
                })?;
                // Whether, not where: a row is a file, and one find settles it.
                let options = Options {
                    hits_per_file: 0,
                    ..Options::default()
                };
                Ok(Scanner::new(pattern, options, ground.clone()).entering(lent.enter.clone()))
            })
            .collect::<Result<Vec<Scanner>>>()?;
        let (mut width, mut told) = (FIRST, Instant::now());
        loop {
            let at = reading.state.lock().at;
            let found = lent.index.search(&SearchRequest {
                query: reading.plan.rest.clone(),
                sort: reading.sort,
                descending: reading.descending,
                page: Page {
                    offset: at,
                    limit: width,
                    count_cap: 1,
                },
            })?;
            for batch in found.hits.chunks(BATCH) {
                if !self.wanted(reading) {
                    return Ok(false);
                }
                let (read, holding) = holding(&scanners, &reading.budget, batch);
                let mut state = reading.state.lock();
                state.at = state.at.saturating_add(read as u32);
                state.scanned += read as u64;
                let grew = !holding.is_empty();
                state
                    .hits
                    .extend(holding.into_iter().map(|row| batch[row].clone()));
                drop(state);
                reading.grew.notify_all();
                if grew && told.elapsed() >= TOLD_EVERY {
                    told = Instant::now();
                    self.told.fetch_add(1, Ordering::Relaxed);
                    (lent.touched)();
                }
                if read < batch.len() {
                    return Ok(false);
                }
            }
            if (found.hits.len() as u32) < width {
                return Ok(true);
            }
            width = (width * 8).min(WIDEST);
        }
    }

    /// Is there still somebody this reading is for?
    fn wanted(&self, reading: &Reading) -> bool {
        let state = reading.state.lock();
        !reading.budget.exhausted()
            && state.hits.len() < state.want
            && state.until.is_some_and(|until| Instant::now() < until)
    }
}

/// The rows of `batch` whose files hold every needle, in order, and how many
/// rows from the front were settled — all of them unless the reading stopped.
fn holding(scanners: &[Scanner], budget: &Budget, batch: &[Hit]) -> (usize, Vec<usize>) {
    let (places, files) = crate::grep::files_of(batch);
    let mut settled = batch.len();
    // Each needle reads only what held the ones before it.
    let mut left: Vec<usize> = (0..files.len()).collect();
    for scanner in scanners {
        let asked: Vec<_> = left.iter().map(|&k| files[k].clone()).collect();
        let outcomes = scanner.scan(&asked, budget);
        let mut kept = Vec::with_capacity(left.len());
        for (k, outcome) in left.iter().zip(outcomes) {
            match outcome {
                Outcome::Found { .. } => kept.push(*k),
                Outcome::NotReached => settled = settled.min(places[*k] as usize),
                _ => {}
            }
        }
        left = kept;
    }
    let rows = left
        .into_iter()
        .map(|k| places[k] as usize)
        .filter(|row| *row < settled)
        .collect();
    (settled, rows)
}

#[cfg(test)]
mod tests {
    use super::*;
    use scour_core::Group;

    fn group(alts: Vec<(bool, Match)>) -> Group {
        Group { alts }
    }

    #[test]
    fn a_content_term_is_taken_out_only_when_it_stands_alone() {
        let name = group(vec![(false, Match::NameContains("a".into()))]);
        let content = |negated| (negated, Match::ContentContains("x".into()));
        let ast = |groups| Ast {
            groups,
            exact_accents: false,
        };
        let both = plan(&ast(vec![name.clone(), group(vec![content(false)])])).expect("a plan");
        assert_eq!(both.needles, ["x"]);
        assert_eq!(both.rest, ast(vec![name.clone()]));
        assert_eq!(plan(&ast(vec![name.clone()])), None);
        assert_eq!(plan(&ast(vec![group(vec![content(true)])])), None);
        let either = group(vec![
            (false, Match::NameContains("a".into())),
            content(false),
        ]);
        assert_eq!(plan(&ast(vec![either.clone()])), None);
        // Left as it was, so the index still says it cannot.
        assert_eq!(without(ast(vec![either.clone()])), ast(vec![either]));
    }
}
