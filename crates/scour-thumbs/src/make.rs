//! Asking the desktop for a picture it has not made yet.
//!
//! The instruction this was written to, in the owner's words: whatever the
//! system preview is, whatever *it* does — have that produce the picture. We
//! do not go into this business ourselves.
//!
//! So nothing here decodes an image, resizes one, or knows what a video is.
//! What it does is run a command the machine declared, put the standard's two
//! text chunks on what came back, and move it into place.
//!
//! ## The three things this is careful about
//!
//! **A bound that can be named.** [`Maker::AT_ONCE`] processes across the
//! whole machine, and it is one number because there is one [`Maker`]. That is
//! the argument for this living in the service: a bridge bounding itself to
//! four, a window bounding itself to four and a terminal bounding itself to
//! four is a machine running twelve video decoders, and none of the three is
//! wrong on its own.
//!
//! **Nothing that grows with a session.** No queue and no memo. What has been
//! tried is on disk — the cache and the failure directory — and what is in
//! flight is bounded by the permits. A caller that asks for the same file
//! twice pays a `stat` the second time.
//!
//! **Never in front of anything.** This blocks, for seconds, and the caller is
//! expected to have arranged for that to be nobody's problem: in `scourd` it
//! arrives on its own connection thread, and in the page it is asked for after
//! scrolling stops and never during a search.

use std::path::Path;
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

/// One file somebody wants a picture of.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Wanted {
    pub path: String,
    /// The original's modification time, which becomes `Thumb::MTime`.
    ///
    /// Taken from the caller because the caller has already `stat`ed it — in
    /// `scourd` that is the same `stat` that fences the path against the
    /// index. Reading it again here would be a second syscall and a window in
    /// which the two disagree.
    pub mtime: i64,
}

/// What came of asking.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Made {
    /// The paths that have a picture now, whether this made it or found it.
    pub ready: Vec<String>,
    /// How many processes were actually started.
    ///
    /// **Carried back rather than only logged**, because it is the number the
    /// design has to be judged on and a claim about it is otherwise
    /// unfalsifiable: a grid of twenty thousand files coming into view is
    /// meant to start a few dozen processes, not twenty thousand, and this is
    /// what lets a window say so out loud.
    pub ran: usize,
}

/// The one thing on the machine allowed to start thumbnailers.
#[derive(Debug)]
pub struct Maker {
    free: Mutex<usize>,
    freed: Condvar,
}

impl Default for Maker {
    fn default() -> Self {
        Maker::new(Maker::AT_ONCE)
    }
}

impl Maker {
    /// How many thumbnailers may run at once, on the whole machine.
    ///
    /// **Four.** These are separate processes doing image and video decoding,
    /// and the machine they run on is also answering searches for the person
    /// who is waiting for them. Four keeps a grid filling visibly while
    /// leaving most of a desktop's cores to the desktop; they are `nice`d on
    /// top of that, so the four are what is left over rather than what is
    /// taken. It is not a preference because there is nothing to prefer: a
    /// larger number finishes a screenful no sooner — the screen is full
    /// either way after the first few — and a smaller one is visible.
    pub const AT_ONCE: usize = 4;

    /// The most files one call will look at.
    ///
    /// A screenful of large tiles is about forty; a screenful of small ones on
    /// a tall display is about a hundred and eighty. This is deliberately
    /// **less** than a screenful: a caller that asks again when the first
    /// answer lands fills the screen in a few rounds, and a caller that has
    /// scrolled away in the meantime never pays for the tiles it left. The
    /// cap is here as well as in the page because a frontend is not a fence.
    pub const BATCH: usize = 32;

    /// How long one thumbnailer is given before it is killed.
    ///
    /// A hung thumbnailer holds a permit, and four hung ones stop every
    /// picture on the machine. Twenty seconds is long enough for a large video
    /// on a cold cache and short enough that four of them are a pause rather
    /// than an outage. A file that times out is recorded as failed, so it
    /// costs twenty seconds once rather than every time it scrolls past.
    pub const PATIENCE: Duration = Duration::from_secs(20);

    pub fn new(at_once: usize) -> Maker {
        Maker {
            free: Mutex::new(at_once.max(1)),
            freed: Condvar::new(),
        }
    }

