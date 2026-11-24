//! The desktop's trash: <https://specifications.freedesktop.org/trash-spec/>.
//! A deletion is a move, so a wrong row is a mistake the file manager can undo.
//! `$XDG_DATA_HOME/Trash` for the home filesystem, `$topdir/.Trash/$uid` or
//! `$topdir/.Trash-$uid` for another, because the move must be a rename. A name
//! is claimed by creating its `info/NAME.trashinfo` with `O_EXCL`, never checked.

mod error;
mod place;
mod put;
pub use error::*;
#[cfg(unix)]
use place::*;
pub use put::*;

#[cfg(all(test, unix))]
mod tests;
