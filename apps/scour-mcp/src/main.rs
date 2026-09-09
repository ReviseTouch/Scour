//! Scour, as something a model can use: a thin mapping onto [`scour_proto`],
//! sending the same requests the command line does over the same socket. The
//! value is that answers are instant and bounded — `scour_tree` lists a
//! million-file directory in the time of a ten-file one and says what it left
//! out. Read-only: rescan, maintenance and shutdown are not exposed.

mod render;
mod tools;

use anyhow::Result;
use rmcp::ServiceExt;
use rmcp::transport::stdio;

#[tokio::main]
async fn main() -> Result<()> {
    // stdout belongs to the protocol; anything for a person goes to stderr.
    let addr = std::env::args()
        .skip_while(|a| a != "--socket")
        .nth(1)
        .unwrap_or_else(|| scour_config::Config::load_or_default().0.socket());

    // Said once either way: "not yet" is not a reason to stop.
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
