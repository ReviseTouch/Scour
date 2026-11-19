//! Walking the tree.

use std::io::Read;
use std::ops::ControlFlow;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use scour_core::{
    Caps, ChangeSink, Entry, EntryId, EntrySink, Error, Meta, Result, ScanOptions, ScanReport,
    Source, SourceId, SourceInfo, SourceKind, WatchHandle,
};
use surefoot::{Event, Priority, Threads, Visit, Walk};

use crate::fs::{FsTraits, MediumExt};
use crate::path;
use crate::rules::Rules;

/// How many entries a walker thread collects before handing them over.
/// Too small and the channel is busy again; too large and the walk stutters.
/// One send an entry made the channel the scan: 8,231,481 voluntary context
/// switches for 870,000 entries, against 44,238 for the same walk batched.
const BATCH: usize = 512;

/// How many batches may be in the air; bound × batch is what the channel holds.
const IN_FLIGHT: usize = 64;

/// How many unreadable subtrees are worth remembering by name.
/// Past this the root is not vouched for: a walk blind in thousands of places proves nothing.
const MAX_BLIND: usize = 4_096;

/// Whether a second walk would find this place the same: refused by permission, or
/// gone between being listed and being read. Anything else — an I/O error, a full
/// file table, a link that loops — may be over by the next walk, and is what a
/// retry is for.
fn is_lasting(e: &std::io::Error) -> bool {
    matches!(
        e.kind(),
        std::io::ErrorKind::PermissionDenied
            | std::io::ErrorKind::NotFound
            | std::io::ErrorKind::NotADirectory
    )
}

/// Which filesystem a path is on, or nothing if it cannot be asked.
/// What matters is that it is the same at the end of a walk as at the start.
fn device_of(p: &std::path::Path) -> Option<u64> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        std::fs::metadata(p).ok().map(|m| m.dev())
    }
    #[cfg(not(unix))]
    {
        // Windows has volume serial numbers through `GetFileInformationByHandle`.
        std::fs::metadata(p).ok().map(|_| 0)
    }
}

#[derive(Debug, Clone)]
pub struct FsSource {
    id: SourceId,
    name: String,
    roots: Vec<PathBuf>,
    kind: SourceKind,
    watch: bool,
    /// What the filesystems under the roots promise, asked once at construction.
    traits: FsTraits,
    /// What they are like to read: thread count and how long events settle.
    medium: crate::fs::Medium,
    /// One per root, decided at construction: neither filesystem nor device changes under a mount.
    probes: Vec<crate::pulse::Probe>,
    /// btrfs subvolumes mounted under the roots, which the walk goes into and
    /// whose changes no root's `ctransid` shows: without them a change in
    /// `/var/log` waited for the half-hourly check. Other sources' roots are
    /// taken out in [`FsSource::with_skip`].
    nested: Vec<PathBuf>,
    /// Other sources' roots under these, which this one does not go into.
    skip: Vec<String>,
    /// See [`scour_core::Source::patience`].
    patience: std::time::Duration,
    /// Taken by every walker thread before its first entry, kept until it ends.
    walk_as: Option<WalkAs>,
    /// Where a change journal's position is kept between runs, when there is one.
    journal_dir: Option<PathBuf>,
    /// The physical disk under the first root; see [`crate::fs::disk_of`].
    disk: Option<String>,
}

/// An identity a walker thread takes before it reads anything: a service on
/// Windows reads a network share as the person who added it, since the share
/// does not know SYSTEM. Impersonation belongs to a thread, so it is asked for
/// on each of them; the guard it returns gives the identity back when dropped.
#[derive(Clone)]
pub struct WalkAs(
    pub std::sync::Arc<dyn Fn() -> Option<Box<dyn std::any::Any + Send>> + Send + Sync>,
);

impl std::fmt::Debug for WalkAs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("WalkAs")
    }
}

