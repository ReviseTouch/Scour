//! Sets the marks, drops privilege, runs Scour.
//!
//! ```text
//! sudo scour-watch -- scourd
//! sudo scour-watch /home /mnt/depo -- scourd
//! sudo scour-watch --show
//! ```
//!
//! `FAN_MARK_FILESYSTEM` needs `CAP_SYS_ADMIN` and `scourd` must not have it.
//! Both are true at once because privilege is spent here, before `scourd`
//! exists, and what survives into it is a descriptor: measured, a process with
//! `CapEff=0` that is handed one reads every event on every marked filesystem
//! while `fanotify_mark` and `fanotify_init` both return EPERM to it. So the
//! descriptor cannot be used to widen the watch, and there is nothing left over
//! to take away.
//!
//! **This program installs nothing.** No unit, no capability on a file, no
//! change to a mount. It mounts one thing, for as long as it takes to set a
//! mark, and unmounts it before it execs — measured, the mark outlives the
//! mount it was placed through, so nothing has to stay behind to hold it.
//!
//! The one part that is not obvious is btrfs. A mark on a subvolume is refused
//! with `EXDEV`, because every subvolume reports a different fsid from the
//! superblock's root and the kernel will not place a superblock mark through a
//! path that disagrees with it. Every subvolume shares one superblock, and
//! `subvolid=5` is the only thing that *is* that superblock's root — so it gets
//! mounted, marked and unmounted, and the single mark then covers all seven
//! subvolumes on this machine. Verified by writing in three of them and
//! receiving the events.

#![cfg(target_os = "linux")]

use std::ffi::{CString, OsStr};
use std::os::unix::ffi::OsStrExt;
use std::process::ExitCode;

/// Where the mark is placed through, for a filesystem that needs a path that is
/// not otherwise mounted. Removed before this program execs.
const TEMP_MOUNT: &str = "/tmp/.scour-watch-sb";

/// The environment variable `scour-source-fs` reads the descriptor from.
const FD_ENV: &str = "SCOUR_FANOTIFY_FD";

// Kernel ABI. Written out rather than taken from `libc`, which does not carry
// the report flags at the time of writing.
const FAN_CLASS_NOTIF: libc::c_uint = 0x0000_0000;
const FAN_NONBLOCK: libc::c_uint = 0x0000_0002;
const FAN_UNLIMITED_QUEUE: libc::c_uint = 0x0000_0010;
const FAN_REPORT_FID: libc::c_uint = 0x0000_0200;
const FAN_REPORT_DIR_FID: libc::c_uint = 0x0000_0400;
const FAN_REPORT_NAME: libc::c_uint = 0x0000_0800;
const FAN_REPORT_TARGET_FID: libc::c_uint = 0x0000_1000;

const FAN_MARK_ADD: libc::c_uint = 0x0000_0001;
const FAN_MARK_FILESYSTEM: libc::c_uint = 0x0000_0100;

const FAN_CREATE: u64 = 0x0000_0100;
const FAN_DELETE: u64 = 0x0000_0200;
const FAN_MOVED_FROM: u64 = 0x0000_0040;
const FAN_MOVED_TO: u64 = 0x0000_0080;
const FAN_DELETE_SELF: u64 = 0x0000_0400;
const FAN_MOVE_SELF: u64 = 0x0000_0800;
const FAN_MODIFY: u64 = 0x0000_0002;
const FAN_ATTRIB: u64 = 0x0000_0004;
const FAN_CLOSE_WRITE: u64 = 0x0000_0008;
const FAN_ONDIR: u64 = 0x4000_0000;

/// What the group is opened with.
///
/// `FAN_CLOEXEC` is deliberately **absent**: the descriptor has to survive the
/// exec, which is the entire mechanism. `FAN_UNLIMITED_QUEUE` because the
/// default ceiling is 16,384 events and the overflow record says nothing at all
/// about what was dropped — `event_len == metadata_len`, no info record, all
/// measured — so a drop can only be answered by walking the tree again. The
/// queue is not free: about 95 bytes an event, so a million unread events is
/// 90 MB of kernel memory. The reader drains at 4.6 million a second, which is
/// what makes that a bound rather than a risk.
const INIT_FLAGS: libc::c_uint = FAN_CLASS_NOTIF
    | FAN_NONBLOCK
    | FAN_UNLIMITED_QUEUE
    | FAN_REPORT_DIR_FID
    | FAN_REPORT_NAME
    | FAN_REPORT_FID
    | FAN_REPORT_TARGET_FID;

