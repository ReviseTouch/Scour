//! One request, one answer: a flat mapping, on purpose. Every decision was made
//! upstream — the engine bounds the page, the index caps the count, the parser
//! never fails — so nothing is left here but naming the method.

use std::path::PathBuf;
use std::sync::Mutex;

use scour_engine::Engine;
use scour_ipc::Emit;
use scour_proto::{Outcome, Request, Response};

/// What every frontend remembers, and where it is written. Not in the engine:
/// which columns somebody shows is a fact about a person, not about the index.
pub struct Kept {
    pub dir: PathBuf,
    pub settings: Mutex<scour_settings::Settings>,
    /// The one thing on this machine allowed to start thumbnailers: three
    /// frontends each holding a bound of their own is no bound at all.
    pub maker: scour_thumbs::Maker,
    /// The configuration this service was started with: a saved rule is turned
    /// back into what the walk skips by [`crate::wire`], from the same function
    /// the service started with, so the engine and the panel cannot drift.
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
        // The one request that is not one answer: frames go out while this runs
        // and the last is returned. An `Err` from a piece is the reader gone,
        // so it stops the walk rather than becoming a reply nobody can read.
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
            // A count is a search for no rows: only the total is paid for.
            let page = scour_core::Page {
                offset: 0,
                limit: 0,
                count_cap: cap,
            };
            let r = engine.search(&query, scour_core::SortKey::Modified, true, page)?;
            Response::Count {
                total: r.total,
                capped: r.capped,
                took_us: r.took_us,
                misread: r.misread,
            }
        }
        Request::Facets { query, by } => Response::Facets(engine.facets(&query, by)?),
        Request::Tree { path, depth, limit } => {
            // Timed here, so the number covers what the caller waited for.
            let began = std::time::Instant::now();
            let root = engine.tree(&path, depth, limit)?;
            Response::Tree {
                root,
                took_us: began.elapsed().as_micros() as u64,
            }
        }
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
        // Written where the change happens, not at shutdown: a killed process
        // never gets to write anything.
        Request::SetSettings { change } => {
            let mut held = kept.settings.lock().unwrap_or_else(|p| p.into_inner());
            let before = rules_of(&held);
            // Folded in, not assigned: what it does not name is another
            // frontend's.
            change.apply(&mut held);
            if let Err(e) = held.save(&kept.dir) {
                scour_core::note!("scourd: settings could not be written: {e}");
            }
            // A rule is the one setting that changes what the index holds, so
            // the engine takes the new set. Compared rather than assumed: this
            // is also the request a window sends for a column drag.
            let after = rules_of(&held);
            if before != after {
                let was = engine.scan_options();
                let now = crate::wire::scan_options_with(&kept.config, &held);
                let opened = opened_up(&was, &now);
                drop(held);
                engine.set_scan_options(now);

                // Tightening first and without a walk: the index already holds
                // every path the answer is about. Timed and reported even when
                // nothing goes, because the pass reads every row.
                let began = std::time::Instant::now();
                match engine.apply_rules() {
                    Ok(n) => scour_core::note!(
                        "scourd: {n} subtree(s) left the index · {} ms",
                        began.elapsed().as_millis()
                    ),
                    Err(e) => scour_core::note!("scourd: the new rules could not be applied: {e}"),
                }

                // A walk only for what was opened: a path rule names its own
                // subtree, a directory or file name can be anywhere. Said out
                // loud, because all three look alike from outside.
                let walk: Vec<Option<String>> = match opened {
                    Opened::Nothing => {
                        scour_core::note!("scourd: rules tightened; no walk needed");
                        Vec::new()
                    }
                    Opened::Subtrees(paths) => {
                        scour_core::note!(
                            "scourd: rules opened {}; walking those",
                            paths.join(", ")
                        );
                        paths.into_iter().map(Some).collect()
                    }
                    Opened::Everything => {
                        scour_core::note!(
                            "scourd: a directory or file name was re-admitted; walking everything"
                        );
                        vec![None]
                    }
                };
                for subtree in walk {
                    // Queued and returned from immediately: the walk runs on
                    // the worker, or a save looks like a frozen window.
                    match engine.rescan(subtree.clone()) {
                        Ok(()) => {}
                        // Outside every root is not a failure: the built-in set
                        // excludes `/proc` and `/tmp`, which no source holds,
                        // so switching one off opens nothing.
                        Err(scour_core::Error::NotFound { .. }) => scour_core::note!(
                            "scourd: {} is outside every source; nothing to walk",
                            subtree.as_deref().unwrap_or("everything")
                        ),
                        Err(e) => scour_core::note!(
                            "scourd: the rules opened {} but it could not be walked: {e}",
                            subtree.as_deref().unwrap_or("everything")
                        ),
                    }
                }
            }
            Response::Accepted
        }
        // Three lists, not one: the built-in set is code, `config.toml` is
        // hand-written, and only the third can be deleted from a window. Read
        // from where each group is written, not from what the engine enforces —
        // it holds the merged set minus what is switched off, so a switched-off
        // rule would vanish from the panel with no way to turn it back on.
        Request::Rules {} => {
            let (bp, bd, bf) = scour_source_fs::platform_defaults();
            let (cp, cd, cf, ca) = crate::wire::config_rules(&kept.config);
            let held = kept.settings.lock().unwrap_or_else(|p| p.into_inner());
            // An entry written twice belongs to the group that cannot be
            // deleted: deleting the other copy would change nothing.
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
        // `stat` first, and that is the fence: only a path the index holds may
        // be looked at, and no frontend has to remember that.
        Request::Preview { path } => {
            let entry = engine.stat(&path)?;
            Response::Preview(scour_preview::look_at(
                std::path::Path::new(&entry.path),
                entry.is_dir,
            ))
        }
        // The same fence as `preview`, and it matters more: this runs a program
        // on the file. Its mtime is what the thumbnail standard requires. An
        // unheld path is dropped, or one deleted file empties a screenful.
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
        // The only request that blocks, so the ceiling is here: a caller asking
        // to sleep for a day would hold a connection thread for a day.
        Request::Await { since, timeout_ms } => Response::Status(engine.await_change(
            since,
            std::time::Duration::from_millis(timeout_ms.min(60_000) as u64),
        )),
        Request::Stats {} => Response::Stats(engine.stats()?),
        Request::Rescan { path } => {
            engine.rescan(path)?;
            Response::Accepted
        }
        // Not a deletion, however it is used: the caller moved the file and
        // this reads what is there now.
        Request::Recheck { paths } => {
            engine.recheck(&paths)?;
            Response::Accepted
        }
        // Flush happens here and has a result worth reporting. The heavy levels
        // are queued for the worker, whose empty placeholder would be a made-up
        // measurement.
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
        // The reply goes out before the accept loop is torn down.
        Request::Shutdown {} => Response::Accepted,
    })
}

