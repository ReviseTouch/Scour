//! One request, one answer.
//!
//! A flat mapping, on purpose. Everything that could be a decision was made
//! upstream — the engine bounds the page, the index caps the count, the parser
//! never fails — so there is nothing left here but naming which method a
//! request means. That is what makes a second frontend cheap.

use std::path::PathBuf;
use std::sync::Mutex;

use scour_engine::Engine;
use scour_ipc::Emit;
use scour_proto::{Outcome, Request, Response};

/// What every frontend remembers, and where it is written.
///
/// **Held here rather than in the engine**, because it is not about the index:
/// which columns somebody shows is a fact about a person. The engine would
/// have to carry it through every layer to reach the one place that serves it.
pub struct Kept {
    pub dir: PathBuf,
    pub settings: Mutex<scour_settings::Settings>,
    /// The one thing on this machine allowed to start thumbnailers.
    ///
    /// **Singular, and that is the whole argument for it being here.** How
    /// many image decoders may run at once is a fact about the machine, not
    /// about a browser; three frontends each holding a sensible bound of their
    /// own is a machine with no bound at all. It sits beside `settings` for
    /// the same reason `settings` is here: it is not about the index, and the
    /// service is the one process every frontend already talks to.
    pub maker: scour_thumbs::Maker,
    /// The configuration this service was started with.
    ///
    /// **Here because a saved rule has to be turned back into what the walk
    /// skips**, and that sum — the built-in set, this file, and what a window
    /// added, minus what has been switched off — is [`crate::wire`]'s to
    /// compute. Recomputing it from the same function the service started with
    /// is what keeps the engine and the panel from drifting into two answers.
    pub config: scour_config::Config,
}

impl Kept {
    pub fn open(dir: PathBuf, config: scour_config::Config) -> Kept {
        Kept {
            settings: Mutex::new(scour_settings::Settings::load(&dir)),
            dir,
            maker: scour_thumbs::Maker::default(),
            config,
        }
    }
}

pub fn dispatch(engine: &Engine, kept: &Kept, req: Request, emit: &mut dyn Emit) -> Outcome {
    match run(engine, kept, req, emit) {
        Ok(r) => Outcome::Ok(r),
        Err(e) => Outcome::Error(e),
    }
}

