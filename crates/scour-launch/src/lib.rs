//! Starting `scourd` when nothing answers.
//!
//! A face that finds no service used to print the name of a program the person
//! had never heard of. Instead it starts one: `scourd` is looked for next to
//! the face's own binary and then on `PATH`, started in a session of its own
//! with its output appended to a log, and the socket is watched until it
//! answers. That makes a menu entry work on a machine with no systemd, and
//! inside a Flatpak, without this crate knowing either exists.
//!
//! Racing a system unit is safe and is not detected: `scourd` holds one writer
//! lock on the index directory, so the loser of the race exits and the winner
//! is the one the poll finds.
//!
//! **`SCOUR_NO_AUTOSTART`** turns all of it off — set it to anything but `0`
//! and a face behaves exactly as it did before: it reports that nothing is
//! listening and starts nothing. Test harnesses and anyone running a private
//! service want that.

mod binary;
mod outcome;
mod start;

pub use binary::{NAME, locate, scourd};
pub use outcome::Outcome;
pub use start::{Autostart, ensure_once, wanted};
