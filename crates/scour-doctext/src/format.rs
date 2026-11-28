//! Which reader a document needs: its first bytes first, its name second.

/// The kinds read here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Format {
    Pdf,
    /// Legacy Word.
    Doc,
    /// Legacy Excel.
    Xls,
    /// Legacy PowerPoint.
    Ppt,
    /// An RFC 822 message: `.eml`.
    Mail,
}

impl Format {
    /// From a document's first bytes and its extension. An OLE2 compound file
    /// says only that it is one; which of the three is the name's to say.
    pub fn of(head: &[u8], extension: &str) -> Option<Format> {
        if head.starts_with(b"%PDF-") {
            return Some(Format::Pdf);
        }
        // The reader the table names for the extension (`scour_formats`).
        let named = match scour_formats::of(extension).map(|f| f.read) {
            Some(scour_formats::Read::Extractor(name)) => Format::named(name),
            _ => None,
        };
        if head.starts_with(b"\xD0\xCF\x11\xE0\xA1\xB1\x1A\xE1") {
            return named.filter(|f| matches!(f, Format::Doc | Format::Xls | Format::Ppt));
        }
        named.filter(|f| *f == Format::Mail)
    }

    /// The name this format is known by on the wire between the two processes.
    pub fn name(self) -> &'static str {
        match self {
            Format::Pdf => "pdf",
            Format::Doc => "doc",
            Format::Xls => "xls",
            Format::Ppt => "ppt",
            Format::Mail => "eml",
        }
    }

    pub fn named(name: &str) -> Option<Format> {
        [
            Format::Pdf,
            Format::Doc,
            Format::Xls,
            Format::Ppt,
            Format::Mail,
        ]
        .into_iter()
        .find(|f| f.name() == name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bytes_first_then_the_name() {
        assert_eq!(Format::of(b"%PDF-1.4", "txt"), Some(Format::Pdf));
        let ole = b"\xD0\xCF\x11\xE0\xA1\xB1\x1A\xE1....";
        assert_eq!(Format::of(ole, "XLS"), Some(Format::Xls));
        assert_eq!(Format::of(ole, "msg"), None);
        assert_eq!(Format::of(b"From: a", "eml"), Some(Format::Mail));
        assert_eq!(Format::of(b"From: a", "txt"), None);
        for f in [
            Format::Pdf,
            Format::Doc,
            Format::Xls,
            Format::Ppt,
            Format::Mail,
        ] {
            assert_eq!(Format::named(f.name()), Some(f));
        }
    }
}
