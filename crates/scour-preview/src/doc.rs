//! Cross-platform multi-page PDF and Office document rendering into RGBA pixels.
//! Uses `CoreGraphics` on macOS, `Windows.Data.Pdf` on Windows, and Poppler
//! (`pdftoppm`), MuPDF (`mutool`), or Ghostscript (`gs`) on Linux/Unix.

use std::path::{Path, PathBuf};
use std::process::Command;

pub use crate::ppm::PageImage;

/// Page count and first-page height/width aspect ratio of a document.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DocInfo {
    pub pages: u32,
    pub aspect: f32,
}

/// A document ready to be rendered page by page.
#[derive(Debug, Clone)]
pub struct PreparedDoc {
    pub pdf_path: PathBuf,
    pub info: DocInfo,
}

/// Whether `path` can be previewed as a scrollable multi-page document.
pub fn is_paged(path: &Path) -> bool {
    if is_pdf(path) {
        return has_pdf_renderer();
    }
    crate::office::is_office(path) && crate::office::can_convert() && has_pdf_renderer()
}

/// Prepare a PDF or Office document and return its page count and aspect ratio.
pub fn prepare(path: &Path) -> Option<PreparedDoc> {
    let pdf_path = if is_pdf(path) {
        std::fs::metadata(path)
            .ok()?
            .is_file()
            .then(|| path.to_path_buf())?
    } else if crate::office::is_office(path) {
        crate::office::to_pdf(path)?
    } else {
        return None;
    };
    let info = inspect_pdf(&pdf_path)?;
    Some(PreparedDoc { pdf_path, info })
}

/// Render 1-based `page` of a prepared PDF at `width_px` into straight RGBA.
pub fn render_page(pdf: &Path, page: u32, width_px: u32) -> Option<PageImage> {
    #[cfg(target_os = "macos")]
    if let Some(img) = crate::pdf_macos::render_page(pdf, page, width_px) {
        return Some(img);
    }
    #[cfg(windows)]
    if let Some(img) = crate::pdf_win::render_page(pdf, page, width_px) {
        return Some(img);
    }
    render_via_pdftoppm(pdf, page, width_px)
        .or_else(|| render_via_mutool(pdf, page, width_px))
        .or_else(|| render_via_gs(pdf, page, width_px))
}

fn inspect_pdf(pdf: &Path) -> Option<DocInfo> {
    #[cfg(target_os = "macos")]
    if let Some(info) = crate::pdf_macos::inspect(pdf) {
        return Some(info);
    }
    #[cfg(windows)]
    if let Some(info) = crate::pdf_win::inspect(pdf) {
        return Some(info);
    }
    inspect_via_pdfinfo(pdf)
        .or_else(|| inspect_via_mutool(pdf))
        .or_else(|| inspect_via_gs(pdf))
}

fn has_pdf_renderer() -> bool {
    pdf_renderer().is_some()
}

/// What draws a PDF's pages here, by name: the system's own, or the first of
/// Poppler, MuPDF and Ghostscript found on the path.
pub fn pdf_renderer() -> Option<&'static str> {
    if cfg!(target_os = "macos") {
        return Some("CoreGraphics");
    }
    if cfg!(windows) {
        return Some("Windows.Data.Pdf");
    }
    [
        ("pdftoppm", "Poppler"),
        ("mutool", "MuPDF"),
        ("gs", "Ghostscript"),
    ]
    .into_iter()
    .find(|(bin, _)| crate::find_on_path(bin).is_some())
    .map(|(_, name)| name)
}

fn is_pdf(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("pdf"))
}

fn inspect_via_pdfinfo(pdf: &Path) -> Option<DocInfo> {
    let bin = crate::find_on_path("pdfinfo")?;
    let mut cmd = Command::new(bin);
    crate::office::no_window(&mut cmd);
    let out = cmd.arg(pdf).output().ok()?;
    out.status
        .success()
        .then(|| parse_pdfinfo(&String::from_utf8_lossy(&out.stdout)))
        .flatten()
}

fn inspect_via_mutool(pdf: &Path) -> Option<DocInfo> {
    let bin = crate::find_on_path("mutool")?;
    let mut cmd = Command::new(bin);
    crate::office::no_window(&mut cmd);
    let out = cmd.args(["info", "-M"]).arg(pdf).output().ok()?;
    out.status
        .success()
        .then(|| parse_mutool_info(&String::from_utf8_lossy(&out.stdout)))
        .flatten()
}

fn inspect_via_gs(pdf: &Path) -> Option<DocInfo> {
    let first = render_via_gs(pdf, 1, 240)?;
    let aspect = (first.height as f32 / first.width.max(1) as f32).clamp(0.25, 4.0);
    // Count pages via Ghostscript's `pdfpagecount` without rendering them.
    let bin = crate::find_on_path("gs")?;
    let mut cmd = Command::new(bin);
    crate::office::no_window(&mut cmd);
    let out = cmd
        .args(["-q", "-dNODISPLAY", "-dNOSAFER", "--permit-file-read="])
        .arg(pdf)
        .args(["-c", "(r) file runpdfbegin pdfpagecount = quit"])
        .arg(pdf)
        .output()
        .ok();
    let pages = out
        .filter(|o| o.status.success())
        .and_then(|o| {
            String::from_utf8_lossy(&o.stdout)
                .trim()
                .parse::<u32>()
                .ok()
        })
        .unwrap_or(1)
        .max(1);
    Some(DocInfo { pages, aspect })
}

