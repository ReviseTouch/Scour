//! The application icon, put into a Windows executable at build time.
//!
//! Explorer shows an `.exe` by the icon in its resources; nothing else gives
//! one. Every app's `build.rs` calls [`embed`], which does nothing off Windows
//! and nothing when no resource compiler is on the path — a check builds
//! without linking, and a machine without `llvm-rc` still builds, iconless.

use std::path::PathBuf;
use std::process::Command;

/// Compile `assets/scour.ico` into a resource and hand it to the linker.
pub fn embed() {
    println!("cargo:rerun-if-changed=build.rs");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let ico = root.join("assets/scour.ico");
    println!("cargo:rerun-if-changed={}", ico.display());
    let Ok(out) = std::env::var("OUT_DIR").map(PathBuf::from) else {
        return;
    };
    let rc = out.join("scour.rc");
    let res = out.join("scour.res");
    // `1 ICON` is the first icon in the file, which is the one Explorer shows.
    if std::fs::write(
        &rc,
        format!(
            "1 ICON \"{}\"\n",
            ico.display().to_string().replace('\\', "/")
        ),
    )
    .is_err()
    {
        return;
    }
    // `rc.exe` on a Windows host, `llvm-rc` anywhere LLVM is.
    let tried = [("rc", vec!["/nologo", "/fo"]), ("llvm-rc", vec!["/FO"])];
    for (tool, flags) in tried {
        let ok = Command::new(tool)
            .args(&flags)
            .arg(&res)
            .arg(&rc)
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if ok {
            println!("cargo:rustc-link-arg-bins={}", res.display());
            return;
        }
    }
    println!(
        "cargo:warning=no resource compiler (rc or llvm-rc); the executables will have no icon"
    );
}
