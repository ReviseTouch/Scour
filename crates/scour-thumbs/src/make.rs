//! Asking the desktop for a picture it has not made yet: run the command the
//! machine declared, stamp the standard's text chunks on it, move it into place.
//! [`Maker::AT_ONCE`] bounds the whole machine, which is why one [`Maker`] lives
//! in the service. Nothing grows with a session — what was tried is on disk. This
//! blocks for seconds, so a caller must never have it in front of anything.

use std::path::Path;
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

/// One file somebody wants a picture of.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Wanted {
    pub path: String,
    /// The original's modification time, which becomes `Thumb::MTime`. From the
    /// caller's own `stat`, so the fence and the stamp cannot disagree.
    pub mtime: i64,
}

/// What came of asking.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Made {
    /// The paths that have a picture now, whether this made it or found it.
    pub ready: Vec<String>,
    /// How many processes were actually started — carried back so that "a grid of
    /// twenty thousand starts a few dozen" is checkable rather than asserted.
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
    /// How many thumbnailers may run at once, on the whole machine. Four fills a
    /// grid visibly while leaving the cores to the desktop; more finishes a
    /// screenful no sooner, and they are `nice`d on top of that.
    pub const AT_ONCE: usize = 4;

    /// The most files one call will look at — deliberately less than a screenful
    /// (40 large tiles, 180 small), so a caller that scrolled away pays nothing.
    /// Enforced here as well as in the page, because a frontend is not a fence.
    pub const BATCH: usize = 32;

    /// How long one thumbnailer is given before it is killed: a hung one holds a
    /// permit, and four hung ones stop every picture on the machine. A timeout is
    /// recorded as a failure, so it costs twenty seconds once rather than each pass.
    pub const PATIENCE: Duration = Duration::from_secs(20);

    pub fn new(at_once: usize) -> Maker {
        Maker {
            free: Mutex::new(at_once.max(1)),
            freed: Condvar::new(),
        }
    }

    /// Make what can be made, and say what is ready. Blocks until the batch is
    /// done; anything already cached or already failed starts nothing.
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

/// Is there a thumbnailer for this name at all? No I/O. A file that is itself a
/// thumbnail is refused here, at the fence, and not only in the callers — see
/// [`crate::is_one`], where drawing the cache writes back into the cache.
pub fn can_make(path: &str) -> bool {
    if crate::is_one(path) {
        return false;
    }
    let name = path.rsplit(['/', '\\']).next().unwrap_or(path);
    crate::known::known().can(name)
}

/// Run the thumbnailer, stamp what came back, and put it where it goes. True when
/// there is a picture at the end; every other outcome records a failure, which is
/// the one thing the caller needs: do not ask again for this version of this file.
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
    // Removed either way, or the cache fills with half-written pictures.
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

/// The standard's substitutions, and the machine's own `nice`. `patience` is a
/// parameter and not [`Maker::PATIENCE`] so a test can kill a hang in 200 ms.
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
    // `nice` rather than `libc::nice` through `pre_exec`: this crate has no
    // `unsafe`, and one extra `fork` is invisible beside a decode. Without `nice`
    // on the machine the command runs at full priority rather than not at all.
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
    // The service's log is not the place for a decoder's opinion of a bad file.
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
        // Polling: `std` has no wait-with-timeout. 50 ms is 400 wakeups across
        // the whole patience window.
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

/// Put the standard's metadata on the picture and move it into place. The rename
/// is atomic, so a reader never opens a half-written thumbnail.
fn install(temp: &Path, destination: &Path, it: &Wanted) -> bool {
    let Ok(raw) = std::fs::read(temp) else {
        return false;
    };
    let Some(stamped) = crate::png::with_text(
        &raw,
        &[
            ("Thumb::URI", crate::cache::uri_of(&it.path)),
            ("Thumb::MTime", it.mtime.to_string()),
            // Optional in the standard: the only record of who filled an entry.
            ("Software", "Scour".to_owned()),
        ],
    ) else {
        return false;
    };
    write_private(temp, &stamped) && std::fs::rename(temp, destination).is_ok()
}

/// Leave a note saying this one cannot be done. Failing to write it just means
/// the file is tried again next time.
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

/// A name nothing else will pick, in the directory the file is going to: a rename
/// across filesystems is not atomic and `$XDG_CACHE_HOME` need not be `/tmp`'s.
fn temp_name() -> String {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    format!(".scour-{}-{n}.png", std::process::id())
}

/// The standard asks for `0700` on directories and `0600` on files: on a shared
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

    /// Counts permits held at the same moment across a batch larger than the bound.
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
        // Names nothing can thumbnail: only the count of files looked at moves.
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
        // The substitution is not public, so a command records what it was given.
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

    /// A thumbnailer that never returns must not hold its permit: four hung ones
    /// would stop every picture on the machine.
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
        // A thumbnailer whose package went away since the declaration was read.
        assert!(!run(
            &["this-command-does-not-exist".to_owned()],
            "/a/b",
            Path::new("/tmp/x.png"),
            p
        ));
    }
}
