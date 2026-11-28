//! What an archive holds, as a sheet: each entry's path, its size and when it
//! was changed, in the order the archive keeps them. A zip by its central
//! directory, read from its end whatever its size; a tar, plain or gzipped,
//! header by header from its start, its contents passed over. Nothing in it
//! is unpacked to disk, and no entry is opened.

use std::fs::File;
use std::io::{BufReader, Read};
use std::path::Path;

use scour_flow::{Align, Block, Flow, GridCell, GridRow, Sheet};

/// Rows listed at most.
const MOST_ROWS: usize = scour_flow::MOST_SHEET_ROWS;
/// Bytes of a gzipped tar unpacked at most, to reach its headers.
const MOST_UNPACKED: u64 = 256 << 20;

/// Whether `path` is an archive listed here: the format table's `zip` and
/// `tar` readers (`scour_formats`).
pub fn lists(path: &Path) -> bool {
    scour_formats::of_path(path).is_some_and(|f| f.flows && matches!(f.like, "zip" | "tar"))
}

/// The listing of the archive opened as `file`.
pub fn listing(mut file: File) -> Result<Flow, String> {
    let mut head = [0u8; 4];
    let got = file.read(&mut head).map_err(|e| e.to_string())?;
    let len = file.metadata().map_err(|e| e.to_string())?.len();
    let mut rows = Vec::new();
    let cut = if head[..got].starts_with(b"PK") {
        zip(&mut file, len, &mut rows)?
    } else {
        use std::io::Seek;
        file.rewind().map_err(|e| e.to_string())?;
        let reader = BufReader::new(file);
        if head[..got].starts_with(&[0x1f, 0x8b]) {
            let unpacked = flate2::read::GzDecoder::new(reader).take(MOST_UNPACKED);
            tar(unpacked, &mut rows)?
        } else {
            tar(reader, &mut rows)?
        }
    };
    let mut all = vec![GridRow {
        cells: ["Name", "Size", "Modified"]
            .into_iter()
            .map(|t| GridCell {
                text: t.to_owned(),
                bold: true,
                shade: Some([0xf6, 0xf8, 0xfa]),
                ..GridCell::default()
            })
            .collect(),
    }];
    all.extend(rows);
    let name_width = all
        .iter()
        .map(|r| r.cells[0].text.chars().count())
        .max()
        .unwrap_or(10)
        .clamp(12, 60) as f32
        * 0.55
        + 1.0;
    Ok(Flow {
        blocks: vec![Block::Sheet(Sheet {
            name: String::new(),
            widths: vec![name_width, 7.0, 10.0],
            rows: all,
            frozen: true,
            cut,
        })],
        cut: false,
    }
    .bounded())
}

/// One entry as a row: a folder's name bold, a file's size beside it.
fn row(name: &str, size: Option<u64>, when: String, folder: bool) -> GridRow {
    GridRow {
        cells: vec![
            GridCell {
                text: name.to_owned(),
                bold: folder,
                ..GridCell::default()
            },
            GridCell {
                text: size.filter(|_| !folder).map(bytes).unwrap_or_default(),
                align: Some(Align::Right),
                ..GridCell::default()
            },
            GridCell {
                text: when,
                ..GridCell::default()
            },
        ],
    }
}

fn zip(file: &mut File, len: u64, rows: &mut Vec<GridRow>) -> Result<bool, String> {
    let mut cd = Vec::new();
    let mut read = 0;
    let mut entries = Vec::new();
    if !crate::zip::load_central(file, len, &mut cd, &mut read, |e| entries.push(e)) {
        return Err("no directory to list".into());
    }
    for e in entries.iter().take(MOST_ROWS) {
        let name = String::from_utf8_lossy(e.name_in(&cd));
        let folder = name.ends_with('/');
        // Past four gigabytes the size is in an extra field this does not read.
        let size = (e.uncomp_size != u64::from(u32::MAX)).then_some(e.uncomp_size);
        rows.push(row(&name, size, dos_time(e.modified), folder));
    }
    Ok(entries.len() > MOST_ROWS)
}

/// A DOS date and time, as `2026-11-28 13:04`; empty when unset.
fn dos_time(v: u32) -> String {
    let (date, time) = (v >> 16, v & 0xffff);
    if date == 0 {
        return String::new();
    }
    format!(
        "{}-{:02}-{:02} {:02}:{:02}",
        1980 + (date >> 9),
        (date >> 5) & 0xf,
        date & 0x1f,
        time >> 11,
        (time >> 5) & 0x3f
    )
}

