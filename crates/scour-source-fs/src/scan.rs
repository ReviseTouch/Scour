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
    Entry(Entry),
    Unreadable(String),
}

#[derive(Debug, Clone)]
pub struct FsSource {
    id: SourceId,
    name: String,
    roots: Vec<PathBuf>,
    kind: SourceKind,
    watch: bool,
    /// What the filesystems under the roots actually promise, asked once at
    /// construction rather than assumed at compile time.
    traits: FsTraits,
    /// What they are like to read: how many threads are worth using, and how
    /// long to let events settle.
    medium: crate::fs::Medium,
}

impl FsSource {
    pub fn new(id: SourceId, name: impl Into<String>, roots: Vec<PathBuf>) -> Self {
        // One `statfs` per root, at construction. A source spanning two
        // filesystems takes the narrower promise of the two — see `FsTraits`.
        let traits = roots
            .iter()
            .map(|r| crate::fs::traits_of(r))
            .reduce(FsTraits::and)
            .unwrap_or(FsTraits::UNKNOWN);
        // The slowest root decides, for the same reason `FsTraits` takes the
        // narrower promise: one spinning disk in the set makes twenty threads
        // the wrong answer for all of them.
        let medium = roots
            .iter()
            .map(|r| crate::fs::medium_of(r))
            .reduce(|a, b| if a.threads(64) <= b.threads(64) { a } else { b })
            .unwrap_or(crate::fs::Medium::Unknown);
        Self {
            id,
            name: name.into(),
            roots,
            kind: SourceKind::Local,
            watch: true,
            traits,
            medium,
        }
    }

    /// Whether this source should be watched for changes.
    ///
    /// Expressed by *withholding the capability* rather than by a flag the
    /// engine has to remember to check. The configuration has had a `watch`
    /// field since it was written and nothing read it — every source was
    /// watched regardless — because the engine asks `caps()` and `caps()` was
    /// a constant. Saying it here is the same sentence the rest of the design
    /// already speaks: a source declares what it can do, and the engine adapts.
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

