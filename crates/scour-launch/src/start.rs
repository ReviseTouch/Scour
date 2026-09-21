//! Starting one, and waiting for it to answer.

use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use crate::Outcome;

/// How long a fresh service is given to open its socket. Opening a 4.6 M-entry
/// index off a cold cache is the slow case, and it is seconds, not tens.
const PATIENCE: Duration = Duration::from_secs(10);

/// How often the socket is looked at. Cheap: a connect that fails is a syscall.
const BEAT: Duration = Duration::from_millis(200);

/// One attempt to have a service at an address.
pub struct Autostart<'a> {
    addr: &'a str,
    log: PathBuf,
    /// Asked instead of depending on the crate that owns the wire; the faces
    /// pass `scour_ipc::is_running`.
    reachable: &'a (dyn Fn(&str) -> bool + Send + Sync),
    exe: Option<PathBuf>,
    patience: Duration,
    beat: Duration,
}

impl std::fmt::Debug for Autostart<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Autostart")
            .field("addr", &self.addr)
            .field("log", &self.log)
            .field("exe", &self.exe)
            .field("patience", &self.patience)
            .finish()
    }
}

impl<'a> Autostart<'a> {
    /// A service at `addr`, its output appended to `log`.
    pub fn new(
        addr: &'a str,
        log: PathBuf,
        reachable: &'a (dyn Fn(&str) -> bool + Send + Sync),
    ) -> Autostart<'a> {
        Autostart {
            addr,
            log,
            reachable,
            exe: None,
            patience: PATIENCE,
            beat: BEAT,
        }
    }

    /// Wait this long instead of ten seconds.
    pub fn within(mut self, patience: Duration) -> Autostart<'a> {
        self.patience = patience;
        self
    }

    /// Look this often instead of five times a second.
    pub fn every(mut self, beat: Duration) -> Autostart<'a> {
        self.beat = beat;
        self
    }

    /// Run this instead of searching for `scourd`.
    pub fn binary(mut self, exe: PathBuf) -> Autostart<'a> {
        self.exe = Some(exe);
        self
    }

    /// Start one if nothing answers, then wait until something does.
    pub fn run(&self) -> Outcome {
        if (self.reachable)(self.addr) {
            return Outcome::AlreadyRunning;
        }
        let exe = match self.exe.clone().or_else(crate::binary::scourd) {
            Some(exe) => exe,
            None => return Outcome::Failed(self.nowhere()),
        };
        let mut child = match self.spawn(&exe) {
            Ok(child) => child,
            Err(e) => return Outcome::Failed(format!("{} could not be started: {e}", exe.display())),
        };
        self.wait_for(&mut child)
    }

    /// Poll the socket, noticing on the way if the child we started gave up.
    /// **A child that died is not the end of the wait**: a system unit racing
    /// us for the index lock wins it, ours exits, and the unit's instance is
    /// what answers. Its status is kept only to explain a silence at the end.
    fn wait_for(&self, child: &mut Child) -> Outcome {
        let deadline = Instant::now() + self.patience;
        let mut died: Option<String> = None;
        loop {
            if (self.reachable)(self.addr) {
                return Outcome::Started;
            }
            if died.is_none()
                && let Ok(Some(status)) = child.try_wait()
                && !status.success()
            {
                died = Some(match status.code() {
                    Some(code) => code.to_string(),
                    None => "a signal".to_string(),
                });
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            std::thread::sleep(self.beat.min(left));
        }
        if (self.reachable)(self.addr) {
            return Outcome::Started;
        }
        let log = self.log.display();
        Outcome::Failed(match died {
            Some(status) => format!("scourd exited with status {status} (see {log})"),
            None => format!(
                "started but did not answer within {} (see {log})",
                spell(self.patience)
            ),
        })
    }

    /// Detached: a new session, no standard input, output appended to the log.
    /// The face is a program somebody closes; the service has to outlive it.
    fn spawn(&self, exe: &std::path::Path) -> std::io::Result<Child> {
        if let Some(dir) = self.log.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.log)?;
        let mut cmd = Command::new(exe);
        // The address, not the configuration: it is the one thing the face and
        // the service have to agree on, and the face already resolved it.
        cmd.arg("--socket")
            .arg(self.addr)
            .stdin(Stdio::null())
            .stdout(Stdio::from(log.try_clone()?))
            .stderr(Stdio::from(log));
        detach(&mut cmd);
        cmd.spawn()
    }

    fn nowhere(&self) -> String {
        let here = std::env::current_exe()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| "this program".into());
        format!("{} not found next to {here} or on PATH", crate::binary::NAME)
    }
}

/// `10 s`, `400 ms` — a duration as the sentence would say it.
fn spell(d: Duration) -> String {
    if d.subsec_millis() == 0 {
        format!("{} s", d.as_secs())
    } else {
        format!("{} ms", d.as_millis())
    }
}

#[cfg(unix)]
fn detach(cmd: &mut Command) {
    use std::os::unix::process::CommandExt;
    // SAFETY: `setsid` is async-signal-safe, which is the whole requirement on
    // a pre-exec hook. Its failure is not worth reporting: it can only fail
    // when the child is already a session leader, which is the wanted state.
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        })
    };
}

#[cfg(windows)]
fn detach(cmd: &mut Command) {
    use std::os::windows::process::CommandExt;
    /// No console handle inherited, and no console of its own.
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    cmd.creation_flags(DETACHED_PROCESS | CREATE_NO_WINDOW);
}

#[cfg(not(any(unix, windows)))]
fn detach(_cmd: &mut Command) {}

/// Is starting a service allowed at all? `SCOUR_NO_AUTOSTART` set to anything
/// but `0` says no.
pub fn wanted() -> bool {
    match std::env::var("SCOUR_NO_AUTOSTART") {
        Ok(v) => v.is_empty() || v == "0",
        Err(_) => true,
    }
}

/// At most one attempt per process, whichever thread gets there first; the
/// others block here and are handed the same answer.
///
/// Once per process on purpose: a face has several connections and each one
/// notices a dead service separately, and `scourd --shutdown` must not be
/// undone by the next keystroke.
pub fn ensure_once(plan: &Autostart<'_>) -> Outcome {
    static DONE: OnceLock<Outcome> = OnceLock::new();
    DONE.get_or_init(|| {
        if wanted() {
            plan.run()
        } else {
            Outcome::Off
        }
    })
    .clone()
}
