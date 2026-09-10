fn main() {
    scour_icon::embed();
    slint_build::compile("ui/main.slint").expect("the interface does not compile");
    old_glibc_math();
}

/// Pin `acosf`/`atan2f` to GLIBC_2.2.5 so the window runs on older glibc.
///
/// Slint reaches `f32::acos`/`f32::atan2`; the linker would otherwise bind them
/// to the correctly rounded versions glibc 2.43 added. A wrapper rather than a
/// `.symver` on the import, because the calls come from Slint's object files.
fn old_glibc_math() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("linux")
        || std::env::var("CARGO_CFG_TARGET_ENV").as_deref() != Ok("gnu")
    {
        return;
    }
    let out = std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let src = out.join("old-libm.c");
    std::fs::write(
        &src,
        r#"
__asm__(".symver scour_acosf, acosf@GLIBC_2.2.5");
__asm__(".symver scour_atan2f, atan2f@GLIBC_2.2.5");
float scour_acosf(float);
float scour_atan2f(float, float);
float acosf(float x) { return scour_acosf(x); }
float atan2f(float y, float x) { return scour_atan2f(y, x); }
"#,
    )
    .expect("writing the compatibility shim");

    let obj = out.join("old-libm.o");
    let cc = std::env::var("CC").unwrap_or_else(|_| "cc".into());
    let run = std::process::Command::new(&cc)
        .args(["-c", "-O2", "-fPIC", "-fno-builtin"])
        .arg(&src)
        .arg("-o")
        .arg(&obj)
        .status();
    // A missing C compiler only costs portability, not this build.
    match run {
        Ok(s) if s.success() => {}
        _ => {
            println!("cargo:warning=no C compiler; this build needs the glibc it was made on");
            return;
        }
    }
    let lib = out.join("libscouroldlibm.a");
    let _ = std::fs::remove_file(&lib);
    let ar = std::process::Command::new("ar")
        .arg("crs")
        .arg(&lib)
        .arg(&obj)
        .status();
    if !matches!(ar, Ok(s) if s.success()) {
        println!("cargo:warning=no ar; this build needs the glibc it was made on");
        return;
    }
    println!("cargo:rustc-link-search=native={}", out.display());
    println!("cargo:rustc-link-lib=static=scouroldlibm");
    println!("cargo:rerun-if-changed=build.rs");
}
