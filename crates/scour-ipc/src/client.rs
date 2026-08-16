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
    /// A [`Client::stream`] was abandoned part way through.
    ///
    /// The frames nobody read are still coming, so this connection can never
    /// be trusted again — the next request would be answered by the tail of
    /// the last one. Refusing to send is the only safe thing left; the caller
    /// opens a new connection, which costs a `connect` and is what a cancelled
    /// download should cost.
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

    /// Ask, and read the one frame that answers.
    ///
    /// **Refuses a request that answers in pieces**, rather than reading the
    /// first of them. Reading it would leave the rest in the buffer and the
    /// next request on this connection would be answered by the leftovers —
    /// which does not look like a failure, it looks like the wrong file. Use
    /// [`Client::stream`].
    pub fn call(&mut self, request: Request) -> Result<Response> {
        if request.streams() {
            return Err(Error::Config {
                detail: format!("{} answers in pieces; use stream()", request.name()),
            });
        }
        let id = self.send(request)?;
        let reply = self.frame(id)?;
        // Cannot happen for a request that says it does not stream, and it is
        // checked anyway: this is the one failure that produces a wrong answer
        // rather than an error, so it is worth two lines to make it an error.
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

    /// Ask for something that arrives in pieces, and be handed each one.
    ///
    /// `on_piece` is called per frame as it arrives — nothing is collected
    /// here, which is the whole point: an export of this index is a couple of
    /// hundred megabytes and the caller is writing them to a socket or a file
    /// as they come. Returning `false` stops reading and hangs the connection
    /// up, because the service is mid-answer and there is no way to tell it to
    /// stop that does not involve inventing a cancel message; dropping the
    /// connection *is* the cancel, and the service's next write fails.
    ///
    /// The terminating frame is returned. An error frame is returned as `Err`,
    /// so a caller that gets `Ok` knows the answer is whole.
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
                // The caller has stopped caring — its own reader went away, in
                // every case that has one. This connection is now mid-answer
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
        // Requests are sent one at a time on this connection, so a reply
        // carrying a different id means the stream has desynchronised — and
        // continuing from there would answer the wrong question.
        if reply.id != id {
            return Err(Error::Unreachable {
                detail: format!("expected reply {id}, got {}", reply.id),
            });
        }
        Ok(reply)
    }
}