    /// Whether this source's filesystems offer an identity that survives a
    /// remount. The watcher needs it for the same reason the scan does.
    pub fn stable_ids(&self) -> bool {
        self.traits.stable_ids
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

    fn caps(&self) -> Caps {
        let mut c = Caps::CONTENT;
        if self.watch {
            c |= Caps::WATCH;
        }
        // One watch covers a subtree on Windows and macOS. On Linux inotify
        // needs one per directory, and a home directory exhausts the per-user
        // limit — which is why this is declared rather than assumed.
        if self.watch && cfg!(any(windows, target_os = "macos")) {
            c |= Caps::RECURSIVE_WATCH;
        }
        // Measured, not assumed. This used to be `cfg!(unix)`, which told a
        // caller that an exFAT stick had stable identities and that an NTFS
        // volume was case-sensitive — the second of which is true for Linux's
        // ntfs3 and false for the same disk under Windows.
        if self.traits.stable_ids {
            c |= Caps::STABLE_IDS;
        }
        if self.traits.case_sensitive {
            c |= Caps::CASE_SENSITIVE;
        }
        c
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

        // Can every root be read at all?
        //
        // The walker reports an unreadable root as one error among many and
        // then finishes normally, so a scan of a root that is not there and a
        // scan of a root that is genuinely empty both come back `entries: 0`.
        // The engine reconciles on that, and reconciling the second is right
        // while reconciling the first deletes the whole index for that source.
        // Unplug a drive, let a share drop, boot before an encrypted home is
        // mounted — measured: five entries became zero.
        let root_unreadable = roots.iter().any(|r| std::fs::read_dir(r).is_err());

        // `ignore`'s parallel walker, with every one of its opinions turned
        // off. It is used here purely as a fast concurrent directory walk: a
        // file index must not skip what `.gitignore` says to skip, because the
        // whole point is finding the file you cannot find.
        let mut builder = WalkBuilder::new(first);
        for r in rest {
            builder.add(r);
        }
        builder
            .standard_filters(false)
            .hidden(!opts.hidden)
            .follow_links(opts.follow_symlinks)
            .same_file_system(false)
            // Zero means "decide for me", and the decision belongs to the
            // device rather than to the core count. `ignore`'s own default is
            // the core count, which is right on NVMe and wrong on a spinning
            // disk, where every extra thread is another seek.
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
        let stable_ids = self.traits.stable_ids;

        // The walker runs on its own threads and the sink is drained on this
        // one, over a bounded channel.
        //
        // The alternative — requiring `EntrySink: Send + Sync` and locking it —
        // would push a lock into every implementation of the trait, including
        // the ones that are single-threaded by nature. A channel keeps the
        // sink's world simple and gives backpressure for free: when the
        // consumer is slower than the disk, the walker waits instead of
        // building an unbounded queue of a million entries in memory.
        let (tx, rx) = crossbeam_channel::bounded::<Msg>(8192);

        std::thread::scope(|scope| {
            let walker_tx = tx.clone();
            let (rules, excluded, unreadable, entries, dirs, cancelled) =
                (&rules, &excluded, &unreadable, &entries, &dirs, &cancelled);
            let walker = scope.spawn(move || {
                builder.build_parallel().run(|| {
                    let tx = walker_tx.clone();
                    Box::new(move |result| {
                        if cancelled.load(Ordering::Relaxed) {
                            return WalkState::Quit;
                        }
                        let de = match result {
                            Ok(de) => de,
                            Err(e) => {
                                // A directory that could not be read is
                                // reported, not swallowed: "the index is
                                // missing things" should be visible rather than
                                // mysterious.
                                unreadable.fetch_add(1, Ordering::Relaxed);
                                let _ = tx.send(Msg::Unreadable(e.to_string()));
                                return WalkState::Continue;
                            }
                        };
                        let is_dir = de.file_type().is_some_and(|t| t.is_dir());
                        let normalised = path::from_path(de.path());
                        let name = de.file_name().to_string_lossy();

                        if !rules.is_empty() && rules.excludes(&normalised, &name, is_dir) {
                            excluded.fetch_add(1, Ordering::Relaxed);
                            // Prune — unless an allowed subtree lives below,
                            // because an allow rule the walk never reaches is
                            // not a rule.
                            return if is_dir && !rules.may_contain_allowed(&normalised) {
                                WalkState::Skip
                            } else {
                                WalkState::Continue
                            };
                        }

                        // Skipping the per-entry `stat` is the difference
                        // between a usable index in a minute and one in ten: a
                        // directory read already knows the name and the type,
                        // and everything else costs one more syscall per file.
                        // What it costs is that sizes and dates arrive later,
                        // in a second pass.
                        let md = if want_meta { de.metadata().ok() } else { None };
                        entries.fetch_add(1, Ordering::Relaxed);
                        if is_dir {
                            dirs.fetch_add(1, Ordering::Relaxed);
                        }
                        if tx
                            .send(Msg::Entry(entry_of(
                                src_id,
                                &normalised,
                                md.as_ref(),
                                is_dir,
                                stable_ids,
                            )))
                            .is_err()
                        {
                            return WalkState::Quit;
                        }
                        WalkState::Continue
                    })
                });
            });
            // The walker holds clones of the sender; this one has to go, or the
            // loop below never ends.
            drop(tx);

            let mut stopping = false;
            for msg in rx {
                if stopping {
                    // Keep draining after a stop so the walker's threads are
                    // never left blocked on a full channel.
                    continue;
                }
                match msg {
                    Msg::Entry(e) => {
                        if sink.push(e).is_stop() {
                            cancelled.store(true, Ordering::Relaxed);
                            stopping = true;
                        }
                    }
                    Msg::Unreadable(detail) => sink.unreadable("", &Error::Io { detail }),
                }
            }
            let _ = walker.join();
        });

        Ok(ScanReport {
            entries: entries.load(Ordering::Relaxed),
            dirs: dirs.load(Ordering::Relaxed),
            excluded: excluded.load(Ordering::Relaxed),
            unreadable: unreadable.load(Ordering::Relaxed),
            took_ms: started.elapsed().as_millis() as u64,
            cancelled: cancelled.load(Ordering::Relaxed),
            root_unreadable,
        })
    }

    fn watch(&self, sink: Box<dyn ChangeSink>) -> Result<Box<dyn WatchHandle>> {
        crate::watch::start(self.clone(), sink)
    }

    fn open(&self, _id: &EntryId) -> Result<Box<dyn Read + Send>> {
        // An id is not a path. Content extraction goes through `stat` to
        // resolve a path first; when a durable path->id map exists this can
        // answer directly.
        Err(Error::unsupported("opening by entry id"))
    }

    fn stat(&self, p: &str) -> Result<Entry> {
        let native = path::to_path(p);
        let md = std::fs::symlink_metadata(&native).map_err(|e| Error::io(&e, p))?;
        Ok(entry_of(
            self.id,
            &path::from_path(&native),
            Some(&md),
            md.is_dir(),
            self.traits.stable_ids,
        ))
    }
}

/// Build an entry from a normalised path and, when it was worth the syscall,
/// its metadata.
///
/// An inode survives a rename, which is what lets a moved file be *updated*
/// rather than deleted and re-added. Windows has no cheap equivalent — its file
/// id requires opening the file — so there the path is the identity and a
/// rename genuinely is two operations. Without metadata there is no inode
/// either, so a stat-less scan falls back to the same thing.
pub(crate) fn entry_of(
    source: SourceId,
    path: &str,
    md: Option<&std::fs::Metadata>,
    is_dir: bool,
    stable_ids: bool,
) -> Entry {
    // On Windows the identity is always the path: the file id there needs the
    // file to be opened, which is the syscall a bulk scan exists to avoid.
    #[cfg(not(unix))]
    let _ = stable_ids;
    let id = match md {
        // `stable_ids` is the filesystem's answer, not the platform's. On FAT
        // and exFAT `st_ino` is invented by the driver and can change across a
        // remount; an identity built from it makes a rescan decide every file
        // is new, which doubles the index and then sweeps the originals away.
        #[cfg(unix)]
        Some(m) if stable_ids => {
            use std::os::unix::fs::MetadataExt;
            EntryId::inode(source, m.dev(), m.ino())
        }
        _ => EntryId::path_hash(source, path),
    };
    Entry {
        id,
        is_dir,
        meta: md
            .map(|m| Meta::from_std(m, is_dir))
            .unwrap_or(Meta::UNKNOWN),
        path: path.to_owned(),
    }
}