/// A tar's headers, read in order; each entry's contents read past.
fn tar(mut r: impl Read, rows: &mut Vec<GridRow>) -> Result<bool, String> {
    let mut header = [0u8; 512];
    let mut long_name: Option<String> = None;
    loop {
        if r.read_exact(&mut header).is_err() || header.iter().all(|&b| b == 0) {
            return if rows.is_empty() {
                Err("not an archive listed here".into())
            } else {
                Ok(false)
            };
        }
        if !checksum_holds(&header) {
            return if rows.is_empty() {
                Err("not a tar".into())
            } else {
                Ok(false)
            };
        }
        let size = number(&header[124..136]);
        let kind = header[156];
        let padded = size.div_ceil(512) * 512;
        // A long name, GNU's or POSIX's, is an entry of its own before the one it names.
        if matches!(kind, b'L' | b'x') && size <= 64 << 10 {
            let mut data = vec![0u8; padded as usize];
            r.read_exact(&mut data).map_err(|e| e.to_string())?;
            let data = &data[..size as usize];
            long_name = if kind == b'L' {
                Some(cstr(data))
            } else {
                pax_path(data)
            };
            continue;
        }
        if kind == b'g' || kind == b'x' || kind == b'L' || kind == b'K' {
            skip(&mut r, padded)?;
            continue;
        }
        let name = long_name.take().unwrap_or_else(|| {
            let name = cstr(&header[0..100]);
            let prefix = if &header[257..262] == b"ustar" {
                cstr(&header[345..500])
            } else {
                String::new()
            };
            if prefix.is_empty() {
                name
            } else {
                format!("{prefix}/{name}")
            }
        });
        let folder = kind == b'5' || name.ends_with('/');
        rows.push(row(
            &name,
            Some(size),
            unix_time(number(&header[136..148]) as i64),
            folder,
        ));
        if rows.len() > MOST_ROWS {
            rows.truncate(MOST_ROWS);
            return Ok(true);
        }
        skip(&mut r, padded)?;
    }
}

fn skip(r: &mut impl Read, n: u64) -> Result<(), String> {
    let copied = std::io::copy(&mut r.take(n), &mut std::io::sink()).map_err(|e| e.to_string())?;
    if copied < n {
        return Err("cut short".into());
    }
    Ok(())
}

/// The header's own sum: what tells a tar from anything else.
fn checksum_holds(h: &[u8; 512]) -> bool {
    let want = number(&h[148..156]);
    let sum: u64 = h
        .iter()
        .enumerate()
        .map(|(i, &b)| {
            if (148..156).contains(&i) {
                32
            } else {
                u64::from(b)
            }
        })
        .sum();
    want == sum
}

/// An octal field, or GNU's base-256 one for a large number.
fn number(field: &[u8]) -> u64 {
    if field.first().is_some_and(|b| b & 0x80 != 0) {
        return field[1..]
            .iter()
            .fold(u64::from(field[0] & 0x7f), |n, &b| (n << 8) | u64::from(b));
    }
    field
        .iter()
        .skip_while(|b| **b == b' ')
        .take_while(|b| (b'0'..=b'7').contains(*b))
        .fold(0, |n, &b| n * 8 + u64::from(b - b'0'))
}

fn cstr(bytes: &[u8]) -> String {
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..end]).into_owned()
}

/// A POSIX extended header's `path`, if it gives one.
fn pax_path(data: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(data);
    text.lines().find_map(|line| {
        let (_, record) = line.split_once(' ')?;
        record.strip_prefix("path=").map(str::to_owned)
    })
}

/// Seconds since 1970, as `2026-11-28 13:04` in UTC.
fn unix_time(secs: i64) -> String {
    if secs <= 0 {
        return String::new();
    }
    let days = secs.div_euclid(86_400);
    let rest = secs.rem_euclid(86_400);
    // Howard Hinnant's days-to-civil.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year}-{month:02}-{day:02} {:02}:{:02}",
        rest / 3600,
        rest % 3600 / 60
    )
}

