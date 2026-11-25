//! The place lens's disk tiles: what the `volumes` and `sources` answers say
//! about each disk, put the way a tile shows it. Pure: the faces ask, this
//! decides, and every face draws the same tile from the same two answers.
//!
//! This crate depends on nothing, so a disk comes in as borrowed fields of
//! the wire's `scour_proto::Disk`, not as the type itself.

use crate::path::untrailed as trim;

/// A disk of the `volumes` answer, borrowed field by field.
#[derive(Debug, Clone, Copy)]
pub struct DiskIn<'a> {
    /// `volume.id`.
    pub id: &'a str,
    /// `volume.label`; the first root names a disk without one.
    pub label: &'a str,
    /// `volume.kind`, as the wire spells it.
    pub kind: &'a str,
    pub fs: &'a str,
    pub roots: &'a [String],
    /// Bytes; 0 for a network disk.
    pub size: u64,
    /// `state`, as the wire spells it.
    pub state: &'a str,
    /// `seen.live` and `seen.walking`.
    pub live: bool,
    pub walking: bool,
}

/// A source of the `sources` answer: its name and roots.
#[derive(Debug, Clone, Copy)]
pub struct SourceIn<'a> {
    pub name: &'a str,
    pub roots: &'a [String],
}

/// What a tile says about a disk, besides its name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shown {
    /// Indexed and watched: what changes is seen as it changes.
    Live,
    /// Indexed, walked again now and then.
    Polled,
    /// Being walked now.
    Walking,
    /// Plugged in and waiting for an answer (`asking` or `later`).
    Asking,
    /// Not indexed as a whole, but sources lie inside it.
    Partly,
    /// Nothing on it is indexed.
    Off,
}

impl Shown {
    /// The token a page's class or a test names it by.
    pub fn as_str(self) -> &'static str {
        match self {
            Shown::Live => "live",
            Shown::Polled => "polled",
            Shown::Walking => "walking",
            Shown::Asking => "asking",
            Shown::Partly => "partly",
            Shown::Off => "off",
        }
    }

    /// The msgid a tile says it with.
    pub fn label(self) -> &'static str {
        match self {
            Shown::Live => "watched live",
            Shown::Polled => "checked now and then",
            Shown::Walking => "being walked",
            Shown::Asking => "waiting for an answer",
            Shown::Partly => "some folders indexed",
            Shown::Off => "not indexed",
        }
    }
}

/// One disk of the place lens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tile {
    pub id: String,
    /// The label, or the first root for a disk without one.
    pub name: String,
    /// One of [`super::KINDS`]; an unknown one is `fixed`, as the wire's default.
    pub kind: &'static str,
    pub fs: String,
    /// The disk's first root: what a click writes as `under:`.
    pub root: String,
    pub size: u64,
    /// The source roots that lie on this disk, by the longest disk root above
    /// each: the tags of a `Partly` tile.
    pub roots: Vec<String>,
    /// A source covers the disk's own root, so `roots` need no tags.
    pub whole: bool,
    pub shown: Shown,
    /// The files on it, once counted.
    pub files: Option<u64>,
    /// The `count` query its files are asked with (cap `u32::MAX`); none when
    /// nothing on it is indexed.
    pub count: Option<String>,
    /// It may offer "Index it": an `asking` or `off` disk, never a `Partly`
    /// one, whose yes would make the whole disk a source.
    pub index_it: bool,
}

impl Tile {
    /// A click on it writes `under:` its root: something on it is indexed.
    pub fn choosable(&self) -> bool {
        self.count.is_some()
    }
}

/// The tiles of `disks`, in their order. `sources` gives the roots inside
/// each disk; `files` the counts so far, by disk id.
pub fn tiles(
    disks: &[DiskIn],
    sources: &[SourceIn],
    files: impl Fn(&str) -> Option<u64>,
) -> Vec<Tile> {
    disks
        .iter()
        .map(|d| tile(d, disks, sources, files(d.id)))
        .collect()
}

