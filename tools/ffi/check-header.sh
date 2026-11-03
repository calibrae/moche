#!/bin/sh
# Checks that every moche_* function declared in moche.h is exported by the
# static library and vice versa. Usage: tools/ffi/check-header.sh [libmoche_ffi.a]
# Builds the debug staticlib when no path is given.
set -eu
root=$(cd "$(dirname "$0")/../.." && pwd)
hdr=$root/crates/moche-ffi/include/moche.h
lib=${1:-}
if [ -z "$lib" ]; then
    (cd "$root" && cargo build -q -p moche-ffi)
    lib=$root/target/debug/libmoche_ffi.a
fi
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

# declarations: "name(" after a return type, at line start or after "*"
grep -Eo '^[A-Za-z_][A-Za-z0-9_ ]*[ *]\*?moche_[a-z0-9_]+\(' "$hdr" \
    | grep -Eo 'moche_[a-z0-9_]+' | sort -u > "$tmp/header"
# defined text symbols (macOS prefixes an underscore)
nm -g -U "$lib" 2>/dev/null | awk '$2 == "T" || $2 == "t" {print $3}' \
    | sed 's/^_//' | grep -E '^moche_' | sort -u > "$tmp/lib"

if [ ! -s "$tmp/header" ] || [ ! -s "$tmp/lib" ]; then
    echo "check-header: empty symbol list (header or library unreadable)" >&2
    exit 2
fi
status=0
if ! diff -u "$tmp/header" "$tmp/lib" > "$tmp/diff"; then
    echo "check-header: header and library disagree (- header only, + library only):" >&2
    cat "$tmp/diff" >&2
    status=1
else
    echo "check-header: ok ($(wc -l < "$tmp/header" | tr -d ' ') symbols)"
fi
exit $status
