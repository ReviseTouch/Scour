//! What is asked and what comes back: the bounds, and each group and how sure
//! it is.

/// What a caller wants out of this.
#[derive(Debug, Clone)]
pub struct Options {
    /// Ignore anything smaller than this. Below a megabyte the candidates are a
    /// million files over 28 GB; above it, nineteen thousand over 141.8 GB.
    pub min_size: u64,
    /// How many bytes may be read to confirm. Zero is a supported answer: the
    /// size groups and their potential saving cost nothing.
    pub read_budget: u64,
    /// At most this many groups in the reply, largest saving first.
    pub top: usize,
}

impl Default for Options {
    fn default() -> Self {
        // A megabyte is the measured knee; a gigabyte of reading confirms the
        // whole interesting range above it.
        Options {
            min_size: 1024 * 1024,
            read_budget: 1024 * 1024 * 1024,
            top: 50,
        }
    }
}

/// How much this group has been proven, rather than guessed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Certainty {
    /// Same size, nothing read. These files *may* be identical.
    Size,
    /// Same size and same first and last four kilobytes. Likely, not proven.
    Edges,
    /// Read end to end and compared. Identical.
    Content,
}

impl Certainty {
    /// A stable word for a wire format or a message id.
    pub fn token(self) -> &'static str {
        match self {
            Certainty::Size => "size",
            Certainty::Edges => "edges",
            Certainty::Content => "content",
        }
    }
}

/// Files that are, or may be, the same file.
#[derive(Debug, Clone)]
pub struct Group {
    /// What each of them weighs; equal size is what makes them a group.
    pub size: u64,
    /// Their paths, in the order they arrived.
    pub paths: Vec<String>,
    pub certainty: Certainty,
}

impl Group {
    /// What deleting all but one would give back. Groups are ordered by this and
    /// not by size: ten copies of 100 MB beat two copies of 400 MB.
    pub fn waste(&self) -> u64 {
        self.size * (self.paths.len() as u64 - 1)
    }
}

/// What one run found.
#[derive(Debug, Clone, Default)]
pub struct Report {
    /// Largest saving first.
    pub groups: Vec<Group>,
    /// How many files were considered at all.
    pub candidates: u64,
    /// Everything the groups could give back, including those `top` left out: a
    /// total that moved with the list length would be one nobody could act on.
    pub waste: u64,
    /// How much of `waste` was read and compared rather than guessed: 39.36 GiB
    /// by size was 18.29 GiB once read, so `waste` alone names files that are
    /// merely the same length.
    pub proven: u64,
    /// Bytes actually read confirming.
    pub read: u64,
    /// Groups the budget did not reach. Without it a partial answer looks like a
    /// complete one.
    pub unconfirmed: u64,
}
