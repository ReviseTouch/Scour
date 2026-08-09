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
///
/// The number is not delicate — anything that turns a send per entry into a
/// send per hundred does almost all of the work — but it is bounded on both
/// sides. Too small and the channel is busy again; too large and the walk
/// stutters, because a batch is invisible to the index until it is sent.
const BATCH: usize = 512;

/// How many batches may be in the air. Bound times batch is what the channel
/// holds, and it is deliberately about what one message used to hold.
const IN_FLIGHT: usize = 64;

/// One walker thread's outgoing buffer.
///
/// **The channel was the scan.** Twenty threads sending one entry each into a
/// bounded queue drained by one is a queue that is always full, so nearly every
/// send parks the thread and nearly every receive wakes one: measured at
/// 8,231,481 voluntary context switches for 870,000 entries — 9.5 a file — and
/// 84 of the 112 core-seconds a start-up cost were the kernel doing that. The
/// same walk on two threads cost 44,238 switches and 7.9 core-seconds.
///
/// The fix is not fewer threads, which only hides it; it is fewer messages.
/// Backpressure is unchanged — [`IN_FLIGHT`] batches is about as many entries
/// as the old bound — and so is what the walk is allowed to get ahead by.
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
///
/// `ignore` gives a visitor no way to say it has finished, but it does drop the
/// box when the thread ends — so this is where the remainder goes. Without it a
/// walk loses up to [`BATCH`] entries a thread, which on a quiet disk is the
/// whole scan.
impl Drop for Batch {
    fn drop(&mut self) {
        self.flush();
    }
}

/// How many unreadable subtrees are worth remembering by name.
///
/// Past this the root is not vouched for at all: a walk that could not look
/// into thousands of places has not proved anything about what is missing.
const MAX_BLIND: usize = 4_096;

