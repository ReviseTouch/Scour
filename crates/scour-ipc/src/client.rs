//! Asking.

use std::io::{BufRead, BufReader, Write};

use interprocess::TryClone;
use interprocess::local_socket::Stream;
use interprocess::local_socket::traits::Stream as _;
use scour_core::{Error, Result};
use scour_proto::{Call, Outcome, Reply, Request, Response};

/// Is a service listening here?
pub fn is_running(addr: &str) -> bool {
    crate::server::name(addr).is_ok_and(|n| Stream::connect(n).is_ok())
}

/// A connection to the service.
///
/// Kept open across requests. A search box sends one per keystroke, and paying
/// for a connection each time would put the connection cost inside the latency
/// this whole project exists to minimise.
pub struct Client {
    write: Stream,
    read: BufReader<Stream>,
    next_id: u64,
}

impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Client")
            .field("next_id", &self.next_id)
            .finish()
    }
}

impl Client {
    pub fn connect(addr: &str) -> Result<Client> {
        let conn = Stream::connect(crate::server::name(addr)?).map_err(|e| Error::Unreachable {
            detail: format!("{addr}: {e}"),
        })?;
        let read = BufReader::new(conn.try_clone().map_err(|e| Error::Unreachable {
            detail: e.to_string(),
        })?);
        Ok(Client {
            write: conn,
            read,
            next_id: 1,
        })
    }

    pub fn call(&mut self, request: Request) -> Result<Response> {
        let id = self.next_id;
        self.next_id += 1;
        let mut text = serde_json::to_string(&Call { id, request }).map_err(|e| Error::Io {
            detail: e.to_string(),
        })?;
        text.push('\n');
        self.write
            .write_all(text.as_bytes())
            .and_then(|()| self.write.flush())
            .map_err(|e| Error::Unreachable {
                detail: e.to_string(),
            })?;

        let mut line = String::new();
        let n = self
            .read
            .read_line(&mut line)
            .map_err(|e| Error::Unreachable {
                detail: e.to_string(),
            })?;
        if n == 0 {
            return Err(Error::Unreachable {
                detail: "the service closed the connection".into(),
            });
        }
        let reply: Reply = serde_json::from_str(line.trim()).map_err(|e| Error::Io {
            detail: e.to_string(),
        })?;
        // Requests are sent one at a time on this connection, so a reply
        // carrying a different id means the stream has desynchronised — and
        // continuing from there would answer the wrong question.
        if reply.id != id {
            return Err(Error::Unreachable {
                detail: format!("expected reply {id}, got {}", reply.id),
            });
        }
        match reply.outcome {
            Outcome::Ok(r) => Ok(r),
            Outcome::Error(e) => Err(e),
        }
    }
}