    /// Make what can be made, and say what is ready.
    ///
    /// Blocks until the batch is done. Everything that is already in the cache
    /// or already recorded as failed is answered without starting anything.
    pub fn make(&self, wanted: &[Wanted]) -> Made {
        let mut ready = Vec::new();
        let mut todo = Vec::new();
        for it in wanted.iter().take(Maker::BATCH) {
            if crate::cache::existing(&it.path).is_some() {
                ready.push(it.path.clone());
            } else if !crate::cache::has_failed(&it.path, it.mtime) && can_make(&it.path) {
                todo.push(it);
            }
        }
        if todo.is_empty() {
            return Made { ready, ran: 0 };
        }
        let done: Mutex<Vec<String>> = Mutex::new(Vec::new());
        std::thread::scope(|scope| {
            for it in &todo {
                scope.spawn(|| {
                    let _permit = self.permit();
                    if attempt(it) {
                        done.lock()
                            .unwrap_or_else(|p| p.into_inner())
                            .push(it.path.clone());
                    }
                });
            }
        });
        ready.extend(done.into_inner().unwrap_or_else(|p| p.into_inner()));
        Made {
            ready,
            ran: todo.len(),
        }
    }

    fn permit(&self) -> Permit<'_> {
        let mut free = self.free.lock().unwrap_or_else(|p| p.into_inner());
        while *free == 0 {
            free = self.freed.wait(free).unwrap_or_else(|p| p.into_inner());
        }
        *free -= 1;
        Permit { maker: self }
    }
}

struct Permit<'a> {
    maker: &'a Maker,
}

impl Drop for Permit<'_> {
    fn drop(&mut self) {
        let mut free = self.maker.free.lock().unwrap_or_else(|p| p.into_inner());
        *free += 1;
        self.maker.freed.notify_one();
    }
}

/// Is there a thumbnailer for this name at all? No I/O.
pub fn can_make(path: &str) -> bool {
    let name = path.rsplit(['/', '\\']).next().unwrap_or(path);
    crate::known::known().can(name)
}

/// Run the thumbnailer, stamp what came back, and put it where it goes.
///
/// True when there is a picture at the end of it. Every other outcome — no
/// command, a process that failed, a process that wrote nothing, a process
/// that wrote something that is not a PNG — records a failure and returns
/// false, because they are all the same thing from the caller's side: do not
/// ask again for this version of this file.
fn attempt(it: &Wanted) -> bool {
    let name = it.path.rsplit(['/', '\\']).next().unwrap_or(&it.path);
    let Some(command) = known_command(name) else {
        record_failure(it);
        return false;
    };
    let destination = crate::cache::destination(&it.path);
    let Some(folder) = destination.parent() else {
        return false;
    };
    if make_private_dir(folder).is_err() {
        return false;
    }
    let temp = folder.join(temp_name());
    let made = run(&command, &it.path, &temp, Maker::PATIENCE) && install(&temp, &destination, it);
    // The thumbnailer's own output is removed whether or not it was any use;
    // what is left behind otherwise is a cache directory slowly filling with
    // half-written pictures nothing will ever look at.
    let _ = std::fs::remove_file(&temp);
    if !made {
        record_failure(it);
    }
    made
}

fn known_command(name: &str) -> Option<Vec<String>> {
    let known = crate::known::known();
    let mime = known.mime_of(name)?;
    known.command_for(mime).map(<[String]>::to_vec)
}