impl FsSource {
    pub fn new(id: SourceId, name: impl Into<String>, roots: Vec<PathBuf>) -> Self {
        // One `statfs` per root. A source spanning two takes the narrower promise.
        let traits = roots
            .iter()
            .map(|r| crate::fs::traits_of(r))
            .reduce(FsTraits::and)
            .unwrap_or(FsTraits::UNKNOWN);
        // The slowest root decides: one spinning disk makes twenty threads wrong for all.
        let medium = roots
            .iter()
            .map(|r| crate::fs::medium_of(r))
            .reduce(|a, b| if a.threads(64) <= b.threads(64) { a } else { b })
            .unwrap_or(crate::fs::Medium::Unknown);
        let probes = roots.iter().map(|r| crate::pulse::probe_for(r)).collect();
        let nested = roots.iter().flat_map(|r| crate::pulse::nested(r)).collect();
        let disk = roots.first().and_then(|r| crate::fs::disk_of(r));
        Self {
            id,
            name: name.into(),
            roots,
            kind: SourceKind::Local,
            watch: true,
            traits,
            medium,
            probes,
            nested,
            skip: Vec::new(),
            patience: std::time::Duration::ZERO,
            walk_as: None,
            journal_dir: None,
            disk,
        }
    }

    /// The path, resolved, and refused if it is not really under a root: a prefix
    /// comparison is not containment. The parent is resolved and the last component
    /// joined back unresolved, so a symlink is `stat`ed as itself. Not the race.
    fn inside(&self, p: &str) -> Result<PathBuf> {
        let native = path::to_path(p);
        let missing = || Error::NotFound { path: p.to_owned() };
        // Lexically first, so the refusal is cheap. The index never produces a `.` or `..`.
        if native.components().any(|c| {
            matches!(
                c,
                std::path::Component::ParentDir | std::path::Component::CurDir
            )
        }) {
            return Err(missing());
        }
        let (parent, name) = match (native.parent(), native.file_name()) {
            (Some(parent), Some(name)) => (parent, name),
            // A root itself has no name to join back on.
            _ => (native.as_path(), std::ffi::OsStr::new("")),
        };
        let real_parent = parent.canonicalize().map_err(|_| missing())?;
        let under = self.roots.iter().any(|root| {
            let root = root.canonicalize().unwrap_or_else(|_| root.clone());
            real_parent == root || real_parent.starts_with(&root)
        });
        if !under {
            return Err(missing());
        }
        Ok(if name.is_empty() {
            real_parent
        } else {
            real_parent.join(name)
        })
    }

    /// Whether this source should be watched for changes.
    /// Expressed by withholding the capability rather than by a flag to remember.
    pub fn with_watch(mut self, watch: bool) -> Self {
        self.watch = watch;
        self
    }

    /// Leave these alone: other sources' roots under this one's. See `skip`.
    pub fn with_skip(mut self, skip: Vec<PathBuf>) -> Self {
        self.nested
            .retain(|n| !skip.iter().any(|s| n == s || n.starts_with(s)));
        self.skip = skip.iter().map(|p| path::from_path(p)).collect();
        self
    }

    /// Walked no oftener than this when nothing watches it, however its pulse moves.
    /// Keep a change journal's position in this directory, so the next run
    /// reads on from it instead of walking.
    pub fn with_journal_dir(mut self, dir: Option<PathBuf>) -> Self {
        self.journal_dir = dir;
        self
    }

    #[cfg_attr(not(windows), allow(dead_code))]
    pub(crate) fn walk_as(&self) -> Option<&WalkAs> {
        self.walk_as.as_ref()
    }

    #[cfg_attr(not(windows), allow(dead_code))]
    pub(crate) fn journal_dir(&self) -> Option<&std::path::Path> {
        self.journal_dir.as_deref()
    }

    /// See [`WalkAs`].
    pub fn with_walk_as(mut self, walk_as: Option<WalkAs>) -> Self {
        self.walk_as = walk_as;
        self
    }

    pub fn with_patience(mut self, least: std::time::Duration) -> Self {
        self.patience = least;
        self
    }

    pub fn with_kind(mut self, kind: SourceKind) -> Self {
        self.kind = kind;
        self
    }

    pub fn roots(&self) -> &[PathBuf] {
        &self.roots
    }

    /// The source id, without going through the trait.
    pub fn source_id(&self) -> SourceId {
        self.id
    }

    /// Whether `st_mode` is the file's own rather than the mount's.
    pub fn real_modes(&self) -> bool {
        self.traits.real_modes
    }

    // Read by the fanotify walk, which is Linux.
    #[cfg(target_os = "linux")]
    /// What the mounts under the roots are like to read.
    /// The fanotify backend makes the same concurrency decision — `DirMap::build`.
    pub(crate) fn medium(&self) -> crate::fs::Medium {
        self.medium
    }
}

