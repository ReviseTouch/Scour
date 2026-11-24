//! Many files at once: a lane a disk, each lane as wide as its disk bears.

use std::any::Any;
use std::io::ErrorKind;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::budget::Budget;
use crate::class::Class;
use crate::excerpt::Hit;
use crate::ground::{Ground, readers};
use crate::order::{Order, arrange};
use crate::pattern::Pattern;
use crate::read::Reader;

/// A file to read. What the caller knows saves opening it: a size says it
/// is empty or over the limit, a class that it is a picture or a video.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct File {
    pub path: PathBuf,
    pub size: Option<u64>,
    /// Only [`Class::HeaderOnly`] is acted on before the file is opened.
    pub class: Option<Class>,
}

impl File {
    pub fn at(path: impl Into<PathBuf>) -> File {
        File {
            path: path.into(),
            size: None,
            class: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Options {
    /// Read what is not text as well, and look for the text as UTF-16 in it.
    pub binary: bool,
    /// Files larger than this are not opened. Zero is no limit.
    pub max_size: u64,
    /// Finds kept from one file. Zero only says whether there is one.
    pub hits_per_file: usize,
    /// About how many bytes of a line are shown around a find.
    pub excerpt: usize,
    /// Read at the lowest disk and CPU priority: a reading nobody waits on.
    pub quiet: bool,
}

impl Default for Options {
    fn default() -> Options {
        Options {
            binary: false,
            max_size: 64 << 20,
            hits_per_file: 3,
            excerpt: 160,
            quiet: false,
        }
    }
}

/// What became of one file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Read to its end; the needle is not in it.
    Clean,
    /// `more`: there are finds past the ones kept, and reading stopped there.
    Found {
        hits: Vec<Hit>,
        more: bool,
    },
    Skipped(Skip),
    Unreadable(ErrorKind),
    /// The budget ran out first. Nothing is known about this file.
    NotReached,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Skip {
    TooBig,
    /// Not text, and binary files were not asked for.
    Binary,
    /// A pipe, a socket, a device, a directory.
    NotAFile,
    /// A picture, a sound, a video or a disk image, and not a run of bytes asked for.
    HeaderOnly,
    /// A document no reader here opens yet (PDF, `.doc`, `.xls`, `.ppt`), and
    /// binary files were not asked for.
    NoReader,
    /// Held open by another program that shares it with nobody (Windows).
    Locked,
    /// Its data is in the cloud and reading it would fetch it: a OneDrive or
    /// iCloud placeholder.
    Offline,
}

/// Called by each reading thread before its first file; what it returns is
/// held until the thread ends. A service reads as the person asking this way.
/// `Err`: that thread reads nothing, and says permission was denied.
pub type Enter = Arc<dyn Fn() -> Result<Option<Box<dyn Any + Send>>, ()> + Send + Sync>;

/// A pattern, and how to read for it.
#[derive(Clone)]
pub struct Scanner {
    pattern: Pattern,
    options: Options,
    ground: Ground,
    enter: Option<Enter>,
    cores: usize,
}

impl std::fmt::Debug for Scanner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Scanner")
            .field("options", &self.options)
            .field("ground", &self.ground)
            .finish_non_exhaustive()
    }
}

impl Scanner {
    pub fn new(pattern: Pattern, options: Options, ground: Ground) -> Scanner {
        Scanner {
            pattern,
            options,
            ground,
            enter: None,
            cores: std::thread::available_parallelism().map_or(2, |n| n.get()),
        }
    }

    /// See [`Enter`].
    pub fn entering(mut self, enter: Option<Enter>) -> Scanner {
        self.enter = enter;
        self
    }

    /// One outcome a file, in the order given. Returns when every file is
    /// read or the budget is out; disks are read side by side.
    pub fn scan(&self, files: &[File], budget: &Budget) -> Vec<Outcome> {
        let mut lanes: Vec<Vec<usize>> = vec![Vec::new(); self.ground.lanes()];
        for (i, file) in files.iter().enumerate() {
            lanes[self.ground.lane_of(&file.path)].push(i);
        }
        let mut outcomes = vec![Outcome::NotReached; files.len()];
        std::thread::scope(|scope| {
            let mut workers = Vec::new();
            for (lane, list) in lanes.iter_mut().enumerate() {
                let medium = self.ground.medium(lane);
                arrange(Order::of(medium, files, list), files, list);
                let (list, next) = (&*list, Arc::new(AtomicUsize::new(0)));
                for _ in 0..readers(medium, self.cores).min(list.len()) {
                    let next = Arc::clone(&next);
                    workers.push(scope.spawn(move || self.work(files, list, &next, budget)));
                }
            }
            for worker in workers {
                for (i, outcome) in worker.join().unwrap_or_default() {
                    outcomes[i] = outcome;
                }
            }
        });
        outcomes
    }

    fn work(
        &self,
        files: &[File],
        list: &[usize],
        next: &AtomicUsize,
        budget: &Budget,
    ) -> Vec<(usize, Outcome)> {
        if self.options.quiet {
            crate::quiet::give_way();
        }
        let identity = self.enter.as_ref().map(|enter| enter());
        let mut reader = Reader::default();
        let mut done = Vec::new();
        while let Some(&i) = list.get(next.fetch_add(1, Ordering::Relaxed)) {
            let outcome = if matches!(identity, Some(Err(()))) {
                Outcome::Unreadable(ErrorKind::PermissionDenied)
            } else if budget.exhausted() {
                Outcome::NotReached
            } else {
                self.one(&mut reader, &files[i], budget)
            };
            done.push((i, outcome));
        }
        done
    }

    fn one(&self, reader: &mut Reader, file: &File, budget: &Budget) -> Outcome {
        match file.size {
            Some(0) => return Outcome::Clean,
            Some(size) if self.options.max_size != 0 && size > self.options.max_size => {
                return Outcome::Skipped(Skip::TooBig);
            }
            _ => {}
        }
        if file.class == Some(Class::HeaderOnly) && !self.pattern.is_raw_bytes() {
            return Outcome::Skipped(Skip::HeaderOnly);
        }
        let (outcome, read) = reader.search(&file.path, &self.pattern, &self.options);
        budget.charge(read);
        outcome
    }
}
