//! Predictive echo against a stock local `mosh-server` through a delaying UDP relay,
//! watched with tmux. Run with
//! `MOCHE_E2E=1 cargo test -p moche-cli --test predict_e2e -- --ignored --nocapture`.
//! Skips (passes) when python3, tmux or mosh-server is missing.

use std::process::Command;

#[test]
#[ignore = "needs mosh-server, tmux and python3; set MOCHE_E2E=1"]
fn predicted_echo_through_a_delaying_relay() {
    if std::env::var_os("MOCHE_E2E").is_none() {
        eprintln!("skip: MOCHE_E2E not set");
        return;
    }
    let script = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/predict_e2e.py");
    let relay = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tools/perf/delay_relay.py"
    );
    let status = match Command::new("python3")
        .args(["-I", script, env!("CARGO_BIN_EXE_moche"), relay])
        .status()
    {
        Ok(s) => s,
        Err(_) => return eprintln!("skip: python3 missing"),
    };
    match status.code() {
        Some(0) => {}
        Some(77) => eprintln!("skip: mosh-server or tmux missing"),
        c => panic!("predict e2e failed: {c:?}"),
    }
}
