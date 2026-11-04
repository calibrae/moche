#!/usr/bin/env bash
# Wire-neutrality proof: run the deterministic golden scenario
# (crates/moche-core/examples/golden.rs) on a git ref and on the working tree and
# require byte-identical output (every datagram emitted, its time, every deadline
# change, decoded events, host paints, and a VT corpus).
#   tools/perf/golden-diff.sh [BASE_REF]      default HEAD
# The example uses only the public API, so it also builds against older commits.
set -euo pipefail
ref="${1:-HEAD}"
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
tmp="$(mktemp -d)"
trap 'git -C "$root" worktree remove --force "$tmp/base" >/dev/null 2>&1 || true; rm -rf "$tmp"' EXIT
git -C "$root" worktree add --detach "$tmp/base" "$ref" >/dev/null
mkdir -p "$tmp/base/crates/moche-core/examples"
cp "$root/crates/moche-core/examples/golden.rs" "$tmp/base/crates/moche-core/examples/golden.rs"
(cd "$tmp/base" && CARGO_TARGET_DIR="$tmp/target" cargo run -q --release -p moche-core --example golden > "$tmp/before.txt")
(cd "$root" && CARGO_TARGET_DIR="$tmp/target-new" cargo run -q --release -p moche-core --example golden > "$tmp/after.txt")
if cmp -s "$tmp/before.txt" "$tmp/after.txt"; then
  echo "golden: IDENTICAL ($(wc -l < "$tmp/after.txt") lines, $(grep -c ' tx ' "$tmp/after.txt") datagrams sent, $(grep -c '^vt ' "$tmp/after.txt") VT cases)"
else
  diff "$tmp/before.txt" "$tmp/after.txt" | head -20
  echo "golden: DIFFERENT" >&2
  exit 1
fi
