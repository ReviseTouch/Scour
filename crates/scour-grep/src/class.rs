//! Which of five things a file is. What the caller knows comes first, so a
//! video is refused without being opened; what its first bytes say comes
//! next, and overrules a name.

/// How a file is read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Class {
    /// Lines of characters: read and searched as they are.
    Text,
    /// Its words are inside a structure — PDF, Office, RTF.
    Document,
    /// Machine code, databases: searched only when binary files are asked for.
    Binary,
    /// Pictures, sound, video, disk images: their bodies hold no words. Read
    /// only for a run of bytes.
    HeaderOnly,
    /// Compressed or packed files. ZIP is entered; the rest are binary.
    Archive,
}

/// What a file's first bytes say it is, beyond text or not: a ZIP or a
/// packed file is an archive, RTF and the unread kinds documents, media
/// header-only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Signature {
    Zip,
    Rtf,
    /// A document with no reader here yet: PDF, OLE2 (`.doc`, `.xls`, `.ppt`, `.msg`).
    Unread,
    Media,
    Packed,
}

/// Magic numbers at offset zero. Only those that change how a file is read:
/// an executable or a database is binary by its NULs already.
const AT_START: &[(&[u8], Signature)] = &[
    (b"PK\x03\x04", Signature::Zip),
    (b"{\\rtf", Signature::Rtf),
    (b"%PDF-", Signature::Unread),
    (b"\xD0\xCF\x11\xE0\xA1\xB1\x1A\xE1", Signature::Unread),
    (b"\x89PNG\r\n\x1A\n", Signature::Media),
    (b"\xFF\xD8\xFF", Signature::Media),
    (b"GIF87a", Signature::Media),
    (b"GIF89a", Signature::Media),
    (b"II*\x00", Signature::Media),
    (b"MM\x00*", Signature::Media),
    (b"RIFF", Signature::Media),
    (b"ID3", Signature::Media),
    (b"fLaC", Signature::Media),
    (b"OggS", Signature::Media),
    (b"\x1A\x45\xDF\xA3", Signature::Media),
    (b"QFI\xFB", Signature::Media),
    (b"conectix", Signature::Media),
    (b"KDMV", Signature::Media),
    (b"\x1F\x8B", Signature::Packed),
    (b"\xFD7zXZ\x00", Signature::Packed),
    (b"\x28\xB5\x2F\xFD", Signature::Packed),
    (b"7z\xBC\xAF\x27\x1C", Signature::Packed),
    (b"Rar!\x1A\x07", Signature::Packed),
    (b"BZh", Signature::Packed),
];

/// What `head`, a file's first bytes, says it is; None for text and for
/// anything without a signature here.
pub(crate) fn signature(head: &[u8]) -> Option<Signature> {
    if let Some((_, sig)) = AT_START.iter().find(|(magic, _)| head.starts_with(magic)) {
        return Some(*sig);
    }
    // ISO/MP4/HEIC box at offset four; ISO 9660 at sector sixteen.
    if head.get(4..8) == Some(b"ftyp") || head.get(0x8001..0x8006) == Some(b"CD001") {
        return Some(Signature::Media);
    }
    (head.get(257..262) == Some(b"ustar")).then_some(Signature::Packed)
}

/// Extensions of disk and VM images: an index files them with archives, but
/// their bodies are as wordless as a video's.
pub fn is_disk_image(extension: &str) -> bool {
    const IMAGES: &[&str] = &[
        "iso", "img", "vmdk", "vdi", "vhd", "vhdx", "qcow2", "qcow", "dmg", "wim", "esd",
    ];
    IMAGES.iter().any(|e| extension.eq_ignore_ascii_case(e))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_bytes_say_what_a_file_is() {
        assert_eq!(signature(b"%PDF-1.7\n"), Some(Signature::Unread));
        assert_eq!(signature(b"{\\rtf1\\ansi"), Some(Signature::Rtf));
        assert_eq!(signature(b"\x89PNG\r\n\x1A\n...."), Some(Signature::Media));
        assert_eq!(signature(b"\0\0\0\x18ftypmp42"), Some(Signature::Media));
        assert_eq!(signature(b"\x1F\x8B\x08"), Some(Signature::Packed));
        assert_eq!(signature(b"fn main() {}"), None);
        assert_eq!(signature(b"\x7FELF\x02"), None);
        let mut tar = vec![b'a'; 300];
        tar[257..262].copy_from_slice(b"ustar");
        assert_eq!(signature(&tar), Some(Signature::Packed));
        assert!(is_disk_image("ISO") && !is_disk_image("zip"));
    }
}
