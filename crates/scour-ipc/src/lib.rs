//! Getting [`scour_proto`] from one process to another over a local socket, in
//! newline-delimited JSON: one message per line, matched by id. An answer may
//! arrive in pieces — frames sharing an id, all but the last carrying
//! [`scour_proto::Reply::more`]; [`Client::call`] refuses those and
//! [`Client::stream`] reads them. The socket buffer is the backpressure.

mod client;
mod server;

pub use client::{Client, is_running};
pub use server::{Emit, Server};
