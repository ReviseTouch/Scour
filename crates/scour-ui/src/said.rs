//! Sentences every face says the same way, as msgids, and how many folders
//! opening a selection may open.

/// Opening a selection's folders: asked about past this many windows, and not
/// offered past [`OPEN_FOLDERS_MOST`] — nobody means fifty Explorer windows.
pub const OPEN_FOLDERS_ASK: usize = 3;

pub const OPEN_FOLDERS_MOST: usize = 20;

/// What every face says while the index is being walked. A number, not a word:
/// a still indicator cannot be told from a stuck one. The msgid rather than the
/// sentence, because only the caller can punctuate a number.
pub const SCANNING: &str = "scanning {n}";

/// What every face says while the index cannot be written down: the changes
/// are in memory, findable, and gone if the service stops. Said in the place
/// the walk is said, and before it — a full disk was twelve lines in a journal
/// nobody reads (14 November) and nothing on screen.
pub const UNWRITTEN: &str = "the index cannot be saved — is the disk full?";

/// The index has grown an unsorted tail, and searching has slowed for it: a
/// week of ordinary use took ordering by path from 1.9 ms to 21.5, and one
/// rebuild put it back. Names the command, not only the condition.
pub const REBUILD_ADVISED: &str = "`scour maintain rebuild` would speed searches up";

/// A `content:` query, on the status line: its files being read, the reading
/// stopped before the last of them, and all of them read.
pub const READING: &str = "reading files: {n}";

pub const READ_HALF: &str = "stopped half-way: {n} files read";

pub const READ_ALL: &str = "{n} files read";

/// The two things to press there: read on from where it stopped, and forget
/// what was found and read the files again.
pub const READ_ON: &str = "go on";

pub const READ_AGAIN: &str = "read again";

/// Which of the three sentences a reading is at.
pub fn reading_said(running: bool, done: bool) -> &'static str {
    match (running, done) {
        (true, _) => READING,
        (false, true) => READ_ALL,
        (false, false) => READ_HALF,
    }
}

/// Words people write as operators that this language does not read as ones:
/// `a or b` looks for three words, and nothing in the colouring says so.
pub const MISTAKEN: [&str; 6] = ["or", "and", "not", "ve", "veya", "değil"];
