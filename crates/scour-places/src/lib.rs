//! Where a person keeps things, and what the volumes under them record.
//!
//! Two questions a file interface has to answer and neither of which is about
//! searching: *what are this desktop's own folders called*, and *does this
//! volume record when a file was read*.
//!
//! ## Why the service answers them
//!
//! Both were in the browser bridge, worked out from `$HOME` and
//! `/proc/self/mounts` on the spot. That is one frontend's copy of a rule, and
//! there are meant to be four — so a terminal interface would parse
//! `user-dirs.dirs` again, a Slint window a third time, and the day one of
//! them got the quoting wrong its sidebar would point at folders that are not
//! there. The same guess had already been wrong once at a higher layer: the
//! page shipped with `/home/hasan` written into it.
//!
//! The service runs where the files are and every frontend already talks to
//! it. So it answers, and a frontend draws what it is told.
//!
//! ## No dependencies but `serde`
//!
//! Nothing here knows what an index is. It reads two files a desktop writes
//! and returns what they say, which is a question about a machine rather than
//! about Scour — the same reasoning that keeps `scour-preview` and
//! `scour-dupes` standing on their own.

use serde::{Deserialize, Serialize};

/// A folder the desktop has its own name for.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Place {
    /// What the desktop calls it, **already in the owner's language** — it is
    /// the last component of the path, and `user-dirs.dirs` is written by the
    /// desktop in the language it was set up in. Nothing here translates.
    pub label: String,
    pub path: String,
}

/// A mount point, and whether the kernel records reads on it.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Mount {
    pub at: String,
    /// False under `noatime`, where `st_atime` is written when the file is
    /// made and never again.
    pub reads: bool,
}

/// Everything this module knows, asked once.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Places {
    pub home: String,
    pub places: Vec<Place>,
    pub mounts: Vec<Mount>,
}

/// Ask the machine.
pub fn look() -> Places {
    let home = std::env::var("HOME").unwrap_or_default();
    Places {
        places: places_of(&home, &user_dirs(&home), |p| {
            std::path::Path::new(p).is_dir()
        }),
        mounts: mounts_from(&read_mounts()),
        home,
    }
}

fn user_dirs(home: &str) -> String {
    if home.is_empty() {
        return String::new();
    }
    std::fs::read_to_string(std::path::Path::new(home).join(".config/user-dirs.dirs"))
        .unwrap_or_default()
}

fn read_mounts() -> String {
    std::fs::read_to_string("/proc/self/mounts").unwrap_or_default()
}

/// The folders named in `user-dirs.dirs`, in the desktop's own words.
///
/// The format is `XDG_DOCUMENTS_DIR="$HOME/Belgeler"` a line, and both the
/// quoting and the `$HOME` are part of it. `exists` is passed in so the rule
/// can be tested without a home directory to arrange.
pub fn places_of(home: &str, text: &str, exists: impl Fn(&str) -> bool) -> Vec<Place> {
    let mut out: Vec<Place> = Vec::new();
    if home.is_empty() {
        return out;
    }
    for line in text.lines() {
        let Some((key, value)) = line.trim().split_once('=') else {
            continue;
        };
        if !key.starts_with("XDG_") || !key.ends_with("_DIR") {
            continue;
        }
        let path = value.trim().trim_matches('"').replace("$HOME", home);
        let label = path.rsplit('/').next().unwrap_or_default().to_owned();
        // `XDG_DESKTOP_DIR` is often the home itself on a headless setup, and
        // a shortcut to everything is not a shortcut.
        if label.is_empty() || path == home || !exists(&path) {
            continue;
        }
        out.push(Place { label, path });
    }
    out.sort_by(|a, b| a.label.cmp(&b.label));
    out.dedup_by(|a, b| a.path == b.path);
    out
}

