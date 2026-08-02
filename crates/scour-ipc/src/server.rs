//! Listening.

use std::io::{BufRead, BufReader, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use interprocess::TryClone;
use interprocess::local_socket::traits::ListenerExt;
use interprocess::local_socket::{ListenerOptions, Stream};
use scour_core::{Error, Result};
use scour_proto::{Call, Outcome, Reply, Request};

/// Turn an address into whatever the platform's socket layer calls a name.
pub(crate) fn name(addr: &str) -> Result<interprocess::local_socket::Name<'_>> {
    use interprocess::local_socket::{GenericFilePath, GenericNamespaced, ToFsName, ToNsName};
    let n = if cfg!(windows) {
        addr.rsplit('\\')
            .next()
            .unwrap_or(addr)
            .to_ns_name::<GenericNamespaced>()
    } else {
        addr.to_fs_name::<GenericFilePath>()
    };
    n.map_err(|e| Error::Unreachable {
        detail: e.to_string(),
    })
}

pub struct Server {
    listener: interprocess::local_socket::Listener,
    addr: String,
}

impl std::fmt::Debug for Server {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Server").field("addr", &self.addr).finish()
    }
}

impl Server {
    /// Listen, clearing a socket left behind by a process that is gone.
    ///
    /// A crash leaves the file on disk on unix, and refusing to start because
    /// of it would mean the service never comes back without manual help. So a
    /// bind failure is checked by trying to *connect*: something answering
    /// means a real instance is running and this one should stop; nothing
    /// answering means the file is a corpse.
    pub fn bind(addr: &str) -> Result<Server> {
        if let Some(dir) = std::path::Path::new(addr).parent()
            && !cfg!(windows)
        {
            let _ = std::fs::create_dir_all(dir);
        }
        match ListenerOptions::new().name(name(addr)?).create_sync() {
            Ok(listener) => Ok(Server {
                listener,
                addr: addr.to_owned(),
            }),
            Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
                if crate::is_running(addr) {
                    return Err(Error::Unreachable {
                        detail: "another Scour service is already listening".into(),
                    });
                }
                let _ = std::fs::remove_file(addr);
                let listener = ListenerOptions::new()
                    .name(name(addr)?)
                    .create_sync()
                    .map_err(|e| Error::io(&e, addr))?;
                Ok(Server {
                    listener,
                    addr: addr.to_owned(),
                })
            }
            Err(e) => Err(Error::io(&e, addr)),
        }
    }

    pub fn addr(&self) -> &str {
        &self.addr
    }

    /// Serve until `stop` is set.
    ///
    /// One thread per connection. A client holds its connection open for a
    /// whole session — the search box sends one request per keystroke — so the
    /// count is the number of open windows, not the number of requests.
    pub fn serve<H>(self, handler: H, stop: Arc<AtomicBool>)
    where
        H: Fn(Request) -> Outcome + Send + Sync + 'static,
    {
        let handler = Arc::new(handler);
        for conn in self.listener.incoming() {
            if stop.load(Ordering::Relaxed) {
                break;
            }
            let Ok(conn) = conn else { continue };
            let handler = Arc::clone(&handler);
            let stop = Arc::clone(&stop);
            let _ = std::thread::Builder::new()
                .name("scour-conn".into())
                .spawn(move || session(conn, handler.as_ref(), &stop));
        }
        if !cfg!(windows) {
            let _ = std::fs::remove_file(&self.addr);
        }
    }
}

fn session<H>(conn: Stream, handler: &H, stop: &AtomicBool)
where
    H: Fn(Request) -> Outcome,
{
    let mut out = match conn.try_clone() {
        Ok(c) => c,
        Err(_) => return,
    };
    let reader = BufReader::new(conn);
    for line in reader.lines() {
        if stop.load(Ordering::Relaxed) {
            return;
        }
        let Ok(line) = line else { return };
        if line.trim().is_empty() {
            continue;
        }
        // A malformed line is answered, not dropped. A client that sent
        // nonsense should be told, and a client waiting for a reply that never
        // comes is the worst failure this layer can produce.
        let reply = match serde_json::from_str::<Call>(&line) {
            Ok(call) => Reply {
                id: call.id,
                outcome: handler(call.request),
            },
            Err(e) => Reply {
                id: 0,
                outcome: Outcome::Error(Error::Config {
                    detail: e.to_string(),
                }),
            },
        };
        let Ok(mut text) = serde_json::to_string(&reply) else {
            return;
        };
        text.push('\n');
        if out.write_all(text.as_bytes()).is_err() || out.flush().is_err() {
            return;
        }
    }
}
