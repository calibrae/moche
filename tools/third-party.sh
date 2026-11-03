#!/bin/sh
# Generates docs/THIRD_PARTY.md: the third-party crates linked into moche-ffi
# for the iOS target (crate, version, licence expression, licence texts copied
# from the registry sources). Needs cargo, python3 and a populated cargo
# registry (run `cargo fetch` first). cargo-about is not used: it is not
# installed here and this does the same job from `cargo metadata`.
#
# Usage: tools/third-party.sh [--check]    (--check fails if the file is stale)
set -eu
root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"
out=docs/THIRD_PARTY.md
tmp=$(mktemp)
trap 'rm -f "$tmp"' EXIT
cargo metadata --format-version 1 --locked --filter-platform aarch64-apple-ios \
    | python3 -I tools/third-party.py moche-ffi > "$tmp"
if [ "${1:-}" = "--check" ]; then
    cmp -s "$tmp" "$out" || { echo "$out is stale: run tools/third-party.sh" >&2; exit 1; }
    echo "$out is up to date"
else
    cp "$tmp" "$out"
    echo "wrote $out"
fi
