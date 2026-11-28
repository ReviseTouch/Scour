//! Many files at once: a lane a disk, each lane as wide as its disk bears.

use std::any::Any;
use std::io::ErrorKind;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use surefoot::Medium;

use crate::budget::Budget;
use crate::class::Class;
use crate::excerpt::Hit;
use crate::ground::{Ground, forced};
use crate::order::{Order, arrange};
use crate::pattern::Pattern;
use crate::raw;
use crate::read::Reader;
use crate::settle::{FLASH_READERS, spins};
use crate::texts::{Doc, DocText, Texts, extension_of, handed_over, unopened};
use crate::zip::search_whole;

/// Below this, a disk that says it spins is taken at its word: a few
/// files are read in moments either way.
const SETTLE_FROM: usize = 64;

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
    /// Search a ZIP-based document's XML as it is, tags and attributes too,
    /// rather than the text it shows: for markup, not for words.
    pub markup: bool,
}

impl Default for Options {
    fn default() -> Options {
        Options {
            binary: false,
            max_size: 64 << 20,
            hits_per_file: 3,
            excerpt: 160,
            quiet: false,
            markup: false,
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
    /// Its data is elsewhere and reading it would download it: a OneDrive or
    /// iCloud placeholder, or any file on a FUSE mount that fetches what it
    /// serves (rclone, sshfs, a cloud drive).
    Offline,
    /// A document that opened and holds no text: a scan without a text layer.
    NoText,
    /// A document its reader could not open: damaged, encrypted, or not what
    /// its name says.
    Broken,
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
    texts: Option<Arc<dyn Texts>>,
    cores: usize,
    /// Files whose raw reading stood, over every scan of this scanner.
    raw: Arc<AtomicUsize>,
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
            texts: None,
            cores: std::thread::available_parallelism().map_or(2, |n| n.get()),
            raw: Arc::default(),
        }
    }

    /// See [`Enter`].
    pub fn entering(mut self, enter: Option<Enter>) -> Scanner {
        self.enter = enter;
        self
    }

