//! What this machine shows documents with beyond Scour's own readers, so a
//! face can say so rather than leave somebody to guess why a PDF has no
//! pages here or Space opens nothing.

use serde::{Deserialize, Serialize};

/// The programs and parts of the system a preview uses here; each empty where
/// there is none.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Means {
    /// What draws a PDF's pages: the system's own, or a program on the path.
    pub pdf_pages: String,
    /// What turns Word, Excel and PowerPoint files into pages. Where there is
    /// none — always on Linux, where LibreOffice is not used — Scour draws
    /// them itself, or shows their text.
    pub office_pages: String,
    /// The desktop's own preview, opened with Space: the program's name.
    pub quick_look: String,
}

/// What is here. `quick_look` is the face's own: a command it was given, or
/// what [`crate::quicklook`] found.
pub fn means(quick_look: Option<&[String]>) -> Means {
    let program = quick_look
        .and_then(|cmd| cmd.first())
        .map(|p| {
            std::path::Path::new(p)
                .file_stem()
                .map_or_else(|| p.clone(), |s| s.to_string_lossy().into_owned())
        })
        .unwrap_or_default();
    Means {
        pdf_pages: crate::doc::pdf_renderer().unwrap_or_default().to_owned(),
        office_pages: crate::office::converter().unwrap_or_default().to_owned(),
        quick_look: program,
    }
}