impl Source for FsSource {
    fn id(&self) -> SourceId {
        self.id
    }

    fn describe(&self) -> SourceInfo {
        SourceInfo {
            id: self.id,
            name: self.name.clone(),
            kind: self.kind,
            roots: self.roots.iter().map(|r| path::from_path(r)).collect(),
            caps: self.caps(),
        }
    }

    /// The roots' pulses, folded into one number.
    /// Folded because any root moving is a yes; `None` only when none can be asked.
    fn patience(&self) -> std::time::Duration {
        self.patience
    }

    fn disk(&self) -> Option<String> {
        self.disk.clone()
    }

    fn pulse(&self) -> Option<u64> {
        let mut any = false;
        let mut folded = 0u64;
        for (root, probe) in self.roots.iter().zip(&self.probes) {
            if let Some(n) = crate::pulse::read(root, probe) {
                any = true;
                folded = folded.rotate_left(17) ^ n;
            }
        }
        // Measured here: `/` and `/var/log` each moved twice a minute, so the
        // system disk was already walked as often as its patience allows.
        for sub in &self.nested {
            if let Some(n) = crate::pulse::subvolume(sub) {
                folded = folded.rotate_left(17) ^ n;
            }
        }
        any.then_some(folded)
    }

    fn caps(&self) -> Caps {
        // **Not `CONTENT`.** `open` refuses unconditionally, and `scour sources` prints this.
        let mut c = Caps::empty();
        if self.watch {
            c |= Caps::WATCH;
        }
        if self.watch && cfg!(any(windows, target_os = "macos")) {
            c |= Caps::RECURSIVE_WATCH;
        }
        // Measured: the same NTFS disk is insensitive under Windows and sensitive under ntfs3.
        if self.traits.case_sensitive {
            c |= Caps::CASE_SENSITIVE;
        }
        c
    }

    /// The walk's own rules, compiled once and handed back as a test.
    /// `Rules::excludes`, not `excludes_path`: the latter has no `is_dir` and
    /// reads generously for the watcher, which as a test for the index is wrong.
    fn excluder(
        &self,
        opts: &ScanOptions,
    ) -> Option<Box<dyn Fn(&str, bool) -> bool + Send + Sync>> {
        let rules = Rules::from_options(&crate::skip::own(opts, &self.roots, &self.skip));
        Some(Box::new(move |path: &str, is_dir: bool| {
            let name = path.rsplit('/').next().unwrap_or(path);
            rules.excludes(path, name, is_dir)
        }))
    }

