use super::*;
use std::collections::BTreeSet;

fn set(exts: &[&str]) -> BTreeSet<String> {
    exts.iter().map(|e| (*e).to_owned()).collect()
}

fn rows(keep: impl Fn(&Format) -> bool) -> BTreeSet<String> {
    FORMATS
        .iter()
        .filter(|f| keep(f))
        .map(|f| f.ext.to_owned())
        .collect()
}

#[test]
fn the_table_is_sorted_and_holds_each_extension_once() {
    for pair in FORMATS.windows(2) {
        assert!(
            pair[0].ext < pair[1].ext,
            "{} before {}",
            pair[0].ext,
            pair[1].ext
        );
    }
    for f in FORMATS {
        assert_eq!(f.ext, f.ext.to_ascii_lowercase(), "{} is lowercase", f.ext);
        assert!(!f.ext.contains('.') && f.ext.len() <= 12, "{}", f.ext);
        // A face is told what it draws; a thumbnailer is looked up by type.
        if f.drawn() || f.thumb {
            assert!(f.mime.is_some(), "{} needs its type", f.ext);
        }
    }
}

#[test]
fn an_extension_is_found_in_any_case_and_nothing_else_is() {
    assert_eq!(of("PDF").map(|f| f.ext), Some("pdf"));
    assert_eq!(of_bytes(b"DocX").map(|f| f.ext), Some("docx"));
    assert_eq!(
        of_path(std::path::Path::new("/a/B.JPEG")).map(|f| f.ext),
        Some("jpeg")
    );
    assert!(of("").is_none() && of("txt").is_none() && of("pdfx").is_none());
    assert!(of("averyverylongextension").is_none());
    for f in FORMATS {
        assert_eq!(of(f.ext), Some(f));
    }
}

// **The lists the table replaced, as they stood (2026-11-28), each against the
// question now asked of it** — and what was added to them since, named where
// it was. A change here is a change in what Scour does with a file, and
// should read as one in the diff.

/// Added 2026-11-28: the macro-enabled documents and the templates of
/// Microsoft's formats (office_oxide reads them as their documents),
/// OpenDocument's templates and its flat XML. Not the legacy templates:
/// `.dot` is Graphviz's as often, `.pot` gettext's.
const VARIANTS: &[&str] = &[
    "docm", "dotm", "dotx", "xlsm", "xltm", "xltx", "pptm", "potm", "potx", "ppsm", "ppsx", "ott",
    "ots", "otp", "fodt", "fods", "fodp",
];
/// The variants that are zips read here, as their documents are.
const ZIPPED: &[&str] = &[
    "docm", "dotm", "dotx", "xlsm", "xltm", "xltx", "pptm", "potm", "potx", "ppsm", "ppsx", "ott",
    "ots", "otp",
];

fn with(old: &[&str], added: &[&str]) -> BTreeSet<String> {
    set(old).union(&set(added)).cloned().collect()
}

#[test]
fn a_variant_is_read_by_a_row_that_reads_itself() {
    for f in FORMATS {
        let like = of(f.like).unwrap_or_else(|| panic!("{} is like {}, not a row", f.ext, f.like));
        assert_eq!(
            like.like, like.ext,
            "{} → {} → {}",
            f.ext, f.like, like.like
        );
        assert_eq!(
            (f.flows, f.converts),
            (like.flows, like.converts),
            "{}",
            f.ext
        );
    }
    for ext in VARIANTS {
        assert!(of(ext).is_some(), "{ext}");
    }
}

#[test]
fn the_preview_draws_what_it_drew() {
    // scour-preview `shape::DRAWN`.
    let whole = [
        ("png", "image/png"),
        ("jpg", "image/jpeg"),
        ("jpeg", "image/jpeg"),
        ("jfif", "image/jpeg"),
        ("gif", "image/gif"),
        ("webp", "image/webp"),
        ("avif", "image/avif"),
        ("bmp", "image/bmp"),
        ("ico", "image/x-icon"),
        ("svg", "image/svg+xml"),
    ];
    let streamed = [
        ("pdf", "application/pdf"),
        ("mp3", "audio/mpeg"),
        ("m4a", "audio/mp4"),
        ("aac", "audio/aac"),
        ("oga", "audio/ogg"),
        ("ogg", "audio/ogg"),
        ("opus", "audio/ogg"),
        ("wav", "audio/wav"),
        ("flac", "audio/flac"),
        ("mp4", "video/mp4"),
        ("m4v", "video/mp4"),
        ("webm", "video/webm"),
        ("ogv", "video/ogg"),
        ("mov", "video/quicktime"),
        ("mkv", "video/x-matroska"),
    ];
    for (ext, mime) in whole {
        let f = of(ext).unwrap();
        assert_eq!((f.shown, f.mime), (Shown::Whole, Some(mime)), "{ext}");
    }
    for (ext, mime) in streamed {
        let f = of(ext).unwrap();
        assert_eq!((f.shown, f.mime), (Shown::Streamed, Some(mime)), "{ext}");
    }
    let drawn: Vec<&str> = whole.iter().chain(&streamed).map(|(e, _)| *e).collect();
    assert_eq!(rows(Format::drawn), set(&drawn));
}

