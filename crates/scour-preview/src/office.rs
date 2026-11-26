//! Headless Office-to-PDF conversion for document previewing: LibreOffice on
//! every OS (with an isolated profile), plus Microsoft Office COM export on
//! Windows when LibreOffice is absent.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant, UNIX_EPOCH};

const OFFICE_EXTS: &[&str] = &[
    "doc", "docx", "odt", "rtf", "ppt", "pptx", "odp", "xls", "xlsx", "ods",
];
const TIMEOUT: Duration = Duration::from_secs(25);
const CACHE_CAP: usize = 64;

static CONVERT_LOCK: Mutex<()> = Mutex::new(());

/// Whether `path` names an Office document convertible to PDF.
pub fn is_office(path: &Path) -> bool {
    let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
        return false;
    };
    let lower = ext.to_ascii_lowercase();
    OFFICE_EXTS.contains(&lower.as_str())
}

/// True when LibreOffice or (on Windows) Microsoft Office can render PDFs.
pub fn can_convert() -> bool {
    soffice().is_some() || ms_office_available()
}

/// Convert an Office document to a cached PDF, returning its path.
pub fn to_pdf(path: &Path) -> Option<PathBuf> {
    if !can_convert() {
        return None;
    }
    let meta = std::fs::metadata(path).ok()?;
    if !meta.is_file() {
        return None;
    }
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs());
    let base = cache_root();
    let pdf_dir = base.join("doc-pdf");
    std::fs::create_dir_all(&pdf_dir).ok()?;
    let target = pdf_dir.join(format!("{:016x}.pdf", key_of(path, meta.len(), mtime)));
    if target.is_file() {
        return Some(target);
    }

    let _guard = CONVERT_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    if target.is_file() {
        return Some(target);
    }

    let result = if let Some(bin) = soffice() {
        convert_via_libreoffice(bin, &base, path, &target)
    } else {
        convert_via_ms_office(path, &target)
    };
    prune_cache(&pdf_dir);
    result.then_some(target)
}

fn convert_via_libreoffice(bin: &Path, base: &Path, input: &Path, target: &Path) -> bool {
    let stage = base.join(format!("stage-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&stage);
    if std::fs::create_dir_all(&stage).is_err() {
        return false;
    }
    let profile = base.join("lo-profile");
    if std::fs::create_dir_all(&profile).is_err() {
        let _ = std::fs::remove_dir_all(&stage);
        return false;
    }
    let env_arg = format!("-env:UserInstallation={}", file_uri(&profile));
    let mut cmd = Command::new(bin);
    cmd.args(["--headless", "--norestore", "--nolockcheck", &env_arg])
        .args(["--convert-to", "pdf", "--outdir"])
        .arg(&stage)
        .arg(input);
    let ok = run_quiet_bounded(cmd, TIMEOUT)
        && first_pdf_in(&stage).is_some_and(|tmp| {
            std::fs::rename(&tmp, target)
                .or_else(|_| std::fs::copy(&tmp, target).map(|_| ()))
                .is_ok()
        });
    let _ = std::fs::remove_dir_all(&stage);
    ok
}

#[cfg(windows)]
fn ms_office_available() -> bool {
    static HAS: LazyLock<bool> = LazyLock::new(|| {
        for base in [
            r"C:\Program Files\Microsoft Office",
            r"C:\Program Files (x86)\Microsoft Office",
        ] {
            for sub in [
                r"root\Office16\WINWORD.EXE",
                r"Office16\WINWORD.EXE",
                r"Office15\WINWORD.EXE",
            ] {
                if Path::new(base).join(sub).is_file() {
                    return true;
                }
            }
        }
        false
    });
    *HAS
}

#[cfg(not(windows))]
fn ms_office_available() -> bool {
    false
}

#[cfg(windows)]
fn convert_via_ms_office(input: &Path, target: &Path) -> bool {
    let ext = input
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let script = match ext.as_str() {
        "doc" | "docx" | "rtf" | "odt" => {
            "$app = New-Object -ComObject Word.Application; $app.Visible = $false; $app.DisplayAlerts = 0; \
             try { $d = $app.Documents.Open($env:SCOUR_IN, $false, $true); $d.ExportAsFixedFormat($env:SCOUR_OUT, 17); $d.Close($false) } \
             finally { $app.Quit() }"
        }
        "ppt" | "pptx" | "odp" => {
            "$app = New-Object -ComObject PowerPoint.Application; \
             try { $p = $app.Presentations.Open($env:SCOUR_IN, -1, 0, 0); $p.SaveAs($env:SCOUR_OUT, 32); $p.Close() } \
             finally { $app.Quit() }"
        }
        "xls" | "xlsx" | "ods" => {
            "$app = New-Object -ComObject Excel.Application; $app.Visible = $false; $app.DisplayAlerts = $false; \
             try { $b = $app.Workbooks.Open($env:SCOUR_IN, 0, $true); $b.ExportAsFixedFormat(0, $env:SCOUR_OUT); $b.Close($false) } \
             finally { $app.Quit() }"
        }
        _ => return false,
    };
    let mut cmd = Command::new("powershell.exe");
    cmd.args([
        "-NoProfile",
        "-NonInteractive",
        "-WindowStyle",
        "Hidden",
        "-Command",
        script,
    ])
    .env("SCOUR_IN", input)
    .env("SCOUR_OUT", target);
    run_quiet_bounded(cmd, TIMEOUT) && target.is_file()
}

#[cfg(not(windows))]
fn convert_via_ms_office(_: &Path, _: &Path) -> bool {
    false
}

pub(crate) fn no_window(cmd: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let _ = cmd;
}

fn run_quiet_bounded(mut cmd: Command, timeout: Duration) -> bool {
    no_window(&mut cmd);
    let Ok(mut child) = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    else {
        return false;
    };
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(s)) => return s.success(),
            Ok(None) if start.elapsed() > timeout => {
                let _ = child.kill();
                let _ = child.wait();
                return false;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(40)),
            Err(_) => return false,
        }
    }
}

