//! Walking the tree.

use std::io::Read;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Instant;

use ignore::{WalkBuilder, WalkState};
use scour_core::{
    Caps, ChangeSink, Entry, EntryId, EntrySink, Error, Meta, Result, ScanOptions, ScanReport,
    Source, SourceId, SourceInfo, SourceKind, WatchHandle,
};

use crate::fs::FsTraits;
use crate::path;
use crate::rules::Rules;

/// What a walker thread sends back.
enum Msg {
    /// A thread's worth of entries, not one entry. See [`Batch`].
    Entries(Vec<Entry>),
    /// Where the walk could not look, and why.
    Unreadable(String, String),
}

/// How many entries a walker thread collects before handing them over.
/// Too small and the channel is busy again; too large and the walk stutters.
const BATCH: usize = 512;

/// How many batches may be in the air; bound × batch is what the channel holds.
const IN_FLIGHT: usize = 64;

/// Step out of the way of everything else on the machine.
/// Per thread, so a query typed during the first scan is still answered at full
/// speed. A cold NTFS walk here is 61 s of device time.
pub(crate) fn stand_aside() {
    // SAFETY: ordinary syscalls on the calling thread, and failing to become
    // polite is not failing to scan. Linux only: `ioprio_set` has no portable name.
    #[cfg(target_os = "linux")]
    unsafe {
        // Ten *more*: `nice` adds where `setpriority` sets, and the unit may say `Nice=15`.
        libc::nice(10);
        // IOPRIO_CLASS_IDLE (3) << IOPRIO_CLASS_SHIFT (13). The disk is the half that matters.
        const IOPRIO_WHO_PROCESS: libc::c_int = 1;
        const IOPRIO_CLASS_IDLE: libc::c_int = 3;
        libc::syscall(
            libc::SYS_ioprio_set,
            IOPRIO_WHO_PROCESS,
            0,
            IOPRIO_CLASS_IDLE << 13,
        );
    }
}

/// One walker thread's outgoing buffer.
/// One send an entry made the channel the scan: 8,231,481 voluntary context switches
/// for 870,000 entries, against 44,238 for the same walk batched.
struct Batch {
    tx: crossbeam_channel::Sender<Msg>,
    buf: Vec<Entry>,
}

impl Batch {
    fn new(tx: crossbeam_channel::Sender<Msg>) -> Batch {
        Batch {
            tx,
            buf: Vec::with_capacity(BATCH),
        }
    }

    /// Returns false once the far end is gone, which is a walk to abandon.
    fn push(&mut self, e: Entry) -> bool {
        self.buf.push(e);
        self.buf.len() < BATCH || self.flush()
    }

    fn flush(&mut self) -> bool {
        if self.buf.is_empty() {
            return true;
        }
        let full = std::mem::replace(&mut self.buf, Vec::with_capacity(BATCH));
        self.tx.send(Msg::Entries(full)).is_ok()
    }
}

/// The tail of a thread's last batch.
/// `ignore` gives a visitor no way to say it has finished but does drop the box
/// when the thread ends; without this a walk loses up to [`BATCH`] a thread.
impl Drop for Batch {
    fn drop(&mut self) {
        self.flush();
    }
}

/// How many unreadable subtrees are worth remembering by name.
/// Past this the root is not vouched for: a walk blind in thousands of places proves nothing.
const MAX_BLIND: usize = 4_096;

