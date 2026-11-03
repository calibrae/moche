//! Compiles and runs the C test against the staticlib, and checks the header
//! against the library's exported symbols. Skipped when no C compiler or `nm`.

use std::path::PathBuf;
use std::process::Command;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn have(tool: &str) -> bool {
    Command::new(tool).arg("--version").output().is_ok() || Command::new(tool).output().is_ok()
}

fn script(name: &str) {
    if !have("cc") || !have("nm") {
        eprintln!("skipping {name}: no cc/nm");
        return;
    }
    let out = Command::new("sh")
        .arg(root().join("tools/ffi").join(name))
        .output()
        .expect("run script");
    assert!(
        out.status.success(),
        "{name} failed:\n{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn c_test_program_passes() {
    script("run-c-test.sh");
}

#[test]
fn header_matches_library_symbols() {
    script("check-header.sh");
}