fn first_pdf_in(dir: &Path) -> Option<PathBuf> {
    std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .find(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("pdf")))
}

fn prune_cache(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut files: Vec<(std::time::SystemTime, PathBuf)> = entries
        .flatten()
        .filter_map(|e| {
            let m = e.metadata().ok()?;
            m.is_file()
                .then_some((m.modified().unwrap_or(UNIX_EPOCH), e.path()))
        })
        .collect();
    if files.len() <= CACHE_CAP {
        return;
    }
    files.sort_by_key(|(t, _)| *t);
    for (_, old) in files.iter().take(files.len() - CACHE_CAP) {
        let _ = std::fs::remove_file(old);
    }
}

fn soffice() -> Option<&'static Path> {
    static BIN: LazyLock<Option<PathBuf>> = LazyLock::new(|| {
        for name in ["libreoffice", "soffice", "soffice.exe"] {
            if let Some(p) = crate::find_on_path(name) {
                return Some(p);
            }
        }
        let fixed: &[&str] = &[
            "/Applications/LibreOffice.app/Contents/MacOS/soffice",
            r"C:\Program Files\LibreOffice\program\soffice.exe",
            r"C:\Program Files (x86)\LibreOffice\program\soffice.exe",
        ];
        fixed.iter().map(PathBuf::from).find(|p| p.is_file())
    });
    BIN.as_deref()
}

/// Where previews made of documents are kept; emptied, they are made again.
pub fn cache_root() -> PathBuf {
    if let Some(xdg) = std::env::var_os("XDG_CACHE_HOME") {
        return PathBuf::from(xdg).join("scour");
    }
    #[cfg(windows)]
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        return PathBuf::from(local).join("scour").join("cache");
    }
    #[cfg(target_os = "macos")]
    if let Some(home) = std::env::var_os("HOME") {
        return PathBuf::from(home).join("Library/Caches/scour");
    }
    std::env::var_os("HOME")
        .map(|h| PathBuf::from(h).join(".cache/scour"))
        .unwrap_or_else(|| std::env::temp_dir().join("scour-cache"))
}

fn file_uri(path: &Path) -> String {
    let raw = path.to_string_lossy().replace('\\', "/");
    let body = raw.strip_prefix('/').unwrap_or(&raw);
    let mut out = String::from("file:///");
    for b in body.bytes() {
        if b.is_ascii_alphanumeric() || b"/-_.~:".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn key_of(path: &Path, len: u64, mtime: u64) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    for b in path.to_string_lossy().bytes() {
        h = (h ^ b as u64).wrapping_mul(0x0100_0000_01b3);
    }
    for b in len.to_le_bytes().into_iter().chain(mtime.to_le_bytes()) {
        h = (h ^ b as u64).wrapping_mul(0x0100_0000_01b3);
    }
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_office_extensions_case_insensitively() {
        assert!(is_office(Path::new("/a/report.DOCX")));
        assert!(is_office(Path::new("/a/legacy.doc")));
        assert!(is_office(Path::new("/a/sheet.xlsx")));
        assert!(!is_office(Path::new("/a/book.pdf")));
        assert!(!is_office(Path::new("/a/notes.txt")));
    }
}
