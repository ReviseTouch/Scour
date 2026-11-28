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
    scour_formats::of_path(path).is_some_and(|f| f.flows && matches!(f.like, "zip" | "tar" | "rar"))
}

/// The listing of the archive opened as `file`.
pub fn listing(mut file: File) -> Result<Flow, String> {
    let mut head = [0u8; 8];
    let got = file.read(&mut head).map_err(|e| e.to_string())?;
    let len = file.metadata().map_err(|e| e.to_string())?.len();
    let mut rows = Vec::new();
    let cut = if head[..got].starts_with(b"PK") {
        zip(&mut file, len, &mut rows)?
    } else if head[..got].starts_with(b"Rar!\x1a\x07") {
        rar(&mut file, len, head.get(6) == Some(&1), &mut rows)?
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

/// A RAR archive's file headers, read and passed over block by block: RAR5's
/// when `five`, else RAR 1.5–4's. Headers are not packed, so nothing is
/// unpacked; an archive whose headers are encrypted lists nothing.
fn rar(file: &mut File, len: u64, five: bool, rows: &mut Vec<GridRow>) -> Result<bool, String> {
    use std::io::{Seek, SeekFrom};
    let mut at: u64 = if five { 8 } else { 7 };
    let mut buf = Vec::new();
    while at < len {
        if rows.len() >= MOST_ROWS {
            return Ok(true);
        }
        // A header in the first 8 KB read; one longer — a name runs to 64 KB —
        // read again whole.
        let mut next = None;
        for want in [8_192u64, 70_000] {
            file.seek(SeekFrom::Start(at)).map_err(|e| e.to_string())?;
            buf.resize((len - at).min(want) as usize, 0);
            let n = read_up_to(file, &mut buf)?;
            let b = &buf[..n];
            let read = if five {
                rar5_block(b, rows)
            } else {
                rar4_block(b, rows)
            };
            match read {
                Err(e) if e == "cut short" && (n as u64) < len - at => continue,
                other => {
                    next = other?;
                    break;
                }
            }
        }
        match next {
            Some(step) if step > 0 => at += step,
            _ => break,
        }
    }
    if rows.is_empty() {
        return Err("nothing listed".into());
    }
    Ok(false)
}

fn read_up_to(file: &mut File, buf: &mut [u8]) -> Result<usize, String> {
    let mut n = 0;
    while n < buf.len() {
        match file.read(&mut buf[n..]) {
            Ok(0) => break,
            Ok(k) => n += k,
            Err(e) => return Err(e.to_string()),
        }
    }
    Ok(n)
}

/// A RAR5 variable-length number, and the bytes it took.
fn vint(b: &[u8]) -> Option<(u64, usize)> {
    let mut n = 0u64;
    for (i, &byte) in b.iter().enumerate().take(10) {
        n |= u64::from(byte & 0x7f) << (7 * i);
        if byte & 0x80 == 0 {
            return Some((n, i + 1));
        }
    }
    None
}

/// One RAR5 block: a file's row if it is one; how far the next block is.
fn rar5_block(b: &[u8], rows: &mut Vec<GridRow>) -> Result<Option<u64>, String> {
    let (size, size_len) = vint(b.get(4..).unwrap_or_default()).ok_or("cut short")?;
    let start = 4 + size_len;
    let head = b.get(start..start + size as usize).ok_or("cut short")?;
    let mut at = 0;
    let next = |at: &mut usize| -> Result<u64, String> {
        let (v, n) = vint(head.get(*at..).unwrap_or_default()).ok_or("cut short")?;
        *at += n;
        Ok(v)
    };
    let kind = next(&mut at)?;
    let flags = next(&mut at)?;
    if flags & 1 != 0 {
        next(&mut at)?;
    }
    let data = if flags & 2 != 0 { next(&mut at)? } else { 0 };
    match kind {
        // Encrypted headers: what the archive holds is not to be read.
        4 => return Err("its names are encrypted".into()),
        5 => return Ok(None),
        2 => {
            let file_flags = next(&mut at)?;
            let size = next(&mut at)?;
            next(&mut at)?; // attributes
            let mtime = if file_flags & 2 != 0 {
                let t = head.get(at..at + 4).ok_or("cut short")?;
                at += 4;
                i64::from(u32::from_le_bytes([t[0], t[1], t[2], t[3]]))
            } else {
                0
            };
            if file_flags & 4 != 0 {
                at += 4;
            }
            next(&mut at)?; // compression
            next(&mut at)?; // host
            let name_len = next(&mut at)? as usize;
            let name = head.get(at..at + name_len).ok_or("cut short")?;
            let name = String::from_utf8_lossy(name);
            let folder = file_flags & 1 != 0;
            rows.push(row(&name, Some(size), unix_time(mtime), folder));
        }
        _ => {}
    }
    Ok(Some((start as u64) + size + data))
}

/// One RAR 1.5–4 block: a file's row if it is one; how far the next block is.
fn rar4_block(b: &[u8], rows: &mut Vec<GridRow>) -> Result<Option<u64>, String> {
    let h = b.get(..7).ok_or("cut short")?;
    let kind = h[2];
    let flags = u16::from_le_bytes([h[3], h[4]]);
    let size = u64::from(u16::from_le_bytes([h[5], h[6]]));
    if size < 7 {
        return Ok(None);
    }
    let add = if flags & 0x8000 != 0 {
        let a = b.get(7..11).ok_or("cut short")?;
        u64::from(u32::from_le_bytes([a[0], a[1], a[2], a[3]]))
    } else {
        0
    };
    match kind {
        // The main header: a flag says its names are encrypted.
        0x73 if flags & 0x80 != 0 => return Err("its names are encrypted".into()),
        0x7b => return Ok(None),
        0x74 => {
            let f = b.get(7..size as usize).ok_or("cut short")?;
            let pack = u64::from(u32::from_le_bytes([f[0], f[1], f[2], f[3]]));
            let mut unpacked = u64::from(u32::from_le_bytes([f[4], f[5], f[6], f[7]]));
            let time = u32::from_le_bytes([f[13], f[14], f[15], f[16]]);
            let name_len = usize::from(u16::from_le_bytes([f[19], f[20]]));
            let mut at = 25;
            let mut high_pack = 0;
            if flags & 0x100 != 0 {
                let hi = f.get(at..at + 8).ok_or("cut short")?;
                high_pack = u64::from(u32::from_le_bytes([hi[0], hi[1], hi[2], hi[3]]));
                unpacked |= u64::from(u32::from_le_bytes([hi[4], hi[5], hi[6], hi[7]])) << 32;
                at += 8;
            }
            let name = f.get(at..at + name_len).ok_or("cut short")?;
            // A Unicode name follows its old-style one after a zero.
            let name = match name.iter().position(|&c| c == 0) {
                Some(z) => &name[..z],
                None => name,
            };
            let name = String::from_utf8(name.to_vec())
                .unwrap_or_else(|_| name.iter().map(|&c| crate::cp1252_char(c)).collect())
                .replace('\\', "/");
            let folder = flags & 0xe0 == 0xe0;
            rows.push(row(&name, Some(unpacked), dos_time(time), folder));
            return Ok(Some(size + (pack | (high_pack << 32))));
        }
        _ => {}
    }
    Ok(Some(size + add))
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
    fn a_rar5_is_its_file_headers() {
        let mut v = b"Rar!\x1a\x07\x01\x00".to_vec();
        let block = |body: &[u8]| {
            let mut out = vec![0u8; 4]; // crc, not checked
            out.push(body.len() as u8);
            out.extend_from_slice(body);
            out
        };
        v.extend(block(&[1, 0, 0])); // main header
        let mut file = vec![2, 2, 5]; // a file, with 5 bytes of data
        file.extend([2, 11, 0]); // has a time; 11 bytes; attributes
        file.extend(1_764_335_040u32.to_le_bytes());
        file.extend([0, 0, 5]);
        file.extend(b"a.txt");
        v.extend(block(&file));
        v.extend(b"hello");
        v.extend(block(&[5, 0, 0])); // end
        let path = std::env::temp_dir().join(format!("scour-listing-{}.rar", std::process::id()));
        std::fs::write(&path, &v).unwrap();
        let flow = listing(File::open(&path).unwrap()).unwrap();
        let _ = std::fs::remove_file(&path);
        assert_eq!(texts(&flow)[1], ["a.txt", "11 B", "2025-11-28 13:04"]);
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
