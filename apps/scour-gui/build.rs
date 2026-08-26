fn main() {
    slint_build::compile("ui/main.slint").expect("the interface does not compile");
    old_glibc_math();
}

/// Ask libm for the maths it has always had, not the maths it added last month.
///
/// **This is what makes the window runnable on somebody else's computer.**
/// Two calls decide it: Slint reaches `f32::acos` and `f32::atan2`, which are
/// `acosf` and `atan2f` in libm — and glibc 2.43 added a second, correctly
/// rounded version of each. The linker binds an unversioned reference to the
/// *newest* version it can see, so a binary built on a machine with glibc 2.43
/// demands 2.43 wherever it is copied. Measured on an Ubuntu 24.04 guest
/// (glibc 2.39): `scour`, `scourd`, `scour-tui`, `scour-web` and `scour-watch`
/// all ran; the window alone died with
/// `libm.so.6: version 'GLIBC_2.43' not found`. Two symbols out of thousands,
/// and they cost the whole desktop face.
///
/// So the reference is pinned to the version that has been in every x86-64
/// glibc since 2002. The old and new implementations differ in the last bit of
/// a rounding, which is a distinction a mouse cursor's angle does not have.
///
/// A wrapper rather than a bare `.symver` on the import, because the calls
/// come from Slint's object files rather than this one: a definition here wins
/// for every reference in the executable, and forwards. `-fno-builtin` so the
/// compiler does not recognise the name and turn the forward into a self-call.
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
    // **A missing compiler is not a build failure.** Everything here is about
    // where the result can be copied to; a machine building for itself does
    // not need it, and refusing to build without a C compiler would be a new
    // requirement bought for nothing.
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