#[test]
fn the_same_documents_are_made_into_pages_and_flow() {
    // scour-preview `office::OFFICE_EXTS`.
    assert_eq!(
        rows(|f| f.converts),
        with(
            &[
                "doc", "docx", "odt", "rtf", "ppt", "pptx", "odp", "xls", "xlsx", "ods"
            ],
            VARIANTS
        )
    );
    // scour-flow `flows`.
    assert_eq!(
        rows(|f| f.flows),
        with(
            &[
                "docx", "doc", "xlsx", "xls", "pptx", "ppt", "odt", "ods", "odp"
            ],
            &[
                VARIANTS,
                &[
                    "md", "markdown", "csv", "tsv", "html", "htm", "xhtml", "epub", "eml"
                ],
                &["zip", "jar", "whl", "tar", "tgz", "gz"],
            ]
            .concat()
        )
    );
}

#[test]
fn words_are_read_where_they_were() {
    // scour-extract `document::kind_of`.
    assert_eq!(
        rows(|f| f.read == Read::Here),
        with(
            &["docx", "odt", "ods", "odp", "pptx", "xlsx", "udf", "rtf"],
            &[ZIPPED, &["fodt", "fods", "fodp"]].concat()
        )
    );
    // scour-grep `texts::handed_over`.
    assert_eq!(
        rows(Format::handed_over),
        set(&[
            "pdf", "doc", "dot", "xls", "xlt", "ppt", "pps", "pot", "eml"
        ])
    );
    // scour-doctext `Format::of`: which of an OLE2 file's three a name says,
    // and a message by its name.
    for (ext, name) in [
        ("doc", "doc"),
        ("dot", "doc"),
        ("xls", "xls"),
        ("xlt", "xls"),
        ("ppt", "ppt"),
        ("pps", "ppt"),
        ("pot", "ppt"),
        ("eml", "eml"),
        ("pdf", "pdf"),
    ] {
        assert_eq!(of(ext).unwrap().read, Read::Extractor(name), "{ext}");
    }
}

#[test]
fn a_zip_is_opened_and_passed_by_as_it_was() {
    // scour-grep `zip::role`.
    assert_eq!(
        rows(|f| f.zip),
        with(
            &[
                "udf", "docx", "xlsx", "pptx", "odt", "ods", "odp", "epub", "zip", "jar", "whl"
            ],
            ZIPPED
        )
    );
    assert_eq!(
        rows(|f| f.markup),
        set(&["xhtml", "html", "htm", "opf", "ncx"])
    );
    assert_eq!(
        rows(|f| f.binary),
        set(&[
            "png", "jpg", "jpeg", "gif", "webp", "avif", "bmp", "ico", "tif", "tiff", "mp3", "mp4",
            "m4a", "mkv", "avi", "mov", "webm", "wav", "flac", "ogg", "opus", "woff", "woff2",
            "ttf", "otf", "eot", "so", "dll", "dylib", "exe", "o", "obj", "a", "lib", "rlib",
            "rmeta", "pdb", "class", "dex", "pyc", "wasm", "7z", "rar", "gz", "bz2", "xz", "zst",
            "lz4", "iso", "img", "dmg", "pdf", "db", "sqlite", "parquet",
        ])
    );
    // Each is one of the three at most, so their order did not matter.
    for f in FORMATS {
        assert!(
            u8::from(f.zip) + u8::from(f.markup) + u8::from(f.binary) <= 1,
            "{}",
            f.ext
        );
    }
}

#[test]
fn thumbnails_and_the_windows_own_pictures_are_the_same() {
    // scour-thumbs `known::fill_unix_fallbacks::CORE_EXTS`.
    let core = [
        ("png", "image/png"),
        ("jpg", "image/jpeg"),
        ("jpeg", "image/jpeg"),
        ("jfif", "image/jpeg"),
        ("webp", "image/webp"),
        ("gif", "image/gif"),
        ("bmp", "image/bmp"),
        ("tif", "image/tiff"),
        ("tiff", "image/tiff"),
        ("avif", "image/avif"),
        ("heic", "image/heic"),
        ("heif", "image/heif"),
        ("jxl", "image/jxl"),
        ("svg", "image/svg+xml"),
        ("pdf", "application/pdf"),
        ("mp4", "video/mp4"),
        ("m4v", "video/mp4"),
        ("mkv", "video/x-matroska"),
        ("webm", "video/webm"),
        ("mov", "video/quicktime"),
        ("avi", "video/x-msvideo"),
        ("mpeg", "video/mpeg"),
        ("mpg", "video/mpeg"),
    ];
    for (ext, mime) in core {
        assert_eq!(of(ext).unwrap().mime, Some(mime), "{ext}");
    }
    let exts: Vec<&str> = core.iter().map(|(e, _)| *e).collect();
    assert_eq!(rows(|f| f.thumb), set(&exts));
    // scour-gui `peeking::has_direct_image_ext`; and since 2026-11-28 the
    // pictures `image` decodes with its features turned on there.
    assert_eq!(
        rows(|f| f.direct),
        with(
            &["png", "jpg", "jpeg", "jfif"],
            &["gif", "webp", "bmp", "ico", "tif", "tiff"]
        )
    );
}

#[test]
fn the_printed_table_has_a_line_for_every_row() {
    let table = table();
    let lines: Vec<&str> = table.lines().collect();
    assert_eq!(lines.len(), FORMATS.len() + 1);
    assert!(lines[0].starts_with("ext"));
    for (line, f) in lines[1..].iter().zip(FORMATS) {
        assert_eq!(line.split_whitespace().next(), Some(f.ext));
    }
    assert!(table.contains("extractor:pdf") && table.contains("application/pdf"));
}