    /// Who reads the documents this crate cannot: see [`Texts`].
    pub fn reading_documents(mut self, texts: Option<Arc<dyn Texts>>) -> Scanner {
        self.texts = texts;
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
            let workers: Vec<_> = lanes
                .into_iter()
                .enumerate()
                .filter(|(_, list)| !list.is_empty())
                .map(|(lane, list)| scope.spawn(move || self.lane(lane, files, list, budget)))
                .collect();
            for worker in workers {
                for (i, outcome) in worker.join().unwrap_or_default() {
                    outcomes[i] = outcome;
                }
            }
        });
        outcomes
    }

    /// Files read raw so far: those whose raw answer stood.
    pub fn read_raw(&self) -> usize {
        self.raw.load(Ordering::Relaxed)
    }

    /// One disk's files: raw from an NTFS volume when that pays, else as
    /// many readers as the disk bears.
    fn lane(
        &self,
        lane: usize,
        files: &[File],
        mut list: Vec<usize>,
        budget: &Budget,
    ) -> Vec<(usize, Outcome)> {
        if self.ground.remote(lane) && std::env::var_os("SCOUR_GREP_REMOTE").is_none() {
            return list
                .into_iter()
                .map(|i| (i, Outcome::Skipped(Skip::Offline)))
                .collect();
        }
        if self.raw_pays(lane, &list)
            && let Some(done) = self.raw_lane(files, &list, budget)
        {
            return done;
        }
        let (medium, readers) = self.settled(lane, files, &list);
        arrange(Order::of(medium, files, &list), files, &mut list);
        let units = self.units(files, list);
        let next = AtomicUsize::new(0);
        let readers = readers.min(units.len());
        if readers <= 1 {
            return self.work(files, &units, &next, budget);
        }
        std::thread::scope(|scope| {
            let (units, next) = (&units, &next);
            let workers: Vec<_> = (0..readers)
                .map(|_| scope.spawn(move || self.work(files, units, next, budget)))
                .collect();
            workers
                .into_iter()
                .flat_map(|w| w.join().unwrap_or_default())
                .collect()
        })
    }

    fn raw_pays(&self, lane: usize, list: &[usize]) -> bool {
        if !cfg!(windows) {
            return false;
        }
        match raw::wanted() {
            raw::Want::Never => false,
            raw::Want::Always => true,
            raw::Want::Spinning => {
                self.ground.medium(lane) == Medium::Spinning && list.len() >= raw::RAW_FROM
            }
        }
    }

    /// The files of one volume read raw, every one the raw read could not
    /// or should not answer — and every one found to hold the pattern —
    /// read the normal way after. None: nothing was read, the lane goes the
    /// normal way.
    fn raw_lane(
        &self,
        files: &[File],
        list: &[usize],
        budget: &Budget,
    ) -> Option<Vec<(usize, Outcome)>> {
        if self.options.quiet {
            crate::quiet::give_way();
        }
        let letter = raw::letter_of(&files[*list.first()?].path)?;
        let (mut done, mut rest, mut asked) = (Vec::new(), Vec::new(), Vec::new());
        for &i in list {
            let file = &files[i];
            if let Some(outcome) = self.unopened(file) {
                done.push((i, outcome));
            } else if raw::letter_of(&file.path) != Some(letter) || self.handed_over(file) {
                rest.push(i);
            } else {
                asked.push((i, file.path.as_path()));
            }
        }
        let mut plan = raw::Plan::prepare(letter, &asked, self.options.max_size)?;
        let identity = self.enter.as_ref().map(|enter| enter());
        if matches!(identity, Some(Err(()))) || !plan.allow() {
            return None;
        }
        let mut reader = Reader::default();
        let mut answered: Vec<(usize, Outcome)> = Vec::new();
        plan.read(&mut |i, bytes| {
            if budget.exhausted() {
                return false;
            }
            budget.charge(bytes.len() as u64);
            match reader.search_bytes(&files[i].path, bytes, &self.pattern, &self.options) {
                // Found: the normal way says what, of the file as it is now.
                Some((Outcome::Found { .. }, _)) | None => rest.push(i),
                Some((outcome, _)) => answered.push((i, outcome)),
            }
            true
        });
        let settled = plan.settle();
        let stood: std::collections::HashSet<usize> = settled.read.into_iter().collect();
        for (i, outcome) in answered {
            if stood.contains(&i) {
                self.raw.fetch_add(1, Ordering::Relaxed);
                done.push((i, outcome));
            } else {
                rest.push(i);
            }
        }
        rest.extend(settled.left);
        rest.sort_by(|&a, &b| files[a].path.cmp(&files[b].path));
        rest.dedup();
        let (began, left) = (std::time::Instant::now(), rest.len());
        for i in rest {
            let outcome = if budget.exhausted() {
                Outcome::NotReached
            } else {
                self.one(&mut reader, &files[i], budget)
            };
            done.push((i, outcome));
        }
        if std::env::var_os("SCOUR_GREP_RAW_LOG").is_some() {
            eprintln!(
                "raw: {left} read the normal way after, {:.3} s",
                began.elapsed().as_secs_f64()
            );
        }
        drop(identity);
        Some(done)
    }

    /// Said of a file without opening it: empty, over the limit, a picture.
    fn unopened(&self, file: &File) -> Option<Outcome> {
        match file.size {
            Some(0) => return Some(Outcome::Clean),
            Some(size) if self.options.max_size != 0 && size > self.options.max_size => {
                return Some(Outcome::Skipped(Skip::TooBig));
            }
            _ => {}
        }
        (file.class == Some(Class::HeaderOnly) && !self.pattern.is_raw_bytes())
            .then_some(Outcome::Skipped(Skip::HeaderOnly))
    }

    /// A document whose text another reader gives: read by its path.
    fn handed_over(&self, file: &File) -> bool {
        self.texts.is_some()
            && !self.pattern.is_raw_bytes()
            && handed_over(&extension_of(&file.path))
    }

    /// A lane's medium and readers, a disk that says it spins asked again
    /// when there is enough to read for it to matter: flash behind a USB
    /// bridge reads as solid, two at a time, in the order given.
    fn settled(&self, lane: usize, files: &[File], list: &[usize]) -> (Medium, usize) {
        let medium = self.ground.medium(lane);
        let readers = self.ground.readers(lane, self.cores);
        if medium != Medium::Spinning || list.len() < SETTLE_FROM {
            return (medium, readers);
        }
        let Some(root) = self.ground.root_of(lane) else {
            return (medium, readers);
        };
        if spins(root, list.iter().map(|&i| files[i].path.as_path())) {
            return (medium, readers);
        }
        (Medium::Solid, forced().unwrap_or(FLASH_READERS))
    }

    /// A lane's files as its readers take them. A kept text's place is set
    /// by when its document was first read, so in another order — another
    /// sort, another search's files — each text costs its group again: by
    /// path, 23,280 kept texts took 3.9 s, against 0.05 s by place (28 Nov).
    fn units(&self, files: &[File], list: Vec<usize>) -> Units {
        let texts = self
            .texts
            .as_deref()
            .filter(|_| !self.pattern.is_raw_bytes());
        let mut kept: Vec<((u32, u32), usize)> = Vec::new();
        let mut rest = Vec::with_capacity(list.len());
        for i in list {
            let path = &files[i].path;
            let place = texts
                .filter(|_| handed_over(&extension_of(path)))
                .and_then(|texts| texts.place(path));
            match place {
                Some(place) => kept.push((place, i)),
                None => rest.push(i),
            }
        }
        kept.sort_unstable();
        let mut units = Units {
            order: Vec::with_capacity(kept.len() + rest.len()),
            ends: Vec::new(),
        };
        for (k, &((group, _), i)) in kept.iter().enumerate() {
            if k > 0 && kept[k - 1].0.0 != group {
                units.ends.push((units.order.len(), true));
            }
            units.order.push(i);
        }
        if !kept.is_empty() {
            units.ends.push((units.order.len(), true));
        }
        for i in rest {
            units.order.push(i);
            units.ends.push((units.order.len(), false));
        }
        units
    }

    fn work(
        &self,
        files: &[File],
        units: &Units,
        next: &AtomicUsize,
        budget: &Budget,
    ) -> Vec<(usize, Outcome)> {
        if self.options.quiet {
            crate::quiet::give_way();
        }
        let identity = self.enter.as_ref().map(|enter| enter());
        let mut reader = Reader::default();
        let mut done = Vec::new();
        while let Some(unit) = units.get(next.fetch_add(1, Ordering::Relaxed)) {
            if matches!(identity, Some(Err(()))) {
                let denied = Outcome::Unreadable(ErrorKind::PermissionDenied);
                done.extend(unit.files.iter().map(|&i| (i, denied.clone())));
            } else if unit.kept {
                self.kept_together(&mut reader, files, unit.files, budget, &mut done);
            } else {
                for &i in unit.files {
                    let outcome = if budget.exhausted() {
                        Outcome::NotReached
                    } else {
                        self.one(&mut reader, &files[i], budget)
                    };
                    done.push((i, outcome));
                }
            }
        }
        done
    }

    /// Documents whose texts are kept in one group, asked for at once; one
    /// whose text no longer answers is read as any other file.
    fn kept_together(
        &self,
        reader: &mut Reader,
        files: &[File],
        group: &[usize],
        budget: &Budget,
        done: &mut Vec<(usize, Outcome)>,
    ) {
        let texts = self.texts.as_deref();
        let (mut asked, mut read) = (Vec::with_capacity(group.len()), Vec::new());
        for &i in group {
            let file = &files[i];
            if budget.exhausted() {
                done.push((i, Outcome::NotReached));
            } else if let Some(outcome) = self.unopened(file) {
                done.push((i, outcome));
            } else if let Some((size, mtime)) = unopened(&file.path) {
                asked.push((i, size, mtime, extension_of(&file.path)));
            } else {
                read.push(i);
            }
        }
        if let Some(texts) = texts {
            let docs: Vec<Doc<'_>> = asked
                .iter()
                .map(|(i, size, mtime, extension)| Doc {
                    path: &files[*i].path,
                    size: *size,
                    mtime: *mtime,
                    extension,
                })
                .collect();
            texts.kept_each(&docs, &mut |k, text| match text {
                Some(text) => {
                    done.push((asked[k].0, outcome_of(text, &self.pattern, &self.options)))
                }
                None => read.push(asked[k].0),
            });
        } else {
            read.extend(asked.iter().map(|a| a.0));
        }
        for i in read {
            let outcome = if budget.exhausted() {
                Outcome::NotReached
            } else {
                let (outcome, bytes) =
                    reader.search(&files[i].path, &self.pattern, &self.options, texts);
                budget.charge(bytes);
                outcome
            };
            done.push((i, outcome));
        }
    }

    /// A document whose text is kept, searched without opening it.
    fn kept(&self, file: &File) -> Option<Outcome> {
        let texts = self.texts.as_deref()?;
        let extension = extension_of(&file.path);
        if self.pattern.is_raw_bytes() || !handed_over(&extension) {
            return None;
        }
        let (size, mtime) = unopened(&file.path)?;
        let doc = Doc {
            path: &file.path,
            size,
            mtime,
            extension: &extension,
        };
        Some(outcome_of(texts.kept(&doc)?, &self.pattern, &self.options))
    }

    fn one(&self, reader: &mut Reader, file: &File, budget: &Budget) -> Outcome {
        if let Some(outcome) = self.unopened(file) {
            return outcome;
        }
        if let Some(kept) = self.kept(file) {
            return kept;
        }
        let texts = self.texts.as_deref();
        let (outcome, read) = reader.search(&file.path, &self.pattern, &self.options, texts);
        budget.charge(read);
        outcome
    }
}

