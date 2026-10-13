//! The kernel's mount table, `/proc/self/mountinfo`, read line by line.

/// One mounted filesystem, as the kernel lists it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Mount {
    /// `major:minor`; anonymous (`0:nn`) for btrfs, whose real device is `source`.
    pub device: String,
    /// What part of the filesystem is mounted here: `/` for all of it, a
    /// subvolume or a bind mount's folder otherwise.
    pub root: String,
    /// Where it is mounted.
    pub at: String,
    pub fs: String,
    /// `/dev/sda1`, `server:/export`, `//host/share`.
    pub source: String,
}

/// Every line that parses. A line the kernel would never write is skipped,
/// not an error: the table is read again on the next change.
pub fn parse(text: &str) -> Vec<Mount> {
    text.lines().filter_map(line).collect()
}

fn line(text: &str) -> Option<Mount> {
    let (head, tail) = text.split_once(" - ")?;
    let head: Vec<&str> = head.split(' ').collect();
    let mut tail = tail.split(' ');
    // id, parent, major:minor, root, mount point, options, optional fields…
    let (device, root, at) = (head.get(2)?, head.get(3)?, head.get(4)?);
    Some(Mount {
        device: (*device).to_string(),
        root: unescape(root),
        at: unescape(at),
        fs: tail.next()?.to_string(),
        source: unescape(tail.next()?),
    })
}

/// The kernel writes a space, a tab, a newline and a backslash as three octal
/// digits after a backslash, and escapes nothing else.
pub fn unescape(field: &str) -> String {
    let bytes = field.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let octal = bytes
            .get(i + 1..i + 4)
            .filter(|d| d.iter().all(|c| (b'0'..=b'7').contains(c)));
        match (bytes[i], octal) {
            (b'\\', Some(d)) => {
                out.push((d[0] - b'0') * 64 + (d[1] - b'0') * 8 + (d[2] - b'0'));
                i += 4;
            }
            (b, _) => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_line_gives_its_device_root_place_type_and_source() {
        let text =
            "36 35 0:42 /@home /home rw,noatime shared:1 master:2 - btrfs /dev/nvme1n1p5 rw,ssd\n";
        assert_eq!(
            parse(text),
            vec![Mount {
                device: "0:42".into(),
                root: "/@home".into(),
                at: "/home".into(),
                fs: "btrfs".into(),
                source: "/dev/nvme1n1p5".into(),
            }]
        );
    }

    #[test]
    fn spaces_and_backslashes_come_back_as_themselves() {
        assert_eq!(unescape(r"/run/media/u/My\040Disk"), "/run/media/u/My Disk");
        assert_eq!(unescape(r"a\134b"), r"a\b");
        // Not three octal digits: left as written.
        assert_eq!(unescape(r"a\9b"), r"a\9b");
    }

    #[test]
    fn a_line_with_no_separator_is_skipped() {
        assert!(parse("garbage\n36 35 8:1 / / rw\n").is_empty());
    }
}
