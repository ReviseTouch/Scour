//! The generator.

use std::collections::HashSet;

use scour_core::{Entry, EntryId, Meta, SourceId};

/// A tiny deterministic PRNG. No dependency, and every run reproduces.
#[derive(Debug)]
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Rng(seed.max(1))
    }

    pub fn next_u64(&mut self) -> u64 {
        // xorshift64*
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    pub fn below(&mut self, n: usize) -> usize {
        (self.next_u64() % n.max(1) as u64) as usize
    }

    pub fn pick<'a, T>(&mut self, v: &'a [T]) -> &'a T {
        &v[self.below(v.len())]
    }

    /// A float in `[0, 1)`.
    pub fn unit(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }
}

const DIR_WORDS: &[&str] = &[
    "src",
    "lib",
    "test",
    "tests",
    "build",
    "dist",
    "target",
    "node_modules",
    "assets",
    "docs",
    "config",
    "cache",
    "include",
    "bin",
    "data",
    "logs",
    "tmp",
    "vendor",
    "public",
    "static",
    "components",
    "modules",
    "services",
    "handlers",
    "models",
    "views",
    "deps",
    "release",
    "Projeler",
    "Belgeler",
    "Downloads",
    "Resimler",
    "Müzik",
    "Masaüstü",
    "debug",
    "incremental",
    "fingerprint",
    "packages",
    "registry",
    "share",
    "state",
    "run",
];

const NAME_WORDS: &[&str] = &[
    "index", "main", "config", "server", "client", "parser", "handler", "widget", "buffer",
    "stream", "socket", "thread", "packet", "record", "schema", "router", "engine", "worker",
    "session", "request", "response", "manager", "factory", "builder", "adapter", "context",
    "rapor", "belge", "sunum", "fatura", "sozlesme", "colpan", "proje", "yedek", "notlar",
];

/// Extension and the kind it implies, so the generated tree exercises every
/// branch of the classifier.
const EXTS: &[&str] = &[
    "rs", "toml", "json", "js", "ts", "py", "sh", "yaml", "md", "txt", "pdf", "docx", "png", "jpg",
    "svg", "mp4", "mp3", "zip", "tar", "gz", "so", "o", "d", "lock", "log", "",
];

const HEX: &[u8] = b"0123456789abcdef";

#[derive(Debug, Clone)]
pub struct MockOptions {
    pub files: usize,
    pub seed: u64,
    /// The newest timestamp; everything is generated backwards from it.
    pub now: i64,
    /// How many files share one timestamp on average. The real index measured
    /// 115. A value of one would be the unrealistic uniform case.
    pub files_per_event: usize,
    pub source: SourceId,
}

impl Default for MockOptions {
    fn default() -> Self {
        Self {
            files: 500_000,
            seed: 42,
            now: 1_785_000_000,
            files_per_event: 115,
            source: SourceId(0),
        }
    }
}

#[derive(Debug, Default)]
pub struct MockFs {
    pub entries: Vec<Entry>,
    /// Directories that exist, for choosing a rename or delete victim.
    pub dirs: Vec<String>,
}

