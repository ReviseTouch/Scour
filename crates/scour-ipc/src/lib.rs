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
//!
//! ## An answer may arrive in pieces
//!
//! This was request/response only, and one request needed more: an export is
//! the whole matching set — 2.24 M rows here — and every shape that fitted the
//! old transport had to either hold all of it or ask for it a page at a time,
//! and paging is quadratic because a page costs what it takes to walk to its
//! offset.
//!
//! **What that needed turned out to be almost nothing.** The framing was
//! already one object per line with an id on it, so a run of frames sharing an
//! id is a legal thing to write and always was; what was missing was a way to
//! say *this is not the last one*. That is [`scour_proto::Reply::more`], one
//! optional boolean, defaulted and omitted when false — so every frame that
//! existed before is byte-identical now.
//!
//! Two things follow, and both are here rather than in the caller:
//!
//! * The handler is given an [`Emit`] to write pieces into. It ignores it for
//!   everything except an export, and the reply it returns is the last frame
//!   either way.
//! * [`Client::call`] **refuses** a request that answers in pieces rather than
//!   reading the first one. Reading it would leave the rest in the buffer, and
//!   the next request on that connection would be answered by the leftovers:
//!   a desynchronised stream reports the wrong file rather than an error, and
//!   that is the failure this layer can least afford. [`Client::stream`] is
//!   the one that reads them.
//!
//! What was **not** needed: a second socket, a length-prefixed framing, a
//! request id per chunk, or an async runtime. The thread that serves the
//! connection writes the pieces as it produces them and the kernel's socket
//! buffer is the backpressure — a reader that stops reading blocks the writer,
//! and a reader that goes away fails its next write, which is how a cancelled
//! download is noticed.

mod client;
mod server;

pub use client::{Client, is_running};
pub use server::{Emit, Server};