/// Every mount point, and whether the kernel records reads on it.
///
/// **A column that shows a number nobody maintains is worse than an empty
/// one.** With `noatime`, `st_atime` is written once — when the file is made —
/// and never again, so a browser profile rewritten every second reports
/// "accessed eleven days ago", which is the day the application was installed.
/// Every one of those numbers is *true* and none of them answers the question
/// the column's heading asks.
///
/// **All of them, not only the `noatime` ones**, because mount points nest and
/// the deepest one owns the file: `/` is `noatime` on this machine while
/// `/mnt/depo` under it is `relatime`, so a list of just the silent mounts
/// would call the whole disk silent. That was the first version, and it marked
/// every row.
///
/// Empty on anything without `/proc/self/mounts` — the honest answer where
/// this cannot be asked, and a frontend then behaves as it did before it could.
pub fn mounts_from(text: &str) -> Vec<Mount> {
    let mut out = Vec::new();
    for line in text.lines() {
        let mut field = line.split_whitespace();
        let (Some(_dev), Some(at), Some(_kind), Some(opts)) =
            (field.next(), field.next(), field.next(), field.next())
        else {
            continue;
        };
        // A mount point with a space in it is written `\040`, and `/proc` does
        // not escape anything else.
        let at = at.replace("\\040", " ");
        let silent = opts.split(',').any(|o| o == "noatime");
        out.push(Mount { at, reads: !silent });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_desktops_own_names_survive_the_quoting() {
        let text = concat!(
            "# a comment\n",
            "XDG_DESKTOP_DIR=\"$HOME/Masaüstü\"\n",
            "XDG_DOCUMENTS_DIR=\"$HOME/Belgeler\"\n",
            "XDG_DOWNLOAD_DIR=\"$HOME\"\n",
            "NOT_XDG=\"$HOME/Nope\"\n",
        );
        let p = places_of("/home/u", text, |_| true);
        assert_eq!(
            p,
            vec![
                Place {
                    label: "Belgeler".into(),
                    path: "/home/u/Belgeler".into()
                },
                Place {
                    label: "Masaüstü".into(),
                    path: "/home/u/Masaüstü".into()
                },
            ],
            "sorted by the name the desktop uses; the home itself is not a shortcut"
        );
    }

    /// A folder named in the file and not on the disk is not offered.
    #[test]
    fn a_shortcut_to_nothing_is_not_a_shortcut() {
        let text = "XDG_MUSIC_DIR=\"$HOME/Müzik\"\nXDG_VIDEOS_DIR=\"$HOME/Videolar\"\n";
        let p = places_of("/home/u", text, |path| path.ends_with("Müzik"));
        assert_eq!(p.len(), 1);
        assert_eq!(p[0].label, "Müzik");
    }

    /// **The nesting is the whole point.** `/` silent and `/mnt/depo` under it
    /// recording is the arrangement on the machine this was written for, and a
    /// list of only the silent mounts would call the whole disk silent.
    #[test]
    fn every_mount_is_reported_not_only_the_silent_ones() {
        let text = concat!(
            "/dev/nvme0n1p2 / btrfs rw,noatime,compress=zstd:3 0 0\n",
            "/dev/nvme1n1p2 /mnt/depo ntfs3 rw,relatime,uid=1000 0 0\n",
            "tmpfs /run/user/1000 tmpfs rw,nosuid,relatime 0 0\n",
        );
        let m = mounts_from(text);
        assert_eq!(m.len(), 3, "all of them, so the deepest match can be found");
        assert_eq!(
            m[0],
            Mount {
                at: "/".into(),
                reads: false
            }
        );
        assert_eq!(
            m[1],
            Mount {
                at: "/mnt/depo".into(),
                reads: true
            }
        );
    }

    #[test]
    fn a_mount_point_with_a_space_is_read_back_whole() {
        let m = mounts_from("/dev/sdb1 /run/media/u/My\\040Disk vfat rw,noatime 0 0\n");
        assert_eq!(m[0].at, "/run/media/u/My Disk");
        assert!(!m[0].reads);
    }

    /// Nothing to read is not a failure — it is the answer on a machine that
    /// has no `/proc`, and a frontend then behaves as it did before it asked.
    #[test]
    fn a_machine_that_cannot_be_asked_says_nothing() {
        assert!(mounts_from("").is_empty());
        assert!(places_of("", "XDG_DOCUMENTS_DIR=\"$HOME/x\"", |_| true).is_empty());
        assert!(places_of("/home/u", "", |_| true).is_empty());
    }
}