/// Build the fake tree.
pub fn generate(opt: &MockOptions) -> MockFs {
    let mut rng = Rng::new(opt.seed);

    // Timestamps: a small set of bulk events over three years.
    let n_events = (opt.files / opt.files_per_event.max(1)).max(16);
    let mut events: Vec<i64> = (0..n_events)
        .map(|_| opt.now - (rng.unit().powf(1.7) * 3.0 * 365.0 * 86_400.0) as i64)
        .collect();
    events.sort_unstable();

    // The directory skeleton. Every directory is an entry too, exactly as a
    // real scanner produces.
    let mut dirs: Vec<String> = [
        "/home/u",
        "/home/u/Projeler",
        "/home/u/.cache",
        "/home/u/.local/share",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    let mut seen: HashSet<String> = dirs.iter().cloned().collect();
    let target_dirs = (opt.files / 12).max(64);
    while dirs.len() < target_dirs {
        let parent = dirs[rng.below(dirs.len())].clone();
        if parent.matches('/').count() > 8 {
            continue; // deep paths exist, but rarely
        }
        let name = if rng.unit() < 0.15 {
            format!("{}-{}", rng.pick(DIR_WORDS), rng.below(500))
        } else {
            rng.pick(DIR_WORDS).to_string()
        };
        let child = format!("{parent}/{name}");
        if seen.insert(child.clone()) {
            dirs.push(child);
        }
    }

    // One big, old tree: the adversarial case, where many matches sit at the
    // far end of the sort order so anything walking newest-first has a long way
    // to go before it can stop.
    let ancient = "/home/u/Projeler/eski-arsiv";
    dirs.push(ancient.to_string());
    // Clustered like the rest, and for the same reason: an archive extraction
    // stamps everything it writes at one instant. Giving these files scattered
    // timestamps instead would quietly make the generated tree *easier* than a
    // real one, since large tie groups are exactly what a page boundary has to
    // cope with.
    let ancient_base = opt.now - 3 * 365 * 86_400;
    let ancient_events: Vec<i64> = (0..32)
        .map(|_| ancient_base - rng.below(90 * 86_400) as i64)
        .collect();

    let mut entries: Vec<Entry> = Vec::with_capacity(opt.files + dirs.len());
    // **No path twice.** A filesystem cannot hold two entries with the same
    // name in the same directory, and a generator that does hands the index a
    // case it will never see — which is worse than a missing case, because
    // every count computed from `MockFs::entries` then disagrees with what a
    // correct index reports. Names are random, so collisions happen: five in
    // 8,667 entries, and they went unnoticed for as long as identity was the
    // inode, which made two files at one path look like two files.
    let mut taken: HashSet<String> = HashSet::with_capacity(opt.files + dirs.len());
    let mut push = |path: String, is_dir: bool, size: i64, mtime: i64| {
        if !taken.insert(path.clone()) {
            return false;
        }
        entries.push(Entry {
            id: EntryId::path_hash(opt.source, &path),
            path,
            is_dir,
            meta: Meta {
                size,
                mtime,
                ctime: mtime,
                atime: mtime,
                mode: if is_dir { 0o40755 } else { 0o100644 },
                uid: 1000,
                gid: 1000,
                disk: (size + 4095) / 4096 * 4096,
                items: if is_dir { 0 } else { -1 },
            },
        });
        true
    };

    for d in &dirs {
        push(d.clone(), true, 0, events[rng.below(events.len())]);
    }

    let ancient_share = opt.files / 12; // ~8% of the tree is old and clustered
    for i in 0..opt.files {
        let old_one = i < ancient_share;
        let dir = if old_one {
            ancient.to_string()
        } else {
            dirs[rng.below(dirs.len())].clone()
        };

        let roll = rng.unit();
        let stem = if roll < 0.45 {
            format!("{}_{}", rng.pick(NAME_WORDS), rng.below(100_000))
        } else if roll < 0.65 {
            (0..16)
                .map(|_| HEX[rng.below(HEX.len())] as char)
                .collect::<String>()
        } else if roll < 0.80 {
            format!(
                "{}-{}.{}.{}",
                rng.pick(NAME_WORDS),
                rng.below(9),
                rng.below(20),
                rng.below(99)
            )
        } else if roll < 0.88 {
            format!(".{}", rng.pick(NAME_WORDS))
        } else {
            format!("{}_{}", rng.pick(NAME_WORDS), rng.pick(NAME_WORDS))
        };
        let ext = *rng.pick(EXTS);
        let name = if ext.is_empty() {
            stem
        } else {
            format!("{stem}.{ext}")
        };

        // Log-normal-ish: a few files are huge and most are small.
        let size = 10f64.powf(rng.unit() * 7.0) as i64;
        let mtime = if old_one {
            ancient_events[rng.below(ancient_events.len())]
        } else {
            events[rng.below(events.len())]
        };
        push(format!("{dir}/{name}"), false, size, mtime);
    }

    MockFs { entries, dirs }
}

/// Statistics that show whether the generated tree really looks like a
/// filesystem, so the numbers can be checked against the real index.
pub fn describe(fs: &MockFs) -> String {
    use scour_core::text::{DefaultFolder, Folder};
    let n = fs.entries.len();
    let mtimes: HashSet<i64> = fs.entries.iter().map(|e| e.meta.mtime).collect();
    let names: HashSet<String> = fs
        .entries
        .iter()
        .map(|e| DefaultFolder.fold(e.name()))
        .collect();
    let depth: usize = fs.entries.iter().map(|e| e.path.matches('/').count()).sum();
    let bytes: usize = fs.entries.iter().map(|e| e.path.len()).sum();
    format!(
        "{n} entries ({} dirs) · {} distinct mtimes ({:.0} files each; the real index measured 115) \
         · {} distinct names · avg depth {:.1} · avg path {} bytes",
        fs.dirs.len(),
        mtimes.len(),
        n as f64 / mtimes.len() as f64,
        names.len(),
        depth as f64 / n as f64,
        bytes / n.max(1)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_tree_is_lumpy_like_a_real_one() {
        let fs = generate(&MockOptions {
            files: 20_000,
            ..Default::default()
        });
        let mtimes: HashSet<i64> = fs.entries.iter().map(|e| e.meta.mtime).collect();
        // The property that matters: far fewer timestamps than files. A uniform
        // generator would produce roughly one each and flatter every design.
        // The real index measured 115 files per distinct timestamp.
        assert!(
            fs.entries.len() / mtimes.len() > 50,
            "timestamps should cluster, got {} files over {} stamps",
            fs.entries.len(),
            mtimes.len()
        );
        assert!(fs.entries.iter().any(|e| e.is_dir));
        assert!(fs.entries.iter().any(|e| e.path.contains("eski-arsiv")));
    }

    #[test]
    fn generation_is_reproducible() {
        let a = generate(&MockOptions {
            files: 500,
            ..Default::default()
        });
        let b = generate(&MockOptions {
            files: 500,
            ..Default::default()
        });
        assert_eq!(a.entries, b.entries);
        let c = generate(&MockOptions {
            files: 500,
            seed: 43,
            ..Default::default()
        });
        assert_ne!(a.entries, c.entries);
    }

    #[test]
    fn identities_are_unique() {
        let fs = generate(&MockOptions {
            files: 5_000,
            ..Default::default()
        });
        let ids: HashSet<_> = fs.entries.iter().map(|e| &e.id).collect();
        assert_eq!(
            ids.len(),
            fs.entries.len(),
            "two entries must never share an id"
        );
    }
}
