//! The page is written in parts and served as one document: `src/page/
//! shell.html` with the style and the script put in where it says, each a run
//! of files in the order their names sort. One scope still — the parts are
//! text, joined, and a name declared twice is still caught by `node --check`.

use std::path::Path;

fn main() {
    scour_icon::embed();
    let page = Path::new("src/page");
    println!("cargo::rerun-if-changed={}", page.display());
    let joined = std::fs::read_to_string(page.join("shell.html"))
        .expect("src/page/shell.html")
        .replacen("@STYLE@\n", &parts(&page.join("style")), 1)
        .replacen("@SCRIPT@\n", &parts(&page.join("script")), 1);
    let out = Path::new(&std::env::var("OUT_DIR").expect("OUT_DIR")).join("page.html");
    std::fs::write(out, joined).expect("write page.html");
}

/// Every file in `dir`, in name order, one after another.
fn parts(dir: &Path) -> String {
    let mut names: Vec<_> = std::fs::read_dir(dir)
        .expect("a page directory")
        .map(|e| e.expect("an entry").path())
        .collect();
    names.sort();
    names
        .iter()
        .map(|p| std::fs::read_to_string(p).expect("a page part"))
        .collect()
}