/// The standard's substitutions, and the machine's own `nice`.
///
/// `patience` is a parameter rather than [`Maker::PATIENCE`] read directly so
/// that the killing can be tested: a test that has to wait twenty seconds to
/// find out whether a hung thumbnailer is killed is a test that gets deleted.
fn run(command: &[String], input: &str, output: &Path, patience: Duration) -> bool {
    let uri = crate::cache::uri_of(input);
    let size = crate::cache::ASKED.to_string();
    let fill = |word: &str| -> String {
        let mut out = String::with_capacity(word.len());
        let mut chars = word.chars();
        while let Some(c) = chars.next() {
            if c != '%' {
                out.push(c);
                continue;
            }
            match chars.next() {
                Some('i') => out.push_str(input),
                Some('u') => out.push_str(&uri),
                Some('o') => out.push_str(&output.to_string_lossy()),
                Some('s') => out.push_str(&size),
                Some('%') => out.push('%'),
                Some(other) => {
                    out.push('%');
                    out.push(other);
                }
                None => out.push('%'),
            }
        }
        out
    };

    let words: Vec<String> = command.iter().map(|w| fill(w)).collect();
    // **`nice` rather than `libc::nice` through `pre_exec`.**
    //
    // The five lines of `unsafe` were the alternative, and this crate has none
    // and is not the place to acquire some for a priority change. The cost is
    // one extra `fork` per thumbnail, against a decode measured in hundreds of
    // milliseconds; it does not show up. When `nice` is not on the machine the
    // command simply runs at full priority, which is worse than niced and much
    // better than not running.
    let mut process = match nice() {
        Some(nice) => {
            let mut c = std::process::Command::new(nice);
            c.arg("-n").arg("10").args(&words);
            c
        }
        None => {
            let mut c = std::process::Command::new(&words[0]);
            c.args(&words[1..]);
            c
        }
    };
    // A thumbnailer that talks is not talking to us, and the service's log is
    // not the place for a video decoder's opinion of a broken file.
    process
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());

    let Ok(mut child) = process.spawn() else {
        return false;
    };
    let began = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) => {}
            Err(_) => return false,
        }
        if began.elapsed() > patience {
            let _ = child.kill();
            let _ = child.wait();
            return false;
        }
        // Polling, because `std` has no wait-with-timeout and the alternative
        // is a signal handler or a second thread per child. Fifty milliseconds
        // is four hundred wakeups across the whole patience window, which is
        // nothing beside what the child is doing.
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn nice() -> Option<&'static Path> {
    static NICE: std::sync::LazyLock<Option<std::path::PathBuf>> = std::sync::LazyLock::new(|| {
        let path = std::env::var_os("PATH")?;
        std::env::split_paths(&path)
            .map(|d| d.join("nice"))
            .find(|p| p.is_file())
    });
    NICE.as_deref()
}

/// Put the standard's metadata on the picture and move it into place.
///
/// **The rename is the point.** A reader that opens a half-written thumbnail
/// gets a broken picture and caches the fact; a rename is atomic, so the file
/// either is not there or is finished.
fn install(temp: &Path, destination: &Path, it: &Wanted) -> bool {
    let Ok(raw) = std::fs::read(temp) else {
        return false;
    };
    let Some(stamped) = crate::png::with_text(
        &raw,
        &[
            ("Thumb::URI", crate::cache::uri_of(&it.path)),
            ("Thumb::MTime", it.mtime.to_string()),
            // Optional in the standard, and worth writing: it is the only
            // record of who filled a cache entry, and the first question when
            // one of them turns out to be wrong.
            ("Software", "Scour".to_owned()),
        ],
    ) else {
        return false;
    };
    write_private(temp, &stamped) && std::fs::rename(temp, destination).is_ok()
}

/// Leave a note saying this one cannot be done.
///
/// Failing to write the note is not itself a failure worth reporting: the file
/// simply gets tried again next time, which is what would happen anyway.
fn record_failure(it: &Wanted) {
    let at = crate::cache::failure(&it.path);
    let Some(folder) = at.parent() else {
        return;
    };
    if make_private_dir(folder).is_err() {
        return;
    }
    let Some(note) = crate::png::with_text(
        &crate::png::one_transparent_pixel(),
        &[
            ("Thumb::URI", crate::cache::uri_of(&it.path)),
            ("Thumb::MTime", it.mtime.to_string()),
            ("Software", "Scour".to_owned()),
        ],
    ) else {
        return;
    };
    let temp = folder.join(temp_name());
    if write_private(&temp, &note) && std::fs::rename(&temp, &at).is_ok() {
        return;
    }
    let _ = std::fs::remove_file(&temp);
}

/// A name nothing else will pick, in the directory the file is going to.
///
/// In the same directory on purpose: a rename across filesystems is not
/// atomic and `$XDG_CACHE_HOME` is not always where `/tmp` is.
fn temp_name() -> String {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    format!(".scour-{}-{n}.png", std::process::id())
}

