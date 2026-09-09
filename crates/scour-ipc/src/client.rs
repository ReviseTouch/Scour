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

/// A connection to the service, kept open across requests: a search box sends one
/// per keystroke, and a `connect` each time is latency inside every one.
pub struct Client {
    write: Stream,
    read: BufReader<Stream>,
    next_id: u64,
    /// A [`Client::stream`] was abandoned part way through, so unread frames are
    /// still coming and the next request would be answered by the last one's tail.
    /// The caller opens a new connection.
    stopped: bool,
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
            stopped: false,
        })
    }

    /// Ask, and read the one frame that answers. Refuses a request that answers in
    /// pieces: reading the first would leave the rest to answer the next request,
    /// which looks like the wrong file rather than an error. Use [`Client::stream`].
    pub fn call(&mut self, request: Request) -> Result<Response> {
        if request.streams() {
            return Err(Error::Config {
                detail: format!("{} answers in pieces; use stream()", request.name()),
            });
        }
        let id = self.send(request)?;
        let reply = self.frame(id)?;
        // Checked anyway: this is the one failure that produces a wrong answer
        // rather than an error.
        if reply.more {
            return Err(Error::Unreachable {
                detail: "the service answered in pieces where one frame was expected".into(),
            });
        }
        match reply.outcome {
            Outcome::Ok(r) => Ok(r),
            Outcome::Error(e) => Err(e),
        }
    }

    /// Ask for something that arrives in pieces and be handed each one; nothing is
    /// collected here, and returning `false` hangs the connection up, which is the
    /// cancel. The terminating frame is returned, an error frame as `Err`.
    pub fn stream(
        &mut self,
        request: Request,
        mut on_piece: impl FnMut(Response) -> bool,
    ) -> Result<Response> {
        let id = self.send(request)?;
        loop {
            let reply = self.frame(id)?;
            if !reply.more {
                return match reply.outcome {
                    Outcome::Ok(r) => Ok(r),
                    Outcome::Error(e) => Err(e),
                };
            }
            let piece = match reply.outcome {
                Outcome::Ok(r) => r,
                // A failure mid-run is the end of the run whatever `more` says.
                Outcome::Error(e) => return Err(e),
            };
            if !on_piece(piece) {
                // The caller stopped reading. This connection is now mid-answer
                // and unusable; see `Client::stopped`.
                self.stopped = true;
                return Err(Error::Unreachable {
                    detail: "the reader stopped part way through".into(),
                });
            }
        }
    }

    fn send(&mut self, request: Request) -> Result<u64> {
        if self.stopped {
            return Err(Error::Unreachable {
                detail: "this connection was abandoned mid-answer".into(),
            });
        }
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
        Ok(id)
    }

    /// One frame, checked against the id it should carry.
    fn frame(&mut self, id: u64) -> Result<Reply> {
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
        // One request at a time on this connection, so a different id means the
        // stream has desynchronised.
        if reply.id != id {
            return Err(Error::Unreachable {
                detail: format!("expected reply {id}, got {}", reply.id),
            });
        }
        Ok(reply)
    }
}
