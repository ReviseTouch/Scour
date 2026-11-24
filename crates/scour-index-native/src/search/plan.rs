//! A query compiled for a segment: its clauses, and the blocks worth opening.

use super::*;
use crate::columns::{BLOCK, Field};
use scour_core::Ast;

/// The blocks a search has to look at, if the trigram index can say. Only a
/// clause that is one plain `NameHas` counts — an OR needs a union and a negation
/// inverts the question, and narrowing wrongly loses files silently.
fn narrow_to_blocks(clauses: &[Clause], seg: &Segment<'_>) -> Option<Vec<u32>> {
    // `SCOUR_NO_TRIGRAM=1` turns the filter off. On 2,137,518 entries, one
    // segment, twenty rounds each, with against without: `rapor` 14.11 ms/37.97,
    // `main` 5.62/36.23, `kütüphane` 0.06/35.50 — between 2.7× and 590×.
    if std::env::var_os("SCOUR_NO_TRIGRAM").is_some() {
        return None;
    }
    let mut out: Option<Vec<u32>> = None;
    for c in clauses {
        let [(false, test)] = &c.alts[..] else {
            continue;
        };
        let blocks = match test {
            Test::NameNear(near) => near.blocks(&seg.tri),
            test => filterable(test).and_then(|needle| seg.tri.candidates(&needle)),
        };
        let Some(blocks) = blocks else {
            continue;
        };
        out = Some(match out {
            None => blocks,
            Some(have) => intersect_blocks(&have, &blocks),
        });
    }
    out
}

/// A string every matching name must contain, if there is one. Containment is
/// the only thing claimed: over-narrowing loses files and reports nothing.
fn filterable(test: &Test) -> Option<Vec<u8>> {
    match test {
        Test::NameHas(n) | Test::NameHasCased { folded: n, .. } => Some(n.folded().to_vec()),
        Test::NameHasMarked(m) => Some(m.folded.folded().to_vec()),
        // A name with extension `pdf` contains `.pdf`. One extension only: a list
        // would need the union of its lists, not the intersection.
        Test::Ext { list, .. } => match &list[..] {
            [only] => Some(format!(".{only}").into_bytes()),
            _ => None,
        },
        // A name matching `rap*or` contains `rap` and contains `or`. The
        // longest run is the most selective of them.
        Test::NameGlob(p) | Test::NameGlobMarked { folded: p, .. } => p
            .split(['*', '?'])
            .max_by_key(|run| run.len())
            .filter(|run| run.len() >= 3)
            .map(|run| run.as_bytes().to_vec()),
        _ => None,
    }
}

fn intersect_blocks(a: &[u32], b: &[u32]) -> Vec<u32> {
    let mut out = Vec::with_capacity(a.len().min(b.len()));
    let (mut i, mut j) = (0usize, 0usize);
    while i < a.len() && j < b.len() {
        match a[i].cmp(&b[j]) {
            std::cmp::Ordering::Less => i += 1,
            std::cmp::Ordering::Greater => j += 1,
            std::cmp::Ordering::Equal => {
                out.push(a[i]);
                i += 1;
                j += 1;
            }
        }
    }
    out
}

/// Alternatives ORed together; `bool` is negation.
#[derive(Debug)]
pub(super) struct Clause {
    pub(super) alts: Vec<(bool, Test)>,
}

/// A query, compiled.
#[derive(Debug, Default)]
pub struct Plan {
    pub(super) clauses: Vec<Clause>,
    /// Blocks the walk has to visit, when the trigram index could say.
    /// `None` is "all of them", which a query with no usable trigram still does.
    candidates: Option<Vec<u32>>,
}

impl Plan {
    /// Compile an [`Ast`] against a segment. Everything that can be resolved once
    /// is resolved here — an `under:` path becomes a range of directory numbers —
    /// so the per-row work is comparisons and nothing else.
    pub fn compile(ast: &Ast, seg: &Segment<'_>) -> Result<Plan, scour_core::Error> {
        let mut clauses = Vec::with_capacity(ast.groups.len());
        for group in &ast.groups {
            let mut alts = Vec::with_capacity(group.alts.len());
            for (negated, m) in &group.alts {
                alts.push((
                    *negated,
                    compile_match(m, ast.exact_accents, ast.strict_typos, seg)?,
                ));
            }
            clauses.push(Clause { alts });
        }
        // Cheapest clause first. A clause is only as cheap as its dearest
        // alternative, because an OR has to try them until one succeeds.
        clauses.sort_by_key(|c| c.alts.iter().map(|(_, t)| t.cost()).max().unwrap_or(0));
        let candidates = narrow_to_blocks(&clauses, seg);
        Ok(Plan {
            clauses,
            candidates,
        })
    }

    pub fn is_empty(&self) -> bool {
        self.clauses.is_empty()
    }