    fn scan(&self, opts: &ScanOptions, sink: &mut dyn EntrySink) -> Result<ScanReport> {
        let started = Instant::now();
        let opts = &*crate::skip::own(opts, &self.roots, &self.skip);
        let rules = Rules::from_options(opts);
        let roots: Vec<PathBuf> = match &opts.subtree {
            Some(s) => vec![path::to_path(s)],
            None => self.roots.clone(),
        };
        if roots.is_empty() {
            return Ok(ScanReport::default());
        }
        // This thread looks at the roots before and after the walk: as the
        // same person the walker threads read them as, or a share the walk
        // read whole is judged unreadable (PC2, 17 November).
        let _as = self.walk_as.as_ref().and_then(|w| (w.0)());

        // A whole drive read from its master file table: seconds where the walk
        // is minutes. Only where this process may open the volume — the
        // service — and only for a whole walk; anything that goes wrong walks.
        #[cfg(windows)]
        if opts.subtree.is_none()
            && let [first] = roots.as_slice()
        {
            let root = path::from_path(first);
            if let Some(volume) = crate::mft::open(&root) {
                match crate::mft::read(&volume, self.id, &root, &rules, opts.hidden, sink) {
                    Ok(c) => {
                        scour_core::note!(
                            "scourd: {root} read from its master file table: {} entries in {} ms",
                            c.entries,
                            started.elapsed().as_millis()
                        );
                        return Ok(ScanReport {
                            entries: c.entries,
                            dirs: c.dirs,
                            excluded: c.excluded,
                            took_ms: started.elapsed().as_millis() as u64,
                            cancelled: c.stopped,
                            vouched: if c.stopped { Vec::new() } else { vec![root] },
                            ..ScanReport::default()
                        });
                    }
                    Err(e) => scour_core::note!(
                        "scourd: the master file table of {root} could not be read ({e}); walking it"
                    ),
                }
            }
        }

        // An unreadable root and a genuinely empty one both come back `entries: 0`,
        // and reconciling the first deletes the source; an unmounted mount is a
        // readable, empty directory. Distrusted per root, and for whole sources only.
        let whole_source = opts.subtree.is_none();
        let before: Vec<(PathBuf, Option<u64>)> = roots
            .iter()
            .map(|r| {
                let ok = match std::fs::read_dir(r) {
                    Err(_) => false,
                    Ok(mut entries) => !(whole_source && entries.next().is_none()),
                };
                (r.clone(), ok.then(|| device_of(r)).flatten())
            })
            .collect();

        // Zero means "decide for me", and the device decides: on a spinning
        // disk every extra thread is another seek. A borrowed identity reads
        // on one thread, which is right for a share anyway.
        let threads = if self.walk_as.is_some() {
            1
        } else if opts.threads == 0 {
            self.medium.threads(
                std::thread::available_parallelism()
                    .map(|n| n.get())
                    .unwrap_or(4),
            )
        } else {
            opts.threads
        };
        let mut walk = Walk::new(roots.iter().cloned())
            .threads(Threads::Fixed(threads))
            .medium(self.medium)
            // Per thread, so a query typed during the first scan is still
            // answered at full speed. A cold NTFS walk here is 61 s of device time.
            .priority(Priority::Background)
            .skip_hidden(!opts.hidden)
            .follow_links(opts.follow_symlinks)
            .batch(BATCH)
            .queue(IN_FLIGHT);
        if let Some(walk_as) = self.walk_as.clone() {
            // Taken on each reading thread before its first read: as SYSTEM a
            // worker found a share's root unreadable and walked nothing
            // (PC2, 17 November).
            walk = walk.on_thread(move || Box::new((walk_as.0)()) as Box<dyn std::any::Any>);
        }

        let src_id = self.id;
        let excluded = AtomicU64::new(0);
        let want_meta = !opts.skip_metadata;
        let real_modes = self.traits.real_modes;
        let rules = &rules;

        let (mut entries, mut dirs, mut unreadable, mut lasting) = (0u64, 0u64, 0u64, 0u64);
        let mut blind: Vec<String> = Vec::new();
        let mut too_blind = false;

        // The visitor runs on the reading threads and the sink is drained on
        // this one, over a bounded queue: backpressure free, and no `Send`
        // bound on the trait.
        let report = walk.run(
            |found| {
                let is_dir = found.is_dir();
                let normalised = path::from_path(found.path());
                // The same encoding as the path, so rules compare like with like.
                let name = path::from_path(std::path::Path::new(found.file_name()));

                if !rules.is_empty() && rules.excludes(&normalised, &name, is_dir) {
                    excluded.fetch_add(1, Ordering::Relaxed);
                    // Prune — unless an allowed subtree lives below it.
                    return if is_dir && !rules.may_contain_allowed(&normalised) {
                        Visit::Prune
                    } else {
                        Visit::Pass
                    };
                }

                // Skipping the per-entry `stat` is a usable index in a minute
                // rather than ten; sizes and dates arrive in a second pass.
                let md = if want_meta {
                    found.metadata().ok()
                } else {
                    None
                };
                Visit::Take(entry_of(
                    src_id,
                    &normalised,
                    md.as_ref(),
                    is_dir,
                    real_modes,
                ))
            },
            |event| {
                match event {
                    Event::Batch(batch) => {
                        for e in batch {
                            entries += 1;
                            dirs += u64::from(e.is_dir);
                            if sink.push(e).is_stop() {
                                return ControlFlow::Break(());
                            }
                        }
                    }
                    // Reported, not swallowed — and the path, not only the
                    // count: an unreadable directory still holds its files and
                    // the sweep must spare it — see `ScanReport::blind`.
                    Event::Unreadable(refusal) => {
                        unreadable += 1;
                        lasting += u64::from(is_lasting(&refusal.error));
                        let at = path::from_path(&refusal.path);
                        // Past the ceiling the root stops being vouched for at all.
                        if blind.len() < MAX_BLIND {
                            blind.push(at.clone());
                        } else {
                            too_blind = true;
                        }
                        let detail = format!("{}: {}", refusal.path.display(), refusal.error);
                        sink.unreadable(&at, &Error::Io { detail });
                    }
                }
                ControlFlow::Continue(())
            },
        );

        // Asked again: a device number that is not the one the walk started on means
        // the walk was about something else.
        let vouched: Vec<String> = before
            .iter()
            .filter(|(root, dev)| dev.is_some() && *dev == device_of(root) && !too_blind)
            .map(|(root, _)| path::from_path(root))
            .collect();

        Ok(ScanReport {
            entries,
            dirs,
            excluded: excluded.load(Ordering::Relaxed),
            unreadable,
            lasting,
            took_ms: started.elapsed().as_millis() as u64,
            cancelled: report.cancelled,
            vouched,
            blind,
        })
    }

