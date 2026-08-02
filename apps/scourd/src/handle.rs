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
            }
        }
        Request::Facets { query, by } => Response::Facets(engine.facets(&query, by)?),
        Request::Tree { path, depth, limit } => Response::Tree {
            root: engine.tree(&path, depth, limit)?,
        },
        Request::Stat { path } => Response::Stat(engine.stat(&path)?),
        Request::Explain { query } => {
            let (description, needs_content) = engine.explain(&query);
            Response::Explain {
                description,
                needs_content,
            }
        }
        Request::Sources {} => Response::Sources {
            sources: engine.sources(),
        },
        Request::Status {} => Response::Status(engine.status()),
        Request::Stats {} => Response::Stats(engine.stats()?),
        Request::Rescan { path } => {
            engine.rescan(path)?;
            Response::Accepted
        }
        Request::Maintain { level } => Response::Maintained(engine.maintain(level)?),
        Request::Syntax {} => Response::Text {
            text: scour_query::SYNTAX.to_owned(),
        },
        // The reply goes out before the accept loop is torn down, so the caller
        // finds out it was heard.
        Request::Shutdown {} => Response::Accepted,
    })
}
