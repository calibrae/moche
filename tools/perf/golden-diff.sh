#!/usr/bin/env bash
# Wire-neutrality proof: run the deterministic golden scenario
# (crates/moche-core/examples/golden.rs) on a git ref and on the working tree and
# require byte-identical output (every datagram emitted, its time, every deadline
# change, decoded events, host paints, and a VT corpus).
#   tools/perf/golden-diff.sh [BASE_REF]      default HEAD
#   tools/perf/golden-diff.sh --report [BASE_REF]   no failure on a difference: print the before/after
#                                              report (datagrams and bytes per phase, key latency)
#   tools/perf/golden-diff.sh --pin             record the working tree's output hash in
#                                              tools/perf/golden.sha256 (for a deliberate wire change)
#   tools/perf/golden-diff.sh --pinned          require the working tree's output to match that hash
# The example uses only the public API, so it also builds against older commits.
set -euo pipefail
mode=diff
case "${1:-}" in --report|--pin|--pinned) mode="${1#--}"; shift ;; esac
ref="${1:-HEAD}"
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
tmp="$(mktemp -d)"
if [ "$mode" = pin ] || [ "$mode" = pinned ]; then
  (cd "$root" && CARGO_TARGET_DIR="$tmp/target-new" cargo run -q --release -p moche-core --example golden > "$tmp/after.txt")
  sum="$(shasum -a 256 "$tmp/after.txt" | cut -d' ' -f1)"
  pin="$root/tools/perf/golden.sha256"
  rm -rf "$tmp"
  if [ "$mode" = pin ]; then echo "$sum" > "$pin"; echo "golden: pinned $sum"; exit 0; fi
  if [ "$sum" = "$(cat "$pin")" ]; then echo "golden: MATCHES the pinned output"; exit 0; fi
  echo "golden: DIFFERENT from the pinned output (re-pin with --pin if the wire change is deliberate)" >&2; exit 1
fi
trap 'git -C "$root" worktree remove --force "$tmp/base" >/dev/null 2>&1 || true; rm -rf "$tmp"' EXIT
git -C "$root" worktree add --detach "$tmp/base" "$ref" >/dev/null
mkdir -p "$tmp/base/crates/moche-core/examples"
cp "$root/crates/moche-core/examples/golden.rs" "$tmp/base/crates/moche-core/examples/golden.rs"
(cd "$tmp/base" && CARGO_TARGET_DIR="$tmp/target" cargo run -q --release -p moche-core --example golden > "$tmp/before.txt")
(cd "$root" && CARGO_TARGET_DIR="$tmp/target-new" cargo run -q --release -p moche-core --example golden > "$tmp/after.txt")
if [ "$mode" = report ]; then
  python3 "$root/tools/perf/golden_report.py" "$tmp/before.txt" "$tmp/after.txt"
  exit 0
fi
if cmp -s "$tmp/before.txt" "$tmp/after.txt"; then
  echo "golden: IDENTICAL ($(wc -l < "$tmp/after.txt") lines, $(grep -c ' tx ' "$tmp/after.txt") datagrams sent, $(grep -c '^vt ' "$tmp/after.txt") VT cases)"
else
  diff "$tmp/before.txt" "$tmp/after.txt" | head -20
  echo "golden: DIFFERENT" >&2
  exit 1
fi