fn parse_pdfinfo(text: &str) -> Option<DocInfo> {
    let mut pages = 0u32;
    let mut aspect = 1.414f32;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("Pages:") {
            pages = rest.trim().parse().unwrap_or(0);
        } else if let Some(rest) = line.strip_prefix("Page size:") {
            let nums: Vec<f32> = rest
                .split_whitespace()
                .filter_map(|w| w.parse::<f32>().ok())
                .collect();
            if nums.len() >= 2 && nums[0] > 1.0 && nums[1] > 1.0 {
                aspect = (nums[1] / nums[0]).clamp(0.25, 4.0);
            }
        }
    }
    (pages > 0).then_some(DocInfo { pages, aspect })
}

fn parse_mutool_info(text: &str) -> Option<DocInfo> {
    let mut pages = 0u32;
    let mut aspect = 1.414f32;
    for line in text.lines() {
        if let Some(rest) = line.trim().strip_prefix("Pages:") {
            pages = rest.trim().parse().unwrap_or(0);
        } else if line.contains('[')
            && line.contains(']')
            && let Some(start) = line.find('[')
            && let Some(end) = line[start..].find(']')
        {
            let nums: Vec<f32> = line[start + 1..start + end]
                .split_whitespace()
                .filter_map(|w| w.parse::<f32>().ok())
                .collect();
            if nums.len() >= 4 {
                let w = (nums[2] - nums[0]).abs();
                let h = (nums[3] - nums[1]).abs();
                if w > 1.0 && h > 1.0 {
                    aspect = (h / w).clamp(0.25, 4.0);
                }
            }
        }
    }
    (pages > 0).then_some(DocInfo { pages, aspect })
}

fn render_via_pdftoppm(pdf: &Path, page: u32, width_px: u32) -> Option<PageImage> {
    let bin = crate::find_on_path("pdftoppm")?;
    let page_s = page.max(1).to_string();
    let width_s = width_px.clamp(64, 2048).to_string();
    let mut cmd = Command::new(bin);
    crate::office::no_window(&mut cmd);
    let out = cmd
        .args(["-scale-to-x", &width_s, "-scale-to-y", "-1"])
        .args(["-f", &page_s, "-l", &page_s])
        .arg(pdf)
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| crate::ppm::parse_p6(&out.stdout))
        .flatten()
}

fn render_via_mutool(pdf: &Path, page: u32, width_px: u32) -> Option<PageImage> {
    let bin = crate::find_on_path("mutool")?;
    let page_s = page.max(1).to_string();
    let width_s = width_px.clamp(64, 2048).to_string();
    let mut cmd = Command::new(bin);
    crate::office::no_window(&mut cmd);
    let out = cmd
        .args(["draw", "-F", "ppm", "-w", &width_s, "-o", "-"])
        .arg(pdf)
        .arg(&page_s)
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| crate::ppm::parse_p6(&out.stdout))
        .flatten()
}

fn render_via_gs(pdf: &Path, page: u32, width_px: u32) -> Option<PageImage> {
    let bin = crate::find_on_path("gs")?;
    let dpi = ((width_px.clamp(64, 2048) as f32 / 8.27) * 1.0)
        .round()
        .clamp(36.0, 240.0) as u32;
    let first = format!("-dFirstPage={}", page.max(1));
    let last = format!("-dLastPage={}", page.max(1));
    let res = format!("-r{dpi}");
    let mut cmd = Command::new(bin);
    crate::office::no_window(&mut cmd);
    let out = cmd
        .args(["-q", "-dSAFER", "-dBATCH", "-dNOPAUSE", "-sDEVICE=ppmraw"])
        .args([
            first.as_str(),
            last.as_str(),
            res.as_str(),
            "-sOutputFile=-",
        ])
        .arg(pdf)
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| crate::ppm::parse_p6(&out.stdout))
        .flatten()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_pdfinfo_and_mutool_metadata() {
        let sample =
            "Title:          Test\nPages:          8\nPage size:      595.28 x 841.89 pts (A4)\n";
        let info = parse_pdfinfo(sample).expect("valid pdfinfo");
        assert_eq!(info.pages, 8);
        assert!((info.aspect - 1.414).abs() < 0.01);

        let mu = "Pages: 3\nMediaboxes (1):\n\t1\t[ 0 0 612 792 ]\n";
        let minfo = parse_mutool_info(mu).expect("valid mutool");
        assert_eq!(minfo.pages, 3);
        assert!((minfo.aspect - (792.0 / 612.0)).abs() < 0.01);
    }
}
