//! Which programs on this machine claim a kind of file, and starting one — what
//! `Open with…` needs and no command prints. Read from `applications/*.desktop`
//! and `mimeapps.list`, earlier directories winning. `NoDisplay`, `Hidden` and a
//! missing `TryExec` are left out; the type of a file is `scour-thumbs`'s answer.

mod claims;
mod launch;
mod reveal;
pub use claims::*;
pub use launch::*;
pub use reveal::*;

mod pick;
pub use pick::pick_folder;

/// The shell's own calls, not `explorer` with arguments: Rust quotes an
/// argument holding a space whole, `"/select,C:\a b\c"`, which Explorer does
/// not parse — it opened Documents for every such path.
#[cfg(windows)]
mod windows;

#[cfg(test)]
mod tests;
