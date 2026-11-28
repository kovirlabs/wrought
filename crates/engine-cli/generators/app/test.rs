//! The site's binary starts: every registration is accepted (a refused one
//! stops it before any command runs) and its routes are listed.

use std::process::Command;

#[test]
fn registrations_are_accepted() {
    let out = Command::new(env!("CARGO_BIN_EXE___bin__"))
        .args(["routes", "--site", env!("CARGO_MANIFEST_DIR")])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}