/// What is asked for.
///
/// `FAN_MODIFY` cannot be dropped in favour of `FAN_CLOSE_WRITE`, which looks
/// like a saving and is not: a writer that keeps its descriptor open — journald,
/// a database with the file mapped, a virtual disk image, an editor — produced
/// **five MODIFY and zero CLOSE_WRITE** in a measurement, and would have stayed
/// invisible until the process died. `FAN_CLOSE_WRITE` is kept beside it as the
/// cheap hint that a write finished, never as proof one happened: an
/// `open(O_WRONLY)` and `close` with nothing written produces one too.
///
/// `FAN_ONDIR` is not optional. Without it `mkdir` and `rmdir` produce **no
/// event**, measured on both filesystems, and Scour indexes directories.
///
/// `FAN_RENAME` is not here on purpose. It is the better event — one atomic
/// record with the old and new name, where the `MOVED_FROM`/`MOVED_TO` pair has
/// no cookie to match them by — but putting both in one mask makes a single
/// `mv` arrive **three times**, measured. It belongs on its own or not at all.
const MASK: u64 = FAN_CREATE
    | FAN_DELETE
    | FAN_MOVED_FROM
    | FAN_MOVED_TO
    | FAN_DELETE_SELF
    | FAN_MOVE_SELF
    | FAN_MODIFY
    | FAN_ATTRIB
    | FAN_CLOSE_WRITE
    | FAN_ONDIR;

/// The three things a measurement said the mask had to be, checked where they
/// cannot be quietly edited away: at compile time rather than in a test, because
/// each of them is a silent failure at run time and none of them is visible in
/// a stack trace.
const _: () = {
    // A writer that keeps its descriptor open produced five MODIFY and zero
    // CLOSE_WRITE, and would be invisible until the process died.
    assert!(MASK & FAN_MODIFY != 0);
    // Without it `mkdir` and `rmdir` produce no event at all.
    assert!(MASK & FAN_ONDIR != 0);
    // FAN_RENAME beside the MOVED pair makes one `mv` arrive three times.
    assert!(MASK & 0x1000_0000 == 0);
    // The reader builds a path out of the parent's handle and the entry name;
    // without both it gets the object's own handle and no name, so every path
    // would be wrong rather than missing.
    assert!(INIT_FLAGS & (FAN_REPORT_DIR_FID | FAN_REPORT_NAME) != 0);
    assert!(INIT_FLAGS & FAN_REPORT_DIR_FID != 0);
    assert!(INIT_FLAGS & FAN_REPORT_NAME != 0);
};

unsafe extern "C" {
    fn fanotify_init(flags: libc::c_uint, event_f_flags: libc::c_uint) -> libc::c_int;
    fn fanotify_mark(
        fd: libc::c_int,
        flags: libc::c_uint,
        mask: u64,
        dirfd: libc::c_int,
        path: *const libc::c_char,
    ) -> libc::c_int;
}

fn err() -> String {
    std::io::Error::last_os_error().to_string()
}

/// A filesystem worth marking: one superblock, however many times it is mounted.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Sb {
    /// The block device behind it, which is what makes two mounts one
    /// superblock. btrfs gives every subvolume its own anonymous device number,
    /// so `major:minor` from `mountinfo` counts the same filesystem seven times
    /// here and the source does not.
    source: String,
    fstype: String,
    /// Somewhere it is mounted, for the filesystems that can be marked through
    /// any of their paths.
    at: String,
}