/// What a rule change re-admits, and therefore what has to be walked: excluding
/// takes entries out of an index that holds them, un-excluding asks for entries
/// only the filesystem has.
#[derive(Debug, PartialEq, Eq)]
enum Opened {
    /// Nothing was re-admitted; the change only ever removes.
    Nothing,
    /// These subtrees were, and nothing else: a path prefix names where it is.
    Subtrees(Vec<String>),
    /// A directory or file *name* was re-admitted, and a name can be anywhere.
    Everything,
}

/// Compare two rule sets and say what the second lets back in: an exclusion
/// went away, or an `allow` — which overrides every exclusion under a prefix —
/// appeared.
fn opened_up(before: &scour_core::ScanOptions, after: &scour_core::ScanOptions) -> Opened {
    let gone = |was: &[String], now: &[String]| -> Vec<String> {
        was.iter()
            .filter(|v| !now.iter().any(|x| x.eq_ignore_ascii_case(v)))
            .cloned()
            .collect()
    };
    // A name that stopped being excluded can match at any depth under any
    // root, and nothing here knows where.
    if !gone(&before.exclude_dirs, &after.exclude_dirs).is_empty()
        || !gone(&before.exclude_files, &after.exclude_files).is_empty()
    {
        return Opened::Everything;
    }
    let mut subtrees = gone(&before.exclude_paths, &after.exclude_paths);

    // An `allow` is only a place when it looks like one: with no leading slash
    // `Rules` reads it as a sequence matched wherever it appears, so it costs
    // what a re-admitted name costs rather than naming a directory to walk.
    let admitted = gone(&after.allow, &before.allow);
    if admitted.iter().any(|v| !v.starts_with('/')) {
        return Opened::Everything;
    }
    subtrees.extend(admitted);

    if subtrees.is_empty() {
        Opened::Nothing
    } else {
        Opened::Subtrees(subtrees)
    }
}

