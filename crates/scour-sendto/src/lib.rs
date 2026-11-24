//! "Send to": the places a file is commonly sent from a menu — a link on the
//! desktop, a mail, a Bluetooth device, an archive beside it, a removable
//! drive — each offered only where this machine can do it. What is slow is
//! started and left running: the file manager and the index see the result.

mod error;
mod send;
mod targets;
pub use error::*;
pub use send::*;
pub use targets::*;

#[cfg(test)]
mod tests;