/// Read `/proc/self/mountinfo` and pick the real filesystems out of it.
///
/// Everything virtual is skipped, and not by a blocklist of names: a filesystem
/// with no block device behind it has nothing for Scour to index. The exception
/// that matters is that `tmpfs` is left out deliberately even though it can be
/// marked — `/tmp` and `/run` are where the churn is and none of it is a file
/// anyone searches for.
fn superblocks() -> Vec<Sb> {
    const SKIP: [&str; 6] = ["tmpfs", "devtmpfs", "overlay", "squashfs", "fuse", "autofs"];
    let text = std::fs::read_to_string("/proc/self/mountinfo").unwrap_or_default();
    let mut out: Vec<Sb> = Vec::new();
    for line in text.lines() {
        // ... - <fstype> <source> <options>
        let Some((left, right)) = line.split_once(" - ") else {
            continue;
        };
        let mut r = right.split_whitespace();
        let (Some(fstype), Some(source)) = (r.next(), r.next()) else {
            continue;
        };
        if !source.starts_with("/dev/") || SKIP.iter().any(|s| fstype.starts_with(s)) {
            continue;
        }
        let Some(at) = left.split_whitespace().nth(4) else {
            continue;
        };
        if out.iter().any(|s| s.source == source) {
            continue;
        }
        out.push(Sb {
            source: source.to_owned(),
            fstype: fstype.to_owned(),
            at: at.replace("\\040", " "),
        });
    }
    out
}

/// Mount the superblock's own root somewhere, for filesystems whose ordinary
/// mounts are not it.
///
/// Only btrfs needs this, and it needs it because of subvolumes. Returns the
/// path to unmount afterwards.
fn expose_root(sb: &Sb) -> Option<String> {
    if sb.fstype != "btrfs" {
        return None;
    }
    let _ = std::fs::create_dir_all(TEMP_MOUNT);
    let src = CString::new(sb.source.as_bytes()).ok()?;
    let dst = CString::new(TEMP_MOUNT).ok()?;
    let fs = CString::new("btrfs").ok()?;
    let opt = CString::new("subvolid=5").ok()?;
    // Read-only: nothing is written through this and it exists for a few
    // microseconds. `MS_PRIVATE` afterwards so the mount does not propagate to
    // peers — without it the unmount leaves a copy behind in another namespace
    // and the directory cannot be removed.
    let rc = unsafe {
        libc::mount(
            src.as_ptr(),
            dst.as_ptr(),
            fs.as_ptr(),
            libc::MS_RDONLY,
            opt.as_ptr().cast(),
        )
    };
    if rc != 0 {
        eprintln!(
            "scour-watch: {} icin subvolid=5 baglanamadi: {}",
            sb.source,
            err()
        );
        let _ = std::fs::remove_dir(TEMP_MOUNT);
        return None;
    }
    unsafe {
        libc::mount(
            std::ptr::null(),
            dst.as_ptr(),
            std::ptr::null(),
            libc::MS_PRIVATE,
            std::ptr::null(),
        );
    }
    Some(TEMP_MOUNT.to_owned())
}

fn unexpose(path: &str) {
    if let Ok(c) = CString::new(path) {
        unsafe { libc::umount2(c.as_ptr(), libc::MNT_DETACH) };
    }
    let _ = std::fs::remove_dir(path);
}

/// Place one mark, and say what happened either way.
fn mark(fd: libc::c_int, sb: &Sb) -> bool {
    let temp = expose_root(sb);
    let through = temp.as_deref().unwrap_or(sb.at.as_str());
    let c = match CString::new(through) {
        Ok(c) => c,
        Err(_) => return false,
    };
    let rc = unsafe {
        fanotify_mark(
            fd,
            FAN_MARK_ADD | FAN_MARK_FILESYSTEM,
            MASK,
            libc::AT_FDCWD,
            c.as_ptr(),
        )
    };
    let ok = rc == 0;
    if ok {
        println!(
            "  isaretlendi  {:<14} {:<7} {}",
            sb.source,
            sb.fstype,
            if temp.is_some() {
                "(superblock kokunden)"
            } else {
                sb.at.as_str()
            }
        );
    } else {
        eprintln!(
            "  ATLANDI      {:<14} {:<7} {}",
            sb.source,
            sb.fstype,
            err()
        );
    }
    if let Some(t) = temp {
        unexpose(&t);
    }
    ok
}

