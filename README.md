# moche

A mosh-compatible client in Rust, written from the protocol and not from mosh's source.
moche talks to a **stock `mosh-server`**: it is a client only. It is made of:

- `moche-core`: a sans-IO core (no sockets, threads or clocks inside) that implements the
  encrypted datagram layer, the state synchronisation protocol (SSP) and a small terminal
  emulator for the screen the server sends;
- `moche`: a command-line client built on the core (ssh bootstrap, UDP socket, raw tty);
- `moche-ffi` and `swift/`: a C API and a Swift package so an iOS app can embed the core
  (the iOS SSH client sigillo is the first consumer).

The name is French for "ugly", a pun on mosh. moche is **not affiliated with the mosh
project**. mosh is GPLv3; no mosh code is included or copied here, and moche was written
without consulting mosh's source (see [docs/CLEAN_ROOM.md](docs/CLEAN_ROOM.md)).

## Status

Version 0.1.0. It works in the tests and lab sessions we ran, and it is much less
field-tested than mosh. What was validated:

- against **`mosh-server` 1.4.0 only** (Debian package), on **IPv4**;
- interactive use (shell, `vim`, `htop`), resizes, roaming by taking an interface down,
  packet loss and delay with `tc netem`, large pastes and floods; the evidence is in
  [docs/observations*.md](docs/observations.md).

See "Not yet / known gaps" below before relying on it.

## Requirements