fn tile(d: &DiskIn, all: &[DiskIn], sources: &[SourceIn], files: Option<u64>) -> Tile {
    let roots = roots_on(d.id, all, sources);
    let whole = roots
        .iter()
        .any(|r| d.roots.iter().any(|x| r.as_str() == trim(x)));
    let indexed = matches!(d.state, "auto" | "chosen" | "configured");
    let shown = match d.state {
        "asking" | "later" => Shown::Asking,
        _ if (indexed || !roots.is_empty()) && d.walking => Shown::Walking,
        _ if indexed && d.live => Shown::Live,
        _ if indexed => Shown::Polled,
        _ if !roots.is_empty() => Shown::Partly,
        _ => Shown::Off,
    };
    // An indexed disk counted before `sources` came is its own source.
    let counted: Vec<String> = match (indexed, roots.is_empty()) {
        (true, true) => d.roots.iter().map(|r| trim(r).to_string()).collect(),
        _ => roots.clone(),
    };
    let count =
        (shown != Shown::Asking && !counted.is_empty()).then(|| count_query(d.id, &counted, all));
    Tile {
        id: d.id.to_string(),
        name: if d.label.is_empty() {
            d.roots.first().cloned().unwrap_or_default()
        } else {
            d.label.to_string()
        },
        kind: kind_of(d.kind),
        fs: d.fs.to_string(),
        root: d.roots.first().map_or("", |r| trim(r)).to_string(),
        size: d.size,
        // Never empty for an indexed disk: a click must have a term to write.
        whole: whole || (indexed && roots.is_empty()),
        roots: counted,
        shown,
        files: count.as_ref().and(files),
        count,
        index_it: matches!(shown, Shown::Asking | Shown::Off),
    }
}

fn kind_of(token: &str) -> &'static str {
    super::KINDS
        .iter()
        .find(|k| **k == token)
        .copied()
        .unwrap_or("fixed")
}

/// Whether `path` is `root` or below it, by whole components.
pub(crate) fn lies_under(path: &str, root: &str) -> bool {
    let (path, root) = (trim(path), trim(root));
    root == "/" && path.starts_with('/')
        || path == root
        || path.strip_prefix(root).is_some_and(|r| r.starts_with('/'))
}

/// The disk a path lies on: the one with the longest root above it.
pub fn disk_of<'a>(path: &str, disks: &[DiskIn<'a>]) -> Option<&'a str> {
    disks
        .iter()
        .flat_map(|d| d.roots.iter().map(move |r| (d.id, trim(r))))
        .filter(|(_, r)| lies_under(path, r))
        .max_by_key(|(_, r)| r.len())
        .map(|(id, _)| id)
}

/// Every source root that lies on the disk `id`, sorted, each once.
fn roots_on(id: &str, disks: &[DiskIn], sources: &[SourceIn]) -> Vec<String> {
    let mut out: Vec<String> = sources
        .iter()
        .flat_map(|s| s.roots.iter())
        .filter(|r| disk_of(r, disks) == Some(id))
        .map(|r| trim(r).to_string())
        .collect();
    out.sort();
    out.dedup();
    out
}

/// `file: under:a|under:b`, less every other disk mounted below those roots:
/// `under:/` would count the disks inside it too.
fn count_query(id: &str, roots: &[String], disks: &[DiskIn]) -> String {
    let within: Vec<String> = roots.iter().map(|r| under(r)).collect();
    let mut q = format!("file: {}", within.join("|"));
    let others = disks.iter().filter(|d| d.id != id).flat_map(|d| d.roots);
    for r in others.map(|r| trim(r)) {
        if roots.iter().any(|x| x != r && lies_under(r, x)) {
            q.push_str(" !");
            q.push_str(&under(r));
        }
    }
    q
}

fn under(path: &str) -> String {
    format!("under:{}", crate::lens::quoted_path(path))
}

#[cfg(test)]
#[path = "tiles_tests.rs"]
mod tests;