/// Put the environment back to the invoking user's.
///
/// **Dropping the user id is not enough**, and the first real run is what said
/// so: `sudo` leaves `HOME=/root`, so `scourd` came up as uid 1000 and went
/// looking for its configuration in `/root/.config/scour`, which it is not
/// allowed to read. Every path a program derives from the environment rather
/// than from `getuid` has this problem — the config, the index, the socket —
/// and setting the identity without setting the environment produces the worst
/// version of it: a process that is the right user and behaves like the wrong
/// one.
///
/// The home directory comes from the password database rather than from
/// `SUDO_HOME`, because there is no such variable and guessing `/home/<name>`
/// is wrong on any machine that does not keep them there.
fn restore_environment(uid: u32, gid: u32) {
    let mut home = String::new();
    let mut name = String::new();
    // SAFETY: `getpwuid` returns a pointer into a static buffer that is valid
    // until the next call, and both strings are copied out before returning.
    unsafe {
        let pw = libc::getpwuid(uid);
        if !pw.is_null() {
            if !(*pw).pw_dir.is_null() {
                home = std::ffi::CStr::from_ptr((*pw).pw_dir)
                    .to_string_lossy()
                    .into_owned();
            }
            if !(*pw).pw_name.is_null() {
                name = std::ffi::CStr::from_ptr((*pw).pw_name)
                    .to_string_lossy()
                    .into_owned();
            }
        }
    }
    if name.is_empty() {
        name = std::env::var("SUDO_USER").unwrap_or_default();
    }

    unsafe {
        if !home.is_empty() {
            // Anything pointing into root's home is `sudo`'s, not the user's,
            // and would send the index or the cache somewhere unreadable. The
            // ones that are set deliberately do not start with `/root`.
            for k in [
                "XDG_CONFIG_HOME",
                "XDG_DATA_HOME",
                "XDG_CACHE_HOME",
                "XDG_STATE_HOME",
            ] {
                if std::env::var(k).is_ok_and(|v| v.starts_with("/root")) {
                    std::env::remove_var(k);
                }
            }
            std::env::set_var("HOME", &home);
        }
        if !name.is_empty() {
            std::env::set_var("USER", &name);
            std::env::set_var("LOGNAME", &name);
        }
        // The session bus and the socket live here, and `sudo` either drops it
        // or points it at root's.
        let run = format!("/run/user/{uid}");
        if std::path::Path::new(&run).is_dir() {
            std::env::set_var("XDG_RUNTIME_DIR", &run);
        }
        for k in ["SUDO_UID", "SUDO_GID", "SUDO_USER", "SUDO_COMMAND"] {
            std::env::remove_var(k);
        }
    }
    let _ = gid;
}

/// Become the user who invoked `sudo`, irreversibly.
///
/// Order matters and is not stylistic: the supplementary groups have to go
/// first, because dropping the user id is what takes away the right to change
/// them. The result is checked rather than assumed — a `setuid` that silently
/// did nothing would leave `scourd` running as root, which is the one outcome
/// this whole design exists to avoid.
/// **Or the user a service unit names.** `SUDO_UID` is how a person running
/// this from a shell says who to become, and there is no shell in a unit file:
/// `--as 1000` is the same sentence for `systemd`, which is what makes a
/// permanent mark possible at all. Without one of the two this program has
/// nobody to drop to and refuses rather than guessing — running the index as
/// root is the single outcome the whole design exists to avoid.
fn become_invoker(asked: Option<u32>) -> Result<(u32, u32), String> {
    let uid: u32 = match asked {
        Some(uid) => uid,
        None => std::env::var("SUDO_UID")
            .map_err(|_| {
                "kime dusulecegi belli degil — sudo ile calistir ya da --as <uid> ver".to_string()
            })?
            .parse()
            .map_err(|_| "SUDO_UID sayi degil".to_string())?,
    };
    let gid: u32 = match asked {
        Some(uid) => group_of(uid).unwrap_or(uid),
        None => std::env::var("SUDO_GID")
            .ok()
            .and_then(|g| g.parse().ok())
            .unwrap_or(uid),
    };
    if uid == 0 {
        return Err("hedef kullanici root — dusurulecek ayricalik yok".into());
    }
    unsafe {
        if libc::setgroups(0, std::ptr::null()) != 0 {
            return Err(format!("setgroups: {}", err()));
        }
        if libc::setgid(gid) != 0 {
            return Err(format!("setgid: {}", err()));
        }
        if libc::setuid(uid) != 0 {
            return Err(format!("setuid: {}", err()));
        }
        if libc::geteuid() != uid || libc::setuid(0) == 0 {
            return Err("ayricalik gercekten dusurulemedi".into());
        }
    }
    Ok((uid, gid))
}

