//! Scour, as something a model can use.
//!
//! A thin mapping onto [`scour_proto`] and nothing more — the same requests the
//! command line sends, over the same socket. That is what the protocol crate
//! was for: adding this frontend meant writing argument structs and a renderer,
//! not a second implementation of anything.
//!
//! What makes this worth having is not `search` — a model can already run
//! `find`. It is that the answers are **instant and bounded**. `scour_tree`
//! lists a directory of a million files in the same time as one of ten, and
//! says how many it left out. `scour_count` answers "how many Rust files are
//! there" without listing them. `scour_facets` answers "what is in here"
//! without reading anything. Those are the questions that make exploring a
//! large filesystem affordable in a context window.
//!
//! Everything here is read-only. Rescanning, maintenance and shutdown exist in
//! the protocol and are deliberately not exposed: a model exploring a
//! filesystem has no business rebuilding an index.

mod render;
mod tools;

use anyhow::Result;
use rmcp::ServiceExt;
use rmcp::transport::stdio;

#[tokio::main]
async fn main() -> Result<()> {
    // stdout belongs to the protocol. Anything said to a human goes to stderr,
    // or it corrupts the stream.
    let addr = std::env::args()
        .skip_while(|a| a != "--socket")
        .nth(1)
        .unwrap_or_else(|| scour_config::Config::load_or_default().0.socket());

    // Said once, for whoever reads the client's server log — and said either
    // way, because "not yet" is not a reason to stop.
    let scour = tools::Scour::new(&addr);
    if scour_ipc::is_running(&addr) {
        eprintln!("scour-mcp: {addr}");
    } else {
        eprintln!("scour-mcp: nothing listening on {addr} yet; will connect when asked");
    }

    let service = scour.serve(stdio()).await?;
    service.waiting().await?;
    Ok(())
}
