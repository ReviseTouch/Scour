//! The table. One row per extension, sorted, each extension once — which
//! `the_table_is_sorted_and_holds_each_extension_once` holds it to.

use crate::{Format, Read, Shown};

/// A row that says nothing yet: shown by its bytes, read as text.
const fn f(ext: &'static str) -> Format {
    Format {
        ext,
        like: ext,
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
    /// Read by `ext`'s reader.
    const fn like(self, ext: &'static str) -> Format {
        Format { like: ext, ..self }
    }
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
    f("bmp").whole("image/bmp").thumbed().direct().binary(),
    f("bz2").binary(),
    f("class").binary(),
    // Separated values, drawn as a sheet.
    f("csv").flows(),
    f("db").binary(),
    f("dex").binary(),
    f("dll").binary(),
    f("dmg").binary(),
    f("doc").converts().flows().extractor("doc"),
    // A macro-enabled document and the templates are a document's container.
    f("docm").like("docx").converts().flows().here().zip(),
    f("docx").converts().flows().here().zip(),
    // Graphviz writes `.dot` too, gettext `.pot`: a legacy template is
    // handed over only when its bytes are OLE2, never drawn by its name.
    f("dot").extractor("doc"),
    f("dotm").like("docx").converts().flows().here().zip(),
    f("dotx").like("docx").converts().flows().here().zip(),
    f("dylib").binary(),
    // A message, drawn: who to whom, its body as a page, what is attached.
    f("eml").flows().extractor("eml"),
    f("eot").binary(),
    // An e-book, drawn chapter by chapter in its reading order.
    f("epub").flows().zip(),
    f("exe").binary(),
    f("flac").streamed("audio/flac").binary(),
    // OpenDocument as one XML file, no zip: its words between its tags.
    f("fodp").like("fodt").converts().flows().here(),
    f("fods").like("fodt").converts().flows().here(),
    f("fodt").converts().flows().here(),
    f("gif").whole("image/gif").thumbed().direct().binary(),
    // An archive, drawn as what it holds; a gzip is read as a tar inside.
    f("gz").like("tar").flows().binary(),
    f("heic").thumb("image/heic"),
    f("heif").thumb("image/heif"),
    // A page, drawn as its words: never its own markup, never its scripts.
    f("htm").like("html").flows().markup(),
    f("html").flows().markup(),
    f("ico").whole("image/x-icon").direct().binary(),
    f("img").binary(),
    // A notebook: its cells, and what they printed.
    f("ipynb").flows(),
    f("iso").binary(),
    f("jar").like("zip").flows().zip(),
    f("jfif").whole("image/jpeg").thumbed().direct(),
    f("jpeg").whole("image/jpeg").thumbed().direct().binary(),
    f("jpg").whole("image/jpeg").thumbed().direct().binary(),
    f("jxl").thumb("image/jxl"),
    f("lib").binary(),
    f("lz4").binary(),
    f("m4a").streamed("audio/mp4").binary(),
    f("m4v").streamed("video/mp4").thumbed(),
    f("markdown").like("md").flows(),
    // Markdown, drawn as it reads: headings, lists, tables, code.
    f("md").flows(),
    f("mkv").streamed("video/x-matroska").thumbed().binary(),
    f("mov").streamed("video/quicktime").thumbed().binary(),
    f("mp3").streamed("audio/mpeg").binary(),
    f("mp4").streamed("video/mp4").thumbed().binary(),
    f("mpeg").thumb("video/mpeg"),
    f("mpg").thumb("video/mpeg"),
    // An Outlook message: drawn as an e-mail is, its words handed over.
    f("msg").flows().extractor("msg"),
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
    // OpenDocument's templates: its documents' containers.
    f("otp").like("odp").converts().flows().here().zip(),
    f("ots").like("ods").converts().flows().here().zip(),
    f("ott").like("odt").converts().flows().here().zip(),
    f("parquet").binary(),
    f("pdb").binary(),
    f("pdf")
        .streamed("application/pdf")
        .thumbed()
        .extractor("pdf")
        .binary(),
    f("png").whole("image/png").thumbed().direct().binary(),
    f("pot").extractor("ppt"),
    f("potm").like("pptx").converts().flows().here().zip(),
    f("potx").like("pptx").converts().flows().here().zip(),
    f("pps").extractor("ppt"),
    f("ppsm").like("pptx").converts().flows().here().zip(),
    f("ppsx").like("pptx").converts().flows().here().zip(),
    f("ppt").converts().flows().extractor("ppt"),
    f("pptm").like("pptx").converts().flows().here().zip(),
    f("pptx").converts().flows().here().zip(),
    f("pyc").binary(),
    f("rar").flows().binary(),
    f("rlib").binary(),
    f("rmeta").binary(),
    f("rtf").converts().flows().here(),
    f("so").binary(),
    f("sqlite").binary(),
    f("svg").whole("image/svg+xml").thumbed(),
    f("tar").flows(),
    f("tgz").like("tar").flows(),
    f("tif").thumb("image/tiff").direct().binary(),
    f("tiff").thumb("image/tiff").direct().binary(),
    f("tsv").like("csv").flows(),
    f("ttf").binary(),
    f("udf").here().zip(),
    f("wasm").binary(),
    f("wav").streamed("audio/wav").binary(),
    f("webm").streamed("video/webm").thumbed().binary(),
    f("webp").whole("image/webp").thumbed().direct().binary(),
    f("whl").like("zip").flows().zip(),
    f("woff").binary(),
    f("woff2").binary(),
    f("xhtml").like("html").flows().markup(),
    f("xls").converts().flows().extractor("xls"),
    f("xlsm").like("xlsx").converts().flows().here().zip(),
    f("xlsx").converts().flows().here().zip(),
    f("xlt").extractor("xls"),
    f("xltm").like("xlsx").converts().flows().here().zip(),
    f("xltx").like("xlsx").converts().flows().here().zip(),
    f("xz").binary(),
    f("zip").flows().zip(),
    f("zst").binary(),
];