fn usage() {
    eprintln!(
        "kullanim:\n  \
         sudo scour-watch -- <komut> [arg...]     butun gercek dosya sistemleri\n  \
         sudo scour-watch <yol>... -- <komut>     yalniz bu yollarin dosya sistemleri\n  \
         sudo scour-watch --show                  ne isaretlenecegini yaz, hicbir sey yapma\n  \
         --as <kullanici|uid>                     kime dusulecek (sudo yoksa: servis birimi)"
    );
}

/// `--as <uid|name>`, if it is there: the id to drop to, and the word that
/// followed the flag so the path list can leave it out.
fn user_arg(paths: &[String]) -> Result<(Option<u32>, Option<String>), String> {
    let Some(at) = paths.iter().position(|a| a == "--as") else {
        return Ok((None, None));
    };
    let who = paths
        .get(at + 1)
        .ok_or_else(|| "--as bir kullanici ya da uid ister".to_string())?;
    if let Ok(uid) = who.parse::<u32>() {
        return Ok((Some(uid), Some(who.clone())));
    }
    let uid = uid_of(who).ok_or_else(|| format!("boyle bir kullanici yok: {who}"))?;
    Ok((Some(uid), Some(who.clone())))
}

/// The numeric id behind a user name, from this machine's own table.
fn uid_of(name: &str) -> Option<u32> {
    std::fs::read_to_string("/etc/passwd")
        .ok()?
        .lines()
        .find_map(|line| {
            let mut f = line.split(':');
            (f.next()? == name).then(|| f.nth(1)?.parse().ok())?
        })
}

