//! Getting [`scour_proto`] from one process to another.
//!
//! A local socket — a Unix domain socket, or a named pipe on Windows — rather
//! than a loopback TCP port. The port is simpler to write and worse in every
//! way that matters: it raises a firewall prompt the first time on two of the
//! three platforms, it is reachable by anything else running on the machine,
//! and it has no notion of who is asking. A socket in the user's own runtime
//! directory is protected by the filesystem's permissions, which is exactly the
//! boundary this needs.
//!
//! The framing is newline-delimited JSON. One message per line, request and
//! reply matched by id. It is not the most compact choice and that is fine:
//! the payload is a page of results, the cost is dominated by the search, and
//! being able to talk to the service with `nc` while debugging is worth more
//! than the bytes.

mod client;
mod server;

pub use client::{Client, is_running};
pub use server::Server;
