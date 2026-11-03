#!/bin/sh
# Compiles crates/moche-ffi/tests/c/test_moche.c with the host C compiler
# against the debug staticlib and runs it. Usage: tools/ffi/run-c-test.sh
set -eu
root=$(cd "$(dirname "$0")/../.." && pwd)
(cd "$root" && cargo build -q -p moche-ffi)
out=$(mktemp -d)
trap 'rm -rf "$out"' EXIT
${CC:-cc} -std=c99 -Wall -Wextra -Werror -pedantic \
    -I "$root/crates/moche-ffi/include" \
    "$root/crates/moche-ffi/tests/c/test_moche.c" \
    "$root/target/debug/libmoche_ffi.a" -lm -o "$out/test_moche"
"$out/test_moche"
