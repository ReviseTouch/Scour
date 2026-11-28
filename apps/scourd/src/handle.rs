//! One request, one answer: a flat mapping, on purpose. Every decision was made
//! upstream — the engine bounds the page, the index caps the count, the parser
//! never fails — so nothing is left here but naming the method.

use std::path::PathBuf;
use std::sync::Mutex;

use scour_engine::Engine;
use scour_ipc::Emit;
use scour_proto::{Outcome, Request, Response};

use crate::live::Live;

mod answers;
mod settings;
mod volumes;
use answers::*;
use settings::*;
use volumes::*;

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
    /// Disks put off until the service starts again, by id. Not a setting:
    /// "not now" is forgotten on purpose.
    pub later: Mutex<std::collections::HashSet<String>>,
}

impl Kept {
    /// What would be indexed now: the disks found, and what was said about them.
    pub fn plan(&self) -> crate::disks::Plan {
        let settings = self
            .settings
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone();
        let later = self.later.lock().unwrap_or_else(|p| p.into_inner()).clone();
        crate::disks::plan(&self.config, &scour_volumes::look(), &settings, &later)
    }

    /// How queries are read now. See [`crate::wire::reading`].
    pub fn reading(&self) -> scour_engine::Reading {
        crate::wire::reading(&self.settings.lock().unwrap_or_else(|p| p.into_inner()))
    }

    pub fn open(dir: PathBuf, config: scour_config::Config) -> Kept {
        Kept {
            settings: Mutex::new(scour_settings::Settings::load(&dir)),
            dir,
            maker: scour_thumbs::Maker::default(),
            config,
            later: Mutex::default(),
        }
    }
}

pub fn dispatch(live: &Live, kept: &Kept, req: Request, emit: &mut dyn Emit) -> Outcome {
    match run(&live.engine(), live, kept, req, emit) {
        Ok(r) => Outcome::Ok(r),
        Err(e) => Outcome::Error(e),
    }
}

