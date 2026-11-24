//! Grouping by size, then head and tail, then every byte, largest first.

use super::*;
use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

/// How much of each end is read in stage two.
pub(super) const EDGE: u64 = 4 * 1024;

/// How much is moved per read while comparing.
pub(super) const CHUNK: usize = 64 * 1024;

/// Find them. Only the size is used before anything is read, and a file that has
/// since disappeared is dropped rather than failing the run.
pub fn find<I>(files: I, opts: &Options) -> Report
where
    I: IntoIterator<Item = (String, u64)>,
{
    find_named(files.into_iter().map(|(p, size)| (p, size, 1)), opts)
}

/// The same, told how many names each file has (`st_nlink`). A file with more
/// than one may be a hard link of another candidate, and only those are looked
/// at before the budget allows anything: a size-only answer stays free.
pub fn find_named<I>(files: I, opts: &Options) -> Report
where
    I: IntoIterator<Item = (String, u64, u64)>,
{
    // Stage one: size to paths. No `stat`, no `open` — the caller had the sizes.
    let mut by_size: HashMap<u64, Vec<(String, u64)>> = HashMap::new();
    let mut candidates = 0u64;
    for (path, size, links) in files {
        if size < opts.min_size {
            continue;
        }
        candidates += 1;
        by_size.entry(size).or_default().push((path, links));
    }

    let mut pending: Vec<(u64, Vec<String>)> = by_size
        .into_iter()
        .filter(|(_, paths)| paths.len() > 1)
        .map(|(size, mut named)| {
            // Sorted first, so the name that stands for a linked file is the
            // same one stage two keeps.
            named.sort();
            let mut files = std::collections::HashSet::new();
            let paths = named
                .into_iter()
                .filter(|(p, links)| {
                    *links < 2 || identity(Path::new(p)).is_none_or(|id| files.insert(id))
                })
                .map(|(p, _)| p)
                .collect::<Vec<_>>();
            (size, paths)
        })
        .filter(|(_, paths)| paths.len() > 1)
        .collect();
    // Largest possible saving first, so a budget runs out where it is worth
    // least. Ties by size then path, so two runs over one disk answer alike.
    pending.sort_by(|a, b| {
        let (wa, wb) = (a.0 * (a.1.len() as u64 - 1), b.0 * (b.1.len() as u64 - 1));
        wb.cmp(&wa)
            .then(b.0.cmp(&a.0))
            .then(a.1.first().cmp(&b.1.first()))
    });

    let mut out: Vec<Group> = Vec::new();
    let mut read = 0u64;
    let mut unconfirmed = 0u64;

    for (size, mut paths) in pending {
        paths.sort();
        // Out of budget: a real size collision and a real possible saving, but
        // it goes into the answer saying it is unconfirmed. Asked before the
        // reading, not after: a budget is what may be read, not where to stop.
        let ends = paths.len() as u64 * size.min(2 * EDGE);
        if read.saturating_add(ends) > opts.read_budget {
            unconfirmed += 1;
            out.push(Group {
                size,
                paths,
                certainty: Certainty::Size,
            });
            continue;
        }

        // Stage two: the ends, which is where nearly all differing files differ.
        let mut buckets: HashMap<u64, Vec<String>> = HashMap::new();
        // Two names of one file — a hard link — are not two copies: deleting
        // one gives back nothing. The first name, sorted, stands for the file.
        let mut files = std::collections::HashSet::new();
        for p in paths {
            if identity(Path::new(&p)).is_some_and(|id| !files.insert(id)) {
                continue;
            }
            // Dropped rather than fatal: one vanished file must not cost the
            // rest of the run its answer.
            if let Ok((mark, n)) = edges(Path::new(&p), size) {
                read += n;
                buckets.entry(mark).or_default().push(p);
            }
        }

        for (_, mut same) in buckets {
            if same.len() < 2 {
                continue;
            }
            same.sort();
            // Stage three: the bytes, a comparison and not a digest, since this
            // decides a deletion. Each class of identical files is its own group.
            let (classes, left, n) = partition(&same, size, opts.read_budget.saturating_sub(read));
            read += n;
            for class in classes.into_iter().filter(|c| c.len() > 1) {
                out.push(Group {
                    size,
                    paths: class,
                    certainty: Certainty::Content,
                });
            }
            // What the budget did not reach still shares a size and its ends.
            if left.len() > 1 {
                unconfirmed += 1;
                out.push(Group {
                    size,
                    paths: left,
                    certainty: Certainty::Edges,
                });
            }
        }
    }

    out.sort_by(|a, b| {
        b.waste()
            .cmp(&a.waste())
            .then(b.size.cmp(&a.size))
            .then(a.paths.first().cmp(&b.paths.first()))
    });
    let waste = out.iter().map(Group::waste).sum();
    let proven = out
        .iter()
        .filter(|g| g.certainty == Certainty::Content)
        .map(Group::waste)
        .sum();
    out.truncate(opts.top);
    Report {
        groups: out,
        candidates,
        waste,
        proven,
        read,
        unconfirmed,
    }
}