/// The primary group of a user id, from the same table.
fn group_of(uid: u32) -> Option<u32> {
    std::fs::read_to_string("/etc/passwd")
        .ok()?
        .lines()
        .find_map(|line| {
            let mut f = line.split(':');
            let _name = f.next()?;
            let _x = f.next()?;
            (f.next()?.parse::<u32>().ok()? == uid).then(|| f.next()?.parse().ok())?
        })
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "-h" || a == "--help") {
        usage();
        return ExitCode::SUCCESS;
    }

    let split = args.iter().position(|a| a == "--");
    let (paths, command) = match split {
        Some(i) => (&args[..i], &args[i + 1..]),
        None => (&args[..], &[][..]),
    };
    let show = paths.iter().any(|a| a == "--show");
    // **Who to become, for a caller that is not a shell.** `--as 1000` or
    // `--as hasan`; see `become_invoker`.
    let asked = match user_arg(paths) {
        Ok(u) => u,
        Err(why) => {
            eprintln!("scour-watch: {why}");
            return ExitCode::FAILURE;
        }
    };
    let wanted: Vec<&String> = paths
        .iter()
        .filter(|a| !a.starts_with("--"))
        .filter(|a| Some(a.as_str()) != asked.1.as_deref())
        .collect();
    let asked = asked.0;

    // The filesystems those paths sit on, or all of them.
    let mut sbs = superblocks();
    if !wanted.is_empty() {
        sbs.retain(|sb| {
            wanted.iter().any(|w| {
                std::fs::canonicalize(w)
                    .ok()
                    .and_then(|p| {
                        let m = std::fs::metadata(&p).ok()?;
                        let at = std::fs::metadata(&sb.at).ok()?;
                        use std::os::unix::fs::MetadataExt;
                        Some(m.dev() == at.dev() || p.starts_with(&sb.at))
                    })
                    .unwrap_or(false)
            })
        });
    }

    if sbs.is_empty() {
        eprintln!("scour-watch: isaretlenecek dosya sistemi bulunamadi");
        return ExitCode::FAILURE;
    }

    if show {
        println!("isaretlenecek dosya sistemleri:");
        for sb in &sbs {
            println!(
                "  {:<14} {:<7} {}{}",
                sb.source,
                sb.fstype,
                sb.at,
                if sb.fstype == "btrfs" {
                    "   (subvolid=5 gecici olarak baglanacak)"
                } else {
                    ""
                }
            );
        }
        return ExitCode::SUCCESS;
    }

    if command.is_empty() {
        usage();
        return ExitCode::FAILURE;
    }

    if unsafe { libc::geteuid() } != 0 {
        eprintln!("scour-watch: root gerekiyor (FAN_MARK_FILESYSTEM icin CAP_SYS_ADMIN)");
        return ExitCode::FAILURE;
    }

    let fd = unsafe { fanotify_init(INIT_FLAGS, libc::O_RDONLY as libc::c_uint) };
    if fd < 0 {
        eprintln!("scour-watch: fanotify_init: {}", err());
        return ExitCode::FAILURE;
    }

    println!("scour-watch:");
    let marked = sbs.iter().filter(|sb| mark(fd, sb)).count();
    if marked == 0 {
        eprintln!("scour-watch: hicbir dosya sistemi isaretlenemedi");
        return ExitCode::FAILURE;
    }

    // Read before the identity is dropped, because the variables that say who
    // invoked this are the first thing `restore_environment` clears.
    let (uid, gid) = match become_invoker(asked) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("scour-watch: {e}");
            return ExitCode::FAILURE;
        }
    };
    restore_environment(uid, gid);
    println!("  {marked} dosya sistemi, uid={uid} gid={gid} olarak calistiriliyor\n");

    // The descriptor has to cross the exec, so the flag that would close it is
    // cleared here rather than never set — `fanotify_init` does not offer the
    // choice separately from the rest of its flags.
    unsafe {
        let f = libc::fcntl(fd, libc::F_GETFD);
        libc::fcntl(fd, libc::F_SETFD, f & !libc::FD_CLOEXEC);
    }

    let program = CString::new(command[0].as_bytes()).unwrap();
    let argv: Vec<CString> = command
        .iter()
        .map(|a| CString::new(a.as_bytes()).unwrap())
        .collect();
    let mut argv_p: Vec<*const libc::c_char> = argv.iter().map(|c| c.as_ptr()).collect();
    argv_p.push(std::ptr::null());

    unsafe { std::env::set_var(FD_ENV, fd.to_string()) };
    unsafe { libc::execvp(program.as_ptr(), argv_p.as_ptr()) };

    eprintln!(
        "scour-watch: {} calistirilamadi: {}",
        OsStr::from_bytes(command[0].as_bytes()).to_string_lossy(),
        err()
    );
    ExitCode::FAILURE
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_superblock_is_listed_once_however_many_times_it_is_mounted() {
        // btrfs gives every subvolume its own anonymous device, so a line-per
        // -mount reading of `mountinfo` counts this filesystem seven times.
        let sbs = superblocks();
        let mut sources: Vec<&str> = sbs.iter().map(|s| s.source.as_str()).collect();
        let before = sources.len();
        sources.sort_unstable();
        sources.dedup();
        assert_eq!(before, sources.len(), "a source appears more than once");
    }

    #[test]
    fn nothing_without_a_block_device_behind_it_is_marked() {
        for sb in superblocks() {
            assert!(sb.source.starts_with("/dev/"), "{sb:?}");
            assert!(
                !sb.fstype.starts_with("tmpfs") && !sb.fstype.starts_with("proc"),
                "{sb:?} has nothing anyone searches for in it"
            );
        }
    }
}
