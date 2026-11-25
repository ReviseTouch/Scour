//! The change journal's records, as `FSCTL_READ_USN_JOURNAL` returns them:
//! which files changed, and when. What a raw read must not trust is told by
//! it — a file written in the last minutes may still have its bytes in the
//! cache and not on the disk.

/// One change: the file's record number, its USN, and when, as a FILETIME.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Change {
    pub record: u64,
    pub usn: i64,
    pub time: i64,
}

/// A filled buffer: the next USN to ask from, then version 2 records. A
/// record of another version is stepped over.
pub(crate) fn parse(buf: &[u8]) -> Option<(i64, Vec<Change>)> {
    let next = i64::from_le_bytes(buf.get(..8)?.try_into().ok()?);
    let mut out = Vec::new();
    let mut at = 8usize;
    while at + 60 <= buf.len() {
        let rec = &buf[at..];
        let len = u32::from_le_bytes(rec[0..4].try_into().ok()?) as usize;
        if len < 60 || at + len > buf.len() {
            break;
        }
        if u16::from_le_bytes(rec[4..6].try_into().ok()?) == 2 {
            out.push(Change {
                record: u64::from_le_bytes(rec[8..16].try_into().ok()?) & 0x0000_FFFF_FFFF_FFFF,
                usn: i64::from_le_bytes(rec[24..32].try_into().ok()?),
                time: i64::from_le_bytes(rec[32..40].try_into().ok()?),
            });
        }
        at += len;
    }
    Some((next, out))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_say_which_file_and_when() {
        let mut buf = 4096i64.to_le_bytes().to_vec();
        for (frn, usn, time) in [(0x0003_0000_0000_0042u64, 100i64, 7i64), (9, 200, 8)] {
            let mut r = vec![0u8; 64];
            r[0..4].copy_from_slice(&64u32.to_le_bytes());
            r[4..6].copy_from_slice(&2u16.to_le_bytes());
            r[8..16].copy_from_slice(&frn.to_le_bytes());
            r[24..32].copy_from_slice(&usn.to_le_bytes());
            r[32..40].copy_from_slice(&time.to_le_bytes());
            buf.extend(r);
        }
        let (next, got) = parse(&buf).unwrap();
        assert_eq!(next, 4096);
        assert_eq!(
            got[0],
            Change {
                record: 0x42,
                usn: 100,
                time: 7
            }
        );
        assert_eq!(got[1].record, 9);
    }
}