    fn watch(&self, opts: &ScanOptions, sink: Box<dyn ChangeSink>) -> Result<Box<dyn WatchHandle>> {
        let inner = crate::watch::start(
            self.clone(),
            &crate::skip::own(opts, &self.roots, &self.skip),
            sink,
        )?;
        Ok(Box::new(crate::skip::Skipping {
            inner,
            roots: self.roots.clone(),
            skip: self.skip.clone(),
        }))
    }

    /// One `stat` a path, straight into the sink — see [`Source::recheck`].
    /// `fresh`, because a path that has become a directory is a subtree.
    fn recheck(&self, paths: &[String], sink: &dyn scour_core::ChangeSink) -> usize {
        for path in paths {
            crate::watch::look(self.id, self.traits.real_modes, path, true, sink);
        }
        paths.len()
    }

    fn open(&self, _id: &EntryId) -> Result<Box<dyn Read + Send>> {
        // An id is not a path: extraction resolves one through `stat` first.
        Err(Error::unsupported("opening by entry id"))
    }

    fn stat(&self, p: &str) -> Result<Entry> {
        let native = self.inside(p)?;
        let md = std::fs::symlink_metadata(&native).map_err(|e| Error::io(&e, p))?;
        Ok(entry_of(
            self.id,
            &path::from_path(&native),
            Some(&md),
            md.is_dir(),
            self.traits.real_modes,
        ))
    }
}

/// Build an entry from a normalised path and, when it was worth the syscall, its
/// metadata. The identity is the path — see below — so metadata is optional.
pub(crate) fn entry_of(
    source: SourceId,
    path: &str,
    md: Option<&std::fs::Metadata>,
    is_dir: bool,
    real_modes: bool,
) -> Entry {
    // **The path, always.** A row is a name, not an object: save-by-rename leaves
    // the old inode's row with nothing able to say it is gone. Measured on the
    // live index, 267 rows at one path — one per save.
    let id = EntryId::path_hash(source, path);
    let mut meta = md
        .map(|m| Meta::from_std(m, is_dir))
        .unwrap_or(Meta::UNKNOWN);
    // A mode the mount invented is not a mode: `/mnt/depo` is mounted 0022, so
    // 293,811 files there classified `exec` against 21,342 beside it. Replaced
    // rather than zeroed, so `mode_string` still prints something true.
    if !real_modes && meta != Meta::UNKNOWN {
        meta.mode = if is_dir { 0o040755 } else { 0o100644 };
    }
    Entry {
        id,
        is_dir,
        meta,
        path: path.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::is_lasting;
    use std::io::{Error, ErrorKind};

    #[test]
    fn a_refusal_or_a_vanished_directory_is_lasting_and_nothing_else_is() {
        assert!(is_lasting(&ErrorKind::PermissionDenied.into()));
        assert!(is_lasting(&ErrorKind::NotFound.into()));
        assert!(is_lasting(&ErrorKind::NotADirectory.into()));
        // What a retry is for.
        assert!(!is_lasting(&ErrorKind::TimedOut.into()));
        #[cfg(unix)]
        {
            assert!(!is_lasting(&Error::from_raw_os_error(libc::EIO)));
            assert!(!is_lasting(&Error::from_raw_os_error(libc::EMFILE)));
        }
        // How the walk reports a link that leads back above itself.
        assert!(!is_lasting(&Error::other("a loop")));
    }
}