    /// The blocks the filter chose, for measurement.
    pub fn candidate_blocks(&self) -> Option<&[u32]> {
        self.candidates.as_deref()
    }

    /// Could any row of this block match? Two comparisons against the range the
    /// block stores. It can only reject, so it turns misses into skips and never
    /// a match into a miss.
    fn block_possible(&self, seg: &Segment<'_>, block: usize) -> bool {
        for c in &self.clauses {
            let [(false, test)] = &c.alts[..] else {
                continue;
            };
            let admits = match test {
                Test::Never => false,
                Test::Num {
                    field,
                    cmp,
                    value,
                    span,
                } => match seg.cols.block_range(*field, block) {
                    Some((lo, hi)) => admits(*cmp, *value, *span, lo, hi),
                    None => true,
                },
                Test::DirIn(scope) => match seg.cols.block_range(Field::DirId, block) {
                    Some((lo, hi)) if lo >= 0 => scope.intersects(lo as u32, hi as u32),
                    _ => true,
                },
                // The same zone map, asked whether any directory in the block's
                // range can hold the path term.
                Test::PathIn(set) => match seg.cols.block_range(Field::DirId, block) {
                    Some((lo, hi)) if lo >= 0 => set.admits(block, lo as u32, hi as u32),
                    _ => true,
                },
                // The same zone map, read as a range of discriminants: a block
                // holding only kinds 10 and 11 cannot answer `kind:image`.
                Test::KindIn(mask) => match seg.cols.block_range(Field::Kind, block) {
                    Some((lo, hi)) if (0..16).contains(&lo) && (0..16).contains(&hi) => {
                        let span = ((1u32 << (hi - lo + 1)) - 1) << lo;
                        u32::from(*mask) & span != 0
                    }
                    _ => true,
                },
                _ => true,
            };
            if !admits {
                return false;
            }
        }
        true
    }

    /// Does answering this require reading names at all? `kind:image size:>10mb`
    /// does not, and the difference is the whole inner loop: a `memchr` and
    /// twenty bytes of memory traffic a row, for tests that never look at it.
    pub(super) fn needs_name(&self) -> bool {
        self.clauses.iter().flat_map(|c| &c.alts).any(|(_, t)| {
            matches!(
                t,
                Test::NameHas(_)
                    | Test::NameGlob(_)
                    | Test::Ext { .. }
                    | Test::PathIn(_)
                    | Test::Regex(_)
                    | Test::NameLen { .. }
                    | Test::NameHasCased { .. }
                    | Test::RegexMarked(_)
                    | Test::NameHasMarked(_)
                    | Test::NameGlobMarked { .. }
                    | Test::NameNear(_)
                    | Test::NameLetters(_)
            )
        })
    }

    /// Does this row match? `name` is the row's name **already folded**, passed in
    /// because the walk reads names sequentially — which is why the arena has no
    /// offsets. Nothing is folded here.
    pub fn accepts(&self, seg: &Segment<'_>, row: usize, name: &[u8]) -> bool {
        for clause in &self.clauses {
            let mut any = false;
            for (negated, test) in &clause.alts {
                if evaluate(test, seg, row, name) != *negated {
                    any = true;
                    break;
                }
            }
            if !any {
                return false;
            }
        }
        true
    }
}

/// Which blocks are worth opening at all: the trigram index says which blocks
/// could contain the text, the zone map which could satisfy the numbers, and both
/// can only remove. **Shared, because the two callers diverging is what went
/// wrong** — a search narrowed and a facet did not.
pub(crate) fn blocks_worth_opening(seg: &Segment<'_>, plan: &Plan) -> Vec<u32> {
    let n_blocks = seg.rows().div_ceil(BLOCK);
    match &plan.candidates {
        Some(c) => c
            .iter()
            .copied()
            .filter(|&b| seg.block_alive(b as usize) && plan.block_possible(seg, b as usize))
            .collect(),
        None => (0..n_blocks as u32)
            .filter(|&b| seg.block_alive(b as usize) && plan.block_possible(seg, b as usize))
            .collect(),
    }
}

/// Adjacent blocks as row ranges. Coalesced first rather than as the walk goes,
/// so a dense set costs no more seeking than a full walk, and the backwards walk
/// can take the same runs from the far end.
pub(super) fn runs_of(blocks: &[u32], rows: usize) -> Vec<(usize, usize)> {
    let mut runs: Vec<(usize, usize)> = Vec::new();
    let mut i = 0usize;
    while i < blocks.len() {
        let mut j = i;
        while j + 1 < blocks.len() && blocks[j + 1] == blocks[j] + 1 {
            j += 1;
        }
        runs.push((
            blocks[i] as usize * BLOCK,
            ((blocks[j] as usize + 1) * BLOCK).min(rows),
        ));
        i = j + 1;
    }
    runs
}