/// Where the walker's error happened.
///
/// The path is wrapped: an unreadable directory arrives as `WithDepth` around
/// `WithPath` around the `io::Error`, so matching the outer variant finds
/// nothing — which is exactly what the first version of this did, and the
/// blind list stayed empty while the count went up.
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
///
/// The number itself means nothing outside this machine and this boot. What
/// matters is that it is the *same* number at the end of a walk as at the
/// start — see `ScanReport::vouched`.
fn device_of(p: &std::path::Path) -> Option<u64> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        std::fs::metadata(p).ok().map(|m| m.dev())
    }
    #[cfg(not(unix))]
    {
        // Windows has volume serial numbers, through
        // `GetFileInformationByHandle` — an open per root, which is a thing to
        // add when this is wired for that platform.
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
    /// What the filesystems under the roots actually promise, asked once at
    /// construction rather than assumed at compile time.
    traits: FsTraits,
    /// What they are like to read: how many threads are worth using, and how
    /// long to let events settle.
    medium: crate::fs::Medium,
    /// How to ask "has anything happened here" without walking anything, one
    /// per root. Decided at construction because the answer depends on the
    /// filesystem and the device, neither of which changes under a mount.
    probes: Vec<crate::pulse::Probe>,
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

    /// The path, resolved, and refused if it is not really under a root.
    ///
    /// **A prefix comparison is not a containment check**, and this is the one
    /// place it was being used as one. `stat` is what stands between a caller
    /// and the filesystem — the web bridge documents it as the fence around
    /// `/api/open`, and the MCP server offers it to a model — while the check
    /// in front of it only asked whether the *string* started with a root.
    /// The kernel does not read strings: `~/../../etc/shadow` starts with the
    /// home directory and resolves to `/etc/shadow`, and so does any path
    /// through a symlink that points out of the tree. Both were confirmed
    /// against the running service.
    ///
    /// So the parent is resolved — which is what follows the symlinks — and
    /// only then compared against the resolved roots. The **last** component is
    /// joined back on unresolved, deliberately: a symlink is a row of its own
    /// and `stat` of it must describe the link, not what it points at.
    ///
    /// This closes the escape, not the race: between the check and the open,
    /// a component can be replaced. Closing that needs `openat2` with
    /// `RESOLVE_BENEATH` on Linux and its equivalents elsewhere, which is worth
    /// doing when this is asked on someone else's behalf across a boundary.
    fn inside(&self, p: &str) -> Result<PathBuf> {
        let native = path::to_path(p);
        let missing = || Error::NotFound { path: p.to_owned() };
        // Lexically first, so the refusal is cheap and says what it means. A
        // `.` or `..` in an indexed path is not a thing the index produces.
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

    /// Whether `st_mode` is the file's own rather than the mount's.
    pub fn real_modes(&self) -> bool {
        self.traits.real_modes
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
    ///
    /// Folded rather than reported separately because the caller's question is
    /// "is it worth looking at this source", and any root moving is a yes.
    /// `None` only when *nothing* under it can be asked cheaply — one root
    /// that can answer is enough to be useful.
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
        // **Not `CONTENT`.** This claimed it unconditionally while `open`
        // refused unconditionally, which made the one capability bit a caller
        // could act on a bit that lied — and `scour sources` printed it, so the
        // lie was on screen. The flag comes back with the first `Extractor`,
        // which is what it is for; until then the honest answer is that this
        // source hands out metadata and nothing else.
        let mut c = Caps::empty();
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
        // caller that an NTFS volume was case-insensitive — true for the disk
        // under Windows and false for the same disk under Linux's ntfs3.
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

        // Can every root be read, and is there anything in it?
        //
        // The walker reports an unreadable root as one error among many and
        // then finishes normally, so a scan of a root that is not there and a
        // scan of a root that is genuinely empty both come back `entries: 0`.
        // The engine reconciles on that, and reconciling the second is right
        // while reconciling the first deletes the whole index for that source.
        // Unplug a drive, let a share drop, boot before an encrypted home is
        // mounted — measured: five entries became zero.
        //
        // **A mount that is not mounted is a readable, empty directory.**
        // `/mnt/depo` unmounted opens fine and lists nothing, and a machine
        // that has just booted is precisely where a scan-on-start meets a
        // volume that is not up yet. So an empty root is not evidence that its
        // contents are gone.
        //
        // Only for a **whole source**, never for a subtree. Emptying a folder
        // is an ordinary thing a person does and the walk of it has to be
        // reconciled, or the folder's contents never leave the index — and an
        // explicit `scour rescan <path>` is a subtree walk, which is how a
        // source root that really was emptied is reconciled on purpose.
        //
        // Per root, not per source: one absent removable disk used to stop a
        // home directory being reconciled at all.
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

        // `ignore`'s parallel walker, with every one of its opinions turned
        // off. It is used here purely as a fast concurrent directory walk: a
        // file index must not skip what `.gitignore` says to skip, because the
        // whole point is finding the file you cannot find.
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
        let real_modes = self.traits.real_modes;

        // The walker runs on its own threads and the sink is drained on this
        // one, over a bounded channel.
        //
        // The alternative — requiring `EntrySink: Send + Sync` and locking it —
        // would push a lock into every implementation of the trait, including
        // the ones that are single-threaded by nature. A channel keeps the
        // sink's world simple and gives backpressure for free: when the
        // consumer is slower than the disk, the walker waits instead of
        // building an unbounded queue of a million entries in memory.
        let (tx, rx) = crossbeam_channel::bounded::<Msg>(IN_FLIGHT);

        std::thread::scope(|scope| {
            let walker_tx = tx.clone();
            let (rules, excluded, unreadable, entries, dirs, cancelled) =
                (&rules, &excluded, &unreadable, &entries, &dirs, &cancelled);
            let walker = scope.spawn(move || {
                builder.build_parallel().run(|| {
                    let tx = walker_tx.clone();
                    let mut batch = Batch::new(tx.clone());
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
                                // The path, not only the count. A directory
                                // that became unreadable after it was indexed
                                // still holds its files, and the sweep has to
                                // be told to spare it — see `ScanReport::blind`.
                                let where_ = where_of(&e).map(path::from_path).unwrap_or_default();
                                let _ = tx.send(Msg::Unreadable(where_, e.to_string()));
                                return WalkState::Continue;
                            }
                        };
                        let is_dir = de.file_type().is_some_and(|t| t.is_dir());
                        let normalised = path::from_path(de.path());
                        // The same encoding as the path it came from, so a
                        // rule comparing them compares like with like.
                        let name = path::from_path(std::path::Path::new(de.file_name()));

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
                        // Bounded: a tree nobody may read produces one of these
                        // per directory, and a list of them is not worth more
                        // memory than the index it protects. Past the ceiling
                        // the root stops being vouched for at all, which is the
                        // safe direction.
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

        // **Asked again, now that the walk is over.** The check before it was
        // one `read_dir`; everything after that was taken on trust, so a volume
        // that went away mid-walk — an unmount, a pulled disk, a share that
        // dropped — still produced a report the engine reconciled against, and
        // reconciling against a filesystem that is not there deletes all of it.
        // A device number that is not the one the walk started on means the
        // walk was about something else.
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

    fn open(&self, _id: &EntryId) -> Result<Box<dyn Read + Send>> {
        // An id is not a path. Content extraction goes through `stat` to
        // resolve a path first; when a durable path->id map exists this can
        // answer directly.
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
    real_modes: bool,
) -> Entry {
    // **The path, always.** The inode was the identity here wherever the
    // filesystem promised a stable one, and that promise is about the *object*
    // while a row is a *name*. Everything that saves carefully — an editor, a
    // browser — writes a temporary file and renames it over the target, so the
    // path survives and the inode does not: the row for the new inode was
    // added and the row for the old one was never removed, because nothing can
    // say "the inode that used to be at this path is gone". Measured on the
    // live index: 267 rows at one path, one per save.
    //
    // The FAT-family measurement that used to be quoted here — `st_ino`
    // invented by the driver, 0 of 50 surviving a remount, from
    // `scripts/fsmatrix.sh` — still stands; it is simply no longer load
    // bearing, because nothing asks the filesystem for an identity any more.
    let id = EntryId::path_hash(source, path);
    let mut meta = md
        .map(|m| Meta::from_std(m, is_dir))
        .unwrap_or(Meta::UNKNOWN);
    // **A mode the mount invented is not a mode.** NTFS and the FAT family
    // have no permissions of their own; what `stat` returns there is `fmask`
    // and `dmask` off the mount line, the same value for every file. Storing
    // it makes `kind_of` read an executable bit that says nothing: on this
    // machine `/mnt/depo` is mounted 0022, so 293,811 of its files were
    // classified `exec` against 21,342 on the real filesystem beside it, and
    // `kind:exec` was useless for finding a program.
    //
    // Replaced rather than zeroed, so `mode_string` still prints something a
    // person recognises — and what it prints is true: no permissions of its
    // own, readable, and executable only if it is a directory.
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
