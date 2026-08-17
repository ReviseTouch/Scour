//! The desktop's thumbnail cache: what is in it, who fills it, and how to ask.
//!
//! **Scour decodes nothing and invents nothing.** Every picture here was made
//! by a program the machine declared for the purpose, in the place the
//! freedesktop thumbnail managing standard says to put it, with the metadata
//! that standard requires. The result is shared: a picture Scour asked for is
//! one Files and Loupe find already made, and the reverse.
//!
//! Four pieces, in the order a request moves through them:
//!
//! * [`cache`] — where a picture lives and what its name means.
//! * [`known`] — what this machine declares it can draw, and with what.
//! * [`make`] — running that command, bounded, and putting the result in place.
//! * [`png`] — the two text chunks without which a thumbnail is invalid.
//!
//! ## Not a Windows story yet
//!
//! `.thumbnailer` files and `$XDG_CACHE_HOME/thumbnails` are a Unix desktop's
//! contract. This compiles everywhere and on a machine with neither it finds
//! nothing, so [`make::can_make`] answers false and nothing is ever asked for
//! — which is exactly the behaviour there was before any of this. Windows has
//! its own thumbnail cache behind `IThumbnailProvider`, and reaching it is a
//! COM story rather than a process one; `docs/ROADMAP.md` says so.

pub mod cache;
pub mod known;
pub mod make;
mod png;

pub use make::{Made, Maker, Wanted, can_make};