- On the remote host: `mosh-server` installed, and UDP ports 60000-61000 reachable (the
  port is taken from the server's `MOSH CONNECT` line).
- ssh on the local machine: moche starts the server through your `ssh` command, then
  drops the ssh connection and talks UDP only.

## Install

Release tarballs for `x86_64-unknown-linux-musl` and `aarch64-unknown-linux-musl` (static)
and `aarch64-apple-darwin` are attached to each
[GitHub release](https://github.com/calibrae/moche/releases), with a `SHA256SUMS` file.

    shasum -a 256 -c SHA256SUMS            # or: sha256sum -c SHA256SUMS
    tar -xzf moche-0.1.0-<target>.tar.gz
    ./moche-0.1.0-<target>/moche --version

The macOS binary is not notarized: a downloaded copy needs
`xattr -d com.apple.quarantine moche`. There is no Windows and no macOS x86_64 build.

From source (the toolchain is pinned in `rust-toolchain.toml`, currently rustc 1.99.0;
rustup installs it by itself):

    cargo build --release -p moche-cli      # target/release/moche

## Usage

    moche user@host                         # start mosh-server over ssh, then connect
    moche user@host -- tmux attach          # run a startup command instead of the shell
    moche --direct HOST PORT                # server already running; key from $MOSH_KEY
    moche --direct HOST PORT --key-stdin    # key read from stdin

Other options: `--server-lang LOCALE` (locale for the remote server, must be UTF-8,
default `en_US.UTF-8`; the local `LANG` is never sent), `--server PATH`, `--ssh CMD`,
`--ssh-arg ARG`, `-p LO:HI` (UDP port range), `--no-altscreen`, `--stats` (transport
counters on exit), `--help`. Quit with `Ctrl-^` then `.`; `Ctrl-^ Ctrl-^` sends a
literal `Ctrl-^`.

## How it works

1. The host app (here `moche`, or your iOS app) runs `mosh-server new` over ssh and reads
   `MOSH CONNECT <port> <key>`; the 22-character key is a 128-bit AES key.
2. Every UDP datagram is `nonce (8 bytes) || AES-128-OCB3 ciphertext || tag`; the top bit of
   the nonce is the direction.
3. Inside: timestamps for the round-trip estimate, and fragments of a zlib-compressed
   protobuf instruction (`old_num`, `new_num`, `ack_num`, `throwaway_num`, `diff`, `chaff`).
4. The server sends each screen update as a diff against a *numbered earlier state*. moche
   keeps numbered framebuffer snapshots, applies each diff to the right base and drops
   states the server has thrown away.
5. A small VT interpreter turns the host bytes in a diff into a cell grid; the CLI paints
   the current grid, the C API hands the app a full repaint or a minimal update.
6. Keystrokes go back the same way; resends, acks and heartbeats follow the SRTT, so the
   session survives roaming and suspends without a new login.

The wire format as moche understands it, with each claim marked sourced, observed or
unverified, is [docs/protocol.md](docs/protocol.md).

## How it compares to stock mosh

Measured, not estimated. Conditions: Linux aarch64 VM client (2 vCPU, Ubuntu 24.04) against a
stock `mosh-server` 1.4.0 in a second VM over a virtual network (RTT about 1.5 ms), static
musl moche release build against the distro `mosh-client` 1.4.0; 3-run medians (3
repetitions per cell, fresh server per run); optimisation rounds 5-7. The stock client
was measured in two runs of the same lab, so its two values are shown. Full tables, method
and caveats: [docs/PERF.md](docs/PERF.md) (section "Linux A/B"). Loopback and macOS
numbers are in the same file; they show the same direction.

| scenario | moche | stock mosh-client |
|---|---:|---:|
| idle 60 s, CPU | 0.007 s | 0.009 s / 0.010 s |
| typing 30 s (300 keys), CPU | 0.155 s | 0.173 s / 0.219 s |
| paste 1 MiB, CPU | 0.035 s | 0.243 s / 0.204 s |
| paste 1 MiB, peak RSS | 3.7 MB | 185 MB / 215 MB |
| idle, peak RSS | 1.2 MB | 13.9 MB |
| startup, launch to first paint | 0.129 s | 0.269 s / 0.274 s |
| binary | 0.63 MB, static, no shared libraries | 0.33 MB, 9 shared libraries |
| typing echo, median (round 6 lab harness, 20 samples) | 28 ms | 17 ms |

The paste figures come from the stock client buffering the pasted input (cause not
investigated); wall time for the paste is about the same for both (0.45 s against 0.42-0.51 s).

## Not yet / known gaps

- No predictive local echo (mosh's best-known feature). The core is designed so it can be
  added later.
- Typing echo is about 10 ms slower than stock at the median (28 ms against 17 ms,
  observations round 6): moche waits 8 ms to batch keystrokes.
- Only tested against `mosh-server` 1.4.0. Other versions are unknown.
- Only IPv4 has been tested; IPv6 may or may not work.
- VT coverage is what the server emitted in our test scenarios (shell, `ls --color`,
  `vim`, `htop`, 256 colours, truecolor, wide characters); an escape sequence we never
  saw may be handled wrongly.
- No Windows build, no macOS x86_64 build, no notarization.
- Much less field-tested than mosh: a handful of lab sessions, not years of use.

## Documentation

- [docs/protocol.md](docs/protocol.md): the wire format and its sources
- [docs/observations.md](docs/observations.md) and `docs/observations-round*.md`: the captures and lab runs behind it
- [docs/PERF.md](docs/PERF.md): resource measurements against stock mosh
- [docs/CLEAN_ROOM.md](docs/CLEAN_ROOM.md): how the code was written, and its limits
- [docs/RELEASING.md](docs/RELEASING.md), [SECURITY.md](SECURITY.md), [CONTRIBUTING.md](CONTRIBUTING.md)
- [swift/README.md](swift/README.md): the iOS package

## AI assistance

The code and documents in this repository were written with the help of an AI model
(Anthropic's Claude, through Claude Code), under the clean-room instructions in
[docs/CLEAN_ROOM.md](docs/CLEAN_ROOM.md), which describes the process.

## Licence

MIT OR Apache-2.0, at your option: [LICENSE-MIT](LICENSE-MIT), [LICENSE-APACHE](LICENSE-APACHE).
Dependencies are permissive only (checked in CI with `cargo deny`); their licences are
in [docs/THIRD_PARTY.md](docs/THIRD_PARTY.md).
