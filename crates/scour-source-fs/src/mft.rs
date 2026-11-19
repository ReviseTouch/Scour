//! The first walk of an NTFS volume, read from its master file table: a walk
//! of PC2's C: was minutes, this is seconds. The reading is surefoot's
//! (`surefoot::mft`); what is here is Scour's side of it — the rules and the
//! hidden setting decide, and the rows become entries.

use std::io;
use std::ops::ControlFlow;

use scour_core::{EntrySink, Meta, SourceId};
use surefoot::mft::{Choice, Record, Seen, Visitor};
pub(crate) use surefoot::mft::{Counts, ReadAt};

use crate::rules::Rules;

/// Read the whole volume into `sink`, as a walk of `root` — the volume's root,
/// `C:/` — would. `hidden` false leaves out what Windows hides and dot names.
pub(crate) fn read(
    volume: &impl ReadAt,
    source: SourceId,
    root: &str,
    rules: &Rules,
    hidden: bool,
    sink: &mut dyn EntrySink,
) -> io::Result<Counts> {
    surefoot::mft::read(
        volume,
        root,
        &mut Rows {
            source,
            rules,
            hidden,
            sink,
        },
    )
}

/// The volume behind a root that is a whole drive, `C:/`, opened for reading.
/// None for anything else, or when this process may not open it.
#[cfg(windows)]
pub(crate) fn open(root: &str) -> Option<std::fs::File> {
    surefoot::mft::open(root).ok()
}

struct Rows<'a> {
    source: SourceId,
    rules: &'a Rules,
    hidden: bool,
    sink: &'a mut dyn EntrySink,
}

impl Visitor for Rows<'_> {
    fn judge(&mut self, seen: &Seen<'_>) -> Choice {
        judge(self.rules, self.hidden, seen)
    }

    fn emit(&mut self, r: Record<'_>) -> ControlFlow<()> {
        let times = r.times.unwrap_or_default();
        let size = if r.is_dir { 0 } else { r.size as i64 };
        let entry = scour_core::Entry {
            id: scour_core::EntryId::path_hash(self.source, r.path),
            path: r.path.to_owned(),
            is_dir: r.is_dir,
            meta: Meta {
                size,
                mtime: times.modified,
                ctime: times.created,
                atime: times.accessed,
                mode: 0,
                uid: 0,
                gid: 0,
                disk: size,
                items: -1,
                links: 1,
            },
        };
        if self.sink.push(entry).is_stop() {
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    }

    fn stopping(&self) -> bool {
        self.sink.stopping()
    }
}

/// What a walk would do with this name: a directory the rules exclude is still
/// gone through when something they allow may be below it.
fn judge(rules: &Rules, hidden: bool, seen: &Seen<'_>) -> Choice {
    let skip = !hidden && (seen.hidden || seen.name.starts_with('.'));
    if !skip && (rules.is_empty() || !rules.excludes(seen.path, seen.name, seen.is_dir)) {
        Choice::Take
    } else if seen.is_dir && rules.may_contain_allowed(seen.path) {
        Choice::Pass
    } else {
        Choice::Prune
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seen<'a>(path: &'a str, is_dir: bool, hidden: bool) -> Seen<'a> {
        Seen {
            path,
            name: path.rsplit('/').next().unwrap_or(path),
            is_dir,
            hidden,
        }
    }

    #[test]
    fn rules_and_hidden_names_decide_as_in_a_walk() {
        let opts = scour_core::ScanOptions {
            exclude_paths: vec!["C:/Windows".into()],
            ..Default::default()
        };
        let rules = Rules::from_options(&opts);
        let j = |path, is_dir, hidden, show| judge(&rules, show, &seen(path, is_dir, hidden));
        assert_eq!(j("C:/Users", true, false, false), Choice::Take);
        assert_eq!(j("C:/Windows", true, false, true), Choice::Prune);
        assert_eq!(j("C:/pagefile.sys", false, true, false), Choice::Prune);
        assert_eq!(j("C:/pagefile.sys", false, true, true), Choice::Take);
        assert_eq!(j("C:/Users/.cache", true, false, false), Choice::Prune);
        assert_eq!(j("C:/Users/.cache", true, false, true), Choice::Take);
    }

    #[test]
    fn an_excluded_directory_with_an_allowed_path_below_is_gone_through() {
        let opts = scour_core::ScanOptions {
            exclude_paths: vec!["C:/Users".into()],
            allow: vec!["C:/Users/hasan/Belgeler".into()],
            ..Default::default()
        };
        let rules = Rules::from_options(&opts);
        assert_eq!(
            judge(&rules, true, &seen("C:/Users", true, false)),
            Choice::Pass
        );
        assert_eq!(
            judge(&rules, true, &seen("C:/Users/x", true, false)),
            Choice::Prune
        );
        assert_eq!(
            judge(&rules, true, &seen("C:/Users/x.txt", false, false)),
            Choice::Prune
        );
    }
}
