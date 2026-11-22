//! Windows native PDF page inspection and rasterization via `Windows.Data.Pdf`.
//! Runs on a dedicated MTA thread (`RoInitialize`) so WinRT async `.join()`
//! never deadlocks an STA UI or shell thread.

use std::path::Path;

use crate::doc::DocInfo;
use crate::ppm::PageImage;

/// Normalize a path into the Win32 backslash form `StorageFile` requires.
#[cfg_attr(not(windows), allow(dead_code))]
fn win32_path(pdf: &Path) -> String {
    let s = pdf.to_string_lossy().replace('/', r"\");
    if let Some(unc) = s.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{unc}")
    } else if let Some(plain) = s.strip_prefix(r"\\?\") {
        plain.to_owned()
    } else {
        s
    }
}

#[cfg(windows)]
mod mta {
    use std::path::PathBuf;
    use std::sync::mpsc::{self, SyncSender};
    use std::sync::{Mutex, OnceLock};

    use crate::doc::DocInfo;
    use crate::ppm::PageImage;

    pub enum Ask {
        Inspect(PathBuf, SyncSender<Option<DocInfo>>),
        Render(PathBuf, u32, u32, SyncSender<Option<PageImage>>),
    }

    pub fn send(ask: Ask) {
        static TX: OnceLock<Mutex<mpsc::Sender<Ask>>> = OnceLock::new();
        let lock = TX.get_or_init(|| {
            let (tx, rx) = mpsc::channel::<Ask>();
            let _ = std::thread::Builder::new()
                .name("scour-winpdf".into())
                .spawn(move || {
                    // SAFETY: initializes the WinRT MTA apartment for this dedicated worker.
                    unsafe {
                        let _ = windows::Win32::System::WinRT::RoInitialize(
                            windows::Win32::System::WinRT::RO_INIT_MULTITHREADED,
                        );
                    }
                    while let Ok(job) = rx.recv() {
                        match job {
                            Ask::Inspect(p, reply) => {
                                let _ = reply.send(super::inspect_mta(&p));
                            }
                            Ask::Render(p, page, w, reply) => {
                                let _ = reply.send(super::render_mta(&p, page, w));
                            }
                        }
                    }
                });
            Mutex::new(tx)
        });
        if let Ok(tx) = lock.lock() {
            let _ = tx.send(ask);
        }
    }
}

#[cfg(windows)]
pub fn inspect(pdf: &Path) -> Option<DocInfo> {
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    mta::send(mta::Ask::Inspect(pdf.to_path_buf(), tx));
    rx.recv().ok().flatten()
}

#[cfg(windows)]
pub fn render_page(pdf: &Path, page_num: u32, width_px: u32) -> Option<PageImage> {
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    mta::send(mta::Ask::Render(pdf.to_path_buf(), page_num, width_px, tx));
    rx.recv().ok().flatten()
}

#[cfg(windows)]
fn inspect_mta(pdf: &Path) -> Option<DocInfo> {
    use windows::Data::Pdf::PdfDocument;
    use windows::Storage::StorageFile;
    use windows::core::HSTRING;

    let clean = win32_path(pdf);
    let path = HSTRING::from(clean.as_str());
    let file = StorageFile::GetFileFromPathAsync(&path).ok()?.join().ok()?;
    let doc = PdfDocument::LoadFromFileAsync(&file).ok()?.join().ok()?;
    let pages = doc.PageCount().ok()?;
    if pages == 0 {
        return None;
    }
    let aspect = doc
        .GetPage(0)
        .ok()
        .and_then(|p| p.Size().ok())
        .filter(|s| s.Width > 1.0 && s.Height > 1.0)
        .map_or(1.414, |s| (s.Height / s.Width).clamp(0.25, 4.0));
    Some(DocInfo { pages, aspect })
}

#[cfg(windows)]
fn render_mta(pdf: &Path, page_num: u32, width_px: u32) -> Option<PageImage> {
    use windows::Data::Pdf::{PdfDocument, PdfPageRenderOptions};
    use windows::Graphics::Imaging::{
        BitmapAlphaMode, BitmapDecoder, BitmapPixelFormat, BitmapTransform, ColorManagementMode,
        ExifOrientationMode,
    };
    use windows::Storage::StorageFile;
    use windows::Storage::Streams::InMemoryRandomAccessStream;
    use windows::core::HSTRING;

    if page_num == 0 {
        return None;
    }
    let clean = win32_path(pdf);
    let path = HSTRING::from(clean.as_str());
    let file = StorageFile::GetFileFromPathAsync(&path).ok()?.join().ok()?;
    let doc = PdfDocument::LoadFromFileAsync(&file).ok()?.join().ok()?;
    if page_num > doc.PageCount().ok()? {
        return None;
    }
    let page = doc.GetPage(page_num - 1).ok()?;
    let stream = InMemoryRandomAccessStream::new().ok()?;
    let opts = PdfPageRenderOptions::new().ok()?;
    opts.SetDestinationWidth(width_px.clamp(64, 2048)).ok()?;
    page.RenderWithOptionsToStreamAsync(&stream, &opts)
        .ok()?
        .join()
        .ok()?;
    stream.Seek(0).ok()?;

    let decoder = BitmapDecoder::CreateAsync(&stream).ok()?.join().ok()?;
    let width = decoder.PixelWidth().ok()?;
    let height = decoder.PixelHeight().ok()?;
    let transform = BitmapTransform::new().ok()?;
    let data = decoder
        .GetPixelDataTransformedAsync(
            BitmapPixelFormat::Rgba8,
            BitmapAlphaMode::Straight,
            &transform,
            ExifOrientationMode::IgnoreExifOrientation,
            ColorManagementMode::DoNotColorManage,
        )
        .ok()?
        .join()
        .ok()?;
    let rgba: Vec<u8> = data.DetachPixelData().ok()?.to_vec();
    let expected = (width as usize)
        .checked_mul(height as usize)?
        .checked_mul(4)?;
    (width > 0 && height > 0 && rgba.len() == expected).then_some(PageImage {
        width,
        height,
        rgba,
    })
}

#[cfg(not(windows))]
#[allow(dead_code)]
pub fn inspect(_: &Path) -> Option<DocInfo> {
    None
}

#[cfg(not(windows))]
#[allow(dead_code)]
pub fn render_page(_: &Path, _: u32, _: u32) -> Option<PageImage> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_slashes_and_verbatim_prefixes_for_winrt() {
        assert_eq!(
            win32_path(Path::new("C:/Users/a/b.pdf")),
            r"C:\Users\a\b.pdf"
        );
        assert_eq!(win32_path(Path::new(r"\\?\C:\a\b.pdf")), r"C:\a\b.pdf");
        assert_eq!(
            win32_path(Path::new(r"\\?\UNC\srv\share\b.pdf")),
            r"\\srv\share\b.pdf"
        );
    }
}
