//! Execute the browser's actual paging and refresh policy without a DOM.

#[test]
fn paging_and_refresh_behaviour() {
    let script = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/page.test.cjs");
    let output = match std::process::Command::new("node")
        .arg("--test")
        .arg(script)
        .output()
    {
        Ok(output) => output,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            eprintln!("page behaviour tests skipped: node is unavailable");
            return;
        }
        Err(error) => panic!("could not run page tests: {error}"),
    };
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
