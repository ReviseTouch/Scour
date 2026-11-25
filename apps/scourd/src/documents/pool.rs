//! The extractor processes: started when first needed, one document at a
//! time each, bounded from outside, started again when one dies or hangs.

use std::io::{BufReader, BufWriter};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::time::{Duration, Instant};

use parking_lot::{Condvar, Mutex};
use scour_extract::wire::{self, Answer};

/// A document taking longer than this is given up and its reader killed.
pub(crate) const PER_FILE: Duration = Duration::from_secs(30);
/// An extractor idle this long is let go: a library keeps what it parsed,
/// and four idle readers held 430 MB between two searches.
const IDLE: Duration = Duration::from_secs(20);
/// Address space an extractor may take.
const MEMORY: u64 = 2 << 30;
/// Text sent back at most, and never more than this many times the input.
const TEXT_CAP: u64 = 256 << 20;
const RATIO: u64 = 100;

struct Worker {
    child: Child,
    input: BufWriter<ChildStdin>,
    answers: Receiver<std::io::Result<Answer>>,
}

pub(crate) struct Pool {
    exe: PathBuf,
    most: usize,
    held: Arc<Held>,
}

/// The idle workers with when each came back, how many there are in all,
/// and whether somebody is watching for idle ones.
struct Held {
    idle: Mutex<Idle>,
    freed: Condvar,
}

struct Idle {
    workers: Vec<(Worker, Instant)>,
    count: usize,
    reaping: bool,
}

impl Pool {
    /// At most `most` processes of the extractor at `exe`.
    pub(crate) fn new(exe: PathBuf, most: usize) -> Pool {
        let idle = Idle {
            workers: Vec::new(),
            count: 0,
            reaping: false,
        };
        let held = Arc::new(Held {
            idle: Mutex::new(idle),
            freed: Condvar::new(),
        });
        Pool {
            exe,
            most: most.max(1),
            held,
        }
    }

    /// The text of a document's `bytes`; `extension` tells `.doc` from `.xls`.
    pub(crate) fn extract(&self, extension: &str, bytes: &[u8]) -> Answer {
        let Some(mut worker) = self.take() else {
            return Answer::Failed("the extractor could not be started".into());
        };
        let cap = cap_for(bytes.len());
        let answer = match wire::write_request(&mut worker.input, extension, cap, bytes) {
            Ok(()) => worker.answers.recv_timeout(PER_FILE),
            Err(e) => Ok(Err(e)),
        };
        match answer {
            Ok(Ok(mut answer)) => {
                self.give_back(worker);
                // Held to the bound here too: the extractor is not trusted to.
                if let Answer::Words(words) = &mut answer {
                    cut(words, cap as usize);
                }
                answer
            }
            // Dead, hung or speaking nonsense: it goes, and this document failed.
            Ok(Err(_)) | Err(RecvTimeoutError::Disconnected) => {
                self.lose(worker);
                Answer::Failed("the extractor stopped".into())
            }
            Err(RecvTimeoutError::Timeout) => {
                self.lose(worker);
                Answer::Failed("the extractor took too long".into())
            }
        }
    }

    fn take(&self) -> Option<Worker> {
        let mut idle = self.held.idle.lock();
        loop {
            if let Some((w, _)) = idle.workers.pop() {
                return Some(w);
            }
            if idle.count < self.most {
                idle.count += 1;
                drop(idle);
                let started = start(&self.exe);
                if started.is_none() {
                    self.held.idle.lock().count -= 1;
                }
                return started;
            }
            self.held.freed.wait(&mut idle);
        }
    }

    fn give_back(&self, worker: Worker) {
        let mut idle = self.held.idle.lock();
        idle.workers.push((worker, Instant::now()));
        if !idle.reaping {
            idle.reaping = true;
            let held = Arc::clone(&self.held);
            let _ = std::thread::Builder::new()
                .name("scourd-extractor-reaper".into())
                .spawn(move || reap(&held));
        }
        drop(idle);
        self.held.freed.notify_one();
    }

    fn lose(&self, worker: Worker) {
        end(worker);
        self.held.idle.lock().count -= 1;
        self.held.freed.notify_one();
    }
}

