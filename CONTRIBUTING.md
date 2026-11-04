# Contributing

moche is small on purpose: a client-only mosh core, a CLI and a C API. Plain and idiomatic
Rust, `unsafe` only in `crates/moche-ffi`, no new dependency without a reason.

## Build and test

The toolchain is pinned in `rust-toolchain.toml` (rustup installs it).

    cargo fmt --check
    cargo clippy --all-targets -- -D warnings
    cargo test --workspace
    cargo deny check licenses            # only MIT/Apache-2.0/BSD/ISC/Zlib/Unicode-3.0
    tools/ffi/check-header.sh            # crates/moche-ffi/include/moche.h matches the exports
    tools/third-party.sh --check         # docs/THIRD_PARTY.md is current (run it without --check to update)

The capture-replay tests skip themselves when the captures are absent (they are not in the
repository). The end-to-end test needs a stock `mosh-server` and python3:
`MOCHE_E2E=1 cargo test -p moche-cli --test e2e -- --ignored --nocapture`.

## Clean room

Do not submit code derived from mosh's source or from any GPL implementation of the mosh
protocol, and do not read them while working on the matching part. Cite a public source or a
capture for protocol behaviour. Read [docs/CLEAN_ROOM.md](docs/CLEAN_ROOM.md) first.

## Hygiene

- No secrets, tokens, private hostnames, IP addresses of real machines or home paths in
  code, tests, docs or commit messages. Use placeholders (`192.0.2.x`, `example.org`).
- Session keys in captures and tests are throwaway keys of lab sessions, never real ones.
- No panics across the C boundary: every export catches them.

## Changes to SSP or the network layer

1. **Wire neutrality.** A change that should not alter what moche sends (refactors,
   performance work) must keep `tools/perf/golden-diff.sh [BASE_REF]` identical: it runs a
   deterministic scenario on a base ref and on your tree and requires byte-identical datagrams,
   timings and paints.
2. **Lab validation against a real mosh-server.** A change to what or when moche sends must
   be tried against a stock `mosh-server` (see `tools/mosh-lab` and `tools/vm-lab`), with
   loss and delay (`tc netem`), and recorded as an `observations` document.
   Unit tests are not enough, because they only check moche against our own reading of the
   protocol. Two bugs that every unit test passed are the reason:
   - B1: "out of sync" at session start under 10 % loss, never recovering; B2: the stock server aborted when moche abandoned an
     in-flight state during a large paste. Found, fixed and re-validated in
     [round 4](docs/observations-round4.md), [round 5](docs/observations-round5.md) and
     [round 6](docs/observations-round6.md).
