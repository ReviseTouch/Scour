//! The table. One row per extension, sorted, each extension once — which
//! `the_table_is_sorted_and_holds_each_extension_once` holds it to.

use crate::{Format, Read, Shown};

/// A row that says nothing yet: shown by its bytes, read as text.
const fn f(ext: &'static str) -> Format {
    Format {
        ext,
        mime: None,
        shown: Shown::Sniffed,
        thumb: false,
        direct: false,
        converts: false,
        flows: false,
        read: Read::Bytes,
        zip: false,
        markup: false,
        binary: false,
    }
}

impl Format {
    /// A picture a face draws whole.
    const fn whole(self, mime: &'static str) -> Format {
        Format {
            mime: Some(mime),
            shown: Shown::Whole,
            ..self
        }
    }
    /// A PDF, audio or video a face streams.
    const fn streamed(self, mime: &'static str) -> Format {
        Format {
            mime: Some(mime),
            shown: Shown::Streamed,
            ..self
        }
    }
    /// A type the desktop's thumbnailers are asked for by.
    const fn thumb(self, mime: &'static str) -> Format {
        Format {
            mime: Some(mime),
            thumb: true,
            ..self
        }
    }
    const fn thumbed(self) -> Format {
        Format {
            thumb: true,
            ..self
        }
    }
    const fn direct(self) -> Format {
        Format {
            direct: true,
            ..self
        }
    }
    const fn converts(self) -> Format {
        Format {
            converts: true,
            ..self
        }
    }
    const fn flows(self) -> Format {
        Format {
            flows: true,
            ..self
        }
    }
    const fn here(self) -> Format {
        Format {
            read: Read::Here,
            ..self
        }
    }
    const fn extractor(self, name: &'static str) -> Format {
        Format {
            read: Read::Extractor(name),
            ..self
        }
    }
    const fn zip(self) -> Format {
        Format { zip: true, ..self }
    }
    const fn markup(self) -> Format {
        Format {
            markup: true,
            ..self
        }
    }
    const fn binary(self) -> Format {
        Format {
            binary: true,
            ..self
        }
    }
}

/// Every extension Scour does something with beyond reading its bytes.
pub static FORMATS: &[Format] = &[
    f("7z").binary(),
    f("a").binary(),
    f("aac").streamed("audio/aac"),
    f("avi").thumb("video/x-msvideo").binary(),
    f("avif").whole("image/avif").thumbed().binary(),
    f("bmp").whole("image/bmp").thumbed().binary(),
    f("bz2").binary(),
    f("class").binary(),
    f("db").binary(),
    f("dex").binary(),
    f("dll").binary(),
    f("dmg").binary(),
    f("doc").converts().flows().extractor("doc"),
    f("docx").converts().flows().here().zip(),
    f("dot").extractor("doc"),
    f("dylib").binary(),
    f("eml").extractor("eml"),
    f("eot").binary(),
    f("epub").zip(),
    f("exe").binary(),
    f("flac").streamed("audio/flac").binary(),
    f("gif").whole("image/gif").thumbed().binary(),
    f("gz").binary(),
    f("heic").thumb("image/heic"),
    f("heif").thumb("image/heif"),
    f("htm").markup(),
    f("html").markup(),
    f("ico").whole("image/x-icon").binary(),
    f("img").binary(),
    f("iso").binary(),
    f("jar").zip(),
    f("jfif").whole("image/jpeg").thumbed().direct(),
    f("jpeg").whole("image/jpeg").thumbed().direct().binary(),
    f("jpg").whole("image/jpeg").thumbed().direct().binary(),
    f("jxl").thumb("image/jxl"),
    f("lib").binary(),
    f("lz4").binary(),
    f("m4a").streamed("audio/mp4").binary(),
    f("m4v").streamed("video/mp4").thumbed(),
    f("mkv").streamed("video/x-matroska").thumbed().binary(),
    f("mov").streamed("video/quicktime").thumbed().binary(),
    f("mp3").streamed("audio/mpeg").binary(),
    f("mp4").streamed("video/mp4").thumbed().binary(),
    f("mpeg").thumb("video/mpeg"),
    f("mpg").thumb("video/mpeg"),
    f("ncx").markup(),
    f("o").binary(),
    f("obj").binary(),
    f("odp").converts().flows().here().zip(),
    f("ods").converts().flows().here().zip(),
    f("odt").converts().flows().here().zip(),
    f("oga").streamed("audio/ogg"),
    f("ogg").streamed("audio/ogg").binary(),
    f("ogv").streamed("video/ogg"),
    f("opf").markup(),
    f("opus").streamed("audio/ogg").binary(),
    f("otf").binary(),
    f("parquet").binary(),
    f("pdb").binary(),
    f("pdf")
        .streamed("application/pdf")
        .thumbed()
        .extractor("pdf")
        .binary(),
    f("png").whole("image/png").thumbed().direct().binary(),
    f("pot").extractor("ppt"),
    f("pps").extractor("ppt"),
    f("ppt").converts().flows().extractor("ppt"),
    f("pptx").converts().flows().here().zip(),
    f("pyc").binary(),
    f("rar").binary(),
    f("rlib").binary(),
    f("rmeta").binary(),
    f("rtf").converts().here(),
    f("so").binary(),
    f("sqlite").binary(),
    f("svg").whole("image/svg+xml").thumbed(),
    f("tif").thumb("image/tiff").binary(),
    f("tiff").thumb("image/tiff").binary(),
    f("ttf").binary(),
    f("udf").here().zip(),
    f("wasm").binary(),
    f("wav").streamed("audio/wav").binary(),
    f("webm").streamed("video/webm").thumbed().binary(),
    f("webp").whole("image/webp").thumbed().binary(),
    f("whl").zip(),
    f("woff").binary(),
    f("woff2").binary(),
    f("xhtml").markup(),
    f("xls").converts().flows().extractor("xls"),
    f("xlsx").converts().flows().here().zip(),
    f("xlt").extractor("xls"),
    f("xz").binary(),
    f("zip").zip(),
    f("zst").binary(),
];