/// The standard asks for `0700` on the directories and `0600` on the files:
/// what a thumbnail is a picture of is nobody else's business, and on a shared
/// machine the cache is a list of what somebody has been looking at.
fn make_private_dir(at: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(at)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(at, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

fn write_private(at: &Path, bytes: &[u8]) -> bool {
    if std::fs::write(at, bytes).is_err() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if std::fs::set_permissions(at, std::fs::Permissions::from_mode(0o600)).is_err() {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The bound is the claim, so it is the thing to test.
    ///
    /// Counts how many permits are held at the same moment across a batch
    /// larger than the bound. This is the number the whole design rests on;
    /// asserting it in prose and not in code is how it comes back.
    #[test]
    fn never_more_than_the_bound_at_once() {
        let maker = Maker::new(3);
        let now = std::sync::atomic::AtomicUsize::new(0);
        let most = std::sync::atomic::AtomicUsize::new(0);
        std::thread::scope(|scope| {
            for _ in 0..20 {
                scope.spawn(|| {
                    let _permit = maker.permit();
                    let held = now.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
                    most.fetch_max(held, std::sync::atomic::Ordering::SeqCst);
                    std::thread::sleep(Duration::from_millis(20));
                    now.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
                });
            }
        });
        assert_eq!(most.load(std::sync::atomic::Ordering::SeqCst), 3);
        assert_eq!(*maker.free.lock().unwrap(), 3, "every permit came back");
    }

    /// A batch bigger than the cap is cut, not queued.
    #[test]
    fn a_batch_is_capped() {
        let maker = Maker::new(2);
        // Names nothing can thumbnail, so nothing is started and the only
        // thing measured is how many were looked at.
        let wanted: Vec<Wanted> = (0..100)
            .map(|i| Wanted {
                path: format!("/nowhere/{i}.this-extension-does-not-exist"),
                mtime: 1,
            })
            .collect();
        let made = maker.make(&wanted);
        assert_eq!(made.ran, 0, "nothing on this machine draws that");
        assert!(made.ready.is_empty());
    }

    #[test]
    fn a_percent_that_is_not_a_placeholder_survives() {
        // Not a public function, so this goes through `run`'s own substitution
        // by way of a command that records what it was given.
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("args");
        let script = dir.path().join("say.sh");
        std::fs::write(
            &script,
            format!("#!/bin/sh\nprintf '%s\\n' \"$@\" > {}\n", out.display()),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let command: Vec<String> = vec![
            script.to_string_lossy().into_owned(),
            "%i".into(),
            "%u".into(),
            "%s".into(),
            "%o".into(),
            "100%%".into(),
            "%z".into(),
        ];
        assert!(run(
            &command,
            "/a/b c.png",
            Path::new("/tmp/out.png"),
            Maker::PATIENCE
        ));
        let said = std::fs::read_to_string(&out).unwrap();
        let lines: Vec<&str> = said.lines().collect();
        assert_eq!(lines[0], "/a/b c.png");
        assert_eq!(lines[1], "file:///a/b%20c.png");
        assert_eq!(lines[2], crate::cache::ASKED.to_string());
        assert_eq!(lines[3], "/tmp/out.png");
        assert_eq!(lines[4], "100%");
        // A substitution nobody defined is left alone rather than eaten.
        assert_eq!(lines[5], "%z");
    }

    /// A thumbnailer that never returns must not hold its permit forever.
    ///
    /// **Four hung ones would stop every picture on the machine**, and hanging
    /// is what a decoder does on a truncated video rather than an exotic
    /// failure. Tested with a short patience and a command that would outlive
    /// the test suite; what is asserted is that it comes back, and quickly.
    #[test]
    fn a_hung_thumbnailer_is_killed() {
        let long = Duration::from_secs(300);
        let began = Instant::now();
        assert!(!run(
            &["sleep".to_owned(), "300".to_owned()],
            "/a/b",
            Path::new("/tmp/x.png"),
            Duration::from_millis(200),
        ));
        assert!(began.elapsed() < long, "it waited the whole sleep out");
        assert!(began.elapsed() < Duration::from_secs(5));
    }

    /// The three ordinary endings, none of which is a picture.
    #[test]
    fn a_process_that_fails_is_not_a_picture() {
        let p = Maker::PATIENCE;
        assert!(run(
            &["true".to_owned()],
            "/a/b",
            Path::new("/tmp/x.png"),
            p
        ));
        assert!(!run(
            &["false".to_owned()],
            "/a/b",
            Path::new("/tmp/x.png"),
            p
        ));
        // A thumbnailer whose package went away between the declaration being
        // read and the command being run.
        assert!(!run(
            &["this-command-does-not-exist".to_owned()],
            "/a/b",
            Path::new("/tmp/x.png"),
            p
        ));
    }
}