fn run(
    engine: &Engine,
    live: &Live,
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
            duplicates_answer(r)
        }
        Request::Grep {
            query,
            pattern,
            mode,
            exact_case,
            binary,
            markup,
            sort,
            descending,
            offset,
            top,
            hits,
            read_budget,
            max_size,
            timeout_ms,
            rate_mib,
        } => {
            let needle = needle_of(mode, pattern)?;
            let ask = scour_engine::GrepAsk {
                needle,
                exact_case,
                // Bytes are looked for where bytes are.
                binary: binary || mode == scour_proto::GrepMode::Hex,
                markup,
                sort,
                descending,
                offset,
                top,
                hits,
                read_budget,
                max_size,
                timeout: std::time::Duration::from_millis(timeout_ms),
                rate: u64::from(rate_mib) << 20,
                background: false,
            };
            // Every reading thread reads as the person asking; see `preview`.
            let enter: scour_grep::Enter = std::sync::Arc::new(crate::as_owner::for_a_read);
            let r = engine.grep(&query, &ask, Some(enter))?;
            Response::Grep(grep_answer(r))
        }
        Request::Settings {} => Response::Settings(Box::new(
            kept.settings
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .clone(),
        )),
        Request::SetSettings { change } => set_settings(engine, live, kept, change),
        Request::Rules {} => rules(kept),
        Request::Places {} => Response::Places(scour_places::look()),
        Request::Volumes {} => volumes(engine, live, kept),
        Request::Postpone { volume } => {
            live.postpone(kept, volume);
            Response::Accepted
        }
        Request::Choose { volume, index } => {
            live.choose(kept, &volume, index);
            Response::Accepted
        }
        // `stat` first, and that is the fence: only a path under a source's
        // root may be looked at — a skipped one too, which this account could
        // open anyway — and no frontend has to remember that. The preview
        // itself opens regular files only: a named pipe would hold this
        // thread for ever.
        Request::Preview { path, at, line } => {
            let entry = engine.stat(&path)?;
            // Read as the person asking, where this is not already them.
            let Ok(_as) = crate::as_owner::for_a_read() else {
                return Err(scour_core::Error::PermissionDenied { path });
            };
            let native = scour_core::path::to_path(&entry.path);
            let mut look = scour_preview::look_near(&native, entry.is_dir, at, line);
            // A legacy Office document with no pages drawn: its words, from an
            // extractor, as a newer one's come from the in-process reader.
            if look.head.is_empty()
                && !entry.is_dir
                && scour_preview::office::is_office(&native)
                && !scour_preview::office::can_convert()
                && let Some(words) = crate::documents::shared().and_then(|d| d.words(&native))
            {
                let mut end = words.len().min(PREVIEW_WORDS);
                while !words.is_char_boundary(end) {
                    end -= 1;
                }
                look.cut = end < words.len();
                look.head = words[..end].to_owned();
                look.shape = "text".to_owned();
                look.kind = "text/plain".to_owned();
            }
            // What the format table draws flows (`scour_formats`): where its
            // pages are not made, and with an extractor to read it — an
            // archive is listed here, and needs none.
            let format = (!entry.is_dir)
                .then(|| scour_formats::of_path(&native))
                .flatten()
                .filter(|f| f.flows);
            look.flow = format.is_some_and(|f| {
                !(f.converts && scour_preview::office::can_convert())
                    && (scour_extract::lists(&native) || crate::documents::shared().is_some())
            });
            Response::Preview(look)
        }
        Request::Flow { path } => {
            let entry = engine.stat(&path)?;
            let Ok(_as) = crate::as_owner::for_a_read() else {
                return Err(scour_core::Error::PermissionDenied { path });
            };
            // An archive is listed here, by its directory or its headers: the
            // extractor would be handed the whole of it. A drawing is kept
            // (`drawn`): the arrows come back, and a window warms the rows
            // beside the one it shows. **A drawing that cannot be made is
            // refused, not failed** — `Io` reads to a window as the connection
            // gone, and an encrypted document took the service down with it.
            let native = scour_core::path::to_path(&entry.path);
            let key = crate::drawn::Key {
                path: entry.path.clone(),
                size: entry.meta.size,
                mtime: entry.meta.mtime,
            };
            let made = if scour_extract::lists(&native) {
                crate::drawn::drawn(key, || {
                    std::fs::File::open(&native)
                        .map_err(|e| e.to_string())
                        .and_then(scour_extract::listing)
                })
            } else {
                let Some(documents) = crate::documents::shared() else {
                    return Err(scour_core::Error::unsupported(
                        "no extractor is installed to read documents",
                    ));
                };
                crate::drawn::drawn(key, || documents.flow(&native))
            };
            match made {
                Ok(flow) => Response::Flow(flow),
                Err(why) => return Err(scour_core::Error::unsupported(why)),
            }
        }
        // The same fence as `preview`, and it matters more: this runs a program
        // on the file. Its mtime is what the thumbnail standard requires. An
        // unheld path is dropped, or one deleted file empties a screenful.
        Request::Thumbnails { files, size } => {
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
            let px = size.unwrap_or(scour_thumbs::cache::ASKED);
            Response::Thumbnails(kept.maker.make_sized(&wanted, px))
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
        Request::Status {} => Response::Status(scour_core::Status {
            asking: live.asking(),
            ..engine.status()
        }),
        // The only request that blocks, so the ceiling is here: a caller asking
        // to sleep for a day would hold a connection thread for a day.
        Request::Await { since, timeout_ms } => {
            let status = engine.await_change(
                since,
                std::time::Duration::from_millis(timeout_ms.min(60_000) as u64),
            );
            Response::Status(scour_core::Status {
                asking: live.asking(),
                ..status
            })
        }
        Request::Stats {} => Response::Stats(engine.stats()?),
        Request::Reread { from_start } => {
            engine.reread(from_start);
            Response::Accepted
        }
        Request::Texts {} => {
            let settings = kept
                .settings
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .clone();
            match crate::documents::shared() {
                Some(documents) => {
                    let held = documents.held(&settings);
                    let needed_mb = held.needed_mb();
                    Response::Texts {
                        on: held.on,
                        dir: held.dir,
                        max_mb: held.max_mb,
                        documents: held.stats.documents,
                        no_text: held.stats.no_text,
                        failed: held.stats.failed,
                        disk: held.stats.disk,
                        dead: held.stats.dead,
                        reader: true,
                        full: held.stats.full,
                        refused: held.stats.refused,
                        needed_mb,
                        memory: held.stats.memory,
                    }
                }
                None => Response::Texts {
                    on: false,
                    dir: String::new(),
                    max_mb: 0,
                    documents: 0,
                    no_text: 0,
                    failed: 0,
                    disk: 0,
                    dead: 0,
                    reader: false,
                    full: false,
                    refused: 0,
                    needed_mb: 0,
                    memory: 0,
                },
            }
        }
        Request::ForgetTexts {} => {
            if let Some(documents) = crate::documents::shared() {
                documents.forget().map_err(|e| scour_core::Error::Io {
                    detail: e.to_string(),
                })?;
            }
            Response::Accepted
        }
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
        Request::Reset {} => {
            live.reset(kept);
            Response::Accepted
        }
        Request::Syntax {} => Response::Text {
            text: scour_query::SYNTAX.to_owned(),
        },
        // The reply goes out before the accept loop is torn down.
        Request::Shutdown {} => Response::Accepted,
        Request::Resources {} => {
            let stats = engine.stats()?;
            let settings = kept
                .settings
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .clone();
            let texts = crate::documents::shared().map(|d| d.held(&settings));
            Response::Resources(crate::resources::collect(&stats, texts.as_ref(), &kept.dir))
        }
        // The index and the texts go the way their own requests go.
        Request::Clear { what } => {
            match what {
                scour_proto::HeldKind::Index => live.reset(kept),
                scour_proto::HeldKind::Texts => {
                    if let Some(documents) = crate::documents::shared() {
                        documents.forget().map_err(|e| scour_core::Error::Io {
                            detail: e.to_string(),
                        })?;
                    }
                }
                other => crate::resources::clear(other)?,
            }
            Response::Accepted
        }
        // As `Shutdown`; `main` starts it again once the index is committed.
        Request::Restart {} if cfg!(unix) => Response::Accepted,
        Request::Restart {} => {
            return Err(scour_core::Error::unsupported(
                "restarting from a face here: restart the service from the system",
            ));
        }
    })
}

/// How much of a legacy Office document's words a preview shows: what
/// `scour-preview` shows of a newer one's.
const PREVIEW_WORDS: usize = 128 * 1024;

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
