//! Two questions a file interface has to answer and neither is about searching:
//! what this desktop's own folders are called, and whether a volume records when
//! a file was read. The service answers both so no face parses `user-dirs.dirs`
//! for itself. Depends on nothing but `serde`; it knows nothing about an index.

use serde::{Deserialize, Serialize};

/// A folder the desktop has its own name for.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Place {
    /// What the desktop calls it: the last component of the path, already in the
    /// owner's language. Nothing here translates.
    pub label: String,
    pub path: String,
}

/// A mount point, and whether the kernel records reads on it.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Mount {
    pub at: String,
    /// False under `noatime`, where `st_atime` is written once and never again.
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

/// Where a desktop puts what it downloads, if it says. By key and not by name:
/// the folder is called something different in every language.
pub fn download_dir(home: &str, text: &str) -> Option<String> {
    if home.is_empty() {
        return None;
    }
    for line in text.lines() {
        let Some((key, value)) = line.trim().split_once('=') else {
            continue;
        };
        if key.trim() != "XDG_DOWNLOAD_DIR" {
            continue;
        }
        let path = value.trim().trim_matches('"').replace("$HOME", home);
        // The home itself is not a download folder, it is the absence of one.
        if path.is_empty() || path == home {
            return None;
        }
        return Some(path);
    }
    None
}

/// The same, read off this machine.
pub fn downloads() -> Option<String> {
    let home = std::env::var("HOME").ok()?;
    download_dir(&home, &user_dirs(&home))
}

/// The folders named in `user-dirs.dirs`, in the desktop's own words. The format
/// is `XDG_DOCUMENTS_DIR="$HOME/Belgeler"`; quoting and `$HOME` are part of it.
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
        // The home itself is often named here, and a shortcut to everything is
        // not a shortcut.
        if label.is_empty() || path == home || !exists(&path) {
            continue;
        }
        out.push(Place { label, path });
    }
    out.sort_by(|a, b| a.label.cmp(&b.label));
    out.dedup_by(|a, b| a.path == b.path);
    out
}

/// Every mount point, and whether the kernel records reads on it — all of them,
/// because mounts nest and the deepest one owns the file, so a list of only the
/// `noatime` ones would call a whole disk silent. Empty without `/proc/self/mounts`.
pub fn mounts_from(text: &str) -> Vec<Mount> {
    let mut out = Vec::new();
    for line in text.lines() {
        let mut field = line.split_whitespace();
        let (Some(_dev), Some(at), Some(_kind), Some(opts)) =
            (field.next(), field.next(), field.next(), field.next())
        else {
            continue;
        };
        // A space is written `\040`; `/proc` escapes nothing else.
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
    fn a_download_folder_is_found_by_key_not_by_name() {
        // The name differs per language, so the key is what identifies it.
        let text = concat!(
            "XDG_DOCUMENTS_DIR=\"$HOME/Belgeler\"\n",
            "XDG_DOWNLOAD_DIR=\"$HOME/İndirilenler\"\n",
        );
        assert_eq!(
            download_dir("/home/u", text).as_deref(),
            Some("/home/u/İndirilenler")
        );
        // Pointed at the home means no download folder.
        assert_eq!(
            download_dir("/home/u", "XDG_DOWNLOAD_DIR=\"$HOME\"\n"),
            None
        );
        assert_eq!(download_dir("/home/u", ""), None);
        assert_eq!(download_dir("", "XDG_DOWNLOAD_DIR=\"/x\"\n"), None);
    }

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

    #[test]
    fn a_shortcut_to_nothing_is_not_a_shortcut() {
        let text = "XDG_MUSIC_DIR=\"$HOME/Müzik\"\nXDG_VIDEOS_DIR=\"$HOME/Videolar\"\n";
        let p = places_of("/home/u", text, |path| path.ends_with("Müzik"));
        assert_eq!(p.len(), 1);
        assert_eq!(p[0].label, "Müzik");
    }

    /// A silent `/` with a recording mount under it: reporting only the silent
    /// ones would call the whole disk silent.
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

    /// Nothing to read is not a failure; it is the answer where there is no `/proc`.
    #[test]
    fn a_machine_that_cannot_be_asked_says_nothing() {
        assert!(mounts_from("").is_empty());
        assert!(places_of("", "XDG_DOCUMENTS_DIR=\"$HOME/x\"", |_| true).is_empty());
        assert!(places_of("/home/u", "", |_| true).is_empty());
    }
}
