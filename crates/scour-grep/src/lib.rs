//! Which of these files hold this text. The caller says which files — an
//! index is good at that — and this reads them: each disk on as many threads
//! as it can bear, the disks side by side, and never more than was allowed.

mod budget;
mod class;
mod excerpt;
mod expr;
mod ground;
mod order;
mod pattern;
mod quiet;
mod read;
mod scan;
mod texts;
mod zip;

pub use budget::Budget;
pub use class::{Class, is_disk_image};
pub use excerpt::Hit;
pub use expr::Expr;
pub use ground::{Ground, readers};
pub use order::Order;
pub use pattern::{BadPattern, Needle, Pattern};
pub use scan::{Enter, File, Options, Outcome, Scanner, Skip};
pub use surefoot::Medium;
pub use texts::{Doc, DocText, Texts};