/// Every idle worker past [`IDLE`] is ended; the thread itself ends when
/// none is left, so a service with nothing to read runs no thread for it.
fn reap(held: &Held) {
    loop {
        std::thread::sleep(IDLE / 2);
        let mut idle = held.idle.lock();
        let (old, young): (Vec<_>, Vec<_>) = std::mem::take(&mut idle.workers)
            .into_iter()
            .partition(|(_, back)| back.elapsed() >= IDLE);
        idle.workers = young;
        idle.count -= old.len();
        let done = idle.workers.is_empty();
        if done {
            idle.reaping = false;
        }
        drop(idle);
        for (w, _) in old {
            end(w);
        }
        if done {
            return;
        }
    }
}

fn end(mut worker: Worker) {
    let _ = worker.child.kill();
    let _ = worker.child.wait();
}

impl Drop for Pool {
    fn drop(&mut self) {
        for (w, _) in self.held.idle.lock().workers.drain(..) {
            end(w);
        }
    }
}

/// Text allowed back for a document of `len` bytes: a hundred times its
/// size, at least a megabyte, at most [`TEXT_CAP`].
fn cap_for(len: usize) -> u64 {
    TEXT_CAP.min((len as u64).saturating_mul(RATIO).max(1 << 20))
}

fn cut(text: &mut String, cap: usize) {
    if text.len() > cap {
        let mut at = cap;
        while !text.is_char_boundary(at) {
            at -= 1;
        }
        text.truncate(at);
    }
}

fn start(exe: &PathBuf) -> Option<Worker> {
    let mut command = Command::new(exe);
    // Nothing of the service's environment: no variable may switch a
    // library's feature on, and no path of the person's is passed along.
    command
        .env_clear()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    bound(&mut command);
    let mut child = command.spawn().ok()?;
    #[cfg(windows)]
    job::confine(&child);
    let input = BufWriter::new(child.stdin.take()?);
    let mut output = BufReader::new(child.stdout.take()?);
    let (tell, answers) = channel();
    std::thread::Builder::new()
        .name("scourd-extractor".into())
        .spawn(move || {
            loop {
                let answer = wire::read_answer(&mut output);
                let end = answer.is_err();
                if tell.send(answer).is_err() || end {
                    break;
                }
            }
        })
        .ok()?;
    Some(Worker {
        child,
        input,
        answers,
    })
}

#[cfg(unix)]
fn bound(command: &mut Command) {
    use std::os::unix::process::CommandExt as _;
    // SAFETY: between fork and exec only async-signal-safe calls are made.
    unsafe {
        command.pre_exec(|| {
            let limit = libc::rlimit {
                rlim_cur: MEMORY as _,
                rlim_max: MEMORY as _,
            };
            libc::setrlimit(libc::RLIMIT_AS, &limit);
            libc::setpriority(libc::PRIO_PROCESS, 0, 10);
            Ok(())
        });
    }
}

#[cfg(not(unix))]
fn bound(_command: &mut Command) {}

/// A job object that holds the extractor to its memory and kills it with
/// the service.
#[cfg(windows)]
mod job {
    use std::os::windows::io::AsRawHandle as _;
    use std::process::Child;

    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        JOB_OBJECT_LIMIT_PROCESS_MEMORY, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JobObjectExtendedLimitInformation, SetInformationJobObject,
    };

    pub(super) fn confine(child: &Child) {
        // SAFETY: a fresh job, filled in and handed the child; the job's handle
        // is kept for the life of the service, so the limit outlives the call.
        unsafe {
            let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            if job.is_null() {
                return;
            }
            let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            info.BasicLimitInformation.LimitFlags =
                JOB_OBJECT_LIMIT_PROCESS_MEMORY | JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            info.ProcessMemoryLimit = super::MEMORY as usize;
            SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                &info as *const _ as *const _,
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            );
            AssignProcessToJobObject(job, child.as_raw_handle());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_back_is_held_to_its_ratio_and_ceiling() {
        assert_eq!(cap_for(10), 1 << 20);
        assert_eq!(cap_for(100 << 10), 100 * (100 << 10));
        assert_eq!(cap_for(64 << 20), TEXT_CAP);
        let mut s = "ğ".repeat(10);
        cut(&mut s, 5);
        assert_eq!(s, "ğğ");
    }

    #[test]
    fn a_missing_extractor_fails_the_document_not_the_service() {
        let pool = Pool::new("/nonexistent/scour-extractor".into(), 2);
        assert!(matches!(pool.extract("pdf", b"%PDF"), Answer::Failed(_)));
        assert!(matches!(pool.extract("pdf", b"%PDF"), Answer::Failed(_)));
    }
}