/// Where the walker's error happened.
/// The path is wrapped: `WithDepth` around `WithPath` around the `io::Error`.
fn where_of(e: &ignore::Error) -> Option<&std::path::Path> {
    match e {
        ignore::Error::WithPath { path, .. } => Some(path),
        ignore::Error::WithDepth { err, .. } | ignore::Error::WithLineNumber { err, .. } => {
            where_of(err)
        }
        ignore::Error::Loop { child, .. } => Some(child),
        ignore::Error::Partial(all) => all.iter().find_map(where_of),
        _ => None,
    }
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
        Self {
            id,
            name: name.into(),
            roots,
            kind: SourceKind::Local,
            watch: true,
            traits,
            medium,
            probes,
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
    fn pulse(&self) -> Option<u64> {
        let mut any = false;
        let mut folded = 0u64;
        for (root, probe) in self.roots.iter().zip(&self.probes) {
            if let Some(n) = crate::pulse::read(root, probe) {
                any = true;
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
        let rules = Rules::from_options(opts);
        Some(Box::new(move |path: &str, is_dir: bool| {
            let name = path.rsplit('/').next().unwrap_or(path);
            rules.excludes(path, name, is_dir)
        }))
    }

    fn scan(&self, opts: &ScanOptions, sink: &mut dyn EntrySink) -> Result<ScanReport> {
        let started = Instant::now();
        let rules = Rules::from_options(opts);
        let roots: Vec<PathBuf> = match &opts.subtree {
            Some(s) => vec![path::to_path(s)],
            None => self.roots.clone(),
        };
        let Some((first, rest)) = roots.split_first() else {
            return Ok(ScanReport::default());
        };

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

        // `ignore`'s parallel walker with its opinions off: an index must hold what
        // `.gitignore` skips.
        let mut blind: Vec<String> = Vec::new();
        let mut too_blind = false;
        let mut builder = WalkBuilder::new(first);
        for r in rest {
            builder.add(r);
        }
        builder
            .standard_filters(false)
            .hidden(!opts.hidden)
            .follow_links(opts.follow_symlinks)
            .same_file_system(false)
            // Zero means "decide for me", and the device decides: on a spinning
            // disk every extra thread is another seek.
            .threads(if opts.threads == 0 {
                self.medium.threads(
                    std::thread::available_parallelism()
                        .map(|n| n.get())
                        .unwrap_or(4),
                )
            } else {
                opts.threads
            });

        let src_id = self.id;
        let excluded = AtomicU64::new(0);
        let unreadable = AtomicU64::new(0);
        let entries = AtomicU64::new(0);
        let dirs = AtomicU64::new(0);
        let cancelled = AtomicBool::new(false);
        let want_meta = !opts.skip_metadata;
        let real_modes = self.traits.real_modes;

        // The walker runs on its own threads and the sink is drained on this one,
        // over a bounded channel: backpressure free, and no `Send` bound on the trait.
        let (tx, rx) = crossbeam_channel::bounded::<Msg>(IN_FLIGHT);

        std::thread::scope(|scope| {
            let walker_tx = tx.clone();
            let (rules, excluded, unreadable, entries, dirs, cancelled) =
                (&rules, &excluded, &unreadable, &entries, &dirs, &cancelled);
            let walker = scope.spawn(move || {
                builder.build_parallel().run(|| {
                    let tx = walker_tx.clone();
                    let mut batch = Batch::new(tx.clone());
                    // On the first entry, not here: `ignore` builds the visitor on the
                    // spawning thread.
                    let mut polite = false;
                    Box::new(move |result| {
                        if !polite {
                            polite = true;
                            stand_aside();
                        }
                        if cancelled.load(Ordering::Relaxed) {
                            return WalkState::Quit;
                        }
                        let de = match result {
                            Ok(de) => de,
                            Err(e) => {
                                // Reported, not swallowed.
                                unreadable.fetch_add(1, Ordering::Relaxed);
                                // The path, not only the count: an unreadable
                                // directory still holds its files and the sweep
                                // must spare it — see `ScanReport::blind`.
                                let where_ = where_of(&e).map(path::from_path).unwrap_or_default();
                                let _ = tx.send(Msg::Unreadable(where_, e.to_string()));
                                return WalkState::Continue;
                            }
                        };
                        let is_dir = de.file_type().is_some_and(|t| t.is_dir());
                        let normalised = path::from_path(de.path());
                        // The same encoding as the path, so rules compare like with like.
                        let name = path::from_path(std::path::Path::new(de.file_name()));

                        if !rules.is_empty() && rules.excludes(&normalised, &name, is_dir) {
                            excluded.fetch_add(1, Ordering::Relaxed);
                            // Prune — unless an allowed subtree lives below it.
                            return if is_dir && !rules.may_contain_allowed(&normalised) {
                                WalkState::Skip
                            } else {
                                WalkState::Continue
                            };
                        }

                        // Skipping the per-entry `stat` is a usable index in a minute
                        // rather than ten; sizes and dates arrive in a second pass.
                        let md = if want_meta { de.metadata().ok() } else { None };
                        entries.fetch_add(1, Ordering::Relaxed);
                        if is_dir {
                            dirs.fetch_add(1, Ordering::Relaxed);
                        }
                        if !batch.push(entry_of(
                            src_id,
                            &normalised,
                            md.as_ref(),
                            is_dir,
                            real_modes,
                        )) {
                            return WalkState::Quit;
                        }
                        WalkState::Continue
                    })
                });
            });
            // The walker holds clones of the sender; this one has to go.
            drop(tx);

            let mut stopping = false;
            for msg in rx {
                if stopping {
                    // Keep draining, so no walker thread is left on a full channel.
                    continue;
                }
                match msg {
                    Msg::Entries(batch) => {
                        for e in batch {
                            if sink.push(e).is_stop() {
                                cancelled.store(true, Ordering::Relaxed);
                                stopping = true;
                                break;
                            }
                        }
                    }
                    Msg::Unreadable(path, detail) => {
                        // Past the ceiling the root stops being vouched for at all.
                        if !path.is_empty() && blind.len() < MAX_BLIND {
                            blind.push(path.clone());
                        } else if !path.is_empty() {
                            too_blind = true;
                        }
                        sink.unreadable(&path, &Error::Io { detail });
                    }
                }
            }
            let _ = walker.join();
        });

        // Asked again: a device number that is not the one the walk started on means
        // the walk was about something else.
        let vouched: Vec<String> = before
            .iter()
            .filter(|(root, dev)| dev.is_some() && *dev == device_of(root) && !too_blind)
            .map(|(root, _)| path::from_path(root))
            .collect();

        Ok(ScanReport {
            entries: entries.load(Ordering::Relaxed),
            dirs: dirs.load(Ordering::Relaxed),
            excluded: excluded.load(Ordering::Relaxed),
            unreadable: unreadable.load(Ordering::Relaxed),
            took_ms: started.elapsed().as_millis() as u64,
            cancelled: cancelled.load(Ordering::Relaxed),
            vouched,
            blind,
        })
    }

    fn watch(&self, opts: &ScanOptions, sink: Box<dyn ChangeSink>) -> Result<Box<dyn WatchHandle>> {
        crate::watch::start(self.clone(), opts, sink)
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
