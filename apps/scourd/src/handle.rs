//! One request, one answer.
//!
//! A flat mapping, on purpose. Everything that could be a decision was made
//! upstream — the engine bounds the page, the index caps the count, the parser
//! never fails — so there is nothing left here but naming which method a
//! request means. That is what makes a second frontend cheap.

use scour_engine::Engine;
use scour_proto::{Outcome, Request, Response};

pub fn dispatch(engine: &Engine, req: Request) -> Outcome {
    match run(engine, req) {
        Ok(r) => Outcome::Ok(r),
        Err(e) => Outcome::Error(e),
    }
}

fn run(engine: &Engine, req: Request) -> scour_core::Result<Response> {
    Ok(match req {
        Request::Search {
            query,
            sort,
            descending,
            page,
        } => Response::Search(engine.search(&query, sort, descending, page)?),
        Request::Count { query, cap } => {
            // A count is a search for no rows at all: the page is empty and
            // only the total is paid for.
            let page = scour_core::Page {
                offset: 0,
                limit: 0,
                count_cap: cap,
            };
            let r = engine.search(&query, scour_core::SortKey::Modified, true, page)?;
            Response::Count {
                total: r.total,
                capped: r.capped,
                misread: r.misread,
            }
        }
        Request::Facets { query, by } => Response::Facets(engine.facets(&query, by)?),
        Request::Tree { path, depth, limit } => Response::Tree {
            root: engine.tree(&path, depth, limit)?,
        },
        Request::Stat { path } => Response::Stat(engine.stat(&path)?),
        Request::Usage { path, top } => {
            Response::Usage(engine.usage(&scour_core::UsageRequest { path, top })?)
        }
        Request::Duplicates {
            under,
            min_size,
            read_budget,
            top,
        } => {
            let r = engine.duplicates(
                &under,
                &scour_dupes::Options {
                    min_size,
                    read_budget,
                    top: top as usize,
                },
            )?;
            Response::Duplicates {
                groups: r
                    .groups
                    .iter()
                    .map(|g| scour_proto::DupGroup {
                        size: g.size,
                        paths: g.paths.clone(),
                        waste: g.waste(),
                        certainty: g.certainty.token().to_owned(),
                    })
                    .collect(),
                candidates: r.candidates,
                waste: r.waste,
                proven: r.proven,
                read: r.read,
                unconfirmed: r.unconfirmed,
            }
        }
        Request::Explain { query, cursor } => {
            let e = engine.explain(&query, cursor);
            Response::Explain {
                description: e.description,
                needs_content: e.needs_content,
                spans: e.spans,
                completions: e.completions,
            }
        }
        Request::Sources {} => Response::Sources {
            sources: engine.sources(),
        },
        Request::Status {} => Response::Status(engine.status()),
        // The only request that blocks, and the ceiling is here rather than in
        // the engine: a caller asking to sleep for a day would hold a
        // connection thread for a day, and the client that wants to wait longer
        // than a minute can ask again.
        Request::Await { since, timeout_ms } => Response::Status(engine.await_change(
            since,
            std::time::Duration::from_millis(timeout_ms.min(60_000) as u64),
        )),
        Request::Stats {} => Response::Stats(engine.stats()?),
        Request::Rescan { path } => {
            engine.rescan(path)?;
            Response::Accepted
        }
        // Flush happens here and has a result worth reporting. The heavy
        // levels are queued for the worker, and reporting their empty
        // placeholder printed `Rebuild: 0 B → 0 B in 0 ms` after a rebuild
        // that demonstrably folded sixteen segments into one — a made-up
        // measurement, which is worse than no measurement.
        Request::Maintain { level } => {
            let report = engine.maintain(level)?;
            if level == scour_core::Maintenance::Flush {
                Response::Maintained(report)
            } else {
                Response::Accepted
            }
        }
        Request::Syntax {} => Response::Text {
            text: scour_query::SYNTAX.to_owned(),
        },
        // The reply goes out before the accept loop is torn down, so the caller
        // finds out it was heard.
        Request::Shutdown {} => Response::Accepted,
    })
}
