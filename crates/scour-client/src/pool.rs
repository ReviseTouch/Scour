//! Held connections, for a face that asks from many threads at once.

use std::sync::Mutex;

use scour_core::Result;
use scour_ipc::Client;
use scour_proto::{Request, Response};

/// Connections kept open between calls, up to a depth, and where to open more.
/// One behind a mutex queues callers: in the page one request in forty took
/// 1,056 ms while the rest took two.
#[derive(Debug)]
pub struct Pool {
    addr: String,
    depth: usize,
    idle: Mutex<Vec<Client>>,
}

impl Pool {
    pub fn new(addr: impl Into<String>, depth: usize) -> Pool {
        Pool {
            addr: addr.into(),
            depth,
            idle: Mutex::new(Vec::new()),
        }
    }

    /// Where the service is, for a caller that needs a connection of its own.
    pub fn addr(&self) -> &str {
        &self.addr
    }

    /// One call. A held connection that fails transiently — the service
    /// restarted underneath — is dropped and the call made once more on a new
    /// one. Any other error is the answer: a refused request is not sent twice.
    pub fn call(&self, request: Request) -> Result<Response> {
        let held = self.lock().pop();
        if let Some(mut open) = held {
            match open.call(request.clone()) {
                Err(e) if e.is_transient() => {}
                out => {
                    self.put_back(open);
                    return out;
                }
            }
        }
        let mut fresh = Client::connect(&self.addr)?;
        let out = fresh.call(request);
        if !out.as_ref().is_err_and(|e| e.is_transient()) {
            self.put_back(fresh);
        }
        out
    }

    /// How many connections are held now.
    pub fn held(&self) -> usize {
        self.lock().len()
    }

    /// Hold a connection opened elsewhere, for the next caller.
    pub fn keep(&self, open: Client) {
        self.put_back(open);
    }

    /// A connection that answered goes back for the next caller.
    fn put_back(&self, open: Client) {
        let mut idle = self.lock();
        if idle.len() < self.depth {
            idle.push(open);
        }
    }

    /// Held connections are just sockets: a panic elsewhere leaves them usable.
    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<Client>> {
        self.idle.lock().unwrap_or_else(|p| p.into_inner())
    }
}
