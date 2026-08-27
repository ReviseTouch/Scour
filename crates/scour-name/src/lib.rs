//! What a file may be called, and giving it a different name.
//!
//! **The rename is one line; the answer to "is that a name" is not.** Three
//! faces offer to rename a file, and three copies of the checks would be three
//! sets of rules that agree today: one would forget that a trailing space is
//! legal on Linux and invisible everywhere, another would allow `..` and turn
//! a rename into a move, a third would let an empty box through and leave the
//! reader looking at a file that has vanished from the list because it is now
//! called nothing.
//!
//! So the checks are here, and each of them returns *why* rather than a bool —
//! a face that can only say "no" is a face that makes somebody guess.
//!
//! ## What this deliberately does not do
//!
//! It does not move files. A new name with a slash in it is refused rather
//! than followed: the reader typed into a box beside a file name, and a box
//! beside a file name that quietly relocates the file is a trap. Moving is a
//! different gesture and belongs to a file manager.

use std::path::{Path, PathBuf};

/// Why a name was refused.
///
/// Each one is a sentence a face can show as it is. They are English source
/// strings and therefore catalogue keys, the same as everything else a person
/// reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// Nothing was typed.
    Empty,
    /// `/` — that is a move, and this is not a move.
    HasSlash,
    /// `.` or `..`, which name a directory rather than a file in one.
    Dots,
    /// A NUL byte, which no filesystem will take.
    HasNul,
    /// Ends in a space or a dot. Legal here, invisible to the reader, and
    /// refused by Windows and by every archive that has to survive a trip
    /// through it.
    TrailingSpace,
    /// Longer than a filesystem component may be.
    TooLong,
    /// Something is already called that, in that folder.
    Taken,
    /// The same name it already has.
    Unchanged,
}

impl Refusal {
    /// The English sentence, and therefore the catalogue key.
    pub fn msgid(self) -> &'static str {
        match self {
            Refusal::Empty => "a name cannot be empty",
            Refusal::HasSlash => "a name cannot contain a slash",
            Refusal::Dots => "that names a folder, not a file in one",
            Refusal::HasNul => "that is not a name a filesystem will take",
            Refusal::TrailingSpace => "a name should not end in a space or a dot",
            Refusal::TooLong => "that name is too long",
            Refusal::Taken => "something is already called that",
            Refusal::Unchanged => "that is the name it already has",
        }
    }
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.msgid())
    }
}

/// The longest a single path component may be, on every filesystem in use.
///
/// `NAME_MAX` is 255 **bytes**, not characters — a Turkish name of 200 letters
/// is over it. Counting characters here would let a name through that the
/// kernel then refuses, and the reader would be told nothing by the box and
/// everything by a failure afterwards.
const NAME_MAX: usize = 255;

/// Would this be a legal new name for that path?
///
/// Answers before anything is touched, so a face can grey a button or colour a
/// box while somebody is still typing.
pub fn check(path: &Path, new_name: &str) -> Result<(), Refusal> {
    if new_name.is_empty() {
        return Err(Refusal::Empty);
    }
    if new_name.contains('/') {
        return Err(Refusal::HasSlash);
    }
    if new_name.contains('\0') {
        return Err(Refusal::HasNul);
    }
    if new_name == "." || new_name == ".." {
        return Err(Refusal::Dots);
    }
    if new_name.ends_with(' ') || new_name.ends_with('.') {
        return Err(Refusal::TrailingSpace);
    }
    if new_name.len() > NAME_MAX {
        return Err(Refusal::TooLong);
    }
    if path.file_name().and_then(|n| n.to_str()) == Some(new_name) {
        return Err(Refusal::Unchanged);
    }
    if beside(path, new_name).symlink_exists() {
        return Err(Refusal::Taken);
    }
    Ok(())
}

/// Give it the new name. Returns where it now is.
///
/// The checks run again here rather than being assumed: between a box being
/// typed into and a button being pressed, something else can create the name
/// that was free.
pub fn rename(path: &Path, new_name: &str) -> Result<PathBuf, Refusal> {
    check(path, new_name)?;
    let to = beside(path, new_name);
    // **`rename` and not `copy` + `remove`.** A rename is atomic and keeps the
    // inode, so anything holding the file open keeps holding the same file, and
    // a failure leaves the original exactly where it was.
    match std::fs::rename(path, &to) {
        Ok(()) => Ok(to),
        // The only failure worth naming separately: somebody won the race
        // between the check above and this line.
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Err(Refusal::Taken),
        Err(_) => Err(Refusal::Taken),
    }
}