/// What a document's text says of the pattern.
pub(crate) fn outcome_of(text: DocText, pattern: &Pattern, options: &Options) -> Outcome {
    match text {
        DocText::Words(words) => search_whole(&words, pattern, options),
        DocText::NoText => Outcome::Skipped(Skip::NoText),
        DocText::Broken => Outcome::Skipped(Skip::Broken),
    }
}

/// What a lane's readers take, one at a time: the documents whose texts are
/// kept in one group, in the order they lie there; then every other file
/// alone, in the lane's order. Each reader holding a group of its own, the
/// groups are opened side by side and none twice.
struct Units {
    order: Vec<usize>,
    /// Where each ends in `order`, and whether it is a group of kept texts.
    ends: Vec<(usize, bool)>,
}

struct Unit<'a> {
    files: &'a [usize],
    kept: bool,
}

impl Units {
    fn len(&self) -> usize {
        self.ends.len()
    }

    fn get(&self, n: usize) -> Option<Unit<'_>> {
        let &(end, kept) = self.ends.get(n)?;
        let start = n.checked_sub(1).map_or(0, |p| self.ends[p].0);
        Some(Unit {
            files: &self.order[start..end],
            kept,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nothing_on_a_mount_that_fetches_is_opened() {
        let dir = tempfile::tempdir().unwrap();
        let (cloud, local) = (dir.path().join("drive"), dir.path().join("disk"));
        std::fs::create_dir_all(&cloud).unwrap();
        std::fs::create_dir_all(&local).unwrap();
        std::fs::write(cloud.join("a.txt"), "a needle").unwrap();
        std::fs::write(local.join("b.txt"), "a needle").unwrap();
        let mut ground = Ground::of([
            (cloud.clone(), Some("rclone".into()), Medium::Network),
            (local.clone(), Some("nvme".into()), Medium::Solid),
        ]);
        ground.fetching(ground.lane_of(&cloud));
        let pattern = Pattern::new(&crate::Needle::Text("needle".into()), false).unwrap();
        let scanner = Scanner::new(pattern, Options::default(), ground);
        let files = [File::at(cloud.join("a.txt")), File::at(local.join("b.txt"))];
        let got = scanner.scan(&files, &Budget::unlimited());
        assert_eq!(got[0], Outcome::Skipped(Skip::Offline));
        assert!(matches!(got[1], Outcome::Found { .. }), "{:?}", got[1]);
    }

    /// A text kept for every `.pdf`, in four groups by its number; each ask
    /// written down, so the test sees how they were asked for.
    #[derive(Default)]
    struct Grouped {
        asked: std::sync::Mutex<Vec<Vec<u32>>>,
    }

    fn number(path: &std::path::Path) -> u32 {
        path.file_stem().unwrap().to_str().unwrap().parse().unwrap()
    }

    fn kept_text(path: &std::path::Path) -> DocText {
        let n = number(path);
        DocText::Words(if n.is_multiple_of(5) {
            format!("{n} needle")
        } else {
            "hay".into()
        })
    }

    impl Texts for Grouped {
        fn kept(&self, doc: &Doc<'_>) -> Option<DocText> {
            self.asked.lock().unwrap().push(vec![number(doc.path)]);
            Some(kept_text(doc.path))
        }
        fn read(&self, _: &Doc<'_>, _: &[u8]) -> DocText {
            DocText::Broken
        }
        fn place(&self, path: &std::path::Path) -> Option<(u32, u32)> {
            let n = number(path);
            Some((n % 4, n))
        }
        fn kept_each(&self, docs: &[Doc<'_>], each: &mut dyn FnMut(usize, Option<DocText>)) {
            let run = docs.iter().map(|d| number(d.path)).collect();
            self.asked.lock().unwrap().push(run);
            for (i, doc) in docs.iter().enumerate() {
                each(i, Some(kept_text(doc.path)));
            }
        }
    }

    #[test]
    fn kept_texts_are_asked_for_a_group_at_a_time_in_their_place() {
        let dir = tempfile::tempdir().unwrap();
        let files: Vec<File> = (0..40)
            .rev()
            .map(|n| {
                let path = dir.path().join(format!("{n:02}.pdf"));
                std::fs::write(&path, b"%PDF").unwrap();
                File::at(path)
            })
            .collect();
        let ground = Ground::of([(dir.path().to_path_buf(), Some("nvme".into()), Medium::Solid)]);
        let pattern = Pattern::new(&crate::Needle::Text("needle".into()), false).unwrap();
        let texts = Arc::new(Grouped::default());
        let scanner = Scanner::new(pattern, Options::default(), ground)
            .reading_documents(Some(texts.clone() as Arc<dyn Texts>));
        let got = scanner.scan(&files, &Budget::unlimited());
        for (file, outcome) in files.iter().zip(&got) {
            let found = matches!(outcome, Outcome::Found { .. });
            assert_eq!(
                found,
                number(&file.path).is_multiple_of(5),
                "{file:?}: {outcome:?}"
            );
        }
        let mut asked = texts.asked.lock().unwrap().clone();
        asked.sort();
        let group = |g: u32| (0..40).filter(|n| n % 4 == g).collect::<Vec<u32>>();
        assert_eq!(asked, [group(0), group(1), group(2), group(3)]);
    }
}
