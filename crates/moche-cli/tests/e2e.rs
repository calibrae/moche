//! End-to-end proof against a stock local `mosh-server`. Run with
//! `MOCHE_E2E=1 cargo test -p moche-cli --test e2e -- --ignored --nocapture`.
//! Skips (passes) when python3 or mosh-server is missing.
//!
//! `e2e.py` drives moche in a pty and records the bytes moche wrote to the
//! tty plus checkpoints; this test replays them into a `Framebuffer` and
//! asserts the resulting screen text at each checkpoint.

use std::path::PathBuf;
use std::process::Command;

use moche_core::vt::Framebuffer;

#[test]
#[ignore = "needs mosh-server and python3; set MOCHE_E2E=1"]
fn local_mosh_server_session() {
    if std::env::var_os("MOCHE_E2E").is_none() {
        eprintln!("skip: MOCHE_E2E not set");
        return;
    }
    let script = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/e2e.py");
    let cap: PathBuf = [env!("CARGO_TARGET_TMPDIR"), "moche-e2e"].iter().collect();
    let _ = std::fs::remove_dir_all(&cap);
    let status = match Command::new("python3")
        .args(["-I", script, env!("CARGO_BIN_EXE_moche")])
        .arg(&cap)
        .status()
    {
        Ok(s) => s,
        Err(_) => return eprintln!("skip: python3 missing"),
    };
    match status.code() {
        Some(0) => {}
        Some(77) => return eprintln!("skip: mosh-server missing"),
        c => panic!("e2e driver failed: {c:?}"),
    }

    let tty = std::fs::read(cap.join("tty.bin")).unwrap();
    let table = std::fs::read_to_string(cap.join("checkpoints.tsv")).unwrap();
    let mut fb = Framebuffer::new(80, 24);
    let mut fed = 0usize;
    let mut failures = Vec::new();
    for line in table.lines() {
        let f: Vec<&str> = line.split('\t').collect();
        let (name, off) = (f[0], f[1].parse::<usize>().unwrap());
        let (cols, rows) = (f[2].parse::<u16>().unwrap(), f[3].parse::<u16>().unwrap());
        if (fb.cols(), fb.rows()) != (cols, rows) {
            fb.resize(cols, rows);
        }
        fb.feed(&tty[fed..off]);
        fed = off;
        let text = fb.text();
        for e in &f[4..] {
            let (min, needle, neg) = if let Some(n) = e.strip_prefix('!') {
                (0, n, true)
            } else if let Some((n, t)) = e.split_once('*') {
                (n.parse::<usize>().unwrap(), t, false)
            } else {
                (1, *e, false)
            };
            let count = text.matches(needle).count();
            let ok = if neg { count == 0 } else { count >= min };
            if !ok {
                failures.push(format!("{name}: expectation {e:?} failed (count {count})"));
            }
        }
        let shown: Vec<String> = text.lines().map(|l| l.trim_end().to_string()).collect();
        eprintln!("--- {name} ({cols}x{rows}) ---\n{}", shown.join("\n"));
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