fn bytes(n: u64) -> String {
    match n {
        n if n >= 1 << 30 => format!("{:.1} GB", n as f64 / f64::from(1u32 << 30)),
        n if n >= 1 << 20 => format!("{:.1} MB", n as f64 / f64::from(1u32 << 20)),
        n if n >= 1 << 10 => format!("{:.0} KB", n as f64 / 1024.0),
        n => format!("{n} B"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(flow: &Flow) -> Vec<Vec<String>> {
        let Some(Block::Sheet(s)) = flow.blocks.first() else {
            panic!("a sheet");
        };
        s.rows
            .iter()
            .map(|r| r.cells.iter().map(|c| c.text.clone()).collect())
            .collect()
    }

    fn tar_entry(name: &str, data: &[u8], kind: u8, mtime: u64) -> Vec<u8> {
        let mut h = [0u8; 512];
        h[..name.len()].copy_from_slice(name.as_bytes());
        h[100..108].copy_from_slice(b"0000644\0");
        h[124..136].copy_from_slice(format!("{:011o}\0", data.len()).as_bytes());
        h[136..148].copy_from_slice(format!("{mtime:011o}\0").as_bytes());
        h[156] = kind;
        h[257..263].copy_from_slice(b"ustar\0");
        h[148..156].copy_from_slice(b"        ");
        let sum: u32 = h.iter().map(|&b| u32::from(b)).sum();
        h[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
        let mut out = h.to_vec();
        out.extend_from_slice(data);
        out.resize(out.len().div_ceil(512) * 512, 0);
        out
    }

    #[test]
    fn a_tar_plain_and_gzipped_is_its_entries() {
        let mut tar = tar_entry("dir/", b"", b'5', 1_764_335_040);
        tar.extend(tar_entry("dir/a.txt", b"hello", b'0', 1_764_335_040));
        tar.extend([0u8; 1024]);
        let dir = std::env::temp_dir().join(format!("scour-listing-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let plain = dir.join("a.tar");
        std::fs::write(&plain, &tar).unwrap();
        let gz = dir.join("a.tgz");
        let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        std::io::Write::write_all(&mut enc, &tar).unwrap();
        std::fs::write(&gz, enc.finish().unwrap()).unwrap();
        for p in [&plain, &gz] {
            let flow = listing(File::open(p).unwrap()).unwrap();
            assert_eq!(
                texts(&flow),
                [
                    vec!["Name", "Size", "Modified"],
                    vec!["dir/", "", "2025-11-28 13:04"],
                    vec!["dir/a.txt", "5 B", "2025-11-28 13:04"],
                ]
            );
        }
        let _ = std::fs::remove_file(&plain);
        let _ = std::fs::remove_file(&gz);
        let _ = std::fs::remove_dir(&dir);
    }

    #[test]
    fn a_zip_is_its_directory_dates_and_all() {
        // One stored file, `a.txt`, written 2026-11-28 13:04.
        let (date, time): (u16, u16) =
            (((2026 - 1980) << 9) | (11 << 5) | 28, (13 << 11) | (4 << 5));
        let mut zip = Vec::new();
        let mut local = b"PK\x03\x04\x14\0\0\0\0\0".to_vec();
        local.extend(time.to_le_bytes());
        local.extend(date.to_le_bytes());
        local.extend([0u8; 4]); // crc, not read
        local.extend(2u32.to_le_bytes());
        local.extend(2u32.to_le_bytes());
        local.extend(5u16.to_le_bytes());
        local.extend(0u16.to_le_bytes());
        local.extend(b"a.txt");
        local.extend(b"hi");
        let cd_at = local.len() as u32;
        zip.extend(&local);
        let mut cd = b"PK\x01\x02\x14\0\x14\0\0\0\0\0".to_vec();
        cd.extend(time.to_le_bytes());
        cd.extend(date.to_le_bytes());
        cd.extend([0u8; 4]);
        cd.extend(2u32.to_le_bytes());
        cd.extend(2u32.to_le_bytes());
        cd.extend(5u16.to_le_bytes());
        cd.extend([0u8; 12]); // extra, comment, disk, attributes
        cd.extend(0u32.to_le_bytes());
        cd.extend(b"a.txt");
        zip.extend(&cd);
        zip.extend(b"PK\x05\x06\0\0\0\0\x01\0\x01\0");
        zip.extend((cd.len() as u32).to_le_bytes());
        zip.extend(cd_at.to_le_bytes());
        zip.extend(0u16.to_le_bytes());
        let path = std::env::temp_dir().join(format!("scour-listing-{}.zip", std::process::id()));
        std::fs::write(&path, &zip).unwrap();
        let flow = listing(File::open(&path).unwrap()).unwrap();
        let _ = std::fs::remove_file(&path);
        assert_eq!(texts(&flow)[1], ["a.txt", "2 B", "2026-11-28 13:04"]);
    }

    #[test]
    fn dates_as_dos_and_unix_write_them() {
        // 2026-11-28 13:04, as DOS packs it.
        let date = ((2026 - 1980) << 9) | (11 << 5) | 28;
        let time = (13 << 11) | (4 << 5);
        assert_eq!(dos_time((date << 16) | time), "2026-11-28 13:04");
        assert_eq!(unix_time(0), "");
        assert_eq!(unix_time(951_782_400), "2000-02-29 00:00");
    }
}
