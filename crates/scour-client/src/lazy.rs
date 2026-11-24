//! One connection, opened when first needed and again after it is lost; the
//! service started when nothing answers.

use std::path::{Path, PathBuf};

use scour_ipc::Client;

/// Have a service at `addr`, starting one if nobody else has: once for the
/// whole program, whichever thread arrives first; the others wait here.
pub fn boot(addr: &str, log: &Path) -> scour_launch::Outcome {
    scour_launch::ensure_once(&scour_launch::Autostart::new(
        addr,
        log.to_path_buf(),
        &scour_ipc::is_running,
    ))
}

/// A connection that is opened on demand and remembers whether it was down,
/// so a face says "lost" and "back" once each rather than at every call.
#[derive(Debug)]
pub struct Lazy {
    addr: String,
    log: PathBuf,
    client: Option<Client>,
    down: bool,
}

/// Why no connection could be had, and whether this is the first failure
/// since it was last up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Down {
    pub why: String,
    pub first: bool,
}

impl Lazy {
    /// `log` is where a service started from here writes.
    pub fn new(addr: impl Into<String>, log: impl Into<PathBuf>) -> Lazy {
        Lazy {
            addr: addr.into(),
            log: log.into(),
            client: None,
            down: false,
        }
    }

    /// Open the connection if there is none, starting a service when the first
    /// try fails. `Ok(true)` when it is up again after being down. The reason
    /// on failure is the last thing worth saying: why the start failed, or where
    /// the service wrote when it started and went away — "no such file" alone
    /// hides both.
    pub fn open(&mut self) -> Result<bool, Down> {
        if self.client.is_some() {
            return Ok(false);
        }
        let opened = Client::connect(&self.addr).or_else(|e| {
            let booted = boot(&self.addr, &self.log);
            Client::connect(&self.addr).map_err(|_| match booted.reason() {
                Some(why) => why.to_string(),
                None if booted.running() => format!("{e} (see {})", self.log.display()),
                None => e.to_string(),
            })
        });
        match opened {
            Ok(c) => {
                self.client = Some(c);
                Ok(std::mem::replace(&mut self.down, false))
            }
            Err(why) => Err(Down {
                why,
                first: !std::mem::replace(&mut self.down, true),
            }),
        }
    }

    /// The open connection, if there is one.
    pub fn client(&mut self) -> Option<&mut Client> {
        self.client.as_mut()
    }

    /// A call failed on it: the next [`Lazy::open`] makes a new one.
    pub fn lost(&mut self) {
        self.client = None;
    }
}