/// One number from the first and last four kilobytes, and how much was read. A
/// file shorter than two edges is read once and whole.
fn edges(path: &Path, size: u64) -> std::io::Result<(u64, u64)> {
    let mut f = File::open(path)?;
    let mut h = Fnv::new();
    // The size goes into the mark, so a short read cannot collide two lengths.
    h.write(&size.to_le_bytes());

    let mut buf = vec![0u8; EDGE as usize];
    let head = f.read(&mut buf)?;
    h.write(&buf[..head]);
    let mut moved = head as u64;

    if size > EDGE * 2 {
        f.seek(SeekFrom::End(-(EDGE as i64)))?;
        let tail = f.read(&mut buf)?;
        h.write(&buf[..tail]);
        moved += tail as u64;
    }
    Ok((h.finish(), moved))
}

/// These, sorted into classes that are identical byte for byte, and how much
/// was read. Each file is held against one member of each class found so far:
/// equality is transitive, so that is enough — but only against the first of
/// them all, `a b b` lost the two that matched each other. Stops before a
/// comparison the budget cannot pay for (two whole files); what it had not
/// reached comes back as the second list.
fn partition(paths: &[String], size: u64, budget: u64) -> (Vec<Vec<String>>, Vec<String>, u64) {
    let mut classes: Vec<Vec<String>> = Vec::new();
    let mut read = 0u64;
    for (at, path) in paths.iter().enumerate() {
        let mut placed = false;
        for class in &mut classes {
            if read.saturating_add(size * 2) > budget {
                return (classes, paths[at..].to_vec(), read);
            }
            match identical(Path::new(&class[0]), Path::new(path)) {
                Ok(true) => {
                    read += size * 2;
                    class.push(path.clone());
                    placed = true;
                    break;
                }
                Ok(false) => read += size, // Parted early; a whole file's worth over-estimates.
                // Unreadable: no class, and no new one either.
                Err(_) => {
                    placed = true;
                    break;
                }
            }
        }
        if !placed {
            classes.push(vec![path.clone()]);
        }
    }
    (classes, Vec::new(), read)
}

/// Which file a name is, where the platform can say: the device and the inode.
#[cfg(unix)]
fn identity(p: &Path) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    std::fs::symlink_metadata(p)
        .ok()
        .map(|m| (m.dev(), m.ino()))
}

#[cfg(not(unix))]
fn identity(_: &Path) -> Option<(u64, u64)> {
    None
}

/// Byte for byte, stopping at the first difference.
fn identical(a: &Path, b: &Path) -> std::io::Result<bool> {
    // The same path twice needs no reading.
    if a == b {
        return Ok(true);
    }
    let (mut fa, mut fb) = (File::open(a)?, File::open(b)?);
    let (mut ba, mut bb) = (vec![0u8; CHUNK], vec![0u8; CHUNK]);
    loop {
        let na = read_full(&mut fa, &mut ba)?;
        let nb = read_full(&mut fb, &mut bb)?;
        if na != nb || ba[..na] != bb[..nb] {
            return Ok(false);
        }
        if na == 0 {
            return Ok(true);
        }
    }
}

/// Fill the buffer, or reach the end trying. `Read::read` may return short at any
/// time, and treating that as end-of-file calls two different files identical.
fn read_full(f: &mut File, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut at = 0;
    while at < buf.len() {
        match f.read(&mut buf[at..])? {
            0 => break,
            n => at += n,
        }
    }
    Ok(at)
}

/// FNV-1a, 64 bits. Only ever a filter: two files that agree here are then
/// compared in full, so no hash decides identity.
struct Fnv(u64);

impl Fnv {
    fn new() -> Self {
        Fnv(0xcbf2_9ce4_8422_2325)
    }
    fn write(&mut self, bytes: &[u8]) {
        for b in bytes {
            self.0 ^= *b as u64;
            self.0 = self.0.wrapping_mul(0x1000_0000_01b3);
        }
    }
    fn finish(&self) -> u64 {
        self.0
    }
}