/// The same folder, a different name.
fn beside(path: &Path, name: &str) -> PathBuf {
    match path.parent() {
        Some(dir) => dir.join(name),
        None => PathBuf::from(name),
    }
}

trait SymlinkExists {
    /// Is there anything there — including a symlink pointing nowhere?
    ///
    /// `Path::exists` follows links, so a broken link reads as free and the
    /// rename then fails with a name the reader was told was available.
    fn symlink_exists(&self) -> bool;
}

impl SymlinkExists for PathBuf {
    fn symlink_exists(&self) -> bool {
        std::fs::symlink_metadata(self).is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sandbox() -> PathBuf {
        let at = std::env::temp_dir().join(format!(
            "scour-name-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .subsec_nanos()
        ));
        std::fs::create_dir_all(&at).unwrap();
        at
    }

    #[test]
    fn every_refusal_says_which_one_it_is() {
        let box_ = sandbox();
        let file = box_.join("notes.txt");
        std::fs::write(&file, b"x").unwrap();

        assert_eq!(check(&file, ""), Err(Refusal::Empty));
        assert_eq!(check(&file, "a/b"), Err(Refusal::HasSlash));
        assert_eq!(check(&file, ".."), Err(Refusal::Dots));
        assert_eq!(check(&file, "a\0b"), Err(Refusal::HasNul));
        assert_eq!(check(&file, "notes "), Err(Refusal::TrailingSpace));
        assert_eq!(check(&file, "notes."), Err(Refusal::TrailingSpace));
        assert_eq!(check(&file, "notes.txt"), Err(Refusal::Unchanged));
        assert_eq!(check(&file, &"x".repeat(256)), Err(Refusal::TooLong));
        assert_eq!(check(&file, "başka.txt"), Ok(()));

        std::fs::remove_dir_all(&box_).ok();
    }

    /// A Turkish name of two hundred letters is over the limit, and a naive
    /// character count says it is not.
    #[test]
    fn the_length_is_counted_in_bytes_because_that_is_what_the_kernel_counts() {
        let box_ = sandbox();
        let file = box_.join("a.txt");
        std::fs::write(&file, b"x").unwrap();
        // 200 characters, 400 bytes.
        let long = "ğ".repeat(200);
        assert_eq!(long.chars().count(), 200);
        assert_eq!(check(&file, &long), Err(Refusal::TooLong));
        std::fs::remove_dir_all(&box_).ok();
    }

    #[test]
    fn a_name_already_in_use_is_refused_before_anything_moves() {
        let box_ = sandbox();
        let a = box_.join("a.txt");
        let b = box_.join("b.txt");
        std::fs::write(&a, b"one").unwrap();
        std::fs::write(&b, b"two").unwrap();

        assert_eq!(check(&a, "b.txt"), Err(Refusal::Taken));
        assert_eq!(rename(&a, "b.txt"), Err(Refusal::Taken));
        // And nothing happened to either of them.
        assert_eq!(std::fs::read(&a).unwrap(), b"one");
        assert_eq!(std::fs::read(&b).unwrap(), b"two");

        std::fs::remove_dir_all(&box_).ok();
    }

    /// A broken symlink is something there, and `Path::exists` says it is not.
    #[test]
    fn a_link_pointing_nowhere_still_holds_its_name() {
        let box_ = sandbox();
        let file = box_.join("real.txt");
        std::fs::write(&file, b"x").unwrap();
        let dangling = box_.join("gone.txt");
        std::os::unix::fs::symlink(box_.join("never-existed"), &dangling).unwrap();
        assert!(!dangling.exists(), "the test's premise: it follows the link");

        assert_eq!(check(&file, "gone.txt"), Err(Refusal::Taken));

        std::fs::remove_dir_all(&box_).ok();
    }

    #[test]
    fn a_rename_keeps_the_bytes_and_moves_nothing_else() {
        let box_ = sandbox();
        let file = box_.join("before.txt");
        std::fs::write(&file, b"hello").unwrap();

        let now = rename(&file, "after.txt").expect("renamed");
        assert_eq!(now, box_.join("after.txt"));
        assert!(!file.exists());
        assert_eq!(std::fs::read(&now).unwrap(), b"hello");

        std::fs::remove_dir_all(&box_).ok();
    }
}
