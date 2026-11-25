//! The single-byte code pages a Turkish machine's older files are written in:
//! Windows-1254, and Windows-1252 for what RTF calls `\ansicpg1252`.

const U: char = '\u{FFFD}';

/// 0x80–0x9F of 1254; 0xA0–0xFF is Latin-1 but for six Turkish letters.
const C1_1254: [char; 32] = [
    '€', U, '‚', 'ƒ', '„', '…', '†', '‡', 'ˆ', '‰', 'Š', '‹', 'Œ', U, U, U, //
    U, '‘', '’', '“', '”', '•', '–', '—', '˜', '™', 'š', '›', 'œ', U, U, 'Ÿ',
];

/// One byte of Windows-1254 as a character; an unassigned byte is `U+FFFD`.
pub fn cp1254_char(b: u8) -> char {
    match b {
        0..=0x7F => b as char,
        0x80..=0x9F => C1_1254[(b - 0x80) as usize],
        0xD0 => 'Ğ',
        0xDD => 'İ',
        0xDE => 'Ş',
        0xF0 => 'ğ',
        0xFD => 'ı',
        0xFE => 'ş',
        _ => b as char,
    }
}

/// One byte of Windows-1252: 1254 with Latin-1's letters where 1254 has Turkish ones.
pub fn cp1252_char(b: u8) -> char {
    match b {
        0x8E => 'Ž',
        0x9E => 'ž',
        0xD0 | 0xDD | 0xDE | 0xF0 | 0xFD | 0xFE => b as char,
        _ => cp1254_char(b),
    }
}

/// The byte 1254 writes `c` as, if it can.
pub fn cp1254_byte(c: char) -> Option<u8> {
    if c.is_ascii() {
        return Some(c as u8);
    }
    (0x80..=0xFFu8).find(|&b| cp1254_char(b) == c)
}

/// Bytes of 1254 as text.
pub fn cp1254_decode(bytes: &[u8]) -> String {
    bytes.iter().map(|&b| cp1254_char(b)).collect()
}

/// A code page RTF names, as a decoder; any it does not know is read as 1252.
pub fn code_page(number: u32) -> fn(u8) -> char {
    match number {
        1254 | 857 => cp1254_char,
        _ => cp1252_char,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_turkish_letters_round_trip() {
        for c in "çğıöşüÇĞİÖŞÜ".chars() {
            let b = cp1254_byte(c).unwrap_or_else(|| panic!("{c} has no byte"));
            assert!(b >= 0x80, "{c}");
            assert_eq!(cp1254_char(b), c);
        }
        assert_eq!(cp1254_decode(b"\xFEu \xE7ok g\xFCzel"), "şu çok güzel");
        assert_eq!(cp1254_byte('Ž'), None);
        assert_eq!(cp1252_char(0xFE), 'þ');
    }
}
