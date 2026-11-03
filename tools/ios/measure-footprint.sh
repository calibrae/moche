#!/bin/bash
# Measure what moche-core costs in an iOS-simulator executable.
# Builds a throwaway staticlib (outside the repo) that reaches Transport and Screen,
# links it with -dead_strip, and compares with an empty main.
# Usage: measure-footprint.sh [MOCHE_SRC] [WORK_DIR]
set -euo pipefail
export DEVELOPER_DIR="${DEVELOPER_DIR:-/Applications/Xcode.app/Contents/Developer}"
export PATH="/opt/homebrew/bin:$HOME/.cargo/bin:$PATH"

SRC="${1:-$HOME/moche-ios/src}"
WORK="${2:-$HOME/moche-ios/probe}"
T=aarch64-apple-ios-sim
SDK="$(xcrun --sdk iphonesimulator --show-sdk-path)"

mkdir -p "$WORK/src"
cat > "$WORK/Cargo.toml" <<TOML
[package]
name = "moche-probe"
version = "0.0.0"
edition = "2021"
publish = false

[lib]
crate-type = ["staticlib"]

[dependencies]
moche-core = { path = "$SRC/crates/moche-core" }

[profile.release]
opt-level = "s"
lto = "fat"
codegen-units = 1
panic = "abort"
strip = "symbols"
TOML
[ -f "$SRC/Cargo.lock" ] && cp "$SRC/Cargo.lock" "$WORK/Cargo.lock"

cat > "$WORK/src/lib.rs" <<'RS'
use moche_core::{crypto::Session, ssp::Transport, Screen};

/// Exercises Transport: new, send_keys, handle_datagram, poll_transmit.
#[no_mangle]
pub extern "C" fn probe_transport(data: *const u8, len: usize) -> usize {
    let s = Session::from_base64("AAECAwQFBgcICQoLDA0ODw").unwrap();
    let mut t = Transport::new(&s, 0, Box::new(|b| b.iter_mut().for_each(|x| *x = 0x5a)));
    t.send_keys(1, b"ls\r");
    let d = unsafe { std::slice::from_raw_parts(data, len) };
    let mut n = t.handle_datagram(10, d).len();
    n += t.next_deadline_ms().unwrap_or(0) as usize;
    while let Some(p) = t.poll_transmit(20) {
        n += p.len();
    }
    n
}

/// Exercises Screen: new, apply, repaint.
#[no_mangle]
pub extern "C" fn probe_screen(data: *const u8, len: usize) -> usize {
    let d = unsafe { std::slice::from_raw_parts(data, len) };
    let mut sc = Screen::new(80, 24);
    let _ = sc.apply(0, 1, Some((100, 30)), d);
    sc.repaint(None).len()
}
RS

cat > "$WORK/empty.c" <<'C'
int main(void) { return 0; }
C
cat > "$WORK/probe.c" <<'C'
#include <stdio.h>
#include <stddef.h>
size_t probe_transport(const unsigned char *, size_t);
size_t probe_screen(const unsigned char *, size_t);
int main(void) {
    const unsigned char junk[] = "\x1b[31mhello\x1b[0m\r\nworld";
    printf("transport=%zu screen=%zu\n", probe_transport(junk, sizeof junk - 1),
           probe_screen(junk, sizeof junk - 1));
    return 0;
}
C

cd "$WORK"
cargo build --release --target "$T"
CC=(clang -target arm64-apple-ios-simulator -isysroot "$SDK" -Wl,-dead_strip)
"${CC[@]}" empty.c -o empty
"${CC[@]}" probe.c "target/$T/release/libmoche_probe.a" -o probe
echo "--- sizes (bytes)"; stat -f '%z %N' empty probe
echo "--- size -m empty"; size -m empty
echo "--- size -m probe"; size -m probe
echo "--- strip -x copies"
cp empty empty.s; cp probe probe.s; strip -x empty.s probe.s; stat -f '%z %N' empty.s probe.s