/// The five lists that decide what the index holds, so a save can ask whether
/// the rules changed. Asked on every save a window makes, a column drag
/// included, so the comparison has to be the cheap one.
fn rules_of(s: &scour_settings::Settings) -> [Vec<String>; 5] {
    [
        s.exclude_paths.clone(),
        s.exclude_dirs.clone(),
        s.exclude_files.clone(),
        s.exclude_allow.clone(),
        s.exclude_off.clone(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use scour_core::ScanOptions;

    fn opts(paths: &[&str], dirs: &[&str], allow: &[&str]) -> ScanOptions {
        ScanOptions {
            exclude_paths: paths.iter().map(|s| s.to_string()).collect(),
            exclude_dirs: dirs.iter().map(|s| s.to_string()).collect(),
            allow: allow.iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        }
    }

    /// Tightening asks for no walk at all — the common case by a wide margin.
    #[test]
    fn adding_a_rule_opens_nothing() {
        assert_eq!(
            opened_up(&opts(&[], &[], &[]), &opts(&["/a"], &["target"], &[])),
            Opened::Nothing
        );
    }

    /// A path rule that goes away names exactly what it re-admits.
    #[test]
    fn a_path_rule_that_goes_away_opens_its_own_subtree() {
        assert_eq!(
            opened_up(&opts(&["/a", "/b"], &[], &[]), &opts(&["/a"], &[], &[])),
            Opened::Subtrees(vec!["/b".into()])
        );
        // An `allow` overrides every exclusion under a prefix, so appearing is
        // what opens a tree.
        assert_eq!(
            opened_up(&opts(&[], &[], &[]), &opts(&[], &[], &["/c"])),
            Opened::Subtrees(vec!["/c".into()])
        );
    }

    /// An `allow` without a leading slash is a sequence, not a place: it
    /// matches wherever it appears, so it costs a walk of everything.
    #[test]
    fn an_allow_that_names_a_sequence_opens_everything() {
        assert_eq!(
            opened_up(&opts(&[], &[], &[]), &opts(&[], &[], &["target/release"])),
            Opened::Everything
        );
        // One of each: the sequence decides, or every `target/release` on the
        // disk stays unwalked.
        assert_eq!(
            opened_up(
                &opts(&[], &[], &[]),
                &opts(&[], &[], &["/c", "target/debug"])
            ),
            Opened::Everything
        );
    }

    /// A directory *name* can be anywhere, so nothing narrower than everything
    /// is honest: the index cannot say where the re-admitted ones are.
    #[test]
    fn a_name_rule_that_goes_away_opens_everything() {
        assert_eq!(
            opened_up(&opts(&[], &["node_modules"], &[]), &opts(&[], &[], &[])),
            Opened::Everything
        );
    }

    /// Case is not a difference, here or in the merge that builds these lists.
    #[test]
    fn a_rule_respelled_is_not_a_rule_removed() {
        assert_eq!(
            opened_up(
                &opts(&[], &["NODE_MODULES"], &[]),
                &opts(&[], &["node_modules"], &[])
            ),
            Opened::Nothing
        );
    }
}
