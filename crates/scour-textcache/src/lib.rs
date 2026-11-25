//! Documents' text kept once, so a second search reads none of them. Texts
//! are appended to blocks of a megabyte or more, each compressed whole; a
//! table says where each document's text is, and under which size and time
//! of the file it was taken.

mod block;
mod key;
mod store;
mod table;

pub use key::{Kept, Key};
pub use store::{Stats, Store};
