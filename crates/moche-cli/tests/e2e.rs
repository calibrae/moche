//! End-to-end proof against a stock local `mosh-server`. Run with
//! `MOCHE_E2E=1 cargo test -p moche-cli --test e2e -- --ignored --nocapture`.
//! Skips (passes) when python3 or mosh-server is missing.

use std::process::Command;

#[test]
#[ignore = "needs mosh-server and python3; set MOCHE_E2E=1"]
fn local_mosh_server_session() {
    if std::env::var_os("MOCHE_E2E").is_none() {
        eprintln!("skip: MOCHE_E2E not set");
        return;
    }
    let script = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/e2e.py");
    let status = match Command::new("python3")
        .args(["-I", script, env!("CARGO_BIN_EXE_moche")])
        .status()
    {
        Ok(s) => s,
        Err(_) => return eprintln!("skip: python3 missing"),
    };
    match status.code() {
        Some(0) => {}
        Some(77) => eprintln!("skip: mosh-server missing"),
        c => panic!("e2e failed: {c:?}"),
    }
}
