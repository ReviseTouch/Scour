//! Settings written and the rules read back: the one place a change to what
//! is skipped is turned into what has to leave the index or be walked again.

use super::*;

/// A setting changed. Written where the change happens, not at shutdown: a
/// killed process never gets to write anything.
pub(super) fn set_settings(
    engine: &Engine,
    live: &Live,
    kept: &Kept,
    change: scour_settings::Change,
) -> Response {
    let replan = change.moves_sources();
    let tunes = change.tunes();
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
    // How words are read is the engine's to know: every question
    // that takes a query parses it there.
    engine.set_reading(crate::wire::reading(&held));
    if tunes {
        crate::wire::tune(&held);
    }
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
                scour_core::note!("scourd: rules opened {}; walking those", paths.join(", "));
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
    } else {
        drop(held);
    }
    // What is said about plugged-in disks, or a folder added: planned
    // again, which swaps the engine when the sources changed. After
    // the lock, which planning takes.
    if replan {
        live.replan(kept);
    }
    Response::Accepted
}

/// Three lists, not one: the built-in set is code, `config.toml` is
/// hand-written, and only the third can be deleted from a window. Read from
/// where each group is written, not from what the engine enforces — it holds
/// the merged set minus what is switched off, so a switched-off rule would
/// vanish from the panel with no way to turn it back on.
pub(super) fn rules(kept: &Kept) -> Response {
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

/// What a rule change re-admits, and therefore what has to be walked: excluding
/// takes entries out of an index that holds them, un-excluding asks for entries
/// only the filesystem has.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Opened {
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
pub(super) fn opened_up(
    before: &scour_core::ScanOptions,
    after: &scour_core::ScanOptions,
) -> Opened {
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
pub(super) fn rules_of(s: &scour_settings::Settings) -> [Vec<String>; 5] {
    [
        s.exclude_paths.clone(),
        s.exclude_dirs.clone(),
        s.exclude_files.clone(),
        s.exclude_allow.clone(),
        s.exclude_off.clone(),
    ]
}