fn run(
    engine: &Engine,
    kept: &Kept,
    req: Request,
    emit: &mut dyn Emit,
) -> scour_core::Result<Response> {
    Ok(match req {
        // **The one request that is not one answer.** Rows are written into
        // frames as the walk produces them and the frames go out while this is
        // still running; what is returned here is the last of them.
        //
        // The `Err` from a piece is the reader having gone away — an ordinary
        // cancelled download. It stops the walk, and it is deliberately not
        // turned into a failure reply: there is nobody left to read one, and
        // the frame would only fail to write as well.
        Request::Export { query, columns } => {
            let mut gone = None;
            let rows = engine.export(&query, &columns, |csv| {
                match emit.piece(Response::ExportChunk { csv }) {
                    Ok(()) => true,
                    Err(e) => {
                        gone = Some(e);
                        false
                    }
                }
            })?;
            match gone {
                Some(e) => return Err(e),
                None => Response::ExportDone { rows },
            }
        }
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
        Request::Usage { path, top, query } => Response::Usage(engine.usage(&path, top, &query)?),
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
        Request::Settings {} => Response::Settings(
            kept.settings
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .clone(),
        ),
        // Written where the change happens rather than at shutdown. The whole
        // reason this moved out of the browser is that a process which is
        // killed never gets to write anything.
        Request::SetSettings { change } => {
            let mut held = kept.settings.lock().unwrap_or_else(|p| p.into_inner());
            let before = rules_of(&held);
            // Folded in rather than assigned. What the change does not name is
            // what another frontend put there.
            change.apply(&mut held);
            if let Err(e) = held.save(&kept.dir) {
                scour_core::note!("scourd: settings could not be written: {e}");
            }
            // **A rule is the one setting that changes what the index holds**,
            // so it is the one that does more than get written down: the engine
            // takes the new set, every watcher re-tunes to it, and the walk that
            // brings the index in line starts on its own.
            //
            // Compared rather than assumed, because this is also the request a
            // window sends when somebody drags a column edge — and a rescan of
            // two volumes for a column width would be an unusable window.
            let after = rules_of(&held);
            if before != after {
                let opts = crate::wire::scan_options_with(&kept.config, &held);
                drop(held);
                engine.set_scan_options(opts);
                // The panel says a change takes effect on the next scan; this
                // is that scan. Queued and returned from immediately — the walk
                // runs on the worker, and a save that blocked for the length of
                // it would look like a frozen window.
                if let Err(e) = engine.rescan(None) {
                    scour_core::note!("scourd: the rules changed but a scan could not start: {e}");
                }
            }
            Response::Accepted
        }
        // Answered without the engine, like `Syntax`: it is a fact about the
        // machine rather than about the index, and the service is asked
        // because it is the one thing every frontend already talks to.
        //
        // **Three lists, not one.** The built-in set is where nearly all of the
        // exclusion happens — `target` alone is 2,087,642 files on the machine
        // this was written for — and it is code; `config.toml` is somebody's
        // hand-written file; only the third can be deleted from a window.
        //
        // **Read from where each group is written, not from what the engine is
        // enforcing**, and that changed with the switches. What the engine
        // holds is the merged set *minus what has been switched off* — so a
        // rule somebody turned off is not in it, and a panel built from it
        // would show the rule vanishing rather than switching, with no way
        // left to turn it back on. The two cannot drift apart regardless:
        // `wire::scan_options_with` builds the engine's set out of exactly
        // these three groups.
        Request::Rules {} => {
            let (bp, bd, bf) = scour_source_fs::platform_defaults();
            let (cp, cd, cf, ca) = crate::wire::config_rules(&kept.config);
            let held = kept.settings.lock().unwrap_or_else(|p| p.into_inner());
            // An entry written in two places is attributed to the group that
            // cannot be deleted, which is the true answer: deleting the other
            // copy would change nothing, because the first still excludes it.
            let rest = |all: Vec<String>, a: &[String], b: &[String]| -> Vec<String> {
                all.into_iter()
                    .filter(|v| {
                        !a.iter().any(|x| x.eq_ignore_ascii_case(v))
                            && !b.iter().any(|x| x.eq_ignore_ascii_case(v))
                    })
                    .collect()
            };
            Response::Rules {
                config_paths: rest(cp, &bp, &held.exclude_paths),
                config_dirs: rest(cd, &bd, &held.exclude_dirs),
                config_files: rest(cf, &bf, &held.exclude_files),
                config_allow: rest(ca, &[], &held.exclude_allow),
                added_paths: held.exclude_paths.clone(),
                added_dirs: held.exclude_dirs.clone(),
                added_files: held.exclude_files.clone(),
                added_allow: held.exclude_allow.clone(),
                off: held.exclude_off.clone(),
                builtin_paths: bp,
                builtin_dirs: bd,
                builtin_files: bf,
            }
        }
        Request::Places {} => Response::Places(scour_places::look()),
        // **`stat` first, and that is the fence.** Only a path the index holds
        // may be looked at — the same rule `/api/open` follows, kept here so
        // that a frontend cannot be the thing that remembers it.
        Request::Preview { path } => {
            let entry = engine.stat(&path)?;
            Response::Preview(scour_preview::look_at(
                std::path::Path::new(&entry.path),
                entry.is_dir,
            ))
        }
        // **The same fence as `preview`, and it matters more here**: this runs
        // a program on the file. Every path is `stat`ed through the engine
        // first, so a path no source owns never reaches a thumbnailer — and
        // the `stat` is not wasted, because the modification time it returns
        // is what the standard requires be written into the picture.
        //
        // A path the index does not hold is dropped rather than refused. A
        // batch is a screenful of tiles and one file deleted since the page
        // drew it is ordinary; failing all thirty-two over it would mean a
        // grid that stops filling whenever anything moves.
        Request::Thumbnails { files } => {
            let wanted: Vec<scour_thumbs::Wanted> = files
                .iter()
                .take(scour_thumbs::Maker::BATCH)
                .filter_map(|path| engine.stat(path).ok())
                .filter(|entry| !entry.is_dir)
                .map(|entry| scour_thumbs::Wanted {
                    path: entry.path.clone(),
                    mtime: entry.meta.mtime,
                })
                .collect();
            Response::Thumbnails(kept.maker.make(&wanted))
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

/// The five lists that decide what the index holds.
///
/// Pulled out so that saving a setting can ask *did the rules change* and get
/// an answer that does not depend on remembering which fields those are. The
/// question is asked on every save a window makes — a column drag is one — so
/// the cheap comparison is the point: everything else in the settings is about
/// how a window looks, and none of it is worth a walk of two volumes.
fn rules_of(s: &scour_settings::Settings) -> [Vec<String>; 5] {
    [
        s.exclude_paths.clone(),
        s.exclude_dirs.clone(),
        s.exclude_files.clone(),
        s.exclude_allow.clone(),
        s.exclude_off.clone(),
    ]
}
